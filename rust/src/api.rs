//! 本地 JSON API（仅监听 127.0.0.1，供 rpcd ucode 与 LuCI 前端调用）。
//!
//! 设计约定：
//!   - 只绑定回环地址，不对局域网暴露；
//!   - 一律返回 `{"ok":bool, ...}`，`ok=false` 时带 `error` 字段；
//!   - AT 口由 daemon 独占持有（整个运行期间持续持有），
//!     所有 AT 操作串行进入 `AtHandle`；
//!   - 每个请求重新读取配置（2 秒缓存），因此改配置后无需重启。

use std::collections::HashMap;
use std::sync::Arc;

use tiny_http::{Header, Method, Response, Server};

use crate::at::{AtHandle, AtResult};
use crate::config::Config;

pub type Json = serde_json::Value;

fn ok(data: Json) -> Json {
    let mut o = serde_json::Map::new();
    o.insert("ok".into(), Json::Bool(true));
    if let Json::Object(m) = data {
        for (k, v) in m {
            o.insert(k, v);
        }
    }
    Json::Object(o)
}

fn err(msg: &str) -> Json {
    serde_json::json!({ "ok": false, "error": msg })
}

fn at_err<T: serde::Serialize>(r: AtResult<T>) -> Json {
    match r {
        Ok(v) => ok(serde_json::json!({ "result": v })),
        Err(e) => err(&e),
    }
}

/// 读取请求体。
fn body(req: &mut tiny_http::Request) -> Json {
    let mut s = String::new();
    let _ = req.as_reader().read_to_string(&mut s);
    if s.trim().is_empty() {
        return Json::Null;
    }
    serde_json::from_str(&s).unwrap_or(Json::Null)
}

fn query(url: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(p) = url.find('?') {
        for pair in url[p + 1..].split('&') {
            let mut kv = pair.splitn(2, '=');
            let k = kv.next().unwrap_or("").to_string();
            let v = kv.next().unwrap_or("").to_string();
            if !k.is_empty() {
                m.insert(k, urlencoding_decode(&v));
            }
        }
    }
    m
}

fn urlencoding_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hexval(b[i + 1]), hexval(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
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

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap()
}

