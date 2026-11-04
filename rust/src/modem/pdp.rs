//! PDP 上下文状态、拨号与去激活。
//!
//! ## 全文最要紧的一条实机教训
//!
//! 上下文**未激活**时，`AT+CGCONTRDP` 与 `AT+CGPADDR` 仍会原样返回上一轮
//! 会话的残留地址（执行 `AT+CGACT=0,1` 后二者返回值毫无变化，而
//! `AT+CGACT?` 明确不再列出该 cid）。早期版本把它们当作激活判据，导致
//! `dial()` 在「已激活且已有地址」处短路返回、永不下发 `AT+CGACT=1,<cid>`，
//! 接口一直写着死地址：ARP 能通（模组代理应答）但三层零回包、
//! `ip -s link` 里 rx_packets 冻结。
//!
//! 因此**激活判据只有一个权威来源：`AT+CGACT?`**。
//! `CGCONTRDP` / `CGPADDR` 只负责提供地址与 DNS，不再单独决定 `active`。

use super::run_as;
use crate::at::queue::AtPriority;
use crate::addr::{is_valid_ipv4, looks_v6, normalize_ipv6, parse_contrdp_addr};
use crate::at::{at_cmd, AtHandle, AtResult};
use crate::config::Config;
use std::time::Duration;

/// PDP 激活后地址可能晚一步就绪，这里轮询若干次（每次 600 ms）。
const ADDR_POLL_ROUNDS: u32 = 5;
const ADDR_POLL_INTERVAL_MS: u64 = 600;

/// 飞行模式自恢复后的注册等待：10 × 5 s = 50 s 上限。
/// 超时后仍继续走原拨号路径 —— 失败信息自带前置体检，不会静默。
///
/// 上限受拨号任务的总预算约束：AT 工作线程对单个任务有 95 s 看门狗，
/// 「CFUN 恢复 + 附着等待 + 激活 + 地址轮询 + 一次激活恢复重试」的最坏
/// 组合必须留出余量，不能各自取最大值。
const RECOVER_REG_POLLS: u32 = 10;
const RECOVER_REG_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// 已注册但 EPS 附着未完成时，补发 `AT+CGATT=1` 后的限时等待：8 × 2.5 s。
const ATTACH_POLLS: u32 = 8;
const ATTACH_POLL_INTERVAL: Duration = Duration::from_millis(2500);

/// 重建 IMS 占用的 PDN 前等待承载释放的时间（秒）。
///
/// 去激活后立即激活，模组侧 PDN 尚未释放，激活会回 `+CME ERROR` 或
/// 激活成功却不发数据地址（与 daemon::dial::redial 的 3 s 同源）。
const IMS_REBUILD_SETTLE_SECS: u64 = 3;

#[derive(Debug, Default, serde::Serialize)]
pub struct PdpState {
    pub cid: u32,
    pub apn: String,
    pub pdp_type: String,
    pub active: bool,
    pub ipv4: String,
    pub ipv6: String,
    /// 模组侧 `AT+CGCONTRDP` 字段 3 上报的 IPv4 子网掩码。
    ///
    /// 手册 §12.2.10 的字段 3 是 `<local_addr and subnet_mask>`，IPv4 形态为
    /// `a1.a2.a3.a4.m1.m2.m3.m4`。部分固件只回地址、不回掩码，此时为空，
    /// 由 `net::gateway::plan_gateway` 回落到默认掩码。
    pub netmask4: String,
    pub dns: Vec<String>,
    /// 模组侧 `AT+CGCONTRDP` 字段位 4 上报的 IPv4 网关（未激活/未上报为空）。
    /// 下发链路在 auto 模式下优先采信该值，见 `net::gateway::plan`。
    pub gw4: String,
    /// 同字段位的 IPv6 网关（点分十进制已解码；当前仅透出展示，
    /// v6 默认路由仍由 RA / 设备路由方案接管）。
    pub gw6: String,
    /// 模组侧 `AT+CGCONTRDP` 上报的 IPv4 MTU（未上报 / 未激活为 None）。
    ///
    /// 运营商承载的 MTU 可能小于以太网默认 1500（如 1430/1460），直接用
    /// 1500 会在大包上踩 PMTU 黑洞。取到后由 `net::iface::apply_mtu` 写进
    /// 主接口；取不到就不动接口配置（不猜）。
    pub mtu: Option<u32>,
    /// `AT+CGCONTRDP` 字段 3 上报的**实际承载 APN**。
    ///
    /// 它可能和 `AT+CGDCONT?` 的配置值不同：IMS 承载抢占数据 cid 时，
    /// CGDCONT 仍显示数据 APN（如 `ctnet`），CGCONTRDP 却回
    /// `ims.MNC011.MCC460.GPRS`。判断数据链路是否被 IMS 占用必须看这个值。
    pub bearer_apn: String,
    /// 当前数据 cid 的激活承载是否为 IMS 类型。
    ///
    /// 判据：承载 APN 含独立 `ims` 段，或 `<IM_CN_Signalling_Flag>=1`
    /// （手册 §12.2.10）；CGDCONT 本身写成 IMS APN 也算。
    /// IMS 承载的地址不是数据链路地址，绝不配到主机网口。
    pub bearer_is_ims: bool,
    pub raw: Vec<(String, String)>,
}

