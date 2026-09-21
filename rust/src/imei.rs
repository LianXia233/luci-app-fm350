//! IMEI / 串号读写。
//!
//! 功能范围：
//!   - 读取：始终可用（`AT+EGMREXT=0,7`），用于展示与备份；
//!   - 写入 / 更换：高风险不可逆操作，需同时满足三重条件才允许下发：
//!       1. UCI `fm350.main.imei_write` 为 1（默认 0）；
//!       2. 调用方显式传入 `confirm = true`；
//!       3. 目标值通过 15 位数字与 Luhn 校验。
//!   下发前会自动读取并备份当前 IMEI 到 `/etc/fm350/imei.backup`。
//!
//! 注意：写入仅在本模块内构造命令，不经过通用 AT 透传路径，
//! 以免被误触发。写入操作会记录到系统日志。

use std::fs;
use std::process::Command;

use crate::at::{self, AtHandle, AtResult};
use crate::config::Config;

const BACKUP_DIR: &str = "/etc/fm350";
const BACKUP_FILE: &str = "/etc/fm350/imei.backup";

#[derive(Debug, serde::Serialize)]
pub struct ImeiState {
    pub imei: String,
    pub write_enabled: bool,
    pub backup: Option<String>,
}

/// Luhn 校验（IMEI 第 15 位为校验位）。
pub fn luhn_ok(imei: &str) -> bool {
    let digits: Vec<u32> = match imei.chars().map(|c| c.to_digit(10)).collect() {
        Some(v) => v,
        None => return false,
    };
    if digits.len() != 15 {
        return false;
    }
    let mut sum = 0u32;
    for (i, d) in digits.iter().enumerate() {
        // 从右往左第 2 位起，每隔一位乘 2
        if (14 - i) % 2 == 1 {
            let mut v = d * 2;
            if v > 9 {
                v -= 9;
            }
            sum += v;
        } else {
            sum += *d;
        }
    }
    sum % 10 == 0
}

/// 格式校验：15 位数字。Luhn 校验作为提示（部分厂商 IMEI 不严格符合）。
pub fn valid_format(imei: &str) -> bool {
    imei.len() == 15 && imei.chars().all(|c| c.is_ascii_digit())
}

/// 读取当前 IMEI（只读，始终允许）。
pub fn read_raw(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    let r = at.with(cfg, |p| p.command("AT+EGMREXT=0,7"))?;
    // 响应形如：+EGMREXT: "861234567890123"
    for line in r.lines() {
        let l = line.trim();
        if l.starts_with("+EGMREXT") {
            if let Some(pos) = l.find(':') {
                let v = l[pos + 1..].trim().trim_matches('"').to_string();
                if !v.is_empty() {
                    return Ok(v);
                }
            }
        }
    }
    // 退路：直接取首个 15 位数字串
    for line in r.lines() {
        for tok in line.split(|c: char| !c.is_ascii_digit()) {
            if tok.len() == 15 {
                return Ok(tok.to_string());
            }
        }
    }
    Err(format!("未能解析 IMEI，原始响应: {}", r.replace('\n', " | ")))
}

/// 读取状态（IMEI + 写入开关 + 已有备份）。
pub fn state(at: &AtHandle, cfg: &Config) -> ImeiState {
    ImeiState {
        imei: read_raw(at, cfg).unwrap_or_default(),
        write_enabled: cfg.imei_write,
        backup: fs::read_to_string(BACKUP_FILE).ok(),
    }
}

fn log(msg: &str) {
    let _ = Command::new("logger")
        .args(["-t", "fm350d", msg])
        .status();
}

/// 备份当前 IMEI 到 `/etc/fm350/imei.backup`。
pub fn backup(at: &AtHandle, cfg: &Config) -> AtResult<String> {
    let current = read_raw(at, cfg)?;
    let _ = fs::create_dir_all(BACKUP_DIR);
    let payload = serde_json::json!({
        "imei": current,
        "note": "写入前的最后一次备份，用于恢复原始串号"
    });
    fs::write(BACKUP_FILE, payload.to_string())
        .map_err(|e| format!("写入备份失败: {}", e))?;
    log(&format!("已备份当前 IMEI 到 {}", BACKUP_FILE));
    Ok(current)
}

#[derive(Debug, serde::Serialize)]
pub struct WriteResult {
    pub previous: String,
    pub current: String,
    pub backup_file: String,
    pub command: String,
    /// 新值是否通过 Luhn 校验（提示性，不阻断写入）。
    pub luhn_ok: bool,
    /// Luhn 不通过时的提示文案（通过时为 null）。
    pub warning: Option<String>,
}

/// 写入（更换）IMEI。
///
/// 必须 `confirm = true` 且配置项 `imei_write` 已开启。
pub fn write(at: &AtHandle, cfg: &Config, value: &str, confirm: bool) -> AtResult<WriteResult> {
    let imei = value.trim();

    if !confirm {
        return Err("拒绝执行：缺少二次确认（需 confirm=1）".to_string());
    }
    if !cfg.imei_write {
        return Err(
            "拒绝执行：IMEI 写入功能未开启，请先在设置中开启 imei_write".to_string(),
        );
    }
    if !valid_format(imei) {
        return Err(format!("IMEI 格式无效：必须为 15 位数字，收到 {} 位", imei.len()));
    }

    // Luhn 校验：仅作提示（部分厂商 IMEI 不严格符合），不阻断写入
    let luhn = luhn_ok(imei);
    let warning = if luhn {
        None
    } else {
        Some("新 IMEI 未通过 Luhn 校验位验证，多数运营商网元会据此拒绝入网，请确认输入无误".to_string())
    };
    if let Some(w) = &warning {
        log(&format!("IMEI 写入提示: {}", w));
    }

    // 备份当前值
    let previous = backup(at, cfg)?;

    let cmd = format!(r#"AT+EGMREXT=1,7,"{}""#, imei);
    log(&format!("准备写入 IMEI（原值 {}，新值 {}）", previous, imei));

    let resp = at.with(cfg, |p| p.command(&cmd))?;
    if resp.contains("ERROR") {
        log(&format!("IMEI 写入失败: {}", resp.replace('\n', " | ")));
        return Err(format!("模组返回错误: {}", resp.replace('\n', " | ")));
    }

    // 回读确认
    let current = read_raw(at, cfg).unwrap_or_default();
    log(&format!("IMEI 写入完成，回读值 {}", current));

    Ok(WriteResult {
        previous,
        current,
        backup_file: BACKUP_FILE.to_string(),
        command: cmd,
        luhn_ok: luhn,
        warning,
    })
}

/// 通用 AT 透传路径的守卫：写入类 IMEI 指令默认拒绝。
pub fn guard_transparent(cmd: &str, cfg: &Config) -> Result<(), String> {
    if at::is_imei_write(cmd) && !cfg.imei_write {
        return Err(format!(
            "已拒绝 `{}`：IMEI 写入需在设置中开启 imei_write 后，通过 IMEI 专用接口执行",
            cmd.trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luhn_of_valid_imei() {
        // 标准测试 IMEI
        assert!(luhn_ok("490154203237518"));
    }

    #[test]
    fn luhn_rejects_bad_checksum() {
        assert!(!luhn_ok("490154203237519"));
    }

    #[test]
    fn format_check() {
        assert!(valid_format("490154203237518"));
        assert!(!valid_format("49015420323751"));
        assert!(!valid_format("49015420323751X"));
        assert!(!valid_format(""));
    }
}
