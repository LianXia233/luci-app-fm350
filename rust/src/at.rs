//! AT 端口访问层（独占 + 轮询式持有）。
//!
//! 依据《Fibocom FM350 AT Commands v2.2》第 2.3 / 2.4 节：
//!   - AT 接口是 TE 发起的一问一答半双工通道，单条命令必须由单一结果码应答；
//!   - Modem 会回显（echo）TE 发来的命令字符；
//!   - URC（主动上报）与结果码格式相同，会异步插入下行数据流。
//!
//! 因此 AT 口是典型的独占串行资源。本模块保证三件事：
//!
//! 1. **独占**：候选 AT 口一律以 `exclusive(true)` 打开，串口层据此执行
//!    `ioctl(TIOCEXCL)` + 排他 `flock`。这两条机制的效力必须分清：
//!
//!      - `flock` 是**建议锁**：凡是同样申请锁的程序（本插件自己的端口
//!        探测、第二个 `fm350d` 实例）都会被拒绝（`EWOULDBLOCK`）；
//!      - `TIOCEXCL` 是内核级强制排他，但**对持有 CAP_SYS_ADMIN 的进程
//!        无效** —— tty_ioctl(2) 原文：`They fail with EBUSY, except for a
//!        process with the CAP_SYS_ADMIN capability`。OpenWrt 上几乎所有
//!        进程（含 LuCI / rpcd / cat）都以 root 运行，因此它**不能**用来
//!        阻止 root 进程 `open()`。
//!
//!    所以"独占"的准确含义是：本插件在整个运行期持续持有该端口并申请排他
//!    锁，不与任何其他程序协商共享；同机制的程序会被挡在门外。对不申请锁
//!    的程序，内核不提供强制排他 —— 这部分由 `other_openers()` 主动巡检
//!    （扫 `/proc/*/fd`）覆盖，把不可强制的部分变成**可观测**：API 与前端会
//!    如实报告"另有 N 个进程打开了该 tty"，daemon 也会在日志里告警。
//!    上层还有 daemon 的 flock 单实例守卫（见 main.rs），保证 fm350d 自身
//!    也不会出现两个实例争抢同一端口。
//!
//! 2. **持久独占**：首次访问时惰性打开端口，此后一直持有，直到进程退出
//!    或串口异常为止 —— 不做任何空闲释放。AT 口在整个 daemon 生命周期内
//!    只属于 fm350d：这既是"必须独占"的要求，也避免了反复开关串口带来的
//!    握手开销（一次 status 会串起二十多条 AT 指令）。
//!
//! 3. **进程内串行**：用互斥锁把并发调用排成队列，保证一问一答。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serialport::SerialPort;

/// 结果码终止标记（手册 2.4.3：响应以结果码结束）。
const TERMINATORS: &[&str] = &[
    "OK",
    "ERROR",
    "BUSY",
    "NO CARRIER",
    "NO ANSWER",
    "NO DIALTONE",
    "+CME ERROR",
    "+CMS ERROR",
];

/// IMEI / 串号相关命令族。凡是属于这些族的指令，一律按"默认拒绝"处理，
/// 只有明确命中的只读形式才放行。
///
/// 安全约束（用户明确要求）：可以有 IMEI 相关**功能**（读取展示），
/// 但绝不允许写入、修改、擦除 IMEI / 串号。
const IMEI_FAMILIES: &[&str] = &["EGMREXT", "EGMR", "SIMEI", "CGSN", "SERIALNUM", "IMEI"];

