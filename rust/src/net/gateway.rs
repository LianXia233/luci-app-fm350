//! 网关决策：规划 → 实测 → 回退。
//!
//! FM350 的 RNDIS 数据通道**不提供 DHCP**，IPv4 必须以静态地址配置：
//! 地址取自 `AT+CGPADDR`、DNS 取自 `AT+GTDNS`，掩码与网关由 `gateway_mode`
//! 决定（默认 `auto`）。
//!
//! ## 为什么非要有网关这一层
//!
//! 无网关的 onlink 方案（`default dev <dev> onlink`）下，主机对**每一个公网
//! IP**都要直接发 ARP 请求，完全依赖模组做 ARP 代理。部分运营商/固件下模组
//! 只对自己的网关 IP 应答 ARP，于是表现为「PDP 已激活、有 IP 有 DNS，却一个
//! 包都发不出去」（tx_errors 持续上涨、rx 恒为 0、内核刷 NETDEV WATCHDOG）。
//!
//! ## 三段式时序（顺序不能变）
//!
//! 1. [`plan_gateway`] —— 纯计算，只决定掩码与候选网关；
//! 2. 地址落进内核（由 `iface::ensure_iface` + `ifup` 完成）；
//! 3. [`finalize_gateway`] —— 此时才做 ARP 实测，失败则 [`fallback_to_onlink`]。
//!
//! 实测不能提前：`ifup` 是异步的，紧跟其后的固定等待经常在地址就绪前探测，
//! **必失败**，会把本来可用的网关误判为不可达并回退掉。

use super::shell::{real, sq};
use crate::addr::is_valid_ipv4;
use crate::config::Config;
use std::fs;
use std::thread::sleep;
use std::time::Duration;

