//! AT 端口枚举与识别。
//!
//! 与 [`crate::at::AtPort`] 的分工：本模块只做**发现**（哪些 tty 可能是
//! AT 口、哪个才是真能应答的那个），不做持有与收发。
//!
//! ## 逆向依据（FM350-GL / MT6880 双域）
//!
//! FM350 是「AP 域 Linux + MD 域 MOLY」的双 SoC 形态：AT 通道由 MD 侧的
//! USB 功能导出，主机侧看到的只是 option / cdc-acm 驱动的 tty。
//! 因此**不能**假设「ttyUSB0 一定是 AT 口」——同一模组会导出
//! DIAG / GNSS / AT / MODEM 等多个 2cb7 接口，只有真正应答 `AT` 的才是。
//!
//! 判定顺序（严格按此执行，不做猜测）：
//!   1. 名称前缀（`ttyUSB*` / `ttyACM*`）粗筛；
//!   2. sysfs 逐级向上读 `idVendor` / `idProduct` / `manufacturer` / `product`，
//!      VID `2cb7`（Fibocom）或描述符含 Fibocom / FM350 → 疑似本模组；
//!   3. 只有在「当前配置路径已消失」的前提下，才对疑似口发 `AT` 探测。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::config::Config;

/// 候选 tty 前缀：FM350 在 `+GTUSBMODE=40`（RNDIS+AT）下的 AT 口由
/// option 驱动导出为 `/dev/ttyUSB*`；部分固件/形态也可能是 CDC-ACM 的
/// `/dev/ttyACM*`。
const AT_CANDIDATE_PREFIXES: &[&str] = &["ttyUSB", "ttyACM"];

/// Fibocom 的 USB Vendor ID。
///
/// 注意与 MediaTek 的 `0e8d` 区分：`0e8d` 出现在 AP 域的 configfs  gadgets
/// 配置里，主机侧枚举到的 Fibocom 模组 VID 是 `2cb7`。
pub const FIBOCOM_VID: &str = "2cb7";

/// 设备名是否属于候选 AT 口（`ttyUSB0` / `ttyACM1` / `ttyUSB1.2` 之类）。
pub fn is_candidate_tty(name: &str) -> bool {
    AT_CANDIDATE_PREFIXES
        .iter()
        .any(|p| match name.strip_prefix(p) {
            Some(rest) => {
                !rest.is_empty()
                    && rest
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
            }
            None => false,
        })
}

/// 依据 USB 描述符判断是否为 Fibocom FM350 系列。
pub fn looks_like_fm350(vid: &str, vendor: &str, product: &str) -> bool {
    let v = vid.trim().to_lowercase();
    let vend = vendor.trim().to_lowercase();
    let prod = product.trim().to_lowercase();
    v == FIBOCOM_VID || vend.contains("fibocom") || prod.contains("fibocom") || prod.contains("fm350")
}

/// 单个候选端口的信息（供 `/api/ports` 与 CLI `ports` 直接序列化）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PortCandidate {
    /// 设备节点绝对路径，如 `/dev/ttyUSB1`。
    pub path: String,
    /// 设备名，如 `ttyUSB1`。
    pub name: String,
    /// 绑定的内核驱动（option / usbserial / cdc_acm ...）。
    pub driver: String,
    pub vid: String,
    pub pid: String,
    pub vendor: String,
    pub product: String,
    /// 是否为 Fibocom 设备（用于把 FM350 的端口排在前面）。
    pub likely_fm350: bool,
    /// 是否等于当前配置的 `at_port`。
    pub current: bool,
    /// 探测结果：`true` = 打不开（多半被独占），`false` = 可用。
    pub busy: Option<bool>,
    /// 探测失败原因或状态说明。
    pub note: String,
}

