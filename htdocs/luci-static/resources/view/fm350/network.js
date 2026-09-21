'use strict';
'require view';
'require form';
'require uci';
'require ui';
'require fm350.api as api';

function notify(res, okMsg) {
	api.notify(res, okMsg);
}

function refresh() {
	window.location.reload();
}

/* ---------------- 动态 SVG 矢量组件 ---------------- */
function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

var SVG_ICONS = {
	// 动态频谱正弦波 (锁频段)
	spectrum: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path class="fm350-anim-wave-flow" d="M2 12c2.5-6 4.5-6 7 0s4.5 6 7 0 4.5-6 6 0" stroke-linecap="round"/>
				<circle class="fm350-anim-freq-dot" cx="9" cy="12" r="2" fill="currentColor"/>
				<line x1="2" y1="20" x2="22" y2="20" stroke-opacity="0.3" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 雷达十字瞄准镜 (锁小区)
	crosshair: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<circle cx="12" cy="12" r="9" stroke-opacity="0.35"/>
				<circle class="fm350-anim-target-pulse" cx="12" cy="12" r="5" stroke="currentColor" stroke-dasharray="2 2"/>
				<line x1="12" y1="2" x2="12" y2="6" stroke-linecap="round"/>
				<line x1="12" y1="18" x2="12" y2="22" stroke-linecap="round"/>
				<line x1="2" y1="12" x2="6" y2="12" stroke-linecap="round"/>
				<line x1="18" y1="12" x2="22" y2="12" stroke-linecap="round"/>
				<circle cx="12" cy="12" r="1.5" fill="currentColor"/>
			</svg>
		`);
	},
	// 物理网卡与数据通道
	nic: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<rect x="3" y="4" width="18" height="14" rx="3"/>
				<path d="M7 18v2M17 18v2M7 8h2M11 8h2M15 8h2" stroke-linecap="round"/>
				<circle class="fm350-anim-packet" cx="8" cy="13" r="1.5" fill="currentColor"/>
				<circle class="fm350-anim-packet-2" cx="16" cy="13" r="1.5" fill="currentColor"/>
			</svg>
		`);
	},
	// 制式优先级梯队
	priority: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M4 18h4M4 12h9M4 6h16" stroke-linecap="round"/>
				<path class="fm350-anim-arrow-down" d="M19 13l2 2 2-2" stroke-linecap="round" stroke-linejoin="round"/>
				<path d="M21 9v6" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 闪电微图标 (用于快捷填选药丸)
	bolt: function() {
		return parseSvg(`
			<svg style="width:12px;height:12px;display:inline-block;vertical-align:-1px;margin-right:4px;" viewBox="0 0 24 24" fill="currentColor">
				<polygon points="13 2 3 14 12 14 11 22 21 10 12 10 13 2"/>
			</svg>
		`);
	}
};

