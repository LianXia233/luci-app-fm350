//! `+GT*` URC 被动消费层（f22-atproxy 的 EIF 语义代理事件）。
//!
//! ## 设计定位：兜底增强，绝不是功能依赖
//!
//! f22-atproxy（刷入修改版 root.squashfs 后存在）把模组 EIF_IND 降维成
//! `+GT*` 形态插进 AT 口下行流。本模块在 AT 读取路径上**顺带**拦截这些行，
//! 转成事件供守护加速巡检；未刷 rootfs 的设备上永远见不到 `+GT*` 行，
//! 拦截层零开销直通，主线（拨号巡检轮询 `AT+CGPADDR` 核对地址）不受任何影响。
//!
//! ## 为什么在读取路径拦截而不是独立读线程
//!
//! AT 口由守护独占持有、互斥锁一问一答（见 [`super`] 模块文档）。独立读
//! 线程必须与命令路径抢锁，会把半双工时序搅乱。读取路径拦截的代价是
//! 「巡检间隔期间 URC 停在模组发送缓冲里」，但串口缓冲足够大，下一次读
//! （任何命令响应或 drain）会一次性带出，解析器按行切分不依赖到达时机。
//!
//! ## 防注入
//!
//! URC 行来自 tty 下行流，atcid 与 atproxy 是同一通道的两个写者。所有字段
//! 解析都做严格字符集校验（地址逐段验数字、动词只允许 `[a-z0-9_]`），
//! 畸形 `+GT` 行一律吞掉并计数，绝不把原始流透传给上层。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 事件队列容量。满员丢最旧（EIF 是加速信号，堆积本身说明消费端停滞）。
const QUEUE_CAP: usize = 64;

/// 单个 `+GT*` 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GtEvent {
    /// RA 缺失（initial=首次 / refresh=刷新）。
    Nora(&'static str),
    /// MTU 变更。
    IfMtu(u64),
    /// 接口状态变化（cause 原样透出，方向由主线结合现实判定）。
    IfSt(i64),
    /// 地址事件汇总（reason 动词 + 计数）。
    IfAddr {
        reason: String,
        cause: i64,
        v4cnt: u32,
        v6cnt: u32,
    },
    /// 具体地址行（V4）。atproxy 每地址一行，行间无顺序依赖。
    Addr4(String),
    /// 具体地址行（V6）。
    Addr6(String),
    /// 兜底事件（未知 reason 动词）。
    Evt { reason: String, cause: i64 },
}

/// `+GTNORA: initial|refresh`
fn parse_nora(rest: &str) -> Option<GtEvent> {
    match rest.trim() {
        "initial" => Some(GtEvent::Nora("initial")),
        "refresh" => Some(GtEvent::Nora("refresh")),
        _ => None,
    }
}

/// `+GTIFMTU: <u64>`
fn parse_mtu(rest: &str) -> Option<GtEvent> {
    rest.trim().parse::<u64>().ok().map(GtEvent::IfMtu)
}

/// `cause=<i64>` 键值提取（其余参数忽略，向前兼容加字段）。
fn parse_cause_kv(rest: &str) -> Option<i64> {
    for part in rest.split(',') {
        let p = part.trim();
        if let Some(v) = p.strip_prefix("cause=") {
            return v.trim().parse::<i64>().ok();
        }
    }
    None
}

/// 动词合法性：非空、`[a-z0-9_]`、不超过 32 字符。
fn is_plausible_verb(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// V4 形态校验：四段点分，每段 1-3 位数字且数值不超过 255。
fn is_plausible_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().map(|v| v <= 255).unwrap_or(false)
        })
}

/// V6 形态校验（宽松）：至少一个冒号，字符全在十六进制/冒号集合，长度上限 45。
fn is_plausible_ipv6(s: &str) -> bool {
    s.len() <= 45
        && s.contains(':')
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() || c == ':')
}