/// 从起点目录逐级向上查找 sysfs 属性。
///
/// `/sys/class/tty/ttyUSB1/device` 是指向 interface 目录的符号链接，
/// 必须先 `canonicalize` 解析，否则文本层面的 `..` 会退回 `/sys/class/tty`。
pub fn find_usb_attr(start: &std::path::Path, attr: &str) -> Option<String> {
    let base = std::fs::canonicalize(start).ok()?;
    let mut cur: Option<PathBuf> = Some(base);
    for _ in 0..6 {
        let dir = cur?;
        if let Ok(s) = std::fs::read_to_string(dir.join(attr)) {
            let t = s.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

/// 尝试独占打开一次以判断端口是否空闲。不发送任何数据，随即释放。
fn probe_busy(path: &str, baudrate: u32) -> (Option<bool>, String) {
    match serialport::new(path, baudrate)
        .timeout(Duration::from_millis(100))
        .open()
    {
        Ok(p) => {
            drop(p);
            (Some(false), "空闲，可独占打开".to_string())
        }
        Err(e) => (Some(true), format!("无法打开：{}", e)),
    }
}

/// 按「当前端口 → 疑似 FM350 → 名称」排序，便于前端把最可能的选项放最前。
fn sort_ports(v: &mut [PortCandidate]) {
    v.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then(b.likely_fm350.cmp(&a.likely_fm350))
            .then(a.name.cmp(&b.name))
    });
}

/// 枚举候选 AT 口。
///
/// `probe = true` 时会逐个尝试独占打开以判断占用情况。
/// 注意：被本进程持有的端口（配置项本身）探测会失败，属预期，前端据
/// `current` 字段区分展示。
pub fn list_ports(cfg: &Config, probe: bool) -> Vec<PortCandidate> {
    let mut out: Vec<PortCandidate> = Vec::new();

    if let Ok(rd) = std::fs::read_dir("/dev") {
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| is_candidate_tty(n))
            .collect();
        names.sort(); // readdir 顺序不稳定，排序保证同构环境行为可复现

        for name in names {
            let path = format!("/dev/{}", name);
            let sysobj = PathBuf::from(format!("/sys/class/tty/{}", name)).join("device");
            let driver = std::fs::read_link(sysobj.join("driver"))
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string()
                })
                .unwrap_or_default();
            let vid = find_usb_attr(&sysobj, "idVendor").unwrap_or_default();
            let pid = find_usb_attr(&sysobj, "idProduct").unwrap_or_default();
            let vendor = find_usb_attr(&sysobj, "manufacturer").unwrap_or_default();
            let product = find_usb_attr(&sysobj, "product").unwrap_or_default();
            let current = path == cfg.at_port;

            let (busy, note) = if probe {
                let (b, n) = probe_busy(&path, cfg.baudrate);
                let n = if current {
                    format!("{}（当前配置端口）", n)
                } else {
                    n
                };
                (b, n)
            } else if current {
                (None, "当前配置端口".to_string())
            } else {
                (None, String::new())
            };

            out.push(PortCandidate {
                likely_fm350: looks_like_fm350(&vid, &vendor, &product),
                path,
                name,
                driver,
                vid,
                pid,
                vendor,
                product,
                current,
                busy,
                note,
            });
        }
    }

    sort_ports(&mut out);
    out
}

/// 当前配置的 AT 口路径**已消失**时，寻找能应答 `AT` 的 Fibocom 串口。
///
/// 调用前置条件（重要）：调用方必须先确认 `cfg.at_port` 路径不存在。
/// 同一模组会导出多个 2cb7 口（DIAG / GNSS / AT），只有真正应答
/// `OK` / `ERROR` 的才是 AT 口；路径仍在时不切换，宁可等待模组重新
/// 枚举（MD 域重启或 `+GTUSBMODE` 切换都会触发 USB 重枚举，tty 编号
/// 会漂移，此时盲切有选错口的风险）。
pub fn identify_at_port(cfg: &Config) -> Option<String> {
    let ports = list_ports(cfg, false);
    for p in &ports {
        if !p.likely_fm350 || p.path == cfg.at_port {
            continue;
        }
        if probe_at_ok(&p.path, cfg.baudrate) {
            return Some(p.path.clone());
        }
    }
    None
}