impl PdpState {
    /// 是否拿到了**任一地址族**的地址。
    ///
    /// v6-only 也是合法会话，所以不能用「必须有 v4」当门槛。
    pub fn has_address(&self) -> bool {
        !self.ipv4.is_empty() || !self.ipv6.is_empty()
    }

    /// 是否是可以拿来配置主机网口的数据会话。
    ///
    /// IMS 承载即便有 IPv6 地址也不能算：它是 IM CN 子系统信令专用承载，
    /// 采信它会让守护反复把「IPv6-only 会话」写进静态接口。
    pub fn usable_for_data(&self) -> bool {
        self.active && self.has_address() && !self.bearer_is_ims
    }
}

/// IMS 类型 APN 判据。
///
/// `ims` 必须作为独立的点分段出现（手册 §12.2.10 的 `<apn>`、§12.2.16
/// 的 `<apn_type>` 取值之一），避免把 `ctimsnet` 这类数据 APN 误判成 IMS。
pub fn is_ims_apn(apn: &str) -> bool {
    let s = apn.trim().trim_matches('"').to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    s.split(|c: char| c == '.' || c == '-' || c == '_')
        .any(|part| part == "ims")
}

/// 从 `AT+CGCONTRDP` 响应里提取指定 cid 的承载 APN 与 IMS 判定。
///
/// 手册 §12.2.10 字段序：`<cid>,<bearer_id>,<apn>,<local_addr and
/// subnet_mask>,<gw_addr>,<DNS_prim>,<DNS_sec>,<P-CSCF_prim>,
/// <P-CSCF_sec>,<IM_CN_Signalling_Flag>,...`。索引 9 的标志为 1 表示
/// 「仅用于 IM CN 子系统信令」，是 IMS 承载的权威依据，与 APN 名互为兜底。
pub fn bearer_apn_info(resp: &str, cid: u32) -> (String, bool) {
    let mut apn = String::new();
    let mut ims = false;
    for row in crate::at::rows(resp, "+CGCONTRDP") {
        if row.first().and_then(|x| x.parse::<u32>().ok()) != Some(cid) {
            continue;
        }
        if let Some(a) = row.get(2) {
            if apn.is_empty() {
                apn = a.clone();
            }
            if is_ims_apn(a) {
                ims = true;
            }
        }
        if row.get(9).map(|x| x.trim() == "1").unwrap_or(false) {
            ims = true;
        }
    }
    (apn, ims)
}

/// 解析 `AT+CGACT?` 响应中指定 cid 的激活状态。
///
/// 返回 `None` 表示模组没回任何 `+CGACT:` 行（该命令在此模组上不可用，
/// 调用方需回落到地址类启发式判据）；返回 `Some(false)` 表示模组列出了上下文
/// 明细，但目标 cid 不存在或状态为 0 —— 这是**权威的未激活**结论。
pub fn cgact_state(resp: &str, cid: u32) -> Option<bool> {
    let mut seen = false;
    let mut active = false;
    for row in crate::at::rows(resp, "+CGACT") {
        seen = true;
        let is_cid = row.first().and_then(|x| x.parse::<u32>().ok()) == Some(cid);
        if is_cid {
            active = row.get(1).map(|x| x == "1").unwrap_or(false);
        }
    }
    if !seen {
        None
    } else {
        Some(active)
    }
}

/// `+CME ERROR` 是否为「重复激活」类（实机为 5847）。
///
/// 这类错误表示上下文**其实已经是激活的**，不该判失败 —— 调用方应重新
/// 读取上下文状态，以「是否拿到地址」为准。
pub fn is_duplicate_activate(resp: &str) -> bool {
    resp.contains("5847")
}

/// 解析 `AT+CGATT?` 的附着状态。
///
/// 返回 `None` 表示命令失败或没有 `+CGATT:` 行（调用方应维持旧行为，
/// 不把「读不到附着状态」升级成门槛）。
pub fn cgatt_state(resp: &str) -> Option<bool> {
    crate::at::first(resp, "+CGATT")
        .first()
        .and_then(|x| x.parse::<u32>().ok())
        .map(|v| v == 1)
}

/// 从一行 `+CGCONTRDP:` 字段里提取 IPv4 MTU。
///
/// TS 27.007 的 `<IPv4_MTU>` 排在尾部字段区，且本固件把标准字段表整体左移
/// （地址+掩码合并在字段 3），位置不固定。这里从字段 7（本方言 DNS2 之后）
/// 起扫描第一个落在合法 MTU 区间 [576, 65535] 的纯整数：
///   * 中间插着的 `<p_cscf_prim/sec>` 是带点的 IP，解析不出整数，天然跳过；
///   * `<IM_CN_Signalling_Flag>` / `<LIPA_ind>` 等标志位取值 0/1/2，被区间滤除；
///   * 去激活后的残留行里 MTU 位为空或 0，同样不命中 —— 不会把残留值配上去。
pub fn contrdp_mtu(row: &[String]) -> Option<u32> {
    row.iter()
        .skip(7)
        .filter_map(|x| x.parse::<u32>().ok())
        .find(|v| (576..=65535).contains(v))
}

/// 拨号前置体检结果。
///
/// 用途：拨号失败时给出**可读的原因**，而不是一句「激活 PDP 失败」。
/// 这是纯诊断，不改变拨号流程 —— 成功路径上一条额外命令都不发。
#[derive(Debug, Default, serde::Serialize)]
pub struct Readiness {
    /// SIM 是否就绪（`+CPIN: READY`）。
    pub sim_ready: Option<bool>,
    /// 模组功能等级（`+CFUN: 1` 为在线）。
    pub cfun: Option<u32>,
    /// EPS 注册状态（`+CEREG` 第 2 个字段：1 = 归属已注册，5 = 漫游已注册）。
    pub reg_state: Option<u32>,
    /// 逐条原始命令回显，便于排障。
    pub raw: Vec<(String, String)>,
}

