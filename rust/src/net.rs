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
    if !dns.is_empty() {
        cmds.push(format!("set network.{}.dns={}", iface, dns.join(" ")));
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
    }

    // ---- 默认路由（onlink，网关不可达也要下发）
    cmds.push(format!("set network.{}=route", route_name));
    cmds.push(format!("set network.{}.interface={}", route_name, iface));
    cmds.push(format!("set network.{}.target=0.0.0.0/0", route_name));
    cmds.push(format!("set network.{}.onlink=1", route_name));
    cmds.push(format!("set network.{}.metric={}", route_name, cfg.metric));

    // ---- 防火墙：归入 wan 区
    for name in [iface.clone(), iface_v6.clone()] {
        if name.is_empty() {
            continue;
        }
        cmds.push(format!("add_list firewall.@zone[0].network={}", name));
    }

    let results = uci_batch(&cmds);
    let failed: Vec<&String> = results.iter().filter(|r| !r.1).map(|r| &r.0).collect();
    if !failed.is_empty() {
        return Err(format!("UCI 写入失败: {:?}", failed));
    }

    let _ = real("uci commit network");
    let _ = real("uci commit firewall");

    // 应用：先 up 接口，再强制补齐设备路由
    let _ = real(&format!("ifup {}", iface));
    if cfg.ipv6 && !iface_v6.is_empty() {
        let _ = real(&format!("ifup {}", iface_v6));
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
    for name in [iface.clone(), iface_v6.clone()] {
        if name.is_empty() {
            continue;
        }
        cmds.push(format!("del_list firewall.@zone[0].network={}", name));
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

/// 拔号成功后一次性把网络拉起。
pub fn apply_after_dial(cfg: &Config, ipv4: &str, dns: &[String]) -> Result<NetStatus, String> {
    if ipv4.is_empty() {
        return Err("缺少 IPv4 地址，无法配置接口".to_string());
    }
    ensure_iface(cfg, ipv4, dns)?;
    Ok(status(cfg))
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
