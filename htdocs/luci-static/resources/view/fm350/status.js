'use strict';
'require view';
'require ui';
'require fm350.api as api';

/*
 * luci-app-fm350 —— 状态总览
 * 数据全部来自 Rust 后端 fm350d 实时上报。
 */

function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

/* ---------------- 动态 SVG 图标库 ---------------- */
var SVG_ICONS = {
	// 动态升腾热浪测温计
	thermometer: function() {
		return parseSvg(`
			<svg class="fm350-svg-thermo" viewBox="0 0 24 24" fill="none" stroke="currentColor">
				<!-- 顶部交替升腾的热浪微气流 -->
				<path class="fm350-anim-heat-wave fm350-hw-1" d="M9 3.5c-.8.8-.8 1.8 0 2.5" stroke-width="1.6" stroke-linecap="round"/>
				<path class="fm350-anim-heat-wave fm350-hw-2" d="M12 2c-.8.9-.8 2 0 2.8" stroke-width="1.6" stroke-linecap="round"/>
				<path class="fm350-anim-heat-wave fm350-hw-3" d="M15 3.5c-.8.8-.8 1.8 0 2.5" stroke-width="1.6" stroke-linecap="round"/>
				<!-- 玻璃管本体 -->
				<path d="M10 7.5v8.13a4 4 0 1 0 4 0V7.5a2 2 0 0 0-4 0z" stroke-width="1.8" stroke-linejoin="round"/>
				<!-- 液柱与底球 -->
				<path d="M12 11v4.5" stroke-width="2" stroke-linecap="round"/>
				<circle class="fm350-anim-thermo-core" cx="12" cy="18" r="2.2" fill="currentColor"/>
			</svg>
		`);
	},
	// 5G 信号塔
	tower: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 20V10M9 20l3-10 3 10M7 16h10" stroke-linecap="round" stroke-linejoin="round"/>
				<circle cx="12" cy="7" r="2" fill="currentColor"/>
				<circle class="fm350-anim-wave fm350-anim-wave-1" cx="12" cy="7" r="4" stroke="currentColor" stroke-width="1.2" fill="none"/>
				<circle class="fm350-anim-wave fm350-anim-wave-2" cx="12" cy="7" r="7" stroke="currentColor" stroke-width="1" stroke-dasharray="2 2" fill="none"/>
			</svg>
		`);
	},
	// 雷达旋转扫描
	radar: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<circle cx="12" cy="12" r="9" stroke-opacity="0.3"/>
				<circle cx="12" cy="12" r="5" stroke-opacity="0.5"/>
				<circle cx="12" cy="12" r="1.5" fill="currentColor"/>
				<line class="fm350-anim-spin" x1="12" y1="12" x2="12" y2="3" stroke="currentColor" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 芯片硬件
	chip: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<rect x="5" y="5" width="14" height="14" rx="2.5"/>
				<rect class="fm350-anim-pulse-subtle" x="8.5" y="8.5" width="7" height="7" rx="1.5" fill="currentColor" fill-opacity="0.15"/>
				<path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 逐路传感器专用迷你芯片图标
	sensorMini: function(isHot) {
		return parseSvg(`
			<svg class="fm350-svg-sensor-mini ${isHot ? 'fm350-anim-hot-shake' : ''}" viewBox="0 0 20 20" fill="none" stroke="currentColor">
				<rect x="4" y="4" width="12" height="12" rx="2.5" stroke-width="1.5"/>
				<circle cx="10" cy="10" r="${isHot ? 3 : 2}" fill="currentColor" fill-opacity="${isHot ? '0.85' : '0.35'}"/>
				<path d="M7 2v2M13 2v2M7 16v2M13 16v2M2 7h2M2 13h2M16 7h2M16 13h2" stroke-width="1.2" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 网络接口
	ethernet: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<rect x="3" y="4" width="18" height="14" rx="3"/>
				<path d="M7 18v2M17 18v2M9 14v-3M12 14v-3M15 14v-3" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 刷新
	refresh: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon fm350-icon-btn" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.19" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		`);
	}
};

/* ---------------- 基础数据与逻辑 ---------------- */
function bars(csq) {
	var n = parseInt(csq);
	if (isNaN(n) || n < 0 || n >= 99) return 0;
	return Math.min(5, Math.floor(n / 6));
}

function barsFromRsrp(rsrp) {
	var n = parseFloat(rsrp);
	if (isNaN(n)) return 0;
	if (n >= -80) return 5;
	if (n >= -90) return 4;
	if (n >= -100) return 3;
	if (n >= -110) return 2;
	if (n >= -120) return 1;
	return 0;
}

function fmt1(x) {
	if (x == null || x === '') return null;
	var n = parseFloat(x);
	if (isNaN(n)) return '' + x;
	return (Math.round(n * 10) / 10).toFixed(1);
}

function grade(rsrp, sinr) {
	var s = parseFloat(sinr);
	if (!isNaN(s))
		return s >= 20 ? 'good' : (s >= 13 ? 'ok' : (s >= 0 ? 'mid' : 'bad'));
	var r = parseFloat(rsrp);
	if (isNaN(r)) return null;
	return r >= -80 ? 'good' : (r >= -90 ? 'ok' : (r >= -100 ? 'mid' : 'bad'));
}

function gradeText(g) {
	return g === 'good' ? _('优') : (g === 'ok' ? _('良') : (g === 'mid' ? _('中') : (g === 'bad' ? _('差') : null)));
}

function rsrqKind(rsrq) {
	var n = parseFloat(rsrq);
	if (isNaN(n)) return null;
	return n >= -10 ? 'good' : (n >= -15 ? 'ok' : (n >= -19.5 ? 'mid' : 'bad'));
}

function overallKind(score) {
	var n = parseFloat(score);
	if (isNaN(n)) return null;
	return n >= 75 ? 'good' : (n >= 50 ? 'ok' : (n >= 25 ? 'mid' : 'bad'));
}

function v(x, dflt) {
	if (x == null || x === '') return (dflt != null) ? dflt : '-';
	return '' + x;
}

function dot(on) {
	return E('span', { 'class': 'fm350-status-indicator ' + (on ? 'fm350-is-up' : 'fm350-is-down') }, [
		E('span', { 'class': 'fm350-indicator-ping' }),
		E('span', { 'class': 'fm350-indicator-core' })
	]);
}

function signalBars(level) {
	var out = [];
	for (var i = 0; i < 5; i++) {
		var active = i < level;
		out.push(E('span', {
			'class': 'fm350-bar' + (active ? ' fm350-bar-on fm350-bar-lvl-' + level : ''),
			'style': 'height: ' + (25 + i * 18) + '%'
		}));
	}
	return E('div', { 'class': 'fm350-bars-wrap' }, out);
}

function tag(text, kind) {
	if (!text) return null;
	return E('span', { 'class': 'fm350-pill-tag ' + (kind ? 'fm350-tag-' + kind : '') }, text);
}

function card(title, valueNode, subText, iconType) {
	var headerNodes = [
		iconType && SVG_ICONS[iconType] ? E('div', { 'class': 'fm350-card-icon-box' }, SVG_ICONS[iconType]()) : '',
		E('span', { 'class': 'fm350-card-title' }, title)
	];
	return E('div', { 'class': 'fm350-glass-card' }, [
		E('div', { 'class': 'fm350-card-header' }, headerNodes),
		E('div', { 'class': 'fm350-card-body' }, [
			E('div', { 'class': 'fm350-card-value' }, valueNode),
			subText ? E('div', { 'class': 'fm350-card-sub' }, subText) : ''
		])
	]);
}

function row(key, value) {
	return E('tr', {}, [
		E('td', { 'class': 'fm350-table-key' }, key),
		E('td', { 'class': 'fm350-table-val' }, v(value))
	]);
}

function joinParts(parts) {
	return parts.filter(function(x) { return x && x !== '-'; }).join(' · ');
}

function operatorText(sig) {
	if (!sig.operator_name) return v(sig.operator);
	return sig.operator_name + (sig.operator ? ' (' + sig.operator + ')' : '');
}

/* 传感器元数据映射：代码与人类友好的中文释义 */
function sensorMeta(id) {
	var map = {
		1:  { code: 'soc_max',      name: 'SoC 综合峰值' },
		2:  { code: 'cpu_little0',  name: 'CPU 核心 0' },
		3:  { code: 'cpu_little1',  name: 'CPU 核心 1' },
		4:  { code: 'cpu_little2',  name: 'CPU 核心 2' },
		7:  { code: 'gpu1',         name: 'GPU 渲染引擎' },
		8:  { code: 'dramc',        name: 'DRAM 内存控制' },
		9:  { code: 'mmsys',        name: '多媒体系统' },
		10: { code: 'md_5g',        name: '5G 基带核心' },
		13: { code: 'soc_dram_ntc', name: 'SoC/DRAM 热敏' },
		14: { code: 'ltepa_ntc',    name: '4G LTE 功放 NTC' },
		15: { code: 'nrpa_ntc',     name: '5G NR 功放 NTC' },
		16: { code: 'rf_ntc',       name: '射频芯片 NTC' },
		19: { code: 'pmic',         name: '主电源管理' },
		20: { code: 'pmic_vcore',   name: 'VCore 核心供电' },
		21: { code: 'pmic_vproc',   name: 'VProc 处理器供电' },
		22: { code: 'pmic_vgpu',    name: 'VGPU 显存供电' }
	};
	return map[id] || { code: 'sensor_' + id, name: _('传感器 #') + id };
}

/* 温度梯级判定：低于45℃清凉(cool)，45~60℃正常(normal)，60~72℃温热(warm)，高于72℃极热(hot) */
function getTempLevel(val) {
	var t = parseFloat(val);
	if (isNaN(t)) return 'normal';
	if (t < 45) return 'cool';
	if (t < 60) return 'normal';
	if (t < 72) return 'warm';
	return 'hot';
}

/* ---------------- 全局样式注入 ---------------- */
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

function injectFrostedGlassTheme() {
	ensureViewport();
	var styleId = 'fm350-theme-glass-styles';
	if (document.getElementById(styleId)) return;

	var css = `
		:root {
			--fm-glass-bg: rgba(255, 255, 255, 0.78);
			--fm-glass-bg-hover: rgba(255, 255, 255, 0.94);
			--fm-glass-border: rgba(255, 255, 255, 0.88);
			--fm-glass-shadow: 0 12px 32px 0 rgba(31, 38, 135, 0.05), 0 2px 6px 0 rgba(0, 0, 0, 0.02);
			--fm-glass-blur: blur(16px) saturate(180%);
			--fm-primary: #2563eb;
			--fm-primary-gradient: linear-gradient(135deg, #2563eb 0%, #06b6d4 100%);
			--fm-text-main: #1e293b;
			--fm-text-muted: #64748b;
			--fm-temp-cool: #10b981;
			--fm-temp-normal: #0ea5e9;
			--fm-temp-warm: #f59e0b;
			--fm-temp-hot: #ef4444;
		}

		.fm350-dashboard {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
			color: var(--fm-text-main);
			padding: 4px 0 32px 0;
		}

		/* 顶部 Hero 卡片 */
		.fm350-hero {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.9), rgba(240, 246, 255, 0.78));
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 20px;
			padding: 24px 28px;
			box-shadow: var(--fm-glass-shadow);
			display: flex;
			align-items: center;
			justify-content: space-between;
			flex-wrap: wrap;
			gap: 16px;
			margin-bottom: 24px;
		}
		.fm350-hero-info h2 {
			margin: 0 0 6px 0;
			font-size: 1.55rem;
			font-weight: 700;
			letter-spacing: -0.02em;
			display: flex;
			align-items: center;
			gap: 12px;
		}
		.fm350-hero-desc {
			color: var(--fm-text-muted);
			font-size: 0.88rem;
		}

		/* 基础毛玻璃卡片 */
		.fm350-glass-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 16px;
			padding: 18px 20px;
			box-shadow: var(--fm-glass-shadow);
			transition: all 0.28s cubic-bezier(0.4, 0, 0.2, 1);
			display: flex;
			flex-direction: column;
			justify-content: space-between;
		}
		.fm350-glass-card:hover {
			background: var(--fm-glass-bg-hover);
			transform: translateY(-2px);
			box-shadow: 0 16px 36px 0 rgba(31, 38, 135, 0.08);
			border-color: #fff;
		}
		.fm350-card-header {
			display: flex;
			align-items: center;
			gap: 10px;
			margin-bottom: 12px;
		}
		.fm350-card-icon-box {
			width: 32px;
			height: 32px;
			border-radius: 9px;
			background: rgba(37, 99, 235, 0.08);
			color: var(--fm-primary);
			display: flex;
			align-items: center;
			justify-content: center;
		}
		.fm350-card-title {
			font-size: 0.82rem;
			font-weight: 600;
			text-transform: uppercase;
			letter-spacing: 0.04em;
			color: #64748b;
		}
		.fm350-card-body {
			display: flex;
			flex-direction: column;
			gap: 6px;
		}
		.fm350-card-value {
			font-size: 1.32rem;
			font-weight: 700;
			color: #0f172a;
			display: flex;
			align-items: baseline;
			flex-wrap: wrap;
			gap: 8px;
		}
		.fm350-card-sub {
			font-size: 0.78rem;
			color: #64748b;
			line-height: 1.45;
		}

		/* 布局栅格 */
		.fm350-section-title {
			font-size: 1.05rem;
			font-weight: 700;
			color: #1e293b;
			margin: 28px 0 14px 4px;
			display: flex;
			align-items: center;
			gap: 10px;
		}
		.fm350-grid {
			display: grid;
			grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
			gap: 16px;
		}
		.fm350-grid-wide {
			grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
		}

		/* ---------------- 温度传感器专属毛玻璃看板 ---------------- */
		.fm350-thermal-dashboard {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.85), rgba(248, 250, 252, 0.75));
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 20px;
			padding: 22px 26px;
			box-shadow: var(--fm-glass-shadow);
			margin-bottom: 18px;
			display: flex;
			align-items: center;
			justify-content: space-between;
			flex-wrap: wrap;
			gap: 20px;
		}
		.fm350-thermal-left {
			display: flex;
			align-items: center;
			gap: 18px;
		}
		.fm350-thermo-badge {
			width: 60px;
			height: 60px;
			border-radius: 16px;
			background: linear-gradient(135deg, rgba(239, 68, 68, 0.12), rgba(245, 158, 11, 0.12));
			color: #ef4444;
			display: flex;
			align-items: center;
			justify-content: center;
			box-shadow: 0 4px 12px rgba(239, 68, 68, 0.1);
		}
		.fm350-thermo-badge.level-cool   { background: rgba(16, 185, 129, 0.12); color: #10b981; box-shadow: 0 4px 12px rgba(16, 185, 129, 0.1); }
		.fm350-thermo-badge.level-normal { background: rgba(14, 165, 233, 0.12); color: #0ea5e9; box-shadow: 0 4px 12px rgba(14, 165, 233, 0.1); }
		.fm350-thermo-badge.level-warm   { background: rgba(245, 158, 11, 0.12); color: #f59e0b; box-shadow: 0 4px 12px rgba(245, 158, 11, 0.1); }
		.fm350-thermo-badge.level-hot    { background: rgba(239, 68, 68, 0.14); color: #ef4444; box-shadow: 0 4px 14px rgba(239, 68, 68, 0.16); }

		.fm350-thermal-main-stat {
			display: flex;
			flex-direction: column;
		}
		.fm350-thermal-main-label {
			font-size: 0.8rem;
			font-weight: 600;
			color: #64748b;
			text-transform: uppercase;
			letter-spacing: 0.05em;
		}
		.fm350-thermal-main-val {
			font-size: 2rem;
			font-weight: 800;
			color: #0f172a;
			line-height: 1.15;
			display: flex;
			align-items: baseline;
			gap: 8px;
		}
		.fm350-thermal-main-val small {
			font-size: 1rem;
			font-weight: 600;
			color: #64748b;
		}

		.fm350-thermal-right-metrics {
			display: flex;
			align-items: center;
			gap: 24px;
			flex-wrap: wrap;
		}
		.fm350-thermal-metric-item {
			display: flex;
			flex-direction: column;
			gap: 2px;
		}
		.fm350-thermal-metric-k {
			font-size: 0.74rem;
			color: #64748b;
			font-weight: 500;
		}
		.fm350-thermal-metric-v {
			font-size: 1.15rem;
			font-weight: 700;
			color: #1e293b;
		}

		/* 传感器逐路卡片网格 */
		.fm350-sensors-grid {
			display: grid;
			grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
			gap: 14px;
		}
		.fm350-sensor-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 14px;
			padding: 14px 16px;
			box-shadow: 0 4px 16px rgba(0, 0, 0, 0.02);
			transition: all 0.25s cubic-bezier(0.4, 0, 0.2, 1);
			display: flex;
			flex-direction: column;
			justify-content: space-between;
			position: relative;
			overflow: hidden;
		}
		.fm350-sensor-card:hover {
			background: var(--fm-glass-bg-hover);
			transform: translateY(-2px);
			box-shadow: 0 10px 24px rgba(31, 38, 135, 0.08);
			border-color: #fff;
		}
		/* 最高峰值卡片特色加亮 */
		.fm350-sensor-card.fm350-is-peak {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.95), rgba(254, 242, 242, 0.85));
			border-color: rgba(239, 68, 68, 0.35);
			box-shadow: 0 8px 24px rgba(239, 68, 68, 0.08);
		}
		.fm350-sensor-card.fm350-is-peak::before {
			content: "";
			position: absolute;
			top: 0; left: 0; right: 0;
			height: 3px;
			background: linear-gradient(90deg, #f59e0b, #ef4444);
		}

		.fm350-sensor-head {
			display: flex;
			align-items: center;
			justify-content: space-between;
			margin-bottom: 8px;
		}
		.fm350-sensor-info {
			display: flex;
			align-items: center;
			gap: 8px;
		}
		.fm350-sensor-id {
			font-size: 0.74rem;
			font-weight: 700;
			color: #64748b;
			background: rgba(100, 116, 139, 0.1);
			padding: 1px 6px;
			border-radius: 6px;
		}
		.fm350-sensor-name {
			font-size: 0.82rem;
			font-weight: 600;
			color: #334155;
		}
		.fm350-sensor-temp-box {
			display: flex;
			align-items: baseline;
			justify-content: space-between;
			margin-top: 4px;
			margin-bottom: 10px;
		}
		.fm350-sensor-val {
			font-size: 1.35rem;
			font-weight: 800;
			color: #0f172a;
			letter-spacing: -0.02em;
		}
		.fm350-sensor-code {
			font-size: 0.72rem;
			color: #94a3b8;
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
		}

		/* 刻度进度条 */
		.fm350-temp-bar-bg {
			height: 5px;
			background: rgba(226, 232, 240, 0.7);
			border-radius: 999px;
			overflow: hidden;
			position: relative;
		}
		.fm350-temp-bar-fill {
			height: 100%;
			border-radius: 999px;
			transition: width 0.4s ease;
		}
		.fill-cool   { background: linear-gradient(90deg, #34d399, #10b981); }
		.fill-normal { background: linear-gradient(90deg, #38bdf8, #0ea5e9); }
		.fill-warm   { background: linear-gradient(90deg, #fbbf24, #f59e0b); }
		.fill-hot    { background: linear-gradient(90deg, #f87171, #ef4444); }

		/* 徽章与标签 */
		.fm350-pill-tag {
			font-size: 0.72rem;
			font-weight: 600;
			padding: 2px 8px;
			border-radius: 9999px;
			background: rgba(148, 163, 184, 0.15);
			color: #475569;
			display: inline-flex;
			align-items: center;
		}
		.fm350-tag-good { background: rgba(16, 185, 129, 0.15); color: #047857; }
		.fm350-tag-ok   { background: rgba(6, 182, 212, 0.15); color: #0e7490; }
		.fm350-tag-mid  { background: rgba(245, 158, 11, 0.15); color: #b45309; }
		.fm350-tag-bad  { background: rgba(239, 68, 68, 0.15); color: #b91c1c; }
		.fm350-peak-badge {
			font-size: 0.68rem;
			font-weight: 700;
			padding: 2px 6px;
			border-radius: 6px;
			background: linear-gradient(135deg, #ef4444, #f59e0b);
			color: #ffffff;
			box-shadow: 0 2px 6px rgba(239, 68, 68, 0.35);
			animation: fm350-pulse-subtle 2s infinite ease-in-out;
		}

		/* 现代表格容器 */
		.fm350-table-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 16px;
			box-shadow: var(--fm-glass-shadow);
			overflow: hidden;
		}
		.fm350-table-card table {
			width: 100%;
			border-collapse: collapse;
			margin: 0;
		}
		.fm350-table-card tr {
			border-bottom: 1px solid rgba(226, 232, 240, 0.5);
			transition: background 0.2s ease;
		}
		.fm350-table-card tr:last-child { border-bottom: none; }
		.fm350-table-card tr:hover { background: rgba(255, 255, 255, 0.65); }
		.fm350-table-key {
			padding: 12px 20px;
			font-size: 0.86rem;
			font-weight: 500;
			color: #64748b;
			width: 38%;
		}
		.fm350-table-val {
			padding: 12px 20px;
			font-size: 0.88rem;
			font-weight: 600;
			color: #1e293b;
		}

		/* 按钮与信号指示 */
		.fm350-refresh-btn {
			background: var(--fm-primary-gradient) !important;
			border: none !important;
			color: #ffffff !important;
			padding: 8px 18px !important;
			border-radius: 12px !important;
			font-weight: 600 !important;
			display: inline-flex !important;
			align-items: center !important;
			gap: 8px !important;
			cursor: pointer;
			box-shadow: 0 4px 14px rgba(37, 99, 235, 0.28);
			transition: all 0.25s ease !important;
		}
		.fm350-refresh-btn:hover { filter: brightness(1.05); transform: translateY(-1px); }
		.fm350-status-indicator { position: relative; display: inline-flex; width: 10px; height: 10px; margin-right: 6px; }
		.fm350-indicator-core { width: 10px; height: 10px; border-radius: 50%; background: #94a3b8; }
		.fm350-indicator-ping { position: absolute; top: 0; left: 0; width: 10px; height: 10px; border-radius: 50%; opacity: 0.75; }
		.fm350-is-up .fm350-indicator-core { background: #10b981; }
		.fm350-is-up .fm350-indicator-ping { background: #34d399; animation: fm350-ping 2s cubic-bezier(0, 0, 0.2, 1) infinite; }
		.fm350-is-down .fm350-indicator-core { background: #ef4444; }

		.fm350-bars-wrap { display: inline-flex; align-items: flex-end; gap: 3px; height: 20px; vertical-align: middle; }
		.fm350-bar { width: 4px; background: #cbd5e1; border-radius: 2px; }
		.fm350-bar-on.fm350-bar-lvl-1 { background: #ef4444; }
		.fm350-bar-on.fm350-bar-lvl-2 { background: #f97316; }
		.fm350-bar-on.fm350-bar-lvl-3 { background: #f59e0b; }
		.fm350-bar-on.fm350-bar-lvl-4 { background: #06b6d4; }
		.fm350-bar-on.fm350-bar-lvl-5 { background: #10b981; }

		/* 骨架阶段加载提示（前端不再阻塞等待后端 AT 采集） */
		.fm350-loading-hint {
			display: flex;
			align-items: center;
			gap: 10px;
			padding: 14px 18px;
			margin: 0 0 18px 0;
			border-radius: 14px;
			background: rgba(37, 99, 235, 0.06);
			border: 1px solid rgba(37, 99, 235, 0.18);
			color: #1d4ed8;
			font-size: 0.86rem;
			font-weight: 600;
			backdrop-filter: blur(8px) saturate(160%);
		}
		.fm350-loading-spin {
			width: 14px;
			height: 14px;
			flex: 0 0 auto;
			border-radius: 50%;
			border: 2px solid rgba(37, 99, 235, 0.25);
			border-top-color: #2563eb;
			animation: fm350-loading-rotate 0.9s linear infinite;
		}
		@keyframes fm350-loading-rotate {
			to { transform: rotate(360deg); }
		}
		.fm350-dashboard-host {
			display: block;
		}

		.fm350-footer-note {
			margin-top: 32px;
			background: rgba(255, 255, 255, 0.55);
			backdrop-filter: var(--fm-glass-blur);
			border: 1px dashed rgba(203, 213, 225, 0.8);
			border-radius: 14px;
			padding: 18px 22px;
			font-size: 0.8rem;
			line-height: 1.6;
			color: #64748b;
		}

		/* ---------------- 矢量 SVG 动画 ---------------- */
		.fm350-svg-icon { width: 18px; height: 18px; display: block; }
		.fm350-svg-thermo { width: 28px; height: 28px; display: block; }
		.fm350-svg-sensor-mini { width: 16px; height: 16px; display: block; }
		.fm350-icon-btn { width: 16px; height: 16px; }

		/* 热浪升腾渐隐动画 */
		@keyframes fm350-steam-rise {
			0% { transform: translateY(0); opacity: 0.1; }
			50% { opacity: 0.9; }
			100% { transform: translateY(-4px); opacity: 0; }
		}
		.fm350-anim-heat-wave {
			animation: fm350-steam-rise 1.8s ease-in-out infinite;
		}
		.fm350-hw-1 { animation-delay: 0.2s; }
		.fm350-hw-2 { animation-delay: 0.6s; }
		.fm350-hw-3 { animation-delay: 1.0s; }

		/* 温度计底球呼吸 */
		@keyframes fm350-thermo-pulse {
			0%, 100% { transform: scale(1); opacity: 0.9; }
			50% { transform: scale(1.2); opacity: 1; filter: drop-shadow(0 0 3px currentColor); }
		}
		.fm350-anim-thermo-core {
			transform-origin: 12px 18px;
			animation: fm350-thermo-pulse 2.2s ease-in-out infinite;
		}

		/* 高温芯片震颤呼吸微动效 */
		@keyframes fm350-heat-shake {
			0%, 100% { transform: scale(1); }
			50% { transform: scale(1.08); filter: drop-shadow(0 0 2px #ef4444); }
		}
		.fm350-anim-hot-shake {
			transform-origin: 10px 10px;
			animation: fm350-heat-shake 1.6s ease-in-out infinite;
		}

		@keyframes fm350-ping { 75%, 100% { transform: scale(2.2); opacity: 0; } }
		@keyframes fm350-wave { 0% { transform: scale(0.85); opacity: 1; } 100% { transform: scale(1.6); opacity: 0; } }
		.fm350-anim-wave { transform-origin: 12px 7px; animation: fm350-wave 2.2s cubic-bezier(0.2, 0.8, 0.2, 1) infinite; }
		.fm350-anim-wave-1 { animation-delay: 0s; }
		.fm350-anim-wave-2 { animation-delay: 1.1s; }
		@keyframes fm350-spin-radar { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }
		.fm350-anim-spin { transform-origin: 12px 12px; animation: fm350-spin-radar 3.5s linear infinite; }
		@keyframes fm350-pulse-subtle { 0%, 100% { transform: scale(1); opacity: 1; } 50% { transform: scale(1.05); opacity: 0.85; } }
		/* chip() 内那个类此前只写在 SVG 标记里、没有规则，脉冲微动效从未生效 */
		.fm350-anim-pulse-subtle { transform-box: fill-box; transform-origin: center; animation: fm350-pulse-subtle 2s infinite ease-in-out; }

		/* ---------------- 响应式布局适配 ----------------
		   仅在小屏（<=767px / <=480px）调整排列、间距与滚动，不改变任何颜色、
		   字体、边框、圆角、阴影等视觉元素；桌面端（>=768px）样式保持不变。 */
		@media (max-width: 767px) {
			.fm350-hero { padding: 16px 18px; gap: 12px; }
			.fm350-glass-card { padding: 14px 16px; }
			.fm350-thermal-dashboard { padding: 16px 16px; gap: 14px; }
			.fm350-table-card { overflow-x: auto; -webkit-overflow-scrolling: touch; }
			.fm350-hero-info h2 { flex-wrap: wrap; }
			.fm350-section-title { margin-left: 2px; }
			.fm350-footer-note { padding: 14px 16px; }
		}
		@media (max-width: 480px) {
			.fm350-hero { padding: 14px 14px; }
			.fm350-glass-card { padding: 12px 12px; }
			.fm350-thermal-dashboard { padding: 14px 14px; }
		}
	`;

	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

return view.extend({
	load: function() {
		/* 关键：不在 load 里阻塞等待后端采集。
		   背景：后端原先每条 AT 命令前会 drain 空等 200 ms，一次 status 串 26 条命令
		   实测约 5.7 s；而 LuCI 在 load() 的 Promise resolve 之前不会调用 render()，
		   整页因此白屏数秒。
		   后端已修复（去空等 + 命令间隔限速），status 现约 1.0 s，但这里仍保留骨架化：
		   一是 AT 通道本身会波动（模组重注册、信号差时单条命令可达数百毫秒），
		   二是界面先可见、再原地补数据，比「整页等一个 Promise」体验稳定得多。 */
		return Promise.resolve(null);
	},

	render: function() {
		api.injectCss();
		injectFrostedGlassTheme();

		var self = this;

		/* build() 即原来的整段渲染逻辑；data 为 null 表示骨架阶段 */
		function build(data, loading) {
		var stRes = (data && data[0]) || {};
		var netRes = (data && data[1]) || {};
		var st = stRes.ok ? (stRes.value || {}) : (loading === true ? {} : null);
		var net = netRes.ok ? (netRes.value || null) : null;

		var info = st ? (st.info || {}) : {};
		var sig = st ? (st.signal || {}) : {};
		var pdp = st ? (st.pdp || {}) : {};
		/* PDP 状态与 IP/DNS 来自 fm350d 对模组的 AT 查询，是会话信息的权威数据源。
		   net.status 仅是主机内核快照，用于接口/路由就绪态，不用于展示当前 PDP IP/DNS。 */
		var pdpActive = !!pdp.active;
		var pdpHasIpv4 = pdpActive && !!pdp.ipv4;
		var pdpHasIpv6 = pdpActive && !!pdp.ipv6;
		var pdpStatus = pdpActive ? _('已连接') :
			(!info.iccid ? _('SIM 未就绪 / 无服务') :
				(sig.reg_state && sig.reg_state.indexOf('已注册') < 0 ? _('无服务 / 未注册') : _('PDP 未激活')));
		var temp = st ? (st.temperature || null) : null;
		var sensors = (temp && temp.sensors) ? temp.sensors : [];

		var level = bars(sig.csq) || barsFromRsrp(sig.rsrp);
		var g = grade(sig.rsrp, sig.sinr);
		var gText = gradeText(g);

		var nodes = [];

		/* 顶部 Hero 控制栏 */
		nodes.push(E('div', { 'class': 'fm350-hero' }, [
			E('div', { 'class': 'fm350-hero-info' }, [
				E('h2', {}, [
					SVG_ICONS.tower(),
					_('FM350-GL 模组状态'),
					loading === true ? tag(_('读取中…'), 'ok')
					                 : (st && st.at_ready ? tag(_('已在线'), 'good') : tag(_('离线'), 'bad'))
				]),
				E('div', { 'class': 'fm350-hero-desc' }, [
					_('固件版本：') + v(info.firmware, '-') + ' · ' + _('PCIe/USB 实时通道正常运行')
				])
			]),
			E('div', { 'class': 'fm350-hero-action' }, [
				E('button', {
					'class': 'fm350-refresh-btn',
					'click': ui.createHandlerFn(self, function(ev) { ev.preventDefault(); location.reload(); })
				}, [
					SVG_ICONS.refresh(),
					_('刷新数据')
				])
			])
		]));

		if (loading === true) {
			nodes.push(E('div', { 'class': 'fm350-loading-hint' }, [
				E('span', { 'class': 'fm350-loading-spin' }),
				_('正在从模组读取实时数据…（AT 通道为串行独占采集，通常需要数秒）')
			]));
		}

		if (!st) {
			nodes.push(E('div', { 'class': 'alert-message warning', 'style': 'margin-bottom: 20px;' }, [
				_('未能从后端 fm350d 取到状态。请确认服务已启动，且 AT 口未被其它进程占用。'),
				E('br'),
				E('code', {}, v(stRes.error, ''))
			]));
		}

		/* ---------------- 一、网络概览 ---------------- */
		nodes.push(E('div', { 'class': 'fm350-section-title' }, [ SVG_ICONS.radar(), _('网络概览') ]));
		nodes.push(E('div', { 'class': 'fm350-grid fm350-grid-wide' }, [
			card(_('综合信号 (SIGNAL)'),
				[ signalBars(sig.overall_level || 0),
				  v(sig.overall != null ? fmt1(sig.overall) + ' / 100' : null, _('未知')),
				  sig.overall_grade ? tag(sig.overall_grade, overallKind(sig.overall)) : '' ],
				(sig.overall_used && sig.overall_used.length)
					? _('由 ') + sig.overall_used.join(' + ') + _(' 加权计算')
					: _('缺少可用指标，未计算'),
				'radar'),

			card(_('运营商'), operatorText(sig), v(sig.reg_state, _('未知')), 'tower'),

			card(_('网络类型'),
				v(sig.rat, _('未知')),
				joinParts([
					sig.band ? _('频段 ') + sig.band : '',
					sig.bandwidth ? _('带宽档位 ') + sig.bandwidth : ''
				]),
				'tower'),

			card(_('信号强度 (RSRP)'),
				[ signalBars(level),
				  v(sig.rsrp != null ? sig.rsrp + ' dBm' : null, _('未知')) ],
				joinParts([
					sig.rssi_dbm != null ? 'RSSI ' + sig.rssi_dbm + ' dBm' : '',
					'CSQ ' + v(sig.csq)
				])),

			card(_('信号质量 (RSRQ)'),
				[ v(sig.rsrq != null ? fmt1(sig.rsrq) + ' dB' : null, _('未知')),
				  sig.rsrq_grade ? tag(sig.rsrq_grade, rsrqKind(sig.rsrq)) : '' ],
				_('优 ≥ -10 dB，良 ≥ -15 dB，中 ≥ -19.5 dB')),

			card(_('信噪比 (SINR)'),
				v(sig.sinr != null ? fmt1(sig.sinr) + ' dB' : null, _('未知')),
				joinParts([
					gText ? _('等级 ') + gText : '',
					_('优 ≥ 20 dB，良 ≥ 13 dB')
				])),

			card(_('服务小区'),
				sig.pci ? 'PCI ' + sig.pci : _('未知'),
				joinParts([
					sig.arfcn ? 'ARFCN ' + sig.arfcn : '',
					sig.lac ? 'TAC ' + sig.lac : '',
					sig.cid ? 'CID ' + sig.cid : ''
				])),

			card(_('模组温度'),
				temp ? v(temp.modem != null ? fmt1(temp.modem) + ' ℃'
					: (temp.peak != null ? fmt1(temp.peak) + ' ℃' : null), _('未知')) : _('未知'),
				temp ? joinParts([
					temp.soc_max != null ? _('SoC 峰值 ') + fmt1(temp.soc_max) + ' ℃' : '',
					temp.peak != null ? _('全片最高 ') + fmt1(temp.peak) + ' ℃' : '',
					sensors.length ? sensors.length + _(' 路有效传感器') : ''
				]) : '',
				'thermometer')
		]));

		/* ---------------- 二、连接与接口 ---------------- */
		nodes.push(E('div', { 'class': 'fm350-section-title' }, [ SVG_ICONS.chip(), _('连接与接口') ]));
		nodes.push(E('div', { 'class': 'fm350-grid' }, [
			card(_('模组型号'),
				v(info.model, _('未知')),
				joinParts([ info.manufacturer, info.firmware ]),
				'chip'),

			card(_('AT 通道'),
				[ dot(st && st.at_ready), v(st && st.at_ready ? _('就绪') : _('不可用')) ],
				v(st ? st.at_port : '', '')),

			card(_('PDP 上下文'),
				[ dot(pdpActive), pdpStatus ],
				_('APN ') + v(pdp.apn) + ' · ' + v(pdp.pdp_type, '')),

			card(_('IPv4 地址'), v(pdpActive ? pdp.ipv4 : null, pdpActive ? _('未分配') : _('未连接')),
				pdpActive ? v(pdp.ipv6, '') : '', 'ethernet'),
			card(_('DNS 服务器'), (pdpActive && pdp.dns && pdp.dns.length) ? pdp.dns.join(', ') : '-', _('来自模组 PDP 状态'))
		]));

		/* ---------------- 三、信号细节 ---------------- */
		nodes.push(E('div', { 'class': 'fm350-section-title' }, [ SVG_ICONS.tower(), _('信号细节') ]));
		nodes.push(E('div', { 'class': 'fm350-table-card' }, [
			E('table', {}, [
				E('tbody', {}, [
					row(_('运营商'), operatorText(sig)),
					row(_('网络类型'), sig.rat),
					row(_('注册状态'), sig.reg_state),
					row(_('频段'), sig.band),
					row(_('频段来源'), sig.band_source),
					row(_('带宽档位'), sig.bandwidth),
					row('RSRP', sig.rsrp != null ? sig.rsrp + ' dBm' : null),
					row('RSRQ', sig.rsrq != null ? fmt1(sig.rsrq) + ' dB' : null),
					row(_('RSRQ 质量等级'), sig.rsrq_grade),
					row('SINR', sig.sinr != null ? fmt1(sig.sinr) + ' dB' : null),
					row(_('信号等级'), gText ? gText + '（' + (g === 'good' ? 'SINR ≥ 20 dB' : g === 'ok' ? 'SINR ≥ 13 dB' : g === 'mid' ? 'SINR ≥ 0 dB' : 'SINR < 0 dB') + '）' : null),
					row(_('综合信号'), sig.overall != null
						? fmt1(sig.overall) + ' / 100（' + v(sig.overall_grade, '') + _('，权重 SINR 0.40 / RSRP 0.35 / RSRQ 0.25') + '）'
						: null),
					row(_('综合信号构成'), (sig.overall_used && sig.overall_used.length) ? sig.overall_used.join(' + ') : null),
					row('RSSI', sig.rssi_dbm != null ? sig.rssi_dbm + ' dBm' : null),
					row('CSQ', sig.csq != null ? sig.csq + (parseInt(sig.csq) >= 99 ? _('（模组不支持）') : '') : null),
					row('BER', sig.ber != null ? sig.ber + (parseInt(sig.ber) >= 99 ? _('（不可用）') : '') : null),
					row('PCI', sig.pci),
					row('ARFCN', sig.arfcn),
					row(_('LAC / TAC'), sig.lac),
					row(_('小区 ID'), sig.cid),
					row(_('载波聚合'), (sig.ca && sig.ca.length) ? sig.ca.join(' / ') : null),
					row(_('信号来源'), sig.source)
				])
			])
		]));

		/* ---------------- 四、温度传感器（重点优化） ---------------- */
		if (temp && sensors.length) {
			nodes.push(E('div', { 'class': 'fm350-section-title' }, [
				SVG_ICONS.thermometer(),
				_('温度传感器矩阵'),
				E('span', { 'class': 'fm350-pill-tag' }, sensors.length + _(' 路传感器工作在线'))
			]));

			// 统计计算
			var peakVal = temp.peak != null ? parseFloat(temp.peak) : null;
			var sum = 0, count = 0, peakMeta = null;
			sensors.forEach(function(s) {
				var id = Array.isArray(s) ? s[0] : (s && s.id);
				var val = Array.isArray(s) ? s[1] : (s && s.value);
				var f = parseFloat(val);
				if (!isNaN(f)) {
					sum += f;
					count++;
					if (peakVal != null && f === peakVal && !peakMeta) {
						peakMeta = sensorMeta(id);
					}
				}
			});
			var avgVal = count > 0 ? (sum / count) : null;
			var overallLevel = getTempLevel(peakVal != null ? peakVal : 45);

			var levelLabelMap = {
				'cool': _('低温良好'),
				'normal': _('运行平稳'),
				'warm': _('轻微发热'),
				'hot': _('高温告警')
			};

			/* 1. 毛玻璃温控总览看板 */
			var thermalBanner = E('div', { 'class': 'fm350-thermal-dashboard' }, [
				E('div', { 'class': 'fm350-thermal-left' }, [
					E('div', { 'class': 'fm350-thermo-badge level-' + overallLevel }, SVG_ICONS.thermometer()),
					E('div', { 'class': 'fm350-thermal-main-stat' }, [
						E('div', { 'class': 'fm350-thermal-main-label' }, _('全片最高传感器温度')),
						E('div', { 'class': 'fm350-thermal-main-val' }, [
							peakVal != null ? fmt1(peakVal) : '-',
							E('small', {}, '℃'),
							tag(levelLabelMap[overallLevel], overallLevel === 'cool' ? 'good' : (overallLevel === 'normal' ? 'ok' : (overallLevel === 'warm' ? 'mid' : 'bad')))
						])
					])
				]),
				E('div', { 'class': 'fm350-thermal-right-metrics' }, [
					E('div', { 'class': 'fm350-thermal-metric-item' }, [
						E('div', { 'class': 'fm350-thermal-metric-k' }, _('最高温度源')),
						E('div', { 'class': 'fm350-thermal-metric-v' }, peakMeta ? peakMeta.name : _('SoC/PA'))
					]),
					E('div', { 'class': 'fm350-thermal-metric-item' }, [
						E('div', { 'class': 'fm350-thermal-metric-k' }, _('全片平均温')),
						E('div', { 'class': 'fm350-thermal-metric-v' }, avgVal != null ? fmt1(avgVal) + ' ℃' : '-')
					]),
					temp.soc_max != null ? E('div', { 'class': 'fm350-thermal-metric-item' }, [
						E('div', { 'class': 'fm350-thermal-metric-k' }, _('SoC 核心峰值')),
						E('div', { 'class': 'fm350-thermal-metric-v' }, fmt1(temp.soc_max) + ' ℃')
					]) : ''
				])
			]);
			nodes.push(thermalBanner);

			/* 2. 传感器网格排版 (Frosted Card Grid) */
			var sensorCards = sensors.map(function(s) {
				var id = Array.isArray(s) ? s[0] : (s && s.id);
				var val = Array.isArray(s) ? s[1] : (s && s.value);
				var meta = sensorMeta(id);
				var fVal = parseFloat(val);
				var hot = (peakVal != null && !isNaN(fVal) && fVal === peakVal);
				var lvl = getTempLevel(val);

				// 进度百分比标定：按 30℃～85℃ 区间换算
				var pct = 0;
				if (!isNaN(fVal)) {
					pct = Math.min(100, Math.max(0, Math.round(((fVal - 30) / (85 - 30)) * 100)));
				}

				return E('div', { 'class': 'fm350-sensor-card' + (hot ? ' fm350-is-peak' : '') }, [
					E('div', { 'class': 'fm350-sensor-head' }, [
						E('div', { 'class': 'fm350-sensor-info' }, [
							SVG_ICONS.sensorMini(hot),
							E('span', { 'class': 'fm350-sensor-id' }, '#' + id),
							E('span', { 'class': 'fm350-sensor-name' }, meta.name)
						]),
						hot ? E('span', { 'class': 'fm350-peak-badge' }, _('峰值 PEAK')) : ''
					]),
					E('div', { 'class': 'fm350-sensor-temp-box' }, [
						E('span', { 'class': 'fm350-sensor-val' }, [
							fmt1(val),
							E('span', { 'style': 'font-size: 0.85rem; font-weight: 500; color: #64748b; margin-left: 2px;' }, '℃')
						]),
						E('span', { 'class': 'fm350-sensor-code' }, meta.code)
					]),
					E('div', { 'class': 'fm350-temp-bar-bg' }, [
						E('div', {
							'class': 'fm350-temp-bar-fill fill-' + lvl,
							'style': 'width: ' + pct + '%'
						})
					])
				]);
			});

			nodes.push(E('div', { 'class': 'fm350-sensors-grid' }, sensorCards));
		}

		/* ---------------- 五、识别信息 ---------------- */
		nodes.push(E('div', { 'class': 'fm350-section-title' }, [ SVG_ICONS.chip(), _('识别信息') ]));
		nodes.push(E('div', { 'class': 'fm350-table-card' }, [
			E('table', {}, [
				E('tbody', {}, [
					row('IMEI', info.imei),
					row(_('序列号'), info.serial),
					row('IMSI', info.imsi),
					row('ICCID', info.iccid),
					row(_('USB 模式'), info.usb_mode),
					row(_('SIM 卡槽'), info.sim_slot),
					row(_('短信中心'), info.sms_center)
				])
			])
		]));

		/* ---------------- 六、网络接口 ---------------- */
		if (net) {
			nodes.push(E('div', { 'class': 'fm350-section-title' }, [ SVG_ICONS.ethernet(), _('网络接口') ]));
			nodes.push(E('div', { 'class': 'fm350-table-card' }, [
				E('table', {}, [
					E('tbody', {}, [
						row(_('IPv4 接口'), net.iface),
						row(_('IPv6 接口'), net.iface_v6),
						row(_('物理网卡'), net.dev),
						row(_('PDP IPv4 地址'), pdpHasIpv4 ? pdp.ipv4 : null),
						row(_('PDP IPv6 地址'), pdpHasIpv6 ? pdp.ipv6 : null),
						/* 观测主机侧默认路由是否就位（v4/v6 分开看），并结合模组 PDP
						   状态与内核接口/路由快照给出链路就绪结论。 */
						row(_('默认路由 IPv4'), pdpHasIpv4 && net.default4 != null ? (net.default4 ? _('已就位') : _('未就位')) : null),
						(pdpHasIpv6 && net.ipv6 && net.ipv6.length)
							? row(_('默认路由 IPv6'), net.default6 != null ? (net.default6 ? _('已就位') : _('未就位')) : null)
							: null,
						row(_('PDP DNS'), (pdpActive && pdp.dns && pdp.dns.length) ? pdp.dns.join(', ') : null),
						row(_('接口路由'), ((pdpHasIpv4 || pdpHasIpv6) && net.routes && net.routes.length) ? net.routes.join('  |  ') : null),
						row(_('接口状态'), net.up ? _('已启用') : _('未启用')),
						row(_('链路就绪'), (pdpActive && net.up &&
							((pdpHasIpv4 && net.default4) ||
								(pdpHasIpv6 && net.ipv6 && net.ipv6.length && net.default6)))
							? _('是') : _('否'))
					])
				])
			]));
		}

		/* 底部半透明提示卡 */
		nodes.push(E('div', { 'class': 'fm350-footer-note' }, [
			_('本页所有数值均为模组实时上报，未取到的项显示 “-”。'),
			_('「频段」与「带宽档位」会把模组上报的原始编码解码为频段名与带宽值；'),
			_('频段未上报时按 ARFCN 推算，两种情况均由「频段来源」标注；'),
			_('「运营商」名称来自插件内置 MCC/MNC 表，数字码为模组上报值。'),
			E('br'),
			_('「RSRQ 质量等级」「信号等级」「综合信号（SIGNAL）」均为派生值：'),
			_('综合信号由 RSRP / RSRQ / SINR 归一化到 0..100 后按'),
			_('SINR 0.40 / RSRP 0.35 / RSRQ 0.25 加权平均（仅对已取到的指标计算，'),
			_('并按可用权重重新归一化），得分构成见「信号细节」表，不参与任何链路判定。'),
			E('br'),
			_('IMEI 与序列号在此页为只读展示。写入需在「服务设置」页显式开启开关并二次确认，'),
			_('后端会在写入前自动备份原值到 /etc/fm350/imei.backup。')
		]));

		return E('div', { 'class': 'fm350-dashboard' }, nodes);
		}

		var host = E('div', { 'class': 'fm350-dashboard-host' });

		/* 1) 先出骨架：界面立刻可见，不再是数秒白屏 */
		host.appendChild(build(null, true));

		/* 2) 并发取数，到达后在同一容器内原地重绘（不整页刷新） */
		Promise.all([ api.status(), api.net() ]).then(function(data) {
			host.removeChild(host.firstChild);
			host.appendChild(build(data, false));
		}).catch(function(e) {
			var hint = host.querySelector('.fm350-loading-hint');
			if (hint)
				hint.textContent = _('读取实时数据失败：') + e;
		});

		return host;
	}
});