impl Readiness {
    /// 是否具备拨号条件：SIM 就绪 + 在线 + 已注册。
    pub fn ok(&self) -> bool {
        matches!(self.sim_ready, Some(true))
            && matches!(self.cfun, Some(1))
            && matches!(self.reg_state, Some(1) | Some(5))
    }

    /// 失败原因的人类可读描述（全部就绪时为空串）。
    pub fn reason(&self) -> String {
        let mut why: Vec<String> = Vec::new();
        if !matches!(self.sim_ready, Some(true)) {
            why.push("SIM 未就绪".to_string());
        }
        if !matches!(self.cfun, Some(1)) {
            why.push(format!("模组未在线（CFUN={}）", fmt_opt(self.cfun)));
        }
        if !matches!(self.reg_state, Some(1) | Some(5)) {
            why.push(format!("网络未注册（CEREG={}）", fmt_opt(self.reg_state)));
        }
        why.join("，")
    }
}

/// Option 值的人类可读形式（None → "未知"，Some(v) → v），日志不带 Some() 包装。
fn fmt_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "未知".into(),
    }
}

/// 拨号前置体检：`CPIN?` / `CFUN?` / `CEREG?`。
pub fn readiness(at: &AtHandle, prio: AtPriority, cfg: &Config) -> Readiness {
    let mut r = Readiness::default();
    let cmds = [
        at_cmd::CPIN_READ,
        at_cmd::CFUN_READ,
        at_cmd::CEREG_READ,
    ];
    let out = super::run_list_as(at, prio, cfg, &cmds);

    for (cmd, resp) in &out {
        if cmd == at_cmd::CPIN_READ {
            r.sim_ready = Some(resp.contains("READY"));
        } else if cmd == at_cmd::CFUN_READ {
            r.cfun = crate::at::rows(resp, "+CFUN")
                .next()
                .and_then(|v| v.first().and_then(|x| x.parse().ok()));
        } else if cmd == at_cmd::CEREG_READ {
            let row = crate::at::rows(resp, "+CEREG").next().unwrap_or_default();
            r.reg_state = row.get(1).or_else(|| row.first()).and_then(|x| x.parse().ok());
        }
    }
    r.raw = out;
    r
}

/// 写 APN（`AT+CGDCONT=<cid>,"<type>","<apn>"`）。
pub fn set_apn(at: &AtHandle, prio: AtPriority, cfg: &Config, apn: &str, pdp_type: &str) -> AtResult<String> {
    if apn.is_empty() {
        return Err("APN 不能为空".to_string());
    }
    run_as(at, prio, cfg, &at_cmd::set_apn(cfg.cid, pdp_type, apn))
}

/// 下发鉴权参数（`AT+CGAUTH=<cid>,<auth>[,<username>,<password>]`）。
///
/// LuCI 页面一直提供 auth/username/password 三项配置，但此前从未真正
/// 下发到模组 —— 需要 PAP/CHAP 的 APN 会在 `AT+CGACT=1` 阶段因鉴权缺失
/// 而激活失败，用户只看到「拨号失败」无从排查。
/// `auth=none` 时显式清零，避免模组记住上一次会话的凭据。
/// 由调用方 best-effort 调用：个别固件对 `CGAUTH=0` 回 ERROR，
/// 不应因此阻断拨号主流程。
pub fn apply_auth(at: &AtHandle, prio: AtPriority, cfg: &Config) -> AtResult<String> {
    run_as(
        at,
        prio,
        cfg,
        &at_cmd::set_auth(cfg.cid, &cfg.auth, &cfg.username, &cfg.password),
    )
}

