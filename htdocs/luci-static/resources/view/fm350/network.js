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
 *    不是结构化 JSON。因此本页必须自行解析 AT 响应。
 *
 *    字段语义用「背靠背采样」确定：同一时刻先取 CESQ、紧接着取小区信息，
 *    两套独立来源应当互相印证。实机结果：
 *      +CESQ: 99,99,255,255,255,255,65,70,77
 *        → 信噪比 15.0 dB、信号强度 -87 dBm、信号质量 -11.0 dB
 *      服务小区行 1,9,460,0,149002,C2840C002,504990,128,,,16,69,69,64
 *        → 倒数第 4 位 16（单位 dB，不是索引）  对上 15.0 dB
 *        → 倒数第 3 位 69 → 信号强度 -88 dBm     对上 -87 dBm
 *        → 倒数第 1 位 64 → 信号质量 -11.5 dB    对上 -11.0 dB
 *    三者误差均不超过 1 dB，属模组的整数量化噪声，可确认映射成立。
 *
 * 2. 倒数第 2 位与倒数第 3 位在实机全部 8 行小区上恒等（69/69、126/126、
 *    52/52、50/50、70/70、66/66、41/41），是同一个量的重复上报，**不是信噪比**。
 *    早期版本曾把它当信噪比索引换算，会给出 11 dB，而真值 15~16 dB —— 错 5 dB。
 *    信噪比必须走倒数第 4 位的原始 dB 值；邻区行没有这个字段，一律显示「未测量」。
 *
 * 3. 索引 126 / 127 / 255 是「未测量」哨兵值，不是真实信号。
 *    实机邻区大量出现 126，若直接换算会得出荒谬数值，故一律标记「未测量」。
 *
 * 3. 载波聚合 AT+GTCAINFO? 在本模组上只回 OK，无数据 —— 面板如实标注「不可用」，
 *    不伪造聚合信息。
 *
 * 4. 制式优先级 AT+QNWPREFCFG="rat_acq_order" 在本模组上返回
 *    +CME ERROR: unknown —— 该能力不存在。本页改为提供「制式选择」
 *    （自动 / 仅 5G / 仅 4G，经 AT+GTACT 的 mode 下发），并对排序功能
 *    明确标注「本模组不支持」。宁可少给功能，也不给点了没反应的按钮。
 *
 * 5. 锁频段 / 锁小区会先下发离线指令再恢复，期间蜂窝链路会中断数十秒。
 *    属危险操作，一律二次确认，并提示「期间会断网」。
 *
 * 面向普通用户的约定：
 *   - 全程中文标签与说明，不在界面上出现原始 AT 指令或内部参数名；
 *   - 每个操作都有明确的成功 / 失败反馈；
 *   - 任何锁定都提供「恢复默认（自动）」入口，避免用户把自己锁死。
 */

/* ================================================================
 * 一、基础工具
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

/* 3GPP 阶梯索引 → 物理量。idx <= 0 或达到上限哨兵时返回 null（未测量）。 */
function idxToLinear(idx, base, step, maxIdx) {
	if (idx == null) return null;
	var n = parseInt(idx, 10);
	if (!isFinite(n) || n <= 0) return null;
	if (maxIdx != null && n > maxIdx) return null;
	return base + (n - 1) * step;
}

/* 信号强度 RSRP：索引 1 → -156 dBm，步长 1 dB；126 及以上为「未测量」哨兵 */
function rsrpFromIdx(idx, rat) {
	if (rat === 4) return idxToLinear(idx, -140, 1, 97);      // LTE（-140 ~ -44 dBm）
	return idxToLinear(idx, -156, 1, 125);                     // NR（-156 ~ -32 dBm）
}

/* 信号质量 RSRQ：索引 1 → -43 dB，步长 0.5 dB；126 及以上为「未测量」哨兵 */
function rsrqFromIdx(idx, rat) {
	if (rat === 4) return idxToLinear(idx, -19.5, 0.5, 34);    // LTE（-19.5 ~ -3 dB）
	return idxToLinear(idx, -43, 0.5, 125);                    // NR（-43 ~ 20 dB）
}

/* 信噪比 SINR：模组在小区信息里直接给 dB 原值（-100 ~ 100），**不是阶梯索引**。
 *
 * 这里曾踩过一个坑：早期版本按「索引」换算倒数第 2 位，得出 11 dB，
 * 而同一时刻用扩展信号指令测得的真值是 15~16 dB —— 差 5 dB。
 * 根因是倒数第 2 位与倒数第 3 位恒等，是同一个量的重复上报，并非信噪比。
 * 信噪比必须取倒数第 4 位的 dB 原值。
 *
 * 越界或缺失一律判为未测量：邻区行该位置是 126 这类哨兵，
 * 当成 126 dB 显示出来就是编造数据。 */
function sinrFromRaw(v) {
	if (v == null || v === '') return null;
	var n = parseInt(v, 10);
	if (!isFinite(n)) return null;
	if (n < -100 || n > 100) return null;
	return n;
}

/* ================================================================
 * 二、制式与频段映射
 * ================================================================ */

var RAT_NAME = {
	0: '2G GSM', 2: '3G WCDMA', 3: '3G TD-SCDMA',
	4: '4G LTE', 5: '2G CDMA', 9: '5G NR'
};

/* AT+GTACT 的模式位。这是本模组实际控制制式的手段。 */
var GTACT_MODE = {
	2: { label: '仅 4G（LTE）', desc: '只在 4G 网络下驻留，5G 不可用' },
	14: { label: '仅 5G（NR）', desc: '只在 5G 网络下驻留，无 5G 覆盖时会脱网' },
	20: { label: '自动（推荐）', desc: '由模组与网络自行选择，通常体验最好' }
};

/*
 * 频段编号 → 人类可读名称。
 * 实机 AT+GTACT? 返回的频段编号分三套编码：
 *   - LTE 频段：1..88 直接就是 B1/B3/B5/B8 …
 *   - NR 编码 A：101..199 = 100 + nXX（101 → n1，141 → n41，171 → n71）
 *   - NR 编码 B：501..599 = 500 + nXX（501 → n1，508 → n8）
 *   - NR 编码 C：5020..5079 = 5000 + nXX（5041 → n41，5078 → n78）
 *
 * 编码 A 的存在有一条硬证据：实机返回的 101..171 序列里，
 * 106 / 109 / 110 / 111 / 115 / 116 / 121~124 / 127 全部缺席 —— 而 3GPP
 * 恰好不存在 n6 / n9 / n10 / n11 / n15 / n16 / n21~n24 / n27。
 * 若按「LTE 频段」解读成 B101，就会出现 3GPP 里根本不存在的 B101。
 *
 * 同一个 5G 频段会在三套编码里重复出现（n41 同时有 141 / 5041），
 * 因此界面按频段名去重后再展示，避免同一个 n41 出现三张一模一样的卡片。
 */
function bandName(code) {
	var n = parseInt(code, 10);
	if (!isFinite(n)) return 'B' + code;
	if (n >= 5000 && n <= 5079) return 'n' + (n - 5000);
	if (n >= 500 && n <= 599) return 'n' + (n - 500);
	if (n >= 101 && n <= 199) return 'n' + (n - 100);
	if (n >= 1 && n <= 88) return 'B' + n;
	return '编号 ' + n;
}

