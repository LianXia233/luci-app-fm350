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

		var info = (data[1] && data[1].ok && data[1].value) || {};
		var imei = (data[2] && data[2].ok && data[2].value) || {};
		/* api.ports 的 key 为空，返回值保留整包：{ ports, current, stats } */
		var pdata = (data[3] && data[3].ok && data[3].value) || {};
		var portList = pdata.ports || [];
		var atStats = pdata.stats || {};
		var curPort = pdata.current
			|| uci.get('fm350', 'main', 'at_port')
			|| '/dev/ttyUSB1';
		/* 开关以 UCI 当前值为准（刚保存但守护进程尚未重读时，userdata 更实时） */
		var writeEnabled = ('' + uci.get('fm350', 'main', 'imei_write')) === '1'
			|| imei.write_enabled === true;
		var m, s, o;

		m = new form.Map('fm350', _('FM350 服务设置'),
			_('守护进程 fm350d 的参数。除「本地 API 端口」需重启服务生效外，其余参数保存后即时生效。'));

		s = m.section(form.NamedSection, 'main', 'fm350', _('服务'));
		s.anonymous = false;

		o = s.option(form.Flag, 'enabled', _('启用守护进程'),
			_('关闭后不再自动拨号，LuCI 页面仍可手动操作'));
		o.default = '1';

		o = s.option(form.Value, 'poll_interval', _('巡检周期（秒）'),
			_('守护进程检查 PDP 状态与补齐路由的间隔，最小 5 秒'));
		o.datatype = 'and(uinteger,min(5))';
		o.default = '30';

		o = s.option(form.Value, 'api_port', _('本地 API 端口'),
			_('仅监听 127.0.0.1，供 rpcd ucode 代理调用，不对局域网开放'));
		o.datatype = 'and(port,min(1))';
		o.default = '8766';

		s = m.section(form.NamedSection, 'main', 'fm350', _('AT 串口'));
		s.description = _('插件独占 AT 口：fm350d 在整个运行期间持续持有该端口并申请排他锁，'
			+ '同样申请排他锁的程序无法打开它；若有进程绕过锁强开，下方「AT 口当前状态」会直接列出其进程号。'
			+ '请选择模组实际导出的 AT 口，通常是 /dev/ttyUSB1 或 /dev/ttyUSB2；'
			+ '改动端口后保存即生效，无需重启服务。');

		/* 把候选端口渲染成一行人类可读描述：路径 · 驱动 · VID:PID · 产品名 · 状态 */
		function portLabel(p) {
			var parts = [];

			if (p.driver)
				parts.push(p.driver);
			if (p.vid && p.pid)
				parts.push(p.vid + ':' + p.pid);
			if (p.product)
				parts.push(p.product);

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

		o = s.option(form.ListValue, 'at_port', _('AT 端口'),
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

		/* 当前值不在候选里（自定义路径 / 设备已拔掉）时补一项，
		   否则打开页面会显示空白，保存后可能把配置写没 */
		if (!seen[curPort])
			o.value(curPort, curPort + _('（当前配置）'));

		o.value('__custom__', _('手动输入其他路径…'));

		/* 选中哨兵项时，改从下方自定义输入框取值 */
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

		o = s.option(form.Button, '_port_probe', _('端口探测'));
		o.inputtitle = _('重新探测');
		o.inputstyle = 'apply';
		o.description = _('重新扫描候选 AT 端口与占用情况。已保存的端口选择不会被改动，'
			+ '刷新页面后下拉列表会带上最新状态');
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
		o.cfgvalue = function() {
			var hold = atStats.open === true
				? (_('已持续独占 ') + (atStats.held_secs || 0) + _(' 秒'))
				: _('尚未打开（等待首次访问时独占打开）');

			var others = atStats.other_pids || [];
			var excl = atStats.open !== true ? ''
				: (others.length
					? _('｜警告：另有 ') + others.length + _(' 个进程也在打开该端口（pid ')
						+ others.join('、') + _('），独占已被破坏')
					: _('｜独占有效：无其他进程打开该端口'));

			return hold + excl + _('｜累计打开 ') + (atStats.opens || 0)
				+ _(' 次、释放 ') + (atStats.releases || 0) + _(' 次');
		};

		o = s.option(form.ListValue, 'baudrate', _('波特率'));
		[9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600].forEach(function(b) {
			o.value(String(b), String(b));
		});
		o.default = '115200';

		o = s.option(form.Value, 'at_timeout', _('指令超时（秒）'));
		o.datatype = 'and(uinteger,min(1))';
		o.default = '10';

		s = m.section(form.NamedSection, 'main', 'fm350', _('模组控制'));

		o = s.option(form.Button, '_reboot', _('重启模组'));
		o.inputtitle = _('重启模组');
		o.inputstyle = 'reset';
		o.description = _('下发 AT+CFUN=1,1，模组会重新初始化并重新注册网络');
		o.onclick = function() {
			return api.reboot().then(function(res) { notify(res, _('重启指令已下发')); });
		};

		o = s.option(form.Button, '_cfun_off', _('进入飞行模式'));
		o.inputtitle = _('飞行模式');
		o.inputstyle = 'reset';
		o.onclick = function() {
			return api.cfun('0').then(function(res) { notify(res, _('已置为飞行模式')); });
		};

		o = s.option(form.Button, '_cfun_on', _('恢复在线模式'));
		o.inputtitle = _('在线模式');
		o.inputstyle = 'apply';
		o.onclick = function() {
			return api.cfun('1').then(function(res) { notify(res, _('已恢复在线模式')); });
		};

		o = s.option(form.ListValue, '_sim', _('SIM 卡槽'));
		o.value('0', _('卡槽 0（实体卡）'));
		o.value('1', _('卡槽 1（eSIM）'));
		o.cfgvalue = function() { return null; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_sim', _('切换'));
		o.inputtitle = _('切换卡槽');
		o.inputstyle = 'apply';
		o.onclick = function() {
			var slot = this.section.getOption('_sim').formvalue(this.section.section) || '0';
			return api.sim(slot).then(function(res) {
				notify(res, _('SIM 卡槽切换指令已下发'));
			});
		};

		o = s.option(form.Value, '_usb', _('USB 模式'));
		o.description = _('40 = RNDIS + AT（FM350 常用）。切换后模组会重新枚举 USB 设备');
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_usb', _('设置'));
		o.inputtitle = _('切换 USB 模式');
		o.inputstyle = 'reset';
		o.onclick = function() {
			var mode = this.section.getOption('_usb').formvalue(this.section.section) || '40';
			return api.usbmode(mode).then(function(res) {
				notify(res, _('USB 模式指令已下发'));
			});
		};

		/* ---------------- IMEI / 串号 ---------------- */
		s = m.section(form.NamedSection, 'main', 'fm350', _('IMEI / 串号'));
		s.description = _('写入 IMEI 属于高风险且不可逆的操作，可能导致设备无法入网或违反当地法规。'
			+ '仅应在设备维修、恢复原厂串号等合法场景下使用。默认关闭。');

		o = s.option(form.Flag, 'imei_write', _('允许写入 IMEI'),
			_('默认关闭。开启后，仍需在下方的写入操作中勾选二次确认才会真正下发'));
		o.default = '0';

		o = s.option(form.DummyValue, '_imei_now', _('当前 IMEI'));
		o.cfgvalue = function() { return imei.imei || _('读取失败'); };

		o = s.option(form.DummyValue, '_imei_backup', _('已有备份'));
		o.cfgvalue = function() { return imei.backup || _('无'); };

		o = s.option(form.Value, '_imei_new', _('新 IMEI'),
			_('15 位数字。部分厂商 IMEI 的末位校验位遵循 Luhn 算法'));
		o.cfgvalue = function() { return ''; };
		o.write = function() {};

		o = s.option(form.Flag, '_imei_confirm', _('我已确认风险'),
			_('勾选后才允许下发写入指令。写入前会自动备份当前 IMEI'));
		o.cfgvalue = function() { return '0'; };
		o.write = function() {};

		o = s.option(form.Button, '_btn_imei', _('写入'));
		o.inputtitle = _('写入 IMEI');
		o.inputstyle = 'reset';
		o.onclick = function() {
			var sec = this.section.section;
			var value = (this.section.getOption('_imei_new').formvalue(sec) || '').trim();
			var confirmed = this.section.getOption('_imei_confirm').formvalue(sec);

			if (!/^[0-9]{15}$/.test(value))
				return ui.addNotification(null, E('p', _('IMEI 必须为 15 位数字')), 'warning');
			if (!writeEnabled)
				return ui.addNotification(null,
					E('p', _('请先在上方勾选“允许写入 IMEI”并保存')), 'warning');
			if (!confirmed)
				return ui.addNotification(null,
					E('p', _('请先勾选“我已确认风险”')), 'warning');

			/* Luhn 仅提示不阻断：校验位错误通常会导致网元拒绝入网 */
			if (!imeiLuhnOk(value))
				ui.addNotification(null,
					E('p', _('提示：新 IMEI 未通过 Luhn 校验位验证，多数运营商网元会据此拒绝入网，请确认输入无误')),
					'warning');

			return api.imeiWrite(value, '1').then(function(res) {
				notify(res, _('IMEI 写入已下发'));
				if (!res || !res.ok)
					return;

				/* 后端 Luhn 提示回显（若已在客户端提示过，此处仍如实展示） */
				var warn = res.value && res.value.warning;
				if (warn)
					ui.addNotification(null, E('p', _(warn)), 'warning');

				setTimeout(function() { window.location.reload(); }, warn ? 3000 : 1200);
			});
		};

		o = s.option(form.Button, '_btn_imei_backup', _('备份'));
		o.inputtitle = _('备份当前 IMEI');
		o.inputstyle = 'apply';
		o.onclick = function() {
			return api.imeiBackup().then(function(res) {
				notify(res, _('已备份当前 IMEI'));
				if (res && res.ok)
					setTimeout(function() { window.location.reload(); }, 800);
			});
		};

		s = m.section(form.NamedSection, 'main', 'fm350', _('只读信息'));

		o = s.option(form.DummyValue, '_imei', _('IMEI'));
		o.cfgvalue = function() { return info.imei || '-'; };

		o = s.option(form.DummyValue, '_fw', _('固件版本'));
		o.cfgvalue = function() { return info.firmware || '-'; };

		o = s.option(form.DummyValue, '_usbnow', _('当前 USB 模式'));
		o.cfgvalue = function() { return info.usb_mode || '-'; };

		return m.render();
	}
});
