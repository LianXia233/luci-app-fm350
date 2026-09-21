//! 网络接口管理与路由守护。
//!
//! FM350 的 RNDIS 数据通道**不提供 DHCP**，IPv4 必须以静态地址配置：
//!   - 地址取自 `AT+CGPADDR`，掩码固定 /32；
//!   - DNS 取自 `AT+GTDNS`；
//!   - 由于没有网关，netifd 不会下发设备路由，需要额外一条
//!     `default dev <dev> metric <m> onlink` 并周期补齐（route guard）。
//!
//! IPv6 通过 `device=@fm350` 的 dhcpv6 子接口获取。
//!
//! 接口、路由与防火墙区段均由本插件独立创建与管理。

use std::fs;
use std::process::Command;
use std::time::Duration;

use crate::config::Config;

/// 可能的 RNDIS/ECM 数据通道驱动。
const DATA_DRIVERS: &[&str] = &["rndis_host", "cdc_ether", "cdc_ncm", "qmi_wwan", "mbim"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// 只回显命令，不执行（用于自测）。
    Dry,
    Real,
}

fn sh(mode: RunMode, script: &str) -> (bool, String) {
    if mode == RunMode::Dry {
        return (true, script.to_string());
    }
    match Command::new("sh").arg("-c").arg(script).output() {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
        ),
        Err(e) => (false, e.to_string()),
    }
}

fn real(script: &str) -> (bool, String) {
    sh(RunMode::Real, script)
}

/// 单引号包裹 shell 参数。
///
/// 为什么需要它：`uci` 的值里一旦含空格（典型是多值 dns "A B"），
/// 未加引号经 `sh -c` 会被拆成两个参数，uci 直接报错退出（实机 rc=255），
/// 进而让整批 UCI 写入被判失败、`ifup` 被短路 —— 接口就再也起不来。
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 定位 wan 防火墙区的下标。
///
/// 原先硬编码 `firewall.@zone[0]`，但 `@zone[N]` 是**按配置文件出现顺序**取的，
/// 并非一定是 wan 区（实机 192.168.10.1 的 `@zone[0]` 就是 lan 区，
/// wan 区在 `@zone[1]`）。把蜂窝接口登记进 lan 区会让 fw4 把它并入 LAN 规则
/// （input/forward 全 ACCEPT），既拿不到 masq 又与 wan 区配置自相矛盾。
///
/// 优先匹配 `name='wan'`；少数配置没有 name，则以 network 列表含 `wan`/`wan6` 兜底。
fn wan_zone_index() -> Option<usize> {
    let (ok, out) = real("uci show firewall 2>/dev/null | grep -c '=zone$'");
    if !ok {
        return None;
    }
    let n: usize = out.trim().parse().unwrap_or(0);
    let mut fallback: Option<usize> = None;
    for i in 0..n {
        let (ok_name, name) = real(&format!("uci -q get firewall.@zone[{}].name", i));
        if ok_name && name.trim() == "wan" {
            return Some(i);
        }
        if fallback.is_none() {
            let (ok_nets, nets) = real(&format!("uci -q get firewall.@zone[{}].network", i));
            if ok_nets && nets.split_whitespace().any(|x| x == "wan" || x == "wan6") {
                fallback = Some(i);
            }
        }
    }
    fallback
}

/// 判断某个 uci 列表是否已包含指定项（用于让 add_list 幂等）。
fn uci_list_contains(key: &str, item: &str) -> bool {
    let (ok, out) = real(&format!("uci -q get {}", key));
    ok && out.split_whitespace().any(|x| x == item)
}


/// 探测数据通道网卡名：优先读驱动，其次按名称兜底。
pub fn detect_dev(cfg: &Config) -> Option<String> {
    if cfg.data_dev != "auto" && !cfg.data_dev.is_empty() {
        return Some(cfg.data_dev.clone());
    }
    let entries = fs::read_dir("/sys/class/net").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let driver = fs::read_link(entry.path().join("device/driver"))
            .map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string())
            .unwrap_or_default();
        if DATA_DRIVERS.iter().any(|d| driver.contains(d)) {
            return Some(name);
        }
    }
    // 兜底：按常见名字猜测
    for name in ["eth2", "eth3", "wwan0", "usb0"] {
        if fs::metadata(format!("/sys/class/net/{}", name)).is_ok() {
            return Some(name.to_string());
        }
    }
    None
}

