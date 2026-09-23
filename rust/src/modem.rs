//! 模组状态查询与控制。
//!
//! 命令选型依据：
//!   - 《Fibocom FM350 AT Commands v2.2》标准命令（CSQ / CEREG / COPS / CGDCONT / CGACT / CGPADDR ...）
//!   - 实机验证过的 FM350 扩展命令（AT+GTACT 锁制式频段、AT+EMMCHLCK 锁小区 PCI、
//!     AT+GTCCINFO 邻区、AT+GTSENRDTEMP 温度）
//!
//! 所有 IMEI / 串号写入类指令在 [`crate::at::is_imei_write`] 处拦截，
//! 只读查询（如 AT+EGMREXT=0,7）正常放行。

use std::collections::BTreeMap;

use crate::at::{self, AtHandle, AtPort, AtResult};
use crate::config::Config;

/// 通用 AT 执行：优先走 daemon 持有的独占端口。
fn run(at: &AtHandle, cfg: &Config, cmd: &str) -> AtResult<String> {
    // IMEI / 串号写入类指令的守卫：默认拒绝，需在设置中开启后走专用接口
    crate::imei::guard_transparent(cmd, cfg)?;
    at.with(cfg, |p| p.command(cmd))
}

fn run_list(at: &AtHandle, cfg: &Config, cmds: &[&str]) -> Vec<(String, String)> {
    cmds.iter()
        .map(|c| {
            let r = run(at, cfg, c).unwrap_or_else(|e| format!("ERR: {}", e));
            (c.to_string(), r)
        })
        .collect()
}

/// 取响应中 `+XXX:` 后的字段。
fn f(resp: &str, prefix: &str) -> Vec<String> {
    at::fields(resp, prefix)
}

// ---------------------------------------------------------------- 信息

#[derive(Debug, serde::Serialize)]
pub struct ModemInfo {
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
    pub imei: String,
    pub imsi: String,
    pub iccid: String,
    pub serial: String,
    pub usb_mode: String,
    pub sim_slot: String,
    pub sms_center: String,
}

pub fn info(at: &AtHandle, cfg: &Config) -> ModemInfo {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (key, cmd) in [
        ("manufacturer", "AT+CGMI"),
        ("model", "AT+CGMM"),
        ("firmware", "AT+CGMR"),
        ("imei", "AT+EGMREXT=0,7"),
        ("imsi", "AT+CIMI"),
        ("iccid", "AT+ICCID"),
        ("serial", "AT+CGSN"),
        ("usb_mode", "AT+GTUSBMODE?"),
        ("sim_slot", "AT+GTDUALSIM?"),
        ("sms_center", "AT+CSCA?"),
    ] {
        let raw = run(at, cfg, cmd).unwrap_or_default();
        map.insert(key.to_string(), clean_value(&raw, cmd));
    }

    // 部分固件对 AT+CGSN 无响应，退回 +EGMREXT 读到的串号
    if map["serial"].is_empty() {
        map.insert("serial".into(), map["imei"].clone());
    }

    ModemInfo {
        manufacturer: map.remove("manufacturer").unwrap_or_default(),
        model: map.remove("model").unwrap_or_default(),
        firmware: map.remove("firmware").unwrap_or_default(),
        imei: map.remove("imei").unwrap_or_default(),
        imsi: map.remove("imsi").unwrap_or_default(),
        iccid: map.remove("iccid").unwrap_or_default(),
        serial: map.remove("serial").unwrap_or_default(),
        usb_mode: map.remove("usb_mode").unwrap_or_default(),
        sim_slot: map.remove("sim_slot").unwrap_or_default(),
        sms_center: map.remove("sms_center").unwrap_or_default(),
    }
}

/// 从原始响应中抽取人类可读的值：优先 `+XXX: value` 行，否则取首个非空非结果码行。
fn clean_value(resp: &str, cmd: &str) -> String {
    let prefix = cmd
        .trim()
        .trim_end_matches('?')
        .trim_start_matches("AT")
        .to_string();
    for line in resp.lines() {
        let l = line.trim();
        if l.is_empty() || l == "OK" || l.starts_with("ERROR") {
            continue;
        }
        if let Some(rest) = l.strip_prefix('+') {
            if let Some(pos) = rest.find(':') {
                let val = rest[pos + 1..].trim().trim_matches('"').to_string();
                if !val.is_empty() {
                    return val;
                }
                continue;
            }
        }
        // 无前缀命令（AT+CGMI 等）直接返回该行
        if prefix.is_empty() || !l.starts_with('+') {
            return l.to_string();
        }
    }
    String::new()
}

// ---------------------------------------------------------------- 信号与注册

#[derive(Debug, Default, serde::Serialize)]
pub struct Signal {
    pub csq: Option<u32>,
    pub rssi_dbm: Option<i32>,
    pub ber: Option<u32>,
    pub rat: String,
    pub reg_state: String,
    pub operator: String,
    /// 运营商可读名（本地 MCC/MNC 查表；未收录时为空串，数字码仍保留在 operator）
    pub operator_name: String,
    pub lac: String,
    pub cid: String,
    pub band: String,
    /// 频段带宽档位（GTCCINFO 服务小区第 10 个字段；模组未上报时为空）
    pub bandwidth: String,
    /// band 的来源标记：模组上报 / 由 ARFCN 推算（模组未上报）
    pub band_source: String,
    pub pci: String,
    pub arfcn: String,
    pub rsrp: Option<i32>,
    /// RSRQ / SS-RSRQ，单位 dB，保留 1 位小数
    pub rsrq: Option<f32>,
    /// SINR / SS-SINR，单位 dB，保留 1 位小数
    pub sinr: Option<f32>,
    /// RSRQ 信号质量等级（优/良/中/差），由 `rsrq` 派生，仅用于展示
    pub rsrq_grade: String,
    /// 综合信号得分 0..100，派生值（公式见 `overall_score`）
    pub overall: Option<f64>,
    /// 综合信号等级 0..5，用于信号条
    pub overall_level: u32,
    /// 综合信号文字等级（优/良/中/差）
    pub overall_grade: String,
    /// 参与综合信号计算的指标名，界面据此标注得分构成
    pub overall_used: Vec<String>,
    pub ca: Vec<String>,
    /// 本次信号数值的实际来源，便于排障（如 "AT+CESQ (5G NR)"）
    pub source: String,
}

/// CSQ RSSI 映射（手册：0 = -113 dBm，每档 +2 dBm，99 = 未知）。
fn csq_to_dbm(csq: u32) -> Option<i32> {
    match csq {
        0..=31 => Some(-113 + 2 * csq as i32),
        99 => None,
        _ => None,
    }
}

// ---------------------------------------------------------------- 信号索引换算
//
// 《FIBOCOM FM350 AT Commands》中 CESQ 与 GTCCINFO 的信号量都是**阶梯索引**而非物理值：
// 索引 n（n ≥ 1）对应区间 [base + (n-1)*step, base + n*step)，索引 0 表示「小于 base」。
// 下面统一取区间**下界**作为结果（保守估计，与手册的列举方式一致）。
// 未知值：CESQ / GTCCINFO 用 255 表示，部分字段（如 rxlev、ber）用 99。

/// 索引是否为有效档位。
fn idx_known(idx: i32, max: i32, unknown: &[i32]) -> bool {
    idx >= 0 && idx <= max && !unknown.contains(&idx)
}

/// 阶梯索引 → 物理值下界。
fn idx_lower(idx: i32, base: f32, step: f32) -> f32 {
    if idx <= 0 {
        base - step
    } else {
        base + (idx as f32 - 1.0) * step
    }
}

/// 保留 1 位小数（避免出现 -11.500001 之类的浮点尾数）。
fn round1(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

/// SS-RSRP（NR）：1 = [-156,-155) dBm，125 = [-32,-31)，126 = ≥ -31，255 = 未知。
fn ss_rsrp_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 126, &[255]) {
        return None;
    }
    Some(idx_lower(idx, -156.0, 1.0) as i32)
}

/// SS-RSRQ（NR）：1 = [-43,-42.5) dB，126 = [19.5,20)，255 = 未知。
fn ss_rsrq_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 126, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -43.0, 0.5)))
}

/// SS-SINR（NR）：1 = [-23,-22.5) dB，127 = ≥ 40，255 = 未知。
fn ss_sinr_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 127, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -23.0, 0.5)))
}

/// RSRP（LTE）：1 = [-140,-139) dBm，96 = [-45,-44)，97 = ≥ -44，255 = 未知。
fn lte_rsrp_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 97, &[255]) {
        return None;
    }
    Some(idx_lower(idx, -140.0, 1.0) as i32)
}

/// RSRQ（LTE）：1 = [-19.5,-19) dB，34 = ≥ -3，255 = 未知。
fn lte_rsrq_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 34, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -19.5, 0.5)))
}

/// RSCP（WCDMA）：1 = [-120,-119) dBm，96 = ≥ -25，255 = 未知。
fn rscp_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 96, &[255]) {
        return None;
    }
    Some(idx_lower(idx, -120.0, 1.0) as i32)
}

/// Ec/Io（WCDMA）：1 = [-24,-23.5) dB，49 = ≥ 0，255 = 未知。
fn ecno_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 49, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -24.0, 0.5)))
}

/// rxlev（GERAN）：1 = [-110,-109) dBm，63 = ≥ -48，99 = 未知。
fn rxlev_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 63, &[99, 255]) {
        return None;
    }
    Some(idx_lower(idx, -110.0, 1.0) as i32)
}

/// GTCCINFO 的 `<rat>` 转文字（0 无效 / 2 WCDMA / 4 LTE / 9 NR）。
fn cell_rat_text(rat: &str) -> &'static str {
    match rat {
        "0" => "无网络",
        "2" => "WCDMA (3G)",
        "4" => "LTE (4G)",
        "9" => "5G NR",
        _ => "",
    }
}

