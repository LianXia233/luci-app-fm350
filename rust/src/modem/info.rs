//! 模组标识信息（`AT+CGMI` / `CGMM` / `CGMR` / `CIMI` / `ICCID` ...）。

use std::collections::BTreeMap;

use super::run;
use crate::at::{at_cmd, AtHandle};
use crate::config::Config;

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

/// 采集项与命令的对应表。
///
/// IMEI 走 `AT+EGMREXT=0,7`（首参 0 = 读）：`AT+CGSN` 在部分固件上不回，
/// 而 `+EGMREXT` 是社区与实机都验证可用的扩展读命令。
const FIELDS: &[(&str, &str)] = &[
    ("manufacturer", at_cmd::CGMI),
    ("model", at_cmd::CGMM),
    ("firmware", at_cmd::CGMR),
    ("imei", at_cmd::EGMREXT_READ_IMEI),
    ("imsi", at_cmd::CIMI),
    ("iccid", at_cmd::ICCID),
    ("serial", at_cmd::CGSN),
    ("usb_mode", at_cmd::GTUSBMODE_READ),
    ("sim_slot", at_cmd::GTDUALSIM_READ),
    ("sms_center", at_cmd::CSCA_READ),
];

pub fn info(at: &AtHandle, cfg: &Config) -> ModemInfo {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (key, cmd) in FIELDS {
        let raw = run(at, cfg, cmd).unwrap_or_default();
        map.insert((*key).to_string(), clean_value(&raw, cmd));
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
///
/// 无前缀命令（`AT+CGMI` 这类只回裸字符串的命令）直接取该行。
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
        // 结果码里的 `+CME ERROR: <text>` / `+CMS ERROR: <text>` **不是值**。
        //
        // 原实现只挡了无前缀的 `ERROR`，于是 `AT+GTSENRDTEMP?`（本模组回
        // `+CME ERROR: phone failure`）这类失败响应会被当成正常取值返回，
        // 前端把 "phone failure" 显示成固件版本/厂商名。这里一并挡掉。
        if l.starts_with("+CME ERROR") || l.starts_with("+CMS ERROR") {
            return String::new();
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
        if prefix.is_empty() || !l.starts_with('+') {
            return l.to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixed_values_are_extracted() {
        assert_eq!(clean_value("+CGMI: Fibocom\nOK", "AT+CGMI"), "Fibocom");
        assert_eq!(
            clean_value("+GTUSBMODE: 40\nOK", "AT+GTUSBMODE?"),
            "40"
        );
    }

    #[test]
    fn bare_responses_are_taken_as_is() {
        // AT+CGMM 只回裸字符串，没有 +XXX: 前缀
        assert_eq!(clean_value("FM350-GL\nOK", "AT+CGMM"), "FM350-GL");
    }

    /// 错误响应必须返回空串，不能被当成值（原实现会把 "phone failure" 当值返回）。
    #[test]
    fn empty_and_error_responses_yield_empty_string() {
        assert_eq!(clean_value("", "AT+CGMI"), "");
        assert_eq!(clean_value("+CME ERROR: phone failure", "AT+CGMR"), "");
        assert_eq!(clean_value("+CMS ERROR: 500", "AT+CGMR"), "");
        assert_eq!(clean_value("ERROR", "AT+CGMR"), "");
        assert_eq!(clean_value("OK", "AT+CGMR"), "");
    }
}
