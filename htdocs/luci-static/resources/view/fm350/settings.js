/*
 * LuCI FM350-GL 5G 模组服务设置页面 (White Frosted Glass Edition)
 * 支持动态矢量动效、毛玻璃卡片、自适应响应式布局与现代化交互
 */
'use strict';
'require view';
'require form';
'require uci';
'require ui';
'require fm350.api as api';

/* IMEI Luhn 校验位验证（算法与后端 imei::luhn_ok 一致）。
   仅作客户端提示，不阻断提交——部分厂商 IMEI 不严格符合 Luhn。 */
function imeiLuhnOk(v) {
	if (!/^[0-9]{15}$/.test(v))
		return false;
	var sum = 0;
	for (var i = 0; i < 15; i++) {
		var d = v.charCodeAt(i) - 48;
		if ((14 - i) % 2 === 1) {
			d *= 2;
			if (d > 9)
				d -= 9;
		}
		sum += d;
	}
	return sum % 10 === 0;
}

/* 后端调用统一走 fm350.api（rpcd ucode 代理 → fm350d） */
function notify(res, okMsg) {
	api.notify(res, okMsg);
}

/* ---------------- 移动网络二态开关 ---------------- */
function cfunLevel(raw) {
	if (typeof raw !== 'string')
		return null;

	var m = /\+CFUN:\s*(\d+)/.exec(raw);
	if (!m)
		return null;

	/* 官方 AT 手册 v2.2 §4.2：AT+CFUN? 返回的是 `+CFUN: <power_mode>,<STK_mode>`，
	   其中 <power_mode> 只有 1=开机 / 2=无效 / 4=飞行模式，**并非回显 <fun>**；
	   <fun> 的取值 0/1/4/5/6/7/8/15 只出现在 AT+CFUN=? 的测试响应里。
	   实测：下发 AT+CFUN=0（最小功能）后回读得到 4 —— 手册定义 4 即 Airplane mode，
	   故 4 必须映射为飞行态，否则用户切到飞行模式后刷新页面会看到「状态未知」。
	   0 与 15 属防御性冗余（个别固件可能直接回显 <fun>），其中 15 = Reset。 */
	if (m[1] === '0' || m[1] === '4')
		return false;
	if (m[1] === '1' || m[1] === '15')
		return true;
	return null;
}

/* 自绘二态开关：点击立即下发 AT+CFUN，下发期间锁定防止连点，
   失败回滚到点击前的状态。LuCI 的 form.Flag 语义是「保存后生效」，
   与「点击即下发」不符，因此这里自绘控件。 */
function createRadioSwitch(initial) {
	var state = { online: initial, busy: false, failed: false };

	var stateLabel = E('span', { 'class': 'fm350-switch-state' });
	var hint = E('span', { 'class': 'fm350-switch-hint' },
		_('开启＝在线模式，可上网与收发短信；关闭＝飞行模式，停止射频收发，节约功耗并降温'));
	var knob = E('span', { 'class': 'fm350-switch-knob' });
	var track = E('span', {
		'class': 'fm350-switch-track',
		'role': 'switch',
		'tabindex': '0',
		'aria-label': _('移动网络')
	}, [ knob ]);

	function paint() {
		var known = state.online === true || state.online === false;
		var on = state.online === true;

		track.classList.toggle('is-on', known && on);
		track.classList.toggle('is-unknown', !known);
		track.classList.toggle('is-busy', state.busy);
		track.setAttribute('aria-checked', known && on ? 'true' : 'false');
		track.setAttribute('aria-disabled', state.busy ? 'true' : 'false');

		stateLabel.classList.toggle('is-off', known && !on);

		if (state.busy)
			stateLabel.textContent = _('正在下发 AT+CFUN 指令，请稍候…');
		else if (!known)
			stateLabel.textContent = state.failed
				? _('未能读取当前状态（可点击开关尝试恢复在线）')
				: _('当前状态未知');
		else
			stateLabel.textContent = on
				? _('在线 · 射频正常收发')
				: _('飞行模式 · 已停用射频信号');
	}

	function toggle() {
		if (state.busy)
			return false;

		var target = state.online === true ? '0' : '1';
		var previous = state.online;

		state.busy = true;
		state.failed = false;
		paint();

		api.cfun(target).then(function(res) {
			state.busy = false;

			if (res && res.ok) {
				state.online = (target === '1');
				paint();
				ui.addNotification(null, E('p', target === '1'
					? _('已切换为在线模式，模组正在重新注册网络，信号稍后恢复')
					: _('已进入飞行模式，模组已停止收发信号')), 'success');
			} else {
				state.online = previous;
				state.failed = true;
				paint();
				ui.addNotification(null, E('p', _('切换失败：')
					+ ((res && res.error) || _('未知错误'))
					+ _('。若提示串口被占用，请到「AT 串口管理」确认端口状态')), 'danger');
			}
		});

		return false;
	}

	track.addEventListener('click', toggle);
	track.addEventListener('keydown', function(ev) {
		if (ev.key === 'Enter' || ev.key === ' ') {
			ev.preventDefault();
			toggle();
		}
	});

	paint();

	return E('div', { 'class': 'fm350-radio-switch' }, [
		track,
		E('div', { 'class': 'fm350-switch-meta' }, [ stateLabel, hint ])
	]);
}

/* ---------------- 动态矢量 SVG 渲染组件库 ---------------- */
function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

