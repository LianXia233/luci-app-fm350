//! 短信（PDU 模式）。
//!
//! FM350 与旧版 WebUI 一致，使用 PDU 模式收发：
//!   - 列表：`AT+CMGF=0` + `AT+CSCS="GSM"` + `AT+CMGL=4`
//!   - 发送：`AT+CMGS=<tpdu 长度>` → 等待 `>` → PDU + Ctrl-Z(0x1A)
//!   - 删除：`AT+CMGD=<index>`
//!   - 存储：`AT+CPMS?` / `AT+CPMS="ME","ME","ME"`
//!
//! 支持 7-bit（GSM 03.38）与 UCS2（中文）两种编码。

use std::time::Duration;

use crate::at::{self, AtHandle, AtResult};
use crate::config::Config;

const CTRL_Z: u8 = 0x1A;

fn run(at: &AtHandle, cfg: &Config, cmd: &str) -> AtResult<String> {
    if at::is_imei_write(cmd) {
        return Err(at::imei_block_reason(cmd));
    }
    at.with(cfg, |p| p.command(cmd))
}

// ---------------------------------------------------------------- 数据结构

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Sms {
    pub index: i64,
    pub status: i64,
    pub sender: String,
    pub timestamp: String,
    pub text: String,
    pub encoding: String,
    pub raw_pdu: String,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Storage {
    pub mem: String,
    pub used: i64,
    pub total: i64,
}

// ---------------------------------------------------------------- 工具

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() % 2 != 0 {
        return Err("PDU 长度为奇数".to_string());
    }
    if !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("PDU 含非十六进制字符".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

/// 半字节交换：把 PDU 中的压缩 BCD 号码还原为数字串。
fn swap_digits(b: &[u8], digits: usize) -> String {
    let mut out = String::new();
    for byte in b {
        out.push(to_digit(byte & 0x0F));
        if out.len() >= digits {
            break;
        }
        out.push(to_digit(byte >> 4));
        if out.len() >= digits {
            break;
        }
    }
    out.truncate(digits);
    out
}

fn to_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        0x0A => '*',
        0x0B => '#',
        0x0C => 'a',
        0x0D => 'b',
        0x0E => 'c',
        _ => 'F',
    }
}

/// 地址类型 → 号码前缀（0x91 国际、0x81/A1 国内）。
fn ton_prefix(t: u8) -> &'static str {
    if t == 0x91 {
        "+"
    } else {
        ""
    }
}

/// 时间戳（7 字节，半字节交换）→ `20YY-MM-DD HH:MM:SS`。
fn decode_timestamp(b: &[u8]) -> String {
    if b.len() < 7 {
        return String::new();
    }
    let d = |x: u8| -> String { format!("{}{}", x & 0x0F, x >> 4) };
    format!(
        "20{}-{}-{} {}:{}:{}",
        d(b[0]),
        d(b[1]),
        d(b[2]),
        d(b[3]),
        d(b[4]),
        d(b[5])
    )
}

/// GSM 03.38 基本字符集（0x00-0x7F）。
const GSM7_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";

/// 7-bit 解包：把 8 位字节流还原为 septet 值序列。
fn unpack7(data: &[u8], septets: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(septets);
    let mut acc: u16 = 0;
    let mut bits: u8 = 0;
    for byte in data {
        acc |= (*byte as u16) << bits;
        bits += 8;
        while bits >= 7 && out.len() < septets {
            out.push((acc & 0x7F) as u8);
            acc >>= 7;
            bits -= 7;
        }
        if out.len() >= septets {
            break;
        }
    }
    if out.len() < septets && bits >= 7 {
        out.push((acc & 0x7F) as u8);
    }
    out
}

/// 7-bit 打包：septet → 8 位字节流。
fn pack7(septets: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut acc: u16 = 0;
    let mut bits: u8 = 0;
    for s in septets {
        acc |= ((*s & 0x7F) as u16) << bits;
        bits += 7;
        if bits >= 8 {
            out.push((acc & 0xFF) as u8);
            acc >>= 8;
            bits -= 8;
        }
    }
    if bits > 0 {
        out.push((acc & 0xFF) as u8);
    }
    out
}

