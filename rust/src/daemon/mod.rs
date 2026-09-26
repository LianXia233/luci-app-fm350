//! 守护主体：一轮巡检的编排。
//!
//! ## 分层
//!
//! ```text
//! CLI (cli.rs) ──► api.rs ──┐
//!                           ├─► daemon::run ──► 各巡检器
//!      LuCI (rpcd) ─────────┘
//! ```
//!
//! 本模块只做**编排**：持有跨轮状态、按固定顺序调用各巡检器、控制巡检周期。
//! 所有具体判定都下沉到子模块，便于单独单测。
//!
//! ## 巡检器职责
//!
//! | 巡检器 | 模块 | 关注什么 |
//! |---|---|---|
//! | 拨号 | [`dial`] | PDP 是否激活、地址是否落地、一直没地址是否该重拨 |
//! | IPv6 | [`v6`] | 模组侧 IPv6 变化、接口缺地址时的恢复时机 |
//! | 参数签名 | [`sig`] | UCI 被外部改动后把运行态拉回一致 |
//! | 数据面 | [`guard::DataGuard`] | 数据网卡计数器 stall |
//! | 公网连通 | [`guard::NetGuard`] | 双栈各自能否出公网 |
//! | 接口冲突 | [`guard::ConflictWatch`] | 同一数据网卡上是否有别的插件也在建接口 |
//! | AT 口 | [`port`] | 配置热切换、端口消失改选、独占事实 |
//! | EIF 兜底 | [`gt`] | `+GT*` URC 消费、地址事件触发巡检加速（可选增强） |
//!
//! ## 顺序为什么是这个顺序
//!
//! 拨号/地址必须排在最前：后续所有巡检都假定「链路上有地址」才有意义
//! （无地址时连通性探测交回地址巡检，见 [`guard::NetGuard`] 的 `last_ok = None`
//! 语义）。AT 口看护排在最后：它可能在本轮释放句柄，放在最前会让本轮所有
//! AT 访问都变成重新打开，白白多一次串口握手。

use std::sync::Arc;

use crate::at::AtHandle;
use crate::config;
use crate::infof;
use crate::net;

pub mod dial;
pub mod gt;
pub mod guard;
pub mod port;
pub mod sig;
pub mod singleton;
pub mod v6;

pub use dial::{redial, DialWatch, NO_ADDR_REDIAL_ROUNDS, NO_SERVICE_RETRY_INTERVAL};
pub use guard::{ConflictWatch, DataGuard, NetGuard};
pub use gt::GtWatch;
pub use port::PortWatch;
pub use singleton::{acquire, Singleton, LOCK_FILE};
pub use v6::V6Watch;

/// 巡检周期下限。
///
/// 每次巡检至少要走一遍 `AT+CGACT?` / `AT+CGPADDR`，且本插件与模组之间是
/// 半双工 AT 通道（命令间隔有下限，见 [`crate::at::port`]）。周期低于 5 s
/// 会把 AT 通道压满，表现为状态刷新本身开始超时。
pub const MIN_POLL_INTERVAL: u64 = 5;

/// 守护主循环（不返回，除非 API 线程与巡检同时失败到无法继续）。
pub fn run(cfg_initial: config::Config) -> Result<(), String> {
    let at = Arc::new(AtHandle::new());

    // API 服务放到子线程，主线程跑巡检任务。
    // AT 句柄是 Arc 共享的，API 侧与巡检侧串行访问同一把互斥锁。
    let at_api = Arc::clone(&at);
    let api_cfg = cfg_initial.clone();
    std::thread::spawn(move || {
        if let Err(e) = crate::api::serve(at_api, api_cfg) {
            crate::warnf!(format_args!("API 退出: {}", e));
        }
    });

    infof!(format_args!("守护已启动"));

    let mut v6 = V6Watch::new();
    let mut dial = DialWatch::new();
    let mut data = DataGuard::new();
    let mut netg = NetGuard::new();
    let mut conflict = ConflictWatch::new();
    let mut port = PortWatch::new();
    let mut gt = GtWatch::new();

    loop {
        // 每轮重新读配置：用户在 LuCI 改的参数下一轮即生效，无需重启服务。
        let cfg = config::load();
        let interval = cfg.poll_interval.max(MIN_POLL_INTERVAL);

        // EIF 兜底加速：消费上一轮巡检期间被截流的 URC 事件。地址类事件
        // 会置起加速标志，让本轮末尾的睡眠压到秒级，下一轮尽快复核地址。
        gt.tick(&cfg);

        dial.tick(&at, &cfg, &mut v6);
        sig::reapply_on_change(&cfg);

        if cfg.route_guard && net::route_guard(&cfg) {
            infof!(format_args!("已补齐默认设备路由"));
        }

        conflict.tick(&cfg);
        data.tick(&at, &cfg);
        if netg.tick(&at, &cfg) {
            dial.defer_after_external_purification();
        }

        port.release_on_change(&at, &cfg);
        port.reselect_if_down(&at, &cfg);
        port.observe_exclusivity(&at, &cfg);

        // 加速标志在取走睡眠时长之后才清零，保证「事件 → 短睡眠」链路生效
        let sleep = gt.sleep_interval(interval);
        gt.consume_accel();
        std::thread::sleep(sleep);
    }
}
