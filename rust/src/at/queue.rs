//! AT 请求优先级调度器（Priority Gate 的同步形态）。
//!
//! ## 设计（参照 luci-app-mt5700 的 at_queue.rs，按 FM350 的线程模型改造）
//!
//! MT5700 的 PriLock 跑在 Tokio 侧（异步锁 + BinaryHeap 等待者）；FM350 的
//! AT 执行在**专用线程**、调用方全在阻塞池/CLI（同步上下文），因此这里是
//! 同步形态：`Mutex<等待队列>` + `Condvar`，worker 线程 [`PriQueue::pop`]
//! 阻塞取件。机制逐条对应：
//!
//! | MT5700 | 本模块 |
//! |---|---|
//! | 5 级优先级（Sms 最高） | [`AtPriority`] P0–P4（短信归 P1 + 事务独占） |
//! | BinaryHeap + WaitToken | `Vec<QueuedJob>`，pop 时全队列扫描取最优 |
//! | 同级 FIFO（seq） | 同有效优先级下 seq 小者先 |
//! | 排队超时（一次性预算） | 调用方 `run()` 的 `blocking_recv_timeout`，超时置取消 |
//! | 取消等待（从队列移除） | [`QueuedJob::cancelled`] 原子标记，pop 时清理跳过 |
//! | 已开始执行不可取消 | pop 之后标记无效（worker 不再看它）—— 串口上半途 |
//! |   | 丢弃会破坏问答状态，与 rt 层"放弃响应不弃任务"同一语义 |
//! | Priority Aging（防饿死） | 每等 [`AGING_TICK`] 有效优先级 +1，封顶 P0 |
//! | 队列容量上限快速失败 | [`PRIORITY_CAPS`] 每级独立上限，满即 Err |
//!
//! ## 事务独占
//!
//! FM350 的多步事务（拨号链 / 短信发送 / 锁频 CFUN=0→GTACT→CFUN=1）是
//! **单个 Job**：拿到执行权后整段跑完才交还串口，天然独占 —— 与 MT5700
//! 的 SmsTransaction 等价，只是边界在 Job 而非命令。
//!
//! ## 为什么 pop 是全队列扫描而不是堆
//!
//! 等待队列受每级上限约束（合计 ≤ 38），扫描 O(n) 常数极小；而 aging 要
//! 改键，堆改键要么懒更新要么重建，复杂度反而更高。扫描还能顺带清理
//! 已取消的等待者（MT5700 的 grant_locked 同款逻辑）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::worker::Job;

/// AT 请求优先级。数值越大越优先；**禁止**调用方一律取最高 —— 分级标准
/// 见 `docs/ARCHITECTURE_MIGRATION.md` §3（P0 恢复 / P1 用户写 / P2 查询 /
/// P3 巡检 / P4 批量后台）。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum AtPriority {
    /// 批量查询 / 非紧急维护（预留）。
    Background = 0,
    /// daemon 周期巡检：pdp 轮询、信号采样、连通性探测。
    Low = 1,
    /// 用户页面拉取的状态查询（status/signal/sms list/ports…）。
    Normal = 2,
    /// 用户主动写操作：拨号控制、锁频、USB 模式、短信发送、IMEI、AT 透传。
    High = 3,
    /// 紧急恢复：端口重选探测、看门狗前置动作、端口强制释放。
    Critical = 4,
}

impl AtPriority {
    pub fn as_usize(self) -> usize {
        self as usize
    }
    pub fn name(self) -> &'static str {
        match self {
            AtPriority::Background => "P4",
            AtPriority::Low => "P3",
            AtPriority::Normal => "P2",
            AtPriority::High => "P1",
            AtPriority::Critical => "P0",
        }
    }
}

/// 每级等待队列的容量上限（按 `[P4,P3,P2,P1,P0]` 索引）。满即快速失败，
/// 不排队 —— 调用方（ucode 预算 / CLI 超时）自己决定何时重试。
pub const PRIORITY_CAPS: [usize; 5] = [2, 4, 16, 8, 8];

/// Priority Aging：每等待这么久，有效优先级 +1（封顶 [`AtPriority::Critical`]）。
/// 低优先级巡检在持续高优先级流量下最多等 5×tick 就会升到 P0，不会饿死。
pub const AGING_TICK: Duration = Duration::from_secs(5);

/// 排队等待上限（按优先级，`[P4,P3,P2,P1,P0]`）。超时 = 取消等待，
/// 与 api::budget 的请求预算是两道独立的闸（这里是"队列满/忙"的护栏，
/// 那是"后端慢"的护栏）。
pub const QUEUE_WAIT: [Duration; 5] = [
    Duration::from_secs(10),
    Duration::from_secs(10),
    Duration::from_secs(15),
    Duration::from_secs(60),
    Duration::from_secs(60),
];

