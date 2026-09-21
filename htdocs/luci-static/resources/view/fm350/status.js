'use strict';
'require view';
'require ui';
'require fm350.api as api';

/*
 * luci-app-fm350 —— 状态总览
 *
 * 数据全部来自 Rust 后端 fm350d 的实时读取（经 rpcd ucode 代理），
 * 未取到的值显示 '-'，不做占位或估算。
 *
 * 页面结构：
 *   一、网络概览   运营商 / 网络类型 / 信号强度 / 信噪比 / 服务小区 / 模组温度
 *   二、连接与接口 模组型号 / AT 通道 / PDP 上下文 / IPv4 / IPv6 / DNS
 *   三、信号细节   全部信号原始指标 + 数据来源
 *   四、温度传感器 逐路读数
 *   五、识别信息   IMEI / 序列号 / IMSI / ICCID 等
 *   六、网络接口   网卡 / 地址 / 路由
 *
 * 信号来源（实测结论，不要改回 AT+CSQ）：
 *   本模组 AT+CSQ 恒返回 99,99（不可用），信号数值必须读 AT+CESQ。
 *   AT+CESQ 字段顺序为
 *     <rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<ss_rsrq>,<ss_rsrp>,<ss_sinr>
 *   NR   (rat=9)：第 7/8/9 位（索引 6/7/8）为 SS-RSRQ / SS-RSRP / SS-SINR
 *   LTE  (rat=4)：索引 4/5 为 RSRQ / RSRP；SINR 不在 CESQ 中，取服务小区的
 *                 <rssnr_value>（该值本身即 dB）
 *   WCDMA(rat=2)：索引 2/3 为 RSCP / Ec-Io
 *   频段 / PCI / ARFCN / TAC / 小区 ID 来自 AT+GTCCINFO? 中
 *   <IsServiceCell>=1 的那一行，由后端按 <rat> 分支解析。
 *   CESQ 返回的是 3GPP 阶梯索引，须按各制式区间换算为 dBm/dB，
 *   后端已完成后端换算，此处只做展示与 1 位小数格式化。
 */

/* CSQ 0..31 映射为 0..5 格；99 及以上表示未知 */
function bars(csq) {
	var n = parseInt(csq);
	if (isNaN(n) || n < 0 || n >= 99)
		return 0;
	return Math.min(5, Math.floor(n / 6));
}

/* CSQ 不可用时按 RSRP 估算格数，仅用于可视化，不参与任何判定 */
function barsFromRsrp(rsrp) {
	var n = parseFloat(rsrp);
	if (isNaN(n))
		return 0;
	if (n >= -80) return 5;
	if (n >= -90) return 4;
	if (n >= -100) return 3;
	if (n >= -110) return 2;
	if (n >= -120) return 1;
	return 0;
}

/* 后端 rsrq / sinr / 温度均为浮点，统一保留 1 位小数 */
function fmt1(x) {
	if (x == null || x === '')
		return null;
	var n = parseFloat(x);
	if (isNaN(n))
		return '' + x;
	return (Math.round(n * 10) / 10).toFixed(1);
}

/*
 * 信号等级：定性描述，阈值集中在此便于核对。
 * 优先看 SINR（更能反映实际可用性），无 SINR 时退回 RSRP。
 * 阈值与后端整体一致，本函数只用于展示，不参与任何链路判定。
 */
function grade(rsrp, sinr) {
	var s = parseFloat(sinr);
	if (!isNaN(s))
		return s >= 20 ? 'good' : (s >= 13 ? 'ok' : (s >= 0 ? 'mid' : 'bad'));
	var r = parseFloat(rsrp);
	if (isNaN(r))
		return null;
	return r >= -80 ? 'good' : (r >= -90 ? 'ok' : (r >= -100 ? 'mid' : 'bad'));
}

function gradeText(g) {
	return g === 'good' ? _('优') : (g === 'ok' ? _('良') : (g === 'mid' ? _('中') : (g === 'bad' ? _('差') : null)));
}

/* RSRQ → 标签配色。阈值与后端 rsrq_grade_text 保持一致 */
function rsrqKind(rsrq) {
	var n = parseFloat(rsrq);
	if (isNaN(n))
		return null;
	return n >= -10 ? 'good' : (n >= -15 ? 'ok' : (n >= -19.5 ? 'mid' : 'bad'));
}

/* 综合信号得分 → 标签配色。阈值与后端 overall_grade_text 保持一致 */
function overallKind(score) {
	var n = parseFloat(score);
	if (isNaN(n))
		return null;
	return n >= 75 ? 'good' : (n >= 50 ? 'ok' : (n >= 25 ? 'mid' : 'bad'));
}

function v(x, dflt) {
	if (x == null || x === '')
		return (dflt != null) ? dflt : '-';
	return '' + x;
}

function dot(on) {
	return E('span', { 'class': on ? 'fm350-dot fm350-dot-up' : 'fm350-dot fm350-dot-down' });
}

function signalBars(level) {
	var out = [];
	for (var i = 0; i < 5; i++)
		out.push(E('span', { 'class': 'fm350-bar' + (i < level ? ' fm350-bar-on' : '') }));
	return E('span', { 'class': 'fm350-bars' }, out);
}

