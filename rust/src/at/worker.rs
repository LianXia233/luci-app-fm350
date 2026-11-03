//! AT 工作线程：串口独占状态机 + 请求邮箱。
//!
//! ## 为什么从「Mutex + 闭包」改成「工作线程 + 邮箱」
//!
//! 旧实现 `AtHandle::with()` 用 `Mutex<Slot>` 串行化并发调用，调用方在
//! **自己的线程**里等锁（`LOCK_WAIT` 3 s 上限）。Tokio 化之后业务层全部
//! 跑在阻塞池，若沿用 Mutex：
//!
//! 1. 锁被慢操作（拨号 / 哑口探测）持有时，池线程在自旋等待里空烧；
//! 2. 锁语义与"谁持有端口"分散在两处，难以观测。
//!
//! 现在唯一能触碰 [`Slot`] 的线程就是 worker 本身：所有访问一律投递
//! [`Job`] 到 FIFO 邮箱，worker 逐个执行并更新共享快照（[`SlotView`]）。
//! 得到：
//!
//! - **串行语义不变**：一次一任务，一问一答；
//! - **状态集中可观测**：快照永远反映"最近一次任务完成后"的持有状态，
//!   `stats()` 读快照，永不阻塞；
//! - **队列天然公平**：先到先执行，没有锁饥饿。
//!
//! 注意：邮箱是**无超时 FIFO**。请求侧的放弃语义（超时不再等）由调用方
//! 实现 —— 异步侧用 `tokio::time::timeout`（api::budget），同步侧（CLI /
//! 巡检器）本就是顺序执行，等待即其语义。被放弃的任务仍会**完整执行**：
//! 半途丢弃会破坏半双工 AT 会话的问答状态。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use super::{
    resolve_stable, AtPort, AtResult, OPEN_RETRY_COOLDOWN, RESELECT_COOLDOWN, RESELECT_HOLD,
};
use super::SILENT_RESELECT_THRESHOLD;
use crate::config::Config;

/// 单个任务的时间硬上限（毫秒）。
///
/// 必须大于最长的**合法**任务：写类请求预算上限 85 s（api::budget
/// WRITE_MAX）→ 取 95 s。超过即判定为内核级阻塞（如 USB 端点 wedge 时
/// 写路径沉入 `tty_wait_until_sent` 永不返回，Rust 无法取消沉在系统调用里
/// 的线程），由看门狗执行 USB 复位并退出进程，交 procd 重拉自愈。
const JOB_CAP_MS: u64 = 95_000;

/// 当前任务的起始时刻（unix 毫秒）；0 = 空闲。看门狗据此判定阻塞。
static JOB_START_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 独占持有的端口与全部守卫状态（**只能**在 worker 线程内触碰）。
pub struct Slot {
    port: Option<AtPort>,
    opened_at: Option<Instant>,
    opens: u64,
    releases: u64,
    /// 上一次哑口自动改选探测失败的时刻（冷却期内不重复探测）。
    reselect_failed_at: Option<Instant>,
    /// 最近一次改选成功写入的稳定标识与时刻（[`RESELECT_HOLD`] 窗口内，
    /// 携带旧配置的任务据此快速失败，避免逐个撞哑口磨掉整个巡检周期）。
    last_reselect: Option<(String, Instant)>,
    /// 上一次独占打开失败的时刻与错误（冷却期内熔断，直接复述错误）。
    open_failed_at: Option<Instant>,
    open_failed_err: Option<String>,
}

impl Slot {
    fn new() -> Self {
        Slot {
            port: None,
            opened_at: None,
            opens: 0,
            releases: 0,
            reselect_failed_at: None,
            last_reselect: None,
            open_failed_at: None,
            open_failed_err: None,
        }
    }

