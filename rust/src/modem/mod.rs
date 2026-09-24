//! 模组状态查询与控制。
//!
//! 按职责切成五块，各自只依赖 [`crate::at`] 与 [`crate::config`]：
//!
//! | 子模块 | 职责 |
//! |---|---|
//! | [`info`] | 厂商 / 型号 / 固件 / IMEI / IMSI / ICCID / USB 模式 / SIM 槽 |
//! | [`signal`] | CSQ / CESQ / CEREG / COPS / GTCCINFO / GTCAINFO → 结构化信号 |
//! | [`band`] | GTCCINFO 的频段、带宽编码解码（三套编码）与 ARFCN 反推 |
//! | [`pdp`] | PDP 上下文状态、拨号、去激活、拨号前置体检 |
//! | [`bind`] | `AT+EMBIND` 数据通道绑定体检与改绑（决定数据给主机还是给模组自身） |
//! | [`lock`] | 锁频段 / 锁小区 / 制式优先级 / SIM / CFUN / USB 模式 |
//! | [`temp`] | 温度传感器 |
//!
//! 命令选型依据：
//!   - 《Fibocom FM350 AT Commands v2.2》标准命令（CSQ / CEREG / COPS /
//!     CGDCONT / CGACT / CGPADDR ...）；
//!   - 实机验证过的 FM350 扩展命令（`+GTACT` 锁制式频段、`+EMMCHLCK` 锁小区、
//!     `+GTCCINFO?` 邻区、`+GTSENRDTEMP=0` 温度、`+GTDNS` DNS）；
//!   - F22 原厂固件逆向得到的 MTK 工程命令 `+EMBIND`（`md1rom`
//!     0x16989c0 响应表，`M-CCMNI` / `M-RNDIS` / `M-MBIM` / `M-LHIF`
//!     四条 L2 通道，绑定由 D2RM 管理且随 RAT 变化）。详见 [`bind`]。
//!
//! 所有 IMEI / 串号写入类指令在 [`crate::at::guard`] 处拦截，
//! 只读查询（如 `AT+EGMREXT=0,7`）正常放行。

pub mod band;
pub mod bind;
pub mod info;
pub mod lock;
pub mod pdp;
pub mod signal;
pub mod temp;

use crate::at::{self, AtHandle, AtResult};
use crate::config::Config;

pub use band::{bandwidth_text, lte_band_from_code, lte_band_from_earfcn, nr_band_from_code, nr_band_from_arfcn, operator_name};
pub use bind::{rebind_to_host, BindState};
pub use info::{info, ModemInfo};
pub use lock::{
    cell_info, lock_band, lock_cell, lock_status, rat_order, reboot, set_cfun, set_rat_order,
    set_sim_slot, set_usb_mode,
};
pub use pdp::{apply_auth, cgact_state, dial, hangup, pdp, set_apn, DialOutcome, PdpState, Readiness};
pub use signal::{signal, Signal};
pub use temp::{sensor_name, Temperature};

/// 通用 AT 执行：优先走 daemon 持有的独占端口。
///
/// 走 IMEI 守卫（[`crate::imei::guard_transparent`]）：未开启 `imei_write`
/// 时拦截写入形式，开启后放行，便于维护者经内部路径调试。
/// 对外透传路径（`/api/at`、CLI `at`）则是无条件拦截，二者不冲突。
pub fn run(at: &AtHandle, cfg: &Config, cmd: &str) -> AtResult<String> {
    crate::imei::guard_transparent(cmd, cfg)?;
    at.with(cfg, |p| p.command(cmd))
}

/// 批量执行并把失败转成可读文本（供「原始命令回显」类接口使用）。
pub fn run_list(at: &AtHandle, cfg: &Config, cmds: &[&str]) -> Vec<(String, String)> {
    cmds.iter()
        .map(|c| {
            let r = run(at, cfg, c).unwrap_or_else(|e| format!("ERR: {}", e));
            (c.to_string(), r)
        })
        .collect()
}

/// 取响应中 `+XXX:` 后的字段（首个匹配行）。
pub fn f(resp: &str, prefix: &str) -> Vec<String> {
    at::fields(resp, prefix)
}

/// 综合状态（信息 + 信号 + PDP + 温度 + AT 通道）。
#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub info: ModemInfo,
    pub signal: Signal,
    pub pdp: PdpState,
    pub temperature: Option<Temperature>,
    pub at_port: String,
    pub at_ready: bool,
}

pub fn status(at: &AtHandle, cfg: &Config) -> Status {
    let ready = at.with(cfg, |p| p.command(at::at_cmd::AT)).is_ok();
    Status {
        info: info::info(at, cfg),
        signal: signal::signal(at, cfg),
        pdp: pdp::pdp(at, cfg),
        temperature: temp::temperature(at, cfg),
        at_port: cfg.at_port.clone(),
        at_ready: ready,
    }
}

/// 写短信中心号码。
pub fn set_sms_center(at: &AtHandle, cfg: &Config, number: &str) -> AtResult<String> {
    run(at, cfg, &at::at_cmd::set_sms_center(number))
}

/// 读一次数据通道绑定方向（`AT+EMBIND?`）。
///
/// 与 [`status`] 分开是有意的：状态刷新是高频路径，而这半双工 AT 通道
/// 每多一条命令就多一次等待。只在两种时机调用它 ——
/// 拨号成功但接口拿不到地址，以及周期性巡检里低频抽查。
/// 返回 `None` 表示命令不可用，调用方按「方向未知」处理，**不要**当成
/// 「方向正确」。
pub fn bind_status(at: &AtHandle, cfg: &Config) -> Option<BindState> {
    bind::read(at, cfg)
}
