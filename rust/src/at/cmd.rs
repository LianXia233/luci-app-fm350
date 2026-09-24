//! AT 指令目录：所有命令字面量与构造函数的**唯一**出处。
//!
//! ## 为什么要有它
//!
//! 重构前 AT 命令字符串散落在 `modem.rs` 各处（`info()` 一张表、`signal()`
//! 若干字面量、`pdp()` 里 `format!` 拼出来的半截命令）。任何一处拼错都
//! 只能靠实机复现才发现，而 FM350 的私有命令出错时往往只回
//! `+CME ERROR: phone failure`，信息量极低。集中之后：
//!   * 命令字面量与它的**逆向依据**写在一起，改动有据可查；
//!   * 带参数的命令统一由构造函数生成，避免引号/逗号拼写差异；
//!   * 命令的「可用形态」被显式声明（读形式 / 写形式 / 只写），
//!     调用方不必再靠试错。
//!
//! ## 命令形态速查（实机验证，FM350-GL / MOLY.NR15.R3）
//!
//! | 命令 | 读形式 | 写形式 | 备注 |
//! |---|---|---|---|
//! | `+GTUSBMODE` | `?` 可用 | `=40/41` | 会引起 USB 重枚举，AT 口会短暂消失 |
//! | `+GTDUALSIM` | `?` 可用 | `=<0|1>` | 切卡 |
//! | `+GTACT` | `?` 可用 | `=<rat>,...` | 需先 `CFUN=0`，写完 `CFUN=1` |
//! | `+EMMCHLCK` | `?` 可用 | `=1,.../ =0` | 锁 PCI，同上需下电 |
//! | `+EPRATL` | `?` **可用** | `=<n>,<rat...>` | 手册只写了写形式，实机读形式可回 `+EPRATL:2,128,4` |
//! | `+GTSENRDTEMP` | `?` **不可用** | `=0` | 读形式回 `+CME ERROR: phone failure` |
//! | `+CGACT` | `?` 可用 | `=1/0,<cid>` | 激活判据的权威来源 |
//! | `+CGCONTRDP` | `=<cid>` | — | 未激活时仍回上一轮残留地址 |
//! | `+CGPADDR` | `=<cid>` | — | 地址权威来源；无 v6 时第三位给 `::1` 占位 |
//! | `+EMBIND` | `?` 可用 | `=1,"<L2P>",<cid>` | 数据通道绑定，按 cid 生效；由 MD 侧 D2RM 执行，合法性随 RAT 变化 |

use crate::config::Config;

// ---------------------------------------------------------------- 基础

/// 通道存活探测。
pub const AT: &str = "AT";
/// 关闭回显（握手阶段下发，之后模组不再回显命令本身）。
pub const ATE0: &str = "ATE0";
/// 打开详细错误上报（`+CME ERROR: <num>` 取代裸 `ERROR`）。
pub const CMEE_VERBOSE: &str = "AT+CMEE=2";

/// 3GPP TS 27.007 `<auth_type>`：0=none，1=PAP，2=CHAP，3=PAP+CHAP。
pub fn auth_code(auth: &str) -> u8 {
    match auth {
        "pap" => 1,
        "chap" => 2,
        "both" => 3,
        _ => 0,
    }
}

// ---------------------------------------------------------------- 模组信息

pub const CGMI: &str = "AT+CGMI";
pub const CGMM: &str = "AT+CGMM";
pub const CGMR: &str = "AT+CGMR";
pub const CGSN: &str = "AT+CGSN";
pub const CIMI: &str = "AT+CIMI";
pub const ICCID: &str = "AT+ICCID";
pub const GTUSBMODE_READ: &str = "AT+GTUSBMODE?";
pub const GTDUALSIM_READ: &str = "AT+GTDUALSIM?";
pub const CSCA_READ: &str = "AT+CSCA?";

