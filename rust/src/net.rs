//! 网络接口管理与路由守护。
//!
//! FM350 的 RNDIS 数据通道**不提供 DHCP**，IPv4 必须以静态地址配置：
//!   - 地址取自 `AT+CGPADDR`；
//!   - DNS 取自 `AT+GTDNS`；
//!   - 掩码与网关由 `gateway_mode` 决定（默认 `auto`）：
//!     * `auto`：优先采信模组 `AT+CGCONTRDP` 上报的网关，模组未报时才按
//!       同网段 `.1` 推导；再**实测该网关的 ARP 是否可解析**（实测放在
//!       地址落进内核之后的 [`apply_after_dial`]），可解析就按
//!       `/24 + gateway` 配置，由 netifd 下发 `default via <gw>`；不可解析
//!       则回退到无网关方案。网关不在地址掩码内时另装 `<gw>/32` 主机路由。
//!     * `off`：历史行为 —— `/32` + `default dev <dev> metric <m> onlink`。
//!     * `static`：直接使用 `gateway` 选项。
//!
//! 为什么要引入网关：无网关的 onlink 方案下，主机对**每一个公网 IP**都要
//! 直接发 ARP 请求，完全依赖模组做 ARP 代理。部分运营商/固件下模组只对
//! 自己的网关 IP 应答 ARP，于是表现为「PDP 已激活、有 IP 有 DNS，却一个包
//! 都发不出去」（tx_errors 持续上涨、rx 恒为 0、内核刷 NETDEV WATCHDOG）。
//!
//! IPv6 与 IPv4 同构：地址取自模组侧 `AT+CGPADDR`（守护读出后静态写入），
//! 默认路由用无网关的设备路由（`default dev <dev>`）并由 route_guard 周期补齐。
//!
//! 为什么不走 dhcpv6（odhcp6c）：FM350 的 RNDIS 数据通道与 IPv4 一样**不转发
//! 运营商的 RA/DHCPv6**（实机 tcpdump 无任何 ICMPv6 RS/RA 往来），odhcp6c 在该
//! 网卡上永远等不到应答，netifd 表现为 fm350v6 每秒 down/up 循环（实机日志复现）。
//! 因此 IPv6 完全由本插件后端自行实现，不依赖任何外部 IPv6 客户端。
//!
//! 接口、路由与防火墙区段均由本插件独立创建与管理。

use std::fs;
use std::process::Command;
use std::time::Duration;

use crate::config::Config;

/// 可能的 RNDIS/ECM 数据通道驱动。
const DATA_DRIVERS: &[&str] = &["rndis_host", "cdc_ether", "cdc_ncm", "qmi_wwan", "mbim"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// 只回显命令，不执行（用于自测）。
    Dry,
    Real,
}

fn sh(mode: RunMode, script: &str) -> (bool, String) {
    if mode == RunMode::Dry {
        return (true, script.to_string());
    }
    match Command::new("sh").arg("-c").arg(script).output() {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
        ),
        Err(e) => (false, e.to_string()),
    }
}

fn real(script: &str) -> (bool, String) {
    sh(RunMode::Real, script)
}

/// 单引号包裹 shell 参数。
///
/// 为什么需要它：`uci` 的值里一旦含空格（典型是多值 dns "A B"），
/// 未加引号经 `sh -c` 会被拆成两个参数，uci 直接报错退出（实机 rc=255），
/// 进而让整批 UCI 写入被判失败、`ifup` 被短路 —— 接口就再也起不来。
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------- 网关决策

/// 判断 IPv4 是否落在「可安全推导同网段网关」的地址段。
///
/// 只覆盖私有地址与运营商 CGNAT 段（10/8、172.16/12、192.168/16、100.64/10）。
/// 公网地址不做推导：蜂窝网络下公网 IP 的网关极少是同网段 `.1`，盲目推导
/// 会写入一条错误网关，反而把原本可用的 onlink 直连彻底堵死。
fn is_private_or_cgnat(ip: &str) -> bool {
    let oct: Vec<u8> = ip.split('.').filter_map(|p| p.parse().ok()).collect();
    if oct.len() != 4 {
        return false;
    }
    matches!(
        (oct[0], oct[1]),
        (10, _) | (172, 16..=31) | (192, 168) | (100, 64..=127)
    )
}

/// 由主机侧 IPv4 推导同网段 `.1` 网关（仅对私有 / CGNAT 段）。
pub fn derive_gateway(ipv4: &str) -> Option<String> {
    if !is_private_or_cgnat(ipv4) {
        return None;
    }
    let mut parts: Vec<&str> = ipv4.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    *parts.last_mut()? = "1";
    Some(parts.join("."))
}

fn mask_or(cfg: &Config, dflt: &str) -> String {
    if cfg.netmask.is_empty() {
        dflt.to_string()
    } else {
        cfg.netmask.clone()
    }
}

/// 模组上报网关的采信条件：本身是合法 IPv4，且不等于本机地址
/// （CGCONTRDP 偶发回 `0.0.0.0` 或与 PDP_addr 相同的占位值）。
fn valid_modem_gw(modem_gw: &str, ipv4: &str) -> Option<String> {
    if crate::modem::is_valid_ipv4(modem_gw) && modem_gw != ipv4 {
        Some(modem_gw.to_string())
    } else {
        None
    }
}

/// 按掩码判断 `gw` 是否与 `ip` 同网段。
///
/// 任一侧解析失败（含掩码非法）都按「不同网段」处理 —— 调用方随后走更
/// 安全的 `<gw>/32` 主机路由路径，宁可多装一条主机路由也不能让
/// netifd 的 `default via <gw>` 被内核以「网关不可达」拒绝。
fn in_same_subnet(ip: &str, gw: &str, mask: &str) -> bool {
    let segs4 = |s: &str| -> bool { s.split('.').count() == 4 };
    if !segs4(ip) || !segs4(gw) || !segs4(mask) {
        return false;
    }
    let parse = |s: &str| -> Option<u32> {
        let mut v = 0u32;
        for part in s.split('.') {
            if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            v = (v << 8) | part.parse::<u32>().ok()?;
        }
        Some(v)
    };
    match (parse(ip), parse(gw), parse(mask)) {
        (Some(a), Some(b), Some(m)) => (a & m) == (b & m),
        _ => false,
    }
}

/// 规划本轮要使用的掩码与网关。返回 `(netmask, Option<gateway>)`；
/// `None` 表示沿用无网关的 onlink 设备路由（历史行为）。
///
/// `modem_gw` 是模组侧 `AT+CGCONTRDP` 字段位 4 上报的网关（可为空）：
/// auto 模式**优先采信模组上报值**，仅在模组未报时才回退到「同网段 .1」
/// 启发式推导 —— 运营商真实网关并不总是 `.1`（审查问题 #5）。模组上报
/// 网关与地址不同网段时掩码仍按默认 /24 规划，由 [`ensure_iface`] 另装
/// `<gw>/32` 主机路由保证网关可达。
///
/// 本函数**不做 ARP 实测**：实测（[`probe_gateway`]）必须等地址落进内核
/// 后才能进行，已移至 [`apply_after_dial`]，见审查问题 #5。
pub fn plan_gateway(cfg: &Config, ipv4: &str, modem_gw: &str) -> (String, Option<String>) {
    match cfg.gateway_mode.as_str() {
        "off" => (mask_or(cfg, "255.255.255.255"), None),
        "static" => {
            if cfg.gateway.is_empty() {
                (mask_or(cfg, "255.255.255.255"), None)
            } else {
                (mask_or(cfg, "255.255.255.0"), Some(cfg.gateway.clone()))
            }
        }
        // auto（默认）
        _ => {
            let candidate = valid_modem_gw(modem_gw, ipv4).or_else(|| derive_gateway(ipv4));
            match candidate {
                Some(gw) => (mask_or(cfg, "255.255.255.0"), Some(gw)),
                None => (mask_or(cfg, "255.255.255.255"), None),
            }
        }
    }
}

/// 探测网关在二层是否可用 —— **只判 ARP，不判 ICMP**。
///
/// 为什么不能用 ping 的返回码：蜂窝网关普遍不回应 ICMP。实机复现为
/// `ping 10.8.217.1` 100% 丢包，而同链路 `ping 223.5.5.5` 正常（21 ms）——
/// 只要 ARP 能解析到网关 MAC，三层转发就是好的。
pub fn probe_gateway(dev: &str, gw: &str) -> bool {
    // 先发一个包触发 ARP 解析（ICMP 无应答无妨）
    let _ = real(&format!(
        "ping -c 1 -W 2 -I {} {} >/dev/null 2>&1",
        dev, gw
    ));
    let (ok, out) = real(&format!("ip neigh show dev {} {} 2>/dev/null", dev, gw));
    if !ok {
        return false;
    }
    // FAILED 表示 ARP 无应答；REACHABLE / STALE / DELAY / PROBE 都算解析成功。
    out.split_whitespace()
        .any(|t| matches!(t, "REACHABLE" | "STALE" | "DELAY" | "PROBE"))
}

/// 读取 uci 里当前生效的网关（供 route_guard 补路由时使用）。
pub fn current_gateway(cfg: &Config) -> Option<String> {
    let (ok, v) = real(&format!("uci -q get network.{}.gateway", cfg.iface));
    if ok {
        let g = v.trim().to_string();
        if !g.is_empty() {
            return Some(g);
        }
    }
    None
}

