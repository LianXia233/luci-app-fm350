'use strict';
'require view';
'require form';
'require uci';
'require ui';
'require fm350.api as api';

/*
 * luci-app-fm350 —— 蜂窝网络管理（邻区扫描 / 锁频段 / 锁小区 / 制式 / 态势诊断）
 *
 * 设计前提（均以实机 FM350-GL 实测为准，不做臆测）：
 *
 * 1. 后端 cell / lock 接口返回的是「AT 指令 + 原始响应文本」的二元组，
 *    不是结构化 JSON。因此本页自行解析 AT 响应。
 *
 *    字段语义用「背靠背采样」确定：同一时刻先取 CESQ、紧接着取小区信息，
 *    两套独立来源应当互相印证。实机结果：
 *      +CESQ: 99,99,255,255,255,255,65,70,77
 *        → 信噪比 15.0 dB、信号强度 -87 dBm、信号质量 -11.0 dB
 *      服务小区行 1,9,460,0,149002,C2840C002,504990,128,,,16,69,69,64
 *        → 倒数第 4 位 16（单位 dB，不是索引）   对上 15.0 dB
 *        → 倒数第 3 位 69 → 信号强度 -88 dBm     对上 -87 dBm
 *        → 倒数第 1 位 64 → 信号质量 -11.5 dB    对上 -11.0 dB
 *    三者误差均不超过 1 dB，属模组的整数量化噪声，映射成立。
 *
  * 2. 倒数第 2 位与倒数第 3 位在实机全部 8 行小区上恒等（69/69、126/126等），
 *    是同一个量的重复上报，非信噪比。这一点可由手册印证：§11.1.15 把 NR 的
 *    <rxlev> 直接定义成 SS-RSRP 阶梯，与 <ss_rsrp> 同表，故两者必然相等。
 *    信噪比取服务小区行的 <ss_sinr>/<rssnr_value> 位；LTE/UMTS 邻区该位
 *    分别是 <bandwidth> / <ecno>，语义完全不同，不取。**NR 邻区第 8 位是
 *    手册 §11.1.15 明列的 <ss_sinr>，按手册解析照取**（实机行为与服务小区
 *    行一致：dB 原值而非阶梯索引，见文件头设计前提 2 的判定）。
 *
 *    注意：手册把这一位定义为 0.5 dB 阶梯索引（-100 → ≤ -50 dB，100 → > 50 dB），
 *    但实机上报的是 dB 原值。用上面那组背靠背采样判定：索引 10 = 16，
 *    按 dB 原值读得 16.0 dB（与 CESQ 的 15.0 dB 相差 1 dB，和另两个字段的
 *    量化误差同级）；按手册阶梯读得 -15.5 dB（相差 30.5 dB）或 8.0 dB（相差 7 dB）。
 *    故此处采信实机、按 dB 原值处理，属**手册与固件的已知分歧**。
 *
 * 3. 索引 255 一律是「未测量」哨兵；99 为 rxlev/ber 的「未测量」。
 *    注意 126 / 127 **不是**哨兵：手册明确 126 = ≥ -31 dBm（ss_rsrp）、
 *    126 = [19.5,20) dB（ss_rsrq）、127 = ≥ 40 dB（ss_sinr），都是有效强信号档，
 *    误判成未测量会把最好的信号显示成空白。
 *
 * 4. 载波聚合 AT+GTCAINFO? 在本模组上只回 OK，无数据 —— 面板如实标注「不可用」。
 *    （手册 §11.1.16 其实完整定义了 PCC/SCC1/SCC2 的字段结构，一旦固件开始上报，
 *    后端 signal.rs 应按该结构解析，而不是原样透出行文本。）
 *
 * 5. 制式选择经由 AT+GTACT 的 <rat> 控制。**制式优先级模组是支持的**：
 *    一是 AT+GTACT 的 <PreferredAct1>/<PreferredAct2> 两个位置参数
 *    （手册 §11.1.14 的 RAT Combination Table，合法值仅 2/3/6）；
 *    二是 AT+EPRATL=<RATnum>,<rat…>（手册 §11.1.13，编码 2/4/128），
 *    后端 modem::lock::set_rat_order 已实现，`fm350d rat` 可读写。
 */

/* ================================================================
 * 一、基础工具与算法
 * ================================================================ */

function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

/* 数值格式化：无效值统一显示为占位，避免输出 NaN / Infinity */
function fmtNum(v, digits, unit) {
	if (v == null || !isFinite(v)) return '—';
	var s = Number(v).toFixed(digits == null ? 0 : digits);
	return unit ? (s + ' ' + unit) : s;
}

/* 3GPP 阶梯索引 → 物理量。
 *
 * 手册的阶梯定义统一为：索引 0 表示「低于 base」，索引 n≥1 对应
 * [base+(n-1)*step, base+n*step)，取**区间下界**（保守估计，与后端
 * signal.rs 的 idx_lower 一致）。索引 0 因此是合法读数，不是「未测量」；
 * 真正的哨兵只有 255（部分字段额外有 99），由调用方先行滤除。 */
function idxToLinear(idx, base, step, maxIdx) {
	if (idx == null) return null;
	var n = parseInt(idx, 10);
	if (!isFinite(n) || n < 0) return null;
	if (n === 255) return null;
	if (maxIdx != null && n > maxIdx) return null;
	return n <= 0 ? base - step : base + (n - 1) * step;
}

/* 信号强度：按制式选表（手册 §11.1.2 / §11.1.15）
 *   LTE ：<rsrp>     索引 1 → -140 dBm，步长 1 dB，97 为最高有效档
 *   NR  ：<ss_rsrp>  索引 1 → -156 dBm，步长 1 dB，126 为最高有效档
 *   WCDMA：<rscp>    索引 1 → -120 dBm，步长 1 dB，96 为最高有效档 */
function rsrpFromIdx(idx, rat) {
	if (rat === 4) return idxToLinear(idx, -140, 1, 97);
	if (rat === 2) return idxToLinear(idx, -120, 1, 96);
	return idxToLinear(idx, -156, 1, 126);
}

/* 信号质量：按制式选表
 *   LTE ：<rsrq>      索引 1 → -19.5 dB，步长 0.5 dB，34 为最高有效档
 *   NR  ：<ss_rsrq>   索引 1 → -43 dB，  步长 0.5 dB，126 为最高有效档
 *   WCDMA：<Ec/Io_lev> 索引 1 → -24 dB， 步长 0.5 dB，49 为最高有效档 */
function rsrqFromIdx(idx, rat) {
	if (rat === 4) return idxToLinear(idx, -19.5, 0.5, 34);
	if (rat === 2) return idxToLinear(idx, -24, 0.5, 49);
	return idxToLinear(idx, -43, 0.5, 126);
}

/* 信噪比：见文件头设计前提 2。
 *
 * 实机在服务小区行上报的是 **dB 原值**，而不是手册定义的 0.5 dB 阶梯索引，
 * 故此处不做阶梯换算。255 为手册明写的无效值；超出物理可信区间（SINR
 * 不可能低于 -100 dB 或高于 100 dB）一律当作未测量，不拿垃圾值入图。 */
function sinrFromRaw(v) {
	if (v == null || v === '') return null;
	var n = parseInt(v, 10);
	if (!isFinite(n) || n === 255) return null;
	if (n < -100 || n > 100) return null;
	return n;
}

/* ================================================================
 * 二、制式与频段映射
 * ================================================================ */

/* <rat> 取值以手册 §11.1.15 的 Defined Values 为权威：
 *   0 = Invalid network（**不是 2G GSM**）、2 = WCDMA、4 = LTE、9 = NR-RAN。
 * 手册未定义 3 / 5，FM350-GL 也不支持 TD-SCDMA / CDMA，故不再自行编造
 * 制式名。与后端 signal.rs::cell_rat_text 保持一致。 */
var RAT_NAME = {
	0: '无网络', 2: '3G WCDMA', 4: '4G LTE', 9: '5G NR'
};

/* AT+GTACT 的 <rat>，手册 §11.1.14 Defined Values 共 8 个合法值。
 *
 * **10 才是 Automatic，20 是「NR/WCDMA/LTE 三模全开」**。两者极易混淆，
 * 因为手册 Note 6 明写：下发 10 之后查询会回显 20 —— 所以读到 20 时
 * 无法区分用户当初设的是 10 还是 20，界面必须把这个不确定性说出来，
 * 不能把 20 标成「自动」。 */
var GTACT_MODE = {
	1:  { label: '仅 3G（UMTS）', desc: '仅驻留 UMTS，无 3G 覆盖时脱网' },
	2:  { label: '仅 4G（LTE）', desc: '仅驻留 4G 网络，5G 不可用' },
	4:  { label: '4G + 3G', desc: 'LTE/UMTS 双模，5G 不可用' },
	10: { label: '自动（Automatic）', desc: '手册定义的自动模式；注意设置后查询会回显为 20（手册 Note 6）' },
	14: { label: '仅 5G（NR）', desc: '仅驻留 5G 网络，无 5G 覆盖时脱网' },
	16: { label: '5G + 3G', desc: 'NR-RAN/WCDMA，不含 LTE' },
	17: { label: '5G + 4G', desc: 'NR-RAN/LTE，不含 WCDMA（国内常用组合）' },
	20: { label: '5G + 4G + 3G（三模全开）', desc: '并非「自动」，而是三种制式同时允许。设置 10 后查询也会回显 20（手册 Note 6），故读到 20 时无法反推原设置' }
};

/* <PreferredAct1> / <PreferredAct2>，手册 §11.1.14：合法值仅 2 / 3 / 6。 */
var PREF_ACT_NAME = { 2: 'WCDMA 优先', 3: 'LTE 优先', 6: 'NR 优先' };

/* 频段编码 -> 频段名。
 *
 * 权威定义在手册 §11.1.14（+GTACT）与 §11.1.16（+GTCAINFO），两处一致：
 *   LTE：`100 + n`，101 = BAND_LTE_1 … **171 = BAND_LTE_71**（上限就是 171）；
 *   NR ：字符串 `50` 直接拼接 band 号，501 = BAND_NR_1 … 509 = BAND_NR_9、
 *        5010 = BAND_NR_10 … **50512 = BAND_NR_512**。
 *
 * 注意 NR 是**字符串拼接而不是数值加法**：`5010` 是 "50"+"10" = n10，
 * 而数值加法会算成 5000+10 或 500+10 而漏掉 n100 以上的频段（如 50512）。
 * 解不出来时返回「编号 n」，**绝不把一个无法确认的编码伪装成频段名**。
 *
 * rat 省略时先试 NR 再试 LTE（与改动前行为一致）。
 */