/// 当前 PDP 上下文状态。
///
/// 激活判据：
///   * 模组回了 `+CGACT:` 行 —— 一律以该行状态为准；
///   * 模组只回裸 `OK`（无 `+CGACT:` 行）—— 回落到
///     `AT+CGCONTRDP` / `AT+CGPADDR` 的地址类启发式判据。
pub fn pdp(at: &AtHandle, prio: AtPriority, cfg: &Config) -> PdpState {
    let mut st = PdpState {
        cid: cfg.cid,
        ..Default::default()
    };
    // 一次巡检只发四条查询，响应在解析的同时记入 raw。
    // 旧实现解析完再 run_list 重发同样四条，相当于每条 AT 命令都付两次
    // 往返；AT 是半双工串行资源，这直接翻倍了状态刷新耗时。
    let mut raw: Vec<(String, String)> = Vec::new();

    let cgdcont = record(&mut raw, at_cmd::CGDCONT_READ, run_as(at, prio, cfg, at_cmd::CGDCONT_READ));
    if let Some(r) = &cgdcont {
        for row in crate::at::rows(r, "+CGDCONT") {
            if row.first().and_then(|x| x.parse::<u32>().ok()) == Some(cfg.cid) {
                st.pdp_type = row.get(1).cloned().unwrap_or_default();
                st.apn = row.get(2).cloned().unwrap_or_default();
            }
        }
    }
    if st.apn.is_empty() {
        st.apn = cfg.apn.clone();
    }
    if st.pdp_type.is_empty() {
        st.pdp_type = cfg.pdp_type.clone();
    }

    let mut contr_dns: Vec<String> = Vec::new();

    // 判据 0（权威）：AT+CGACT?
    let cgact_resp = record(&mut raw, at_cmd::CGACT_READ, run_as(at, prio, cfg, at_cmd::CGACT_READ));
    let cgact = cgact_resp
        .as_ref()
        .and_then(|r| cgact_state(r, cfg.cid));

    // CGCONTRDP：只提供地址与 DNS 备用值，不再单独作为激活判据。
    let cmd_contr = at_cmd::contrdp(cfg.cid);
    let contr = record(&mut raw, &cmd_contr, run_as(at, prio, cfg, &cmd_contr));
    let mut contr_ipv4 = String::new();
    let mut contr_ipv6 = String::new();
    let mut contr_mask4 = String::new();
    let mut contr_gw4 = String::new();
    let mut contr_gw6 = String::new();
    let mut contr_mtu: Option<u32> = None;
    // 承载 APN / IMS 判定：先于地址解析算好，IMS 承载的地址随后一律不采信。
    let (bearer_apn, contr_ims) = contr
        .as_deref()
        .map(|r| bearer_apn_info(r, cfg.cid))
        .unwrap_or_default();
    if let Some(r) = &contr {
        for row in crate::at::rows(&r, "+CGCONTRDP") {
            // <cid>,<bearer>,"<apn>","<PDP_addr>","<gw>","<dns1>","<dns2>",...
            //
            // 手册 §12.2.10 的字段 3 是 `<local_addr and subnet_mask>`：
            // IPv4 为 "addr.mask" 八段拼接。把它拆开后，地址与掩码都能用，
            // 掩码优先于「同网段 .1 → /24」的猜测（见 plan_gateway）。
            if let Some(x) = row.get(3) {
                if let Some(p) = parse_contrdp_addr(x) {
                    if p.v6 {
                        if contr_ipv6.is_empty() {
                            contr_ipv6 = p.addr;
                        }
                    } else {
                        if contr_ipv4.is_empty() {
                            contr_ipv4 = p.addr;
                            // MTU 只从**有 v4 地址的那一行**取：v6 行的尾部
                            // 字段是另一套布局，混着扫会采错来源。
                            contr_mtu = contrdp_mtu(&row);
                        }
                        if contr_mask4.is_empty() {
                            if let Some(m) = p.mask {
                                contr_mask4 = m;
                            }
                        }
                    }
                }
            }
            // 字段位 4：实机字段表标注为 <gw>（模组方言；空串 = 未下发）。
            // auto 模式优先采信这里，而不是再靠「同网段 .1」猜。
            // 同一位置在 IPv6 会话下给点分十进制 IPv6 网关，一并解码到 gw6。
            if contr_gw4.is_empty() && contr_gw6.is_empty() {
                if let Some(x) = row.get(4) {
                    if is_valid_ipv4(x) {
                        contr_gw4 = x.clone();
                    } else if let Some(a6) = normalize_ipv6(x) {
                        contr_gw6 = a6;
                    }
                }
            }
            for item in row.iter().skip(5).take(2) {
                if is_valid_ipv4(item) {
                    contr_dns.push(item.clone());
                } else if contr_ipv6.is_empty() {
                    // 同一字段位在 IPV4V6 下按地址族切换：实机 cid=1 的
                    // <primary>/<secondary> 位给的就是点分十进制 IPv6。
                    if let Some(a6) = normalize_ipv6(item) {
                        contr_ipv6 = a6;
                    }
                }
            }
        }
    }

    // CGPADDR：地址的权威来源（本模组最可靠）。
    //
    // 同时记录「IPv6 字段位是否出现」：FM350 在网络未下发 IPv6 时把该位写成
    // `0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1`（::1 占位）。只要该位出现过，就以它
    // 为准 —— 是占位就判定「无 IPv6」，绝不回落到 CGCONTRDP 的残留值，
    // 否则会拿陈旧地址配出一条永远不通的 v6 默认路由。
    let cmd_addr = at_cmd::pdp_addr(cfg.cid);
    let addr_resp = record(&mut raw, &cmd_addr, run_as(at, prio, cfg, &cmd_addr));
    let mut addr_ipv4 = String::new();
    let mut addr_ipv6 = String::new();
    let mut v6_slot_seen = false;
    if let Some(r) = &addr_resp {
        // +CGPADDR: <cid>,<PDP_addr>[,<PDP_addr6>]
        let row = crate::at::first(r, "+CGPADDR");
        for item in row.iter().skip(1) {
            if looks_v6(item) {
                v6_slot_seen = true;
            }
            // 存归一化后的标准写法：FM350 的点分十进制形态在此被解码，
            // 前端拿到的是可读 IPv6，而不是 16 段十进制原始串。
            if let Some(a6) = normalize_ipv6(item) {
                if addr_ipv6.is_empty() {
                    addr_ipv6 = a6;
                }
            } else if is_valid_ipv4(item) {
                if addr_ipv4.is_empty() {
                    addr_ipv4 = item.clone();
                }
            }
        }
        if addr_ipv4.is_empty() {
            if let Some(x) = crate::at::first_ipv4(r) {
                if is_valid_ipv4(&x) {
                    addr_ipv4 = x;
                }
            }
        }
    }

    // 激活结论：CGACT? 可用时以它为准，否则回落到地址类启发式。
    //
    // 回落判据必须挑得极其小心。FM350 的 `AT+CGACT?` **只回 OK、不列 `+CGACT:`
    // 行**（模组方言），于是 `cgact` 恒为 `None` —— 这里不是罕见的异常兜底，
    // 而是 FM350 上的**常态执行路径**，判错就直接让拨号状态机走错分支：
    //
    //   * 不能用「CGCONTRDP 是否有返回行」（原判据 `contr_present`）：上下文
    //     去激活后该命令照样返回行，只是地址字段为空串。实机即如此 ——
    //     `+CGCONTRDP: 1,,"cmiot5g","","",...`，地址与网关位都是空，行却在；
    //   * 不能单独采信 `addr_ipv4`（来自 CGPADDR）：去激活后它原样返回上一轮
    //     地址（本模块顶部注释已记载）。
    //
    // 两者都会把「已去激活 + 残留死地址」误判为激活，`dial()` 随即在
    // 「已激活且已有地址」处短路、永不下发 `AT+CGACT=1,<cid>`。
    //
    // 只有 CGCONTRDP 的**地址字段**（`contr_ipv4` / `contr_ipv6`）在去激活后
    // 确实为空，可以作回落来源。判不出的最坏后果是多拨一次，方向是安全的。
    let active = cgact.unwrap_or(!contr_ipv4.is_empty() || !contr_ipv6.is_empty());

    // 未激活时的地址是上一轮会话的残留，一律不采信，避免上层把死地址写进接口。
    if active {
        st.ipv4 = if !addr_ipv4.is_empty() {
            addr_ipv4
        } else {
            contr_ipv4
        };
        st.ipv6 = addr_ipv6;
        if st.ipv6.is_empty() && !v6_slot_seen {
            // CGPADDR 没给过 IPv6 位（老固件）时才用 CGCONTRDP 兜底。
            st.ipv6 = contr_ipv6;
        }
        st.gw4 = contr_gw4;
        st.gw6 = contr_gw6;
        st.netmask4 = contr_mask4;
        st.mtu = contr_mtu;
    }
    st.active = active;

    // DNS：优先 AT+GTDNS，其次 CGCONTRDP 自带
    if st.active {
        if let Ok(r) = run_as(at, prio, cfg, &at_cmd::gtdns(cfg.cid)) {
            // 保留 IPv6 DNS：GTDNS 同时给 v4/v6 服务器，旧版把 v6 全部丢掉，
            // v6-only / 双栈场景下 resolver 拿不到任何 v6 DNS。
            // 点分占位（0.0....1）会被 normalize_ipv6 判为占位自动滤除。
            st.dns = crate::at::first(&r, "+GTDNS")
                .into_iter()
                .skip(1)
                .filter_map(|x| {
                    if is_valid_ipv4(&x) {
                        Some(x)
                    } else {
                        normalize_ipv6(&x)
                    }
                })
                .collect();
        }
        if st.dns.is_empty() {
            st.dns = contr_dns;
        }
    }

    // 数据链路 APN 守卫：IMS 类型承载不算数据链路。
    st.bearer_apn = bearer_apn;
    st.bearer_is_ims = contr_ims || is_ims_apn(&st.bearer_apn) || is_ims_apn(&st.apn);
    if st.bearer_is_ims {
        // IMS 承载通常只给 IPv6。采信它会让上层把它当成「IPv6-only 数据会话」
        // 反复写进静态接口，netifd 不停 up/down，而实际三层零回包。
        st.ipv4.clear();
        st.ipv6.clear();
        st.gw4.clear();
        st.gw6.clear();
        st.netmask4.clear();
        st.dns.clear();
        st.mtu = None;
    }

    st.raw = raw;
    st
}

