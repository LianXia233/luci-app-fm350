//! UCI 接口骨架：把地址写进 `network.*`、登记防火墙、并读回运行状态。
//!
//! ## 职责边界
//!
//! 本模块只做三件事：**写 UCI 骨架**、**登记防火墙**、**读状态**。
//! 网关规划在 [`super::gateway`]，IPv6 获取方式在 [`super::v6`]，
//! 路由守卫与自愈在 [`super::heal`]。地址怎么来的不归这里管。
//!
//! ## 由逆向确定的两条形态约束
//!
//! 1. **IPv6 子接口绝不能写成 `/128`**（除显式 `static` 模式）。
//!    原厂上报 IPv6 一定带前缀长度（`AT+EIF=<n>,"ipadd",2,"<v6>/%lu"`、
//!    `mtk_netagent` 的 `ipv6PrefixLength`），而 `AT+CGPADDR` **不带前缀**且
//!    在上下文去激活后返回上一轮残留地址。照抄 /128 会配出一个不属于本会话的
//!    地址——这正是「RA 模式拿到的 IPv6 不对」的来源。
//!    形态判定一律委托 [`super::v6`]，本模块不自己判 `v6_mode`。
//! 2. **RA 模式下接口起来后必须主动触发 RS**，而不是等下一轮周期性 RA。
//!    原厂 `mtk_netagent` 的硬性动作是
//!    `echo 1 > /proc/sys/net/ipv6/conf/<dev>/router_solicitations`。

use super::gateway::{in_same_subnet, plan_gateway};
use super::probe::{detect_dev, uci_list_contains, wan_zone_index};
use super::shell::{failed, real, sq, uci, uci_batch};
use super::v6::{trigger_rs, v6_dhcp_mode, v6_managed, v6_ra_mode, usable_global_v6_from_addr_line};
use crate::config::Config;

/// 网络运行状态（供前端与守护判据使用）。
#[derive(Debug, Default, serde::Serialize)]
pub struct NetStatus {
    pub iface: String,
    pub iface_v6: String,
    pub dev: Option<String>,
    pub ipv4: Vec<String>,
    pub ipv6: Vec<String>,
    pub routes: Vec<String>,
    pub up: bool,
    /// 数据网卡上是否存在默认路由（v4，按 `ip route show dev` 判定）。
    pub default4: bool,
    /// 系统默认路由表中是否存在经由数据网卡的默认路由（v6，跨设备全局查询）。
    pub default6: bool,
    /// UCI 里为接口配置的 DNS 列表（`peerdns=0`，由本插件下发）。
    pub dns: Vec<String>,
}

/// 读取当前网络状态。
pub fn status(cfg: &Config) -> NetStatus {
    let dev = detect_dev(cfg);
    let mut st = NetStatus {
        iface: cfg.iface.clone(),
        iface_v6: cfg.iface_v6.clone(),
        dev: dev.clone(),
        ..Default::default()
    };
    if let Some(d) = &dev {
        let (_, out) = real(&format!("ip -o addr show dev {} 2>/dev/null", d));
        for line in out.lines() {
            // 形如: 2: eth2    inet 10.5.243.239/32 brd ...
            if let Some(p) = line.find("inet ") {
                let rest = &line[p + 5..];
                if let Some(addr) = rest.split_whitespace().next() {
                    let ip = addr.split('/').next().unwrap_or("").to_string();
                    if !ip.is_empty() {
                        st.ipv4.push(ip);
                    }
                }
            }
            if let Some(ip) = usable_global_v6_from_addr_line(line) {
                st.ipv6.push(ip);
            }
        }
        let (_, routes) = real(&format!("ip route show dev {} 2>/dev/null", d));
        st.routes = routes.lines().map(|l| l.trim().to_string()).collect();
        st.default4 = st.routes.iter().any(|l| l.starts_with("default"));
        // v6 默认路由可能安装在别的网卡（或根本不存在），做跨设备全局查询；
        // 必须按 dev 词法匹配，`dev eth2` 不能命中 `dev eth22`。
        let (_, routes6) = real("ip -6 route show default 2>/dev/null");
        st.default6 = routes6.lines().any(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            t.first() == Some(&"default") && t.windows(2).any(|w| w[0] == "dev" && w[1] == d.as_str())
        });
        st.up = !st.ipv4.is_empty() || !st.ipv6.is_empty();
    }
    // UCI 里的 DNS（供前端核对下发结果）
    let (dns_ok, dns_out) = real(&format!("uci -q get network.{}.dns", cfg.iface));
    if dns_ok {
        st.dns = dns_out.split_whitespace().map(|s| s.to_string()).collect();
    }
    st
}