/// 影响 netifd 运行态的网络参数签名。
///
/// 裸 `uci set`（CLI `fm350d set`、ubus ucode、外部脚本）只写 UCI delta，
/// 不会触发 netifd 重读（`config.change` 只由 rpcd/LuCI 应用路径发出，
/// 见 OpenWrt ticket #17305）；网络包也不是 procd 管的服务，本插件的
/// procd reload 触发管不到它。把关键参数做成签名、与「最近一次成功下发
/// 时的快照」比对，发现不一致且接口上有地址时强制重新下发（审查问题 #13）。
pub fn config_sig(cfg: &Config) -> String {
    format!(
        "metric={}|gw_mode={}|gw={}|mask={}|dev={}|ipv6={}|v6_mode={}|iface={}|iface_v6={}",
        cfg.metric,
        cfg.gateway_mode,
        cfg.gateway,
        cfg.netmask,
        cfg.data_dev,
        cfg.ipv6,
        cfg.v6_mode,
        cfg.iface,
        cfg.iface_v6
    )
}

const APPLIED_SIG_FILE: &str = "/var/run/fm350d.applied_sig";

/// 最近一次成功下发时的参数签名（/var/run，重启即清）。
pub fn applied_sig() -> Option<String> {
    fs::read_to_string(APPLIED_SIG_FILE)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 记录当前参数签名（仅由成功下发路径调用）。
pub fn mark_applied(cfg: &Config) {
    let _ = fs::write(APPLIED_SIG_FILE, config_sig(cfg));
}

/// 清除签名（拆除接口时调用，强制下次带地址巡检重新下发）。
pub fn clear_applied() {
    let _ = fs::remove_file(APPLIED_SIG_FILE);
}

/// 读取 UCI 里接口当前的 dns 列表（供参数重下发时原样保留）。
pub fn configured_dns(cfg: &Config) -> Vec<String> {
    let (ok, out) = real(&format!("uci -q get network.{}.dns", cfg.iface));
    if ok {
        out.split_whitespace().map(|s| s.to_string()).collect()
    } else {
        Vec::new()
    }
}

/// 定位 wan 防火墙区的下标。
///
/// 原先硬编码 `firewall.@zone[0]`，但 `@zone[N]` 是**按配置文件出现顺序**取的，
/// 并非一定是 wan 区（实机 192.168.10.1 的 `@zone[0]` 就是 lan 区，
/// wan 区在 `@zone[1]`）。把蜂窝接口登记进 lan 区会让 fw4 把它并入 LAN 规则
/// （input/forward 全 ACCEPT），既拿不到 masq 又与 wan 区配置自相矛盾。
///
/// 优先匹配 `name='wan'`；少数配置没有 name，则以 network 列表含 `wan`/`wan6` 兜底。
fn wan_zone_index() -> Option<usize> {
    let (ok, out) = real("uci show firewall 2>/dev/null | grep -c '=zone$'");
    if !ok {
        return None;
    }
    let n: usize = out.trim().parse().unwrap_or(0);
    let mut fallback: Option<usize> = None;
    for i in 0..n {
        let (ok_name, name) = real(&format!("uci -q get firewall.@zone[{}].name", i));
        if ok_name && name.trim() == "wan" {
            return Some(i);
        }
        if fallback.is_none() {
            let (ok_nets, nets) = real(&format!("uci -q get firewall.@zone[{}].network", i));
            if ok_nets && nets.split_whitespace().any(|x| x == "wan" || x == "wan6") {
                fallback = Some(i);
            }
        }
    }
    fallback
}

/// 判断某个 uci 列表是否已包含指定项（用于让 add_list 幂等）。
fn uci_list_contains(key: &str, item: &str) -> bool {
    let (ok, out) = real(&format!("uci -q get {}", key));
    ok && out.split_whitespace().any(|x| x == item)
}

/// 读取网卡对应 USB 设备的 idVendor（sysfs 逐级向上）。
fn dev_usb_vid(name: &str) -> Option<String> {
    crate::at::find_usb_attr(
        &std::path::PathBuf::from(format!("/sys/class/net/{}/device", name)),
        "idVendor",
    )
}

/// 探测数据通道网卡名：优先读驱动，其次按名称兜底。
///
/// `data_dev=auto` 下可能同时命中多张 cdc_* 网卡（同机挂了别的 CDC 设备，
/// 或同模组枚举出多个网络功能）：先按名字排序保证选择可复现，再优先挑
/// USB VID 为 Fibocom(2cb7) 的那张（审查问题 #8）。
pub fn detect_dev(cfg: &Config) -> Option<String> {
    if cfg.data_dev != "auto" && !cfg.data_dev.is_empty() {
        return Some(cfg.data_dev.clone());
    }
    let entries = fs::read_dir("/sys/class/net").ok()?;
    let mut candidates: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let driver = fs::read_link(entry.path().join("device/driver"))
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_default();
        if DATA_DRIVERS.iter().any(|d| driver.contains(d)) {
            candidates.push(name);
        }
    }
    if !candidates.is_empty() {
        candidates.sort(); // readdir 顺序不稳定，排序保证同构环境行为可复现
        if let Some(fib) = candidates
            .iter()
            .find(|n| dev_usb_vid(n).as_deref() == Some("2cb7"))
        {
            return Some(fib.clone());
        }
        return Some(candidates.remove(0));
    }
    // 兜底：按常见名字猜测
    for name in ["eth2", "eth3", "wwan0", "usb0"] {
        if fs::metadata(format!("/sys/class/net/{}", name)).is_ok() {
            return Some(name.to_string());
        }
    }
    None
}

/// UCI 写入辅助。
fn uci(script: &str) -> (bool, String) {
    real(&format!("uci -q {}", script))
}

fn uci_batch(lines: &[String]) -> Vec<(String, bool, String)> {
    lines
        .iter()
        .map(|l| (l.clone(), uci(l).0, "".to_string()))
        .collect()
}

