//! 配置读写（UCI：`/etc/config/fm350`）。
//!
//! 独立配置体系：本插件只读写 UCI `fm350` 配置文件的 `main` 节。

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PACKAGE: &str = "fm350";
const SECTION: &str = "main";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default = "default_at_port")]
    pub at_port: String,
    #[serde(default = "default_baudrate")]
    pub baudrate: u32,
    #[serde(default = "default_at_timeout")]
    pub at_timeout: u64,
    #[serde(default = "default_apn")]
    pub apn: String,
    #[serde(default = "default_username")]
    pub username: String,
    #[serde(default = "default_password")]
    pub password: String,
    #[serde(default = "default_auth")]
    pub auth: String,
    #[serde(default = "default_pdp_type")]
    pub pdp_type: String,
    #[serde(default = "default_cid")]
    pub cid: u32,
    #[serde(default = "default_iface")]
    pub iface: String,
    #[serde(default = "default_iface_v6")]
    pub iface_v6: String,
    #[serde(default = "default_data_dev")]
    pub data_dev: String,
    #[serde(default = "default_metric")]
    pub metric: u32,
    #[serde(default = "default_ipv6")]
    pub ipv6: bool,
    /// 是否把上行 IPv6 前缀委派给 LAN（对应 netifd dhcpv6 的 `extendprefix`）。
    ///
    /// 默认开启。蜂窝侧通常只下发一个 /64，不置此项时该前缀不会分配给 lan，
    /// 表现为「WAN 有 IPv6、局域网设备没有」。
    #[serde(default = "default_extendprefix")]
    pub extendprefix: bool,
    #[serde(default = "default_auto_dial")]
    pub auto_dial: bool,
    #[serde(default = "default_route_guard")]
    pub route_guard: bool,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// IMEI / 串号写入开关。默认关闭：写入属高风险不可逆操作，
    /// 必须显式开启且调用方二次确认后才允许下发。
    #[serde(default = "default_imei_write")]
    pub imei_write: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_at_port() -> String { "/dev/ttyUSB1".into() }
fn default_baudrate() -> u32 { 115200 }
fn default_at_timeout() -> u64 { 10 }
fn default_apn() -> String { "cmiot5g".into() }
fn default_username() -> String { String::new() }
fn default_password() -> String { String::new() }
fn default_auth() -> String { "none".into() }
fn default_pdp_type() -> String { "IPV4V6".into() }
fn default_cid() -> u32 { 1 }
fn default_iface() -> String { "fm350".into() }
fn default_iface_v6() -> String { "fm350v6".into() }
fn default_data_dev() -> String { "auto".into() }
fn default_metric() -> u32 { 30 }
fn default_ipv6() -> bool { true }
fn default_extendprefix() -> bool { true }
fn default_auto_dial() -> bool { true }
fn default_route_guard() -> bool { true }
fn default_poll_interval() -> u64 { 30 }
fn default_api_port() -> u16 { 8766 }
fn default_imei_write() -> bool { false }
fn default_enabled() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        serde_json::from_str("{}").unwrap()
    }
}

fn uci_get(opt: &str) -> Option<String> {
    let out = Command::new("uci")
        .args(["-q", "get", &format!("{}.{}.{}", PACKAGE, SECTION, opt)])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn uci_set(opt: &str, val: &str) -> Result<(), String> {
    let st = Command::new("uci")
        .args(["set", &format!("{}.{}.{}={}", PACKAGE, SECTION, opt, val)])
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err(format!("uci set {} 失败", opt));
    }
    Ok(())
}

/// 配置缓存：`load()` 每次要 fork 十几次 `uci`，逐请求调用代价过高。
/// 这里做 2 秒 TTL 的进程内缓存，兼顾"改完即生效"与"不拖慢 API"。
///
/// daemon 巡检不走这里（每轮直接 `load()`），保证长时间无 API 访问时
/// 也能感知变更；API 路径走这里，保证改完配置下一次请求就生效。
static CONFIG_CACHE: Mutex<Option<(Instant, Config)>> = Mutex::new(None);

/// 带 2 秒 TTL 的配置读取（供 API 请求路径使用）。
pub fn load_cached() -> Config {
    const TTL: Duration = Duration::from_secs(2);
    if let Ok(g) = CONFIG_CACHE.lock() {
        if let Some((t, c)) = g.as_ref() {
            if t.elapsed() < TTL {
                return c.clone();
            }
        }
    }
    let c = load();
    if let Ok(mut g) = CONFIG_CACHE.lock() {
        *g = Some((Instant::now(), c.clone()));
    }
    c
}

pub fn load() -> Config {
    Config {
        at_port: uci_get("at_port").unwrap_or_else(default_at_port),
        baudrate: uci_get("baudrate")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_baudrate),
        at_timeout: uci_get("at_timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_at_timeout),
        apn: uci_get("apn").unwrap_or_else(default_apn),
        username: uci_get("username").unwrap_or_default(),
        password: uci_get("password").unwrap_or_default(),
        auth: uci_get("auth").unwrap_or_else(default_auth),
        pdp_type: uci_get("pdp_type").unwrap_or_else(default_pdp_type),
        cid: uci_get("cid")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_cid),
        iface: uci_get("iface").unwrap_or_else(default_iface),
        iface_v6: uci_get("iface_v6").unwrap_or_else(default_iface_v6),
        data_dev: uci_get("data_dev").unwrap_or_else(default_data_dev),
        metric: uci_get("metric")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_metric),
        ipv6: uci_get("ipv6").map(|v| v == "1").unwrap_or(true),
        extendprefix: uci_get("extendprefix").map(|v| v == "1").unwrap_or(true),
        auto_dial: uci_get("auto_dial").map(|v| v == "1").unwrap_or(true),
        route_guard: uci_get("route_guard").map(|v| v == "1").unwrap_or(true),
        poll_interval: uci_get("poll_interval")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_poll_interval),
        api_port: uci_get("api_port")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_api_port),
        imei_write: uci_get("imei_write").map(|v| v == "1").unwrap_or(false),
        enabled: uci_get("enabled").map(|v| v == "1").unwrap_or(true),
    }
}

/// 保存配置：仅更新传入的字段，其余保持不变。
pub fn save(patch: &serde_json::Value) -> Result<(), String> {
    let obj = patch
        .as_object()
        .ok_or_else(|| "配置体必须为 JSON 对象".to_string())?;

    // 首次写入时确保 section 存在
    let _ = Command::new("uci")
        .args(["set", &format!("{}.{}={}", PACKAGE, SECTION, PACKAGE)])
        .status();

    for (k, v) in obj.iter() {
        let val = match v {
            serde_json::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => continue,
        };

        // AT 端口必须是绝对设备路径，避免写入 "ttyUSB1" 这类相对值后
        // open() 在 cwd 下静默失败，用户却看到「保存成功」。
        if k == "at_port" && !val.starts_with('/') {
            return Err(format!(
                "AT 端口必须是绝对路径（如 /dev/ttyUSB1），当前为『{}』",
                val
            ));
        }

        uci_set(k, &val)?;
    }
    let st = Command::new("uci")
        .args(["commit", PACKAGE])
        .status()
        .map_err(|e| e.to_string())?;
    if !st.success() {
        return Err("uci commit 失败".to_string());
    }
    Ok(())
}
