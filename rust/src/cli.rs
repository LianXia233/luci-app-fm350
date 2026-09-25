//! 命令行入口：所有子命令的派发。
//!
//! ## 两种运行形态
//!
//! 1. `fm350d daemon`：常驻，独占持有 AT 口并提供本地 JSON API，
//!    同时负责自动拨号与路由守护；
//! 2. `fm350d <子命令>`：一次性执行。若 daemon 在运行则通过 API 转发
//!    （避免争抢 AT 口），否则自行短暂打开 AT 口完成操作后释放。
//!
//! ## 转发优先
//!
//! 所有需要 AT 口的子命令都先尝试 daemon 的本地 API（[`via_daemon`]），
//! 失败才本地直连。这不是优化而是**必需**：daemon 全程独占 AT 口，CLI 若
//! 直接打开会被 `TIOCEXCL` 挡住或互相打断会话。
//!
//! 唯一例外是 `ports`（不触碰 AT 数据，仅枚举）：即使 daemon 不在也应能用，
//! 因为用户恰恰是在 daemon 拉不起来的时候需要它来排障。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::api::Json;
use crate::at::AtHandle;
use crate::config::{self, Config};
use crate::{daemon, imei, modem, net, sms};

/// 连接 daemon 的超时：拿不到就当它没在运行，走本地直连。
const DAEMON_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

pub const USAGE: &str = "\
fm350d —— FM350 模组后端守护与命令行工具

用法:
  fm350d daemon                       以守护模式运行（监听 127.0.0.1 的 JSON API）
  fm350d status                       综合状态（信息 + 信号 + PDP + 温度）
  fm350d info                         模组信息（厂商/型号/固件/IMEI/IMSI/ICCID）
  fm350d signal                       信号与注册状态
  fm350d pdp                          PDP 上下文状态
  fm350d net                          网络接口状态
  fm350d ports [--no-probe]           枚举候选 AT 端口与占用状态
  fm350d cell                         邻区与载波聚合信息
  fm350d lock                         当前锁定状态（频段/小区）
  fm350d lock-band <参数...>          锁频段，如 14 / 2 / 20 / 20,6,3,5078
  fm350d lock-cell <参数...>          锁小区，如 1,11,0,627264,280,3 / 0 取消
  fm350d dial                         拨号并配置网络接口
  fm350d hangup                       断开并拆除接口
  fm350d at <命令>                    透传 AT 指令（IMEI 写入类被拦截）
  fm350d sms list                     短信列表
  fm350d sms send <号码> <内容>       发送短信
  fm350d sms delete <序号>            删除短信
  fm350d sms storage [ME|SM]          查询/设置短信存储
  fm350d smsc [号码]                  查询/设置短信中心
  fm350d imei read                    读取 IMEI（只读）
  fm350d imei backup                  备份当前 IMEI 到 /etc/fm350/imei.backup
  fm350d imei write <15位> --confirm  写入/更换 IMEI（需先开启 imei_write）
  fm350d rat [顺序]                   查询/设置制式优先级，如 NR:LTE:WCDMA
  fm350d sim <0|1>                    切换 SIM 卡槽
  fm350d cfun <0|1>                   飞行模式 / 在线模式
  fm350d usbmode <模式>               设置 USB 模式（40 = RNDIS+AT）
  fm350d reboot                       重启模组
  fm350d config                       输出当前配置 JSON
  fm350d set <键> <值>                写入配置项

安全: IMEI 写入属高风险不可逆操作。默认关闭，需先开启 fm350.main.imei_write，
      再显式带 --confirm 才可执行；执行前会自动备份原值。
";

