//! 短信（PDU 模式）。
//!
//! FM350 与模组原厂 WebUI 一致使用 PDU 模式（不是 TEXT 模式）：TEXT 模式下
//! 中文与长短信都不可靠。
//!
//! ## 子模块
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`gsm7`] | GSM 03.38 / UCS2 编解码 |
//! | [`pdu`] | PDU 字段解析与构造 |
//! | 本文件 | AT 交互与对外操作 |
//!
//! ## 交互序列
//!
//! ```text
//! 列表：AT+CMGF=0 → AT+CSCS="GSM" → AT+CMGL=4
//! 发送：AT+CMGF=0 → AT+CSCS="GSM" → AT+CMGS=<tpdu 长度>
//!       → 等 '>' → 写 PDU + Ctrl-Z(0x1A) → 等 OK
//! 删除：AT+CMGD=<index>
//! 存储：AT+CPMS? / AT+CPMS="SM","SM","SM"
//! ```
//!
//! 实机 `AT+CPMS=?` **只返回 `("SM")`**，不支持 `"ME"` —— 传 ME 会直接
//! `ERROR`，所以默认值只能是 SM。

use std::time::Duration;

use crate::at::{at_cmd, is_imei_write, AtHandle, AtResult};
use crate::config::Config;

pub mod gsm7;
pub mod pdu;

pub use pdu::{decode_pdu, parse_list, parse_storage};

/// PDU 发送结束符。
const CTRL_Z: u8 = 0x1A;

/// 等待模组给出 `>` 提示符的超时。
const PROMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// 一条短信（解析结果）。
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Sms {
    /// SIM 上的存储序号（`AT+CMGD` 用它定位）。
    pub index: i64,
    /// 读取状态（0 = 已读，1 = 未读等，按 3GPP TS 27.005）。
    pub status: i64,
    pub sender: String,
    /// `20YY-MM-DD HH:MM:SS`。
    pub timestamp: String,
    pub text: String,
    /// `GSM7` / `UCS2` / `8BIT`。
    pub encoding: String,
    /// 原始 PDU。解析有误时这是复盘的唯一证据，必须保留。
    pub raw_pdu: String,
}

/// 短信存储用量。
#[derive(Debug, Default, serde::Serialize)]
pub struct Storage {
    pub mem: String,
    pub used: i64,
    pub total: i64,
}

/// 执行一条短信类 AT 命令（顺带挡住 IMEI 写入类命令）。
///
/// 短信模块自己不该被拿去写号，但 `list/send/...` 的命令串有一部分由外部
/// 参数拼成（如 `AT+CMGD=<index>`），统一过一次守卫成本极低。
fn run(at: &AtHandle, cfg: &Config, cmd: &str) -> AtResult<String> {
    if is_imei_write(cmd) {
        return Err(crate::at::imei_block_reason(cmd));
    }
    at.with(cfg, |p| p.command(cmd))
}

/// 列出全部短信。
pub fn list(at: &AtHandle, cfg: &Config) -> AtResult<Vec<Sms>> {
    let _ = run(at, cfg, at_cmd::CMGF_PDU)?;
    let _ = run(at, cfg, at_cmd::CSCS_GSM)?;
    let resp = run(at, cfg, at_cmd::CMGL_ALL)?;
    Ok(parse_list(&resp))
}

/// 发送短信。
///
/// 编码选择由 `build_submit_pdu` 决定：能进 GSM 7-bit 表就用 7-bit，否则
/// UCS2（中文）。**不在这里截断长度** —— 超长短信应由调用方提示，而不是
/// 悄悄截断后发出去。
pub fn send(at: &AtHandle, cfg: &Config, number: &str, text: &str) -> AtResult<String> {
    if text.is_empty() {
        return Err("短信内容不能为空".to_string());
    }
    let (pdu, tpdu_len) = pdu::build_submit_pdu(number, text)?;

    let _ = run(at, cfg, at_cmd::CMGF_PDU)?;
    let _ = run(at, cfg, at_cmd::CSCS_GSM)?;

    at.with(cfg, |p| {
        // 下发 AT+CMGS 后模组回 '>' 提示符，再写入 PDU 与 Ctrl-Z
        let _ = p.command(&at_cmd::cmgs(tpdu_len));
        p.wait_for(">", PROMPT_TIMEOUT)?;
        p.write_raw(pdu.as_bytes())?;
        p.write_raw(&[CTRL_Z])?;
        p.wait_for("OK", Duration::from_secs(cfg.at_timeout.max(30)))
    })
    .map(|r| r.replace('\n', " "))
}

/// 删除指定序号的短信。
pub fn delete(at: &AtHandle, cfg: &Config, index: i64) -> AtResult<String> {
    run(at, cfg, &at_cmd::cmgd(index))
}

/// 查询短信存储用量。
pub fn storage(at: &AtHandle, cfg: &Config) -> AtResult<Storage> {
    let r = run(at, cfg, at_cmd::CPMS_READ)?;
    Ok(parse_storage(&r))
}

/// 设置短信存储（三处都设成同一个：`AT+CPMS=<mem>,<mem>,<mem>`）。
pub fn set_storage(at: &AtHandle, cfg: &Config, mem: &str) -> AtResult<String> {
    run(at, cfg, &at_cmd::cpms_set(mem))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GSM7 与 UCS2 命令字面量集中定义，避免多处硬写。
    #[test]
    fn sms_command_literals_are_centralized() {
        assert_eq!(at_cmd::CMGF_PDU, "AT+CMGF=0");
        assert_eq!(at_cmd::CSCS_GSM, "AT+CSCS=\"GSM\"");
        assert_eq!(at_cmd::CMGL_ALL, "AT+CMGL=4");
        assert_eq!(at_cmd::CPMS_READ, "AT+CPMS?");
        assert_eq!(at_cmd::cmgd(3), "AT+CMGD=3");
        assert_eq!(at_cmd::cmgs(20), "AT+CMGS=20");
        // 三处存储槽都要设，实机只支持 SM
        assert_eq!(at_cmd::cpms_set("SM"), "AT+CPMS=\"SM\",\"SM\",\"SM\"");
    }

    #[test]
    fn empty_text_is_rejected_before_touching_modem() {
        let at = AtHandle::new();
        let cfg = Config::default();
        assert!(send(&at, &cfg, "10086", "").is_err());
    }

    /// 号码非法时必须在构造 PDU 阶段就失败，不能把无效 PDU 发给模组。
    #[test]
    fn invalid_number_is_rejected_before_touching_modem() {
        let at = AtHandle::new();
        let cfg = Config::default();
        assert!(send(&at, &cfg, "abc", "hi").is_err());
    }
}