/// 把一次查询的结果记入 raw 回显，同时把响应交给解析逻辑。
///
/// 失败时沿用 `run_list` 的 `ERR: ...` 形态，保证前端展示与旧版一致。
fn record(
    raw: &mut Vec<(String, String)>,
    cmd: &str,
    res: AtResult<String>,
) -> Option<String> {
    match res {
        Ok(r) => {
            raw.push((cmd.to_string(), r.clone()));
            Some(r)
        }
        Err(e) => {
            raw.push((cmd.to_string(), format!("ERR: {}", e)));
            None
        }
    }
}

/// 拨号结果：除了 PDP 状态，还带出失败时的诊断信息。
#[derive(Debug, serde::Serialize)]
pub struct DialOutcome {
    pub pdp: PdpState,
    /// 拨号失败时的前置体检（仅失败路径填充，成功路径为 None）。
    pub readiness: Option<Readiness>,
}

/// 拨号：确保 APN 正确 → 下发鉴权 → 激活 PDP → 读取地址。
///
/// ## 前置门槛（按序，任一不过即失败）
///
/// 1. **在线**：CFUN=4（飞行模式）时自恢复，限时等待注册；
/// 2. **EPS 附着**（`AT+CGATT?`）：未注册时附着不可能完成，直接失败——
///    旧版会继续下发 CGDCONT/CGAUTH/CGACT 再轮询 3 s 地址，整条链路在
///    「搜索中」弱网状态下每轮空转十几秒；已注册但附着未完成时补发一次
///    `AT+CGATT=1` 并限时等待；
/// 3. APN / 鉴权：每次拨号前重写（模组无持久化命令，见 `at::cmd` 模块注释）。
///
/// ## 激活判据与重试
///
/// 幂等：上下文已激活且已取得**任一地址族**的地址时直接返回，不重复下发
/// `AT+CGACT=1`（重复激活模组会回 `+CME ERROR: 5847`）。
///
/// 若模组回激活类错误，不再直接判失败，而是重新读取上下文状态：
/// 只要拿到有效地址即视为成功；确属真失败（非 5847）时做**一次**有界恢复
/// 重试（去激活 → 2 s → 再激活），连续失败交给守护的退避机制。
/// 彻底失败时才做一次前置体检，把「SIM 未就绪 / 未注册」这类真实原因写进
/// 错误信息。
pub fn dial(at: &AtHandle, cfg: &Config) -> AtResult<PdpState> {
    // 拨号事务整体 P1 High（用户主动操作 / 拨号控制，见分级文档 §3）。
    let prio = AtPriority::High;
    // 在线自恢复：模组重启（换 IMEI / usbmode / CFUN=1,1 等维修操作之后）
    // 或异常掉电后可能停留在飞行模式（CFUN=4）。此前只在拨号失败后报告、
    // 从不恢复 —— 守护每轮拨号都会死循环在「前置体检：模组未在线」。这里
    // 发现未在线时主动 CFUN=1 并限时等待注册，再继续后续拨号动作。
    let pre = readiness(at, prio, cfg);
    let mut reg_ok = matches!(pre.reg_state, Some(1) | Some(5));
    if pre.cfun == Some(4) {
        let _ = run_as(at, prio, cfg, &at_cmd::set_cfun(1));
        for _ in 0..RECOVER_REG_POLLS {
            std::thread::sleep(RECOVER_REG_POLL_INTERVAL);
            if let Ok(r) = run_as(at, prio, cfg, at_cmd::CEREG_READ) {
                let row = crate::at::rows(&r, "+CEREG").next().unwrap_or_default();
                if matches!(
                    row.get(1)
                        .or_else(|| row.first())
                        .and_then(|x| x.parse::<u32>().ok()),
                    Some(1) | Some(5)
                ) {
                    reg_ok = true;
                    break;
                }
            }
        }
    }

    // ---- EPS 附着门
    //
    // `attach == None`（命令失败/解析不出）不设门槛：维持旧行为，避免把
    // 「读不到附着状态」升级成拨号失败。
    let attach = run_as(at, prio, cfg, at_cmd::CGATT_READ)
        .ok()
        .and_then(|r| cgatt_state(&r));
    if attach == Some(false) {
        if !reg_ok {
            return Err(format!(
                "EPS 未附着且网络未注册（CEREG={}，CGATT=0），等待注册后再拨",
                fmt_opt(pre.reg_state)
            ));
        }
        // 已注册但附着未完成（弱网注册抖动窗口）：显式补发一次附着请求。
        crate::infof!(format_args!("已注册但 EPS 未附着，补发 AT+CGATT=1 并等待"));
        let _ = run_as(at, prio, cfg, &at_cmd::cgatt_attach(1));
        let mut attached = false;
        for _ in 0..ATTACH_POLLS {
            std::thread::sleep(ATTACH_POLL_INTERVAL);
            if let Ok(r) = run_as(at, prio, cfg, at_cmd::CGATT_READ) {
                if cgatt_state(&r) == Some(true) {
                    attached = true;
                    break;
                }
            }
        }
        if !attached {
            return Err(format!(
                "EPS 附着未在 {} 秒内完成（已注册，CGATT 仍为 0）",
                ATTACH_POLLS * ATTACH_POLL_INTERVAL.as_secs() as u32
            ));
        }
    }

    let _ = set_apn(at, prio, cfg, &cfg.apn, &cfg.pdp_type);
    // 鉴权必须在 CGACT 之前就位
    let _ = apply_auth(at, prio, cfg);

    // 已激活且已取得地址时直接返回，避免重复激活
    let mut before = pdp(at, prio, cfg);

    // 数据链路 APN 守卫：检测到当前为 IMS APN 时跳过它，重新下发正确的
    // 数据链路 APN 并重建 PDN。
    //
    // 实机（FM350-GL / 81600.0000.00.29.23.24 / 46011）现象：`AT+CGDCONT?`
    // 的 cid 1 是 `ctnet`，但 `AT+CGCONTRDP=1` 回报承载 APN 为
    // `ims.MNC011.MCC460.GPRS`，IPv4 位为空、只有一条 IPv6 —— IMS 承载
    // 抢占了数据 cid。CGDCONT 是持久化定义（手册 §12.2.1 Persistent=Yes），
    // 只重写定义不会改变已建承载，必须「去激活 → 等待释放 → 重新激活」，
    // 模组才会按数据 APN 重新建立 PDN。
    if before.bearer_is_ims {
        crate::infof!(format_args!(
            "数据 cid {} 被 IMS 承载占用（承载 APN={}），跳过并重新下发数据 APN {}",
            cfg.cid, before.bearer_apn, cfg.apn
        ));
        let _ = set_apn(at, prio, cfg, &cfg.apn, &cfg.pdp_type);
        let _ = apply_auth(at, prio, cfg);
        let _ = run_as(at, prio, cfg, &at_cmd::deactivate(cfg.cid));
        std::thread::sleep(Duration::from_secs(IMS_REBUILD_SETTLE_SECS));
        let _ = run_as(at, prio, cfg, &at_cmd::activate(cfg.cid));
        before = poll_for_address(at, prio, cfg);
        if before.bearer_is_ims {
            return Err(format!(
                "重新下发数据 APN {} 后激活承载仍为 IMS（{}），跳过该承载",
                cfg.apn, before.bearer_apn
            ));
        }
        if before.usable_for_data() {
            return Ok(before);
        }
    }

    if before.usable_for_data() {
        return Ok(before);
    }

    let r = run_as(at, prio, cfg, &at_cmd::activate(cfg.cid))?;
    if crate::at::has_error(&r) {
        // 重复激活（+CME ERROR: 5847）等：以「是否取到地址」为准，轮询若干次
        let dup = is_duplicate_activate(&r);
        let mut st = poll_for_address(at, prio, cfg);
        if st.active {
            return Ok(st);
        }
        // 真失败（非重复激活）：一次有界恢复重试。模组侧偶发的激活失败
        // （PDN 释放滞后、承载协商竞态）通常去激活再激活一轮即恢复；
        // 只试一次，连续失败交给守护的退避，不在单次拨号里反复折腾。
        if !dup {
            let _ = run_as(at, prio, cfg, &at_cmd::deactivate(cfg.cid));
            std::thread::sleep(Duration::from_secs(2));
            let r2 = run_as(at, prio, cfg, &at_cmd::activate(cfg.cid))?;
            if !crate::at::has_error(&r2) || is_duplicate_activate(&r2) {
                st = poll_for_address(at, prio, cfg);
                if st.active {
                    return Ok(st);
                }
            }
        }
        let mut msg = format!("激活 PDP 失败: {}", r.replace('\n', " "));
        if dup {
            msg.push_str("（模组报告重复激活，但上下文中未取到任何地址）");
        }
        // 失败时才做体检：成功路径上一条额外命令都不发
        let rd = readiness(at, prio, cfg);
        if !rd.ok() {
            msg.push_str(&format!("；前置体检：{}", rd.reason()));
        }
        return Err(msg);
    }

    // PDP 激活后地址可能晚一步就绪，轮询几次（任一地址族到位即可）
    let st = poll_for_address(at, prio, cfg);
    if !st.active && !st.has_address() {
        return Err("PDP 激活后未取得任何地址（IPv4/IPv6）".to_string());
    }
    Ok(st)
}