/// 派发一次请求。返回 JSON。
pub fn dispatch(
    at: &AtHandle,
    cfg: &Config,
    method: &Method,
    path: &str,
    q: &HashMap<String, String>,
    payload: &Json,
) -> Json {
    let cfg = cfg.clone();
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
            let ports = crate::at::list_ports(&cfg, probe);
            ok(serde_json::json!({
                "ports": ports,
                "current": cfg.at_port,
                "stats": at.stats(&cfg),
            }))
        }

        "/api/status" => ok(serde_json::json!({ "status": crate::modem::status(at, &cfg) })),
        "/api/info" => ok(serde_json::json!({ "info": crate::modem::info(at, &cfg) })),
        "/api/signal" => ok(serde_json::json!({ "signal": crate::modem::signal(at, &cfg) })),
        "/api/pdp" => ok(serde_json::json!({ "pdp": crate::modem::pdp(at, &cfg) })),
        "/api/net" => ok(serde_json::json!({ "net": crate::net::status(&cfg) })),
        "/api/cell" => ok(serde_json::json!({ "cell": crate::modem::cell_info(at, &cfg) })),
        "/api/lock" => ok(serde_json::json!({ "lock": crate::modem::lock_status(at, &cfg) })),

        "/api/dial" => {
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
            // APN 变更时落盘
            if c2.apn != cfg.apn || c2.pdp_type != cfg.pdp_type {
                let _ = crate::config::save(&serde_json::json!({
                    "apn": c2.apn,
                    "pdp_type": c2.pdp_type,
                }));
            }

            match crate::modem::dial(at, &c2) {
                Ok(pdp_state) => {
                    // 不再用 .ok() 吞掉错误：网络配置失败必须让调用方看见原因，
                    // 否则前端只能看到 net:null 而完全无从排查（实测踩过）。
                    match crate::net::apply_after_dial(
                        &c2,
                        &pdp_state.ipv4,
                        &pdp_state.ipv6,
                        &pdp_state.dns,
                        &pdp_state.gw4,
                    ) {
                        Ok(n) => ok(serde_json::json!({ "pdp": pdp_state, "net": n })),
                        Err(e) => ok(serde_json::json!({
                            "pdp": pdp_state,
                            "net": serde_json::Value::Null,
                            "net_error": e,
                        })),
                    }
                }
                Err(e) => err(&e),
            }
        }

        "/api/hangup" => {
            let r = crate::modem::hangup(at, &cfg);
            let _ = crate::net::teardown_iface(&cfg);
            at_err(r)
        }

        // ---- IMEI / 串号 ----
        // 读取始终允许；写入需 imei_write=1 且 confirm=1，执行前自动备份。
        "/api/imei" => {
            if *method == Method::Post {
                let value = payload
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let confirm = payload
                    .get("confirm")
                    .map(|v| v == true || v.as_str() == Some("1"))
                    .unwrap_or(false);
                match crate::imei::write(at, &cfg, &value, confirm) {
                    Ok(v) => ok(serde_json::json!({ "result": v })),
                    Err(e) => err(&e),
                }
            } else {
                ok(serde_json::json!({ "imei": crate::imei::state(at, &cfg) }))
            }
        }

        "/api/imei/backup" => at_err(crate::imei::backup(at, &cfg)),

        "/api/at" => {
            let cmd = payload
                .get("cmd")
                .and_then(|v| v.as_str())
                .or_else(|| q.get("cmd").map(|s| s.as_str()))
                .unwrap_or("")
                .to_string();
            if cmd.trim().is_empty() {
                return err("缺少 cmd");
            }
            if crate::at::is_imei_write(&cmd) {
                return err(&crate::at::imei_block_reason(&cmd));
            }
            at_err(at.with(&cfg, |p| p.command(&cmd)))
        }

        "/api/lock/band" => {
            let args: Vec<String> = payload
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|x| match x {
                            Json::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
                .or_else(|| q.get("args").map(|s| s.split(',').map(|x| x.to_string()).collect()))
                .unwrap_or_default();
            at_err(crate::modem::lock_band(at, &cfg, &args))
        }

        "/api/lock/cell" => {
            let args: Vec<String> = payload
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .map(|x| match x {
                            Json::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .collect()
                })
                .or_else(|| q.get("args").map(|s| s.split(',').map(|x| x.to_string()).collect()))
                .unwrap_or_default();
            at_err(crate::modem::lock_cell(at, &cfg, &args))
        }

        "/api/rat" => {
            if *method == Method::Post {
                let order: Vec<String> = payload
                    .get("order")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                at_err(crate::modem::set_rat_order(at, &cfg, &order))
            } else {
                at_err(crate::modem::rat_order(at, &cfg))
            }
        }

        "/api/sim" => {
            let slot = payload
                .get("slot")
                .and_then(|v| v.as_u64())
                .or_else(|| q.get("slot").and_then(|s| s.parse().ok()))
                .unwrap_or(0) as u32;
            at_err(crate::modem::set_sim_slot(at, &cfg, slot))
        }

        "/api/reboot" => at_err(crate::modem::reboot(at, &cfg)),

        "/api/cfun" => {
            let mode = payload
                .get("mode")
                .and_then(|v| v.as_u64())
                .or_else(|| q.get("mode").and_then(|s| s.parse().ok()))
                .unwrap_or(1) as u32;
            at_err(crate::modem::set_cfun(at, &cfg, mode))
        }

        "/api/usbmode" => {
            let mode = payload
                .get("mode")
                .and_then(|v| v.as_u64())
                .or_else(|| q.get("mode").and_then(|s| s.parse().ok()))
                .unwrap_or(40) as u32;
            at_err(crate::modem::set_usb_mode(at, &cfg, mode))
        }

        "/api/sms/list" => match crate::sms::list(at, &cfg) {
            Ok(v) => ok(serde_json::json!({ "messages": v })),
            Err(e) => err(&e),
        },

        "/api/sms/send" => {
            let number = payload
                .get("number")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = payload
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if number.is_empty() {
                return err("缺少收件人号码");
            }
            at_err(crate::sms::send(at, &cfg, &number, &text))
        }

        "/api/sms/delete" => {
            let index = payload
                .get("index")
                .and_then(|v| v.as_i64())
                .or_else(|| q.get("index").and_then(|s| s.parse().ok()));
            match index {
                Some(i) => at_err(crate::sms::delete(at, &cfg, i)),
                None => err("缺少 index"),
            }
        }

        "/api/sms/storage" => {
            if *method == Method::Post {
                let mem = payload
                    .get("mem")
                    .and_then(|v| v.as_str())
                    /* 默认值取 "SM"：实机 `AT+CPMS=?` 只返回 ("SM")，
                       "ME" 不在受支持列表内；与 storage() 解析失败的回落值一致。 */
                    .unwrap_or("SM")
                    .to_string();
                at_err(crate::sms::set_storage(at, &cfg, &mem))
            } else {
                match crate::sms::storage(at, &cfg) {
                    Ok(v) => ok(serde_json::json!({ "storage": v })),
                    Err(e) => err(&e),
                }
            }
        }

        "/api/sms/smsc" => {
            if *method == Method::Post {
                let n = payload
                    .get("number")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                at_err(crate::modem::set_sms_center(at, &cfg, &n))
            } else {
                at_err(at.with(&cfg, |p| p.command("AT+CSCA?")))
            }
        }

        _ => err(&format!("未知路径: {}", path)),
    }
}

/// 启动 API 服务（阻塞）。
pub fn serve(at: Arc<AtHandle>, cfg: Config) -> Result<(), String> {
    let addr = format!("127.0.0.1:{}", cfg.api_port);
    let server = Server::http(&addr).map_err(|e| format!("监听 {} 失败: {}", addr, e))?;
    eprintln!("fm350d: API 监听 {}", addr);

    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        let path = url.split('?').next().unwrap_or("/").to_string();
        let q = query(&url);
        let method = req.method().clone();
        let payload = body(&mut req);
        // 每个请求都取一次最新配置（内部 2 秒缓存）：
        // 用户在 LuCI 改完 at_port / apn / 各类开关后无需重启 daemon 即可生效。
        let cfg = crate::config::load_cached();
        let json = dispatch(&at, &cfg, &method, &path, &q, &payload);
        let text = json.to_string();
        let mut resp = Response::from_string(text);
        resp.add_header(json_header());
        let _ = req.respond(resp);
    }
    Ok(())
}
