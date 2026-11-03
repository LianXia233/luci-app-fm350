//! 极简异步 HTTP/1.1 服务器（仅服务 127.0.0.1 的 JSON API）。
//!
//! ## 为什么替换 tiny_http
//!
//! tiny_http 的 accept 循环是**单线程串行**的：一个请求的 handler 跑多久，
//! 后续连接就在 backlog 里排多久 —— 这正是旧架构下「一个哑口 status 请求
//! 拖垮整个 API 队列」的直接来源。本实现：
//!
//! 1. accept 与报文读写全部经 tokio（非阻塞），每个连接独立 task；
//! 2. handler 是 async 闭包，业务层在 `api::serve` 里经
//!    [`crate::rt::blocking_scope`] 进阻塞池，事件循环永不被业务拖住；
//! 3. 报文上限（头 16 KB / 体 1 MB）与 IO 超时（30 s）硬编码，恶意或
//!    半死连接不可能钉住 task。
//!
//! 协议面刻意最小化：调用方只有 `fm350d` CLI（HTTP/1.0 + Connection: close）
//! 与本机排障 curl，响应一律 `Connection: close`、一请求一连接，不实现
//! keep-alive / chunked / 压缩。需要更复杂的 HTTP 时应换库而不是扩这个文件。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// API 统一 JSON 类型（与 api::http 的别名等价，避免 rt → api 依赖）。
pub type Json = serde_json::Value;

/// 请求头长度上限。
const MAX_HEAD: usize = 16 * 1024;
/// 请求体长度上限。
const MAX_BODY: usize = 1024 * 1024;
/// 单次 IO（读/写）超时。半死连接最多占 task 30 s，之后丢弃。
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// 请求方法。只实现本 API 用到的两种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

/// 解析后的请求。
pub struct Request {
    pub method: Method,
    /// 不含 query 的路径（如 `/api/ports`）。
    pub path: String,
    /// query string（已 percent-decode）。
    pub query: HashMap<String, String>,
    /// 请求体 JSON；缺失 / 非法一律为 `Null`（与旧 http::body 语义一致）。
    pub payload: Json,
}

/// 异步 handler 的统一形态：请求 → 异步 JSON 响应。
///
/// 用 trait object（`dyn Fn`）而不是泛型 `F: Fn`：调用方（api::serve）
/// 以 `Arc<dyn Fn>` 共享进每个连接 task，`dyn Fn` 本身实现 Fn，免去
/// `Arc<F>` 经两层泛型推导 `Fut` 的麻烦。
pub type Handler = Arc<dyn Fn(Request) -> PinnedJson + Send + Sync>;

/// handler 返回的 boxed future。
pub type PinnedJson = std::pin::Pin<Box<dyn Future<Output = Json> + Send>>;