/// USB 模式写入（40 = RNDIS+AT；41 为另一档 RNDIS 组合）。
///
/// **副作用**：切换会触发 USB 重新枚举，AT 口与数据网卡都会短暂消失。
/// 调用方必须在下发后等待重枚举完成，不能立即追问。
pub fn set_usb_mode(mode: u32) -> String {
    format!("AT+GTUSBMODE={}", mode)
}

/// 切换 SIM 卡槽。
pub fn set_sim_slot(slot: u32) -> String {
    format!("AT+GTDUALSIM={}", slot)
}

/// 写短信中心号码。
///
/// `145` = 国际号码（带 `+` 或 `86` 前缀），`161` = 国内号码。
pub fn set_sms_center(number: &str) -> String {
    let international = number.starts_with('+') || number.starts_with("86");
    let normalized = if number.starts_with('+') {
        number.to_string()
    } else if number.starts_with("86") {
        format!("+{}", number)
    } else {
        number.to_string()
    };
    format!(
        r#"AT+CSCA="{}",{}"#,
        normalized,
        if international { 145 } else { 161 }
    )
}

// ---------------------------------------------------------------- 信号与注册

pub const CSQ: &str = "AT+CSQ";
/// 扩展信号质量（3GPP TS 27.007 §8.69）。
///
/// 字段顺序：`<rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<ss_rsrq>,<ss_rsrp>,<ss_sinr>`。
/// 本模组上 `AT+CSQ` 恒为 `99,99`（不可用），信号必须读本命令。
pub const CESQ: &str = "AT+CESQ";
pub const CEREG_READ: &str = "AT+CEREG?";
pub const CREG_READ: &str = "AT+CREG?";
pub const COPS_READ: &str = "AT+COPS?";
/// 服务小区 / 邻区（Fibocom 扩展，手册 11.1.15）。
pub const GTCCINFO_READ: &str = "AT+GTCCINFO?";
/// 载波聚合。
pub const GTCAINFO_READ: &str = "AT+GTCAINFO?";

// ---------------------------------------------------------------- PDP 与拨号

pub const CGDCONT_READ: &str = "AT+CGDCONT?";
pub const CGACT_READ: &str = "AT+CGACT?";

