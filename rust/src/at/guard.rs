//! AT 指令安全策略：IMEI / 串号写入拦截。
//!
//! 用户硬约束：**可以有 IMEI 相关功能（读取展示），但绝不允许写入、修改、
//! 擦除 IMEI / 串号。** 本模块是这条约束的唯一落点，`/api/at` 与 CLI `at`
//! 两条透传路径都必须先过这里。

/// IMEI / 串号相关命令族。
///
/// 判定采用**默认拒绝**：只要属于这些族且不是明确的只读形式，一律拦截。
const IMEI_FAMILIES: &[&str] = &["EGMREXT", "EGMR", "SIMEI", "CGSN", "SERIALNUM", "IMEI"];

/// 判断是否为 IMEI / 串号的**写入**指令。
///
/// 放行规则（与原实现逐条一致）：
///   1. 不属于 IMEI 命令族 → 不拦截；
///   2. 属于该族但为查询形式（`AT+CGSN`、`AT+CGSN?`、`AT+EGMR?`）→ 放行；
///   3. 属于该族且首参为 `0` 的读形式（`AT+EGMREXT=0,7`、`AT+EGMR=0,7`）→ 放行；
///   4. 其余一切携带参数的写形式（尤其 `AT+EGMREXT=1,...`、
///      `AT+EGMR=1,7,"xxx"`、`AT+SIMEI=...`、`AT+CGSN=...`）→ 拦截。
pub fn is_imei_write(cmd: &str) -> bool {
    let raw = cmd.trim();
    let upper = raw.to_uppercase();
    let body = upper.strip_prefix("AT+").unwrap_or(&upper).trim();

    let family = match IMEI_FAMILIES.iter().find(|f| body.starts_with(*f)) {
        Some(f) => *f,
        None => return false,
    };

    let rest = body[family.len()..].trim();
    if rest.is_empty() || rest.starts_with('?') {
        return false;
    }

    if let Some(args) = rest.strip_prefix('=') {
        let first = args.split(',').next().unwrap_or("").trim();
        if first == "0" {
            return false;
        }
    }

    true
}

/// 解释拦截原因，供 API 返回给前端。
pub fn block_reason(cmd: &str) -> String {
    format!(
        "已拒绝执行 `{}`：IMEI / 串号写入指令被安全策略拦截（只读查询不受影响）",
        cmd.trim()
    )
}

/// 模组信息读取使用的 IMEI 只读命令。
///
/// 原实现把 `0,7` 这个魔数写死在 `info()` 里；这里提为常量并注明语义，
/// 避免日后有人在"整理魔法数"时误改成写形式。
///
/// 字面量**不在此处重复定义**，直接指向 [`crate::at::cmd::EGMREXT_READ_IMEI`]
/// —— AT 指令的单一来源必须是 `cmd`，否则守卫与执行点会各自漂移。
pub const IMEI_READ_CMD: &str = crate::at::cmd::EGMREXT_READ_IMEI;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_forms_are_allowed() {
        for c in [
            "AT+EGMREXT=0,7",
            "at+egmrext=0,7",
            "AT+EGMR=0,7",
            "AT+CGSN",
            "AT+CGSN?",
            "AT+EGMR?",
            "AT+EGMREXT?",
            " AT+EGMREXT=0,7 \r\n",
            IMEI_READ_CMD,
        ] {
            assert!(!is_imei_write(c), "只读形式被误拦截: {}", c);
        }
    }

    #[test]
    fn write_forms_are_blocked() {
        for c in [
            "AT+EGMREXT=1,7,\"861234567890123\"",
            "AT+EGMR=1,7,\"861234567890123\"",
            "AT+EGMREXT=1,7",
            "AT+SIMEI=861234567890123",
            "AT+SIMEI=\"861234567890123\"",
            "AT+CGSN=861234567890123",
            "at+egmrext=1,7,\"x\"",
            "AT+EGMR=2,7,\"x\"",
        ] {
            assert!(is_imei_write(c), "写入形式未被拦截: {}", c);
        }
    }

    #[test]
    fn unrelated_commands_pass() {
        for c in [
            "AT+CSQ",
            "AT+CGDCONT=1,\"IPV4V6\",\"cmiot5g\"",
            "AT+CFUN=1,1",
            "AT+GTUSBMODE=40",
        ] {
            assert!(!is_imei_write(c), "普通命令被误拦截: {}", c);
        }
    }
}