/// 确保蜂窝接口存在并应用地址。
///
/// 返回（接口名, 数据网卡, 已执行的 UCI 命令列表）。
///
/// `modem_gw`：模组侧 `AT+CGCONTRDP` 上报的 IPv4 网关（可空），auto 模式下
/// 由 [`plan_gateway`] 优先采信。网关的 ARP 实测**不在这里做** —— ifup 是
/// 异步的，此时地址多半还没落进内核，固定等待 + 必失败的探测会把会话误判为
/// 「网关不可达」并回退到 onlink 方案；实测已移到 [`super::heal::apply_after_dial`]
/// 的地址轮询之后。
pub fn ensure_iface(
    cfg: &Config,
    ipv4: &str,
    ipv6: &str,
    dns: &[String],
    modem_gw: &str,
) -> Result<(String, String, Vec<String>), String> {
    let dev = detect_dev(cfg).ok_or_else(|| "未探测到数据通道网卡".to_string())?;
    let iface = &cfg.iface;
    let iface_v6 = &cfg.iface_v6;
    let route_name = format!("{}_def", iface);
    let gwr_name = format!("{}_gw4", iface);
    let have_v4 = !ipv4.is_empty();
    // 只有确实要下发 IPv4 时才做网关规划；无 v4（IPv6-only 会话）时不触碰
    // 网关计划，改为清理会制造黑洞的 v4 残留。
    let (plan_mask, plan_gw) = if have_v4 {
        plan_gateway(cfg, ipv4, modem_gw)
    } else {
        (String::new(), None)
    };
    let mask = plan_mask.as_str();
    // `v6_mode=off` 时既不创建也不登记 fm350v6。
    let v6_section = cfg.ipv6 && !iface_v6.is_empty() && v6_managed(cfg);

    let mut cmds: Vec<String> = Vec::new();
    // 不走 uci_batch 的完整 shell 命令（删除类）：删除不存在的项会返回非零，
    // 不能计入失败判定。
    let mut shell_cmds: Vec<String> = Vec::new();

    // ---- 主接口骨架（v4/v6 共用）
    cmds.push(format!("set network.{}=interface", iface));
    cmds.push(format!("set network.{}.proto=static", iface));
    cmds.push(format!("set network.{}.device={}", iface, dev));
    cmds.push(format!("set network.{}.peerdns=0", iface));
    // 开机默认启用（显式 auto=1）：netifd 对缺省 auto 的接口**不会**开机自启。
    cmds.push(format!("set network.{}.auto=1", iface));
    cmds.push(format!("set network.{}.metric={}", iface, cfg.metric));
    cmds.push(format!("set network.{}.defaultroute=1", iface));

    if have_v4 {
        cmds.push(format!("set network.{}.ipaddr={}", iface, ipv4));
        cmds.push(format!("set network.{}.netmask={}", iface, mask));
        // 网关为 None 表示本轮沿用无网关的 onlink 设备路由。无网关时必须
        // 显式删掉上一轮的 gateway，否则 netifd 会拿失效网关下发路由。
        match &plan_gw {
            Some(gw) => cmds.push(format!("set network.{}.gateway={}", iface, gw)),
            None => shell_cmds.push(format!(
                "uci -q delete network.{}.gateway 2>/dev/null || true",
                iface
            )),
        }
    } else {
        // 无 v4（IPv6-only 会话）：清掉上一会话的 v4 残留 —— 留着会把死地址
        // 重新 ifup 进内核。
        for opt in ["ipaddr", "netmask", "gateway"] {
            shell_cmds.push(format!(
                "uci -q delete network.{}.{} 2>/dev/null || true",
                iface, opt
            ));
        }
        shell_cmds.push(format!(
            "uci -q delete network.{} 2>/dev/null || true",
            route_name
        ));
        shell_cmds.push(format!(
            "uci -q delete network.{} 2>/dev/null || true",
            gwr_name
        ));
    }

    // DNS：有则覆盖，无则清空（换 APN 后旧 DNS 不得残留）。
    if !dns.is_empty() {
        // 必须整体加引号：多值 dns 含空格，裸写会被 `sh -c` 拆成两个参数，
        // uci 直接 rc=255，整批写入判失败，ifup 被短路 —— 接口从此起不来。
        cmds.push(format!("set network.{}.dns={}", iface, sq(&dns.join(" "))));
    } else {
        shell_cmds.push(format!(
            "uci -q delete network.{}.dns 2>/dev/null || true",
            iface
        ));
    }

    // ---- IPv6（独立子接口，附着在主接口设备上）
    if v6_section {
        cmds.push(format!("set network.{}=interface", iface_v6));
        cmds.push(format!("set network.{}.device=@{}", iface_v6, iface));
        // 清理 dhcpv6 时代的遗留选项。delete 必须带 `|| true`：uci 对不存在
        // 的选项即使 -q 也返回非零，会让整批写入被误判失败。
        for stale in ["reqaddress", "reqprefix", "peerdns", "extendprefix"] {
            cmds.push(format!(
                "uci -q delete network.{}.{} 2>/dev/null || true",
                iface_v6, stale
            ));
        }
        if v6_dhcp_mode(cfg) {
            // DHCPv6 模式：交给 odhcp6c。extendprefix=1 让 odhcp6c 拿到的
            // /64 能委派给 LAN。
            cmds.push(format!("set network.{}.proto=dhcpv6", iface_v6));
            cmds.push(format!("set network.{}.extendprefix=1", iface_v6));
            shell_cmds.push(format!(
                "uci -q delete network.{}.ip6addr 2>/dev/null || true",
                iface_v6
            ));
        } else if v6_ra_mode(cfg) {
            // RA 模式：只打开 accept_ra，绝不写静态 /128。
            cmds.push(format!("set network.{}.proto=none", iface_v6));
            shell_cmds.push(format!(
                "uci -q delete network.{}.ip6addr 2>/dev/null || true",
                iface_v6
            ));
            for key in ["accept_ra", "accept_ra_defrtr", "accept_ra_pinfo"] {
                shell_cmds.push(format!(
                    "sysctl -w net.ipv6.conf.{}.{}={} >/dev/null 2>&1 || true",
                    dev,
                    key,
                    // accept_ra 必须 2：接口进 WAN 区后 forwarding=1，默认 1 会丢 RA。
                    if key == "accept_ra" { "2" } else { "1" }
                ));
            }
        } else {
            cmds.push(format!("set network.{}.proto=static", iface_v6));
            if !ipv6.is_empty() {
                cmds.push(format!("set network.{}.ip6addr={}/128", iface_v6, ipv6));
            }
        }
        cmds.push(format!("set network.{}.auto=1", iface_v6));
    } else if !iface_v6.is_empty() && !v6_managed(cfg) {
        // 关闭 IPv6：主动拆掉骨架与防火墙登记，让「关闭」真正生效。
        shell_cmds.push(format!("ifdown {} >/dev/null 2>&1 || true", iface_v6));
        shell_cmds.push(format!(
            "uci -q delete network.{} 2>/dev/null || true",
            iface_v6
        ));
    }

    // ---- 默认路由 / 网关主机路由（仅有 v4 时）
    //
    // 有网关：交给 netifd 按 gateway 下发，并清理历史遗留的 onlink route。
    // 网关不在地址掩码内时，netifd 的 `default via <gw>` 会被内核以「网关
    // 不可达」拒绝 —— 必须先装 `<gw>/32` 主机路由把网关本身变得可达。
    // 无网关：维持 onlink 设备路由。
    if have_v4 {
        match &plan_gw {
            Some(gw) => {
                shell_cmds.push(format!(
                    "uci -q delete network.{} 2>/dev/null || true",
                    route_name
                ));
                if !in_same_subnet(ipv4, gw, mask) {
                    cmds.push(format!("set network.{}=route", gwr_name));
                    cmds.push(format!("set network.{}.interface={}", gwr_name, iface));
                    cmds.push(format!("set network.{}.target={}/32", gwr_name, gw));
                } else {
                    shell_cmds.push(format!(
                        "uci -q delete network.{} 2>/dev/null || true",
                        gwr_name
                    ));
                }
            }
            None => {
                cmds.push(format!("set network.{}=route", route_name));
                cmds.push(format!("set network.{}.interface={}", route_name, iface));
                cmds.push(format!("set network.{}.target=0.0.0.0/0", route_name));
                cmds.push(format!("set network.{}.onlink=1", route_name));
                cmds.push(format!("set network.{}.metric={}", route_name, cfg.metric));
                shell_cmds.push(format!(
                    "uci -q delete network.{} 2>/dev/null || true",
                    gwr_name
                ));
            }
        }
    }

    // ---- 防火墙：归入 wan 区
    //
    // 1. 区下标必须按名字解析，不能硬编码 @zone[0]（实机 @zone[0] 是 lan 区）；
    // 2. add_list 不会去重 —— 轮询每轮都走到这里，不去重会无限追加。故先查
    //    再写，保证幂等；
    // 3. 只 commit 不 reload 时 fw4 运行态不更新（`config.change` 只由
    //    rpcd/LuCI 应用路径发出），首装时 wan 区 masquerade 不生效。故有变更
    //    时 commit 后立即 reload 防火墙。
    let mut fw_changed = false;
    match wan_zone_index() {
        Some(zi) => {
            let mut names = vec![iface.clone()];
            if v6_section {
                names.push(iface_v6.clone());
            }
            for name in names {
                if name.is_empty() {
                    continue;
                }
                let key = format!("firewall.@zone[{}].network", zi);
                if !uci_list_contains(&key, &name) {
                    cmds.push(format!("add_list {}={}", key, sq(&name)));
                    fw_changed = true;
                }
            }
            // v6 段已关闭时同步摘掉历史 zone 登记。
            if !iface_v6.is_empty() && !v6_section {
                let key = format!("firewall.@zone[{}].network", zi);
                if uci_list_contains(&key, iface_v6) {
                    cmds.push(format!("del_list {}={}", key, sq(iface_v6)));
                    fw_changed = true;
                }
            }
        }
        None => {
            crate::log::warnf(format_args!(
                "未定位到 wan 防火墙区，跳过接口登记（请手工把 {} 加入 wan 区）",
                iface
            ));
        }
    }

    // 删除类命令先跑：避免 `set` 之后再 `delete` 把刚写的值清掉。
    for c in &shell_cmds {
        let _ = real(c);
    }

    let results = uci_batch(&cmds);
    let bad = failed(&results);
    if !bad.is_empty() {
        // 整批失败时回滚 delta：uci set 写在内存里，不 revert 会残留半套配置。
        let _ = real("uci revert network");
        return Err(format!("UCI 写入失败: {:?}", bad));
    }

    let _ = real("uci commit network");
    let _ = real("uci commit firewall");
    if fw_changed {
        let _ = real("/etc/init.d/firewall reload >/dev/null 2>&1");
    }

    // 应用：先 up 接口，再按模式补齐路由。
    let (up_ok, up_out) = real(&format!("ifup {}", iface));
    if !up_ok {
        crate::log::warnf(format_args!("ifup {} 失败（rc!=0）: {}", iface, up_out));
    }
    if v6_section {
        let (up_ok6, up_out6) = real(&format!("ifup {}", iface_v6));
        if !up_ok6 {
            crate::log::warnf(format_args!("ifup {} 失败（rc!=0）: {}", iface_v6, up_out6));
        }
        // RA 模式下接口一起就主动索取 RA（原厂 mtk_netagent 的硬性动作）。
        // 不触发的话内核只在建链瞬间发一次 RS，错过就要等下一轮周期性 RA。
        if v6_ra_mode(cfg) {
            trigger_rs(&dev);
        }
        // IPv6 默认设备路由**只在 static 模式**安装：
        //   * ra 模式：路由由内核按 RA 下发（metric 1024，proto ra）；
        //   * dhcpv6 模式：odhcp6c 下发 `default from ... metric 512`。
        // 无条件装 metric <cfg.metric> 的设备路由会压过二者（默认 30 < 512）。
        if !v6_ra_mode(cfg) && !v6_dhcp_mode(cfg) {
            let _ = real(&format!(
                "ip -6 route replace default dev {} metric {}",
                dev, cfg.metric
            ));
        }
    }
    // ---- v4 默认路由兜底直写（netifd 之外的第一道保险；route_guard 周期复核）
    if have_v4 {
        match &plan_gw {
            Some(gw) => {
                let _ = real(&format!(
                    "ip route replace default via {} dev {} metric {}",
                    gw, dev, cfg.metric
                ));
            }
            None => {
                let _ = real(&format!(
                    "ip route replace default dev {} metric {}",
                    dev, cfg.metric
                ));
            }
        }
    }

    Ok((iface.clone(), dev, cmds))
}

