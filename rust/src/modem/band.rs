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
        // n20（791-821）与 n28（758-803）在 791-803 MHz 重叠，同样标注而不猜。
        _ if (791.0..=803.0).contains(&mhz) => "n20/n28",
        _ if (791.0..=821.0).contains(&mhz) => "n20",
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

/// 判定 NR 频段号是否可解。
///
/// FR1（1..=105）取「3GPP TS 38.101-1 已分配集合」并上「手册明列的 9 / 10」：
/// 手册 §11.1.14 与 §11.1.16 都直接写出了 `509 BAND_NR_9`、`5010 BAND_NR_10`，
/// 属于本文档的权威定义，不能因为 3GPP 未分配就拒解。
///
/// 旧实现用的是一份不完整的「未分配」**黑名单**（只列了 6/9/10/11/15/16/
/// 21~24/27），于是 `504` 会被解成根本不存在的 `n4`、`5017` 解成 `n17`
/// （n17 是 LTE 频段）—— 恰恰是黑名单想拦的那类假阳性。
/// 改成白名单后不再依赖「记得住哪些编号不存在」。
///
/// FR2 与扩展区间沿用实机 `AT+GTACT=?` 回显佐证的范围，不自行放宽。
/// **权威依据始终是模组自己回的候选列表**（手册 §11.1.14 的测试形式会给出
/// `(list of supported <nr_band>s)`），本表只是离线兜底。
/// 与前端 `network.js::nrBandNo` 保持完全一致。
fn nr_band_no_is_valid(n: i32) -> bool {
    const FR1_ASSIGNED: [i32; 63] = [
        1, 2, 3, 5, 7, 8, 9, 10, 12, 13, 14, 18, 20, 25, 26, 28, 29, 30, 34, 38, 39, 40,
        41, 46, 48, 65, 66, 67, 68, 69, 70, 71, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82,
        83, 84, 85, 86, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 103,
        104, 105,
    ];
    if (1..=105).contains(&n) {
        return FR1_ASSIGNED.contains(&n);
    }
    (256..=269).contains(&n) || (510..=512).contains(&n) || (670..=710).contains(&n)
}

/// 由 `AT+GTCCINFO?` 的 `<band>` 编码反解 NR 频段名。
///
/// 手册 §11.1.14 与 §11.1.16 给出的是**十进制字符串拼接**这一条规则：
/// `50` 直接后接 band 号 —— `501` = BAND_NR_1 … `509` = BAND_NR_9、
/// `5010` = BAND_NR_10 … `50512` = BAND_NR_512。
///
/// 旧实现把「数值加法」当成第一优先规则（基数 5000 / 500 / 100），其中
/// 5000+n、500+n 与字符串规则同解，但 **基数 100 这一档与手册相冲**：
/// 手册明写 `101..171` 是 **LTE** 编码（101 = BAND_LTE_1），把 `141` 解成
/// `n41` 是在 NR 语境下凭空造出一个手册未定义的编码。现只保留手册规则，
/// 解不出来就返回 `None`，由调用方保留原始码并显式标注「编码未识别」。
pub fn nr_band_from_code(code: &str) -> Option<String> {
    let c = code.trim();
    // 手册规则：`50` + band 号（至少一位）
    let rest = c.strip_prefix("50")?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i32 = rest.parse().ok()?;
    if n < 1 || !nr_band_no_is_valid(n) {
        return None;
    }
    Some(format!("n{}", n))
}

/// 由 `AT+GTCCINFO?` 的 `<band>` 编码反解 LTE 频段名。
///
/// 手册 §11.1.14 / §11.1.16：`101` = BAND_LTE_1 … **`171` = BAND_LTE_71**，
/// 即 `100 + n`，上限就是 171。旧实现写成 `v > 100 && (1..=88).contains(&(v-100))`，
/// 等价于放行到 188，会把 `188` 这种手册未定义的编码解成 `B88`。
///
/// 另保留实机观测到的 band 号直写形态（`3` = B3、`41` = B41）作为兜底，
/// 频段号需落在 3GPP 已分配区间（1..=88，其中 16 未分配）。
pub fn lte_band_from_code(code: &str) -> Option<String> {
    let c = code.trim();
    if c.is_empty() || c.len() > 6 || !c.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v: i32 = c.parse().ok()?;

    // 手册定义的 100 + n，上限 171
    if (101..=171).contains(&v) {
        return Some(format!("B{}", v - 100));
    }
    // 实机 band 号直写兜底
    if (1..=88).contains(&v) && v != 16 {
        return Some(format!("B{}", v));
    }
    None
}

