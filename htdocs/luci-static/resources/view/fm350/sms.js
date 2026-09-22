'use strict';
'require view';
'require ui';
'require fm350.api as api';

/* 短信读写全部经 luci.fm350.sms 对象（rpcd ucode 代理 → fm350d） */
function notify(res, okMsg) {
	api.notify(res, okMsg);
}

var statusText = {
	0: _('未读短信'),
	1: _('已读短信'),
	2: _('草稿/未发'),
	3: _('已发送')
};

/* ---------------- 动态 SVG 矢量组件 ---------------- */
function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

var SVG_ICONS = {
	// 动态浮动与轨迹纸飞机
	paperPlane: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path class="fm350-anim-plane-bob" d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z" stroke-linecap="round" stroke-linejoin="round"/>
				<path class="fm350-anim-trail" d="M2 19c2 0 4-1 5-3" stroke-dasharray="2 2" stroke-linecap="round"/>
			</svg>
		`);
	},
	// SIM 卡存储芯片脉冲
	storageSim: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M6 2h9l5 5v13a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z"/>
				<rect class="fm350-anim-pulse-subtle" x="8" y="10" width="8" height="8" rx="1.5" fill="currentColor" fill-opacity="0.15"/>
				<path d="M12 10v8M8 14h8" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 动态信箱抽屉
	inbox: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<polyline points="22 12 16 12 14 15 10 15 8 12 2 12"/>
				<path d="M5.45 5.11L2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>
			</svg>
		`);
	},
	// 发射箭头 (发送按键)
	sendArrow: function() {
		return parseSvg(`
			<svg style="width:16px;height:16px;display:inline-block;vertical-align:-2px;margin-right:6px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<line x1="22" y1="2" x2="11" y2="13"/>
				<polygon points="22 2 15 22 11 13 2 9 22 2"/>
			</svg>
		`);
	},
	// 清理刷子
	brush: function() {
		return parseSvg(`
			<svg style="width:14px;height:14px;display:inline-block;vertical-align:-2px;margin-right:4px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M18 2l4 4-10 10H8v-4L18 2z"/>
				<path d="M2 22h20"/>
			</svg>
		`);
	},
	// 垃圾桶 (删除)
	trash: function() {
		return parseSvg(`
			<svg style="width:14px;height:14px;display:inline-block;vertical-align:-2px;margin-right:4px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<polyline points="3 6 5 6 21 6"/>
				<path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
			</svg>
		`);
	},
	// 刷新
	refresh: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<path d="M21.5 2v6h-6M21.34 15.57a10 10 0 1 1-.57-8.38l5.67-5.19" stroke-linecap="round" stroke-linejoin="round"/>
			</svg>
		`);
	}
};

/* ---------------- 注入白色毛玻璃设计系统 ---------------- */
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
	var styleId = 'fm350-sms-glass-styles';
	if (document.getElementById(styleId)) return;

	var css = `
		:root {
			--fm-glass-bg: rgba(255, 255, 255, 0.78);
			--fm-glass-bg-hover: rgba(255, 255, 255, 0.94);
			--fm-glass-border: rgba(255, 255, 255, 0.88);
			--fm-glass-shadow: 0 12px 32px 0 rgba(31, 38, 135, 0.05), 0 2px 8px 0 rgba(0, 0, 0, 0.02);
			--fm-glass-blur: blur(18px) saturate(180%);
			--fm-primary: #2563eb;
			--fm-primary-gradient: linear-gradient(135deg, #2563eb 0%, #06b6d4 100%);
			--fm-text-main: #0f172a;
			--fm-text-muted: #64748b;
			--fm-success: #10b981;
			--fm-warning: #f59e0b;
			--fm-danger: #ef4444;
		}

		.fm350-sms-page {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
			color: var(--fm-text-main);
			padding: 4px 0 32px 0;
		}

		/* 顶部短信 Hero 态势卡片 */
		.fm350-sms-hero {
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
			margin-bottom: 24px;
			position: relative;
			overflow: hidden;
		}
		.fm350-sms-hero::after {
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

		/* 存储池胶囊指示 */
		.fm350-storage-box {
			background: rgba(255, 255, 255, 0.88);
			border: 1px solid rgba(226, 232, 240, 0.9);
			border-radius: 14px;
			padding: 10px 16px;
			display: flex;
			align-items: center;
			gap: 14px;
			box-shadow: 0 2px 8px rgba(0,0,0,0.02);
		}
		.fm350-storage-info {
			display: flex;
			flex-direction: column;
			gap: 4px;
		}
		.fm350-storage-title {
			font-size: 0.74rem;
			font-weight: 600;
			color: #64748b;
			text-transform: uppercase;
			letter-spacing: 0.04em;
		}
		.fm350-storage-val {
			font-size: 0.88rem;
			font-weight: 700;
			color: #1e293b;
		}
		.fm350-storage-bar-bg {
			width: 90px;
			height: 6px;
			background: #e2e8f0;
			border-radius: 999px;
			overflow: hidden;
		}
		.fm350-storage-bar-fill {
			height: 100%;
			background: var(--fm-primary-gradient);
			border-radius: 999px;
			transition: width 0.3s ease;
		}

		/* 区块标题 */
		.fm350-section-title {
			font-size: 1.05rem;
			font-weight: 700;
			color: #1e293b;
			margin: 28px 0 14px 4px;
			display: flex;
			align-items: center;
			gap: 10px;
		}

		/* ---------------- 发送短信卡片 ---------------- */
		.fm350-compose-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 18px;
			padding: 24px;
			box-shadow: var(--fm-glass-shadow);
			margin-bottom: 24px;
			transition: all 0.25s ease;
		}
		.fm350-compose-card:hover {
			background: var(--fm-glass-bg-hover);
			box-shadow: 0 16px 36px 0 rgba(31, 38, 135, 0.08);
		}
		.fm350-input-row {
			display: flex;
			gap: 12px;
			margin-bottom: 14px;
			align-items: center;
			flex-wrap: wrap;
		}
		.fm350-input-num-box {
			flex: 1;
			min-width: 240px;
			position: relative;
		}
		.fm350-clean-btn {
			background: rgba(255, 255, 255, 0.9) !important;
			border: 1px solid rgba(203, 213, 225, 0.85) !important;
			border-radius: 10px !important;
			padding: 8px 14px !important;
			font-size: 0.82rem !important;
			font-weight: 600 !important;
			color: #475569 !important;
			cursor: pointer;
			display: inline-flex !important;
			align-items: center !important;
			transition: all 0.2s ease !important;
		}
		.fm350-clean-btn:hover {
			background: #f8fafc !important;
			color: var(--fm-primary) !important;
			border-color: rgba(37, 99, 235, 0.3) !important;
		}

		.fm350-input-text,
		.fm350-input-textarea {
			width: 100%;
			box-sizing: border-box;
			background: rgba(255, 255, 255, 0.9) !important;
			border: 1px solid rgba(203, 213, 225, 0.85) !important;
			border-radius: 12px !important;
			padding: 10px 14px !important;
			font-size: 0.88rem !important;
			color: #1e293b !important;
			outline: none !important;
			transition: all 0.2s ease !important;
			font-family: inherit;
		}
		.fm350-input-text:focus,
		.fm350-input-textarea:focus {
			border-color: var(--fm-primary) !important;
			box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.16) !important;
			background: #ffffff !important;
		}
		.fm350-input-textarea {
			resize: vertical;
			min-height: 80px;
			line-height: 1.5;
			margin-bottom: 12px;
		}

		.fm350-compose-footer {
			display: flex;
			align-items: center;
			justify-content: space-between;
			flex-wrap: wrap;
			gap: 12px;
		}
		.fm350-compose-tips {
			font-size: 0.78rem;
			color: var(--fm-text-muted);
		}
		.fm350-send-btn {
			background: var(--fm-primary-gradient) !important;
			border: none !important;
			color: #ffffff !important;
			padding: 9px 24px !important;
			border-radius: 12px !important;
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			box-shadow: 0 4px 14px rgba(37, 99, 235, 0.28) !important;
			display: inline-flex !important;
			align-items: center !important;
			transition: all 0.25s ease !important;
		}
		.fm350-send-btn:hover {
			filter: brightness(1.06);
			transform: translateY(-1px);
			box-shadow: 0 6px 20px rgba(37, 99, 235, 0.38) !important;
		}
		.fm350-send-btn:disabled {
			opacity: 0.6;
			cursor: not-allowed;
			transform: none !important;
		}

		/* ---------------- 短信列表毛玻璃表格 ---------------- */
		.fm350-table-card {
			background: var(--fm-glass-bg);
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 18px;
			box-shadow: var(--fm-glass-shadow);
			overflow: hidden;
			margin-bottom: 18px;
		}
		.fm350-table-card table {
			width: 100%;
			border-collapse: collapse;
			margin: 0;
		}
		.fm350-table-card th {
			background: rgba(248, 250, 252, 0.65);
			padding: 12px 16px;
			font-size: 0.78rem;
			font-weight: 600;
			color: #64748b;
			text-transform: uppercase;
			letter-spacing: 0.04em;
			border-bottom: 1px solid rgba(226, 232, 240, 0.7);
			text-align: left;
		}
		.fm350-table-card td {
			padding: 14px 16px;
			font-size: 0.86rem;
			color: #1e293b;
			border-bottom: 1px solid rgba(226, 232, 240, 0.5);
			vertical-align: middle;
			transition: background 0.2s ease;
		}
		.fm350-table-card tr:last-child td {
			border-bottom: none;
		}
		.fm350-table-card tr:hover td {
			background: rgba(255, 255, 255, 0.65);
		}

		/* 短信内容气泡 */
		.fm350-msg-bubble {
			background: rgba(241, 245, 249, 0.6);
			border: 1px solid rgba(226, 232, 240, 0.7);
			padding: 8px 12px;
			border-radius: 10px;
			font-size: 0.85rem;
			line-height: 1.5;
			color: #0f172a;
			word-break: break-word;
			max-width: 480px;
		}

		/* 状态胶囊徽章 */
		.fm350-status-pill {
			display: inline-flex;
			align-items: center;
			gap: 5px;
			padding: 3px 9px;
			border-radius: 999px;
			font-size: 0.74rem;
			font-weight: 700;
		}
		.fm350-status-unread {
			background: rgba(16, 185, 129, 0.14);
			color: #047857;
			border: 1px solid rgba(16, 185, 129, 0.25);
		}
		.fm350-status-read {
			background: rgba(148, 163, 184, 0.14);
			color: #475569;
			border: 1px solid rgba(148, 163, 184, 0.2);
		}
		.fm350-status-sent {
			background: rgba(37, 99, 235, 0.12);
			color: #1d4ed8;
			border: 1px solid rgba(37, 99, 235, 0.2);
		}
		.fm350-pulse-dot {
			width: 6px;
			height: 6px;
			border-radius: 50%;
			background: #10b981;
			animation: fm350-dot-glow 1.8s infinite ease-in-out;
		}

		/* 删除微按钮 */
		.fm350-del-btn {
			background: rgba(254, 242, 242, 0.85) !important;
			border: 1px solid rgba(239, 68, 68, 0.25) !important;
			color: #dc2626 !important;
			padding: 5px 12px !important;
			border-radius: 8px !important;
			font-size: 0.76rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			display: inline-flex !important;
			align-items: center !important;
			transition: all 0.2s ease !important;
		}
		.fm350-del-btn:hover {
			background: #ef4444 !important;
			color: #ffffff !important;
			border-color: #ef4444 !important;
			box-shadow: 0 2px 8px rgba(239, 68, 68, 0.24);
		}

		/* 刷新按钮 */
		.fm350-refresh-bar {
			display: flex;
			justify-content: flex-end;
			margin-top: 14px;
		}
		.fm350-refresh-btn {
			background: rgba(255, 255, 255, 0.9) !important;
			border: 1px solid rgba(203, 213, 225, 0.85) !important;
			border-radius: 11px !important;
			padding: 7px 16px !important;
			font-size: 0.84rem !important;
			font-weight: 600 !important;
			color: #334155 !important;
			cursor: pointer;
			display: inline-flex !important;
			align-items: center !important;
			gap: 6px !important;
			box-shadow: 0 2px 6px rgba(0,0,0,0.02) !important;
			transition: all 0.2s ease !important;
		}
		.fm350-refresh-btn:hover {
			background: #ffffff !important;
			color: var(--fm-primary) !important;
			border-color: var(--fm-primary) !important;
			transform: translateY(-1px);
			box-shadow: 0 4px 12px rgba(37, 99, 235, 0.12) !important;
		}

		/* 空状态 */
		.fm350-empty-inbox {
			padding: 42px 20px;
			text-align: center;
			color: #94a3b8;
		}
		.fm350-empty-inbox p {
			margin: 8px 0 0 0;
			font-size: 0.88rem;
		}

		/* 底部提示卡 */
		.fm350-footer-notice {
			margin-top: 24px;
			background: rgba(255, 255, 255, 0.55);
			backdrop-filter: var(--fm-glass-blur);
			border: 1px dashed rgba(203, 213, 225, 0.8);
			border-radius: 14px;
			padding: 14px 20px;
			font-size: 0.8rem;
			color: #64748b;
			line-height: 1.5;
		}

		/* ---------------- SVG 动效 ---------------- */
		.fm350-svg-icon { width: 20px; height: 20px; display: block; }
		@keyframes fm350-plane-bob {
			0%, 100% { transform: translateY(0) rotate(0deg); }
			50% { transform: translateY(-3px) rotate(-3deg); }
		}
		.fm350-anim-plane-bob {
			animation: fm350-plane-bob 2.8s ease-in-out infinite;
		}
		@keyframes fm350-trail-glow {
			0%, 100% { opacity: 0.2; }
			50% { opacity: 0.8; }
		}
		.fm350-anim-trail {
			animation: fm350-trail-glow 2s ease-in-out infinite;
		}
		@keyframes fm350-pulse-subtle {
			0%, 100% { transform: scale(1); opacity: 0.15; }
			50% { transform: scale(1.15); opacity: 0.35; }
		}
		.fm350-anim-pulse-subtle {
			transform-origin: 12px 14px;
			animation: fm350-pulse-subtle 2.5s ease-in-out infinite;
		}
		@keyframes fm350-dot-glow {
			0%, 100% { transform: scale(1); opacity: 0.8; }
			50% { transform: scale(1.3); opacity: 1; filter: drop-shadow(0 0 3px #10b981); }
		}

		/* ---------------- 响应式布局适配 ----------------
		   仅在小屏（<=767px / <=480px）调整排列、间距与滚动，不改变任何颜色、
		   字体、边框、圆角、阴影等视觉元素；桌面端（>=768px）样式保持不变。
		   短信表格列宽固定较宽，小屏改为横向滚动，保证各列内容完整可读。 */
		@media (max-width: 767px) {
			.fm350-sms-hero { padding: 16px 18px; gap: 14px; }
			.fm350-compose-card { padding: 16px 14px; }
			.fm350-table-card { overflow-x: auto; -webkit-overflow-scrolling: touch; }
			.fm350-table-card table { min-width: 900px; }
			.fm350-hero-meta h2 { flex-wrap: wrap; }
			.fm350-input-num-box { min-width: 0; flex-basis: 100%; }
			.fm350-storage-box { max-width: 100%; }
			.fm350-msg-bubble { max-width: 100%; }
		}
		@media (max-width: 480px) {
			.fm350-sms-hero { padding: 14px 14px; }
			.fm350-compose-card { padding: 14px 12px; }
			.fm350-storage-box { padding: 8px 12px; }
		}
	`;

	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

return view.extend({
	load: function() {
		return Promise.all([
			api.smsList(),
			api.smsStorage()
		]);
	},

	render: function(data) {
		api.injectCss();
		injectFrostedGlassTheme();

		var list = (data[0] && data[0].ok && data[0].value) || [];
		var storage = (data[1] && data[1].ok && data[1].value) || null;

		var wrap = E('div', { 'class': 'fm350-sms-page' });

		/* ---------------- 顶部 Hero 与存储池状态看板 ---------------- */
		var storagePercent = 0;
		if (storage && storage.total > 0) {
			storagePercent = Math.min(100, Math.round((storage.used / storage.total) * 100));
		}

		var storageBoxNode = storage ? E('div', { 'class': 'fm350-storage-box' }, [
			SVG_ICONS.storageSim(),
			E('div', { 'class': 'fm350-storage-info' }, [
				E('div', { 'class': 'fm350-storage-title' }, _('存储位置：') + storage.mem),
				E('div', { 'class': 'fm350-storage-val' }, storage.used + ' / ' + storage.total + _(' 条')),
				E('div', { 'class': 'fm350-storage-bar-bg' }, [
					E('div', { 'class': 'fm350-storage-bar-fill', 'style': 'width: ' + storagePercent + '%' })
				])
			])
		]) : '';

		var heroBanner = E('div', { 'class': 'fm350-sms-hero' }, [
			E('div', { 'class': 'fm350-hero-left' }, [
				E('div', { 'class': 'fm350-hero-avatar' }, [ SVG_ICONS.paperPlane() ]),
				E('div', { 'class': 'fm350-hero-meta' }, [
					E('h2', {}, [ _('短消息管理'), E('span', { 'style': 'font-size:0.8rem; font-weight:600; color:#64748b;' }, 'SMS Center') ]),
					E('p', {}, _('PDU 模式双向收发 · 中文 UCS2 / 纯英文 GSM 7-bit 硬件级自适应'))
				])
			]),
			storageBoxNode
		]);
		wrap.appendChild(heroBanner);

		/* ---------------- 发送短信卡片 ---------------- */
		wrap.appendChild(E('div', { 'class': 'fm350-section-title' }, [ SVG_ICONS.paperPlane(), _('发送短信') ]));

		var inputNumber = E('input', {
			'class': 'fm350-input-text',
			'type': 'text',
			'placeholder': _('收件人号码（例如 10086 或 +8613800138000）')
		});

		var btnClean = E('button', {
			'class': 'fm350-clean-btn',
			'click': function(ev) {
				ev.preventDefault();
				inputNumber.value = inputNumber.value.replace(/[^\d+]/g, '');
			}
		}, [ SVG_ICONS.brush(), _('整理号码') ]);

		var inputText = E('textarea', {
			'class': 'fm350-input-textarea',
			'rows': 3,
			'placeholder': _('输入短信正文内容...')
		});

		var btnSend = E('button', {
			'class': 'fm350-send-btn',
			'click': send
		}, [ SVG_ICONS.sendArrow(), _('立即发送') ]);

		function send(ev) {
			ev.preventDefault();
			var number = inputNumber.value.trim();
			var text = inputText.value;
			if (!number)
				return ui.addNotification(null, E('p', _('请填写收件人号码')), 'warning');
			if (!text)
				return ui.addNotification(null, E('p', _('请填写短信内容')), 'warning');

			btnSend.disabled = true;
			api.smsSend(number, text).then(function(res) {
				notify(res, _('短信已下发'));
				btnSend.disabled = false;
				if (res && res.ok) {
					inputText.value = '';
					setTimeout(function() { window.location.reload(); }, 800);
				}
			}).catch(function(e) {
				btnSend.disabled = false;
				ui.addNotification(null, E('p', _('发送异常：') + e), 'danger');
			});
		}

		var composeCard = E('div', { 'class': 'fm350-compose-card' }, [
			E('div', { 'class': 'fm350-input-row' }, [
				E('div', { 'class': 'fm350-input-num-box' }, inputNumber),
				btnClean
			]),
			inputText,
			E('div', { 'class': 'fm350-compose-footer' }, [
				E('div', { 'class': 'fm350-compose-tips' }, _('回车可直接换行，超长短信将由模组底层自动拆分拼接发送')),
				btnSend
			])
		]);
		wrap.appendChild(composeCard);

		/* ---------------- 短信收件箱列表 ---------------- */
		wrap.appendChild(E('div', { 'class': 'fm350-section-title' }, [
			SVG_ICONS.inbox(),
			_('收件箱列表'),
			E('span', { 'style': 'font-size: 0.74rem; font-weight: 600; color: #64748b; background: rgba(148, 163, 184, 0.15); padding: 2px 8px; border-radius: 999px;' },
				list.length + _(' 条记录'))
		]));

		var tableCard = E('div', { 'class': 'fm350-table-card' });

		if (!list.length) {
			tableCard.appendChild(E('div', { 'class': 'fm350-empty-inbox' }, [
				SVG_ICONS.inbox(),
				E('p', {}, _('暂无短信记录，等待模组接收...'))
			]));
		} else {
			var table = E('table');
			var thead = E('thead', {}, E('tr', {}, [
				E('th', { 'style': 'width: 60px;' }, _('序号')),
				E('th', { 'style': 'width: 110px;' }, _('状态')),
				E('th', { 'style': 'width: 160px;' }, _('发件人')),
				E('th', { 'style': 'width: 170px;' }, _('接收时间')),
				E('th', {}, _('内容')),
				E('th', { 'style': 'width: 90px;' }, _('编码')),
				E('th', { 'style': 'width: 90px; text-align: center;' }, _('操作'))
			]));
			table.appendChild(thead);

			var tbody = E('tbody');
			list.forEach(function(msg) {
				var isUnread = (msg.status === 0 || msg.status === '0');
				var isSent = (msg.status === 3 || msg.status === '3');

				var statusClass = isUnread ? 'fm350-status-unread' : (isSent ? 'fm350-status-sent' : 'fm350-status-read');
				var statusBadge = E('span', { 'class': 'fm350-status-pill ' + statusClass }, [
					isUnread ? E('span', { 'class': 'fm350-pulse-dot' }) : '',
					statusText[msg.status] || String(msg.status)
				]);

				var btnDel = E('button', {
					'class': 'fm350-del-btn',
					'click': function(ev) {
						ev.preventDefault();
						api.smsDelete('' + msg.index).then(function(res) {
							notify(res, _('已删除'));
							setTimeout(function() { window.location.reload(); }, 600);
						});
					}
				}, [ SVG_ICONS.trash(), _('删除') ]);

				tbody.appendChild(E('tr', {}, [
					E('td', { 'style': 'font-family: ui-monospace, monospace; font-weight: 700; color: #64748b;' }, '#' + String(msg.index)),
					E('td', {}, statusBadge),
					E('td', { 'style': 'font-family: ui-monospace, monospace; font-weight: 600; color: #1e293b;' }, msg.sender || '-'),
					E('td', { 'style': 'font-size: 0.8rem; color: #64748b; font-family: ui-monospace, monospace;' }, msg.timestamp || '-'),
					E('td', {}, E('div', { 'class': 'fm350-msg-bubble' }, msg.text || '')),
					E('td', { 'style': 'font-size: 0.78rem; color: #64748b;' }, msg.encoding || '-'),
					E('td', { 'style': 'text-align: center;' }, btnDel)
				]));
			});
			table.appendChild(tbody);
			tableCard.appendChild(table);
		}
		wrap.appendChild(tableCard);

		/* ---------------- 底部刷新与说明 ---------------- */
		wrap.appendChild(E('div', { 'class': 'fm350-refresh-bar' }, [
			E('button', {
				'class': 'fm350-refresh-btn',
				'click': function(ev) { ev.preventDefault(); window.location.reload(); }
			}, [ SVG_ICONS.refresh(), _('刷新收件箱') ])
		]));

		wrap.appendChild(E('div', { 'class': 'fm350-footer-notice' }, [
			_('提示：短信由后端 fm350d 经 AT 口直接读取存储池中的 PDU 报文并完成 UCS2/7-bit 解码。'),
			_('删除短信操作直接操作 SIM 卡或模块存储空间，删除后不可逆。')
		]));

		return wrap;
	}
});