/// `+GTIFADDR: <reason>,cause=<i>,v4cnt=<n>,v6cnt=<n>`
fn parse_ifaddr(rest: &str) -> Option<GtEvent> {
    let parts: Vec<&str> = rest.split(',').map(|p| p.trim()).collect();
    let reason = *parts.first()?;
    if !is_plausible_verb(reason) {
        return None;
    }
    let mut cause = 0i64;
    let mut v4cnt = 0u32;
    let mut v6cnt = 0u32;
    for p in &parts[1..] {
        if let Some(v) = p.strip_prefix("cause=") {
            cause = v.parse().ok()?;
        } else if let Some(v) = p.strip_prefix("v4cnt=") {
            v4cnt = v.parse().ok()?;
        } else if let Some(v) = p.strip_prefix("v6cnt=") {
            v6cnt = v.parse().ok()?;
        }
    }
    Some(GtEvent::IfAddr {
        reason: reason.to_string(),
        cause,
        v4cnt,
        v6cnt,
    })
}

/// 单行解析入口：区分前缀后分发。
pub fn parse_line(line: &str) -> Option<GtEvent> {
    let l = line.trim();
    if let Some(rest) = l.strip_prefix("+GTNORA: ") {
        return parse_nora(rest);
    }
    if let Some(rest) = l.strip_prefix("+GTIFMTU: ") {
        return parse_mtu(rest);
    }
    if let Some(rest) = l.strip_prefix("+GTIFST: ") {
        return parse_cause_kv(rest).map(GtEvent::IfSt);
    }
    if let Some(rest) = l.strip_prefix("+GTIFADDR: ") {
        return parse_ifaddr(rest);
    }
    if let Some(rest) = l.strip_prefix("+GTIFADDR4: ") {
        let a = rest.trim();
        return is_plausible_ipv4(a)
            .then(|| GtEvent::Addr4(a.to_string()));
    }
    if let Some(rest) = l.strip_prefix("+GTIFADDR6: ") {
        let a = rest.trim();
        return is_plausible_ipv6(a)
            .then(|| GtEvent::Addr6(a.to_string()));
    }
    if let Some(rest) = l.strip_prefix("+GTIFEVT: ") {
        // 兜底：`<reason>,cause=<i>`；reason 兜住全部残余，保证不丢事件
        let (reason, cause) = match rest.rsplit_once(",cause=") {
            Some((r, c)) => (r, c.trim().parse::<i64>().unwrap_or(-1)),
            None => (rest, -1i64),
        };
        return is_plausible_verb(reason)
            .then(|| GtEvent::Evt {
                reason: reason.to_string(),
                cause,
            });
    }
    None
}

// ---------------------------------------------------------------- 全局队列与状态

#[derive(Default)]
struct FeedInner {
    queue: VecDeque<GtEvent>,
    /// 队列满被丢弃的事件数。
    dropped: u64,
    /// 形态非法被吞掉的 `+GT` 行数。
    malformed: u64,
}

static FEED: Mutex<FeedInner> = Mutex::new(FeedInner {
    queue: VecDeque::new(),
    dropped: 0,
    malformed: 0,
});

/// 守护消费的跨轮状态快照（`/api/eif` 暴露）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct GtState {
    /// 累计捕获事件数。
    pub events: u64,
    /// 队列满丢弃数。
    pub dropped: u64,
    /// 畸形 `+GT` 行数。
    pub malformed: u64,
    /// 最近事件动词（空 = 从未收到）。
    pub last_reason: String,
    pub last_cause: i64,
    /// 最近事件时间（unix 秒，0 = 从未收到）。
    pub last_at: u64,
    /// 最近地址事件携带的 V4 快照（仅状态展示，不作为配置来源 —— V4 配置
    /// 主线是拨号巡检的 `AT+CGPADDR` 核对，EIF 只做加速）。
    pub v4: Vec<String>,
    pub v6: Vec<String>,
    /// 触发加速巡检的次数。
    pub refreshes: u64,
}

