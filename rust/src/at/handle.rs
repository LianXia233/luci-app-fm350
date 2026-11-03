//! `AtHandle` 门面：进程内唯一的 AT 串口句柄。
//!
//! ## 唯一入口：优先级调度
//!
//! 所有 AT 请求（巡检、RPC、CLI、看护）一律经 [`AtHandle::run`] 进入
//! [`PriQueue`](super::queue::PriQueue)：
//!
//! 1. **入队**：按调用方声明的 [`AtPriority`] 排队；该级队列满 → 立即
//!    返回「队列已满」错误（快速失败，不挤占别级、不无限堆积）；
//! 2. **排队**：同级 FIFO、跨级按优先级、[`AGING_TICK`](super::queue::AGING_TICK)
//!    升级防饿死；排队超时（[`QUEUE_WAIT`](super::queue::QUEUE_WAIT)）置
//!    取消标记 —— 任务从队列清除，**不会执行**；
//! 3. **执行**：worker 取件后整段跑完（事务独占），已开始的执行不可取消；
//! 4. **回执**：完成或错误经 oneshot 返回。
//!
//! Web 侧的请求预算（api::budget）在 api 层统一施加：到时**放弃响应**，
//! 已开始的 AT 任务照常完成（半途丢弃会破坏半双工会话的问答状态）。
//!
//! ## ⚠️ `run()` 不得在 async 上下文调用
//!
//! 阻塞等待进入 Tokio 运行时上下文会 panic。调用约定：巡检器 tick 与
//! route dispatch 经 `rt::blocking_scope` 运行在阻塞池，CLI 一次性命令
//! 运行在运行时之外 —— 两个入口都满足。

use std::sync::Arc;
use std::time::Duration;

use super::queue::{self, AtPriority, PriQueue, QueueSnapshot, QUEUE_WAIT};
use super::worker::{self, CloseJob, ExecJob, Slot, SlotView};
use super::{AtResult, AtStats};
use crate::config::Config;

/// 全局 AT 端口句柄（daemon 单点持有，进程内串行，全程独占）。
///
/// Clone 廉价（队列句柄 + 快照 Arc），可随意共享进任务/线程。
pub struct AtHandle {
    queue: Arc<PriQueue>,
    view: Arc<std::sync::Mutex<SlotView>>,
}

impl Clone for AtHandle {
    fn clone(&self) -> Self {
        AtHandle {
            queue: Arc::clone(&self.queue),
            view: Arc::clone(&self.view),
        }
    }
}

impl AtHandle {
    pub fn new() -> Self {
        let w = worker::spawn();
        AtHandle {
            queue: w.queue,
            view: w.view,
        }
    }

