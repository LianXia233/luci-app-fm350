//! 强制 DNS：拨号成功后把固定公共 DNS 写进插件创建的接口。
//!
//! ## 为什么需要覆盖模组 DNS
//!
//! FM350 在 `AT+GTDNS` / `AT+CGCONTRDP` 里上报的是运营商 DNS。实机上常见
//! 三种问题：跨网解析慢、被运营商劫持、IPv6 DNS 只给一个甚至不给。插件
//! 创建的接口本来就用 `peerdns=0` 关闭了 peer DNS，若不显式写 `dns`，
//! resolver 只能回落到其他接口的上游。
//!
//! 这里把「配置项 → 实际下发列表」的规则集中到一处：
//!
//! * `dns_force=1`（默认）时，接口 DNS **只**用 `dns4_*` / `dns6_*` 的值，
//!   模组上报值仅作为两者都为空时的兜底；
//! * `dns_force=0` 时保持旧行为，沿用模组上报的 DNS；
//! * 主接口承载 IPv4 + IPv6 两组服务器；IPv6 子接口只写 IPv6 组，
//!   保证 `resolv.conf.auto` 在 v4-only / v6-only 会话下都拿得到可用上游；
//! * 非法或空配置项被静默跳过，绝不把脏值写进 UCI（dnsmasq 会因此启动失败）。
//!
//! 下发的落点是 `network.<iface>.dns`（空格分隔的多值）。netifd 的 static
//! 协议会把它转成 `proto_add_dns_server`，`peerdns=0` 则阻止运营商 DNS 混入。

use crate::addr::{is_valid_ipv4, normalize_ipv6};
use crate::config::Config;

/// 去重且保持原顺序（配置项顺序即解析优先级）。
fn dedup(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let item = item.trim().to_string();
        if !item.is_empty() && !out.iter().any(|x| x == &item) {
            out.push(item);
        }
    }
    out
}

/// 配置中的 IPv4 强制 DNS（按首选、备用顺序）。
///
/// 仅 `dns_force=1` 时有意义；非法项被过滤。
pub fn forced_v4(cfg: &Config) -> Vec<String> {
    if !cfg.dns_force {
        return Vec::new();
    }
    dedup(
        [&cfg.dns4_primary, &cfg.dns4_secondary]
            .into_iter()
            .filter(|s| is_valid_ipv4(s.trim()))
            .map(|s| s.trim().to_string()),
    )
}

/// 是否存在受管的 IPv6 通道（与 `net::v6::v6_managed` 的判据保持一致）。
///
/// 这里不直接调用 `v6_managed`，避免 `dns` 与 `v6` 两个模块互相依赖；
/// 判据本身很简单，重复一份比引入环更清晰。
fn v6_active(cfg: &Config) -> bool {
    cfg.ipv6 && cfg.v6_mode != "off" && !cfg.iface_v6.is_empty()
}

/// 配置中的 IPv6 强制 DNS（按首选、备用顺序）。
///
/// `ipv6=0` 或 `v6_mode=off` 时返回空：没有受管的 IPv6 通道时，
/// 下发不可达的 v6 上游只会给解析器增加无谓的超时。
pub fn forced_v6(cfg: &Config) -> Vec<String> {
    if !cfg.dns_force || !v6_active(cfg) {
        return Vec::new();
    }
    dedup(
        [&cfg.dns6_primary, &cfg.dns6_secondary]
            .into_iter()
            .filter_map(|s| normalize_ipv6(s.trim())),
    )
}

/// 主接口要下发的完整 DNS 列表（IPv4 在前、IPv6 在后）。
pub fn forced_all(cfg: &Config) -> Vec<String> {
    let mut all = forced_v4(cfg);
    all.extend(forced_v6(cfg));
    dedup(all)
}

/// 主接口最终使用的 DNS：强制模式覆盖模组值；未启用或强制列表为空时沿用模组值。
pub fn effective(cfg: &Config, modem_dns: &[String]) -> Vec<String> {
    if cfg.dns_force {
        let forced = forced_all(cfg);
        if !forced.is_empty() {
            return forced;
        }
    }
    dedup(modem_dns.iter().cloned())
}

