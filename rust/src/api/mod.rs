//! 本地 JSON API（仅监听 127.0.0.1，供 rpcd ucode 与 LuCI 前端调用）。
//!
//! ## 子模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`http`] | 响应信封（ok / err / at_err） |
//! | [`route`] | 路由表与参数提取（同步业务层，跑在阻塞池） |
//! | [`budget`] | 读/写类请求超时预算 |
//!
//! 传输层在 [`crate::rpc`]：newline-JSON over TCP（一请求一连接，
//! method 与旧 `/api/<路径>` 一一对应，业务信封不变）。
//!
//! ## 四条不能破的约定
//!
//! 1. **只绑回环地址**。这个口没有任何鉴权，把它暴露到局域网等同于把
//!    AT 指令（含短信读取、写号类操作）开放给局域网内任何人。
//! 2. **每个请求重新读配置**（内部有缓存）。用户在 LuCI 改完 `at_port` /
//!    `apn` / 各类开关后无需重启 daemon 即生效 —— 前端就是按这个假设写的。
//! 3. **AT 口由 worker 线程独占持有**（[`crate::at::worker`]）。所有 AT
//!    操作进 FIFO 邮箱串行执行；业务层不做任何并发假设。
//! 4. **事件循环零阻塞**（Tokio 化的核心收益）：accept、报文读写、请求
//!    预算全部异步；dispatch 与巡检 tick 经 `rt::blocking_scope` 跑在
//!    阻塞池，慢请求只会占住一个池线程，绝不拖死服务器与巡检节奏。
//!    超时请求放弃**响应**但任务照常完成（半途丢弃破坏 AT 会话）。

use std::sync::Arc;

use tokio::net::TcpListener;

use crate::at::AtHandle;
use crate::config::Config;
use crate::log::infof;

pub mod budget;
pub mod http;
pub mod route;

pub use http::Json;

/// 监听地址。**硬编码回环**，不接受配置覆盖 —— 见模块头第 1 条。
pub fn bind_addr(cfg: &Config) -> String {
    format!("127.0.0.1:{}", cfg.api_port)
}

/// 启动 API 服务（async，永不返回除非监听失败）。
///
/// 传输与派发都在 [`crate::rpc`]：method → 路径映射、预算、阻塞池 dispatch、
/// 连接数上限与各类超时。本函数只负责绑定与生命周期。
pub async fn serve(at: Arc<AtHandle>, cfg_initial: Config) -> Result<(), String> {
    let addr = bind_addr(&cfg_initial);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("监听 {} 失败: {}", addr, e))?;
    infof(format_args!("API 监听 {}", addr));
    crate::rpc::serve(listener, at).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_addr_is_loopback_only() {
        let mut c = Config::default();
        c.api_port = 9090;
        assert_eq!(bind_addr(&c), "127.0.0.1:9090");
    }

    /// 端口无论怎么配都不能绑到 0.0.0.0 —— 这个口没有鉴权。
    #[test]
    fn bind_addr_never_exposes_lan() {
        let mut c = Config::default();
        c.api_port = 1;
        let a = bind_addr(&c);
        assert!(a.starts_with("127.0.0.1:"), "实际为 {}", a);
    }

    /// 端到端：起服务 → newline-JSON 请求 → 响应行。
    /// 覆盖 accept → 行读取 → 解析 → dispatch（阻塞池）→ 响应全链路。
    #[test]
    fn serve_round_trip_over_rpc() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let at = Arc::new(AtHandle::new());
        let mut cfg = Config::default();
        cfg.api_port = 18766;
        let srv_cfg = cfg.clone();
        let _guard = rt.spawn(async move {
            let _ = serve(at, srv_cfg).await;
        });

        rt.block_on(async {
            // 等监听就绪（最多 2 s）
            let mut connected = false;
            for _ in 0..40 {
                if std::net::TcpStream::connect(("127.0.0.1", cfg.api_port)).is_ok() {
                    connected = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert!(connected, "服务未监听");

            let stream = std::net::TcpStream::connect(("127.0.0.1", cfg.api_port)).unwrap();
            let mut stream = tokio::net::TcpStream::from_std(stream).unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let req = serde_json::json!({
                "id": 7, "method": "ports",
                "params": { "probe": "0" },
            });
            stream
                .write_all(format!("{}\n", req).as_bytes())
                .await
                .unwrap();
            let mut buf = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_to_end(&mut buf))
                .await
                .expect("响应超时")
                .unwrap();
            let resp: serde_json::Value = serde_json::from_slice(&buf).expect("响应应为一行 JSON");
            // 响应行 = 业务体（无外包裹）
            assert_eq!(resp["ok"], serde_json::Value::Bool(true));
            assert!(resp["ports"].is_array(), "缺 ports 字段: {}", resp);
            assert!(buf.ends_with(b"\n"), "响应必须以换行结尾");
        });
    }

    /// 非法 JSON 必须回 -32700 且携带 code，连接以一条响应正常收尾。
    #[test]
    fn serve_rejects_invalid_json() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let at = Arc::new(AtHandle::new());
        let mut cfg = Config::default();
        cfg.api_port = 18767;
        let srv_cfg = cfg.clone();
        let _guard = rt.spawn(async move {
            let _ = serve(at, srv_cfg).await;
        });

        rt.block_on(async {
            for _ in 0..40 {
                if std::net::TcpStream::connect(("127.0.0.1", cfg.api_port)).is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let stream = std::net::TcpStream::connect(("127.0.0.1", cfg.api_port)).unwrap();
            let mut stream = tokio::net::TcpStream::from_std(stream).unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            stream.write_all(b"this is not json\n").await.unwrap();
            let mut buf = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_to_end(&mut buf))
                .await
                .expect("响应超时")
                .unwrap();
            let resp: serde_json::Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(resp["ok"], serde_json::Value::Bool(false));
            assert_eq!(resp["code"], serde_json::json!(crate::rpc::code::PARSE));
        });
    }

    /// 模拟 busybox nc 的行为：写完请求立即 **shutdown 写端**（半关闭），
    /// 再读响应。实机曾出现「nc 秒退空输出」，本测试验证半关闭路径。
    #[test]
    fn serve_responds_to_half_closed_client() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let at = Arc::new(AtHandle::new());
        let mut cfg = Config::default();
        cfg.api_port = 18768;
        let srv_cfg = cfg.clone();
        let _guard = rt.spawn(async move {
            let _ = serve(at, srv_cfg).await;
        });

        rt.block_on(async {
            for _ in 0..40 {
                if std::net::TcpStream::connect(("127.0.0.1", cfg.api_port)).is_ok() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            let stream = std::net::TcpStream::connect(("127.0.0.1", cfg.api_port)).unwrap();
            let mut stream = tokio::net::TcpStream::from_std(stream).unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            stream
                .write_all(b"{\"id\":9,\"method\":\"ping\",\"params\":{}}\n")
                .await
                .unwrap();
            // nc 的关键动作：stdin EOF → shutdown 写端（FIN）
            stream.shutdown().await.unwrap();
            let mut buf = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_to_end(&mut buf))
                .await
                .expect("响应超时")
                .unwrap();
            assert!(!buf.is_empty(), "半关闭后必须仍收到响应");
            let resp: serde_json::Value = serde_json::from_slice(&buf).unwrap();
            assert_eq!(resp["pong"], serde_json::Value::Bool(true));
        });
    }
}
