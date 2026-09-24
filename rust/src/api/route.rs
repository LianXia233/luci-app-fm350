//! 路由表：把 HTTP 请求映射到具体业务动作。
//!
//! ## 约定
//!
//! * 一律返回 `{"ok":bool, ...}`；`ok=false` 时带 `error`（信封见 [`super::http`]）。
//! * **不对外暴露任何会改变模组身份的写操作**：IMEI 写入被 `at::is_imei_write`
//!   拦截（`/api/at` 与 CLI 两处都要拦，`AT+EGMREXT=1,7,...` 一旦落下去不可逆）。
//! * AT 口由 daemon 独占持有，所有 AT 操作串行进入 `AtHandle`，本层不做并发。

use std::collections::HashMap;

use tiny_http::Method;

use super::http::{at_err, err, ok, Json};
use crate::at::AtHandle;
use crate::config::Config;

/// 派发一次请求。
pub fn dispatch(
    at: &AtHandle,
    cfg: &Config,
    method: &Method,
    path: &str,
    q: &HashMap<String, String>,
    payload: &Json,
) -> Json {
    match path {
        "/api/ping" => ok(serde_json::json!({ "pong": true, "version": env!("CARGO_PKG_VERSION") })),

        "/api/config" => {
            if *method == Method::Post {
                match crate::config::save(payload) {
                    Ok(()) => ok(serde_json::json!({ "config": crate::config::load() })),
                    Err(e) => err(&e),
                }
            } else {
                ok(serde_json::json!({ "config": cfg }))
            }
        }

        // 候选 AT 口枚举 + 当前持有情况。不触碰 AT 数据，仅做独占探测。
        "/api/ports" => {
            let probe = if *method == Method::Post {
                true
            } else {
                q.get("probe").map(|v| v != "0").unwrap_or(true)
            };
            ok(serde_json::json!({
                "ports": crate::at::list_ports(cfg, probe),
                "current": cfg.at_port,
                "stats": at.stats(cfg),
            }))
        }

        // ---- 只读状态 ----
        "/api/status" => ok(serde_json::json!({ "status": crate::modem::status(at, cfg) })),
        "/api/info" => ok(serde_json::json!({ "info": crate::modem::info(at, cfg) })),
        "/api/signal" => ok(serde_json::json!({ "signal": crate::modem::signal(at, cfg) })),
        "/api/pdp" => ok(serde_json::json!({ "pdp": crate::modem::pdp(at, cfg) })),
        "/api/net" => ok(serde_json::json!({ "net": crate::net::status(cfg) })),
        "/api/cell" => ok(serde_json::json!({ "cell": crate::modem::cell_info(at, cfg) })),
        "/api/lock" => ok(serde_json::json!({ "lock": crate::modem::lock_status(at, cfg) })),

        // ---- 拨号 / 挂断 ----
        "/api/dial" => dial(at, cfg, payload),
        "/api/hangup" => {
            let r = crate::modem::hangup(at, cfg);
            let _ = crate::net::teardown_iface(cfg);
            at_err(r)
        }

        // ---- IMEI / 串号 ----
        // 读取始终允许；写入需 imei_write=1 且 confirm=1，执行前自动备份。
        // 守卫在 imei::write 内部，这里只做参数转发。
        "/api/imei" => {
            if *method == Method::Post {
                let value = arg_str(payload, q, "value").unwrap_or_default();
                let confirm = payload
                    .get("confirm")
                    .map(|v| v == true || v.as_str() == Some("1"))
                    .unwrap_or(false);
                match crate::imei::write(at, cfg, &value, confirm) {
                    Ok(v) => ok(serde_json::json!({ "result": v })),
                    Err(e) => err(&e),
                }
            } else {
                ok(serde_json::json!({ "imei": crate::imei::state(at, cfg) }))
            }
        }
        "/api/imei/backup" => at_err(crate::imei::backup(at, cfg)),

        // ---- AT 透传（高风险，IMEI 写入类被拦截）----
        "/api/at" => {
            let cmd = arg_str(payload, q, "cmd").unwrap_or_default();
            if cmd.trim().is_empty() {
                return err("缺少 cmd");
            }
            if crate::at::is_imei_write(&cmd) {
                return err(&crate::at::imei_block_reason(&cmd));
            }
            at_err(at.with(cfg, |p| p.command(&cmd)))
        }

        // ---- 锁频 / 锁小区 ----
        "/api/lock/band" => at_err(crate::modem::lock_band(at, cfg, &arg_list(payload, q, "args"))),
        "/api/lock/cell" => at_err(crate::modem::lock_cell(at, cfg, &arg_list(payload, q, "args"))),

        // ---- 制式 / SIM / 功能等级 ----
        "/api/rat" => {
            if *method == Method::Post {
                let order: Vec<String> = payload
                    .get("order")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                at_err(crate::modem::set_rat_order(at, cfg, &order))
            } else {
                at_err(crate::modem::rat_order(at, cfg))
            }
        }
        "/api/sim" => {
            let slot = arg_u64(payload, q, "slot").unwrap_or(0) as u32;
            at_err(crate::modem::set_sim_slot(at, cfg, slot))
        }
        "/api/cfun" => {
            let mode = arg_u64(payload, q, "mode").unwrap_or(1) as u32;
            at_err(crate::modem::set_cfun(at, cfg, mode))
        }
        "/api/usbmode" => {
            let mode = arg_u64(payload, q, "mode").unwrap_or(40) as u32;
            at_err(crate::modem::set_usb_mode(at, cfg, mode))
        }
        "/api/reboot" => at_err(crate::modem::reboot(at, cfg)),

        // ---- 短信 ----
        "/api/sms/list" => match crate::sms::list(at, cfg) {
            Ok(v) => ok(serde_json::json!({ "messages": v })),
            Err(e) => err(&e),
        },
        "/api/sms/send" => {
            let number = arg_str(payload, q, "number").unwrap_or_default();
            let text = arg_str(payload, q, "text").unwrap_or_default();
            if number.is_empty() {
                return err("缺少收件人号码");
            }
            at_err(crate::sms::send(at, cfg, &number, &text))
        }
        "/api/sms/delete" => match arg_i64(payload, q, "index") {
            Some(i) => at_err(crate::sms::delete(at, cfg, i)),
            None => err("缺少 index"),
        },
        "/api/sms/storage" => {
            if *method == Method::Post {
                // 默认值取 "SM"：实机 `AT+CPMS=?` 只返回 ("SM")，"ME" 不在
                // 受支持列表内；与 storage() 解析失败的回落值一致。
                let mem = arg_str(payload, q, "mem").unwrap_or_else(|| "SM".to_string());
                at_err(crate::sms::set_storage(at, cfg, &mem))
            } else {
                match crate::sms::storage(at, cfg) {
                    Ok(v) => ok(serde_json::json!({ "storage": v })),
                    Err(e) => err(&e),
                }
            }
        }
        "/api/sms/smsc" => {
            if *method == Method::Post {
                let n = arg_str(payload, q, "number").unwrap_or_default();
                at_err(crate::modem::set_sms_center(at, cfg, &n))
            } else {
                at_err(at.with(cfg, |p| p.command("AT+CSCA?")))
            }
        }

        _ => err(&format!("未知路径: {}", path)),
    }
}