/// 判断是否为 IMEI / 串号的**写入**指令。
///
/// 判定策略（默认拒绝）：
///   1. 不属于 IMEI 命令族 → 不拦截；
///   2. 属于该族但为查询形式（`AT+CGSN`、`AT+CGSN?`、`AT+EGMR?`）→ 放行；
///   3. 属于该族且为 `AT+EGMREXT=0,<n>` / `AT+EGMR=0,<n>` 这类读形式 → 放行；
///   4. 其余一切携带参数的写形式（尤其 `AT+EGMREXT=1,...`、`AT+EGMR=1,7,"xxx"`、
///      `AT+SIMEI=...`、`AT+CGSN=...`）→ 拦截。
pub fn is_imei_write(cmd: &str) -> bool {
    let raw = cmd.trim();
    let upper = raw.to_uppercase();
    let body = upper.strip_prefix("AT+").unwrap_or(&upper).trim();

    let family = match IMEI_FAMILIES.iter().find(|f| body.starts_with(*f)) {
        Some(f) => *f,
        None => return false,
    };

    // 无参数或纯查询：AT+CGSN / AT+CGSN? / AT+EGMR?
    let rest = body[family.len()..].trim();
    if rest.is_empty() || rest.starts_with('?') {
        return false;
    }

    // 读形式：AT+EGMREXT=0,<n> —— 首参为 0 表示读
    if let Some(args) = rest.strip_prefix('=') {
        let first = args.split(',').next().unwrap_or("").trim();
        if first == "0" {
            return false;
        }
    }

    true
}

