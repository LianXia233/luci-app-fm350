//! HTTP 编解码与统一响应封装。
//!
//! 把「HTTP 长什么样」和「业务返回什么」分开：本模块只关心报文的读写与
//! 信封格式，路由逻辑见 [`super::route`]。
//!
//! 之所以自己实现 query / percent-decoding 而不用第三方库：`Cargo.toml`
//! 的重构约束是**不新增依赖**（见 [`crate`] 模块头），而这里用到的只是
//! `?a=b&c=d` 这种一维键值解析。

use std::collections::HashMap;

use tiny_http::{Header, Request, Response};

use crate::at::AtResult;

/// API 返回的统一 JSON 类型。
pub type Json = serde_json::Value;

/// 成功信封：把 `data` 的字段平铺到 `{"ok":true, ...}`。
///
/// 不平铺（写成 `{"ok":true,"data":{...}}`）会让前端每个调用点都多剥一层，
/// 且与重构前的响应形态不一致 —— 前端与 rpcd ucode 是按现状写的。
pub fn ok(data: Json) -> Json {
    let mut o = serde_json::Map::new();
    o.insert("ok".into(), Json::Bool(true));
    if let Json::Object(m) = data {
        for (k, v) in m {
            o.insert(k, v);
        }
    }
    Json::Object(o)
}

/// 失败信封。`ok=false` 时**必须**带 `error`，否则前端只能显示空白。
pub fn err(msg: &str) -> Json {
    serde_json::json!({ "ok": false, "error": msg })
}

/// AT 类操作的结果封装（成功进 `result` 字段）。
pub fn at_err<T: serde::Serialize>(r: AtResult<T>) -> Json {
    match r {
        Ok(v) => ok(serde_json::json!({ "result": v })),
        Err(e) => err(&e),
    }
}

/// 读取请求体。解析失败或非 JSON 一律降级为 `Null`（调用方自行判空），
/// 不把 400 抛给前端 —— 多数调用点传的就是空体。
pub fn body(req: &mut Request) -> Json {
    let mut s = String::new();
    let _ = req.as_reader().read_to_string(&mut s);
    if s.trim().is_empty() {
        return Json::Null;
    }
    serde_json::from_str(&s).unwrap_or(Json::Null)
}

/// 解析 query string。键为空的段直接丢弃，值做 percent-decoding。
pub fn query(url: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let Some(pos) = url.find('?') else {
        return m;
    };
    for pair in url[pos + 1..].split('&') {
        let mut kv = pair.splitn(2, '=');
        let k = kv.next().unwrap_or("").to_string();
        let v = kv.next().unwrap_or("").to_string();
        if !k.is_empty() {
            m.insert(k, percent_decode(&v));
        }
    }
    m
}

/// percent-decoding（`+` 视为空格），非法转义原样保留。
pub fn percent_decode(s: &str) -> String {
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

/// 单个十六进制字符的值。
pub fn hexval(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// JSON 响应头。显式带 charset，避免 ucode / 浏览器按本地默认编码猜。
pub fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap()
}

/// 构造 JSON 响应。
pub fn respond(json: Json) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut resp = Response::from_string(json.to_string());
    resp.add_header(json_header());
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_flattens_fields() {
        let j = ok(serde_json::json!({ "pdp": 1 }));
        assert_eq!(j["ok"], Json::Bool(true));
        assert_eq!(j["pdp"], Json::from(1));
        assert!(j.get("data").is_none(), "不应额外包一层 data");
    }

    #[test]
    fn err_always_carries_error() {
        let j = err("boom");
        assert_eq!(j["ok"], Json::Bool(false));
        assert_eq!(j["error"], Json::String("boom".into()));
    }

    #[test]
    fn at_err_maps_both_branches() {
        let okj: AtResult<u8> = Ok(7);
        assert_eq!(at_err(okj)["result"], Json::from(7));
        let bad: AtResult<u8> = Err("nope".into());
        assert_eq!(at_err(bad)["error"], Json::String("nope".into()));
    }

    #[test]
    fn query_parses_and_decodes() {
        let m = query("/api/at?cmd=AT%2BCSQ&x=1&empty=");
        assert_eq!(m.get("cmd").map(|s| s.as_str()), Some("AT+CSQ"));
        assert_eq!(m.get("x").map(|s| s.as_str()), Some("1"));
        assert!(!m.contains_key("empty") || m["empty"].is_empty());
    }

    #[test]
    fn query_without_question_mark_is_empty() {
        assert!(query("/api/status").is_empty());
    }

    #[test]
    fn percent_decode_handles_plus_and_invalid_escapes() {
        assert_eq!(percent_decode("A+B"), "A B");
        assert_eq!(percent_decode("AT%2BCSQ"), "AT+CSQ");
        // 非法转义（%ZZ）原样保留，不能 panic 也不能吞掉字符
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
