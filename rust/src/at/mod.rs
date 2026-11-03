//! AT 端口访问层：独占持有 + 进程内串行。
//!
//! ## 独占语义（必须看清，否则会误判"为什么还会被抢"）
//!
//! 依据《Fibocom FM350 AT Commands v2.2》2.3 / 2.4 节：AT 接口是 TE 发起的
//! 一问一答半双工通道，Modem 会回显命令字符，URC 与结果码格式相同并异步
//! 插入下行流。因此 AT 口是典型的独占串行资源。本模块保证三件事：
//!
//! 1. **独占**：候选 AT 口以 `exclusive(true)` 打开，串口层据此执行
//!    `ioctl(TIOCEXCL)` + 排他 `flock`。两者效力不同，必须分清：
//!      - `flock` 是建议锁，凡是同样申请锁的程序（本插件自己的端口探测、
//!        第二个 `fm350d` 实例）都会被拒绝（`EWOULDBLOCK`）；
//!      - `TIOCEXCL` 是内核级强制排他，但**对持有 CAP_SYS_ADMIN 的进程无效**
//!        （tty_ioctl(2)：`They fail with EBUSY, except for a process with the
//!        CAP_SYS_ADMIN capability`）。OpenWrt 上几乎所有进程（LuCI / rpcd /
//!        cat）都以 root 运行，所以它**不能**阻止 root 进程 `open()`。
//!
//!    所以"独占"的准确含义是：本插件在整个运行期持续持有该端口并申请排他锁，
//!    不与任何其他程序协商共享；同机制的程序会被挡在门外。对不申请锁的程序，
//!    内核不提供强制排他 —— 这部分由 `other_openers()` 主动巡检（扫
//!    `/proc/*/fd`）覆盖，把不可强制的部分变成**可观测**。上层还有 daemon 的
//!    flock 单实例守卫（见 `daemon::singleton`），保证 fm350d 自身也不出现
//!    两个实例争抢同一端口。
//!
//! 2. **持久独占**：首次访问时惰性打开，此后一直持有，直到进程退出或串口
//!    异常为止 —— 不做任何空闲释放。一次 status 会串起二十多条 AT 指令，
//!    反复开关串口的握手开销无法接受。
//!
//! 3. **进程内串行**：全部访问进 AT 工作线程的 FIFO 邮箱（[`worker`]），
//!    worker 逐个执行，保证一问一答。外部经 [`handle::AtHandle`] 投递。
//!
//! ## 子模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`self`] | `AtPort` 同步串口会话、协议解析、守卫常量 |
//! | [`port`] | 端口**发现**（sysfs 扫描、稳定标识、哑口探测），不持有 |
//! | [`worker`] | 工作线程：邮箱 FIFO + 独占状态机（热切换/熔断/哑口改选） |
//! | [`handle`] | `AtHandle` 门面：同步 `with()` / 快照读取 |
//! | [`cmd`] | AT 指令常量与写入守卫（IMEI 拦截） |
//! | [`parse`] | 响应字段/行解析 |
//! | [`urc`] | `+GT*` URC 行过滤器与事件队列 |
//! | [`guard`] | 高风险指令识别 |

pub mod cmd;
pub mod guard;
pub mod handle;
pub mod parse;
pub mod port;
pub mod urc;
pub mod worker;

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use serialport::SerialPort;

pub use cmd as at_cmd;
pub use guard::{block_reason, is_imei_write, IMEI_READ_CMD};
pub use handle::AtHandle;
pub use parse::{field, first, first_ipv4, has_error, is_ok, raw_lines, rows, scalar};
pub use port::{
    any_fm350_present, by_id_link, find_usb_attr, has_udev, identify_at_port, is_candidate_tty,
    list_ports, looks_like_fm350, reset_usb_device, resolve_stable, stable_path, sysfs_stable_id,
    PortCandidate,
};
pub use urc::{snapshot as gt_snapshot, GtEvent};

/// 旧代码里 `at::fields(resp, prefix)` 的等价物，保留这个名字以减小调用点改动。
pub fn fields(resp: &str, prefix: &str) -> Vec<String> {
    parse::first(resp, prefix)
}

/// 旧代码里 `at::imei_block_reason(cmd)` 的等价物。
pub fn imei_block_reason(cmd: &str) -> String {
    guard::block_reason(cmd)
}