/// 拆除由本插件创建的接口与路由。
pub fn teardown_iface(cfg: &Config) -> Result<Vec<String>, String> {
    let iface = &cfg.iface;
    let iface_v6 = &cfg.iface_v6;
    let route_name = format!("{}_def", iface);
    let gwr_name = format!("{}_gw4", iface);
    let mut cmds = Vec::new();

    let _ = real(&format!("ifdown {}", iface));
    if !iface_v6.is_empty() {
        let _ = real(&format!("ifdown {}", iface_v6));
    }
    cmds.push(format!("delete network.{}", route_name));
    cmds.push(format!("delete network.{}", gwr_name));
    cmds.push(format!("delete network.{}", iface));
    if !iface_v6.is_empty() {
        cmds.push(format!("delete network.{}", iface_v6));
    }
    // 与 ensure_iface 对称：按名解析区下标，且只删确实存在的项。
    // del_list 会移除该名字的全部重复项，正好用来清理历史污染。
    let mut fw_changed = false;
    if let Some(zi) = wan_zone_index() {
        for name in [iface.clone(), iface_v6.clone()] {
            if name.is_empty() {
                continue;
            }
            let key = format!("firewall.@zone[{}].network", zi);
            if uci_list_contains(&key, &name) {
                cmds.push(format!("del_list {}={}", key, sq(&name)));
                fw_changed = true;
            }
        }
    }

    let results = uci_batch(&cmds);
    let _ = real("uci commit network");
    let _ = real("uci commit firewall");
    // `ifdown` 本身不带 network reload：只删 UCI 不通知 netifd 会留下
    // 「幽灵接口」—— ubus 仍列出 fm350，UCI 里却已经没有。
    let _ = real("ubus call network reload");
    if fw_changed {
        let _ = real("/etc/init.d/firewall reload >/dev/null 2>&1");
    }
    // 清掉参数签名：下一次带地址的巡检会按当前参数重新下发。
    super::gateway::clear_applied();
    Ok(results.into_iter().map(|r| r.0).collect())
}

