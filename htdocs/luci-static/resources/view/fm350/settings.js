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

/* ---------------- 动态矢量 SVG 渲染组件 ---------------- */
function parseSvg(svgStr) {
	var div = document.createElement('div');
	div.innerHTML = svgStr.trim();
	return div.firstElementChild;
}

var SVG_ICONS = {
	// 双齿轮持续自转微动效 (守护进程服务)
	cogs: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<g class="fm350-anim-spin-cw">
					<circle cx="9" cy="9" r="3"/>
					<path d="M9 1v2M9 15v2M1 9h2M15 9h2M3.34 3.34l1.42 1.42M13.24 13.24l1.42 1.42M3.34 14.66l1.42-1.42M13.24 4.76l1.42-1.42"/>
				</g>
				<g class="fm350-anim-spin-ccw">
					<circle cx="17" cy="17" r="2"/>
					<path d="M17 12v1M17 21v1M12 17h1M21 17h1M13.5 13.5l.7.7M19.8 19.8l.7.7M13.5 20.5l.7-.7M19.8 14.2l.7-.7"/>
				</g>
			</svg>
		`);
	},
	// 串口通信端子与动态脉冲
	serial: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<rect x="3" y="6" width="18" height="12" rx="3"/>
				<circle cx="7" cy="12" r="1.5" fill="currentColor"/>
				<circle cx="12" cy="12" r="1.5" fill="currentColor"/>
				<circle cx="17" cy="12" r="1.5" fill="currentColor"/>
				<line class="fm350-anim-tx-rx" x1="6" y1="2" x2="6" y2="6" stroke="currentColor" stroke-linecap="round"/>
				<line class="fm350-anim-tx-rx-2" x1="18" y1="2" x2="18" y2="6" stroke="currentColor" stroke-linecap="round"/>
			</svg>
		`);
	},
	// 模组控制与飞行模式
	radioMode: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path class="fm350-anim-plane-fly" d="M17.8 19.2L16 11l3.5-3.5C21 6 21.5 4 21 3.5c-.5-.5-2.5 0-4 1.5L13.5 8.5 5.3 6.7c-.8-.2-1.5.1-1.8.8l-.5 1.1 5.3 3.6-3.1 3.1-2.2-.4c-.4-.1-.8.1-1 .5l-.3.6 3.1 1.8 1.8 3.1c.5.2.9-.2.5-1l-.4-2.2 3.1-3.1 3.6 5.3 1.1-.5c.7-.3 1-1 .8-1.8z" stroke-linejoin="round"/>
			</svg>
		`);
	},
	// 高危安全警示盾牌 (IMEI 维护)
	shieldDanger: function() {
		return parseSvg(`
			<svg class="fm350-svg-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
				<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/>
				<line x1="12" y1="8" x2="12" y2="12" stroke-width="2" stroke-linecap="round"/>
				<circle class="fm350-anim-danger-blink" cx="12" cy="16" r="1.2" fill="currentColor"/>
			</svg>
		`);
	},
	// 端口重新探测微标
	probe: function() {
		return parseSvg(`
			<svg style="width:14px;height:14px;display:inline-block;vertical-align:-2px;margin-right:4px;" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
				<circle cx="12" cy="12" r="9" stroke-opacity="0.4"/>
				<path class="fm350-anim-spin-cw" d="M12 3a9 9 0 0 1 9 9" stroke-linecap="round"/>
				<circle cx="12" cy="12" r="2" fill="currentColor"/>
			</svg>
		`);
	}
};

/* ---------------- 注入白色毛玻璃设计系统 ---------------- */
function injectFrostedGlassTheme() {
	var styleId = 'fm350-service-glass-styles';
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

		.fm350-service-page {
			font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
			color: var(--fm-text-main);
			padding: 4px 0 32px 0;
		}

		/* 顶部服务与串口 Hero 态势看板 */
		.fm350-serv-hero {
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
		.fm350-serv-hero::after {
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
		.fm350-hero-meta h3 {
			margin: 0 0 4px 0;
			font-size: 1.35rem;
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
		.fm350-hero-chips {
			display: flex;
			align-items: center;
			gap: 10px;
			flex-wrap: wrap;
		}
		.fm350-chip-badge {
			display: inline-flex;
			align-items: center;
			gap: 6px;
			background: rgba(255, 255, 255, 0.88);
			border: 1px solid rgba(226, 232, 240, 0.9);
			padding: 5px 12px;
			border-radius: 10px;
			font-size: 0.82rem;
			font-weight: 600;
			color: #334155;
		}
		.fm350-chip-badge code {
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			color: var(--fm-primary);
			font-size: 0.84rem;
		}

		/* ---------------- 毛玻璃表单卡片重绘 ---------------- */
		.fm350-service-page .cbi-map { margin: 0; }
		.fm350-service-page .cbi-map-descr {
			color: var(--fm-text-muted);
			font-size: 0.86rem;
			margin-bottom: 20px;
			padding-left: 2px;
		}
		.fm350-service-page .cbi-section {
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
		.fm350-service-page .cbi-section:hover {
			background: var(--fm-glass-bg-hover) !important;
			box-shadow: 0 16px 36px 0 rgba(31, 38, 135, 0.08) !important;
		}
		.fm350-service-page .cbi-section-legend {
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
		.fm350-service-page .cbi-section-descr {
			font-size: 0.8rem !important;
			color: var(--fm-text-muted) !important;
			margin-bottom: 16px !important;
			line-height: 1.55 !important;
		}
		.fm350-service-page .cbi-value {
			padding: 10px 0 !important;
			border-bottom: 1px dashed rgba(226, 232, 240, 0.5) !important;
			display: flex;
			align-items: center;
			flex-wrap: wrap;
		}
		.fm350-service-page .cbi-value:last-child {
			border-bottom: none !important;
		}
		.fm350-service-page .cbi-value-title {
			font-size: 0.88rem !important;
			font-weight: 600 !important;
			color: #475569 !important;
			width: 25% !important;
			min-width: 140px !important;
		}
		.fm350-service-page .cbi-value-field {
			flex: 1 !important;
		}

		/* ---------------- IMEI 高危区域专属毛玻璃样式 ---------------- */
		.fm350-danger-card {
			background: linear-gradient(135deg, rgba(255, 255, 255, 0.90), rgba(255, 247, 237, 0.82)) !important;
			border: 1px solid rgba(251, 146, 60, 0.35) !important;
			box-shadow: 0 12px 32px 0 rgba(249, 115, 22, 0.07) !important;
			position: relative;
		}
		.fm350-danger-card::before {
			content: "";
			position: absolute;
			top: 0; left: 0; right: 0;
			height: 3px;
			background: linear-gradient(90deg, #f59e0b, #ef4444);
			border-radius: 18px 18px 0 0;
		}

		/* 输入框与选择控件 */
		.fm350-service-page input[type="text"],
		.fm350-service-page select {
			background: rgba(255, 255, 255, 0.90) !important;
			border: 1px solid rgba(203, 213, 225, 0.9) !important;
			border-radius: 10px !important;
			padding: 7px 12px !important;
			font-size: 0.88rem !important;
			color: #1e293b !important;
			transition: all 0.2s ease !important;
			outline: none !important;
		}
		.fm350-service-page input[type="text"]:focus,
		.fm350-service-page select:focus {
			border-color: var(--fm-primary) !important;
			box-shadow: 0 0 0 3px rgba(37, 99, 235, 0.16) !important;
			background: #ffffff !important;
		}
		.fm350-service-page .cbi-value-description {
			font-size: 0.78rem !important;
			color: var(--fm-text-muted) !important;
			margin-top: 6px !important;
			line-height: 1.4 !important;
		}

		/* 按钮体系 */
		.fm350-service-page .cbi-button-apply {
			background: var(--fm-primary-gradient) !important;
			border: none !important;
			color: #ffffff !important;
			padding: 8px 18px !important;
			border-radius: 11px !important;
			font-size: 0.86rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			box-shadow: 0 4px 14px rgba(37, 99, 235, 0.26) !important;
			transition: all 0.25s ease !important;
		}
		.fm350-service-page .cbi-button-apply:hover {
			filter: brightness(1.06);
			transform: translateY(-1px);
			box-shadow: 0 6px 18px rgba(37, 99, 235, 0.35) !important;
		}
		.fm350-service-page .cbi-button-reset {
			background: rgba(254, 242, 242, 0.92) !important;
			border: 1px solid rgba(239, 68, 68, 0.3) !important;
			color: #dc2626 !important;
			padding: 8px 18px !important;
			border-radius: 11px !important;
			font-size: 0.86rem !important;
			font-weight: 600 !important;
			cursor: pointer;
			transition: all 0.25s ease !important;
		}
		.fm350-service-page .cbi-button-reset:hover {
			background: #ef4444 !important;
			color: #ffffff !important;
			border-color: #ef4444 !important;
			transform: translateY(-1px);
			box-shadow: 0 4px 14px rgba(239, 68, 68, 0.28) !important;
		}

		/* 状态微徽章 */
		.fm350-hold-badge {
			display: inline-block;
			background: rgba(15, 23, 42, 0.05);
			border: 1px solid rgba(203, 213, 225, 0.7);
			border-radius: 8px;
			padding: 6px 12px;
			font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
			font-size: 0.82rem;
			color: #334155;
			line-height: 1.5;
		}
		.fm350-hold-badge.is-warn {
			background: rgba(254, 242, 242, 0.8);
			border-color: rgba(239, 68, 68, 0.3);
			color: #dc2626;
		}
		.fm350-badge-online {
			background: rgba(16, 185, 129, 0.14);
			color: #047857;
			padding: 2px 8px;
			border-radius: 999px;
			font-size: 0.72rem;
			font-weight: 700;
		}

		/* ---------------- SVG 动效规则 ---------------- */
		.fm350-svg-icon { width: 20px; height: 20px; display: block; }
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
			animation: fm350-spin-clockwise 7s linear infinite;
		}
		.fm350-anim-spin-ccw {
			transform-origin: 17px 17px;
			animation: fm350-spin-c-clockwise 4.5s linear infinite;
		}

		@keyframes fm350-tx-rx {
			0%, 100% { opacity: 0.2; transform: translateY(0); }
			50% { opacity: 1; transform: translateY(2px); }
		}
		.fm350-anim-tx-rx { animation: fm350-tx-rx 1.4s infinite ease-in-out; }
		.fm350-anim-tx-rx-2 { animation: fm350-tx-rx 1.4s infinite ease-in-out 0.7s; }

		@keyframes fm350-plane-glide {
			0%, 100% { transform: translate(0, 0); }
			50% { transform: translate(1.5px, -1.5px); }
		}
		.fm350-anim-plane-fly { animation: fm350-plane-glide 2.5s ease-in-out infinite; }

		@keyframes fm350-danger-pulse {
			0%, 100% { opacity: 0.4; }
			50% { opacity: 1; filter: drop-shadow(0 0 3px #ef4444); }
		}
		.fm350-anim-danger-blink { animation: fm350-danger-pulse 1.8s ease-in-out infinite; }
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
			api.ports('1')
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

		var m, s, o;

		m = new form.Map('fm350', _('FM350 服务设置'),
			_('守护进程 fm350d 的运行参数。除「本地 API 端口」需重启服务生效外，其余修改保存后即时生效。'));

		/* ---------------- 一、服务 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('守护进程设置'));
		s.anonymous = false;

		o = s.option(form.Flag, 'enabled', _('启用守护进程'),
			_('关闭后不再自动周期巡检与重拨，LuCI 页面仍可手动操作'));
		o.default = '1';

		o = s.option(form.Value, 'poll_interval', _('巡检周期（秒）'),
			_('守护进程检查 PDP 状态与补齐路由的间隔，最小设定 5 秒'));
		o.datatype = 'and(uinteger,min(5))';
		o.default = '30';

		o = s.option(form.Value, 'api_port', _('本地 API 端口'),
			_('仅监听 127.0.0.1 供 rpcd ucode 代理调用，不对局域网开放'));
		o.datatype = 'and(port,min(1))';
		o.default = '8766';

		/* ---------------- 二、AT 串口 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('AT 串口管理'));
		s.description = _('插件独占 AT 口：fm350d 在整个运行期间持续持有该端口并申请排他锁，'
			+ '同样申请排他锁的程序无法打开它；若有进程绕过锁强开，下方「AT 口当前状态」会直接列出其进程号。'
			+ '请选择模组实际导出的 AT 口，通常是 /dev/ttyUSB1 或 /dev/ttyUSB2；'
			+ '改动端口后保存即生效，无需重启服务。');

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
				state += _(' 疑似 FM350');

			return p.path + tail + state;
		}

		o = s.option(form.ListValue, 'at_port', _('AT 端口选择'),
			_('候选来自后端扫描 /dev/ttyUSB* 与 /dev/ttyACM*，并附带内核驱动、USB 标识与占用探测结果'));
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
			_('仅在上方选择「手动输入其他路径…」时生效。填写设备节点绝对路径，例如 /dev/ttyUSB2'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};
		o.depends('at_port', '__custom__');

		o = s.option(form.Button, '_port_probe', _('端口占用探测'));
		o.inputtitle = _('重新探测扫描');
		o.inputstyle = 'apply';
		o.description = _('重新扫描候选 AT 端口与占用情况。已保存的选择不会改动，刷新后列表带上最新占用');
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
		o.description = _('由后端实时上报：已持续持有的秒数、累计打开 / 释放次数，以及是否有其他进程也在打开该端口。');
		o.rawhtml = true;
		o.cfgvalue = function() {
			var hold = atStats.open === true
				? (_('已独占持有时长：') + (atStats.held_secs || 0) + _(' 秒'))
				: _('尚未打开（等待首次访问独占唤醒）');

			var others = atStats.other_pids || [];
			var isWarn = others.length > 0;
			var excl = atStats.open !== true ? ''
				: (others.length
					? _(' ｜ 警告：另有 ') + others.length + _(' 个进程也在打开（PID: ') + others.join('、') + _('），排他独占被破坏！')
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

		/* ---------------- 三、模组控制 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('模组控制指令'));

		o = s.option(form.Button, '_reboot', _('模组冷复位'));
		o.inputtitle = _('重启模组 (CFUN=1,1)');
		o.inputstyle = 'reset';
		o.description = _('下发 AT+CFUN=1,1，模组底层将重新枚举并重新寻网注册');
		o.onclick = function() {
			return api.reboot().then(function(res) { notify(res, _('重启指令已下发')); });
		};

		o = s.option(form.Button, '_cfun_off', _('飞行模式切换'));
		o.inputtitle = _('进入飞行模式 (CFUN=0)');
		o.inputstyle = 'reset';
		o.onclick = function() {
			return api.cfun('0').then(function(res) { notify(res, _('已置为飞行模式')); });
		};

		o = s.option(form.Button, '_cfun_on', _('网络通信恢复'));
		o.inputtitle = _('在线模式 (CFUN=1)');
		o.inputstyle = 'apply';
		o.onclick = function() {
			return api.cfun('1').then(function(res) { notify(res, _('已恢复在线模式')); });
		};

		o = s.option(form.ListValue, '_sim', _('SIM 物理/电子卡槽'));
		o.value('0', _('卡槽 0（物理 SIM 卡）'));
		o.value('1', _('卡槽 1（板载 eSIM）'));
		o.cfgvalue = function() { return null; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_sim', _('切换当前卡槽'));
		o.inputtitle = _('下发卡槽切换');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var slot = this.section.getOption('_sim').formvalue(this.section.section) || '0';
			return api.sim(slot).then(function(res) {
				notify(res, _('SIM 卡槽切换指令已下发'));
			});
		};

		o = s.option(form.Value, '_usb', _('模组 USB 模式'));
		o.description = _('40 = RNDIS + AT（FM350 标准组合）。切换后模组将重新枚举 USB 硬件树');
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_usb', _('应用 USB 模式'));
		o.inputtitle = _('下发模式切换');
		o.inputstyle = 'reset';
		o.onclick = function() {
			var mode = this.section.getOption('_usb').formvalue(this.section.section) || '40';
			return api.usbmode(mode).then(function(res) {
				notify(res, _('USB 模式指令已下发'));
			});
		};

		/* ---------------- 四、IMEI / 串号（高危防护卡片） ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('IMEI / 设备识别码维护'));
		s.description = _('警告：写入 IMEI 属于高风险操作，操作不当可能导致设备被拒绝入网。'
			+ '仅用于合规维修、原厂串号恢复等用途。默认锁定禁止修改。');

		o = s.option(form.Flag, 'imei_write', _('解锁 IMEI 写入权限'),
			_('默认关闭。开启并保存后，仍需在下方勾选二次风险确认方允许下发'));
		o.default = '0';

		o = s.option(form.DummyValue, '_imei_now', _('当前生效 IMEI'));
		o.cfgvalue = function() { return imei.imei || _('读取失败'); };

		o = s.option(form.DummyValue, '_imei_backup', _('已有自动备份'));
		o.cfgvalue = function() { return imei.backup || _('无备份文件'); };

		o = s.option(form.Value, '_imei_new', _('设定新 IMEI 串号'),
			_('输入 15 位纯数字。前端将自动进行 Luhn 算法校验'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Flag, '_imei_confirm', _('我已知晓并确认风险'),
			_('勾选此项确认已备份原串号并承担写入后果'));
		o.cfgvalue = function() { return '0'; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_imei', _('下发写入'));
		o.inputtitle = _('执行写入 IMEI');
		o.inputstyle = 'reset';
		o.onclick = function() {
			var sec = this.section.section;
			var value = (this.section.getOption('_imei_new').formvalue(sec) || '').trim();
			var confirmed = this.section.getOption('_imei_confirm').formvalue(sec);

			if (!/^[0-9]{15}$/.test(value))
				return ui.addNotification(null, E('p', _('IMEI 必须为 15 位数字')), 'warning');
			if (!writeEnabled)
				return ui.addNotification(null,
					E('p', _('请先在上方勾选“允许写入 IMEI”并保存配置')), 'warning');
			if (!confirmed)
				return ui.addNotification(null,
					E('p', _('请勾选“我已知晓并确认风险”二次确认框')), 'warning');

			if (!imeiLuhnOk(value))
				ui.addNotification(null,
					E('p', _('提示：新 IMEI 未通过 Luhn 算法校验，多数基站网元会据此拒绝入网，请务必核实输入')),
					'warning');

			return api.imeiWrite(value, '1').then(function(res) {
				notify(res, _('IMEI 写入指令已下发'));
				if (!res || !res.ok)
					return;

				var warn = res.value && res.value.warning;
				if (warn)
					ui.addNotification(null, E('p', _(warn)), 'warning');

				setTimeout(function() { window.location.reload(); }, warn ? 3000 : 1200);
			});
		};

		o = s.option(form.Button, '_btn_imei_backup', _('手动备份原厂 IMEI'));
		o.inputtitle = _('立即备份原串号');
		o.inputstyle = 'apply';
		o.onclick = function() {
			return api.imeiBackup().then(function(res) {
				notify(res, _('原厂 IMEI 已备份'));
				if (res && res.ok)
					setTimeout(function() { window.location.reload(); }, 800);
			});
		};

		/* ---------------- 五、只读信息 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('模组硬件诊断信息'));

		o = s.option(form.DummyValue, '_imei', _('IMEI 标识'));
		o.cfgvalue = function() { return info.imei || '-'; };

		o = s.option(form.DummyValue, '_fw', _('模组基带固件'));
		o.cfgvalue = function() { return info.firmware || '-'; };

		o = s.option(form.DummyValue, '_usbnow', _('当前生效 USB 模式'));
		o.cfgvalue = function() { return info.usb_mode || '-'; };

		/* 渲染整体容器并前置插入 Hero 看板 */
		return m.render().then(function(mapNode) {
			var sections = mapNode.querySelectorAll('.cbi-section');
			if (sections && sections.length >= 4) {
				sections[3].classList.add('fm350-danger-card');
			}

			var heroBanner = E('div', { 'class': 'fm350-serv-hero' }, [
				E('div', { 'class': 'fm350-hero-left' }, [
					E('div', { 'class': 'fm350-hero-avatar' }, [ SVG_ICONS.cogs() ]),
					E('div', { 'class': 'fm350-hero-meta' }, [
						E('h3', {}, [
							_('FM350D 守护进程'),
							(uci.get('fm350', 'main', 'enabled') === '0')
								? E('span', { 'style': 'color:#ef4444;font-size:0.75rem;' }, _('○ 已停用'))
								: E('span', { 'class': 'fm350-badge-online' }, _('● 运行正常'))
						]),
						E('p', {}, _('巡检周期：') + (uci.get('fm350', 'main', 'poll_interval') || '30') + _(' 秒 · API 端口：') + (uci.get('fm350', 'main', 'api_port') || '8766'))
					])
				]),
				E('div', { 'class': 'fm350-hero-chips' }, [
					E('div', { 'class': 'fm350-chip-badge' }, [ 'AT Port', E('code', {}, curPort) ]),
					E('div', { 'class': 'fm350-chip-badge' }, [
						'排他独占',
						isExclOk ? E('span', { 'style': 'color:#10b981;font-weight:700;' }, '正常持有')
						         : E('span', { 'style': 'color:#f59e0b;font-weight:700;' }, '待命/注意')
					]),
					E('div', { 'class': 'fm350-chip-badge' }, [ '固件', E('code', {}, info.firmware || '-') ])
				])
			]);

			return E('div', { 'class': 'fm350-service-page' }, [
				heroBanner,
				mapNode
			]);
		});
	}
});