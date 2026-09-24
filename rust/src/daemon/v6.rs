//! IPv6 维护工位：模组侧轮询、刷新时机判定、恢复动作分级。
//!
//! ## 这里修掉的是两个实机缺陷
//!
//! 1. **「默认拿到的 IPv6 不对」**：见 [`crate::net::v6::v6_ra_mode`] —— 旧实现
//!    把「既不是 static 也不是 dhcpv6 也不是 off」的一切值（含空值、拼错的
//!    值）统统判成 RA，于是默认配置走了一条用户没选过的路径；且 RA 分支在
//!    被动等待失败后会把 `AT+CGPADDR` 的地址当 `/128` 静态写下去，而原厂上报
//!    IPv6 时**一定带前缀长度**（`AT+EIF=<netif>,"ipadd",2,"<v6>/%lu"`），
//!    且该命令在去激活后仍返回上一轮残留地址 —— 组合起来就是「能拿到地址，
//!    但地址是错的」。
//! 2. **「fm350v6 接口频繁重启」**：见 [`crate::net::v6::needs_refresh`] ——
//!    旧判据要求「模组上报地址必须出现在接口地址列表里」，而这在 dhcpv6
//!    （odhcp6c 的 IA_NA）与 ra（内核 SLAAC）两种模式下**恒不成立**，于是每个
//!    巡检周期都命中一次 `ifup fm350v6`，刚拿到的租约立刻被打断。
//!
//! 本模块负责「什么时候该动 IPv6、动到什么程度」的**时机与判据**，
//! 具体怎么动（写地址 / 刷接口 / 索取 RA）由 [`crate::net::v6`] 实现。
//!
//! ## 逆向依据
//!
//! 原厂 `mtk_netagent` 对 IPv6 的处理是**事件驱动 + 主动索取**，不是定时
//! 重刷：它在 `ifc_ipv6_irat_triger_rs` 里写
//! `/proc/sys/net/ipv6/conf/ccmni<id>/router_solicitations` 主动触发 RS，
//! 并用 `IPv6 address lost after IRAT` 这类事件做补偿。因此本模块里所有
//! 「定时」都是**兜底**，正常路径应当由地址变化事件驱动。

use std::time::{Duration, Instant};

use crate::config::Config;
use crate::{infof, warnf};
use crate::modem::PdpState;
use crate::net;
use crate::net::NetStatus;

/// 模组侧 IPv6 的最短轮询间隔：配置项允许更小的值，但本机不低于此值。
///
/// 每次轮询都要走一遍 `AT+CGPADDR`，而这条命令在上下文去激活后仍返回上一轮
/// 残留地址（见 [`crate::modem::pdp`] 模块注释），因此它的**可信度有限** ——
/// 轮询越密，误判机会越多，收益却几乎为零。
pub const V6_MODEM_POLL_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// 恢复刷新的常规间隔（确实见过 v6 之后）。
pub const V6_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// 「该有 v6 却从没见过」时的恢复刷新静默期。
///
/// 纯 IPv4 环境（运营商不发 v6、或 RA 始终不下发）下，若按 60 s 一次
/// `ifup fm350v6`，日志会每分钟刷一条「未发现有效全局 IPv6」，接口也跟着
/// 每分钟弹一次。从未见过 v6 时判定为环境如此，拉长到 30 分钟只做兜底。
pub const V6_SILENT_RECOVERY_INTERVAL: Duration = Duration::from_secs(1800);

/// IPv6 维护的跨轮状态。
pub struct V6Watch {
    /// 上次执行恢复刷新的时刻（`None` = 从未执行过，首轮视为到期）。
    last_refresh: Option<Instant>,
    /// 上次轮询模组侧 IPv6 的时刻。
    last_modem_poll: Option<Instant>,
    /// 上次定时刷新的时刻。
    last_scheduled: Instant,
    /// 上次观测到的模组侧 IPv6（用于发现变化）。
    last_modem_ipv6: String,
    /// 是否在模组侧观察到过 v6。
    modem_seen: bool,
    /// 是否在接口侧观察到过 v6。
    net_seen: bool,
}

impl Default for V6Watch {
    fn default() -> Self {
        Self {
            last_refresh: None,
            last_modem_poll: None,
            last_scheduled: Instant::now(),
            last_modem_ipv6: String::new(),
            modem_seen: false,
            net_seen: false,
        }
    }
}