/// 确保蜂窝接口存在并应用地址。
///
/// 返回（接口名, 数据网卡, 已执行的 UCI 命令列表）。
///
/// `modem_gw`：模组侧 `AT+CGCONTRDP` 上报的 IPv4 网关（可空），auto 模式
/// 下由 [`plan_gateway`] 优先采信。网关的 ARP 实测**不在这里做** ——
/// ifup 是异步的，此时地址多半还没落进内核，500 ms 固定等待 + 必失败的
/// 探测会把会话误判为「网关不可达」并回退到 onlink 方案（审查问题 #5）；
/// 实测已移至 [`apply_after_dial`] 的地址轮询之后。
pub fn ensure_iface(
    cfg: &Config,
    ipv4: &str,
    ipv6: &str,
    dns: &[String],
    modem_gw: &str,
) -> Result<(String, String, Vec<String>), String> {
    let dev = detect_dev(cfg).ok_or_else(|| "未探测到数据通道网卡".to_string())?;
    let iface = &cfg.iface;
    let iface_v6 = &cfg.iface_v6;
    let route_name = format!("{}_def", iface);
    let gwr_name = format!("{}_gw4", iface);
    let have_v4 = !ipv4.is_empty();
    // 只有确实要下发 IPv4 时才做网关规划；无 v4（IPv6-only 会话）时不触碰
    // 网关计划，改为清理会制造黑洞的 v4 残留（ipaddr/netmask/gateway/_def）。
    let (plan_mask, plan_gw) = if have_v4 {
        plan_gateway(cfg, ipv4, modem_gw)
    } else {
        (String::new(), None)
    };
    let mask = plan_mask.as_str();
    // v6_mode=off 时既不创建也不登记 fm350v6（审查问题：off 只是不刷新，
    // 接口骨架与防火墙登记会一直残留）。
    let v6_section = cfg.ipv6 && !iface_v6.is_empty() && v6_managed(cfg);

    let mut cmds: Vec<String> = Vec::new();
    // 不走 uci_batch（它是 `uci -q <子命令>` 形式）的完整 shell 命令，
    // 用于删除类操作 —— 删除不存在的项会返回非零，不能计入失败判定。
    let mut shell_cmds: Vec<String> = Vec::new();

    // ---- 主接口骨架（v4/v6 共用）
    cmds.push(format!("set network.{}=interface", iface));
    cmds.push(format!("set network.{}.proto=static", iface));
    cmds.push(format!("set network.{}.device={}", iface, dev));
    cmds.push(format!("set network.{}.peerdns=0", iface));
    // 开机默认启用（显式 auto=1）：netifd 对缺省 auto 的接口**不会**开机自启，
    // 实机 ifstatus 即为 "autostart": false，导致重启后蜂窝接口要等守护轮询才起来。
    cmds.push(format!("set network.{}.auto=1", iface));
    cmds.push(format!("set network.{}.metric={}", iface, cfg.metric));
    cmds.push(format!("set network.{}.defaultroute=1", iface));

    if have_v4 {
        cmds.push(format!("set network.{}.ipaddr={}", iface, ipv4));
        cmds.push(format!("set network.{}.netmask={}", iface, mask));
        // 网关：plan_gw 为 None 表示本轮沿用无网关的 onlink 设备路由。
        // 无网关时必须显式删掉上一轮可能写进去的 gateway，否则 netifd 会拿
        // 一个已失效的网关去下发路由（表现为接口起来了却没有任何默认路由）。
        match &plan_gw {
            Some(gw) => cmds.push(format!("set network.{}.gateway={}", iface, gw)),
            None => shell_cmds.push(format!(
                "uci -q delete network.{}.gateway 2>/dev/null || true",
                iface
            )),
        }
    } else {
        // 无 v4（IPv6-only 会话）：清掉上一会话的 v4 地址/掩码/网关与默认
        // 路由残留 —— 留着会把死地址重新 ifup 进内核（审查问题 #1）。
        for opt in ["ipaddr", "netmask", "gateway"] {
            shell_cmds.push(format!(
                "uci -q delete network.{}.{} 2>/dev/null || true",
                iface, opt
            ));
        }
        shell_cmds.push(format!(
            "uci -q delete network.{} 2>/dev/null || true",
            route_name
        ));
        shell_cmds.push(format!(
            "uci -q delete network.{} 2>/dev/null || true",
            gwr_name
        ));
    }

    // DNS：有则覆盖，无则清空（换 APN 后旧 DNS 不得残留；审查问题 #9）。
    if !dns.is_empty() {
        // 必须整体加引号：多值 dns 含空格，裸写会被 `sh -c` 拆成两个参数，
        // uci 直接 rc=255，整批写入判失败，ifup 被短路 —— 接口从此再也起不来。
        cmds.push(format!("set network.{}.dns={}", iface, sq(&dns.join(" "))));
    } else {
        shell_cmds.push(format!(
            "uci -q delete network.{}.dns 2>/dev/null || true",
            iface
        ));
    }

    // ---- IPv6（独立子接口，附着在主接口设备上）
    //
    // 地址由守护从模组侧（AT+CGPADDR / AT+CGCONTRDP）读出后静态写入，
    // 不走 dhcpv6/odhcp6c —— RNDIS 通道不转发 RA/DHCPv6，见模块头注释。
    // 掩码固定 /128，与 IPv4 的 /32 对称：不产生直连路由，默认路由走设备路由。
    if v6_section {
        cmds.push(format!("set network.{}=interface", iface_v6));
        cmds.push(format!("set network.{}.device=@{}", iface_v6, iface));
        // 清理 dhcpv6 时代的遗留选项：proto 已切 static/none，留着既无意义也会误导。
        // delete 必须带 `|| true`：uci 对不存在的选项即使 -q 也返回非零，
        // 会让整批写入被误判失败（实机踩过）。
        for stale in ["reqaddress", "reqprefix", "peerdns", "extendprefix"] {
            cmds.push(format!(
                "uci -q delete network.{}.{} 2>/dev/null || true",
                iface_v6, stale
            ));
        }
        if v6_dhcp_mode(cfg) {
            // DHCPv6 模式：交给 odhcp6c（不写静态 /128，也不碰 sysctl）。
            // extendprefix=1 让 odhcp6c 拿到的 /64 能委派给 LAN，内网设备
            // 也能用上 IPv6 —— 主流第三方插件就是这个做法，实机已验证。
            cmds.push(format!("set network.{}.proto=dhcpv6", iface_v6));
            cmds.push(format!("set network.{}.extendprefix=1", iface_v6));
            shell_cmds.push(format!(
                "uci -q delete network.{}.ip6addr 2>/dev/null || true",
                iface_v6
            ));
        } else if v6_ra_mode(cfg) {
            // RA 模式：只打开 accept_ra，绝不写静态 /128 —— 模组侧
            // `AT+CGCONTRDP` 在上下文去激活后仍返回上一轮的残留地址，照抄会
            // 配出一个根本不属于本会话的 v6 地址。
            cmds.push(format!("set network.{}.proto=none", iface_v6));
            shell_cmds.push(format!(
                "uci -q delete network.{}.ip6addr 2>/dev/null || true",
                iface_v6
            ));
            shell_cmds.push(format!(
                "sysctl -w net.ipv6.conf.{}.accept_ra=2 >/dev/null 2>&1 || true", dev
            ));
            shell_cmds.push(format!(
                "sysctl -w net.ipv6.conf.{}.accept_ra_defrtr=1 >/dev/null 2>&1 || true", dev
            ));
            shell_cmds.push(format!(
                "sysctl -w net.ipv6.conf.{}.accept_ra_pinfo=1 >/dev/null 2>&1 || true", dev
            ));
        } else {
            cmds.push(format!("set network.{}.proto=static", iface_v6));
            if !ipv6.is_empty() {
                cmds.push(format!("set network.{}.ip6addr={}/128", iface_v6, ipv6));
            }
        }
        // 同主接口：缺省 auto 时 netifd 不开机自启（LuCI 显示「开机时未启动」）。
        cmds.push(format!("set network.{}.auto=1", iface_v6));
    } else if !iface_v6.is_empty() && !v6_managed(cfg) {
        // 关闭 IPv6（ipv6=0 或 v6_mode=off）：旧版本创建过 fm350v6 却只停止
        // 刷新，骨架与防火墙登记会一直残留 —— 这里主动拆掉，让「关闭」真正
        // 生效（审查问题：off/关闭模式下接口骨架残留）。
        shell_cmds.push(format!("ifdown {} >/dev/null 2>&1 || true", iface_v6));
        shell_cmds.push(format!(
            "uci -q delete network.{} 2>/dev/null || true",
            iface_v6
        ));
    }

    // ---- 默认路由 / 网关主机路由（仅有 v4 时）
    //
    // 有网关：交给 netifd 按 gateway 下发，并清理历史遗留的 onlink route
    // （两者并存时 ARP 行为不可预期，且 onlink 会让主机跳过网关直接问 ARP）。
    // 网关不在地址掩码内（点对点 /32、模组上报的异网段网关、static 模式配错
    // 网段等）时，netifd 的 `default via <gw>` 会被内核以「网关不可达」拒绝
    // —— 必须先装 `<gw>/32` 主机路由把网关本身变得可达（审查问题 #5）。
    // 无网关：维持 onlink 设备路由 —— netifd 不会为无网关接口自动下发。
    if have_v4 {
        match &plan_gw {
            Some(gw) => {
                shell_cmds.push(format!(
                    "uci -q delete network.{} 2>/dev/null || true",
                    route_name
                ));
                if !in_same_subnet(ipv4, gw, mask) {
                    cmds.push(format!("set network.{}=route", gwr_name));
                    cmds.push(format!("set network.{}.interface={}", gwr_name, iface));
                    cmds.push(format!("set network.{}.target={}/32", gwr_name, gw));
                } else {
                    shell_cmds.push(format!(
                        "uci -q delete network.{} 2>/dev/null || true",
                        gwr_name
                    ));
                }
            }
            None => {
                cmds.push(format!("set network.{}=route", route_name));
                cmds.push(format!("set network.{}.interface={}", route_name, iface));
                cmds.push(format!("set network.{}.target=0.0.0.0/0", route_name));
                cmds.push(format!("set network.{}.onlink=1", route_name));
                cmds.push(format!("set network.{}.metric={}", route_name, cfg.metric));
                shell_cmds.push(format!(
                    "uci -q delete network.{} 2>/dev/null || true",
                    gwr_name
                ));
            }
        }
    }

    // ---- 防火墙：归入 wan 区
    //
    // 三个要点（前两个是实机踩出来的）：
    //  1. 区下标必须按名字解析，不能硬编码 @zone[0]（实机 @zone[0] 是 lan 区）；
    //  2. add_list 不会去重 —— 轮询每轮都会走到这里，不去重就会无限追加
    //     （实测 95 s 追加 3 组，累计到过 9 组重复）。故先查再写，保证幂等；
    //  3. 只 commit 不 reload 时 fw4 的运行态不会更新（`config.change` 只由
    //     rpcd/LuCI 应用路径发出，裸 uci commit 不发 —— OpenWrt ticket
    //     #17305）：首装/后装时 wan 区 masquerade 不生效，内网彻底无 NAT
    //     （审查问题 #2）。故有变更时 commit 后立即 reload 防火墙。
    let mut fw_changed = false;
    match wan_zone_index() {
        Some(zi) => {
            let mut names = vec![iface.clone()];
            if v6_section {
                names.push(iface_v6.clone());
            }
            for name in names {
                if name.is_empty() {
                    continue;
                }
                let key = format!("firewall.@zone[{}].network", zi);
                if !uci_list_contains(&key, &name) {
                    cmds.push(format!("add_list {}={}", key, sq(&name)));
                    fw_changed = true;
                }
            }
            // v6 段已关闭时同步摘掉历史 zone 登记，否则 wan 区会一直挂着
            // 一个已删除的网络名（fw4 每次 reload 都会告警一次）。
            if !iface_v6.is_empty() && !v6_section {
                let key = format!("firewall.@zone[{}].network", zi);
                if uci_list_contains(&key, iface_v6) {
                    cmds.push(format!("del_list {}={}", key, sq(iface_v6)));
                    fw_changed = true;
                }
            }
        }
        None => {
            eprintln!(
                "fm350d: 未定位到 wan 防火墙区，跳过接口登记（请手工把 {} 加入 wan 区）",
                iface
            );
        }
    }

    // 删除类命令先跑：避免 `set` 之后再 `delete` 把刚写的值清掉。
    for c in &shell_cmds {
        let _ = real(c);
    }

    let results = uci_batch(&cmds);
    let failed: Vec<&String> = results.iter().filter(|r| !r.1).map(|r| &r.0).collect();
    if !failed.is_empty() {
        // 整批失败时回滚 delta：uci set 写在内存里，不 revert 会残留半套
        // 配置、污染下一次 commit（审查问题：批量失败不回滚）。
        let _ = real("uci revert network");
        return Err(format!("UCI 写入失败: {:?}", failed));
    }

    let _ = real("uci commit network");
    let _ = real("uci commit firewall");
    if fw_changed {
        let _ = real("/etc/init.d/firewall reload >/dev/null 2>&1");
    }

    // 应用：先 up 接口，再按模式补齐路由。
    // ifup 的结果不能丢：接口起不来是本插件最不希望出现的静默故障
    // （蜂窝链路无 IP、无默认路由，而前端只看到一片空白）。
    let (up_ok, up_out) = real(&format!("ifup {}", iface));
    if !up_ok {
        eprintln!("fm350d: ifup {} 失败（rc!=0）: {}", iface, up_out);
    }
    if v6_section {
        let (up_ok6, up_out6) = real(&format!("ifup {}", iface_v6));
        if !up_ok6 {
            eprintln!("fm350d: ifup {} 失败（rc!=0）: {}", iface_v6, up_out6);
        }
        // IPv6 默认设备路由**只在 static 模式**安装：
        //   * ra 模式：路由由内核按 RA 下发，metric 固定 1024（`proto ra`）；
        //   * dhcpv6 模式：odhcp6c 下发 `default from ... metric 512`。
        // 旧版无条件装 `metric <cfg.metric>`（默认 30）的设备路由，直接压过
        // RA/odhcp6c 把全部 v6 流量逼成 onlink（审查问题 #4）。
        if !v6_ra_mode(cfg) && !v6_dhcp_mode(cfg) {
            let _ = real(&format!(
                "ip -6 route replace default dev {} metric {}",
                dev, cfg.metric
            ));
        }
    }
    // ---- v4 默认路由兜底直写（netifd 之外的第一道保险；route_guard 周期复核）
    if have_v4 {
        match &plan_gw {
            Some(gw) => {
                let _ = real(&format!(
                    "ip route replace default via {} dev {} metric {}",
                    gw, dev, cfg.metric
                ));
            }
            None => {
                let _ = real(&format!(
                    "ip route replace default dev {} metric {}",
                    dev, cfg.metric
                ));
            }
        }
    }

    Ok((iface.clone(), dev, cmds))
}