/// 统一错误类型（沿用原实现的 `String`，避免引入 anyhow 等新依赖）。
pub type AtResult<T> = Result<T, String>;

/// 端口持有统计（供 `/api/ports` 与排障展示）。
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
    pub other_pids: Vec<u64>,
}

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

/// 相邻两条 AT 命令之间的**最小间隔**（发令时刻之差的下限）。
///
/// 存在理由：模组的 AT 解析器需要喘息时间，短时间连续下发会把 AT 打挂。
/// 原先这个喘息是被「drain 的 200 ms 空等」顺带提供的（26 条命令 ≈ 5.7 s，
/// 平均间隔约 220 ms）。优化去掉空等之后命令间隔会骤降到几十毫秒，因此必须
/// 显式补回来 —— 否则提速的代价就是把模组 AT 打死。
///
/// 30 ms：115200 下一条短命令往返约 20~40 ms，通常不成为瓶颈；但上层连续
/// 发起命令（如 status 一次 26 条）时，它把密度硬性钳住。
const MIN_CMD_GAP: Duration = Duration::from_millis(30);

/// 端口 read 超时。
///
/// 必须保持很小：`drain()` / `command()` 都以 read 超时作为轮询节拍。原实现
/// 默认 200 ms 会让每条 AT 命令白等 200 ms（一次 status 串起 20+ 条即数秒级
/// 空等）。真正的命令超时由 `AtPort::timeout`（at_timeout，默认 10 s）在
/// `command()` 的 deadline 循环里控制，与本值无关。
const READ_TIMEOUT: Duration = Duration::from_millis(20);

/// 排空接收缓冲时临时压到的超时（近乎非阻塞）。
const DRAIN_TIMEOUT: Duration = Duration::from_millis(1);

/// 握手阶段的等待（下发 ATE0 后给模组一点时间）。
const HANDSHAKE_WAIT: Duration = Duration::from_millis(120);

/// 哑口判定阈值：**连续**多少条 AT 命令零字节应答，判定当前端口失联。
///
/// 适用场景：FM350 会导出 7 个 ttyUSB（DIAG / GNSS / AT / MODEM / log ...），
/// 只有真正的 AT 口才会应答；USB 重枚举后 tty 编号漂移，配置里指向的口
/// 可能变成一个「存在但永不说话」的哑口。此时每条命令都会等满
/// `at_timeout`（默认 10 s），一轮巡检 20+ 条即数分钟，整个后端表现为瘫痪。
/// 连续 3 条零应答（约 30 s）足以与「模组短暂重启」区分开。
const SILENT_RESELECT_THRESHOLD: u32 = 3;

/// 哑口改选的冷却时间：自动探测失败后多久内不重复探测。
///
/// 探测本身要对每个候选口独占打开 + 发 `AT` 等 0.8 s（7 个口最多约 6 s），
/// 若模组整体离线，每次巡检都探测会拖垮巡检节奏；冷却期内先如实报错。
const RESELECT_COOLDOWN: Duration = Duration::from_secs(60);

/// 改选保持窗口：自动改选成功后的这段时间内，携带**旧配置**的任务直接
/// 快速失败，不再逐个撞哑口。
///
/// 场景：改选把新端口写进 UCI 后，本巡检周期内已排队的任务仍携带旧的
/// `cfg` 副本（每个任务 3 条静默命令 × at_timeout ≈ 30 s 才能触发改选）
/// —— 一个拨号流程能磨上几分钟才收敛。窗口期内跳过它们，等下一轮
/// 巡检读到新配置即可正常工作。窗口取 120 s > 一个完整巡检周期。
const RESELECT_HOLD: Duration = Duration::from_secs(120);

/// 打开失败后的重试冷却：最近一次独占打开失败后，多久内不再重复尝试。
///
/// 模组未识别 / 被拔出时，配置端口不存在，每次 API 调用都会走到 open 失败
/// 分支。open 本身虽然快速失败（ENOENT），但「每个请求都撞一次内核 +
/// sysfs」在 LuCI 高频轮询下毫无意义，还会放大 rpcd 同步转发延迟。冷却期
/// 内直接返回上次错误（熔断），到点后再放行一次真实探测——模组插回后
/// 最多多等一个冷却周期即可恢复。
const OPEN_RETRY_COOLDOWN: Duration = Duration::from_secs(5);

