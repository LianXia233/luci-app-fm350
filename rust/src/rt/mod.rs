//! Tokio 异步运行时层：事件驱动基础设施，**不含任何业务逻辑**。
//!
//! 传输层（本地 newline-JSON RPC）在 [`crate::rpc`]；本模块只提供运行时
//! 构建与同步段托管。
//!
//! ## 事件驱动模型（谁跑在哪）
//!
//! ```text
//! tokio current_thread 事件循环（永不阻塞）
//!   ├─ rpc::serve accept 循环          每连接一个 task，报文读写全异步
//!   ├─ daemon 巡检循环                  定时器驱动（tokio::time::sleep）
//!   │    └─ 每个阻塞段 → blocking_scope 派发到阻塞池
//!   └─ 请求预算 tokio::time::timeout   超时放弃响应，AT 会话照常完成
//!
//! 阻塞池（tokio blocking pool）
//!   ├─ route::dispatch（业务层，同步实现原样保留）
//!   └─ 各巡检器 tick（AT + shell，同步实现原样保留）
//!
//! AT 工作线程（专用，见 at::worker）
//!   └─ 邮箱 FIFO，串口独占状态机，唯一能触碰 Slot 的线程
//! ```
//!
//! 设计取舍：业务层（modem / net / sms / daemon 巡检器）**保持同步实现**，
//! 经 [`blocking_scope`] 跑在阻塞池 —— 它们全是"fork shell + AT 一问一答"
//! 的串行流程，改写成 async 只会增加状态机复杂度而不改变吞吐（AT 口本身
//! 就是半双工串行的）。异步的价值在事件循环侧：accept、定时器、请求预算
//! 全部非阻塞，任何一段慢代码都拖不死服务器与巡检节奏。

use std::future::Future;

/// 构建单线程 Tokio 运行时并阻塞执行根 future。
///
/// 用 current_thread 而非 multi-thread：事件循环上只有 accept 与定时器，
/// 所有重活（业务 dispatch、巡检 tick、AT 会话）都在阻塞池与 AT 工作线程，
/// 多线程事件循环只会平白增加调度开销与二进制体积。
pub fn start<F>(fut: F) -> Result<(), String>
where
    F: Future<Output = Result<(), String>>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("构建 Tokio 运行时失败: {}", e))?
        .block_on(fut)
}

/// 把同步阻塞段派发到阻塞池执行，**事件循环不被拖住**。
///
/// 这是"同步业务层接入异步世界"的唯一入口：巡检 tick 与 route::dispatch
/// 的实现保持原样（同步），从这里进入异步编排。
///
/// 返回 `Err` 仅当阻塞任务 panic（JoinError）——由 `panic = "abort"` 语义，
/// release 构建下进程直接退出、procd 重拉，这个分支主要服务测试构建。
pub async fn blocking_scope<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(|e| {
        if e.is_panic() {
            format!("阻塞任务 panic: {}", e)
        } else {
            format!("阻塞任务被取消: {}", e)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// blocking_scope 必须把闭包送到别的线程并带回返回值。
    #[test]
    fn blocking_scope_round_trips_value() {
        let r = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                blocking_scope(move || {
                    assert!(
                        tokio::runtime::Handle::try_current().is_ok(),
                        "阻塞池内也应有运行时上下文"
                    );
                    41 + 1
                })
                .await
            });
        assert_eq!(r.unwrap(), 42);
    }
}
