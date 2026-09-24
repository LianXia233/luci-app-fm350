//! PDU 解析与构造（SMS-DELIVER / SMS-SUBMIT）。
//!
//! ## 为什么要读懂 PDU
//!
//! 模组在 PDU 模式下只吐十六进制串，`AT+CMGL` 的每一行都要自己拆字段。
//! 常见故障不是「解不出来」，而是**解出来是错的** —— 号码少一位、中文变问号、
//! 时间戳差十年，都是字段边界算错的症状。因此这里的每个偏移都写了依据。
//!
//! ## 字段布局（SMS-DELIVER，不含 UDH 时）
//!
//! ```text
//! [SMSC 长度][SMSC...][PDU-Type][发件人位数][地址类型][号码(半字节交换)]
//! [TP-PID][TP-DCS][时间戳 7B][UDL][UD...]
//! ```
//!
//! 编码（UCS2 / 8bit / 7bit）由 **TP-DCS 的 bit3-2** 决定，不由字符集命令决定。

use super::gsm7;
use super::{Sms, Storage};

// ---------------------------------------------------------------- 十六进制

/// 十六进制串 → 字节。容忍空格/换行（模组回显常带缩进）。
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
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

/// 字节 → 大写十六进制串（模组只认大写）。
pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02X}", x)).collect()
}

// ---------------------------------------------------------------- 字段解码