impl V6Watch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录模组侧上报的 IPv6（用于「是否见过 v6」的分级）。
    pub fn note_modem(&mut self, v6: &str) {
        if !v6.is_empty() {
            self.modem_seen = true;
        }
    }

    /// 记录接口侧持有的 IPv6 列表（同上）。
    pub fn note_iface(&mut self, v6: &[String]) {
        if !v6.is_empty() {
            self.net_seen = true;
        }
    }

    /// 本轮是否到了轮询模组侧 IPv6 的时机。
    pub fn poll_due(&self, cfg: &Config) -> bool {
        modem_poll_due(cfg, self.last_modem_poll.map(|t| t.elapsed()))
    }

    /// 消费一次轮询额度，并把模组侧地址与上次记录比对后按需下发。
    ///
    /// 返回是否发生了地址变化（无论下发成功与否），供调用方决定是否
    /// 需要重新读取接口状态。
    pub fn sync_modem(&mut self, cfg: &Config, v6: &str) -> bool {
        let now = Instant::now();
        self.last_modem_poll = Some(now);

        if !v6.is_empty() && v6 != self.last_modem_ipv6 {
            infof!(format_args!("模组侧 IPv6 更新为 {}", v6));
            self.last_modem_ipv6 = v6.to_string();
            if net::apply_ipv6_addr(cfg, v6) {
                infof!(format_args!("模组侧 IPv6 变化，已应用到 {}", cfg.iface_v6));
            } else {
                warnf!(format_args!("模组侧 IPv6 变化，应用 {} 失败", cfg.iface_v6));
            }
            self.last_refresh = Some(now);
            self.last_scheduled = now;
            return true;
        }
        if v6.is_empty() && !self.last_modem_ipv6.is_empty() {
            infof!(format_args!("模组侧 IPv6 暂未上报"));
            self.last_modem_ipv6.clear();
        }
        false
    }

    /// 恢复刷新的间隔：见过 v6 用常规周期；从没见过则拉长到静默期。
    pub fn recovery_interval(&self) -> Duration {
        recovery_interval(self.modem_seen || self.net_seen)
    }

    /// IPv6 常规维护：判定本轮是否需要干预并执行。
    ///
    /// 仅在「模组侧管理 v6」且 PDN 不是纯 IPv4 时调用（调用方负责前置判断）。
    pub fn maintain(&mut self, cfg: &Config, st: &PdpState, ns: &NetStatus) {
        if !net::v6_managed(cfg) {
            return;
        }
        // 判据一：接口侧的 IPv6 状态本身需要干预（缺地址 / static 模式下不一致）
        let kind = net::needs_refresh(cfg, &st.ipv6, &ns.ipv6);
        // 判据二：接口上完全没有全局 v6，且距上次恢复已过静默期
        let recovery_due = ns.ipv6.is_empty()
            && crate::daemon::v6::recovery_due(
                self.last_refresh.map(|t| t.elapsed()),
                self.recovery_interval(),
            );
        // 判据三：到达用户配置的定时刷新周期
        let scheduled_due =
            scheduled_due(cfg, self.last_scheduled.elapsed());

        if kind.is_none() && !recovery_due && !scheduled_due {
            return;
        }

        let reason = match kind {
            Some(_) if !st.ipv6.is_empty() => "接口未持有模组侧 IPv6",
            Some(_) => "接口无全局 IPv6",
            None if recovery_due => "未发现有效全局 IPv6",
            _ => "到达 IPv6 定时刷新周期",
        };

        // 模组侧没报地址时只能刷接口（让 RA / DHCPv6 自己去要）；
        // 报了地址才按地址下发。绝不拿空地址去写静态配置。
        let done = if st.ipv6.is_empty() {
            net::refresh_ipv6_iface(cfg)
        } else {
            net::apply_ipv6_addr(cfg, &st.ipv6)
        };
        if done {
            infof!(format_args!("{}，已刷新 {}", reason, cfg.iface_v6));
        } else {
            warnf!(format_args!("{}，刷新 {} 失败", reason, cfg.iface_v6));
        }

        let now = Instant::now();
        self.last_refresh = Some(now);
        self.last_scheduled = now;
    }
}