/// 轮询上下文直到拿到任一地址族的地址或次数用尽，返回最后一轮状态。
///
/// 是否激活以 `active` 字段为准：未激活时 CGPADDR/CGCONTRDP 给出的是
/// 上一轮会话残留，**不能**因为「有地址」就返回成功。
fn poll_for_address(at: &AtHandle, prio: AtPriority, cfg: &Config) -> PdpState {
    let mut st = pdp(at, prio, cfg);
    for _ in 0..ADDR_POLL_ROUNDS {
        if st.has_address() {
            return st;
        }
        std::thread::sleep(std::time::Duration::from_millis(ADDR_POLL_INTERVAL_MS));
        st = pdp(at, prio, cfg);
    }
    st
}

/// 断开：去激活 PDP（P1 High：用户主动挂断 / 净化动作）。
pub fn hangup(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run_as(at, AtPriority::High, cfg, &at_cmd::deactivate(cfg.cid))
}

#[cfg(test)]
mod ims_tests {
    use super::*;

    #[test]
    fn ims_apn_is_recognized_by_segment() {
        assert!(is_ims_apn("ims"));
        assert!(is_ims_apn("IMS"));
        assert!(is_ims_apn("ims.MNC011.MCC460.GPRS"));
        assert!(is_ims_apn("\"ims.mnc011.mcc460.gprs\""));
        assert!(!is_ims_apn("ctnet"));
        assert!(!is_ims_apn("3gnet"));
        assert!(!is_ims_apn("cmnet"));
        assert!(!is_ims_apn("ctimsnet"), "含 ims 子串的数据 APN 不得误判");
        assert!(!is_ims_apn(""));
    }