/// UCI 写入辅助。
fn uci(script: &str) -> (bool, String) {
    real(&format!("uci -q {}", script))
}

fn uci_batch(lines: &[String]) -> Vec<(String, bool, String)> {
    lines.iter().map(|l| (l.clone(), uci(l).0, "".to_string())).collect()
}

/// 确保蜂窝接口存在并应用地址。
///
/// 返回（接口名, 数据网卡, 已执行的 UCI 命令列表）。
pub fn ensure_iface(cfg: &Config, ipv4: &str, dns: &[String]) -> Result<(String, String, Vec<String>), String> {
    let dev = detect_dev(cfg).ok_or_else(|| "未探测到数据通道网卡".to_string())?;
    let iface = &cfg.iface;
    let iface_v6 = &cfg.iface_v6;
    let route_name = format!("{}_def", iface);
    let mask = "255.255.255.255";

    let mut cmds: Vec<String> = Vec::new();

    // ---- IPv4 主接口（静态 /32）
    cmds.push(format!("set network.{}=interface", iface));
    cmds.push(format!("set network.{}.proto=static", iface));
    cmds.push(format!("set network.{}.device={}", iface, dev));
    cmds.push(format!("set network.{}.ipaddr={}", iface, ipv4));
    cmds.push(format!("set network.{}.netmask={}", iface, mask));
    cmds.push(format!("set network.{}.peerdns=0", iface));
    // 开机默认启用（显式 auto=1）：netifd 对缺省 auto 的接口**不会**开机自启，
    // 实机 ifstatus 即为 "autostart": false，导致重启后蜂窝接口要等守护轮询才起来。
    cmds.push(format!("set network.{}.auto=1", iface));
    if !dns.is_empty() {
        // 必须整体加引号：多值 dns 含空格，裸写会被 `sh -c` 拆成两个参数，
        // uci 直接 rc=255，整批写入判失败，ifup 被短路 —— 接口从此再也起不来。
        cmds.push(format!("set network.{}.dns={}", iface, sq(&dns.join(" "))));
    }
    cmds.push(format!("set network.{}.metric={}", iface, cfg.metric));
    cmds.push(format!("set network.{}.defaultroute=1", iface));

    // ---- IPv6（dhcpv6，附着在主接口上）
    if cfg.ipv6 && !iface_v6.is_empty() {
        cmds.push(format!("set network.{}=interface", iface_v6));
        cmds.push(format!("set network.{}.proto=dhcpv6", iface_v6));
        cmds.push(format!("set network.{}.device=@{}", iface_v6, iface));
        cmds.push(format!("set network.{}.reqaddress=try", iface_v6));
        cmds.push(format!("set network.{}.reqprefix=auto", iface_v6));
        cmds.push(format!("set network.{}.peerdns=1", iface_v6));
        // 同主接口：缺省 auto 时 netifd 不开机自启（LuCI 显示「开机时未启动」）。
        cmds.push(format!("set network.{}.auto=1", iface_v6));
    }

    // ---- 默认路由（onlink，网关不可达也要下发）
    cmds.push(format!("set network.{}=route", route_name));
    cmds.push(format!("set network.{}.interface={}", route_name, iface));
    cmds.push(format!("set network.{}.target=0.0.0.0/0", route_name));
    cmds.push(format!("set network.{}.onlink=1", route_name));
    cmds.push(format!("set network.{}.metric={}", route_name, cfg.metric));

    // ---- 防火墙：归入 wan 区
    //
    // 两个要点（都是实机踩出来的）：
    //  1. 区下标必须按名字解析，不能硬编码 @zone[0]（实机 @zone[0] 是 lan 区）；
    //  2. add_list 不会去重 —— 轮询每轮都会走到这里，不去重就会无限追加
    //     （实测 95 s 追加 3 组，累计到过 9 组重复）。故先查再写，保证幂等。
    match wan_zone_index() {
        Some(zi) => {
            for name in [iface.clone(), iface_v6.clone()] {
                if name.is_empty() {
                    continue;
                }
                let key = format!("firewall.@zone[{}].network", zi);
                if !uci_list_contains(&key, &name) {
                    cmds.push(format!("add_list {}={}", key, sq(&name)));
                }
            }
        }
        None => {
            eprintln!(
                "fm350d: 未定位到 wan 防火墙区，跳过接口登记（请手工把 {} 加入 wan 区）",
                iface
            );
        }
    }

    let results = uci_batch(&cmds);
    let failed: Vec<&String> = results.iter().filter(|r| !r.1).map(|r| &r.0).collect();
    if !failed.is_empty() {
        return Err(format!("UCI 写入失败: {:?}", failed));
    }

    let _ = real("uci commit network");
    let _ = real("uci commit firewall");

    // 应用：先 up 接口，再强制补齐设备路由。
    // ifup 的结果不能丢：接口起不来是本插件最不希望出现的静默故障
    // （蜂窝链路无 IP、无默认路由，而前端只看到一片空白）。
    let (up_ok, up_out) = real(&format!("ifup {}", iface));
    if !up_ok {
        eprintln!("fm350d: ifup {} 失败（rc!=0）: {}", iface, up_out);
    }
    if cfg.ipv6 && !iface_v6.is_empty() {
        let (up_ok6, up_out6) = real(&format!("ifup {}", iface_v6));
        if !up_ok6 {
            eprintln!("fm350d: ifup {} 失败（rc!=0）: {}", iface_v6, up_out6);
        }
    }
    let _ = real(&format!("ip route replace default dev {} metric {}", dev, cfg.metric));

    Ok((iface.clone(), dev, cmds))
}