// ---------------------------------------------------------------- 参数提取

/// 取字符串参数：优先 body，其次 query。空串视为未提供（让调用方走默认值）。
fn arg_str(payload: &Json, q: &HashMap<String, String>, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| q.get(key).filter(|s| !s.is_empty()).map(|s| s.as_str()))
        .map(str::to_string)
}

/// 取无符号整数参数。
fn arg_u64(payload: &Json, q: &HashMap<String, String>, key: &str) -> Option<u64> {
    payload
        .get(key)
        .and_then(|v| v.as_u64())
        .or_else(|| q.get(key).and_then(|s| s.parse().ok()))
}

/// 取有符号整数参数（短信序号等可能为负）。
fn arg_i64(payload: &Json, q: &HashMap<String, String>, key: &str) -> Option<i64> {
    payload
        .get(key)
        .and_then(|v| v.as_i64())
        .or_else(|| q.get(key).and_then(|s| s.parse().ok()))
}

/// 取字符串数组参数：body 里是数组，query 里退化为逗号分隔。
fn arg_list(payload: &Json, q: &HashMap<String, String>, key: &str) -> Vec<String> {
    payload
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .map(|x| match x {
                    Json::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect()
        })
        .or_else(|| {
            q.get(key)
                .map(|s| s.split(',').map(str::to_string).collect())
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- 拨号

/// 拨号：允许本次调用覆盖 APN / PDP 类型，覆盖值立即落盘。
fn dial(at: &AtHandle, cfg: &Config, payload: &Json) -> Json {
    let apn = payload
        .get("apn")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&cfg.apn)
        .to_string();
    let pdp = payload
        .get("pdp_type")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&cfg.pdp_type)
        .to_string();

    let mut c2 = cfg.clone();
    c2.apn = apn;
    c2.pdp_type = pdp;
    // APN 变更时落盘：否则下次 daemon 巡检又会用回旧值，表现为「改了没生效」。
    if c2.apn != cfg.apn || c2.pdp_type != cfg.pdp_type {
        let _ = crate::config::save(&serde_json::json!({
            "apn": c2.apn,
            "pdp_type": c2.pdp_type,
        }));
    }

    match crate::modem::dial(at, &c2) {
        Ok(st) => {
            // 不再用 .ok() 吞掉错误：网络配置失败必须让调用方看见原因，
            // 否则前端只能看到 net:null 而完全无从排查（实测踩过）。
            match crate::net::apply_after_dial(&c2, &st.ipv4, &st.ipv6, &st.dns, &st.gw4) {
                Ok(n) => ok(serde_json::json!({ "pdp": st, "net": n })),
                Err(e) => ok(serde_json::json!({
                    "pdp": st,
                    "net": serde_json::Value::Null,
                    "net_error": e,
                })),
            }
        }
        Err(e) => {
            // 拨号失败时补一条 `AT+EMBIND?` 体检结论。
            //
            // 逆向结论：主机侧之所以有 RNDIS，是因为 cid 被绑到了 `M-RNDIS`
            // （或 `M-MBIM`）。若绑定落在 `M-CCMNI`，数据面直接进模组自己的
            // OpenWrt，主机**必然拿不到地址** —— 而此时拨号流程会一路"成功"，
            // 只有地址始终为空。没有这条提示，现场只能靠猜。
            let mut msg = e;
            if let Some(b) = crate::modem::bind_status(at, &c2) {
                if !b.host_bound() {
                    msg.push_str(&format!("；数据通道体检：{}", b.verdict()));
                }
            }
            err(&msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_q() -> HashMap<String, String> {
        HashMap::new()
    }

    #[test]
    fn arg_str_prefers_body_over_query() {
        let p = serde_json::json!({ "cmd": "AT+CSQ" });
        let mut q = HashMap::new();
        q.insert("cmd".to_string(), "AT+CEREG?".to_string());
        assert_eq!(arg_str(&p, &q, "cmd"), Some("AT+CSQ".to_string()));
    }

    #[test]
    fn arg_str_treats_empty_as_absent() {
        let p = serde_json::json!({ "cmd": "" });
        let mut q = HashMap::new();
        q.insert("cmd".to_string(), "AT+CSQ".to_string());
        assert_eq!(arg_str(&p, &q, "cmd"), Some("AT+CSQ".to_string()));
    }

    #[test]
    fn arg_u64_reads_both_sources() {
        let p = serde_json::json!({ "slot": 1 });
        assert_eq!(arg_u64(&p, &empty_q(), "slot"), Some(1));
        let mut q = HashMap::new();
        q.insert("mode".to_string(), "0".to_string());
        assert_eq!(arg_u64(&Json::Null, &q, "mode"), Some(0));
        assert_eq!(arg_u64(&Json::Null, &empty_q(), "mode"), None);
    }

    #[test]
    fn arg_list_splits_query_by_comma() {
        let mut q = HashMap::new();
        q.insert("args".to_string(), "20,6,3".to_string());
        assert_eq!(arg_list(&Json::Null, &q, "args"), vec!["20", "6", "3"]);
    }

    #[test]
    fn unknown_path_is_rejected() {
        let at = AtHandle::new();
        let cfg = Config::default();
        let j = dispatch(&at, &cfg, &Method::Get, "/api/nope", &empty_q(), &Json::Null);
        assert_eq!(j["ok"], Json::Bool(false));
        assert!(j["error"].as_str().unwrap().contains("未知路径"));
    }

    /// IMEI 写入必须被拦在 AT 透传口之外 —— 这是不可逆操作。
    #[test]
    fn at_passthrough_blocks_imei_write() {
        let at = AtHandle::new();
        let cfg = Config::default();
        let payload = serde_json::json!({ "cmd": "AT+EGMREXT=1,7,\"123456789012345\"" });
        let j = dispatch(&at, &cfg, &Method::Post, "/api/at", &empty_q(), &payload);
        assert_eq!(j["ok"], Json::Bool(false), "IMEI 写入必须被拒绝");
        assert!(j["error"].as_str().unwrap().contains("IMEI"));
    }

    #[test]
    fn at_passthrough_requires_cmd() {
        let at = AtHandle::new();
        let cfg = Config::default();
        let j = dispatch(&at, &cfg, &Method::Post, "/api/at", &empty_q(), &Json::Null);
        assert_eq!(j["error"], Json::String("缺少 cmd".into()));
    }
}