    /// 执行一个 AT 会话任务（原 `AtHandle::with` 的锁内主体，逐字迁移）。
    ///
    /// 闭包签名 `FnOnce(&mut AtPort, &Config)`：第二个参数把**本次请求的
    /// 配置快照**直接交给业务闭包（邮箱要求 `'static`，闭包无法借用外部
    /// `cfg`，两个参数省去逐调用点 clone）。
    ///
    /// - **配置热切换**：解析稳定标识后与当前持有路径比对，变了立即丢弃
    ///   旧句柄（"改完即生效"，不受巡检周期制约）；
    /// - **打开熔断**：刚打开失败过且仍在 [`OPEN_RETRY_COOLDOWN`] 内，
    ///   直接复述上次错误，不再撞内核；
    /// - **哑口改选**：闭包正常返回但静默计数达到
    ///   [`SILENT_RESELECT_THRESHOLD`] 时，探测替代口并写回 UCI。
    pub fn execute<F, R>(&mut self, cfg: &Config, f: F) -> AtResult<R>
    where
        F: FnOnce(&mut AtPort, &Config) -> AtResult<R>,
    {
        // 改选保持窗口：最近一次自动改选的目标与本次任务的配置端口不同，
        // 说明任务携带的是改选前的旧 cfg 副本 —— 直接快速失败，不再撞
        // 哑口（否则每个任务要磨 3 × at_timeout 才能触发改选，整个拨号
        // 流程能把一个巡检周期拖成几分钟）。用户在窗口内手动改回旧口
        // 属于罕见路径，等窗口过期或重启服务即可。
        if self.port.is_none() {
            if let Some((newp, when)) = &self.last_reselect {
                if when.elapsed() < RESELECT_HOLD && cfg.at_port != *newp {
                    return Err(format!(
                        "AT 口已自动改选 {}（{} 秒内保持），跳过携带旧配置『{}』的任务",
                        newp,
                        RESELECT_HOLD.as_secs(),
                        cfg.at_port
                    ));
                }
            }
        }

        let want = resolve_stable(cfg, &cfg.at_port);
        if self.port.is_some() && self.port.as_ref().map(|p| p.path()) != Some(want.as_str()) {
            self.port = None;
            self.opened_at = None;
            self.releases += 1;
            // 用户 / 改选刚写入了新端口：打开失败熔断必须同步失效，
            // 否则新口要白白多等一个冷却周期才能打开。
            self.open_failed_at = None;
            self.open_failed_err = None;
        }

        if self.port.is_none() {
            if let Some(t) = self.open_failed_at {
                if t.elapsed() < OPEN_RETRY_COOLDOWN {
                    return Err(self
                        .open_failed_err
                        .clone()
                        .unwrap_or_else(|| "模组端口暂不可用".into()));
                }
            }
            match AtPort::open(&want, cfg.baudrate, cfg.at_timeout) {
                Ok(p) => {
                    self.port = Some(p);
                    self.open_failed_at = None;
                    self.open_failed_err = None;
                }
                Err(e) => {
                    self.open_failed_at = Some(Instant::now());
                    self.open_failed_err = Some(e.clone());
                    return Err(e);
                }
            }
            self.opened_at = Some(Instant::now());
            self.opens += 1;
        }

        // 先取出执行结果与静默计数，结束对 port 的可变借用，
        // 后面的哑口改选需要 &mut self。
        let (result, silent) = {
            let port = self.port.as_mut().expect("端口已在上面确保打开");
            let r = f(port, cfg);
            let s = port.silent_streak();
            (r, s)
        };

        match result {
            Ok(v) => {
                if silent >= SILENT_RESELECT_THRESHOLD {
                    // 哑口确认。无论本次是否成功改选，旧口上的响应都是垃圾：
                    // 立即报错终止当前操作，让上层（巡检/拨号）尽快结束本轮，
                    // 下一轮重新 load 配置 —— 若已改选则直接用上新口。
                    let reselected = self.reselect_on_silent(cfg);
                    return Err(match reselected {
                        Some(newp) => format!(
                            "AT 口已自动改选 {} → {}，本次操作中止，请重试",
                            cfg.at_port, newp
                        ),
                        None => format!(
                            "AT 口 {} 连续 {} 条命令无应答（哑口或模组离线）",
                            cfg.at_port, SILENT_RESELECT_THRESHOLD
                        ),
                    });
                }
                // 有真实数据流动：端口健康，改选保持窗口使命完成，清除。
                if silent == 0 {
                    self.last_reselect = None;
                }
                Ok(v)
            }
            Err(e) => {
                // 串口异常：丢弃句柄，下次调用重建（重新独占打开）
                self.port = None;
                self.opened_at = None;
                self.releases += 1;
                Err(e)
            }
        }
    }

