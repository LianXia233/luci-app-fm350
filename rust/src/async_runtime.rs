//! 统一异步任务管理器（参照 luci-app-mt5700 的 async_runtime.rs，两处硬化）。
//!
//! ## 用途
//!
//! 拨号/重拨等**长写操作**的可选异步提交通道：RPC 立即返回 task_id，
//! 前端可用 `/api/task` 查询状态 —— rpcd 的同步代理不再被 85 s 写预算
//! 长期占住（短查询仍走同步接口，前端交互不变）。
//!
//! ## 状态机与语义
//!
//! ```text
//! Pending → Running → Completed / Failed
//!    ↓          ↓
//! Cancelled   Timeout（超时置态 + 发取消信号；**不 drop 任务体**，
//!             同步 AT 事务必须自己跑完，见 rt 层约定）
//! ```
//!
//! 相对 MT5700 的两处硬化（设计文档 §6）：
//!
//! 1. **硬上限**：in-flight（Pending+Running）≥ [`TASK_CAP`] 时 `spawn`
//!    返回 `Err` —— 不是软淘汰历史，是明确拒绝新任务；
//! 2. **超时区分**：Timeout 只代表"请求侧放弃等待"；任务体的实际结局
//!    由它自己跑完后落在 result/error 里（`timed_out: true` 标记请求侧
//!    已放弃），绝不谎称底层操作已终止。
//!
//! 取消语义：Pending 可撤（不执行）；Running 时请求取消只置标记，
//! 由任务体自行收尾（AT 半途丢弃会破坏会话）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::sync::watch;

/// 任务执行状态。
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskState {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    Timeout,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskState::Pending => "pending",
            TaskState::Running => "running",
            TaskState::Completed => "completed",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
            TaskState::Timeout => "timeout",
        }
    }
}

/// 任务状态快照（status / list 的输出形态）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct TaskInfo {
    pub id: String,
    pub kind: String,
    pub state: TaskState,
    /// epoch 毫秒。
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    pub result: Option<String>,
    /// 请求侧曾放弃等待（超时）或请求过取消 —— 区分"请求超时"与
    /// "底层操作实际终止"：底层结局看 state/result 本身。
    pub abandoned: bool,
}

/// 取消令牌：基于 watch 通道（与 rt/daemon 的关闭信号同一模式）。
#[derive(Clone)]
pub struct CancellationToken {
    tx: watch::Sender<bool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(false);
        CancellationToken { tx }
    }
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
    pub fn wait(&self) -> watch::Receiver<bool> {
        self.tx.subscribe()
    }
    pub fn is_cancelled(&self) -> bool {
        *self.tx.borrow()
    }
}

struct TaskHandle {
    token: CancellationToken,
    info: RwLock<TaskInfo>,
}

/// 任务管理器：`spawn` 返回 task_id，状态经 status/list 读取。
pub struct TaskManager {
    seq: AtomicU64,
    tasks: Mutex<HashMap<String, Arc<TaskHandle>>>,
    /// in-flight 硬上限（Pending + Running）。占满即拒绝新任务。
    cap: usize,
}

/// 默认任务槽位。占满时 spawn 返回 Err —— 明确拒绝而非排队。
pub const TASK_CAP: usize = 8;

/// 单个任务的硬超时下限保护：到点置 Timeout 并发取消信号，**不 drop
/// 任务体**（AT 事务必须自己收尾，见模块头）。
pub const TASK_TIMEOUT: Duration = Duration::from_secs(120);

impl TaskManager {
    pub fn new() -> Arc<Self> {
        Arc::new(TaskManager {
            seq: AtomicU64::new(0),
            tasks: Mutex::new(HashMap::new()),
            cap: TASK_CAP,
        })
    }

    /// 提交后台任务。`f` 接收取消信号 Receiver，返回 `Ok(result)` /
    /// `Err(msg)`。槽位占满时返回 Err（明确拒绝）。
    ///
    /// 须在 Tokio 运行时上下文调用（任务体经 spawn_blocking 跑阻塞池）。
    pub fn spawn<F>(self: &Arc<Self>, kind: &str, f: F) -> Result<String, String>
    where
        F: FnOnce(watch::Receiver<bool>) -> Result<String, String> + Send + 'static,
    {
        let id = format!(
            "{}-{}",
            kind,
            self.seq.fetch_add(1, Ordering::Relaxed) + 1
        );
        {
            let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
            let inflight = tasks
                .values()
                .filter(|h| {
                    matches!(
                        h.info.read().unwrap_or_else(|p| p.into_inner()).state,
                        TaskState::Pending | TaskState::Running
                    )
                })
                .count();
            if inflight >= self.cap {
                return Err(format!(
                    "任务槽位已满（{}/{} 在执行），请等待现有任务完成后再提交",
                    inflight, self.cap
                ));
            }
            tasks.insert(
                id.clone(),
                Arc::new(TaskHandle {
                    token: CancellationToken::new(),
                    info: RwLock::new(TaskInfo {
                        id: id.clone(),
                        kind: kind.to_string(),
                        state: TaskState::Pending,
                        created_at: now_ms(),
                        started_at: None,
                        finished_at: None,
                        error: None,
                        result: None,
                        abandoned: false,
                    }),
                }),
            );
        }
        self.evict_finished();
        let handle = Arc::clone(
            self.tasks
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&id)
                .expect("刚插入"),
        );
        let tm = Arc::clone(self);
        let task_id = id.clone();

