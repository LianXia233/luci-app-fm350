//! 保活与自愈：数据面健康巡检 + 公网连通性保活。
//!
//! ## 两层，互不替代
//!
//! | 层 | 看什么 | 说明 |
//! |---|---|---|
//! | 数据面健康（`data_guard`） | 数据网卡的 `tx_errors` / `tx_packets` 计数器 | 发现 USB 数据端点 stall —— ARP 能通、地址路由都在，但一个包都发不出去 |
//! | 公网连通性（`net_guard`） | 绑定数据网卡向公共 DNS 发 ICMP | 发现「拿到地址却出不去」—— PDP 激活、地址/路由/网关 ARP 都正常，但运营商侧承载异常 |
//!
//! 两者都不能省：数据面 stall 时 ICMP 会失败，看起来像断网；而承载异常时
//! 数据面计数器完全正常。只留一层会各自漏掉另一类故障。
//!
//! ## 分级与冷却
//!
//! 恢复动作按代价递增分级（复位网卡 → 重拨 → 重启模组），同级反复无效才升级。
//! 每级之间强制冷却：重拨一次要几十秒，期间整条链路是断的，探测目标抖动时
//! 若不冷却会把链路反复打断（恢复风暴）。

use std::time::{Duration, Instant};

use crate::at::AtHandle;
use crate::config::Config;
use crate::{infof, warnf};
use crate::modem;
use crate::net;
use crate::net::DataHealth;

use super::dial::redial;

/// 数据面自愈的冷却时间：同一级恢复动作至少要间隔这么久。
pub const RECOVER_COOLDOWN: Duration = Duration::from_secs(300);

/// 公网连通性保活的恢复冷却时间（每栈独立计时）。
pub const NET_GUARD_COOLDOWN: Duration = Duration::from_secs(300);

/// 数据面自愈：连续多少轮无进展才动手。
///
/// 单轮误判代价太大（重拨会断网），所以必须累计。轮数由 `data_guard_rounds`
/// 配置，最小 1。
pub const DATA_GUARD_MIN_ROUNDS: u32 = 1;

// ---------------------------------------------------------------- 数据面

/// 数据面健康巡检状态。
pub struct DataGuard {
    /// 连续判定为 stall 的轮数。
    stall_rounds: u32,
    /// 上一轮的收发统计（判据是比较增量，不是绝对值）。
    last_health: Option<DataHealth>,
    /// 已完成到第几级自愈。
    recover_level: u32,
    /// 上次执行恢复动作的时刻。
    last_recover: Option<Instant>,
}

impl Default for DataGuard {
    fn default() -> Self {
        Self {
            stall_rounds: 0,
            last_health: None,
            recover_level: 0,
            last_recover: None,
        }
    }
}