/* 频段编号归属的制式分组，用于界面按制式归类 */
function bandGroup(code) {
	var n = parseInt(code, 10);
	if (!isFinite(n)) return 'other';
	if (n >= 5000 && n <= 5079) return 'nr';
	if (n >= 500 && n <= 599) return 'nr';
	if (n >= 101 && n <= 199) return 'nr';
	if (n >= 1 && n <= 88) return 'lte';
	/* 越界编号一律归入 other：宁可如实标注“无法确认”，也不编造频段名。
	 * 实机 AT+GTACT? 的返回里存在 101 这类无法映射为 LTE/NR 频段的条目。 */
	return 'other';
}

/*
 * 常用预设组合。
 * 说明：频段编号取自实机锁定状态查询的实际返回值，
 * 组合按中国大陆现网常见搭配给出，避免用户逐个勾选。
 */
var BAND_PRESETS = [
	{ id: 'auto', label: '恢复默认（自动）', hint: '不锁频段，由模组自行选择',
	  args: '20' },
	{ id: 'nr_main', label: '5G 主力频段', hint: 'n41 / n78 / n79，覆盖国内 5G 主力频段',
	  args: '20,5041,5078,5079' },
	{ id: 'nr_all', label: '5G 全频段', hint: '模组支持的全部 5G 频段（n20 ~ n79）',
	  args: '20,5020,5025,5028,5030,5038,5040,5041,5048,5066,5071,5077,5078,5079' },
	{ id: 'lte_main', label: '4G 主力频段', hint: 'B1 / B3 / B5 / B8，国内 4G 常用',
	  args: '20,1,3,5,8' },
	{ id: 'lte_all', label: '4G 全频段', hint: '模组支持的全部 4G 频段（B1 ~ B8）',
	  args: '20,1,2,3,4,5,6,8' },
	{ id: 'mixed', label: '5G 主力 + 4G 主力', hint: '兼顾覆盖与回落，适合日常固定场所',
	  args: '20,1,3,5,8,5041,5078,5079' },
	{ id: 'nr_only_41', label: '仅 n41', hint: '锁定单一 5G 频段，仅用于对比测试',
	  args: '20,5041' }
];

/* 由 ARFCN 反推频段（仅在无法从锁定列表获得时使用，结果标注为「推算」） */
function arfcnToBandLabel(arfcn, rat) {
	var n = parseInt(arfcn, 10);
	if (!isFinite(n) || n <= 0) return null;
	if (rat === 4) {
		// LTE：直接按 EARFCN 区间判定（不引入浮点换算）
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
	// NR：按 TS 38.104 全局频率栅格换算（0–3000 MHz 用 5 kHz 步长），再查频段范围
	var freq = n * 0.005; // MHz
	if (freq >= 2110 && freq <= 2170) return 'n1';
	if (freq >= 1805 && freq <= 1880) return 'n3';
	if (freq >= 869 && freq <= 894) return 'n5';
	if (freq >= 925 && freq <= 960) return 'n8';
	if (freq >= 791 && freq <= 862) return 'n20';
	if (freq >= 758 && freq <= 803) return 'n28';
	if (freq >= 2570 && freq <= 2620) return 'n38';
	if (freq >= 2496 && freq <= 2690) return 'n41';
	if (freq >= 3300 && freq <= 4200) return 'n77/n78';
	if (freq >= 4400 && freq <= 5000) return 'n79';
	return null;
}

/* ================================================================
 * 三、AT 响应解析
 * ================================================================ */

/*
 * 解析小区信息响应。
 * 实机样例（服务小区 14 字段 / 邻区 12 字段）：
 *   +GTCCINFO:
 *   1,9,460,0,149002,C2840C002,504990,128,,,16,69,69,64      <- 服务小区
 *   2,9,460,0,FFFFFFF,00FFFFFFF,504990,643,,126,126,64       <- 邻区
 *   2,9,,,FFFFFFF,00FFFFFFF,152650,163,,70,70,62
 *   OK
 *
 * 前 8 个字段三种制式通用：
 *   0 是否服务小区(1=是,2=邻区)  1 制式  2 MCC  3 MNC
 *   4 位置区/跟踪区  5 小区标识  6 频点  7 PCI
 *   8 频段（本模组在 5G 下恒为空，此时按频点推算并标注）
 *
 * 末尾四个字段（自倒数第 4 位起）：
 *   -4 信噪比，单位 dB 的原值（非索引）
 *   -3 信号强度 RSRP 的阶梯索引
 *   -2 与 -3 恒等的重复上报，不使用
 *   -1 信号质量 RSRQ 的阶梯索引
 */
function parseGtccinfo(text) {
	var out = { serving: null, neighbors: [], rawOk: false };
	if (typeof text !== 'string') return out;
	var lines = text.split('\n');
	for (var i = 0; i < lines.length; i++) {
		var line = lines[i].trim();
		if (!line || line === 'OK' || line.indexOf('+GTCCINFO') === 0) continue;
		if (!/^[0-9]/.test(line)) continue;
		var f = line.split(',');
		if (f.length < 8) continue;
		out.rawOk = true;

		var isServing = f[0] === '1';
		var rat = parseInt(f[1], 10);
		var len = f.length;
		var sinrRaw = len >= 4 ? (f[len - 4] || '').trim() : '';
		var rsrpRaw = len >= 3 ? (f[len - 3] || '').trim() : '';
		var rsrqRaw = len >= 1 ? (f[len - 1] || '').trim() : '';

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
			band: (f[8] || '').trim(),
			rsrpIdx: isFinite(parseInt(rsrpRaw, 10)) ? parseInt(rsrpRaw, 10) : null,
			rsrqIdx: isFinite(parseInt(rsrqRaw, 10)) ? parseInt(rsrqRaw, 10) : null
		};
		cell.rsrp = rsrpFromIdx(cell.rsrpIdx, rat);
		cell.rsrq = rsrqFromIdx(cell.rsrqIdx, rat);
		/* 邻区行的倒数第 4 位是 126 这类哨兵或空值，会被 sinrFromRaw 判为未测量，
		 * 界面如实显示「未测量」，不换算成数值。 */
		cell.sinr = sinrFromRaw(sinrRaw);
		if (!cell.band) {
			var guess = arfcnToBandLabel(cell.arfcn, rat);
			cell.band = guess || '';
			cell.bandGuessed = !!guess;
		}
		if (isServing) out.serving = cell;
		else out.neighbors.push(cell);
	}
	return out;
}

/*
 * 解析 AT+GTACT? 响应。
 * 实机样例：+GTACT: 20,6,3,1,2,4,5,8,101,...,5041,5078,5079
 * 首位是模式（20 自动 / 14 仅 5G / 2 仅 4G），其后为模组支持的频段编号清单。
 */
function parseGtact(text) {
	var out = { mode: null, modeLabel: '未知', modeDesc: '', bands: [], raw: '' };
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
	// 首个字段之后依次是参数位与频段位；频段编号均为纯数字，直接收集
	for (var i = 1; i < parts.length; i++) {
		var v = parts[i].trim();
		if (v === '' || !/^\d+$/.test(v)) continue;
		out.bands.push(parseInt(v, 10));
	}
	return out;
}