static STATE: Mutex<GtState> = Mutex::new(GtState {
    events: 0,
    dropped: 0,
    malformed: 0,
    last_reason: String::new(),
    last_cause: 0,
    last_at: 0,
    v4: Vec::new(),
    v6: Vec::new(),
    refreshes: 0,
});

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 把一行已判定的 URC 事件入队并更新状态。
fn push_event(ev: GtEvent) {
    if let Ok(mut g) = FEED.lock() {
        if g.queue.len() >= QUEUE_CAP {
            g.queue.pop_front();
            g.dropped += 1;
        }
        g.queue.push_back(ev.clone());
    }
    if let Ok(mut s) = STATE.lock() {
        s.events += 1;
        s.last_at = unix_now();
        match &ev {
            GtEvent::Nora(k) => {
                s.last_reason = format!("no_ra_{k}");
            }
            GtEvent::IfMtu(_) => s.last_reason = "mtu".into(),
            GtEvent::IfSt { .. } => s.last_reason = "ifst".into(),
            GtEvent::IfAddr { reason, cause, .. } => {
                s.last_reason = reason.clone();
                s.last_cause = *cause;
            }
            GtEvent::Addr4(a) => {
                if !s.v4.contains(a) {
                    s.v4.push(a.clone());
                    if s.v4.len() > 8 {
                        s.v4.remove(0);
                    }
                }
            }
            GtEvent::Addr6(a) => {
                if !s.v6.contains(a) {
                    s.v6.push(a.clone());
                    if s.v6.len() > 8 {
                        s.v6.remove(0);
                    }
                }
            }
            GtEvent::Evt { reason, cause } => {
                s.last_reason = reason.clone();
                s.last_cause = *cause;
            }
        }
    }
}

/// 读取路径统一入口：整行已按 `\n` 切好。
/// 返回 `true` 表示该行已被 URC 层吞掉（不进命令响应数据流）。
pub fn capture_line(line: &str) -> bool {
    if !line.starts_with("+GT") {
        return false;
    }
    match parse_line(line) {
        Some(ev) => {
            push_event(ev);
            true
        }
        None => {
            // 畸形 +GT 行：吞掉并计数，绝不透传
            if let Ok(mut g) = FEED.lock() {
                g.malformed += 1;
            }
            true
        }
    }
}

