//! `AtHandle` 门面：进程内唯一的 AT 串口句柄。
//!
//! ## 两种调用形态，同一邮箱
//!
//! | 形态 | 方法 | 跑在哪 | 超时语义 |
//! |---|---|---|---|
//! | 同步 | [`AtHandle::with`] | 阻塞池（巡检 / dispatch）/ CLI 主线程 | 无超时：等待即语义 |
//! | 异步 | （预留）经 [`super::worker::ExecJob`] | 事件循环 | `tokio::time::timeout` |
//!
//! API 侧的请求预算在 `api::serve` 统一施加（`tokio::time::timeout` 包住
//! `blocking_scope(dispatch)`）：到时**放弃响应**但任务照常完成 —— 半途
//! 丢弃会破坏半双工 AT 会话的问答状态。
//!
//! ## ⚠️ `with()` 不得在 async 上下文调用
//!
//! `blocking_recv()` 进入 Tokio 运行时上下文会 panic。调用约定：
//! 巡检器 tick 与 route dispatch 经 `rt::blocking_scope` 运行在阻塞池，
//! CLI 一次性命令运行在运行时之外 —— 两个入口都满足。

use std::sync::Arc;

use tokio::sync::oneshot;

use super::worker::{self, CloseJob, ExecJob, Job, Slot, SlotView};
use super::{AtResult, AtStats};
use crate::config::Config;

/// 全局 AT 端口句柄（daemon 单点持有，进程内串行，全程独占）。
///
/// Clone 廉价（邮箱发送端 + 快照 Arc），可随意共享进任务/线程。
pub struct AtHandle {
    tx: std::sync::mpsc::Sender<Box<dyn Job>>,
    view: Arc<std::sync::Mutex<SlotView>>,
}

impl Clone for AtHandle {
    fn clone(&self) -> Self {
        AtHandle {
            tx: self.tx.clone(),
            view: Arc::clone(&self.view),
        }
    }
}

impl AtHandle {
    pub fn new() -> Self {
        let w = worker::spawn();
        AtHandle {
            tx: w.tx,
            view: w.view,
        }
    }

    /// 在 AT 工作线程内执行闭包，保证一问一答串行（**同步，阻塞到完成**）。
    ///
    /// 闭包签名 `FnOnce(&mut AtPort, &Config)`：第二参是**本次请求的配置
    /// 快照**（邮箱要求 `'static`，闭包不能借用外部 `cfg`，此设计免去
    /// 逐调用点 clone）。需要配置时写 `|p, c| ... c.at_timeout ...`，
    /// 不需要时写 `|p, _| ...`。
    ///
    /// 守卫语义（由 [`super::worker::Slot::execute`] 实现，逐字沿用旧锁体）：
    ///
    /// 1. **配置热切换**：配置里的端口变了就立刻换，"改完即生效"；
    /// 2. **打开熔断**：刚打开失败且在冷却期内，直接复述错误不撞内核；
    /// 3. **哑口自动改选**：连续 [`SILENT_RESELECT_THRESHOLD`](super::SILENT_RESELECT_THRESHOLD)
    ///    条命令零应答时，探测仍应答 `AT` 的 Fibocom 口并写回 UCI。
    ///
    /// 端口**持久独占**：首次访问惰性打开，此后一直持有，只有串口异常
    /// 才丢弃句柄重建。无超时：调用方在阻塞池 / CLI 上，等待即其语义；
    /// Web 侧的超时放弃由 api::serve 的请求预算实现。
    ///
    /// # Panics
    /// 在 Tokio 运行时的 async 上下文中调用（见模块头约定）。
    pub fn with<F, R>(&self, cfg: &Config, f: F) -> AtResult<R>
    where
        F: FnOnce(&mut super::AtPort, &Config) -> AtResult<R> + Send + 'static,
        R: Send + 'static,
    {
        let (rtx, rrx) = oneshot::channel();
        self.send(ExecJob {
            cfg: cfg.clone(),
            f,
            reply: rtx,
        });
        // blocking_recv 返回 Result<T, RecvError>：worker 侧 drop 回执端
        // （线程退出）时为 Err —— 翻译成可读错误。
        match rrx.blocking_recv() {
            Ok(r) => r,
            Err(_) => Err("AT 工作线程已退出（服务异常），请重启 fm350d 服务".into()),
        }
    }

    /// 当前实际持有的端口路径（未持有时为 `None`）。读快照，不阻塞。
    pub fn current_path(&self) -> Option<String> {
        self.lock_view().current.clone()
    }

    /// 强制关闭端口（换口 / 停服 / 串口异常后的重建）。
    pub fn close(&self) {
        let (rtx, rrx) = oneshot::channel();
        if self.send(CloseJob { reply: rtx }) {
            let _ = rrx.blocking_recv();
        }
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
        }
    }

    /// 投递任务。邮箱异常（worker 已退出）时静默失败 —— 回执端会拿到
    /// `None` 并翻译成可读错误。
    fn send<J: Job + 'static>(&self, job: J) -> bool {
        self.tx.send(Box::new(job)).is_ok()
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

    /// 沙箱无串口：with() 必须快速失败并给出可读错误（打开熔断后第二次
    /// 调用应复述同一错误而不是再次尝试打开）。
    #[test]
    fn with_fails_fast_without_serial_device() {
        let h = AtHandle::new();
        let cfg = Config::default();
        let r1: AtResult<()> = h.with(&cfg, |_, _| panic!("沙箱不应打开成功"));
        let e1 = r1.expect_err("无设备必须失败");
        assert!(e1.contains("打开") || e1.contains("端口"), "错误: {}", e1);
        let r2: AtResult<()> = h.with(&cfg, |_, _| panic!("熔断期内不应重试打开"));
        let e2 = r2.expect_err("冷却期内必须复述错误");
        assert_eq!(e1, e2, "熔断必须复述上次错误");
    }

    /// close() 后快照归零；stats() 反映未持有状态。
    #[test]
    fn close_resets_view() {
        let h = AtHandle::new();
        h.close();
        assert_eq!(h.current_path(), None);
        let cfg = Config::default();
        let s = h.stats(&cfg);
        assert!(!s.open);
        assert_eq!(s.held_secs, 0);
    }

    /// 多次失败后 opens/releases 计数守恒：打开失败不计 opens，
    /// 每次失败丢弃句柄计一次 releases……实际打开失败时未持有、不计。
    #[test]
    fn stats_counters_stay_consistent() {
        let h = AtHandle::new();
        let cfg = Config::default();
        let _ = h.with(&cfg, |_, _| Ok::<(), String>(()));
        let s = h.stats(&cfg);
        assert_eq!(s.opens, 0, "打开失败不得计入 opens");
    }
}