/*
 * 解析 AT+EMMCHLCK? 响应。
 * 实机样例：+EMMCHLCK: 0        → 未锁小区
 * 锁定态形如：+EMMCHLCK: 1,<...>,<arfcn>,<pci>,<...>
 */
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

/* 从 cell / lock 的二元组响应里按指令名取出对应响应文本 */
function pickResp(pairs, cmd) {
	if (!Array.isArray(pairs)) return '';
	for (var i = 0; i < pairs.length; i++) {
		if (pairs[i] && pairs[i][0] && pairs[i][0].indexOf(cmd) === 0)
			return pairs[i][1] || '';
	}
	return '';
}

/* ================================================================
 * 四、样式（与本项目其它页面一致的毛玻璃主题，仅注入一次）
 * ================================================================ */

function injectTheme() {
	var styleId = 'fm350-network-v2-styles';
	if (document.getElementById(styleId)) return;
	var css = [
		'.fm350-net2-wrap{max-width:1180px}',
		'.fm350-net2-card{background:var(--background-color-highlight,rgba(255,255,255,.55));',
		' border:1px solid var(--border-color,rgba(0,0,0,.08));border-radius:14px;',
		' padding:16px 18px;margin-bottom:16px;backdrop-filter:blur(10px)}',
		'.fm350-net2-title{margin:0 0 4px;font-size:15px;font-weight:600;display:flex;',
		' align-items:center;gap:8px}',
		'.fm350-net2-sub{margin:0 0 12px;font-size:12px;opacity:.7;line-height:1.6}',
		'.fm350-net2-row{display:flex;flex-wrap:wrap;gap:10px;align-items:center}',
		'.fm350-net2-kpi{display:flex;flex-wrap:wrap;gap:12px}',
		'.fm350-net2-kpi>div{flex:1 1 150px;min-width:150px;padding:10px 12px;border-radius:10px;',
		' background:var(--background-color-highlight,rgba(255,255,255,.4))}',
		'.fm350-net2-kpi .k{font-size:11px;opacity:.65;margin-bottom:4px}',
		'.fm350-net2-kpi .v{font-size:17px;font-weight:600;word-break:break-all}',
		'.fm350-net2-kpi .u{font-size:11px;opacity:.6;margin-left:3px;font-weight:400}',
		'.fm350-net2-tag{display:inline-block;padding:2px 8px;border-radius:999px;font-size:11px;',
		' border:1px solid currentColor;margin-left:6px}',
		'.fm350-net2-tag.ok{color:#2e7d32}',
		'.fm350-net2-tag.warn{color:#ef6c00}',
		'.fm350-net2-tag.danger{color:#c62828}',
		'.fm350-net2-tag.lock{color:#1565c0}',
		'.fm350-net2-tag.muted{color:#777}',
		'.fm350-net2-table{width:100%;border-collapse:collapse;font-size:12.5px}',
		'.fm350-net2-table th,.fm350-net2-table td{padding:7px 8px;text-align:left;',
		' border-bottom:1px solid var(--border-color,rgba(0,0,0,.07));white-space:nowrap}',
		'.fm350-net2-table th{cursor:pointer;user-select:none;font-weight:600;opacity:.85}',
		'.fm350-net2-table th:hover{opacity:1;text-decoration:underline}',
		'.fm350-net2-table tr.serving{background:rgba(21,101,192,.08)}',
		'.fm350-net2-table tr.muted td{opacity:.55}',
		'.fm350-net2-bar{display:inline-block;width:52px;height:6px;border-radius:3px;',
		' background:rgba(0,0,0,.12);overflow:hidden;vertical-align:middle}',
		'.fm350-net2-bar>i{display:block;height:100%;border-radius:3px}',
		'.fm350-net2-chip{border:1px solid var(--border-color,rgba(0,0,0,.15));border-radius:999px;',
		' padding:4px 11px;font-size:12px;cursor:pointer;user-select:none;',
		' background:var(--background-color-highlight,rgba(255,255,255,.4))}',
		'.fm350-net2-chip.on{background:#1565c0;color:#fff;border-color:#1565c0}',
		'.fm350-net2-chip.preset{background:rgba(21,101,192,.1);border-color:rgba(21,101,192,.35)}',
		'.fm350-net2-group{margin:10px 0 4px;font-size:12px;font-weight:600;opacity:.75}',
		'.fm350-net2-note{font-size:11.5px;opacity:.7;line-height:1.7;margin-top:8px}',
		'.fm350-net2-diag{margin:6px 0;padding:8px 12px;border-radius:8px;font-size:12.5px;',
		' border-left:3px solid #999;background:rgba(0,0,0,.04)}',
		'.fm350-net2-diag.warn{border-left-color:#ef6c00;background:rgba(239,108,0,.08)}',
		'.fm350-net2-diag.danger{border-left-color:#c62828;background:rgba(198,40,40,.08)}',
		'.fm350-net2-diag.ok{border-left-color:#2e7d32;background:rgba(46,125,50,.08)}',
		'.fm350-net2-trend{width:100%;height:74px;display:block}',
		'.fm350-net2-empty{padding:14px;font-size:12.5px;opacity:.65;text-align:center}',
		'.fm350-net2-mask{position:fixed;inset:0;background:rgba(0,0,0,.35);z-index:9999;',
		' display:flex;align-items:center;justify-content:center}',
		'.fm350-net2-modal{background:var(--background-color,#fff);border-radius:12px;',
		' padding:18px 20px;max-width:440px;box-shadow:0 10px 30px rgba(0,0,0,.25)}',
		'.fm350-net2-modal h4{margin:0 0 10px;font-size:15px}',
		'.fm350-net2-modal p{margin:0 0 14px;font-size:13px;line-height:1.7}'
	].join('');
	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

/* 动态图标 */
var ICONS = {
	radar: function() {
		return parseSvg('<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" ' +
			'stroke="currentColor" stroke-width="1.7" width="18" height="18">' +
			'<circle cx="12" cy="12" r="9" stroke-opacity=".35"/>' +
			'<circle cx="12" cy="12" r="4.5" stroke-opacity=".7"/>' +
			'<circle cx="15" cy="9" r="1.6" fill="currentColor" stroke="none"/>' +
			'<line x1="12" y1="12" x2="21" y2="12" stroke-linecap="round"/></svg>');
	},
	band: function() {
		return parseSvg('<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" ' +
			'stroke="currentColor" stroke-width="1.7" width="18" height="18">' +
			'<path d="M2 12c2.5-6 4.5-6 7 0s4.5 6 7 0 4.5-6 6 0" stroke-linecap="round"/>' +
			'<line x1="2" y1="20" x2="22" y2="20" stroke-opacity=".3" stroke-linecap="round"/></svg>');
	},
	target: function() {
		return parseSvg('<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" ' +
			'stroke="currentColor" stroke-width="1.7" width="18" height="18">' +
			'<circle cx="12" cy="12" r="8.5" stroke-opacity=".35"/>' +
			'<circle cx="12" cy="12" r="3.5"/><line x1="12" y1="1.5" x2="12" y2="6"/>' +
			'<line x1="12" y1="18" x2="12" y2="22.5"/><line x1="1.5" y1="12" x2="6" y2="12"/>' +
			'<line x1="18" y1="12" x2="22.5" y2="12"/></svg>');
	},
	gear: function() {
		return parseSvg('<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" ' +
			'stroke="currentColor" stroke-width="1.7" width="18" height="18">' +
			'<circle cx="12" cy="12" r="3.2"/>' +
			'<path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1' +
			'M19.1 4.9L17 7M7 17l-2.1 2.1" stroke-linecap="round"/></svg>');
	}
};

/* ================================================================
 * 五、危险操作二次确认（自建，避免依赖不确定的框架弹窗 API）
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
					E('button', {
						'class': 'btn',
						'click': function() { done(false); }
					}, '取消'),
					E('button', {
						'class': 'btn cbi-button-apply',
						'click': function() { done(true); }
					}, okText || '确认执行')
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
 * 六、信号质量评价（统一阈值，界面与诊断共用）
 * ================================================================ */

function gradeOf(kind, v) {
	if (v == null || !isFinite(v)) return { level: 0, text: '未测量', color: '#9e9e9e' };
	if (kind === 'rsrp') {
		if (v >= -85) return { level: 4, text: '优', color: '#2e7d32' };
		if (v >= -95) return { level: 3, text: '良', color: '#7cb342' };
		if (v >= -105) return { level: 2, text: '中', color: '#ef6c00' };
		if (v >= -115) return { level: 1, text: '弱', color: '#e65100' };
		return { level: 1, text: '很差', color: '#c62828' };
	}
	if (kind === 'rsrq') {
		if (v >= -10) return { level: 4, text: '优', color: '#2e7d32' };
		if (v >= -15) return { level: 3, text: '良', color: '#7cb342' };
		if (v >= -20) return { level: 2, text: '中', color: '#ef6c00' };
		return { level: 1, text: '差', color: '#c62828' };
	}
	// sinr
	if (v >= 20) return { level: 4, text: '优', color: '#2e7d32' };
	if (v >= 13) return { level: 3, text: '良', color: '#7cb342' };
	if (v >= 0) return { level: 2, text: '中', color: '#ef6c00' };
	return { level: 1, text: '差', color: '#c62828' };
}

function signalBar(grade) {
	var pct = [0, 25, 50, 75, 100][grade.level] || 0;
	return E('span', { 'class': 'fm350-net2-bar', 'title': grade.text }, [
		E('i', { 'style': 'width:' + pct + '%;background:' + grade.color })
	]);
}

/* ================================================================
 * 七、视图主体
 * ================================================================ */

/* 当前活动的页面状态，供 remove() 清理定时器。整页重建不会替换 host，
 * 因此页面存活判断可以直接挂在 state.host 上。 */
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
			/* 信号趋势与重选统计：只在本次页面会话内累计，如实标注来源 */
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
			/* 锁小区表单的输入值单独留存：整页重建时不能被预填值冲掉 */
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

	/* 页面切走时必须停掉定时器，否则会一直对模组下发查询指令 */
	remove: function() {
		if (ACTIVE && ACTIVE.timer) {
			clearTimeout(ACTIVE.timer);
			ACTIVE.timer = null;
			ACTIVE.intervalMs = 0;
		}
	}
});

