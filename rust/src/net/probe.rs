//! 设备探测、数据面健康度与公网连通性探测。
//!
//! 只做**观测**，不做任何写操作。三个观测维度对应三类不同性质的故障：
//!
//! | 维度 | 回答的问题 | 典型故障 |
//! |---|---|---|
//! | [`detect_dev`] | 数据网卡还在不在 | USB 重枚举、模组掉电、USB 模式切换 |
//! | [`data_plane_stalled`] | 端点有没有卡死 | 与其它 modem 插件争抢导致的 URB stall |
//! | [`check_connectivity4`] / [`check_connectivity6`] | 真能出公网吗 | 基带附着但核心网承载丢失 |

use super::shell::{real, sq};
use crate::at::find_usb_attr;
use crate::config::Config;
use std::fs;
use std::path::PathBuf;

/// 可能的 RNDIS / ECM / NCM / MBIM 数据通道驱动。
///
/// 注意这里**没有** `ccmni`：那是模组内部 AP 域的 CCCI 网卡，主机侧不存在
/// （详见 `modem::bind` 的逆向结论）。
pub const DATA_DRIVERS: &[&str] = &["rndis_host", "cdc_ether", "cdc_ncm", "qmi_wwan", "mbim"];

/// Fibocom 的 USB VID（主机侧枚举到的模组 VID）。
const FIBOCOM_VID: &str = "2cb7";

/// 读取网卡对应 USB 设备的 idVendor（sysfs 逐级向上）。
fn dev_usb_vid(name: &str) -> Option<String> {
    find_usb_attr(&PathBuf::from(format!("/sys/class/net/{}/device", name)), "idVendor")
}

