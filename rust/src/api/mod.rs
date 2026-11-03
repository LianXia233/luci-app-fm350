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
use crate::rt;

pub mod budget;
pub mod http;
pub mod route;

pub use http::Json;

/// 监听地址。**硬编码回环**，不接受配置覆盖 —— 见模块头第 1 条。
pub fn bind_addr(cfg: &Config) -> String {
    format!("127.0.0.1:{}", cfg.api_port)
}

/// 启动 API 服务（async，永不返回除非监听失败）。
pub async fn serve(at: Arc<AtHandle>, cfg_initial: Config) -> Result<(), String> {
    let addr = bind_addr(&cfg_initial);
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| format!("监听 {} 失败: {}", addr, e))?;
    infof(format_args!("API 监听 {}", addr));

    let handler: rt::server::Handler = {
        let at = Arc::clone(&at);
        Arc::new(move |req: rt::server::Request| {
            let at = Arc::clone(&at);
            Box::pin(async move {
                // 每个请求都取一次最新配置（内部带缓存）：配置热生效的前提。
                let cfg = crate::config::load_cached();
                let budget = budget::for_request(&cfg, req.method, &req.path);
                let fut = rt::blocking_scope(move || {
                    route::dispatch(&at, &cfg, &req.method, &req.path, &req.query, &req.payload)
                });
                match tokio::time::timeout(budget, fut).await {
                    Ok(Ok(json)) => json,
                    Ok(Err(e)) => http::err(&e),
                    Err(_) => http::err(&format!(
                        "请求处理超过 {} 秒（串口忙或模组无应答），已放弃本次响应；稍后重试",
                        budget.as_secs()
                    )),
                }
            })
        })
    };

    rt::server::serve(listener, handler).await
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

    /// 端到端：起服务 → CLI 风格的 HTTP/1.0 请求 → JSON 响应。
    /// 覆盖 accept → 解析 → dispatch（阻塞池）→ 响应全链路。
    #[test]
    fn serve_round_trip_over_http() {
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
            stream
                .write_all(b"GET /api/ports?probe=0 HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
                .await
                .unwrap();
            let mut buf = Vec::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), stream.read_to_end(&mut buf))
                .await
                .expect("响应超时")
                .unwrap();
            let text = String::from_utf8_lossy(&buf);
            assert!(text.starts_with("HTTP/1.1 200 OK"), "响应: {}", text);
            assert!(text.contains("\"ports\""), "缺 ports 字段: {}", text);
        });
    }
}
