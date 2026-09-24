//! 信号质量与注册状态。
//!
//! ## 数据来源与为什么是这些命令
//!
//! * `AT+CSQ` 在本模组上**恒为 `99,99`**（不可用），只能作为粗粒度兜底；
//! * `AT+CESQ`（3GPP TS 27.007 §8.69）才是真正的信号来源，但它的九个字段
//!   是**阶梯索引**而非物理值，且只有当前制式对应的那几个索引有效；
//! * 制式（`<rat>`）必须来自 `AT+GTCCINFO?` 的服务小区行 —— 只有知道制式，
//!   才知道该读 CESQ 的哪几个下标、用哪张换算表；
//! * LTE 没有 SINR 索引，`+GTCCINFO` 服务小区第 10 个字段是 `<rssnr_value>`，
//!   该值**直接就是 dB**，不再走阶梯换算。

use super::band;
use super::run;
use crate::addr::is_link_local_v6;
use crate::at::{at_cmd, AtHandle};
use crate::config::Config;

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

// ---------------------------------------------------------------- 索引换算
//
// CESQ 与 GTCCINFO 的信号量都是**阶梯索引**而非物理值：
// 索引 n（n ≥ 1）对应区间 [base + (n-1)*step, base + n*step)，索引 0 表示
// 「小于 base」。下面统一取区间**下界**作为结果（保守估计，与手册的列举
// 方式一致）。未知值：CESQ / GTCCINFO 用 255 表示，部分字段（如 rxlev、ber）
// 用 99。

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
pub fn ss_rsrp_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 126, &[255]) {
        return None;
    }
    Some(idx_lower(idx, -156.0, 1.0) as i32)
}

/// SS-RSRQ（NR）：1 = [-43,-42.5) dB，126 = [19.5,20)，255 = 未知。
pub fn ss_rsrq_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 126, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -43.0, 0.5)))
}

/// SS-SINR（NR）：1 = [-23,-22.5) dB，127 = ≥ 40，255 = 未知。
pub fn ss_sinr_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 127, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -23.0, 0.5)))
}

/// RSRP（LTE）：1 = [-140,-139) dBm，96 = [-45,-44)，97 = ≥ -44，255 = 未知。
pub fn lte_rsrp_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 97, &[255]) {
        return None;
    }
    Some(idx_lower(idx, -140.0, 1.0) as i32)
}

/// RSRQ（LTE）：1 = [-19.5,-19) dB，34 = ≥ -3，255 = 未知。
pub fn lte_rsrq_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 34, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -19.5, 0.5)))
}

/// RSCP（WCDMA）：1 = [-120,-119) dBm，96 = ≥ -25，255 = 未知。
pub fn rscp_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 96, &[255]) {
        return None;
    }
    Some(idx_lower(idx, -120.0, 1.0) as i32)
}

/// Ec/Io（WCDMA）：1 = [-24,-23.5) dB，49 = ≥ 0，255 = 未知。
pub fn ecno_db(idx: i32) -> Option<f32> {
    if !idx_known(idx, 49, &[255]) {
        return None;
    }
    Some(round1(idx_lower(idx, -24.0, 0.5)))
}

/// rxlev（GERAN）：1 = [-110,-109) dBm，63 = ≥ -48，99 = 未知。
pub fn rxlev_dbm(idx: i32) -> Option<i32> {
    if !idx_known(idx, 63, &[99, 255]) {
        return None;
    }
    Some(idx_lower(idx, -110.0, 1.0) as i32)
}

/// CSQ RSSI 映射（手册：0 = -113 dBm，每档 +2 dBm，99 = 未知）。
pub fn csq_to_dbm(csq: u32) -> Option<i32> {
    match csq {
        0..=31 => Some(-113 + 2 * csq as i32),
        _ => None,
    }
}

/// GTCCINFO 的 `<rat>` 转文字（0 无效 / 2 WCDMA / 4 LTE / 9 NR）。
pub fn cell_rat_text(rat: &str) -> &'static str {
    match rat {
        "0" => "无网络",
        "2" => "WCDMA (3G)",
        "4" => "LTE (4G)",
        "9" => "5G NR",
        _ => "",
    }
}

/// 注册状态码转文字（3GPP TS 27.007）。
pub fn reg_text(state: &str) -> &'static str {
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
pub fn act_text(act: &str) -> &'static str {
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

// ---------------------------------------------------------------- 派生评分

/// 把一个物理量线性映射到 0..100，超出标称范围即截断。
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