/*
 * 服务小区的信号读数以后端综合信号为准。
 * 后端用扩展信号指令换算，与状态页完全同源；本页若自行按小区信息索引换算，
 * 会出现「状态页 -87 dBm、本页 -88 dBm」这种同源不同值的困惑。
 * 只有后端没给到的项才保留本页的解析结果。
 */
function applySignal(state, sig) {
	if (!sig || !state.cells.serving) return;
	var s = state.cells.serving;
	if (sig.rsrp != null) s.rsrp = sig.rsrp;
	if (sig.rsrq != null) s.rsrq = sig.rsrq;
	if (sig.sinr != null) s.sinr = sig.sinr;
	/* 后端同样可能是按频点推算出来的频段（band_source 会写明「推算」）。
	 * 这里必须沿用后端自己的来源标注，否则会把推算值当成模组上报值显示。 */
	if (sig.band) {
		s.band = sig.band;
		s.bandGuessed = (typeof sig.band_source === 'string' &&
			sig.band_source.indexOf('推算') >= 0);
	}
	if (sig.rat) s.ratName = sig.rat;
}

/* ----------------------------------------------------------------
 * 渲染总入口：分区块构建，任一区块重建不影响其它区块
 * ---------------------------------------------------------------- */
function renderAll(host, state) {
	while (host.firstChild) host.removeChild(host.firstChild);
	host.appendChild(buildSituation(state, host));
	host.appendChild(buildNeighbors(state, host));
	host.appendChild(buildBandLock(state, host));
	host.appendChild(buildCellLock(state, host));
	host.appendChild(buildRat(state, host));
}

/* ----------------------------------------------------------------
 * 区块一：实时锁定态势与诊断
 * ---------------------------------------------------------------- */
function buildSituation(state, host) {
	var s = state.cells.serving;

	var kpis = [];
	function kpi(label, value, unit, tag) {
		return E('div', {}, [
			E('div', { 'class': 'k' }, label),
			E('div', { 'class': 'v' }, [
				value == null ? '—' : String(value),
				unit ? E('span', { 'class': 'u' }, unit) : '',
				tag || ''
			])
		]);
	}

	var bandText = '—';
	if (s) {
		bandText = s.band ? s.band + (s.bandGuessed ? '（推算）' : '') : '未知';
	}

	kpis.push(kpi('当前驻留制式', s ? s.ratName : '—'));
	kpis.push(kpi('驻留频段', bandText));
	kpis.push(kpi('物理小区标识 PCI', s && s.pci ? s.pci : '—'));
	kpis.push(kpi('频点', s && s.arfcn ? s.arfcn : '—'));

	var rsrpGrade = gradeOf('rsrp', s ? s.rsrp : null);
	var rsrqGrade = gradeOf('rsrq', s ? s.rsrq : null);
	var sinrGrade = gradeOf('sinr', s ? s.sinr : null);
	kpis.push(kpi('信号强度 RSRP', fmtNum(s ? s.rsrp : null, 0), 'dBm',
		E('span', { 'class': 'fm350-net2-tag', 'style': 'color:' + rsrpGrade.color }, rsrpGrade.text)));
	kpis.push(kpi('信号质量 RSRQ', fmtNum(s ? s.rsrq : null, 1), 'dB',
		E('span', { 'class': 'fm350-net2-tag', 'style': 'color:' + rsrqGrade.color }, rsrqGrade.text)));
	kpis.push(kpi('信噪比 SINR', fmtNum(s ? s.sinr : null, 1), 'dB',
		E('span', { 'class': 'fm350-net2-tag', 'style': 'color:' + sinrGrade.color }, sinrGrade.text)));

	var lockText, lockCls;
	if (state.emm.locked) {
		lockText = '已锁定小区';
		lockCls = 'fm350-net2-tag lock';
	} else {
		lockText = '未锁定小区';
		lockCls = 'fm350-net2-tag muted';
	}
	kpis.push(E('div', {}, [
		E('div', { 'class': 'k' }, '锁定状态'),
		E('div', { 'class': 'v' }, [
			state.gtact.modeLabel || '未知',
			E('span', { 'class': lockCls }, lockText)
		])
	]));

	var trend = buildTrend(state);
	var diags = buildDiagnose(state);

	return E('div', { 'class': 'fm350-net2-card' }, [
		E('h3', { 'class': 'fm350-net2-title' }, [ICONS.radar(), '实时锁定态势']),
		E('p', { 'class': 'fm350-net2-sub' },
			'显示模组当前实际驻留的小区与锁定情况。信号数值由模组上报的索引换算而来，' +
			'与状态页同源；未测量的项目显示「—」，不做估算。'),
		E('div', { 'class': 'fm350-net2-kpi' }, kpis),
		E('div', { 'style': 'margin-top:14px' }, [
			E('div', { 'class': 'k', 'style': 'font-size:11px;opacity:.65;margin-bottom:4px' },
				'信号趋势（本次打开页面后的历次采样，最多保留 40 点）'),
			trend
		]),
		E('div', { 'style': 'margin-top:14px' }, [
			E('div', { 'class': 'k', 'style': 'font-size:11px;opacity:.65;margin-bottom:6px' },
				'诊断结论'),
			diags
		])
	]);
}

