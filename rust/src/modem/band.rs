//! `AT+GTCCINFO?` 的频段 / 带宽编码解码，以及 ARFCN 反推频段。
//!
//! ## 为什么需要它
//!
//! `+GTCCINFO` 服务小区的第 9、10 个字段是**编码值而不是人可读名**。
//! 早期版本把它们原样透出，界面于是显示「频段 5041 / 带宽档位 500」，
//! 看似有值、实则不可读，排障时完全派不上用场。
//!
//! 本模块的规则全部来自实机佐证 + 《Fibocom FM350 AT Commands》p126 / p133
//! / p191，解不出来时**返回 `None` 由调用方保留原始码**，绝不猜测。

/// 由 NR-ARFCN 反推频段号（3GPP TS 38.104 表 5.4.2.1-1 的全局频率栅格 +
/// TS 38.101-1 表 5.4.2.1-1 的各频段频率范围）。
///
/// 之所以需要本函数：FM350 在 NR 制式下 `+GTCCINFO?` 的 `<band>` 字段
/// 实测为空，显示 `-` 对排障没有帮助。
/// 返回 `None` 表示落在本地表未覆盖的范围 —— **不做猜测**，宁可显示空。
pub fn nr_band_from_arfcn(arfcn: i64) -> Option<&'static str> {
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
///
/// 直接按区间判定，不引入浮点换算，也就不会出现边界误差。
pub fn lte_band_from_earfcn(earfcn: i64) -> Option<&'static str> {
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
/// 与实机佐证一致 —— `AT+GTACT` 回显的 `101..171` 序列里
/// 106/109/110/111/115/116/121~124/127 全部缺席。
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
/// 该字段存在**两套生成规则**：
///
/// 1. **数值加法**：`基数 + 频段号`，基数分三档 ——
///    `141` = 100 + 41 = n41、`101` = 100 + 1 = n1、`171` = 100 + 71 = n71；
///    `501` = 500 + 1 = n1；`5041` = 5000 + 41 = n41。
///    实机 NR 服务小区上报的 `5041` 即属此列。
/// 2. **十进制字符串拼接**：`50` 直接后接频段号。实机 `AT+GTACT` 候选列表里
///    出现过 `50512`，按数值加法无解（5000 + 512 = 5512 ≠ 50512），
///    而 `50` 接 `512` 恰好成立。
///
/// 两套规则在部分取值上同解（`5041` 两种解释都指向 n41），故先试数值加法、
/// 再试字符串拼接；候选频段号一律经过已分配性校验。
pub fn nr_band_from_code(code: &str) -> Option<String> {
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
/// 两套编码：
/// - **数值加法**：`100 + 频段号` —— `101` = B1 …… `171` = B71；
/// - **band 号直写** —— `1` = B1、`3` = B3、`41` = B41。
///
/// 两者都要求频段号落在 3GPP 已分配区间（LTE 为 1..=88，其中 16 未分配）。
pub fn lte_band_from_code(code: &str) -> Option<String> {
    let c = code.trim();
    if c.is_empty() || c.len() > 6 || !c.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i32 = c.parse().ok()?;
    let accept = |n: i32| (1..=88).contains(&n) && n != 16;

    if v > 100 && accept(v - 100) {
        return Some(format!("B{}", v - 100));
    }
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
pub fn bandwidth_text(rat: &str, code: &str) -> Option<String> {
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
/// 非权威映射：`AT+COPS?` 在 numeric 格式下只回数字码（实测本模组回
/// "46000"），这里补一个可读名只为界面友好，原始数字码始终保留在
/// `operator` 字段。未收录的编码返回空串，界面照旧显示数字码。
pub fn operator_name(code: &str) -> &'static str {
    match code {
        "46000" | "46002" | "46004" | "46007" | "46008" => "中国移动",
        "46001" | "46006" | "46009" => "中国联通",
        "46003" | "46005" | "46011" | "46012" => "中国电信",
        "46015" => "中国广电",
        "46020" => "中国铁通",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nr_band_matches_live_arfcn() {
        // 实机服务小区 NARFCN = 504990 → 504990 × 5 kHz = 2524.95 MHz → n41
        assert_eq!(nr_band_from_arfcn(504_990), Some("n41"));
    }

    #[test]
    fn nr_band_marks_overlapping_bands() {
        // 2595 MHz 同时落在 n38（2570-2620）与 n41（2496-2690）内
        assert_eq!(nr_band_from_arfcn(519_000), Some("n38/n41"));
        // 3500 MHz → ARFCN 633333（3000 MHz 以上改用 15 kHz 步长）→ n77/n78
        assert_eq!(nr_band_from_arfcn(633_333), Some("n77/n78"));
        // 4800 MHz → ARFCN 720000 → n79
        assert_eq!(nr_band_from_arfcn(720_000), Some("n79"));
    }

    #[test]
    fn nr_band_out_of_table_returns_none() {
        assert_eq!(nr_band_from_arfcn(0), None);
        assert_eq!(nr_band_from_arfcn(3_000_000), None);
    }

    #[test]
    fn nr_band_code_decodes_all_three_encodings() {
        assert_eq!(nr_band_from_code("5041").as_deref(), Some("n41"));
        assert_eq!(nr_band_from_code("501").as_deref(), Some("n1"));
        assert_eq!(nr_band_from_code("141").as_deref(), Some("n41"));
        assert_eq!(nr_band_from_code("5078").as_deref(), Some("n78"));
        assert_eq!(nr_band_from_code("5079").as_deref(), Some("n79"));
        assert_eq!(nr_band_from_code("50512").as_deref(), Some("n512"));
        // 未分配编号与非法输入必须返回 None，不得猜
        assert_eq!(nr_band_from_code("5006"), None); // n6 未分配
        assert_eq!(nr_band_from_code("5027"), None); // n27 未分配
        assert_eq!(nr_band_from_code("500"), None);
        assert_eq!(nr_band_from_code(""), None);
        assert_eq!(nr_band_from_code("n41"), None);
    }

    #[test]
    fn lte_band_code_decodes_both_encodings() {
        assert_eq!(lte_band_from_code("101").as_deref(), Some("B1"));
        assert_eq!(lte_band_from_code("171").as_deref(), Some("B71"));
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
        assert_eq!(bandwidth_text("9", "500").as_deref(), Some("100 MHz"));
        assert_eq!(bandwidth_text("9", "250").as_deref(), Some("50 MHz"));
        assert_eq!(bandwidth_text("9", "450").as_deref(), Some("90 MHz"));
        assert_eq!(bandwidth_text("9", "25").as_deref(), Some("5 MHz"));
        assert_eq!(bandwidth_text("9", "1000").as_deref(), Some("200 MHz"));
        assert_eq!(bandwidth_text("9", "2000").as_deref(), Some("400 MHz"));
        assert_eq!(bandwidth_text("4", "100").as_deref(), Some("20 MHz"));
        assert_eq!(bandwidth_text("4", "6").as_deref(), Some("1.4 MHz"));
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
        assert_eq!(operator_name("31026"), "");
        assert_eq!(operator_name(""), "");
    }
}
