//! 主机侧网络层：把模组拿到的地址变成 OpenWrt 上真正能上网的接口。
//!
//! ## 边界（由 F22 原厂固件逆向给出，不是经验之谈）
//!
//! 数据面**终止在 MD 内部**：MD 的 `usbcore → rndis_* → ipcore → PDN`，
//! AP/主机的 Linux 内核不在数据路径上。由此推出三条硬约束：
//!
//! 1. 主机侧只会看到一张 USB 网卡（`rndis_host` / `cdc_ether` / `cdc_ncm` /
//!    `qmi_wwan` / `mbim`），**永远看不到 `ccmni%d`** —— 那是 AP 域内核内置的
//!    CCCI 网卡，只服务模组自己的 OpenWrt（`mtk_netagent`）。
//! 2. 地址来源不是本插件拍脑袋决定的：IPv4/IPv6 同属一个 PDP 上下文
//!    （默认 `IPV4V6`），但**默认路由不同表**（v4 走 main，v6 走独立表）。
//! 3. `AT+EMBIND` 的 L2P 落点是**互斥**的：绑 `M-CCMNI` 就给模组自己，绑
//!    `M-RNDIS` / `M-MBIM` 才给主机。拨号成功却拿不到地址时，先查这里
//!    （见 [`crate::modem::bind`]）。
//!
//! ## 子模块职责
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`shell`] | 所有外部命令的唯一出口（`sh -c` / `uci`），集中转义与失败判定 |
//! | [`probe`] | 数据网卡探测、连通性探测、数据面停滞判定 |
//! | [`gateway`] | IPv4 网关规划、ARP 实测、参数签名与幂等落盘 |
//! | [`v6`] | IPv6 获取方式判定、RA 索取、地址解析、子接口形态 |
//! | [`iface`] | UCI 接口骨架与防火墙登记（v4/v6 主入口） |
//! | [`heal`] | 路由守卫、自愈动作、接口重启 |

pub mod gateway;
pub mod heal;
pub mod iface;
pub mod probe;
pub mod shell;
pub mod v6;

pub use gateway::{
    applied_sig, clear_applied, config_sig, configured_dns, current_gateway, derive_gateway,
    dns_matches, mark_applied, plan_gateway, probe_gateway,
};
pub use heal::{
    apply_after_dial, bounce_data_dev, bounce_iface_v4, bounce_iface_v6, line_has_metric,
    reset_usb_data_dev, route_guard,
};
pub use iface::{ensure_autostart, ensure_iface, status, teardown_iface, NetStatus};
pub use probe::{
    check_connectivity4, check_connectivity6, data_health, data_plane_stalled, detect_dev,
    foreign_ifaces_on_dev, DataHealth,
};
pub use shell::{failed, real, sh, uci, uci_batch, RunMode};
pub use v6::{
    apply_ipv6_addr, enable_and_solicit_v6_ra, enable_v6_ra, has_global_v6, needs_refresh,
    ra_v6_ready, refresh_ipv6_iface, solicit_and_wait_ra, trigger_rs, v6_dhcp_mode, v6_managed,
    v6_ra_mode, v6_static_mode, RefreshKind, V6_FALLBACK_METRIC,
};

/// 数据网卡驱动白名单（不含 `ccmni`：它属于 AP 域，主机侧不可见）。
///
/// 保留在此便于外部直接引用；判定实现见 [`probe::DATA_DRIVERS`]。
pub use probe::DATA_DRIVERS;
