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

use super::run;
use crate::addr::{is_valid_ipv4, looks_v6, normalize_ipv6};
use crate::at::{at_cmd, AtHandle, AtResult};
use crate::config::Config;

/// PDP 激活后地址可能晚一步就绪，这里轮询若干次（每次 600 ms）。
const ADDR_POLL_ROUNDS: u32 = 5;
const ADDR_POLL_INTERVAL_MS: u64 = 600;

#[derive(Debug, Default, serde::Serialize)]
pub struct PdpState {
    pub cid: u32,
    pub apn: String,
    pub pdp_type: String,
    pub active: bool,
    pub ipv4: String,
    pub ipv6: String,
    pub dns: Vec<String>,
    /// 模组侧 `AT+CGCONTRDP` 字段位 4 上报的 IPv4 网关（未激活/未上报为空）。
    /// 下发链路在 auto 模式下优先采信该值，见 `net::gateway::plan`。
    pub gw4: String,
    /// 同字段位的 IPv6 网关（点分十进制已解码；当前仅透出展示，
    /// v6 默认路由仍由 RA / 设备路由方案接管）。
    pub gw6: String,
    pub raw: Vec<(String, String)>,
}

impl PdpState {
    /// 是否拿到了**任一地址族**的地址。
    ///
    /// v6-only 也是合法会话，所以不能用「必须有 v4」当门槛。
    pub fn has_address(&self) -> bool {
        !self.ipv4.is_empty() || !self.ipv6.is_empty()
    }
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
            why.push(format!("模组未在线（CFUN={:?}）", self.cfun));
        }
        if !matches!(self.reg_state, Some(1) | Some(5)) {
            why.push(format!("网络未注册（CEREG={:?}）", self.reg_state));
        }
        why.join("，")
    }
}

