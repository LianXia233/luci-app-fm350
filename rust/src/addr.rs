//! IPv4 / IPv6 地址工具（与模组协议无关的纯函数）。
//!
//! ## 为什么单独成模块
//!
//! 重构前这些函数放在 `modem.rs`，而 `net.rs` 又要反过来
//! `use crate::modem::is_valid_ipv4`，形成 `net → modem` 的反向依赖
//! （网络层依赖模组协议层，方向是错的）。下沉之后依赖方向变成
//! `modem → addr`、`net → addr`，两侧都不再互相知晓。
//!
//! ## 语义边界（改动这里的代价很高，务必看清）
//!
//! * [`is_valid_ipv4`] **拒绝** `0.0.0.0`：FM350 在未分配地址时会回
//!   `0.0.0.0`，若判为有效，上层会把死地址写进接口。
//! * [`normalize_ipv6`] **拒绝占位地址**：全零（`::`）与纯 `::1`。
//!   FM350 在 IPv6 未分配时回 `0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1`，
//!   必须判为无效，否则会配出一条永远不通的 v6 默认路由。
//! * 冒号写法**原样返回**（只做结构校验与 zone id / 方括号剥离），
//!   不做重新压缩 —— 保持与既有 UCI 写入值逐字节一致。

/// 是否为合法 IPv4 点分十进制。
///
/// 判定：四段、每段纯数字且 <= 255、且不是 `0.0.0.0`。
/// 含 `:` 一律拒绝（FM350 的 IPv6 会写成 16 段点分十进制，段数不同本就
/// 会被拒绝，但显式排除更直观）。
pub fn is_valid_ipv4(s: &str) -> bool {
    if s.is_empty() || s.contains(':') {
        return false;
    }
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let numeric = parts.iter().all(|p| {
        !p.is_empty()
            && p.chars().all(|c| c.is_ascii_digit())
            && p.parse::<u16>().map(|n| n <= 255).unwrap_or(false)
    });
    numeric && s != "0.0.0.0"
}

/// 是否为合法 IPv4 子网掩码（点分十进制、四段、且为连续 1）。
///
/// 手册 §12.2.10 的 `<local_addr and subnet_mask>` 会把地址与掩码拼成
/// `a1.a2.a3.a4.m1.m2.m3.m4` 一起上报。掩码必须连续，`255.0.255.0`
/// 这类非连续值直接拒绝，避免把错误掩码写进 netifd。
pub fn is_valid_netmask4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let mut bits: u32 = 0;
    for p in parts {
        if p.is_empty() || p.len() > 3 || !p.chars().all(|c| c.is_ascii_digit()) {
            return false;
        }
        match p.parse::<u16>() {
            Ok(n) if n <= 255 => bits = (bits << 8) | n as u32,
            _ => return false,
        }
    }
    // 连续 1：取反后必须是 2^n - 1（0 掩码单独拒绝）。
    if bits == 0 {
        return false;
    }
    let inv = !bits;
    inv & inv.wrapping_add(1) == 0
}

/// `AT+CGCONTRDP` 字段 3（`<local_addr and subnet_mask>`）的解析结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContrAddr {
    /// 归一化后的地址（IPv6 已转成冒号写法）。
    pub addr: String,
    /// IPv4 掩码（手册规定随地址一起上报；未上报时为 `None`）。
    pub mask: Option<String>,
    /// 是否为 IPv6。
    pub v6: bool,
}

/// 解析 `AT+CGCONTRDP` 的 `<local_addr and subnet_mask>`。
///
/// 手册 §12.2.10 的形态：
///   * IPv4：`a1.a2.a3.a4.m1.m2.m3.m4`（8 段），也兼容只回 4 段地址的固件；
///   * IPv6：16 段地址 + 16 段掩码（32 段），也兼容只回 16 段地址。
///
/// 仅接受合法地址；掩码非法时保留地址、丢弃掩码（上层回落到默认掩码），
/// 不因为掩码异常把整个地址判死。
pub fn parse_contrdp_addr(s: &str) -> Option<ContrAddr> {
    let s = s.trim().trim_matches('"');
    if s.is_empty() {
        return None;
    }
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        4 => {
            if is_valid_ipv4(s) {
                Some(ContrAddr {
                    addr: s.to_string(),
                    mask: None,
                    v6: false,
                })
            } else {
                None
            }
        }
        8 => {
            let addr = parts[..4].join(".");
            let mask = parts[4..].join(".");
            if !is_valid_ipv4(&addr) {
                return None;
            }
            Some(ContrAddr {
                addr,
                mask: is_valid_netmask4(&mask).then_some(mask),
                v6: false,
            })
        }
        16 => normalize_ipv6(s).map(|addr| ContrAddr {
            addr,
            mask: None,
            v6: true,
        }),
        32 => {
            let addr = normalize_ipv6(&parts[..16].join("."))?;
            let mask = parts[16..].join(".");
            Some(ContrAddr {
                addr,
                mask: Some(mask),
                v6: true,
            })
        }
        _ => None,
    }
}

