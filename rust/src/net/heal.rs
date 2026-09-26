//! 路由守卫与自愈：把「配置写对了但就是不通」的现场拉回来。
//!
//! ## 为什么需要这一层
//!
//! netifd **不会**为无网关接口下发设备路由；周期性 RA 会被 ifdown/ifup 冲掉；
//! USB 数据端点会偶发 stall。这些都不是「配置写错」，而是运行态漂移，
//! 只能靠周期巡检补。
//!
//! ## 自愈强度分级（不要一上来就重启）
//!
//! | 级别 | 动作 | 适用 | 副作用 |
//! |---|---|---|---|
//! | 1 | 补/删路由 | 路由缺失或残留 | 无 |
//! | 2 | `ifdown/ifup` 单个接口 | v4 或 v6 单侧不通 | 该侧重新协商 |
//! | 3 | `ip link` 硬复位数据网卡 | 地址/路由漂移（netdev 层） | v6 一并失联，需拉回 |
//! | 4 | USB 驱动解绑重绑 | 端点 stall（两类形态见 [`data_plane_stalled`]） | netdev 重建、可能改名 |
//! | 5 | 去激活重拨 / 重启模组 | 以上全部无效 | 双栈中断，甚至整机复位 |
//!
//! 判定顺序必须是 1 → 2 → 3 → 4，见 [`reset_usb_data_dev`]、[`bounce_data_dev`]
//! 与 [`data_plane_stalled`]。

use std::thread::sleep;
use std::time::Duration;

use super::gateway::{current_gateway, dns_matches, finalize_gateway, mark_applied};
use super::iface::{ensure_iface, status, NetStatus};
use super::probe::detect_dev;
use super::shell::real;
use super::v6::{
    enable_and_solicit_v6_ra, has_global_v6, v6_dhcp_mode, v6_managed, v6_ra_mode, v6_static_mode,
    V6_FALLBACK_METRIC,
};
use crate::config::Config;

