//! 拨号巡检：PDP 状态机 —— 未激活就拨、地址偏了就补、一直没地址就重拨。
//!
//! ## 状态机
//!
//! ```text
//! AT+CGACT?  ── 未激活 ─────────────► modem::dial() → apply_after_dial()
//!                  └─ 连败达阈值 ───► 清 UCI 会话地址 → ifdown → 300 s 重拨退避
//!            │
//!            ├─ 已激活 + 有 v4 ──────► 地址偏差则重新下发 → IPv6 维护 → 补自启
//!            │
//!            └─ 已激活 + 无 v4 ──────► 有 v6：IPv6-only 会话，下发 v6 + 补自启
//!                                     无 v6：累计轮数，达阈值去激活并净化
//! ```
//!
//! ## 唯一权威判据
//!
//! `active` 只认 `AT+CGACT?`。`AT+CGPADDR` / `AT+CGCONTRDP` 在上下文去激活后
//! **仍原样返回上一轮会话的残留地址**，拿它们当激活判据会让 `dial()` 在「已
//! 激活且已有地址」处短路返回、永不下发 `AT+CGACT=1`：接口写着死地址，ARP
//! 能通（模组代理应答）但三层零回包。详见 [`crate::modem::pdp`] 模块注释。

use std::time::{Duration, Instant};

use crate::at::AtHandle;
use crate::config::Config;
use crate::{infof, warnf};
use crate::modem::{self, PdpState};
use crate::net;
use crate::net::NetStatus;

use super::v6::V6Watch;

/// 激活却完全取不到地址时，连续多少轮后去激活强制重拨。
///
/// 地址迟迟不下发时 `CGACT=0 → 1` 往往能重新拿到，但重拨期间整条链路是断的，
/// 因此必须累计若干轮再动手，不能一轮没地址就重拨。
pub const NO_ADDR_REDIAL_ROUNDS: u32 = 3;

/// 无服务净化后自动重拨的最短间隔，避免无 SIM / 未注册时高频激活 PDP。
pub const NO_SERVICE_RETRY_INTERVAL: Duration = Duration::from_secs(300);

/// 拨号巡检的跨轮状态。
pub struct DialWatch {
    /// 激活却一个地址都没有的连续轮数。
    no_addr_rounds: u32,
    /// PDP 未激活时连续拨号失败的轮数。
    dial_fail_rounds: u32,
    /// 无服务净化后的重拨截止时刻。
    retry_after: Option<Instant>,
}

impl Default for DialWatch {
    fn default() -> Self {
        Self {
            no_addr_rounds: 0,
            dial_fail_rounds: 0,
            retry_after: None,
        }
    }
}