/// 拆除由本插件创建的接口与路由。
pub fn teardown_iface(cfg: &Config) -> Result<Vec<String>, String> {
    let iface = &cfg.iface;
    let iface_v6 = &cfg.iface_v6;
    let route_name = format!("{}_def", iface);
    let mut cmds = Vec::new();

    let _ = real(&format!("ifdown {}", iface));
    if !iface_v6.is_empty() {
        let _ = real(&format!("ifdown {}", iface_v6));
    }
    cmds.push(format!("delete network.{}", route_name));
    cmds.push(format!("delete network.{}", iface));
    if !iface_v6.is_empty() {
        cmds.push(format!("delete network.{}", iface_v6));
    }
    // 与 ensure_iface 对称：按名解析区下标，且只删确实存在的项。
    // del_list 会移除该名字的全部重复项，正好用来清理历史污染。
    if let Some(zi) = wan_zone_index() {
        for name in [iface.clone(), iface_v6.clone()] {
            if name.is_empty() {
                continue;
            }
            let key = format!("firewall.@zone[{}].network", zi);
            if uci_list_contains(&key, &name) {
                cmds.push(format!("del_list {}={}", key, sq(&name)));
            }
        }
    }

    let results = uci_batch(&cmds);
    let _ = real("uci commit network");
    let _ = real("uci commit firewall");
    // 失败的项（如接口本来就不存在）忽略
    Ok(results.into_iter().map(|r| r.0).collect())
}

#[derive(Debug, Default, serde::Serialize)]
pub struct NetStatus {
    pub iface: String,
    pub iface_v6: String,
    pub dev: Option<String>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub routes: Vec<String>,
    pub up: bool,
}

/// 读取当前网络状态。
pub fn status(cfg: &Config) -> NetStatus {
    let dev = detect_dev(cfg);
    let mut st = NetStatus {
        iface: cfg.iface.clone(),
        iface_v6: cfg.iface_v6.clone(),
        dev: dev.clone(),
        ..Default::default()
    };
    if let Some(d) = &dev {
        let (_, out) = real(&format!("ip -o addr show dev {} 2>/dev/null", d));
        for line in out.lines() {
            // 形如: 2: eth2    inet 10.5.243.239/32 brd ...
            if let Some(p) = line.find("inet ") {
                let rest = &line[p + 5..];
                if let Some(addr) = rest.split_whitespace().next() {
                    let ip = addr.split('/').next().unwrap_or("").to_string();
                    if !ip.is_empty() {
                        st.ipv4.push(ip);
                    }
                }
            }
            if let Some(p) = line.find("inet6 ") {
                let rest = &line[p + 6..];
                if let Some(addr) = rest.split_whitespace().next() {
                    let ip = addr.split('/').next().unwrap_or("").to_string();
                    if !ip.is_empty() {
                        st.ipv6.push(ip);
                    }
                }
            }
        }
        let (_, routes) = real(&format!("ip route show dev {} 2>/dev/null", d));
        st.routes = routes.lines().map(|l| l.trim().to_string()).collect();
        st.up = !st.ipv4.is_empty() || !st.ipv6.is_empty();
    }
    st
}