/// 通过 daemon 的本地 API 执行（daemon 未运行时返回 None）。
///
/// `read_timeout` 由调用方按语义决定：AT 类命令需要覆盖命令往返时间
/// （最长 at_timeout），而 `ports` 这类纯枚举命令给短超时 —— daemon
/// 无响应时快速回落到本地扫描，避免 LuCI 页面长时间转圈。
fn via_daemon(
    cfg: &Config,
    method: &str,
    path: &str,
    payload: Option<&Json>,
    read_timeout: Duration,
) -> Option<Json> {
    let addr = format!("127.0.0.1:{}", cfg.api_port);
    let mut stream = TcpStream::connect_timeout(&addr.parse().ok()?, DAEMON_CONNECT_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(read_timeout)).ok()?;

    let body = payload.map(|p| p.to_string()).unwrap_or_default();
    let req = format!(
        "{} {} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        method,
        path,
        body.len(),
        body
    );
    stream.write_all(req.as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut raw = String::new();
    stream.read_to_string(&mut raw).ok()?;
    let payload = raw.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(payload).ok()
}

/// 序列化成功值（无需 Result 包装）。
fn ok_value<T: serde::Serialize>(v: T) -> Json {
    match serde_json::to_value(v) {
        Ok(j) => serde_json::json!({ "ok": true, "result": j }),
        Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
    }
}

/// 把 Result 统一成 API 风格的 JSON。
fn ok_or_err<T: serde::Serialize>(r: Result<T, String>) -> Json {
    match r {
        Ok(v) => serde_json::json!({ "ok": true, "result": v }),
        Err(e) => serde_json::json!({ "ok": false, "error": e }),
    }
}

fn print_json(v: &Json) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

/// CLI 执行：优先转发给 daemon，否则本地直连 AT 口。
///
/// 读类（GET）命令的 daemon 转发读超时收敛到 [`CLI_READ_TIMEOUT`]：
/// daemon 存在但 AT 会话被长期占用时（如哑口每条命令等满 at_timeout），
/// 60 s 的等待会让 rpcd ucode 的同步 popen 一起挂住，LuCI 页面随之卡死；
/// 15 s 内失败后回落本地直连，快速给出「端口已被独占」类明确错误。
/// 写类（POST，如 dial 全流程含 PDP 激活）保留长超时，避免正常慢操作被掐断。
fn run_cli<F>(cfg: &Config, method: &str, path: &str, payload: Option<&Json>, local: F)
where
    F: FnOnce(&AtHandle, &Config) -> Json,
{
    let read_timeout = if method == "GET" {
        Duration::from_secs(CLI_READ_TIMEOUT.max(cfg.at_timeout))
    } else {
        Duration::from_secs(cfg.at_timeout.max(60))
    };
    if let Some(j) = via_daemon(cfg, method, path, payload, read_timeout) {
        print_json(&j);
        return;
    }
    let handle = AtHandle::new();
    let j = local(&handle, cfg);
    print_json(&j);
}

/// 读类命令经 daemon 转发的读超时下限（秒）。
/// 必须小于 rpcd ucode 读类兜底 timeout（fm350.uc 的 20 s），留出余量。
const CLI_READ_TIMEOUT: u64 = 15;

/// 守护模式入口：单实例守卫 + 主循环。
fn run_daemon(cfg: Config) {
    let _guard = match daemon::acquire() {
        Some(g) => g,
        None => {
            eprintln!("fm350d: 已有实例在运行（{}），本进程退出", daemon::LOCK_FILE);
            return;
        }
    };
    if let Err(e) = daemon::run(cfg) {
        eprintln!("fm350d: {}", e);
        std::process::exit(1);
    }
}

/// 入口：解析 `args`（不含 argv[0]）并派发。
pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{}", USAGE);
        return;
    }

    let cmd = args[0].clone();
    let cfg = config::load();

    if cmd == "daemon" {
        run_daemon(cfg);
        return;
    }

    let rest = &args[1..];

    match cmd.as_str() {
        "status" => run_cli(&cfg, "GET", "/api/status", None, |a, c| {
            ok_value(modem::status(a, c))
        }),
        "info" => run_cli(&cfg, "GET", "/api/info", None, |a, c| {
            ok_value(modem::info(a, c))
        }),
        "signal" => run_cli(&cfg, "GET", "/api/signal", None, |a, c| {
            ok_value(modem::signal(a, c))
        }),
        "pdp" => run_cli(&cfg, "GET", "/api/pdp", None, |a, c| {
            ok_value(modem::pdp(a, c))
        }),
        "net" => print_json(&serde_json::json!({ "ok": true, "net": net::status(&cfg) })),
        // 端口枚举不占用 AT 口；daemon 在跑时优先走它，探测结果更准
        // （daemon 自己持有端口时能如实报告"由本进程持有"）。
        "ports" => {
            let probe = !rest.iter().any(|x| x == "--no-probe");
            let path = if probe {
                "/api/ports"
            } else {
                "/api/ports?probe=0"
            };
            // 关键：这里用 2 秒短读超时。daemon 存在但无响应（巡检阻塞 /
            // 单实例锁竞争 / 半死状态）时，若沿用 run_cli 的最长 60 秒
            // 超时，`fm350d ports` 会白等一分钟，LuCI 页面表现为
            // 「扫描不到任何端口」。短超时让本地扫描立即接管。
            match via_daemon(&cfg, "GET", path, None, Duration::from_secs(2)) {
                Some(j) => print_json(&j),
                None => print_json(&serde_json::json!({
                    "ok": true,
                    "ports": crate::at::list_ports(&cfg, probe),
                    "current": cfg.at_port,
                })),
            }
        }
        "cell" => run_cli(&cfg, "GET", "/api/cell", None, |a, c| {
            ok_value(modem::cell_info(a, c))
        }),
        "lock" => run_cli(&cfg, "GET", "/api/lock", None, |a, c| {
            ok_value(modem::lock_status(a, c))
        }),

        "lock-band" => {
            let args: Vec<String> = rest.to_vec();
            let payload = serde_json::json!({ "args": args });
            run_cli(&cfg, "POST", "/api/lock/band", Some(&payload), |a, c| {
                ok_or_err(modem::lock_band(a, c, &args))
            })
        }
        "lock-cell" => {
            let args: Vec<String> = rest.to_vec();
            let payload = serde_json::json!({ "args": args });
            run_cli(&cfg, "POST", "/api/lock/cell", Some(&payload), |a, c| {
                ok_or_err(modem::lock_cell(a, c, &args))
            })
        }

        "dial" => run_cli(
            &cfg,
            "POST",
            "/api/dial",
            // 与 api 的 /api/dial 对齐：payload 携带 apn/pdp_type，本地直连
            // 路径按本地最新配置执行。
            Some(&serde_json::json!({ "apn": cfg.apn, "pdp_type": cfg.pdp_type })),
            |a, c| match modem::dial(a, c) {
                Ok(p) => {
                    // 不把网络配置失败压成 net:false：以 net_error 带出原因
                    match net::apply_after_dial(c, &p.ipv4, &p.ipv6, &p.dns, &p.gw4) {
                        Ok(n) => serde_json::json!({ "ok": true, "pdp": p, "net": n }),
                        Err(e) => serde_json::json!({
                            "ok": true,
                            "pdp": p,
                            "net": serde_json::Value::Null,
                            "net_error": e,
                        }),
                    }
                }
                Err(e) => serde_json::json!({ "ok": false, "error": e }),
            },
        ),
        "hangup" => run_cli(&cfg, "POST", "/api/hangup", None, |a, c| {
            let r = modem::hangup(a, c);
            let _ = net::teardown_iface(c);
            ok_or_err(r)
        }),

        "at" => {
            let c = rest.join(" ");
            if c.trim().is_empty() {
                eprintln!("缺少 AT 指令");
                std::process::exit(2);
            }
            let payload = serde_json::json!({ "cmd": c });
            run_cli(&cfg, "POST", "/api/at", Some(&payload), |a, cc| {
                if crate::at::is_imei_write(&c) {
                    return serde_json::json!({ "ok": false, "error": crate::at::imei_block_reason(&c) });
                }
                ok_or_err(a.with(cc, |p| p.command(&c)))
            })
        }

        "sms" => sms_command(&cfg, rest),
        "smsc" => match rest.first().cloned() {
            Some(n) => {
                let payload = serde_json::json!({ "number": n });
                run_cli(&cfg, "POST", "/api/sms/smsc", Some(&payload), |a, c| {
                    ok_or_err(modem::set_sms_center(a, c, &n))
                })
            }
            None => run_cli(&cfg, "GET", "/api/sms/smsc", None, |a, c| {
                ok_or_err(a.with(c, |p| p.command(crate::at::at_cmd::CSCA_READ)))
            }),
        },

        "imei" => imei_command(&cfg, rest),

        "rat" => match rest.first().cloned() {
            Some(order) => {
                let v: Vec<String> = order.split(':').map(|s| s.to_string()).collect();
                let payload = serde_json::json!({ "order": v });
                run_cli(&cfg, "POST", "/api/rat", Some(&payload), |a, c| {
                    ok_or_err(modem::set_rat_order(a, c, &v))
                })
            }
            None => run_cli(&cfg, "GET", "/api/rat", None, |a, c| {
                ok_or_err(modem::rat_order(a, c))
            }),
        },

        "sim" => {
            let slot: u32 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            let payload = serde_json::json!({ "slot": slot });
            run_cli(&cfg, "POST", "/api/sim", Some(&payload), |a, c| {
                ok_or_err(modem::set_sim_slot(a, c, slot))
            })
        }
        "cfun" => {
            let mode: u32 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            let payload = serde_json::json!({ "mode": mode });
            run_cli(&cfg, "POST", "/api/cfun", Some(&payload), |a, c| {
                ok_or_err(modem::set_cfun(a, c, mode))
            })
        }
        "usbmode" => {
            let mode: u32 = rest.first().and_then(|s| s.parse().ok()).unwrap_or(40);
            let payload = serde_json::json!({ "mode": mode });
            run_cli(&cfg, "POST", "/api/usbmode", Some(&payload), |a, c| {
                ok_or_err(modem::set_usb_mode(a, c, mode))
            })
        }
        "reboot" => run_cli(&cfg, "POST", "/api/reboot", None, |a, c| {
            ok_or_err(modem::reboot(a, c))
        }),

        "config" => print_json(&serde_json::json!({ "ok": true, "config": cfg })),
        "set" => {
            let key = rest.first().cloned().unwrap_or_default();
            let val = rest.get(1).cloned().unwrap_or_default();
            if key.is_empty() {
                eprintln!("用法: fm350d set <键> <值>");
                std::process::exit(2);
            }
            match config::save(&serde_json::json!({ key: val })) {
                Ok(()) => print_json(&serde_json::json!({ "ok": true, "config": config::load() })),
                Err(e) => print_json(&serde_json::json!({ "ok": false, "error": e })),
            }
        }

        other => {
            eprintln!("未知命令: {}\n", other);
            print!("{}", USAGE);
            std::process::exit(2);
        }
    }
}