/// RSRQ 信号质量等级。阈值集中在此，便于与文档核对。
/// 一般不高于 -10 dB 视为优、-10 ~ -15 dB 为良、-15 ~ -19.5 dB 为中。
pub fn rsrq_grade_text(rsrq: f64) -> &'static str {
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
pub fn overall_score(
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
pub fn overall_level(score: f64) -> u32 {
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
pub fn overall_grade_text(score: f64) -> &'static str {
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

/// 保留 1 位小数（f64 版本；信号换算仍用 f32 的 `round1`）。
///
/// 温度用 f64 而非 f32：读数形如 47.2 ℃ 在 f32 下无法精确表示，
/// `serde_json` 会序列化成 `47.20000076293945`（实机已复现）。
pub fn round1_f64(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

// ---------------------------------------------------------------- 采集

/// 从 `AT+GTCCINFO?` 响应中取出**服务小区**行的字段。
///
/// 响应可能有多行（每行一个小区），首个字段为 `<IsServiceCell>`：
/// 1 = 服务小区，2 = 邻区。只看服务小区。
/// 从 `AT+GTCCINFO?` 的响应中取出**服务小区**那一行（`<IsServiceCell>=1`）。
///
/// ## 这里修掉的是原实现的一处死代码
///
/// 原实现写的是：
///
/// ```ignore
/// if l.is_empty() || l.starts_with("OK") || l.starts_with("+GTCCINFO") {
///     continue;
/// }
/// ```
///
/// 但 `AT+GTCCINFO?` 的每一行**恰恰都以 `+GTCCINFO: ` 开头**，于是所有行都被
/// 跳过、服务小区永远解析不到。连带失效的三项（都在这个分支里取值）：
///   * LTE 的 SINR（服务小区第 10 个字段 `<rssnr_value>`）—— **LTE 下 SINR
///     恒为空**，`AT+CESQ` 本身不带 LTE 的 SINR；
///   * `band`（由第 8 字段反解）与 `band_source`；
///   * `bandwidth`（由 `<rat>` + 第 9 字段换算）。
///
/// 修正：先剥离 `+GTCCINFO:` 前缀，再按首个字段筛服务小区。同时兼容「前缀已被
/// 上层剥掉」的输入形态。
fn service_cell(resp: &str) -> Vec<String> {
    for line in resp.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with("OK") {
            continue;
        }
        let body = l.strip_prefix("+GTCCINFO:").unwrap_or(l).trim();
        if body.is_empty() {
            continue;
        }
        let first = body.split(',').next().unwrap_or("").trim();
        if first != "1" {
            continue; // 只看服务小区，2 = 邻区
        }
        return body
            .split(',')
            .map(|x| x.trim().trim_matches('"').to_string())
            .collect();
    }
    Vec::new()
}

pub fn signal(at: &AtHandle, cfg: &Config) -> Signal {
    let mut s = Signal::default();

    if let Ok(r) = run(at, cfg, at_cmd::CSQ) {
        let v = super::f(&r, "+CSQ");
        if let Some(c) = v.first().and_then(|x| x.parse().ok()) {
            s.csq = Some(c);
            s.rssi_dbm = csq_to_dbm(c);
        }
        s.ber = v.get(1).and_then(|x| x.parse().ok());
    }

    // EPS 注册状态（5G/LTE 优先）
    let reg = run(at, cfg, at_cmd::CEREG_READ)
        .or_else(|_| run(at, cfg, at_cmd::CREG_READ))
        .unwrap_or_default();
    let rv = super::f(&reg, "+CEREG");
    let rv = if rv.is_empty() {
        super::f(&reg, "+CREG")
    } else {
        rv
    };
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

    if let Ok(r) = run(at, cfg, at_cmd::COPS_READ) {
        let v = super::f(&r, "+COPS");
        if let Some(op) = v.get(2) {
            s.operator = op.clone();
            s.operator_name = band::operator_name(&s.operator).to_string();
        }
        if s.rat.is_empty() {
            if let Some(act) = v.get(3) {
                s.rat = act_text(act).to_string();
            }
        }
    }

    // ---- 扩展信号质量（AT+CESQ）----
    //
    // 字段顺序：<rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<ss_rsrq>,<ss_rsrp>,<ss_sinr>
    // 实测：+CESQ: 99,99,255,255,255,255,65,75,75（当前为 NR，故前 6 位不可用）
    let cesq: Vec<Option<i32>> = run(at, cfg, at_cmd::CESQ)
        .map(|r| {
            super::f(&r, "+CESQ")
                .iter()
                .map(|x| x.parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default();
    let ci = |i: usize| -> Option<i32> { cesq.get(i).copied().flatten() };

    // ---- 服务小区信息（AT+GTCCINFO?）----
    //
    // <rat> 决定字段布局：2 = WCDMA，4 = LTE/eMTC/NB-IoT，9 = NR。
    // 三种制式的服务小区前 10 个字段位置一致：
    //   <IsServiceCell>,<rat>,<mcc>,<mnc>,<tac|lac>,<cellid>,<arfcn>,<pci|psc>,<band>,<bandwidth>
    let cell: Vec<String> = run(at, cfg, at_cmd::GTCCINFO_READ)
        .map(|r| service_cell(&r))
        .unwrap_or_default();
    let cell_rat = cell.get(1).cloned().unwrap_or_default();
    let cget = |i: usize| -> String { cell.get(i).cloned().unwrap_or_default() };

    if !cell.is_empty() {
        s.lac = cget(4);
        s.cid = cget(5);
        s.arfcn = cget(6);
        s.pci = cget(7);

        let raw_band = cget(8);
        let raw_bw = cget(9);

        // 后两个字段是**编码值而非人可读名**：实机 NR 服务小区返回
        // `...,504990,128,5041,500,...` —— `5041` 是 `50|41` = n41、
        // `500` 是带宽的 1/5 = 100 MHz。解不出来时保留原始码并显式标注，
        // 不把未识别的值伪装成频段名或带宽值。
        if !raw_band.is_empty() {
            let decoded = match cell_rat.as_str() {
                "9" => band::nr_band_from_code(&raw_band),
                "4" => band::lte_band_from_code(&raw_band),
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
            s.bandwidth = band::bandwidth_text(&cell_rat, &raw_bw).unwrap_or(raw_bw);
        }
    }

    let rat_txt = cell_rat_text(&cell_rat);
    if !rat_txt.is_empty() {
        s.rat = rat_txt.to_string();
    }

    // ---- 按制式选择对应的索引换算 ----
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
            // GTCCINFO LTE 服务小区第 10 个字段为 <rssnr_value>，
            // 单位 dB（-100..100，255 = 无效），不再走阶梯换算。
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
    // 此时按 ARFCN / EARFCN 反推频段号，并**显式标注**为推算值。
    // 只在 NR(9) 与 LTE(4) 下推算：UARFCN 的编号体系不同，不参与。
    if s.band.is_empty() {
        if let Ok(n) = s.arfcn.parse::<i64>() {
            let derived = match cell_rat.as_str() {
                "9" => band::nr_band_from_arfcn(n),
                "4" => band::lte_band_from_earfcn(n),
                _ => None,
            };
            if let Some(b) = derived {
                s.band = b.to_string();
                s.band_source = "由 ARFCN 推算（模组未上报）".to_string();
            }
        }
    }

    // RSRQ 质量等级与综合信号——两者都是**派生值**
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

    // 载波聚合：结构未定，先原样呈现前 8 行
    if let Ok(r) = run(at, cfg, at_cmd::GTCAINFO_READ) {
        s.ca = crate::at::raw_lines(&r, "+GTCAINFO:", 8);
    }

    let _ = is_link_local_v6; // 保持 addr 的引用清晰（信号侧不使用 v6 判定）

    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ss_rsrp_matches_manual_examples() {
        assert_eq!(ss_rsrp_dbm(1), Some(-156));
        assert_eq!(ss_rsrp_dbm(125), Some(-32));
        assert_eq!(ss_rsrp_dbm(126), Some(-31));
        assert_eq!(ss_rsrp_dbm(255), None);
        // 实机 CESQ 值 75 → 取下界即 -156+74 = -82
        assert_eq!(ss_rsrp_dbm(75), Some(-82));
        assert_eq!(ss_rsrp_dbm(74), Some(-83));
        assert_eq!(ss_rsrp_dbm(0), Some(-157));
    }

    #[test]
    fn ss_rsrq_and_sinr_have_half_db_steps() {
        assert_eq!(ss_rsrq_db(1), Some(-43.0));
        assert_eq!(ss_rsrq_db(64), Some(-11.5));
        assert_eq!(ss_rsrq_db(65), Some(-11.0));
        assert_eq!(ss_rsrq_db(126), Some(19.5));
        assert_eq!(ss_rsrq_db(255), None);

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

    #[test]
    fn overall_renormalizes_when_metric_missing() {
        let (only_rsrp, used) = overall_score(Some(-80), None, None).expect("score");
        assert_eq!(used, vec!["RSRP"]);
        assert!((only_rsrp - 62.5).abs() < 0.05, "score = {}", only_rsrp);

        let (mixed, _) = overall_score(Some(-80), None, Some(15.0)).expect("score");
        assert!(mixed > 60.0 && mixed < 75.0, "score = {}", mixed);
    }

    #[test]
    fn overall_is_none_without_any_metric() {
        assert!(overall_score(None, None, None).is_none());
    }

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

    #[test]
    fn service_cell_picks_only_the_serving_row() {
        let resp = "+GTCCINFO: 2,9,460,0,1234,5678,504990,129,5041,500\n\
                    +GTCCINFO: 1,9,460,0,1234,9999,504990,128,5041,500\n\
                    OK";
        let c = service_cell(resp);
        assert_eq!(c.get(5).map(|s| s.as_str()), Some("9999"), "应取服务小区行");
        assert_eq!(c.get(8).map(|s| s.as_str()), Some("5041"));
    }
}