function nrBandNo(n) {
	/* NR band 号是否可解。
	 *
	 * FR1（1..105）取「3GPP TS 38.101-1 已分配集合」并上「手册明列的 9 / 10」：
	 * 手册 §11.1.14 / §11.1.16 直接写出了 `509 BAND_NR_9` 与 `5010 BAND_NR_10`，
	 * 属于本文档的权威定义，不能因为 3GPP 未分配就拒解。
	 * 旧实现用的是一份不完整的「未分配」黑名单，会把 `504` 解成
	 * 根本不存在的 n4、`5017` 解成 n17（那是 LTE 频段）。
	 *
	 * FR2 与扩展区间沿用实机 `AT+GTACT=?` 回显佐证的范围，不自行放宽。
	 * 权威依据始终是模组自己回的候选列表，本表只是离线兜底。
	 * 与后端 band.rs::nr_band_no_is_valid 保持完全一致。 */
	var FR1 = [1,2,3,5,7,8,9,10,12,13,14,18,20,25,26,28,29,30,34,38,39,40,41,
	           46,48,65,66,67,68,69,70,71,73,74,75,76,77,78,79,80,81,82,83,84,
	           85,86,89,90,91,92,93,94,95,96,97,98,99,100,101,102,103,104,105];
	if (n >= 1 && n <= 105) return FR1.indexOf(n) >= 0;
	return (n >= 256 && n <= 269) || (n >= 510 && n <= 512) || (n >= 670 && n <= 710);
}

function nrBandName(code) {
	var c = ('' + code).trim();
	if (!/^50\d+$/.test(c)) return null;          // 手册：NR 编码必为 "50"+n
	var n = parseInt(c.slice(2), 10);
	if (!isFinite(n) || !nrBandNo(n)) return null;
	return 'n' + n;
}

function lteBandName(n) {
	/* 手册定义的 100+n，上限 171（BAND_LTE_71）；越界不猜 */
	if (n >= 101 && n <= 171) return 'B' + (n - 100);
	/* 实机另观测到 band 号直写形态（3 = B3、41 = B41），保留为兜底。
	 * B16 在 3GPP 未分配，一并排除。 */
	if (n >= 1 && n <= 88 && n !== 16) return 'B' + n;
	return null;
}

function bandName(code, rat) {
	var c = ('' + code).trim();
	var n = parseInt(c, 10);
	if (!isFinite(n)) return '编号 ' + c;
	if (rat === 4) return lteBandName(n) || ('编号 ' + n);
	if (rat === 9) return nrBandName(c) || ('编号 ' + n);
	return nrBandName(c) || lteBandName(n) || ('编号 ' + n);
}

/* 频段分组：判定依据与 bandName 一致（手册 §11.1.14 / §11.1.16）。
 * 旧实现把 101..199 归为 NR，但手册明写 101..171 是 **LTE** 编码
 * （101 = BAND_LTE_1 … 171 = BAND_LTE_71），会把 B41 错分到 5G 矩阵里。 */
function bandGroup(code) {
	var c = ('' + code).trim();
	if (nrBandName(c)) return 'nr';
	var n = parseInt(c, 10);
	if (!isFinite(n)) return 'other';
	return lteBandName(n) ? 'lte' : 'other';
}

/* AT+GTACT? 回显频段编码的分类（手册 §11.1.14）。
 *
 * 注意 GTACT 回显的编码体系**与 GTCCINFO 的 <band> 不同**：GTACT 里
 * 0 = 自动选频、1..10 = <umts_band>、101..171 = <lte_band>、"50"+n = <nr_band>
 * （手册没有 GTCCINFO 那套 band 号直写形态）。
 *
 * 实机回显形如 `20,6,3,1,2,4,…,101,102,…`（CHANGELOG 1.0.0-r17）：其中
 * `1,2,4…` 是 **UMTS 频段**（1 = BAND_UMTS_I …）。若沿用 bandGroup 的
 * LTE 直写兜底，它们会被渲染成「4G B1/B2/B4」芯片——用户勾选后再下发，
 * 名义上锁的是 4G 频段，模组实际收到的却是 3G 编码，正是 BAND_PRESETS
 * 修正说明里点名要防的那类错误。 */
function gtactBandKind(code) {
	var c = ('' + code).trim();
	if (!/^[0-9]+$/.test(c)) return 'other';
	var n = parseInt(c, 10);
	if (n === 0) return 'auto';
	if (n >= 1 && n <= 10) return 'umts';
	if (nrBandName(c)) return 'nr';
	if (n >= 101 && n <= 171) return 'lte';
	return 'other';
}

/* GTACT 回显编码 → 展示名（UMTS 走罗马数字表，0 显式标注）。 */
function gtactBandLabel(code) {
	var kind = gtactBandKind(code);
	if (kind === 'umts') {
		var ub = parseInt(code, 10);
		return UMTS_BAND_NAME[ub] || ('编号 ' + code);
	}
	if (kind === 'auto') return '自动选频';
	return bandName(code) || ('编号 ' + code);
}

/* 一键预设。
 *
 * **参数位序是手册硬性规定的位置参数**，不能省（§11.1.14 Syntax）：
 *
 *     AT+GTACT=<rat>,<PreferredAct1>,<PreferredAct2>,<band_1>,<band_2>,…
 *
 * 且 Note 3 明写：三模（rat=20）下 PreferredAct1/2 只有手册
 * 「RAT Combination Table」里那十种组合合法，**其余一律 rejected**。
 * 旧版本的预设写成 `20,5041,5078,5079`，模组会把 5041 当成
 * PreferredAct1、把 5078 当成 PreferredAct2，只把 5079 当频段 ——
 * 按 Note 3 应当直接被拒；即使固件容错，锁到的也只是单个 n79。
 * 不需要优先制式时必须用**空位占位**（`20,,,<band…>`）把前两位让出来。
 *
 * LTE 频段号同样必须用手册编码：B1/B3/B5/B8 = **101/103/105/108**。
 * 旧版本直写 `1,3,5,8`，而 1..10 在手册里是 <umts_band>
 * （1 = BAND_UMTS_I），rat=20 含 WCDMA，极易被当成锁 3G 频段。
 */
var BAND_PRESETS = [
	{ id: 'auto', label: '恢复默认（全频段）', hint: '三模全开、不限频段（手册 RAT 组合表合法行：20 + 两个优先位未填）', args: '20' },
	{ id: 'nr_main', label: '5G 主力频段', hint: 'n41 / n78 / n79，国内 5G 核心主频；NR 优先于 LTE', args: '20,6,3,5041,5078,5079' },
	{ id: 'nr_all', label: '5G 全频段', hint: '锁定模组支持的全部 5G 频段；NR 优先于 LTE', args: '20,6,3,5020,5025,5028,5030,5038,5040,5041,5048,5066,5071,5077,5078,5079' },
	{ id: 'lte_main', label: '4G 主力频段', hint: 'B1 / B3 / B5 / B8；LTE 优先于 NR。注意手册 Note 5：锁 LTE 频段不影响 NR 频段配置，如需彻底禁用 5G 请改用「仅 4G」制式', args: '20,3,6,101,103,105,108' },
	{ id: 'mixed', label: '5G + 4G 黄金组合', hint: 'B1/B3/B5/B8 + n41/n78/n79，兼顾速率与回落覆盖；NR 优先于 LTE', args: '20,6,3,101,103,105,108,5041,5078,5079' }
];

function arfcnToBandLabel(arfcn, rat) {
	var n = parseInt(arfcn, 10);
	if (!isFinite(n) || n <= 0) return null;
	if (rat === 4) {
		if (n >= 0 && n <= 599) return 'B1';
		if (n >= 1200 && n <= 1949) return 'B3';
		if (n >= 2400 && n <= 2649) return 'B5';
		if (n >= 3450 && n <= 3799) return 'B8';
		if (n >= 36200 && n <= 36349) return 'B34';
		if (n >= 37750 && n <= 38249) return 'B38';
		if (n >= 38250 && n <= 38649) return 'B39';
		if (n >= 38650 && n <= 39649) return 'B40';
		if (n >= 39650 && n <= 41589) return 'B41';
		return null;
	}
	/* NR-ARFCN 是**两段式全局频率栅格**（3GPP TS 38.104 §5.4.2.1）：
	 *   0 … 599999        → F = N × 5 kHz            （0 – 3000 MHz）
	 *   600000 … 2016666  → F = 3000 + (N-600000) × 15 kHz （3000 – 24250 MHz）
	 * 旧实现全程只乘 0.005，于是 3 GHz 以上的频段全部推不出来：
	 * n78 的 ARFCN 633333 会被算成 3166.7 MHz（落在 n77/n78 区间外）而返回 null，
	 * 国内 5G 主力频之一因此永远显示不出频段名。与后端 band.rs 保持一致。 */
	var freq;
	if (n <= 599999) freq = n * 0.005;
	else if (n <= 2016666) freq = 3000 + (n - 600000) * 0.015;
	else return null;

	/* n38（2570-2620）整体落在 n41（2496-2690）内，n78（3300-3800）落在
	 * n77（3300-4200）内，n20（791-821）与 n28（758-803）在 791-803 重叠 ——
	 * 仅凭 ARFCN 无法区分，显式标注重叠而不二选一。 */
	if (freq >= 2110 && freq <= 2170) return 'n1';
	if (freq >= 1805 && freq <= 1880) return 'n3';
	if (freq >= 869 && freq <= 894) return 'n5';
	if (freq >= 925 && freq <= 960) return 'n8';
	if (freq >= 791 && freq <= 803) return 'n20/n28';
	if (freq >= 791 && freq <= 821) return 'n20';
	if (freq >= 758 && freq <= 803) return 'n28';
	if (freq >= 1880 && freq <= 1920) return 'n39';
	if (freq >= 2010 && freq <= 2025) return 'n34';
	if (freq >= 2300 && freq <= 2400) return 'n40';
	if (freq >= 2570 && freq <= 2620) return 'n38/n41';
	if (freq >= 2496 && freq <= 2690) return 'n41';
	if (freq >= 3300 && freq <= 4200) return 'n77/n78';
	if (freq >= 4400 && freq <= 5000) return 'n79';
	return null;
}

/* ================================================================
 * 三、AT 响应解析
 * ================================================================ */

/* AT+GTCCINFO? 的字段布局。
 *
 * 手册 §11.1.15 按「制式 × 服务/邻区」定义了 **6 种互不相同的布局**，
 * 字段数和同一位置的语义都不同，因此必须按布局取索引，
 * 不能用「倒数第 N 位」猜（旧实现就是这么做的，后果见下）：
 *
 *   UMTS 服务(15)：0 IsServiceCell,1 rat,2 mcc,3 mnc,4 lac,5 cellid,6 uarfcn,
 *                  7 psc,8 band,9 ecno,10 rscp,11 rac,12 rxlev,13 reserved,14 Ec/Io_lev
 *   UMTS 邻区(15)：… 7 psc,8 cell_type,9 rank_pos,10 ranking_status,11 ecno,
 *                  12 pathloss,13 rxlev,14 rscp
 *   LTE  服务(14)：… 8 band,9 bandwidth,10 rssnr_value,11 rxlev,12 rsrp,13 rsrq
 *   LTE  邻区(12)：… 7 physicalcellId,8 bandwidth,9 rxlev,10 rsrp,11 rsrq
 *   NR   服务(14)：… 8 band,9 bandwidth,10 ss_sinr,11 rxlev,12 ss_rsrp,13 ss_rsrq
 *   NR   邻区(12)：… 7 physicalcellId,8 ss_sinr,9 rxlev,10 ss_rsrp,11 ss_rsrq
 *
 * 旧实现用 len-4 / len-3 / len-1 定位信噪比与信号，并无条件把 f[8] 当频段，
 * 对照上表可得三类误读：
 *   • LTE 邻区的 f[8] 是 **<bandwidth>**，f[len-4] 也是它 —— 带宽码 25
 *     会被当成「频段 B25」与「信噪比 25 dB」同时显示；
 *   • NR 邻区的 f[8] 是 **<ss_sinr>**，会被渲染成「B77」之类的假频段；
 *   • UMTS 服务小区的 f[len-4]=f[11] 是 **<rac>**（路由区码 0-255），
 *     会被当成信噪比；f[8] 是 <band>，但那是 <umts_band>（1..10）编码，
 *     按 LTE/NR 表解会得到错误的频段名。
 *    因此 LTE / UMTS 邻区一律不取信噪比；NR 邻区第 8 位是手册明列的
 *    <ss_sinr>，与布局表一致，照取。
 */
