//! fm350d —— luci-app-fm350 的 Rust 后端。
//!
//! 两种运行形态：
//!   1. `fm350d daemon`：常驻，独占持有 AT 口并提供本地 JSON API，
//!      同时负责自动拨号与路由守护；
//!   2. `fm350d <子命令>`：一次性执行。若 daemon 在运行则通过 API 转发（避免争抢 AT 口），
//!      否则自行短暂打开 AT 口完成操作后释放。
//!
//! 独立配置体系：配置文件为 `/etc/config/fm350`（section `main`）。

mod api;
mod at;
mod config;
mod imei;
mod modem;
mod net;
mod sms;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::Json;

const USAGE: &str = "\
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

// ---------------------------------------------------------------- daemon 转发

/// 通过 daemon 的本地 API 执行（daemon 未运行时返回 None）。
fn via_daemon(
    cfg: &config::Config,
    method: &str,
    path: &str,
    payload: Option<&Json>,
) -> Option<Json> {
    let addr = format!("127.0.0.1:{}", cfg.api_port);
    let mut stream =
        TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(500)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(cfg.at_timeout.max(60))))
        .ok()?;

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

fn print_json(v: &Json) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
    );
}

/// CLI 执行：优先转发给 daemon，否则本地直连 AT 口。
fn run_cli<F>(cfg: &config::Config, method: &str, path: &str, payload: Option<&Json>, local: F)
where
    F: FnOnce(&at::AtHandle, &config::Config) -> Json,
{
    if let Some(j) = via_daemon(cfg, method, path, payload) {
        print_json(&j);
        return;
    }
    let handle = at::AtHandle::new();
    let j = local(&handle, cfg);
    print_json(&j);
}

// ---------------------------------------------------------------- daemon

// ---------------------------------------------------------------- 单实例守卫

/// 运行时锁文件。AT 口本身是独占资源（TIOCEXCL），但换包升级或
/// procd 自动拉起后紧接着又执行 start 时，可能出现两个实例同时存活，
/// 互相争抢串口并刷一连串 "Unable to acquire exclusive lock"。
/// 这里用 flock 做兜底：抢不到锁的实例直接退出，绝不触碰 AT 口。
const LOCK_FILE: &str = "/var/run/fm350d.lock";
const V6_REFRESH_MIN_INTERVAL: Duration = Duration::from_secs(60);
const V6_MODEM_POLL_MIN_INTERVAL: Duration = Duration::from_secs(60);

#[cfg(unix)]
fn acquire_singleton() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;

    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(LOCK_FILE)
        .ok()?;

    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return None;
    }
    Some(f)
}

/// 非 Unix 主机（仅开发机本地 cargo check/test 用）不做锁定，
/// 目标平台始终是 Linux，因此该分支不会进入实际部署。
#[cfg(not(unix))]
fn acquire_singleton() -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(std::env::temp_dir().join("fm350d.lock"))
        .ok()
}

