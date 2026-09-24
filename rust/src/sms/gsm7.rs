//! GSM 03.38（7-bit）与 UCS2 编解码。
//!
//! PDU 模式下短信正文有两种承载：
//!   * **7-bit**（GSM 03.38 默认表）：最多 160 个 septet，英文/数字最省；
//!   * **UCS2**：最多 70 个 UTF-16 码元，中文必须走它。
//!
//! 选哪个由发送侧决定：能全部塞进 7-bit 表就用 7-bit，否则回落 UCS2
//! （见 [`super::pdu::build_submit_pdu`]）。解析侧则以 TP-DCS 字段为准。

/// GSM 03.38 基本字符集（0x00–0x7F）。
///
/// 注意 0x00–0x0F 段含 `£ ¥ è é` 等符号，位置与 ASCII 不同 —— 直接按 ASCII
/// 查表会把 `@` 之后的字符整体错位，这是 PDU 解析最常见的错字来源。
const GSM7_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";

/// 7-bit 解包：8 位字节流 → septet 序列。
///
/// 是 7→8 打包的逆运算：低 7 位先出，跨字节的余位累加到下一次。
pub fn unpack7(data: &[u8], septets: usize) -> Vec<u8> {
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

/// 7-bit 打包：septet 序列 → 8 位字节流。
pub fn pack7(septets: &[u8]) -> Vec<u8> {
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

/// 扩展表字符（`ESC` 之后的那个 septet）。
///
/// 只列出实机上真会遇到的几个；未收录的一律回落 `?`，比塞一个错字符安全。
fn ext_char(c: u8) -> char {
    match c {
        0x0A => '\u{000C}', // form feed
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
}

/// septet 序列 → 字符串（含 `0x1B` 转义处理）。
pub fn gsm7_to_string(septets: &[u8]) -> String {
    let basic: Vec<char> = GSM7_BASIC.chars().collect();
    let mut out = String::new();
    let mut escape = false;
    for s in septets {
        let v = *s;
        if escape {
            out.push(ext_char(v));
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

/// 字符串 → septet 序列。
///
/// 返回 `None` 表示**存在无法用 GSM 7-bit 表示的字符**（典型是中文），
/// 调用方应回落 UCS2 —— 而不是在这里强行替换成 `?` 把内容静默改掉。
pub fn string_to_gsm7(text: &str) -> Option<Vec<u8>> {
    let basic: Vec<char> = GSM7_BASIC.chars().collect();
    let mut out = Vec::new();
    for c in text.chars() {
        if let Some(p) = basic.iter().position(|x| *x == c) {
            out.push(p as u8);
            continue;
        }
        let ext = match c {
            '{' => 0x28,
            '}' => 0x29,
            '[' => 0x3C,
            ']' => 0x3E,
            '^' => 0x14,
            '\\' => 0x2F,
            '~' => 0x3D,
            '|' => 0x40,
            '€' => 0x65,
            _ => return None,
        };
        out.push(0x1B);
        out.push(ext);
    }
    Some(out)
}

/// UCS2（大端 UTF-16）字节流 → 字符串。
pub fn ucs2_to_string(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks(2)
        .map(|c| ((c[0] as u16) << 8) | (*c.get(1).unwrap_or(&0) as u16))
        .collect();
    String::from_utf16_lossy(&units)
}

/// 字符串 → UCS2（大端 UTF-16）字节流。
pub fn utf8_to_ucs2(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for u in s.encode_utf16() {
        out.push((u >> 8) as u8);
        out.push((u & 0xFF) as u8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_table_covers_every_defined_slot() {
        // 3GPP TS 23.038 定义了 127 个字符：0x1F 保留未定义，其余 0x00-0x7F
        // 全部有定义。表里少一个字符，后面所有 septet 的映射都会错位。
        assert_eq!(GSM7_BASIC.chars().count(), 127);
    }

    #[test]
    fn ascii_text_roundtrips() {
        let septets = string_to_gsm7("Hello").unwrap();
        assert_eq!(septets.len(), 5);
        assert_eq!(gsm7_to_string(&septets), "Hello");
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let septets = string_to_gsm7("Hello, world! 0123").unwrap();
        assert_eq!(unpack7(&pack7(&septets), septets.len()), septets);
    }

    /// 中文必须被判为「无法用 7-bit 表示」，让调用方回落 UCS2。
    #[test]
    fn chinese_is_not_representable_in_gsm7() {
        assert!(string_to_gsm7("你好").is_none());
        assert!(string_to_gsm7("abc你好").is_none());
    }

    #[test]
    fn escape_sequences_roundtrip() {
        let septets = string_to_gsm7("a{b}c").unwrap();
        assert_eq!(gsm7_to_string(&septets), "a{b}c");
    }

    #[test]
    fn ucs2_roundtrip() {
        let bytes = utf8_to_ucs2("你好");
        assert_eq!(bytes.len(), 4);
        assert_eq!(ucs2_to_string(&bytes), "你好");
    }
}