function gtccinfoLayout(rat, isServing) {
	if (rat === 2) {
		return isServing
			? { len: 15, band: 8, bw: null, sinr: null, rsrp: 10, rsrq: 14, bandEnc: 'umts' }
			: { len: 15, band: null, bw: null, sinr: null, rsrp: 14, rsrq: null, bandEnc: 'umts' };
	}
	if (rat === 4) {
		return isServing
			? { len: 14, band: 8, bw: 9, sinr: 10, rsrp: 12, rsrq: 13, bandEnc: 'lte' }
			: { len: 12, band: null, bw: 8, sinr: null, rsrp: 10, rsrq: 11, bandEnc: 'lte' };
	}
	if (rat === 9) {
		return isServing
			? { len: 14, band: 8, bw: 9, sinr: 10, rsrp: 12, rsrq: 13, bandEnc: 'nr' }
			/* NR 邻区手册明列 12 字段：8 = <ss_sinr>、9 = <rxlev>、10 = <ss_rsrp>、
			 * 11 = <ss_rsrq>。该位**确实带信噪比**（LTE 邻区同一位置是
			 * <bandwidth>、UMTS 邻区是 <ecno>，语义完全不同），必须按手册解析。 */
			: { len: 12, band: null, bw: null, sinr: 8, rsrp: 10, rsrq: 11, bandEnc: 'nr' };
	}
	return null;
}

/* <umts_band>：手册 §11.1.14 定义为 1 = BAND_UMTS_I … 10 = BAND_UMTS_X，
 * 与 LTE/NR 的编码体系完全不同，必须单独走这张表。 */
var UMTS_BAND_NAME = {
	1: 'B1 (I)', 2: 'B2 (II)', 3: 'B3 (III)', 4: 'B4 (IV)', 5: 'B5 (V)',
	6: 'B6 (VI)', 7: 'B7 (VII)', 8: 'B8 (VIII)', 9: 'B9 (IX)', 10: 'B10 (X)'
};

function parseGtccinfo(text) {
	var out = { serving: null, neighbors: [], rawOk: false };
	if (typeof text !== 'string') return out;
	var lines = text.split('\n');
	for (var i = 0; i < lines.length; i++) {
		var line = lines[i].trim();
		/* **必须先剥掉 `+GTCCINFO:` 前缀**。
		 *
		 * `AT+GTCCINFO?` 的每一行数据恰恰都以这个前缀开头，旧实现却把
		 * 「以 +GTCCINFO 开头」当成噪声行直接 continue —— 于是所有小区行全被
		 * 跳过，serving / neighbors 永远为空，整张邻区扫描表长期无数据。
		 * 后端 signal.rs::service_cell 早就修过同一处死代码并在文档里记了案，
		 * 前端这一份漏掉了。此处与后端保持一致：先剥前缀，再按首字段筛服务小区，
		 * 同时兼容「前缀已被上层剥掉」的输入形态。 */
		var body = line.indexOf('+GTCCINFO:') === 0
			? line.slice('+GTCCINFO:'.length).trim()
			: line;
		if (!body || !/^[0-9]/.test(body)) continue;
		var f = body.split(',');
		if (f.length < 8) continue;
		out.rawOk = true;

		var isServing = f[0].trim() === '1';
		var rat = parseInt(f[1], 10);
		var L = gtccinfoLayout(rat, isServing);

		/* 按布局取字段；未定义的位置为 null，不拿相邻字段凑数 */
		function pick(idx) {
			if (idx == null || idx >= f.length) return '';
			return (f[idx] || '').trim();
		}
		function pickIdx(idx) {
			var v = parseInt(pick(idx), 10);
			return isFinite(v) ? v : null;
		}

		var rawBand = L ? pick(L.band) : '';
		var cell = {
			serving: isServing,
			rat: rat,
			ratName: RAT_NAME[rat] || ('制式 ' + rat),
			mcc: f[2] || '',
			mnc: f[3] || '',
			tac: f[4] || '',
			cellId: f[5] || '',
			arfcn: f[6] || '',
			pci: f[7] || '',
			bandwidth: L ? pickIdx(L.bw) : null,
			rsrpIdx: L ? pickIdx(L.rsrp) : null,
			rsrqIdx: L ? pickIdx(L.rsrq) : null,
			/* 字段数与手册布局不符时标记出来，不把可疑数据当真 */
			layoutOk: !!(L && f.length >= L.len)
		};
		cell.rsrp = rsrpFromIdx(cell.rsrpIdx, rat);
		cell.rsrq = rsrqFromIdx(cell.rsrqIdx, rat);
		cell.sinr = L ? sinrFromRaw(pick(L.sinr)) : null;

		if (!rawBand) {
			var guess = arfcnToBandLabel(cell.arfcn, rat);
			cell.band = guess || '';
			cell.bandGuessed = !!guess;
		} else if (L && L.bandEnc === 'umts') {
			var ub = parseInt(rawBand, 10);
			cell.band = UMTS_BAND_NAME[ub] || ('编号 ' + rawBand);
			cell.bandGuessed = false;
		} else {
			/* 模组上报的是编码：必须解码后再展示，否则同表内会出现
			 * 「5041」（上报，未解码）与「n41 (推算)」两种写法并存。 */
			cell.band = bandName(rawBand, rat);
			cell.bandGuessed = false;
		}
		if (isServing) out.serving = cell;
		else out.neighbors.push(cell);
	}
	return out;
}

/* AT+GTACT? 回显解析。
 *
 * 手册 §11.1.14 的读写响应同构，都是位置参数：
 *     +GTACT: <rat>,<PreferredAct1>,<PreferredAct2>,<band_1>,<band_2>,…
 * 所以第 2、3 位是**优先制式**（合法值仅 2/3/6，空 = 未指定），
 * 从第 4 位起才是频段。
 *
 * 旧实现从 index 1 开始全部当成频段，于是 `+GTACT: 20,6,3,5078`
 * 会解出 bands = [6, 3, 5078]，进而把 PreferredAct 渲染成两个
 * 幽灵频段芯片（B6 / B3）列在「4G 频段矩阵」里；用户一旦勾选它们，
 * 下发的是 `20,6,3,6,3,…` —— 6/3 在手册里是 <umts_band>，
 * 结果是去锁了两个 3G 频段。 */
function parseGtact(text) {
	var out = { mode: null, modeLabel: '未知', modeDesc: '', pref: [], prefNames: [], bands: [], raw: '' };
	if (typeof text !== 'string') return out;
	var m = text.match(/\+GTACT:\s*([^\r\n]*)/);
	if (!m) return out;
	var body = m[1].trim();
	out.raw = body;
	var parts = body.split(',');
	var mode = parseInt(parts[0], 10);
	if (isFinite(mode)) {
		out.mode = mode;
		var info = GTACT_MODE[mode];
		if (info) { out.modeLabel = info.label; out.modeDesc = info.desc; }
		else { out.modeLabel = '自定义模式（' + mode + '）'; }
	}
	for (var i = 1; i <= 2 && i < parts.length; i++) {
		var pv = parts[i].trim();
		if (!/^\d+$/.test(pv)) continue;      // 空位 = 未指定，跳过
		var pn = parseInt(pv, 10);
		out.pref.push(pn);
		out.prefNames.push(PREF_ACT_NAME[pn] || ('非法优先值 ' + pn));
	}
	for (var j = 3; j < parts.length; j++) {
		var v = parts[j].trim();
		if (v === '' || !/^\d+$/.test(v)) continue;
		out.bands.push(parseInt(v, 10));
	}
	return out;
}

function parseEmmchlock(text) {
	var out = { locked: false, params: [], raw: '' };
	if (typeof text !== 'string') return out;
	var m = text.match(/\+EMMCHLCK:\s*([^\r\n]*)/);
	if (!m) return out;
	var body = m[1].trim();
	out.raw = body;
	var parts = body.split(',');
	if (parts.length && parts[0].trim() === '0') { out.locked = false; return out; }
	out.locked = true;
	out.params = parts;
	return out;
}

function pickResp(pairs, cmd) {
	if (!Array.isArray(pairs)) return '';
	for (var i = 0; i < pairs.length; i++) {
		if (pairs[i] && pairs[i][0] && pairs[i][0].indexOf(cmd) === 0)
			return pairs[i][1] || '';
	}
	return '';
}

/* ================================================================
 * 四、高品质白色毛玻璃样式 (White Glassmorphism UI)
 * ================================================================ */

/* 移动端视口保证：部分固件 LuCI 基座未注入 viewport meta，手机浏览器会按
   980px 布局宽度渲染，导致响应式媒体查询不生效。这里幂等补注，不覆盖已有值。 */
function ensureViewport() {
	if (document.querySelector('meta[name="viewport"]'))
		return;
	var meta = document.createElement('meta');
	meta.setAttribute('name', 'viewport');
	meta.setAttribute('content', 'width=device-width, initial-scale=1.0');
	document.head.appendChild(meta);
}

