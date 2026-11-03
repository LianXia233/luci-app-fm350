//! `fm350d` —— luci-app-fm350 的守护进程与命令行实现。
//!
//! ## 分层
//!
//! ```text
//! cli / api          入口：命令行与 HTTP（rpcd 经 ucode 转发）
//!   └─ rt            Tokio 运行时 + 异步 HTTP 服务器（事件驱动基础设施）
//!   └─ daemon        周期巡检：拨号、保活、IPv4/IPv6 连通性维护、自愈
//!        ├─ modem   模组状态与动作（AT 指令的业务语义）
//!        │    └─ at AT 端口独占持有（工作线程 + 邮箱）+ 响应解析
//!        └─ net     主机侧网络：接口、网关、路由、IPv6、自愈
//!   └─ imei / sms   高风险或独立子域，各自闭环
//! ```
//!
//! 依赖方向严格单向：上层可以调下层，下层**不反过来**调上层。
//! `at` 不知道什么是拨号，`net` 不知道什么是 AT 指令。
//!
//! ## 运行模型（Tokio 事件驱动）
//!
//! | 位置 | 跑什么 | 阻塞策略 |
//! |---|---|---|
//! | current_thread 事件循环 | HTTP accept、报文读写、巡检定时器、请求预算 | 全异步，永不阻塞 |
//! | tokio 阻塞池 | route::dispatch、各巡检器 tick（同步实现原样保留） | 慢请求占一个池线程 |
//! | AT 工作线程 | FIFO 邮箱 + 串口独占状态机（at::worker） | 唯一触碰串口的线程 |
//! | CLI 一次性命令 | 经 daemon API 转发或本地直连，无运行时 | 纯阻塞，进程即用即退 |
//!
//! 请求超时预算（api::budget）在事件循环侧施加：到时放弃**响应**，任务
//! 照常完成 —— 半途丢弃会破坏半双工 AT 会话的问答状态。
//!
//! ## 两条硬约束
//!
//! 1. **单一静态二进制**：模块拆分只发生在源码层，产物仍是一个 `fm350d`。
//!    依赖保持在最小面：tokio（按需裁剪 feature 的异步运行时）+
//!    serialport / serde / libc，不再有别的。
//! 2. **AT 口独占**：见 [`at`] 模块头。整个进程只有一个 AT 工作线程。

pub mod addr;
pub mod api;
pub mod at;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod imei;
pub mod log;
pub mod modem;
pub mod net;
pub mod rt;
pub mod sms;