/// 注册状态码转文字（3GPP TS 27.007）。
fn reg_text(state: &str) -> &'static str {
    match state {
        "0" => "未注册",
        "1" => "已注册（归属网络）",
        "2" => "搜索中",
        "3" => "注册被拒绝",
        "4" => "未知",
        "5" => "已注册（漫游）",
        _ => "未知",
    }
}

/// 接入技术代码转文字。
fn act_text(act: &str) -> &'static str {
    match act {
        "0" => "GSM",
        "2" => "UTRAN",
        "3" => "GSM/EGPRS",
        "4" => "UTRAN HSDPA",
        "5" => "UTRAN HSUPA",
        "6" => "UTRAN HSDPA/HSUPA",
        "7" => "E-UTRAN (LTE)",
        "8" => "EC-GSM-IoT",
        "9" => "E-UTRAN (NB-S1)",
        "10" => "E-UTRAN (NB-S2)",
        "11" => "NR (5G)",
        "12" => "NG-RAN",
        "13" => "NR (5G) + E-UTRA",
        _ => "未知",
    }
}

/// 由 NR-ARFCN 反推频段号（3GPP TS 38.104 表 5.4.2.1-1 的全局频率栅格 +
/// TS 38.101-1 表 5.4.2.1-1 的各频段频率范围）。
///
/// 之所以需要本函数：FM350 在 NR 制式下 `AT+GTCCINFO?` 的 `<band>` 字段
/// 实测为空（见 README「已知注意事项」），显示 `-` 对排障没有帮助。
/// 返回 `None` 表示落在本地表未覆盖的范围——**不做猜测**，宁可显示空。
fn nr_band_from_arfcn(arfcn: i64) -> Option<&'static str> {
    // FR1 栅格：0 - 3000 MHz 用 5 kHz 步长；3000 - 24250 MHz 用 15 kHz 步长
    // （后者覆盖 n77/n78/n79）。
    let mhz = if (0..=599_999).contains(&arfcn) {
        arfcn as f64 * 0.005
    } else if (600_000..=2_016_666).contains(&arfcn) {
        3000.0 + (arfcn as f64 - 600_000.0) * 0.015
    } else {
        return None;
    };

    Some(match mhz {
        _ if (2110.0..=2170.0).contains(&mhz) => "n1",
        _ if (1805.0..=1880.0).contains(&mhz) => "n3",
        _ if (869.0..=894.0).contains(&mhz) => "n5",
        _ if (925.0..=960.0).contains(&mhz) => "n8",
        _ if (758.0..=803.0).contains(&mhz) => "n28",
        _ if (2010.0..=2025.0).contains(&mhz) => "n34",
        // n38 的下行频段（2570-2620 MHz）整体落在 n41（2496-2690 MHz）内，
        // 仅凭 ARFCN 无法区分，此处显式标注重叠而不是二选一。
        _ if (2570.0..=2620.0).contains(&mhz) => "n38/n41",
        _ if (1880.0..=1920.0).contains(&mhz) => "n39",
        _ if (2300.0..=2400.0).contains(&mhz) => "n40",
        _ if (2496.0..=2690.0).contains(&mhz) => "n41",
        // n77（3300-4200）与 n78（3300-3800）重叠，同样标注而不猜。
        _ if (3300.0..=4200.0).contains(&mhz) => "n77/n78",
        _ if (4400.0..=5000.0).contains(&mhz) => "n79",
        _ => return None,
    })
}

/// 由 E-UTRA EARFCN 反推频段号（3GPP TS 36.101 表 5.7.3-1 的取值区间）。
/// 直接按区间判定，不引入浮点换算，也就不会出现边界误差。
fn lte_band_from_earfcn(earfcn: i64) -> Option<&'static str> {
    Some(match earfcn {
        0..=599 => "B1",
        1200..=1949 => "B3",
        2400..=2649 => "B5",
        3450..=3799 => "B8",
        36200..=36349 => "B34",
        37750..=38249 => "B38",
        38250..=38649 => "B39",
        38650..=39649 => "B40",
        39650..=41589 => "B41",
        _ => return None,
    })
}

/// 判定 NR 频段号是否为 3GPP 已分配编号。
///
/// `<band>` 的编码解出候选频段号后，必须先过这一关：
/// 否则 `5006` 会被解成根本不存在的 `n6`。
/// 未分配编号的依据是 3GPP TS 38.101-1 / 38.101-2，
/// 与 README「三套编码」一节的实机佐证一致 ——
/// `AT+GTACT` 回显的 `101..171` 序列里 106/109/110/111/115/116/121~124/127 全部缺席。
fn nr_band_no_is_valid(n: i32) -> bool {
    // FR1 低段 1..=105 中 3GPP 未分配的编号
    const UNASSIGNED: [i32; 11] = [6, 9, 10, 11, 15, 16, 21, 22, 23, 24, 27];
    if (1..=105).contains(&n) {
        return !UNASSIGNED.contains(&n);
    }
    // FR2 与后续补充频段只按已分配区间放行，不做「任意数字都算合法」的宽松处理
    (256..=269).contains(&n) || (510..=512).contains(&n) || (670..=710).contains(&n)
}

/// 由 `AT+GTCCINFO?` 的 `<band>` 编码反解 NR 频段名。
///
/// 该字段是编码而非人可读名，且存在**两套生成规则**（手册 p126 / p133，
/// 另见 README「三套编码」一节）：
///
/// 1. **数值加法**：`基数 + 频段号`，基数分三档。手册与实机的例子都能对上 ——
///    `141` = 100 + 41 = n41、`101` = 100 + 1 = n1、`171` = 100 + 71 = n71；
///    `501` = 500 + 1 = n1；`5041` = 5000 + 41 = n41。
///    实机 NR 服务小区上报的 `5041` 即属此列。
/// 2. **十进制字符串拼接**：`50` 直接后接频段号。实机 `AT+GTACT` 候选列表里
///    出现过 `50512`，按数值加法无解（5000 + 512 = 5512 ≠ 50512），
///    而 `50` 接 `512` 恰好成立。
///
/// 两套规则在部分取值上同解（`5041` 两种解释都指向 n41），故先试数值加法、
/// 再试字符串拼接；候选频段号一律经过已分配性校验。
/// 解不出即返回 `None` —— 由调用方保留原始码，**不猜**。
fn nr_band_from_code(code: &str) -> Option<String> {
    let c = code.trim();
    if c.is_empty() || c.len() > 6 || !c.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i32 = c.parse().ok()?;

    // 规则 1：数值加法。基数从大到小试，先命中先返回。
    for base in [5000, 500, 100] {
        if v > base && nr_band_no_is_valid(v - base) {
            return Some(format!("n{}", v - base));
        }
    }

    // 规则 2：字符串拼接。前缀按长度降序，避免 `5041` 被 `50` 先行吃掉后误判。
    for prefix in ["500", "100", "50"] {
        if let Some(rest) = c.strip_prefix(prefix) {
            if rest.is_empty() {
                continue;
            }
            if let Ok(n) = rest.parse::<i32>() {
                if nr_band_no_is_valid(n) {
                    return Some(format!("n{}", n));
                }
            }
        }
    }
    None
}

/// 由 `AT+GTCCINFO?` 的 `<band>` 编码反解 LTE 频段名。
///
/// 两套编码（手册 p126 / p133）：
/// - **数值加法**：`100 + 频段号` —— `101` = 100 + 1 = B1 …… `171` = 100 + 71 = B71；
/// - **band 号直写** —— `1` = B1、`3` = B3、`41` = B41。
///
/// 先试 `100 + n`、再按直写解读；两者都要求频段号落在 3GPP 已分配区间
/// （LTE 为 1..=88，其中 16 未分配）。解不出返回 `None`。
fn lte_band_from_code(code: &str) -> Option<String> {
    let c = code.trim();
    if c.is_empty() || c.len() > 6 || !c.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i32 = c.parse().ok()?;
    let accept = |n: i32| (1..=88).contains(&n) && n != 16;

    // 规则 1：`100 + n`
    if v > 100 && accept(v - 100) {
        return Some(format!("B{}", v - 100));
    }
    // 规则 2：band 号直写
    if accept(v) {
        return Some(format!("B{}", v));
    }
    None
}

