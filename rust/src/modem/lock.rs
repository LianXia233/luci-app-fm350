//! 网络偏好控制：锁频段 / 锁小区 / 制式优先级 / SIM / CFUN / USB 模式。
//!
//! ## 下电—下发—上电序列
//!
//! 锁频（`+GTACT`）与锁小区（`+EMMCHLCK`）都要求模组处于离线态，因此统一
//! 走 `CFUN=0` → 下发 → `CFUN=1`。中间那条的失败必须冒泡给调用方
//! （`?`），首尾两条是 best-effort —— 个别固件对重复 `CFUN=0` 回 ERROR。

use super::{run, run_list};
use crate::at::{at_cmd, AtHandle, AtResult};
use crate::config::Config;

/// 锁制式与频段（`AT+GTACT`）。
///
/// `<rat>` 取值（手册 §11.1.14）：1 = UMTS，2 = LTE，4 = LTE/UMTS，
/// **10 = Automatic**，14 = NR-RAN，16 = NR-RAN/WCDMA，17 = NR-RAN/LTE，
/// 20 = NR-RAN/WCDMA/LTE。**20 并非「自动」，20 是三模全开；10 才是 Automatic。**
/// 手册 Note 6 说明：下发 10（自动）之后查询会回显 20，两者极易混淆。
///
/// **参数是位置参数**，频段前必须给 `<PreferredAct1>`、`<PreferredAct2>`
/// 留出位置（不需要时用空位占位）：
///
/// ```text
/// AT+GTACT=<rat>[,[<PreferredAct1>],[<PreferredAct2>][,<band_1>[,<band_2>…]]]
/// ```
///
/// 手册 Note 3：三模（rat=20）下 PreferredAct1/2 只有 RAT Combination Table
/// 里那十种组合合法，**其余一律 rejected**。把频段写在第 2 位（如
/// `20,5041,5078`）会被当成 PreferredAct1=5041 而直接被拒。
///
/// 频段编码：LTE 为 `100 + n`（101 = B1 … 171 = B71）；NR 为 `"50"` 与
/// band 号十进制拼接（501 = n1 … 5041 = n41 … 50512 = n512）；
/// UMTS 为 1..10；0 = 自动选频。
///
/// 例：`AT+GTACT=20,6,3,5078` = 三模 + NR 优先于 LTE + 锁 n78。
pub fn lock_band(at: &AtHandle, cfg: &Config, args: &[String]) -> AtResult<Vec<(String, String)>> {
    if args.is_empty() {
        return Err("缺少 AT+GTACT 参数，例如 14 / 2 / 20 / 20,6,3,5078".to_string());
    }
    // 下发前先在本地校验：本函数走的是 CFUN=0 → 下发 → CFUN=1 的下电上电
    // 序列，一次失败就意味着白付一次数十秒的断网。手册把合法参数写得很死
    // （rat 枚举、PreferredAct 仅 2/3/6、三模组合表），没理由等到模组报错。
    validate_gtact(args)?;
    offline_apply(at, cfg, &at_cmd::set_gtact(&args.join(",")))
}

/// 手册 §11.1.14 定义的 `<rat>` 合法枚举。
const GTACT_RATS: [u32; 8] = [1, 2, 4, 10, 14, 16, 17, 20];

/// 手册 §11.1.14 定义的 `<PreferredAct1>` / `<PreferredAct2>` 合法值：
/// 2 = WCDMA preferred、3 = LTE preferred、6 = NR-RAN preferred。
const GTACT_PREFS: [u32; 3] = [2, 3, 6];

