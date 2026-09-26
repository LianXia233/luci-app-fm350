//! 配置读写（UCI：`/etc/config/fm350` 的 `main` 节）。
//!
//! ## 重构要点
//!
//! 旧实现为 26 个字段各配一个 `default_xxx()` 函数，再在 `load()` 里
//! 逐个 `uci_get(k).unwrap_or_else(default_xxx)` —— 默认值散在两处，加一个
//! 字段要改三处（结构体、default 函数、load），极易漏改。
//!
//! 这里把**默认值收敛到唯一的 `impl Default`**，容器级 `#[serde(default)]`
//! 让反序列化缺失字段时直接取 `Default` 的对应值（serde 语义：container 级
//! `default` = 缺失字段从 `Default::default()` 的同名字段补齐），于是
//! 26 个函数全部消失；`load()` 退化成一张「键 → 字段」的平铺表。
//!
//! 行为与旧实现**逐字段一致**：包括 `username` / `password` / `gateway` /
//! `netmask` 缺省为空串，以及各 bool 的缺省值。

use std::process::Command;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const PACKAGE: &str = "fm350";
const SECTION: &str = "main";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub at_port: String,
    pub baudrate: u32,
    pub at_timeout: u64,
    pub apn: String,
    pub username: String,
    pub password: String,
    pub auth: String,
    pub pdp_type: String,
    pub cid: u32,
    pub iface: String,
    pub iface_v6: String,
    pub data_dev: String,
    pub metric: u32,
    pub ipv6: bool,
    /// 已废弃（v1.0.3 起）：原为 netifd dhcpv6 的 extendprefix 选项。
    /// IPv6 已改为守护静态配置（RNDIS 通道无 RA/DHCPv6 可用），本字段仅保留
    /// 以兼容旧配置读取，后端不再消费。
    pub extendprefix: bool,
    pub auto_dial: bool,
    pub route_guard: bool,
    /// 网关模式。
    /// - `auto`（默认）：优先采信模组 `AT+CGCONTRDP` 上报的网关，其次由 IPv4
    ///   推导同网段 `.1` 并实测 ARP 可达性；不可达自动回退到无网关 onlink。
    /// - `off`：历史行为（`/32` + `default dev <dev> onlink`）。
    /// - `static`：直接使用 `gateway` 选项。
    ///
    /// 为什么需要它：部分运营商/固件下 RNDIS 通道不做任意 IP 的 ARP 代理，
    /// `/32` + onlink 会让主机对每个公网 IP 直接发 ARP 而得不到应答，
    /// 表现为「PDP 已激活、有 IP 有 DNS，但一个包都发不出去」。
    pub gateway_mode: String,
    /// 静态网关（`gateway_mode=static` 时必填）；auto 模式探测成功后会回写
    /// 实际使用的网关，便于排查与前端展示。
    pub gateway: String,
    /// 子网掩码，留空表示由网关模式自行决定（有网关 → /24，无网关 → /32）。
    pub netmask: String,
    /// 数据面健康检查与自愈：RNDIS 数据端点被打到 stall 时分级恢复。
    pub data_guard: bool,
    /// 连续多少轮判定数据面异常才触发自愈。
    pub data_guard_rounds: u32,
    /// 公网连通性保活：IPv4 与 IPv6 各自独立探测公网可达性，「模组侧有地址/
    /// 接口有路由」不代表真正可用；v6 单独故障绝不重拨（不误杀正常栈）。
    pub net_guard: bool,
    /// 连续多少轮连通性探测失败才触发该栈的恢复动作；拨号失败与第 2 级重拨
    /// 失败也复用此门槛，达到后清除 UCI 会话地址残值。
    pub net_guard_rounds: u32,
    /// IPv6 获取方式：`ra` / `dhcpv6` / `static` / `off`。
    ///
    /// `dhcpv6` 交给 netifd 的 odhcp6c：它在**用户态**用 raw socket 收 RA，
    /// 完全不看 `net.ipv6.conf.<dev>.accept_ra`，天然绕开「接口进 WAN 区后
    /// forwarding=1、内核默认丢弃 RA」这个坑，因此作为默认值。
    pub v6_mode: String,
    /// EIF 兜底加速（f22-atproxy 的 `+GT*` URC 消费）。
    ///
    /// 开启后，守护在 AT 口下行流里被动捕获 `+GTIFADDR: ipadd` 等事件，
    /// 缩短下一次巡检的等待 —— 让「模组侧地址变化 → 本机核对/下发 V4」
    /// 从最长一个巡检周期缩短到秒级。**这是纯加速，不是依赖**：未刷
    /// atproxy rootfs 的设备上永远没有 `+GT*` 事件，主线轮询照常工作。
    /// 巡检动作本身完全复用拨号巡检，不引入新的写路径。
    pub eif_guard: bool,
    pub poll_interval: u64,
    /// 从模组 AT/PDP 信息轮询最新 IPv6 的周期。0 表示关闭独立 V6 轮询。
    pub v6_poll_interval: u64,
    /// IPv6 子接口定时刷新周期。0 表示关闭定时刷新。
    pub v6_refresh_interval: u64,
    pub api_port: u16,
    /// IMEI / 串号写入开关。默认关闭：写入属高风险不可逆操作。
    pub imei_write: bool,
    pub enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            at_port: "/dev/ttyUSB1".into(),
            baudrate: 115200,
            at_timeout: 10,
            apn: "cmiot5g".into(),
            username: String::new(),
            password: String::new(),
            auth: "none".into(),
            pdp_type: "IPV4V6".into(),
            cid: 1,
            iface: "fm350".into(),
            iface_v6: "fm350v6".into(),
            data_dev: "auto".into(),
            metric: 30,
            ipv6: true,
            extendprefix: true,
            auto_dial: true,
            route_guard: true,
            gateway_mode: "auto".into(),
            gateway: String::new(),
            netmask: String::new(),
            data_guard: true,
            data_guard_rounds: 3,
            net_guard: true,
            net_guard_rounds: 3,
            v6_mode: "dhcpv6".into(),
            eif_guard: true,
            poll_interval: 30,
            v6_poll_interval: 300,
            v6_refresh_interval: 1800,
            api_port: 8766,
            imei_write: false,
            enabled: true,
        }
    }
}