/// 拆除由本插件创建的接口与路由。
pub fn teardown_iface(cfg: &Config) -> Result<Vec<String>, String> {
    let iface = &cfg.iface;
    let iface_v6 = &cfg.iface_v6;
    let route_name = format!("{}_def", iface);
    let gwr_name = format!("{}_gw4", iface);
    let mut cmds = Vec::new();

    let _ = real(&format!("ifdown {}", iface));
    if !iface_v6.is_empty() {
        let _ = real(&format!("ifdown {}", iface_v6));
    }
    cmds.push(format!("delete network.{}", route_name));
    cmds.push(format!("delete network.{}", gwr_name));
    cmds.push(format!("delete network.{}", iface));
    if !iface_v6.is_empty() {
        cmds.push(format!("delete network.{}", iface_v6));
    }
    // 与 ensure_iface 对称：按名解析区下标，且只删确实存在的项。
    // del_list 会移除该名字的全部重复项，正好用来清理历史污染。
    let mut fw_changed = false;
    if let Some(zi) = wan_zone_index() {
        for name in [iface.clone(), iface_v6.clone()] {
            if name.is_empty() {
                continue;
            }
            let key = format!("firewall.@zone[{}].network", zi);
            if uci_list_contains(&key, &name) {
                cmds.push(format!("del_list {}={}", key, sq(&name)));
                fw_changed = true;
            }
        }
    }

    let results = uci_batch(&cmds);
    let _ = real("uci commit network");
    let _ = real("uci commit firewall");
    // `ifdown` 本身不带 network reload（OpenWrt 的 /sbin/ifup 源码里只有 up
    // 路径先 `ubus call network reload`）：只删 UCI 不通知 netifd 会留下
    // 「幽灵接口」—— ubus 仍列出 fm350，UCI 里却已经没有（审查问题 #7）。
    let _ = real("ubus call network reload");
    if fw_changed {
        let _ = real("/etc/init.d/firewall reload >/dev/null 2>&1");
    }
    // 清掉参数签名：下一次带地址的巡检会按当前参数重新下发。
    clear_applied();
    // 失败的项（如接口本来就不存在）忽略
    Ok(results.into_iter().map(|r| r.0).collect())
}

#[derive(Debug, Default, serde::Serialize)]
pub struct NetStatus {
    pub iface: String,
    pub iface_v6: String,
    pub dev: Option<String>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub routes: Vec<String>,
    pub up: bool,
    /// 数据网卡上是否存在默认路由（v4，按 `ip route show dev` 判定）。
    pub default4: bool,
    /// 系统默认路由表中是否存在经由数据网卡的默认路由（v6，跨设备全局查询）。
    pub default6: bool,
    /// UCI 里为接口配置的 DNS 列表（peerdns=0，由本插件下发）。
    pub dns: Vec<String>,
}

/// 读取当前网络状态。
pub fn status(cfg: &Config) -> NetStatus {
    let dev = detect_dev(cfg);
    let mut st = NetStatus {
        iface: cfg.iface.clone(),
        iface_v6: cfg.iface_v6.clone(),
        dev: dev.clone(),
        ..Default::default()
    };
    if let Some(d) = &dev {
        let (_, out) = real(&format!("ip -o addr show dev {} 2>/dev/null", d));
        for line in out.lines() {
            // 形如: 2: eth2    inet 10.5.243.239/32 brd ...
            if let Some(p) = line.find("inet ") {
                let rest = &line[p + 5..];
                if let Some(addr) = rest.split_whitespace().next() {
                    let ip = addr.split('/').next().unwrap_or("").to_string();
                    if !ip.is_empty() {
                        st.ipv4.push(ip);
                    }
                }
            }
            if let Some(ip) = usable_global_v6_from_addr_line(line) {
                st.ipv6.push(ip);
            }
        }
        let (_, routes) = real(&format!("ip route show dev {} 2>/dev/null", d));
        st.routes = routes.lines().map(|l| l.trim().to_string()).collect();
        st.default4 = st.routes.iter().any(|l| l.starts_with("default"));
        // v6 默认路由可能安装在别的网卡（或根本不存在），做跨设备全局查询；
        // 必须按 dev 词法匹配，`dev eth2` 不能命中 `dev eth22`。
        let (_, routes6) = real("ip -6 route show default 2>/dev/null");
        st.default6 = routes6.lines().any(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            t.first() == Some(&"default")
                && t.windows(2).any(|w| w[0] == "dev" && w[1] == d.as_str())
        });
        st.up = !st.ipv4.is_empty() || !st.ipv6.is_empty();
    }
    // UCI 里的 DNS（供前端核对下发结果；审查问题 #9 的展示侧）
    let (dns_ok, dns_out) = real(&format!("uci -q get network.{}.dns", cfg.iface));
    if dns_ok {
        st.dns = dns_out.split_whitespace().map(|s| s.to_string()).collect();
    }
    st
}

/// 从 `ip -o addr` 的一行中提取仍在有效期内的公网/全局 IPv6。
///
/// 仅排除 `valid_lft 0sec`，不排除 `preferred_lft 0sec`：后者表示地址已
/// deprecated，不适合新连接优先选择，但在 valid_lft 归零前仍是可用地址。
fn usable_global_v6_from_addr_line(line: &str) -> Option<String> {
    let p = line.find("inet6 ")?;
    let rest = &line[p + 6..];
    let addr = rest.split_whitespace().next()?;
    let ip = addr.split('/').next().unwrap_or("");

    // 滤掉链路本地（fe80::/10）：任何 UP 的网卡都会自带一个，计入后会制造
    // 两个假阳性 —— 前端长期显示「有 IPv6」，且 st.up 在只有 link-local 时
    // 也判为在线。
    if ip.is_empty() || is_link_local_v6(ip) {
        return None;
    }
    if !addr_line_has_valid_lifetime(line) {
        return None;
    }
    Some(ip.to_string())
}

fn addr_line_has_valid_lifetime(line: &str) -> bool {
    let mut iter = line.split_whitespace();
    while let Some(tok) = iter.next() {
        if tok == "valid_lft" {
            return match iter.next() {
                Some("forever") => true,
                Some(v) if v.ends_with("sec") => v
                    .trim_end_matches("sec")
                    .parse::<u64>()
                    .map(|n| n > 0)
                    .unwrap_or(false),
                Some(_) => true,
                None => false,
            };
        }
    }
    // BusyBox/iproute2 输出异常或旧版本不带 lifetime 时，保守沿用原行为。
    true
}

/// 判断是否为 IPv6 链路本地地址（`fe80::/10`，即首组落在 `fe80`~`febf`）。
fn is_link_local_v6(ip: &str) -> bool {
    match u16::from_str_radix(ip.split(':').next().unwrap_or(""), 16) {
        Ok(v) => v & 0xffc0 == 0xfe80,
        Err(_) => false,
    }
}

