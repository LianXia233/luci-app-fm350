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
    /// 已废弃（v1.0.3 起）：原为 netifd dhcpv6 的 extendprefix 选项。
    /// IPv6 已改为守护静态配置（RNDIS 通道无 RA/DHCPv6 可用），本字段仅保留
    /// 以兼容旧配置读取，后端不再消费。
    #[serde(default = "default_extendprefix")]
    pub extendprefix: bool,
    #[serde(default = "default_auto_dial")]
    pub auto_dial: bool,
    #[serde(default = "default_route_guard")]
    pub route_guard: bool,
    /// 网关模式。
    /// - `auto`（默认）：尝试由 IPv4 推导同网段网关并实测可达性，可达则按
    ///   「/24 + 网关」配置；不可达自动回退到无网关的 onlink 设备路由。
    /// - `off`：维持历史行为（/32 + `default dev <dev> onlink`）。
    /// - `static`：直接使用 `gateway` 选项指定的网关。
    ///
    /// 为什么需要它：部分运营商/固件下 RNDIS 通道不做任意 IP 的 ARP 代理，
    /// /32 + onlink 会让主机对每个公网 IP 直接发 ARP 而得不到应答，
    /// 表现为「PDP 已激活、有 IP 有 DNS，但一个包都发不出去」。
    #[serde(default = "default_gateway_mode")]
    pub gateway_mode: String,
    /// 静态网关（`gateway_mode=static` 时必填）；auto 模式探测成功后会回写
    /// 实际使用的网关，便于排查与前端展示。
    #[serde(default)]
    pub gateway: String,
    /// 子网掩码，留空表示由网关模式自行决定（有网关 → /24，无网关 → /32）。
    #[serde(default)]
    pub netmask: String,
    /// 数据面健康检查与自愈：RNDIS 数据端点被打到 stall 时分级恢复。
    #[serde(default = "default_data_guard")]
    pub data_guard: bool,
    /// 连续多少轮判定数据面异常才触发自愈（默认 3 轮）。
    #[serde(default = "default_data_guard_rounds")]
    pub data_guard_rounds: u32,
    /// IPv6 获取方式：
    /// - `ra`：由内核按运营商 RA 自动配置（SLAAC 地址 + `via fe80::` 默认路由），
    ///   插件只负责打开 `accept_ra`。**默认**，实机验证可用的方案。
    /// - `static`：旧的静态方案 —— 把模组侧读到的地址以 /128 写入并补
    ///   无网关的 onlink 默认路由。仅在模组固件不转发 RA 时使用。
    /// - `off`：不托管 IPv6。
    #[serde(default = "default_v6_mode")]
    pub v6_mode: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval: u64,
    /// 从模组 AT/PDP 信息轮询最新 IPv6 的周期。0 表示关闭独立 V6 轮询。
    #[serde(default = "default_v6_poll_interval")]
    pub v6_poll_interval: u64,
    /// IPv6 子接口定时刷新周期。0 表示关闭定时刷新；地址失效兜底刷新不受影响。
    #[serde(default = "default_v6_refresh_interval")]
    pub v6_refresh_interval: u64,
    #[serde(default = "default_api_port")]
    pub api_port: u16,
    /// IMEI / 串号写入开关。默认关闭：写入属高风险不可逆操作，
    /// 必须显式开启且调用方二次确认后才允许下发。
    #[serde(default = "default_imei_write")]
    pub imei_write: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_at_port() -> String {
    "/dev/ttyUSB1".into()
}
fn default_baudrate() -> u32 {
    115200
}
fn default_at_timeout() -> u64 {
    10
}
fn default_apn() -> String {
    "cmiot5g".into()
}
fn default_username() -> String {
    String::new()
}
fn default_password() -> String {
    String::new()
}
fn default_auth() -> String {
    "none".into()
}
fn default_pdp_type() -> String {
    "IPV4V6".into()
}
fn default_cid() -> u32 {
    1
}
fn default_iface() -> String {
    "fm350".into()
}
fn default_iface_v6() -> String {
    "fm350v6".into()
}
fn default_data_dev() -> String {
    "auto".into()
}
fn default_metric() -> u32 {
    30
}
fn default_ipv6() -> bool {
    true
}
fn default_extendprefix() -> bool {
    true
}
fn default_auto_dial() -> bool {
    true
}
fn default_route_guard() -> bool {
    true
}
fn default_gateway_mode() -> String {
    "auto".into()
}
fn default_data_guard() -> bool {
    true
}
fn default_data_guard_rounds() -> u32 {
    3
}
fn default_v6_mode() -> String {
    "ra".into()
}
fn default_poll_interval() -> u64 {
    30
}
fn default_v6_poll_interval() -> u64 {
    300
}
fn default_v6_refresh_interval() -> u64 {
    1800
}
fn default_api_port() -> u16 {
    8766
}
fn default_imei_write() -> bool {
    false
}
fn default_enabled() -> bool {
    true
}

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
        gateway_mode: uci_get("gateway_mode").unwrap_or_else(default_gateway_mode),
        gateway: uci_get("gateway").unwrap_or_default(),
        netmask: uci_get("netmask").unwrap_or_default(),
        data_guard: uci_get("data_guard").map(|v| v == "1").unwrap_or(true),
        data_guard_rounds: uci_get("data_guard_rounds")
            .and_then(|x| x.parse().ok())
            .unwrap_or_else(default_data_guard_rounds),
        v6_mode: uci_get("v6_mode").unwrap_or_else(default_v6_mode),
        poll_interval: uci_get("poll_interval")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_poll_interval),
        v6_poll_interval: uci_get("v6_poll_interval")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_v6_poll_interval),
        v6_refresh_interval: uci_get("v6_refresh_interval")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(default_v6_refresh_interval),
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