/// 拨号前置体检：`CPIN?` / `CFUN?` / `CEREG?`。
pub fn readiness(at: &AtHandle, cfg: &Config) -> Readiness {
    let mut r = Readiness::default();
    let cmds = [
        at_cmd::CPIN_READ,
        at_cmd::CFUN_READ,
        at_cmd::CEREG_READ,
    ];
    let out = super::run_list(at, cfg, &cmds);

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
pub fn set_apn(at: &AtHandle, cfg: &Config, apn: &str, pdp_type: &str) -> AtResult<String> {
    if apn.is_empty() {
        return Err("APN 不能为空".to_string());
    }
    run(at, cfg, &at_cmd::set_apn(cfg.cid, pdp_type, apn))
}

/// 下发鉴权参数（`AT+CGAUTH=<cid>,<auth>[,<username>,<password>]`）。
///
/// LuCI 页面一直提供 auth/username/password 三项配置，但此前从未真正
/// 下发到模组 —— 需要 PAP/CHAP 的 APN 会在 `AT+CGACT=1` 阶段因鉴权缺失
/// 而激活失败，用户只看到「拨号失败」无从排查。
/// `auth=none` 时显式清零，避免模组记住上一次会话的凭据。
/// 由调用方 best-effort 调用：个别固件对 `CGAUTH=0` 回 ERROR，
/// 不应因此阻断拨号主流程。
pub fn apply_auth(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(
        at,
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
pub fn pdp(at: &AtHandle, cfg: &Config) -> PdpState {
    let mut st = PdpState {
        cid: cfg.cid,
        ..Default::default()
    };

    if let Ok(r) = run(at, cfg, at_cmd::CGDCONT_READ) {
        for row in crate::at::rows(&r, "+CGDCONT") {
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
    let cgact = run(at, cfg, at_cmd::CGACT_READ)
        .ok()
        .and_then(|r| cgact_state(&r, cfg.cid));

    // CGCONTRDP：只提供地址与 DNS 备用值，不再单独作为激活判据。
    let mut contr_present = false;
    let mut contr_ipv4 = String::new();
    let mut contr_ipv6 = String::new();
    let mut contr_gw4 = String::new();
    let mut contr_gw6 = String::new();
    if let Ok(r) = run(at, cfg, &at_cmd::contrdp(cfg.cid)) {
        for row in crate::at::rows(&r, "+CGCONTRDP") {
            contr_present = true;
            // <cid>,<bearer>,"<apn>","<PDP_addr>","<gw>","<dns1>","<dns2>",...
            if contr_ipv4.is_empty() {
                if let Some(x) = row.get(3) {
                    if is_valid_ipv4(x) {
                        contr_ipv4 = x.clone();
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
    let mut addr_ipv4 = String::new();
    let mut addr_ipv6 = String::new();
    let mut v6_slot_seen = false;
    if let Ok(r) = run(at, cfg, &at_cmd::pdp_addr(cfg.cid)) {
        // +CGPADDR: <cid>,<PDP_addr>[,<PDP_addr6>]
        let row = crate::at::first(&r, "+CGPADDR");
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
            if let Some(x) = crate::at::first_ipv4(&r) {
                if is_valid_ipv4(&x) {
                    addr_ipv4 = x;
                }
            }
        }
    }

    // 激活结论：CGACT? 可用时以它为准，否则回落到地址类启发式。
    let active = cgact.unwrap_or(contr_present || !addr_ipv4.is_empty() || !addr_ipv6.is_empty());

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
    }
    st.active = active;

    // DNS：优先 AT+GTDNS，其次 CGCONTRDP 自带
    if st.active {
        if let Ok(r) = run(at, cfg, &at_cmd::gtdns(cfg.cid)) {
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

    st.raw = super::run_list(
        at,
        cfg,
        &[
            at_cmd::CGDCONT_READ,
            at_cmd::CGACT_READ,
            &at_cmd::contrdp(cfg.cid),
            &at_cmd::pdp_addr(cfg.cid),
        ],
    );
    st
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
/// 幂等：上下文已激活且已取得**任一地址族**的地址时直接返回，不重复下发
/// `AT+CGACT=1`（重复激活模组会回 `+CME ERROR: 5847`）。
///
/// 若模组回激活类错误，不再直接判失败，而是重新读取上下文状态：
/// 只要拿到有效地址即视为成功；彻底失败时才做一次前置体检，把
/// 「SIM 未就绪 / 未注册」这类真实原因写进错误信息。
pub fn dial(at: &AtHandle, cfg: &Config) -> AtResult<PdpState> {
    let _ = set_apn(at, cfg, &cfg.apn, &cfg.pdp_type);
    // 鉴权必须在 CGACT 之前就位
    let _ = apply_auth(at, cfg);

    // 已激活且已取得地址时直接返回，避免重复激活
    let before = pdp(at, cfg);
    if before.active && before.has_address() {
        return Ok(before);
    }

    let r = run(at, cfg, &at_cmd::activate(cfg.cid))?;
    if crate::at::has_error(&r) {
        // 重复激活（+CME ERROR: 5847）等：以「是否取到地址」为准，轮询若干次
        let dup = is_duplicate_activate(&r);
        let mut st = pdp(at, cfg);
        for _ in 0..ADDR_POLL_ROUNDS {
            if st.has_address() {
                return Ok(st);
            }
            std::thread::sleep(std::time::Duration::from_millis(ADDR_POLL_INTERVAL_MS));
            st = pdp(at, cfg);
        }
        if st.active {
            return Ok(st);
        }
        let mut msg = format!("激活 PDP 失败: {}", r.replace('\n', " "));
        if dup {
            msg.push_str("（模组报告重复激活，但上下文中未取到任何地址）");
        }
        // 失败时才做体检：成功路径上一条额外命令都不发
        let rd = readiness(at, cfg);
        if !rd.ok() {
            msg.push_str(&format!("；前置体检：{}", rd.reason()));
        }
        return Err(msg);
    }

    // PDP 激活后地址可能晚一步就绪，轮询几次（任一地址族到位即可）
    let mut st = pdp(at, cfg);
    for _ in 0..ADDR_POLL_ROUNDS {
        if st.has_address() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(ADDR_POLL_INTERVAL_MS));
        st = pdp(at, cfg);
    }
    if !st.active && !st.has_address() {
        return Err("PDP 激活后未取得任何地址（IPv4/IPv6）".to_string());
    }
    Ok(st)
}

/// 断开：去激活 PDP。
pub fn hangup(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(at, cfg, &at_cmd::deactivate(cfg.cid))
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