/// IPv6 子接口刷新：用于守护发现全局 IPv6 消失或有效期归零后的自恢复。
///
/// 正常情况下静态地址常驻内核；这里是兜底，让 netifd 状态异常或地址被
/// 意外移除时，下一轮巡检能重新拉起 fm350v6（ifup static 会重新应用
/// uci 里已记录的 ip6addr）。
pub fn refresh_ipv6_iface(cfg: &Config) -> bool {
    if !v6_managed(cfg) {
        return false;
    }
    if v6_ra_mode(cfg) {
        if let Some(dev) = detect_dev(cfg) {
            enable_v6_ra(&dev);
            ensure_v6_ra_proto(cfg);
        }
    } else if v6_dhcp_mode(cfg) {
        ensure_v6_dhcpv6_proto(cfg);
    }
    let (ok, _) = real(&format!("ifup {}", cfg.iface_v6));
    ok
}

/// IPv6 是否交由本插件托管（`ipv6=1` 且 `v6_mode != off`）。
pub fn v6_managed(cfg: &Config) -> bool {
    cfg.ipv6 && !cfg.iface_v6.is_empty() && cfg.v6_mode != "off"
}

/// 是否走 RA 模式：`v6_mode` 为 `ra`（默认）或未识别值时都按 RA 处理；
/// 显式 `static` / `dhcpv6` / `off` 各自走自己的分支。
pub fn v6_ra_mode(cfg: &Config) -> bool {
    cfg.ipv6 && !matches!(cfg.v6_mode.as_str(), "static" | "dhcpv6" | "off")
}

/// 是否走 DHCPv6 模式：把 IPv6 交给 netifd 的 odhcp6c（与主流第三方插件同款做法）。
///
/// 为什么这条路径最稳：odhcp6c 在**用户态**用 raw socket 收 ICMPv6 RA，
/// 完全不看 `net.ipv6.conf.<dev>.accept_ra`。蜂窝网卡进 WAN 区后
/// `forwarding=1`、内核默认值会让 RA 全丢（实机 `accept_ra=0` 时第三方插件的
/// v6 照样通），而 odhcp6c 天然绕开了这个坑，不需要插件去改 sysctl。
pub fn v6_dhcp_mode(cfg: &Config) -> bool {
    cfg.ipv6 && cfg.v6_mode == "dhcpv6"
}

/// 打开数据网卡的 IPv6 RA 接收。
///
/// 为什么必须是 `accept_ra=2`：蜂窝接口被 netifd 放进 WAN 区后
/// `net.ipv6.conf.<dev>.forwarding=1`，内核默认（`accept_ra=1`）会**直接丢弃**
/// 所有 RA。只有 2 表示「即使开了转发也接收 RA」。实机正是卡在这里：
/// 模组一直在发 RA（`ip -6 neigh` 里能看到 router 标记），但内核一条都不处理。
pub fn enable_v6_ra(dev: &str) -> bool {
    let mut ok = true;
    for c in [
        format!("sysctl -w net.ipv6.conf.{}.accept_ra=2", dev),
        format!("sysctl -w net.ipv6.conf.{}.accept_ra_defrtr=1", dev),
        format!("sysctl -w net.ipv6.conf.{}.accept_ra_pinfo=1", dev),
    ] {
        let (o, _) = real(&format!("{} >/dev/null 2>&1 || true", c));
        ok = ok && o;
    }
    ok
}

/// 内核是否已按 RA 拿到 IPv6（默认路由或 SLAAC 地址）。
pub fn ra_v6_ready(dev: &str) -> bool {
    let (_, routes) = real(&format!("ip -6 route show default dev {} 2>/dev/null", dev));
    if routes
        .lines()
        .any(|l| l.trim().starts_with("default") && l.contains("proto ra"))
    {
        return true;
    }
    let (_, addrs) = real(&format!("ip -o -6 addr show dev {} 2>/dev/null", dev));
    addrs
        .lines()
        .any(|l| l.contains("inet6 ") && l.contains("proto kernel_ra"))
}

/// RA 模式下把 v6 子接口切成「不托管地址」的形态，并清掉历史静态地址。
///
/// `proto=none` 让 netifd 只把设备拉起、不写任何地址，地址与默认路由全部
/// 交给内核按 RA 处理 —— 这样插件那条 `default dev <dev> metric <m>` 就不会
/// 以更低 metric 压过 RA 下发的 `default via fe80::2 metric 1024`。
fn ensure_v6_ra_proto(cfg: &Config) {
    let iface_v6 = &cfg.iface_v6;
    let (ok_proto, proto) = real(&format!("uci -q get network.{}.proto", iface_v6));
    if !ok_proto || proto.trim() != "none" {
        let _ = uci(&format!("set network.{}.proto=none", iface_v6));
    }
    let _ = real(&format!(
        "uci -q delete network.{}.ip6addr 2>/dev/null || true",
        iface_v6
    ));
    let _ = real("uci commit network");
}

/// DHCPv6 模式下把 v6 子接口交给 netifd 的 odhcp6c。
///
/// `proto=dhcpv6` + `extendprefix=1`：地址、默认路由、`/64` 前缀委派全部由
/// odhcp6c/netifd 负责，插件既不写静态地址也不改 sysctl。
fn ensure_v6_dhcpv6_proto(cfg: &Config) {
    let iface_v6 = &cfg.iface_v6;
    let (ok_proto, proto) = real(&format!("uci -q get network.{}.proto", iface_v6));
    if !ok_proto || proto.trim() != "dhcpv6" {
        let _ = uci(&format!("set network.{}.proto=dhcpv6", iface_v6));
    }
    let (ok_ext, ext) = real(&format!("uci -q get network.{}.extendprefix", iface_v6));
    if !ok_ext || ext.trim() != "1" {
        let _ = uci(&format!("set network.{}.extendprefix=1", iface_v6));
    }
    // 旧版本可能留下静态地址，留着会和 odhcp6c 抢同一个地址
    let _ = real(&format!(
        "uci -q delete network.{}.ip6addr 2>/dev/null || true",
        iface_v6
    ));
    let _ = real("uci commit network");
}

/// 把模组侧 IPv6 地址写入 v6 接口（静态）并补齐设备路由。
///
/// 与 [`ensure_iface`] 的区别：本函数只处理 IPv6 一侧，供守护在「模组侧
/// IPv6 变化」或「接口缺地址」时调用，避免为补一个 v6 地址而整批重写
/// IPv4 配置。地址变化时才会真正写 uci；proto 每次都会校验为 static
/// （兜底迁移旧的 dhcpv6 配置）。
///
/// 返回是否执行了写入/刷新动作。
pub fn apply_ipv6_addr(cfg: &Config, ipv6: &str) -> bool {
    if !v6_managed(cfg) || ipv6.is_empty() {
        return false;
    }
    let iface_v6 = &cfg.iface_v6;
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };

    // DHCPv6 模式：IPv6 完全交给 odhcp6c，插件不写地址、不改 sysctl。
    // 这里只兜底确认接口形态正确（proto=dhcpv6 + extendprefix=1），
    // 然后触发一次 ifup 让 netifd 重新拉起 odhcp6c。
    if v6_dhcp_mode(cfg) {
        ensure_v6_dhcpv6_proto(cfg);
        let _ = real(&format!("ifup {}", iface_v6));
        return true;
    }

    // RA 模式：地址与默认路由由内核按运营商 RA 处理，插件不写静态 /128。
    // 只有当 RA 迟迟不来（个别固件/运营商确实不转发 RA）时才回落到静态方案。
    if v6_ra_mode(cfg) {
        enable_v6_ra(&dev);
        ensure_v6_ra_proto(cfg);
        if ra_v6_ready(&dev) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_secs(6));
        enable_v6_ra(&dev);
        if ra_v6_ready(&dev) {
            return true;
        }
        eprintln!("fm350d: 未收到运营商 RA，回落到模组侧静态 IPv6");
    }

    let mut cmds: Vec<String> = Vec::new();
    // section 缺失时 uci set <name>.<opt> 会报错，先确保接口存在
    let (ok_sec, sec) = real(&format!("uci -q get network.{}", iface_v6));
    if !ok_sec || sec.trim() != "interface" {
        cmds.push(format!("set network.{}=interface", iface_v6));
    }
    // proto 兜底校正为 static（从 dhcpv6 旧配置升级的场景）
    let (ok_proto, proto) = real(&format!("uci -q get network.{}.proto", iface_v6));
    if !ok_proto || proto.trim() != "static" {
        cmds.push(format!("set network.{}.proto=static", iface_v6));
    }
    for stale in ["reqaddress", "reqprefix", "peerdns", "extendprefix"] {
        cmds.push(format!("delete network.{}.{}", iface_v6, stale));
    }
    // 设备归属同样兜底（@<主接口>），防止历史配置指向错误设备
    let iface = &cfg.iface;
    let (ok_dev, cur_dev) = real(&format!("uci -q get network.{}.device", iface_v6));
    if !ok_dev || cur_dev.trim() != format!("@{}", iface) {
        cmds.push(format!("set network.{}.device=@{}", iface_v6, iface));
    }
    cmds.push(format!("set network.{}.ip6addr={}/128", iface_v6, ipv6));
    cmds.push(format!("set network.{}.auto=1", iface_v6));

    for c in &cmds {
        let _ = uci(c);
    }
    let _ = real("uci commit network");
    let _ = real(&format!("ifup {}", iface_v6));
    let _ = real(&format!(
        "ip -6 route replace default dev {} metric {}",
        dev, cfg.metric
    ));
    true
}