impl DataGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一轮数据面巡检。
    pub fn tick(&mut self, at: &AtHandle, cfg: &Config) {
        if !cfg.data_guard || !cfg.enabled {
            return;
        }
        let healthy = self.observe(cfg);
        if healthy {
            return;
        }
        let rounds_needed = cfg.data_guard_rounds.max(DATA_GUARD_MIN_ROUNDS);
        if self.stall_rounds < rounds_needed {
            return;
        }
        if !cooled(self.last_recover, RECOVER_COOLDOWN) {
            return;
        }
        self.escalate(at, cfg);
    }

    /// 采样并更新 stall 计数；返回本轮数据面是否健康。
    fn observe(&mut self, cfg: &Config) -> bool {
        let Some(dev) = net::detect_dev(cfg) else {
            return true;
        };
        let Some(cur) = net::data_health(&dev) else {
            // 读不到统计（网卡刚消失）：既不能判健康也不能判 stall，跳过
            return true;
        };
        let Some(prev) = self.last_health else {
            // 首轮只取基线，不判 stall
            self.last_health = Some(cur);
            return true;
        };
        self.last_health = Some(cur);
        let stalled = net::data_plane_stalled(&prev, &cur);
        if stalled {
            self.stall_rounds += 1;
        } else {
            // 恢复进展即清零，同时把自愈等级一起退回去：下次再出问题
            // 仍从代价最小的第 1 级开始试。
            self.stall_rounds = 0;
            self.recover_level = 0;
        }
        !stalled
    }

    /// 执行下一级自愈动作。
    fn escalate(&mut self, at: &AtHandle, cfg: &Config) {
        let level = self.recover_level + 1;
        warnf!(format_args!(
            "数据面连续 {} 轮无进展（tx_errors 增长而 tx_packets 不动），执行第 {} 级自愈",
            self.stall_rounds, level
        ));
        let done = match level {
            // 第 1 级：复位数据网卡（不动基带，代价最小）
            1 => net::bounce_data_dev(cfg),
            // 第 2 级：重新拨号（重建 PDP 与接口，双栈都会短暂中断）
            2 => redial(at, cfg),
            // 第 3 级及以上：重启模组（AT+CFUN=1,1），代价最大，放在最后
            _ => match modem::reboot(at, cfg) {
                Ok(_) => true,
                Err(e) => {
                    warnf!(format_args!("自愈重启模组失败: {}", e));
                    false
                }
            },
        };
        infof!(format_args!(
            "第 {} 级自愈{}，等待下一轮复核",
            level,
            if done { "已执行" } else { "执行失败" }
        ));
        self.recover_level = level;
        self.last_recover = Some(Instant::now());
        self.stall_rounds = 0;
        // 自愈后基线失效（计数器可能已被清零），下一轮重新取基准
        self.last_health = net::detect_dev(cfg).and_then(|d| net::data_health(&d));
    }
}

// ---------------------------------------------------------------- 公网连通性

/// 单个地址栈的连通性保活状态。
#[derive(Default)]
struct StackGuard {
    /// 连续探测失败的轮数。
    fail_rounds: u32,
    /// 已执行到第几级恢复。
    recover_level: u32,
    /// 上轮探测结果。`None` = 接口没地址，探测不适用（交回地址巡检兜底）。
    last_ok: Option<bool>,
    last_recover: Option<Instant>,
}

/// 公网连通性保活（双栈独立计数、独立分级、独立冷却）。
#[derive(Default)]
pub struct NetGuard {
    v4: StackGuard,
    v6: StackGuard,
}

