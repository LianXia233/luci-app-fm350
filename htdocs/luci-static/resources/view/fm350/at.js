'use strict';
'require view';
'require ui';
'require fm350.api as api';

/* 常用指令分组与快捷入口（均为只读或安全查询操作） */
var PRESET_GROUPS = [
	{
		group: _('基础信息'),
		items: [
			{ label: _('模组信息'), cmd: 'ATI' },
			{ label: _('USB 模式'), cmd: 'AT+GTUSBMODE?' },
			/* 手册 18.3 仅定义写形式 `+GTSENRDTEMP=<sensor_id>`，无 `?` 读形式；
			   发 `AT+GTSENRDTEMP?` 会返回 +CME ERROR: phone failure。
			   <sensor_id> = 0 表示一次返回全部传感器读数。 */
			{ label: _('模组温度'), cmd: 'AT+GTSENRDTEMP=0' },
			{ label: _('读取 IMEI (只读)'), cmd: 'AT+EGMREXT=0,7' }
		]
	},
	{
		group: _('网络与注册'),
		items: [
			{ label: _('信号强度'), cmd: 'AT+CSQ' },
			{ label: _('扩展信号'), cmd: 'AT+CESQ' },
			{ label: _('网络注册'), cmd: 'AT+CEREG?' },
			{ label: _('运营商查询'), cmd: 'AT+COPS?' }
		]
	},
	{
		group: _('PDP 与路由'),
		items: [
			{ label: _('PDP 上下文'), cmd: 'AT+CGDCONT?' },
			{ label: _('PDP 激活状态'), cmd: 'AT+CGACT?' },
			{ label: _('分配 IP 地址'), cmd: 'AT+CGPADDR=1' }
		]
	},
	{
		group: _('射频与小区'),
		items: [
			{ label: _('小区与邻区'), cmd: 'AT+GTCCINFO?' },
			{ label: _('载波聚合'), cmd: 'AT+GTCAINFO?' },
			{ label: _('锁频段状态'), cmd: 'AT+GTACT?' },
			{ label: _('锁小区状态'), cmd: 'AT+EMMCHLCK?' }
		]
	}
];

/* ---------------- 动态 SVG 矢量图标库 ---------------- */
function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

var SVG_ICONS = {
	// 动态命令行控制台
	terminal: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<polyline points="4 17 10 11 4 5" stroke-linecap="round" stroke-linejoin="round"/>
				<line class="fm350-anim-cursor-blink" x1="12" y1="19" x2="20" y2="19" stroke-linecap="round" stroke-width="2.2"/>
			</svg>
		`);
	},
	// 安全沙箱盾牌
	shieldSecurity: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>
				<path class="fm350-anim-pulse-subtle" d="M9 12l2 2 4-4" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		`);
	},
	// 执行火箭/发射微标
	play: function() {
		return parseSvg(`
			<svg style="width:14px;height:14px;display:inline-block;vertical-align:-2px;margin-right:4px;" viewBox="0 0 24 24" fill="currentColor">
				<polygon points="5 3 19 12 5 21 5 3"/>
			</svg>
		`);
	},
	// 清屏扫帚/清空
	clear: function() {
		return parseSvg(`
			<svg style="width:13px;height:13px;display:inline-block;vertical-align:-2px;margin-right:4px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
				<line x1="10" y1="11" x2="10" y2="17"/>
				<line x1="14" y1="11" x2="14" y2="17"/>
			</svg>
		`);
	},
	// 剪贴板复制
	copy: function() {
		return parseSvg(`
			<svg style="width:13px;height:13px;display:inline-block;vertical-align:-2px;margin-right:4px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
				<path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
			</svg>
		`);
	},
	// 快捷药丸闪电
	bolt: function() {
		return parseSvg(`
			<svg style="width:11px;height:11px;display:inline-block;vertical-align:-1px;margin-right:4px;" viewBox="0 0 24 24" fill="currentColor">
				<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>
			</svg>
		`);
	}
};