/// 是否为链路本地 IPv6（`fe80::/10`，首组落在 `fe80`~`febf`）。
///
/// 任何 UP 的网卡都会自带一个 link-local 地址，把它计入「有 IPv6」
/// 会制造长期假阳性（前端显示有 v6、`st.up` 误判在线）。
pub fn is_link_local_v6(ip: &str) -> bool {
    match u16::from_str_radix(ip.split(':').next().unwrap_or(""), 16) {
        Ok(v) => v & 0xffc0 == 0xfe80,
        Err(_) => false,
    }
}

/// 统计一段冒号分隔文本中的合法组数（空串 = 0 组；任一组非法返回 `None`）。
///
/// 尾段若还含 `::`，`split(':')` 会产生空组，同样判非法 —— 这正是
/// 「两个 `::`」被拒绝的机制。
fn count_v6_groups(part: &str) -> Option<usize> {
    if part.is_empty() {
        return Some(0);
    }
    let mut n = 0usize;
    for g in part.split(':') {
        if g.is_empty() || g.len() > 4 || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        n += 1;
    }
    Some(n)
}

/// 冒号形式的结构校验：至多一个 `::`，每组 1~4 位十六进制；
/// 无 `::` 时必须恰好 8 组，有 `::` 时显式组数必须 < 8。
fn well_formed_colon_v6(core: &str) -> bool {
    let mut split = core.splitn(2, "::");
    let head = split.next().unwrap_or("");
    match split.next() {
        None => count_v6_groups(head) == Some(8),
        Some(tail) => match (count_v6_groups(head), count_v6_groups(tail)) {
            (Some(a), Some(b)) => a + b < 8,
            _ => false,
        },
    }
}

/// FM350 特有的 IPv6 表示：**点分十进制**（16 个字节逐段写成十进制）。
///
/// 实机原始响应：
///
/// ```text
/// AT+CGPADDR=1    -> +CGPADDR: 1,"10.7.45.240","0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1"
/// AT+CGCONTRDP=1  -> +CGCONTRDP: 1,,"cmiot5g","","","36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8",...
/// ```
///
/// 解码示例：`36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8` → `2409:8057:2000::8`
/// （`36.9` 得 `0x2409`，`128.87` 得 `0x8057`，`32.0` 得 `0x2000`，其余为零）。
///
/// 占位地址返回 `None`：全零，或仅最低字节为 1（即 `::1`）。
fn decode_dotted_ipv6(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 16 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u16 = p.parse().ok()?;
        if n > 255 {
            return None;
        }
        bytes[i] = n as u8;
    }
    if bytes.iter().all(|b| *b == 0) {
        return None;
    }
    if bytes[..15].iter().all(|b| *b == 0) && bytes[15] == 1 {
        return None;
    }
    let groups: Vec<u16> = (0..8)
        .map(|i| ((bytes[i * 2] as u16) << 8) | bytes[i * 2 + 1] as u16)
        .collect();
    Some(format_ipv6_groups(&groups))
}

/// 把 8 个 16 位组格式化为标准 IPv6 文本（RFC 5952：压缩最长的连续零段）。
fn format_ipv6_groups(g: &[u16]) -> String {
    let (mut best_start, mut best_len, mut cur_start, mut cur_len) = (0usize, 0usize, 0usize, 0usize);
    for (i, v) in g.iter().enumerate() {
        if *v == 0 {
            if cur_len == 0 {
                cur_start = i;
            }
            cur_len += 1;
            if cur_len > best_len {
                best_len = cur_len;
                best_start = cur_start;
            }
        } else {
            cur_len = 0;
        }
    }
    let hex = |s: &[u16]| {
        s.iter()
            .map(|v| format!("{:x}", v))
            .collect::<Vec<_>>()
            .join(":")
    };
    if best_len < 2 {
        return hex(g);
    }
    let (head, tail) = (hex(&g[..best_start]), hex(&g[best_start + best_len..]));
    match (head.is_empty(), tail.is_empty()) {
        (true, true) => "::".to_string(),
        (true, false) => format!("::{}", tail),
        (false, true) => format!("{}::", head),
        (false, false) => format!("{}::{}", head, tail),
    }
}