/// `fm350d sms <子命令>`。
fn sms_command(cfg: &Config, rest: &[String]) {
    let sub = rest.first().cloned().unwrap_or_default();
    match sub.as_str() {
        "list" => run_cli(cfg, "GET", "/api/sms/list", None, |a, c| {
            ok_or_err(sms::list(a, c))
        }),
        "send" => {
            let number = rest.get(1).cloned().unwrap_or_default();
            let text = rest.get(2).cloned().unwrap_or_default();
            let payload = serde_json::json!({ "number": number, "text": text });
            run_cli(cfg, "POST", "/api/sms/send", Some(&payload), |a, c| {
                ok_or_err(sms::send(a, c, &number, &text))
            })
        }
        "delete" => {
            let index: i64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
            let payload = serde_json::json!({ "index": index });
            run_cli(cfg, "POST", "/api/sms/delete", Some(&payload), |a, c| {
                ok_or_err(sms::delete(a, c, index))
            })
        }
        "storage" => match rest.get(1).cloned() {
            Some(mem) => {
                let payload = serde_json::json!({ "mem": mem });
                run_cli(cfg, "POST", "/api/sms/storage", Some(&payload), |a, c| {
                    ok_or_err(sms::set_storage(a, c, &mem))
                })
            }
            None => run_cli(cfg, "GET", "/api/sms/storage", None, |a, c| {
                ok_or_err(sms::storage(a, c))
            }),
        },
        other => {
            eprintln!("未知 sms 子命令: {}", other);
            std::process::exit(2);
        }
    }
}