/// 校验 `AT+GTACT` 的位置参数。不合法时返回可直接展示的中文错误。
fn validate_gtact(args: &[String]) -> AtResult<()> {
    let tok = |i: usize| args.get(i).map(|s| s.trim()).unwrap_or("");

    // 第 1 位：<rat>
    let rat: u32 = tok(0)
        .parse()
        .map_err(|_| format!("<rat> 必须是整数，当前为 {:?}", tok(0)))?;
    if !GTACT_RATS.contains(&rat) {
        return Err(format!(
            "<rat>={} 不在手册 §11.1.14 的枚举内，合法值为 {:?}（10 = 自动，20 = 三模全开）",
            rat, GTACT_RATS
        ));
    }

    // 第 2、3 位：<PreferredAct1> / <PreferredAct2>，允许留空（= Not stated）
    let pref = |i: usize| -> AtResult<Option<u32>> {
        let t = tok(i);
        if t.is_empty() {
            return Ok(None);
        }
        let v: u32 = t.parse().map_err(|_| {
            format!(
                "第 {} 位是 <PreferredAct{}>，必须是整数或留空，当前为 {:?}。\
                 频段参数要从第 4 位开始写，例如 20,6,3,5078 或 20,,,5078",
                i + 1,
                i,
                t
            )
        })?;
        if !GTACT_PREFS.contains(&v) {
            return Err(format!(
                "<PreferredAct{}>={} 非法，手册只允许 2(WCDMA) / 3(LTE) / 6(NR-RAN)。\
                 若本意是锁频段，请把它移到第 4 位之后",
                i, v
            ));
        }
        Ok(Some(v))
    };
    let p1 = pref(1)?;
    let p2 = pref(2)?;

    // 手册 Note 3 + RAT Combination Table：三模下只有这十种组合合法
    if rat == 20 {
        let ok = matches!(
            (p1, p2),
            (None, None)
                | (Some(2), None)
                | (Some(3), None)
                | (Some(6), None)
                | (Some(2), Some(3))
                | (Some(2), Some(6))
                | (Some(3), Some(2))
                | (Some(3), Some(6))
                | (Some(6), Some(2))
                | (Some(6), Some(3))
        );
        if !ok {
            return Err(format!(
                "三模（rat=20）下 PreferredAct 组合 ({:?},{:?}) 不在手册 RAT Combination Table 内，\
                 模组会直接 rejected",
                p1, p2
            ));
        }
    }

    // 第 4 位起：<band_n>，允许空位（未指定则自动选频）
    for (k, item) in args.iter().enumerate().skip(3) {
        let t = item.trim();
        if t.is_empty() {
            continue;
        }
        if t.parse::<u32>().is_err() {
            return Err(format!(
                "第 {} 个频段参数 {:?} 不是非负整数（手册编码：LTE=100+n、NR=\"50\"+n、UMTS=1..10、0=自动）",
                k + 1,
                t
            ));
        }
    }
    Ok(())
}

/// 锁小区 / PCI（`AT+EMMCHLCK=1,<...>,<arfcn>,<pci>,<...>`，取消为 `=0`）。
pub fn lock_cell(at: &AtHandle, cfg: &Config, args: &[String]) -> AtResult<Vec<(String, String)>> {
    if args.is_empty() {
        return Err("缺少 AT+EMMCHLCK 参数，例如 1,11,0,627264,280,3 或 0（取消）".to_string());
    }
    offline_apply(at, cfg, &at_cmd::set_emmchclk(&args.join(",")))
}

/// 下电 → 下发 → 上电，返回三条命令的回显。
///
/// CFUN 字面量统一走 [`at_cmd::set_cfun`]，不在本文件重复书写 ——
/// `at/cmd.rs` 开头声明了自己是所有命令字面量的**唯一出处**，
/// 这里硬写 `"AT+CFUN=0"` 会让日后改命令形态时漏改一处。
fn offline_apply(at: &AtHandle, cfg: &Config, cmd: &str) -> AtResult<Vec<(String, String)>> {
    let off = at_cmd::set_cfun(0);
    let on = at_cmd::set_cfun(1);
    let mut out = Vec::new();
    out.push((off.clone(), run(at, cfg, &off).unwrap_or_default()));
    out.push((cmd.to_string(), run(at, cfg, cmd)?));
    out.push((on.clone(), run(at, cfg, &on).unwrap_or_default()));
    Ok(out)
}

/// 查询当前锁定状态。
pub fn lock_status(at: &AtHandle, cfg: &Config) -> Vec<(String, String)> {
    run_list(at, cfg, &[at_cmd::GTACT_READ, at_cmd::EMMCHLCK_READ])
}

/// 邻区 / 服务小区信息。
pub fn cell_info(at: &AtHandle, cfg: &Config) -> Vec<(String, String)> {
    run_list(at, cfg, &[at_cmd::GTCCINFO_READ, at_cmd::GTCAINFO_READ])
}

/// 制式名 / 编码 → `+EPRATL` 的 `<rat>` 编码。
///
/// 2 = UMTS，4 = LTE，128 = NR。同时接受已是编码的数字字符串。
/// **这里的编码与 `+GTACT` 的 `<rat>` 不同**：`+GTACT` 用 1/2/4/10/14…，
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
/// 手册 11.1.13 只描述了写形式，未列出读形式；但 2026-09-22 实机实测
/// （FM350-GL）`AT+EPRATL?` 可正常返回 `+EPRATL:<num>,<rat…>` —— 本次回读
/// `+EPRATL:2,128,4`，即「2 个优先制式，NR(128) 优先于 LTE(4)」。
///
/// 与 `lock_status()` 的分工：`AT+GTACT?` 返回的是**当前选定制式与频段锁定**，
/// 语义是「锁网锁频配置」，不是优先顺序列表，不能代替本函数。
pub fn rat_order(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(at, cfg, at_cmd::EPRATL_READ)
}

