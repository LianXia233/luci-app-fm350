//! 守护主体：一轮巡检的编排（Tokio 事件驱动）。
//!
//! ## 分层
//!
//! ```text
//! CLI (cli.rs) ──► api.rs ──┐
//!                           ├─► daemon::run ──► 各巡检器（阻塞池）
//!      LuCI (rpcd) ─────────┘        │
//!                                    └─ api::serve（异步 task，每连接独立）
//! ```
//!
//! 本模块只做**编排**：持有跨轮状态、按固定顺序调用各巡检器、控制巡检周期。
//! 所有具体判定都下沉到子模块，便于单独单测。
//!
//! ## 运行模型（Tokio 化后）
//!
//! `run()` 本身是 async fn，跑在 `rt::start` 构建的 current_thread 事件循环：
//!
//! - API 服务是独立 task（`tokio::spawn`），accept/报文/预算全异步；
//! - 巡检循环由 `tokio::time::sleep` 驱动（可被取消、不占线程）；
//! - 每个阻塞段（AT 会话 + shell 子进程）经 `rt::blocking_scope` 派发到
//!   **阻塞池**，事件循环在等待期间照常处理 API 请求与定时器；
//! - 巡检器自身实现保持同步（逐字未改），move-in/move-out 模式传递。
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
use std::time::Duration;

use crate::at::AtHandle;
use crate::config;
use crate::infof;
use crate::rt;
use crate::warnf;

pub mod dial;
pub mod gt;
pub mod guard;
pub mod port;
pub mod sig;
pub mod singleton;
pub mod sshfwd;
pub mod v6;

pub use dial::{redial, DialWatch, NO_ADDR_REDIAL_ROUNDS, NO_SERVICE_RETRY_INTERVAL};
pub use guard::{ConflictWatch, DataGuard, NetGuard};
pub use gt::GtWatch;
pub use port::PortWatch;
pub use singleton::{acquire, Singleton, LOCK_FILE};
pub use sshfwd::SshFwdWatch;
pub use v6::V6Watch;

/// 巡检周期下限。
///
/// 每次巡检至少要走一遍 `AT+CGACT?` / `AT+CGPADDR`，且本插件与模组之间是
/// 半双工 AT 通道（命令间隔有下限，见 [`crate::at::port`]）。周期低于 5 s
/// 会把 AT 通道压满，表现为状态刷新本身开始超时。
pub const MIN_POLL_INTERVAL: u64 = 5;

/// SSH 转发巡检的节流周期（SshFwdWatch 内部另有细粒度节流）。
const SSHFWD_TICK: Duration = Duration::from_secs(15);