    /// 以指定优先级在 AT 工作线程内执行闭包（**同步，阻塞到完成**）。
    ///
    /// 闭包签名 `FnOnce(&mut AtPort, &Config)`：第二参是**本次请求的配置
    /// 快照**（`'static` 要求，此设计免去逐调用点 clone）。
    ///
    /// 优先级必须逐调用点声明（`docs/ARCHITECTURE_MIGRATION.md` §3）：
    /// P0 恢复 / P1 用户写（拨号、锁频、短信发送、IMEI、透传）/ P2 查询 /
    /// P3 巡检 / P4 后台。**禁止**一律取最高。
    ///
    /// 守卫语义（由 [`super::worker::Slot::execute`] 实现）：
    ///
    /// 1. **配置热切换**：配置里的端口变了就立刻换，"改完即生效"；
    /// 2. **打开熔断**：刚打开失败且在冷却期内，直接复述错误不撞内核；
    /// 3. **哑口自动改选**：连续零应答时探测替代口并写回 UCI。
    pub fn run<F, R>(&self, prio: AtPriority, cfg: &Config, f: F) -> AtResult<R>
    where
        F: FnOnce(&mut super::AtPort, &Config) -> AtResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let ticket = queue::fresh_ticket();
        let pushed = self.queue.push(
            Box::new(ExecJob {
                cfg: cfg.clone(),
                f,
                reply: rtx,
            }),
            prio,
            Arc::clone(&ticket),
        );
        if let Err(e) = pushed {
            return Err(e);
        }
        // 排队 + 执行的等待上限：到时置取消标记。若任务已开始执行
        // （worker 已取件），取消标记无效 —— 任务照常跑完，本次调用
        // 只是放弃等待（响应侧超时，与 api::budget 同一语义）。
        match rrx.recv_timeout(QUEUE_WAIT[prio.as_usize()]) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                ticket.store(true, std::sync::atomic::Ordering::Release);
                Err(format!(
                    "AT 请求排队等待超时（{}，上限 {} 秒）：串口忙或模组无应答，已取消等待",
                    prio.name(),
                    QUEUE_WAIT[prio.as_usize()].as_secs()
                ))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("AT 工作线程已退出（服务异常），请重启 fm350d 服务".into())
            }
        }
    }

    /// 当前实际持有的端口路径（未持有时为 `None`）。读快照，不阻塞。
    pub fn current_path(&self) -> Option<String> {
        self.lock_view().current.clone()
    }

    /// 强制关闭端口（换口 / 停服 / 串口异常后的重建）。P0 级插队。
    pub fn close(&self) {
        let (rtx, rrx) = std::sync::mpsc::channel();
        if self
            .queue
            .push(
                Box::new(CloseJob { reply: rtx }),
                AtPriority::Critical,
                queue::fresh_ticket(),
            )
            .is_ok()
        {
            let _ = rrx.recv_timeout(Duration::from_secs(10));
        }
    }

    /// 调度队列统计（等待分布 / 执行与取消计数 / 最近耗时）。
    pub fn queue_snapshot(&self) -> QueueSnapshot {
        self.queue.snapshot()
    }

    /// 端口持有统计（供 `/api/ports` 与排障展示）。
    ///
    /// 读快照 + 一次 `/proc` 扫描（独占事实观测），均为廉价只读操作；
    /// 长任务（拨号）期间统计滞后到任务结束，见 [`SlotView`]。
    pub fn stats(&self, cfg: &Config) -> AtStats {
        // 展示用路径：配置原值（用户认得出自己填的是什么）
        let path = cfg.at_port.clone();
        // 扫描用路径：解析后的真实节点（/proc/*/fd 里只会出现真节点）
        let scan_path = super::resolve_stable(cfg, &cfg.at_port);
        let v = self.lock_view();
        let other_pids = if v.current.is_some() {
            super::other_openers(&scan_path)
        } else {
            Vec::new()
        };
        AtStats {
            path,
            open: v.current.is_some(),
            held_secs: v.held_secs,
            opens: v.opens,
            releases: v.releases,
            other_pids,
            queue: self.queue_snapshot(),
        }
    }

    fn lock_view(&self) -> SlotView {
        self.view
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Default for AtHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// `Slot` 在本模块可见性下只被 worker 使用；这里引用以锁定类型关系。
#[allow(dead_code)]
type _SlotUsed = Slot;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// 沙箱无串口：run() 必须快速失败并给出可读错误（打开熔断后第二次
    /// 调用应复述同一错误而不是再次尝试打开）。
    #[test]
    fn run_fails_fast_without_serial_device() {
        let h = AtHandle::new();
        let cfg = Config::default();
        let r1: AtResult<()> = h.run(AtPriority::Normal, &cfg, |_, _| panic!("沙箱不应打开成功"));
        let e1 = r1.expect_err("无设备必须失败");
        assert!(e1.contains("打开") || e1.contains("端口"), "错误: {}", e1);
        let r2: AtResult<()> =
            h.run(AtPriority::Normal, &cfg, |_, _| panic!("熔断期内不应重试打开"));
        let e2 = r2.expect_err("冷却期内必须复述错误");
        assert_eq!(e1, e2, "熔断必须复述上次错误");
    }

    /// close() 后快照归零；stats() 反映未持有状态；队列统计可见。
    #[test]
    fn close_resets_view() {
        let h = AtHandle::new();
        h.close();
        assert_eq!(h.current_path(), None);
        let cfg = Config::default();
        let s = h.stats(&cfg);
        assert!(!s.open);
        assert_eq!(s.held_secs, 0);
        assert_eq!(s.queue.executed, 1, "close 是一次 P0 任务");
    }

    /// 多次失败后 opens/releases 计数守恒：打开失败不计 opens。
    #[test]
    fn stats_counters_stay_consistent() {
        let h = AtHandle::new();
        let cfg = Config::default();
        let _ = h.run(AtPriority::Normal, &cfg, |_, _| Ok::<(), String>(()));
        let s = h.stats(&cfg);
        assert_eq!(s.opens, 0, "打开失败不得计入 opens");
    }
}