/// 设置制式优先顺序（`AT+EPRATL`）。
///
/// 历史修正：本函数原使用 `AT+QNWPREFCFG`（高通 / Quectel 平台专有），
/// FM350-GL（联发科 T700）不支持，属跨平台误植。
/// 手册 §11.1.13 的 `<RAT num>` 还定义了 **0 = No prefer**（清除优先级），
/// 旧实现无法表达这一档（空列表直接被拒），于是用户一旦设了优先级
/// 就只能再设一组、没法恢复默认。现支持传 `0` / `none` / `off` / `clear` 显式清除。
pub fn set_rat_order(at: &AtHandle, cfg: &Config, order: &[String]) -> AtResult<String> {
    // 清除优先级：AT+EPRATL=0
    let only = order.first().map(|s| s.trim().to_ascii_lowercase()).unwrap_or_default();
    if order.len() == 1 && matches!(only.as_str(), "0" | "none" | "off" | "clear" | "auto") {
        return run(at, cfg, "AT+EPRATL=0");
    }

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
    run(at, cfg, &at_cmd::set_eprartl(&codes))
}

/// 切换 SIM 卡槽。
///
/// FM350 只有 0 / 1 两个卡槽；非法值直接拒绝，避免把 `AT+GTDUALSIM=7`
/// 这类无意义指令发给模组（部分固件会回 OK 但状态不变，用户误以为成功）。
pub fn set_sim_slot(at: &AtHandle, cfg: &Config, slot: u32) -> AtResult<String> {
    if slot > 1 {
        return Err(format!("SIM 卡槽只能是 0 或 1，当前为 {}", slot));
    }
    run(at, cfg, &at_cmd::set_sim_slot(slot))
}

/// 在线/飞行模式。允许 0（最低功能）、1（在线）、4（飞行模式）。
pub fn set_cfun(at: &AtHandle, cfg: &Config, mode: u32) -> AtResult<String> {
    if !matches!(mode, 0 | 1 | 4) {
        return Err(format!("CFUN 模式只能是 0、1 或 4，当前为 {}", mode));
    }
    run(at, cfg, &at_cmd::set_cfun(mode))
}

/// 设置 USB 模式。
///
/// 手册 §13.1.1 对本机型只定义了两档：
///   * `40` = RNDIS+AT+AP(GNSS)+META+DEBUG+NPT+ADB
///   * `41` = 在 40 基础上额外开 AP(LOG)+AP(META)（**出厂默认值**）
///
/// 旧实现只卡了 `1..=255`，于是 `AT+GTUSBMODE=7` 这类手册未定义的值也能下去。
/// 本命令的副作用是 **USB 重新枚举**（AT 口与数据网卡都会短暂消失，
/// 前端提示等待 20–30 秒），把一个非法值发下去的代价是一次无谓的断网，
/// 因此改成严格白名单。前端 settings.js 本来就只给 40/41 两个选项，
/// 此处是把同一道约束补到 CLI 与 HTTP 接口上。
pub fn set_usb_mode(at: &AtHandle, cfg: &Config, mode: u32) -> AtResult<String> {
    if !matches!(mode, 40 | 41) {
        return Err(format!(
            "USB 模式只能是 40（标准：RNDIS+AT+GNSS+META+DEBUG+NPT+ADB）或 \
             41（诊断：额外开 AP(LOG)+AP(META)，手册 §13.1.1），当前为 {}",
            mode
        ));
    }
    run(at, cfg, &at_cmd::set_usb_mode(mode))
}

