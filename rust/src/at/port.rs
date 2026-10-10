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

/// 在 `/dev/serial/by-id` 下查找解析到指定设备的稳定符号链接。
///
/// 返回形如 `/dev/serial/by-id/usb-Fibocom_FM350-GL_...-if01` 的路径。
/// 判定方式：对每个链接 `readlink` 后与目标设备做 canonicalize 比对，
/// 悬空链接与不相关设备一律跳过。目录不存在（无 udev / 精简固件）返回 `None`。
pub fn by_id_link(path: &str) -> Option<String> {
    let want = std::fs::canonicalize(path).ok()?;
    let rd = std::fs::read_dir("/dev/serial/by-id").ok()?;
    for e in rd.flatten() {
        let Ok(t) = std::fs::read_link(e.path()) else {
            continue;
        };
        let full = if t.is_absolute() {
            t
        } else {
            match e.path().parent() {
                Some(p) => p.join(t),
                None => continue,
            }
        };
        if std::fs::canonicalize(&full).is_ok_and(|c| c == want) {
            return Some(e.path().to_string_lossy().to_string());
        }
    }
    None
}

/// 从内核 sysfs 推导「物理位置稳定标识」——**不依赖 udev**。
///
/// 为什么需要它：`/dev/serial/by-id` 是 udev（或 eudev）生成的，精简固件
/// （ImmortalWrt / OpenWrt 默认）**不装 udev**，目录根本不存在，于是
/// [`by_id_link`] 恒返回 `None`、稳定路径功能在这些设备上完全失效 ——
/// 实机验证已确认（7 个 ttyUSB，`by_id` 全为空串）。
///
/// 内核自身提供的等价物是 **USB 物理拓扑路径**（即 udev `by-path` 的
/// 命名依据），由 `canonicalize` 后的 sysfs 路径推导：
///
/// ```text
/// /sys/devices/platform/soc/11200000.usb/usb2/2-1/2-1:1.6/ttyUSB3
///                                      ^^^  ^^^^^^^  ^^^^^^
///                                      bus  deviface ttyname
/// ```
///
/// 返回 `usb-11200000.usb-2-1:1.6` 形式的标识：只要模组插在同一物理口，
/// 无论 `ttyUSB*` 编号如何漂移（重枚举 / 其他 USB 串口设备插拔导致编号
/// 前移后移），该标识都不变；不同物理口则不同。
///
/// 与 by-id 的分工：by-id 优先（含序列号，跨物理口也稳定），本函数是
/// **无 udev 设备的兜底**，保证稳定路径功能不是「有 udev 才有」。
pub fn sysfs_stable_id(path: &str) -> Option<String> {
    let name = tty_name(path)?;
    // sysfs 层级（实机确认）：
    //   /sys/class/tty/ttyUSB3/device -> ../../../ttyUSB3
    //   canonicalize 后 = /sys/devices/.../usb2/2-1/2-1:1.6/ttyUSB3
    //                                             ^^^  ^^^^^^^  ^^^^^^
    //                                             dev  iface    tty 节点
    // 注意：`device` 链接指向的是 **tty 节点自身**（不是接口目录），
    // 接口目录是它的父级，USB 设备目录是祖父级，总线目录是曾祖父级。
    let node = std::fs::canonicalize(
        std::path::Path::new(&format!("/sys/class/tty/{}", name)).join("device"),
    )
    .or_else(|_| std::fs::canonicalize(format!("/sys/class/tty/{}", name)))
    .ok()?;

    // 收集路径组件，从后向前找「接口目录」（见 `stable_id_from_components`）
    let comps: Vec<String> = node
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    stable_id_from_components(&comps)
}