/// 7-bit septet → 字符（含扩展表处理）。
fn gsm7_to_string(septets: &[u8]) -> String {
    let basic: Vec<char> = GSM7_BASIC.chars().collect();
    let ext = |c: u8| -> char {
        match c {
            0x0A => '\u{000C}',
            0x14 => '^',
            0x28 => '{',
            0x29 => '}',
            0x2F => '\\',
            0x3C => '[',
            0x3D => '~',
            0x3E => ']',
            0x40 => '|',
            0x65 => '€',
            _ => '?',
        }
    };
    let mut out = String::new();
    let mut escape = false;
    for s in septets {
        let v = *s;
        if escape {
            out.push(ext(v));
            escape = false;
            continue;
        }
        if v == 0x1B {
            escape = true;
            continue;
        }
        out.push(*basic.get(v as usize).unwrap_or(&'?'));
    }
    out
}

/// 文本 → 7-bit septet（无法表示的字符返回 None）。
fn string_to_gsm7(text: &str) -> Option<Vec<u8>> {
    let basic: Vec<char> = GSM7_BASIC.chars().collect();
    let mut out = Vec::new();
    for c in text.chars() {
        if let Some(p) = basic.iter().position(|x| *x == c) {
            out.push(p as u8);
        } else {
            match c {
                '{' | '}' | '[' | ']' | '^' | '\\' | '~' | '|' | '€' => {
                    out.push(0x1B);
                    out.push(match c {
                        '{' => 0x28,
                        '}' => 0x29,
                        '[' => 0x3C,
                        ']' => 0x3E,
                        '^' => 0x14,
                        '\\' => 0x2F,
                        '~' => 0x3D,
                        '|' => 0x40,
                        _ => 0x65,
                    });
                }
                _ => return None,
            }
        }
    }
    Some(out)
}

fn ucs2_to_string(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks(2)
        .map(|c| ((c[0] as u16) << 8) | (*c.get(1).unwrap_or(&0) as u16))
        .collect();
    String::from_utf16_lossy(&units)
}

fn utf8_to_ucs2(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for u in s.encode_utf16() {
        out.push((u >> 8) as u8);
        out.push((u & 0xFF) as u8);
    }
    out
}

// ---------------------------------------------------------------- PDU 解析

/// 解析一条 SMS-DELIVER PDU。
pub fn decode_pdu(index: i64, status: i64, pdu: &str) -> Result<Sms, String> {
    let b = hex_decode(pdu)?;
    let mut i = 0usize;
    if b.is_empty() {
        return Err("PDU 为空".to_string());
    }

    // SMSC 信息（长度字节 + 内容）
    let smsc_len = b[i] as usize;
    i += 1 + smsc_len;
    if i + 2 > b.len() {
        return Err("PDU 过短".to_string());
    }

    let pdu_type = b[i];
    i += 1;
    let addr_digits = b[i] as usize;
    i += 1;
    let addr_type = b[i];
    i += 1;
    let addr_octets = (addr_digits + 1) / 2;
    let sender = format!(
        "{}{}",
        ton_prefix(addr_type),
        swap_digits(&b[i..(i + addr_octets).min(b.len())], addr_digits)
    );
    i += addr_octets;

    let _tp_pid = *b.get(i).unwrap_or(&0);
    i += 1;
    let tp_dcs = *b.get(i).unwrap_or(&0);
    i += 1;

    let timestamp = decode_timestamp(b.get(i..i + 7).unwrap_or(&[]));
    i += 7;

    let udl = *b.get(i).unwrap_or(&0) as usize;
    i += 1;

    // UDH（UDHI = bit6 of pdu_type）
    let mut udh_octets = 0usize;
    if pdu_type & 0x40 != 0 && i < b.len() {
        let udhl = b[i] as usize;
        udh_octets = 1 + udhl;
        i += udh_octets;
    }

    let ud = &b[i..];

    let (text, encoding) = match tp_dcs & 0x0C {
        0x08 => {
            // UCS2：UDL 为字节数
            (ucs2_to_string(&ud[..ud.len().min(udl)]), "UCS2".to_string())
        }
        0x04 => (String::from_utf8_lossy(ud).to_string(), "8BIT".to_string()),
        _ => {
            // 7-bit：跳过 UDH 占用的 septets 后解包
            let skip_bits = (udh_octets * 8 + 6) / 7;
            let septets = pack_count(ud, udl);
            let all = unpack7(ud, septets);
            let body = if skip_bits < all.len() {
                all[skip_bits..].to_vec()
            } else {
                Vec::new()
            };
            (gsm7_to_string(&body), "GSM7".to_string())
        }
    };

    Ok(Sms {
        index,
        status,
        sender,
        timestamp,
        text,
        encoding,
        raw_pdu: pdu.to_string(),
    })
}