fn daemon_loop(cfg_initial: config::Config) -> Result<(), String> {
    let at = Arc::new(at::AtHandle::new());

    // API 服务放到子线程，主线程跑巡检任务
    let at_api = Arc::clone(&at);
    let api_cfg = cfg_initial.clone();
    std::thread::spawn(move || {
        if let Err(e) = api::serve(at_api, api_cfg) {
            eprintln!("fm350d: API 退出: {}", e);
        }
    });

    eprintln!("fm350d: 守护已启动");
    // 上一次观测到的"其他占用者"。只在集合变化时打印，避免每轮刷日志。
    let mut last_intruders: Vec<u64> = Vec::new();
    let mut last_v6_refresh: Option<Instant> = None;
    let mut last_v6_modem_poll: Option<Instant> = None;
    let mut last_v6_scheduled_refresh = Instant::now();
    let mut last_modem_ipv6 = String::new();
    loop {
        let cfg = config::load();
        let interval = cfg.poll_interval.max(5);

        let v6_modem_poll_due = cfg.enabled
            && cfg.ipv6
            && !cfg.iface_v6.is_empty()
            && cfg.v6_poll_interval > 0
            && last_v6_modem_poll
                .map(|t| {
                    t.elapsed() >= Duration::from_secs(cfg.v6_poll_interval.max(60))
                        && t.elapsed() >= V6_MODEM_POLL_MIN_INTERVAL
                })
                .unwrap_or(true);

        if cfg.enabled && cfg.auto_dial {
            let st = modem::pdp(&at, &cfg);
            if v6_modem_poll_due {
                last_v6_modem_poll = Some(Instant::now());
                if !st.ipv6.is_empty() && st.ipv6 != last_modem_ipv6 {
                    eprintln!("fm350d: 模组侧 IPv6 更新为 {}", st.ipv6);
                    last_modem_ipv6 = st.ipv6.clone();
                    if net::apply_ipv6_addr(&cfg, &st.ipv6) {
                        eprintln!("fm350d: 模组侧 IPv6 变化，已应用到 {}", cfg.iface_v6);
                    } else {
                        eprintln!("fm350d: 模组侧 IPv6 变化，应用 {} 失败", cfg.iface_v6);
                    }
                    let now = Instant::now();
                    last_v6_refresh = Some(now);
                    last_v6_scheduled_refresh = now;
                } else if st.ipv6.is_empty() && !last_modem_ipv6.is_empty() {
                    eprintln!("fm350d: 模组侧 IPv6 暂未上报");
                    last_modem_ipv6.clear();
                }
            }
            if !st.active {
                eprintln!("fm350d: PDP 未激活，尝试自动拨号");
                match modem::dial(&at, &cfg) {
                    Ok(p) => {
                        if p.ipv4.is_empty() {
                            eprintln!("fm350d: 拨号成功但未取得 IPv4，稍后重试");
                        } else {
                            match net::apply_after_dial(&cfg, &p.ipv4, &p.ipv6, &p.dns) {
                                Ok(n) => eprintln!("fm350d: 已拨号并配置网络 {:?}", n.ipv4),
                                Err(e) => eprintln!("fm350d: 配置网络失败: {}", e),
                            }
                        }
                    }
                    Err(e) => eprintln!("fm350d: 拨号失败: {}", e),
                }
            } else if !st.ipv4.is_empty() {
                // 地址变化或接口缺失时重新应用。
                // 这里必须打日志：原先 `let _ =` 把错误吞掉，出现过
                // 「模组 PDP 正常、主机侧却一直没有 IP」的静默故障。
                // 成功时只在确有偏差的那一轮打印，不会每 30 s 刷屏。
                let ns = net::status(&cfg);
                if !ns.ipv4.contains(&st.ipv4) {
                    match net::apply_after_dial(&cfg, &st.ipv4, &st.ipv6, &st.dns) {
                        Ok(n) => eprintln!(
                            "fm350d: 接口缺失或地址变化，已重新应用网络配置 {:?}",
                            n.ipv4
                        ),
                        Err(e) => eprintln!("fm350d: 重新应用网络配置失败: {}", e),
                    }
                }
                if cfg.ipv6 && !cfg.iface_v6.is_empty() {
                    // 模组侧有 IPv6 而接口上没有（或不是同一个地址）时，
                    // 直接把模组侧地址静态写入并补设备路由 —— 静态方案下
                    // 「刷新」的意义就是重新应用模组侧地址，而非 ifup 空转。
                    let v6_missing = !st.ipv6.is_empty()
                        && !ns.ipv6.iter().any(|a| a == &st.ipv6);
                    let v6_refresh_due = cfg.v6_refresh_interval > 0
                        && last_v6_scheduled_refresh.elapsed()
                            >= Duration::from_secs(cfg.v6_refresh_interval.max(60));
                    let v6_recovery_due = ns.ipv6.is_empty()
                        && last_v6_refresh
                            .map(|t| t.elapsed() >= V6_REFRESH_MIN_INTERVAL)
                            .unwrap_or(true);

                    if v6_missing || v6_recovery_due || v6_refresh_due {
                        let reason = if v6_missing {
                            "接口未持有模组侧 IPv6"
                        } else if v6_recovery_due {
                            "未发现有效全局 IPv6"
                        } else {
                            "到达 IPv6 定时刷新周期"
                        };
                        let done = if st.ipv6.is_empty() {
                            net::refresh_ipv6_iface(&cfg)
                        } else {
                            net::apply_ipv6_addr(&cfg, &st.ipv6)
                        };
                        if done {
                            eprintln!("fm350d: {}，已刷新 {}", reason, cfg.iface_v6);
                        } else {
                            eprintln!("fm350d: {}，刷新 {} 失败", reason, cfg.iface_v6);
                        }
                        let now = Instant::now();
                        last_v6_refresh = Some(now);
                        last_v6_scheduled_refresh = now;
                    }
                }
                // 开机自启补齐：上面只在「地址有偏差」时才重写配置，稳态下
                // auto 一旦不是 1 就永远补不回来（LuCI 显示「开机时未启动」）。
                // 这里每轮无条件校验一次，成本 2~4 次 uci get；仅在确有修正时打日志。
                let fixed = net::ensure_autostart(&cfg);
                if !fixed.is_empty() {
                    eprintln!("fm350d: 已把接口 {:?} 恢复为开机自启", fixed);
                }
            }
        } else if v6_modem_poll_due {
            let st = modem::pdp(&at, &cfg);
            last_v6_modem_poll = Some(Instant::now());
            if !st.ipv6.is_empty() && st.ipv6 != last_modem_ipv6 {
                eprintln!("fm350d: 模组侧 IPv6 更新为 {}", st.ipv6);
                last_modem_ipv6 = st.ipv6.clone();
                if net::apply_ipv6_addr(&cfg, &st.ipv6) {
                    eprintln!("fm350d: 模组侧 IPv6 变化，已应用到 {}", cfg.iface_v6);
                } else {
                    eprintln!("fm350d: 模组侧 IPv6 变化，应用 {} 失败", cfg.iface_v6);
                }
                let now = Instant::now();
                last_v6_refresh = Some(now);
                last_v6_scheduled_refresh = now;
            } else if st.ipv6.is_empty() && !last_modem_ipv6.is_empty() {
                eprintln!("fm350d: 模组侧 IPv6 暂未上报");
                last_modem_ipv6.clear();
            }
        }

        if cfg.route_guard {
            if net::route_guard(&cfg) {
                eprintln!("fm350d: 已补齐默认设备路由");
            }
        }

        // AT 口由 daemon 持续独占，此处不释放端口。
        // 唯一例外：用户在 LuCI 改了 at_port——释放旧句柄，
        // 让下次访问按新端口重新独占打开，无需重启服务。
        match at.current_path() {
            Some(cur) if cur != cfg.at_port => {
                eprintln!(
                    "fm350d: AT 端口配置变更 {} → {}，释放旧句柄",
                    cur, cfg.at_port
                );
                at.close();
            }
            _ => {}
        }

        // 独占事实巡检：TIOCEXCL 对 root（CAP_SYS_ADMIN）无效，内核不提供
        // 强制排他（见 at.rs 模块注释），因此"是否真被抢占"只能主动观测。
        // 占用者集合变化时才打印，避免每轮刷日志。
        let st = at.stats(&cfg);
        if st.open && st.other_pids != last_intruders {
            if st.other_pids.is_empty() {
                if !last_intruders.is_empty() {
                    eprintln!(
                        "fm350d: AT 口 {} 恢复独占（此前被 pid {:?} 占用）",
                        st.path, last_intruders
                    );
                }
            } else {
                eprintln!(
                    "fm350d: 警告：AT 口 {} 另有进程打开（pid {:?}），独占事实上已被破坏",
                    st.path, st.other_pids
                );
            }
            last_intruders = st.other_pids.clone();
        }

        std::thread::sleep(Duration::from_secs(interval));
    }
}

