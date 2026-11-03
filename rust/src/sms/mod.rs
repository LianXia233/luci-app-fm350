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
//! | [`archive`] | 本地发件归档（JSONL，见其模块头） |
//! | 本文件 | AT 交互与对外操作 |
//!
//! ## 交互序列
//!
//! ```text
//! 列表：AT+CMGF=0 → AT+CSCS="GSM" → AT+CMGL=4
//! 发送：AT+CMGF=0 → AT+CSCS="GSM" → AT+CMGS=<tpdu 长度>
//!       → 等 '>' → 写 PDU + Ctrl-Z(0x1A) → 等 OK
//!       （任一步失败 → ESC(0x1B) 中止输入态 → AT 重同步）
//! 删除：AT+CMGD=<index>
//! 存储：AT+CPMS? / AT+CPMS="SM","SM","SM"
//! ```
//!
//! 实机 `AT+CPMS=?` **只返回 `("SM")`**，不支持 `"ME"` —— 传 ME 会直接
//! `ERROR`，所以默认值只能是 SM。
//!
//! ## 发送失败为什么必须补 ESC（实机故障：发错短信后整个 LuCI 拖死）
//!
//! 手册 §8.1.16 的 PDU 形态是 `AT+CMGS=<length><CR>` → PDU →
//! `<ctrl-Z/ESC>`：**ESC 与 Ctrl-Z 一样是文档化的结束符**。若 `>` 提示符
//! 没等到、或写入中途失败就放弃，模组可能停在「等 PDU 正文」的半开输入态
//! —— 紧随其后的任何命令（状态轮询的 AT+CSQ、列表的 AT+CMGL）都会被当成
//! PDU 吞掉，模组不再按 AT 语义应答，daemon 每轮巡检全部超时，rpcd 同步
//! 代理把整个 LuCI 拖死。因此 `send` 的**每一条失败路径**都必须先写 ESC
//! 中止可能的输入态，再用一条 `AT` 确认端口恢复 AT 语义，然后才把错误
//! 交出去。（MT5700 项目 sms.rs 有同一结论的独立实机验证，编号 issue。）

use std::time::Duration;

use crate::at::queue::AtPriority;
use crate::at::{at_cmd, is_imei_write, AtHandle, AtResult};
use crate::config::Config;

pub mod archive;
pub mod gsm7;
pub mod pdu;

pub use pdu::{decode_pdu, parse_list, parse_storage};

/// PDU 发送结束符。
const CTRL_Z: u8 = 0x1A;
/// 输入态中止符（手册 §8.1.16：`<ctrl-Z/ESC>` 的 ESC 一侧）。
const ESC: u8 = 0x1B;

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
fn run(at: &AtHandle, prio: AtPriority, cfg: &Config, cmd: &str) -> AtResult<String> {
    if is_imei_write(cmd) {
        return Err(crate::at::imei_block_reason(cmd));
    }
    // 邮箱任务要求 'static：命令串拷贝进闭包（字符串极短，成本可忽略）
    let owned = cmd.to_string();
    at.run(prio, cfg, move |p, _| p.command(&owned))
}

/// 列出全部短信。
pub fn list(at: &AtHandle, cfg: &Config) -> AtResult<Vec<Sms>> {
    let _ = run(at, AtPriority::Normal, cfg, at_cmd::CMGF_PDU)?;
    let _ = run(at, AtPriority::Normal, cfg, at_cmd::CSCS_GSM)?;
    let resp = run(at, AtPriority::Normal, cfg, at_cmd::CMGL_ALL)?;
    Ok(parse_list(&resp))
}

/// 发送短信。
///
/// 编码选择由 `build_submit_pdu` 决定：能进 GSM 7-bit 表就用 7-bit，否则
/// UCS2（中文）。**不在这里截断长度** —— 超长短信应由调用方提示，而不是
/// 悄悄截断后发出去。
///
/// 形态取 FM350 手册 §8.1.16 的**标准两段式**（真 CR、等 `>`、PDU +
/// Ctrl-Z）。MT5700 模组存在「字面 `\r` 一次性形态」的私有优化，本模组
/// 手册没有记载该形态，不做照搬。
///
/// **失败路径一律先中止输入态再返回**（见模块头「为什么必须补 ESC」）：
/// 这不是锦上添花，而是「发一条失败短信拖死整个 LuCI」的直接解药。
pub fn send(at: &AtHandle, cfg: &Config, number: &str, text: &str) -> AtResult<String> {
    if text.is_empty() {
        return Err("短信内容不能为空".to_string());
    }
    let (pdu, tpdu_len) = pdu::build_submit_pdu(number, text)?;

    let _ = run(at, AtPriority::High, cfg, at_cmd::CMGF_PDU)?;
    let _ = run(at, AtPriority::High, cfg, at_cmd::CSCS_GSM)?;

    let r = at.run(AtPriority::High, cfg, move |p, c| {
        // 下发 AT+CMGS 后模组回 '>' 提示符，再写入 PDU 与 Ctrl-Z。
        // 必须用 command_prompt：普通 command() 会等结果码，把 '>' 当成
        // 中间输出一直等到超时，随后 wait_for 只能拿到空气。
        let outcome = (|| -> AtResult<String> {
            p.command_prompt(&at_cmd::cmgs(tpdu_len), ">", PROMPT_TIMEOUT)?;
            p.write_raw(pdu.as_bytes())?;
            p.write_raw(&[CTRL_Z])?;
            // 配置快照经闭包第二参带入（'static 邮箱任务不能借用外部 cfg）
            p.wait_result(Duration::from_secs(c.at_timeout.max(30)))
        })();
        if outcome.is_err() {
            // 失败收尾：中止可能残留的半开输入态 + 重同步。
            // 即使模组根本没进输入态，ESC 在命令态也是无害的。
            let _ = p.write_raw(&[ESC]);
            std::thread::sleep(Duration::from_millis(300));
            // AT 重同步：能拿到应答说明端口已回到 AT 语义；拿不到把
            // 重同步失败追加进错误信息（这通常是更深的故障，如模组重启中）。
            if let Err(sync_err) = p.command("AT") {
                return Err(format!(
                    "{}；发送失败后端口重同步也失败（{}），串口可能需要看门狗恢复",
                    outcome.unwrap_err(),
                    sync_err
                ));
            }
            return outcome;
        }
        outcome
    });
    r.map(|s| s.replace('\n', " "))
}

/// 删除指定序号的短信。
pub fn delete(at: &AtHandle, cfg: &Config, index: i64) -> AtResult<String> {
    run(at, AtPriority::High, cfg, &at_cmd::cmgd(index))
}

/// 查询短信存储用量。
pub fn storage(at: &AtHandle, cfg: &Config) -> AtResult<Storage> {
    let r = run(at, AtPriority::Normal, cfg, at_cmd::CPMS_READ)?;
    Ok(parse_storage(&r))
}

/// 设置短信存储（三处都设成同一个：`AT+CPMS=<mem>,<mem>,<mem>`）。
pub fn set_storage(at: &AtHandle, cfg: &Config, mem: &str) -> AtResult<String> {
    run(at, AtPriority::Normal, cfg, &at_cmd::cpms_set(mem))
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