/// 由 `AT+GTCCINFO?` 的 `<band>` 编码反解 UMTS 频段名。
///
/// 手册 §11.1.14 的 `<umts_band>` 是一张**独立的 1..10 小表**，与 LTE 的
/// `100 + n`、NR 的 `"50" + n` 互不相干：
/// `1` = BAND_UMTS_I、`2` = BAND_UMTS_II … `10` = BAND_UMTS_X（罗马数字）。
///
/// 旧实现完全没处理这一档（`match` 里只有 "9" 与 "4"），WCDMA 下会把
/// 原始编码直接当频段名透出。另需注意：同一串数字在 `<rat>=20` 的
/// `AT+GTACT` 里会被模组当成 UMTS 频段（参见 BAND_PRESETS 的修正说明）。
pub fn umts_band_from_code(code: &str) -> Option<String> {
    let n: i32 = code.trim().parse().ok()?;
    // 先卡区间再下标：`n as usize - 1` 在 n = 0 时会整数下溢 panic，
    // 而 AT 回显里出现 0 / 空值 / 负数是常事，不能拿它直接做下标。
    if !(1..=10).contains(&n) {
        return None;
    }
    const ROMAN: [&str; 10] = [
        "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X",
    ];
    Some(format!("B{} ({})", n, ROMAN[(n - 1) as usize]))
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
    fn nr_band_code_follows_the_manual_string_rule() {
        // 手册 §11.1.14 / §11.1.16：NR 编码 = "50" 拼接 band 号
        assert_eq!(nr_band_from_code("501").as_deref(), Some("n1"));
        assert_eq!(nr_band_from_code("509").as_deref(), Some("n9"));   // 手册明列
        assert_eq!(nr_band_from_code("5010").as_deref(), Some("n10")); // 手册明列
        assert_eq!(nr_band_from_code("5041").as_deref(), Some("n41"));
        assert_eq!(nr_band_from_code("5078").as_deref(), Some("n78"));
        assert_eq!(nr_band_from_code("5079").as_deref(), Some("n79"));
        assert_eq!(nr_band_from_code("50100").as_deref(), Some("n100"));
        assert_eq!(nr_band_from_code("50512").as_deref(), Some("n512"));
        assert_eq!(nr_band_from_code("50257").as_deref(), Some("n257")); // FR2

        // 101..171 是手册定义的 **LTE** 编码，在 NR 语境下不得解成 n41；
        // 解不出来就返回 None，由调用方保留原始码并标注「编码未识别」。
        assert_eq!(nr_band_from_code("141"), None);

        // 3GPP 未分配且手册也未明列的编号，不得凭空造出频段名
        assert_eq!(nr_band_from_code("504"), None);  // n4 不存在
        assert_eq!(nr_band_from_code("5006"), None); // n6 不存在
        assert_eq!(nr_band_from_code("5017"), None); // n17 是 LTE 频段
        assert_eq!(nr_band_from_code("5027"), None); // n27 不存在

        // 非法输入
        assert_eq!(nr_band_from_code("500"), None);
        assert_eq!(nr_band_from_code("50"), None);
        assert_eq!(nr_band_from_code(""), None);
        assert_eq!(nr_band_from_code("n41"), None);
    }

    #[test]
    fn lte_band_code_follows_the_manual_range() {
        assert_eq!(lte_band_from_code("101").as_deref(), Some("B1"));
        assert_eq!(lte_band_from_code("171").as_deref(), Some("B71")); // 手册上限
        assert_eq!(lte_band_from_code("3").as_deref(), Some("B3"));    // 实机直写兜底
        assert_eq!(lte_band_from_code("41").as_deref(), Some("B41"));
        // 手册的 101..171 是连续枚举，按字面解码（模组不支持的编号
        // 本来就不会出现在 `AT+GTACT=?` 的候选列表里）
        assert_eq!(lte_band_from_code("116").as_deref(), Some("B16"));
        // 超出手册上限不得解成 B88
        assert_eq!(lte_band_from_code("188"), None);
        assert_eq!(lte_band_from_code("16"), None); // 直写形态下 B16 未分配
        assert_eq!(lte_band_from_code(""), None);
        assert_eq!(lte_band_from_code("B3"), None);
    }

    #[test]
    fn umts_band_uses_its_own_table() {
        // 手册 §11.1.14 <umts_band>：1 = BAND_UMTS_I … 10 = BAND_UMTS_X
        assert_eq!(umts_band_from_code("1").as_deref(), Some("B1 (I)"));
        assert_eq!(umts_band_from_code("3").as_deref(), Some("B3 (III)"));
        assert_eq!(umts_band_from_code("8").as_deref(), Some("B8 (VIII)"));
        assert_eq!(umts_band_from_code("10").as_deref(), Some("B10 (X)"));
        // 越界与非法输入一律 None，不猜
        assert_eq!(umts_band_from_code("0"), None);
        assert_eq!(umts_band_from_code("11"), None);
        assert_eq!(umts_band_from_code(""), None);
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