/// 判断 IPv4 是否落在「可安全推导同网段网关」的地址段。
///
/// 只覆盖私有地址与运营商 CGNAT 段（10/8、172.16/12、192.168/16、100.64/10）。
/// 公网地址不做推导：蜂窝网络下公网 IP 的网关极少是同网段 `.1`，盲目推导
/// 会写入一条错误网关，反而把原本可用的 onlink 直连彻底堵死。
pub fn is_private_or_cgnat(ip: &str) -> bool {
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

/// 用户显式配置了掩码就用用户的，否则用该模式下的默认掩码。
fn mask_or(cfg: &Config, dflt: &str) -> String {
    if cfg.netmask.is_empty() {
        dflt.to_string()
    } else {
        cfg.netmask.clone()
    }
}

/// 模组上报网关的采信条件：本身是合法 IPv4，且不等于本机地址
/// （`AT+CGCONTRDP` 偶发回 `0.0.0.0` 或与 PDP 地址相同的占位值）。
fn valid_modem_gw(modem_gw: &str, ipv4: &str) -> Option<String> {
    if is_valid_ipv4(modem_gw) && modem_gw != ipv4 {
        Some(modem_gw.to_string())
    } else {
        None
    }
}

/// 按掩码判断 `gw` 是否与 `ip` 同网段。
///
/// 任一侧解析失败（含掩码非法）都按「不同网段」处理 —— 调用方随后走更
/// 安全的 `<gw>/32` 主机路由路径，宁可多装一条主机路由也不能让 netifd 的
/// `default via <gw>` 被内核以「网关不可达」拒绝。
pub fn in_same_subnet(ip: &str, gw: &str, mask: &str) -> bool {
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
/// auto 模式**优先采信模组上报值**（`modem_gw` 来自 `AT+CGCONTRDP` 字段位 4），
/// 仅在模组未报时才回退到「同网段 .1」启发式 —— 运营商真实网关并不总是 `.1`。
/// 模组上报网关与地址不同网段时掩码仍按 `/24` 规划，由 `iface::ensure_iface`
/// 另装 `<gw>/32` 主机路由保证网关可达。
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
    let _ = real(&format!("ping -c 1 -W 2 -I {} {} >/dev/null 2>&1", dev, gw));
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

// ---------------------------------------------------------------- 参数签名

/// 影响 netifd 运行态的网络参数签名。
///
/// 裸 `uci set`（CLI、ubus ucode、外部脚本）只写 UCI delta，不会触发 netifd
/// 重读（`config.change` 只由 rpcd/LuCI 应用路径发出，见 OpenWrt ticket
/// #17305）；网络包也不是 procd 管的服务，本插件的 procd reload 触发不到它。
/// 把关键参数做成签名、与「最近一次成功下发时的快照」比对，发现不一致且
/// 接口上有地址时强制重新下发。
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

// ---------------------------------------------------------------- 实测与回退

/// 网关 ARP 实测与回退。**必须在地址已落进内核之后调用。**
pub fn finalize_gateway(cfg: &Config, dev: &str, ipv4: &str, modem_gw: &str) {
    let (_, plan_gw) = plan_gateway(cfg, ipv4, modem_gw);
    if let Some(gw) = plan_gw {
        sleep(Duration::from_millis(500));
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
pub fn fallback_to_onlink(cfg: &Config, dev: &str, gw: &str) {
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

/// DNS 是否真的写进了 UCI。
///
/// 调用方给了 DNS 却没写进去（批量静默失败 / 被 revert）时必须报错，
/// 否则 resolver 会停在陈旧 DNS 上，表现为「能 ping 通 IP 但打不开网页」。
pub fn dns_matches(cfg: &Config, dns: &[String]) -> Result<(), String> {
    if dns.is_empty() {
        return Ok(());
    }
    let cur = configured_dns(cfg);
    let missing: Vec<&String> = dns
        .iter()
        .filter(|d| !cur.iter().any(|x| x.as_str() == d.as_str()))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "地址已就位但 DNS 配置缺失（期望含 {:?}，uci 实际 {:?}）",
            missing, cur
        ))
    }
}

/// 把 DNS 列表拼成 uci 多值写法（整体加引号，见 [`sq`] 的必要性说明）。
pub fn dns_value(dns: &[String]) -> String {
    sq(&dns.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        let mut c = Config::default();
        c.ipv6 = false;
        c
    }

    #[test]
    fn gateway_is_derived_only_for_private_or_cgnat() {
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
        let auto = Config {
            gateway_mode: "auto".into(),
            ..base()
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
            ..base()
        };
        assert_eq!(plan_gateway(&off, "10.8.217.45", "").1, None);
        assert_eq!(plan_gateway(&off, "10.8.217.45", "").0, "255.255.255.255");

        let st = Config {
            gateway_mode: "static".into(),
            gateway: "10.8.217.254".into(),
            ..base()
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
            ..base()
        };
        // 模组上报的网关优先于「同网段 .1」推导
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "10.8.217.254").1.as_deref(),
            Some("10.8.217.254")
        );
        // 占位网关 0.0.0.0 不采信，回退推导
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "0.0.0.0").1.as_deref(),
            Some("10.8.217.1")
        );
        // 模组网关 == 本机地址：不采信
        assert_eq!(
            plan_gateway(&auto, "10.8.217.45", "10.8.217.45").1.as_deref(),
            Some("10.8.217.1")
        );
        // 公网地址 + 模组上报网关：采信上报值
        let (m, g) = plan_gateway(&auto, "36.112.8.10", "36.112.8.1");
        assert_eq!(g.as_deref(), Some("36.112.8.1"));
        assert_eq!(m, "255.255.255.0");
        // 异网段网关仍采信（主机路由由 ensure_iface 补）
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
        assert!(!in_same_subnet("10.8.217", "10.8.217.1", "255.255.255.0"));
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
    fn dns_value_is_single_quoted_as_one_argument() {
        let dns = vec!["223.5.5.5".to_string(), "119.29.29.29".to_string()];
        assert_eq!(dns_value(&dns), "'223.5.5.5 119.29.29.29'");
    }
}