/// 判断一行 `ip route` 输出是否匹配给定 metric。
///
/// 两个坑（审查问题：metric=0 日志噪声 / 前缀误匹配）：
///   * `ip route` 在 metric=0 时**不打印** `metric` 字段，按字面子串永远
///     匹配不到，路由每次巡检都被误报缺失、又每轮 `replace` 刷一条日志；
///   * 必须按词法单元比较 —— 子串 `metric 3` 会误命中 `metric 30`。
fn line_has_metric(line: &str, metric: u32) -> bool {
    let toks: Vec<&str> = line.split_whitespace().collect();
    match toks.iter().position(|t| *t == "metric") {
        Some(i) => toks.get(i + 1).and_then(|v| v.parse::<u32>().ok()) == Some(metric),
        None => metric == 0,
    }
}

/// 路由守护：netifd 不会为无网关接口下发设备路由，这里周期补齐。
///
/// IPv4 与 IPv6 各自独立检查；返回是否执行了补齐动作。
pub fn route_guard(cfg: &Config) -> bool {
    if !cfg.route_guard {
        return false;
    }
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };
    let mut acted = false;

    // ---- IPv4：只有当接口确实拿到地址时才补路由，避免空路由污染主表
    let (_, out) = real(&format!("ip -o -4 addr show dev {} 2>/dev/null", dev));
    if out.contains("inet ") {
        let wanted = match current_gateway(cfg) {
            Some(gw) => format!("default via {} dev {} metric {}", gw, dev, cfg.metric),
            None => format!("default dev {} metric {}", dev, cfg.metric),
        };
        let (_, routes) = real(&format!("ip route show dev {} 2>/dev/null", dev));
        let has_wanted = routes
            .lines()
            .any(|l| l.trim().starts_with("default") && line_has_metric(l, cfg.metric));
        if !has_wanted {
            let (ok, _) = real(&format!("ip route replace {}", wanted));
            acted = acted || ok;
        }
    }

    // ---- IPv6：静态地址同样无网关，默认路由需要周期补齐。
    if v6_managed(cfg) {
        let ra = v6_ra_mode(cfg);
        let dhcp = v6_dhcp_mode(cfg);
        if ra {
            // RA 模式下每轮兜底打开 accept_ra：USB 复位/网卡重建后 sysctl 会回到
            // 默认值，而 forwarding=1 时默认值 0 会让内核彻底丢弃 RA。
            enable_v6_ra(&dev);
        }
        // dhcpv6 模式不改任何 sysctl：路由与地址归 odhcp6c/netifd 管。
        let (_, routes6) = real(&format!("ip -6 route show dev {} 2>/dev/null", dev));

        // ra/dhcpv6 模式：任何写在 cfg.metric 上的**无网关**默认设备路由都是
        // 历史残留（旧版 ensure_iface 曾无条件安装），必须无条件清理 ——
        // 不能等「有全局地址」才动：SLAAC/DHCPv6 地址到位之前它就在压制
        // RA/odhcp6c 下发的路由（审查问题 #4）。与兜底 metric 相同时是本
        // 函数自己维护的路由，跳过。
        if (ra || dhcp) && cfg.metric != V6_FALLBACK_METRIC {
            let stale = routes6.lines().any(|l| {
                let t = l.trim();
                t.starts_with("default") && !t.contains(" via ") && line_has_metric(t, cfg.metric)
            });
            if stale {
                let _ = real(&format!(
                    "ip -6 route del default dev {} metric {} 2>/dev/null || true",
                    dev, cfg.metric
                ));
                acted = true;
            }
        }

        // 只有当设备上确有全局 IPv6 地址时才补兜底路由（link-local 不算）。
        let (_, out6) = real(&format!("ip -o -6 addr show dev {} 2>/dev/null", dev));
        let has_global_v6 = out6
            .lines()
            .filter(|l| l.contains("inet6 "))
            .filter_map(|l| usable_global_v6_from_addr_line(l))
            .next()
            .is_some();
        if has_global_v6 {
            // RA 路由由内核按 RA 下发，metric 固定 1024（`proto ra`）。
            // 这里维护的 onlink 路由只是**兜底**：metric 取 2048，比 RA 差，
            // 所以 RA 在时由 RA 优先，RA 还没来（或 netifd 重启把 RA 路由冲掉
            // 到下一轮 RA 到达之间）时由它兜住流量。
            //
            // 为什么不直接删掉 onlink 路由：实机观察到 RA 路由会被 ifdown/ifup
            // 冲掉、且要等下一次 RA（最长数百秒）才回来，删除会造成
            // 「默认路由真空」，`ping -6` 直接报 Network unreachable。
            // 为什么不把 onlink 放在低 metric：那会压过 RA 路由，强制主机对
            // 每个目的地址直接发 NS，完全依赖模组做 NDP 代理。
            // dhcpv6 模式下 odhcp6c 装的是 `default from <prefix> via fe80::2
            // metric 512`，同样必须让兜底路由排在它后面。
            let metric6 = if ra || dhcp {
                V6_FALLBACK_METRIC
            } else {
                cfg.metric
            };
            let wanted6 = format!("default dev {} metric {}", dev, metric6);
            let has_wanted = routes6
                .lines()
                .any(|l| l.trim().starts_with("default") && line_has_metric(l, metric6));
            if !has_wanted {
                let (ok6, _) = real(&format!("ip -6 route replace {}", wanted6));
                acted = acted || ok6;
            }
        }
    }

    acted
}

/// RA 模式下 onlink 兜底 IPv6 默认路由的 metric。
///
/// 必须大于内核按 RA 下发默认路由的 metric（1024），否则兜底路由会压过 RA 路由。
const V6_FALLBACK_METRIC: u32 = 2048;

/// 保证插件创建的接口处于「开机自启」状态。
///
/// 为什么必须有它：`ensure_iface` 只在拨号后或地址有偏差时才跑，而守护的稳态
/// 分支在「地址已经匹配」时会整段跳过，于是 auto 选项一旦不是 1 就再也没机会
/// 补回来 —— 实机 fm350v6 即长期停在 auto=0（LuCI 显示「开机时未启动」），
/// 而手工 uci 明明能写进去。
///
/// 这里只做补齐：接口 section 已存在且 auto≠1 时才写入，成本 2~4 次 uci get。
/// 返回被修正的接口名列表，为空表示无需改动（调用方据此避免刷日志）。
pub fn ensure_autostart(cfg: &Config) -> Vec<String> {
    let mut names = vec![cfg.iface.clone()];
    if cfg.ipv6 && !cfg.iface_v6.is_empty() {
        names.push(cfg.iface_v6.clone());
    }

    let mut touched: Vec<String> = Vec::new();
    for name in names {
        if name.is_empty() {
            continue;
        }
        // 仅在接口已存在时补齐：section 缺失时 uci set 会直接报错
        let (ok_sec, sec) = real(&format!("uci -q get network.{}", name));
        if !ok_sec || sec.trim() != "interface" {
            continue;
        }
        let key = format!("network.{}.auto", name);
        let (ok_auto, val) = real(&format!("uci -q get {}", key));
        if ok_auto && val.trim() == "1" {
            continue;
        }
        if uci(&format!("set {}=1", key)).0 {
            touched.push(name);
        }
    }
    if !touched.is_empty() {
        let _ = real("uci commit network");
    }
    touched
}

// ---------------------------------------------------------------- 数据面健康与自愈

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DataHealth {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
}

/// 读取数据网卡收发统计（走 sysfs，成本极低，可每轮调用）。
pub fn data_health(dev: &str) -> Option<DataHealth> {
    let rd = |n: &str| -> Option<u64> {
        fs::read_to_string(format!("/sys/class/net/{}/statistics/{}", dev, n))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    };
    Some(DataHealth {
        rx_packets: rd("rx_packets")?,
        tx_packets: rd("tx_packets")?,
        tx_errors: rd("tx_errors")?,
    })
}

/// 判定数据面是否卡死。
///
/// 判据：**tx_errors 在涨，而 tx_packets 不动**。正常链路上 tx_errors 恒为 0，
/// 一旦 USB 数据端点被打到 stall，内核每次提交 URB 都会记一次错误、包却一个
/// 也发不出去（实机：tx_packets 停在 2，tx_errors 从百级一路涨到千级，
/// 同时刷 `NETDEV WATCHDOG: transmit queue timed out`）。
///
/// 不用「RX 不增长」作判据：空闲链路上本来就没有下行流量。
pub fn data_plane_stalled(prev: &DataHealth, cur: &DataHealth) -> bool {
    cur.tx_errors > prev.tx_errors && cur.tx_packets <= prev.tx_packets
}

/// 轻量复位数据网卡：只 down/up 网卡并重新 ifup，不动基带、不重启模组。
///
/// 端点偶发 stall（典型诱因是与其它 modem 插件争抢接口）多数能被这一级恢复。
pub fn bounce_data_dev(cfg: &Config) -> bool {
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };
    // 先走 netifd 的 ifdown：让它同步移除地址/路由并把运行态置 down，
    // 否则裸 `ip link set down` 会造成「内核链路 down、netifd 仍以为 up」
    // 的状态漂移（审查问题：bounce 直接操作裸链路）。
    let _ = real(&format!("ifdown {}", cfg.iface));
    if v6_managed(cfg) {
        let _ = real(&format!("ifdown {}", cfg.iface_v6));
    }
    // 硬复位数据端点（stall 自愈的核心动作）：即便 ifdown 因异常没能落下
    // 链路，这里也强制拉低；命令幂等，链路已 down 时无副作用。
    let _ = real(&format!("ip link set {} down", dev));
    std::thread::sleep(Duration::from_secs(2));
    let _ = real(&format!("ip link set {} up", dev));
    std::thread::sleep(Duration::from_secs(1));
    // 回到 netifd：由它按 UCI 重新下发地址与路由（裸 up 不会恢复配置）。
    let (ok, _) = real(&format!("ifup {}", cfg.iface));
    // 数据面复位会让 v6 子接口（device=@主接口）跟着失联，这里一并拉回，
    // 否则 v6 侧要等下一轮 refresh 才恢复。
    if v6_managed(cfg) {
        let _ = real(&format!("ifup {}", cfg.iface_v6));
    }
    ok
}

