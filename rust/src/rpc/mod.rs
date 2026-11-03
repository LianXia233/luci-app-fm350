//! 本地 RPC 传输层：newline-JSON over TCP（仅监听 127.0.0.1）。
//!
//! ## 协议
//!
//! 一行请求、一行响应、一行一个 UTF-8 JSON，`\n` 结尾：
//!
//! ```text
//! 请求  {"id":1,"method":"status","params":{}}
//! 响应  {"id":1,"ok":true,"result":{"ok":true,"status":…}}
//!       {"id":1,"ok":false,"code":-32005,"error":"…"}
//! ```
//!
//! * `method` 与旧 `/api/<路径>` 一一对应（`status` → GET `/api/status`，
//!   `dial` → POST `/api/dial`，写类判据见 [`crate::api::budget::is_write_path`]）；
//!   `params` 对读类展开为 query 字符串表、对写类原样作为 payload ——
//!   **返回的业务信封与旧 HTTP 完全一致**，前端与 ucode 不感知传输层切换。
//! * `result` 是业务信封（`{"ok":…}`），`ok`/`code`/`error` 是传输层结论：
//!   `ok=false` 时 `result` 缺省。错误码沿用 JSON-RPC 习惯：
//!   -32700 解析失败（连接保留）、-32701 请求行超长（断开）、
//!   -32004 连接数满、-32005 请求预算超时。
//!
//! ## 为什么一请求一连接
//!
//! 调用方是 rpcd ucode（busybox `nc < /tmp/请求文件`）与 CLI：都是"发一行、
//! 收一行、进程退出"。保持连接复用对它们没有收益，反而让半开连接、
//! stdin EOF 时机这类问题进入关键路径。每连接最多处理一个请求，
//! 响应写完即关闭 —— 最简单的正确形态。
//!
//! ## 背压
//!
//! 并发连接上限 [`MAX_CONNS`]（Semaphore）：满载时新连接直接收到一条
//! `-32004` 后关闭，**不排队** —— 让调用方（ucode 有自己的超时预算）
//! 尽快失败重试，而不是把负载攒在 daemon 里。

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::at::AtHandle;
use crate::rt;

/// 请求行的最大长度（1 MiB）：status/ports 的 raw 回显是最大的合法负载。
pub const MAX_LINE: usize = 1024 * 1024;
/// 并发连接上限。超过即快速失败（-32004），不排队。
pub const MAX_CONNS: usize = 8;
/// 读请求行的空闲上限：调用方连上却不发数据的时长。
pub const READ_IDLE: Duration = Duration::from_secs(30);
/// 写响应的上限：回环 TCP 上 1 MiB 绰绰有余，超时只可能是对端死了。
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// 传输层错误码（JSON-RPC 习惯值，与业务信封的 `ok:false` 区分）。
pub mod code {
    pub const PARSE: i32 = -32700;
    pub const TOO_LARGE: i32 = -32701;
    pub const BUSY: i32 = -32004;
    pub const DEADLINE: i32 = -32005;
}

/// 路由层使用的 HTTP 风格方法（只有路由语义，不再是传输协议）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Get,
    Post,
}

/// 一行 RPC 请求。
#[derive(Debug, serde::Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// 成功响应行：**业务信封本身就是响应行**（`{"ok":true,…}`）。
///
/// 为什么不再包一层 `{id, ok, result}`：ucode 薄代理把响应行原样透传，
/// 前端 `parseRaw` 按旧 CLI 契约解析**业务体**；包一层信封会让前端取值
/// 整体偏移一层（实机故障：状态页全部字段读到 undefined，整页离线）。
/// request_id 仍随请求携带，并在传输层错误时回显 —— 一请求一连接下，
/// 成功路径的业务体本身就是最直接的应答。
pub fn resp_ok(_id: &Value, result: Value) -> Value {
    result
}

/// 失败响应行（传输层错误；业务错误仍在业务体自己的 `ok:false` 里）。
pub fn resp_err(id: &Value, c: i32, msg: &str) -> Value {
    serde_json::json!({ "id": id, "ok": false, "code": c, "error": msg })
}

/// 启动 RPC 服务（async，直到监听失败或收到关闭信号）。
pub async fn serve(
    listener: TcpListener,
    at: Arc<AtHandle>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let sem = Arc::new(Semaphore::new(MAX_CONNS));
    loop {
        // 关闭信号优先：停止 accept，让在途连接自然收尾（一请求一连接）。
        // accept 用 1s 超时轮询（不引入 tokio macros），关闭响应 ≤1s。
        if *shutdown.borrow() {
            crate::log::infof(format_args!("RPC 停止接受新连接"));
            return Ok(());
        }
        let (stream, _peer) = match tokio::time::timeout(
            std::time::Duration::from_secs(1),
            listener.accept(),
        )
        .await
        {
            Err(_elapsed) => continue,
            Ok(v) => match v {
                Ok(x) => x,
                Err(e) => {
                    crate::log::warnf(format_args!("RPC accept 失败: {}", e));
                    continue;
                }
            },
        };
        // 满载快速失败：尝试拿许可，拿不到就回一条 BUSY 后立刻关闭。
        // 不排队 —— 调用方（ucode 预算 / CLI 超时）自己决定何时重试。
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                let mut s = stream;
                let line = resp_err(&Value::Null, code::BUSY, "RPC 连接数已达上限，请稍后重试");
                let _ = s.write_all(format!("{}\n", line).as_bytes()).await;
                continue;
            }
        };
        let at = Arc::clone(&at);
        tokio::spawn(async move {
            let _permit = permit; // 连接结束（含 panic 展开前 drop）即归还
            handle_conn(stream, at).await;
        });
    }
}