/// 把 AT 上报的 IPv6 归一化为可写进 UCI / 可展示的标准写法；非法或占位返回 `None`。
///
/// 两条通路：
///   * 标准冒号形式 —— 先做**结构校验**，再排除全零占位。旧版只要含十六进制
///     字符就采信，`gg::1`、`2409::8::1`（两个 `::`）这类脏值会被原样写进
///     UCI；现由 [`well_formed_colon_v6`] 拦下。
///   * FM350 的点分十进制形式 —— 走 [`decode_dotted_ipv6`]。
///
/// zone id（`%eth2`）与方括号展示形态（`[2409::1]`）在此剥掉。
pub fn normalize_ipv6(s: &str) -> Option<String> {
    if s.contains(':') {
        let core = s
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split('%')
            .next()
            .unwrap_or("");
        if core.is_empty() || !well_formed_colon_v6(core) {
            return None;
        }
        let hex: String = core.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        if hex.is_empty() || hex.chars().all(|c| c == '0') {
            return None;
        }
        Some(core.to_string())
    } else {
        decode_dotted_ipv6(s)
    }
}

/// 字符串是否「看起来像」 IPv6（含冒号，或为 16 段点分十进制）。
///
/// 用于 `AT+CGPADDR` 的**地址族判别**：`CGPADDR` 的第三个字段位一旦出现
/// 过（哪怕是 `::1` 占位），就说明网络侧对 IPv6 表过态 —— 此时绝不能
/// 再用 `CGCONTRDP` 的残留值兜底。
pub fn looks_v6(s: &str) -> bool {
    let s = s.trim().trim_matches('"');
    if s.contains(':') {
        return true;
    }
    s.split('.').count() == 16
}