/// 把底层打开串口的错误翻译成可读中文，便于用户在 LuCI 上定位问题。
fn friendly_open_err(path: &str, e: &str) -> String {
    let low = e.to_lowercase();
    if low.contains("exclusive") || low.contains("busy") || low.contains("resource") {
        format!("打开 {} 失败：端口已被其他程序独占（{}）", path, e)
    } else if low.contains("permission") || low.contains("denied") {
        format!("打开 {} 失败：权限不足（{}）", path, e)
    } else if low.contains("no such") || low.contains("not found") || low.contains("enoent") {
        format!("打开 {} 失败：设备不存在（{}）", path, e)
    } else {
        format!("打开 {} 失败: {}", path, e)
    }
}

/// 独占持有的 AT 端口（同步会话，仅 worker 线程触碰）。
pub struct AtPort {
    port: Box<dyn SerialPort>,
    path: String,
    /// 真正的命令超时（秒），由 `at_timeout` 配置项决定。
    timeout: Duration,
    /// 上一条命令的发出时刻，用于落实 `MIN_CMD_GAP`。
    last_cmd_at: Option<Instant>,
    /// URC 行过滤器：所有下行字节先过这里，`+GT*` 行被截流入事件队列
    /// （见 [`urc`]），其余行原样进入命令响应数据流。
    gate: urc::LineGate,
    /// 连续零应答计数：`command()` 全程未收到任何字节则 +1，收到数据清零。
    /// 供哑口判定（[`SILENT_RESELECT_THRESHOLD`]）使用。
    silent_streak: u32,
}