function tag(text, kind) {
	if (!text)
		return null;
	return E('span', { 'class': 'fm350-tag' + (kind ? ' fm350-tag-' + kind : '') }, text);
}

function card(title, valueNode, subText) {
	return E('div', { 'class': 'fm350-card' }, [
		E('h4', {}, title),
		E('div', { 'class': 'fm350-value' }, valueNode),
		subText ? E('div', { 'class': 'fm350-sub' }, subText) : ''
	]);
}

function row(key, value) {
	return E('tr', {}, [
		E('th', { 'class': 'fm350-col-key' }, key),
		E('td', {}, v(value))
	]);
}

/* 由若干片段拼出副标题，自动丢弃空值 */
function joinParts(parts) {
	return parts.filter(function(x) { return x && x !== '-'; }).join(' · ');
}

/* 运营商展示形式：可读名 (数字码)；无查表结果时只显示数字码 */
function operatorText(sig) {
	if (!sig.operator_name)
		return v(sig.operator);
	return sig.operator_name + (sig.operator ? ' (' + sig.operator + ')' : '');
}

/*
 * 传感器编号 → 名称。与后端 modem::sensor_name 保持一致
 * （FM350 AT 手册 18.3 温度传感器对照表）。
 * 编号不在表内的返回空串，页面只显示 #编号。
 */
function sensorName(id) {
	var map = {
		1: 'soc_max', 2: 'cpu_little0', 3: 'cpu_little1', 4: 'cpu_little2',
		7: 'gpu1', 8: 'dramc', 9: 'mmsys', 10: 'md_5g',
		13: 'soc_dram_ntc', 14: 'ltepa_ntc', 15: 'nrpa_ntc', 16: 'rf_ntc',
		19: 'pmic', 20: 'pmic_vcore', 21: 'pmic_vproc', 22: 'pmic_vgpu'
	};
	return map[id] || '';
}