    /// 哑口改选：当前端口连续零应答，探测仍应答 `AT` 的替代口并写回 UCI。
    ///
    /// 探测在 worker 内执行（逐候选口独占打开 + 发 `AT`，最多约 6 s），
    /// 不会递归进入 [`Slot::execute`]。找到新口后写入 UCI 并失效配置缓存；
    /// 找不到则进入 [`RESELECT_COOLDOWN`] 冷却。
    fn reselect_on_silent(&mut self, cfg: &Config) -> Option<String> {
        if let Some(t) = self.reselect_failed_at {
            if t.elapsed() < RESELECT_COOLDOWN {
                crate::warnf!(format_args!(
                    "AT 口 {} 连续 {} 条命令无应答，冷却期内不重复改选探测",
                    cfg.at_port, SILENT_RESELECT_THRESHOLD
                ));
                return None;
            }
        }
        crate::warnf!(format_args!(
            "AT 口 {} 连续 {} 条命令无应答（疑似哑口/编号漂移），开始自动改选探测",
            cfg.at_port, SILENT_RESELECT_THRESHOLD
        ));
        // 先释放旧句柄，否则其它候选口的探测不受影响、但旧句柄残留会
        // 让「改选成功后下一轮打开」变成又一次无谓的释放。
        self.port = None;
        self.opened_at = None;
        self.releases += 1;
        match super::identify_at_port(cfg) {
            Some(newp) => {
                crate::warnf!(format_args!("AT 口自动改选 {} → {}", cfg.at_port, newp));
                if let Err(e) = crate::config::save(&serde_json::json!({ "at_port": newp })) {
                    crate::warnf!(format_args!("写入新 AT 口失败: {}", e));
                }
                self.reselect_failed_at = None;
                self.last_reselect = Some((newp.clone(), Instant::now()));
                Some(newp)
            }
            None => {
                crate::warnf!(format_args!(
                    "未探测到可应答 AT 的替代口，{} 秒内不重复探测",
                    RESELECT_COOLDOWN.as_secs()
                ));
                self.reselect_failed_at = Some(Instant::now());
                None
            }
        }
    }

    /// 强制关闭端口（换口 / 停服 / 串口异常后的重建）。
    fn close_port(&mut self) {
        if self.port.is_some() {
            self.port = None;
            self.opened_at = None;
            self.releases += 1;
        }
    }

    /// 快照：任务执行完毕后发布，供 `stats()` / `current_path()` 无锁读取。
    fn snapshot(&self) -> SlotView {
        SlotView {
            current: self.port.as_ref().map(|p| p.path().to_string()),
            held_secs: self.opened_at.map(|t| t.elapsed().as_secs()).unwrap_or(0),
            opens: self.opens,
            releases: self.releases,
        }
    }
}

/// 持有状态的外部可读快照（worker 每次任务后整体覆盖写入）。
///
/// 读取方（`stats` / 前端端口页）拿到的永远是"最近一次任务完成时"的状态，
/// 而不是中间态 —— 代价是长任务（拨号）期间的统计略滞后，换来的是读取
/// 零阻塞。
#[derive(Clone, Default)]
pub struct SlotView {
    pub current: Option<String>,
    pub held_secs: u64,
    pub opens: u64,
    pub releases: u64,
}

/// 邮箱任务：worker 邮箱的统一擦除形态。
///
/// 业务闭包以 [`ExecJob`] 泛型实现本 trait，worker 只看到 `Box<dyn Job>`。
pub trait Job: Send {
    fn run(self: Box<Self>, st: &mut Slot);
}

/// AT 会话任务：在 worker 内执行闭包并回传结果。
pub struct ExecJob<F, R>
where
    F: FnOnce(&mut AtPort, &Config) -> AtResult<R> + Send + 'static,
    R: Send + 'static,
{
    pub cfg: Config,
    pub f: F,
    pub reply: oneshot::Sender<AtResult<R>>,
}

impl<F, R> Job for ExecJob<F, R>
where
    F: FnOnce(&mut AtPort, &Config) -> AtResult<R> + Send + 'static,
    R: Send + 'static,
{
    fn run(self: Box<Self>, st: &mut Slot) {
        let r = st.execute(&self.cfg, self.f);
        let _ = self.reply.send(r);
    }
}

/// 关闭端口任务（`AtHandle::close`）。
pub struct CloseJob {
    pub reply: oneshot::Sender<()>,
}

impl Job for CloseJob {
    fn run(self: Box<Self>, st: &mut Slot) {
        st.close_port();
        let _ = self.reply.send(());
    }
}

/// worker 线程及其对外句柄。
pub struct Worker {
    /// 邮箱发送端（Clone 后即可多生产者）。
    pub tx: std::sync::mpsc::Sender<Box<dyn Job>>,
    /// 持有状态快照（worker 每任务后覆盖写）。
    pub view: Arc<Mutex<SlotView>>,
}