/// 估算 7-bit 负载所需的 septet 数（UDL 直接给出，此处做上限保护）。
fn pack_count(ud: &[u8], udl: usize) -> usize {
    let cap = ud.len() * 8 / 7 + 1;
    udl.min(cap)
}

/// 解析 `AT+CMGL=4` 的整段响应。
pub fn parse_list(resp: &str) -> Vec<Sms> {
    let mut out = Vec::new();
    let mut cur_index: Option<i64> = None;
    let mut cur_status: i64 = 0;

    for line in resp.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix("+CMGL:") {
            let v: Vec<&str> = rest.split(',').map(|x| x.trim()).collect();
            if let Some(ix) = v.first().and_then(|x| x.parse().ok()) {
                cur_index = Some(ix);
            }
            cur_status = v.get(1).and_then(|x| x.parse().ok()).unwrap_or(0);
            continue;
        }
        if l == "OK" || l.starts_with("ERROR") {
            continue;
        }
        // PDU 负载行
        if let Some(ix) = cur_index {
            if let Ok(sms) = decode_pdu(ix, cur_status, l) {
                out.push(sms);
            }
            cur_index = None;
        }
    }
    out
}

// ---------------------------------------------------------------- PDU 构造

/// 构造 SMS-SUBMIT PDU（SMSC 长度置 0，使用模组内默认短信中心）。
pub fn build_submit_pdu(number: &str, text: &str) -> Result<(String, usize), String> {
    let (addr_type, digits) = if let Some(d) = number.strip_prefix('+') {
        (0x91u8, d.to_string())
    } else if number.starts_with("86") {
        (0x91u8, number.to_string())
    } else {
        (0x81u8, number.to_string())
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return Err("收件人号码格式无效".to_string());
    }

    // 地址：按数字成对交换
    let mut addr_bytes = Vec::new();
    let chars: Vec<char> = digits.chars().collect();
    let mut k = 0usize;
    while k < chars.len() {
        let hi = chars[k].to_digit(10).unwrap_or(0) as u8;
        let lo = chars.get(k + 1).and_then(|c| c.to_digit(10)).unwrap_or(0xF) as u8;
        addr_bytes.push((lo << 4) | hi);
        k += 2;
    }

    let mut tpdu: Vec<u8> = Vec::new();
    tpdu.push(0x01); // SMS-SUBMIT，无有效期，无 UDH
    tpdu.push(0x00); // TP-MR
    tpdu.push(digits.len() as u8); // 地址长度（数字个数）
    tpdu.push(addr_type);
    tpdu.extend_from_slice(&addr_bytes);
    tpdu.push(0x00); // TP-PID

    if let Some(septets) = string_to_gsm7(text) {
        tpdu.push(0x00); // TP-DCS: 7-bit
        tpdu.push(septets.len() as u8); // UDL = septet 数
        tpdu.extend_from_slice(&pack7(&septets));
    } else {
        let ucs2 = utf8_to_ucs2(text);
        tpdu.push(0x08); // TP-DCS: UCS2
        tpdu.push(ucs2.len() as u8); // UDL = 字节数
        tpdu.extend_from_slice(&ucs2);
    }

    // 最终 PDU = 00（SMSC 长度 0） + TPDU
    let mut full = vec![0x00u8];
    full.extend_from_slice(&tpdu);
    Ok((hex_encode(&full), tpdu.len()))
}

// ---------------------------------------------------------------- 对外操作