/// 守护主循环（async，永不返回，除非阻塞池/巡检致命失败）。
pub async fn run(cfg_initial: config::Config) -> Result<(), String> {
    let at = Arc::new(AtHandle::new());

    // 优雅关闭：SIGTERM（procd 重启/停服）与 SIGINT 触发关闭信号 ——
    // API 停止 accept、巡检循环在当前睡眠点退出，进程正常返回。
    // Windows 构建无 unix signal（仅本地开发），直接不支持优雅退出。
    let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
    let sd_tx2 = sd_tx.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            // 每个信号一个监听 task（无 select! 依赖）：任一到达即触发关闭。
            let sd1 = sd_tx2.clone();
            tokio::spawn(async move {
                let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM 失败");
                let _ = term.recv().await;
                let _ = sd1.send(true);
            });
            let sd2 = sd_tx2.clone();
            tokio::spawn(async move {
                let mut int = signal(SignalKind::interrupt()).expect("注册 SIGINT 失败");
                let _ = int.recv().await;
                let _ = sd2.send(true);
            });
            // 主监听等待两个 task 之一触发（信号到达前挂起，无 CPU 消耗）。
            let mut sd_self = sd_tx2.subscribe();
            while !*sd_self.borrow() {
                if sd_self.changed().await.is_err() {
                    break;
                }
            }
            return;
        }
        #[cfg(not(unix))]
        {
            // 非 unix 平台：等待一个永不触发的信号（不优雅关闭）。
            let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
            let _ = rx.await;
        }
        crate::infof!(format_args!("收到退出信号，正在优雅关闭"));
        let _ = sd_tx.send(true);
    });
    let mut sd_api = sd_rx.clone();
    let mut sd_loop = sd_rx.clone();

    // API 服务独立 task：事件循环同时服务巡检定时器。
    // AT 句柄 Clone 共享，API 侧与巡检侧都进同一个 FIFO 邮箱串行执行。
    tokio::spawn({
        let at = Arc::clone(&at);
        let cfg0 = cfg_initial;
        async move {
            if let Err(e) = crate::api::serve(at, cfg0, sd_api).await {
                warnf!(format_args!("API 退出: {}", e));
            }
        }
    });

    infof!(format_args!("守护已启动"));

    // 巡检器有跨轮状态，且阻塞池任务要求 'static：用元组携带，每段
    // move-in / move-out（`w.N` 取出 → 闭包内 tick → 写回）。
    let mut w = (
        DialWatch::new(),    // w.0 拨号
        V6Watch::new(),      // w.1 IPv6
        DataGuard::new(),    // w.2 数据面
        NetGuard::new(),     // w.3 公网连通
        ConflictWatch::new(),// w.4 接口冲突
        PortWatch::new(),    // w.5 AT 口
        GtWatch::new(),      // w.6 EIF（留在事件循环，纯内存）
        SshFwdWatch::new(),  // w.7 SSH 转发
    );
    let mut last_ssh = tokio::time::Instant::now();

    loop {
        // 每轮重新读配置：用户在 LuCI 改的参数下一轮即生效，无需重启服务。
        // （config::load 会 fork 二十余次 uci，本身是阻塞段 —— 放进池里。）
        let cfg = rt::blocking_scope(config::load).await?;
        let interval = cfg.poll_interval.max(MIN_POLL_INTERVAL);

        // EIF 兜底加速：消费上一轮巡检期间被截流的 URC 事件（纯内存）。
        // 地址类事件会置起加速标志，让本轮末尾的睡眠压到秒级。
        w.6.tick(&cfg);

        // —— 阻塞段 1：拨号巡检 + IPv6（AT + shell）——
        {
            let at1 = Arc::clone(&at);
            let c1 = cfg.clone();
            let (d, v) = (w.0, w.1);
            let (d, v) = rt::blocking_scope(move || {
                let mut d = d;
                let mut v = v;
                d.tick(&at1, &c1, &mut v);
                (d, v)
            })
            .await?;
            w.0 = d;
            w.1 = v;
        }

        // —— 阻塞段 2：参数签名 / 路由守卫 / 接口冲突 / 数据面 / 公网连通 ——
        let net_purified = {
            let at2 = Arc::clone(&at);
            let c2 = cfg.clone();
            let (data, netg, conflict) = (w.2, w.3, w.4);
            let (conflict, data, netg, purified, route_fixed) = rt::blocking_scope(move || {
                sig::reapply_on_change(&c2);
                let route_fixed = c2.route_guard && crate::net::route_guard(&c2);
                let mut conflict = conflict;
                let mut data = data;
                let mut netg = netg;
                conflict.tick(&c2);
                data.tick(&at2, &c2);
                let purified = netg.tick(&at2, &c2);
                (conflict, data, netg, purified, route_fixed)
            })
            .await?;
            w.2 = data;
            w.3 = netg;
            w.4 = conflict;
            if route_fixed {
                infof!(format_args!("已补齐默认设备路由"));
            }
            purified
        };
        if net_purified {
            w.0.defer_after_external_purification();
        }

        // —— 阻塞段 3：AT 口看护（可能释放/改选句柄）——
        {
            let at3 = Arc::clone(&at);
            let c3 = cfg.clone();
            let port = w.5;
            let (port, _at_stats) = rt::blocking_scope(move || {
                let mut port = port;
                port.release_on_change(&at3, &c3);
                port.reselect_if_down(&at3, &c3);
                let st = port.observe_exclusivity(&at3, &c3);
                (port, st)
            })
            .await?;
            w.5 = port;
        }

        // —— SSH 转发巡检：不依赖 AT 句柄，内部再节流；关闭态零副作用 ——
        if last_ssh.elapsed() >= SSHFWD_TICK {
            last_ssh = tokio::time::Instant::now();
            let c4 = cfg.clone();
            let sshfwd = w.7;
            w.7 = rt::blocking_scope(move || {
                let mut sshfwd = sshfwd;
                sshfwd.tick(&c4);
                sshfwd
            })
            .await?;
        }

        // 加速标志在取走睡眠时长之后才清零，保证「事件 → 短睡眠」链路生效。
        // tokio 定时器：睡眠期间事件循环照常服务 API 请求。
        let sleep = w.6.sleep_interval(interval);
        w.6.consume_accel();
        // 睡眠可被关闭信号打断（timeout 轮询形态，不引入 tokio macros）：
        // 收到即退出主循环（在途巡检段天然结束）。
        // 睡眠分片轮询关闭信号（500ms 粒度，不引入 tokio macros）：
        // 收到即退出主循环，在途巡检段天然结束。
        let mut remaining = sleep;
        loop {
            if *sd_loop.borrow() {
                infof!(format_args!("巡检循环退出，fm350d 已优雅关闭"));
                return Ok(());
            }
            let slice = remaining.min(std::time::Duration::from_millis(500));
            tokio::time::sleep(slice).await;
            remaining = remaining.saturating_sub(slice);
            if remaining.is_zero() {
                break;
            }
        }
    }
}