function injectTheme() {
	ensureViewport();
	var styleId = 'fm350-network-v2-glass';
	if (document.getElementById(styleId)) return;
	var css = [
		':root {',
		'  --fm-glass-bg: rgba(255, 255, 255, 0.72);',
		'  --fm-glass-bg-hover: rgba(255, 255, 255, 0.85);',
		'  --fm-glass-border: rgba(255, 255, 255, 0.85);',
		'  --fm-glass-border-subtle: rgba(0, 0, 0, 0.06);',
		'  --fm-glass-shadow: 0 10px 30px -5px rgba(0, 20, 60, 0.06), 0 2px 8px rgba(0, 0, 0, 0.03);',
		'  --fm-glass-blur: blur(18px) saturate(180%);',
		'  --fm-primary: #0284c7;',
		'  --fm-primary-dark: #0369a1;',
		'  --fm-primary-light: rgba(2, 132, 199, 0.1);',
		'  --fm-accent-nr: #7c3aed;',
		'  --fm-text-main: #1e293b;',
		'  --fm-text-muted: #64748b;',
		'}',
		'@media (prefers-color-scheme: dark) {',
		'  :root {',
		'    --fm-glass-bg: rgba(30, 41, 59, 0.75);',
		'    --fm-glass-bg-hover: rgba(40, 53, 75, 0.85);',
		'    --fm-glass-border: rgba(255, 255, 255, 0.12);',
		'    --fm-glass-border-subtle: rgba(255, 255, 255, 0.08);',
		'    --fm-glass-shadow: 0 12px 35px -5px rgba(0, 0, 0, 0.35);',
		'    --fm-text-main: #f8fafc;',
		'    --fm-text-muted: #94a3b8;',
		'  }',
		'}',
		'.fm350-net2-wrap { max-width: 1240px; margin: 0 auto; padding: 4px; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif; color: var(--fm-text-main); }',
		
		/* 顶部标题与态势大卡 */
		'.fm350-glass-card {',
		'  background: var(--fm-glass-bg);',
		'  backdrop-filter: var(--fm-glass-blur);',
		'  -webkit-backdrop-filter: var(--fm-glass-blur);',
		'  border: 1px solid var(--fm-glass-border);',
		'  box-shadow: var(--fm-glass-shadow);',
		'  border-radius: 20px;',
		'  padding: 22px 24px;',
		'  margin-bottom: 20px;',
		'  transition: transform 0.25s cubic-bezier(0.16, 1, 0.3, 1), box-shadow 0.25s;',
		'  position: relative;',
		'  overflow: hidden;',
		'}',
		'.fm350-glass-card::before {',
		'  content: ""; position: absolute; top: 0; left: 0; right: 0; height: 1.5px;',
		'  background: linear-gradient(90deg, transparent, rgba(255,255,255,0.8), transparent);',
		'  pointer-events: none;',
		'}',
		
		/* 标题与副标 */
		'.fm350-card-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; }',
		'.fm350-net2-title { margin: 0; font-size: 16px; font-weight: 700; display: flex; align-items: center; gap: 10px; color: var(--fm-text-main); letter-spacing: -0.2px; }',
		'.fm350-net2-sub { margin: -6px 0 16px; font-size: 12.5px; color: var(--fm-text-muted); line-height: 1.6; }',
		
		/* KPI 仪表盘网格 */
		'.fm350-kpi-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 12px; margin-bottom: 16px; }',
		'.fm350-kpi-box {',
		'  background: rgba(255, 255, 255, 0.45);',
		'  border: 1px solid var(--fm-glass-border-subtle);',
		'  border-radius: 14px;',
		'  padding: 12px 14px;',
		'  display: flex; flex-direction: column; justify-content: space-between;',
		'  box-shadow: inset 0 1px 2px rgba(255,255,255,0.6);',
		'  transition: background 0.2s, transform 0.2s;',
		'}',
		'@media (prefers-color-scheme: dark) {',
		'  .fm350-kpi-box { background: rgba(255, 255, 255, 0.04); box-shadow: none; }',
		'}',
		'.fm350-kpi-box:hover { background: var(--fm-glass-bg-hover); transform: translateY(-2px); }',
		'.fm350-kpi-label { font-size: 11.5px; font-weight: 500; color: var(--fm-text-muted); margin-bottom: 6px; display: flex; align-items: center; justify-content: space-between; }',
		'.fm350-kpi-val { font-size: 20px; font-weight: 700; color: var(--fm-text-main); display: flex; align-items: baseline; gap: 4px; line-height: 1.2; word-break: break-all; }',
		'.fm350-kpi-val .u { font-size: 11px; font-weight: 500; color: var(--fm-text-muted); }',
		
		/* 状态徽章与呼吸灯 */
		'.fm350-badge { display: inline-flex; align-items: center; gap: 5px; padding: 3px 9px; border-radius: 30px; font-size: 11.5px; font-weight: 600; }',
		'.fm350-badge.ok { background: rgba(16, 185, 129, 0.12); color: #059669; }',
		'.fm350-badge.warn { background: rgba(245, 158, 11, 0.12); color: #d97706; }',
		'.fm350-badge.danger { background: rgba(239, 68, 68, 0.12); color: #dc2626; }',
		'.fm350-badge.lock { background: rgba(2, 132, 199, 0.12); color: #0284c7; }',
		'.fm350-badge.nr { background: rgba(124, 58, 237, 0.12); color: #7c3aed; }',
		'.fm350-badge.muted { background: rgba(100, 116, 139, 0.12); color: #64748b; }',
		'.fm350-dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; display: inline-block; }',
		'.fm350-dot.pulse { animation: fm-pulse 2s infinite cubic-bezier(0.4, 0, 0.6, 1); }',
		'@keyframes fm-pulse { 0%, 100% { opacity: 1; transform: scale(1); } 50% { opacity: 0.4; transform: scale(1.3); } }',

		/* 趋势图表区 */
		'.fm350-trend-container {',
		'  background: rgba(255, 255, 255, 0.35);',
		'  border: 1px solid var(--fm-glass-border-subtle);',
		'  border-radius: 14px; padding: 14px 16px;',
		'  margin-top: 14px;',
		'}',
		'@media (prefers-color-scheme: dark) {',
		'  .fm350-trend-container { background: rgba(0, 0, 0, 0.2); }',
		'}',
		'.fm350-net2-trend { width: 100%; height: 86px; display: block; overflow: visible; }',
		
		/* 诊断条目 */
		'.fm350-diag-grid { display: grid; gap: 8px; margin-top: 12px; }',
		'.fm350-net2-diag {',
		'  padding: 10px 14px; border-radius: 12px; font-size: 12.5px; line-height: 1.6;',
		'  background: rgba(255, 255, 255, 0.5); border: 1px solid var(--fm-glass-border-subtle);',
		'  border-left: 4px solid #64748b; color: var(--fm-text-main);',
		'}',
		'@media (prefers-color-scheme: dark) {',
		'  .fm350-net2-diag { background: rgba(255, 255, 255, 0.04); }',
		'}',
		'.fm350-net2-diag.ok { border-left-color: #10b981; background: rgba(16, 185, 129, 0.06); }',
		'.fm350-net2-diag.warn { border-left-color: #f59e0b; background: rgba(245, 158, 11, 0.06); }',
		'.fm350-net2-diag.danger { border-left-color: #ef4444; background: rgba(239, 68, 68, 0.06); }',

		/* 表格区域 */
		'.fm350-table-wrap { overflow-x: auto; margin-top: 12px; border-radius: 12px; border: 1px solid var(--fm-glass-border-subtle); }',
		'.fm350-net2-table { width: 100%; border-collapse: separate; border-spacing: 0; font-size: 13px; }',
		'.fm350-net2-table th {',
		'  background: rgba(255, 255, 255, 0.6);',
		'  color: var(--fm-text-muted); font-weight: 600; padding: 10px 12px; text-align: left;',
		'  border-bottom: 1px solid var(--fm-glass-border-subtle); cursor: pointer; user-select: none;',
		'  transition: color 0.15s;',
		'}',
		'@media (prefers-color-scheme: dark) { .fm350-net2-table th { background: rgba(255, 255, 255, 0.05); } }',
		'.fm350-net2-table th:hover { color: var(--fm-primary); }',
		'.fm350-net2-table td { padding: 9px 12px; border-bottom: 1px solid var(--fm-glass-border-subtle); white-space: nowrap; }',
		'.fm350-net2-table tr:last-child td { border-bottom: none; }',
		'.fm350-net2-table tr:hover td { background: rgba(255, 255, 255, 0.4); }',
		'.fm350-net2-table tr.serving { background: rgba(2, 132, 199, 0.08); font-weight: 600; }',
		'.fm350-net2-table tr.muted td { opacity: 0.55; }',

		/* 信号指示条 */
		'.fm350-meter { display: inline-flex; width: 44px; height: 7px; border-radius: 4px; background: rgba(0,0,0,0.08); overflow: hidden; vertical-align: middle; margin-right: 6px; }',
		'@media (prefers-color-scheme: dark) { .fm350-meter { background: rgba(255,255,255,0.1); } }',
		'.fm350-meter-fill { height: 100%; border-radius: 4px; transition: width 0.3s ease; }',

		/* 交互组件：Chips / 选项卡 */
		'.fm350-chips-flex { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; }',
		'.fm350-net2-chip {',
		'  border: 1px solid var(--fm-glass-border-subtle); border-radius: 10px;',
		'  padding: 6px 13px; font-size: 12.5px; font-weight: 500; cursor: pointer; user-select: none;',
		'  background: rgba(255, 255, 255, 0.6); color: var(--fm-text-main);',
		'  transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1); display: inline-flex; align-items: center; gap: 6px;',
		'}',
		'@media (prefers-color-scheme: dark) { .fm350-net2-chip { background: rgba(255,255,255,0.05); } }',
		'.fm350-net2-chip:hover { transform: translateY(-1.5px); background: var(--fm-glass-bg-hover); box-shadow: 0 3px 10px rgba(0,0,0,0.05); }',
		'.fm350-net2-chip.on {',
		'  background: linear-gradient(135deg, #0284c7 0%, #0369a1 100%);',
		'  color: #ffffff; border-color: #0284c7;',
		'  box-shadow: 0 4px 12px rgba(2, 132, 199, 0.35);',
		'}',
		'.fm350-net2-chip.preset {',
		'  background: rgba(2, 132, 199, 0.06); border-color: rgba(2, 132, 199, 0.25); color: var(--fm-primary);',
		'}',
		'.fm350-net2-chip.preset:hover { background: rgba(2, 132, 199, 0.15); }',

		/* 输入框与按钮美化 */
		'.fm350-ctrl-row { display: flex; flex-wrap: wrap; gap: 10px; align-items: center; margin: 12px 0; }',
		'.fm350-glass-input {',
		'  background: rgba(255, 255, 255, 0.7) !important;',
		'  border: 1px solid var(--fm-glass-border-subtle) !important;',
		'  border-radius: 9px !important; padding: 7px 11px !important;',
		'  font-size: 13px !important; color: var(--fm-text-main) !important;',
		'  outline: none; transition: border-color 0.2s, box-shadow 0.2s;',
		'}',
		'@media (prefers-color-scheme: dark) { .fm350-glass-input { background: rgba(255,255,255,0.08) !important; } }',
		'.fm350-glass-input:focus { border-color: var(--fm-primary) !important; box-shadow: 0 0 0 3px rgba(2, 132, 199, 0.2) !important; }',
		'.fm350-btn-glass {',
		'  background: linear-gradient(135deg, #0284c7 0%, #0369a1 100%) !important; color: #fff !important;',
		'  border: none !important; border-radius: 9px !important; padding: 7px 16px !important;',
		'  font-size: 13px !important; font-weight: 600 !important; cursor: pointer; display: inline-flex;',
		'  align-items: center; gap: 6px; box-shadow: 0 3px 10px rgba(2, 132, 199, 0.25); transition: all 0.2s !important;',
		'}',
		'.fm350-btn-glass:hover { transform: translateY(-1px); box-shadow: 0 5px 14px rgba(2, 132, 199, 0.35); }',
		'.fm350-btn-ghost {',
		'  background: rgba(255, 255, 255, 0.6) !important; color: var(--fm-text-main) !important;',
		'  border: 1px solid var(--fm-glass-border-subtle) !important; border-radius: 9px !important;',
		'  padding: 7px 14px !important; font-size: 13px !important; font-weight: 500 !important; cursor: pointer;',
		'  transition: all 0.2s !important;',
		'}',
		'.fm350-btn-ghost:hover { background: var(--fm-glass-bg-hover) !important; transform: translateY(-1px); }',

		/* 危险操作弹窗 */
		'.fm350-net2-mask { position: fixed; inset: 0; background: rgba(15, 23, 42, 0.45); backdrop-filter: blur(8px); z-index: 99999; display: flex; align-items: center; justify-content: center; }',
		'.fm350-net2-modal {',
		'  background: var(--fm-glass-bg); backdrop-filter: var(--fm-glass-blur); -webkit-backdrop-filter: var(--fm-glass-blur);',
		'  border: 1px solid var(--fm-glass-border); border-radius: 18px; padding: 22px 24px; max-width: 440px; width: 90%;',
		'  box-shadow: 0 20px 45px rgba(0, 0, 0, 0.2); animation: fm-scale 0.2s cubic-bezier(0.16, 1, 0.3, 1);',
		'}',
		'@keyframes fm-scale { from { opacity: 0; transform: scale(0.94); } to { opacity: 1; transform: scale(1); } }',
		'.fm350-net2-modal h4 { margin: 0 0 10px; font-size: 16px; font-weight: 700; color: var(--fm-text-main); }',
		'.fm350-net2-modal p { margin: 0 0 16px; font-size: 13px; color: var(--fm-text-muted); line-height: 1.6; }',

		/* 图标动效 */
		'.fm350-rot-scan { transform-origin: 12px 12px; animation: fm-spin 2.6s linear infinite; }',
		'@keyframes fm-spin { 100% { transform: rotate(360deg); } }',
		'.fm350-wave-anim { animation: fm-wave 2s ease-in-out infinite alternate; }',
		'@keyframes fm-wave { 0% { stroke-dashoffset: 0; } 100% { stroke-dashoffset: 24; } }',

		/* ---------------- 响应式布局适配 ----------------
		   仅在小屏（<=767px / <=480px）调整排列、间距与滚动，不改变任何颜色、
		   字体、边框、圆角、阴影等视觉元素；桌面端（>=768px）样式保持不变。
		   邻区表格列多且禁止折行，小屏固定最小宽度后由外层容器横向滚动。 */
		'@media (max-width: 767px) {',
		'  .fm350-net2-wrap { padding: 2px; }',
		'  .fm350-glass-card { padding: 16px 16px; }',
		'  .fm350-card-header { flex-wrap: wrap; gap: 8px; }',
		'  .fm350-net2-title { flex-wrap: wrap; }',
		'  .fm350-net2-sub { margin-bottom: 12px; }',
		'  .fm350-net2-table { min-width: 760px; }',
		'  .fm350-table-wrap { -webkit-overflow-scrolling: touch; }',
		'  .fm350-glass-input { max-width: 100%; }',
		'}',
		'@media (max-width: 480px) {',
		'  .fm350-glass-card { padding: 14px 12px; }',
		'  .fm350-net2-modal { padding: 16px 14px; }',
		'}'
	].join('');
	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

/* ================================================================
 * 五、动态矢量 SVG 图标引擎 (Dynamic Animated SVGs)
 * ================================================================ */

var DYNAMIC_ICONS = {
	/* 态势雷达：带旋转扫描波与呼吸内环 */
	radar: function(isScanning) {
		return parseSvg(
			'<svg class="fm350-svg-icon" viewBox="0 0 24 24" width="22" height="22" fill="none">' +
				'<circle cx="12" cy="12" r="10" stroke="#0284c7" stroke-opacity="0.25" stroke-width="1.5"/>' +
				'<circle cx="12" cy="12" r="6" stroke="#0284c7" stroke-opacity="0.45" stroke-width="1.2"/>' +
				'<circle cx="12" cy="12" r="2.2" fill="#0284c7"/>' +
				'<g class="' + (isScanning ? 'fm350-rot-scan' : '') + '">' +
					'<path d="M12 12 L21 7 A10 10 0 0 0 12 2 Z" fill="url(#radar-beam)" fill-opacity="0.45"/>' +
					'<line x1="12" y1="12" x2="21" y2="7" stroke="#0284c7" stroke-width="1.6" stroke-linecap="round"/>' +
				'</g>' +
				'<defs>' +
					'<linearGradient id="radar-beam" x1="12" y1="12" x2="21" y2="7">' +
						'<stop offset="0%" stop-color="#0284c7" stop-opacity="0.1"/>' +
						'<stop offset="100%" stop-color="#38bdf8" stop-opacity="0.8"/>' +
					'</linearGradient>' +
				'</defs>' +
			'</svg>'
		);
	},
	/* 频段载波：动态频域波浪 */
	band: function() {
		return parseSvg(
			'<svg class="fm350-svg-icon" viewBox="0 0 24 24" width="22" height="22" fill="none">' +
				'<path d="M2 12 C 5 4, 8 4, 11 12 C 14 20, 17 20, 22 12" stroke="#7c3aed" stroke-width="2" stroke-linecap="round" stroke-dasharray="32" class="fm350-wave-anim"/>' +
				'<line x1="2" y1="20" x2="22" y2="20" stroke="currentColor" stroke-opacity="0.2" stroke-width="1.5" stroke-linecap="round"/>' +
				'<circle cx="11" cy="12" r="2" fill="#7c3aed"/>' +
			'</svg>'
		);
	},
	/* 锁小区：对焦十字与光标瞄准 */
	target: function() {
		return parseSvg(
			'<svg class="fm350-svg-icon" viewBox="0 0 24 24" width="22" height="22" fill="none">' +
				'<circle cx="12" cy="12" r="8.5" stroke="#0284c7" stroke-opacity="0.3" stroke-width="1.5"/>' +
				'<circle cx="12" cy="12" r="4" stroke="#0284c7" stroke-width="1.8"/>' +
				'<circle cx="12" cy="12" r="1.5" fill="#0284c7"/>' +
				'<line x1="12" y1="1" x2="12" y2="5" stroke="#0284c7" stroke-width="1.8" stroke-linecap="round"/>' +
				'<line x1="12" y1="19" x2="12" y2="23" stroke="#0284c7" stroke-width="1.8" stroke-linecap="round"/>' +
				'<line x1="1" y1="12" x2="5" y2="12" stroke="#0284c7" stroke-width="1.8" stroke-linecap="round"/>' +
				'<line x1="19" y1="12" x2="23" y2="12" stroke="#0284c7" stroke-width="1.8" stroke-linecap="round"/>' +
			'</svg>'
		);
	},
	/* 网络制式齿轮 */
	gear: function() {
		return parseSvg(
			'<svg class="fm350-svg-icon" viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="#059669" stroke-width="1.8" stroke-linecap="round">' +
				'<circle cx="12" cy="12" r="3.2"/>' +
				'<path d="M12 2v2.5M12 19.5V22M2 12h2.5M19.5 12H22M4.9 4.9l1.8 1.8M17.3 17.3l1.8 1.8M19.1 4.9L17.3 6.7M6.7 17.3L4.9 19.1"/>' +
			'</svg>'
		);
	}
};

/* 信号强度分级评价 */
function gradeOf(kind, v) {
	if (v == null || !isFinite(v)) return { level: 0, text: '未测量', color: '#94a3b8', cls: 'muted' };
	if (kind === 'rsrp') {
		if (v >= -85) return { level: 4, text: '极佳', color: '#10b981', cls: 'ok' };
		if (v >= -95) return { level: 3, text: '良好', color: '#059669', cls: 'ok' };
		if (v >= -105) return { level: 2, text: '中等', color: '#f59e0b', cls: 'warn' };
		if (v >= -115) return { level: 1, text: '较弱', color: '#f97316', cls: 'warn' };
		return { level: 1, text: '极弱', color: '#ef4444', cls: 'danger' };
	}
	if (kind === 'rsrq') {
		if (v >= -10) return { level: 4, text: '极佳', color: '#10b981', cls: 'ok' };
		if (v >= -15) return { level: 3, text: '良好', color: '#059669', cls: 'ok' };
		if (v >= -20) return { level: 2, text: '中等', color: '#f59e0b', cls: 'warn' };
		return { level: 1, text: '较差', color: '#ef4444', cls: 'danger' };
	}
	if (v >= 20) return { level: 4, text: '极佳', color: '#10b981', cls: 'ok' };
	if (v >= 13) return { level: 3, text: '良好', color: '#059669', cls: 'ok' };
	if (v >= 0) return { level: 2, text: '一般', color: '#f59e0b', cls: 'warn' };
	return { level: 1, text: '较差', color: '#ef4444', cls: 'danger' };
}

function signalMeter(grade) {
	var pct = [0, 25, 50, 75, 100][grade.level] || 0;
	return E('span', { 'class': 'fm350-meter', 'title': grade.text }, [
		E('i', { 'class': 'fm350-meter-fill', 'style': 'width:' + pct + '%;background:' + grade.color })
	]);
}

/* ================================================================
 * 六、二次确认模态窗
 * ================================================================ */

function confirmDanger(title, body, okText) {
	return new Promise(function(resolve) {
		var done = function(v) {
			document.body.removeChild(mask);
			resolve(v);
		};
		var mask = E('div', { 'class': 'fm350-net2-mask' }, [
			E('div', { 'class': 'fm350-net2-modal' }, [
				E('h4', {}, title),
				E('p', {}, body),
				E('div', { 'style': 'display:flex;gap:10px;justify-content:flex-end' }, [
					E('button', { 'class': 'fm350-btn-ghost', 'click': function() { done(false); } }, '取消'),
					E('button', { 'class': 'fm350-btn-glass', 'click': function() { done(true); } }, okText || '立即确认')
				])
			])
		]);
		mask.addEventListener('click', function(ev) {
			if (ev.target === mask) done(false);
		});
		document.body.appendChild(mask);
	});
}

/* ================================================================
 * 七、视图主体 Controller
 * ================================================================ */

var ACTIVE = null;

return view.extend({
	load: function() {
		return Promise.all([
			api.lock(),
			api.cell(),
			api.net(),
			api.signal()
		]);
	},

	render: function(data) {
		api.injectCss();
		injectTheme();

		var lockPairs = (data[0] && data[0].ok && data[0].value) || [];
		var cellPairs = (data[1] && data[1].ok && data[1].value) || [];
		var net = (data[2] && data[2].ok && data[2].value) || {};
		var sig = (data[3] && data[3].ok && data[3].value) || null;

		var state = {
			gtact: parseGtact(pickResp(lockPairs, 'AT+GTACT?')),
			emm: parseEmmchlock(pickResp(lockPairs, 'AT+EMMCHLCK?')),
			cells: parseGtccinfo(pickResp(cellPairs, 'AT+GTCCINFO?')),
			ca: pickResp(cellPairs, 'AT+GTCAINFO?'),
			net: net,
			history: [],
			pciChanges: 0,
			lastPci: null,
			scanning: false,
			timer: null,
			intervalMs: 0,
			sortKey: 'rsrp',
			sortDesc: true,
			filterRat: '',
			filterMinRsrp: null,
			selectedBands: {},
			cellForm: null,
			host: null
		};

		applySignal(state, sig);
		if (state.cells.serving) {
			state.lastPci = state.cells.serving.pci;
			state.history.push({
				rsrp: state.cells.serving.rsrp,
				rsrq: state.cells.serving.rsrq,
				sinr: state.cells.serving.sinr
			});
		}

		var host = E('div', { 'class': 'fm350-net2-wrap' });
		state.host = host;
		ACTIVE = state;
		renderAll(host, state);
		return host;
	},

	remove: function() {
		if (ACTIVE && ACTIVE.timer) {
			clearTimeout(ACTIVE.timer);
			ACTIVE.timer = null;
			ACTIVE.intervalMs = 0;
		}
	}
});

function applySignal(state, sig) {
	if (!sig || !state.cells.serving) return;
	var s = state.cells.serving;
	if (sig.rsrp != null) s.rsrp = sig.rsrp;
	if (sig.rsrq != null) s.rsrq = sig.rsrq;
	if (sig.sinr != null) s.sinr = sig.sinr;
	if (sig.band) {
		s.band = sig.band;
		s.bandGuessed = (typeof sig.band_source === 'string' && sig.band_source.indexOf('推算') >= 0);
	}
	if (sig.rat) s.ratName = sig.rat;
}

function renderAll(host, state) {
	while (host.firstChild) host.removeChild(host.firstChild);
	host.appendChild(buildSituation(state, host));
	host.appendChild(buildNeighbors(state, host));
	host.appendChild(buildBandLock(state, host));
	host.appendChild(buildCellLock(state, host));
	host.appendChild(buildRat(state, host));
}

/* ----------------------------------------------------------------
 * 区块一：实时锁定态势与遥测诊断
 * ---------------------------------------------------------------- */
function buildSituation(state, host) {
	var s = state.cells.serving;

	var kpiBox = function(label, val, unit, tag) {
		return E('div', { 'class': 'fm350-kpi-box' }, [
			E('div', { 'class': 'fm350-kpi-label' }, [
				label,
				tag || ''
			]),
			E('div', { 'class': 'fm350-kpi-val' }, [
				val == null ? '—' : String(val),
				unit ? E('span', { 'class': 'u' }, unit) : ''
			])
		]);
	};

	var bandText = '—';
	if (s) bandText = s.band ? s.band + (s.bandGuessed ? ' (推算)' : '') : '未知';

	var rsrpGrade = gradeOf('rsrp', s ? s.rsrp : null);
	var rsrqGrade = gradeOf('rsrq', s ? s.rsrq : null);
	var sinrGrade = gradeOf('sinr', s ? s.sinr : null);

	var ratBadge = s ? (s.rat === 9 ? 'fm350-badge nr' : 'fm350-badge ok') : 'fm350-badge muted';
	var lockBadge = state.emm.locked ? 'fm350-badge lock' : 'fm350-badge muted';

	var kpis = [
		kpiBox('驻留网络制式', s ? s.ratName : '—', null,
			E('span', { 'class': ratBadge }, [
				E('i', { 'class': 'fm350-dot pulse' }),
				s ? (s.rat === 9 ? '5G 活跃' : '4G 活跃') : '未就绪'
			])
		),
		kpiBox('工作频段 Band', bandText, null),
		kpiBox('物理基站 PCI', s && s.pci ? s.pci : '—', null),
		kpiBox('下行载波频点', s && s.arfcn ? s.arfcn : '—', null),
		kpiBox('信号接收功率 RSRP', fmtNum(s ? s.rsrp : null, 0), 'dBm',
			E('span', { 'class': 'fm350-badge ' + rsrpGrade.cls }, rsrpGrade.text)
		),
		kpiBox('信号接收质量 RSRQ', fmtNum(s ? s.rsrq : null, 1), 'dB',
			E('span', { 'class': 'fm350-badge ' + rsrqGrade.cls }, rsrqGrade.text)
		),
		kpiBox('无线信噪比 SINR', fmtNum(s ? s.sinr : null, 1), 'dB',
			E('span', { 'class': 'fm350-badge ' + sinrGrade.cls }, sinrGrade.text)
		),
		kpiBox('小区锁定状态', gtactSummary(state.gtact), null,
			E('span', { 'class': lockBadge }, state.emm.locked ? '已物理锁定' : '自由驻留')
		)
	];

	return E('div', { 'class': 'fm350-glass-card' }, [
		E('div', { 'class': 'fm350-card-header' }, [
			E('h3', { 'class': 'fm350-net2-title' }, [DYNAMIC_ICONS.radar(state.scanning), '实时网络态势与射频指标']),
			E('span', { 'class': 'fm350-badge ' + (s ? 'ok' : 'muted') }, [
				E('i', { 'class': 'fm350-dot ' + (s ? 'pulse' : '') }),
				s ? '模组射频已在线' : '等待信号同步'
			])
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'毫秒级遥测模组实时射频物理层指标。所有数据由底层 AT 通道直出换算，杜绝插值造假。'),
		E('div', { 'class': 'fm350-kpi-grid' }, kpis),
		buildTrend(state),
		buildDiagnose(state)
	]);
}

/* 动态面积渐变趋势折线图 (Dynamic Gradient Area Chart) */
function buildTrend(state) {
	var pts = state.history.filter(function(h) { return h.rsrp != null; });
	if (pts.length < 2) {
		return E('div', { 'class': 'fm350-trend-container', 'style': 'text-align:center;padding:24px;font-size:12.5px;color:var(--fm-text-muted)' },
			'至少需要连续采样 2 次以绘制射频时序图。请点击下方「立即扫描」或开启自动轮询。');
	}

	var show = pts.slice(-40);
	var W = 100, H = 34;

	function yOfRsrp(v) {
		var t = (v + 125) / 65; // -125dBm 到 -60dBm
		t = Math.max(0, Math.min(1, t));
		return H - t * H;
	}

	function yOfSinr(v) {
		var t = (v + 5) / 35;   // -5dB 到 30dB
		t = Math.max(0, Math.min(1, t));
		return H - t * H;
	}

	var pathRsrp = '', areaRsrp = '';
	var pathSinr = '';

	for (var i = 0; i < show.length; i++) {
		var x = (i / (show.length - 1)) * W;
		var yR = yOfRsrp(show[i].rsrp);
		var seg = (i === 0 ? 'M' : 'L') + x.toFixed(2) + ' ' + yR.toFixed(2);
		pathRsrp += seg;
		areaRsrp += seg;

		if (show[i].sinr != null) {
			var yS = yOfSinr(show[i].sinr);
			pathSinr += (pathSinr ? ' L' : 'M') + x.toFixed(2) + ' ' + yS.toFixed(2);
		}
	}
	areaRsrp += ' L' + W + ' ' + H + ' L0 ' + H + ' Z';

	var svg = '<svg class="fm350-net2-trend" viewBox="0 0 ' + W + ' ' + H + '" preserveAspectRatio="none">' +
		'<defs>' +
			'<linearGradient id="rsrp-fill" x1="0" y1="0" x2="0" y2="1">' +
				'<stop offset="0%" stop-color="#0284c7" stop-opacity="0.35"/>' +
				'<stop offset="100%" stop-color="#0284c7" stop-opacity="0.0"/>' +
			'</linearGradient>' +
		'</defs>' +
		'<line x1="0" y1="' + yOfRsrp(-85) + '" x2="' + W + '" y2="' + yOfRsrp(-85) +
			'" stroke="#10b981" stroke-opacity=".3" stroke-width=".4" stroke-dasharray="2 2"/>' +
		'<line x1="0" y1="' + yOfRsrp(-105) + '" x2="' + W + '" y2="' + yOfRsrp(-105) +
			'" stroke="#f59e0b" stroke-opacity=".3" stroke-width=".4" stroke-dasharray="2 2"/>' +
		'<path d="' + areaRsrp + '" fill="url(#rsrp-fill)"/>' +
		'<path d="' + pathRsrp + '" fill="none" stroke="#0284c7" stroke-width="1.2" stroke-linecap="round"/>' +
		'<path d="' + pathSinr + '" fill="none" stroke="#10b981" stroke-width="1.0" stroke-linecap="round" stroke-dasharray="1 0.6"/>' +
	'</svg>';

	return E('div', { 'class': 'fm350-trend-container' }, [
		E('div', { 'style': 'display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;' }, [
			E('span', { 'style': 'font-size:11.5px;font-weight:600;color:var(--fm-text-muted)' }, '时域射频波动曲线 (最近采样 40 点)'),
			E('div', { 'style': 'display:flex;gap:12px;font-size:11px;' }, [
				E('span', { 'style': 'color:#0284c7;font-weight:600' }, '● RSRP 强度 (dBm)'),
				E('span', { 'style': 'color:#10b981;font-weight:600' }, '■ SINR 质噪比 (dB)')
			])
		]),
		parseSvg(svg)
	]);
}

/* 诊断结论 */
function buildDiagnose(state) {
	var s = state.cells.serving;
	var list = [];

	if (!s) {
		list.push(E('div', { 'class': 'fm350-net2-diag warn' }, '模组尚未捕获有效驻留小区信号，建议执行「立即扫描」。'));
		return E('div', { 'class': 'fm350-diag-grid' }, list);
	}

	if (s.rsrp != null) {
		if (s.rsrp < -110) {
			list.push(E('div', { 'class': 'fm350-net2-diag danger' },
				'🚨 弱覆盖预警：当前 RSRP 为 ' + fmtNum(s.rsrp, 0) + ' dBm，极易发生调制降阶或掉网，建议微调天线极化方向或外接高增益天线。'));
		} else if (s.rsrp < -100) {
			list.push(E('div', { 'class': 'fm350-net2-diag warn' },
				'⚠️ 覆盖边缘：信号接收功率 ' + fmtNum(s.rsrp, 0) + ' dBm，处于小区边缘盲区边缘。'));
		} else {
			list.push(E('div', { 'class': 'fm350-net2-diag ok' },
				'✨ 射频链路良好：当前物理信道信号强度充足 (' + fmtNum(s.rsrp, 0) + ' dBm)。'));
		}
	}

	var coAll = state.cells.neighbors.filter(function(n) { return n.arfcn && s.arfcn && n.arfcn === s.arfcn; });
	if (coAll.length > 0) {
		var strong = coAll.filter(function(n) { return n.rsrp != null && s.rsrp != null && n.rsrp > s.rsrp - 6; });
		if (strong.length) {
			list.push(E('div', { 'class': 'fm350-net2-diag warn' },
				'⚡ 强同频干扰源：检测到 ' + strong.length + ' 个强同频邻区信号相差不足 6 dB (最强 PCI: ' + (strong[0].pci || '—') + ' / ' + fmtNum(strong[0].rsrp, 0) + ' dBm)，可能压制下行速率，建议手动锁定至纯净 PCI。'));
		}
	}

	if (state.pciChanges >= 2) {
		list.push(E('div', { 'class': 'fm350-net2-diag warn' },
			'🔄 乒乓重选警报：会话期间物理基站 PCI 已发生 ' + state.pciChanges + ' 次跳变，建议进行物理小区锁定以固化路由。'));
	}

	if (state.emm.locked) {
		list.push(E('div', { 'class': 'fm350-net2-diag ok' },
			'🔒 锁定防护生效：设备已锁定于专有物理小区，漫游及自动重选已被隔离。'));
	}

	return E('div', { 'class': 'fm350-diag-grid' }, list);
}

/* ----------------------------------------------------------------
 * 区块二：邻区扫描 (感知雷达与基站列表)
 * ---------------------------------------------------------------- */
function buildNeighbors(state, host) {
	var scanBtn = E('button', {
		'class': 'fm350-btn-glass',
		'click': function() { doScan(state, host); }
	}, [DYNAMIC_ICONS.radar(state.scanning), state.scanning ? '正在扫描基站…' : '立即全频扫描']);

	var autoSel = E('select', { 'class': 'fm350-glass-input' }, [
		E('option', { 'value': '0' }, '关闭自动扫描'),
		E('option', { 'value': '30000' }, '每 30 秒轮询'),
		E('option', { 'value': '60000' }, '每 1 分钟轮询'),
		E('option', { 'value': '300000' }, '每 5 分钟轮询')
	]);
	autoSel.value = String(state.intervalMs || 0);
	autoSel.addEventListener('change', function(ev) {
		setAutoScan(state, host, parseInt(ev.target.value, 10) || 0);
	});

	var ratSel = E('select', { 'class': 'fm350-glass-input' }, [
		E('option', { 'value': '' }, '全部制式'),
		E('option', { 'value': '9' }, '5G NR'),
		E('option', { 'value': '4' }, '4G LTE')
	]);
	ratSel.value = state.filterRat || '';
	ratSel.addEventListener('change', function(ev) {
		state.filterRat = ev.target.value;
		refreshTable(state, host);
	});

	var minSel = E('select', { 'class': 'fm350-glass-input' }, [
		E('option', { 'value': '' }, '所有信号强度'),
		E('option', { 'value': '-85' }, '≥ -85 dBm (极佳)'),
		E('option', { 'value': '-95' }, '≥ -95 dBm (良)'),
		E('option', { 'value': '-105' }, '≥ -105 dBm (中等)')
	]);
	minSel.value = (state.filterMinRsrp == null) ? '' : String(state.filterMinRsrp);
	minSel.addEventListener('change', function(ev) {
		state.filterMinRsrp = ev.target.value === '' ? null : parseInt(ev.target.value, 10);
		refreshTable(state, host);
	});

	var tbody = E('tbody');
	var table = E('table', { 'class': 'fm350-net2-table' }, [
		E('thead', {}, [E('tr', {}, [
			E('th', {}, '基站属性'),
			E('th', {}, '制式'),
			E('th', {}, '频段'),
			E('th', { 'data-key': 'arfcn' }, '频点 ARFCN ↕'),
			E('th', { 'data-key': 'pci' }, '物理小区 PCI ↕'),
			E('th', {}, '基站标识 ECI/NCI'),
			E('th', { 'data-key': 'rsrp' }, 'RSRP 强度 ↕'),
			E('th', { 'data-key': 'rsrq' }, 'RSRQ 质量 ↕'),
			E('th', { 'data-key': 'sinr' }, 'SINR 信噪比 ↕'),
			E('th', { 'style': 'text-align:right' }, '快捷操作')
		])]),
		tbody
	]);

	var ths = table.querySelectorAll('th[data-key]');
	for (var i = 0; i < ths.length; i++) {
		(function(th) {
			th.addEventListener('click', function() {
				var k = th.getAttribute('data-key');
				if (state.sortKey === k) state.sortDesc = !state.sortDesc;
				else { state.sortKey = k; state.sortDesc = true; }
				refreshTable(state, host);
			});
		})(ths[i]);
	}

	state.tbody = tbody;

	var card = E('div', { 'class': 'fm350-glass-card' }, [
		E('div', { 'class': 'fm350-card-header' }, [
			E('h3', { 'class': 'fm350-net2-title' }, [DYNAMIC_ICONS.radar(state.scanning), '周边基站感知与邻区扫描']),
			E('span', { 'class': 'fm350-badge ok' }, 'GTCCINFO 引擎')
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'监听当前地理位置下空口下行同步信令。支持点击表头多维排序，无感静默扫描不中断当前链路。'),
		E('div', { 'class': 'fm350-ctrl-row' }, [
			scanBtn,
			E('span', { 'style': 'font-size:12px;margin-left:6px;' }, '自动轮询:'), autoSel,
			E('span', { 'style': 'font-size:12px;margin-left:6px;' }, '制式过滤:'), ratSel,
			E('span', { 'style': 'font-size:12px;margin-left:6px;' }, '信号阈值:'), minSel
		]),
		E('div', { 'class': 'fm350-table-wrap' }, table)
	]);

	refreshTable(state, host);
	return card;
}

function refreshTable(state, host) {
	var tbody = state.tbody;
	if (!tbody) return;
	while (tbody.firstChild) tbody.removeChild(tbody.firstChild);

	var list = state.cells.neighbors.slice();
	if (state.cells.serving) list.unshift(state.cells.serving);

	if (state.filterRat) {
		list = list.filter(function(c) { return String(c.rat) === state.filterRat; });
	}
	if (state.filterMinRsrp != null) {
		list = list.filter(function(c) { return c.rsrp != null && c.rsrp >= state.filterMinRsrp; });
	}

	var key = state.sortKey || 'rsrp';
	var numeric = (key === 'arfcn' || key === 'pci');
	list.sort(function(a, b) {
		var va = a[key], vb = b[key];
		if (numeric) {
			va = (va === '' || va == null) ? null : parseInt(va, 10);
			vb = (vb === '' || vb == null) ? null : parseInt(vb, 10);
		}
		if (va == null && vb == null) return 0;
		if (va == null) return 1;
		if (vb == null) return -1;
		if (typeof va === 'string') return state.sortDesc ? vb.localeCompare(va) : va.localeCompare(vb);
		return state.sortDesc ? vb - va : va - vb;
	});

	if (!list.length) {
		tbody.appendChild(E('tr', {}, [
			E('td', { 'colspan': '10', 'style': 'text-align:center;padding:26px;color:var(--fm-text-muted)' },
				'未检索到匹配的基站。请尝试重置筛选或执行立即扫描。')
		]));
		return;
	}

	list.forEach(function(c) {
		var g = gradeOf('rsrp', c.rsrp);
		var gq = gradeOf('rsrq', c.rsrq);
		var gs = gradeOf('sinr', c.sinr);
		var cls = [];
		if (c.serving) cls.push('serving');
		if (c.rsrp == null) cls.push('muted');

		var action = '';
		if (!c.serving && c.pci) {
			action = E('button', {
				'class': 'fm350-btn-ghost',
				'style': 'padding:3px 10px!important;font-size:12px!important;',
				'click': function() { quickLockPci(state, host, c); }
			}, '锁定本小区');
		} else if (c.serving) {
			action = E('span', { 'class': 'fm350-badge lock' }, '当前主驻留');
		}

		tbody.appendChild(E('tr', { 'class': cls.join(' ') }, [
			E('td', {}, [
				c.serving ? E('span', { 'class': 'fm350-badge ok' }, '服务小区') : E('span', { 'class': 'fm350-badge muted' }, '邻近候选'),
				/* layoutOk=false 表示该行字段数与手册布局不符，数据不可信，显式标注 */
				c.layoutOk === false ? E('span', { 'class': 'fm350-badge warn' }, '布局异常') : ''
			]),
			E('td', {}, E('span', { 'class': c.rat === 9 ? 'fm350-badge nr' : 'fm350-badge ok' }, c.ratName)),
			E('td', {}, c.band ? (c.band + (c.bandGuessed ? ' (推算)' : '')) : '—'),
			E('td', { 'style': 'font-family:monospace;font-weight:600;' }, c.arfcn || '—'),
			E('td', { 'style': 'font-family:monospace;font-weight:600;' }, c.pci || '—'),
			E('td', { 'style': 'font-family:monospace;font-size:12px;' }, (c.cellId && c.cellId.indexOf('FFFF') < 0) ? c.cellId : '—'),
			E('td', {}, [signalMeter(g), fmtNum(c.rsrp, 0, 'dBm')]),
			E('td', {}, [signalMeter(gq), fmtNum(c.rsrq, 1, 'dB')]),
			E('td', {}, [signalMeter(gs), fmtNum(c.sinr, 1, 'dB')]),
			E('td', { 'style': 'text-align:right' }, action || '—')
		]));
	});
}

function doScan(state, host) {
	if (state.scanning) return;
	state.scanning = true;
	renderAll(host, state);

	return Promise.all([api.cell(), api.lock(), api.signal()]).then(function(r) {
		var cellPairs = (r[0] && r[0].ok && r[0].value) || [];
		var lockPairs = (r[1] && r[1].ok && r[1].value) || [];
		var sig = (r[2] && r[2].ok && r[2].value) || null;
		state.cells = parseGtccinfo(pickResp(cellPairs, 'AT+GTCCINFO?'));
		state.ca = pickResp(cellPairs, 'AT+GTCAINFO?');
		state.gtact = parseGtact(pickResp(lockPairs, 'AT+GTACT?'));
		state.emm = parseEmmchlock(pickResp(lockPairs, 'AT+EMMCHLCK?'));
		applySignal(state, sig);

		if (state.cells.serving) {
			var s = state.cells.serving;
			state.history.push({ rsrp: s.rsrp, rsrq: s.rsrq, sinr: s.sinr });
			if (state.history.length > 40) state.history.shift();
			if (state.lastPci != null && s.pci !== state.lastPci) state.pciChanges++;
			state.lastPci = s.pci;
		}
		state.scanning = false;
		renderAll(host, state);
	}).catch(function(e) {
		state.scanning = false;
		renderAll(host, state);
		ui.addNotification(null, E('p', '射频扫描失败：' + (e && e.message ? e.message : '未知错误')), 'danger');
	});
}

function setAutoScan(state, host, ms) {
	if (state.timer) { clearTimeout(state.timer); state.timer = null; }
	state.intervalMs = ms;
	if (!ms) return;
	var alive = function() { return !!(state.host && document.body.contains(state.host)); };
	var tick = function() {
		if (!alive()) { state.timer = null; return; }
		Promise.resolve(doScan(state, host)).then(function() {
			if (!alive()) { state.timer = null; return; }
			state.timer = setTimeout(tick, ms);
		});
	};
	state.timer = setTimeout(tick, ms);
}

/* ----------------------------------------------------------------
 * 区块三：锁频段 (Frequency Band Lock)
 * ---------------------------------------------------------------- */
function buildBandLock(state, host) {
	/* 这里的编码来自 `AT+GTACT?`，语义是**当前已配置的频段锁定**
	 * （实机回显形如 `20,6,3,1,2,4,…,101,102,…`），不是「模组支持的全部频段」。
	 * 后者要靠测试形式 `AT+GTACT=?`（手册 §11.1.14：依次给出
	 * supported <Rat>s / <PreferredAct1>s / <PreferredAct2>s / <gsm_band>s /
	 * <umts_band>s / <lte_band>s / <cdma_band>s / <evdo_band>s / <nr_band>s），
	 * 已加入 AT 控制台快捷入口。parseGtact 已修正为跳过前两个
	 * PreferredAct 位，不再把它们当成频段渲染出幽灵芯片。
	 *
	 * 回显中的 1..10 是 <umts_band>（CHANGELOG r17 实机回显），必须单独分组
	 * 并走罗马数字表，见 gtactBandKind。 */
	var lockedCodes = state.gtact.bands || [];
	var groups = { lte: [], nr: [], umts: [], other: [] };
	var seen = {};

	lockedCodes.forEach(function(b) {
		var kind = gtactBandKind(b);
		if (kind === 'other') { groups.other.push(b); return; }
		var name = gtactBandLabel(b);
		if (seen[name]) return;
		seen[name] = true;
		groups[kind].push(b);
	});

	function chipRow(list, dotColor) {
		if (!list.length) return E('div', { 'class': 'fm350-net2-sub' }, '模组未上报此类可用频段。');
		return E('div', { 'class': 'fm350-chips-flex' }, list.map(function(b) {
			var on = !!state.selectedBands[b];
			return E('span', {
				'class': 'fm350-net2-chip' + (on ? ' on' : ''),
				'click': function() {
					if (state.selectedBands[b]) delete state.selectedBands[b];
					else state.selectedBands[b] = true;
					renderAll(host, state);
				}
			}, [
				E('i', { 'class': 'fm350-dot', 'style': 'background:' + (on ? '#fff' : dotColor) }),
				gtactBandLabel(b)
			]);
		}));
	}

	var selCount = Object.keys(state.selectedBands).length;

	var applyBtn = E('button', {
		'class': 'fm350-btn-glass',
		'click': function() {
			var codes = Object.keys(state.selectedBands).map(Number);
			if (!codes.length) {
				ui.addNotification(null, E('p', '请至少勾选一个目标频段'), 'warning');
				return;
			}
			var args = '20,6,3,' + codes.join(',');
			applyLockBand(state, host, args, '即将下发频段锁定：限定为所勾选的 ' + codes.length + ' 个工作频段');
		}
	}, '应用勾选的频段');

	var presetRow = E('div', { 'class': 'fm350-chips-flex' },
		BAND_PRESETS.map(function(p) {
			return E('span', {
				'class': 'fm350-net2-chip preset',
				'title': p.hint,
				'click': function() {
					if (p.id === 'auto') {
						applyLockBand(state, host, p.args, '即将恢复为默认（模组全频自适应）');
					} else {
						applyLockBand(state, host, p.args, '即将应用「' + p.label + '」预设配置');
					}
				}
			}, '⚡ ' + p.label);
		})
	);

	return E('div', { 'class': 'fm350-glass-card' }, [
		E('div', { 'class': 'fm350-card-header' }, [
			E('h3', { 'class': 'fm350-net2-title' }, [DYNAMIC_ICONS.band(), '工作频段限定与锁定 (Band Lock)']),
			E('span', { 'class': 'fm350-badge lock' }, '当前：' + gtactSummary(state.gtact))
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'强行约束射频只在指定载波频段驻留。用于避开拥堵基站或锁定专属大带宽频段。'),
		E('div', { 'style': 'margin-bottom:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:var(--fm-text-muted)' }, '常用一键预设:'),
			presetRow
		]),
		E('div', { 'style': 'margin-top:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:#7c3aed' }, '5G NR 频段矩阵:'),
			chipRow(groups.nr, '#7c3aed')
		]),
		E('div', { 'style': 'margin-top:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:#d97706' }, '3G UMTS 频段矩阵:'),
			chipRow(groups.umts, '#d97706')
		]),
		E('div', { 'style': 'margin-top:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:#0284c7' }, '4G LTE 频段矩阵:'),
			chipRow(groups.lte, '#0284c7')
		]),
		E('div', { 'class': 'fm350-ctrl-row', 'style': 'margin-top:16px;' }, [
			applyBtn,
			E('button', {
				'class': 'fm350-btn-ghost',
				'click': function() { state.selectedBands = {}; renderAll(host, state); }
			}, '重置选择'),
			E('span', { 'style': 'font-size:12px;color:var(--fm-text-muted)' }, '已选中 ' + selCount + ' 个频段')
		])
	]);
}

