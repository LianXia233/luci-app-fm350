//! 本地 JSON API（仅监听 127.0.0.1，供 rpcd ucode 与 LuCI 前端调用）。
//!
//! ## 子模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`http`] | HTTP 报文读写与统一响应信封 |
//! | [`route`] | 路由表与参数提取 |
//!
//! ## 三条不能破的约定
//!
//! 1. **只绑回环地址**。这个口没有任何鉴权，把它暴露到局域网等同于把
//!    AT 指令（含短信读取、写号类操作）开放给局域网内任何人。
//! 2. **每个请求重新读配置**（内部有缓存）。用户在 LuCI 改完 `at_port` /
//!    `apn` / 各类开关后无需重启 daemon 即生效 —— 前端就是按这个假设写的。
//! 3. **AT 口由 daemon 独占持有**。所有 AT 操作串行进入 `AtHandle`，本层
//!    不做任何并发；一次请求卡住不会让第二个请求抢到串口。

use std::sync::Arc;

use tiny_http::Server;

use crate::at::AtHandle;
use crate::config::Config;
use crate::log::infof;

pub mod http;
pub mod route;

pub use http::Json;
pub use route::dispatch;

/// 监听地址。**硬编码回环**，不接受配置覆盖 —— 见模块头第 1 条。
pub fn bind_addr(cfg: &Config) -> String {
    format!("127.0.0.1:{}", cfg.api_port)
}

/// 启动 API 服务（阻塞，永不返回除非监听失败）。
pub fn serve(at: Arc<AtHandle>, cfg: Config) -> Result<(), String> {
    let addr = bind_addr(&cfg);
    let server = Server::http(&addr).map_err(|e| format!("监听 {} 失败: {}", addr, e))?;
    infof(format_args!("API 监听 {}", addr));

    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        let path = url.split('?').next().unwrap_or("/").to_string();
        let q = http::query(&url);
        let method = req.method().clone();
        let payload = http::body(&mut req);
        // 每个请求都取一次最新配置（内部带缓存）：配置热生效的前提。
        let cfg = crate::config::load_cached();
        let json = dispatch(&at, &cfg, &method, &path, &q, &payload);
        let _ = req.respond(http::respond(json));
    }
    Ok(())
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
}
