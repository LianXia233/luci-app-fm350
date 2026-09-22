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
 *    是同一个量的重复上报，非信噪比。信噪比严格取倒数第 4 位的原始 dB 值；
 *    邻区行无此字段，一律显示「未测量」。
 *
 * 3. 索引 126 / 127 / 255 是「未测量」哨兵值，一律标记为未测量，严禁误算。
 *
 * 4. 载波聚合 AT+GTCAINFO? 在本模组上只回 OK，无数据 —— 面板如实标注「不可用」。
 *
 * 5. 制式选择经由 AT+GTACT 的 mode 统一控制；制式优先级模组不支持，不做虚假按键。
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

/* 3GPP 阶梯索引 → 物理量。idx <= 0 或达到上限哨兵时返回 null（未测量）。 */
function idxToLinear(idx, base, step, maxIdx) {
	if (idx == null) return null;
	var n = parseInt(idx, 10);
	if (!isFinite(n) || n <= 0) return null;
	if (maxIdx != null && n > maxIdx) return null;
	return base + (n - 1) * step;
}

/* 信号强度 RSRP：索引 1 → -156 dBm，步长 1 dB；126 及以上为哨兵 */
function rsrpFromIdx(idx, rat) {
	if (rat === 4) return idxToLinear(idx, -140, 1, 97);      // LTE（-140 ~ -44 dBm）
	return idxToLinear(idx, -156, 1, 125);                     // NR（-156 ~ -32 dBm）
}

/* 信号质量 RSRQ：索引 1 → -43 dB，步长 0.5 dB；126 及以上为哨兵 */
function rsrqFromIdx(idx, rat) {
	if (rat === 4) return idxToLinear(idx, -19.5, 0.5, 34);    // LTE（-19.5 ~ -3 dB）
	return idxToLinear(idx, -43, 0.5, 125);                    // NR（-43 ~ 20 dB）
}

/* 信噪比 SINR：模组直接上报 dB 原值（-100 ~ 100） */
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

var GTACT_MODE = {
	2: { label: '仅 4G（LTE）', desc: '仅驻留 4G 网络，5G 不可用' },
	14: { label: '仅 5G（NR）', desc: '仅驻留 5G 网络，无 5G 覆盖时脱网' },
	20: { label: '自动（推荐）', desc: '基站与模组自协商最佳连接' }
};

/* 频段编码 -> 频段名。
 *
 * 模组在 AT+GTACT / AT+GTCCINFO 里上报的是**编码**而非频段名，且同一个数字
 * 在两种制式下含义不同，必须按制式选表：
 *   LTE：`100 + n`（101 = B1 …… 171 = B71），另有 band 号直写（3 = B3）；
 *   NR ：数值加法三档（5000 + n / 500 + n / 100 + n），
 *        以及 `50` 前缀拼接（50512 = n512）。
 * rat 省略时按 NR 优先解析，与本次新增前的行为一致，不影响锁频段 UI。
 */
function bandName(code, rat) {
	var n = parseInt(code, 10);
	if (!isFinite(n)) return 'B' + code;
	if (rat === 4) {
		if (n > 100 && n - 100 <= 88) return 'B' + (n - 100);
		if (n >= 1 && n <= 88) return 'B' + n;
		return '编号 ' + n;
	}
	if (n >= 5000 && n <= 5079) return 'n' + (n - 5000);
	if (n >= 500 && n <= 599) return 'n' + (n - 500);
	if (n >= 101 && n <= 199) return 'n' + (n - 100);
	if (n >= 1 && n <= 88) return 'B' + n;
	return '编号 ' + n;
}

function bandGroup(code) {
	var n = parseInt(code, 10);
	if (!isFinite(n)) return 'other';
	if (n >= 5000 && n <= 5079) return 'nr';
	if (n >= 500 && n <= 599) return 'nr';
	if (n >= 101 && n <= 199) return 'nr';
	if (n >= 1 && n <= 88) return 'lte';
	return 'other';
}

var BAND_PRESETS = [
	{ id: 'auto', label: '恢复默认（全频段）', hint: '解除频段限制，由模组自适应', args: '20' },
	{ id: 'nr_main', label: '5G 主力频段', hint: 'n41 / n78 / n79，覆盖国内 5G 核心主频', args: '20,5041,5078,5079' },
	{ id: 'nr_all', label: '5G 全频段', hint: '锁定模组支持的全部 5G 频段', args: '20,5020,5025,5028,5030,5038,5040,5041,5048,5066,5071,5077,5078,5079' },
	{ id: 'lte_main', label: '4G 主力频段', hint: 'B1 / B3 / B5 / B8 国内主流频段', args: '20,1,3,5,8' },
	{ id: 'mixed', label: '5G + 4G 黄金组合', hint: '兼顾速率与基站回落覆盖', args: '20,1,3,5,8,5041,5078,5079' }
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
	var freq = n * 0.005;
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
		cell.sinr = sinrFromRaw(sinrRaw);
		if (!cell.band) {
			var guess = arfcnToBandLabel(cell.arfcn, rat);
			cell.band = guess || '';
			cell.bandGuessed = !!guess;
		} else {
			/* 模组上报的是编码：必须解码后再展示，否则同表内会出现
			 * 「5041」（上报，未解码）与「n41 (推算)」两种写法并存。 */
			cell.band = bandName(cell.band, rat);
			cell.bandGuessed = false;
		}
		if (isServing) out.serving = cell;
		else out.neighbors.push(cell);
	}
	return out;
}

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
	for (var i = 1; i < parts.length; i++) {
		var v = parts[i].trim();
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
		kpiBox('小区锁定状态', state.gtact.modeLabel || '未知', null,
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
			E('td', {}, c.serving ? E('span', { 'class': 'fm350-badge ok' }, '服务小区') : E('span', { 'class': 'fm350-badge muted' }, '邻近候选')),
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
	var supported = state.gtact.bands || [];
	var groups = { lte: [], nr: [], other: [] };
	var seen = {};

	supported.forEach(function(b) {
		if (bandGroup(b) === 'other') { groups.other.push(b); return; }
		var name = bandName(b);
		if (seen[name]) return;
		seen[name] = true;
		groups[bandGroup(b)].push(b);
	});

	function chipRow(list, isNr) {
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
				E('i', { 'class': 'fm350-dot', 'style': 'background:' + (on ? '#fff' : (isNr ? '#7c3aed' : '#0284c7')) }),
				bandName(b)
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
			E('span', { 'class': 'fm350-badge lock' }, '当前：' + (state.gtact.modeLabel || '未知'))
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'强行约束射频只在指定载波频段驻留。用于避开拥堵基站或锁定专属大带宽频段。'),
		E('div', { 'style': 'margin-bottom:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:var(--fm-text-muted)' }, '常用一键预设:'),
			presetRow
		]),
		E('div', { 'style': 'margin-top:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:#7c3aed' }, '5G NR 频段矩阵:'),
			chipRow(groups.nr, true)
		]),
		E('div', { 'style': 'margin-top:14px;' }, [
			E('div', { 'style': 'font-size:12px;font-weight:600;margin-bottom:8px;color:#0284c7' }, '4G LTE 频段矩阵:'),
			chipRow(groups.lte, false)
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
			E('span', { 'class': 'fm350-badge ok' }, state.gtact.modeLabel || '自适应')
		]),
		E('p', { 'class': 'fm350-net2-sub' },
			'指定模组与基站交互的协议世代。设定将写入模组 NVRAM 实时生效。'),
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