/* 信号趋势折线：无数据时给出明确占位，不画空图充数 */
function buildTrend(state) {
	var pts = state.history.filter(function(h) { return h.rsrp != null; });
	if (pts.length < 2) {
		return E('div', { 'class': 'fm350-net2-empty' },
			'至少需要两次采样才能绘制趋势，请先执行「立即扫描」。');
	}
	var show = pts.slice(-40);
	var W = 100, H = 30;
	// RSRP 常见区间 -120 ~ -60 dBm，超出即截断到边界
	function yOf(v) {
		var t = (v + 120) / 60;
		t = Math.max(0, Math.min(1, t));
		return H - t * H;
	}
	// SINR 必须单独用量程（0 ~ 30 dB）：
	// 若与 RSRP 共用 -120~-60 的映射，12 dB 这类正常值会被截断贴到顶线，
	// 曲线变成一条直线，与“按 0–30 dB 缩放”的说明不符。
	function yOfSinr(v) {
		var t = v / 30;
		t = Math.max(0, Math.min(1, t));
		return H - t * H;
	}
	function pathOf(key) {
		var f = (key === 'sinr') ? yOfSinr : yOf;
		var d = '';
		for (var i = 0; i < show.length; i++) {
			var v = show[i][key];
			if (v == null) continue;
			var x = (i / (show.length - 1)) * W;
			d += (d ? ' L' : 'M') + x.toFixed(2) + ' ' + f(v).toFixed(2);
		}
		return d;
	}
	var svg = '<svg class="fm350-net2-trend" viewBox="0 0 ' + W + ' ' + H + '" ' +
		'preserveAspectRatio="none">' +
		'<line x1="0" y1="' + yOf(-85) + '" x2="' + W + '" y2="' + yOf(-85) +
		'" stroke="#2e7d32" stroke-opacity=".25" stroke-width=".4" stroke-dasharray="2 2"/>' +
		'<line x1="0" y1="' + yOf(-105) + '" x2="' + W + '" y2="' + yOf(-105) +
		'" stroke="#ef6c00" stroke-opacity=".25" stroke-width=".4" stroke-dasharray="2 2"/>' +
		'<path d="' + pathOf('rsrp') + '" fill="none" stroke="#1565c0" stroke-width=".8"/>' +
		'<path d="' + pathOf('sinr') + '" fill="none" stroke="#7cb342" stroke-width=".6" ' +
		'stroke-opacity=".8"/>' +
		'</svg>';
	return E('div', {}, [
		parseSvg(svg),
		E('div', { 'class': 'fm350-net2-note' },
			'蓝线为信号强度 RSRP（越高越好），绿线为信噪比 SINR（按 0–30 dB 缩放显示）。' +
			'虚线为「优」与「中」的参考线。')
	]);
}

/* 诊断结论：全部基于本页已采到的真实数据，不做无依据的猜测 */
function buildDiagnose(state) {
	var s = state.cells.serving;
	var list = [];

	if (!s) {
		list.push(E('div', { 'class': 'fm350-net2-diag' },
			'未能读到驻留小区信息，无法给出诊断。可先执行一次「立即扫描」。'));
		return E('div', {}, list);
	}

	/* 1. 弱覆盖 */
	if (s.rsrp != null) {
		if (s.rsrp < -110) {
			list.push(E('div', { 'class': 'fm350-net2-diag danger' },
				'弱覆盖：当前信号强度 ' + fmtNum(s.rsrp, 0) + ' dBm 已低于 -110 dBm，' +
				'可能出现上网慢、掉线。建议调整设备位置或改用外接天线。'));
		} else if (s.rsrp < -100) {
			list.push(E('div', { 'class': 'fm350-net2-diag warn' },
				'覆盖偏弱：信号强度 ' + fmtNum(s.rsrp, 0) + ' dBm。' +
				'可用，但在边缘位置可能不稳定。'));
		} else {
			list.push(E('div', { 'class': 'fm350-net2-diag ok' },
				'覆盖正常：信号强度 ' + fmtNum(s.rsrp, 0) + ' dBm。'));
		}
	} else {
		list.push(E('div', { 'class': 'fm350-net2-diag' },
			'信号强度未测量，跳过覆盖相关判断。'));
	}

	/* 2. 同频干扰：同频点邻区信号接近服务小区
	 * 必须区分两种「看不出来」：真的没有同频邻区，还是有但模组没测到信号。
	 * 合并成一句「未检测到」会让人误以为现场不存在同频邻区。 */
	var coAll = state.cells.neighbors.filter(function(n) {
		return n.arfcn && s.arfcn && n.arfcn === s.arfcn;
	});
	if (!coAll.length) {
		list.push(E('div', { 'class': 'fm350-net2-diag' },
			'未检测到同频点邻区，无同频干扰迹象。'));
	} else {
		var co = coAll.filter(function(n) { return n.rsrp != null; });
		if (!co.length) {
			list.push(E('div', { 'class': 'fm350-net2-diag' },
				'检测到 ' + coAll.length + ' 个与当前小区同频点的邻区，' +
				'但模组未上报它们的信号强度，因此无法判断是否构成干扰。'));
		} else {
			var strong = co.filter(function(n) {
				return s.rsrp != null && n.rsrp > s.rsrp - 6;
			});
			if (strong.length) {
				list.push(E('div', { 'class': 'fm350-net2-diag warn' },
					'疑似同频干扰：检测到 ' + strong.length + ' 个与当前小区同频点的邻区，' +
					'且信号与当前小区相差不足 6 dB' +
					'（最强 ' + fmtNum(strong[0].rsrp, 0) + ' dBm，PCI ' + (strong[0].pci || '—') + '）。' +
					'这类情况下速率可能受影响，可尝试锁定到信号更干净的小区。'));
			} else {
				list.push(E('div', { 'class': 'fm350-net2-diag ok' },
					'同频情况正常：' + co.length + ' 个同频邻区信号均明显弱于当前小区' +
					'（另有 ' + (coAll.length - co.length) + ' 个同频邻区未测量）。'));
			}
		}
	}

	/* 3. 频繁重选：基于本次会话内 PCI 变化次数 */
	if (state.pciChanges >= 2) {
		list.push(E('div', { 'class': 'fm350-net2-diag warn' },
			'小区切换偏频繁：本次页面打开期间驻留小区已变更 ' + state.pciChanges +
			' 次。若持续发生，可尝试锁定到信号最稳定的小区。'));
	} else {
		list.push(E('div', { 'class': 'fm350-net2-diag ok' },
			'驻留稳定：本次页面打开期间未观察到频繁的小区切换。'));
	}

	/* 4. 锁定状态提示 */
	if (state.emm.locked) {
		list.push(E('div', { 'class': 'fm350-net2-diag warn' },
			'当前处于锁定小区状态。若移动到该小区覆盖之外，可能导致无法上网，' +
			'必要时请点击下方的「解除小区锁定」。'));
	}
	if (state.gtact.mode != null && state.gtact.mode !== 20) {
		list.push(E('div', { 'class': 'fm350-net2-diag warn' },
			'当前制式被限定为「' + state.gtact.modeLabel + '」，' +
			'不在该制式覆盖范围内时会脱网。'));
	}

	/* 5. 载波聚合能力说明（如实告知不可用） */
	if (!state.ca || state.ca.trim() === '' || state.ca.trim() === 'OK') {
		list.push(E('div', { 'class': 'fm350-net2-diag' },
			'载波聚合信息：本模组未上报聚合载波数据，因此不展示该项。'));
	}

	return E('div', {}, list);
}

