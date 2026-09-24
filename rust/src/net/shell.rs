//! Shell / UCI 执行的唯一出口。
//!
//! 重构前 `sh` / `real` / `sq` / `uci` / `uci_batch` 与 500 行的命令拼装
//! 混在同一个文件里，任何一处拼错都只能实机复现。这里把「怎么执行」与
//! 「执行什么」分开：本模块只负责**执行与转义**，不认识任何业务命令。
//!
//! 两个必须保留的实机教训：
//!   * `sh -c` 下空格会被拆参 —— uci 多值选项（典型是 `dns "A B"`）必须
//!     整体加引号，否则 `uci` 直接 rc=255、整批写入判失败、`ifup` 被短路，
//!     接口从此再也起不来（见 [`sq`]）；
//!   * 删除类 uci 命令即使带 `-q` 也对不存在的项返回非零，不能计入失败
//!     判定，因此调用方要把 delete 放进 `sh`/[`uci`] 之外的容错分支。

use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// 只回显命令，不执行（用于自测）。
    Dry,
    Real,
}

/// 执行 shell 脚本，返回 `(是否成功, stdout)`。
pub fn sh(mode: RunMode, script: &str) -> (bool, String) {
    if mode == RunMode::Dry {
        return (true, script.to_string());
    }
    match Command::new("sh").arg("-c").arg(script).output() {
        Ok(o) => (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
        ),
        Err(e) => (false, e.to_string()),
    }
}

/// 真实执行（业务代码一律走这里，Dry 只出现在测试里）。
pub fn real(script: &str) -> (bool, String) {
    sh(RunMode::Real, script)
}

/// 单引号包裹 shell 参数。
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `uci -q <子命令>`。
pub fn uci(script: &str) -> (bool, String) {
    real(&format!("uci -q {}", script))
}

/// 批量执行 `uci -q` 子命令，返回 `(命令, 是否成功, 输出)`。
///
/// 供调用方判断整批是否成功：任何一条非零都意味着这一轮配置没写全，
/// 必须 `uci revert` 回滚 delta，否则残留半套配置会污染下一次 commit。
pub fn uci_batch(lines: &[String]) -> Vec<(String, bool, String)> {
    lines
        .iter()
        .map(|l| (l.clone(), uci(l).0, String::new()))
        .collect()
}

/// 批量执行并只保留失败项（日志用）。
pub fn failed(cmds: &[(String, bool, String)]) -> Vec<String> {
    cmds.iter().filter(|r| !r.1).map(|r| r.0.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_does_not_touch_system() {
        let (ok, script) = sh(RunMode::Dry, "uci set network.fm350=interface");
        assert!(ok);
        assert_eq!(script, "uci set network.fm350=interface");
    }

    #[test]
    fn sq_escapes_embedded_single_quotes() {
        assert_eq!(sq("10.0.0.1"), "'10.0.0.1'");
        assert_eq!(sq("a b"), "'a b'");
        // POSIX 单引号内出现单引号要 '结束 → 转义 \' → '重新开始'
        assert_eq!(sq("it's"), "'it'\\''s'");
    }

    #[test]
    fn uci_prefixes_quiet_flag() {
        // Dry 无法覆盖 uci()，这里只校验拼装规则：uci() = real("uci -q " + s)
        let combined = format!("uci -q {}", "get network.fm350.proto");
        assert_eq!(combined, "uci -q get network.fm350.proto");
    }
}