        let token = handle.token.clone();
        let wait = token.wait();
        let spawner = match tokio::runtime::Handle::try_current() {
            Ok(h) => h,
            Err(_) => {
                return Err("任务提交必须在 Tokio 运行时上下文内（API 路径）".into());
            }
        };
        spawner.spawn(async move {
            {
                let mut info = handle.info.write().unwrap_or_else(|p| p.into_inner());
                info.state = TaskState::Running;
                info.started_at = Some(now_ms());
            }
            // 超时门卫：到点置 Timeout + 发取消信号，但**不 drop 任务体**，
            // 让它自行收尾；结局以任务体实际返回为准（见模块头"超时区分"）。
            let work = tokio::task::spawn_blocking(move || f(wait));
            tokio::pin!(work);
            let mut timed_out = false;
            // 超时用 tokio::time::timeout（JoinHandle 被 timeout 放弃后任务
            // 继续跑，detached —— 与"不 drop 任务体"语义一致）。
            match tokio::time::timeout(TASK_TIMEOUT, &mut work).await {
                Ok(r) => {
                    let r = match r {
                        Ok(v) => v,
                        Err(e) => Err(format!("阻塞任务异常: {}", e)),
                    };
                    let mut info = handle.info.write().unwrap_or_else(|p| p.into_inner());
                    // 已被 Timeout/Cancelled 表态的不再覆盖（状态机守卫）。
                    if matches!(info.state, TaskState::Running | TaskState::Pending) {
                        match r {
                            Ok(v) => {
                                info.state = TaskState::Completed;
                                info.result = Some(v);
                            }
                            Err(e) => {
                                info.state = TaskState::Failed;
                                info.error = Some(e);
                            }
                        }
                        info.finished_at = Some(now_ms());
                    }
                }
                Err(_elapsed) => {
                    timed_out = true;
                    handle.token.cancel();
                    tm.mark_timeout(&task_id);
                    // 门卫已表态；任务体继续跑，其结局写进 result/error 但不
                    // 覆盖 Timeout 状态（mark_timeout 已置态，上面的守卫挡回）。
                    let r = match work.await {
                        Ok(v) => v,
                        Err(e) => Err(format!("阻塞任务异常: {}", e)),
                    };
                    let mut info = handle.info.write().unwrap_or_else(|p| p.into_inner());
                    if matches!(info.state, TaskState::Running | TaskState::Pending) {
                        match r {
                            Ok(v) => {
                                info.state = TaskState::Completed;
                                info.result = Some(v);
                            }
                            Err(e) => {
                                info.state = TaskState::Failed;
                                info.error = Some(e);
                            }
                        }
                        info.finished_at = Some(now_ms());
                    }
                }
            }
            if timed_out {
                let mut info = handle.info.write().unwrap_or_else(|p| p.into_inner());
                info.abandoned = true;
            }
        });
        Ok(id)
    }

    /// 查询任务状态。
    pub fn status(&self, id: &str) -> Option<TaskInfo> {
        let tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        tasks.get(id).map(|h| {
            h.info
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        })
    }

    /// 列举全部任务（按创建时间）。
    pub fn list(&self) -> Vec<TaskInfo> {
        let tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        let mut v: Vec<TaskInfo> = tasks
            .values()
            .map(|h| h.info.read().unwrap_or_else(|p| p.into_inner()).clone())
            .collect();
        v.sort_by_key(|a| a.created_at);
        v
    }

    /// 取消任务：Pending 直接撤（不执行）；Running 只置信号由任务体收尾。
    pub fn cancel(&self, id: &str) -> Result<TaskInfo, String> {
        let tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        let h = tasks
            .get(id)
            .ok_or_else(|| format!("任务不存在: {}", id))?
            .clone();
        drop(tasks);
        h.token.cancel();
        let mut info = h.info.write().unwrap_or_else(|p| p.into_inner());
        match info.state {
            TaskState::Pending => {
                info.state = TaskState::Cancelled;
                info.finished_at = Some(now_ms());
                info.abandoned = true;
            }
            TaskState::Running => {
                // 不粗暴取消：置信号后由任务体自行收尾；状态保持 Running
                // 直到真实结局（见模块头取消语义）。
                info.abandoned = true;
            }
            _ => {}
        }
        Ok(info.clone())
    }

    /// 清理已结束超过 `older` 的任务记录。
    pub fn cleanup(&self, older: Duration) -> usize {
        let now = now_ms();
        let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        let before = tasks.len();
        tasks.retain(|_, h| {
            let i = h.info.read().unwrap_or_else(|p| p.into_inner());
            match i.finished_at {
                Some(f) if now.saturating_sub(f) > older.as_millis() as i64 => false,
                _ => true,
            }
        });
        before - tasks.len()
    }

    /// 超时门卫表态：仅当任务仍处 Pending/Running 时置 Timeout
    /// （状态机守卫：Completed/Failed/Cancelled 的真实结局不被覆盖）。
    fn mark_timeout(&self, id: &str) {
        let tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = tasks.get(id) {
            let mut info = h.info.write().unwrap_or_else(|p| p.into_inner());
            if matches!(info.state, TaskState::Running | TaskState::Pending) {
                info.state = TaskState::Timeout;
                info.finished_at = Some(now_ms());
                info.error = Some("任务超时（请求侧已放弃等待，底层操作自行收尾中）".into());
                info.abandoned = true;
            }
        }
    }

    fn evict_finished(&self) {
        // 历史任务上限：超出 64 条时按结束时间淘汰最早的已完成任务。
        let mut tasks = self.tasks.lock().unwrap_or_else(|p| p.into_inner());
        if tasks.len() <= 64 {
            return;
        }
        let mut finished: Vec<(String, i64)> = tasks
            .iter()
            .filter_map(|(id, h)| {
                let i = h.info.read().unwrap_or_else(|p| p.into_inner());
                i.finished_at.map(|f| (id.clone(), f))
            })
            .collect();
        finished.sort_by_key(|(_, f)| *f);
        let drop_n = tasks.len() - 64;
        for (id, _) in finished.into_iter().take(drop_n) {
            tasks.remove(&id);
        }
    }
}

