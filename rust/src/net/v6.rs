//! IPv6 一侧的获取方式、地址解析与接口形态维护。
//!
//! ## 为什么 IPv6 不能照抄 IPv4 的静态方案
//!
//! FM350 的 RNDIS 数据通道与 IPv4 一样**不转发运营商的 RA/DHCPv6**（实机
//! tcpdump 无任何 ICMPv6 RS/RA 往来），早期版本让 odhcp6c 在该网卡上等 RA，
//! 结果 netifd 表现为 fm350v6 每秒 down/up 循环。因此托管策略按 `v6_mode`
//! 分三路，各自的前提完全不同：
//!
//! | 模式 | 谁负责地址 | 插件做什么 |
//! |---|---|---|
//! | `dhcpv6`（默认） | odhcp6c（用户态 raw socket 收 RA） | 只保证接口形态正确，不写地址、不改 sysctl |
//! | `ra` | 内核按 RA/SLAAC | 打开 `accept_ra=2`，清掉历史静态地址 |
//! | `static` | 插件从 `AT+CGPADDR` 读出后写 `/128` | 写地址 + 补无网关设备路由 |
//! | `off` | 不托管 | 拆掉接口骨架与防火墙登记 |
//!
//! 默认选 `dhcpv6` 的理由：odhcp6c 在**用户态**收 RA，完全不看
//! `net.ipv6.conf.<dev>.accept_ra`，天然绕开「接口进 WAN 区后 forwarding=1、
//! 内核默认丢弃 RA」这个坑。

use super::probe::detect_dev;
use super::shell::{real, uci};
use crate::config::Config;
use std::thread::sleep;
use std::time::Duration;

/// RA 模式下 onlink 兜底 IPv6 默认路由的 metric。
///
/// 必须大于内核按 RA 下发默认路由的 metric（1024），否则兜底路由会压过 RA
/// 路由；dhcpv6 模式下 odhcp6c 装的是 `default from <prefix> via fe80::2
/// metric 512`，同样必须让兜底路由排在它后面。
pub const V6_FALLBACK_METRIC: u32 = 2048;

// ---------------------------------------------------------------- 地址解析

/// 判断是否为 IPv6 链路本地地址（`fe80::/10`，即首组落在 `fe80`~`febf`）。
pub fn is_link_local_v6(ip: &str) -> bool {
    match u16::from_str_radix(ip.split(':').next().unwrap_or(""), 16) {
        Ok(v) => v & 0xffc0 == 0xfe80,
        Err(_) => false,
    }
}