pub fn list(at: &AtHandle, cfg: &Config) -> AtResult<Vec<Sms>> {
    let _ = run(at, cfg, "AT+CMGF=0")?;
    let _ = run(at, cfg, r#"AT+CSCS="GSM""#)?;
    let resp = run(at, cfg, "AT+CMGL=4")?;
    Ok(parse_list(&resp))
}

pub fn send(at: &AtHandle, cfg: &Config, number: &str, text: &str) -> AtResult<String> {
    if text.is_empty() {
        return Err("短信内容不能为空".to_string());
    }
    let (pdu, tpdu_len) = build_submit_pdu(number, text)?;

    let _ = run(at, cfg, "AT+CMGF=0")?;
    let _ = run(at, cfg, r#"AT+CSCS="GSM""#)?;

    at.with(cfg, |p| {
        // 下发 AT+CMGS 后模组回 '>' 提示符，再写入 PDU 与 Ctrl-Z
        let _ = p.command(&format!("AT+CMGS={}", tpdu_len));
        p.wait_for(">", Duration::from_secs(5))?;
        p.write_raw(pdu.as_bytes())?;
        p.write_raw(&[CTRL_Z])?;
        p.wait_for("OK", Duration::from_secs(cfg.at_timeout.max(30)))
    })
    .map(|r| r.replace('\n', " "))
}

pub fn delete(at: &AtHandle, cfg: &Config, index: i64) -> AtResult<String> {
    run(at, cfg, &format!("AT+CMGD={}", index))
}

pub fn storage(at: &AtHandle, cfg: &Config) -> AtResult<Storage> {
    let r = run(at, cfg, "AT+CPMS?")?;
    let v = at::fields(&r, "+CPMS");
    Ok(Storage {
        mem: v.first().cloned().unwrap_or_else(|| "SM".to_string()),
        used: v.get(1).and_then(|x| x.parse().ok()).unwrap_or(0),
        total: v.get(2).and_then(|x| x.parse().ok()).unwrap_or(0),
    })
}

pub fn set_storage(at: &AtHandle, cfg: &Config, mem: &str) -> AtResult<String> {
    run(at, cfg, &format!(r#"AT+CPMS="{}","{}","{}""#, mem, mem, mem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip() {
        let b = vec![0x01u8, 0xAB, 0xFF];
        assert_eq!(hex_encode(&b), "01ABFF");
        assert_eq!(hex_decode("01ABFF").unwrap(), b);
    }

    #[test]
    fn gsm7_roundtrip() {
        let septets = string_to_gsm7("Hello").unwrap();
        assert_eq!(septets.len(), 5);
        assert_eq!(gsm7_to_string(&septets), "Hello");
    }

    #[test]
    fn ucs2_roundtrip() {
        let bytes = utf8_to_ucs2("你好");
        assert_eq!(bytes.len(), 4);
        assert_eq!(ucs2_to_string(&bytes), "你好");
    }

    #[test]
    fn build_submit_pdu_chinese() {
        let (pdu, len) = build_submit_pdu("+8613800138000", "测试").unwrap();
        assert!(pdu.starts_with("00"));
        assert_eq!(pdu.len(), (len + 1) * 2);
    }

    #[test]
    fn swap_digits_works() {
        // 8613800138000 -> 68 31 08 10 03 08 00 F0
        let b = [0x68u8, 0x31, 0x08, 0x10, 0x83, 0x00, 0xF0];
        assert_eq!(swap_digits(&b, 13), "8613800138000");
    }

    #[test]
    fn decode_deliver_pdu() {
        // 一条真实格式的 7-bit DELIVER PDU（SMSC 长度 0）
        let septets = string_to_gsm7("Hi").unwrap();
        let ud = pack7(&septets);
        let mut pdu: Vec<u8> = vec![
            0x00, // SMSC len 0
            0x04, // SMS-DELIVER
            13,   // 号码位数（8613800138000）
            0x91,
        ];
        let addr = [0x68u8, 0x31, 0x08, 0x10, 0x83, 0x00, 0xF0];
        pdu.extend_from_slice(&addr);
        pdu.push(0x00); // PID
        pdu.push(0x00); // DCS 7bit
        pdu.extend_from_slice(&[0x21, 0x40, 0x13, 0x06, 0x91, 0x80, 0x30]); // 时间戳
        pdu.push(2); // UDL
        pdu.extend_from_slice(&ud);
        let s = hex_encode(&pdu);
        let sms = decode_pdu(1, 0, &s).unwrap();
        assert_eq!(sms.text, "Hi");
        assert_eq!(sms.encoding, "GSM7");
        assert_eq!(sms.sender, "+8613800138000");
    }
}