/// 启动服务：accept 循环 + 每连接 task。
///
/// `handler` 在每连接 task 内被 await；它应当把重活派发到阻塞池，
/// 让事件循环立即回到 accept。
pub async fn serve(listener: TcpListener, handler: Handler) -> Result<(), String> {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let h = Arc::clone(&handler);
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, h).await {
                        // 单连接失败只影响该连接：记日志、继续 accept。
                        // 正常的客户端提前断开（对端复位）也算在内。
                        crate::warnf!(format_args!("HTTP 连接异常结束: {}", e));
                    }
                });
            }
            Err(e) => {
                // EMFILE 等瞬时错误：退避后继续，不让 accept 循环退出。
                crate::warnf!(format_args!("accept 失败（{}），100ms 后重试", e));
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn handle_conn(mut stream: TcpStream, handler: Handler) -> Result<(), std::io::Error> {
    // ---- 读头部（直到 \r\n\r\n）----
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        if buf.len() > MAX_HEAD {
            return reject(&mut stream, 431, "request header too large").await;
        }
        let n = match tokio::time::timeout(IO_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => return reject(&mut stream, 408, "read timeout").await,
        };
        if n == 0 {
            // 对端在头部收完前断开：无事可做，直接结束。
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before request head completed",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find(&buf, b"\r\n\r\n") {
            break pos;
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let req_line = lines.next().unwrap_or("");
    let mut parts = req_line.split_whitespace();
    let method = match parts.next() {
        Some("GET") => Method::Get,
        Some("POST") => Method::Post,
        _ => return reject(&mut stream, 405, "method not allowed").await,
    };
    let target = parts.next().unwrap_or("/");

    let mut content_length = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    if content_length > MAX_BODY {
        return reject(&mut stream, 413, "request body too large").await;
    }

    // ---- 读体（头部缓冲里可能已带上开头几个字节）----
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = match tokio::time::timeout(IO_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
            Err(_) => return reject(&mut stream, 408, "body read timeout").await,
        };
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);
    let payload: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);

    let (path, query) = split_target(target);
    let json = handler(Request {
        method,
        path,
        query,
        payload,
    })
    .await;

    let out = json.to_string();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        out.len(),
        out
    );
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(resp.as_bytes()))
        .await
        .map_err(|_| io_err("write timeout"))??;
    tokio::time::timeout(IO_TIMEOUT, stream.flush())
        .await
        .map_err(|_| io_err("flush timeout"))??;
    Ok(())
}

/// 拒绝响应：写一行状态即关连接（调用方不再读它）。
async fn reject(stream: &mut TcpStream, code: u16, msg: &str) -> Result<(), std::io::Error> {
    let body = format!("{{\"ok\":false,\"error\":\"{}\"}}", msg);
    let resp = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        code,
        msg,
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.flush().await;
    Err(io_err(msg))
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, msg)
}

/// 在缓冲中找子序列第一次出现的位置。
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 拆 `path?query`；query 按 `a=b&c=d` 解析并 percent-decode。
///
/// 自 rt 层建立起从 api::http 迁入：编解码属于 HTTP 基础设施，
/// 与信封构造（api::http）分层。
fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, qs) = match target.find('?') {
        Some(pos) => (&target[..pos], &target[pos + 1..]),
        None => (target, ""),
    };
    let mut m = HashMap::new();
    for pair in qs.split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next().unwrap_or("");
        let v = kv.next().unwrap_or("");
        if !k.is_empty() {
            m.insert(percent_decode(k), percent_decode(v));
        }
    }
    (path.to_string(), m)
}

/// percent-decoding（`+` 视为空格），非法转义原样保留。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_locates_subsequence() {
        assert_eq!(find(b"abc\r\n\r\nx", b"\r\n\r\n"), Some(3));
        assert_eq!(find(b"abc", b"\r\n\r\n"), None);
        assert_eq!(find(b"", b"\r\n\r\n"), None);
    }

    #[test]
    fn split_target_parses_query_and_decodes() {
        let (p, q) = split_target("/api/at?cmd=AT%2BCSQ&x=1&empty=");
        assert_eq!(p, "/api/at");
        assert_eq!(q.get("cmd").map(String::as_str), Some("AT+CSQ"));
        assert_eq!(q.get("x").map(String::as_str), Some("1"));
        assert!(!q.contains_key("empty") || q["empty"].is_empty());
    }

    #[test]
    fn split_target_without_question_mark_is_bare_path() {
        let (p, q) = split_target("/api/status");
        assert_eq!(p, "/api/status");
        assert!(q.is_empty());
    }

    #[test]
    fn percent_decode_handles_plus_and_invalid_escapes() {
        assert_eq!(percent_decode("A+B"), "A B");
        assert_eq!(percent_decode("AT%2BCSQ"), "AT+CSQ");
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
    }

    #[test]
    fn hexval_covers_both_cases() {
        assert_eq!(hexval(b'9'), Some(9));
        assert_eq!(hexval(b'a'), Some(10));
        assert_eq!(hexval(b'F'), Some(15));
        assert_eq!(hexval(b'g'), None);
    }
}