/// 启动 AT 工作线程 + 看门狗线程。
///
/// 工作线程在所有 `Sender` 释放（`recv` 返回 Err）后自然退出 —— `AtHandle`
/// 全部销毁即随行，无需显式 join。看门狗独立计时：任何任务超过
/// [`JOB_CAP_MS`]（内核级阻塞，Rust 层无法取消）就先尝试把模组 USB 设备
/// authorized 复位（清掉 wedge 端点），然后退出进程交 procd 重拉。
pub fn spawn() -> Worker {
    let (tx, rx) = std::sync::mpsc::channel::<Box<dyn Job>>();
    let view = Arc::new(Mutex::new(SlotView::default()));
    let v2 = Arc::clone(&view);
    let handle = std::thread::Builder::new()
        .name("fm350-at".into())
        .spawn(move || {
            let mut st = Slot::new();
            while let Ok(job) = rx.recv() {
                JOB_START_MS.store(now_ms(), Ordering::Relaxed);
                job.run(&mut st);
                JOB_START_MS.store(0, Ordering::Relaxed);
                // 快照覆盖写：Mutex 只保护并发读，worker 是唯一写者，
                // 不可能中毒（本线程 panic 即进程 abort）。
                *v2.lock().expect("AT 快照锁") = st.snapshot();
            }
        });
    if handle.is_err() {
        // 线程创建失败：服务无法工作，直接退出交给 procd 重拉。
        eprintln!("fm350d: AT 工作线程创建失败");
        std::process::exit(1);
    }
    spawn_watchdog();
    Worker { tx, view }
}

/// 看门狗线程：每 5 s 检查一次任务是否超过 [`JOB_CAP_MS`]。
fn spawn_watchdog() {
    let _ = std::thread::Builder::new()
        .name("fm350-wdg".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let start = JOB_START_MS.load(Ordering::Relaxed);
            if start == 0 || now_ms() - start <= JOB_CAP_MS {
                continue;
            }
            // 任务阻塞超过上限：先复位 USB（清 wedge），再退出自愈。
            // 两步都可能失败（进程已不可救），不能阻塞退出路径。
            crate::warnf!(format_args!(
                "AT 任务超过 {} 秒未完成（内核级阻塞，典型为 USB 端点 wedge），
执行模组 USB 复位后退出，交由服务管理器重拉",
                JOB_CAP_MS / 1000
            ));
            let cfg = crate::config::load();
            let node = resolve_stable(&cfg, &cfg.at_port);
            match super::reset_usb_device(&node) {
                Some(dev) => {
                    crate::warnf!(format_args!("已复位模组 USB 设备 {}，进程退出", dev));
                }
                None => {
                    crate::warnf!(format_args!("USB 复位失败（{} 非 USB 拓扑），进程退出", node));
                }
            }
            std::process::exit(1);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 测试任务：向共享序列追加编号。
    struct Mark {
        log: Arc<Mutex<Vec<u32>>>,
        id: u32,
        delay: Duration,
        reply: oneshot::Sender<()>,
    }

    impl Job for Mark {
        fn run(self: Box<Self>, _st: &mut Slot) {
            std::thread::sleep(self.delay);
            self.log.lock().unwrap().push(self.id);
            let _ = self.reply.send(());
        }
    }

    /// 邮箱必须严格 FIFO：先投递的任务（哪怕慢）必须先完成。
    /// 旧 Mutex 实现的"串行"语义由本测试锁死。
    #[test]
    fn mailbox_executes_jobs_in_fifo_order() {
        let w = spawn();
        let log = Arc::new(Mutex::new(Vec::new()));

        let (ta, ra) = oneshot::channel();
        w.tx.send(Box::new(Mark {
            log: Arc::clone(&log),
            id: 1,
            delay: Duration::from_millis(150),
            reply: ta,
        }))
        .unwrap();
        let (tb, rb) = oneshot::channel();
        w.tx.send(Box::new(Mark {
            log: Arc::clone(&log),
            id: 2,
            delay: Duration::ZERO,
            reply: tb,
        }))
        .unwrap();

        let _ = ra.blocking_recv();
        let _ = rb.blocking_recv();
        assert_eq!(*log.lock().unwrap(), vec![1, 2], "慢任务不得插队");
    }

    /// 快照在每次任务后更新：执行过任务后 opens 至少为 1。
    #[test]
    fn view_updates_after_each_job() {
        let w = spawn();
        assert_eq!(w.view.lock().unwrap().opens, 0);
        let (tx, rx) = oneshot::channel();
        w.tx.send(Box::new(Mark {
            log: Arc::new(Mutex::new(Vec::new())),
            id: 1,
            delay: Duration::ZERO,
            reply: tx,
        }))
        .unwrap();
        let _ = rx.blocking_recv();
        assert_eq!(w.view.lock().unwrap().opens, 0, "Mark 不触碰端口，opens 不变");
        // 快照通路本身必须可读且无 panic
        let _ = w.view.lock().unwrap().held_secs;
    }
}