/// 判断一行 `ip route` 输出是否匹配给定 metric。
///
/// 两个坑（实机踩出来的）：
///   * `ip route` 在 metric=0 时**不打印** `metric` 字段，按字面子串永远匹配不到，
///     路由每次巡检都被误报缺失、又每轮 `replace` 刷一条日志；
///   * 必须按词法单元比较 —— 子串 `metric 3` 会误命中 `metric 30`。
pub fn line_has_metric(line: &str, metric: u32) -> bool {
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
            // RA 模式下每轮兜底打开 accept_ra 并**主动索取一次 RS**：USB 复位 /
            // 网卡重建后 sysctl 会回到默认值，而 forwarding=1 时默认值 0 会让
            // 内核彻底丢弃 RA；只设 sysctl 不触发 RS 又要等下一轮周期性 RA。
            enable_and_solicit_v6_ra(&dev);
        }
        // dhcpv6 模式不改任何 sysctl：路由与地址归 odhcp6c/netifd 管。
        let (_, routes6) = real(&format!("ip -6 route show dev {} 2>/dev/null", dev));

        // ra/dhcpv6 模式：任何写在 cfg.metric 上的**无网关**默认设备路由都是
        // 历史残留（旧版曾无条件安装），必须无条件清理 —— 不能等「有全局地址」
        // 才动：SLAAC/DHCPv6 地址到位之前它就在压制 RA/odhcp6c 下发的路由。
        // 与兜底 metric 相同时是本函数自己维护的路由，跳过。
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
        if has_global_v6(&dev) {
            // RA 路由由内核按 RA 下发，metric 固定 1024（`proto ra`）。这里维护
            // 的 onlink 路由只是**兜底**：metric 取 2048，比 RA 差，所以 RA 在时
            // 由 RA 优先，RA 还没来（或 netifd 重启把 RA 路由冲掉、到下一轮 RA
            // 到达之间）时由它兜住流量。
            //
            // 为什么不直接删掉 onlink 路由：实机观察到 RA 路由会被 ifdown/ifup
            // 冲掉、且要等下一次 RA（最长数百秒）才回来，删除会造成「默认路由
            // 真空」，`ping -6` 直接报 Network unreachable。
            // 为什么不把 onlink 放在低 metric：那会压过 RA 路由，强制主机对
            // 每个目的地址直接发 NS，完全依赖模组做 NDP 代理。
            // dhcpv6 模式下 odhcp6c 装的是 `default from <prefix> via fe80::2
            // metric 512`，同样必须让兜底路由排在它后面。
            let metric6 = if ra || dhcp { V6_FALLBACK_METRIC } else { cfg.metric };
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

/// 复位 USB 数据端点：解绑并重新绑定承载数据网卡的 USB 驱动。
///
/// ## 为什么必须单独有这一级
///
/// [`bounce_data_dev`] 只做 `ip link down/up`，动的是 netdev 层：它既清不掉
/// USB 端点的 halt 状态，也重建不了 usbnet 的 URB 队列。而数据面 stall 的根因
/// 恰恰在 USB 端点，所以 bounce 对这类故障**结构性无效**。
///
/// 实机证据（FM350-GL，驱动 rndis_host，控制接口 2-1:1.0 + 数据接口 2-1:1.1）：
///   * 只 bounce（含反复执行）：`tx_errors` 一路涨到 787、`tx_packets` 停在 1、
///     `rx` 恒为 0，链路持续不通；
///   * 改为驱动 unbind/bind 一次：netdev 立即重建（ifindex 变化）、计数归零，
///     ARP 状态由 FAILED 变为 INCOMPLETE —— 说明请求确实发出去了，主机侧
///     驱动状态已被清干净。
///
/// ## 风险与必须处理的副作用
///
/// 解绑期间数据网卡会短暂消失，且 netdev 名**可能变化**（内核按最小可用编号
/// 重新分配，RNDIS 通常仍拿回原名，但不能依赖）。因此绑定后必须重新识别名字
/// 并把 uci 的 `device` 同步成新名，否则 netifd 会一直盯着一个已消失的设备。
///
/// 另注：当 uci 的 `data_dev` 被配成固定设备名（非 `auto`）时，[`detect_dev`]
/// 不会重新扫描，改名就检测不到 —— 这是既有行为，使用固定名时应避免走到
/// 这一级，或改用 `auto`。
pub fn reset_usb_data_dev(cfg: &Config) -> bool {
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };

    // 1) 由 netdev 反查它挂在哪个 USB 接口、由哪个驱动承载。
    let (ok_if, iface) = real(&format!(
        "readlink -f /sys/class/net/{}/device 2>/dev/null | sed 's#.*/##'",
        dev
    ));
    let iface = iface.trim().to_string();
    if !ok_if || iface.is_empty() {
        return false;
    }
    let (_, drv) = real(&format!(
        "basename $(readlink -f /sys/class/net/{}/device/driver 2>/dev/null) 2>/dev/null",
        dev
    ));
    let drv = drv.trim().to_string();
    if drv.is_empty() {
        return false;
    }

    let bus = format!("/sys/bus/usb/drivers/{}", drv);

    // 2) 解绑。驱动会连带释放它 claim 的伙伴接口（RNDIS 的数据接口），
    //    所以只需对控制接口操作一次。
    let _ = real(&format!("echo {} > {}/unbind 2>/dev/null", iface, bus));
    sleep(Duration::from_secs(2));

    // 3) 重新绑定。绑定偶发失败（实机第 1 次即成功，但必须留重试），
    //    失败时重复写入即可，不要据此判定设备已死。
    let mut bound = false;
    for _ in 0..3 {
        let _ = real(&format!("echo {} > {}/bind 2>/dev/null", iface, bus));
        sleep(Duration::from_secs(4));
        let (ok_now, cur) = real(&format!(
            "basename $(readlink -f /sys/class/net/{}/device/driver 2>/dev/null) 2>/dev/null",
            dev
        ));
        if ok_now && cur.trim() == drv {
            bound = true;
            break;
        }
    }
    if !bound {
        return false;
    }

    // 4) 重新识别 netdev 名并同步 uci，避免 netifd 盯错设备。
    let new_dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };
    if new_dev != dev {
        let _ = real(&format!(
            "uci -q set network.{}.device={}; uci -q commit network",
            cfg.iface, new_dev
        ));
    }

    // 5) 回到 netifd：由它按 UCI 重新下发地址与路由（裸 up 不会恢复配置）。
    let (ok, _) = real(&format!("ifup {}", cfg.iface));
    if v6_managed(cfg) {
        let _ = real(&format!("ifup {}", cfg.iface_v6));
        // RA 模式：链路重建后 sysctl 可能已回默认，必须重新打开并主动发 RS。
        if v6_ra_mode(cfg) {
            enable_and_solicit_v6_ra(&new_dev);
        }
    }
    ok
}