/* ----------------------------------------------------------------
 * 区块四：锁小区 (Cell & PCI Lock)
 * ---------------------------------------------------------------- */
function buildCellLock(state, host) {
	var s = state.cells.serving;
	if (!state.cellForm) {
		state.cellForm = { pci: s ? (s.pci || '') : '', arfcn: s ? (s.arfcn || '') : '' };
	}

	var pciInput = E('input', {
		'type': 'text', 'class': 'fm350-glass-input',
		'style': 'width:130px', 'placeholder': 'PCI (如 128)'
	});
	var arfcnInput = E('input', {
		'type': 'text', 'class': 'fm350-glass-input',
		'style': 'width:150px', 'placeholder': '频点 (如 504990)'
	});
	pciInput.value = state.cellForm.pci;
	arfcnInput.value = state.cellForm.arfcn;
	pciInput.addEventListener('input', function() { state.cellForm.pci = pciInput.value; });
	arfcnInput.addEventListener('input', function() { state.cellForm.arfcn = arfcnInput.value; });

	var unlockBtn = E('button', {
		'class': 'fm350-btn-ghost',
		'click': function() {
			applyLockCell(state, host, '0', '即将解除物理小区锁定，恢复基站漫游自治');
		}
	}, '解除小区锁定');

	return E('div', { 'class': 'fm350-glass-card' }, [
		E('div', { 'class': 'fm350-card-header' }, [
			E('h3', { 'class': 'fm350-net2-title' }, [DYNAMIC_ICONS.target(), '物理小区锁定 (Cell Lock / PCI)']),
			state.emm.locked ? E('span', { 'class': 'fm350-badge lock' }, '🔒 当前已物理锁定') : E('span', { 'class': 'fm350-badge muted' }, '🔓 当前未锁小区')
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'将模组收发机完全固化于单颗物理小区 (频点 + PCI)。彻底杜绝在多个发射塔之间来回漂移。'),
		E('div', { 'class': 'fm350-ctrl-row' }, [
			E('span', { 'style': 'font-size:12.5px;font-weight:600' }, '物理小区标识 PCI:'),
			pciInput,
			E('span', { 'style': 'font-size:12.5px;font-weight:600' }, '下行频点 ARFCN:'),
			arfcnInput,
			E('button', {
				'class': 'fm350-btn-glass',
				'click': function() {
					var pci = (pciInput.value || '').trim();
					var arfcn = (arfcnInput.value || '').trim();
					var err = validateCell(pci, arfcn, state);
					if (err) { ui.addNotification(null, E('p', err), 'warning'); return; }
					var args = '1,11,0,' + arfcn + ',' + pci + ',3';
					applyLockCell(state, host, args, '即将下发指令：锁定频点 ' + arfcn + '、PCI ' + pci);
				}
			}, '锁定此参数'),
			unlockBtn
		])
	]);
}

function validateCell(pci, arfcn, state) {
	if (!pci) return '请填写物理小区 PCI';
	if (!arfcn) return '请填写下行频点';
	if (!/^\d+$/.test(pci)) return 'PCI 必须为纯数字';
	if (!/^\d+$/.test(arfcn)) return '频点必须为纯数字';
	var pNum = parseInt(pci, 10);
	if (pNum < 0 || pNum > 1007) return 'PCI 越界 (取值范围 0 ~ 1007)';
	var aNum = parseInt(arfcn, 10);
	if (aNum <= 0) return '频点必须为正整数';

	var all = (state.cells.neighbors || []).concat(state.cells.serving ? [state.cells.serving] : []);
	var hit = all.filter(function(c) {
		return String(c.pci) === String(pNum) && String(c.arfcn) === String(aNum);
	});
	if (!hit.length) {
		return '注意：扫描列表中未发现此 PCI 与频点的小区，盲锁可能导致脱网，请确认无误后重试。';
	}
	return null;
}

/* ----------------------------------------------------------------
 * 区块五：网络制式锁定
 * ---------------------------------------------------------------- */

/* 制式锁定状态的完整文字：除了 <rat>，把手册 §11.1.14 的
 * <PreferredAct1>/<PreferredAct2> 一并说出来。
 *
 * 这两个位置参数决定了多模并存时的**驻留优先顺序**（手册 RAT Combination
 * Table），旧版完全不展示，用户看到「三模全开」却不知道其实 NR 优先。 */
function gtactSummary(g) {
	var label = (g && g.modeLabel) || '未知';
	if (!g || !g.prefNames || !g.prefNames.length) return label;
	return label + '·' + g.prefNames.join(' > ');
}

function buildRat(state, host) {
	var cur = state.gtact.mode;
	var modeBtns = Object.keys(GTACT_MODE).map(function(k) {
		var info = GTACT_MODE[k];
		var isCur = String(cur) === String(k);
		return E('span', {
			'class': 'fm350-net2-chip' + (isCur ? ' on' : ''),
			'title': info.desc,
			'click': function() {
				if (isCur) return;
				applyRat(state, host, k, '即将切换网络制式为「' + info.label + '」');
			}
		}, [
			E('i', { 'class': 'fm350-dot', 'style': 'background:' + (isCur ? '#fff' : '#059669') }),
			info.label
		]);
	});

	return E('div', { 'class': 'fm350-glass-card' }, [
		E('div', { 'class': 'fm350-card-header' }, [
			E('h3', { 'class': 'fm350-net2-title' }, [DYNAMIC_ICONS.gear(), '蜂窝制式首选项 (RAT Mode)']),
			E('span', { 'class': 'fm350-badge ok' }, gtactSummary(state.gtact))
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'指定模组与基站交互的协议世代。设定将写入模组 NVRAM 实时生效。'
			+ '注：手册 Note 6 说明设置「自动（10）」后查询会回显为 20，'
			+ '因此两者在回读时无法区分，当前状态以模组回显为准。'),
		E('div', { 'class': 'fm350-chips-flex' }, modeBtns)
	]);
}

/* ================================================================
 * 八、指令下发与安全反馈
 * ================================================================ */

function applyOp(state, host, dangerText, runFn, okMsg) {
	return confirmDanger('网络重置安全确认',
		dangerText + '。执行过程中模组将重启射频协议栈，蜂窝连接会短暂中断 15~40 秒并自动重连，确认继续？',
		'立即执行').then(function(yes) {
		if (!yes) return null;
		ui.addNotification(null, E('p', '指令已发送至底层 AT 通道，正在重协商射频…'), 'info');
		return runFn().then(function(res) {
			api.notify(res, okMsg);
			return doScan(state, host);
		});
	});
}

function applyLockBand(state, host, args, desc) {
	return applyOp(state, host, desc, function() {
		return api.lockBand(args);
	}, '频段锁定规则已成功下发');
}

function applyLockCell(state, host, args, desc) {
	return applyOp(state, host, desc, function() {
		return api.lockCell(args);
	}, '物理小区锁定指令已下发');
}

function applyRat(state, host, mode, desc) {
	return applyOp(state, host, desc, function() {
		return api.lockBand(String(mode));
	}, '网络制式已下发');
}

function quickLockPci(state, host, cell) {
	var desc = '即将锁定至基站频点 ' + cell.arfcn + '、PCI ' + cell.pci + ' (当前强度 ' + fmtNum(cell.rsrp, 0, 'dBm') + ')';
	var args = '1,11,0,' + cell.arfcn + ',' + cell.pci + ',3';
	return applyLockCell(state, host, args, desc);
}