return view.extend({
	load: function() {
		return Promise.all([
			api.status(),
			api.net()
		]);
	},

	render: function(data) {
		api.injectCss();

		var stRes = data[0] || {};
		var netRes = data[1] || {};
		var st = stRes.ok ? (stRes.value || {}) : null;
		var net = netRes.ok ? (netRes.value || null) : null;

		var info = st ? (st.info || {}) : {};
		var sig = st ? (st.signal || {}) : {};
		var pdp = st ? (st.pdp || {}) : {};
		var temp = st ? (st.temperature || null) : null;
		var sensors = (temp && temp.sensors) ? temp.sensors : [];

		/* 信号格数：CSQ 优先，CSQ 不可用时按 RSRP 估算 */
		var level = bars(sig.csq) || barsFromRsrp(sig.rsrp);
		var g = grade(sig.rsrp, sig.sinr);
		var gText = gradeText(g);

		var nodes = [];

		nodes.push(E('h2', { 'name': 'content' }, _('FM350-GL 模组状态')));

		if (!st) {
			nodes.push(E('div', { 'class': 'fm350-notice fm350-notice-danger' }, [
				_('未能从后端 fm350d 取到状态。请确认服务已启动，且 AT 口未被其它进程占用。'),
				E('br'),
				E('code', {}, v(stRes.error, ''))
			]));
		}

		nodes.push(E('div', { 'class': 'fm350-toolbar' }, [
			E('span', { 'class': 'fm350-muted' }, _('以下为本页加载时的实时快照')),
			E('span', { 'class': 'fm350-spacer' }),
			E('button', {
				'class': 'btn cbi-button cbi-button-action',
				'click': ui.createHandlerFn(this, function(ev) { ev.preventDefault(); location.reload(); })
			}, _('刷新'))
		]));

		/* ---------------- 一、网络概览 ---------------- */
		nodes.push(E('h3', {}, _('网络概览')));
		nodes.push(E('div', { 'class': 'fm350-grid fm350-grid-wide' }, [
			/*
			 * 综合信号（SIGNAL）为派生指标：后端把 RSRP / RSRQ / SINR
			 * 归一化到 0..100 后加权平均（SINR 0.40 / RSRP 0.35 / RSRQ 0.25），
			 * 只对已取到的指标计算并按可用权重重新归一化。
			 * 副标题列出参与计算的指标，避免把这个得分当成模组上报值。
			 */
			card(_('综合信号（SIGNAL）'),
				[ signalBars(sig.overall_level || 0),
				  v(sig.overall != null ? fmt1(sig.overall) + ' / 100' : null, _('未知')),
				  sig.overall_grade ? tag(sig.overall_grade, overallKind(sig.overall)) : '' ],
				(sig.overall_used && sig.overall_used.length)
					? _('由 ') + sig.overall_used.join(' + ') + _(' 加权计算')
					: _('缺少可用指标，未计算')),

			card(_('运营商'), operatorText(sig), v(sig.reg_state, _('未知'))),

			card(_('网络类型'),
				v(sig.rat, _('未知')),
				joinParts([
					sig.band ? _('频段 ') + sig.band : '',
					sig.bandwidth ? _('带宽档位 ') + sig.bandwidth : ''
				])),

			card(_('信号强度（RSRP）'),
				[ signalBars(level),
				  v(sig.rsrp != null ? sig.rsrp + ' dBm' : null, _('未知')) ],
				joinParts([
					sig.rssi_dbm != null ? 'RSSI ' + sig.rssi_dbm + ' dBm' : '',
					'CSQ ' + v(sig.csq)
				])),

			card(_('信号质量（RSRQ）'),
				[ v(sig.rsrq != null ? fmt1(sig.rsrq) + ' dB' : null, _('未知')),
				  sig.rsrq_grade ? tag(sig.rsrq_grade, rsrqKind(sig.rsrq)) : '' ],
				_('阈值：优 ≥ -10 dB，良 ≥ -15 dB，中 ≥ -19.5 dB')),

			card(_('信噪比（SINR）'),
				v(sig.sinr != null ? fmt1(sig.sinr) + ' dB' : null, _('未知')),
				joinParts([
					gText ? _('等级 ') + gText : '',
					_('阈值：优 ≥ 20 dB，良 ≥ 13 dB')
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
				]) : '')
		]));

		/* ---------------- 二、连接与接口 ---------------- */
		nodes.push(E('h3', {}, _('连接与接口')));
		nodes.push(E('div', { 'class': 'fm350-grid' }, [
			card(_('模组'),
				v(info.model, _('未知')),
				joinParts([ info.manufacturer, info.firmware ])),

			card(_('AT 通道'),
				[ dot(st && st.at_ready), v(st && st.at_ready ? _('就绪') : _('不可用')) ],
				v(st ? st.at_port : '', '')),

			card(_('PDP 上下文'),
				[ dot(pdp.active), v(pdp.active ? _('已连接') : _('未连接')) ],
				_('APN ') + v(pdp.apn) + ' · ' + v(pdp.pdp_type, '')),

			card(_('IPv4 地址'), v(pdp.ipv4, _('未分配')), v(pdp.ipv6, '')),
			card(_('DNS'), (pdp.dns && pdp.dns.length) ? pdp.dns.join(', ') : '-', _('来自 AT+GTDNS'))
		]));

		/* ---------------- 三、信号细节 ---------------- */
		nodes.push(E('h3', {}, _('信号细节')));
		nodes.push(E('table', { 'class': 'fm350-table' }, [
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
		]));

		/* ---------------- 四、温度传感器 ---------------- */
		if (temp && sensors.length) {
			nodes.push(E('h3', {}, _('温度传感器')));
			nodes.push(E('table', { 'class': 'fm350-table' }, [
				E('tbody', {}, sensors.map(function(s) {
					/* 后端类型为 Vec<(u32, f32)>，JSON 序列化为 [编号, 温度] */
					var id = Array.isArray(s) ? s[0] : (s && s.id);
					var val = Array.isArray(s) ? s[1] : (s && s.value);
					var nm = sensorName(id);
					var hot = (temp.peak != null && parseFloat(val) === parseFloat(temp.peak));
					return E('tr', { 'class': hot ? 'fm350-temp-hot' : '' }, [
						E('th', { 'class': 'fm350-col-key' }, '#' + v(id) + (nm ? ' ' + nm : '')),
						E('td', {}, fmt1(val) + ' ℃' + (hot ? _('（最高）') : ''))
					]);
				}))
			]));
		}

		/* ---------------- 五、识别信息 ---------------- */
		nodes.push(E('h3', {}, _('识别信息')));
		nodes.push(E('table', { 'class': 'fm350-table' }, [
			E('tbody', {}, [
				row('IMEI', info.imei),
				row(_('序列号'), info.serial),
				row('IMSI', info.imsi),
				row('ICCID', info.iccid),
				row(_('USB 模式'), info.usb_mode),
				row(_('SIM 卡槽'), info.sim_slot),
				row(_('短信中心'), info.sms_center)
			])
		]));

		/* ---------------- 六、网络接口 ---------------- */
		if (net) {
			nodes.push(E('h3', {}, _('网络接口')));
			nodes.push(E('table', { 'class': 'fm350-table' }, [
				E('tbody', {}, [
					row(_('IPv4 接口'), net.iface),
					row(_('IPv6 接口'), net.iface_v6),
					row(_('物理网卡'), net.dev),
					row(_('IPv4 地址'), (net.ipv4 && net.ipv4.length) ? net.ipv4.join(', ') : null),
					row(_('IPv6 地址'), (net.ipv6 && net.ipv6.length) ? net.ipv6.join(', ') : null),
					row(_('接口路由'), (net.routes && net.routes.length) ? net.routes.join('  |  ') : null),
					row(_('接口状态'), net.up ? _('已启用') : _('未启用'))
				])
			]));
		}

		nodes.push(E('div', { 'class': 'fm350-notice' }, [
			_('本页所有数值均为模组实时上报，未取到的项显示 “-”。'),
			_('「频段」在模组未上报时会按 ARFCN 推算，并以「频段来源」标注；'),
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

		return E('div', { 'class': 'fm350-wrap' }, nodes);
	}
});