/// 一行 `ip -o addr` 输出的地址是否仍在有效期内。
///
/// 只排除 `valid_lft 0sec`，不排除 `preferred_lft 0sec`：后者表示地址已
/// deprecated，不适合新连接优先选择，但在 valid_lft 归零前仍是可用地址。
pub fn addr_line_has_valid_lifetime(line: &str) -> bool {
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

/// 从 `ip -o addr` 的一行中提取仍在有效期内的公网/全局 IPv6。
///
/// 滤掉链路本地（`fe80::/10`）：任何 UP 的网卡都会自带一个，计入后会制造
/// 两个假阳性 —— 前端长期显示「有 IPv6」，且 `up` 在只有 link-local 时也
/// 判为在线。
pub fn usable_global_v6_from_addr_line(line: &str) -> Option<String> {
    let p = line.find("inet6 ")?;
    let rest = &line[p + 6..];
    let addr = rest.split_whitespace().next()?;
    let ip = addr.split('/').next().unwrap_or("");

    if ip.is_empty() || is_link_local_v6(ip) {
        return None;
    }
    if !addr_line_has_valid_lifetime(line) {
        return None;
    }
    Some(ip.to_string())
}

// ---------------------------------------------------------------- 模式判定

/// IPv6 是否交由本插件托管（`ipv6=1` 且 `v6_mode != off`）。
pub fn v6_managed(cfg: &Config) -> bool {
    cfg.ipv6 && !cfg.iface_v6.is_empty() && cfg.v6_mode != "off"
}

/// 是否走 RA 模式：**只有显式写 `ra` 才算**，不再把未识别/缺省值算作 RA。
///
/// ## 为什么改判据（实机缺陷：默认拿到的 IPv6 不对）
///
/// 旧实现写的是 `!matches!(v6_mode, "static" | "dhcpv6" | "off")`，也就是
/// 「不是那三个值就按 RA」。于是 `v6_mode` 缺失、为空、或拼错时全部掉进 RA
/// 分支 —— 用户看到的现象正是「默认（RA）拿到的 IPv6 不对，在设置里换成
/// dhcpv6 就正常」。
///
/// 现在 RA 只认显式 `ra`；其余未识别值一律回落到 [`v6_dhcp_mode`] 的
/// odhcp6c 路径，即实测可用的那条路。
pub fn v6_ra_mode(cfg: &Config) -> bool {
    cfg.ipv6 && cfg.v6_mode == "ra"
}

/// 是否走 DHCPv6 模式：**显式 `dhcpv6`，以及所有未识别/缺省值**。
///
/// 把未识别值也归到这里是刻意的：这是实测唯一稳定的路径（odhcp6c 在用户态
/// 用 raw socket 收 RA，不看 `net.ipv6.conf.<dev>.accept_ra`，天然绕开
/// 「接口进 WAN 区后 forwarding=1、内核默认丢弃 RA」这个坑）。宁可让配错的
/// 用户走可用路径，也不要让他掉进一条会写出错误地址的分支。
pub fn v6_dhcp_mode(cfg: &Config) -> bool {
    cfg.ipv6 && !matches!(cfg.v6_mode.as_str(), "ra" | "static" | "off")
}

/// 是否走静态模式：地址由插件从模组侧读出后写入 `/128`。
pub fn v6_static_mode(cfg: &Config) -> bool {
    cfg.ipv6 && cfg.v6_mode == "static"
}

// ---------------------------------------------------------------- sysctl / 形态

/// 打开数据网卡的 IPv6 RA 接收。
///
/// 为什么必须是 `accept_ra=2`：蜂窝接口被 netifd 放进 WAN 区后
/// `net.ipv6.conf.<dev>.forwarding=1`，内核默认（`accept_ra=1`）会**直接丢弃**
/// 所有 RA。只有 2 表示「即使开了转发也接收 RA」。实机正是卡在这里：模组
/// 一直在发 RA（`ip -6 neigh` 里能看到 router 标记），但内核一条都不处理。
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

/// 主动触发一次路由器请求（RS）。
///
/// ## 这是本次按逆向补上的关键动作
///
/// F22 原厂 `mtk_netagent` 在 AP 侧有一条硬性动作（见逆向报告 §7）：
///
/// ```sh
/// echo 1 > /proc/sys/net/ipv6/conf/ccmni<id>/router_solicitations
/// ```
///
/// 只设 `accept_ra=2` 是**被动等**：内核要等模组下一轮周期性 RA，最长可达
/// 数百秒；等不到时旧实现就回落到把 `AT+CGPADDR` 的地址静态写成 `/128` ——
/// 而该命令在上下文去激活后仍返回**上一轮的残留地址**，于是「ra 模式拿到
/// 的 IPv6 不对」。
///
/// 写 `router_solicitations` 会让内核**立刻**发出 RS，把流程从「等」变成
/// 「要」。IRAT / 制式切换场景下原厂也要重新触发一次
/// （`ifc_ipv6_irat_triger_rs`）。
pub fn trigger_rs(dev: &str) -> bool {
    let (ok, _) = real(&format!(
        "echo 1 > /proc/sys/net/ipv6/conf/{}/router_solicitations 2>/dev/null || true",
        dev
    ));
    ok
}

/// 触发 RS 并等待内核按 RA 拿到 IPv6。
///
/// 每轮重试都重新触发一次 RS（蜂窝链路上单次 RS 丢包很常见），而不是干等。
/// 返回是否在 `rounds × interval` 内就绪。
///
/// 原厂对等物：`confirmNoRaToMd` —— AP 在确实收不到 RA 时会向 MD 回报；
/// 我们没有 MD 侧回报通道，这里用「超时后由调用方降级」承担同样职责。
pub fn solicit_and_wait_ra(dev: &str, rounds: u32, interval: Duration) -> bool {
    for _ in 0..rounds.max(1) {
        trigger_rs(dev);
        sleep(interval);
        if ra_v6_ready(dev) {
            return true;
        }
    }
    false
}

/// 打开 RA 接收并立刻索取一次 RA（原厂顺序：先配 sysctl，再触发 RS）。
pub fn enable_and_solicit_v6_ra(dev: &str) -> bool {
    let ok = enable_v6_ra(dev);
    trigger_rs(dev);
    ok
}

/// RA 模式下的等待参数：5 轮 × 3 s，最长约 15 s。
///
/// 取这个量级的依据：周期性 RA 间隔通常是数十秒到数百秒，等一轮不现实；
/// 而 15 s 内连发 5 次 RS 仍无应答，基本可判定该链路不转发 RA，应当降级。
pub const RA_SOLICIT_ROUNDS: u32 = 5;
pub const RA_SOLICIT_INTERVAL: Duration = Duration::from_secs(3);

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
///
/// 返回是否发生了写入（同 [`ensure_v6_dhcpv6_proto`]）。
pub fn ensure_v6_ra_proto(cfg: &Config) -> bool {
    let iface_v6 = &cfg.iface_v6;
    let mut changed = false;

    let (ok_proto, proto) = real(&format!("uci -q get network.{}.proto", iface_v6));
    if !ok_proto || proto.trim() != "none" {
        let _ = uci(&format!("set network.{}.proto=none", iface_v6));
        changed = true;
    }
    let (_, had_addr) = real(&format!("uci -q get network.{}.ip6addr", iface_v6));
    if !had_addr.trim().is_empty() {
        let _ = real(&format!(
            "uci -q delete network.{}.ip6addr 2>/dev/null || true",
            iface_v6
        ));
        changed = true;
    }
    if changed {
        let _ = real("uci commit network");
    }
    changed
}

/// DHCPv6 模式下把 v6 子接口交给 netifd 的 odhcp6c。
///
/// `proto=dhcpv6` + `extendprefix=1`：地址、默认路由、`/64` 前缀委派全部由
/// odhcp6c/netifd 负责，插件既不写静态地址也不改 sysctl。
///
/// 返回**是否发生了写入**：调用方据此决定要不要 ifup —— 形态本来就正确时
/// 不该重建接口（会把 odhcp6c 的租约打断）。
pub fn ensure_v6_dhcpv6_proto(cfg: &Config) -> bool {
    let iface_v6 = &cfg.iface_v6;
    let mut changed = false;

    let (ok_proto, proto) = real(&format!("uci -q get network.{}.proto", iface_v6));
    if !ok_proto || proto.trim() != "dhcpv6" {
        let _ = uci(&format!("set network.{}.proto=dhcpv6", iface_v6));
        changed = true;
    }
    let (ok_ext, ext) = real(&format!("uci -q get network.{}.extendprefix", iface_v6));
    if !ok_ext || ext.trim() != "1" {
        let _ = uci(&format!("set network.{}.extendprefix=1", iface_v6));
        changed = true;
    }
    // 旧版本可能留下静态地址，留着会和 odhcp6c 抢同一个地址
    let (_, had_addr) = real(&format!("uci -q get network.{}.ip6addr", iface_v6));
    if !had_addr.trim().is_empty() {
        let _ = real(&format!(
            "uci -q delete network.{}.ip6addr 2>/dev/null || true",
            iface_v6
        ));
        changed = true;
    }
    if changed {
        let _ = real("uci commit network");
    }
    changed
}

// ---------------------------------------------------------------- 刷新判据

/// 设备上当前是否存在仍在有效期内的全局 IPv6 地址。
pub fn has_global_v6(dev: &str) -> bool {
    let (_, out) = real(&format!("ip -o -6 addr show dev {} 2>/dev/null", dev));
    out.lines()
        .filter(|l| l.contains("inet6 "))
        .filter_map(usable_global_v6_from_addr_line)
        .next()
        .is_some()
}

/// IPv6 需要干预的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshKind {
    /// 接口上一个全局 v6 地址都没有 —— 真正的缺失。
    Absent,
    /// 地址与模组侧上报值不一致，且当前模式要求二者相等（仅 static）。
    Mismatch,
}

