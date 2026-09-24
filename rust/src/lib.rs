//! `fm350d` —— luci-app-fm350 的守护进程与命令行实现。
//!
//! ## 分层
//!
//! ```text
//! cli / api          入口：命令行与 HTTP（rpcd 经 ucode 转发）
//!   └─ daemon        周期巡检：拨号、保活、IPv4/IPv6 连通性维护、自愈
//!        ├─ modem   模组状态与动作（AT 指令的业务语义）
//!        │    └─ at AT 端口独占持有 + 响应解析
//!        └─ net     主机侧网络：接口、网关、路由、IPv6、自愈
//!   └─ imei / sms   高风险或独立子域，各自闭环
//! ```
//!
//! 依赖方向严格单向：上层可以调下层，下层**不反过来**调上层。
//! `at` 不知道什么是拨号，`net` 不知道什么是 AT 指令。
//!
//! ## 两条硬约束
//!
//! 1. **单一静态二进制**：模块拆分只发生在源码层，产物仍是一个 `fm350d`。
//!    因此不引入 anyhow / tokio / clap 等任何新第三方依赖（`Cargo.toml` 与
//!    重构前逐字一致）。
//! 2. **AT 口独占**：见 [`at`] 模块头。整个进程只有一个 AT 句柄。

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
pub mod sms;