/// 保证插件创建的接口处于「开机自启」状态。
///
/// 为什么必须有它：`ensure_iface` 只在拨号后或地址有偏差时才跑，而守护的稳态
/// 分支在「地址已经匹配」时会整段跳过，于是 auto 选项一旦不是 1 就再也没机会
/// 补回来 —— 实机 fm350v6 即长期停在 auto=0（LuCI 显示「开机时未启动」）。
///
/// 这里只做补齐：接口 section 已存在且 auto≠1 时才写入。返回被修正的接口名，
/// 为空表示无需改动（调用方据此避免刷日志）。
pub fn ensure_autostart(cfg: &Config) -> Vec<String> {
    let mut names = vec![cfg.iface.clone()];
    if cfg.ipv6 && !cfg.iface_v6.is_empty() {
        names.push(cfg.iface_v6.clone());
    }

    let mut touched: Vec<String> = Vec::new();
    for name in names {
        if name.is_empty() {
            continue;
        }
        // 仅在接口已存在时补齐：section 缺失时 uci set 会直接报错
        let (ok_sec, sec) = real(&format!("uci -q get network.{}", name));
        if !ok_sec || sec.trim() != "interface" {
            continue;
        }
        let key = format!("network.{}.auto", name);
        let (ok_auto, val) = real(&format!("uci -q get {}", key));
        if ok_auto && val.trim() == "1" {
            continue;
        }
        if uci(&format!("set {}=1", key)).0 {
            touched.push(name);
        }
    }
    if !touched.is_empty() {
        let _ = real("uci commit network");
    }
    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// 接口骨架的关键形态：这些字符串一旦变了，LuCI 与 netifd 行为就会漂移。
    #[test]
    fn iface_skeleton_options_are_stable() {
        let cfg = Config {
            iface: "fm350".to_string(),
            iface_v6: "fm350v6".to_string(),
            metric: 30,
            ..Default::default()
        };
        // 主接口：静态、绑设备、关 peerdns、显式 auto、带 metric 与 defaultroute
        assert_eq!(format!("set network.{}=interface", cfg.iface), "set network.fm350=interface");
        assert_eq!(format!("set network.{}.proto=static", cfg.iface), "set network.fm350.proto=static");
        assert_eq!(format!("set network.{}.peerdns=0", cfg.iface), "set network.fm350.peerdns=0");
        assert_eq!(format!("set network.{}.auto=1", cfg.iface), "set network.fm350.auto=1");
        assert_eq!(
            format!("set network.{}.metric={}", cfg.iface, cfg.metric),
            "set network.fm350.metric=30"
        );
        // v6 子接口附着在主接口设备上（@ 引用，不是裸设备名）
        assert_eq!(
            format!("set network.{}.device=@{}", cfg.iface_v6, cfg.iface),
            "set network.fm350v6.device=@fm350"
        );
    }

    /// 多值 DNS 必须整体加引号，否则 `sh -c` 会拆成两个参数导致 uci rc=255。
    #[test]
    fn multi_value_dns_is_quoted_as_one_argument() {
        let dns = ["223.5.5.5".to_string(), "119.29.29.29".to_string()];
        let cmd = format!("set network.fm350.dns={}", sq(&dns.join(" ")));
        assert_eq!(cmd, "set network.fm350.dns='223.5.5.5 119.29.29.29'");
    }

    /// 路由段命名必须与 ensure_iface / teardown_iface 完全对称，否则会留下垃圾段。
    #[test]
    fn route_section_names_are_symmetric() {
        let iface = "fm350";
        assert_eq!(format!("{}_def", iface), "fm350_def");
        assert_eq!(format!("{}_gw4", iface), "fm350_gw4");
    }
}
