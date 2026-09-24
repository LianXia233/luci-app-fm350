//! 模组温度（`AT+GTSENRDTEMP`）。
//!
//! ## 命令形式的坑
//!
//! **读形式 `AT+GTSENRDTEMP?` 在本模组上返回 `+CME ERROR: phone failure`**，
//! 必须使用带参数的写形式 `AT+GTSENRDTEMP=<sensor_id>`；`<sensor_id> = 0`
//! 表示一次返回全部 23 路传感器。返回值单位为 0.001 ℃
//! （实测 `+GTSENRDTEMP: 10,46066` → 46.066 ℃）。
//!
//! 传感器编号（手册 18.3 表）：1 = soc_max，2-4 = cpu_little0-2，7 = gpu1，
//! 8 = dramc，9 = mmsys，10 = md_5g（5G 基带），13 = soc_dram_ntc，
//! 14 = ltepa_ntc，15 = nrpa_ntc，16 = rf_ntc，19-22 = pmic 系列。
//! **读数为 0 表示该路未装配**，必须跳过而不是记成 0 ℃。

use crate::at::{at_cmd, AtHandle};
use crate::config::Config;
use crate::modem::run;

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

/// 读一次温度。全部通路读数均为 0（未装配）或命令失败时返回 `None`。
pub fn temperature(at: &AtHandle, cfg: &Config) -> Option<Temperature> {
    let r = run(at, cfg, at_cmd::GTSENRDTEMP_ALL).ok()?;

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
    let peak = sensors.iter().map(|(_, v)| *v).fold(None, |acc: Option<f64>, v| {
        match acc {
            Some(x) if x >= v => Some(x),
            _ => Some(v),
        }
    });

    Some(Temperature {
        modem: get(10),
        soc_max: get(1),
        peak,
        sensors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_names() {
        assert_eq!(sensor_name(10), "md_5g");
        assert_eq!(sensor_name(1), "soc_max");
        assert_eq!(sensor_name(15), "nrpa_ntc");
        assert_eq!(sensor_name(5), "");
        assert_eq!(sensor_name(0), "");
        assert_eq!(sensor_name(999), "");
    }

    #[test]
    fn rounding_keeps_one_decimal() {
        assert_eq!(round1_f64(46.066), 46.1);
        assert_eq!(round1_f64(47.2), 47.2);
        assert_eq!(round1_f64(-0.04), 0.0);
    }

    /// 读数精度：f64 的最短往返表示不得出现浮点噪声。
    #[test]
    fn temperature_json_has_no_float_noise() {
        let t = Temperature {
            modem: Some(round1_f64(45485.0 / 1000.0)),
            soc_max: Some(round1_f64(47200.0 / 1000.0)),
            peak: Some(round1_f64(47200.0 / 1000.0)),
            sensors: vec![
                (1, round1_f64(47200.0 / 1000.0)),
                (10, round1_f64(45485.0 / 1000.0)),
            ],
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"modem\":45.5"), "{}", s);
        assert!(s.contains("\"soc_max\":47.2"), "{}", s);
        assert!(!s.contains("47.200000"), "{}", s);
    }
}
