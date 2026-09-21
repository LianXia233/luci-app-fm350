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

return view.extend({
	load: function() {
		return Promise.all([
			uci.load('fm350'),
			api.pdp()
		]);
	},

	render: function(data) {
		api.injectCss();

		var pdp = (data[1] && data[1].ok && data[1].value) || {};
		var m, s, o;

		m = new form.Map('fm350', _('FM350 拨号与 APN'),
			_('配置 PDP 上下文并控制拨号。修改后需点击“保存并拨号”才会真正下发到模组。'));

		s = m.section(form.NamedSection, 'main', 'fm350', _('PDP 上下文'));
		s.anonymous = false;

		o = s.option(form.Value, 'apn', _('APN'),
			_('接入点名称。移动 cmiot5g / cmnet，电信 ctnet，联通 3gnet，广电 cbnet'));
		o.rmempty = false;
		o.default = 'cmiot5g';

		o = s.option(form.ListValue, 'pdp_type', _('PDP 类型'));
		o.value('IPV4V6', _('IPv4 + IPv6'));
		o.value('IP', _('仅 IPv4'));
		o.value('IPV6', _('仅 IPv6'));
		o.default = 'IPV4V6';

		o = s.option(form.Value, 'cid', _('PDP 上下文 ID'),
			_('一般保持 1。多 PDP 场景才需要调整'));
		o.datatype = 'uinteger';
		o.default = '1';

		o = s.option(form.ListValue, 'auth', _('认证方式'));
		o.value('none', _('无认证'));
		o.value('pap', 'PAP');
		o.value('chap', 'CHAP');
		o.value('both', 'PAP / CHAP');
		o.default = 'none';

		o = s.option(form.Value, 'username', _('用户名'),
			_('多数运营商留空'));
		o.depends('auth', 'pap');
		o.depends('auth', 'chap');
		o.depends('auth', 'both');

		o = s.option(form.Value, 'password', _('密码'));
		o.password = true;
		o.depends('auth', 'pap');
		o.depends('auth', 'chap');
		o.depends('auth', 'both');
		o.password = true;

		o = s.option(form.Flag, 'auto_dial', _('开机自动拨号'),
			_('守护进程发现 PDP 未激活时自动重拨'));
		o.default = '1';

		s = m.section(form.NamedSection, 'main', 'fm350', _('当前状态'));

		o = s.option(form.DummyValue, '_state', _('连接状态'));
		o.cfgvalue = function() {
			return pdp.active ? _('已连接') : _('未连接');
		};

		o = s.option(form.DummyValue, '_addr', _('IP 地址'));
		o.cfgvalue = function() {
			var parts = [];
			if (pdp.ipv4) parts.push(pdp.ipv4);
			if (pdp.ipv6) parts.push(pdp.ipv6);
			return parts.length ? parts.join('  /  ') : _('未分配');
		};

		o = s.option(form.DummyValue, '_dns', _('DNS'));
		o.cfgvalue = function() {
			return (pdp.dns && pdp.dns.length) ? pdp.dns.join(', ') : '-';
		};

		s = m.section(form.NamedSection, 'main', 'fm350', _('操作'));

		o = s.option(form.Button, '_btn_dial', _('拨号'));
		o.inputstyle = 'apply';
		o.inputtitle = _('保存并拨号');
		o.onclick = function() {
			return uci.save()
				.then(function() {
					return api.dial(uci.get('fm350', 'main', 'apn') || '');
				})
				.then(function(res) { notify(res, _('已拨号')); })
				.catch(function(e) {
					ui.addNotification(null, E('p', _('拨号异常：') + e), 'danger');
				});
		};

		o = s.option(form.Button, '_btn_hangup', _('断开'));
		o.inputstyle = 'reset';
		o.inputtitle = _('断开连接');
		o.onclick = function() {
			return api.hangup()
				.then(function(res) { notify(res, _('已断开')); });
		};

		return m.render();
	}
});