/* ---------------- 注入白色毛玻璃与终端样式系统 ---------------- */
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
	var styleId = 'fm350-at-glass-styles';
	if (document.getElementById(styleId)) return;

	var css = `
		:root {
			--fm-glass-bg: rgba(255, 255, 255, 0.80);
			--fm-glass-bg-hover: rgba(255, 255, 255, 0.94);
			--fm-glass-border: rgba(255, 255, 255, 0.88);
			--fm-glass-shadow: 0 12px 32px 0 rgba(31, 38, 135, 0.05), 0 2px 8px 0 rgba(0, 0, 0, 0.02);
			--fm-glass-blur: blur(18px) saturate(180%);
			--fm-primary: #2563eb;
			--fm-primary-gradient: linear-gradient(135deg, #2563eb 0%, #06b6d4 100%);
			--fm-text-main: #0f172a;
			--fm-text-muted: #64748b;
			--fm-term-bg: #0b1329;
			--fm-term-head: #1e293b;
		}

		.fm350-at-page {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
			color: var(--fm-text-main);
			padding: 4px 0 32px 0;
		}

		/* 顶部 Hero 态势看板 */
		.fm350-at-hero {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.90), rgba(240, 246, 255, 0.82));
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 20px;
			padding: 22px 28px;
			box-shadow: var(--fm-glass-shadow);
			display: flex;
			align-items: center;
			justify-content: space-between;
			flex-wrap: wrap;
			gap: 18px;
			margin-bottom: 22px;
			position: relative;
			overflow: hidden;
		}
		.fm350-at-hero::after {
			content: "";
			position: absolute;
			top: -40px; right: -40px;
			width: 170px; height: 170px;
			background: radial-gradient(circle, rgba(37, 99, 235, 0.08) 0%, transparent 70%);
			border-radius: 50%;
			pointer-events: none;
		}
		.fm350-hero-left {
			display: flex;
			align-items: center;
			gap: 16px;
		}
		.fm350-hero-avatar {
			width: 56px;
			height: 56px;
			border-radius: 16px;
			background: linear-gradient(135deg, rgba(37, 99, 235, 0.12), rgba(6, 182, 212, 0.12));
			color: var(--fm-primary);
			display: flex;
			align-items: center;
			justify-content: center;
			box-shadow: 0 4px 12px rgba(37, 99, 235, 0.1);
		}
		.fm350-hero-meta h2 {
			margin: 0 0 4px 0;
			font-size: 1.4rem;
			font-weight: 700;
			color: #0f172a;
			display: flex;
			align-items: center;
			gap: 10px;
		}
		.fm350-hero-meta p {
			margin: 0;
			font-size: 0.82rem;
			color: var(--fm-text-muted);
		}
		.fm350-hero-status-pill {
			display: inline-flex;
			align-items: center;
			gap: 6px;
			background: rgba(16, 185, 129, 0.12);
			border: 1px solid rgba(16, 185, 129, 0.25);
			color: #047857;
			padding: 5px 12px;
			border-radius: 999px;
			font-size: 0.8rem;
			font-weight: 700;
		}

		/* 安全保护策略提醒卡 */
		.fm350-security-notice {
			background: linear-gradient(135deg, rgba(255, 247, 237, 0.85), rgba(254, 242, 242, 0.85));
			backdrop-filter: var(--fm-glass-blur);
			border: 1px solid rgba(251, 146, 60, 0.35);
			border-radius: 16px;
			padding: 14px 18px;
			margin-bottom: 22px;
			display: flex;
			align-items: flex-start;
			gap: 14px;
			box-shadow: 0 4px 14px rgba(249, 115, 22, 0.05);
		}
		.fm350-sec-icon-wrap {
			color: #ea580c;
			flex-shrink: 0;
			margin-top: 1px;
		}
		.fm350-sec-content {
			font-size: 0.82rem;
			line-height: 1.55;
			color: #9a3412;
		}
		.fm350-sec-content strong {
			color: #c2410c;
		}

		/* ---------------- 指令输入交互卡片 ---------------- */
		.fm350-input-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 18px;
			padding: 18px 22px;
			box-shadow: var(--fm-glass-shadow);
			margin-bottom: 22px;
			display: flex;
			align-items: center;
			gap: 12px;
			flex-wrap: wrap;
			transition: all 0.25s ease;
		}
		.fm350-input-card:hover {
			background: var(--fm-glass-bg-hover);
			box-shadow: 0 16px 36px 0 rgba(31, 38, 135, 0.08);
		}
		.fm350-at-input-box {
			flex: 1;
			min-width: 260px;
			position: relative;
		}
		.fm350-at-input {
			width: 100%;
			box-sizing: border-box;
			background: rgba(255, 255, 255, 0.92) !important;
			border: 1px solid rgba(203, 213, 225, 0.9) !important;
			border-radius: 12px !important;
			padding: 10px 16px !important;
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace !important;
			font-size: 0.92rem !important;
			font-weight: 600 !important;
			color: #0f172a !important;
			outline: none !important;
			transition: all 0.2s ease !important;
		}
		.fm350-at-input:focus {
			border-color: var(--fm-primary) !important;
			box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.16) !important;
			background: #ffffff !important;
		}
		.fm350-at-run-btn {
			background: var(--fm-primary-gradient) !important;
			border: none !important;
			color: #ffffff !important;
			padding: 10px 24px !important;
			border-radius: 12px !important;
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			display: inline-flex !important;
			align-items: center !important;
			box-shadow: 0 4px 14px rgba(37, 99, 235, 0.28) !important;
			transition: all 0.25s ease !important;
			user-select: none;
		}
		.fm350-at-run-btn:hover {
			filter: brightness(1.06);
			transform: translateY(-1px);
			box-shadow: 0 6px 20px rgba(37, 99, 235, 0.38) !important;
		}
		.fm350-at-run-btn:disabled {
			opacity: 0.6;
			cursor: not-allowed;
			transform: none !important;
		}

		/* ---------------- 常用指令分组面板 ---------------- */
		.fm350-presets-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 18px;
			padding: 18px 22px;
			box-shadow: var(--fm-glass-shadow);
			margin-bottom: 22px;
		}
		.fm350-presets-title {
			font-size: 0.84rem;
			font-weight: 700;
			color: #475569;
			text-transform: uppercase;
			letter-spacing: 0.05em;
			margin-bottom: 12px;
			display: flex;
			align-items: center;
			justify-content: space-between;
		}
		.fm350-group-grid {
			display: grid;
			grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
			gap: 14px;
		}
		.fm350-group-col {
			display: flex;
			flex-direction: column;
			gap: 6px;
		}
		.fm350-group-label {
			font-size: 0.74rem;
			font-weight: 600;
			color: #94a3b8;
			padding-left: 2px;
		}
		.fm350-chip-btn {
			background: rgba(255, 255, 255, 0.90);
			border: 1px solid rgba(226, 232, 240, 0.9);
			border-radius: 10px;
			padding: 6px 12px;
			font-size: 0.78rem;
			font-weight: 600;
			color: #334155;
			cursor: pointer;
			display: flex;
			align-items: center;
			justify-content: space-between;
			transition: all 0.2s ease;
			user-select: none;
		}
		.fm350-chip-btn code {
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			font-size: 0.74rem;
			color: var(--fm-primary);
		}
		.fm350-chip-btn:hover {
			background: #ffffff;
			border-color: var(--fm-primary);
			color: var(--fm-primary);
			transform: translateY(-1px);
			box-shadow: 0 3px 10px rgba(37, 99, 235, 0.15);
		}
		.fm350-chip-btn:active {
			transform: translateY(1px);
		}

		/* ---------------- 专业深色亚克力终端控制台 ---------------- */
		.fm350-terminal-card {
			background: var(--fm-term-bg);
			border-radius: 18px;
			border: 1px solid rgba(255, 255, 255, 0.12);
			box-shadow: 0 16px 40px rgba(11, 19, 41, 0.35);
			overflow: hidden;
			display: flex;
			flex-direction: column;
		}
		.fm350-terminal-header {
			background: var(--fm-term-head);
			padding: 10px 18px;
			display: flex;
			align-items: center;
			justify-content: space-between;
			border-bottom: 1px solid rgba(255, 255, 255, 0.08);
		}
		.fm350-mac-dots {
			display: flex;
			align-items: center;
			gap: 7px;
		}
		.fm350-mac-dot {
			width: 11px;
			height: 11px;
			border-radius: 50%;
		}
		.fm350-dot-red    { background: #ef4444; }
		.fm350-dot-yellow { background: #f59e0b; }
		.fm350-dot-green  { background: #10b981; }

		.fm350-term-title {
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			font-size: 0.78rem;
			font-weight: 600;
			color: #94a3b8;
			letter-spacing: 0.05em;
		}
		.fm350-term-actions {
			display: flex;
			align-items: center;
			gap: 8px;
		}
		.fm350-term-act-btn {
			background: rgba(255, 255, 255, 0.08);
			border: 1px solid rgba(255, 255, 255, 0.12);
			border-radius: 7px;
			padding: 4px 10px;
			font-size: 0.72rem;
			font-weight: 600;
			color: #cbd5e1;
			cursor: pointer;
			transition: all 0.2s ease;
		}
		.fm350-term-act-btn:hover {
			background: rgba(255, 255, 255, 0.18);
			color: #ffffff;
		}

		/* 终端打印区 */
		.fm350-terminal-viewport {
			min-height: 280px;
			max-height: 460px;
			overflow-y: auto;
			padding: 16px 20px;
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			font-size: 0.86rem;
			line-height: 1.6;
			color: #e2e8f0;
			white-space: pre-wrap;
			word-break: break-all;
		}

		/* 终端内行级高亮颜色 */
		.fm350-term-echo {
			color: #38bdf8;
			font-weight: 700;
			margin-top: 6px;
			display: flex;
			align-items: center;
			gap: 6px;
		}
		.fm350-term-resp {
			color: #f1f5f9;
			margin-left: 12px;
		}
		.fm350-term-ok {
			color: #34d399;
			font-weight: 700;
		}
		.fm350-term-err {
			color: #f87171;
			font-weight: 700;
			background: rgba(239, 68, 68, 0.1);
			padding: 2px 6px;
			border-radius: 4px;
			display: inline-block;
		}
		.fm350-term-wait {
			color: #64748b;
			font-style: italic;
		}

		/* ---------------- SVG 动效 ---------------- */
		.fm350-svg-icon { width: 20px; height: 20px; display: block; }
		@keyframes fm350-cursor-blink {
			0%, 100% { opacity: 1; }
			50% { opacity: 0; }
		}
		.fm350-anim-cursor-blink {
			animation: fm350-cursor-blink 1s infinite;
		}
		@keyframes fm350-pulse-subtle {
			0%, 100% { transform: scale(1); opacity: 1; }
			50% { transform: scale(1.06); opacity: 0.75; }
		}
		.fm350-anim-pulse-subtle {
			transform-origin: 12px 12px;
			animation: fm350-pulse-subtle 2.2s infinite ease-in-out;
		}

		/* ---------------- 响应式布局适配 ----------------
		   仅在小屏（<=767px / <=480px）调整排列、间距与滚动，不改变任何颜色、
		   字体、边框、圆角、阴影等视觉元素；桌面端（>=768px）样式保持不变。 */
		@media (max-width: 767px) {
			.fm350-at-hero { padding: 16px 18px; gap: 12px; }
			.fm350-security-notice { padding: 12px 14px; gap: 10px; }
			.fm350-input-card { padding: 14px 16px; }
			.fm350-presets-card { padding: 14px 16px; }
			.fm350-hero-meta h2 { flex-wrap: wrap; }
			.fm350-terminal-header { flex-wrap: wrap; }
			.fm350-term-title { flex: 1 1 auto; min-width: 120px; }
			.fm350-terminal-actions { flex-wrap: wrap; }
			.fm350-terminal-viewport { padding: 12px 14px; }
			.fm350-at-input-box { min-width: 0; flex-basis: 100%; }
			.fm350-chip-btn code { word-break: break-all; }
		}
		@media (max-width: 480px) {
			.fm350-at-hero { padding: 14px 14px; }
			.fm350-input-card { padding: 12px 12px; }
			.fm350-presets-card { padding: 12px 12px; }
			.fm350-security-notice { padding: 11px 12px; }
			.fm350-presets-title { flex-wrap: wrap; gap: 6px; }
		}
	`;

	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