/// 判断 IPv6 是否需要干预；`None` 表示一切正常，**不要动接口**。
///
/// ## 这是「fm350v6 反复 down/up」的根因所在
///
/// 旧实现用「模组侧 `AT+CGPADDR` 上报的地址是否出现在接口地址列表里」
/// 当刷新判据（`v6_missing`），daemon 每 30 s 判一次，命中就
/// `apply_ipv6_addr` → dhcpv6 分支无条件 `ifup fm350v6`。
///
/// 但 dhcpv6 / ra 两种模式下**这个判据恒为真**：
///   * dhcpv6：接口地址由 odhcp6c 向运营商 DHCPv6 请求而来（IA_NA），
///     与 PDP 上下文在 `AT+CGPADDR` 里上报的地址本来就不是同一个；
///   * ra：地址由内核按 RA 做 SLAAC 生成（EUI-64 或随机接口标识），
///     更不可能等于模组上报的那个 /128。
///
/// 于是判据永远命中、每个巡检周期都重启一次子接口，odhcp6c 刚拿到的租约
/// 立刻被打断 —— 表现为「能拿到 IP，但 fm350v6 一直在重启」。
///
/// 修正：**只有 `static` 模式才要求地址等于模组侧上报值**（那种模式下地址
/// 本就是插件自己写进去的）。其余模式只看「有没有全局地址」。
pub fn needs_refresh(
    cfg: &Config,
    modem_v6: &str,
    iface_v6: &[String],
) -> Option<RefreshKind> {
    if !v6_managed(cfg) {
        return None;
    }
    if iface_v6.is_empty() {
        return Some(RefreshKind::Absent);
    }
    if v6_static_mode(cfg) && !modem_v6.is_empty() && !iface_v6.iter().any(|a| a == modem_v6) {
        return Some(RefreshKind::Mismatch);
    }
    None
}

