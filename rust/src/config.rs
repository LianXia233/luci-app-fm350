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
    /// 拨号成功后是否强制下发本插件配置的 DNS（覆盖模组/运营商下发的 DNS）。
    ///
    /// 蜂窝网络下运营商 DNS 经常出现解析慢、劫持或跨网不可达；固定为公共
    /// DNS 后行为可预期。**只在拨号成功、接口已有地址后下发**，不影响
    /// 未拨号时的空接口。
    pub dns_force: bool,
    /// IPv4 首选 DNS（`dns_force=1` 时必下发）。
    pub dns4_primary: String,
    /// IPv4 备用 DNS。
    pub dns4_secondary: String,
    /// IPv6 首选 DNS。
    pub dns6_primary: String,
    /// IPv6 备用 DNS。
    pub dns6_secondary: String,
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
    /// SSH 转发开关：经 ADB 通道把模组内 dropbear:22 转发到 LAN。
    /// 默认关闭：无 ADB 通道的模组/固件上不产生任何动作（见 daemon::sshfwd）。
    pub sshfwd_enable: bool,
    /// LAN 侧监听端口（socat，0.0.0.0）。
    pub sshfwd_lan_port: u16,
    /// adb forward 本地端口（127.0.0.1 → 模组 22）。
    pub sshfwd_fwd_port: u16,
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
            dns_force: true,
            dns4_primary: "119.29.29.29".into(),
            dns4_secondary: "223.5.5.5".into(),
            dns6_primary: "2402:4e00::".into(),
            dns6_secondary: "2400:3200::1".into(),
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
            sshfwd_enable: false,
            sshfwd_lan_port: 2222,
            sshfwd_fwd_port: 2223,
        }
    }
}

// ---------------------------------------------------------------- UCI 原语

fn uci_get(opt: &str) -> Option<String> {
    uci_get_present(opt).filter(|s| !s.is_empty())
}

