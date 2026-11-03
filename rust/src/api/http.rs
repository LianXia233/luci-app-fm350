//! API 响应信封：把「业务返回什么」统一成固定形状。
//!
//! HTTP 报文的编解码（请求行解析、query、percent-decode、响应字节流）
//! 属于传输层，已迁往 [`crate::rt::server`]；本模块只负责信封。
//!
//! 信封约定：一律 `{"ok":bool, ...}`；`ok=false` 时**必须**带 `error`，
//! 否则前端只能显示空白。

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

/// 失败信封。
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
}