/* ---------------- 注入白色毛玻璃样式系统 ---------------- */
function injectFrostedGlassTheme() {
	var styleId = 'fm350-network-glass-styles';
	if (document.getElementById(styleId)) return;

	var css = `
		:root {
			--fm-glass-bg: rgba(255, 255, 255, 0.78);
			--fm-glass-bg-hover: rgba(255, 255, 255, 0.94);
			--fm-glass-border: rgba(255, 255, 255, 0.88);
			--fm-glass-shadow: 0 12px 32px 0 rgba(31, 38, 135, 0.05), 0 2px 6px 0 rgba(0, 0, 0, 0.02);
			--fm-glass-blur: blur(18px) saturate(180%);
			--fm-primary: #2563eb;
			--fm-primary-gradient: linear-gradient(135deg, #2563eb 0%, #06b6d4 100%);
			--fm-text-main: #0f172a;
			--fm-text-muted: #64748b;
			--fm-success: #10b981;
			--fm-warning: #f59e0b;
			--fm-danger: #ef4444;
		}

		.fm350-net-page {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
			color: var(--fm-text-main);
			padding: 4px 0 32px 0;
		}

		/* 顶部数据网卡 Hero 态势卡片 */
		.fm350-net-hero {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.90), rgba(240, 246, 255, 0.80));
			backdrop-filter: var(--fm-glass-blur);
			-webkit-backdrop-filter: var(--fm-glass-blur);
			border: 1px solid var(--fm-glass-border);
			border-radius: 20px;
			padding: 22px 26px;
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
		.fm350-net-hero::after {
			content: "";
			position: absolute;
			top: -40px; right: -40px;
			width: 170px; height: 170px;
			background: radial-gradient(circle, rgba(6, 182, 212, 0.09) 0%, transparent 70%);
			border-radius: 50%;
			pointer-events: none;
		}
		.fm350-hero-left {
			display: flex;
			align-items: center;
			gap: 16px;
		}
		.fm350-hero-icon-box {
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
		.fm350-hero-meta h3 {
			margin: 0 0 4px 0;
			font-size: 1.32rem;
			font-weight: 700;
			display: flex;
			align-items: center;
			gap: 10px;
			color: #0f172a;
		}
		.fm350-hero-meta p {
			margin: 0;
			font-size: 0.82rem;
			color: var(--fm-text-muted);
		}
		.fm350-hero-pills {
			display: flex;
			align-items: center;
			gap: 10px;
			flex-wrap: wrap;
		}
		.fm350-data-pill {
			display: inline-flex;
			align-items: center;
			gap: 6px;
			background: rgba(255, 255, 255, 0.85);
			border: 1px solid rgba(226, 232, 240, 0.9);
			padding: 5px 12px;
			border-radius: 10px;
			font-size: 0.8rem;
			font-weight: 600;
			color: #334155;
		}
		.fm350-data-pill code {
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			color: var(--fm-primary);
			font-size: 0.82rem;
		}

		/* ---------------- 毛玻璃表单区块重构 ---------------- */
		.fm350-net-page .cbi-map { margin: 0; }
		.fm350-net-page .cbi-map-descr {
			color: var(--fm-text-muted);
			font-size: 0.86rem;
			margin-bottom: 20px;
			padding-left: 2px;
		}
		.fm350-net-page .cbi-section {
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
		.fm350-net-page .cbi-section:hover {
			background: var(--fm-glass-bg-hover) !important;
			box-shadow: 0 16px 36px 0 rgba(31, 38, 135, 0.08) !important;
		}
		.fm350-net-page .cbi-section-legend {
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
		.fm350-net-page .cbi-section-descr {
			font-size: 0.8rem !important;
			color: var(--fm-text-muted) !important;
			margin-bottom: 16px !important;
			line-height: 1.5 !important;
		}
		.fm350-net-page .cbi-value {
			padding: 10px 0 !important;
			border-bottom: 1px dashed rgba(226, 232, 240, 0.5) !important;
			display: flex;
			align-items: center;
			flex-wrap: wrap;
		}
		.fm350-net-page .cbi-value:last-child {
			border-bottom: none !important;
		}
		.fm350-net-page .cbi-value-title {
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			color: #475569 !important;
			width: 25% !important;
			min-width: 140px !important;
		}
		.fm350-net-page .cbi-value-field {
			flex: 1 !important;
		}

		/* 现代输入控件 */
		.fm350-net-page input[type="text"],
		.fm350-net-page select {
			background: rgba(255, 255, 255, 0.90) !important;
			border: 1px solid rgba(203, 213, 225, 0.9) !important;
			border-radius: 10px !important;
			padding: 7px 12px !important;
			font-size: 0.88rem !important;
			color: #1e293b !important;
			transition: all 0.2s ease !important;
			outline: none !important;
		}
		.fm350-net-page input[type="text"]:focus,
		.fm350-net-page select:focus {
			border-color: var(--fm-primary) !important;
			box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.16) !important;
			background: #ffffff !important;
		}
		.fm350-net-page .cbi-value-description {
			font-size: 0.78rem !important;
			color: var(--fm-text-muted) !important;
			margin-top: 6px !important;
			line-height: 1.4 !important;
		}

		/* 快捷预设胶囊 */
		.fm350-quick-presets {
			display: flex;
			align-items: center;
			gap: 6px;
			flex-wrap: wrap;
			margin-top: 8px;
		}
		.fm350-preset-chip {
			font-size: 0.72rem;
			font-weight: 600;
			padding: 3px 9px;
			border-radius: 8px;
			background: rgba(37, 99, 235, 0.08);
			color: var(--fm-primary);
			border: 1px solid rgba(37, 99, 235, 0.18);
			cursor: pointer;
			user-select: none;
			transition: all 0.2s ease;
		}
		.fm350-preset-chip:hover {
			background: var(--fm-primary);
			color: #ffffff;
			border-color: var(--fm-primary);
			transform: translateY(-1px);
			box-shadow: 0 3px 8px rgba(37, 99, 235, 0.22);
		}

		/* 动作按钮 */
		.fm350-net-page .cbi-button-apply {
			background: var(--fm-primary-gradient) !important;
			border: none !important;
			color: #ffffff !important;
			padding: 8px 20px !important;
			border-radius: 11px !important;
			font-size: 0.86rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			box-shadow: 0 4px 14px rgba(37, 99, 235, 0.26) !important;
			transition: all 0.25s ease !important;
		}
		.fm350-net-page .cbi-button-apply:hover {
			filter: brightness(1.06);
			transform: translateY(-1px);
			box-shadow: 0 6px 18px rgba(37, 99, 235, 0.35) !important;
		}
		.fm350-net-page .cbi-button-reset {
			background: rgba(254, 242, 242, 0.9) !important;
			border: 1px solid rgba(239, 68, 68, 0.3) !important;
			color: #dc2626 !important;
			padding: 8px 18px !important;
			border-radius: 11px !important;
			font-size: 0.86rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			transition: all 0.25s ease !important;
		}
		.fm350-net-page .cbi-button-reset:hover {
			background: #ef4444 !important;
			color: #ffffff !important;
			border-color: #ef4444 !important;
			transform: translateY(-1px);
			box-shadow: 0 4px 14px rgba(239, 68, 68, 0.26) !important;
		}

		/* 诊断终端卡片 */
		.fm350-terminal-card {
			background: rgba(15, 23, 42, 0.04);
			border: 1px solid rgba(203, 213, 225, 0.6);
			border-radius: 12px;
			padding: 12px 16px;
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			font-size: 0.8rem;
			line-height: 1.6;
			color: #334155;
			white-space: pre-wrap;
			word-break: break-all;
			max-height: 240px;
			overflow-y: auto;
		}

		.fm350-badge-ok {
			background: rgba(16, 185, 129, 0.14);
			color: #047857;
			padding: 2px 8px;
			border-radius: 999px;
			font-size: 0.72rem;
			font-weight: 700;
		}

		/* ---------------- SVG 动效 ---------------- */
		.fm350-svg-icon { width: 19px; height: 19px; display: block; }

		@keyframes fm350-wave-flow {
			0% { stroke-dashoffset: 0; }
			100% { stroke-dashoffset: 30; }
		}
		.fm350-anim-wave-flow {
			stroke-dasharray: 4 2;
			animation: fm350-wave-flow 2.8s linear infinite;
		}

		@keyframes fm350-freq-pulse {
			0%, 100% { transform: scale(1); opacity: 0.6; }
			50% { transform: scale(1.3); opacity: 1; filter: drop-shadow(0 0 3px currentColor); }
		}
		.fm350-anim-freq-dot {
			transform-origin: 9px 12px;
			animation: fm350-freq-pulse 2s ease-in-out infinite;
		}

		@keyframes fm350-target-pulse {
			0%, 100% { transform: rotate(0deg) scale(1); opacity: 0.7; }
			50% { transform: rotate(180deg) scale(1.15); opacity: 1; }
		}
		.fm350-anim-target-pulse {
			transform-origin: 12px 12px;
			animation: fm350-target-pulse 4s linear infinite;
		}

		@keyframes fm350-packet-blink {
			0%, 100% { opacity: 0.2; }
			50% { opacity: 1; }
		}
		.fm350-anim-packet {
			animation: fm350-packet-blink 1.2s ease-in-out infinite;
		}
		.fm350-anim-packet-2 {
			animation: fm350-packet-blink 1.2s ease-in-out infinite 0.6s;
		}

		@keyframes fm350-arrow-shift {
			0%, 100% { transform: translateY(0); }
			50% { transform: translateY(2px); }
		}
		.fm350-anim-arrow-down {
			animation: fm350-arrow-shift 1.5s ease-in-out infinite;
		}
	`;

	var style = E('style', { 'id': styleId }, css);
	document.head.appendChild(style);
}

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('fm350'),
			api.net(),
			api.lock(),
			api.cell()
		]);
	},

	render: function(data) {
		api.injectCss();
		injectFrostedGlassTheme();

		var net = (data[1] && data[1].ok && data[1].value) || {};
		var lock = (data[2] && data[2].ok && data[2].value) || [];
		var cell = (data[3] && data[3].ok && data[3].value) || [];

		var m, s, o;

		m = new form.Map('fm350', _('FM350 网络与锁频'),
			_('蜂窝接口的 IPv4 采用静态 /32 拓扑（模组 RNDIS 通道不开启 DHCP 握手），默认出口路由由守护进程按周期自动守护补齐。'));

		/* ---------------- 接口状态 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('接口当前状态'));
		s.anonymous = false;

		o = s.option(form.DummyValue, '_dev', _('数据通道网卡'));
		o.cfgvalue = function() { return net.dev || _('未探测到'); };

		o = s.option(form.DummyValue, '_ipv4', _('IPv4 地址'));
		o.cfgvalue = function() { return (net.ipv4 && net.ipv4.length) ? net.ipv4.join(', ') : _('未分配'); };

		o = s.option(form.DummyValue, '_ipv6', _('IPv6 地址'));
		o.cfgvalue = function() { return (net.ipv6 && net.ipv6.length) ? net.ipv6.join(', ') : _('未分配'); };

		o = s.option(form.DummyValue, '_routes', _('有效路由'));
		o.cfgvalue = function() {
			return (net.routes && net.routes.length) ? net.routes.join('  |  ') : _('无');
		};

		/* ---------------- 接口参数 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('网络接口参数'));

		o = s.option(form.Value, 'iface', _('IPv4 接口名称'));
		o.default = 'fm350';
		o.rmempty = false;

		o = s.option(form.Value, 'iface_v6', _('IPv6 接口名称'),
			_('以 device=@<IPv4 接口> 方式附着，留空表示不启用 IPv6 子接口'));
		o.default = 'fm350v6';

		o = s.option(form.Flag, 'ipv6', _('启用 IPv6 双栈支持'));
		o.default = '1';

		o = s.option(form.Value, 'data_dev', _('数据网卡绑定'),
			_('auto 表示按驱动内核链（rndis_host / cdc_ether 等）自动匹配绑定'));
		o.default = 'auto';

		o = s.option(form.Value, 'metric', _('路由优先级 (Metric)'),
			_('数值越小优先级越高。若希望蜂窝作主出口，需低于有线 WAN（OpenWrt 默认 10）'));
		o.datatype = 'uinteger';
		o.default = '30';

		o = s.option(form.Flag, 'route_guard', _('路由守护进程'),
			_('netifd 不会为无网关接口下发设备路由，开启后由 fm350d 周期自动补充 default dev 路由'));
		o.default = '1';

		/* ---------------- 锁频段 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('锁频段控制（AT+GTACT）'));
		s.description = _('实机验证过的取值：2 = 仅 4G，14 = 仅 5G（不限频段），20 = 自动；'
			+ '带频段锁定如 20,6,3,5078；部分固件需写成 14,,,5041。操作会自动离线（CFUN=0）后恢复网络。');

		o = s.option(form.Value, '_band_args', _('锁频参数'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		var bandPresets = [
			{ label: '全自动选网', val: '20' },
			{ label: '仅限 5G (全频段)', val: '14' },
			{ label: '仅限 4G (全频段)', val: '2' }
		];
		var bandChips = bandPresets.map(function(item) {
			return E('span', {
				'class': 'fm350-preset-chip',
				'click': function(ev) {
					ev.preventDefault();
					var input = document.querySelector('input[name="cbid.fm350.main._band_args"]') ||
					            document.querySelector('#widget\\.cbid\\.fm350\\.main\\._band_args');
					if (input) {
						input.value = item.val;
						input.dispatchEvent(new Event('change', { bubbles: true }));
					}
				}
			}, [ SVG_ICONS.bolt(), item.label + ' (' + item.val + ')' ]);
		});

		/* 本构建 form.js 的 description 只接受字符串（typeof this.description === 'string'），
		   传 DOM 节点会被静默丢弃 => 药丸栏不能在 option 上挂，改为渲染后按字段名就位注入 */
		var bandChipsPanel = E('div', {}, [
			E('div', {}, _('直接填写 AT+GTACT= 后的参数，或点击快捷填选：')),
			E('div', { 'class': 'fm350-quick-presets' }, bandChips)
		]);

		o = s.option(form.Button, '_btn_band', _('下发锁频'));
		o.inputtitle = _('锁定选定频段');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var args = this.section.getOption('_band_args').formvalue(this.section.section);
			if (!args)
				return ui.addNotification(null, E('p', _('请先填写或选择锁频参数')), 'warning');
			return api.lockBand(args).then(function(res) {
				notify(res, _('锁频指令已下发'));
				setTimeout(refresh, 1200);
			});
		};

		o = s.option(form.Button, '_btn_band_off', _('解除锁频'));
		o.inputtitle = _('恢复自动选网 (20)');
		o.inputstyle = 'reset';
		o.onclick = function() {
			return api.lockBand('20').then(function(res) {
				notify(res, _('已恢复自动选网'));
				setTimeout(refresh, 1200);
			});
		};

		/* ---------------- 锁小区 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('锁小区与 PCI（AT+EMMCHLCK）'));
		s.description = _('格式：AT+EMMCHLCK=1,11,0,<射频>,<PCI>,<...>；取消锁定填 0。'
			+ '可结合状态页面的附近小区查看。注意：硬件冷重启或断电后锁定大概率不会保留。');

		o = s.option(form.Value, '_cell_args', _('锁小区参数'),
			_('例如 1,11,0,627264,280,3；若解除锁定直接填 0'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_cell', _('下发锁小区'));
		o.inputtitle = _('锁定物理小区');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var args = this.section.getOption('_cell_args').formvalue(this.section.section);
			if (!args)
				return ui.addNotification(null, E('p', _('请先填写锁小区参数')), 'warning');
			return api.lockCell(args).then(function(res) {
				notify(res, _('锁小区指令已下发'));
				setTimeout(refresh, 1200);
			});
		};

		o = s.option(form.Button, '_btn_cell_off', _('取消小区锁'));
		o.inputtitle = _('解除锁定 (0)');
		o.inputstyle = 'reset';
		o.onclick = function() {
			return api.lockCell('0').then(function(res) {
				notify(res, _('已取消小区锁定'));
				setTimeout(refresh, 1200);
			});
		};

		/* ---------------- 制式优先级 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('网络制式优先级（AT+QNWPREFCFG）'));

		o = s.option(form.Value, '_rat', _('优先级顺序'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		var ratPresets = [
			{ label: '5G 优先', val: 'NR:LTE' },
			{ label: '4G 优先', val: 'LTE:NR' },
			{ label: '全模式漫游', val: 'NR:LTE:WCDMA' }
		];
		var ratChips = ratPresets.map(function(item) {
			return E('span', {
				'class': 'fm350-preset-chip',
				'click': function(ev) {
					ev.preventDefault();
					var input = document.querySelector('input[name="cbid.fm350.main._rat"]') ||
					            document.querySelector('#widget\\.cbid\\.fm350\\.main\\._rat');
					if (input) {
						input.value = item.val;
						input.dispatchEvent(new Event('change', { bubbles: true }));
					}
				}
			}, [ SVG_ICONS.bolt(), item.label + ' (' + item.val + ')' ]);
		});

		/* 同上：description 只认字符串，药丸栏改为后置注入 */
		var ratChipsPanel = E('div', {}, [
			E('div', {}, _('用冒号分隔各制式；点击药丸可快速填入：')),
			E('div', { 'class': 'fm350-quick-presets' }, ratChips)
		]);

		o = s.option(form.Button, '_btn_rat', _('应用制式顺序'));
		o.inputtitle = _('下发优先级');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var order = this.section.getOption('_rat').formvalue(this.section.section);
			if (!order)
				return ui.addNotification(null, E('p', _('请先填写优先级顺序')), 'warning');
			return api.rat(order).then(function(res) { notify(res, _('制式优先级已下发')); });
		};

		/* ---------------- 只读诊断信息 ---------------- */
		if (lock.length || cell.length) {
			s = m.section(form.NamedSection, 'main', 'fm350', _('实时锁定态势与邻区诊断'));

			o = s.option(form.DummyValue, '_lock', _('当前锁定列表'));
			o.rawhtml = true;
			o.cfgvalue = function() {
				var content = lock.map(function(x) {
					return x[0] + '  =>  ' + String(x[1]).replace(/\n/g, ' / ');
				}).join('\n');
				return E('div', { 'class': 'fm350-terminal-card' }, content || _('无活动锁频记录'));
			};

			o = s.option(form.DummyValue, '_cell', _('服务小区与邻区原语'));
			o.rawhtml = true;
			o.cfgvalue = function() {
				var content = cell.map(function(x) {
					return x[0] + '  =>  ' + String(x[1]).replace(/\n/g, ' / ');
				}).join('\n');
				return E('div', { 'class': 'fm350-terminal-card' }, content || _('无邻区测量数据'));
			};
		}

		/* 把被 form.js 丢弃的快捷药丸栏，按字段名就位注入到该字段的描述容器。
		   用 data-widget-id / id 定位，避免 CSS 选择器里的点号转义坑。 */
		function injectChipPanel(root, fieldName, node) {
			var wid = 'widget.cbid.fm350.main.' + fieldName;
			var input = root.querySelector('.cbi-value[data-name="' + fieldName + '"] input') ||
			            root.querySelector('[data-widget-id="' + wid + '"]') ||
			            document.getElementById(wid);
			var field = input ? (input.closest('.cbi-value-field') || input.parentNode) : null;
			if (!field) {
				console.warn('[fm350] 快捷药丸栏注入失败：未定位到 ' + fieldName + ' 字段容器');
				return;
			}
			var box = field.querySelector('.cbi-value-description');
			if (!box) {
				box = E('div', { 'class': 'cbi-value-description' });
				field.appendChild(box);
			}
			box.appendChild(node);
		}

		/* 渲染整体容器并前置插入数据网卡 Hero 态势看板 */
		return m.render().then(function(mapNode) {
			var hasDev = !!net.dev;
			var heroBanner = E('div', { 'class': 'fm350-net-hero' }, [
				E('div', { 'class': 'fm350-hero-left' }, [
					E('div', { 'class': 'fm350-hero-icon-box' }, [ SVG_ICONS.nic() ]),
					E('div', { 'class': 'fm350-hero-meta' }, [
						E('h3', {}, [
							_('数据通道：') + (net.dev || _('未识别')),
							hasDev ? E('span', { 'class': 'fm350-badge-ok' }, _('UP 正常通信'))
							       : E('span', { 'style': 'color:#ef4444;font-size:0.75rem;' }, _('DOWN 未就绪'))
						]),
						E('p', {}, _('主接口名：') + (uci.get('fm350', 'main', 'iface') || 'fm350') + ' · ' + _('路由守护：') + (uci.get('fm350', 'main', 'route_guard') === '0' ? _('关闭') : _('运行中')))
					])
				]),
				E('div', { 'class': 'fm350-hero-pills' }, [
					E('div', { 'class': 'fm350-data-pill' }, [ 'IPv4', E('code', {}, (net.ipv4 && net.ipv4.length) ? net.ipv4.join(', ') : _('未分配')) ]),
					E('div', { 'class': 'fm350-data-pill' }, [ 'IPv6', E('code', {}, (net.ipv6 && net.ipv6.length) ? net.ipv6.join(', ') : _('未分配')) ]),
					E('div', { 'class': 'fm350-data-pill' }, [ 'Metric', E('code', {}, uci.get('fm350', 'main', 'metric') || '30') ])
				])
			]);

			var pageRoot = E('div', { 'class': 'fm350-net-page' }, [
				heroBanner,
				mapNode
			]);
			injectChipPanel(pageRoot, '_band_args', bandChipsPanel);
			injectChipPanel(pageRoot, '_rat', ratChipsPanel);
			return pageRoot;
		});
	}
});