/* ----------------------------------------------------------------
 * 区块二：邻区扫描
 * ---------------------------------------------------------------- */
function buildNeighbors(state, host) {
	var scanBtn = E('button', {
		'class': 'btn cbi-button-apply',
		'click': function() { doScan(state, host); }
	}, state.scanning ? '扫描中…' : '立即扫描');

	var autoSel = E('select', {}, [
		E('option', { 'value': '0' }, '不自动扫描'),
		E('option', { 'value': '30000' }, '每 30 秒'),
		E('option', { 'value': '60000' }, '每 1 分钟'),
		E('option', { 'value': '300000' }, '每 5 分钟')
	]);
	/* 重建后必须把选中态写回，否则下拉显示「不自动扫描」而定时器仍在跑。 */
	autoSel.value = String(state.intervalMs || 0);
	autoSel.addEventListener('change', function(ev) {
		var v = parseInt(ev.target.value, 10) || 0;
		setAutoScan(state, host, v);
	});

	var ratSel = E('select', {}, [
		E('option', { 'value': '' }, '全部制式'),
		E('option', { 'value': '9' }, '5G NR'),
		E('option', { 'value': '4' }, '4G LTE'),
		E('option', { 'value': '2' }, '3G WCDMA')
	]);
	ratSel.value = state.filterRat || '';
	ratSel.addEventListener('change', function(ev) {
		state.filterRat = ev.target.value;
		refreshTable(state, host);
	});

	var minSel = E('select', {}, [
		E('option', { 'value': '' }, '不限信号强度'),
		E('option', { 'value': '-85' }, '不弱于 -85 dBm（优）'),
		E('option', { 'value': '-95' }, '不弱于 -95 dBm（良）'),
		E('option', { 'value': '-105' }, '不弱于 -105 dBm（中）')
	]);
	minSel.value = (state.filterMinRsrp == null) ? '' : String(state.filterMinRsrp);
	minSel.addEventListener('change', function(ev) {
		state.filterMinRsrp = ev.target.value === '' ? null : parseInt(ev.target.value, 10);
		refreshTable(state, host);
	});

	var tbody = E('tbody');
	var table = E('table', { 'class': 'fm350-net2-table' }, [
		E('thead', {}, [E('tr', {}, [
			E('th', {}, '类型'),
			E('th', {}, '制式'),
			E('th', {}, '频段'),
			E('th', { 'data-key': 'arfcn' }, '频点'),
			E('th', { 'data-key': 'pci' }, 'PCI'),
			E('th', {}, '小区标识'),
			E('th', { 'data-key': 'rsrp' }, '信号强度'),
			E('th', { 'data-key': 'rsrq' }, '信号质量'),
			E('th', { 'data-key': 'sinr' }, '信噪比'),
			E('th', {}, '操作')
		])]),
		tbody
	]);

	/* 表头排序：点击切换升降序 */
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

	var card = E('div', { 'class': 'fm350-net2-card' }, [
		E('h3', { 'class': 'fm350-net2-title' }, [ICONS.radar(), '邻区扫描']),
		E('p', { 'class': 'fm350-net2-sub' },
			'列出模组当前能收到的周边基站。信号越强越靠前；' +
			'点击表头可按该列排序。标记「未测量」的项目是模组没有上报，不是零值。'),
		E('div', { 'class': 'fm350-net2-row', 'style': 'margin-bottom:10px' }, [
			scanBtn,
			E('span', { 'style': 'font-size:12px' }, '自动刷新：'),
			autoSel,
			E('span', { 'style': 'font-size:12px' }, '制式：'),
			ratSel,
			E('span', { 'style': 'font-size:12px' }, '信号：'),
			minSel
		]),
		table,
		E('div', { 'class': 'fm350-net2-note' },
			'说明：扫描通过查询模组的小区信息完成，过程不影响正在进行的网络连接。' +
			'邻区数量与现场环境有关，通常能见到数个到十余个。' +
			'模组不上报邻区的信噪比，因此该列对邻区显示「未测量」，只有当前小区有值。')
	]);

	refreshTable(state, host);
	return card;
}

/* 邻区表格数据填充（排序 / 筛选在此统一处理） */
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
		list = list.filter(function(c) {
			return c.rsrp != null && c.rsrp >= state.filterMinRsrp;
		});
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
		if (va == null) return 1;      // 未测量的永远排在后面
		if (vb == null) return -1;
		if (typeof va === 'string') return state.sortDesc ? vb.localeCompare(va) : va.localeCompare(vb);
		return state.sortDesc ? vb - va : va - vb;
	});

	if (!list.length) {
		tbody.appendChild(E('tr', {}, [
			E('td', { 'colspan': '10', 'class': 'fm350-net2-empty' },
				'没有符合条件的小区。可调整筛选条件，或点击「立即扫描」重新获取。')
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
				'class': 'btn cbi-button-action',
				'click': function() { quickLockPci(state, host, c); }
			}, '锁定此小区');
		} else if (c.serving) {
			action = E('span', { 'style': 'font-size:11px;opacity:.6' }, '当前驻留');
		}

		tbody.appendChild(E('tr', { 'class': cls.join(' ') }, [
			E('td', {}, c.serving ? '当前小区' : '邻区'),
			E('td', {}, c.ratName),
			E('td', {}, c.band ? (c.band + (c.bandGuessed ? '（推算）' : '')) : '—'),
			E('td', {}, c.arfcn || '—'),
			E('td', {}, c.pci || '—'),
			E('td', {}, (c.cellId && c.cellId.indexOf('FFFF') < 0) ? c.cellId : '—'),
			E('td', {}, [signalBar(g), ' ', fmtNum(c.rsrp, 0, 'dBm')]),
			E('td', {}, [signalBar(gq), ' ', fmtNum(c.rsrq, 1, 'dB')]),
			E('td', {}, [signalBar(gs), ' ', fmtNum(c.sinr, 1, 'dB')]),
			E('td', {}, action || '')
		]));
	});
}

