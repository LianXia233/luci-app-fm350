//! 请求超时预算：每类端点的放弃时限。
//!
//! ## 为什么必须有预算（LuCI 卡死防御的最后一环）
//!
//! rpcd 的 ucode 插件是**同步转发**：一次 ubus 调用堵住 rpcd 主循环多久，
//! 整个 LuCI（菜单、session、所有应用的 ubus）就堵多久。后端任何"慢而不
//! 死"的路径（哑口 AT 超时 ×N、拨号全流程）都必须在这里被钳住：到时放弃
//! 响应（任务照常完成，保证 AT 会话一致），前端拿到明确错误，rpcd 恢复调度。
//!
//! ## 与相邻层的对齐关系（改一处必须核对全链）
//!
//! ```text
//! api::budget（本文件，daemon 内） < rpcd fm350.uc TMO_READ(12s)/TMO_WRITE(90s)
//!   < 前端 api.js Promise.race（读 30s / 写 95s）
//! ```
//! daemon 的预算必须**小于** rpcd 的兜底，让"daemon 的明确错误"先到；
//! rpcd 的兜底必须**小于**前端护栏，让"代理的超时错误"先到。

use std::time::Duration;

use crate::config::Config;
use crate::rpc::Method;

/// 读类预算：健康请求 < 1.5 s，预算给到 10 s 覆盖慢查询；
/// 必须 < rpcd fm350.uc 的读类兜底（TMO_READ = 12 s）。
pub const READ: Duration = Duration::from_secs(10);

/// 写类预算下限（拨号全流程、锁频等长操作的最低保障）。
const WRITE_MIN: u64 = 60;
/// 写类预算上限：必须 ≤ CLI 写类转发上限（cli.rs CLI_WRITE_TIMEOUT_MAX=85）
/// 且 < rpcd 写类兜底（TMO_WRITE = 90 s）。
const WRITE_MAX: u64 = 85;

/// 属于"写类"的路径（POST）。这些操作会走完整 AT 写流程或长 shell 流程。
const WRITE_PATHS: &[&str] = &[
    "/api/dial",
    "/api/hangup",
    "/api/at",
    "/api/lock/band",
    "/api/lock/cell",
    "/api/rat",
    "/api/sim",
    "/api/cfun",
    "/api/usbmode",
    "/api/reboot",
    "/api/imei",
    "/api/imei/backup",
    "/api/sms/send",
    "/api/sms/delete",
    "/api/sms/storage",
    "/api/sms/smsc",
    "/api/sms",
    "/api/set",
    "/api/modexec",
    "/api/sshfwd/exec",
    "/api/sshfwd/bootstrap",
];

/// 计算一个请求的预算。
///
/// 写类 = `at_timeout` 钳位到 `[WRITE_MIN, WRITE_MAX]`（与 CLI 转发语义
/// 一致）；其余一律读类 [`READ`]。
pub fn for_request(cfg: &Config, method: Method, path: &str) -> Duration {
    if method == Method::Post && WRITE_PATHS.contains(&path) {
        Duration::from_secs(cfg.at_timeout.clamp(WRITE_MIN, WRITE_MAX))
    } else {
        READ
    }
}

/// RPC 传输层的 method 映射判据：该路径属于写类（POST）吗。
///
/// `rpc::handle` 用它把 `{"method":"dial"}` 映射成 POST `/api/dial`。
/// 与 [`WRITE_PATHS`] 单一出处，改写类清单不会漏改映射。
pub fn is_write_path(path: &str) -> bool {
    WRITE_PATHS.contains(&path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::Method as M;

    #[test]
    fn reads_get_fixed_budget() {
        let cfg = Config::default();
        assert_eq!(for_request(&cfg, M::Get, "/api/status"), READ);
        assert_eq!(for_request(&cfg, M::Get, "/api/ports"), READ);
        // 非写类 POST 同样按读类处理（config / ports 探测）
        assert_eq!(for_request(&cfg, M::Post, "/api/config"), READ);
    }

    #[test]
    fn writes_get_at_timeout_clamped() {
        let cfg = Config::default();
        // 默认 at_timeout=10 → 抬到下限 60
        assert_eq!(for_request(&cfg, M::Post, "/api/dial"), Duration::from_secs(60));
        // 过大时压回上限
        let mut big = Config::default();
        big.at_timeout = 600;
        assert_eq!(for_request(&big, M::Post, "/api/at"), Duration::from_secs(85));
    }

    /// 链路对齐守护：读类预算必须小于 rpcd ucode 读类兜底（12 s），
    /// 否则 rpcd 先超时，用户看到的是代理错误而非后端的真实错误。
    #[test]
    fn read_budget_below_rpcd_read_tmo() {
        assert!(READ < Duration::from_secs(12));
    }

    /// 写类上限必须低于 rpcd 写类兜底（90 s），给序列化/传输留余量。
    #[test]
    fn write_max_below_rpcd_write_tmo() {
        assert!(WRITE_MAX < 90);
    }

    #[test]
    fn write_paths_are_exhaustive_for_control_ops() {
        let cfg = Config::default();
        for p in [
            "/api/dial",
            "/api/hangup",
            "/api/at",
            "/api/reboot",
            "/api/sms/send",
            "/api/lock/band",
        ] {
            assert_ne!(
                for_request(&cfg, M::Post, p),
                READ,
                "{} 应属于写类预算",
                p
            );
        }
    }
}