/// 独占打开候选口，发一条 `AT` 并在限时内等待结果码。
///
/// `OK` / `ERROR` / `+CME ERROR` 任一都算「这是 AT 口」（口本身状态不佳
/// 不代表选错了设备）；超时无应答则判否。
fn probe_at_ok(path: &str, baudrate: u32) -> bool {
    #[cfg(unix)]
    let builder = serialport::new(path, baudrate)
        .timeout(Duration::from_millis(100))
        .exclusive(true);
    #[cfg(not(unix))]
    let builder = serialport::new(path, baudrate).timeout(Duration::from_millis(100));

    let mut port = match builder.open() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let _ = port.write_all(b"\rAT\r");
    let _ = port.flush();
    let deadline = Instant::now() + Duration::from_millis(800);
    let mut buf = String::new();
    let mut chunk = [0u8; 64];
    while Instant::now() < deadline {
        match port.read(&mut chunk) {
            Ok(0) => std::thread::sleep(Duration::from_millis(50)),
            Ok(n) => {
                buf.push_str(&String::from_utf8_lossy(&chunk[..n]));
                if buf.contains("OK")
                    || buf.contains("ERROR")
                    || buf.contains("+CME")
                    || buf.contains("+CMS")
                {
                    return true;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_prefix_filter() {
        for ok in ["ttyUSB0", "ttyUSB1", "ttyACM0", "ttyUSB1.2"] {
            assert!(is_candidate_tty(ok), "应识别为候选端口: {}", ok);
        }
        for no in ["ttyS0", "ttyAMA0", "ttyUSB", "ttyUSBa", "sda", "tty"] {
            assert!(!is_candidate_tty(no), "不应识别为候选端口: {}", no);
        }
    }

    #[test]
    fn fm350_descriptor_detection() {
        assert!(looks_like_fm350("2cb7", "", ""));
        assert!(looks_like_fm350("2CB7", "", ""));
        assert!(looks_like_fm350("", "Fibocom Wireless Inc.", ""));
        assert!(looks_like_fm350("", "", "FM350-GL"));
        // MediaTek AP 域的 VID 不算：主机侧枚举到的模组是 Fibocom VID
        assert!(!looks_like_fm350("0e8d", "MediaTek", ""));
        assert!(!looks_like_fm350("1a86", "QinHeng Electronics", "CH340"));
        assert!(!looks_like_fm350("", "", ""));
    }

    #[test]
    fn find_usb_attr_walks_up_and_reads_sysfs_value() {
        // 构造 <tmp>/a/b/c 目录树，把 idVendor 放在 a 层，
        // 从 c 出发应能向上两级读到，证明符号链接解析后的逐级查找正确。
        let root = std::env::temp_dir().join(format!("fm350-port-test-{}", std::process::id()));
        let a = root.join("a");
        let deeper = a.join("b").join("c");
        std::fs::create_dir_all(&deeper).unwrap();
        std::fs::write(a.join("idVendor"), "2cb7\n").unwrap();
        std::fs::write(a.join("product"), "FM350-GL\n").unwrap();

        assert_eq!(
            find_usb_attr(&deeper, "idVendor"),
            Some("2cb7".to_string())
        );
        assert_eq!(
            find_usb_attr(&deeper, "product"),
            Some("FM350-GL".to_string())
        );
        assert_eq!(find_usb_attr(&deeper, "notExist"), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn port_sort_puts_current_and_fm350_first() {
        fn c(name: &str, current: bool, fm: bool) -> PortCandidate {
            PortCandidate {
                path: format!("/dev/{}", name),
                name: name.to_string(),
                driver: String::new(),
                vid: String::new(),
                pid: String::new(),
                vendor: String::new(),
                product: String::new(),
                likely_fm350: fm,
                current,
                busy: None,
                note: String::new(),
            }
        }
        let mut v = vec![
            c("ttyUSB3", false, false),
            c("ttyUSB0", false, true),
            c("ttyUSB1", true, true),
            c("ttyUSB2", false, false),
        ];
        sort_ports(&mut v);
        assert_eq!(v[0].name, "ttyUSB1", "当前端口应排第一");
        assert_eq!(v[1].name, "ttyUSB0", "疑似 FM350 应排第二");
        assert_eq!(v[2].name, "ttyUSB2");
        assert_eq!(v[3].name, "ttyUSB3");
    }
}