/* 手动扫描：拉最新小区信息，同时更新趋势与重选统计 */
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
		ui.addNotification(null, E('p', '扫描失败：' + (e && e.message ? e.message : '未知错误')), 'danger');
	});
}

/* 定时扫描：节点被移除（页面切走）时自动停止，避免后台空转 */
function setAutoScan(state, host, ms) {
	if (state.timer) { clearTimeout(state.timer); state.timer = null; }
	state.intervalMs = ms;
	if (!ms) return;
	/* 存活判断必须挂在稳定的 host 上：
	 * renderAll() 会重建全部子节点，tbody 的旧引用在第一次扫描后就不在文档里了，
	 * 拿它判存活会导致自动扫描跑一轮就静默停止。 */
	var alive = function() {
		return !!(state.host && document.body.contains(state.host));
	};
	var tick = function() {
		if (!alive()) { state.timer = null; return; }
		// 等本轮真正结束再排下一次，避免扫描耗时超过间隔时并发下发 AT
		Promise.resolve(doScan(state, host)).then(function() {
			if (!alive()) { state.timer = null; return; }
			state.timer = setTimeout(tick, ms);
		});
	};
	state.timer = setTimeout(tick, ms);
}

/* ----------------------------------------------------------------
 * 区块三：锁频段
 * ---------------------------------------------------------------- */
function buildBandLock(state, host) {
	var supported = state.gtact.bands || [];

	var groups = { lte: [], nr: [], other: [] };
	var seen = {};
	supported.forEach(function(b) {
		if (bandGroup(b) === 'other') { groups.other.push(b); return; }
		var name = bandName(b);
		/* 同一个 5G 频段在模组里同时存在 141 / 5041 等三套编码，按名称去重，
		 * 否则界面会出现三张都写着 n41 的卡片。 */
		if (seen[name]) return;
		seen[name] = true;
		groups[bandGroup(b)].push(b);
	});
	/* 展示用：去重后的全部频段名 */
	var allNames = groups.nr.map(bandName).concat(groups.lte.map(bandName));

	function chipRow(list) {
		if (!list.length) return E('div', { 'class': 'fm350-net2-note' }, '本模组未上报该制式的频段。');
		return E('div', { 'class': 'fm350-net2-row' }, list.map(function(b) {
			var on = !!state.selectedBands[b];
			return E('span', {
				'class': 'fm350-net2-chip' + (on ? ' on' : ''),
				'click': function() {
					if (state.selectedBands[b]) delete state.selectedBands[b];
					else state.selectedBands[b] = true;
					renderAll(host, state);
				},
				'title': '频段编号 ' + b
			}, bandName(b));
		}));
	}

	var selCount = Object.keys(state.selectedBands).length;

	/* 模组上报里可能存在无法映射为频段的编号，如实列出并说明已忽略，
	 * 不把它们编造成 B101 一类的频段名误导用户。 */
	function otherNote() {
		if (!groups.other.length) return null;
		return E('div', { 'class': 'fm350-net2-note', 'style': 'margin-top:10px' },
			'模组还上报了以下无法确认为频段编号的条目，已忽略：' + groups.other.join('、'));
	}

	var applyBtn = E('button', {
		'class': 'btn cbi-button-apply',
		'click': function() {
			var codes = Object.keys(state.selectedBands).map(Number);
			if (!codes.length) {
				ui.addNotification(null, E('p', '请至少选择一个频段'), 'warning');
				return;
			}
			var args = '20,6,3,' + codes.join(',');
			applyLockBand(state, host, args,
				'即将把可用频段限定为所选的 ' + codes.length + ' 个频段');
		}
	}, '应用所选频段');

	var presetRow = E('div', { 'class': 'fm350-net2-row' },
		BAND_PRESETS.map(function(p) {
			return E('span', {
				'class': 'fm350-net2-chip preset',
				'title': p.hint,
				'click': function() {
					if (p.id === 'auto') {
						applyLockBand(state, host, p.args, '即将恢复为默认（不锁频段）');
						return;
					}
					// 预设直接下发，避免用户逐个勾选
					applyLockBand(state, host, p.args, '即将应用预设「' + p.label + '」：' + p.hint);
				}
			}, p.label);
		})
	);

	var current = allNames.length
		? allNames.slice(0, 24).join('、') +
			(allNames.length > 24 ? ' 等 ' + allNames.length + ' 个' : '')
		: '模组未上报';

	return E('div', { 'class': 'fm350-net2-card' }, [
		E('h3', { 'class': 'fm350-net2-title' }, [ICONS.band(), '锁频段']),
		E('p', { 'class': 'fm350-net2-sub' },
			'限定模组只在选定的频段上工作。常用于固定场所优化，或排查某个频段的异常。' +
			'不确定时请保持「恢复默认」。'),
		E('div', {}, [
			E('div', { 'class': 'k', 'style': 'font-size:11px;opacity:.65;margin-bottom:6px' },
				'当前生效：' + (state.gtact.modeLabel || '未知')),
			E('div', { 'class': 'fm350-net2-note', 'style': 'margin-top:0' }, '可用频段：' + current)
		]),
		E('div', { 'style': 'margin-top:12px' }, [
			E('div', { 'class': 'fm350-net2-group' }, '常用组合（点击直接应用）'),
			presetRow
		]),
		E('div', { 'style': 'margin-top:14px' }, [
			E('div', { 'class': 'fm350-net2-group' }, '手动选择：5G 频段'),
			chipRow(groups.nr)
		]),
		E('div', { 'style': 'margin-top:10px' }, [
			E('div', { 'class': 'fm350-net2-group' }, '手动选择：4G 频段'),
			chipRow(groups.lte)
		]),
		E('div', { 'class': 'fm350-net2-row', 'style': 'margin-top:14px' }, [
			applyBtn,
			E('button', {
				'class': 'btn',
				'click': function() {
					state.selectedBands = {};
					renderAll(host, state);
				}
			}, '清空选择'),
			E('span', { 'style': 'font-size:12px;opacity:.7' }, '已选 ' + selCount + ' 个')
		]),
		otherNote() || '',
		E('div', { 'class': 'fm350-net2-note' },
			'提示：下方只列出模组当前已启用的频段，避免填入无效编号导致模组报错。' +
			'频段名称以 n 开头为 5G，以 B 开头为 4G。')
	]);
}

/* ----------------------------------------------------------------
 * 区块四：锁小区与 PCI
 * ---------------------------------------------------------------- */