/// `fm350d imei <子命令>`。
fn imei_command(cfg: &Config, rest: &[String]) {
    let sub = rest.first().cloned().unwrap_or_default();
    match sub.as_str() {
        "read" => run_cli(cfg, "GET", "/api/imei", None, |a, c| {
            ok_value(imei::state(a, c))
        }),
        "backup" => run_cli(cfg, "POST", "/api/imei/backup", None, |a, c| {
            ok_or_err(imei::backup(a, c))
        }),
        "write" => {
            let value = rest.get(1).cloned().unwrap_or_default();
            let confirm = rest.iter().any(|x| x == "--confirm");
            let payload = serde_json::json!({ "value": value, "confirm": confirm });
            run_cli(cfg, "POST", "/api/imei", Some(&payload), |a, c| {
                ok_or_err(imei::write(a, c, &value, confirm))
            })
        }
        other => {
            eprintln!("未知 imei 子命令: {}", other);
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 帮助文本必须列出全部子命令 —— 这是用户排障时唯一能看到的入口文档。
    #[test]
    fn usage_lists_every_subcommand() {
        for cmd in [
            "daemon", "status", "info", "signal", "pdp", "net", "ports", "cell", "lock",
            "lock-band", "lock-cell", "dial", "hangup", "at", "sms", "smsc", "imei", "rat",
            "sim", "cfun", "usbmode", "reboot", "config", "set",
        ] {
            assert!(USAGE.contains(&format!("fm350d {}", cmd)), "缺少 {}", cmd);
        }
        // 安全提示必须保留：这是唯一一处说明 IMEI 写入风险的地方
        assert!(USAGE.contains("高风险不可逆"));
    }

    /// IMEI 写入拦截必须同时存在于 CLI 与 API 两条路径 —— 单测锁住 CLI 这条。
    #[test]
    fn imei_write_guard_literal_is_shared() {
        let cmd = crate::at::at_cmd::egmrext_write_imei("123456789012345");
        assert!(crate::at::is_imei_write(&cmd));
    }
}