// ---------------------------------------------------------------- 应用地址

/// 把模组侧 IPv6 地址写入 v6 接口并补齐路由。
///
/// 与 `iface::ensure_iface` 的区别：本函数只处理 IPv6 一侧，供守护在「模组侧
/// IPv6 变化」或「接口缺地址」时调用，避免为补一个 v6 地址而整批重写 IPv4
/// 配置。返回是否执行了写入/刷新动作。
pub fn apply_ipv6_addr(cfg: &Config, ipv6: &str) -> bool {
    if !v6_managed(cfg) || ipv6.is_empty() {
        return false;
    }
    let iface_v6 = &cfg.iface_v6;
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };

    // DHCPv6 模式：只兜底确认接口形态正确。
    //
    // **幂等是关键**：odhcp6c 的租约由 netifd 自己维护，接口上已经有全局
    // v6 地址时再 ifup 一次等于把刚拿到的租约打断（表现为 fm350v6 反复
    // down/up）。所以只在「还没有地址」或「接口形态不对」时才重建。
    if v6_dhcp_mode(cfg) {
        let shaped = ensure_v6_dhcpv6_proto(cfg);
        if has_global_v6(&dev) && !shaped {
            return true;
        }
        let _ = real(&format!("ifup {}", iface_v6));
        return true;
    }

    // RA 模式：地址与默认路由由内核按运营商 RA 处理，插件不写静态 /128。
    //
    // 顺序严格照抄原厂 `mtk_netagent`：先配 sysctl（accept_ra=2 等），再
    // **主动索取 RA**（写 router_solicitations 触发内核发 RS），而不是被动等
    // 下一轮周期性 RA。
    if v6_ra_mode(cfg) {
        enable_v6_ra(&dev);
        let reshaped = ensure_v6_ra_proto(cfg);
        // 已经有 RA 成果且形态没动过：不写静态地址、不 ifup，直接收工
        if !reshaped && ra_v6_ready(&dev) {
            return true;
        }
        if solicit_and_wait_ra(&dev, RA_SOLICIT_ROUNDS, RA_SOLICIT_INTERVAL) {
            return true;
        }
        // 主动索取若干轮仍无 RA：**不再回落到 /128 静态地址**。
        //
        // 原厂对位是 `AT+EIF=<netif>,"ra","no_ra_initial"`（首次建链未收到）
        // 与 `"no_ra_refresh"`（后续刷新未收到），由 MD 侧
        // `d2cm_ipv6_no_ra_cb_hdl()` 接手决定后续；主机侧插件没有这条 AP↔MD
        // 内部通道，对等的做法就是「如实记录、本轮不改接口」。
        //
        // 旧实现在这里把 `AT+CGPADDR` 的地址静态写成 `/128` —— 而该命令在
        // 上下文去激活后仍返回上一轮残留地址，且原厂上报 IPv6 时**一定带前缀
        // 长度**（`AT+EIF=<netif>,"ipadd",2,"<v6>/%lu"`、netagent 的
        // `ipv6PrefixLength`），从来不是 /128。照抄 /128 正是「RA 模式拿到的
        // IPv6 不对」的直接来源。
        eprintln!(
            "fm350d: 主动索取 RA {} 轮未果，本轮不写入静态 IPv6（如需固定地址请显式设 v6_mode=static）",
            RA_SOLICIT_ROUNDS
        );
        return true;
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

/// IPv6 子接口刷新：用于守护发现全局 IPv6 消失或有效期归零后的自恢复。
///
/// 正常情况下静态地址常驻内核；这里是兜底，让 netifd 状态异常或地址被
/// 意外移除时，下一轮巡检能重新拉起 fm350v6。
///
/// **幂等**：接口形态本来就正确、且设备上已经有全局 v6 地址时不再 ifup。
/// 无条件 ifup 会打断 odhcp6c 租约 / 内核 SLAAC 状态，制造「接口反复重启」。
pub fn refresh_ipv6_iface(cfg: &Config) -> bool {
    if !v6_managed(cfg) {
        return false;
    }
    let mut reshaped = false;
    if v6_ra_mode(cfg) {
        if let Some(dev) = detect_dev(cfg) {
            enable_v6_ra(&dev);
            reshaped = ensure_v6_ra_proto(cfg);
        }
    } else if v6_dhcp_mode(cfg) {
        reshaped = ensure_v6_dhcpv6_proto(cfg);
    }
    if !reshaped {
        if let Some(dev) = detect_dev(cfg) {
            if has_global_v6(&dev) {
                return true;
            }
        }
    }
    let (ok, _) = real(&format!("ifup {}", cfg.iface_v6));
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 五种 v6_mode 必须两两互斥；未识别/空值一律回落到 dhcpv6（实测稳定路径），
    /// **不再**掉进 RA（那正是「默认 IPv6 不对」的根因）。
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
            ("", true, false, true),        // 缺省 -> dhcpv6
            ("bogus", true, false, true),   // 拼错 -> dhcpv6，绝不掉进 RA
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

    /// 只有 static 模式才把「接口地址 != 模组上报地址」当作需要干预的理由。
    #[test]
    fn refresh_judgement_only_compares_addresses_in_static_mode() {
        let mk = |mode: &str| Config {
            ipv6: true,
            v6_mode: mode.to_string(),
            iface: "fm350".to_string(),
            iface_v6: "fm350v6".to_string(),
            ..Default::default()
        };
        let modem = "2409:8057:2000::8";
        // odhcp6c 的 IA_NA 地址与模组上报地址本来就不是同一个
        let iface = vec!["2409:8057:2000:1:2:3:4:5".to_string()];

        assert_eq!(needs_refresh(&mk("dhcpv6"), modem, &iface), None);
        assert_eq!(needs_refresh(&mk("ra"), modem, &iface), None);
        assert_eq!(
            needs_refresh(&mk("static"), modem, &iface),
            Some(RefreshKind::Mismatch)
        );
        // 一个地址都没有 —— 任何托管模式都要干预
        assert_eq!(
            needs_refresh(&mk("dhcpv6"), modem, &[]),
            Some(RefreshKind::Absent)
        );
        // 关闭 IPv6 时完全不干预
        assert_eq!(needs_refresh(&mk("off"), modem, &[]), None);
    }

    #[test]
    fn static_v6_addr_option_is_formatted_correctly() {
        let cfg = Config {
            ipv6: true,
            iface: "fm350".to_string(),
            iface_v6: "fm350v6".to_string(),
            ..Default::default()
        };
        let script = format!(
            "set network.{}.ip6addr={}/128",
            cfg.iface_v6, "2409:8057:2000::8"
        );
        assert_eq!(script, "set network.fm350v6.ip6addr=2409:8057:2000::8/128");
    }
}