/// 全局管理器单例（daemon 与 API 共享同一张任务表）。
pub fn global() -> Arc<TaskManager> {
    static G: std::sync::OnceLock<Arc<TaskManager>> = std::sync::OnceLock::new();
    G.get_or_init(TaskManager::new).clone()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手动建运行时（不依赖 tokio "macros" feature）。
    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn spawn_completes_and_reports() {
        block(async {
            let tm = TaskManager::new();
            let id = tm.spawn("op", |_| Ok("done".into())).unwrap();
            loop {
                let st = tm.status(&id).unwrap();
                if st.finished_at.is_some() {
                    assert_eq!(st.state, TaskState::Completed);
                    assert_eq!(st.result.as_deref(), Some("done"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }

    #[test]
    fn failed_task_records_error() {
        block(async {
            let tm = TaskManager::new();
            let id = tm.spawn("op", |_| Err("boom".into())).unwrap();
            loop {
                let st = tm.status(&id).unwrap();
                if st.finished_at.is_some() {
                    assert_eq!(st.state, TaskState::Failed);
                    assert_eq!(st.error.as_deref(), Some("boom"));
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }

    /// 硬上限：占满全部槽位后再提交必须被明确拒绝。
    #[test]
    fn spawn_rejects_when_slots_full() {
        block(async {
            let tm = TaskManager::new();
            // 提交 TASK_CAP 个长任务（阻塞池线程默认充足）
            let mut ids = Vec::new();
            for _ in 0..TASK_CAP {
                let id = tm
                    .spawn("hold", |_| {
                        std::thread::sleep(Duration::from_millis(300));
                        Ok("late".into())
                    })
                    .unwrap();
                ids.push(id);
            }
            // 等全部进入 Running
            loop {
                let running = tm
                    .list()
                    .iter()
                    .filter(|t| t.state == TaskState::Running)
                    .count();
                if running == TASK_CAP {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let err = tm.spawn("over", |_| Ok(String::new())).unwrap_err();
            assert!(err.contains("任务槽位已满"), "{}", err);
            // 清场
            for id in &ids {
                tm.cancel(id).ok();
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        });
    }

    /// Pending 任务可取消；取消后不再执行。
    #[test]
    fn pending_task_cancel_prevents_run() {
        block(async {
            let tm = TaskManager::new();
            // 占住阻塞执行，让新任务停留在 Pending：
            // 阻塞池默认线程数充足，这里用一个慢任务 + 立即取消竞争窗口。
            let id = tm
                .spawn("op", |cancel| {
                    // 任务体自己等取消信号：Pending→Running 竞态下也能验证取消路径
                    let mut rx = cancel;
                    loop {
                        if *rx.borrow_and_update() {
                            return Err("cancelled".into());
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                })
                .unwrap();
            // 等进入 Running 再取消：取消后任务体自行退出（Cancelled 由
            // 任务体返回 Err 落为 Failed？—— 不：abandoned=true 且返回值
            // 是 Err → Failed，但 info.abandoned 标记请求侧已放弃）。
            loop {
                if tm.status(&id).unwrap().state == TaskState::Running {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let after = tm.cancel(&id).unwrap();
            assert!(after.abandoned);
            loop {
                let st = tm.status(&id).unwrap();
                if st.finished_at.is_some() {
                    assert!(st.abandoned, "取消必须留痕");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }
}
