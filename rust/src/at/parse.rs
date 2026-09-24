//! AT 响应解析：统一入口。
//!
//! ## 为什么需要这个模块
//!
//! 重构前同一个「按行扫前缀 → 去 `+XXX:` → 按逗号拆 → 去引号」的句式在
//! `modem.rs` 里被手写了 5 份（CGDCONT / CGACT / CGCONTRDP / GTCCINFO /
//! GTSENRDTEMP），另有 3 套互不兼容的变体（`fields()` 取首行、`clean_value()`
//! 取首个非空行、`run_list()` 干脆不解析）。任何一处改解析规则都要同步 4
//! 处，是本次重构要消灭的头号重复源。
//!
//! 这里提供唯一的解析入口，语义与原实现**逐条对齐**：
//!   - `rows()`   对应手写扫描（多行响应，每行一组字段）
//!   - `first()`  对应旧 `at::fields()`（取首个匹配行）
//!   - `scalar()` 对应旧 `clean_value()`（单值命令，如 AT+CGMI）
//!   - `first_ipv4()` 保持原有实现

/// 遍历响应中所有以 `prefix` 开头的信息行，返回每行的字段向量。
///
/// `prefix` 传完整前缀（如 `"+CGDCONT:"` 或 `"+CGDCONT"`），内部统一按
/// 「以该前缀开头」匹配，与原先各处的 `starts_with` 判定等价。
///
/// 字段处理：按 `,` 切分，逐项 `trim()` 并剥掉两侧双引号。
pub fn rows<'a>(resp: &'a str, prefix: &str) -> impl Iterator<Item = Vec<String>> + 'a {
    let p = prefix.to_string();
    resp.lines().filter_map(move |line| {
        let l = line.trim();
        if !l.starts_with(p.as_str()) {
            return None;
        }
        let rest = l[p.len()..].trim().trim_start_matches(':').trim();
        Some(
            rest.split(',')
                .map(|s| s.trim().trim_matches('"').to_string())
                .collect(),
        )
    })
}

/// 取首个匹配行的字段向量（旧 `at::fields()` 的等价物）。
pub fn first(resp: &str, prefix: &str) -> Vec<String> {
    rows(resp, prefix).next().unwrap_or_default()
}

/// 取首个匹配行的指定下标字段；越界或不存在返回 `None`。
pub fn field(resp: &str, prefix: &str, idx: usize) -> Option<String> {
    first(resp, prefix).get(idx).cloned().filter(|s| !s.is_empty())
}

/// 单值命令取值（旧 `clean_value()` 的等价物）。
///
/// 适用于 `AT+CGMI` / `AT+CGMR` / `AT+GTUSBMODE?` 这类只回一个标量的命令：
///   1. 优先取 `+XXX: ` 之后的内容；
///   2. 否则取首个既非空、又不是结果码的行。
pub fn scalar(resp: &str, prefix: &str) -> Option<String> {
    for line in resp.lines() {
        let l = line.trim();
        if l.is_empty() || is_result_code(l) {
            continue;
        }
        if let Some(rest) = l.strip_prefix(prefix) {
            let v = rest.trim().trim_start_matches(':').trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
            continue;
        }
        if !l.starts_with('+') {
            return Some(l.trim_matches('"').to_string());
        }
    }
    None
}

/// 是否为 AT 结果码行（`OK` / `ERROR` / `+CME ERROR: ...` 等）。
pub fn is_result_code(line: &str) -> bool {
    let l = line.trim();
    matches!(l, "OK" | "ERROR" | "BUSY" | "NO CARRIER" | "NO ANSWER" | "NO DIALTONE")
        || l.starts_with("+CME ERROR")
        || l.starts_with("+CMS ERROR")
}

/// 响应是否以 `OK` 结束。
pub fn is_ok(resp: &str) -> bool {
    resp.lines()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l == "OK")
        .unwrap_or(false)
}

/// 响应是否包含 `ERROR`（含 `+CME ERROR` / `+CMS ERROR`）。
pub fn has_error(resp: &str) -> bool {
    resp.contains("ERROR")
}

/// 从响应中取首个 IPv4（用于 `AT+CGPADDR` 兜底）。
///
/// 判定保持原实现：四段点分十进制、纯数字与点、排除 `0.0.0.0` 与
/// `255.255.255.255`。
pub fn first_ipv4(resp: &str) -> Option<String> {
    for line in resp.lines() {
        for token in line.split(&[',', '"', ' '][..]) {
            let t = token.trim();
            if t.split('.').count() == 4 && t.chars().all(|c| c.is_ascii_digit() || c == '.') {
                if !t.starts_with("0.0.0.0") && t != "255.255.255.255" {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

/// 取所有 `+XXX:` 行的原始文本（不拆字段）。
///
/// 用于载波聚合这类"结构未定、先原样呈现"的场景（旧代码里 `ca` 就是这么取的）。
pub fn raw_lines(resp: &str, prefix: &str, limit: usize) -> Vec<String> {
    resp.lines()
        .filter(|l| l.trim_start().starts_with(prefix))
        .map(|l| l.trim().to_string())
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_splits_multi_line_response() {
        let resp = "+CGDCONT: 1,\"IPV4V6\",\"cmiot5g\",\"\",0,0\n+CGDCONT: 2,\"IP\",\"3gnet\"\nOK";
        let all: Vec<Vec<String>> = rows(resp, "+CGDCONT").collect();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0][0], "1");
        assert_eq!(all[0][1], "IPV4V6");
        assert_eq!(all[0][2], "cmiot5g");
        assert_eq!(all[1][2], "3gnet");
    }

    #[test]
    fn first_and_field_match_legacy_behaviour() {
        let resp = "+CSQ: 99,99\nOK";
        assert_eq!(first(resp, "+CSQ"), vec!["99".to_string(), "99".to_string()]);
        assert_eq!(field(resp, "+CSQ", 0).as_deref(), Some("99"));
        assert_eq!(field(resp, "+CSQ", 9), None);
        assert_eq!(field(resp, "+NOPE", 0), None);
    }

    #[test]
    fn scalar_reads_prefixed_and_bare_values() {
        assert_eq!(scalar("+CGMI: Fibocom\nOK", "+CGMI").as_deref(), Some("Fibocom"));
        assert_eq!(scalar("FM350-GL\nOK", "+CGMM").as_deref(), Some("FM350-GL"));
        assert_eq!(scalar("+CME ERROR: phone failure", "+CGMR"), None);
    }

    #[test]
    fn result_code_and_ok_detection() {
        assert!(is_ok("+CSQ: 1,2\nOK"));
        assert!(!is_ok("+CSQ: 1,2\nERROR"));
        assert!(is_result_code("+CME ERROR: phone failure"));
        assert!(has_error("+CME ERROR: 100"));
        assert!(!has_error("OK"));
    }

    #[test]
    fn first_ipv4_filters_placeholder_addresses() {
        assert_eq!(first_ipv4("+CGPADDR: 1,\"10.1.2.3\"").as_deref(), Some("10.1.2.3"));
        assert_eq!(first_ipv4("+CGPADDR: 1,\"0.0.0.0\""), None);
        assert_eq!(first_ipv4("+CGPADDR: 1,\"255.255.255.255\""), None);
    }
}