/// 轻量复位数据网卡：只 down/up 网卡并重新 ifup，不动基带、不重启模组。
///
/// 适用于**地址/路由漂移**一类 netdev 层问题。对 USB 端点 stall 无效 ——
/// 那种情况请用 [`reset_usb_data_dev`]（stall 的根因在 USB 层，down/up 够不着）。
pub fn bounce_data_dev(cfg: &Config) -> bool {
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };
    // 先走 netifd 的 ifdown：让它同步移除地址/路由并把运行态置 down，否则裸
    // `ip link set down` 会造成「内核链路 down、netifd 仍以为 up」的状态漂移。
    let _ = real(&format!("ifdown {}", cfg.iface));
    if v6_managed(cfg) {
        let _ = real(&format!("ifdown {}", cfg.iface_v6));
    }
    // 硬复位数据端点（stall 自愈的核心动作）：即便 ifdown 因异常没能落下链路，
    // 这里也强制拉低；命令幂等，链路已 down 时无副作用。
    let _ = real(&format!("ip link set {} down", dev));
    sleep(Duration::from_secs(2));
    let _ = real(&format!("ip link set {} up", dev));
    sleep(Duration::from_secs(1));
    // 回到 netifd：由它按 UCI 重新下发地址与路由（裸 up 不会恢复配置）。
    let (ok, _) = real(&format!("ifup {}", cfg.iface));
    // 数据面复位会让 v6 子接口（device=@主接口）跟着失联，这里一并拉回，
    // 否则 v6 侧要等下一轮 refresh 才恢复。
    if v6_managed(cfg) {
        let _ = real(&format!("ifup {}", cfg.iface_v6));
        // RA 模式：链路重建后 sysctl 可能已回默认，必须重新打开并主动发 RS。
        if v6_ra_mode(cfg) {
            enable_and_solicit_v6_ra(&dev);
        }
    }
    ok
}

/// 轻量重建 IPv4 侧接口：只 ifdown/ifup 主接口，不碰数据网卡链路与 v6 子接口。
///
/// 与 [`bounce_data_dev`] 的分工：端点 stall 需要 USB 驱动层复位（见
/// [`reset_usb_data_dev`]）；而公网连通性丢失且 v6 独立存活时，上述任何
/// 硬复位都会误伤正常的 v6 —— 这里只让 netifd 重新下发 v4 地址/路由/网关，
/// v6 子接口保持原状。
pub fn bounce_iface_v4(cfg: &Config) -> bool {
    let _ = real(&format!("ifdown {}", cfg.iface));
    sleep(Duration::from_secs(2));
    let (ok, _) = real(&format!("ifup {}", cfg.iface));
    ok
}

/// 轻量重建 IPv6 侧子接口：只 ifdown/ifup v6 接口，主接口与 v4 完全不受影响。
///
/// dhcpv6 模式下该动作等价于重启 odhcp6c —— 重新发 SOLICIT、重新请求前缀，
/// 是「有 v6 地址但 v6 公网不可达」时最对症的恢复；ra 模式下重新触发内核
/// RS/RA 流程。返回 ifup 是否成功。
pub fn bounce_iface_v6(cfg: &Config) -> bool {
    if !v6_managed(cfg) {
        return false;
    }
    if v6_dhcp_mode(cfg) {
        let _ = super::v6::ensure_v6_dhcpv6_proto(cfg);
    } else if v6_ra_mode(cfg) {
        if let Some(dev) = detect_dev(cfg) {
            enable_and_solicit_v6_ra(&dev);
        }
        let _ = super::v6::ensure_v6_ra_proto(cfg);
    }
    let _ = real(&format!("ifdown {}", cfg.iface_v6));
    sleep(Duration::from_secs(2));
    let (ok, _) = real(&format!("ifup {}", cfg.iface_v6));
    ok
}