impl NetGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一轮双栈连通性巡检。
    pub fn tick(&mut self, at: &AtHandle, cfg: &Config) {
        if !cfg.net_guard || !cfg.enabled {
            return;
        }
        let Some(dev) = net::detect_dev(cfg) else {
            return;
        };
        let ns = net::status(cfg);
        let rounds = cfg.net_guard_rounds.max(1);

        self.tick_v4(at, cfg, &dev, &ns, rounds);
        self.tick_v6(at, cfg, &dev, &ns, rounds);
    }

    fn tick_v4(&mut self, at: &AtHandle, cfg: &Config, dev: &str, ns: &net::NetStatus, rounds: u32) {
        // 接口没有 v4 地址属于「地址未下发」，由拨号/地址巡检负责，
        // 这里不重复兜底 —— 否则两个巡检会抢着 ifup。
        if ns.ipv4.is_empty() {
            if self.v4.last_ok.is_some() {
                infof!(format_args!(
                    "net_guard: 接口 {} 暂无 IPv4 地址，连通性探测交回地址巡检",
                    cfg.iface
                ));
            }
            self.v4.reset();
            self.v4.last_ok = None;
            return;
        }

        if net::check_connectivity4(dev) {
            if self.v4.last_ok == Some(false) {
                infof!(format_args!("net_guard: IPv4 公网连通性已恢复"));
            }
            self.v4.reset();
            self.v4.last_ok = Some(true);
            return;
        }

        if self.v4.last_ok != Some(false) {
            warnf!(format_args!(
                "net_guard: IPv4 公网连通性丢失（地址 {:?}），开始连续观测",
                ns.ipv4
            ));
        }
        self.v4.last_ok = Some(false);
        self.v4.fail_rounds += 1;
        if self.v4.fail_rounds < rounds || !cooled(self.v4.last_recover, NET_GUARD_COOLDOWN) {
            return;
        }
        self.v4.recover_level += 1;
        self.v4.fail_rounds = 0;
        let done = if self.v4.recover_level == 1 {
            warnf!(format_args!(
                "net_guard: IPv4 连续 {} 轮不可达，重建接口 {}（第 1 级）",
                rounds, cfg.iface
            ));
            net::bounce_iface_v4(cfg)
        } else {
            warnf!(format_args!(
                "net_guard: 接口级重建无效，去激活重拨（第 2 级，双栈短暂中断）"
            ));
            redial(at, cfg)
        };
        infof!(format_args!(
            "net_guard: IPv4 第 {} 级恢复{}",
            self.v4.recover_level,
            if done { "已执行" } else { "执行失败" }
        ));
        self.v4.last_recover = Some(Instant::now());
    }

    fn tick_v6(&mut self, at: &AtHandle, cfg: &Config, dev: &str, ns: &net::NetStatus, rounds: u32) {
        if ns.ipv6.is_empty() {
            if self.v6.last_ok.is_some() {
                infof!(format_args!(
                    "net_guard: 接口 {} 暂无全局 IPv6 地址，探测交回 v6 地址巡检",
                    cfg.iface_v6
                ));
            }
            self.v6.reset();
            self.v6.last_ok = None;
            return;
        }

        if net::check_connectivity6(dev) {
            if self.v6.last_ok == Some(false) {
                infof!(format_args!("net_guard: IPv6 公网连通性已恢复"));
            }
            self.v6.reset();
            self.v6.last_ok = Some(true);
            return;
        }

        if self.v6.last_ok != Some(false) {
            warnf!(format_args!(
                "net_guard: IPv6 公网连通性丢失（地址 {:?}），开始连续观测",
                ns.ipv6
            ));
        }
        self.v6.last_ok = Some(false);
        self.v6.fail_rounds += 1;
        if self.v6.fail_rounds < rounds || !cooled(self.v6.last_recover, NET_GUARD_COOLDOWN) {
            return;
        }
        self.v6.fail_rounds = 0;

        if self.v6.recover_level < 1 {
            self.v6.recover_level = 1;
            warnf!(format_args!(
                "net_guard: IPv6 连续 {} 轮不可达，重建 v6 子接口 {}（第 1 级）",
                rounds, cfg.iface_v6
            ));
            let done = net::bounce_iface_v6(cfg);
            infof!(format_args!(
                "net_guard: IPv6 第 1 级恢复{}",
                if done { "已执行" } else { "执行失败" }
            ));
            self.v6.last_recover = Some(Instant::now());
            return;
        }

        // v6 独占故障**绝不重拨**：重拨会重建双栈，为修一个栈去断另一个栈
        // 是净亏。只有双栈同时异常才升级为重拨。
        let v4_down_too = ns.ipv4.is_empty() || !net::check_connectivity4(dev);
        if !v4_down_too {
            warnf!(format_args!(
                "net_guard: IPv6 独占故障（IPv4 正常），仅重建 v6 子接口 {}",
                cfg.iface_v6
            ));
            let done = net::bounce_iface_v6(cfg);
            infof!(format_args!(
                "net_guard: IPv6 子接口重建{}",
                if done { "已执行" } else { "执行失败" }
            ));
            self.v6.last_recover = Some(Instant::now());
            return;
        }

        self.v6.recover_level += 1;
        warnf!(format_args!(
            "net_guard: IPv6 持续不可达且 IPv4 同样异常，去激活重拨（第 {} 级）",
            self.v6.recover_level + 1
        ));
        let done = redial(at, cfg);
        infof!(format_args!(
            "net_guard: 双栈重拨{}",
            if done { "已执行" } else { "执行失败" }
        ));
        self.v6.last_recover = Some(Instant::now());
        // 重拨同时重建双栈，v4 侧的计数与冷却一并复位，避免紧接着又重拨一次
        self.v4.reset();
        self.v4.last_recover = Some(Instant::now());
    }
}