var SVG_ICONS = {
	// 守护进程：双动力同步啮合齿轮
	cogs: function(cls) {
		return parseSvg(`
			<svg class="fm350-svg-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<g class="fm350-anim-spin-cw">
					<circle cx="9" cy="9" r="3.2"/>
					<path d="M9 1.5v2.2M9 14.3v2.2M1.5 9h2.2M14.3 9h2.2M3.7 3.7l1.6 1.6M12.7 12.7l1.6 1.6M3.7 14.3l1.6-1.6M12.7 5.3l1.6-1.6" stroke-linecap="round"/>
				</g>
				<g class="fm350-anim-spin-ccw">
					<circle cx="17.2" cy="17.2" r="2.2"/>
					<path d="M17.2 12.8v1.4M17.2 20.2v1.4M12.8 17.2h1.4M20.2 17.2h1.4M14.1 14.1l1 1M19.3 19.3l1 1M14.1 20.3l1-1M19.3 15.1l1-1" stroke-linecap="round"/>
				</g>
			</svg>
		`);
	},
	// 串口通信：端口端子与动态脉冲数据流
	serial: function(cls) {
		return parseSvg(`
			<svg class="fm350-svg-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<rect x="2.5" y="5.5" width="19" height="13" rx="3.5"/>
				<circle cx="6.5" cy="12" r="1.5" fill="currentColor"/>
				<circle cx="12" cy="12" r="1.5" fill="currentColor"/>
				<circle cx="17.5" cy="12" r="1.5" fill="currentColor"/>
				<line class="fm350-anim-tx" x1="6.5" y1="1.5" x2="6.5" y2="5.5" stroke="currentColor" stroke-linecap="round"/>
				<line class="fm350-anim-rx" x1="17.5" y1="1.5" x2="17.5" y2="5.5" stroke="currentColor" stroke-linecap="round"/>
				<circle class="fm350-anim-pulse-point" cx="12" cy="12" r="3.2" stroke="currentColor" stroke-opacity="0.3"/>
			</svg>
		`);
	},
	// 模组控制：无线电基站天线与动态脉冲波
	radioMode: function(cls) {
		return parseSvg(`
			<svg class="fm350-svg-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 18v3M8 21h8" stroke-linecap="round"/>
				<circle cx="12" cy="15" r="2" fill="currentColor"/>
				<path class="fm350-anim-wave-1" d="M8.5 11.5a5 5 0 0 1 7 0" stroke-linecap="round"/>
				<path class="fm350-anim-wave-2" d="M5.5 8.5a9 9 0 0 1 13 0" stroke-linecap="round"/>
				<path class="fm350-anim-wave-3" d="M2.5 5.5a13.5 13.5 0 0 1 19 0" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 安全警示盾牌：高危锁闭与呼吸光环 (IMEI 维护)
	shieldDanger: function(cls) {
		return parseSvg(`
			<svg class="fm350-svg-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 2.5l7 3.2v6.1c0 5.4-3.5 9.8-7 11.2-3.5-1.4-7-5.8-7-11.2V5.7l7-3.2z"/>
				<line x1="12" y1="8" x2="12" y2="12.5" stroke-width="2" stroke-linecap="round"/>
				<circle class="fm350-anim-danger-blink" cx="12" cy="16.2" r="1.3" fill="currentColor"/>
			</svg>
		`);
	},
	// 硬件诊断芯片：微处理器与发光引脚
	diagnostic: function(cls) {
		return parseSvg(`
			<svg class="fm350-svg-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<rect x="5" y="5" width="14" height="14" rx="2.5"/>
				<rect class="fm350-anim-chip-die" x="9" y="9" width="6" height="6" rx="1" fill="currentColor" fill-opacity="0.15"/>
				<path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 端口雷达探测
	probe: function(cls) {
		return parseSvg(`
			<svg class="fm350-btn-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<circle cx="12" cy="12" r="9.5" stroke-opacity="0.3"/>
				<path class="fm350-anim-spin-cw" d="M12 2.5a9.5 9.5 0 0 1 9.5 9.5" stroke-linecap="round"/>
				<circle cx="12" cy="12" r="2.2" fill="currentColor"/>
			</svg>
		`);
	},
	// SIM 卡切换
	simSwitch: function(cls) {
		return parseSvg(`
			<svg class="fm350-btn-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M4 4.5A2.5 2.5 0 0 1 6.5 2H14l5 5v12.5a2.5 2.5 0 0 1-2.5 2.5h-10A2.5 2.5 0 0 1 4 19.5v-15z"/>
				<path d="M8 12h8M13 9l3 3-3 3" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		`);
	},
	// USB 模式切换
	usbMode: function(cls) {
		return parseSvg(`
			<svg class="fm350-btn-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<circle cx="12" cy="19" r="2" fill="currentColor"/>
				<path d="M12 17V5M12 5l-2.5 2.5M12 5l2.5 2.5"/>
				<path d="M12 12h4.5a1.5 1.5 0 0 0 1.5-1.5V9"/>
				<circle cx="18" cy="8" r="1.5"/>
				<path d="M12 14.5H7.5A1.5 1.5 0 0 1 6 13V11"/>
				<rect x="4.5" y="8" width="3" height="3"/>
			</svg>
		`);
	},
	// 模组重启 / 冷复位
	reboot: function(cls) {
		return parseSvg(`
			<svg class="fm350-btn-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l.67.75" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		`);
	},
	// 备份原厂 IMEI
	backup: function(cls) {
		return parseSvg(`
			<svg class="fm350-btn-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/>
				<polyline points="17 21 17 13 7 13 7 21"/>
				<polyline points="7 3 7 8 15 8"/>
			</svg>
		`);
	},
	// 下发写入 IMEI
	writeKey: function(cls) {
		return parseSvg(`
			<svg class="fm350-btn-icon ${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M11 4H4a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-7"/>
				<path d="M18.5 2.5a2.121 2.121 0 0 1 3 3L12 15l-4 1 1-4 9.5-9.5z"/>
			</svg>
		`);
	}
};

/* ---------------- 注入白色毛玻璃现代设计系统 ---------------- */
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
	var styleId = 'fm350-service-glass-styles';
	if (document.getElementById(styleId)) return;

	var css = `
		:root {
			/* 白色毛玻璃核心变量体系 (Light Glass Theme) */
			--fm-glass-bg: rgba(255, 255, 255, 0.76);
			--fm-glass-bg-hover: rgba(255, 255, 255, 0.92);
			--fm-glass-bg-card: rgba(255, 255, 255, 0.82);
			--fm-glass-border: rgba(255, 255, 255, 0.95);
			--fm-glass-border-subtle: rgba(226, 232, 240, 0.8);
			--fm-glass-border-focus: rgba(37, 99, 235, 0.45);
			--fm-glass-shadow: 0 10px 30px -4px rgba(15, 23, 42, 0.05), 0 4px 12px -2px rgba(15, 23, 42, 0.02);
			--fm-glass-shadow-hover: 0 16px 38px -4px rgba(37, 99, 235, 0.09), 0 6px 16px -2px rgba(15, 23, 42, 0.03);
			--fm-glass-blur: blur(22px) saturate(190%);

			/* 品牌与状态色彩 */
			--fm-primary: #2563eb;
			--fm-primary-dark: #1d4ed8;
			--fm-primary-gradient: linear-gradient(135deg, #2563eb 0%, #0ea5e9 100%);
			--fm-primary-glow: 0 6px 20px -2px rgba(37, 99, 235, 0.35);

			--fm-danger: #ef4444;
			--fm-danger-gradient: linear-gradient(135deg, #f43f5e 0%, #e11d48 100%);
			--fm-danger-glow: 0 6px 20px -2px rgba(225, 29, 72, 0.35);

			--fm-success: #10b981;
			--fm-warning: #f59e0b;

			--fm-text-title: #0f172a;
			--fm-text-body: #334155;
			--fm-text-muted: #64748b;
			--fm-text-light: #94a3b8;
		}

		/* 页面容器整体微调 */
		.fm350-service-page {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
			color: var(--fm-text-body);
			padding: 6px 0 36px 0;
			position: relative;
		}

		/* 顶部服务 Hero 看板 (Bento 态势展示) */
		.fm350-serv-hero {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.92) 0%, rgba(240, 246, 255, 0.85) 100%);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 22px;
			padding: 22px 28px;
			box-shadow: var(--fm-glass-shadow), inset 0 1px 1px 0 rgba(255, 255, 255, 0.95);
			display: flex;
			align-items: center;
			justify-content: space-between;
			flex-wrap: wrap;
			gap: 18px;
			margin-bottom: 24px;
			position: relative;
			overflow: hidden;
			transition: all 0.3s ease;
		}
		.fm350-serv-hero:hover {
			box-shadow: var(--fm-glass-shadow-hover);
			transform: translateY(-1px);
		}
		.fm350-serv-hero::after {
			content: "";
			position: absolute;
			top: -45px; right: -45px;
			width: 180px; height: 180px;
			background: radial-gradient(circle, rgba(14, 165, 233, 0.12) 0%, transparent 70%);
			border-radius: 50%;
			pointer-events: none;
		}
		.fm350-hero-left {
			display: flex;
			align-items: center;
			gap: 16px;
			z-index: 1;
		}
		.fm350-hero-avatar {
			width: 58px;
			height: 58px;
			border-radius: 17px;
			background: linear-gradient(135deg, rgba(37, 99, 235, 0.12) 0%, rgba(14, 165, 233, 0.16) 100%);
			border: 1px solid rgba(255, 255, 255, 0.9);
			color: var(--fm-primary);
			display: flex;
			align-items: center;
			justify-content: center;
			box-shadow: 0 8px 18px -4px rgba(37, 99, 235, 0.18);
			position: relative;
			flex-shrink: 0;
		}
		.fm350-hero-meta h3 {
			margin: 0 0 5px 0;
			font-size: 1.35rem;
			font-weight: 700;
			display: flex;
			align-items: center;
			gap: 10px;
			color: var(--fm-text-title);
			letter-spacing: -0.01em;
		}
		.fm350-hero-meta p {
			margin: 0;
			font-size: 0.84rem;
			color: var(--fm-text-muted);
			display: flex;
			align-items: center;
			gap: 6px;
			flex-wrap: wrap;
		}

		/* 状态胶囊徽标 */
		.fm350-status-pill {
			display: inline-flex;
			align-items: center;
			gap: 6px;
			padding: 3px 10px;
			border-radius: 999px;
			font-size: 0.74rem;
			font-weight: 600;
			background: rgba(16, 185, 129, 0.12);
			color: #047857;
			border: 1px solid rgba(16, 185, 129, 0.25);
		}
		.fm350-status-pill.is-offline {
			background: rgba(239, 68, 68, 0.12);
			color: #b91c1c;
			border-color: rgba(239, 68, 68, 0.25);
		}
		.fm350-dot-pulse {
			width: 7px;
			height: 7px;
			border-radius: 50%;
			background: currentColor;
			box-shadow: 0 0 0 2px rgba(16, 185, 129, 0.35);
			animation: fm350-pulse-ring 2s infinite ease-in-out;
		}
		.fm350-status-pill.is-offline .fm350-dot-pulse {
			box-shadow: 0 0 0 2px rgba(239, 68, 68, 0.35);
			animation: none;
		}

		/* Hero 右侧监控芯片 */
		.fm350-hero-chips {
			display: flex;
			align-items: center;
			gap: 10px;
			flex-wrap: wrap;
			z-index: 1;
		}
		.fm350-chip-badge {
			display: inline-flex;
			align-items: center;
			gap: 7px;
			background: rgba(255, 255, 255, 0.88);
			border: 1px solid rgba(226, 232, 240, 0.9);
			padding: 6px 14px;
			border-radius: 12px;
			font-size: 0.82rem;
			font-weight: 600;
			color: var(--fm-text-body);
			box-shadow: 0 2px 6px rgba(15, 23, 42, 0.03);
			transition: all 0.2s ease;
		}
		.fm350-chip-badge:hover {
			border-color: rgba(37, 99, 235, 0.3);
			background: #ffffff;
		}
		.fm350-chip-badge code {
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			color: var(--fm-primary);
			font-size: 0.84rem;
			background: rgba(37, 99, 235, 0.06);
			padding: 2px 6px;
			border-radius: 6px;
		}

		/* ---------------- 毛玻璃表单卡片 (Section) ---------------- */
		.fm350-service-page .cbi-map { margin: 0; }
		.fm350-service-page .cbi-map-descr {
			color: var(--fm-text-muted);
			font-size: 0.88rem;
			margin-bottom: 22px;
			padding-left: 4px;
			line-height: 1.5;
		}
		.fm350-service-page .cbi-section {
			background: var(--fm-glass-bg) !important;
			backdrop-filter: var(--fm-glass-blur) !important;
			-webkit-backdrop-filter: var(--fm-glass-blur) !important;
			border: 1px solid var(--fm-glass-border) !important;
			border-radius: 20px !important;
			padding: 24px 26px !important;
			box-shadow: var(--fm-glass-shadow), inset 0 1px 0 rgba(255, 255, 255, 0.95) !important;
			margin-bottom: 24px !important;
			transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1) !important;
			position: relative;
		}
		.fm350-service-page .cbi-section:hover {
			background: var(--fm-glass-bg-hover) !important;
			box-shadow: var(--fm-glass-shadow-hover), inset 0 1px 0 rgba(255, 255, 255, 0.95) !important;
		}

		/* 卡片标题与图标栏 */
		.fm350-service-page .cbi-section-legend {
			font-size: 1.1rem !important;
			font-weight: 700 !important;
			color: var(--fm-text-title) !important;
			border-bottom: 1px solid rgba(226, 232, 240, 0.7) !important;
			padding-bottom: 14px !important;
			margin-bottom: 16px !important;
			display: flex !important;
			align-items: center !important;
			gap: 10px !important;
			letter-spacing: -0.01em;
		}
		.fm350-legend-icon {
			width: 26px;
			height: 26px;
			border-radius: 8px;
			background: rgba(37, 99, 235, 0.08);
			color: var(--fm-primary);
			display: inline-flex;
			align-items: center;
			justify-content: center;
			flex-shrink: 0;
		}
		.fm350-legend-icon svg {
			width: 17px;
			height: 17px;
		}

		.fm350-service-page .cbi-section-descr {
			font-size: 0.83rem !important;
			color: var(--fm-text-muted) !important;
			margin-bottom: 18px !important;
			line-height: 1.55 !important;
		}

		/* 表单项行样式 */
		.fm350-service-page .cbi-value {
			padding: 12px 0 !important;
			border-bottom: 1px dashed rgba(226, 232, 240, 0.65) !important;
			display: flex;
			align-items: center;
			flex-wrap: wrap;
			transition: background-color 0.15s ease;
		}
		.fm350-service-page .cbi-value:hover {
			background-color: rgba(248, 250, 252, 0.5);
			border-radius: 8px;
		}
		.fm350-service-page .cbi-value:last-child {
			border-bottom: none !important;
		}
		.fm350-service-page .cbi-value-title {
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			color: #334155 !important;
			width: 26% !important;
			min-width: 140px !important;
		}
		.fm350-service-page .cbi-value-field {
			flex: 1 !important;
		}

		/* ---------------- IMEI 高危区域专属毛玻璃样式 ---------------- */
		.fm350-danger-card {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.90) 0%, rgba(255, 241, 242, 0.84) 100%) !important;
			border: 1px solid rgba(244, 63, 94, 0.3) !important;
			box-shadow: 0 10px 30px -4px rgba(225, 29, 72, 0.06), inset 0 1px 0 rgba(255, 255, 255, 0.95) !important;
		}
		.fm350-danger-card::before {
			content: "";
			position: absolute;
			top: 0; left: 0; right: 0;
			height: 3.5px;
			background: var(--fm-danger-gradient);
			border-radius: 20px 20px 0 0;
		}
		.fm350-danger-card .fm350-legend-icon {
			background: rgba(244, 63, 94, 0.12);
			color: #e11d48;
		}

		/* 冷复位危险行隔离 */
		.fm350-danger-row {
			background: linear-gradient(135deg, rgba(255, 251, 245, 0.94), rgba(255, 241, 242, 0.88)) !important;
			border: 1px solid rgba(239, 68, 68, 0.25) !important;
			border-left: 4px solid var(--fm-danger) !important;
			border-radius: 14px !important;
			padding: 14px 16px !important;
			margin-top: 12px !important;
		}

		/* 输入框与选择控件优化 */
		.fm350-service-page input[type="text"],
		.fm350-service-page select {
			background: rgba(255, 255, 255, 0.92) !important;
			border: 1px solid var(--fm-glass-border-subtle) !important;
			border-radius: 11px !important;
			padding: 8px 14px !important;
			font-size: 0.88rem !important;
			color: #1e293b !important;
			transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1) !important;
			outline: none !important;
			box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02) !important;
		}
		.fm350-service-page input[type="text"]:focus,
		.fm350-service-page select:focus {
			border-color: var(--fm-primary) !important;
			box-shadow: 0 0 0 3.5px rgba(37, 99, 235, 0.15), 0 2px 8px rgba(37, 99, 235, 0.08) !important;
			background: #ffffff !important;
		}
		.fm350-service-page .cbi-value-description {
			font-size: 0.79rem !important;
			color: var(--fm-text-muted) !important;
			margin-top: 6px !important;
			line-height: 1.45 !important;
		}

		/* ---------------- 现代化按钮体系 (美观毛玻璃与交互) ---------------- */
		.fm350-service-page .cbi-button {
			display: inline-flex !important;
			align-items: center !important;
			justify-content: center !important;
			gap: 7px !important;
			padding: 8px 18px !important;
			border-radius: 12px !important;
			font-size: 0.86rem !important;
			font-weight: 600 !important;
			cursor: pointer !important;
			transition: all 0.2s cubic-bezier(0.16, 1, 0.3, 1) !important;
			text-decoration: none !important;
			user-select: none !important;
			border: none !important;
		}

		/* 主要操作按钮 (蓝色渐变) */
		.fm350-service-page .cbi-button-apply {
			background: var(--fm-primary-gradient) !important;
			color: #ffffff !important;
			box-shadow: var(--fm-primary-glow) !important;
		}
		.fm350-service-page .cbi-button-apply:hover {
			filter: brightness(1.08) !important;
			transform: translateY(-1.5px) !important;
			box-shadow: 0 8px 24px -2px rgba(37, 99, 235, 0.42) !important;
		}
		.fm350-service-page .cbi-button-apply:active {
			transform: translateY(0.5px) scale(0.98) !important;
		}

		/* 危险 / 复位按钮 (玫瑰红渐变与警示毛玻璃) */
		.fm350-service-page .cbi-button-reset {
			background: linear-gradient(135deg, rgba(254, 242, 242, 0.95), rgba(255, 228, 230, 0.95)) !important;
			border: 1px solid rgba(244, 63, 94, 0.35) !important;
			color: #e11d48 !important;
			box-shadow: 0 2px 8px rgba(225, 29, 72, 0.08) !important;
		}
		.fm350-service-page .cbi-button-reset:hover {
			background: var(--fm-danger-gradient) !important;
			color: #ffffff !important;
			border-color: transparent !important;
			transform: translateY(-1.5px) !important;
			box-shadow: var(--fm-danger-glow) !important;
		}
		.fm350-service-page .cbi-button-reset:active {
			transform: translateY(0.5px) scale(0.98) !important;
		}

		/* 按钮内部内联 SVG 微标 */
		.fm350-btn-icon {
			width: 15px;
			height: 15px;
			flex-shrink: 0;
			vertical-align: -2px;
		}

		/* 状态微徽章 (AT Hold Badge) */
		.fm350-hold-badge {
			display: inline-block;
			background: rgba(255, 255, 255, 0.9);
			border: 1px solid rgba(203, 213, 225, 0.8);
			border-radius: 10px;
			padding: 8px 14px;
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			font-size: 0.82rem;
			color: #334155;
			line-height: 1.6;
			box-shadow: 0 2px 6px rgba(0, 0, 0, 0.02);
		}
		.fm350-hold-badge.is-warn {
			background: rgba(254, 242, 242, 0.95);
			border-color: rgba(239, 68, 68, 0.4);
			color: #b91c1c;
		}

		/* ---------------- SVG 动效关键帧 ---------------- */
		.fm350-svg-icon {
			width: 22px;
			height: 22px;
			display: block;
		}
		@keyframes fm350-spin-clockwise {
			from { transform: rotate(0deg); }
			to { transform: rotate(360deg); }
		}
		@keyframes fm350-spin-c-clockwise {
			from { transform: rotate(0deg); }
			to { transform: rotate(-360deg); }
		}
		.fm350-anim-spin-cw {
			transform-origin: 9px 9px;
			animation: fm350-spin-clockwise 7.5s linear infinite;
		}
		.fm350-anim-spin-ccw {
			transform-origin: 17.2px 17.2px;
			animation: fm350-spin-c-clockwise 5s linear infinite;
		}

		/* 串口收发数据流动效 */
		@keyframes fm350-data-tx {
			0%, 100% { opacity: 0.3; transform: translateY(0); }
			50% { opacity: 1; transform: translateY(2px); stroke: #0ea5e9; }
		}
		@keyframes fm350-data-rx {
			0%, 100% { opacity: 0.3; transform: translateY(0); }
			50% { opacity: 1; transform: translateY(-2px); stroke: #2563eb; }
		}
		.fm350-anim-tx { animation: fm350-data-tx 1.3s infinite ease-in-out; }
		.fm350-anim-rx { animation: fm350-data-rx 1.3s infinite ease-in-out 0.65s; }
		@keyframes fm350-pulse-pt {
			0%, 100% { transform: scale(0.9); opacity: 0.2; }
			50% { transform: scale(1.3); opacity: 0.8; }
		}
		.fm350-anim-pulse-point {
			transform-origin: 12px 12px;
			animation: fm350-pulse-pt 2s infinite ease-in-out;
		}

		/* 射频天线广播波 */
		@keyframes fm350-wave-pulse {
			0% { opacity: 0.2; }
			50% { opacity: 1; stroke: #2563eb; }
			100% { opacity: 0.2; }
		}
		.fm350-anim-wave-1 { animation: fm350-wave-pulse 2s infinite ease-in-out; }
		.fm350-anim-wave-2 { animation: fm350-wave-pulse 2s infinite ease-in-out 0.4s; }
		.fm350-anim-wave-3 { animation: fm350-wave-pulse 2s infinite ease-in-out 0.8s; }

		/* 危险闪烁 */
		@keyframes fm350-danger-blink {
			0%, 100% { opacity: 0.35; }
			50% { opacity: 1; filter: drop-shadow(0 0 4px #ef4444); }
		}
		.fm350-anim-danger-blink { animation: fm350-danger-blink 1.8s ease-in-out infinite; }

		@keyframes fm350-pulse-ring {
			0%, 100% { box-shadow: 0 0 0 0 rgba(16, 185, 129, 0.4); }
			50% { box-shadow: 0 0 0 4px rgba(16, 185, 129, 0); }
		}

		/* ---------------- 移动网络二态开关 (iOS/macOS 白色毛玻璃风格) ---------------- */
		.fm350-radio-switch {
			display: flex;
			align-items: center;
			gap: 16px;
			flex-wrap: wrap;
		}
		.fm350-switch-track {
			position: relative;
			flex: none;
			box-sizing: border-box;
			width: 56px;
			height: 32px;
			border-radius: 999px;
			background: rgba(148, 163, 184, 0.38);
			border: 1px solid rgba(148, 163, 184, 0.55);
			cursor: pointer;
			outline: none;
			transition: all 0.25s cubic-bezier(0.16, 1, 0.3, 1);
			box-shadow: inset 0 2px 4px rgba(0, 0, 0, 0.05);
		}
		.fm350-switch-track.is-on {
			background: var(--fm-primary-gradient);
			border-color: rgba(37, 99, 235, 0.85);
			box-shadow: 0 4px 14px -2px rgba(37, 99, 235, 0.38), inset 0 1px 1px rgba(255, 255, 255, 0.4);
		}
		.fm350-switch-track.is-unknown {
			border-style: dashed;
		}
		.fm350-switch-track.is-busy {
			cursor: progress;
			opacity: 0.65;
		}
		.fm350-switch-track:focus-visible {
			box-shadow: 0 0 0 3.5px rgba(37, 99, 235, 0.28);
		}
		.fm350-switch-knob {
			position: absolute;
			top: 3.5px;
			left: 4px;
			width: 23px;
			height: 23px;
			border-radius: 50%;
			background: #ffffff;
			box-shadow: 0 2px 6px rgba(15, 23, 42, 0.25);
			transition: transform 0.25s cubic-bezier(0.16, 1, 0.3, 1);
		}
		.fm350-switch-track.is-on .fm350-switch-knob {
			transform: translateX(25px);
		}
		.fm350-switch-meta {
			display: flex;
			flex-direction: column;
			gap: 3px;
			min-width: 0;
		}
		.fm350-switch-state {
			font-size: 0.88rem;
			font-weight: 600;
			color: #334155;
		}
		.fm350-switch-state.is-off {
			color: #b45309;
		}
		.fm350-switch-hint {
			font-size: 0.78rem;
			color: var(--fm-text-muted);
		}

		/* ---------------- 移动端与响应式适配 ---------------- */
		@media (max-width: 767px) {
			.fm350-serv-hero { padding: 18px 20px; gap: 14px; }
			.fm350-service-page .cbi-section { padding: 18px 18px !important; }
			.fm350-service-page .cbi-value-title { width: 100% !important; min-width: 0 !important; margin-bottom: 6px; }
			.fm350-service-page .cbi-section-legend { flex-wrap: wrap; }
			.fm350-hero-meta h3 { flex-wrap: wrap; }
			.fm350-hold-badge { word-break: break-all; }
			.fm350-service-page .cbi-button { width: 100%; margin-top: 4px; }
		}
		@media (max-width: 480px) {
			.fm350-serv-hero { padding: 14px 16px; border-radius: 16px; }
			.fm350-service-page .cbi-section { padding: 15px 14px !important; border-radius: 16px !important; }
			.fm350-service-page .cbi-map-descr { margin-bottom: 14px; }
		}
	`;

	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('fm350'),
			api.info(),
			api.imeiRead(),
			api.ports('1'),
			api.at('AT+CFUN?')
		]);
	},

	render: function(data) {
		api.injectCss();
		injectFrostedGlassTheme();

		var info = (data[1] && data[1].ok && data[1].value) || {};
		var imei = (data[2] && data[2].ok && data[2].value) || {};
		var pdata = (data[3] && data[3].ok && data[3].value) || {};
		var portList = pdata.ports || [];
		var atStats = pdata.stats || {};
		var curPort = pdata.current
			|| uci.get('fm350', 'main', 'at_port')
			|| '/dev/ttyUSB1';
		var writeEnabled = ('' + uci.get('fm350', 'main', 'imei_write')) === '1'
			|| imei.write_enabled === true;

		var isExclOk = atStats.open === true && (!atStats.other_pids || atStats.other_pids.length === 0);
		var cfunNow = cfunLevel((data[4] && data[4].ok && data[4].value) || null);

		var m, s, o;

		m = new form.Map('fm350', _('FM350 服务设置'),
			_('守护进程 fm350d 核心运行参数与硬件交互。除「本地 API 端口」需重启服务生效外，其余修改保存后即时生效。'));

		/* ---------------- 一、服务设置 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('守护进程设置'));
		s.anonymous = false;

		o = s.option(form.Flag, 'enabled', _('启用守护进程'),
			_('关闭后将停止后台周期巡检与断线重拨逻辑，LuCI 页面仍可手动下发指令'));
		o.default = '1';

		o = s.option(form.Value, 'poll_interval', _('巡检周期（秒）'),
			_('守护进程巡检 PDP 驻网状态与补齐默认路由的间隔，最小设定 5 秒'));
		o.datatype = 'and(uinteger,min(5))';
		o.default = '30';

		o = s.option(form.Value, 'api_port', _('本地 API 端口'),
			_('仅监听 127.0.0.1 供 rpcd ucode 代理调用，默认 8766，不对外网及局域网开放'));
		o.datatype = 'and(port,min(1))';
		o.default = '8766';

		/* ---------------- 二、IPv6 前缀委派 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('IPv6 前缀委派'));
		s.description = _('蜂窝运营商通常只下发一个 /64 前缀。'
			+ '开启委派后该前缀会分配给 LAN，局域网设备也能获得 IPv6 地址；'
			+ '关闭则只有路由器自身的蜂窝接口持有 IPv6。'
			+ '修改保存后需重新拨号（或重启服务）才会生效。');
		s.anonymous = false;

		o = s.option(form.Flag, 'ipv6', _('启用 IPv6 子接口'),
			_('关闭后不再创建 fm350v6 子接口，蜂窝侧仅使用 IPv4'));
		o.default = '1';
		o.rmempty = false;

		o = s.option(form.Flag, 'extendprefix', _('向下委派 IPv6 前缀'),
			_('把上行 /64 前缀分配给 LAN（netifd extendprefix）。'
				+ '仅当运营商确实下发前缀时才会生效'));
		o.default = '1';
		o.rmempty = false;
		o.depends('ipv6', '1');

		o = s.option(form.Value, 'v6_poll_interval', _('IPv6 模组轮询周期（秒）'),
			_('守护进程会按此周期通过 AT+CGPADDR/AT+CGCONTRDP 读取模组侧最新 IPv6；发现模组侧 IPv6 变化时会刷新 IPv6 子接口。设为 0 可关闭独立 V6 轮询'));
		o.datatype = 'uinteger';
		o.validate = function(section_id, value) {
			var n = parseInt(value, 10);
			if (/^\d+$/.test(value) && (n === 0 || n >= 60))
				return true;
			return _('请输入 0，或不小于 60 的整数秒数');
		};
		o.default = '300';
		o.rmempty = false;
		o.depends('ipv6', '1');

		o = s.option(form.Value, 'v6_refresh_interval', _('IPv6 定时刷新周期（秒）'),
			_('守护进程会按此周期刷新 IPv6 子接口，避免 RA/DHCPv6 状态异常导致地址过期后不恢复；设为 0 可关闭定时刷新。即使关闭，巡检发现没有有效全局 IPv6 时仍会自动刷新'));
		o.datatype = 'uinteger';
		o.validate = function(section_id, value) {
			var n = parseInt(value, 10);
			if (/^\d+$/.test(value) && (n === 0 || n >= 60))
				return true;
			return _('请输入 0，或不小于 60 的整数秒数');
		};
		o.default = '1800';
		o.rmempty = false;
		o.depends('ipv6', '1');

		/* ---------------- 三、AT 串口管理 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('AT 串口管理'));
		s.description = _('插件独占 AT 口：fm350d 运行期间持续持有该端口并申请排他锁，'
			+ '其它申请排他锁的程序将无法并发争抢；若有进程绕过锁强开，下方状态栏将直接列出其 PID。'
			+ '请选择模组实际导出的 AT 口，通常为 /dev/ttyUSB1 或 /dev/ttyUSB2；更改端口保存即生效。');

		function portLabel(p) {
			var parts = [];
			if (p.driver) parts.push(p.driver);
			if (p.vid && p.pid) parts.push(p.vid + ':' + p.pid);
			if (p.product) parts.push(p.product);

			var tail = parts.length ? ' · ' + parts.join(' · ') : '';
			var state = '';

			if (p.current)
				state = _('（当前配置）');
			else if (p.busy === true)
				state = _('（被其他程序占用）');
			else if (p.busy === false)
				state = _('（空闲可用）');

			if (p.likely_fm350 && !p.current)
				state += _(' [疑似 FM350]');

			return p.path + tail + state;
		}

		o = s.option(form.ListValue, 'at_port', _('AT 端口选择'),
			_('候选端口来自后端扫描 /dev/ttyUSB* 与 /dev/ttyACM*，带内核驱动标识与占用探测'));
		o.default = '/dev/ttyUSB1';
		o.rmempty = false;
		o.cfgvalue = function(section_id) {
			return uci.get('fm350', section_id, 'at_port') || '/dev/ttyUSB1';
		};

		var seen = {};
		portList.forEach(function(p) {
			o.value(p.path, portLabel(p));
			seen[p.path] = true;
		});

		if (!seen[curPort])
			o.value(curPort, curPort + _('（当前配置）'));

		o.value('__custom__', _('手动输入其他路径…'));

		o.write = function(section_id, formvalue) {
			if (formvalue === '__custom__')
				formvalue = (this.section.getOption('_at_port_manual').formvalue(section_id) || '').trim();

			if (!formvalue)
				return;

			uci.set('fm350', section_id, 'at_port', formvalue);
		};

		o = s.option(form.Value, '_at_port_manual', _('自定义 AT 端口'),
			_('仅在上方选择「手动输入其他路径…」时生效。填写绝对路径，例如 /dev/ttyUSB2'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};
		o.depends('at_port', '__custom__');

		o = s.option(form.Button, '_port_probe', _('端口占用探测'));
		o.inputtitle = _('重新探测扫描');
		o.inputstyle = 'apply';
		o.description = _('主动触发后端扫描候选 AT 端口与占用状态。不会修改已保存的配置项');
		o.onclick = function() {
			return api.ports('1').then(function(res) {
				if (!res || !res.ok) {
					ui.addNotification(null,
						E('p', _('探测失败：') + ((res && res.error) || _('未知错误'))), 'danger');
					return;
				}

				var list = (res.ports || []).map(function(p) {
					var st = p.current ? _('当前配置')
						: (p.busy === true ? _('被占用') : (p.busy === false ? _('空闲') : _('未探测')));
					return p.path + '（' + (p.driver || _('未知驱动')) + '，' + st + '）';
				});

				ui.addNotification(null, E('p', _('探测完成：')
					+ (list.length ? list.join('；') : _('未发现候选端口'))), 'info');
			});
		};

		o = s.option(form.DummyValue, '_at_hold', _('AT 口当前状态'));
		o.description = _('实时遥测：持续排他独占持有时长、打开与释放计数，以及是否存在并发冲突进程');
		o.rawhtml = true;
		o.cfgvalue = function() {
			var hold = atStats.open === true
				? (_('已独占持有时长：') + (atStats.held_secs || 0) + _(' 秒'))
				: _('尚未打开（等待首次通信请求激活）');

			var others = atStats.other_pids || [];
			var isWarn = others.length > 0;
			var excl = atStats.open !== true ? ''
				: (others.length
					? _(' ｜ 警告：另有 ') + others.length + _(' 个进程占用中（PID: ') + others.join('、') + _('），独占已被破坏！')
					: _(' ｜ 独占有效：无冲突进程'));

			var statsText = _(' ｜ 累计打开 ') + (atStats.opens || 0) + _(' 次 / 释放 ') + (atStats.releases || 0) + _(' 次');
			return E('div', { 'class': 'fm350-hold-badge' + (isWarn ? ' is-warn' : '') }, hold + excl + statsText);
		};

		o = s.option(form.ListValue, 'baudrate', _('串行波特率'));
		[9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600].forEach(function(b) {
			o.value(String(b), String(b));
		});
		o.default = '115200';

		o = s.option(form.Value, 'at_timeout', _('指令超时上限（秒）'));
		o.datatype = 'and(uinteger,min(1))';
		o.default = '10';

		/* ---------------- 四、模组控制指令 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('模组控制指令'));
		s.description = _('日常运维常用控制。「移动网络」开关点击即时生效；切换 USB 模式后模组需重新枚举，耗时约 20–30 秒。');

		o = s.option(form.DummyValue, '_radio', _('移动网络'));
		o.rawhtml = true;
		o.description = _('开启＝在线模式（CFUN=1），关闭＝飞行模式（CFUN=0）。点击开关立即下发生效，无需保存配置');
		o.cfgvalue = function() { return createRadioSwitch(cfunNow); };

		o = s.option(form.ListValue, '_sim', _('SIM 卡槽'));
		o.description = _('选择物理卡槽或板载 eSIM 后，点击右侧按钮下发指令');
		o.value('0', _('卡槽 0（物理 SIM 卡）'));
		o.value('1', _('卡槽 1（板载 eSIM）'));
		o.cfgvalue = function() { return null; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_sim', _('切换 SIM 卡槽'));
		o.inputtitle = _('切换卡槽');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var slot = this.section.getOption('_sim').formvalue(this.section.section) || '0';
			return api.sim(slot).then(function(res) {
				notify(res, _('SIM 卡槽切换指令已下发'));
			});
		};

		var usbNow = ('' + (info.usb_mode || '')).trim();

		o = s.option(form.ListValue, '_usb', _('USB 接口模式'));
		o.description = _('模组 USB 端口组合配置。模式 40 为标准组合（上网 + AT 管理 + GNSS 定位）；'
			+ '模式 41 额外开放模组日志与元数据通道，适用于深度排障。写入后模组将重新枚举。');
		o.value('40', _('标准模式（推荐）· 上网 + 管理 + 定位'));
		o.value('41', _('诊断模式 · 标准模式 + 模组底层日志 / 元数据'));
		if (usbNow !== '' && usbNow !== '40' && usbNow !== '41')
			o.value(usbNow, _('当前配置 ') + usbNow + _('（自定义组合）'));
		o.cfgvalue = function() { return usbNow !== '' ? usbNow : '40'; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_usb', _('应用 USB 模式'));
		o.inputtitle = _('下发模式切换');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var mode = this.section.getOption('_usb').formvalue(this.section.section) || '';

			if (!mode)
				return ui.addNotification(null, E('p', _('请先选择要切换到的 USB 模式')), 'warning');
			if (mode === usbNow)
				return ui.addNotification(null, E('p', _('当前已处于该模式，无需重复下发')), 'info');

			return api.usbmode(mode).then(function(res) {
				notify(res, _('USB 模式已切换，模组正在重新枚举，约 20–30 秒后恢复通信'));
			});
		};

		o = s.option(form.Button, '_reboot', _('模组冷复位'));
		o.inputtitle = _('执行模组重启');
		o.inputstyle = 'reset';
		o.description = _('下发 AT+CFUN=1,1 指令，模组底层软重启并重新寻网注册。约中断联网 30 秒。');
		o.onclick = function() {
			return api.reboot().then(function(res) { notify(res, _('重启指令已下发')); });
		};

		/* ---------------- 五、IMEI / 设备识别码维护 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('IMEI / 设备识别码维护'));
		s.description = _('安全警告：写入 IMEI 属于高风险底层操作，不当变更可能导致基站鉴权失败或拒绝入网。'
			+ '仅限设备维修与合法原厂串号恢复使用。默认锁定保护。');

		o = s.option(form.Flag, 'imei_write', _('解锁 IMEI 写入权限'),
			_('默认锁定。开启并保存后，仍需在下方完成二次知情确认方可下发写入'));
		o.default = '0';

		o = s.option(form.DummyValue, '_imei_now', _('当前生效 IMEI'));
		o.cfgvalue = function() { return imei.imei || _('读取失败'); };

		o = s.option(form.DummyValue, '_imei_backup', _('已有自动备份'));
		o.cfgvalue = function() { return imei.backup || _('暂无备份文件'); };

		o = s.option(form.Value, '_imei_new', _('设定新 IMEI 串号'),
			_('请输入 15 位数字。前端将自动进行 Luhn 校验位复核'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Flag, '_imei_confirm', _('我已知晓并确认风险'),
			_('勾选此项确认已妥善备份原厂串号，并自愿承担操作风险'));
		o.cfgvalue = function() { return '0'; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_imei', _('下发写入'));
		o.inputtitle = _('执行写入新 IMEI');
		o.inputstyle = 'reset';
		o.onclick = function() {
			var sec = this.section.section;
			var value = (this.section.getOption('_imei_new').formvalue(sec) || '').trim();
			var confirmed = this.section.getOption('_imei_confirm').formvalue(sec);

			if (!/^[0-9]{15}$/.test(value))
				return ui.addNotification(null, E('p', _('IMEI 必须为 15 位连续数字')), 'warning');
			if (!writeEnabled)
				return ui.addNotification(null,
					E('p', _('请先在上方勾选“解锁 IMEI 写入权限”并点击页面底部保存配置')), 'warning');
			if (!confirmed)
				return ui.addNotification(null,
					E('p', _('请先勾选“我已知晓并确认风险”二次确认复选框')), 'warning');

			if (!imeiLuhnOk(value))
				ui.addNotification(null,
					E('p', _('警告：输入的 IMEI 未通过 Luhn 校验位算法计算，运营商核心网可能拒绝入网，请务必核对！')),
					'warning');

			return api.imeiWrite(value, '1').then(function(res) {
				notify(res, _('IMEI 写入指令已下发'));
				if (!res || !res.ok)
					return;

				var warn = res.value && res.value.warning;
				if (warn)
					ui.addNotification(null, E('p', _(warn)), 'warning');

				setTimeout(function() { window.location.reload(); }, warn ? 3200 : 1500);
			});
		};

		o = s.option(form.Button, '_btn_imei_backup', _('手动备份原厂 IMEI'));
		o.inputtitle = _('立即备份当前串号');
		o.inputstyle = 'apply';
		o.onclick = function() {
			return api.imeiBackup().then(function(res) {
				notify(res, _('原厂 IMEI 备份成功'));
				if (res && res.ok)
					setTimeout(function() { window.location.reload(); }, 800);
			});
		};

		/* ---------------- 六、模组硬件诊断信息 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('模组硬件诊断信息'));
		s.description = _('模组底层固件版本与生效参数，只读显示');

		o = s.option(form.DummyValue, '_imei', _('IMEI 标识'));
		o.cfgvalue = function() { return info.imei || '-'; };

		o = s.option(form.DummyValue, '_fw', _('模组基带固件'));
		o.cfgvalue = function() { return info.firmware || '-'; };

		o = s.option(form.DummyValue, '_usbnow', _('当前生效 USB 模式'));
		o.cfgvalue = function() { return info.usb_mode || '-'; };

		/* 渲染整体容器并前置插入 Hero 看板与动效增强 */
		return m.render().then(function(mapNode) {
			var sections = mapNode.querySelectorAll('.cbi-section');

			// Section 0: 守护进程设置
			if (sections[0]) {
				var leg0 = sections[0].querySelector('.cbi-section-legend');
				if (leg0) {
					var icon0 = E('span', { 'class': 'fm350-legend-icon' }, [ SVG_ICONS.cogs() ]);
					leg0.insertBefore(icon0, leg0.firstChild);
				}
			}

			// Section 1: AT 串口管理
			if (sections[1]) {
				var leg1 = sections[1].querySelector('.cbi-section-legend');
				if (leg1) {
					var icon1 = E('span', { 'class': 'fm350-legend-icon' }, [ SVG_ICONS.serial() ]);
					leg1.insertBefore(icon1, leg1.firstChild);
				}
			}

			// Section 2: 模组控制指令
			if (sections[2]) {
				var leg2 = sections[2].querySelector('.cbi-section-legend');
				if (leg2) {
					var icon2 = E('span', { 'class': 'fm350-legend-icon' }, [ SVG_ICONS.radioMode() ]);
					leg2.insertBefore(icon2, leg2.firstChild);
				}
			}

			// Section 3: IMEI 维护 (高危卡片样式注入)
			if (sections[3]) {
				sections[3].classList.add('fm350-danger-card');
				var leg3 = sections[3].querySelector('.cbi-section-legend');
				if (leg3) {
					var icon3 = E('span', { 'class': 'fm350-legend-icon' }, [ SVG_ICONS.shieldDanger() ]);
					leg3.insertBefore(icon3, leg3.firstChild);
				}
			}

			// Section 4: 模组硬件诊断信息
			if (sections[4]) {
				var leg4 = sections[4].querySelector('.cbi-section-legend');
				if (leg4) {
					var icon4 = E('span', { 'class': 'fm350-legend-icon' }, [ SVG_ICONS.diagnostic() ]);
					leg4.insertBefore(icon4, leg4.firstChild);
				}
			}

			// 冷复位危险行视觉隔离
			var rebootAnchor = mapNode.querySelector('[id$="-_reboot"]');
			if (rebootAnchor) {
				var rebootRow = rebootAnchor.closest('.cbi-value');
				if (rebootRow)
					rebootRow.classList.add('fm350-danger-row');
			}

			// 为各个操作按钮注入动态 SVG 图标
			var btnProbe = mapNode.querySelector('[id$="-_port_probe"] button, [id$="-_port_probe"] input[type="button"]');
			if (btnProbe && btnProbe.tagName === 'BUTTON') {
				btnProbe.insertBefore(SVG_ICONS.probe(), btnProbe.firstChild);
			}

			var btnSim = mapNode.querySelector('[id$="-_btn_sim"] button');
			if (btnSim) {
				btnSim.insertBefore(SVG_ICONS.simSwitch(), btnSim.firstChild);
			}

			var btnUsb = mapNode.querySelector('[id$="-_btn_usb"] button');
			if (btnUsb) {
				btnUsb.insertBefore(SVG_ICONS.usbMode(), btnUsb.firstChild);
			}

			var btnReboot = mapNode.querySelector('[id$="-_reboot"] button');
			if (btnReboot) {
				btnReboot.insertBefore(SVG_ICONS.reboot(), btnReboot.firstChild);
			}

			var btnImeiWrite = mapNode.querySelector('[id$="-_btn_imei"] button');
			if (btnImeiWrite) {
				btnImeiWrite.insertBefore(SVG_ICONS.writeKey(), btnImeiWrite.firstChild);
			}

			var btnImeiBackup = mapNode.querySelector('[id$="-_btn_imei_backup"] button');
			if (btnImeiBackup) {
				btnImeiBackup.insertBefore(SVG_ICONS.backup(), btnImeiBackup.firstChild);
			}

			var isDaemonActive = uci.get('fm350', 'main', 'enabled') !== '0';

			var heroBanner = E('div', { 'class': 'fm350-serv-hero' }, [
				E('div', { 'class': 'fm350-hero-left' }, [
					E('div', { 'class': 'fm350-hero-avatar' }, [ SVG_ICONS.cogs() ]),
					E('div', { 'class': 'fm350-hero-meta' }, [
						E('h3', {}, [
							_('FM350D 守护进程'),
							isDaemonActive
								? E('span', { 'class': 'fm350-status-pill' }, [
									E('span', { 'class': 'fm350-dot-pulse' }),
									_('运行正常')
								  ])
								: E('span', { 'class': 'fm350-status-pill is-offline' }, [
									E('span', { 'class': 'fm350-dot-pulse' }),
									_('已停用')
								  ])
						]),
						E('p', {}, [
							E('span', {}, _('巡检周期：') + (uci.get('fm350', 'main', 'poll_interval') || '30') + _(' 秒')),
							E('span', { 'style': 'opacity:0.5;' }, '•'),
							E('span', {}, _('V6 轮询：') + (uci.get('fm350', 'main', 'v6_poll_interval') || '300') + _(' 秒')),
							E('span', { 'style': 'opacity:0.5;' }, '•'),
							E('span', {}, _('V6 刷新：') + (uci.get('fm350', 'main', 'v6_refresh_interval') || '1800') + _(' 秒')),
							E('span', { 'style': 'opacity:0.5;' }, '•'),
							E('span', {}, _('API 端口：') + (uci.get('fm350', 'main', 'api_port') || '8766'))
						])
					])
				]),
				E('div', { 'class': 'fm350-hero-chips' }, [
					E('div', { 'class': 'fm350-chip-badge' }, [
						_('AT 串口'),
						E('code', {}, curPort)
					]),
					E('div', { 'class': 'fm350-chip-badge' }, [
						_('排他锁'),
						isExclOk
							? E('span', { 'style': 'color:#10b981;font-weight:700;' }, _('独占正常'))
							: E('span', { 'style': 'color:#f59e0b;font-weight:700;' }, _('状态注意'))
					]),
					E('div', { 'class': 'fm350-chip-badge' }, [
						_('基带固件'),
						E('code', {}, info.firmware || '-')
					])
				])
			]);

			return E('div', { 'class': 'fm350-service-page' }, [
				heroBanner,
				mapNode
			]);
		});
	}
});