    /// 2026-11-04 实机样本：cgdcont=ctnet，实际承载却是 ims，且
    /// <IM_CN_Signalling_Flag>=1（索引 9）。
    #[test]
    fn contrdp_real_ims_bearer_is_detected() {
        let resp = concat!(
            "+CGCONTRDP: 1,6,\"ims.MNC011.MCC460.GPRS\",\"\",\"\",",
            "\"36.14.0.31.0.1.0.0.0.0.0.0.0.0.0.1\",",
            "\"36.14.0.31.0.1.0.0.0.0.0.0.0.0.0.51\",",
            "\"36.14.0.248.240.5.16.0.0.0.0.0.0.0.0.37\",",
            "\"36.14.1.135.194.128.4.0.0.0.0.0.0.0.0.116\",1,,0,,,,,,,,,,,,0\nOK"
        );
        let (apn, ims) = bearer_apn_info(resp, 1);
        assert!(apn.contains("ims"), "实际承载 APN = {}", apn);
        assert!(ims, "APN 名 / IM CN 信令标志应判为 IMS");
        let (_, other) = bearer_apn_info(resp, 2);
        assert!(!other, "其它 cid 不应被误判");
    }

    #[test]
    fn data_apn_bearer_is_not_ims() {
        let resp = concat!(
            "+CGCONTRDP: 1,5,\"ctnet\",\"10.1.2.3.255.255.255.0\",",
            "\"10.1.2.1\",\"202.96.128.86\",\"202.96.134.133\",",
            "\"\",\"\",0,,0\nOK"
        );
        let (apn, ims) = bearer_apn_info(resp, 1);
        assert_eq!(apn, "ctnet");
        assert!(!ims);
    }
}