/// 写 PDP 上下文：`AT+CGDCONT=<cid>,"<type>","<apn>"`。
pub fn set_apn(cid: u32, pdp_type: &str, apn: &str) -> String {
    format!(r#"AT+CGDCONT={},"{}","{}""#, cid, pdp_type, apn)
}

/// 写鉴权：`AT+CGAUTH=<cid>,<auth>[,<user>,<pass>]`。
///
/// `auth=none` 时也必须显式下发 `=0`，否则模组会沿用上一次会话的凭据。
pub fn set_auth(cid: u32, auth: &str, username: &str, password: &str) -> String {
    let code = auth_code(auth);
    if code == 0 {
        return format!("AT+CGAUTH={},0", cid);
    }
    format!(
        r#"AT+CGAUTH={},{},"{}","{}""#,
        cid, code, username, password
    )
}

/// 激活 PDP。重复激活会回 `+CME ERROR: 5847`，属**可容忍**结果。
pub fn activate(cid: u32) -> String {
    format!("AT+CGACT=1,{}", cid)
}

/// 去激活 PDP。
pub fn deactivate(cid: u32) -> String {
    format!("AT+CGACT=0,{}", cid)
}

/// 读 PDP 上下文动态参数（地址 / 网关 / DNS）。
///
/// **陷阱**：上下文去激活后本命令仍会返回上一轮的残留地址，
/// 只能用来取地址，绝不能作为激活判据。
pub fn contrdp(cid: u32) -> String {
    format!("AT+CGCONTRDP={}", cid)
}

/// 读 PDP 地址（本模组最可靠的地址来源）。
pub fn pdp_addr(cid: u32) -> String {
    format!("AT+CGPADDR={}", cid)
}

/// 读 DNS（Fibocom 扩展，同时给 v4/v6 服务器）。
pub fn gtdns(cid: u32) -> String {
    format!("AT+GTDNS={}", cid)
}

// ---------------------------------------------------------------- 短信（PDU）

/// 切 PDU 模式。FM350 与原厂 WebUI 一致走 PDU（非 TEXT）：TEXT 模式下中文
/// 与长短信都不可靠。
pub const CMGF_PDU: &str = "AT+CMGF=0";

/// 字符集设为 GSM 7-bit 默认表（UCS2 内容由 TP-DCS 字段单独指定，
/// 与本设置无关）。
pub const CSCS_GSM: &str = "AT+CSCS=\"GSM\"";

/// 列出全部短信（`4` = ALL）。
pub const CMGL_ALL: &str = "AT+CMGL=4";

/// 发送短信：`<len>` 是 **TPDU 字节数**（不含 SMSC 长度字节）。
pub fn cmgs(tpdu_len: usize) -> String {
    format!("AT+CMGS={}", tpdu_len)
}

/// 删除指定序号短信。
pub fn cmgd(index: i64) -> String {
    format!("AT+CMGD={}", index)
}

/// 读当前短信存储（`+CPMS: <mem>,<used>,<total>`）。
pub const CPMS_READ: &str = "AT+CPMS?";

/// 设置短信存储。
///
/// 实机 `AT+CPMS=?` 只返回 `("SM")`，**不支持 "ME"** —— 传 "ME" 会直接
/// `ERROR`，因此默认值只能是 "SM"。
pub fn cpms_set(mem: &str) -> String {
    format!("AT+CPMS=\"{}\",\"{}\",\"{}\"", mem, mem, mem)
}

// ---------------------------------------------------------------- IMEI / 串号

/// 读 IMEI（**只读**，始终允许）。
///
/// `AT+EGMREXT` 未被 Fibocom FM350 AT 手册（v2.2 / V2.10）收录，属社区与
/// 实机验证可用的扩展命令。`0` = 读，`7` = IMEI 项。
pub const EGMREXT_READ_IMEI: &str = "AT+EGMREXT=0,7";

/// 写 IMEI（**高危不可逆**，仅 [`crate::imei::write`] 允许调用）。
///
/// 写入类命令在本模块集中构造，通用 AT 透传路径（`/api/at`、CLI `at`）由
/// [`crate::at::is_imei_write`] 无条件拦截，避免被误触发。
pub fn egmrext_write_imei(imei: &str) -> String {
    format!("AT+EGMREXT=1,7,\"{}\"", imei)
}

// ---------------------------------------------------------------- 数据通道绑定

/// 数据通道绑定开关（MTK `AT+E*` 工程命令族，**不是** Fibocom 扩展）。
///
/// 逆向依据：`md1rom` 0x16989c0 起的响应格式串，与 `+EMSESS` / `+EMFRQ` /
/// `+EMSESSCFG` 同表：
///
/// ```text
/// +EMBIND: %d
/// ,"M-CCMNI",%d
/// ,"M-RNDIS",%d
/// ,"M-MBIM",%d
/// ,"M-LHIF",%d
/// +EMBIND: (0,1),(list of supported <L2P>s),(1-16)
/// ```
///
/// 三参数：`1=<enable>`（0 解绑 / 1 绑定）、`2=<L2P>`、`3=<cid 1-16>`。
/// 绑定动作由 MD 侧 **D2RM** 执行，状态机为 bind / unbind / rebind，
/// 且合法性校验**与当前 RAT 绑定**（固件内字符串 `rebind, invalid rat`、
/// `[D2RM_DB_ERROR] unbind cnf fail, rat = %d, err = %d`）。
/// 因此改绑必须在**已注册**之后进行，否则很容易吃到 `invalid rat`。
///
/// ## 这条命令决定插件能不能上网
///
/// `M-CCMNI` 与 `M-RNDIS` 是**互斥的落点，不是上下游**：
///
/// | L2P | 数据去向 | 谁拿到 IP |
/// |---|---|---|
/// | `M-CCMNI` | MD → CCCI → AP 域内核 `ccmni%d` | 模组自己的 OpenWrt（`mtk_netagent` 下发） |
/// | `M-RNDIS` | MD 内部 RNDIS → USB | **外部主机** |
/// | `M-MBIM` | MD 内部 MBIM → USB | **外部主机** |
/// | `M-LHIF` | MD 内部 | 不对外 |
///
/// 结论：本插件运行在主机侧，**不能**把 `ccmni0..19` 当数据网卡
/// （主机侧 `/sys/class/net` 里根本没有它）；反过来说，若业务 cid 被绑到了
/// `M-CCMNI`，主机侧必然拿不到地址 —— 这是「拨号成功但接口起不来」
/// 的一类根因，见 [`crate::modem::bind`]。

/// `M-CCMNI`：数据落到 AP 域内核的 `ccmni%d`，只服务模组自身，主机不可见。
pub const L2P_CCMNI: &str = "M-CCMNI";
/// `M-RNDIS`：数据经 USB RNDIS 送到外部主机（本插件默认依赖的通道）。
pub const L2P_RNDIS: &str = "M-RNDIS";
/// `M-MBIM`：数据经 USB MBIM 送到外部主机。
pub const L2P_MBIM: &str = "M-MBIM";
/// `M-LHIF`：MD 内部通道，不对外暴露。
pub const L2P_LHIF: &str = "M-LHIF";

/// 读当前绑定状态。
pub const EMBIND_READ: &str = "AT+EMBIND?";

/// 查询本固件支持的绑定组合：`+EMBIND: (0,1),(<L2P>...),(1-16)`。
pub const EMBIND_TEST: &str = "AT+EMBIND=?";

/// 建立绑定：`AT+EMBIND=1,"<l2p>",<cid>`。
pub fn bind_l2p(l2p: &str, cid: u32) -> String {
    format!("AT+EMBIND=1,\"{}\",{}", l2p, cid)
}

/// 解除绑定：`AT+EMBIND=0,"<l2p>",<cid>`。
pub fn unbind_l2p(l2p: &str, cid: u32) -> String {
    format!("AT+EMBIND=0,\"{}\",{}", l2p, cid)
}

// ---------------------------------------------------------------- 锁频 / 锁小区 / 制式

pub const GTACT_READ: &str = "AT+GTACT?";
pub const EMMCHLCK_READ: &str = "AT+EMMCHLCK?";
pub const EPRATL_READ: &str = "AT+EPRATL?";

/// `AT+GTACT=<rat>[,<pre1>[,<pre2>[,<band...>]]]`
///
/// `<rat>`：1=UMTS，2=LTE，4=LTE/UMTS，**10=Automatic**，14=NR-RAN，
/// 16=NR-RAN/WCDMA，17=NR-RAN/LTE，20=NR-RAN/WCDMA/LTE（三模全开）。
/// 注意 **20 不是自动，10 才是**；下发 10 之后查询会回显 20（手册 Note 6）。
pub fn set_gtact(param: &str) -> String {
    format!("AT+GTACT={}", param)
}

/// 锁小区 / PCI。`=0` 为取消。
pub fn set_emmchclk(param: &str) -> String {
    format!("AT+EMMCHLCK={}", param)
}

/// 制式优先顺序：`<rat>` 取 2=UMTS / 4=LTE / 128=NR。
///
/// 编码**与 `+GTACT` 的 `<rat>` 不同**，不要混用。
pub fn set_eprartl(codes: &[&str]) -> String {
    format!("AT+EPRATL={},{}", codes.len(), codes.join(","))
}

// ---------------------------------------------------------------- 电源 / 温度

/// 飞行 / 在线模式。
pub fn set_cfun(mode: u32) -> String {
    format!("AT+CFUN={}", mode)
}

/// 重启模组。
pub const REBOOT: &str = "AT+CFUN=1,1";

/// 温度读取（`=0` 一次返回全部 23 路传感器，单位 0.001 ℃）。
///
/// **只写形式**：`AT+GTSENRDTEMP?` 在本模组上返回
/// `+CME ERROR: phone failure`，必须用带参数的写形式。
pub const GTSENRDTEMP_ALL: &str = "AT+GTSENRDTEMP=0";

// ---------------------------------------------------------------- SIM 就绪判定

pub const CPIN_READ: &str = "AT+CPIN?";

/// 拨号前置体检用到的命令序列（顺序即检查顺序）。
pub const READINESS_CMDS: &[&str] = &[CPIN_READ, CFUN_READ, CEREG_READ];

/// 模组功能等级查询。
pub const CFUN_READ: &str = "AT+CFUN?";

/// 一次拨号完整的命令序列（供文档与排障对照，实际流程见 `modem::pdp::dial`）。
pub fn dial_sequence(cfg: &Config) -> Vec<String> {
    vec![
        set_apn(cfg.cid, &cfg.pdp_type, &cfg.apn),
        set_auth(cfg.cid, &cfg.auth, &cfg.username, &cfg.password),
        CGACT_READ.to_string(),
        contrdp(cfg.cid),
        pdp_addr(cfg.cid),
        activate(cfg.cid),
        pdp_addr(cfg.cid),
        gtdns(cfg.cid),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_code_matches_27007() {
        assert_eq!(auth_code("none"), 0);
        assert_eq!(auth_code("pap"), 1);
        assert_eq!(auth_code("chap"), 2);
        assert_eq!(auth_code("both"), 3);
        // 未知值保守按 none 处理，不下发半吊子鉴权
        assert_eq!(auth_code(""), 0);
        assert_eq!(auth_code("PAP"), 0); // UCI 里只有小写枚举，按字面匹配
    }

    #[test]
    fn pdp_commands_are_well_formed() {
        assert_eq!(
            set_apn(1, "IPV4V6", "cmiot5g"),
            r#"AT+CGDCONT=1,"IPV4V6","cmiot5g""#
        );
        assert_eq!(set_auth(1, "none", "u", "p"), "AT+CGAUTH=1,0");
        assert_eq!(set_auth(1, "pap", "u", "p"), r#"AT+CGAUTH=1,1,"u","p""#);
        assert_eq!(activate(1), "AT+CGACT=1,1");
        assert_eq!(deactivate(1), "AT+CGACT=0,1");
        assert_eq!(contrdp(3), "AT+CGCONTRDP=3");
        assert_eq!(pdp_addr(3), "AT+CGPADDR=3");
        assert_eq!(gtdns(3), "AT+GTDNS=3");
    }

    #[test]
    fn sms_center_normalizes_number_format() {
        assert_eq!(set_sms_center("+8613800100500"), r#"AT+CSCA="+8613800100500",145"#);
        assert_eq!(set_sms_center("8613800100500"), r#"AT+CSCA="+8613800100500",145"#);
        assert_eq!(set_sms_center("13800100500"), r#"AT+CSCA="13800100500",161"#);
    }

    #[test]
    fn epratl_uses_its_own_rat_encoding() {
        // 与 +GTACT 的 1/2/4/10/14/20 完全不同，这里是 2/4/128
        assert_eq!(set_eprartl(&["128", "4"]), "AT+EPRATL=2,128,4");
    }

    #[test]
    fn temperature_command_is_write_form() {
        // 读形式在本模组回 +CME ERROR: phone failure，必须是写形式
        assert_eq!(GTSENRDTEMP_ALL, "AT+GTSENRDTEMP=0");
    }
}
