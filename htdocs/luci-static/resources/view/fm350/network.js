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

		var net = (data[1] && data[1].ok && data[1].value) || {};
		var lock = (data[2] && data[2].ok && data[2].value) || [];
		var cell = (data[3] && data[3].ok && data[3].value) || [];

		var m, s, o;

		m = new form.Map('fm350', _('FM350 网络与锁频'),
			_('蜂窝接口的 IPv4 为静态 /32 地址（FM350 的 RNDIS 通道不提供 DHCP），默认路由由守护进程周期补齐。'));

		/* ---------------- 当前网络状态 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('接口状态'));
		s.anonymous = false;

		o = s.option(form.DummyValue, '_dev', _('数据通道网卡'));
		o.cfgvalue = function() { return net.dev || _('未探测到'); };

		o = s.option(form.DummyValue, '_ipv4', _('IPv4'));
		o.cfgvalue = function() { return (net.ipv4 && net.ipv4.length) ? net.ipv4.join(', ') : _('未分配'); };

		o = s.option(form.DummyValue, '_ipv6', _('IPv6'));
		o.cfgvalue = function() { return (net.ipv6 && net.ipv6.length) ? net.ipv6.join(', ') : _('未分配'); };

		o = s.option(form.DummyValue, '_routes', _('路由'));
		o.cfgvalue = function() {
			return (net.routes && net.routes.length) ? net.routes.join(' | ') : _('无');
		};

		/* ---------------- 接口参数 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('接口参数'));

		o = s.option(form.Value, 'iface', _('IPv4 接口名'));
		o.default = 'fm350';
		o.rmempty = false;

		o = s.option(form.Value, 'iface_v6', _('IPv6 接口名'),
			_('以 device=@<IPv4 接口> 方式附着，留空表示不启用 IPv6 子接口'));
		o.default = 'fm350v6';

		o = s.option(form.Flag, 'ipv6', _('启用 IPv6'));
		o.default = '1';

		o = s.option(form.Value, 'data_dev', _('数据通道网卡'),
			_('auto 表示按驱动自动探测（rndis_host / cdc_ether 等）'));
		o.default = 'auto';

		o = s.option(form.Value, 'metric', _('路由优先级'),
			_('数值越小优先级越高。若希望蜂窝作主出口，需低于有线 WAN（默认 10）'));
		o.datatype = 'uinteger';
		o.default = '30';

		o = s.option(form.Flag, 'route_guard', _('路由守护'),
			_('netifd 不会为无网关接口下发设备路由，需周期补齐 default dev 路由'));
		o.default = '1';

		/* ---------------- 锁频段 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('锁频段（AT+GTACT）'));
		s.description = _('实机验证过的取值：2 = 仅 4G，14 = 仅 5G（不限频段），20 = 自动；'
			+ '带频段如 20,6,3,5078；个别固件需写成 14,,,5041。操作会自动先离线（CFUN=0）再恢复。');

		o = s.option(form.Value, '_band_args', _('锁频参数'),
			_('直接填写 AT+GTACT= 后的参数部分'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_band', _('锁频段'));
		o.inputtitle = _('下发锁定');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var args = this.section.getOption('_band_args').formvalue(this.section.section);
			if (!args)
				return ui.addNotification(null, E('p', _('请先填写锁频参数')), 'warning');
			return api.lockBand(args).then(function(res) {
				notify(res, _('锁频指令已下发'));
				setTimeout(refresh, 1200);
			});
		};

		o = s.option(form.Button, '_btn_band_off', _('取消锁频'));
		o.inputtitle = _('恢复自动');
		o.inputstyle = 'reset';
		o.onclick = function() {
			return api.lockBand('20').then(function(res) {
				notify(res, _('已恢复自动选网'));
				setTimeout(refresh, 1200);
			});
		};

		/* ---------------- 锁小区 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('锁小区 / PCI（AT+EMMCHLCK）'));
		s.description = _('格式：AT+EMMCHLCK=1,11,0,<射频>,<PCI>,<...>；取消锁定填 0。'
			+ '可用 AT+GTCCINFO? 查看附近小区。注意：断电后锁定大概率不保存。');

		o = s.option(form.Value, '_cell_args', _('锁小区参数'),
			_('例如 1,11,0,627264,280,3；取消填 0'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_cell', _('锁小区'));
		o.inputtitle = _('下发锁定');
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

		o = s.option(form.Button, '_btn_cell_off', _('取消锁小区'));
		o.inputtitle = _('取消');
		o.inputstyle = 'reset';
		o.onclick = function() {
			return api.lockCell('0').then(function(res) {
				notify(res, _('已取消小区锁定'));
				setTimeout(refresh, 1200);
			});
		};

		/* ---------------- 制式优先级 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('制式优先级（AT+QNWPREFCFG）'));

		o = s.option(form.Value, '_rat', _('优先级顺序'),
			_('用冒号分隔，例如 NR:LTE:WCDMA。部分固件不支持该指令'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_rat', _('设置'));
		o.inputtitle = _('下发');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var order = this.section.getOption('_rat').formvalue(this.section.section);
			if (!order)
				return ui.addNotification(null, E('p', _('请先填写优先级顺序')), 'warning');
			return api.rat(order).then(function(res) { notify(res, _('已下发')); });
		};

		/* ---------------- 只读信息 ---------------- */
		if (lock.length || cell.length) {
			s = m.section(form.NamedSection, 'main', 'fm350', _('锁定状态与小区信息'));

			o = s.option(form.DummyValue, '_lock', _('当前锁定'));
			o.cfgvalue = function() {
				return lock.map(function(x) { return x[0] + '  =>  ' + String(x[1]).replace(/\n/g, ' / '); }).join('\n');
			};

			o = s.option(form.DummyValue, '_cell', _('小区 / 邻区'));
			o.cfgvalue = function() {
				return cell.map(function(x) { return x[0] + '  =>  ' + String(x[1]).replace(/\n/g, ' / '); }).join('\n');
			};
		}

		return m.render();
	}
});