// ---------------------------------------------------------------- 入口

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "help" {
        print!("{}", USAGE);
        return;
    }

    let cmd = args[0].clone();
    let cfg = config::load();

    if cmd == "daemon" {
        // 单实例守卫：拿不到锁说明已有实例在跑，直接退出，不争抢串口
        let _guard = match acquire_singleton() {
            Some(g) => g,
            None => {
                eprintln!("fm350d: 已有实例在运行（{}），本进程退出", LOCK_FILE);
                return;
            }
        };
        if let Err(e) = daemon_loop(cfg) {
            eprintln!("fm350d: {}", e);
            std::process::exit(1);
        }
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
            match via_daemon(&cfg, "GET", path, None) {
                Some(j) => print_json(&j),
                None => print_json(&serde_json::json!({
                    "ok": true,
                    "ports": at::list_ports(&cfg, probe),
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
            Some(&Json::Null),
            |a, c| match modem::dial(a, c) {
                Ok(p) => {
                    let n = net::apply_after_dial(c, &p.ipv4, &p.ipv6, &p.dns);
                    serde_json::json!({ "ok": true, "pdp": p, "net": n.ok() })
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
                if at::is_imei_write(&c) {
                    return serde_json::json!({ "ok": false, "error": at::imei_block_reason(&c) });
                }
                ok_or_err(a.with(cc, |p| p.command(&c)))
            })
        }

        "sms" => {
            let sub = rest.first().cloned().unwrap_or_default();
            match sub.as_str() {
                "list" => run_cli(&cfg, "GET", "/api/sms/list", None, |a, c| {
                    ok_or_err(sms::list(a, c))
                }),
                "send" => {
                    let number = rest.get(1).cloned().unwrap_or_default();
                    let text = rest.get(2).cloned().unwrap_or_default();
                    let payload = serde_json::json!({ "number": number, "text": text });
                    run_cli(&cfg, "POST", "/api/sms/send", Some(&payload), |a, c| {
                        ok_or_err(sms::send(a, c, &number, &text))
                    })
                }
                "delete" => {
                    let index: i64 = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1);
                    let payload = serde_json::json!({ "index": index });
                    run_cli(&cfg, "POST", "/api/sms/delete", Some(&payload), |a, c| {
                        ok_or_err(sms::delete(a, c, index))
                    })
                }
                "storage" => match rest.get(1).cloned() {
                    Some(mem) => {
                        let payload = serde_json::json!({ "mem": mem });
                        run_cli(&cfg, "POST", "/api/sms/storage", Some(&payload), |a, c| {
                            ok_or_err(sms::set_storage(a, c, &mem))
                        })
                    }
                    None => run_cli(&cfg, "GET", "/api/sms/storage", None, |a, c| {
                        ok_or_err(sms::storage(a, c))
                    }),
                },
                other => {
                    eprintln!("未知 sms 子命令: {}", other);
                    std::process::exit(2);
                }
            }
        }

        "smsc" => match rest.first().cloned() {
            Some(n) => {
                let payload = serde_json::json!({ "number": n });
                run_cli(&cfg, "POST", "/api/sms/smsc", Some(&payload), |a, c| {
                    ok_or_err(modem::set_sms_center(a, c, &n))
                })
            }
            None => run_cli(&cfg, "GET", "/api/sms/smsc", None, |a, c| {
                ok_or_err(a.with(c, |p| p.command("AT+CSCA?")))
            }),
        },

        "imei" => {
            let sub = rest.first().cloned().unwrap_or_default();
            match sub.as_str() {
                "read" => run_cli(&cfg, "GET", "/api/imei", None, |a, c| {
                    ok_value(imei::state(a, c))
                }),
                "backup" => run_cli(&cfg, "POST", "/api/imei/backup", None, |a, c| {
                    ok_or_err(imei::backup(a, c))
                }),
                "write" => {
                    let value = rest.get(1).cloned().unwrap_or_default();
                    let confirm = rest.iter().any(|x| x == "--confirm");
                    let payload = serde_json::json!({ "value": value, "confirm": confirm });
                    run_cli(&cfg, "POST", "/api/imei", Some(&payload), |a, c| {
                        ok_or_err(imei::write(a, c, &value, confirm))
                    })
                }
                other => {
                    eprintln!("未知 imei 子命令: {}", other);
                    std::process::exit(2);
                }
            }
        }

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