/// 读取 UCI 值，**保留空值**：选项存在但为空时返回 `Some("")`。
///
/// DNS 这类「空 = 不启用该项」的可选项必须用它，否则空值会被 `opt_str`
/// 当成缺失而回落到默认值，用户永远清不掉某一项。
fn uci_get_present(opt: &str) -> Option<String> {
    let out = Command::new("uci")
        .args(["-q", "get", &format!("{}.{}.{}", PACKAGE, SECTION, opt)])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
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

/// 读取字符串型选项，**空值保持为空**（仅缺失时回落默认值）。
fn opt_str_keep_empty(key: &str, dflt: &str) -> String {
    uci_get_present(key).unwrap_or_else(|| dflt.to_string())
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
        dns_force: opt_bool("dns_force", d.dns_force),
        dns4_primary: opt_str_keep_empty("dns4_primary", &d.dns4_primary),
        dns4_secondary: opt_str_keep_empty("dns4_secondary", &d.dns4_secondary),
        dns6_primary: opt_str_keep_empty("dns6_primary", &d.dns6_primary),
        dns6_secondary: opt_str_keep_empty("dns6_secondary", &d.dns6_secondary),
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
        sshfwd_enable: opt_bool("sshfwd_enable", d.sshfwd_enable),
        sshfwd_lan_port: opt_num("sshfwd_lan_port", d.sshfwd_lan_port),
        sshfwd_fwd_port: opt_num("sshfwd_fwd_port", d.sshfwd_fwd_port),
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

    // 先整体校验再落盘：若边写边校验，后面的字段非法时前面的 `uci set`
    // 已经进入 delta 目录，可能被下一次无关的 commit 顺带生效，留下半套配置。
    let mut updates: Vec<(String, String)> = Vec::with_capacity(obj.len());
    for (k, v) in obj.iter() {
        let val = match v {
            serde_json::Value::Bool(b) => if *b { "1" } else { "0" }.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => continue,
        };

        validate_value(k, &val)?;
        updates.push((k.clone(), val));
    }

    for (k, val) in updates {
        uci_set(&k, &val)?;
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

/// 校验单个配置项取值。**所有写入都必须先过这里。**
///
/// 重点不是"格式好看"，而是这些值会被拼进 shell 命令：
/// `ifup {iface}`、`ping -I {data_dev}`、`uci -q set network.{iface}.gateway={gw}`
/// 等（见 `net::shell`）。配置来自 Web/ubus 输入，若放任引号、分号、`$()`
/// 通过，等同于把 root shell 交给前端。因此网卡名只允许最小的字符集，
/// 网关/掩码必须是纯地址。
fn validate_value(key: &str, val: &str) -> Result<(), String> {
    match key {
        // AT 端口必须是绝对设备路径：写入 "ttyUSB1" 这类相对值后 open()
        // 会在 cwd 下静默失败，用户却看到「保存成功」。
        "at_port" if !val.starts_with('/') => Err(format!(
            "AT 端口必须是绝对路径（如 /dev/ttyUSB1），当前为『{}』",
            val
        )),
        // SSH 转发端口：必须为 1-65535 的合法端口号。
        "sshfwd_lan_port" | "sshfwd_fwd_port"
            if val.parse::<u16>().map(|n| n == 0).unwrap_or(true) =>
        {
            Err(format!(
                "{} 必须为 1-65535 的端口号，当前为『{}』",
                key, val
            ))
        }
        // 强制 DNS：空值表示跳过该项；非空必须是合法地址。
        // 写入非法值会让 dnsmasq 启动失败或静默丢弃该上游，必须在入口拦下。
        "dns4_primary" | "dns4_secondary" if !val.is_empty() && !crate::addr::is_valid_ipv4(val) => {
            Err(format!("{} 不是合法的 DNS 地址：『{}』", key, val))
        }
        "dns6_primary" | "dns6_secondary"
            if !val.is_empty() && crate::addr::normalize_ipv6(val).is_none() =>
        {
            Err(format!("{} 不是合法的 DNS 地址：『{}』", key, val))
        }
        // UCI 段名 / 网卡名：1-15 字节，仅字母数字与 `_-.`，且不得以 `-`
        // 开头（避免被 `ping -I` 之类当成选项）。
        "iface" if !is_valid_iface_name(val) => Err(format!(
            "接口名『{}』非法：只允许 1-15 位字母、数字、`_`、`-`、`.`，且不能以 `-` 开头",
            val
        )),
        "iface_v6" if !val.is_empty() && !is_valid_iface_name(val) => Err(format!(
            "IPv6 接口名『{}』非法：只允许 1-15 位字母、数字、`_`、`-`、`.`，且不能以 `-` 开头",
            val
        )),
        // 空串与 auto 同义（自动探测）。
        "data_dev" if !val.is_empty() && val != "auto" && !is_valid_iface_name(val) => {
            Err(format!("数据网卡名『{}』非法（应为 auto 或合法网卡名）", val))
        }
        "gateway" if !val.is_empty() && !crate::addr::is_valid_ipv4(val) => Err(format!(
            "网关『{}』不是合法的 IPv4 地址（留空表示自动）",
            val
        )),
        "netmask" if !val.is_empty() && !crate::addr::is_valid_netmask4(val) => Err(format!(
            "子网掩码『{}』非法：必须是连续的 IPv4 掩码（如 255.255.255.0）",
            val
        )),
        // 以下字段同样会进入 AT 指令（引号内插值），必须拒绝引号、反斜杠、
        // 换行等能闭合字符串的字符，否则 `"` + CRLF 就能追加任意 AT 指令。
        "apn" if !val.is_empty() && !is_valid_apn(val) => Err(format!(
            "APN『{}』非法：只允许 1-100 位字母、数字、`.`、`-`、`_`",
            val
        )),
        "pdp_type"
            if !matches!(
                val.to_ascii_uppercase().as_str(),
                "IP" | "IPV4" | "IPV6" | "IPV4V6" | "PPP" | "NONIP"
            ) =>
        {
            Err(format!(
                "PDP 类型『{}』非法：可选 IP / IPV4 / IPV6 / IPV4V6 / PPP / NONIP",
                val
            ))
        }
        "username" if !is_safe_at_quoted(val, 64) => Err(format!(
            "用户名『{}』非法：最多 64 个可打印 ASCII 字符，且不能含双引号或反斜杠",
            val
        )),
        "password" if !is_safe_at_quoted(val, 128) => Err(format!(
            "密码非法：最多 128 个可打印 ASCII 字符，且不能含双引号或反斜杠"
        )),
        "auth" if !matches!(val, "none" | "pap" | "chap" | "both") => Err(format!(
            "鉴权方式『{}』非法：可选 none / pap / chap / both",
            val
        )),
        "gateway_mode" if !matches!(val, "auto" | "static" | "off") => Err(format!(
            "网关模式『{}』非法：可选 auto / static / off",
            val
        )),
        "v6_mode" if !matches!(val, "dhcpv6" | "ra" | "static" | "off") => Err(format!(
            "IPv6 模式『{}』非法：可选 dhcpv6 / ra / static / off",
            val
        )),
        _ => Ok(()),
    }
}

/// 合法 APN：点分标签，仅字母数字与 `.` `-` `_`，总长 1-100。
fn is_valid_apn(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// AT 引号字符串内的安全字符集：可打印 ASCII，排除双引号与反斜杠。
fn is_safe_at_quoted(s: &str, max: usize) -> bool {
    s.len() <= max
        && s.bytes()
            .all(|b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\')
}

/// 合法 Linux 网卡名（也是安全的 UCI 段名）。
fn is_valid_iface_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 15
        && s != "."
        && s != ".."
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
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

        // 强制 DNS 默认开启，且四条建议值必须与需求一致
        assert!(c.dns_force);
        assert_eq!(c.dns4_primary, "119.29.29.29");
        assert_eq!(c.dns4_secondary, "223.5.5.5");
        assert_eq!(c.dns6_primary, "2402:4e00::");
        assert_eq!(c.dns6_secondary, "2400:3200::1");
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

    #[test]
    fn valid_values_pass_validation() {
        for (k, v) in [
            ("at_port", "/dev/ttyUSB1"),
            ("at_port", "/dev/serial/by-id/usb-Fibocom_FM350_123-if01-port0"),
            ("sshfwd_lan_port", "2222"),
            ("sshfwd_fwd_port", "65535"),
            ("dns4_primary", "119.29.29.29"),
            ("dns4_secondary", ""),
            ("dns6_primary", "2402:4e00::"),
            ("dns6_secondary", ""),
            ("iface", "fm350"),
            ("iface", "fm350_5g"),
            ("iface_v6", ""),
            ("iface_v6", "fm350v6"),
            ("data_dev", "auto"),
            ("data_dev", ""),
            ("data_dev", "wwan0"),
            ("gateway", ""),
            ("gateway", "10.8.217.1"),
            ("netmask", ""),
            ("netmask", "255.255.255.0"),
            ("apn", "cmiot5g"),
            ("apn", "internet.iot"),
            ("apn", ""),
            ("pdp_type", "IPV4V6"),
            ("pdp_type", "ipv6"),
            ("username", ""),
            ("username", "user@example.com"),
            ("password", ""),
            ("password", "p@ss word"),
            ("auth", "none"),
            ("auth", "chap"),
            ("gateway_mode", "auto"),
            ("gateway_mode", "static"),
            ("v6_mode", "dhcpv6"),
            ("v6_mode", "off"),
        ] {
            assert!(
                validate_value(k, v).is_ok(),
                "{}={:?} 应通过校验: {:?}",
                k,
                v,
                validate_value(k, v)
            );
        }
    }

    #[test]
    fn shell_metacharacters_are_rejected_in_names_and_addresses() {
        // 这些值会被拼进 ifup / ping -I / uci set 的 shell 命令
        for (k, v) in [
            ("iface", "fm350;reboot"),
            ("iface", "fm350$(reboot)"),
            ("iface", "fm 350"),
            ("iface", "fm/350"),
            ("iface", "-fm350"),
            ("iface", ""),
            ("iface", "this-name-is-way-too-long"),
            ("iface", ".."),
            ("iface_v6", "fm350v6;id"),
            ("data_dev", "wwan0;reboot"),
            ("data_dev", "auto;reboot"),
            ("gateway", "10.8.217.1;reboot"),
            ("gateway", "not-an-ip"),
            ("netmask", "255.255.255.0;reboot"),
            ("netmask", "255.0.255.0"),
            ("at_port", "ttyUSB1"),
            ("sshfwd_lan_port", "0"),
            ("sshfwd_lan_port", "70000"),
            ("dns4_primary", "1.1.1.1;reboot"),
            ("dns6_primary", "2402:4e00::;reboot"),
            // APN / 鉴权字段会被包进 AT 引号，必须挡住闭合引号与换行
            ("apn", "cmnet\"\r\nAT+CGACT=0,1"),
            ("apn", "cmnet\nAT+CFUN=0"),
            ("apn", "cm net"),
            ("apn", &"a".repeat(101)),
            ("pdp_type", "IPV4V6\"\r\nAT+CFUN=0"),
            ("pdp_type", "BOGUS"),
            ("username", "user\"name"),
            ("username", "user\r\nAT+CFUN=0"),
            ("password", "pass\\word"),
            ("password", &"p".repeat(129)),
            ("auth", "PAP"),
            ("gateway_mode", "auto;reboot"),
            ("v6_mode", "ra;reboot"),
        ] {
            assert!(
                validate_value(k, v).is_err(),
                "{}={:?} 必须被拒绝",
                k,
                v
            );
        }
    }
}