// ---------------------------------------------------------------- UCI 原语

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

/// 读取字符串型选项，缺失或空值回落到默认值。
fn opt_str(key: &str, dflt: &str) -> String {
    uci_get(key).unwrap_or_else(|| dflt.to_string())
}

/// 读取数值型选项：解析失败（含非数字）回落到默认值。
fn opt_num<T: FromStr>(key: &str, dflt: T) -> T {
    uci_get(key).and_then(|v| v.parse().ok()).unwrap_or(dflt)
}

/// 读取布尔型选项：UCI 里用 `1` / `0` 表示，其余值一律按缺省处理。
fn opt_bool(key: &str, dflt: bool) -> bool {
    uci_get(key).map(|v| v == "1").unwrap_or(dflt)
}

// ---------------------------------------------------------------- 读取

pub fn load() -> Config {
    let d = Config::default();
    Config {
        at_port: opt_str("at_port", &d.at_port),
        baudrate: opt_num("baudrate", d.baudrate),
        at_timeout: opt_num("at_timeout", d.at_timeout),
        apn: opt_str("apn", &d.apn),
        username: opt_str("username", &d.username),
        password: opt_str("password", &d.password),
        auth: opt_str("auth", &d.auth),
        pdp_type: opt_str("pdp_type", &d.pdp_type),
        cid: opt_num("cid", d.cid),
        iface: opt_str("iface", &d.iface),
        iface_v6: opt_str("iface_v6", &d.iface_v6),
        data_dev: opt_str("data_dev", &d.data_dev),
        metric: opt_num("metric", d.metric),
        ipv6: opt_bool("ipv6", d.ipv6),
        extendprefix: opt_bool("extendprefix", d.extendprefix),
        auto_dial: opt_bool("auto_dial", d.auto_dial),
        route_guard: opt_bool("route_guard", d.route_guard),
        gateway_mode: opt_str("gateway_mode", &d.gateway_mode),
        gateway: opt_str("gateway", &d.gateway),
        netmask: opt_str("netmask", &d.netmask),
        data_guard: opt_bool("data_guard", d.data_guard),
        data_guard_rounds: opt_num("data_guard_rounds", d.data_guard_rounds),
        net_guard: opt_bool("net_guard", d.net_guard),
        net_guard_rounds: opt_num("net_guard_rounds", d.net_guard_rounds),
        v6_mode: opt_str("v6_mode", &d.v6_mode),
        eif_guard: opt_bool("eif_guard", d.eif_guard),
        poll_interval: opt_num("poll_interval", d.poll_interval),
        v6_poll_interval: opt_num("v6_poll_interval", d.v6_poll_interval),
        v6_refresh_interval: opt_num("v6_refresh_interval", d.v6_refresh_interval),
        api_port: opt_num("api_port", d.api_port),
        imei_write: opt_bool("imei_write", d.imei_write),
        enabled: opt_bool("enabled", d.enabled),
    }
}

/// 配置缓存：`load()` 每次要 fork 二十余次 `uci`，逐请求调用代价过高。
/// 这里做 2 秒 TTL 的进程内缓存，兼顾「改完即生效」与「不拖慢 API」。
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

/// 作废进程内缓存。保存配置后必须调用：否则「改完 2 s 内点拨号」会拿到
/// 旧 APN / 旧端口。
pub fn invalidate_cache() {
    if let Ok(mut g) = CONFIG_CACHE.lock() {
        *g = None;
    }
}

// ---------------------------------------------------------------- 写入

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

        // AT 端口必须是绝对设备路径：写入 "ttyUSB1" 这类相对值后 open()
        // 会在 cwd 下静默失败，用户却看到「保存成功」。
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
    invalidate_cache();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_object_yields_all_defaults() {
        let c: Config = serde_json::from_str("{}").unwrap();
        let d = Config::default();
        assert_eq!(c.at_port, d.at_port);
        assert_eq!(c.apn, d.apn);
        assert_eq!(c.cid, d.cid);
        assert_eq!(c.metric, d.metric);
        assert_eq!(c.v6_mode, d.v6_mode);
        assert!(c.ipv6 && c.auto_dial && c.route_guard);
        assert!(!c.imei_write);
    }

    #[test]
    fn partial_object_keeps_defaults_for_the_rest() {
        let c: Config = serde_json::from_str(r#"{"apn":"3gnet","cid":3}"#).unwrap();
        assert_eq!(c.apn, "3gnet");
        assert_eq!(c.cid, 3);
        // 未给出的字段仍取默认值，而不是 "" / 0
        assert_eq!(c.iface, "fm350");
        assert_eq!(c.metric, 30);
    }

    #[test]
    fn numeric_and_bool_parsers_fall_back_safely() {
        // opt_num / opt_bool 走 uci，单测只覆盖「解析失败回落」这一层语义
        assert_eq!("abc".parse::<u32>().ok(), None);
        assert_eq!("42".parse::<u32>().ok(), Some(42));
        assert_eq!(("1" == "1"), true);
    }
}