/// 等待队列中的一个任务。
pub struct QueuedJob {
    pub job: Box<dyn Job>,
    prio: AtPriority,
    pub seq: u64,
    pub enqueued_at: Instant,
    /// 等待取消标记：调用方超时放弃时置位，pop 时清理跳过。
    /// **pop 之后置位无效**（worker 不再看它）—— 已开始的执行不可取消。
    cancelled: Arc<AtomicBool>,
}

impl QueuedJob {
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancelled)
    }
}

/// 有效优先级 = 基础优先级 + aging 升级（封顶 P0）。
fn effective(prio: AtPriority, enqueued_at: Instant, now: Instant) -> u8 {
    let ticks = (now.duration_since(enqueued_at).as_secs() / AGING_TICK.as_secs()) as u8;
    prio.as_usize() as u8 + ticks.min(AtPriority::Critical.as_usize() as u8 - prio.as_usize() as u8)
}

/// 队列统计快照（观测用：`/api/ports` 的 stats 随包透出）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct QueueSnapshot {
    /// 各级当前等待数（[P4,P3,P2,P1,P0]）。
    pub waiting: [usize; 5],
    pub executed: u64,
    pub cancelled: u64,
    pub rejected: u64,
    /// 最近一次任务的排队耗时与执行耗时（毫秒；0 表示尚无样本）。
    pub last_wait_ms: u64,
    pub last_exec_ms: u64,
}

struct PriState {
    jobs: Vec<QueuedJob>,
    seq: u64,
    executed: u64,
    cancelled: u64,
    rejected: u64,
    last_wait_ms: u64,
    last_exec_ms: u64,
}

/// 优先级等待队列：**所有 AT 请求的唯一入口**。
pub struct PriQueue {
    state: Mutex<PriState>,
    signal: Condvar,
}

impl PriQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(PriQueue {
            state: Mutex::new(PriState {
                jobs: Vec::new(),
                seq: 0,
                executed: 0,
                cancelled: 0,
                rejected: 0,
                last_wait_ms: 0,
                last_exec_ms: 0,
            }),
            signal: Condvar::new(),
        })
    }

    /// 入队。对应优先级队列已满时**立即失败**（不排队、不挤占别级）。
    pub fn push(
        &self,
        job: Box<dyn Job>,
        prio: AtPriority,
        cancelled: Arc<AtomicBool>,
    ) -> Result<u64, String> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let waiting = st.jobs.iter().filter(|q| q.prio == prio).count();
        if waiting >= PRIORITY_CAPS[prio.as_usize()] {
            st.rejected += 1;
            return Err(format!(
                "AT 请求队列已满（{}，等待 {}/{}）：串口忙或模组无应答，请稍后重试",
                prio.name(),
                waiting,
                PRIORITY_CAPS[prio.as_usize()]
            ));
        }
        st.seq += 1;
        let seq = st.seq;
        st.jobs.push(QueuedJob {
            job,
            prio,
            seq,
            enqueued_at: Instant::now(),
            cancelled,
        });
        drop(st);
        self.signal.notify_one();
        Ok(seq)
    }

    /// 取下一个要执行的任务：阻塞直到队列非空。
    ///
    /// 选取规则：跳过已取消的等待者（顺带清理）→ 有效优先级最高者 →
    /// 同级 seq 最小者（FIFO）。
    pub fn pop(&self) -> QueuedJob {
        loop {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                // 队列里只有已取消的等待者（或为空）：清理后继续等。
                let before = st.jobs.len();
                st.jobs.retain(|q| !q.cancelled.load(Ordering::Acquire));
                st.cancelled += (before - st.jobs.len()) as u64;
                if !st.jobs.is_empty() {
                    break;
                }
                let (guard, _) = self
                    .signal
                    .wait_timeout(st, Duration::from_millis(500))
                    .unwrap_or_else(|p| p.into_inner());
                st = guard;
            }
            let now = Instant::now();
            let before = st.jobs.len();
            st.jobs.retain(|q| !q.cancelled.load(Ordering::Acquire));
            st.cancelled += (before - st.jobs.len()) as u64;
            // 取有效优先级最高者；同级按 seq（FIFO）。
            let best = st
                .jobs
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    let ka = effective(a.prio, a.enqueued_at, now);
                    let kb = effective(b.prio, b.enqueued_at, now);
                    ka.cmp(&kb).then_with(|| b.seq.cmp(&a.seq))
                })
                .map(|(i, _)| i);
            if let Some(i) = best {
                let q = st.jobs.remove(i);
                st.executed += 1;
                st.last_wait_ms = q.enqueued_at.elapsed().as_millis() as u64;
                // 执行耗时要等 job 跑完才知道（Job trait 无回调）：
                // pop 记排队耗时，worker 在 [`PriQueue::finish`] 回填执行耗时。
                return q;
            }
            // 全被清光（理论不可达，防御性回环）。
            drop(st);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// worker 完成一个任务后回填执行耗时（统计观测）。
    pub fn finish(&self, wait_ms: u64, exec_ms: u64) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.last_wait_ms = wait_ms;
        st.last_exec_ms = exec_ms;
    }

    /// 队列状态快照（含统计），读锁开销一次。
    pub fn snapshot(&self) -> QueueSnapshot {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut waiting = [0usize; 5];
        for q in &st.jobs {
            waiting[q.prio.as_usize()] += 1;
        }
        QueueSnapshot {
            waiting,
            executed: st.executed,
            cancelled: st.cancelled,
            rejected: st.rejected,
            last_wait_ms: st.last_wait_ms,
            last_exec_ms: st.last_exec_ms,
        }
    }
}