#[cfg(test)]
mod cgact_tests {
    use super::{cgact_state, is_duplicate_activate};

    #[test]
    fn cid_absent_means_inactive() {
        // 实机故障现场：只有 cid 0 激活，插件用的 cid 1 根本不在列表里
        assert_eq!(cgact_state("+CGACT: 0,1\nOK", 1), Some(false));
    }

    #[test]
    fn cid_present_state_parsed() {
        assert_eq!(cgact_state("+CGACT: 0,1\n+CGACT: 1,1\nOK", 1), Some(true));
        assert_eq!(cgact_state("+CGACT: 1,0\nOK", 1), Some(false));
        assert_eq!(cgact_state("+CGACT: 0,1\nOK", 0), Some(true));
    }

    #[test]
    fn bare_ok_is_unknown() {
        // 部分 FM350 固件只回裸 OK，调用方需回落到地址类判据
        assert_eq!(cgact_state("OK", 1), None);
        assert_eq!(cgact_state("", 1), None);
        assert_eq!(cgact_state("+CME ERROR: unknown", 1), None);
    }

    #[test]
    fn duplicate_activate_is_recognized() {
        assert!(is_duplicate_activate("+CME ERROR: 5847"));
        assert!(!is_duplicate_activate("+CME ERROR: 100"));
        assert!(!is_duplicate_activate("OK"));
    }
}

#[cfg(test)]
mod cgatt_mtu_tests {
    use super::{cgatt_state, contrdp_mtu};

    #[test]
    fn cgatt_state_parses_real_device_output() {
        // 2026-11 实机（搜索中）：+CGATT: 0
        assert_eq!(cgatt_state("+CGATT: 0\nOK"), Some(false));
        assert_eq!(cgatt_state("+CGATT: 1\nOK"), Some(true));
    }

    #[test]
    fn cgatt_state_absent_is_none() {
        assert_eq!(cgatt_state("+CME ERROR: unknown"), None);
        assert_eq!(cgatt_state("OK"), None);
        assert_eq!(cgatt_state(""), None);
        // 非数值字段不解析
        assert_eq!(cgatt_state("+CGATT: \nOK"), None);
    }

    /// 2026-11 实机残留行（去激活态）：尾部只有 0 与空串，绝不能采出 MTU。
    const STALE_ROWS: &str = concat!(
        r#"+CGCONTRDP: 1,5,"ctnet.MNC011.MCC460.GPRS","","","202.96.128.86","202.96.134.133","","",0,,0,,,,,,,,,,,,0"#,
        "\n",
        r#"+CGCONTRDP: 1,5,"ctnet.MNC011.MCC460.GPRS","","","36.14.0.31.0.1.0.0.0.0.0.0.0.0.0.1","36.14.0.31.0.1.0.0.0.0.0.0.0.0.0.51","","",0,,0,,,,,,,,,,,,0"#,
    );

    fn parse_row(line: &str) -> Vec<String> {
        // 与 crate::at::rows 的字段切分一致：逗号分隔、去引号
        crate::at::rows(line, "+CGCONTRDP").next().unwrap()
    }

    #[test]
    fn stale_rows_yield_no_mtu() {
        for line in STALE_ROWS.lines() {
            let row = parse_row(line);
            assert_eq!(contrdp_mtu(&row), None, "{}", line);
        }
    }

    #[test]
    fn mtu_from_activated_row_is_found() {
        // 合成激活行：尾部 IPv4_MTU 位给 1500
        let row: Vec<String> = vec![
            "1", "5", "ctnet", "10.11.12.13.255.255.255.252", "10.11.12.14",
            "202.96.128.86", "202.96.134.133", "", "", "0", "", "1500",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(contrdp_mtu(&row), Some(1500));
    }

    #[test]
    fn small_flag_fields_and_ips_are_ignored() {
        let row: Vec<String> = vec![
            "1", "5", "apn", "a", "gw", "10.168.1.3", "10.168.1.4", "", "",
            "1", "", "2",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(contrdp_mtu(&row), None, "标志位与 IP 不得被当作 MTU");
        // 低于 576 的整数同样不采
        let row2: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, x)| if i == 11 { "575".to_string() } else { x.clone() })
            .collect();
        assert_eq!(contrdp_mtu(&row2), None);
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;

    #[test]
    fn not_ready_reports_all_missing_pieces() {
        let r = Readiness {
            sim_ready: Some(false),
            cfun: Some(0),
            reg_state: Some(2),
            raw: Vec::new(),
        };
        assert!(!r.ok());
        let why = r.reason();
        assert!(why.contains("SIM 未就绪"), "{}", why);
        assert!(why.contains("未在线"), "{}", why);
        assert!(why.contains("未注册"), "{}", why);
    }

    #[test]
    fn ready_when_registered_home_or_roaming() {
        for st in [1u32, 5] {
            let r = Readiness {
                sim_ready: Some(true),
                cfun: Some(1),
                reg_state: Some(st),
                raw: Vec::new(),
            };
            assert!(r.ok(), "reg={} 应判为就绪", st);
            assert_eq!(r.reason(), "");
        }
    }

    #[test]
    fn unknown_state_is_not_ready() {
        let r = Readiness::default();
        assert!(!r.ok());
    }
}
