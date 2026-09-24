//! 统一日志出口。
//!
//! 重构前：`eprintln!("fm350d: ...")` 散落在 main.rs / net.rs 共 5 处以上，
//! 前缀硬写、无等级、无时间戳，daemon 巡检与一次性 CLI 的日志形态也不一致。
//! 这里收敛为单一出口，行为保持等价（仍写 stderr，仍带 `fm350d: ` 前缀），
//! 便于后续按需接入 syslog 而不改动调用点。

/// 日志等级。当前仅用于过滤，输出格式保持不变（不引入新依赖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 常规状态变化（拨号成功、地址变化、恢复动作已执行）。
    Info,
    /// 需要用户注意但不影响主流程（探测失败、外部接口冲突）。
    Warn,
}

const PREFIX: &str = "fm350d";

/// 打印一行日志。格式与重构前逐字一致：`fm350d: <正文>`。
pub fn log(level: Level, msg: &str) {
    match level {
        // 等级暂不改变输出形态，仅保留分类能力
        Level::Info | Level::Warn => eprintln!("{}: {}", PREFIX, msg),
    }
}

/// 常规日志。
pub fn info(msg: &str) {
    log(Level::Info, msg);
}

/// 告警日志。
pub fn warn(msg: &str) {
    log(Level::Warn, msg);
}

/// 格式化后打印，避免每个调用点手写 `format!`。
pub fn infof(args: std::fmt::Arguments) {
    log(Level::Info, &std::fmt::format(args));
}

/// 格式化后打印告警。
pub fn warnf(args: std::fmt::Arguments) {
    log(Level::Warn, &std::fmt::format(args));
}

/// 把 `log::infof(format_args!(...))` 缩写成 `infof!(...)`。
///
/// 两个分支：`infof!(format_args!("...", x))` 与 `infof!("...", x)` 都合法
/// （前者是主体代码的历史写法，保留以免大面积改动调用点）。
#[macro_export]
macro_rules! infof {
    (format_args!($($arg:tt)*)) => { $crate::log::infof(format_args!($($arg)*)) };
    ($($arg:tt)*) => { $crate::log::infof(format_args!($($arg)*)) };
}

/// 同上，告警版本。
#[macro_export]
macro_rules! warnf {
    (format_args!($($arg:tt)*)) => { $crate::log::warnf(format_args!($($arg)*)) };
    ($($arg:tt)*) => { $crate::log::warnf(format_args!($($arg)*)) };
}