/// 从 sysfs 路径组件里提取 USB 拓扑稳定标识（纯函数，便于单测）。
///
/// 输入形如（实机 ImmortalWrt / kernel 6.18，MT7981 + FM350-GL）：
/// ```text
/// ["", "sys", "devices", "platform", "soc", "11200000.usb",
///  "usb2", "2-1", "2-1:1.6", "ttyUSB3"]
/// ```
/// 从后向前找**接口目录** `2-1:1.6`（含 `.`，尾部纯数字接口号，
/// 头部为数字 / `-` / `:` 组成的 USB 拓扑），其祖父级 `usb2` 为总线、
/// 父级 `2-1` 为设备。返回 `usb-usb2-2-1:1.6`。
///
/// 关键点：`11200000.usb` 同样含 `.`，但尾部 `usb` 非数字，会被排除；
/// 若头部规则漏掉 `:`，`2-1:1.6` 会被跳过并一路误配到 `11200000.usb`
/// —— 实测踩过此坑，故单测固定覆盖。
fn stable_id_from_components(comps: &[String]) -> Option<String> {
    let mut iface_idx = None;
    for i in (0..comps.len()).rev() {
        let c = &comps[i];
        if let Some((head, tail)) = c.rsplit_once('.') {
            if !head.is_empty()
                && !tail.is_empty()
                && tail.chars().all(|x| x.is_ascii_digit())
                && head
                    .chars()
                    .all(|x| x.is_ascii_digit() || x == '-' || x == ':')
            {
                iface_idx = Some(i);
                break;
            }
        }
        // 最多向上找 3 层，避免误伤路径前段的无关目录
        if comps.len() - i > 3 {
            break;
        }
    }
    let iface_idx = iface_idx?;

    // 接口目录的父级 = USB 设备目录，再父级 = 总线目录（usbN）
    let usb_dev = comps.get(iface_idx.checked_sub(1)?)?.clone();
    let bus_name = comps.get(iface_idx.checked_sub(2)?)?.clone();

    // 只认 USB 拓扑（总线目录须形如 usbN），platform / pci 设备不适用
    if !bus_name.starts_with("usb") || usb_dev.is_empty() {
        return None;
    }
    Some(format!("usb-{}-{}", bus_name, comps[iface_idx]))
}

/// `/dev/ttyUSB3` -> `ttyUSB3`；非 `/dev/` 前缀返回 `None`。
fn tty_name(path: &str) -> Option<&str> {
    path.strip_prefix("/dev/").filter(|s| !s.is_empty())
}

/// 是否安装了 udev（决定 `/dev/serial/by-id` 是否可能可用）。
///
/// 精简固件（OpenWrt / ImmortalWrt 默认）为 procd + mdev/hotplug，
/// 不提供 udev，`by-id` 目录永远不存在。用它给前端输出可读提示，
/// 而不是让用户面对一个「★ 稳定路径」却永远不出现的下拉选项。
pub fn has_udev() -> bool {
    std::path::Path::new("/dev/serial/by-id").is_dir()
        || which_udevadm().is_some()
}