/// 找出同样绑定在该数据网卡上的**非本插件** uci 接口。
///
/// 典型场景：设备上另装了其它 modem 管理插件（如 ModemManager），它们也会在同一个
/// 网卡上建接口（如 `network.2_1`）并周期性拨号、改写接口。两个守护同时
/// 操作一块模组会互相打断（接口反复 down/up、AT 口争用），最终把数据端点
/// 打到 stall —— 表现为"配置全对却就是上不了网"。
pub fn foreign_ifaces_on_dev(cfg: &Config, dev: &str) -> Vec<String> {
    let script = format!(
        "for s in $(uci -q show network 2>/dev/null | sed -n 's/^network\\.\\([^.=]*\\)=interface$/\\1/p'); do \
         d=$(uci -q get network.$s.device 2>/dev/null); \
         [ -z \"$d\" ] && d=$(uci -q get network.$s.ifname 2>/dev/null); \
         [ \"$d\" = {} ] && echo \"$s\"; \
         done",
        sq(dev)
    );
    let (_, out) = real(&script);
    out.lines()
        .map(|l| l.trim().to_string())
        .filter(|n| !n.is_empty() && n != &cfg.iface && n != &cfg.iface_v6)
        .collect()
}

/// 拔号成功后一次性把网络拉起。
///
/// `modem_gw`：模组侧上报的网关（见 [`plan_gateway`]）。地址族校验规则：
///   * 给了 v4 —— 必须出现在内核里（历史语义，保持不变）；
///   * 给了 v6 且为 static 模式 —— 也必须出现（地址由我们自己写）；
///   * ra / dhcpv6 模式 —— v6 由内核 RA / odhcp6c 后续下发，不在此卡关，
///     缺失由守护的 v6_missing / recovery 分支自愈。
/// 两个族都为空才直接报错（审查问题 #1：v6-only 会话曾被「必须有 v4」的
/// 门槛挡死，守护在 `active` 分支里无限空转）。
pub fn apply_after_dial(
    cfg: &Config,
    ipv4: &str,
    ipv6: &str,
    dns: &[String],
    modem_gw: &str,
) -> Result<NetStatus, String> {
    if ipv4.is_empty() && ipv6.is_empty() {
        return Err("缺少 IPv4/IPv6 地址，无法配置接口".to_string());
    }
    ensure_iface(cfg, ipv4, ipv6, dns, modem_gw)?;

    // `ifup` 是异步的：netifd 受理后，地址要过一会儿才落进内核。
    // 直接读一次 status() 常常读到空地址，于是把「刚拨上」误报成失败
    // （实机复现：日志打出「已重新应用网络配置 []」，而接口随后确实通了；
    //  拨号页也会瞬间显示未上线）；反过来，网卡名配错导致根本没起来时，
    // 又会被当成成功（net.up=false 却无任何错误）。
    // 因此这里有限轮询等地址落地：通常 <500 ms 即返回，最多等 3 s。
    let v6_enforced = !ipv6.is_empty() && v6_managed(cfg) && !v6_ra_mode(cfg) && !v6_dhcp_mode(cfg);
    let ok_v4 = |last: &NetStatus| ipv4.is_empty() || last.ipv4.iter().any(|a| a == ipv4);
    let ok_v6 = |last: &NetStatus| !v6_enforced || last.ipv6.iter().any(|a| a == ipv6);
    let mut last = status(cfg);
    for _ in 0..12 {
        if ok_v4(&last) && ok_v6(&last) {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        last = status(cfg);
    }
    if !(ok_v4(&last) && ok_v6(&last)) {
        return Err(format!(
            "接口 {} 未在 3 s 内取得期望地址（v4 期望 {:?}，实际 {:?}；v6 期望 {:?}，实际 {:?}；网卡 {}）",
            cfg.iface,
            ipv4,
            last.ipv4,
            ipv6,
            last.ipv6,
            last.dev.as_deref().unwrap_or("?")
        ));
    }

    // 地址已落地：此时才做网关 ARP 实测（审查问题 #5 —— 必须在内核有地址
    // 之后，紧跟 ifup 的固定 500 ms 等待经常在地址就绪前探测、必失败）。
    if !ipv4.is_empty() {
        if let Some(dev) = detect_dev(cfg) {
            finalize_gateway(cfg, &dev, ipv4, modem_gw);
            // fallback 可能触发了 ifup，重新读一次状态（下方直接返回它）
            last = status(cfg);
        }
    }

    // DNS 校验：调用方给了 DNS 却没写进 UCI（批量静默失败/被 revert）时
    // 必须报错，否则 resolver 停在陈旧 DNS 上（审查问题 #9）。
    if !dns.is_empty() {
        let cur = configured_dns(cfg);
        let missing: Vec<&String> = dns
            .iter()
            .filter(|d| !cur.iter().any(|x| x.as_str() == d.as_str()))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "地址已就位但 DNS 配置缺失（期望含 {:?}，uci 实际 {:?}）",
                missing, cur
            ));
        }
    }

    mark_applied(cfg);
    Ok(last)
}

/// 网关 ARP 实测与回退。**必须在地址已落进内核之后调用**
/// （[`apply_after_dial`] 的地址轮询之后）—— ifup 异步，紧跟其后的固定
/// 500 ms 等待经常在地址就绪前探测，必失败，会把会话误判为「网关不可达」
/// 并回退到 onlink 方案（审查问题 #5）。
fn finalize_gateway(cfg: &Config, dev: &str, ipv4: &str, modem_gw: &str) {
    let (_, plan_gw) = plan_gateway(cfg, ipv4, modem_gw);
    if let Some(gw) = plan_gw {
        std::thread::sleep(Duration::from_millis(500));
        if probe_gateway(dev, &gw) {
            // 回写实际网关，便于 `uci show fm350` 直接看到、也便于前端展示
            let _ = real(&format!(
                "uci -q set fm350.main.gateway={}; uci -q commit fm350",
                gw
            ));
        } else {
            fallback_to_onlink(cfg, dev, &gw);
        }
    }
}