impl StackGuard {
    /// 复位故障计数与自愈等级（**不动 `last_ok`** —— 那是「是否有地址」的
    /// 状态，由调用方单独维护）。
    fn reset(&mut self) {
        self.fail_rounds = 0;
        self.recover_level = 0;
    }
}

/// 距上次恢复动作是否已过冷却期。`None`（从未恢复过）视为已冷却。
pub fn cooled(last: Option<Instant>, cooldown: Duration) -> bool {
    last.map(|t| t.elapsed() >= cooldown).unwrap_or(true)
}

// ---------------------------------------------------------------- 接口冲突

/// 接口冲突告警：同一张数据网卡上是否还有别的插件建的接口。
///
/// ## 为什么必须显式喊出来
///
/// 两个 modem 插件同时管一块模组时会互相打断（AT 口争用 + 接口反复
/// down/up），最终把 RNDIS 数据端点打到 stall。这类故障的**配置看起来完全
/// 正常** —— 地址、路由、DNS 都在，只是包发不出去，极难定位。冲突存在时
/// 不去自动处理（我们没有权限拆别人的接口），只保证它一定出现在日志里。
pub struct ConflictWatch {
    last_foreign: Vec<String>,
}

impl Default for ConflictWatch {
    fn default() -> Self {
        Self {
            last_foreign: Vec::new(),
        }
    }
}

impl ConflictWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 一轮冲突巡检。仅在集合变化时打印，避免每轮刷日志。
    pub fn tick(&mut self, cfg: &Config) {
        if !cfg.enabled {
            return;
        }
        let Some(dev) = net::detect_dev(cfg) else {
            return;
        };
        let foreign = net::foreign_ifaces_on_dev(cfg, &dev);
        if foreign == self.last_foreign {
            return;
        }
        if foreign.is_empty() {
            if !self.last_foreign.is_empty() {
                infof!(format_args!(
                    "数据网卡 {} 上的外部接口 {:?} 已消失",
                    dev, self.last_foreign
                ));
            }
        } else {
            warnf!(format_args!(
                "警告：数据网卡 {} 上还存在其它插件的接口 {:?}，与本插件的 {} 冲突\
                 （两者会互相拨号、反复重置链路），请只保留其中一个管理该模组",
                dev, foreign, cfg.iface
            ));
        }
        self.last_foreign = foreign;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_recover_is_never_blocked_by_cooldown() {
        assert!(cooled(None, RECOVER_COOLDOWN));
    }

    #[test]
    fn cooldown_blocks_immediate_repeat() {
        let t = Instant::now();
        assert!(!cooled(Some(t), RECOVER_COOLDOWN));
    }

    /// 数据面最小轮数必须是 1：`max()` 的下限不能把配置值抬到 0 以上之外。
    #[test]
    fn data_guard_rounds_floor_is_one() {
        assert_eq!(DATA_GUARD_MIN_ROUNDS, 1);
        assert_eq!(0u32.max(DATA_GUARD_MIN_ROUNDS), 1);
    }

    /// 两个冷却常量都必须远大于一次重拨耗时（数十秒），否则恢复风暴。
    #[test]
    fn cooldowns_are_far_longer_than_a_redial() {
        assert!(RECOVER_COOLDOWN >= Duration::from_secs(120));
        assert!(NET_GUARD_COOLDOWN >= Duration::from_secs(120));
    }
}