/// 拨号成功后一次性把网络拉起。
///
/// 地址族校验规则：
///   * 给了 v4 —— 必须出现在内核里；
///   * 给了 v6 且为 **static 模式** —— 也必须出现（地址由我们自己写）；
///   * ra / dhcpv6 模式 —— v6 由内核 RA / odhcp6c 后续下发，不在此卡关，
///     缺失由守护的 `needs_refresh` / 恢复分支自愈。
/// 两个族都为空才直接报错（v6-only 会话曾被「必须有 v4」的门槛挡死，守护在
/// active 分支里无限空转）。
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

    // `ifup` 是异步的：netifd 受理后，地址要过一会儿才落进内核。直接读一次
    // status() 常常读到空地址，于是把「刚拨上」误报成失败；反过来，网卡名配错
    // 导致根本没起来时，又会被当成成功。因此这里有限轮询等地址落地：
    // 通常 <500 ms 即返回，最多等 3 s。
    let v6_enforced = !ipv6.is_empty() && v6_static_mode(cfg);
    let ok_v4 = |last: &NetStatus| ipv4.is_empty() || last.ipv4.iter().any(|a| a == ipv4);
    let ok_v6 = |last: &NetStatus| !v6_enforced || last.ipv6.iter().any(|a| a == ipv6);
    let mut last = status(cfg);
    for _ in 0..12 {
        if ok_v4(&last) && ok_v6(&last) {
            break;
        }
        sleep(Duration::from_millis(250));
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

    // 地址已落地：此时才做网关 ARP 实测 —— ifup 异步，紧跟其后的固定等待
    // 经常在地址就绪前探测，必失败，会把会话误判为「网关不可达」并回退到
    // onlink 方案。
    if !ipv4.is_empty() {
        if let Some(dev) = detect_dev(cfg) {
            finalize_gateway(cfg, &dev, ipv4, modem_gw);
            // fallback 可能触发了 ifup，重新读一次状态（下方直接返回它）
            last = status(cfg);
        }
    }

    // DNS 校验：调用方给了 DNS 却没写进 UCI（批量静默失败/被 revert）时必须
    // 报错，否则 resolver 停在陈旧 DNS 上。
    if !dns.is_empty() {
        dns_matches(cfg, dns)?;
    }

    mark_applied(cfg);
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// metric=0 时 `ip route` 根本不打印 metric 字段——必须按这个语义判定。
    #[test]
    fn metric_zero_means_absent_field() {
        assert!(line_has_metric("default dev eth2 scope link", 0));
        assert!(!line_has_metric("default dev eth2 scope link", 30));
    }

    /// 必须按词法单元比较：子串 `metric 3` 不能命中 `metric 30`。
    #[test]
    fn metric_matches_whole_token_only() {
        assert!(line_has_metric("default dev eth2 metric 30 ", 30));
        assert!(!line_has_metric("default dev eth2 metric 30 ", 3));
        assert!(!line_has_metric("default dev eth2 metric 300", 30));
    }

    /// 兜底 metric 必须严格大于 RA 的 1024 与 odhcp6c 的 512，否则会压过它们。
    #[test]
    fn fallback_metric_outranks_both_ra_and_dhcpv6() {
        assert!(V6_FALLBACK_METRIC > 1024, "RA 默认路由 metric 为 1024");
        assert!(V6_FALLBACK_METRIC > 512, "odhcp6c 默认路由 metric 为 512");
        assert_eq!(V6_FALLBACK_METRIC, 2048);
    }
}