impl AtPort {
    pub fn open(path: &str, baudrate: u32, timeout_secs: u64) -> AtResult<Self> {
        let builder = serialport::new(path, baudrate).timeout(READ_TIMEOUT);
        // `exclusive()` 是 unix 专有 API（Windows 上无此开关）。
        // 显式写出，让"必须独占"成为可审查的约束，而非依赖外部默认值。
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
            gate: urc::LineGate::default(),
            silent_streak: 0,
        };
        p.handshake();
        Ok(p)
    }

    /// 握手：确认 AT 通道可用，关闭回显并开启详细错误上报。
    fn handshake(&mut self) {
        // 注意：**绝不调用 flush()**（= tcdrain）。USB 串口端点被模组打楔
        // （URC 停摆 / tx 缓冲排不空）时 tcdrain 会在内核里无界阻塞
        // （tty_wait_until_sent），把 AT 工作线程永久钉死 —— 实机
        // 实验固件复现（2026-11：模组数据面异常后 AT 口整体 wedge）。
        // write_all 只进内核缓冲即返回，配合读超时 + 哑口改选即可自愈。
        let _ = self.port.write_all(b"ATE0\r");
        std::thread::sleep(HANDSHAKE_WAIT);
        let _ = self.drain();
        let _ = self.command("AT+CMEE=2");
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// 排空接收缓冲。
    ///
    /// 每条命令前调用一次。临时把超时压到 1 ms 做非阻塞排空，读完立即恢复
    /// 原超时值（不改构造时的配置，避免影响 `command()` 的轮询节拍）。
    /// 排空的数据同样过 URC 过滤器：巡检间隔期间插进来的 `+GT*` 行在这里
    /// 被截流入队，其余数据本来就要丢弃。
    fn drain(&mut self) -> AtResult<()> {
        let orig = self.port.timeout();
        let _ = self.port.set_timeout(DRAIN_TIMEOUT);
        let mut buf = [0u8; 512];
        let mut sink = Vec::new();
        loop {
            match self.port.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    self.gate.feed(&buf[..n], &mut sink);
                    sink.clear();
                    continue;
                }
            }
        }
        let _ = self.port.set_timeout(orig);
        Ok(())
    }

    /// 发送一条 AT 命令并返回规范化响应（自动补 `\r`，剥离回显）。
    pub fn command(&mut self, cmd: &str) -> AtResult<String> {
        self.pace();

        let line = if cmd.ends_with('\r') {
            cmd.to_string()
        } else {
            format!("{}\r", cmd)
        };
        let _ = self.drain();
        self.port
            .write_all(line.as_bytes())
            .map_err(|e| format!("写入失败: {}", e))?;
        // 不 flush（tcdrain）：见 handshake() 内注释 —— wedge 端点上无界阻塞

        let deadline = Instant::now() + self.timeout;
        let mut raw = String::new();
        let mut buf = [0u8; 256];
        let mut chunk = Vec::new();
        let mut received = 0usize;
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                Ok(n) => {
                    received += n;
                    // URC 行在此被截流入队，只有业务响应进入 raw
                    self.gate.feed(&buf[..n], &mut chunk);
                    raw.push_str(&String::from_utf8_lossy(&chunk));
                    chunk.clear();
                    if finished(&raw) {
                        break;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        // 哑口观测：整条命令一个字节都没回来（连 URC 都没有）→ 计数 +1；
        // 任何数据到达都说明端口是活的，清零重计。
        if received == 0 {
            self.silent_streak = self.silent_streak.saturating_add(1);
        } else {
            self.silent_streak = 0;
        }
        Ok(normalize(&raw, cmd))
    }

    /// 发送一条**以提示符结束**的命令（如 `AT+CMGS` 的 `>`）。
    ///
    /// 与 [`command`](Self::command) 的区别：不等结果码，收到提示符立即返回，
    /// 把随后的数据通道留给调用方（`AT+CMGS` 需要在 `>` 之后写 PDU）。
    /// 若模组直接回 `ERROR` 等结果码，则立即失败而不是空等到超时。
    pub fn command_prompt(
        &mut self,
        cmd: &str,
        prompt: &str,
        timeout: Duration,
    ) -> AtResult<String> {
        self.pace();

        let line = if cmd.ends_with('\r') {
            cmd.to_string()
        } else {
            format!("{}\r", cmd)
        };
        let _ = self.drain();
        self.port
            .write_all(line.as_bytes())
            .map_err(|e| format!("写入失败: {}", e))?;
        // 不 flush（tcdrain）：见 handshake() 内注释

        let deadline = Instant::now() + timeout;
        let mut raw = String::new();
        let mut buf = [0u8; 256];
        let mut chunk = Vec::new();
        let mut received = 0usize;
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                Ok(n) => {
                    received += n;
                    self.gate.feed(&buf[..n], &mut chunk);
                    raw.push_str(&String::from_utf8_lossy(&chunk));
                    chunk.clear();
                    // 提示符（如 "> "）不带换行，会被 LineGate 留在残余缓冲
                    // 而不是进 raw —— 除 raw 外必须探测 pending；命中后取走
                    // 残余并并入 raw，防止它混入后续 PDU 阶段的应答。
                    if self.gate.pending().windows(prompt.len()).any(|w| w == prompt.as_bytes()) {
                        raw.push_str(&String::from_utf8_lossy(&self.gate.take_pending()));
                    }
                    if raw.contains(prompt) {
                        self.silent_streak = 0;
                        return Ok(normalize(&raw, cmd));
                    }
                    if finished(&raw) {
                        self.silent_streak = 0;
                        return Err(format!(
                            "{} 未给出 '{}' 提示符：{}",
                            cmd,
                            prompt,
                            normalize(&raw, cmd)
                        ));
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        if received == 0 {
            self.silent_streak = self.silent_streak.saturating_add(1);
        } else {
            self.silent_streak = 0;
        }
        Err(format!("等待 '{}' 提示符超时", prompt))
    }

    /// 等待一条结果码（`OK` / `ERROR` / `+CME ERROR` / `+CMS ERROR` 等）。
    ///
    /// 与 [`command`](Self::command) 的区别：这里**区分成功与失败**，`OK`
    /// 返回规范化响应，其它结果码直接作为错误返回，便于上层展示真实原因。
    /// 用于 `AT+CMGS` 写完 PDU 之后的收尾。
    pub fn wait_result(&mut self, timeout: Duration) -> AtResult<String> {
        let deadline = Instant::now() + timeout;
        let mut raw = String::new();
        let mut buf = [0u8; 256];
        let mut chunk = Vec::new();
        while Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                Ok(n) => {
                    self.gate.feed(&buf[..n], &mut chunk);
                    raw.push_str(&String::from_utf8_lossy(&chunk));
                    chunk.clear();
                    if finished(&raw) {
                        let out = normalize(&raw, "");
                        return if last_line(&out) == "OK" {
                            Ok(out)
                        } else {
                            Err(out)
                        };
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        Err("等待模组结果码超时".to_string())
    }

    /// 命令限速：保证与上一条命令的发出时刻至少相隔 [`MIN_CMD_GAP`]。
    ///
    /// 等的是「发令时刻」之差，模组应答本身更慢时自然满足，因此不会把原本
    /// 就慢的命令拖得更慢，只在命令排得很密时才生效。
    fn pace(&mut self) {
        if let Some(prev) = self.last_cmd_at {
            let gap = prev.elapsed();
            if gap < MIN_CMD_GAP {
                std::thread::sleep(MIN_CMD_GAP - gap);
            }
        }
        self.last_cmd_at = Some(Instant::now());
    }

    /// 连续零应答计数（哑口判定依据）。
    pub fn silent_streak(&self) -> u32 {
        self.silent_streak
    }

    /// 发送原始数据（用于 `AT+CMGS` 的 PDU 阶段）。
    ///
    /// 不 flush（tcdrain）：wedge 端点上无界阻塞，见 [`AtPort::handshake`]。
    pub fn write_raw(&mut self, data: &[u8]) -> AtResult<()> {
        self.port
            .write_all(data)
            .map_err(|e| format!("写入失败: {}", e))
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

/// 取规范化响应的最后一行（结果码所在行）。
fn last_line(text: &str) -> &str {
    text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// 扫描 `/proc/*/fd`，找出**除本进程外**打开了 `path` 的进程号。
///
/// 存在的理由：`TIOCEXCL` 对 root 无效，内核不提供强制排他，所以"是否真的
/// 独占"只能靠主动观测。返回空表示当前无人抢占；非空表示独占事实已被破坏，
/// 需在日志与前端暴露。
#[cfg(target_os = "linux")]
pub fn other_openers(path: &str) -> Vec<u64> {
    // 仅本函数需要（非 Linux 分支没有 /proc 可扫），故放在函数内引入，
    // 避免在 Windows/macOS 上产生 unused import 警告。
    use std::path::PathBuf;

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

/// 非 Linux 平台没有 `/proc`，返回空（Windows 由系统保证独占打开）。
#[cfg(not(target_os = "linux"))]
pub fn other_openers(_path: &str) -> Vec<u64> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_is_finished_only_on_result_code() {
        assert!(finished("+CSQ: 1,2\r\nOK\r\n"));
        assert!(finished("+CME ERROR: 100\r\n"));
        assert!(!finished("+CSQ: 1,2\r\n"));
        assert!(!finished(""));
    }

    #[test]
    fn normalize_strips_echo_and_blank_lines() {
        let out = normalize("AT+CSQ\r+CSQ: 99,99\r\r\nOK\r\n", "AT+CSQ");
        assert_eq!(out, "+CSQ: 99,99\nOK");
    }

    #[test]
    fn last_line_returns_terminal_code() {
        assert_eq!(last_line("+CMGS: 5\nOK"), "OK");
        assert_eq!(last_line("+CMS ERROR: 500"), "+CMS ERROR: 500");
        assert_eq!(last_line(""), "");
    }

    #[test]
    fn open_errors_are_translated_to_chinese() {
        let s = friendly_open_err("/dev/ttyUSB1", "Unable to acquire exclusive lock on serial port");
        assert!(s.contains("已被其他程序独占"));
        assert!(s.contains("/dev/ttyUSB1"));
        assert!(friendly_open_err("/dev/ttyX", "Permission denied").contains("权限不足"));
        assert!(friendly_open_err("/dev/ttyX", "No such file or directory").contains("设备不存在"));
        assert!(friendly_open_err("/dev/ttyX", "weird failure").contains("weird failure"));
    }

    #[test]
    fn other_openers_never_reports_self() {
        assert!(other_openers("/dev/fm350-no-such-port-xyz").is_empty());
    }

    /// 哑口阈值含义守护：3 条命令 × at_timeout(默认 10 s) ≈ 30 s，必须
    /// 远小于一轮巡检的总耗时，否则改选永远轮不到。
    #[test]
    fn silent_threshold_is_small() {
        assert!(SILENT_RESELECT_THRESHOLD <= 5);
    }

    /// 熔断冷却必须显著小于请求侧预算（api::budget READ = 10 s），
    /// 否则模组插回后的恢复探测会被连续的熔断错误挡住。
    #[test]
    fn open_cooldown_below_api_read_budget() {
        assert!(OPEN_RETRY_COOLDOWN < Duration::from_secs(10));
    }
}