/// 在常见路径下查找 udevadm。找不到返回 `None`。
fn which_udevadm() -> Option<std::path::PathBuf> {
    for p in ["/usr/sbin/udevadm", "/sbin/udevadm", "/usr/bin/udevadm"] {
        let pb = std::path::PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    None
}

/// 优先返回稳定路径，找不到时原样返回设备节点。
///
/// 优先级：
///   1. `/dev/serial/by-id/...`（udev 生成，含 USB 序列号，最稳）；
///   2. 由 sysfs 拓扑推导的稳定标识（无 udev 也能用，见 [`sysfs_stable_id`]）。
///
/// 自动改选与配置写入都应经过它：写 `/dev/ttyUSB1` 会在下次重枚举后
/// 指向另一个接口（编号漂移），稳定路径则始终指向同一物理接口。
///
/// 注意：返回值可能是**非 `/dev/` 的拓扑标识**（形如 `usb-usb2-2-1:1.6`），
/// 后端 `resolve_stable(path)` 会把它还原成当前实际的 tty 节点。
pub fn stable_path(path: &str) -> String {
    if let Some(b) = by_id_link(path) {
        return b;
    }
    if let Some(s) = sysfs_stable_id(path) {
        return s;
    }
    path.to_string()
}

/// 把 [`stable_path`] 产生的稳定标识还原为**当前**实际可用的设备节点。
///
/// 三条输入形态：
///   - 正常设备节点（`/dev/ttyUSB3`）：原样返回；
///   - by-id 符号链接：只要链接存在就能用（udev 已维护），原样返回；
///   - 拓扑标识（`usb-usb2-2-1:1.6`）：扫当前候选口，找出 sysfs 拓扑
///     匹配的那个，返回其**当前** `/dev/ttyUSB*` 节点 —— 这正是编号
///     漂移后仍需正确打开同一物理口的关键。
///
/// 找不到匹配（模组真不在）时返回原字符串，交由后续 open 报明确错误。
///
/// 注意：内部调用 [`scan_ports`] 而非 [`list_ports`] —— 后者为了算
/// `current` 会反过来调用本函数，直接用它会形成**无限递归导致栈溢出**
/// （实机踩过：`thread has overflowed its stack`）。
pub fn resolve_stable(_cfg: &Config, stable: &str) -> String {
    if stable.starts_with('/') {
        return stable.to_string();
    }
    if !stable.starts_with("usb-") {
        return stable.to_string();
    }
    for p in scan_ports() {
        if sysfs_stable_id(&p.path).as_deref() == Some(stable) {
            return p.path;
        }
    }
    stable.to_string()
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
    /// 稳定路径（`/dev/serial/by-id/...`，重启 / 重枚举不变）。
    ///
    /// 与 `path` 同时存在时前端与自动改选应优先使用它：`ttyUSB*` 编号会随
    /// USB 枚举顺序漂移，by-id 链接以 USB 序列号 / 接口路径命名，是 udev
    /// 官方提供的稳定别名。无 udev 的精简固件上为空串。
    pub by_id: String,
    /// 拓扑稳定标识（形如 `usb-usb2-2-1:1.6`），**不依赖 udev**。
    ///
    /// 无 udev 时 `by_id` 恒为空串，稳定路径功能会失效；本字段由 sysfs
    /// USB 拓扑推导，只要模组插在同一物理口就恒定，是前者的兜底。
    /// 既非标准设备节点也不是 by-id 链接，后端 `resolve_stable()` 负责
    /// 还原为当前实际 tty 节点。
    pub stable: String,
    /// 设备名，如 `ttyUSB1`。
    pub name: String,
    /// 绑定的内核驱动（option / usbserial / cdc_acm ...）。
    pub driver: String,
    /// USB 接口功能描述符（`interface` 文件），如 `USB COM Port` / `ADB Interface`。
    ///
    /// 用于向用户解释「这个口是干什么的」。注意 FM350 的多个 AT/DIAG 口
    /// **描述符可能完全相同**（实机 7 个口全是 `USB COM Port`），因此
    /// 不能据此判定哪个是 AT 口 —— 它只作展示，判定仍靠实际探测。
    pub interface: String,
    pub vid: String,
    pub pid: String,
    pub vendor: String,
    pub product: String,
    /// 是否为 Fibocom 设备（用于把 FM350 的端口排在前面）。
    pub likely_fm350: bool,
    /// 是否等于当前配置的 `at_port`（含稳定标识解析后的等价比对）。
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
///
/// 兼容性回退：个别内核/裁剪固件上该 `device` 链接可能缺失或悬空
/// （`canonicalize` 失败）。此时从 `/sys/class/tty/<name>` 自身解析——
/// class 目录本身就是指向设备目录的符号链接，同样能向上走到 USB
/// 描述符层（idVendor / manufacturer / product 在设备层，接口层只有
/// idVendor / idProduct，向上走两级都能取到）。
pub fn find_usb_attr(start: &std::path::Path, attr: &str) -> Option<String> {
    let base = std::fs::canonicalize(start)
        .ok()
        .or_else(|| start.parent().and_then(|p| std::fs::canonicalize(p).ok()))?;
    let mut cur: Option<PathBuf> = Some(base);
    // 深度放宽到 8：平台设备树（platform/soc/.../usb1/1-1/1-1:1.0/ttyUSB0）
    // 的嵌套可达 6 层以上，6 层在某些 SoC 布局下会差一级取不到描述符。
    for _ in 0..8 {
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

/// 把多个来源的名称合并去重，仅保留候选 tty 前缀（纯函数，便于单测）。
fn merge_candidate_names(sources: impl IntoIterator<Item = Vec<String>>) -> Vec<String> {
    let mut set = std::collections::BTreeSet::new();
    for names in sources {
        for n in names {
            if is_candidate_tty(&n) {
                set.insert(n);
            }
        }
    }
    set.into_iter().collect()
}

/// 收集系统里全部候选 tty 名称，三个来源合并（去重、字典序）：
///
/// 1. `/sys/class/tty`：**内核注册表，权威来源**。只要驱动注册了 tty
///    类设备就一定在此，与 `/dev` 节点是否已创建无关（devtmpfs 在
///    热插拔瞬间可能滞后，某些精简固件甚至不建节点）；
/// 2. `/dev`：设备节点；
/// 3. `/dev/serial/by-id` / `/dev/serial/by-path`：udev 符号链接
///    （桌面发行版常见），解析目标文件名后按同样规则过滤。
///
/// 任一来源异常（目录不存在 / 不可读 / 链接悬空）都不影响其它来源，
/// 保证「系统已有的串口」只要被内核识别就一定能枚举到。
fn collect_candidate_names() -> Vec<String> {
    let mut sys = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/tty") {
        for e in rd.flatten() {
            sys.push(e.file_name().to_string_lossy().to_string());
        }
    }

    let mut dev = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/dev") {
        for e in rd.flatten() {
            dev.push(e.file_name().to_string_lossy().to_string());
        }
    }

    let mut link = Vec::new();
    for dir in ["/dev/serial/by-id", "/dev/serial/by-path"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if let Ok(t) = std::fs::read_link(e.path()) {
                    if let Some(f) = t.file_name() {
                        link.push(f.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    merge_candidate_names([sys, dev, link])
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
///
/// 与 [`scan_ports`] 的分工：本函数是**对外入口**，额外完成「哪个口等于
/// 当前配置」的判定（需要 `resolve_stable` 解析稳定标识）。`scan_ports`
/// 是纯扫描，不依赖配置，供 `resolve_stable` 内部使用以**避免递归**。
pub fn list_ports(cfg: &Config, probe: bool) -> Vec<PortCandidate> {
    let mut out = scan_ports();
    // 配置里写的是稳定标识（拓扑 / by-id）时，先解析成当前真实节点，
    // 再与各候选口比对 —— 否则 `current` 恒为 false，前端看不到「当前配置」。
    let cfg_resolved = resolve_stable(cfg, &cfg.at_port);

    for p in out.iter_mut() {
        p.current = p.path == cfg.at_port || p.path == cfg_resolved;

        if !probe {
            // 不探测：仅标注「当前配置端口」，其余留空。
            p.note = if p.current {
                "当前配置端口".to_string()
            } else {
                String::new()
            };
            continue;
        }

        // 节点不存在时不探测（open 必然失败，报「被占用」反而误导），如实
        // 标注「等待重枚举」—— 正对应「sysfs 已注册但 devtmpfs 节点尚未创建」
        // 的热插拔瞬间，前端可据此区分。
        if !std::fs::metadata(&p.path).is_ok() {
            p.note = "设备节点缺失（等待模组重新枚举）".to_string();
            continue;
        }

        let (busy, mut note) = probe_busy(&p.path, cfg.baudrate);
        if p.current {
            note = format!("{}（当前配置端口）", note);
        }
        p.busy = busy;
        p.note = note;
    }

    sort_ports(&mut out);
    out
}

/// 纯扫描：枚举候选口并填好静态属性（驱动 / 描述符 / 稳定标识）。
///
/// **不做** `current` 判定（那需要解析配置，会反向调用 `resolve_stable`），
/// 因此可安全地被 `resolve_stable` 调用而不产生递归。
fn scan_ports() -> Vec<PortCandidate> {
    let mut out: Vec<PortCandidate> = Vec::new();

    // 从「内核注册表 + 设备节点 + udev 链接」三个来源合并出候选名：
    // 只要设备已被内核识别，就一定能被枚举到（不再只依赖 /dev readdir）。
    for name in collect_candidate_names() {
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
        // interface 目录内的 `interface` 文件即接口功能描述符。
        // sysobj 可能不可 canonicalize，用向上查找兜底。
        let interface = find_usb_attr(&sysobj, "interface")
            .or_else(|| {
                std::fs::canonicalize(&sysobj)
                    .ok()
                    .and_then(|d| std::fs::read_to_string(d.join("interface")).ok())
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_default();
        // by-id 稳定别名：仅设备节点真实存在时可解析（悬空无意义）
        let by_id = if std::fs::metadata(&path).is_ok() {
            by_id_link(&path).unwrap_or_default()
        } else {
            String::new()
        };
        // 拓扑稳定标识：不依赖 udev，无 by-id 时的兜底稳定路径
        let stable = sysfs_stable_id(&path).unwrap_or_default();

        // 只填静态属性：`current` / `busy` / `note` 依赖配置与探测，由
        // [`list_ports`] 补充（本函数要能被 `resolve_stable` 复用，不得递归）。
        out.push(PortCandidate {
            likely_fm350: looks_like_fm350(&vid, &vendor, &product),
            by_id,
            stable,
            path,
            name,
            driver,
            interface,
            vid,
            pid,
            vendor,
            product,
            current: false,
            busy: None,
            note: String::new(),
        });
    }

    out
}

/// 系统里是否存在**任一**疑似 FM350 的候选口（不触碰 AT 数据）。
///
/// 供 [`crate::api`] 的模组缺席预检使用：只做 sysfs 枚举，亚毫秒。
/// 用 [`scan_ports`] 而非 [`list_ports`] —— 前者无需配置、不解析稳定标识，
/// 少一次全量扫描（预检在忙等路径上被反复调用，省的是实打实的时间）。
pub fn any_fm350_present(_cfg: &Config) -> bool {
    scan_ports().iter().any(|p| p.likely_fm350)
}

/// 当前配置的 AT 口路径**已消失**时，寻找能应答 `AT` 的 Fibocom 串口。
///
/// 调用前置条件（重要）：调用方必须先确认 `cfg.at_port` 路径不存在。
/// 同一模组会导出多个 2cb7 口（DIAG / GNSS / AT），只有真正应答
/// `OK` / `ERROR` 的才是 AT 口；路径仍在时不切换，宁可等待模组重新
/// 枚举（MD 域重启或 `+GTUSBMODE` 切换都会触发 USB 重枚举，tty 编号
/// 会漂移，此时盲切有选错口的风险）。
///
/// 返回值为稳定路径（by-id 优先，无 udev 时用拓扑标识）：USB 重枚举后
/// `ttyUSB*` 编号会漂移，而稳定标识指向同一物理接口，一次改选就能长期
/// 免疫漂移 —— 这也是本函数存在的意义，否则每次漂移都要重新探测。
pub fn identify_at_port(cfg: &Config) -> Option<String> {
    let ports = list_ports(cfg, false);
    let want = resolve_stable(cfg, &cfg.at_port);
    for p in &ports {
        if !p.likely_fm350 || p.path == want {
            continue;
        }
        if probe_at_ok(&p.path, cfg.baudrate) {
            return Some(stable_path(&p.path));
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

    /// `/sys/class/tty/<name>/device` 悬空（canonicalize 失败）时，
    /// 必须回退到 class 目录自身并仍能读到 USB 描述符。
    #[cfg(unix)]
    #[test]
    fn find_usb_attr_falls_back_when_device_link_is_dangling() {
        let root = std::env::temp_dir().join(format!("fm350-attr-fb-{}", std::process::id()));
        let class = root.join("ttyUSB0");
        std::fs::create_dir_all(&class).unwrap();
        std::fs::write(class.join("idVendor"), "2cb7\n").unwrap();
        std::fs::write(class.join("product"), "FM350-GL\n").unwrap();
        // 悬空符号链接：canonicalize(device) 必然失败，走 class 目录回退
        std::os::unix::fs::symlink("/nonexistent-fm350-target", class.join("device")).unwrap();

        let start = class.join("device");
        assert_eq!(find_usb_attr(&start, "idVendor"), Some("2cb7".to_string()));
        assert_eq!(
            find_usb_attr(&start, "product"),
            Some("FM350-GL".to_string())
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 多来源合并：去重、过滤非候选名（ttyS0 / sda）、字典序输出。
    #[test]
    fn merge_candidate_names_dedupes_and_filters() {
        let merged = merge_candidate_names(vec![
            vec![
                "ttyUSB0".into(),
                "ttyUSB1".into(),
                "ttyS0".into(),
                "sda".into(),
                "tty".into(),
            ],
            vec!["ttyUSB1".into(), "ttyACM3".into(), "ttyUSBa".into()],
        ]);
        assert_eq!(merged, vec!["ttyACM3", "ttyUSB0", "ttyUSB1"]);
    }

    /// 三个来源全部为空时返回空，不 panic。
    #[test]
    fn merge_candidate_names_handles_empty_sources() {
        assert!(merge_candidate_names(Vec::<Vec<String>>::new()).is_empty());
        assert!(merge_candidate_names(vec![Vec::<String>::new()]).is_empty());
    }

    /// by-id 稳定路径：目录不存在（无 udev）时返回 None、stable_path 原样回退。
    #[test]
    fn stable_path_falls_back_to_node() {
        // 沙箱/固件上多半没有 /dev/serial/by-id，即便有也不会解析到这个假路径
        let p = "/dev/fm350-no-such-port-xyz";
        assert_eq!(by_id_link(p), None);
        assert_eq!(stable_path(p), p);
    }

    /// 拓扑稳定标识：非 USB 设备（不存在的假 tty）必须返回 None，
    /// 不得把任意路径都当作 USB 拓扑而产生幻觉标识。
    #[test]
    fn sysfs_stable_id_none_for_non_usb() {
        assert_eq!(sysfs_stable_id("/dev/fm350-no-such-port-xyz"), None);
        assert_eq!(sysfs_stable_id("/not/dev/ttyUSB0"), None);
        assert_eq!(sysfs_stable_id("ttyUSB0"), None);
    }

    /// **实机路径回归**：ImmortalWrt / kernel 6.18 + FM350-GL 的真实 sysfs
    /// 组件序列，必须解析出 `usb-usb2-2-1:1.6`。
    ///
    /// 这条单测锁死两个曾导致线上失效的细节：
    ///   1. `device` 链接 canonicalize 后指向的是 **tty 节点自身**，
    ///      接口目录在它上一级；
    ///   2. 接口目录头部含 `:`（`2-1:1.6`），若只放行数字与 `-`，
    ///      会跳过后误配到 `11200000.usb`。
    #[test]
    fn stable_id_from_real_device_path() {
        let comps: Vec<String> = [
            "",
            "sys",
            "devices",
            "platform",
            "soc",
            "11200000.usb",
            "usb2",
            "2-1",
            "2-1:1.6",
            "ttyUSB3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            stable_id_from_components(&comps),
            Some("usb-usb2-2-1:1.6".to_string())
        );
    }

    /// 同一模组的其他接口必须解析出**不同**标识（否则稳定路径无区分度）。
    #[test]
    fn stable_id_differs_per_interface() {
        let mk = |iface: &str| -> Vec<String> {
            ["", "sys", "devices", "platform", "soc", "11200000.usb", "usb2", "2-1",
             iface, "ttyUSB0"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        };
        let a = stable_id_from_components(&mk("2-1:1.2"));
        let b = stable_id_from_components(&mk("2-1:1.6"));
        assert_eq!(a, Some("usb-usb2-2-1:1.2".to_string()));
        assert_eq!(b, Some("usb-usb2-2-1:1.6".to_string()));
        assert_ne!(a, b, "不同接口必须产生不同稳定标识");
    }

    /// 非 USB 总线（platform 设备）必须返回 None，不得编造标识。
    #[test]
    fn stable_id_rejects_non_usb_bus() {
        // 总线目录是 `mmc0` 而非 `usbN`
        let comps: Vec<String> = ["", "sys", "devices", "platform", "soc", "mmc0", "mmc0:1.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(stable_id_from_components(&comps), None);
    }

    /// 组件过短（无祖父级）必须返回 None 而非 panic。
    #[test]
    fn stable_id_handles_short_paths() {
        assert_eq!(stable_id_from_components(&[]), None);
        assert_eq!(
            stable_id_from_components(&["usb2".to_string()]),
            None
        );
        assert_eq!(
            stable_id_from_components(&["usb2".to_string(), "2-1:1.6".to_string()]),
            None,
            "缺少总线级父目录时必须拒绝"
        );
    }

    /// 拓扑标识必须能被识别为「需解析」：以 `usb-` 开头且不是绝对路径。
    /// resolve_stable 对非稳定形态原样返回（不 panic、不改写）。
    #[test]
    fn resolve_stable_passthrough_for_unknown_forms() {
        let cfg = Config::default();
        assert_eq!(resolve_stable(&cfg, "/dev/ttyUSB3"), "/dev/ttyUSB3");
        // 拓扑标识但系统无匹配口 -> 原样返回，交给 open 报明确错误
        let ghost = "usb-usb99-9-9:1.9";
        assert_eq!(resolve_stable(&cfg, ghost), ghost);
    }

    /// `has_udev()` 必须是纯探测、不 panic（结果随环境而异）。
    #[test]
    fn has_udev_probe_is_safe() {
        let _ = has_udev();
    }

    /// `any_fm350_present` 在无模组环境下必须为 false（沙箱 CI 即此环境）。
    /// 若为 true 说明描述符匹配过于宽松 —— 正是实机暴露的 7 口全中问题。
    #[test]
    fn any_fm350_present_false_without_modem() {
        let cfg = Config::default();
        // 沙箱里通常没有任何 ttyUSB/2cb7 设备
        let ports = list_ports(&cfg, false);
        let claimed: Vec<&str> = ports
            .iter()
            .filter(|p| p.likely_fm350)
            .map(|p| p.name.as_str())
            .collect();
        // 只允许「真有 Fibocom 描述符」的口被判中，不能凭空产生
        for p in &ports {
            if p.likely_fm350 {
                let v = p.vid.to_lowercase();
                let vin = p.vendor.to_lowercase();
                let pin = p.product.to_lowercase();
                assert!(
                    v == FIBOCOM_VID
                        || vin.contains("fibocom")
                        || pin.contains("fibocom")
                        || pin.contains("fm350"),
                    "误判为 FM350：{} vid={} vendor={} product={}",
                    p.name,
                    p.vid,
                    p.vendor,
                    p.product
                );
            }
        }
        let _ = claimed;
    }

    /// 按「当前端口 → 疑似 FM350 → 名称」排序，便于前端把最可能的选项放最前。
    #[test]
    fn port_sort_puts_current_and_fm350_first() {
        fn c(name: &str, current: bool, fm: bool) -> PortCandidate {
            PortCandidate {
                path: format!("/dev/{}", name),
                by_id: String::new(),
                stable: String::new(),
                name: name.to_string(),
                driver: String::new(),
                interface: String::new(),
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