/// 半字节交换：把 PDU 中的压缩 BCD 号码还原成数字串。
///
/// 每个字节存**两位数字，低半字节在前**；奇数位时末字节高半字节填 `0xF`。
pub fn swap_digits(b: &[u8], digits: usize) -> String {
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

/// 半字节 → 字符。`0x0A/0x0B/0x0C-0x0E` 是 `* # a b c`，其余按 `F` 处理
/// （填充位，不该出现在有效号码里）。
pub fn to_digit(n: u8) -> char {
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

/// 地址类型 → 号码前缀：`0x91` 为国际格式（带 `+`），其余不加。
pub fn ton_prefix(t: u8) -> &'static str {
    if t == 0x91 {
        "+"
    } else {
        ""
    }
}

/// 时间戳（7 字节，半字节交换）→ `20YY-MM-DD HH:MM:SS`。
pub fn decode_timestamp(b: &[u8]) -> String {
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

/// 估算 7-bit 负载的 septet 数：UDL 直接给出，这里只做上限保护，
/// 防止越界 `slice` panic（PDU 被截断时 UDL 可能大于实际负载）。
fn pack_count(ud: &[u8], udl: usize) -> usize {
    let cap = ud.len() * 8 / 7 + 1;
    udl.min(cap)
}

/// 解析一条 SMS-DELIVER PDU。
pub fn decode_pdu(index: i64, status: i64, pdu: &str) -> Result<Sms, String> {
    let b = hex_decode(pdu)?;
    if b.is_empty() {
        return Err("PDU 为空".to_string());
    }
    let mut i = 0usize;

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

    // UDH：仅当 PDU-Type 的 UDHI 位（bit6）置位时存在
    let mut udh_octets = 0usize;
    if pdu_type & 0x40 != 0 && i < b.len() {
        let udhl = b[i] as usize;
        udh_octets = 1 + udhl;
        i += udh_octets;
    }

    let ud = &b[i..];

    // 编码由 TP-DCS 的 bit3-2 决定：0x08=UCS2，0x04=8bit，其余=7bit
    let (text, encoding) = match tp_dcs & 0x0C {
        0x08 => (
            gsm7::ucs2_to_string(&ud[..ud.len().min(udl)]),
            "UCS2".to_string(),
        ),
        0x04 => (String::from_utf8_lossy(ud).to_string(), "8BIT".to_string()),
        _ => {
            let skip_bits = (udh_octets * 8 + 6) / 7;
            let septets = pack_count(ud, udl);
            let all = gsm7::unpack7(ud, septets);
            let body = if skip_bits < all.len() {
                all[skip_bits..].to_vec()
            } else {
                Vec::new()
            };
            (gsm7::gsm7_to_string(&body), "GSM7".to_string())
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

/// 解析 `AT+CMGL=4` 的整段响应。
///
/// 形态是「`+CMGL:` 头行 + 紧随其后的 PDU 负载行」交替出现；头行里的第 1
/// 个字段是序号、第 2 个是状态。解析失败的单条**跳过而非整批失败** ——
/// 一条坏短信不该让整个列表打不开。
pub fn parse_list(resp: &str) -> Vec<Sms> {
    let mut out = Vec::new();
    let mut cur_index: Option<i64> = None;
    let mut cur_status: i64 = 0;

    for line in resp.lines() {
        let l = line.trim();
        if l.is_empty() || l == "OK" || l.starts_with("ERROR") {
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
        if let Some(ix) = cur_index {
            if let Ok(sms) = decode_pdu(ix, cur_status, l) {
                out.push(sms);
            }
            cur_index = None;
        }
    }
    out
}

/// 解析 `AT+CPMS?` 响应。
pub fn parse_storage(resp: &str) -> Storage {
    let v = crate::at::fields(resp, "+CPMS");
    Storage {
        // 实机 `AT+CPMS=?` 只返回 ("SM")，解析不出时回落 SM
        mem: v.first().cloned().unwrap_or_else(|| "SM".to_string()),
        used: v.get(1).and_then(|x| x.parse().ok()).unwrap_or(0),
        total: v.get(2).and_then(|x| x.parse().ok()).unwrap_or(0),
    }
}

// ---------------------------------------------------------------- PDU 构造

/// 构造 SMS-SUBMIT PDU（SMSC 长度置 0 → 使用模组内默认短信中心）。
///
/// 返回 `(十六进制 PDU, TPDU 字节数)`。注意 `AT+CMGS=<n>` 要的是 **TPDU
/// 字节数**，不含前导的 SMSC 长度字节 —— 多算一个字节模组会一直等数据。
pub fn build_submit_pdu(number: &str, text: &str) -> Result<(String, usize), String> {
    // `+` 开头或 `86` 开头按国际格式（0x91），否则国内格式（0x81）
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

    // 地址按数字成对交换，奇数位末字节高半字节填 0xF
    let chars: Vec<char> = digits.chars().collect();
    let mut addr_bytes = Vec::new();
    let mut k = 0usize;
    while k < chars.len() {
        let hi = chars[k].to_digit(10).unwrap_or(0) as u8;
        let lo = chars
            .get(k + 1)
            .and_then(|c| c.to_digit(10))
            .unwrap_or(0xF) as u8;
        addr_bytes.push((lo << 4) | hi);
        k += 2;
    }

    let mut tpdu: Vec<u8> = Vec::new();
    tpdu.push(0x01); // SMS-SUBMIT，无有效期、无 UDH
    tpdu.push(0x00); // TP-MR
    tpdu.push(digits.len() as u8); // 地址长度（数字个数）
    tpdu.push(addr_type);
    tpdu.extend_from_slice(&addr_bytes);
    tpdu.push(0x00); // TP-PID

    // 能全部塞进 GSM 7-bit 表就用 7-bit（更省、单条能发 160 字符），
    // 否则回落 UCS2（中文必然走这条）。
    if let Some(septets) = gsm7::string_to_gsm7(text) {
        tpdu.push(0x00); // TP-DCS: 7-bit
        tpdu.push(septets.len() as u8); // UDL = septet 数
        tpdu.extend_from_slice(&gsm7::pack7(&septets));
    } else {
        let ucs2 = gsm7::utf8_to_ucs2(text);
        tpdu.push(0x08); // TP-DCS: UCS2
        tpdu.push(ucs2.len() as u8); // UDL = 字节数
        tpdu.extend_from_slice(&ucs2);
    }

    // 最终 PDU = 00（SMSC 长度 0） + TPDU
    let mut full = vec![0x00u8];
    full.extend_from_slice(&tpdu);
    Ok((hex_encode(&full), tpdu.len()))
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
    fn hex_tolerates_whitespace_and_rejects_bad_input() {
        assert_eq!(hex_decode("01 AB\nFF").unwrap(), vec![0x01, 0xAB, 0xFF]);
        assert!(hex_decode("ABC").is_err(), "奇数长度必须报错");
        assert!(hex_decode("ZZ").is_err(), "非十六进制必须报错");
    }

    #[test]
    fn swap_digits_works() {
        // 8613800138000 -> 68 31 08 10 83 00 F0
        let b = [0x68u8, 0x31, 0x08, 0x10, 0x83, 0x00, 0xF0];
        assert_eq!(swap_digits(&b, 13), "8613800138000");
    }

    #[test]
    fn timestamp_is_decoded_by_nibble_swap() {
        // 2021-10-12 04:16:38 在 PDU 里存成半字节交换的形式（首个数字在低半
        // 字节）：21→0x12, 10→0x01, 12→0x12, 04→0x40, 16→0x61, 38→0x83，
        // 末字节为时区
        let b = [0x12u8, 0x01, 0x21, 0x40, 0x61, 0x83, 0x00];
        assert_eq!(decode_timestamp(&b), "2021-10-12 04:16:38");
    }

    #[test]
    fn timestamp_requires_seven_bytes() {
        assert_eq!(decode_timestamp(&[0x12, 0x10]), "");
    }

    #[test]
    fn ton_prefix_marks_international_numbers() {
        assert_eq!(ton_prefix(0x91), "+");
        assert_eq!(ton_prefix(0x81), "");
    }

    #[test]
    fn build_submit_pdu_chinese() {
        let (pdu, len) = build_submit_pdu("+8613800138000", "测试").unwrap();
        assert!(pdu.starts_with("00"), "SMSC 长度字节必须是 00");
        assert_eq!(pdu.len(), (len + 1) * 2);
    }

    /// `AT+CMGS` 要的是 TPDU 字节数（不含 SMSC 长度字节），这里必须自洽。
    #[test]
    fn cmgs_length_excludes_smsc_byte() {
        let (pdu, len) = build_submit_pdu("+8613800138000", "Hi").unwrap();
        assert_eq!(pdu.len() / 2, len + 1);
    }

    #[test]
    fn build_rejects_malformed_number() {
        assert!(build_submit_pdu("", "x").is_err());
        assert!(build_submit_pdu("+8613800abc", "x").is_err());
    }

    #[test]
    fn decode_deliver_pdu() {
        let septets = gsm7::string_to_gsm7("Hi").unwrap();
        let ud = gsm7::pack7(&septets);
        let mut pdu: Vec<u8> = vec![
            0x00, // SMSC len 0
            0x04, // SMS-DELIVER
            13,   // 号码位数（8613800138000）
            0x91,
        ];
        pdu.extend_from_slice(&[0x68u8, 0x31, 0x08, 0x10, 0x83, 0x00, 0xF0]);
        pdu.push(0x00); // PID
        pdu.push(0x00); // DCS 7bit
        pdu.extend_from_slice(&[0x21, 0x40, 0x13, 0x06, 0x91, 0x80, 0x30]);
        pdu.push(2); // UDL
        pdu.extend_from_slice(&ud);
        let s = hex_encode(&pdu);
        let sms = decode_pdu(1, 0, &s).unwrap();
        assert_eq!(sms.text, "Hi");
        assert_eq!(sms.encoding, "GSM7");
        assert_eq!(sms.sender, "+8613800138000");
    }

    /// UCS2 短信：TP-DCS = 0x08，UDL 是**字节数**不是字符数。
    #[test]
    fn decode_ucs2_deliver_pdu() {
        let body = gsm7::utf8_to_ucs2("你好");
        let mut pdu: Vec<u8> = vec![0x00, 0x04, 13, 0x91];
        pdu.extend_from_slice(&[0x68u8, 0x31, 0x08, 0x10, 0x83, 0x00, 0xF0]);
        pdu.push(0x00); // PID
        pdu.push(0x08); // DCS UCS2
        pdu.extend_from_slice(&[0x21, 0x40, 0x13, 0x06, 0x91, 0x80, 0x30]);
        pdu.push(body.len() as u8);
        pdu.extend_from_slice(&body);
        let sms = decode_pdu(2, 1, &hex_encode(&pdu)).unwrap();
        assert_eq!(sms.encoding, "UCS2");
        assert_eq!(sms.text, "你好");
    }

    #[test]
    fn parse_list_pairs_header_with_payload() {
        let septets = gsm7::string_to_gsm7("Hi").unwrap();
        let ud = gsm7::pack7(&septets);
        let mut pdu: Vec<u8> = vec![0x00, 0x04, 13, 0x91];
        pdu.extend_from_slice(&[0x68u8, 0x31, 0x08, 0x10, 0x83, 0x00, 0xF0]);
        pdu.push(0x00);
        pdu.push(0x00);
        pdu.extend_from_slice(&[0x21, 0x40, 0x13, 0x06, 0x91, 0x80, 0x30]);
        pdu.push(2);
        pdu.extend_from_slice(&ud);
        // 实机形态：头行给出 PDU 字节数，PDU 十六进制串在**下一行**
        let resp = format!(
            "+CMGL: 1,0,,{}\r\n{}\r\nOK",
            pdu.len(),
            hex_encode(&pdu)
        );
        let list = parse_list(&resp);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].index, 1);
        assert_eq!(list[0].text, "Hi");
    }

    /// 一条坏 PDU 不能让整个列表打不开。
    #[test]
    fn parse_list_skips_undecodable_entries() {
        let resp = "+CMGL: 3,0,,ZZZZ\r\n+CMGL: 4,0,,00\r\nOK";
        assert!(parse_list(resp).is_empty());
    }

    #[test]
    fn parse_storage_falls_back_to_sm() {
        let s = parse_storage("+CPMS: \"SM\",3,50\r\nOK");
        assert_eq!(s.mem, "SM");
        assert_eq!(s.used, 3);
        assert_eq!(s.total, 50);
        let empty = parse_storage("");
        assert_eq!(empty.mem, "SM");
    }
}