/// 测试与断言用的布尔入口：合法 IPv6（非占位）为 true。
#[cfg(test)]
fn is_valid_ipv6(s: &str) -> bool {
    normalize_ipv6(s).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_accepts_real_fm350_address() {
        // 实机 AT+CGPADDR=1 返回的真实地址
        assert!(is_valid_ipv4("10.5.23.212"));
        assert!(is_valid_ipv4("221.179.38.7"));
    }

    #[test]
    fn ipv4_rejects_fm350_dummy_v6_address() {
        // FM350 在 IPv6 未分配时回 16 段点分十进制伪地址
        let dummy = "0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1";
        assert!(!is_valid_ipv4(dummy));
        assert!(!is_valid_ipv4("36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8"));
    }

    #[test]
    fn ipv4_rejects_edge_cases() {
        assert!(!is_valid_ipv4(""));
        assert!(!is_valid_ipv4("0.0.0.0"));
        assert!(!is_valid_ipv4("10.5.23"));
        assert!(!is_valid_ipv4("10.5.23.256"));
        assert!(!is_valid_ipv4("10.5.23.a"));
        assert!(!is_valid_ipv4("::1"));
    }

    #[test]
    fn ipv6_accepts_proper_notation() {
        assert!(is_valid_ipv6("2409:8a00:1234::1"));
        assert!(is_valid_ipv6("fe80::1"));
    }

    #[test]
    fn ipv6_rejects_dummy_and_empty() {
        assert!(!is_valid_ipv6(""));
        assert!(!is_valid_ipv6("10.5.23.212"));
        assert!(!is_valid_ipv6("0:0:0:0:0:0:0:0"));
        assert!(!is_valid_ipv6("0000:0000:0000:0000:0000:0000:0000:0000"));
    }

    #[test]
    fn ipv6_decodes_fm350_dotted_notation() {
        // 实机 AT+CGCONTRDP=1 原始响应中的 <PDP_addr> 与 <gateway>
        let addr = "36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8";
        let gw = "36.9.128.87.32.0.0.4.0.0.0.0.0.0.0.8";
        assert!(is_valid_ipv6(addr));
        assert_eq!(normalize_ipv6(addr).as_deref(), Some("2409:8057:2000::8"));
        assert_eq!(normalize_ipv6(gw).as_deref(), Some("2409:8057:2000:4::8"));
    }

    #[test]
    fn ipv6_rejects_fm350_dotted_placeholder() {
        let dummy = "0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1";
        assert!(!is_valid_ipv6(dummy));
        assert_eq!(normalize_ipv6(dummy), None);
        assert!(!is_valid_ipv6("0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0"));
        assert!(!is_valid_ipv6("1.2.3.4"));
        assert!(!is_valid_ipv6("36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.256"));
    }

    #[test]
    fn ipv6_colon_structure_enforced() {
        assert_eq!(normalize_ipv6("gg::1"), None);
        assert_eq!(normalize_ipv6("2409::8::1"), None); // 两个 ::
        assert_eq!(normalize_ipv6("2409:8057:2000:4:0:0:0:8:1"), None); // 9 组
        assert_eq!(normalize_ipv6("fe80:1:2:3:4:5:6"), None); // 7 组缺 ::
        assert_eq!(normalize_ipv6(":2409::1"), None); // 头部空组
        assert_eq!(normalize_ipv6("2409:8057:2000:4:0:0:0:80000"), None); // 组过长
        // zone id 与方括号展示形态
        assert_eq!(normalize_ipv6("fe80::1%eth2").as_deref(), Some("fe80::1"));
        assert_eq!(
            normalize_ipv6("[2409:8057::8]").as_deref(),
            Some("2409:8057::8")
        );
        // 恰好 8 组、无 :: —— 原样返回，不重新压缩
        assert_eq!(
            normalize_ipv6("2409:8057:2000:4:0:0:0:8").as_deref(),
            Some("2409:8057:2000:4:0:0:0:8")
        );
    }

    #[test]
    fn link_local_detection() {
        assert!(is_link_local_v6("fe80::1"));
        assert!(is_link_local_v6("FE80::200:11ff:fe12:1314"));
        assert!(is_link_local_v6("febf::1"));
        assert!(!is_link_local_v6("2409:8d5b:358:43b::1"));
        assert!(!is_link_local_v6("::1"));
        assert!(!is_link_local_v6(""));
    }

    #[test]
    fn looks_v6_discriminates_address_family() {
        assert!(looks_v6("2409:8057::8"));
        assert!(looks_v6("0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1")); // 占位也算 v6 位出现
        assert!(!looks_v6("10.5.23.212"));
        assert!(!looks_v6(""));
    }

    /// 手册 §12.2.10：IPv4 的字段 3 是「地址 + 掩码」8 段拼接。
    #[test]
    fn contrdp_parses_ipv4_with_subnet_mask() {
        let p = parse_contrdp_addr("10.5.23.212.255.255.255.0").unwrap();
        assert_eq!(p.addr, "10.5.23.212");
        assert_eq!(p.mask.as_deref(), Some("255.255.255.0"));
        assert!(!p.v6);
    }

    #[test]
    fn contrdp_accepts_bare_ipv4_from_old_firmware() {
        let p = parse_contrdp_addr("10.5.23.212").unwrap();
        assert_eq!(p.addr, "10.5.23.212");
        assert_eq!(p.mask, None);
        assert!(!p.v6);
    }

    #[test]
    fn contrdp_parses_dotted_ipv6_and_mask() {
        let addr = "36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8";
        let mask = "255.255.255.255.255.255.255.255.255.255.255.255.255.255.255.255";
        let p = parse_contrdp_addr(&format!("{}.{}", addr, mask)).unwrap();
        assert_eq!(p.addr, "2409:8057:2000::8");
        assert!(p.v6);
    }

    #[test]
    fn contrdp_rejects_dummy_and_bad_mask() {
        // 占位地址（全零 / ::1）必须拒绝
        assert!(parse_contrdp_addr("0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1").is_none());
        // 非连续掩码：保留地址、丢弃掩码
        let p = parse_contrdp_addr("10.5.23.212.255.0.255.0").unwrap();
        assert_eq!(p.addr, "10.5.23.212");
        assert_eq!(p.mask, None);
    }

    #[test]
    fn netmask_validation_requires_contiguous_ones() {
        assert!(is_valid_netmask4("255.255.255.0"));
        assert!(is_valid_netmask4("255.255.255.252"));
        assert!(is_valid_netmask4("255.255.255.255"));
        assert!(is_valid_netmask4("128.0.0.0"));
        assert!(!is_valid_netmask4("255.0.255.0"));
        assert!(!is_valid_netmask4("0.0.0.0"));
        assert!(!is_valid_netmask4("255.255.255"));
        assert!(!is_valid_netmask4("256.0.0.0"));
    }
}