/// 重启模组。
pub fn reboot(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    run(at, cfg, at_cmd::REBOOT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rat_code_accepts_names_and_codes() {
        assert_eq!(rat_code("NR"), Some("128"));
        assert_eq!(rat_code("5G"), Some("128"));
        assert_eq!(rat_code("LTE"), Some("4"));
        assert_eq!(rat_code("4G"), Some("4"));
        assert_eq!(rat_code("WCDMA"), Some("2"));
        assert_eq!(rat_code("UMTS"), Some("2"));
        assert_eq!(rat_code("128"), Some("128"));
        assert_eq!(rat_code("gsm"), None);
        assert_eq!(rat_code(""), None);
    }

    /// +EPRATL 与 +GTACT 的 rat 编码不是同一套，不能互相套用。
    #[test]
    fn epratl_encoding_differs_from_gtact() {
        assert_eq!(at_cmd::set_eprartl(&["128", "4"]), "AT+EPRATL=2,128,4");
        // GTACT 的自动是 10，20 是三模全开
        assert_eq!(at_cmd::set_gtact("10"), "AT+GTACT=10");
    }

    /// 非法参数必须在触碰 AT 口之前就被拒绝（AtHandle 未打开任何端口）。
    #[test]
    fn invalid_hardware_args_are_rejected_locally() {
        let at = AtHandle::new();
        let cfg = Config::default();
        assert!(set_sim_slot(&at, &cfg, 2).is_err());
        assert!(set_cfun(&at, &cfg, 9).is_err());
        assert!(set_usb_mode(&at, &cfg, 0).is_err());
        assert!(set_usb_mode(&at, &cfg, 256).is_err());
        assert!(set_usb_mode(&at, &cfg, 4000).is_err());
    }

    /// 手册 §13.1.1 对本机型只定义了 40 / 41 两档。
    /// 旧实现放行 1..=255，会把未定义值发下去并白白触发一次 USB 重枚举。
    #[test]
    fn usb_mode_is_whitelisted_to_the_documented_values() {
        let at = AtHandle::new();
        let cfg = Config::default();
        for bad in [1u32, 7, 39, 42, 255] {
            let e = set_usb_mode(&at, &cfg, bad).unwrap_err();
            assert!(e.contains("40"), "mode={} 的错误未说明合法值：{}", bad, e);
        }
    }

    /// 手册 §11.1.14：`AT+GTACT` 是位置参数，频段必须从第 4 位开始，
    /// 且三模下的 PreferredAct 组合必须在 RAT Combination Table 内。
    #[test]
    fn gtact_positional_args_are_validated() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();

        // 合法：lock.rs 文档里的例子、以及前端 BAND_PRESETS 修正后的形式
        assert!(validate_gtact(&s(&["20"])).is_ok());
        assert!(validate_gtact(&s(&["20", "6", "3", "5078"])).is_ok());
        assert!(validate_gtact(&s(&["20", "6", "3", "5041", "5078", "5079"])).is_ok());
        assert!(validate_gtact(&s(&["20", "3", "6", "101", "103", "105", "108"])).is_ok());
        assert!(validate_gtact(&s(&["20", "", "", "5041"])).is_ok()); // 空位占位
        assert!(validate_gtact(&s(&["10"])).is_ok());                 // 10 = Automatic
        assert!(validate_gtact(&s(&["17", "6"])).is_ok());            // 双模只第 2 位有效

        // 非法：旧版 BAND_PRESETS 的形状 —— 频段被当成了 PreferredAct
        let e = validate_gtact(&s(&["20", "5041", "5078", "5079"])).unwrap_err();
        assert!(e.contains("PreferredAct1"), "{}", e);
        // 手册 Note 3：三模下 (2,2) 不在 RAT Combination Table 内
        assert!(validate_gtact(&s(&["20", "2", "2"])).is_err());
        // (未指定, 3) 同样不在表内：表里只有 (2/3/6, 未指定) 与成对的六种组合
        assert!(validate_gtact(&s(&["20", "", "3", "5041"])).is_err());
        // <rat> 枚举外的值
        assert!(validate_gtact(&s(&["3"])).is_err());
        assert!(validate_gtact(&s(&["21"])).is_err());
        // PreferredAct 只允许 2/3/6
        assert!(validate_gtact(&s(&["20", "1", "3"])).is_err());
        assert!(validate_gtact(&s(&["20", "4", ""])).is_err());
        // 频段参数必须是非负整数
        assert!(validate_gtact(&s(&["20", "6", "3", "n78"])).is_err());
    }

    /// 手册 §11.1.13 的 `<RAT num>` = 0 表示 No prefer，应能下发清除命令。
    #[test]
    fn rat_order_can_clear_preference() {
        let at = AtHandle::new();
        let cfg = Config::default();
        // 未开端口，故这里只验证「没在参数校验阶段就被拒」：
        // 错误应来自 AT 通道而不是参数校验。
        for token in ["0", "none", "clear"] {
            let e = set_rat_order(&at, &cfg, &[token.to_string()]).unwrap_err();
            assert!(!e.contains("缺少有效制式"), "{} 被误当成非法制式：{}", token, e);
        }
    }
}