/// 恢复刷新间隔的纯函数形式（可单测）。
///
/// `seen` = 是否在模组侧或接口侧见过 v6。
pub fn recovery_interval(seen: bool) -> Duration {
    if seen {
        V6_REFRESH_MIN_INTERVAL
    } else {
        V6_SILENT_RECOVERY_INTERVAL
    }
}

/// 模组侧 IPv6 是否该轮询（纯函数形式）。
///
/// `last` = 距上次轮询的时长，`None` 表示从未轮询过（首轮视为到期）。
pub fn modem_poll_due(cfg: &Config, last: Option<Duration>) -> bool {
    cfg.enabled
        && cfg.ipv6
        && !cfg.iface_v6.is_empty()
        && cfg.v6_poll_interval > 0
        && last
            .map(|t| {
                t >= Duration::from_secs(cfg.v6_poll_interval.max(60))
                    && t >= V6_MODEM_POLL_MIN_INTERVAL
            })
            .unwrap_or(true)
}

/// 到达用户配置的定时刷新周期（纯函数形式）。
pub fn scheduled_due(cfg: &Config, elapsed: Duration) -> bool {
    cfg.v6_refresh_interval > 0
        && elapsed >= Duration::from_secs(cfg.v6_refresh_interval.max(60))
}

/// 距上次恢复刷新是否已过给定间隔（纯函数形式）。
///
/// `None`（从未恢复过）视为到期：首轮允许干预一次，否则接口上一个 v6
/// 都没有时会永远静默。
pub fn recovery_due(last: Option<Duration>, interval: Duration) -> bool {
    last.map(|t| t >= interval).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg_with(v6_poll: u64, v6_refresh: u64) -> Config {
        let mut c = Config::default();
        c.enabled = true;
        c.ipv6 = true;
        c.iface_v6 = "fm350v6".to_string();
        c.v6_poll_interval = v6_poll;
        c.v6_refresh_interval = v6_refresh;
        c
    }

    #[test]
    fn recovery_interval_is_longer_when_v6_never_seen() {
        assert_eq!(recovery_interval(true), V6_REFRESH_MIN_INTERVAL);
        assert_eq!(recovery_interval(false), V6_SILENT_RECOVERY_INTERVAL);
        assert!(V6_SILENT_RECOVERY_INTERVAL > V6_REFRESH_MIN_INTERVAL);
    }

    #[test]
    fn first_round_is_always_due() {
        assert!(modem_poll_due(&cfg_with(300, 300), None));
        assert!(recovery_due(None, V6_REFRESH_MIN_INTERVAL));
    }

    /// 配置项允许设得更小，但本机下限是 60 s —— 轮询过密只会放大
    /// `AT+CGPADDR` 残留地址带来的误判。
    #[test]
    fn poll_interval_is_clamped_to_a_floor() {
        let c = cfg_with(5, 0);
        assert!(!modem_poll_due(&c, Some(Duration::from_secs(10))));
        assert!(modem_poll_due(&c, Some(Duration::from_secs(61))));
    }

    #[test]
    fn poll_is_skipped_when_v6_disabled_or_interval_zero() {
        let mut c = cfg_with(0, 0);
        assert!(!modem_poll_due(&c, None), "v6_poll_interval=0 时不轮询");
        c = cfg_with(300, 0);
        c.enabled = false;
        assert!(!modem_poll_due(&c, None));
        c.enabled = true;
        c.ipv6 = false;
        assert!(!modem_poll_due(&c, None));
    }

    #[test]
    fn scheduled_refresh_respects_configured_interval() {
        let c = cfg_with(300, 600);
        assert!(!scheduled_due(&c, Duration::from_secs(599)));
        assert!(scheduled_due(&c, Duration::from_secs(600)));
        let off = cfg_with(300, 0);
        assert!(!scheduled_due(&off, Duration::from_secs(99999)));
    }

    /// 见过 v6 之后恢复刷新回到常规周期 —— 真出问题时不能等 30 分钟。
    #[test]
    fn seen_flag_shortens_recovery() {
        let mut w = V6Watch::new();
        assert_eq!(w.recovery_interval(), V6_SILENT_RECOVERY_INTERVAL);
        w.note_modem("2408::1");
        assert_eq!(w.recovery_interval(), V6_REFRESH_MIN_INTERVAL);
    }
}