/// 把服务小区的 `<bandwidth>` 编码换算为可读带宽。
///
/// 同一套数字在两种制式下含义完全不同，必须由 `<rat>` 决定用哪张表：
/// - **NR**（手册 p191）：编码即带宽的 1/5，单位 MHz ——
///   `25` = 5 MHz、`50` = 10 MHz、`100` = 20 MHz、`250` = 50 MHz、
///   `450` = 90 MHz、**`500` = 100 MHz**、`1000` = 200 MHz、`2000` = 400 MHz。
///   实机 NR 服务小区返回 `500`，即 100 MHz。
/// - **LTE**：编码是资源块（RB）数 ——
///   `6` = 1.4 MHz、`15` = 3 MHz、`25` = 5 MHz、`50` = 10 MHz、
///   `75` = 15 MHz、`100` = 20 MHz。
///
/// 编码落在表外返回 `None`，由调用方保留原始值。
fn bandwidth_text(rat: &str, code: &str) -> Option<String> {
    let n: i32 = code.trim().parse().ok()?;
    match rat {
        "9" => {
            // 手册 p191 明确列出的 NR 带宽档位
            const NR_TABLE: [i32; 8] = [25, 50, 100, 250, 450, 500, 1000, 2000];
            if NR_TABLE.contains(&n) {
                Some(format!("{} MHz", n / 5))
            } else {
                None
            }
        }
        "4" => match n {
            6 => Some("1.4 MHz".to_string()),
            15 => Some("3 MHz".to_string()),
            25 => Some("5 MHz".to_string()),
            50 => Some("10 MHz".to_string()),
            75 => Some("15 MHz".to_string()),
            100 => Some("20 MHz".to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// MCC+MNC → 运营商名称。
///
/// 非权威映射：`AT+COPS?` 在 numeric 格式下只回数字码（实测本模组回 "46000"），
/// 这里补一个可读名只为界面友好，原始数字码始终保留在 `operator` 字段。
/// 未收录的编码返回空串，界面照旧显示数字码。
fn operator_name(code: &str) -> &'static str {
    match code {
        "46000" | "46002" | "46004" | "46007" | "46008" => "中国移动",
        "46001" | "46006" | "46009" => "中国联通",
        "46003" | "46005" | "46011" | "46012" => "中国电信",
        "46015" => "中国广电",
        "46020" => "中国铁通",
        _ => "",
    }
}

/// 把一个物理量线性映射到 0..100，超出标称范围即截断。
/// 标称区间取自各指标的可测范围（与 README 的换算表一致）。
fn score_linear(v: f64, lo: f64, hi: f64) -> f64 {
    let s = (v - lo) / (hi - lo) * 100.0;
    if s < 0.0 {
        0.0
    } else if s > 100.0 {
        100.0
    } else {
        s
    }
}

/// RSRQ 信号质量等级。阈值集中在此，便于与 README 核对。
/// 一般不高于 -10 dB 视为优、-10 ~ -15 dB 为良、-15 ~ -19.5 dB 为中。
fn rsrq_grade_text(rsrq: f64) -> &'static str {
    if rsrq >= -10.0 {
        "优"
    } else if rsrq >= -15.0 {
        "良"
    } else if rsrq >= -19.5 {
        "中"
    } else {
        "差"
    }
}

/// 综合信号（SIGNAL）：把 RSRP / RSRQ / SINR 归一化后加权平均。
///
/// 权重：SINR 0.40 > RSRP 0.35 > RSRQ 0.25 —— SINR 最能反映实际可用性。
/// **只对已取到的指标计算**，并按可用权重重新归一化：
/// 缺失指标不会把得分拉低（例如 LTE 下没有 SINR 时仍能得到合理评分）。
/// 第二个返回值是参与计算的指标名，界面据此说明得分构成，
/// 避免把派生得分误读为模组上报值。
fn overall_score(
    rsrp: Option<i32>,
    rsrq: Option<f32>,
    sinr: Option<f32>,
) -> Option<(f64, Vec<&'static str>)> {
    let mut sum = 0.0f64;
    let mut wsum = 0.0f64;
    let mut used: Vec<&'static str> = Vec::new();

    if let Some(v) = rsrp {
        sum += score_linear(v as f64, -140.0, -44.0) * 0.35;
        wsum += 0.35;
        used.push("RSRP");
    }
    if let Some(v) = rsrq {
        sum += score_linear(v as f64, -19.5, -3.0) * 0.25;
        wsum += 0.25;
        used.push("RSRQ");
    }
    if let Some(v) = sinr {
        sum += score_linear(v as f64, -23.0, 30.0) * 0.40;
        wsum += 0.40;
        used.push("SINR");
    }
    if wsum <= 0.0 {
        return None;
    }
    Some((round1_f64(sum / wsum), used))
}

/// 综合信号得分 → 0..5 的信号条格数。
fn overall_level(score: f64) -> u32 {
    if score >= 80.0 {
        5
    } else if score >= 60.0 {
        4
    } else if score >= 40.0 {
        3
    } else if score >= 20.0 {
        2
    } else if score > 0.0 {
        1
    } else {
        0
    }
}

/// 综合信号得分 → 文字等级。
fn overall_grade_text(score: f64) -> &'static str {
    if score >= 75.0 {
        "优"
    } else if score >= 50.0 {
        "良"
    } else if score >= 25.0 {
        "中"
    } else {
        "差"
    }
}

pub fn signal(at: &AtHandle, cfg: &Config) -> Signal {
    let mut s = Signal::default();

    if let Ok(r) = run(at, cfg, "AT+CSQ") {
        let v = f(&r, "+CSQ");
        if let Some(c) = v.first().and_then(|x| x.parse().ok()) {
            s.csq = Some(c);
            s.rssi_dbm = csq_to_dbm(c);
        }
        s.ber = v.get(1).and_then(|x| x.parse().ok());
    }

    // EPS 注册状态（5G/LTE 优先）
    let reg = run(at, cfg, "AT+CEREG?")
        .or_else(|_| run(at, cfg, "AT+CREG?"))
        .unwrap_or_default();
    let rv = f(&reg, "+CEREG");
    let rv = if rv.is_empty() { f(&reg, "+CREG") } else { rv };
    if let Some(st) = rv.get(1).or_else(|| rv.first()) {
        s.reg_state = format!("{} ({})", reg_text(st), st);
    }
    if let Some(act) = rv.get(4) {
        s.rat = act_text(act).to_string();
    }
    if rv.len() > 3 {
        s.lac = rv.get(2).cloned().unwrap_or_default();
        s.cid = rv.get(3).cloned().unwrap_or_default();
    }

    if let Ok(r) = run(at, cfg, "AT+COPS?") {
        let v = f(&r, "+COPS");
        if let Some(op) = v.get(2) {
            s.operator = op.clone();
            s.operator_name = operator_name(&s.operator).to_string();
        }
        if s.rat.is_empty() {
            if let Some(act) = v.get(3) {
                s.rat = act_text(act).to_string();
            }
        }
    }

    // ---- 扩展信号质量（AT+CESQ，3GPP TS 27.007 §8.69）----
    //
    // 本模组上 AT+CSQ 恒为 99,99（不可用），信号必须读 CESQ。
    // 字段顺序：<rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<ss_rsrq>,<ss_rsrp>,<ss_sinr>
    // 实测：+CESQ: 99,99,255,255,255,255,65,75,75（当前为 NR，故前 6 位不可用）
    let cesq: Vec<Option<i32>> = run(at, cfg, "AT+CESQ")
        .map(|r| {
            f(&r, "+CESQ")
                .iter()
                .map(|x| x.parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default();
    let ci = |i: usize| -> Option<i32> { cesq.get(i).copied().flatten() };

    // ---- 服务小区信息（AT+GTCCINFO?，手册 11.1.15）----
    //
    // 响应可能有多行（每行一个小区），首个字段为 <IsServiceCell>：1 = 服务小区，2 = 邻区。
    // <rat> 决定字段布局：2 = WCDMA，4 = LTE/eMTC/NB-IoT，9 = NR。
    // 三种制式的服务小区前 9 个字段位置一致：
    //   <IsServiceCell>,<rat>,<mcc>,<mnc>,<tac|lac>,<cellid>,<arfcn>,<pci|psc>,<band>,...
    let mut cell: Vec<String> = Vec::new();
    let mut cell_rat = String::new();
    if let Ok(r) = run(at, cfg, "AT+GTCCINFO?") {
        for line in r.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with("OK") || l.starts_with("+GTCCINFO") {
                continue;
            }
            let first = l.split(',').next().unwrap_or("").trim();
            if first != "1" {
                continue; // 只看服务小区
            }
            cell = l
                .split(',')
                .map(|x| x.trim().trim_matches('"').to_string())
                .collect();
            cell_rat = cell.get(1).cloned().unwrap_or_default();
            break;
        }
    }
    let cget = |i: usize| -> String { cell.get(i).cloned().unwrap_or_default() };

    if !cell.is_empty() {
        // 手册 11.1.15：三种制式的服务小区前 10 个字段位置一致
        //   <IsServiceCell>,<rat>,<mcc>,<mnc>,<tac|lac>,<cellid>,<arfcn>,<pci|psc>,<band>,<bandwidth>
        //
        // 但后两个字段是**编码值而非人可读名**：实机 NR 服务小区返回
        // `...,504990,128,5041,500,...` —— `5041` 是 `50|41` = n41、`500` 是带宽的 1/5 = 100 MHz。
        // 早期版本把编码原样透出，界面于是显示「频段 5041 / 带宽档位 500」，
        // 看似有值、实则不可读。现按制式解码；编码落在表外时保留原始码并显式标注，
        // 不把未识别的值伪装成频段名或带宽值。
        s.lac = cget(4);
        s.cid = cget(5);
        s.arfcn = cget(6);
        s.pci = cget(7);

        let raw_band = cget(8);
        let raw_bw = cget(9);

        if !raw_band.is_empty() {
            let decoded = match cell_rat.as_str() {
                "9" => nr_band_from_code(&raw_band),
                "4" => lte_band_from_code(&raw_band),
                _ => None,
            };
            match decoded {
                Some(name) => {
                    s.band = name;
                    s.band_source = "模组上报".to_string();
                }
                None => {
                    s.band = raw_band.clone();
                    s.band_source = "模组上报（编码未识别）".to_string();
                }
            }
        }

        if !raw_bw.is_empty() {
            s.bandwidth = bandwidth_text(&cell_rat, &raw_bw).unwrap_or(raw_bw);
        }
    }

    let rat_txt = cell_rat_text(&cell_rat);
    if !rat_txt.is_empty() {
        s.rat = rat_txt.to_string();
    }

    // ---- 按制式选择对应的索引换算 ----
    //
    // NR：ss_rsrq@6 / ss_rsrp@7 / ss_sinr@8
    // LTE：rsrq@4 / rsrp@5（SINR 不在 CESQ 中，取 GTCCINFO 服务小区的 <rssnr_value>，该值直接为 dB）
    // WCDMA：rscp@2 / ecno@3
    match cell_rat.as_str() {
        "9" => {
            s.rsrp = ci(7).and_then(ss_rsrp_dbm);
            s.rsrq = ci(6).and_then(ss_rsrq_db);
            s.sinr = ci(8).and_then(ss_sinr_db);
            if s.rsrp.is_some() {
                s.source = "AT+CESQ (5G NR)".to_string();
            }
        }
        "4" => {
            s.rsrp = ci(5).and_then(lte_rsrp_dbm);
            s.rsrq = ci(4).and_then(lte_rsrq_db);
            // GTCCINFO LTE 服务小区第 10 个字段为 <rssnr_value>，单位 dB（-100..100，255 = 无效）
            s.sinr = cell
                .get(10)
                .and_then(|x| x.parse::<i32>().ok())
                .filter(|v| (-100..=100).contains(v))
                .map(|v| v as f32);
            if s.rsrp.is_some() {
                s.source = "AT+CESQ (LTE)".to_string();
            }
        }
        "2" => {
            s.rsrp = ci(2).and_then(rscp_dbm);
            s.rsrq = ci(3).and_then(ecno_db);
            if s.rsrp.is_some() {
                s.source = "AT+CESQ (WCDMA, 值为 RSCP/Ec-Io)".to_string();
            }
        }
        _ => {
            // 制式未知（未注册 / 无服务小区）：按 LTE 布局尝试一次，
            // 并保留 CSQ 的粗粒度强度作为兜底。
            s.rsrp = ci(5).and_then(lte_rsrp_dbm);
            s.rsrq = ci(4).and_then(lte_rsrq_db);
            if s.rsrp.is_some() {
                s.source = "AT+CESQ (未知制式，按 LTE 解析)".to_string();
            }
        }
    }

    // <band> 字段在部分固件版本 / 部分制式下仍会上报为空。
    // 此时按 ARFCN / EARFCN 反推频段号，并**显式标注**为推算值，
    // 避免把推算结果与模组上报值混为一谈。
    // 只在 NR(9) 与 LTE(4) 下推算：UARFCN 的编号体系不同，不参与。
    if s.band.is_empty() {
        if let Ok(n) = s.arfcn.parse::<i64>() {
            let derived = match cell_rat.as_str() {
                "9" => nr_band_from_arfcn(n),
                "4" => lte_band_from_earfcn(n),
                _ => None,
            };
            if let Some(b) = derived {
                s.band = b.to_string();
                s.band_source = "由 ARFCN 推算（模组未上报）".to_string();
            }
        }
    }

    // RSRQ 质量等级与综合信号——两者都是**派生值**，
    // 由上面的实测指标换算而来，界面会分别标注其构成与来源。
    if let Some(q) = s.rsrq {
        s.rsrq_grade = rsrq_grade_text(q as f64).to_string();
    }
    if let Some((score, used)) = overall_score(s.rsrp, s.rsrq, s.sinr) {
        s.overall = Some(score);
        s.overall_level = overall_level(score);
        s.overall_grade = overall_grade_text(score).to_string();
        s.overall_used = used.iter().map(|x| x.to_string()).collect();
    }

    // CSQ 不可用时用 CESQ 的 rxlev 兜底 RSSI（GERAN 档位）
    if s.rssi_dbm.is_none() {
        s.rssi_dbm = ci(0).and_then(rxlev_dbm);
    }

    // 载波聚合
    if let Ok(r) = run(at, cfg, "AT+GTCAINFO?") {
        s.ca = r
            .lines()
            .filter(|l| l.trim_start().starts_with("+GTCAINFO:"))
            .map(|l| l.trim().to_string())
            .take(8)
            .collect();
    }

    s
}

// ---------------------------------------------------------------- PDP 与拨号

#[derive(Debug, Default, serde::Serialize)]
pub struct PdpState {
    pub cid: u32,
    pub apn: String,
    pub pdp_type: String,
    pub active: bool,
    pub ipv4: String,
    pub ipv6: String,
    pub dns: Vec<String>,
    pub raw: Vec<(String, String)>,
}

// ---------------------------------------------------------------- 地址有效性

/// IPv4 有效性：必须为 4 段点分十进制，且不为 0.0.0.0。
///
/// FM350 在 IPv6 未分配时会回形如 `0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1` 的伪地址，
/// 段数为 16，会被本函数正确判为无效。
fn is_valid_ipv4(s: &str) -> bool {
    if s.is_empty() || s.contains(':') {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let numeric = parts.iter().all(|p| {
        !p.is_empty()
            && p.chars().all(|c| c.is_ascii_digit())
            && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
    });
    numeric && s != "0.0.0.0"
}

/// FM350 特有的 IPv6 表示：**点分十进制**。
///
/// 该模组在 `AT+CGPADDR` / `AT+CGCONTRDP` 中把 IPv6 写成 16 个十进制数，
/// 每个数为一个字节，相邻两数按大端合成一个 16 位组。实机原始响应：
///
/// ```text
/// AT+CGPADDR=0   -> +CGPADDR: 0,"10.7.45.240","0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1"
/// AT+CGCONTRDP=1 -> +CGCONTRDP: 1,,"cmiot5g","","","36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8",...
/// ```
///
/// 解码示例：`36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8` 转为 `2409:8057:2000::8`
/// （`36.9` 得 `0x2409`，`128.87` 得 `0x8057`，`32.0` 得 `0x2000`，其余为零）。
///
/// 占位地址（模组尚未分配 IPv6）返回 `None`：全零，或仅最低字节为 1（即 `::1`）。
/// 必须排除占位，否则会把「未分配」误判成「已激活」，掩盖真实的拨号失败。
fn decode_dotted_ipv6(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 16 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u16 = p.parse().ok()?;
        if n > 255 {
            return None;
        }
        bytes[i] = n as u8;
    }
    if bytes.iter().all(|b| *b == 0) {
        return None;
    }
    if bytes[..15].iter().all(|b| *b == 0) && bytes[15] == 1 {
        return None;
    }
    let groups: Vec<u16> = (0..8)
        .map(|i| ((bytes[i * 2] as u16) << 8) | bytes[i * 2 + 1] as u16)
        .collect();
    Some(format_ipv6_groups(&groups))
}

/// 把 8 个 16 位组格式化为标准 IPv6 文本（含 `::` 压缩）。
fn format_ipv6_groups(g: &[u16]) -> String {
    let (mut bs, mut bl, mut cs, mut cl) = (0usize, 0usize, 0usize, 0usize);
    for (i, v) in g.iter().enumerate() {
        if *v == 0 {
            if cl == 0 {
                cs = i;
            }
            cl += 1;
            if cl > bl {
                bl = cl;
                bs = cs;
            }
        } else {
            cl = 0;
        }
    }
    let hex = |s: &[u16]| {
        s.iter()
            .map(|v| format!("{:x}", v))
            .collect::<Vec<_>>()
            .join(":")
    };
    if bl < 2 {
        return hex(g);
    }
    let (head, tail) = (hex(&g[..bs]), hex(&g[bs + bl..]));
    match (head.is_empty(), tail.is_empty()) {
        (true, true) => "::".to_string(),
        (true, false) => format!("::{}", tail),
        (false, true) => format!("{}::", head),
        (false, false) => format!("{}::{}", head, tail),
    }
}

/// 把 AT 上报的 IPv6 归一化为标准冒号表示；非法或占位返回 `None`。
///
/// 两条通路：标准冒号形式直接采信；FM350 的点分十进制形式先解码。
/// 返回值统一为标准写法，前端可直接展示，不必再感知模组方言。
pub fn normalize_ipv6(s: &str) -> Option<String> {
    if s.contains(':') {
        if s.is_empty() {
            return None;
        }
        let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.is_empty() || hex.chars().all(|c| c == '0') {
            return None;
        }
        Some(s.to_string())
    } else {
        decode_dotted_ipv6(s)
    }
}

/// IPv6 有效性：标准冒号形式（非全零），或 FM350 的点分十进制形式（非占位）。
///
/// 生产路径直接消费 `normalize_ipv6` 的解码结果（需要拿到标准写法而非布尔值），
/// 这里保留一个布尔入口供断言使用，让测试能直白表达「有效 / 无效」。
#[cfg(test)]
fn is_valid_ipv6(s: &str) -> bool {
    normalize_ipv6(s).is_some()
}

/// 解析 `AT+CGACT?` 响应中指定 cid 的激活状态。
///
/// 返回 `None` 表示模组没回任何 `+CGACT:` 行（该命令在此模组上不可用，
/// 调用方需回落到地址类启发式判据）；返回 `Some(false)` 表示模组列出了上下文
/// 明细，但目标 cid 不存在或状态为 0 —— 这是**权威的未激活**结论。
pub fn cgact_state(resp: &str, cid: u32) -> Option<bool> {
    let mut seen = false;
    let mut active = false;
    for line in resp.lines() {
        let l = line.trim();
        if !l.starts_with("+CGACT:") {
            continue;
        }
        seen = true;
        let rest = l.trim_start_matches("+CGACT:").trim();
        let v: Vec<String> = rest.split(',').map(|x| x.trim().to_string()).collect();
        if v.first().and_then(|x| x.parse::<u32>().ok()) == Some(cid) {
            active = v.get(1).map(|x| x == "1").unwrap_or(false);
        }
    }
    if !seen {
        None
    } else {
        Some(active)
    }
}

/// 当前 PDP 上下文状态。
///
/// 激活判据（`AT+CGACT?` 权威）：
///   * 模组回了 `+CGACT:` 行 —— 一律以该行状态为准。
///   * 模组只回裸 `OK`（无 `+CGACT:` 行）—— 回落到
///     `AT+CGCONTRDP` / `AT+CGPADDR` 的地址类启发式判据。
///
/// 实机教训（FM350-GL，APN cmiot5g）：上下文**未激活**时，`AT+CGCONTRDP`
/// 与 `AT+CGPADDR` 仍会原样返回上一轮会话的残留地址（执行 `AT+CGACT=1,0`
/// 后二者返回值毫无变化，而 `AT+CGACT?` 明确不再列出该 cid）。此前把它们
/// 当作激活判据，导致 `dial()` 在「已激活且已有地址」处短路返回、永不下发
/// `AT+CGACT=1,<cid>`，接口一直写着死地址：ARP 能通（模组代理应答）但三层
/// 零回包、`ip -s link` 里 rx_packets 冻结。
///
/// 因此二者现在只负责提供地址与 DNS，不再单独决定 `active`。
pub fn pdp(at: &AtHandle, cfg: &Config) -> PdpState {
    let mut st = PdpState {
        cid: cfg.cid,
        ..Default::default()
    };

    if let Ok(r) = run(at, cfg, "AT+CGDCONT?") {
        for line in r.lines() {
            let l = line.trim();
            if !l.starts_with("+CGDCONT:") {
                continue;
            }
            let rest = l.trim_start_matches("+CGDCONT:").trim();
            let v: Vec<String> = rest
                .split(',')
                .map(|x| x.trim().trim_matches('"').to_string())
                .collect();
            if v.first().and_then(|x| x.parse::<u32>().ok()) == Some(cfg.cid) {
                st.pdp_type = v.get(1).cloned().unwrap_or_default();
                st.apn = v.get(2).cloned().unwrap_or_default();
            }
        }
    }
    if st.apn.is_empty() {
        st.apn = cfg.apn.clone();
    }
    if st.pdp_type.is_empty() {
        st.pdp_type = cfg.pdp_type.clone();
    }

    let mut contr_dns: Vec<String> = Vec::new();

    // 判据 0（权威）：AT+CGACT?
    //
    // 只要模组列出了上下文明细，就以它的状态为唯一激活结论；未列出本 cid
    // 或状态为 0 即判定未激活。
    let cgact = run(at, cfg, "AT+CGACT?")
        .ok()
        .and_then(|r| cgact_state(&r, cfg.cid));

    // CGCONTRDP：只提供地址与 DNS 备用值，不再单独作为激活判据。
    let mut contr_present = false;
    let mut contr_ipv4 = String::new();
    let mut contr_ipv6 = String::new();
    if let Ok(r) = run(at, cfg, &format!("AT+CGCONTRDP={}", cfg.cid)) {
        for line in r.lines() {
            let l = line.trim();
            if !l.starts_with("+CGCONTRDP:") {
                continue;
            }
            contr_present = true;
            let rest = l.trim_start_matches("+CGCONTRDP:").trim();
            // <cid>,<bearer>,"<apn>","<PDP_addr>","<gw>","<dns1>","<dns2>",...
            let v: Vec<String> = rest
                .split(',')
                .map(|x| x.trim().trim_matches('"').to_string())
                .collect();
            if contr_ipv4.is_empty() {
                if let Some(x) = v.get(3) {
                    if is_valid_ipv4(x) {
                        contr_ipv4 = x.clone();
                    }
                }
            }
            for item in v.iter().skip(5).take(2) {
                if is_valid_ipv4(item) {
                    contr_dns.push(item.clone());
                } else if contr_ipv6.is_empty() {
                    // 同一字段位在 IPV4V6 下按地址族切换：实机 cid=1 的
                    // <primary>/<secondary> 位给的就是点分十进制 IPv6。
                    if let Some(a6) = normalize_ipv6(item) {
                        contr_ipv6 = a6;
                    }
                }
            }
        }
    }

    // CGPADDR：地址的权威来源（本模组最可靠）。
    //
    // 同时记录「IPv6 字段位是否出现」：FM350 在网络未下发 IPv6 时把该位写成
    // `0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1`（::1 占位）。只要该位出现过，就以它
    // 为准 —— 是占位就判定「无 IPv6」，绝不回落到 CGCONTRDP 的残留值，
    // 否则会拿陈旧地址配出一条永远不通的 v6 默认路由。
    let mut addr_ipv4 = String::new();
    let mut addr_ipv6 = String::new();
    let mut v6_slot_seen = false;
    if let Ok(r) = run(at, cfg, &format!("AT+CGPADDR={}", cfg.cid)) {
        // +CGPADDR: <cid>,<PDP_addr>[,<PDP_addr6>]
        let v = f(&r, "+CGPADDR");
        for item in v.iter().skip(1) {
            let looks_v6 = item.contains(':') || item.split('.').count() == 16;
            if looks_v6 {
                v6_slot_seen = true;
            }
            // 存归一化后的标准写法：FM350 的点分十进制形态在此被解码，
            // 前端拿到的是可读 IPv6，而不是 16 段十进制原始串。
            if let Some(a6) = normalize_ipv6(item) {
                if addr_ipv6.is_empty() {
                    addr_ipv6 = a6;
                }
            } else if is_valid_ipv4(item) {
                if addr_ipv4.is_empty() {
                    addr_ipv4 = item.clone();
                }
            }
        }
        if addr_ipv4.is_empty() {
            if let Some(x) = at::first_ipv4(&r) {
                if is_valid_ipv4(&x) {
                    addr_ipv4 = x;
                }
            }
        }
    }

    // 激活结论：CGACT? 可用时以它为准，否则回落到地址类启发式。
    let active = cgact.unwrap_or(contr_present || !addr_ipv4.is_empty() || !addr_ipv6.is_empty());

    // 未激活时的地址是上一轮会话的残留，一律不采信，避免上层把死地址写进接口。
    if active {
        st.ipv4 = if !addr_ipv4.is_empty() {
            addr_ipv4
        } else {
            contr_ipv4
        };
        st.ipv6 = addr_ipv6;
        if st.ipv6.is_empty() && !v6_slot_seen {
            // CGPADDR 没给过 IPv6 位（老固件）时才用 CGCONTRDP 兜底。
            st.ipv6 = contr_ipv6;
        }
    }
    st.active = active;

    // DNS：优先 AT+GTDNS，其次 CGCONTRDP 自带
    if st.active {
        if let Ok(r) = run(at, cfg, &format!("AT+GTDNS={}", cfg.cid)) {
            st.dns = f(&r, "+GTDNS")
                .into_iter()
                .skip(1)
                .filter(|x| is_valid_ipv4(x))
                .collect();
        }
        if st.dns.is_empty() {
            st.dns = contr_dns;
        }
    }

    st.raw = run_list(
        at,
        cfg,
        &[
            "AT+CGDCONT?",
            "AT+CGACT?",
            &format!("AT+CGCONTRDP={}", cfg.cid),
            &format!("AT+CGPADDR={}", cfg.cid),
        ],
    );
    st
}

#[cfg(test)]
mod cgact_tests {
    use super::cgact_state;

    #[test]
    fn cid_absent_means_inactive() {
        // 实机故障现场：只有 cid 0 激活，插件用的 cid 1 根本不在列表里
        assert_eq!(cgact_state("+CGACT: 0,1\nOK", 1), Some(false));
    }

    #[test]
    fn cid_present_state_parsed() {
        assert_eq!(cgact_state("+CGACT: 0,1\n+CGACT: 1,1\nOK", 1), Some(true));
        assert_eq!(cgact_state("+CGACT: 1,0\nOK", 1), Some(false));
        assert_eq!(cgact_state("+CGACT: 0,1\nOK", 0), Some(true));
    }

    #[test]
    fn bare_ok_is_unknown() {
        // 部分 FM350 固件只回裸 OK，调用方需回落到地址类判据
        assert_eq!(cgact_state("OK", 1), None);
        assert_eq!(cgact_state("", 1), None);
        assert_eq!(cgact_state("+CME ERROR: unknown", 1), None);
    }
}

/// 写入 APN（`AT+CGDCONT=<cid>,"<type>","<apn>"`）。
pub fn set_apn(at: &AtHandle, cfg: &Config, apn: &str, pdp_type: &str) -> AtResult<String> {
    if apn.is_empty() {
        return Err("APN 不能为空".to_string());
    }
    let cmd = format!(r#"AT+CGDCONT={},"{}","{}""#, cfg.cid, pdp_type, apn);
    run(at, cfg, &cmd)
}

/// 拨号：确保 APN 正确 → 激活 PDP → 读取地址。
///
/// 幂等：上下文已激活且已取得地址时直接返回，不重复下发 `AT+CGACT=1`。
/// 若模组回 `+CME ERROR: 5847`（重复激活 PDP）等激活类错误，
/// 不再直接判失败，而是重新读取上下文状态：只要拿到有效地址即视为成功。
pub fn dial(at: &AtHandle, cfg: &Config) -> AtResult<PdpState> {
    let _ = set_apn(at, cfg, &cfg.apn, &cfg.pdp_type);

    // 已激活且已取得地址时直接返回，避免重复激活（模组会回 +CME ERROR: 5847）
    let before = pdp(at, cfg);
    if before.active && !before.ipv4.is_empty() {
        return Ok(before);
    }

    let r = run(at, cfg, &format!("AT+CGACT=1,{}", cfg.cid))?;
    if r.contains("ERROR") {
        // 重复激活（+CME ERROR: 5847）等：以「是否取到地址」为准，轮询若干次
        let mut st = pdp(at, cfg);
        for _ in 0..5 {
            if !st.ipv4.is_empty() {
                return Ok(st);
            }
            std::thread::sleep(std::time::Duration::from_millis(600));
            st = pdp(at, cfg);
        }
        if st.active {
            return Ok(st);
        }
        return Err(format!("激活 PDP 失败: {}", r.replace('\n', " ")));
    }

    // PDP 激活后地址可能晚一步就绪，轮询几次
    let mut st = pdp(at, cfg);
    for _ in 0..5 {
        if !st.ipv4.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
        st = pdp(at, cfg);
    }
    if !st.active && st.ipv4.is_empty() {
        return Err("PDP 激活后未取得 IPv4 地址".to_string());
    }
    Ok(st)
}

/// 断开：去激活 PDP。
pub fn hangup(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(at, cfg, &format!("AT+CGACT=0,{}", cfg.cid))
}

// ---------------------------------------------------------------- 网络偏好 / 锁频 / 锁小区

/// 锁制式与频段。
///
/// FM350 官方命令（手册 11.1.14）：
/// `AT+GTACT=[<rat>[,[<PreferredAct1>],[<PreferredAct2>][,<band_1>[,<band_2>[,...]]]]]`
///
/// `<rat>` 取值（手册 p125）：1 = UMTS，2 = LTE，4 = LTE/UMTS，**10 = Automatic**，
/// 14 = NR-RAN，16 = NR-RAN/WCDMA，17 = NR-RAN/LTE，20 = NR-RAN/WCDMA/LTE。
/// **注意：20 并非「自动」，20 是三模全开；10 才是 Automatic。**
/// 手册 Note 6 说明：下发 10（自动）之后查询会回显 20，两者极易混淆。
///
/// `<PreferredAct1>` / `<PreferredAct2>`：2 = WCDMA 优先，3 = LTE 优先，6 = NR-RAN 优先；
/// 仅在三模（`<rat>` = 20）下第二个参数才有效。
///
/// 频段编码（手册 p126 / p133）：LTE 为 `100 + n`（101 = B1 … 171 = B71）；
/// NR 为 `"50"` 与 band 号十进制拼接（501 = n1 … 5041 = n41 … 50512 = n512）；
/// 0 = 自动选择全部支持频段。
///
/// 例：`AT+GTACT=20,6,3,5078` = 三模 + NR 优先于 LTE + 锁 n78；
/// `AT+GTACT=14,,,5041` = 仅 NR + 锁 n41（空参数跳过两个 PreferredAct）。
/// 先置 `AT+CFUN=0`（离线）再下发，最后 `AT+CFUN=1` 恢复。
pub fn lock_band(at: &AtHandle, cfg: &Config, args: &[String]) -> AtResult<Vec<(String, String)>> {
    if args.is_empty() {
        return Err("缺少 AT+GTACT 参数，例如 14 / 2 / 20 / 20,6,3,5078".to_string());
    }
    let param = args.join(",");
    let mut out = Vec::new();
    out.push(("AT+CFUN=0".into(), run(at, cfg, "AT+CFUN=0").unwrap_or_default()));
    out.push((
        format!("AT+GTACT={}", param),
        run(at, cfg, &format!("AT+GTACT={}", param))?,
    ));
    out.push(("AT+CFUN=1".into(), run(at, cfg, "AT+CFUN=1").unwrap_or_default()));
    Ok(out)
}

/// 锁小区 / PCI。
///
/// FM350 实机命令：`AT+EMMCHLCK=1,<...>,<arfcn>,<pci>,<...>`
/// 取消：`AT+EMMCHLCK=0`
pub fn lock_cell(at: &AtHandle, cfg: &Config, args: &[String]) -> AtResult<Vec<(String, String)>> {
    if args.is_empty() {
        return Err("缺少 AT+EMMCHLCK 参数，例如 1,11,0,627264,280,3 或 0（取消）".to_string());
    }
    let param = args.join(",");
    let mut out = Vec::new();
    out.push(("AT+CFUN=0".into(), run(at, cfg, "AT+CFUN=0").unwrap_or_default()));
    out.push((
        format!("AT+EMMCHLCK={}", param),
        run(at, cfg, &format!("AT+EMMCHLCK={}", param))?,
    ));
    out.push(("AT+CFUN=1".into(), run(at, cfg, "AT+CFUN=1").unwrap_or_default()));
    Ok(out)
}

/// 查询当前锁定状态。
pub fn lock_status(at: &AtHandle, cfg: &Config) -> Vec<(String, String)> {
    run_list(at, cfg, &["AT+GTACT?", "AT+EMMCHLCK?"])
}

/// 邻区 / 服务小区信息。
pub fn cell_info(at: &AtHandle, cfg: &Config) -> Vec<(String, String)> {
    run_list(at, cfg, &["AT+GTCCINFO?", "AT+GTCAINFO?"])
}

/// 制式名 / 编码 → `+EPRATL` 的 `<rat>` 编码（手册 11.1.13）。
///
/// 2 = UMTS，4 = LTE，128 = NR。同时接受已是编码的数字字符串。
/// 注意这里的编码**与 `+GTACT` 的 `<rat>` 不同**：`+GTACT` 用 1/2/4/10/14…，
/// `+EPRATL` 只用 2/4/128。
fn rat_code(item: &str) -> Option<&'static str> {
    match item.trim().to_ascii_uppercase().as_str() {
        "UMTS" | "WCDMA" | "3G" | "2" => Some("2"),
        "LTE" | "4G" | "4" => Some("4"),
        "NR" | "5G" | "128" => Some("128"),
        _ => None,
    }
}

/// 读取当前制式优先顺序（`AT+EPRATL?`）。
///
/// 手册 11.1.13 只描述了写形式 `AT+EPRATL=<num>,<rat…>`，未列出 `?` 读形式；
/// 但 2026-09-22 实机实测（FM350-GL，Revision 81600.0000.00.29.24.02）
/// `AT+EPRATL?` 可正常返回 `+EPRATL:<num>,<rat…>` —— 本次回读 `+EPRATL:2,128,4`，
/// 即「2 个优先制式，NR(128) 优先于 LTE(4)」。
/// 故读通路与 `set_rat_order()` 的写通路保持对称，同用 `+EPRATL`。
///
/// 与 `lock_status()` 的分工：`AT+GTACT?` 返回的是**当前选定制式与频段锁定**
/// （实测 `+GTACT: 20,6,3,1,2,4,5,8,101,…`，首字段为 `<rat>`、其余为频段列表），
/// 语义是「锁网锁频配置」，不是优先顺序列表，不能代替本函数。
pub fn rat_order(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(at, cfg, "AT+EPRATL?")
}

/// 设置制式优先顺序（FM350 官方命令 `AT+EPRATL`，手册 11.1.13）。
///
/// 手册语法：`AT+EPRATL=<RAT num>,[<rat1>,<rat2>…]`
///   - `<RAT num>`：0 = 不设优先；1-4 = 后续优先制式的个数；
///   - `<rat>`：2 = UMTS，4 = LTE，128 = NR；越靠前优先级越高。
///
/// 历史修正：本函数原使用 `AT+QNWPREFCFG`（`"rat_acq_order"` / `"mode_pref"`），
/// 该命令属**高通 / Quectel 平台**专有，FM350-GL（联发科 T700）不支持，
/// 属跨平台误植；两版官方手册（v2.2 / V2.10）全文均未收录该命令。
pub fn set_rat_order(at: &AtHandle, cfg: &Config, order: &[String]) -> AtResult<String> {
    let mut codes: Vec<&'static str> = Vec::new();
    for item in order {
        if let Some(code) = rat_code(item) {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
    }
    if codes.is_empty() {
        return Err("缺少有效制式：可填 UMTS / LTE / NR（或编码 2 / 4 / 128）".to_string());
    }
    if codes.len() > 4 {
        return Err("优先制式最多 4 个".to_string());
    }
    run(
        at,
        cfg,
        &format!("AT+EPRATL={},{}", codes.len(), codes.join(",")),
    )
}

/// 切换 SIM 卡槽（`AT+GTDUALSIM=<0|1>`）。
pub fn set_sim_slot(at: &AtHandle, cfg: &Config, slot: u32) -> AtResult<String> {
    run(at, cfg, &format!("AT+GTDUALSIM={}", slot))
}

/// 模组温度（手册 18.3 `+GTSENRDTEMP`）。
///
/// 注意命令形式：**读形式 `AT+GTSENRDTEMP?` 在本模组上返回 `+CME ERROR: phone failure`**，
/// 必须使用带参数的写形式 `AT+GTSENRDTEMP=<sensor_id>`；`<sensor_id> = 0` 表示
/// 一次返回全部 23 路传感器。返回值单位为 0.001 ℃
/// （实测 `+GTSENRDTEMP: 10,46066` → 46.066 ℃）。
///
/// 传感器编号（手册 18.3 表）：1 = soc_max，2-4 = cpu_little0-2，7 = gpu1，8 = dramc，
/// 9 = mmsys，10 = md_5g（5G 基带），13 = soc_dram_ntc，14 = ltepa_ntc，
/// 15 = nrpa_ntc，16 = rf_ntc，19-22 = pmic 系列。读数为 0 表示该路未装配。
#[derive(Debug, Default, serde::Serialize)]
pub struct Temperature {
    /// 5G 基带（md_5g，传感器 10）温度，℃。
    ///
    /// 用 f64 而非 f32：读数形如 47.2 ℃ 在 f32 下无法精确表示，
    /// `serde_json` 会序列化成 `47.20000076293945`（实机已复现）。
    /// f64 的最短往返表示即为 `47.2`。
    pub modem: Option<f64>,
    /// 全片最高温（soc_max，传感器 1），℃
    pub soc_max: Option<f64>,
    /// 所有有效读数的最高温，℃
    pub peak: Option<f64>,
    /// 全部有效读数：(传感器编号, ℃)
    pub sensors: Vec<(u32, f64)>,
}

/// 传感器编号 → 名称（手册 18.3 表；未列出的编号返回空串）。
/// 当前仅测试引用；保留供后续前端 API 扩展使用。
#[allow(dead_code)]
pub fn sensor_name(id: u32) -> &'static str {
    match id {
        1 => "soc_max",
        2 => "cpu_little0",
        3 => "cpu_little1",
        4 => "cpu_little2",
        7 => "gpu1",
        8 => "dramc",
        9 => "mmsys",
        10 => "md_5g",
        13 => "soc_dram_ntc",
        14 => "ltepa_ntc",
        15 => "nrpa_ntc",
        16 => "rf_ntc",
        19 => "pmic",
        20 => "pmic_vcore",
        21 => "pmic_vproc",
        22 => "pmic_vgpu",
        _ => "",
    }
}

/// 保留 1 位小数（f64 版本，供温度使用；信号换算仍用 f32 的 `round1`）。
fn round1_f64(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

pub fn temperature(at: &AtHandle, cfg: &Config) -> Option<Temperature> {
    let r = run(at, cfg, "AT+GTSENRDTEMP=0").ok()?;

    let mut sensors: Vec<(u32, f64)> = Vec::new();
    for line in r.lines() {
        let l = line.trim();
        let rest = match l.strip_prefix("+GTSENRDTEMP") {
            Some(x) => x.trim_start_matches(':').trim(),
            None => continue,
        };
        let mut it = rest.split(',');
        let id = it.next().and_then(|x| x.trim().parse::<u32>().ok());
        let raw = it.next().and_then(|x| x.trim().parse::<i32>().ok());
        if let (Some(id), Some(raw)) = (id, raw) {
            if raw != 0 {
                // 0 表示该路未装配，直接跳过
                sensors.push((id, round1_f64(raw as f64 / 1000.0)));
            }
        }
    }
    if sensors.is_empty() {
        return None;
    }

    let get = |id: u32| sensors.iter().find(|(i, _)| *i == id).map(|(_, v)| *v);
    let peak = sensors
        .iter()
        .map(|(_, v)| *v)
        .fold(None, |acc: Option<f64>, v| match acc {
            Some(x) if x >= v => Some(x),
            _ => Some(v),
        });

    Some(Temperature {
        modem: get(10),
        soc_max: get(1),
        peak,
        sensors,
    })
}

/// 重启模组（`AT+CFUN=1,1`）。
pub fn reboot(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(at, cfg, "AT+CFUN=1,1")
}

/// 在线/飞行模式（`AT+CFUN=<0|1>`）。
pub fn set_cfun(at: &AtHandle, cfg: &Config, mode: u32) -> AtResult<String> {
    run(at, cfg, &format!("AT+CFUN={}", mode))
}

/// 设置 USB 模式（FM350 常用 40 = RNDIS+AT）。
pub fn set_usb_mode(at: &AtHandle, cfg: &Config, mode: u32) -> AtResult<String> {
    run(at, cfg, &format!("AT+GTUSBMODE={}", mode))
}

// ---------------------------------------------------------------- 综合状态

#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub info: ModemInfo,
    pub signal: Signal,
    pub pdp: PdpState,
    pub temperature: Option<Temperature>,
    pub at_port: String,
    pub at_ready: bool,
}

pub fn status(at: &AtHandle, cfg: &Config) -> Status {
    let ready = at.with(cfg, |p: &mut AtPort| p.command("AT")).is_ok();
    Status {
        info: info(at, cfg),
        signal: signal(at, cfg),
        pdp: pdp(at, cfg),
        temperature: temperature(at, cfg),
        at_port: cfg.at_port.clone(),
        at_ready: ready,
    }
}

/// 短信中心号码写入。
pub fn set_sms_center(at: &AtHandle, cfg: &Config, number: &str) -> AtResult<String> {
    let international = number.starts_with('+') || number.starts_with("86");
    let normalized = if number.starts_with('+') {
        number.to_string()
    } else if number.starts_with("86") {
        format!("+{}", number)
    } else {
        number.to_string()
    };
    run(
        at,
        cfg,
        &format!(
            r#"AT+CSCA="{}",{}"#,
            normalized,
            if international { 145 } else { 161 }
        ),
    )
}

#[cfg(test)]
mod is_valid_tests {
    use super::{is_valid_ipv4, is_valid_ipv6, normalize_ipv6};

    #[test]
    fn ipv4_accepts_real_fm350_address() {
        // 实机 AT+CGPADDR=1 返回的真实地址
        assert!(is_valid_ipv4("10.5.23.212"));
        assert!(is_valid_ipv4("221.179.38.7"));
    }

    #[test]
    fn ipv4_rejects_fm350_dummy_v6_address() {
        // FM350 在 IPv6 未分配时回 16 段点分十进制伪地址
        let dummy = "0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1";
        assert!(!is_valid_ipv4(dummy));
        assert!(!is_valid_ipv4("36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8"));
    }

    #[test]
    fn ipv4_rejects_edge_cases() {
        assert!(!is_valid_ipv4(""));
        assert!(!is_valid_ipv4("0.0.0.0"));
        assert!(!is_valid_ipv4("10.5.23"));
        assert!(!is_valid_ipv4("10.5.23.256"));
        assert!(!is_valid_ipv4("10.5.23.a"));
        assert!(!is_valid_ipv4("::1"));
    }

    #[test]
    fn ipv6_accepts_proper_notation() {
        assert!(is_valid_ipv6("2409:8a00:1234::1"));
        assert!(is_valid_ipv6("fe80::1"));
    }

    #[test]
    fn ipv6_rejects_dummy_and_empty() {
        assert!(!is_valid_ipv6(""));
        assert!(!is_valid_ipv6("10.5.23.212"));
        // 全零地址（:: 展开）
        assert!(!is_valid_ipv6("0:0:0:0:0:0:0:0"));
        assert!(!is_valid_ipv6("0000:0000:0000:0000:0000:0000:0000:0000"));
    }

    #[test]
    fn ipv6_decodes_fm350_dotted_notation() {
        // 实机 AT+CGCONTRDP=1 原始响应中的 <PDP_addr> 与 <gateway>
        let addr = "36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8";
        let gw = "36.9.128.87.32.0.0.4.0.0.0.0.0.0.0.8";
        assert!(is_valid_ipv6(addr));
        assert_eq!(normalize_ipv6(addr).as_deref(), Some("2409:8057:2000::8"));
        assert_eq!(normalize_ipv6(gw).as_deref(), Some("2409:8057:2000:4::8"));
    }

    #[test]
    fn ipv6_rejects_fm350_dotted_placeholder() {
        // 实机 AT+CGPADDR=0 第三字段：IPv6 未分配时的占位
        let dummy = "0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1";
        assert!(!is_valid_ipv6(dummy));
        assert_eq!(normalize_ipv6(dummy), None);
        assert!(!is_valid_ipv6("0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0"));
        // 段数不足 / 越界值仍判无效
        assert!(!is_valid_ipv6("1.2.3.4"));
        assert!(!is_valid_ipv6("36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.256"));
    }

    #[test]
    fn ipv6_compression_edge_cases() {
        assert_eq!(
            normalize_ipv6("2409:8a00:1234::1").as_deref(),
            Some("2409:8a00:1234::1")
        );
        // 全零组（16 段全 0）属占位，判无效
        assert_eq!(normalize_ipv6("0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0"), None);
    }
}

#[cfg(test)]
mod signal_index_tests {
    use super::*;

    #[test]
    fn ss_rsrp_matches_manual_examples() {
        // 手册：1 = [-156,-155)，125 = [-32,-31)，126 = ≥ -31，255 = 未知
        assert_eq!(ss_rsrp_dbm(1), Some(-156));
        assert_eq!(ss_rsrp_dbm(125), Some(-32));
        assert_eq!(ss_rsrp_dbm(126), Some(-31));
        assert_eq!(ss_rsrp_dbm(255), None);
        // 实机 CESQ 值 75 → -82？取下界即 -156+74 = -82
        assert_eq!(ss_rsrp_dbm(75), Some(-82));
        assert_eq!(ss_rsrp_dbm(74), Some(-83));
        assert_eq!(ss_rsrp_dbm(0), Some(-157));
    }

    #[test]
    fn ss_rsrq_and_sinr_have_half_db_steps() {
        // 手册：SS-RSRQ 1 = [-43,-42.5)，步长 0.5
        assert_eq!(ss_rsrq_db(1), Some(-43.0));
        assert_eq!(ss_rsrq_db(64), Some(-11.5));
        assert_eq!(ss_rsrq_db(65), Some(-11.0));
        assert_eq!(ss_rsrq_db(126), Some(19.5));
        assert_eq!(ss_rsrq_db(255), None);

        // 手册：SS-SINR 1 = [-23,-22.5)，127 = ≥ 40
        assert_eq!(ss_sinr_db(1), Some(-23.0));
        assert_eq!(ss_sinr_db(75), Some(14.0));
        assert_eq!(ss_sinr_db(127), Some(40.0));
        assert_eq!(ss_sinr_db(255), None);
    }

    #[test]
    fn lte_and_umts_indexes() {
        assert_eq!(lte_rsrp_dbm(1), Some(-140));
        assert_eq!(lte_rsrp_dbm(96), Some(-45));
        assert_eq!(lte_rsrp_dbm(97), Some(-44));
        assert_eq!(lte_rsrp_dbm(255), None);

        assert_eq!(lte_rsrq_db(1), Some(-19.5));
        assert_eq!(lte_rsrq_db(34), Some(-3.0));

        assert_eq!(rscp_dbm(1), Some(-120));
        assert_eq!(rscp_dbm(96), Some(-25));

        assert_eq!(ecno_db(1), Some(-24.0));
        assert_eq!(ecno_db(49), Some(0.0));
    }

    #[test]
    fn rxlev_unknown_is_99() {
        assert_eq!(rxlev_dbm(1), Some(-110));
        assert_eq!(rxlev_dbm(63), Some(-48));
        // CESQ 实测 rxlev = 99 表示不可用（非 GERAN 服务小区）
        assert_eq!(rxlev_dbm(99), None);
        assert_eq!(rxlev_dbm(255), None);
    }

    #[test]
    fn csq_99_is_unknown() {
        assert_eq!(csq_to_dbm(99), None);
        assert_eq!(csq_to_dbm(0), Some(-113));
        assert_eq!(csq_to_dbm(31), Some(-51));
    }

    #[test]
    fn cell_rat_mapping() {
        assert_eq!(cell_rat_text("9"), "5G NR");
        assert_eq!(cell_rat_text("4"), "LTE (4G)");
        assert_eq!(cell_rat_text("2"), "WCDMA (3G)");
        assert_eq!(cell_rat_text("0"), "无网络");
        assert_eq!(cell_rat_text("99"), "");
    }

    #[test]
    fn sensor_names() {
        assert_eq!(sensor_name(10), "md_5g");
        assert_eq!(sensor_name(1), "soc_max");
        assert_eq!(sensor_name(15), "nrpa_ntc");
        assert_eq!(sensor_name(5), "");
    }
}

#[cfg(test)]
mod band_derive_tests {
    use super::*;

    #[test]
    fn nr_band_matches_live_arfcn() {
        // 实机服务小区 NARFCN = 504990 → 504990 × 5 kHz = 2524.95 MHz → n41
        assert_eq!(nr_band_from_arfcn(504_990), Some("n41"));
    }

    #[test]
    fn nr_band_marks_overlapping_bands() {
        // 2595 MHz 同时落在 n38（2570-2620）与 n41（2496-2690）内，
        // 必须标注重叠而不是任意选一个。
        assert_eq!(nr_band_from_arfcn(519_000), Some("n38/n41"));
        // 3500 MHz → ARFCN 633333（3000 MHz 以上改用 15 kHz 步长）→ n77/n78
        assert_eq!(nr_band_from_arfcn(633_333), Some("n77/n78"));
        // 4800 MHz → ARFCN 720000 → n79
        assert_eq!(nr_band_from_arfcn(720_000), Some("n79"));
    }

    #[test]
    fn nr_band_out_of_table_returns_none() {
        assert_eq!(nr_band_from_arfcn(0), None);          // 0 MHz 不在任何频段
        assert_eq!(nr_band_from_arfcn(3_000_000), None);  // 超出 FR1 栅格
    }

    #[test]
    fn nr_band_code_decodes_all_three_encodings() {
        // 实机 NR 服务小区 <band> = 5041，即 50|41
        assert_eq!(nr_band_from_code("5041").as_deref(), Some("n41"));
        // `500` 前缀那套（README 举例）：501 = 500|1
        assert_eq!(nr_band_from_code("501").as_deref(), Some("n1"));
        // `100` 前缀那套（README 举例）：141 = 100|41
        assert_eq!(nr_band_from_code("141").as_deref(), Some("n41"));
        // 高位写法
        assert_eq!(nr_band_from_code("5078").as_deref(), Some("n78"));
        assert_eq!(nr_band_from_code("5079").as_deref(), Some("n79"));
        assert_eq!(nr_band_from_code("50512").as_deref(), Some("n512"));
        // 未分配编号与非法输入必须返回 None，不得猜
        assert_eq!(nr_band_from_code("5006"), None); // n6 未分配
        assert_eq!(nr_band_from_code("5027"), None); // n27 未分配
        assert_eq!(nr_band_from_code("500"), None);  // 剥离前缀后余 "0"
        assert_eq!(nr_band_from_code(""), None);
        assert_eq!(nr_band_from_code("n41"), None);
    }

    #[test]
    fn lte_band_code_decodes_both_encodings() {
        // `100` 前缀那套：101 = B1
        assert_eq!(lte_band_from_code("101").as_deref(), Some("B1"));
        assert_eq!(lte_band_from_code("171").as_deref(), Some("B71"));
        // band 号直写
        assert_eq!(lte_band_from_code("3").as_deref(), Some("B3"));
        assert_eq!(lte_band_from_code("41").as_deref(), Some("B41"));
        // B16 未分配；越界与非法输入同样返回 None
        assert_eq!(lte_band_from_code("116"), None);
        assert_eq!(lte_band_from_code("16"), None);
        assert_eq!(lte_band_from_code(""), None);
        assert_eq!(lte_band_from_code("B3"), None);
    }

    #[test]
    fn bandwidth_code_maps_by_rat() {
        // NR（手册 p191）：编码 = MHz × 5。实机返回 500 → 100 MHz
        assert_eq!(bandwidth_text("9", "500").as_deref(), Some("100 MHz"));
        assert_eq!(bandwidth_text("9", "250").as_deref(), Some("50 MHz"));
        assert_eq!(bandwidth_text("9", "450").as_deref(), Some("90 MHz"));
        assert_eq!(bandwidth_text("9", "25").as_deref(), Some("5 MHz"));
        assert_eq!(bandwidth_text("9", "1000").as_deref(), Some("200 MHz"));
        assert_eq!(bandwidth_text("9", "2000").as_deref(), Some("400 MHz"));
        // LTE：编码 = RB 数
        assert_eq!(bandwidth_text("4", "100").as_deref(), Some("20 MHz"));
        assert_eq!(bandwidth_text("4", "6").as_deref(), Some("1.4 MHz"));
        // 表外编码与未知制式必须返回 None，由调用方保留原始码
        assert_eq!(bandwidth_text("9", "777"), None);
        assert_eq!(bandwidth_text("4", "250"), None);
        assert_eq!(bandwidth_text("2", "500"), None);
        assert_eq!(bandwidth_text("9", ""), None);
    }

    #[test]
    fn lte_band_ranges() {
        assert_eq!(lte_band_from_earfcn(300), Some("B1"));
        assert_eq!(lte_band_from_earfcn(1650), Some("B3"));
        assert_eq!(lte_band_from_earfcn(2550), Some("B5"));
        assert_eq!(lte_band_from_earfcn(3600), Some("B8"));
        assert_eq!(lte_band_from_earfcn(36300), Some("B34"));
        assert_eq!(lte_band_from_earfcn(38000), Some("B38"));
        assert_eq!(lte_band_from_earfcn(38400), Some("B39"));
        assert_eq!(lte_band_from_earfcn(39000), Some("B40"));
        assert_eq!(lte_band_from_earfcn(40000), Some("B41"));
        assert_eq!(lte_band_from_earfcn(1000), None);
    }

    #[test]
    fn operator_lookup_covers_three_carriers() {
        assert_eq!(operator_name("46000"), "中国移动");
        assert_eq!(operator_name("46002"), "中国移动");
        assert_eq!(operator_name("46001"), "中国联通");
        assert_eq!(operator_name("46003"), "中国电信");
        assert_eq!(operator_name("46015"), "中国广电");
        // 未收录的编码必须返回空串，由界面回落到数字码
        assert_eq!(operator_name("31026"), "");
        assert_eq!(operator_name(""), "");
    }
}

#[cfg(test)]
mod temperature_precision_tests {
    use super::*;

    /// 回归：47.2 ℃ 曾被 f32 序列化成 47.20000076293945。
    #[test]
    fn temperature_json_has_no_float_noise() {
        let t = Temperature {
            modem: Some(round1_f64(45485.0 / 1000.0)),
            soc_max: Some(round1_f64(47200.0 / 1000.0)),
            peak: Some(round1_f64(47200.0 / 1000.0)),
            sensors: vec![(1, round1_f64(47200.0 / 1000.0)), (10, round1_f64(45485.0 / 1000.0))],
        };
        let json = serde_json::to_string(&t).expect("serialize");
        assert!(json.contains("47.2"), "json = {}", json);
        assert!(json.contains("45.5"), "json = {}", json);
        assert!(!json.contains("47.20000"), "仍出现浮点尾数: {}", json);
        assert!(!json.contains("45.48500"), "仍出现浮点尾数: {}", json);
    }

    #[test]
    fn round1_f64_keeps_one_decimal() {
        assert_eq!(round1_f64(47.044), 47.0);
        assert_eq!(round1_f64(47.200), 47.2);
        assert_eq!(round1_f64(45.485), 45.5);
        assert_eq!(round1_f64(-0.04), -0.0);
    }

    /// 0 表示该路未装配，必须跳过（用真实响应前两路验证解析规则）。
    #[test]
    fn zero_reading_is_skipped() {
        let ids: Vec<u32> = [(1u32, 47044i32), (2, 0), (3, 46463)]
            .iter()
            .filter(|(_, raw)| *raw != 0)
            .map(|(id, _)| *id)
            .collect();
        assert_eq!(ids, vec![1, 3]);
    }
}

#[cfg(test)]
mod overall_signal_tests {
    use super::*;

    /// 实机读数：RSRP -81 dBm、RSRQ -11.0 dB、SINR 15.0 dB
    /// 归一化后 RSRP 61.5 / RSRQ 51.5 / SINR 71.7，
    /// 加权（0.35/0.25/0.40）得 63.1，等级 4，文字「良」。
    #[test]
    fn overall_matches_live_reading() {
        let (score, used) = overall_score(Some(-81), Some(-11.0), Some(15.0)).expect("score");
        assert_eq!(used, vec!["RSRP", "RSRQ", "SINR"]);
        assert!((score - 63.1).abs() < 0.05, "score = {}", score);
        assert_eq!(overall_level(score), 4);
        assert_eq!(overall_grade_text(score), "良");
    }

    /// 缺失指标按可用权重重新归一化，不得把得分拉低。
    #[test]
    fn overall_renormalizes_when_metric_missing() {
        // 只有 RSRP 时，得分就等于 RSRP 的归一化值
        let (only_rsrp, used) = overall_score(Some(-80), None, None).expect("score");
        assert_eq!(used, vec!["RSRP"]);
        assert!((only_rsrp - 62.5).abs() < 0.05, "score = {}", only_rsrp);

        // 补上 SINR 后权重重新分配，仍应是同量级
        let (mixed, _) = overall_score(Some(-80), None, Some(15.0)).expect("score");
        assert!(mixed > 60.0 && mixed < 75.0, "score = {}", mixed);
    }

    #[test]
    fn overall_is_none_without_any_metric() {
        assert!(overall_score(None, None, None).is_none());
    }

    /// 边界：指标达到各自标称极值时得分必须被截断在 0..100。
    #[test]
    fn overall_clamps_to_range() {
        let (best, _) = overall_score(Some(-44), Some(-3.0), Some(30.0)).expect("score");
        assert_eq!(best, 100.0);
        let (worst, _) = overall_score(Some(-140), Some(-19.5), Some(-23.0)).expect("score");
        assert_eq!(worst, 0.0);
        assert_eq!(overall_level(0.0), 0);
    }

    #[test]
    fn rsrq_grade_thresholds() {
        assert_eq!(rsrq_grade_text(-9.0), "优");
        assert_eq!(rsrq_grade_text(-10.0), "优");
        assert_eq!(rsrq_grade_text(-12.0), "良");
        assert_eq!(rsrq_grade_text(-15.0), "良");
        assert_eq!(rsrq_grade_text(-18.0), "中");
        assert_eq!(rsrq_grade_text(-19.5), "中");
        assert_eq!(rsrq_grade_text(-20.0), "差");
    }
}