/// 解释拦截原因，供 API 返回给前端。
pub fn imei_block_reason(cmd: &str) -> String {
    format!(
        "已拒绝执行 `{}`：IMEI / 串号写入指令被安全策略拦截（只读查询不受影响）",
        cmd.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::is_imei_write;

    #[test]
    fn read_only_forms_are_allowed() {
        for c in [
            "AT+EGMREXT=0,7",
            "at+egmrext=0,7",
            "AT+EGMR=0,7",
            "AT+CGSN",
            "AT+CGSN?",
            "AT+EGMR?",
            "AT+EGMREXT?",
            " AT+EGMREXT=0,7 \r\n",
        ] {
            assert!(!is_imei_write(c), "只读形式被误拦截: {}", c);
        }
    }

    #[test]
    fn write_forms_are_blocked() {
        for c in [
            "AT+EGMREXT=1,7,\"861234567890123\"",
            "AT+EGMR=1,7,\"861234567890123\"",
            "AT+EGMREXT=1,7",
            "AT+SIMEI=861234567890123",
            "AT+SIMEI=\"861234567890123\"",
            "AT+CGSN=861234567890123",
            "at+egmrext=1,7,\"x\"",
            "AT+EGMR=2,7,\"x\"",
        ] {
            assert!(is_imei_write(c), "写入形式未被拦截: {}", c);
        }
    }

    #[test]
    fn unrelated_commands_pass() {
        for c in ["AT+CSQ", "AT+CGDCONT=1,\"IPV4V6\",\"cmiot5g\"", "AT+CFUN=1,1"] {
            assert!(!is_imei_write(c), "普通命令被误拦截: {}", c);
        }
    }
}

pub type AtResult<T> = Result<T, String>;

/// 把底层打开串口的错误翻译成可读中文，便于用户在 LuCI 上定位问题。
fn friendly_open_err(path: &str, e: &str) -> String {
    let low = e.to_lowercase();
    if low.contains("exclusive") || low.contains("busy") || low.contains("resource") {
        format!("打开 {} 失败：端口已被其他程序独占（{}）", path, e)
    } else if low.contains("permission") || low.contains("denied") {
        format!("打开 {} 失败：权限不足（{}）", path, e)
    } else if low.contains("no such") || low.contains("not found") || low.contains("ENOENT") {
        format!("打开 {} 失败：设备不存在（{}）", path, e)
    } else {
        format!("打开 {} 失败: {}", path, e)
    }
}

/// 相邻两条 AT 命令之间的**最小间隔**（发令时刻之差的下限）。
///
/// 为什么需要它：模组的 AT 解析器需要喘息时间，短时间连续下发会把 AT 打挂。
/// 原先这个喘息是被「drain 的 200 ms 空等」顺带提供的（26 条命令 ≈ 5.7 s，
/// 平均间隔约 220 ms）。优化去掉了空等之后，命令间隔会骤降到几十毫秒，
/// 因此必须把「最小间隔」显式补回来，否则提速的代价就是把模组 AT 打死。
///
/// 取值 30 ms：115200 下一条短命令往返约 20~40 ms，此值通常不成为瓶颈；
/// 但一旦上层连续发起命令（如 status 一次 26 条），它就把密度硬性钳住。
const MIN_CMD_GAP: Duration = Duration::from_millis(30);

/// 独占持有的 AT 端口。
pub struct AtPort {
    port: Box<dyn SerialPort>,
    path: String,
    timeout: Duration,
    /// 上一条命令的发出时刻，用于落实 MIN_CMD_GAP
    last_cmd_at: Option<Instant>,
}

impl AtPort {
    pub fn open(path: &str, baudrate: u32, timeout_secs: u64) -> AtResult<Self> {
        // 端口 read 超时直接决定「空缓冲时 read 阻塞多久」，必须保持很小：
        // drain()/command() 都以 read 超时作为轮询节拍，默认 200 ms 会让每条
        // AT 命令白等 200 ms（一次 status 串起 20+ 条命令即数秒级空等）。
        // 真正的命令超时由 `timeout` 字段（at_timeout，默认 10 s）在 command()
        // 的 deadline 循环里控制，与本值无关。
        let builder = serialport::new(path, baudrate).timeout(Duration::from_millis(20));
        // `exclusive()` 是 unix 专有 API（Windows 上无此开关）。
        // posix 侧据此执行 ioctl(TIOCEXCL) + 排他 flock：
        //   - flock 挡住同样申请锁的程序（端口探测、第二实例）；
        //   - TIOCEXCL 对 root（CAP_SYS_ADMIN）无效，不能阻止 root open()。
        // 显式写出，让"必须独占"成为可审查的约束，而非依赖外部默认值。
        // 对不申请锁的程序，靠 other_openers() 主动巡检兜住（见模块注释）。
        #[cfg(unix)]
        let builder = builder.exclusive(true);
        let port = builder
            .open()
            .map_err(|e| friendly_open_err(path, &e.to_string()))?;
        let mut p = AtPort {
            port,
            path: path.to_string(),
            timeout: Duration::from_secs(timeout_secs),
            last_cmd_at: None,
        };
        p.handshake();
        Ok(p)
    }

    /// 握手：确认 AT 通道可用，关闭回显并开启详细错误上报。
    fn handshake(&mut self) {
        let _ = self.port.write_all(b"ATE0\r");
        let _ = self.port.flush();
        std::thread::sleep(Duration::from_millis(120));
        let _ = self.drain();
        let _ = self.command("AT+CMEE=2");
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// 排空接收缓冲。
    ///
    /// 性能要点（实机实测，插件加载慢的根因所在）：本函数在每条 AT 命令前
    /// 都会被调用一次；串口在缓冲区为空时 read 会一直阻塞到端口超时才返回，
    /// 若沿用默认的 200 ms 端口超时，则**每条命令都要白等 200 ms**。
    /// 实测 26 条命令的 status 接口因此耗时 5.7 s，其中约 5.2 s 就是这个等待。
    /// 故这里临时把超时压到 1 ms 做非阻塞排空，读完立即恢复原超时值
    /// （不改构造时的超时配置，避免影响 command() 的轮询节拍）。
    fn drain(&mut self) -> AtResult<()> {
        let orig = self.port.timeout();
        let _ = self.port.set_timeout(Duration::from_millis(1));
        let mut buf = [0u8; 512];
        loop {
            match self.port.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        let _ = self.port.set_timeout(orig);
        Ok(())
    }

    /// 发送一条 AT 命令并返回规范化响应（自动补 \r，剥离回显与 URC 干扰）。
    pub fn command(&mut self, cmd: &str) -> AtResult<String> {
        // 限速：保证与上一条命令的发出时刻至少相隔 MIN_CMD_GAP。
        // 注意这里等的是「发令时刻」之差，模组应答本身更慢时自然满足，
        // 因此不会把原本就慢的命令拖得更慢，只在命令排得很密时才生效。
        if let Some(prev) = self.last_cmd_at {
            let gap = prev.elapsed();
            if gap < MIN_CMD_GAP {
                std::thread::sleep(MIN_CMD_GAP - gap);
            }
        }
        self.last_cmd_at = Some(Instant::now());

        let line = if cmd.ends_with('\r') {
            cmd.to_string()
        } else {
            format!("{}\r", cmd)
        };
        let _ = self.drain();
        self.port
            .write_all(line.as_bytes())
            .map_err(|e| format!("写入失败: {}", e))?;
        let _ = self.port.flush();

        let deadline = Instant::now() + self.timeout;
        let mut raw = String::new();
        let mut buf = [0u8; 256];
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(n) => {
                    raw.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if finished(&raw) {
                        break;
                    }
                }
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        Ok(normalize(&raw, cmd))
    }

    /// 发送原始数据（用于 AT+CMGS 的 PDU 阶段）。
    pub fn write_raw(&mut self, data: &[u8]) -> AtResult<()> {
        self.port
            .write_all(data)
            .map_err(|e| format!("写入失败: {}", e))?;
        self.port.flush().map_err(|e| format!("flush 失败: {}", e))
    }

    /// 等待出现指定提示符（如 AT+CMGS 的 '>'）。
    pub fn wait_for(&mut self, needle: &str, timeout: Duration) -> AtResult<String> {
        let deadline = Instant::now() + timeout;
        let mut raw = String::new();
        let mut buf = [0u8; 256];
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                Ok(n) => {
                    raw.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if raw.contains(needle) {
                        return Ok(raw);
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        Err(format!("等待 '{}' 超时", needle))
    }
}

/// 判断响应是否已收敛：末尾非空行必须是结果码。
fn finished(text: &str) -> bool {
    for line in text.replace('\r', "\n").lines().rev() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        return TERMINATORS
            .iter()
            .any(|t| l == *t || l.starts_with(&format!("{}:", t)));
    }
    false
}

/// 剥离回显行，仅保留信息行与结果码。
fn normalize(raw: &str, cmd: &str) -> String {
    let echo = cmd.trim().to_string();
    raw.replace('\r', "\n")
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter(|l| *l != echo)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 端口持有统计（供 /api/ports 与排障展示）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AtStats {
    pub path: String,
    /// 物理端口此刻是否仍被本进程持有。
    pub open: bool,
    /// 已连续持有多少秒（未持有时为 0）。
    pub held_secs: u64,
    /// 累计打开次数。
    pub opens: u64,
    /// 累计释放次数。
    pub releases: u64,
    /// 除本进程外，当前也打开了该 tty 的进程号（空 = 独占有效）。
    ///
    /// `TIOCEXCL` 对 root 无效，内核不提供强制排他，因此"是否真独占"
    /// 只能主动观测。见模块注释第 1 条。
    pub other_pids: Vec<u64>,
}

struct Slot {
    port: Option<AtPort>,
    /// 本次打开的时刻，用于统计"已连续持有多少秒"。
    opened_at: Option<Instant>,
    opens: u64,
    releases: u64,
}

/// 全局 AT 端口句柄（daemon 单点持有，进程内串行，全程独占）。
pub struct AtHandle {
    inner: Mutex<Slot>,
}

impl AtHandle {
    pub fn new() -> Self {
        AtHandle {
            inner: Mutex::new(Slot {
                port: None,
                opened_at: None,
                opens: 0,
                releases: 0,
            }),
        }
    }

    /// 在锁内执行闭包，保证一问一答串行。
    ///
    /// 端口**持久独占**：首次访问时惰性打开，此后一直持有，不做空闲释放。
    /// 只有串口异常（读写失败）才丢弃句柄，下次调用重新独占打开。
    pub fn with<F, R>(&self, cfg: &crate::config::Config, f: F) -> AtResult<R>
    where
        F: FnOnce(&mut AtPort) -> AtResult<R>,
    {
        let mut slot = self.inner.lock().map_err(|e| format!("锁失败: {}", e))?;

        // 配置里的端口变了就立刻换：丢弃旧句柄，下面按新端口重新独占打开。
        // 放在这里（而不是只靠 daemon 巡检）是为了让"改完即生效"，
        // 不受 poll_interval（默认 30 秒）制约。
        if slot.port.as_ref().map(|p| p.path()) != Some(cfg.at_port.as_str()) && slot.port.is_some() {
            slot.port = None;
            slot.opened_at = None;
            slot.releases += 1;
        }

        if slot.port.is_none() {
            slot.port = Some(AtPort::open(
                &cfg.at_port,
                cfg.baudrate,
                cfg.at_timeout,
            )?);
            slot.opened_at = Some(Instant::now());
            slot.opens += 1;
        }

        let port = slot.port.as_mut().expect("端口已在上面确保打开");
        match f(port) {
            Ok(v) => Ok(v),
            Err(e) => {
                // 串口异常：丢弃句柄，下次调用重建（重新独占打开）
                slot.port = None;
                slot.opened_at = None;
                slot.releases += 1;
                Err(e)
            }
        }
    }

    /// 当前实际持有的端口路径（未持有时为 `None`）。
    ///
    /// daemon 用它感知 `at_port` 是否被改过：改了就先释放旧句柄，
    /// 让下一次访问按新端口重新独占打开，无需重启服务。
    pub fn current_path(&self) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.port.as_ref().map(|p| p.path().to_string()))
    }

    /// 强制关闭端口。
    ///
    /// 正常运行期间**不会**调用它——AT 口由 daemon 持续独占。
    /// 仅用于：切换端口、服务停止、串口异常后的重建。
    pub fn close(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if g.port.is_some() {
                g.port = None;
                g.opened_at = None;
                g.releases += 1;
            }
        }
    }

    /// 端口持有统计（供 /api/ports 与排障展示）。
    ///
    /// `other_pids` 需要遍历 `/proc`，因此先把锁内的状态取出来，
    /// 释放锁之后再扫描，避免拖长 AT 串行锁的占用时间。
    pub fn stats(&self, cfg: &crate::config::Config) -> AtStats {
        let path = cfg.at_port.clone();
        let (open, held_secs, opens, releases) = match self.inner.lock() {
            Ok(g) => (
                g.port.is_some(),
                g.opened_at.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                g.opens,
                g.releases,
            ),
            Err(_) => (false, 0, 0, 0),
        };
        let other_pids = if open { other_openers(&path) } else { Vec::new() };
        AtStats {
            path,
            open,
            held_secs,
            opens,
            releases,
            other_pids,
        }
    }
}

/// 扫描 `/proc/*/fd`，找出**除本进程外**打开了 `path` 的进程号。
///
/// 存在的理由：`TIOCEXCL` 对 root（CAP_SYS_ADMIN）无效，内核不提供强制
/// 排他（见模块注释第 1 条），所以"是否真的独占"只能靠主动观测。
/// 返回空表示当前无人抢占；非空表示独占事实已被破坏，需在日志与前端暴露。
///
/// 非 Linux 平台没有 `/proc`，返回空（Windows 由系统保证独占打开）。
#[cfg(target_os = "linux")]
pub fn other_openers(path: &str) -> Vec<u64> {
    let want = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let me = u64::from(std::process::id());
    let mut out: Vec<u64> = Vec::new();
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return out,
    };
    for ent in proc_dir.flatten() {
        let pid: u64 = match ent.file_name().to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == me {
            continue;
        }
        let fds = match std::fs::read_dir(ent.path().join("fd")) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for fd in fds.flatten() {
            if let Ok(t) = std::fs::read_link(fd.path()) {
                if t == want {
                    out.push(pid);
                    break;
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(not(target_os = "linux"))]
pub fn other_openers(_path: &str) -> Vec<u64> {
    Vec::new()
}

// ---------------------------------------------------------------- 端口枚举

/// 候选 tty 前缀：FM350 在 `+GTUSBMODE=40` 下的 AT 口是 option 驱动导出的
/// `/dev/ttyUSB*`；部分固件/模组形态也可能是 CDC-ACM 的 `/dev/ttyACM*`。
const AT_CANDIDATE_PREFIXES: &[&str] = &["ttyUSB", "ttyACM"];

/// Fibocom 的 USB Vendor ID。
const FIBOCOM_VID: &str = "2cb7";

/// 设备名是否属于候选 AT 口（`ttyUSB0` / `ttyACM1` / `ttyUSB1.2` 之类）。
pub fn is_candidate_tty(name: &str) -> bool {
    AT_CANDIDATE_PREFIXES.iter().any(|p| match name.strip_prefix(p) {
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
    v == FIBOCOM_VID
        || vend.contains("fibocom")
        || prod.contains("fibocom")
        || prod.contains("fm350")
}

/// 单个候选端口的信息。
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
fn find_usb_attr(start: &Path, attr: &str) -> Option<String> {
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
        Err(e) => {
            let s = e.to_string();
            (Some(true), format!("无法打开：{}", s))
        }
    }
}

/// 按"当前端口 → 疑似 FM350 → 名称"排序，便于前端把最可能的选项放最前。
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
pub fn list_ports(cfg: &crate::config::Config, probe: bool) -> Vec<PortCandidate> {
    let mut out: Vec<PortCandidate> = Vec::new();

    if let Ok(rd) = std::fs::read_dir("/dev") {
        let mut names: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| is_candidate_tty(n))
            .collect();
        names.sort();

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

/// 从响应中提取首个 `+XXX: ` 之后的字段列表。
pub fn fields(resp: &str, prefix: &str) -> Vec<String> {
    for line in resp.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix(prefix) {
            let rest = rest.trim_start_matches(':').trim();
            return rest
                .split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .collect();
        }
    }
    Vec::new()
}

/// 从响应中取首个 IPv4（用于 AT+CGPADDR）。
pub fn first_ipv4(resp: &str) -> Option<String> {
    for line in resp.lines() {
        for token in line.split(&[',', '"', ' '][..]) {
            let t = token.trim();
            if t.split('.').count() == 4 && t.chars().all(|c| c.is_ascii_digit() || c == '.') {
                if !t.starts_with("0.0.0.0") && t != "255.255.255.255" {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod poll_tests {
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
        assert!(!looks_like_fm350("1a86", "QinHeng Electronics", "CH340"));
        assert!(!looks_like_fm350("", "", ""));
    }

    #[test]
    fn find_usb_attr_walks_up_and_reads_sysfs_value() {
        // 构造 <tmp>/a/b/c 目录树，把 idVendor 放在 a 层，
        // 从 c 出发应能向上两级读到，证明符号链接解析后的逐级查找正确。
        let root = std::env::temp_dir().join(format!("fm350-at-test-{}", std::process::id()));
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
    fn other_openers_never_reports_self_or_missing_path() {
        // 不存在的设备：不应报出任何占用者（各平台都应为空）。
        assert!(other_openers("/dev/fm350-no-such-port-xyz").is_empty());
        // 自己打开的文件不应把自己算成"他人占用"。
        let f = std::fs::File::open("/etc/hostname").or_else(|_| std::fs::File::open("/etc/hosts"));
        if let Ok(f) = f {
            let path = "/etc/hostname";
            let others = other_openers(path);
            assert!(
                !others.contains(&u64::from(std::process::id())),
                "把自己误报为占用者: {:?}",
                others
            );
            drop(f);
        }
    }

    #[test]
    fn open_error_is_translated() {
        let s = friendly_open_err("/dev/ttyUSB1", "Unable to acquire exclusive lock on serial port");
        assert!(s.contains("已被其他程序独占"), "独占冲突未被识别: {}", s);
        assert!(s.contains("/dev/ttyUSB1"), "路径缺失: {}", s);

        assert!(friendly_open_err("/dev/ttyX", "Permission denied").contains("权限不足"));
        assert!(friendly_open_err("/dev/ttyX", "No such file or directory").contains("设备不存在"));
        assert!(friendly_open_err("/dev/ttyX", "weird failure").contains("weird failure"));
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