/// 路由守护：netifd 不会为无网关接口下发设备路由，这里周期补齐。
///
/// 返回是否执行了补齐动作。
pub fn route_guard(cfg: &Config) -> bool {
    if !cfg.route_guard {
        return false;
    }
    let dev = match detect_dev(cfg) {
        Some(d) => d,
        None => return false,
    };
    // 只有当接口确实拿到地址时才补路由，避免空路由污染主表
    let (_, out) = real(&format!("ip -o -4 addr show dev {} 2>/dev/null", dev));
    if !out.contains("inet ") {
        return false;
    }
    let wanted = format!("default dev {} metric {}", dev, cfg.metric);
    let (_, routes) = real(&format!("ip route show dev {} 2>/dev/null", dev));
    if routes.lines().any(|l| l.trim().starts_with("default") && l.contains(&format!("metric {}", cfg.metric))) {
        return false;
    }
    let (ok, _) = real(&format!("ip route replace {}", wanted));
    ok
}

/// 保证插件创建的接口处于「开机自启」状态。
///
/// 为什么必须有它：`ensure_iface` 只在拨号后或地址有偏差时才跑，而守护的稳态
/// 分支在「地址已经匹配」时会整段跳过，于是 auto 选项一旦不是 1 就再也没机会
/// 补回来 —— 实机 fm350v6 即长期停在 auto=0（LuCI 显示「开机时未启动」），
/// 而手工 uci 明明能写进去。
///
/// 这里只做补齐：接口 section 已存在且 auto≠1 时才写入，成本 2~4 次 uci get。
/// 返回被修正的接口名列表，为空表示无需改动（调用方据此避免刷日志）。
pub fn ensure_autostart(cfg: &Config) -> Vec<String> {
    let mut names = vec![cfg.iface.clone()];
    if cfg.ipv6 && !cfg.iface_v6.is_empty() {
        names.push(cfg.iface_v6.clone());
    }

    let mut touched: Vec<String> = Vec::new();
    for name in names {
        if name.is_empty() {
            continue;
        }
        // 仅在接口已存在时补齐：section 缺失时 uci set 会直接报错
        let (ok_sec, sec) = real(&format!("uci -q get network.{}", name));
        if !ok_sec || sec.trim() != "interface" {
            continue;
        }
        let key = format!("network.{}.auto", name);
        let (ok_auto, val) = real(&format!("uci -q get {}", key));
        if ok_auto && val.trim() == "1" {
            continue;
        }
        if uci(&format!("set {}=1", key)).0 {
            touched.push(name);
        }
    }
    if !touched.is_empty() {
        let _ = real("uci commit network");
    }
    touched
}

/// 拔号成功后一次性把网络拉起。
pub fn apply_after_dial(cfg: &Config, ipv4: &str, dns: &[String]) -> Result<NetStatus, String> {
    if ipv4.is_empty() {
        return Err("缺少 IPv4 地址，无法配置接口".to_string());
    }
    ensure_iface(cfg, ipv4, dns)?;

    // `ifup` 是异步的：netifd 受理后，地址要过一会儿才落进内核。
    // 直接读一次 status() 常常读到空地址，于是把「刚拨上」误报成失败
    // （实机复现：日志打出「已重新应用网络配置 []」，而接口随后确实通了；
    //  拨号页也会瞬间显示未上线）；反过来，网卡名配错导致根本没起来时，
    // 又会被当成成功（net.up=false 却无任何错误）。
    // 因此这里有限轮询等地址落地：通常 <500 ms 即返回，最多等 3 s。
    let mut last = status(cfg);
    for _ in 0..12 {
        if last.ipv4.iter().any(|a| a == ipv4) {
            return Ok(last);
        }
        std::thread::sleep(Duration::from_millis(250));
        last = status(cfg);
    }
    Err(format!(
        "接口 {} 未在 3 s 内取得期望地址 {}（实际 {:?}，网卡 {}）",
        cfg.iface,
        ipv4,
        last.ipv4,
        last.dev.as_deref().unwrap_or("?")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_does_not_touch_system() {
        let (ok, script) = sh(RunMode::Dry, "uci set network.fm350=interface");
        assert!(ok);
        assert_eq!(script, "uci set network.fm350=interface");
    }
}