return view.extend({
	render: function() {
		api.injectCss();
		injectFrostedGlassTheme();

		var pristine = true;
		var commandHistory = [];
		var historyIndex = -1;

		var wrap = E('div', { 'class': 'fm350-at-page' });

		/* ---------------- 一、Hero 状态顶栏 ---------------- */
		var heroBanner = E('div', { 'class': 'fm350-at-hero' }, [
			E('div', { 'class': 'fm350-hero-left' }, [
				E('div', { 'class': 'fm350-hero-avatar' }, [ SVG_ICONS.terminal() ]),
				E('div', { 'class': 'fm350-hero-meta' }, [
					E('h2', {}, [ _('AT 交互终端'), E('span', { 'style': 'font-size:0.8rem; font-weight:600; color:#64748b;' }, 'Serial CLI') ]),
					E('p', {}, _('经由 fm350d 守护进程与独占 AT 串口交互 · 支持方向键历史翻阅'))
				])
			]),
			E('div', { 'class': 'fm350-hero-status-pill' }, [
				E('span', { 'style': 'width:8px; height:8px; border-radius:50%; background:#10b981;' }),
				_('通道就绪 · 排他独占')
			])
		]);
		wrap.appendChild(heroBanner);

		/* ---------------- 二、安全策略通知 ---------------- */
		var secNotice = E('div', { 'class': 'fm350-security-notice' }, [
			E('div', { 'class': 'fm350-sec-icon-wrap' }, [ SVG_ICONS.shieldSecurity() ]),
			E('div', { 'class': 'fm350-sec-content' }, [
				E('strong', {}, _('安全沙箱保护机制：')),
				_('IMEI / 串号写入类指令（如 AT+EGMREXT=1,*、AT+EGMR=1,*、AT+SIMEI=*、AT+CGSN=*）由网关防护策略默认拦截。'
					+ '如需维护原厂串号，请至「服务设置」中开启写入开关并使用受控专用接口操作。')
			])
		]);
		wrap.appendChild(secNotice);

		/* ---------------- 三、终端屏幕构建 ---------------- */
		var termViewport = E('div', { 'class': 'fm350-terminal-viewport' }, [
			E('span', { 'class': 'fm350-term-wait' }, _('等待输入 AT 指令…（回车发送，键盘 ↑ / ↓ 可快速调用历史指令）'))
		]);

		function append(line, type) {
			if (pristine) {
				termViewport.innerHTML = '';
				pristine = false;
			}

			var rowNode;
			if (type === 'echo') {
				rowNode = E('div', { 'class': 'fm350-term-echo' }, [
					E('span', { 'style': 'color:#38bdf8;' }, 'fm350@modem:~$'),
					E('span', {}, line)
				]);
			} else if (type === 'err') {
				rowNode = E('div', { 'class': 'fm350-term-err' }, line);
			} else {
				// 处理多行文本与 OK / ERROR 语法高亮
				var formatted = line.split('\n').map(function(l) {
					var trimL = l.trim();
					if (trimL === 'OK') {
						return E('div', { 'class': 'fm350-term-ok' }, l);
					} else if (trimL.indexOf('ERROR') !== -1) {
						return E('div', { 'class': 'fm350-term-err' }, l);
					}
					return E('div', {}, l);
				});
				rowNode = E('div', { 'class': 'fm350-term-resp' }, formatted);
			}

			termViewport.appendChild(rowNode);
			termViewport.scrollTop = termViewport.scrollHeight;
		}

		/* 终端顶栏操作 */
		var btnClear = E('button', {
			'class': 'fm350-term-act-btn',
			'click': function(ev) {
				ev.preventDefault();
				termViewport.innerHTML = '';
				pristine = true;
				append(_('控制台屏幕已清空。等待输入指令…'), 'wait');
			}
		}, [ SVG_ICONS.clear(), _('清屏') ]);

		var btnCopy = E('button', {
			'class': 'fm350-term-act-btn',
			'click': function(ev) {
				ev.preventDefault();
				var text = termViewport.innerText;
				if (navigator.clipboard && navigator.clipboard.writeText) {
					navigator.clipboard.writeText(text).then(function() {
						ui.addNotification(null, E('p', _('终端回显内容已成功复制到剪贴板')), 'info');
					});
				} else {
					ui.addNotification(null, E('p', _('浏览器限制，未能直接访问剪贴板')), 'warning');
				}
			}
		}, [ SVG_ICONS.copy(), _('复制回显') ]);

		var termCard = E('div', { 'class': 'fm350-terminal-card' }, [
			E('div', { 'class': 'fm350-terminal-header' }, [
				E('div', { 'class': 'fm350-mac-dots' }, [
					E('span', { 'class': 'fm350-mac-dot fm350-dot-red' }),
					E('span', { 'class': 'fm350-mac-dot fm350-dot-yellow' }),
					E('span', { 'class': 'fm350-mac-dot fm350-dot-green' })
				]),
				E('div', { 'class': 'fm350-term-title' }, 'TTY SESSION : fm350d (RAW_AT)'),
				E('div', { 'class': 'fm350-term-actions' }, [ btnClear, btnCopy ])
			]),
			termViewport
		]);

		/* ---------------- 四、指令输入卡片 ---------------- */
		var input = E('input', {
			'class': 'fm350-at-input',
			'type': 'text',
			'placeholder': _('输入 AT 指令（例如 AT+CSQ、ATI、AT+GTCCINFO?）'),
			'autocomplete': 'off',
			'spellcheck': 'false'
		});

		var btnRun = E('button', {
			'class': 'fm350-at-run-btn',
			'click': function(ev) {
				ev.preventDefault();
				run(input.value.trim());
			}
		}, [ SVG_ICONS.play(), _('下发执行 (Enter)') ]);

		function run(cmd) {
			if (!cmd) return;

			// 记录历史
			if (commandHistory.length === 0 || commandHistory[commandHistory.length - 1] !== cmd) {
				commandHistory.push(cmd);
			}
			historyIndex = commandHistory.length;

			append(cmd, 'echo');
			btnRun.disabled = true;

			api.at(cmd).then(function(res) {
				btnRun.disabled = false;
				if (res && res.ok) {
					var outStr = String(res.value || '').replace(/\r/g, '').trim();
					append(outStr || 'OK');
				} else {
					append(_('执行错误：') + ((res && res.error) || _('未知异常')), 'err');
				}
				input.value = '';
				input.focus();
			}).catch(function(e) {
				btnRun.disabled = false;
				append(_('RPC 通信错误：') + e, 'err');
				input.focus();
			});
		}

		// 键盘监听：Enter 下发，上下方向键遍历历史
		input.addEventListener('keydown', function(ev) {
			if (ev.key === 'Enter') {
				ev.preventDefault();
				run(input.value.trim());
			} else if (ev.key === 'ArrowUp') {
				ev.preventDefault();
				if (commandHistory.length > 0) {
					if (historyIndex > 0) historyIndex--;
					input.value = commandHistory[historyIndex] || '';
				}
			} else if (ev.key === 'ArrowDown') {
				ev.preventDefault();
				if (commandHistory.length > 0) {
					if (historyIndex < commandHistory.length - 1) {
						historyIndex++;
						input.value = commandHistory[historyIndex];
					} else {
						historyIndex = commandHistory.length;
						input.value = '';
					}
				}
			}
		});

		var inputCard = E('div', { 'class': 'fm350-input-card' }, [
			E('div', { 'class': 'fm350-at-input-box' }, input),
			btnRun
		]);
		wrap.appendChild(inputCard);

		/* ---------------- 五、常用指令快捷药丸分组 ---------------- */
		var groupCols = PRESET_GROUPS.map(function(grp) {
			var chips = grp.items.map(function(item) {
				return E('div', {
					'class': 'fm350-chip-btn',
					'click': function(ev) {
						ev.preventDefault();
						input.value = item.cmd;
						run(item.cmd);
					}
				}, [
					E('span', {}, [ SVG_ICONS.bolt(), item.label ]),
					E('code', {}, item.cmd)
				]);
			});

			return E('div', { 'class': 'fm350-group-col' }, [
				E('div', { 'class': 'fm350-group-label' }, grp.group),
				E('div', { 'style': 'display:flex; flex-direction:column; gap:6px;' }, chips)
			]);
		});

		var presetsCard = E('div', { 'class': 'fm350-presets-card' }, [
			E('div', { 'class': 'fm350-presets-title' }, [
				E('span', {}, _('常用 AT 指令快捷库')),
				E('span', { 'style': 'font-size:0.75rem; color:#94a3b8; font-weight:normal;' }, _('点击即刻填入并执行'))
			]),
			E('div', { 'class': 'fm350-group-grid' }, groupCols)
		]);
		wrap.appendChild(presetsCard);

		/* ---------------- 六、终端视窗挂载 ---------------- */
		wrap.appendChild(termCard);

		return wrap;
	}
});