impl DialWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一轮拨号巡检。`v6` 由调用方跨轮持有，IPv6 的时机判定不在这里。
    pub fn tick(&mut self, at: &AtHandle, cfg: &Config, v6: &mut V6Watch) {
        if !cfg.enabled {
            return;
        }

        if cfg.auto_dial {
            let st = modem::pdp(at, cfg);
            v6.note_modem(&st.ipv6);
            // 模组侧 IPv6 轮询：即便本轮不拨号也要跟踪地址变化（auto_dial
            // 关闭时用户仍可能手动拨号，插件需要把新地址写下去）。
            if v6.poll_due(cfg) {
                v6.sync_modem(cfg, &st.ipv6);
            }
            self.drive(at, cfg, v6, &st);
        } else if v6.poll_due(cfg) {
            // 自动拨号关闭：不再碰 PDP，只跟踪模组侧 IPv6 变化。
            // 这里是 `else if` 而非无条件 —— 未到期时不要为了看一眼地址
            // 就每轮都去读 `AT+CGPADDR`（它会返回残留地址，读得越勤误判越多）。
            let st = modem::pdp(at, cfg);
            v6.note_modem(&st.ipv6);
            v6.sync_modem(cfg, &st.ipv6);
        }
    }

    /// 按 PDP 状态分派。
    fn drive(&mut self, at: &AtHandle, cfg: &Config, v6: &mut V6Watch, st: &PdpState) {
        // PDN 显式为纯 IPv4 时跳过一切 IPv6 维护（避免无谓的 ifup 与日志）。
        let pdp_v4_only = st.pdp_type.eq_ignore_ascii_case("IP");

        if !st.active {
            self.on_inactive(at, cfg);
            return;
        }
        if !st.ipv4.is_empty() {
            self.on_active_v4(cfg, v6, st, pdp_v4_only);
            return;
        }
        self.on_active_without_v4(at, cfg, v6, st);
    }

    /// 未激活：拨号；连续失败达到门槛后清理上次会话残值并进入长退避。
    fn on_inactive(&mut self, at: &AtHandle, cfg: &Config) {
        self.no_addr_rounds = 0;

        if retry_waiting(self.retry_after.as_ref(), Instant::now()) {
            return;
        }
        if self.retry_after.take().is_some() {
            infof!(format_args!("无服务重拨退避结束，恢复拨号巡检"));
        }

        infof!(format_args!("PDP 未激活，尝试自动拨号"));
        match modem::dial(at, cfg) {
            Ok(p) => {
                if p.ipv4.is_empty() && p.ipv6.is_empty() {
                    // 拨号动作本身返回成功但没有地址：若上下文仍激活，交给
                    // no_addr_rounds 处理；若未激活，则按一次失败累计。
                    if p.active {
                        self.dial_fail_rounds = 0;
                        self.retry_after = None;
                        infof!(format_args!("PDP 已激活但暂未取得任何地址，稍后复核"));
                    } else {
                        warnf!(format_args!("拨号后 PDP 仍未激活且没有地址"));
                        self.note_dial_failure(cfg);
                    }
                    return;
                }
                self.dial_fail_rounds = 0;
                self.retry_after = None;
                match net::apply_after_dial(cfg, &p.ipv4, &p.ipv6, &p.dns, &p.gw4) {
                    Ok(n) => infof!(format_args!("已拨号并配置网络 {:?}", n.ipv4)),
                    Err(e) => warnf!(format_args!("配置网络失败: {}", e)),
                }
            }
            Err(e) => {
                warnf!(format_args!("拨号失败: {}", e));
                self.note_dial_failure(cfg);
            }
        }
    }

    /// 连续拨号失败达到 `net_guard_rounds` 后清除持久化会话地址。
    fn note_dial_failure(&mut self, cfg: &Config) {
        self.dial_fail_rounds = self.dial_fail_rounds.saturating_add(1);
        let rounds = cfg.net_guard_rounds.max(1);
        if !dial_purge_due(self.dial_fail_rounds, rounds) {
            return;
        }

        warnf!(format_args!(
            "拨号连续失败 {} 轮，进入无服务净化态",
            self.dial_fail_rounds
        ));
        self.purify_and_backoff(cfg);
    }

    /// 清除上次拨号的 UCI 地址残值，并将自动重拨间隔拉长到 300 秒。
    fn purify_and_backoff(&mut self, cfg: &Config) {
        match net::clear_session_addresses(cfg) {
            Ok(()) => infof!(format_args!("已清理蜂窝接口残留地址与 DNS，保留接口骨架")),
            Err(e) => warnf!(format_args!("清理蜂窝接口残留配置失败: {}", e)),
        }
        self.defer_after_external_purification();
    }

    /// 另一个巡检器已完成净化时同步进入同一重拨退避窗口。
    pub(super) fn defer_after_external_purification(&mut self) {
        self.no_addr_rounds = 0;
        self.dial_fail_rounds = 0;
        self.retry_after = Some(Instant::now() + NO_SERVICE_RETRY_INTERVAL);
        infof!(format_args!(
            "无服务态自动重拨间隔调整为 {} 秒",
            NO_SERVICE_RETRY_INTERVAL.as_secs()
        ));
    }

    /// 已激活且拿到了 IPv4：地址偏差则重新下发，随后维护 IPv6 与自启。
    fn on_active_v4(&mut self, cfg: &Config, v6: &mut V6Watch, st: &PdpState, pdp_v4_only: bool) {
        self.no_addr_rounds = 0;
        self.dial_fail_rounds = 0;
        self.retry_after = None;

        let mut ns = net::status(cfg);
        v6.note_iface(&ns.ipv6);

        // 地址变化或接口缺失时重新应用。这里必须打日志：早先 `let _ =`
        // 把错误吞掉，出现过「模组 PDP 正常、主机侧却一直没有 IP」的静默
        // 故障。成功时只在确有偏差的那一轮打印，不会每轮刷屏。
        if !ns.ipv4.contains(&st.ipv4) {
            match net::apply_after_dial(cfg, &st.ipv4, &st.ipv6, &st.dns, &st.gw4) {
                Ok(n) => {
                    infof!(format_args!(
                        "接口缺失或地址变化，已重新应用网络配置 {:?}",
                        n.ipv4
                    ));
                    ns = net::status(cfg);
                }
                Err(e) => warnf!(format_args!("重新应用网络配置失败: {}", e)),
            }
        }

        if net::v6_managed(cfg) && !pdp_v4_only {
            v6.maintain(cfg, st, &ns);
        }

        // 开机自启补齐：上面只在「地址有偏差」时才重写配置，稳态下 auto
        // 一旦不是 1 就永远补不回来（LuCI 显示「开机时未启动」）。这里每轮
        // 无条件校验一次，成本 2~4 次 `uci get`；仅在确有修正时打日志。
        report_autostart(cfg);
    }

    /// 已激活但没有 IPv4：v6-only 会话照常配，两个族都空则累计后强制重拨。
    fn on_active_without_v4(
        &mut self,
        at: &AtHandle,
        cfg: &Config,
        v6: &mut V6Watch,
        st: &PdpState,
    ) {
        self.dial_fail_rounds = 0;

        if !st.ipv6.is_empty() {
            self.no_addr_rounds = 0;
            self.retry_after = None;
            let ns = net::status(cfg);
            v6.note_iface(&ns.ipv6);
            // 判据与 `needs_refresh` 保持一致：dhcpv6 / ra 模式下接口地址本来
            // 就不等于模组上报值，按相等判定会每轮命中、把 v6 子接口反复弹起。
            if net::needs_refresh(cfg, &st.ipv6, &ns.ipv6).is_some() {
                match net::apply_after_dial(cfg, "", &st.ipv6, &st.dns, &st.gw4) {
                    Ok(_) => infof!(format_args!("IPv6-only 会话已配置到 {}", cfg.iface)),
                    Err(e) => warnf!(format_args!("IPv6-only 会话配置失败: {}", e)),
                }
            }
            report_autostart(cfg);
            return;
        }

        // PDP 虽仍报告 active，但没有任何地址时也不可绕过净化后的退避，
        // 否则 hangup 被模组拒绝时会每 3 轮再次去激活/净化。
        if retry_waiting(self.retry_after.as_ref(), Instant::now()) {
            self.no_addr_rounds = 0;
            return;
        }
        if self.retry_after.take().is_some() {
            infof!(format_args!("无服务重拨退避结束，恢复无地址巡检"));
        }

        self.no_addr_rounds += 1;
        if self.no_addr_rounds < NO_ADDR_REDIAL_ROUNDS {
            return;
        }
        warnf!(format_args!(
            "PDP 已激活但连续 {} 轮未取到任何地址，去激活并清理会话残值",
            self.no_addr_rounds
        ));
        if let Err(e) = modem::hangup(at, cfg) {
            warnf!(format_args!("去激活 PDP 失败: {}", e));
        }
        self.purify_and_backoff(cfg);
    }
}