function buildCellLock(state, host) {
	var s = state.cells.serving;

	/* 首次进入时用当前驻留小区预填；此后以用户实际输入为准，
	 * 否则一次自动扫描触发的整页重建就会把正在编辑的内容清空。 */
	if (!state.cellForm) {
		state.cellForm = {
			pci: s ? (s.pci || '') : '',
			arfcn: s ? (s.arfcn || '') : ''
		};
	}

	var pciInput = E('input', { 'type': 'text', 'class': 'cbi-input-text', 'style': 'width:110px' });
	var arfcnInput = E('input', { 'type': 'text', 'class': 'cbi-input-text', 'style': 'width:130px' });
	pciInput.value = state.cellForm.pci;
	arfcnInput.value = state.cellForm.arfcn;
	pciInput.addEventListener('input', function() { state.cellForm.pci = pciInput.value; });
	arfcnInput.addEventListener('input', function() { state.cellForm.arfcn = arfcnInput.value; });

	var unlockBtn = E('button', {
		'class': 'btn',
		'click': function() {
			applyLockCell(state, host, '0', '即将解除小区锁定，恢复由模组自行选择小区');
		}
	}, '解除小区锁定');

	return E('div', { 'class': 'fm350-net2-card' }, [
		E('h3', { 'class': 'fm350-net2-title' }, [ICONS.target(), '锁小区']),
		E('p', { 'class': 'fm350-net2-sub' },
			'把模组固定在指定的小区上，避免它在信号相近的小区之间来回切换。' +
			'仅建议在信号稳定、位置固定的场景使用。'),
		E('div', { 'class': 'fm350-net2-row' }, [
			E('span', { 'style': 'font-size:12px' }, '物理小区标识 PCI：'),
			pciInput,
			E('span', { 'style': 'font-size:12px' }, '频点：'),
			arfcnInput,
			E('button', {
				'class': 'btn cbi-button-apply',
				'click': function() {
					var pci = (pciInput.value || '').trim();
					var arfcn = (arfcnInput.value || '').trim();
					var err = validateCell(pci, arfcn, state);
					if (err) {
						ui.addNotification(null, E('p', err), 'warning');
						return;
					}
					// 参数顺序由模组指令约定：启用位, 制式位, 保留位, 频点, PCI, 频段指示
					var args = '1,11,0,' + arfcn + ',' + pci + ',3';
					applyLockCell(state, host, args,
						'即将把模组锁定到频点 ' + arfcn + '、PCI ' + pci + ' 的小区');
				}
			}, '锁定'),
			unlockBtn
		]),
		E('div', { 'style': 'margin-top:10px' }, [
			state.emm.locked
				? E('span', { 'class': 'fm350-net2-tag lock' }, '当前：已锁定小区')
				: E('span', { 'class': 'fm350-net2-tag muted' }, '当前：未锁定小区')
		]),
		E('div', { 'class': 'fm350-net2-note' },
			'输入项已预填当前驻留小区的值。也可以在下方的邻区列表中直接点击「锁定此小区」。' +
			'锁定后若移动到该小区覆盖范围外，将无法上网，届时需要解除锁定。')
	]);
}

/* 锁小区参数校验：把无效输入挡在前端，避免下发后模组报错 */
function validateCell(pci, arfcn, state) {
	if (!pci) return '请填写物理小区标识 PCI';
	if (!arfcn) return '请填写频点';
	if (!/^\d+$/.test(pci)) return 'PCI 只能是数字';
	if (!/^\d+$/.test(arfcn)) return '频点只能是数字';
	var pNum = parseInt(pci, 10);
	if (pNum < 0 || pNum > 1007) return 'PCI 应在 0 到 1007 之间';
	var aNum = parseInt(arfcn, 10);
	if (aNum <= 0) return '频点必须为正整数';

	/* 冲突提示：该频点 + PCI 组合不在已扫到的邻区里 */
	var all = (state.cells.neighbors || []).concat(state.cells.serving ? [state.cells.serving] : []);
	var hit = all.filter(function(c) {
		return String(c.pci) === String(pNum) && String(c.arfcn) === String(aNum);
	});
	if (!hit.length) {
		return '当前扫描结果中没有频点 ' + arfcn + '、PCI ' + pci +
			' 的小区。继续锁定可能导致无法上网，请确认后重试，或先执行一次扫描。';
	}
	return null;
}

/* ----------------------------------------------------------------
 * 区块五：制式与优先级
 * ---------------------------------------------------------------- */
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
				applyRat(state, host, k, '即将把网络制式设为「' + info.label + '」：' + info.desc);
			}
		}, info.label);
	});

	return E('div', { 'class': 'fm350-net2-card' }, [
		E('h3', { 'class': 'fm350-net2-title' }, [ICONS.gear(), '网络制式']),
		E('p', { 'class': 'fm350-net2-sub' },
			'选择模组允许使用的网络制式。设置后会立即下发并生效。'),
		E('div', { 'class': 'fm350-net2-row' }, modeBtns),
		E('div', { 'style': 'margin-top:10px' }, [
			cur != null
				? E('span', { 'class': 'fm350-net2-tag lock' }, '当前：' + (state.gtact.modeLabel || '未知'))
				: E('span', { 'class': 'fm350-net2-tag muted' }, '当前状态未知')
		]),
		E('div', { 'class': 'fm350-net2-note' },
			'关于「优先级排序」：本模组未提供制式优先级的查询与设置能力' +
			'（相应查询返回不支持），因此本页面不提供排序功能，避免给出点了没有实际效果的选项。' +
			'如需限定制式，请使用上方的制式选择。')
	]);
}

/* ================================================================
 * 八、操作下发（统一：二次确认 → 执行 → 反馈 → 刷新）
 * ================================================================ */

function applyOp(state, host, dangerText, runFn, okMsg) {
	return confirmDanger('请确认操作',
		dangerText + '。执行期间蜂窝网络会短暂中断并自动重连，通常需要数十秒。是否继续？',
		'确认执行').then(function(yes) {
		if (!yes) return null;
		ui.addNotification(null, E('p', '正在下发设置，请稍候…'), 'info');
		return runFn().then(function(res) {
			api.notify(res, okMsg);
			// 无论成功失败都刷新一次，让用户看到真实结果
			return doScan(state, host);
		});
	});
}

function applyLockBand(state, host, args, desc) {
	return applyOp(state, host, desc, function() {
		return api.lockBand(args);
	}, '频段设置已下发');
}

function applyLockCell(state, host, args, desc) {
	return applyOp(state, host, desc, function() {
		return api.lockCell(args);
	}, '小区锁定设置已下发');
}

function applyRat(state, host, mode, desc) {
	return applyOp(state, host, desc, function() {
		return api.lockBand(String(mode));
	}, '制式设置已下发');
}

function quickLockPci(state, host, cell) {
	var desc = '即将锁定到频点 ' + cell.arfcn + '、PCI ' + cell.pci +
		' 的小区（信号 ' + fmtNum(cell.rsrp, 0, 'dBm') + '）';
	var args = '1,11,0,' + cell.arfcn + ',' + cell.pci + ',3';
	return applyLockCell(state, host, args, desc);
}