/// 取消句柄（调用方持有，超时放弃时置位）。
pub type CancelTicket = Arc<AtomicBool>;

/// 建一个未取消的取消句柄。
pub fn fresh_ticket() -> CancelTicket {
    Arc::new(AtomicBool::new(false))
}

/// 全局任务序号（跨队列单调，供日志关联）。队列内 seq 由 PriState 维护。
pub static GLOBAL_SEQ: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::at::worker::{CloseJob, ExecJob};
    use crate::config::Config;

    /// 构造一个立即完成的占位 AT 任务（不触碰串口）。
    fn nop_job() -> Box<dyn Job> {
        let (tx, _rx) = std::sync::mpsc::channel();
        Box::new(ExecJob {
            cfg: Config::default(),
            f: move |_, _| Ok(AtPriority::Normal),
            reply: tx,
        })
    }

    fn push_at(q: &PriQueue, prio: AtPriority) -> Result<u64, String> {
        q.push(nop_job(), prio, fresh_ticket())
    }

    #[test]
    fn pops_in_priority_order_then_fifo() {
        let q = PriQueue::new();
        // 入队顺序：Low → High → Normal → High（同级 FIFO 验证）
        push_at(&q, AtPriority::Low).unwrap();
        push_at(&q, AtPriority::High).unwrap();
        push_at(&q, AtPriority::Normal).unwrap();
        push_at(&q, AtPriority::High).unwrap();
        // 弹出顺序必须是优先级降序：High → High → Normal → Low；
        // 两条同级 High 按 seq 升序（FIFO）。跨级比较 seq 无意义。
        let p1 = q.pop();
        let p2 = q.pop();
        let p3 = q.pop();
        let p4 = q.pop();
        assert_eq!(p1.prio, AtPriority::High);
        assert_eq!(p2.prio, AtPriority::High);
        assert_eq!(p3.prio, AtPriority::Normal);
        assert_eq!(p4.prio, AtPriority::Low);
        assert!(p1.seq < p2.seq, "同级必须 FIFO");
    }

    #[test]
    fn aging_promotes_starved_low_priority() {
        let q = PriQueue::new();
        push_at(&q, AtPriority::Low).unwrap();
        push_at(&q, AtPriority::Normal).unwrap();
        // 人为把 Low 的入队时间拨回 2 个 aging tick：Low(1) → 1+2 = 3 ≥ Normal(2)
        {
            let mut st = q.state.lock().unwrap();
            if let Some(low) = st.jobs.iter_mut().find(|j| j.prio == AtPriority::Low) {
                low.enqueued_at = Instant::now() - AGING_TICK * 2 - Duration::from_secs(1);
            }
        }
        let first = q.pop();
        assert_eq!(first.prio, AtPriority::Low, "老化的低优先级必须先于 Normal");
    }

    #[test]
    fn queue_full_fast_fails_per_priority() {
        let q = PriQueue::new();
        // P3（Low）上限 4
        for _ in 0..PRIORITY_CAPS[AtPriority::Low.as_usize()] {
            push_at(&q, AtPriority::Low).unwrap();
        }
        let e = push_at(&q, AtPriority::Low).unwrap_err();
        assert!(e.contains("队列已满"), "{}", e);
        // 其它优先级不受影响
        push_at(&q, AtPriority::Normal).unwrap();
    }

    #[test]
    fn cancelled_waiters_are_skipped_and_counted() {
        let q = PriQueue::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        let t1 = fresh_ticket();
        q.push(
            Box::new(CloseJob { reply: tx }),
            AtPriority::High,
            Arc::clone(&t1),
        )
        .unwrap();
        push_at(&q, AtPriority::Normal).unwrap();
        t1.store(true, Ordering::Release);
        let next = q.pop();
        assert_eq!(next.prio, AtPriority::Normal, "已取消者必须被跳过");
        assert_eq!(q.snapshot().cancelled, 1);
    }

    #[test]
    fn snapshot_reports_waiting_per_priority() {
        let q = PriQueue::new();
        push_at(&q, AtPriority::Normal).unwrap();
        push_at(&q, AtPriority::Normal).unwrap();
        push_at(&q, AtPriority::High).unwrap();
        let s = q.snapshot();
        assert_eq!(s.waiting[AtPriority::Normal.as_usize()], 2);
        assert_eq!(s.waiting[AtPriority::High.as_usize()], 1);
    }
}
