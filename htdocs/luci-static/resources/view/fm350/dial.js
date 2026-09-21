'use strict';
'require view';
'require form';
'require uci';
'require ui';
'require fm350.api as api';

/* 后端调用统一走 fm350.api（rpcd ucode 代理 → fm350d） */
function notify(res, okMsg) {
	api.notify(res, okMsg);
}

/* ---------------- 动态 SVG 生成助手 ---------------- */
function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

var SVG_ICONS = {
	// 5G 天线与发射电磁波
	antenna: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 21V11M8 21l4-10 4 10M6 17h12" stroke-linecap="round" stroke-linejoin="round"/>
				<circle cx="12" cy="7" r="2.5" fill="currentColor"/>
				<circle class="fm350-anim-wave fm350-anim-wave-1" cx="12" cy="7" r="5" stroke="currentColor" stroke-width="1.2" fill="none"/>
				<circle class="fm350-anim-wave fm350-anim-wave-2" cx="12" cy="7" r="8.5" stroke="currentColor" stroke-width="1" stroke-dasharray="2 2" fill="none"/>
			</svg>
		`);
	},
	// 动态旋转星链与全球数据网
	globe: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<circle cx="12" cy="12" r="9" stroke-opacity="0.4"/>
				<ellipse class="fm350-anim-globe-spin" cx="12" cy="12" rx="4" ry="9" stroke="currentColor"/>
				<path d="M3.5 9h17M3.5 15h17" stroke-opacity="0.4" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 拨号起飞火箭
	rocket: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M4.5 16.5c-1.5 1.26-2 5-2 5s3.74-.5 5-2c.71-.84.7-2.13-.09-2.91a2.18 2.18 0 0 0-2.91-.09z"/>
				<path d="M12 15l-3-3a22 22 0 0 1 2-3.95A12.88 12.88 0 0 1 22 2c0 2.72-.78 7.5-6 11a22.35 22.35 0 0 1-4 2z"/>
				<path class="fm350-anim-flame" d="M9 18c-1 1.5-2 2-3 2" stroke-linecap="round"/>
				<circle cx="15.5" cy="8.5" r="1.5" fill="currentColor"/>
			</svg>
		`);
	},
	// 断开挂断
	disconnect: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M18.36 6.64a9 9 0 1 1-12.73 0M12 2v10" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 认证与密钥锁
	shield: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>
				<circle class="fm350-anim-pulse-subtle" cx="12" cy="11" r="2.5" fill="currentColor" fill-opacity="0.3"/>
			</svg>
		`);
	},
	// 闪电快捷标识
	bolt: function() {
		return parseSvg(`
			<svg style="width:13px;height:13px;display:inline-block;vertical-align:-2px;margin-right:3px;" viewBox="0 0 24 24" fill="currentColor">
				<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>
			</svg>
		`);
	}
};

/* ---------------- 注入白色毛玻璃设计系统 ---------------- */
function injectFrostedGlassTheme() {
	var styleId = 'fm350-dial-glass-styles';
	if (document.getElementById(styleId)) return;

	var css = `
		:root {
			--fm-glass-bg: rgba(255, 255, 255, 0.80);
			--fm-glass-bg-hover: rgba(255, 255, 255, 0.94);
			--fm-glass-border: rgba(255, 255, 255, 0.90);
			--fm-glass-shadow: 0 12px 32px 0 rgba(31, 38, 135, 0.05), 0 2px 8px 0 rgba(0, 0, 0, 0.02);
			--fm-glass-blur: blur(18px) saturate(180%);
			--fm-primary: #2563eb;
			--fm-primary-gradient: linear-gradient(135deg, #2563eb 0%, #06b6d4 100%);
			--fm-danger-gradient: linear-gradient(135deg, #ef4444 0%, #f97316 100%);
			--fm-text-main: #0f172a;
			--fm-text-muted: #64748b;
			--fm-success: #10b981;
			--fm-warning: #f59e0b;
			--fm-danger: #ef4444;
		}

		.fm350-dial-page {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
			color: var(--fm-text-main);
			padding: 4px 0 32px 0;
		}

		/* 顶部 PDP 实时流光态势看板 */
		.fm350-pdp-hero {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.90), rgba(240, 247, 255, 0.82));
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
			gap: 20px;
			margin-bottom: 24px;
			position: relative;
			overflow: hidden;
		}
		.fm350-pdp-hero::after {
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
			gap: 18px;
		}
		.fm350-hero-avatar {
			width: 58px;
			height: 58px;
			border-radius: 16px;
			display: flex;
			align-items: center;
			justify-content: center;
			transition: all 0.3s ease;
		}
		.fm350-hero-avatar.is-active {
			background: linear-gradient(135deg, rgba(16, 185, 129, 0.16), rgba(6, 182, 212, 0.16));
			color: #059669;
			box-shadow: 0 4px 14px rgba(16, 185, 129, 0.18);
		}
		.fm350-hero-avatar.is-down {
			background: rgba(148, 163, 184, 0.14);
			color: #64748b;
		}
		.fm350-hero-details h3 {
			margin: 0 0 6px 0;
			font-size: 1.35rem;
			font-weight: 700;
			display: flex;
			align-items: center;
			gap: 10px;
			color: #0f172a;
		}
		.fm350-hero-details p {
			margin: 0;
			font-size: 0.84rem;
			color: var(--fm-text-muted);
		}

		.fm350-hero-badges {
			display: flex;
			align-items: center;
			gap: 10px;
			flex-wrap: wrap;
		}
		.fm350-chip-pill {
			display: inline-flex;
			align-items: center;
			gap: 6px;
			background: rgba(255, 255, 255, 0.85);
			border: 1px solid rgba(226, 232, 240, 0.85);
			padding: 5px 12px;
			border-radius: 10px;
			font-size: 0.82rem;
			font-weight: 600;
			color: #334155;
			box-shadow: 0 2px 6px rgba(0,0,0,0.02);
		}
		.fm350-chip-pill code {
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			color: var(--fm-primary);
			font-size: 0.84rem;
		}

		/* 状态徽章 */
		.fm350-badge {
			font-size: 0.74rem;
			font-weight: 700;
			padding: 3px 10px;
			border-radius: 999px;
			display: inline-flex;
			align-items: center;
			gap: 5px;
		}
		.fm350-badge-online {
			background: rgba(16, 185, 129, 0.14);
			color: #047857;
			border: 1px solid rgba(16, 185, 129, 0.25);
		}
		.fm350-badge-offline {
			background: rgba(239, 68, 68, 0.12);
			color: #b91c1c;
			border: 1px solid rgba(239, 68, 68, 0.25);
		}

		/* ---------------- 毛玻璃表单重绘 ---------------- */
		.fm350-dial-page .cbi-map {
			margin: 0;
		}
		.fm350-dial-page .cbi-map-descr {
			color: var(--fm-text-muted);
			font-size: 0.88rem;
			margin-bottom: 20px;
			padding-left: 2px;
		}
		.fm350-dial-page .cbi-section {
			background: var(--fm-glass-bg) !important;
			backdrop-filter: var(--fm-glass-blur) !important;
			-webkit-backdrop-filter: var(--fm-glass-blur) !important;
			border: 1px solid var(--fm-glass-border) !important;
			border-radius: 18px !important;
			padding: 22px 24px !important;
			box-shadow: var(--fm-glass-shadow) !important;
			margin-bottom: 22px !important;
			transition: all 0.25s ease !important;
		}
		.fm350-dial-page .cbi-section:hover {
			background: var(--fm-glass-bg-hover) !important;
			box-shadow: 0 16px 36px 0 rgba(31, 38, 135, 0.08) !important;
		}
		.fm350-dial-page .cbi-section-legend {
			font-size: 1.05rem !important;
			font-weight: 700 !important;
			color: #1e293b !important;
			border-bottom: 1px solid rgba(226, 232, 240, 0.6) !important;
			padding-bottom: 12px !important;
			margin-bottom: 16px !important;
			display: flex !important;
			align-items: center !important;
			gap: 8px !important;
		}
		.fm350-dial-page .cbi-value {
			padding: 10px 0 !important;
			border-bottom: 1px dashed rgba(226, 232, 240, 0.5) !important;
			display: flex;
			align-items: center;
			flex-wrap: wrap;
		}
		.fm350-dial-page .cbi-value:last-child {
			border-bottom: none !important;
		}
		.fm350-dial-page .cbi-value-title {
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			color: #475569 !important;
			width: 25% !important;
			min-width: 140px !important;
		}
		.fm350-dial-page .cbi-value-field {
			flex: 1 !important;
		}

		/* 现代圆角表单控件 */
		.fm350-dial-page input[type="text"],
		.fm350-dial-page input[type="password"],
		.fm350-dial-page select {
			background: rgba(255, 255, 255, 0.90) !important;
			border: 1px solid rgba(203, 213, 225, 0.9) !important;
			border-radius: 10px !important;
			padding: 7px 12px !important;
			font-size: 0.88rem !important;
			color: #1e293b !important;
			box-shadow: 0 1px 3px rgba(0, 0, 0, 0.02) !important;
			transition: all 0.2s ease !important;
			outline: none !important;
		}
		.fm350-dial-page input[type="text"]:focus,
		.fm350-dial-page input[type="password"]:focus,
		.fm350-dial-page select:focus {
			border-color: var(--fm-primary) !important;
			box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.16) !important;
			background: #ffffff !important;
		}
		.fm350-dial-page .cbi-value-description {
			font-size: 0.78rem !important;
			color: var(--fm-text-muted) !important;
			margin-top: 6px !important;
			line-height: 1.4 !important;
		}

		/* ---------------- APN 一键快捷选填药丸 ---------------- */
		.fm350-apn-quick-bar {
			display: flex;
			align-items: center;
			gap: 6px;
			flex-wrap: wrap;
			margin-top: 8px;
		}
		.fm350-apn-pill {
			font-size: 0.72rem;
			font-weight: 600;
			padding: 3px 9px;
			border-radius: 8px;
			background: rgba(37, 99, 235, 0.07);
			color: var(--fm-primary);
			border: 1px solid rgba(37, 99, 235, 0.16);
			cursor: pointer;
			user-select: none;
			transition: all 0.2s ease;
		}
		.fm350-apn-pill:hover {
			background: var(--fm-primary);
			color: #ffffff;
			border-color: var(--fm-primary);
			transform: translateY(-1px);
			box-shadow: 0 3px 8px rgba(37, 99, 235, 0.22);
		}

		/* ---------------- 高质感操作按钮 ---------------- */
		.fm350-btn-group {
			display: flex;
			gap: 14px;
			flex-wrap: wrap;
			padding: 6px 0;
		}
		.fm350-dial-page .cbi-button-apply {
			background: var(--fm-primary-gradient) !important;
			border: none !important;
			color: #ffffff !important;
			padding: 9px 22px !important;
			border-radius: 12px !important;
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			box-shadow: 0 4px 14px rgba(37, 99, 235, 0.28) !important;
			transition: all 0.25s ease !important;
			display: inline-flex !important;
			align-items: center !important;
			gap: 8px !important;
		}
		.fm350-dial-page .cbi-button-apply:hover {
			filter: brightness(1.06);
			transform: translateY(-1px);
			box-shadow: 0 6px 20px rgba(37, 99, 235, 0.38) !important;
		}
		.fm350-dial-page .cbi-button-reset {
			background: rgba(254, 242, 242, 0.9) !important;
			border: 1px solid rgba(239, 68, 68, 0.3) !important;
			color: #dc2626 !important;
			padding: 9px 22px !important;
			border-radius: 12px !important;
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			box-shadow: 0 2px 8px rgba(239, 68, 68, 0.06) !important;
			transition: all 0.25s ease !important;
			display: inline-flex !important;
			align-items: center !important;
			gap: 8px !important;
		}
		.fm350-dial-page .cbi-button-reset:hover {
			background: #ef4444 !important;
			color: #ffffff !important;
			border-color: #ef4444 !important;
			transform: translateY(-1px);
			box-shadow: 0 4px 16px rgba(239, 68, 68, 0.28) !important;
		}

		/* ---------------- 动画规则 ---------------- */
		.fm350-svg-icon { width: 20px; height: 20px; display: block; }
		@keyframes fm350-wave {
			0% { transform: scale(0.8); opacity: 1; }
			100% { transform: scale(1.6); opacity: 0; }
		}
		.fm350-anim-wave {
			transform-origin: 12px 7px;
			animation: fm350-wave 2.2s cubic-bezier(0.2, 0.8, 0.2, 1) infinite;
		}
		.fm350-anim-wave-1 { animation-delay: 0s; }
		.fm350-anim-wave-2 { animation-delay: 1.1s; }

		@keyframes fm350-globe-spin {
			0%, 100% { transform: scaleX(1); }
			50% { transform: scaleX(0.2); }
		}
		.fm350-anim-globe-spin {
			transform-origin: 12px 12px;
			animation: fm350-globe-spin 4s ease-in-out infinite;
		}

		@keyframes fm350-flame {
			0%, 100% { opacity: 0.5; transform: translateY(0); }
			50% { opacity: 1; transform: translateY(1.5px); }
		}
		.fm350-anim-flame {
			animation: fm350-flame 0.8s ease-in-out infinite;
		}

		@keyframes fm350-pulse-subtle {
			0%, 100% { transform: scale(1); opacity: 0.3; }
			50% { transform: scale(1.2); opacity: 0.6; }
		}
		.fm350-anim-pulse-subtle {
			transform-origin: 12px 11px;
			animation: fm350-pulse-subtle 2s infinite ease-in-out;
		}
	`;

	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('fm350'),
			api.pdp()
		]);
	},

	render: function(data) {
		api.injectCss();
		injectFrostedGlassTheme();

		var pdp = (data[1] && data[1].ok && data[1].value) || {};
		var isActive = !!pdp.active;
		var m, s, o;

		m = new form.Map('fm350', _('FM350 拨号与 APN'),
			_('配置模块 PDP 上下文参数并管理无线链路状态。修改后请点击“保存并拨号”下发生效。'));

		/* ---------------- 一、PDP 上下文 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('PDP 上下文配置'));
		s.anonymous = false;

		o = s.option(form.Value, 'apn', _('接入点 (APN)'));
		o.rmempty = false;
		o.default = 'cmiot5g';

		// 注入便捷的运营商快捷选填小药丸组件
		var apnPresets = [
			{ label: '移动 5G', val: 'cmiot5g' },
			{ label: '移动通用', val: 'cmnet' },
			{ label: '电信 5G/4G', val: 'ctnet' },
			{ label: '联通 5G/4G', val: '3gnet' },
			{ label: '广电 5G', val: 'cbnet' }
		];
		var pillsNodes = apnPresets.map(function(item) {
			return E('span', {
				'class': 'fm350-apn-pill',
				'click': function(ev) {
					ev.preventDefault();
					/* 实测本固件 LuCI 不监听 'change'：form.js 只在 optionEl 上监听
					   'widget-change'，且在此构建下改 DOM 不会触发 onchange。
					   所以必须显式写进 uci 变更集，否则点保存时这个值不会被持久化。
					   输入框的权威标识是 id（本构建 input 无 name 属性）。 */
					var cell = ev.currentTarget && ev.currentTarget.closest
						? ev.currentTarget.closest('.cbi-value-field')
						: null;
					var input = (cell && cell.querySelector('input')) ||
					            document.querySelector('#widget\\.cbid\\.fm350\\.main\\.apn');
					if (input)
						input.value = item.val;
					uci.set('fm350', 'main', 'apn', item.val);
					if (input) {
						input.dispatchEvent(new Event('input', { bubbles: true }));
						input.dispatchEvent(new Event('widget-change', { bubbles: true }));
					}
				}
			}, [ SVG_ICONS.bolt(), item.label + ' (' + item.val + ')' ]);
		});

		/* form.js:274 只接受字符串型 description，传 DOM 节点会被静默丢弃，
		   因此药丸栏不在这里挂，而是在 m.render() 完成后后置注入（见 render 尾部）*/
		var pillBar = E('div', { 'class': 'fm350-apn-quick-bar' }, pillsNodes);

		o = s.option(form.ListValue, 'pdp_type', _('PDP 协议类型'));
		o.value('IPV4V6', _('IPv4 + IPv6（双栈推荐）'));
		o.value('IP', _('仅 IPv4'));
		o.value('IPV6', _('仅 IPv6'));
		o.default = 'IPV4V6';

		o = s.option(form.Value, 'cid', _('上下文 ID (CID)'),
			_('默认一般保持 1。多 PDP 或专用切片网络时才需调整'));
		o.datatype = 'uinteger';
		o.default = '1';

		o = s.option(form.ListValue, 'auth', _('鉴权认证方式'));
		o.value('none', _('无认证（默认）'));
		o.value('pap', 'PAP');
		o.value('chap', 'CHAP');
		o.value('both', 'PAP / CHAP 混合');
		o.default = 'none';

		o = s.option(form.Value, 'username', _('认证用户名'),
			_('绝大多数国内运营商留空即可'));
		o.depends('auth', 'pap');
		o.depends('auth', 'chap');
		o.depends('auth', 'both');

		o = s.option(form.Value, 'password', _('认证密码'));
		o.password = true;
		o.depends('auth', 'pap');
		o.depends('auth', 'chap');
		o.depends('auth', 'both');

		o = s.option(form.Flag, 'auto_dial', _('开机与断线自动重拨'),
			_('守护进程定期巡检，若发现连接掉线则自动发起重拨恢复链路'));
		o.default = '1';

		/* ---------------- 二、当前状态 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('链路运行细节'));

		o = s.option(form.DummyValue, '_state', _('连接状态'));
		o.cfgvalue = function() {
			return pdp.active ? _('已连接 (ACTIVE)') : _('未连接 (DISCONNECTED)');
		};

		o = s.option(form.DummyValue, '_addr', _('已分配 IP 地址'));
		o.cfgvalue = function() {
			var parts = [];
			if (pdp.ipv4) parts.push(pdp.ipv4);
			if (pdp.ipv6) parts.push(pdp.ipv6);
			return parts.length ? parts.join('  |  ') : _('未分配');
		};

		o = s.option(form.DummyValue, '_dns', _('DNS 域名解析'));
		o.cfgvalue = function() {
			return (pdp.dns && pdp.dns.length) ? pdp.dns.join(', ') : '-';
		};

		/* ---------------- 三、操作控制 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('链路操作'));

		o = s.option(form.Button, '_btn_dial', _('立即拨号'));
		o.inputstyle = 'apply';
		o.inputtitle = _('保存并拨号');
		o.onclick = function() {
			/* 一旦提供自定义 onclick，form.Button 的点击处理就只调用它、
			   不再自动 map.save()（form.js：`if (this.onclick) return this.onclick(...)`）。
			   而 map.save() 才是「先 parse() 用 formvalue() 从实时 DOM 读值写进
			   uci 变更集，再 uci.save() 落盘」的正确路径。少了 parse()，
			   表单里改过的 APN（含快捷药丸写入的值）不会被保存，
			   拨号用的仍是 uci 里的旧值。 */
			return m.parse()
				.then(function() { return uci.save(); })
				.then(function() {
					return api.dial(uci.get('fm350', 'main', 'apn') || '');
				})
				.then(function(res) { notify(res, _('拨号指令已下发')); })
				.catch(function(e) {
					ui.addNotification(null, E('p', _('拨号异常：') + e), 'danger');
				});
		};

		o = s.option(form.Button, '_btn_hangup', _('断开链路'));
		o.inputstyle = 'reset';
		o.inputtitle = _('断开连接');
		o.onclick = function() {
			return api.hangup()
				.then(function(res) { notify(res, _('断开连接指令已下发')); });
		};

		/* 渲染整体容器并前置插入 PDP Hero 实时态势看板 */
		return m.render().then(function(mapNode) {
			var ipList = [];
			if (pdp.ipv4) ipList.push(E('div', { 'class': 'fm350-chip-pill' }, [ 'IPv4', E('code', {}, pdp.ipv4) ]));
			if (pdp.ipv6) ipList.push(E('div', { 'class': 'fm350-chip-pill' }, [ 'IPv6', E('code', {}, pdp.ipv6) ]));
			if (pdp.dns && pdp.dns.length) {
				ipList.push(E('div', { 'class': 'fm350-chip-pill' }, [ 'DNS', E('code', {}, pdp.dns.join(', ')) ]));
			}

			var heroBanner = E('div', { 'class': 'fm350-pdp-hero' }, [
				E('div', { 'class': 'fm350-hero-left' }, [
					E('div', { 'class': 'fm350-hero-avatar ' + (isActive ? 'is-active' : 'is-down') }, [
						isActive ? SVG_ICONS.antenna() : SVG_ICONS.globe()
					]),
					E('div', { 'class': 'fm350-hero-details' }, [
						E('h3', {}, [
							_('PDP 无线链路'),
							isActive ? E('span', { 'class': 'fm350-badge fm350-badge-online' }, _('● 已在线接入'))
							         : E('span', { 'class': 'fm350-badge fm350-badge-offline' }, _('○ 未连接'))
						]),
						E('p', {}, _('接入点：') + (pdp.apn || uci.get('fm350', 'main', 'apn') || '-') + ' · ' + _('模式：') + (pdp.pdp_type || 'IPV4V6'))
					])
				]),
				E('div', { 'class': 'fm350-hero-badges' }, ipList.length ? ipList : [
					E('span', { 'style': 'font-size:0.82rem; color:#94a3b8;' }, _('暂无分配地址，等待拨号激活...'))
				])
			]);

			var pageRoot = E('div', { 'class': 'fm350-dial-page' }, [
				heroBanner,
				mapNode
			]);

			/* APN 快捷药丸后置注入：定位 APN 输入框 -> 父级
			   .cbi-value-field -> 内部 .cbi-value-description */
			/* 本构建 input 无 name 属性，id 才是权威；后两条为其他 LuCI 版本兜底 */
			var apnInput = pageRoot.querySelector('#widget\\.cbid\\.fm350\\.main\\.apn') ||
			               pageRoot.querySelector('.cbi-value[data-name="apn"] input') ||
			               pageRoot.querySelector('input[name="cbid.fm350.main.apn"]');
			var apnField = apnInput
				? (apnInput.closest('.cbi-value-field') || apnInput.parentNode)
				: null;

			if (apnField) {
				var descBox = apnField.querySelector('.cbi-value-description');
				if (!descBox) {
					descBox = E('div', { 'class': 'cbi-value-description' });
					apnField.appendChild(descBox);
				}
				descBox.appendChild(E('div', {}, _('常用运营商快捷填选：')));
				descBox.appendChild(pillBar);
			}
			else {
				console.warn('[fm350] APN 药丸栏注入失败：未定位到 APN 字段容器');
			}

			return pageRoot;
		});
	}
});
