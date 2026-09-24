//! 拨号巡检：PDP 状态机 —— 未激活就拨、地址偏了就补、一直没地址就重拨。
//!
//! ## 状态机（与原实现逐分支等价）
//!
//! ```text
//! AT+CGACT?  ── 未激活 ─────────────► modem::dial() → apply_after_dial()
//!            │
//!            ├─ 已激活 + 有 v4 ──────► 地址偏差则重新下发 → IPv6 维护 → 补自启
//!            │
//!            └─ 已激活 + 无 v4 ──────► 有 v6：IPv6-only 会话，下发 v6 + 补自启
//!                                     无 v6：累计轮数，达阈值去激活强制重拨
//! ```
//!
//! ## 唯一权威判据
//!
//! `active` 只认 `AT+CGACT?`。`AT+CGPADDR` / `AT+CGCONTRDP` 在上下文去激活后
//! **仍原样返回上一轮会话的残留地址**，拿它们当激活判据会让 `dial()` 在「已
//! 激活且已有地址」处短路返回、永不下发 `AT+CGACT=1`：接口写着死地址，ARP
//! 能通（模组代理应答）但三层零回包。详见 [`crate::modem::pdp`] 模块注释。

use std::time::Duration;

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

/// 拨号巡检的跨轮状态。
pub struct DialWatch {
    /// 激活却一个地址都没有的连续轮数。
    no_addr_rounds: u32,
}

impl Default for DialWatch {
    fn default() -> Self {
        Self { no_addr_rounds: 0 }
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

    /// 未激活：直接拨号。
    fn on_inactive(&mut self, at: &AtHandle, cfg: &Config) {
        self.no_addr_rounds = 0;
        infof!(format_args!("PDP 未激活，尝试自动拨号"));
        match modem::dial(at, cfg) {
            Ok(p) => {
                if p.ipv4.is_empty() && p.ipv6.is_empty() {
                    // 两个族都空：拨号动作本身成功但没拿到任何地址，
                    // 等下一轮再试（v6-only 是合法会话，不算异常）。
                    infof!(format_args!("拨号成功但未取得任何地址，稍后重试"));
                    return;
                }
                match net::apply_after_dial(cfg, &p.ipv4, &p.ipv6, &p.dns, &p.gw4) {
                    Ok(n) => infof!(format_args!("已拨号并配置网络 {:?}", n.ipv4)),
                    Err(e) => warnf!(format_args!("配置网络失败: {}", e)),
                }
            }
            Err(e) => warnf!(format_args!("拨号失败: {}", e)),
        }
    }

    /// 已激活且拿到了 IPv4：地址偏差则重新下发，随后维护 IPv6 与自启。
    fn on_active_v4(&mut self, cfg: &Config, v6: &mut V6Watch, st: &PdpState, pdp_v4_only: bool) {
        self.no_addr_rounds = 0;

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
        if !st.ipv6.is_empty() {
            self.no_addr_rounds = 0;
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

        self.no_addr_rounds += 1;
        if self.no_addr_rounds < NO_ADDR_REDIAL_ROUNDS {
            return;
        }
        warnf!(format_args!(
            "PDP 已激活但连续 {} 轮未取到任何地址，去激活以重新拨号",
            self.no_addr_rounds
        ));
        let _ = modem::hangup(at, cfg);
        self.no_addr_rounds = 0;
    }
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
    }
}