/// 是否已达到拨号失败净化门槛（最小为 1 轮）。
fn dial_purge_due(failed_rounds: u32, configured_rounds: u32) -> bool {
    failed_rounds >= configured_rounds.max(1)
}

/// 是否仍在净化后的拨号退避窗口内。
fn retry_waiting(retry_at: Option<&Instant>, now: Instant) -> bool {
    retry_at.map(|deadline| now.lt(deadline)).unwrap_or(false)
}

/// 补开机自启并在确有修正时打印。
fn report_autostart(cfg: &Config) {
    let fixed = net::ensure_autostart(cfg);
    if !fixed.is_empty() {
        infof!(format_args!("已把接口 {:?} 恢复为开机自启", fixed));
    }
}

/// 去激活 → 等待 → 重拨 → 下发。
///
/// 抽出来是因为数据面自愈与公网连通性保活都要用它。中间那 3 s 不能省：
/// 去激活后立即激活，模组侧 PDN 还没完全释放，激活会回 `+CME ERROR` 或
/// 激活成功却不发地址。
pub fn redial(at: &AtHandle, cfg: &Config) -> bool {
    let _ = modem::hangup(at, cfg);
    std::thread::sleep(Duration::from_secs(3));
    match modem::dial(at, cfg) {
        Ok(p) if !p.ipv4.is_empty() || !p.ipv6.is_empty() => {
            net::apply_after_dial(cfg, &p.ipv4, &p.ipv6, &p.dns, &p.gw4).is_ok()
        }
        Ok(_) => false,
        Err(e) => {
            warnf!(format_args!("重拨失败: {}", e));
            false
        }
    }
}

/// 接口状态快照（`status()` 的薄封装，便于本模块内一致调用）。
#[allow(dead_code)]
pub fn net_status(cfg: &Config) -> NetStatus {
    net::status(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阈值必须 ≥ 2：单轮「还没拿到地址」在运营商承载慢启动时很常见，
    /// 一轮就重拨会制造不必要的断网。
    #[test]
    fn redial_threshold_tolerates_slow_address_delivery() {
        assert!(NO_ADDR_REDIAL_ROUNDS >= 2);
    }

    #[test]
    fn watch_starts_with_no_accumulated_rounds() {
        let w = DialWatch::new();
        assert_eq!(w.no_addr_rounds, 0);
        assert_eq!(w.dial_fail_rounds, 0);
        assert!(w.retry_after.is_none());
    }

    #[test]
    fn unavailable_session_purges_only_after_configured_failures() {
        assert!(!dial_purge_due(1, 3));
        assert!(!dial_purge_due(2, 3));
        assert!(dial_purge_due(3, 3));
        assert!(dial_purge_due(1, 0), "门槛最小为 1 轮");
    }

    #[test]
    fn purified_session_uses_long_redial_backoff() {
        assert_eq!(NO_SERVICE_RETRY_INTERVAL, Duration::from_secs(300));
    }

    #[test]
    fn redial_is_suppressed_until_backoff_deadline() {
        let now = Instant::now();
        let deadline = now + NO_SERVICE_RETRY_INTERVAL;
        assert!(retry_waiting(Some(&deadline), now));
        assert!(!retry_waiting(Some(&now), now));
        assert!(!retry_waiting(None, now));
    }
}