/// IPv6 子接口最终使用的 DNS。
///
/// 强制模式下即使 v6 配置为空也**不回落**到运营商 DNS（否则「强制」失效）；
/// 此时主接口仍持有完整列表，由主接口兜底。非强制模式沿用模组 v6 DNS。
pub fn effective_v6(cfg: &Config, modem_dns: &[String]) -> Vec<String> {
    if cfg.dns_force {
        return forced_v6(cfg);
    }
    dedup(
        modem_dns
            .iter()
            .filter(|s| normalize_ipv6(s).is_some())
            .cloned(),
    )
}

/// 两组 DNS 是否等价（顺序无关，重复项忽略）。
pub fn same_servers(a: &[String], b: &[String]) -> bool {
    let a = dedup(a.iter().cloned());
    let b = dedup(b.iter().cloned());
    a.len() == b.len() && a.iter().all(|x| b.contains(x))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    /// 默认值必须就是需求里约定的四条。
    #[test]
    fn defaults_match_the_required_public_dns() {
        let c = cfg();
        assert_eq!(
            forced_v4(&c),
            vec!["119.29.29.29".to_string(), "223.5.5.5".to_string()]
        );
        assert_eq!(
            forced_v6(&c),
            vec!["2402:4e00::".to_string(), "2400:3200::1".to_string()]
        );
        assert_eq!(forced_all(&c).len(), 4);
    }

    /// 强制模式必须覆盖模组/运营商 DNS，而不是追加。
    #[test]
    fn forced_dns_overrides_modem_dns() {
        let c = cfg();
        let modem = vec![
            "10.0.0.1".to_string(),
            "2409:8057::1".to_string(),
        ];
        assert_eq!(effective(&c, &modem), forced_all(&c));
        assert_eq!(effective_v6(&c, &modem), forced_v6(&c));
    }

    /// 关闭强制后回到旧行为：只去重，不改写。
    #[test]
    fn disabled_mode_keeps_modem_dns() {
        let mut c = cfg();
        c.dns_force = false;
        let modem = vec!["10.0.0.1".to_string(), "10.0.0.1".to_string()];
        assert_eq!(effective(&c, &modem), vec!["10.0.0.1".to_string()]);
    }

    /// 空配置项与非法地址不得进入 UCI。
    #[test]
    fn invalid_entries_are_filtered() {
        let mut c = cfg();
        c.dns4_primary = "".into();
        c.dns4_secondary = "999.1.1.1".into();
        c.dns6_primary = "not-v6".into();
        c.dns6_secondary = "".into();
        assert!(forced_v4(&c).is_empty());
        assert!(forced_v6(&c).is_empty());
        // 强制列表全空时回落到模组 DNS，避免把 resolver 清空
        let modem = vec!["1.1.1.1".to_string()];
        assert_eq!(effective(&c, &modem), modem);
    }

    /// 关闭 IPv6 后不再下发 v6 上游。
    #[test]
    fn ipv6_disabled_drops_v6_servers() {
        let mut c = cfg();
        c.ipv6 = false;
        assert!(forced_v6(&c).is_empty());
        assert_eq!(forced_all(&c).len(), 2);
    }

    /// `v6_mode=off` 同样没有受管 v6 通道，不能下发不可达的上游。
    #[test]
    fn v6_mode_off_drops_v6_servers() {
        let mut c = cfg();
        c.v6_mode = "off".into();
        assert!(forced_v6(&c).is_empty());
        assert_eq!(forced_all(&c).len(), 2);
    }

    #[test]
    fn duplicate_servers_are_removed_order_kept() {
        let mut c = cfg();
        c.dns4_secondary = "119.29.29.29".into();
        assert_eq!(forced_v4(&c), vec!["119.29.29.29".to_string()]);
    }

    #[test]
    fn same_servers_ignores_order_and_duplicates() {
        assert!(same_servers(
            &["1.1.1.1".to_string(), "2.2.2.2".to_string()],
            &["2.2.2.2".to_string(), "1.1.1.1".to_string(), "1.1.1.1".to_string()]
        ));
        assert!(!same_servers(
            &["1.1.1.1".to_string()],
            &["1.1.1.1".to_string(), "2.2.2.2".to_string()]
        ));
    }
}