/// ARP 实测失败时的整体回退：网关相关的所有 UCI 项一并清掉（含新版的
/// `<iface>_gw4` 主机路由与展示用 `fm350.main.gateway`），回到无网关的
/// onlink 设备路由方案。
fn fallback_to_onlink(cfg: &Config, dev: &str, gw: &str) {
    let iface = &cfg.iface;
    let route_name = format!("{}_def", iface);
    let gwr_name = format!("{}_gw4", iface);
    eprintln!(
        "fm350d: 网关 {} 在 {} 上未解析到 MAC（ARP 无应答），回退为无网关 onlink 设备路由",
        gw, dev
    );
    for c in [
        format!("uci -q delete network.{}.gateway 2>/dev/null || true", iface),
        format!("uci -q delete network.{} 2>/dev/null || true", gwr_name),
        format!("uci -q set network.{}.netmask=255.255.255.255", iface),
        format!("uci -q set network.{}=route", route_name),
        format!("uci -q set network.{}.interface={}", route_name, iface),
        format!("uci -q set network.{}.target=0.0.0.0/0", route_name),
        format!("uci -q set network.{}.onlink=1", route_name),
        format!("uci -q set network.{}.metric={}", route_name, cfg.metric),
        // 展示用网关一并清除，避免 uci show 里留着一个并未生效的值
        "uci -q delete fm350.main.gateway 2>/dev/null || true".to_string(),
    ] {
        let _ = real(&c);
    }
    let _ = real("uci -q commit network");
    let _ = real("uci -q commit fm350");
    let _ = real(&format!("ifup {}", iface));
    let _ = real(&format!(
        "ip route replace default dev {} metric {}",
        dev, cfg.metric
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_does_not_touch_system() {
        let (ok, script) = sh(RunMode::Dry, "uci set network.fm350=interface");
        assert!(ok);
        assert_eq!(script, "uci set network.fm350=interface");
    }

    #[test]
    fn link_local_v6_detection() {
        assert!(is_link_local_v6("fe80::1"));
        assert!(is_link_local_v6("FE80::200:11ff:fe12:1314"));
        assert!(is_link_local_v6("febf::1"));
        assert!(!is_link_local_v6("2409:8d5b:358:43b::1"));
        assert!(!is_link_local_v6("2409:8d5b:358:43b:200:11ff:fe12:1314"));
        assert!(!is_link_local_v6("::1"));
        assert!(!is_link_local_v6(""));
    }

    #[test]
    fn v6_addr_line_requires_valid_lifetime() {
        let ok = "2: eth2 inet6 2409:8d5b:358:43b::8/64 scope global dynamic valid_lft 3588sec preferred_lft 3588sec";
        let deprecated = "2: eth2 inet6 2409:8d5b:358:43b::9/64 scope global dynamic valid_lft 120sec preferred_lft 0sec";
        let expired = "2: eth2 inet6 2409:8d5b:358:43b::a/64 scope global dynamic valid_lft 0sec preferred_lft 0sec";
        let forever = "2: eth2 inet6 2409:8d5b:358:43b::b/64 scope global valid_lft forever preferred_lft forever";
        let link_local = "2: eth2 inet6 fe80::200:11ff:fe12:1314/64 scope link valid_lft forever preferred_lft forever";

        assert_eq!(
            usable_global_v6_from_addr_line(ok).as_deref(),
            Some("2409:8d5b:358:43b::8")
        );
        assert_eq!(
            usable_global_v6_from_addr_line(deprecated).as_deref(),
            Some("2409:8d5b:358:43b::9")
        );
        assert_eq!(usable_global_v6_from_addr_line(expired), None);
        assert_eq!(
            usable_global_v6_from_addr_line(forever).as_deref(),
            Some("2409:8d5b:358:43b::b")
        );
        assert_eq!(usable_global_v6_from_addr_line(link_local), None);
    }

    #[test]
    fn static_v6_addr_option_is_formatted_correctly() {
        let cfg = Config {
            ipv6: true,
            iface: "fm350".to_string(),
            iface_v6: "fm350v6".to_string(),
            ..Default::default()
        };
        // 只校验选项拼装逻辑，不触碰真实 uci：用 Dry 模式回显
        let script = format!("set network.{}.ip6addr={}/128", cfg.iface_v6, "2409:8057:2000::8");
        let (ok, out) = sh(RunMode::Dry, &script);
        assert!(ok);
        assert_eq!(out, "set network.fm350v6.ip6addr=2409:8057:2000::8/128");
    }

    /// 四种 v6_mode 必须两两互斥，且未识别值回落到 ra（兼容旧配置）。
    #[test]
    fn v6_mode_predicates_are_mutually_exclusive() {
        let mk = |mode: &str, ipv6: bool| Config {
            ipv6,
            v6_mode: mode.to_string(),
            iface: "fm350".to_string(),
            iface_v6: "fm350v6".to_string(),
            ..Default::default()
        };
        for (mode, managed, ra, dhcp) in [
            ("dhcpv6", true, false, true),
            ("ra", true, true, false),
            ("static", true, false, false),
            ("off", false, false, false),
            ("", true, true, false), // 未识别/缺省 -> ra
        ] {
            let c = mk(mode, true);
            assert_eq!(v6_managed(&c), managed, "managed mode={}", mode);
            assert_eq!(v6_ra_mode(&c), ra, "ra mode={}", mode);
            assert_eq!(v6_dhcp_mode(&c), dhcp, "dhcp mode={}", mode);
        }
        // ipv6 总开关关闭时任何模式都不托管
        let off = mk("dhcpv6", false);
        assert!(!v6_managed(&off) && !v6_ra_mode(&off) && !v6_dhcp_mode(&off));
    }

    #[test]
    fn gateway_is_derived_only_for_private_or_cgnat() {
        // 运营商 CGNAT / 私有地址：按同网段 .1 推导（同类插件通行策略）
        assert_eq!(derive_gateway("10.8.217.45").as_deref(), Some("10.8.217.1"));
        assert_eq!(derive_gateway("10.30.133.8").as_deref(), Some("10.30.133.1"));
        assert_eq!(derive_gateway("192.168.1.7").as_deref(), Some("192.168.1.1"));
        assert_eq!(derive_gateway("100.64.0.9").as_deref(), Some("100.64.0.1"));
        assert_eq!(derive_gateway("172.16.5.9").as_deref(), Some("172.16.5.1"));
        // 公网地址不推导：蜂窝公网 .1 通常不是网关，写错会堵死链路
        assert_eq!(derive_gateway("36.112.8.10"), None);
        assert_eq!(derive_gateway("120.196.165.7"), None);
        assert_eq!(derive_gateway("not-an-ip"), None);
    }

    #[test]
    fn gateway_mode_plans_netmask_and_gateway() {
        let base = Config {
            ipv6: false,
            ..Default::default()
        };

        let auto = Config {
            gateway_mode: "auto".into(),
            ..base.clone()
        };
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "").1.as_deref(),
            Some("10.8.217.1")
        );
        assert_eq!(plan_gateway(&auto, "10.8.217.45", "").0, "255.255.255.0");
        // 公网地址：模组未报网关时不推导，掩码回到 /32
        assert_eq!(plan_gateway(&auto, "36.112.8.10", "").1, None);
        assert_eq!(plan_gateway(&auto, "36.112.8.10", "").0, "255.255.255.255");

        let off = Config {
            gateway_mode: "off".into(),
            ..base.clone()
        };
        assert_eq!(plan_gateway(&off, "10.8.217.45", "").1, None);
        assert_eq!(plan_gateway(&off, "10.8.217.45", "").0, "255.255.255.255");

        let st = Config {
            gateway_mode: "static".into(),
            gateway: "10.8.217.254".into(),
            ..base.clone()
        };
        assert_eq!(
            plan_gateway(&st, "10.8.217.45", "").1.as_deref(),
            Some("10.8.217.254")
        );
    }

    #[test]
    fn modem_gateway_preferred_in_auto() {
        let auto = Config {
            gateway_mode: "auto".into(),
            ..Default::default()
        };
        // 模组上报的网关优先于「同网段 .1」推导（审查问题 #5）
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "10.8.217.254").1.as_deref(),
            Some("10.8.217.254")
        );
        // 非法/占位网关（0.0.0.0）回退到推导
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "0.0.0.0").1.as_deref(),
            Some("10.8.217.1")
        );
        // 模组网关 == 本机地址：不采信
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "10.8.217.45").1.as_deref(),
            Some("10.8.217.1")
        );
        // 公网地址 + 模组上报网关：采信上报值（纯推导在公网下会放弃网关）
        let (m, g) = plan_gateway(&auto, "36.112.8.10", "36.112.8.1");
        assert_eq!(g.as_deref(), Some("36.112.8.1"));
        assert_eq!(m, "255.255.255.0");
        // 模组上报异网段网关：仍采信（主机路由由 ensure_iface 补）
        let (m2, g2) = plan_gateway(&auto, "10.8.217.45", "172.16.0.1");
        assert_eq!(g2.as_deref(), Some("172.16.0.1"));
        assert_eq!(m2, "255.255.255.0");
    }

    #[test]
    fn in_same_subnet_follows_mask() {
        assert!(in_same_subnet("10.8.217.45", "10.8.217.1", "255.255.255.0"));
        assert!(!in_same_subnet(
            "10.8.217.45",
            "172.16.0.1",
            "255.255.255.0"
        ));
        // /32：除自身外任何网关都要走主机路由
        assert!(!in_same_subnet(
            "10.8.217.45",
            "10.8.217.1",
            "255.255.255.255"
        ));
        // 掩码非法时保守判为不同网段（触发主机路由）
        assert!(!in_same_subnet("10.8.217.45", "10.8.217.1", "garbage"));
        // 任一侧不是点分四段
        assert!(!in_same_subnet("10.8.217", "10.8.217.1", "255.255.255.0"));
    }

    #[test]
    fn metric0_route_lines_match_without_metric_token() {
        // ip route 在 metric=0 时不打印 metric 字段（审查问题：日志噪声）
        assert!(line_has_metric("default via 10.8.217.1 dev eth2", 0));
        assert!(!line_has_metric("default via 10.8.217.1 dev eth2", 30));
        assert!(line_has_metric("default dev eth2 metric 30", 30));
        assert!(line_has_metric("default dev eth2", 0));
        // 词法比较：metric 30 不能被 metric 300 撞上
        assert!(!line_has_metric("default dev eth2 metric 300", 30));
        assert!(!line_has_metric("proto kernel scope link", 30));
    }

    #[test]
    fn config_sig_tracks_netifd_relevant_params() {
        let a = Config::default();
        assert_eq!(config_sig(&a), config_sig(&Config::default()));
        let b = Config {
            metric: a.metric.wrapping_add(1),
            ..Config::default()
        };
        assert_ne!(config_sig(&a), config_sig(&b));
        let c = Config {
            gateway_mode: "static".into(),
            ..Config::default()
        };
        assert_ne!(config_sig(&a), config_sig(&c));
        let d = Config {
            ipv6: !a.ipv6,
            ..Config::default()
        };
        assert_ne!(config_sig(&a), config_sig(&d));
    }

    #[test]
    fn stalled_judges_tx_errors_not_idle_rx() {
        let a = DataHealth {
            rx_packets: 0,
            tx_packets: 2,
            tx_errors: 100,
        };
        // 端点 stall：包发不出去（tx_packets 不动），错误计数却在涨
        let stalled = DataHealth {
            rx_packets: 0,
            tx_packets: 2,
            tx_errors: 145,
        };
        assert!(data_plane_stalled(&a, &stalled));

        // 正常链路：tx_packets 在涨（即便同时有零星错误）
        let healthy = DataHealth {
            rx_packets: 0,
            tx_packets: 30,
            tx_errors: 101,
        };
        assert!(!data_plane_stalled(&a, &healthy));

        // 空闲链路：计数完全不变，不算 stall（没流量是正常的）
        assert!(!data_plane_stalled(&a, &a));
    }
}