/// 处理一条连接：读一行 → 派发 → 写一行 → 关闭。
async fn handle_conn(stream: TcpStream, at: Arc<AtHandle>) {
    let (mut reader, mut writer) = stream.into_split();
    let mut line: Vec<u8> = Vec::new();

    let read_res = tokio::time::timeout(
        READ_IDLE,
        read_line_limited(&mut reader, &mut line, MAX_LINE),
    )
    .await;
    let resp = match read_res {
        Err(_) => resp_err(&Value::Null, code::PARSE, "读取请求超时（连接空闲过久）"),
        Ok(Err(ReadError::TooLong)) => {
            // 超长直接断开：不读剩余字节，避免被恶意大输入拖住。
            crate::log::warnf(format_args!("RPC 请求行超长（>{} 字节），断开", MAX_LINE));
            let _ = tokio::time::timeout(
                WRITE_TIMEOUT,
                writer.write_all(
                    format!(
                        "{}\n",
                        resp_err(&Value::Null, code::TOO_LARGE, "请求行超过长度上限")
                    )
                    .as_bytes(),
                ),
            )
            .await;
            return;
        }
        Ok(Err(ReadError::Io(_))) | Ok(Ok(true)) => {
            // IO 错误或对端在发出任何数据前就关闭：安静结束。
            return;
        }
        Ok(Ok(false)) => {
            let req = serde_json::from_slice::<RpcRequest>(&line);
            match req {
                Ok(r) => {
                    let v = handle(r, Arc::clone(&at)).await;
                    v
                }
                Err(e) => {
                    resp_err(&Value::Null, code::PARSE, &format!("无效的 RPC 请求: {}", e))
                }
            }
        }
    };

    let mut payload = serde_json::to_vec(&resp).unwrap_or_else(|_| {
        serde_json::to_vec(&resp_err(
            &Value::Null,
            code::PARSE,
            "响应序列化失败",
        ))
        .unwrap()
    });
    payload.push(b'\n');
    let _ = tokio::time::timeout(WRITE_TIMEOUT, writer.write_all(&payload)).await;
    let _ = writer.flush().await;
}

/// 处理一个请求：method 映射 → 预算 → 阻塞池 dispatch。
///
/// 超时只放弃**响应**，dispatch 任务照常完成（半途丢弃会破坏 AT 会话，
/// 这是 rt 层的既定语义，传输层换成 RPC 后不变）。
async fn handle(req: RpcRequest, at: Arc<AtHandle>) -> Value {
    let cfg = crate::config::load_cached();
    let path = format!("/api/{}", req.method.trim().trim_matches('/'));
    let method = if crate::api::budget::is_write_path(&path) {
        Method::Post
    } else {
        Method::Get
    };
    let budget = crate::api::budget::for_request(&cfg, method, &path);
    let q = params_to_query(&req.params);
    let payload = req.params.clone();
    let fut = rt::blocking_scope(move || {
        crate::api::route::dispatch(&at, &cfg, &method, &path, &q, &payload)
    });
    match tokio::time::timeout(budget, fut).await {
        Ok(Ok(json)) => resp_ok(&req.id, json),
        Ok(Err(e)) => resp_ok(&req.id, crate::api::http::err(&e)),
        Err(_) => resp_err(
            &req.id,
            code::DEADLINE,
            &format!(
                "请求处理超过 {} 秒（串口忙或模组无应答），已放弃本次响应；稍后重试",
                budget.as_secs()
            ),
        ),
    }
}

/// 读类请求的 `params` → query 表：只取字符串字段。
///
/// 写类请求的 params（含对象/数组）不进 query，由 dispatch 直接读 payload。
pub fn params_to_query(params: &Value) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Value::Object(o) = params {
        for (k, v) in o {
            if let Value::String(s) = v {
                m.insert(k.clone(), s.clone());
            }
        }
    }
    m
}

/// 逐行读取的错误。
#[derive(Debug)]
pub enum ReadError {
    /// 超过 `max` 仍未遇到换行。
    TooLong,
    Io(std::io::Error),
}

