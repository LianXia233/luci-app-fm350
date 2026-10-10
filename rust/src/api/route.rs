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

/// 模组缺席预检（LuCI 卡死防御第 0 层）。
///
/// 判定：配置的 AT 口节点不存在，**且**系统里没有任何疑似 FM350 的候选口
/// —— 即「模组未识别 / 已拔出」状态。此时所有依赖 AT 通道的接口立即返回
/// 明确错误，不再进入 `AtHandle`（打开失败也要等熔断判定，且哑口状态下
/// 单条命令要等满 at_timeout）。
///
/// 为什么不能只看配置端口：用户把 `at_port` 配错（指向不存在的 ttyUSB9）
/// 而模组其实在线时，候选口扫描仍会发现 2cb7 设备 —— 这种情况必须放行，
/// 让 open 错误本身（"设备不存在"）指明是配置问题。
///
/// 成本：一次 metadata + 一次 sysfs 读目录（亚毫秒），远低于任何 AT 往返。
fn modem_absent(cfg: &Config) -> Option<String> {
    if std::fs::metadata(&cfg.at_port).is_ok() {
        return None;
    }
    let has_fm350 = crate::at::list_ports(cfg, false)
        .iter()
        .any(|p| p.likely_fm350);
    if has_fm350 {
        return None;
    }
    Some(format!(
        "模组未识别：配置的 AT 口 {} 不存在，且未发现任何 Fibocom (2cb7) 串口。\
请检查模组供电与 USB 枚举，或到「高级配置 → AT 串口管理」重新扫描选择端口",
        cfg.at_port
    ))
}

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
        // AT 依赖类端点统一过模组缺席预检：模组未识别时亚毫秒返回，
        // 绝不进入 AT 串行队列拖住 rpcd。
        "/api/status" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => ok(serde_json::json!({ "status": crate::modem::status(at, cfg) })),
        },
        "/api/info" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => ok(serde_json::json!({ "info": crate::modem::info(at, cfg) })),
        },
        "/api/signal" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => ok(serde_json::json!({ "signal": crate::modem::signal(at, cfg) })),
        },
        "/api/pdp" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => ok(serde_json::json!({ "pdp": crate::modem::pdp(at, cfg) })),
        },
        "/api/net" => ok(serde_json::json!({ "net": crate::net::status(cfg) })),
        "/api/cell" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => ok(serde_json::json!({ "cell": crate::modem::cell_info(at, cfg) })),
        },
        "/api/lock" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => ok(serde_json::json!({ "lock": crate::modem::lock_status(at, cfg) })),
        },
        // EIF 兜底通道状态（f22-atproxy 的 +GT* URC 消费情况）。
        // 未刷修改 rootfs 的设备上 events 恒为 0，前端据此显示「未启用」。
        "/api/eif" => ok(serde_json::json!({ "eif": crate::at::urc::snapshot() })),

        // SSH 转发巡检快照：daemon 侧异步巡检，这里只读共享状态，不做任何
        // shell/adb 动作 —— 请求永远即时返回，绝不阻塞。
        "/api/sshfwd" => ok(serde_json::json!({ "sshfwd": crate::daemon::sshfwd::snapshot() })),

        // 模组 shell 命令执行（ADB / SSH 双通道）。同步执行但有 15s 超时
        // 包裹，adbd 离线时快速失败而非挂死 rpcd 代理。
        "/api/sshfwd/exec" => {
            if *method == Method::Post {
                let channel = arg_str(payload, q, "channel").unwrap_or_default();
                let cmd = arg_str(payload, q, "cmd").unwrap_or_default();
                let r = match channel.as_str() {
                    "adb" => crate::daemon::sshfwd::adb_exec(&cmd),
                    "ssh" => crate::daemon::sshfwd::ssh_exec(cfg, &cmd),
                    _ => Err("channel 必须为 adb 或 ssh".to_string()),
                };
                match r {
                    Ok(v) => ok(serde_json::json!({ "result": v })),
                    Err(e) => err(&e),
                }
            } else {
                err("仅支持 POST")
            }
        }

        // SSH 公钥引导：经 ADB 把 router 公钥注入模组 dropbear 免密清单。
        "/api/sshfwd/bootstrap" => {
            if *method == Method::Post {
                match crate::daemon::sshfwd::ssh_bootstrap() {
                    Ok(v) => ok(serde_json::json!({ "result": v })),
                    Err(e) => err(&e),
                }
            } else {
                err("仅支持 POST")
            }
        }

        // ---- 拨号 / 挂断 ----
        "/api/dial" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => dial(at, cfg, payload),
        },
        "/api/hangup" => {
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
            let r = crate::modem::hangup(at, cfg);
            let _ = crate::net::teardown_iface(cfg);
            at_err(r)
        }

        // ---- IMEI / 串号 ----
        // 读取始终允许；写入需 imei_write=1 且 confirm=1，执行前自动备份。
        // 守卫在 imei::write 内部，这里只做参数转发。
        "/api/imei" => {
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
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
        "/api/imei/backup" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => at_err(crate::imei::backup(at, cfg)),
        },

        // ---- AT 透传（高风险，IMEI 写入类被拦截）----
        "/api/at" => {
            let cmd = arg_str(payload, q, "cmd").unwrap_or_default();
            if cmd.trim().is_empty() {
                return err("缺少 cmd");
            }
            if crate::at::is_imei_write(&cmd) {
                return err(&crate::at::imei_block_reason(&cmd));
            }
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
            at_err(at.with(cfg, |p| p.command(&cmd)))
        }

        // ---- 锁频 / 锁小区 ----
        "/api/lock/band" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => at_err(crate::modem::lock_band(at, cfg, &arg_list(payload, q, "args"))),
        },
        "/api/lock/cell" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => at_err(crate::modem::lock_cell(at, cfg, &arg_list(payload, q, "args"))),
        },

        // ---- 制式 / SIM / 功能等级 ----
        "/api/rat" => {
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
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
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
            let slot = arg_u64(payload, q, "slot").unwrap_or(0) as u32;
            at_err(crate::modem::set_sim_slot(at, cfg, slot))
        }
        "/api/cfun" => {
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
            let mode = arg_u64(payload, q, "mode").unwrap_or(1) as u32;
            at_err(crate::modem::set_cfun(at, cfg, mode))
        }
        "/api/usbmode" => {
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
            let mode = arg_u64(payload, q, "mode").unwrap_or(40) as u32;
            at_err(crate::modem::set_usb_mode(at, cfg, mode))
        }
        "/api/reboot" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => at_err(crate::modem::reboot(at, cfg)),
        },

        // ---- 短信 ----
        "/api/sms/list" => match modem_absent(cfg) {
            Some(e) => err(&e),
            None => match crate::sms::list(at, cfg) {
                Ok(v) => ok(serde_json::json!({ "messages": v })),
                Err(e) => err(&e),
            },
        },
        "/api/sms/send" => {
            let number = arg_str(payload, q, "number").unwrap_or_default();
            let text = arg_str(payload, q, "text").unwrap_or_default();
            if number.is_empty() {
                return err("缺少收件人号码");
            }
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
            at_err(crate::sms::send(at, cfg, &number, &text))
        }
        "/api/sms/delete" => match arg_i64(payload, q, "index") {
            Some(i) => match modem_absent(cfg) {
                Some(e) => err(&e),
                None => at_err(crate::sms::delete(at, cfg, i)),
            },
            None => err("缺少 index"),
        },
        "/api/sms/storage" => {
            if let Some(e) = modem_absent(cfg) {
                return err(&e);
            }
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

    /// 模组缺席预检：沙箱环境无串口设备，默认配置必然命中「端口不存在且
    /// 无 2cb7 候选」→ 必须返回带指引的错误；这是 LuCI 防卡死的核心分支。
    #[test]
    fn modem_absent_reports_when_no_fm350_anywhere() {
        let cfg = Config::default();
        let r = modem_absent(&cfg);
        let msg = r.expect("无串口环境下必须判为模组缺席");
        assert!(msg.contains("模组未识别"), "错误信息: {}", msg);
        assert!(msg.contains(&cfg.at_port), "必须带上当前配置端口: {}", msg);
    }

    /// AT 透传在模组缺席时必须被预检拦截（不得进入 AT 串行队列）。
    #[test]
    fn at_passthrough_fast_fails_when_modem_absent() {
        let at = AtHandle::new();
        let cfg = Config::default();
        let payload = serde_json::json!({ "cmd": "AT+CSQ" });
        let j = dispatch(&at, &cfg, &Method::Post, "/api/at", &empty_q(), &payload);
        assert_eq!(j["ok"], Json::Bool(false));
        assert!(
            j["error"].as_str().unwrap().contains("模组未识别"),
            "错误信息: {}",
            j["error"]
        );
    }

    /// 端口枚举不受预检影响：模组缺席时它恰恰是用户排障的入口。
    #[test]
    fn ports_endpoint_bypasses_modem_absence_guard() {
        let at = AtHandle::new();
        let cfg = Config::default();
        let j = dispatch(&at, &cfg, &Method::Get, "/api/ports", &empty_q(), &Json::Null);
        assert_eq!(j["ok"], Json::Bool(true));
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