/// 探测数据通道网卡名：优先读驱动，其次按名称兜底。
///
/// `data_dev=auto` 下可能同时命中多张 cdc_* 网卡（同机挂了别的 CDC 设备，
/// 或同模组枚举出多个网络功能）：先按名字排序保证选择可复现，再优先挑
/// USB VID 为 Fibocom 的那张。
pub fn detect_dev(cfg: &Config) -> Option<String> {
    if cfg.data_dev != "auto" && !cfg.data_dev.is_empty() {
        return Some(cfg.data_dev.clone());
    }
    let entries = fs::read_dir("/sys/class/net").ok()?;
    let mut candidates: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let driver = fs::read_link(entry.path().join("device/driver"))
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .unwrap_or_default();
        if DATA_DRIVERS.iter().any(|d| driver.contains(d)) {
            candidates.push(name);
        }
    }
    if !candidates.is_empty() {
        // readdir 顺序不稳定，排序保证同构环境行为可复现
        candidates.sort();
        if let Some(fib) = candidates
            .iter()
            .find(|n| dev_usb_vid(n).as_deref() == Some(FIBOCOM_VID))
        {
            return Some(fib.clone());
        }
        return Some(candidates.remove(0));
    }
    // 兜底：按常见名字猜测
    for name in ["eth2", "eth3", "wwan0", "usb0"] {
        if fs::metadata(format!("/sys/class/net/{}", name)).is_ok() {
            return Some(name.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------- 防火墙区

/// 定位 wan 防火墙区的下标。
///
/// 原先硬编码 `firewall.@zone[0]`，但 `@zone[N]` 是**按配置文件出现顺序**取的，
/// 并非一定是 wan 区（实机 192.168.10.1 的 `@zone[0]` 就是 lan 区，wan 区在
/// `@zone[1]`）。把蜂窝接口登记进 lan 区会让 fw4 把它并入 LAN 规则
/// （input/forward 全 ACCEPT），既拿不到 masq 又与 wan 区配置自相矛盾。
///
/// 优先匹配 `name='wan'`；少数配置没有 name，则以 network 列表含 `wan`/`wan6` 兜底。
pub fn wan_zone_index() -> Option<usize> {
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
pub fn uci_list_contains(key: &str, item: &str) -> bool {
    let (ok, out) = real(&format!("uci -q get {}", key));
    ok && out.split_whitespace().any(|x| x == item)
}

/// 找出同样绑定在该数据网卡上的**非本插件** uci 接口。
///
/// 典型场景：设备上另装了其它 modem 管理插件（如 ModemManager），它们也会
/// 在同一个网卡上建接口并周期性拨号、改写接口。两个守护同时操作一块模组会
/// 互相打断（接口反复 down/up、AT 口争用），最终把数据端点打到 stall ——
/// 表现为「配置全对却就是上不了网」。
pub fn foreign_ifaces_on_dev(cfg: &Config, dev: &str) -> Vec<String> {
    let script = format!(
        "for s in $(uci -q show network 2>/dev/null | sed -n 's/^network\\.\\([^.=]*\\)=interface$/\\1/p'); do \
         d=$(uci -q get network.$s.device 2>/dev/null); \
         [ -z \"$d\" ] && d=$(uci -q get network.$s.ifname 2>/dev/null); \
         [ \"$d\" = {} ] && echo \"$s\"; \
         done",
        sq(dev)
    );
    let (_, out) = real(&script);
    out.lines()
        .map(|l| l.trim().to_string())
        .filter(|n| !n.is_empty() && n != &cfg.iface && n != &cfg.iface_v6)
        .collect()
}

// ---------------------------------------------------------------- 数据面健康

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DataHealth {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
}

/// 读取数据网卡收发统计（走 sysfs，成本极低，可每轮调用）。
pub fn data_health(dev: &str) -> Option<DataHealth> {
    let rd = |n: &str| -> Option<u64> {
        fs::read_to_string(format!("/sys/class/net/{}/statistics/{}", dev, n))
            .ok()
            .and_then(|s| s.trim().parse().ok())
    };
    Some(DataHealth {
        rx_packets: rd("rx_packets")?,
        tx_packets: rd("tx_packets")?,
        tx_errors: rd("tx_errors")?,
    })
}

/// 判定数据面是否卡死。
///
/// 判据：**tx_errors 在涨，而 tx_packets 不动**。正常链路上 tx_errors 恒为 0，
/// 一旦 USB 数据端点被打到 stall，内核每次提交 URB 都会记一次错误、包却一个
/// 也发不出去（实机：tx_packets 停在 2，tx_errors 从百级一路涨到千级，
/// 同时刷 `NETDEV WATCHDOG: transmit queue timed out`）。
///
/// 不用「RX 不增长」作判据：空闲链路上本来就没有下行流量。
pub fn data_plane_stalled(prev: &DataHealth, cur: &DataHealth) -> bool {
    cur.tx_errors > prev.tx_errors && cur.tx_packets <= prev.tx_packets
}

// ---------------------------------------------------------------- 公网连通性

/// IPv4 连通性探测目标（任一可达即判定该栈连通）。
///
/// 选型依据：双公共 DNS 的 ICMP 在国内蜂窝/宽带链路上基本恒可达，两者同时
/// 失联基本可断定是本机链路问题而非目标故障；仅当全部目标在超时内无应答
/// 才判定该栈断网，避免单目标抖动误触发恢复动作。
pub const NET_CHECK_TARGETS_V4: &[&str] = &["223.5.5.5", "119.29.29.29"];

/// IPv6 连通性探测目标（与 v4 同构的双目标设计）。
pub const NET_CHECK_TARGETS_V6: &[&str] = &["2400:3200::1", "2402:4e00::"];

/// 探测单个目标：ICMP echo，绑定数据网卡发出。
///
/// 为什么绑定 `-I <dev>`：多 WAN 场景下 IPv4 默认路由可能指向别的接口
/// （metric 更优），不绑定网卡会把「别的 WAN 通」误判成蜂窝链路通。
/// BusyBox ping 的 `-I` 接受网卡名；IPv6 目标优先走 `ping`，老固件的
/// BusyBox 未合并 ping6 应用时回退 `ping6`。
fn ping_iface(dev: &str, target: &str, timeout: u32) -> bool {
    let v4 = real(&format!(
        "ping -c 1 -W {} -I {} {} >/dev/null 2>&1",
        timeout, dev, target
    ));
    if v4.0 {
        return true;
    }
    if target.contains(':') {
        return real(&format!(
            "ping6 -c 1 -W {} -I {} {} >/dev/null 2>&1",
            timeout, dev, target
        ))
        .0;
    }
    false
}

/// IPv4 公网连通性探测：全部目标不可达才返回 false。
///
/// 判据是「真正能出去」，而不是「模组侧有地址」——PDP 激活、CGPADDR 有值
/// 甚至 ARP 网关可解析都不代表运营商侧会话健康（基带附着但核心网承载丢失
/// 时模组仍会代理 ARP 应答）。
pub fn check_connectivity4(dev: &str) -> bool {
    NET_CHECK_TARGETS_V4.iter().any(|t| ping_iface(dev, t, 3))
}

/// IPv6 公网连通性探测：与 v4 完全独立，任一目标可达即判定 v6 连通。
pub fn check_connectivity6(dev: &str) -> bool {
    NET_CHECK_TARGETS_V6.iter().any(|t| ping_iface(dev, t, 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_judges_tx_errors_not_idle_rx() {
        let a = DataHealth {
            rx_packets: 0,
            tx_packets: 2,
            tx_errors: 100,
        };
        // 端点 stall：包发不出去（tx_packets 不动），错误计数却在涨
        let stalled = DataHealth {
            rx_packets: 0,
            tx_packets: 2,
            tx_errors: 145,
        };
        assert!(data_plane_stalled(&a, &stalled));

        // 正常链路：tx_packets 在涨（即便同时有零星错误）
        let healthy = DataHealth {
            rx_packets: 0,
            tx_packets: 30,
            tx_errors: 101,
        };
        assert!(!data_plane_stalled(&a, &healthy));

        // 空闲链路：计数完全不变，不算 stall（没流量是正常的）
        assert!(!data_plane_stalled(&a, &a));
    }

    #[test]
    fn data_drivers_exclude_ap_side_ccmni() {
        // ccmni 属于模组内部 AP 域，主机侧永远不该把它选作数据网卡
        assert!(!DATA_DRIVERS.iter().any(|d| d.contains("ccmni")));
        assert!(DATA_DRIVERS.contains(&"rndis_host"));
        assert!(DATA_DRIVERS.contains(&"cdc_ether"));
    }
}
