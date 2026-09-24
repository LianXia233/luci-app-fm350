//! EIF 兜底加速巡检：消费 `+GT*` URC 事件，驱动下一轮巡检提前。
//!
//! ## 职责边界（刻意收窄）
//!
//! 本巡检器**不做任何配置动作**：地址核对、接口下发、路由修复全部是
//! 拨号巡检（[`super::dial`]）的既有职责。这里只做两件事：
//!
//! 1. 每轮把 AT 读取路径截流进队列的 URC 事件取走，落进状态快照
//!    （`/api/eif` 可查），让「刷了 atproxy rootfs」的设备多一路可见性；
//! 2. 收到地址类事件（`ipadd` / `ipdel`）时请求**加速**：把本轮的巡检
//!    间隔睡眠压到秒级，让拨号巡检尽快用 `AT+CGPADDR` 复核地址并按需
//!    重新下发。加速只是提前跑既有主线，不引入新写路径。
//!
//! ## 为什么不直接由 URC 触发重拨 / 改地址
//!
//! URC 行是 atproxy 降维后的告知性信号，不代表「本机地址错了」——
//! 多数 `ipadd` 事件只是模组内部接口的常规变更。若直接据此重拨，等于把
//! 一条不可靠信号插进了拨号状态机，既绕过 `NO_ADDR_REDIAL_ROUNDS` 的
//! 防抖，也会在事件风暴时反复断网。让既有巡检去做判定，天然继承它的
//! 全部防抖与幂等语义。

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::at::urc::{self, GtEvent};
use crate::config::Config;
use crate::infof;

/// 加速时的巡检睡眠秒数（正常间隔被压到该值以内一轮）。
pub const ACCEL_SLEEP: u64 = 5;

/// 两次加速之间的最小冷却。EIF 事件可能在短时间连发（换站、RA 刷新、
/// 地址增删），没有冷却会把巡检间隔长期钉在秒级，AT 通道被压满。
pub const ACCEL_COOLDOWN: u64 = 60;

/// EIF 兜底巡检的跨轮状态。
pub struct GtWatch {
    /// 上次触发加速的时刻（None = 本进程尚未触发过）。
    last_accel: Option<Instant>,
    /// 本轮是否请求加速（主循环读取后清零）。
    accel_pending: bool,
}

impl Default for GtWatch {
    fn default() -> Self {
        Self {
            last_accel: None,
            accel_pending: false,
        }
    }
}

impl GtWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 消费一批 URC 事件。返回是否请求加速。
    pub fn tick(&mut self, cfg: &Config) -> bool {
        let events = urc::take_events();
        if events.is_empty() {
            self.accel_pending = false;
            return false;
        }
        let mut accel = false;
        for ev in &events {
            if let GtEvent::IfAddr { reason, v4cnt, v6cnt, .. } = ev {
                // 只认地址类动词；其余（mtu/ifst/ho 等）不需要动网络面
                if reason == "ipadd" || reason == "ipdel" {
                    if cfg.eif_guard && self.cooldown_over() {
                        accel = true;
                    }
                    infof!(format_args!(
                        "EIF 地址事件 {}（v4={}, v6={}）",
                        reason, v4cnt, v6cnt
                    ));
                }
            }
            match ev {
                GtEvent::Nora(kind) => {
                    infof!(format_args!("EIF RA 缺失事件: {}", kind));
                    // RA 缺失与 V4 无关，但 IPv6 获取依赖 RA；也值得提前核对
                    if cfg.eif_guard && self.cooldown_over() {
                        accel = true;
                    }
                }
                GtEvent::IfAddr { .. } => {}
                GtEvent::Addr4(a) => infof!(format_args!("EIF 模组侧 V4: {}", a)),
                GtEvent::Addr6(a) => infof!(format_args!("EIF 模组侧 V6: {}", a)),
                _ => {}
            }
        }
        if accel {
            self.last_accel = Some(Instant::now());
            self.accel_pending = true;
            urc::note_refresh();
            infof!(format_args!(
                "EIF 事件触发加速：{} 秒后提前执行一轮拨号巡检",
                ACCEL_SLEEP
            ));
        }
        self.accel_pending
    }

    /// 主循环据此后取本轮睡眠时长。
    pub fn sleep_interval(&self, normal: u64) -> Duration {
        if self.accel_pending {
            Duration::from_secs(normal.min(ACCEL_SLEEP))
        } else {
            Duration::from_secs(normal)
        }
    }

    /// 消费加速标志（主循环取走后失效）。
    pub fn consume_accel(&mut self) {
        self.accel_pending = false;
    }

    fn cooldown_over(&self) -> bool {
        match self.last_accel {
            None => true,
            Some(t) => t.elapsed() >= Duration::from_secs(ACCEL_COOLDOWN),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(eif: bool) -> Config {
        Config {
            eif_guard: eif,
            ..Default::default()
        }
    }

    #[test]
    fn cooldown_limiter() {
        let w = GtWatch::new();
        assert!(w.cooldown_over()); // 从未触发过
    }

    #[test]
    fn sleep_interval_shrinks_only_when_pending() {
        let mut w = GtWatch::new();
        assert_eq!(w.sleep_interval(30), Duration::from_secs(30));
        w.accel_pending = true;
        assert_eq!(w.sleep_interval(30), Duration::from_secs(ACCEL_SLEEP));
        // 正常间隔小于加速值时不放大
        assert_eq!(w.sleep_interval(3), Duration::from_secs(3));
        w.consume_accel();
        assert_eq!(w.sleep_interval(30), Duration::from_secs(30));
    }

    #[test]
    fn tick_on_empty_queue_is_noop() {
        // 队列是全局的：并行测试（urc::tests）可能注入过事件，先取空再断言
        let _ = urc::take_events();
        let mut w = GtWatch::new();
        assert!(!w.tick(&cfg(true)));
    }
}