/// 守护每轮取走全部积压事件。
pub fn take_events() -> Vec<GtEvent> {
    match FEED.lock() {
        Ok(mut g) => g.queue.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

/// 状态快照（含队列侧计数）。
pub fn snapshot() -> GtState {
    let mut s = STATE.lock().map(|g| g.clone()).unwrap_or_default();
    if let Ok(g) = FEED.lock() {
        s.dropped = g.dropped;
        s.malformed = g.malformed;
    }
    s
}

/// 累计加速次数（守护触发加速巡检时调用）。
pub fn note_refresh() {
    if let Ok(mut s) = STATE.lock() {
        s.refreshes += 1;
    }
}

// ---------------------------------------------------------------- 跨 read 行分割器

/// 字节层行分割 + URC 过滤。
///
/// 串口 read 不保证按行返回，URC 可能被切在任意字节边界。所有读取点先把
/// chunk 喂进来：完整行走 [`capture_line`]（命中的不进 `out`），未闭合的
/// 残余留在内部缓冲等下一个 chunk 闭合。
///
/// 残余不主动放行：结果码总是 `\r\n` 结尾，闭合前的残余几乎只会是 URC 分片；
/// 把它留给下一次 read 处理比「到期放行半行进响应」更安全。
#[derive(Default)]
pub struct LineGate {
    buf: Vec<u8>,
}

impl LineGate {
    pub fn feed(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        self.buf.extend_from_slice(chunk);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            // 剥行尾 \n（与可选的 \r）
            let mut end = line.len() - 1;
            if end > 0 && line[end - 1] == b'\r' {
                end -= 1;
            }
            let text = String::from_utf8_lossy(&line[..end]);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                // 空行保持原样穿过（normalize 依赖行结构）
                out.extend_from_slice(&line);
            } else if !capture_line(trimmed) {
                out.extend_from_slice(&line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_documented_forms() {
        assert_eq!(
            parse_line("+GTNORA: initial"),
            Some(GtEvent::Nora("initial"))
        );
        assert_eq!(
            parse_line("+GTNORA: refresh"),
            Some(GtEvent::Nora("refresh"))
        );
        assert_eq!(parse_line("+GTIFMTU: 1500"), Some(GtEvent::IfMtu(1500)));
        assert_eq!(parse_line("+GTIFST: cause=3"), Some(GtEvent::IfSt(3)));
        assert_eq!(
            parse_line("+GTIFADDR: ipadd,cause=0,v4cnt=1,v6cnt=2"),
            Some(GtEvent::IfAddr {
                reason: "ipadd".into(),
                cause: 0,
                v4cnt: 1,
                v6cnt: 2
            })
        );
        assert_eq!(
            parse_line("+GTIFADDR4: 10.22.33.44"),
            Some(GtEvent::Addr4("10.22.33.44".into()))
        );
        assert_eq!(
            parse_line("+GTIFADDR6: 2408:8207:1::5"),
            Some(GtEvent::Addr6("2408:8207:1::5".into()))
        );
        assert_eq!(
            parse_line("+GTIFEVT: ho,cause=7"),
            Some(GtEvent::Evt {
                reason: "ho".into(),
                cause: 7
            })
        );
    }

    #[test]
    fn rejects_injected_or_malformed_lines() {
        // 地址注入：CR/LF 已由行切分天然剥离，这里验证形态门
        assert_eq!(parse_line("+GTIFADDR4: 999.1.1.1"), None);
        assert_eq!(parse_line("+GTIFADDR4: 1.2.3"), None);
        assert_eq!(parse_line("+GTIFADDR4: a.b.c.d"), None);
        assert_eq!(parse_line("+GTIFADDR6: nocolon"), None);
        assert_eq!(parse_line("+GTIFADDR: BadVerb,cause=0,v4cnt=0,v6cnt=0"), None);
        assert_eq!(parse_line("+GTIFMTU: 15x0"), None);
        // 普通 AT 响应绝不能被误吞
        assert_eq!(parse_line("+CSQ: 99,99"), None);
        assert_eq!(parse_line("OK"), None);
    }

    #[test]
    fn capture_line_swallows_gt_and_passes_normal_lines() {
        assert!(capture_line("+GTNORA: initial"));
        assert!(capture_line("+GTRUBBISH")); // 畸形 +GT 也吞
        assert!(!capture_line("+CSQ: 99,99"));
        assert!(!capture_line("OK"));
    }

    #[test]
    fn line_gate_handles_split_urc_across_reads() {
        let mut g = LineGate::default();
        let mut out = Vec::new();
        // URC 行被切成两半，夹在正常响应里
        g.feed(b"+CSQ: 99,99\r\n+GTIFAD", &mut out);
        assert_eq!(String::from_utf8_lossy(&out), "+CSQ: 99,99\r\n");
        out.clear();
        g.feed(b"DR4: 1.2.3.4\r\nOK\r\n", &mut out);
        let s = String::from_utf8_lossy(&out).to_string();
        assert_eq!(s, "OK\r\n"); // URC 已被吞
    }

    #[test]
    fn line_gate_keeps_unterminated_residue() {
        let mut g = LineGate::default();
        let mut out = Vec::new();
        g.feed(b"AT+CGACT?\r", &mut out); // 无 \n 结尾：留在缓冲
        assert!(out.is_empty());
        g.feed(b"\n+CGACT: 1,1\r\nOK\r\n", &mut out);
        let s = String::from_utf8_lossy(&out).to_string();
        assert!(s.contains("+CGACT: 1,1"));
        assert!(s.contains("OK"));
    }

    #[test]
    fn state_snapshot_tracks_events() {
        // 状态是全局的：这里只验证 capture 后快照字段被推进
        let before = snapshot().events;
        assert!(capture_line("+GTIFADDR4: 192.0.2.9"));
        let after = snapshot();
        assert_eq!(after.events, before + 1);
        assert!(after.v4.contains(&"192.0.2.9".to_string()));
    }
}
