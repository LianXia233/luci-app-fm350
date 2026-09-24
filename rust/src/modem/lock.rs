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
/// `<rat>` 取值：1 = UMTS，2 = LTE，4 = LTE/UMTS，**10 = Automatic**，
/// 14 = NR-RAN，16 = NR-RAN/WCDMA，17 = NR-RAN/LTE，20 = NR-RAN/WCDMA/LTE。
/// **20 并非「自动」，20 是三模全开；10 才是 Automatic。**
/// 手册 Note 6 说明：下发 10（自动）之后查询会回显 20，两者极易混淆。
///
/// 频段编码：LTE 为 `100 + n`（101 = B1 … 171 = B71）；NR 为 `"50"` 与
/// band 号十进制拼接（501 = n1 … 5041 = n41 … 50512 = n512）；0 = 自动。
///
/// 例：`AT+GTACT=20,6,3,5078` = 三模 + NR 优先于 LTE + 锁 n78。
pub fn lock_band(at: &AtHandle, cfg: &Config, args: &[String]) -> AtResult<Vec<(String, String)>> {
    if args.is_empty() {
        return Err("缺少 AT+GTACT 参数，例如 14 / 2 / 20 / 20,6,3,5078".to_string());
    }
    offline_apply(at, cfg, &at_cmd::set_gtact(&args.join(",")))
}

/// 锁小区 / PCI（`AT+EMMCHLCK=1,<...>,<arfcn>,<pci>,<...>`，取消为 `=0`）。
pub fn lock_cell(at: &AtHandle, cfg: &Config, args: &[String]) -> AtResult<Vec<(String, String)>> {
    if args.is_empty() {
        return Err("缺少 AT+EMMCHLCK 参数，例如 1,11,0,627264,280,3 或 0（取消）".to_string());
    }
    offline_apply(at, cfg, &at_cmd::set_emmchclk(&args.join(",")))
}

/// 下电 → 下发 → 上电，返回三条命令的回显。
fn offline_apply(at: &AtHandle, cfg: &Config, cmd: &str) -> AtResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    out.push(("AT+CFUN=0".into(), run(at, cfg, "AT+CFUN=0").unwrap_or_default()));
    out.push((cmd.to_string(), run(at, cfg, cmd)?));
    out.push(("AT+CFUN=1".into(), run(at, cfg, "AT+CFUN=1").unwrap_or_default()));
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
    run(at, cfg, &at_cmd::set_eprartl(&codes))
}

/// 切换 SIM 卡槽。
pub fn set_sim_slot(at: &AtHandle, cfg: &Config, slot: u32) -> AtResult<String> {
    run(at, cfg, &at_cmd::set_sim_slot(slot))
}

/// 在线/飞行模式。
pub fn set_cfun(at: &AtHandle, cfg: &Config, mode: u32) -> AtResult<String> {
    run(at, cfg, &at_cmd::set_cfun(mode))
}

/// 设置 USB 模式（40 = RNDIS+AT）。
///
/// 注意：切换会触发 USB 重新枚举，AT 口与数据网卡都会短暂消失。
pub fn set_usb_mode(at: &AtHandle, cfg: &Config, mode: u32) -> AtResult<String> {
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
}