/// 带上限的一行读取（半包 / 粘包天然由 `\n` 定界解决）。
///
/// 返回 `Ok(true)` 表示超长（缓冲已截断在 `max`）；`Ok(false)` 表示拿到
/// 一行（含"EOF 结尾的无换行残行"—— `nc < 文件` 时文件末尾可能没有
/// 换行）。EOF 且无任何数据返回 `Ok(true)` 之外的 `Ok(false)` 空行？不：
/// 空输入返回 `Ok(Ok(false))` 且 `line` 为空，由调用方按解析错误处理。
pub async fn read_line_limited<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max: usize,
) -> Result<bool, ReadError> {
    out.clear();
    let mut chunk = [0u8; 4096];
    loop {
        let n = reader.read(&mut chunk).await.map_err(ReadError::Io)?;
        if n == 0 {
            // EOF：有数据 = 无换行残行（合法），无数据 = 空行（解析会报错）。
            return Ok(false);
        }
        if out.len() + n > max {
            out.extend_from_slice(&chunk[..max.saturating_sub(out.len())]);
            return Ok(true);
        }
        if let Some(pos) = chunk[..n].iter().position(|&b| b == b'\n') {
            out.extend_from_slice(&chunk[..pos]);
            return Ok(false);
        }
        out.extend_from_slice(&chunk[..n]);
    }
}

/// 同步客户端：发一行、收一行（供 CLI 转发与测试）。
///
/// 返回业务信封（响应的 `result` 字段）；传输层失败或 `ok=false` 时返回
/// `None`，调用方按"daemon 不可用"回落本地路径。
pub fn call(
    addr: &str,
    method: &str,
    params: &Value,
    connect_timeout: Duration,
    read_timeout: Duration,
) -> Option<Value> {
    let mut stream = std::net::TcpStream::connect_timeout(
        &addr.parse().ok()?,
        connect_timeout,
    )
    .ok()?;
    stream.set_read_timeout(Some(read_timeout)).ok()?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT)).ok()?;
    let req = serde_json::json!({ "id": 1, "method": method, "params": params });
    let mut line = serde_json::to_string(&req).ok()?.into_bytes();
    line.push(b'\n');
    use std::io::Write;
    stream.write_all(&line).ok()?;
    stream.flush().ok()?;

    let mut buf = Vec::new();
    stream.take((MAX_LINE + 1) as u64).read_to_end(&mut buf).ok()?;
    let resp: Value = serde_json::from_slice(&buf).ok()?;
    // 响应行 = 业务信封：ok=true 整体返回；ok=false（业务或传输层失败）
    // 返回 None，调用方按「daemon 不可用」回落本地路径。
    if resp.get("ok").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_to_query_takes_strings_only() {
        let p = serde_json::json!({ "cmd": "AT+CSQ", "n": 3, "flag": true, "nest": {} });
        let q = params_to_query(&p);
        assert_eq!(q.get("cmd").map(String::as_str), Some("AT+CSQ"));
        assert_eq!(q.len(), 1, "非字符串字段不进 query");
    }

    #[test]
    fn resp_body_is_envelope_itself() {
        // 响应行 = 业务体（无 {id,ok,result} 外包裹）：ucode 原样透传后
        // 前端 parseRaw 直接拿到 {"ok":true, <业务字段>}，与旧 CLI 契约一致。
        let id = serde_json::json!(42);
        let ok = resp_ok(&id, serde_json::json!({ "ok": true, "pong": true }));
        assert_eq!(ok["ok"], Value::Bool(true));
        assert_eq!(ok["pong"], Value::Bool(true));
        assert!(ok.get("result").is_none(), "不得再包一层 result");

        let e = resp_err(&Value::Null, code::BUSY, "忙");
        assert_eq!(e["ok"], Value::Bool(false));
        assert_eq!(e["code"], serde_json::json!(code::BUSY));
        assert_eq!(e["error"], Value::String("忙".into()));
    }

    /// 不依赖 tokio "macros" feature（保持 release 依赖面最小）：手动建运行时。
    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn read_line_frames_newline_and_reports_overflow() {
        block(async {
            let mut r = tokio_test_stream(b"hello\nworld");
            let mut out = Vec::new();
            assert!(!read_line_limited(&mut r, &mut out, 64).await.unwrap());
            assert_eq!(out, b"hello");

            let mut r = tokio_test_stream(b"no-newline-eof");
            let mut out = Vec::new();
            assert!(!read_line_limited(&mut r, &mut out, 64).await.unwrap());
            assert_eq!(out, b"no-newline-eof");

            let mut r = tokio_test_stream(b"aaaaaaaaaa");
            let mut out = Vec::new();
            assert!(
                read_line_limited(&mut r, &mut out, 4).await.unwrap(),
                "超长应返回 true"
            );
            assert_eq!(out.len(), 4);
        });
    }

    #[test]
    fn read_line_empty_input_is_empty_line() {
        block(async {
            let mut r = tokio_test_stream(b"");
            let mut out = Vec::new();
            assert!(!read_line_limited(&mut r, &mut out, 64).await.unwrap());
            assert!(out.is_empty());
        });
    }

    /// 构造一个从给定字节读取的内存 AsyncRead。
    fn tokio_test_stream(data: &'static [u8]) -> std::io::Cursor<&'static [u8]> {
        std::io::Cursor::new(data)
    }
}
