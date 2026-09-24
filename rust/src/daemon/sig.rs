//! 参数签名巡检：裸 `uci set` / 服务重启后把运行态重新拉回与 UCI 一致。
//!
//! ## 解决什么问题
//!
//! 本插件只在**拨号成功 / 地址变化**这两种时机下发接口配置。于是存在一类
//! 静默失配：用户（或别的脚本）绕过 LuCI 直接 `uci set network.fm350.xxx`
//! 并 `service network restart`，netifd 按新 UCI 把接口拉起来了，但本插件的
//! 内部记账还停留在旧值 —— 此后只要地址不变，就再也不会下发，运行态与 UCI
//! 的分歧会一直挂着，直到下次重拨才偶然对齐。
//!
//! 这里用「当前 UCI 参数签名 vs 上次成功下发时的签名」来发现这类分歧，
//! 发现后**按当前 UCI/内核状态原样重下发一次**。
//!
//! ## 两条必须遵守的边界
//!
//! 1. **签名文件不存在时只登记、不下发**。首次安装 / 重启后 `/var/run` 被
//!    清空都会走到这个分支，此时 netifd 刚按 UCI 拉起，运行态天然同步，
//!    再弹一次接口是纯粹的骚扰（也是早期版本开机那一下莫名断网的原因）。
//! 2. **下发失败不覆盖签名**。这样下一轮会继续重试，而不是把一次失败永久
//!    记账成「已同步」。

use crate::config::Config;
use crate::{infof, warnf};
use crate::net;

/// 签名巡检并按需重下发。
pub fn reapply_on_change(cfg: &Config) {
    if !cfg.enabled {
        return;
    }
    let sig = net::config_sig(cfg);
    let Some(prev) = net::applied_sig() else {
        // 首次运行 / 重启后运行目录被清空：netifd 已按 UCI 拉起，无需弹跳
        net::mark_applied(cfg);
        return;
    };
    if prev == sig {
        return;
    }

    let ns = net::status(cfg);
    if ns.ipv4.is_empty() && ns.ipv6.is_empty() {
        // 接口上什么都没有：属于「地址未下发」范畴，交给拨号巡检处理，
        // 这里不要重复兜底，否则两个巡检会互相抢着 ifup。
        return;
    }

    // 用 UCI 里已记录的网关作模组网关候选：既保住原网关选择，也保住
    // ARP 实测已经通过的事实（不必重新探测一遍）。
    let gw4 = net::current_gateway(cfg).unwrap_or_default();
    let v4 = ns.ipv4.first().map(|s| s.as_str()).unwrap_or("");
    let v6 = ns.ipv6.first().map(|s| s.as_str()).unwrap_or("");
    let dns = net::configured_dns(cfg);

    match net::apply_after_dial(cfg, v4, v6, &dns, &gw4) {
        Ok(_) => infof!(format_args!("网络参数已变更，已重新下发接口配置")),
        Err(e) => warnf!(format_args!("网络参数变更后重下发失败: {}", e)),
    }
}
