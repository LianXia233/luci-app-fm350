'use strict';
'require view';
'require ui';
'require fm350.api as api';

/* 常用指令快捷入口（均为只读或安全操作） */
var PRESETS = [
	{ label: _('模组信息'), cmd: 'ATI' },
	{ label: _('信号强度'), cmd: 'AT+CSQ' },
	{ label: _('注册状态'), cmd: 'AT+CEREG?' },
	{ label: _('运营商'), cmd: 'AT+COPS?' },
	{ label: _('PDP 上下文'), cmd: 'AT+CGDCONT?' },
	{ label: _('PDP 激活状态'), cmd: 'AT+CGACT?' },
	{ label: _('IP 地址'), cmd: 'AT+CGPADDR=1' },
	{ label: _('温度'), cmd: 'AT+GTSENRDTEMP?' },
	{ label: _('USB 模式'), cmd: 'AT+GTUSBMODE?' },
	{ label: _('邻区信息'), cmd: 'AT+GTCCINFO?' },
	{ label: _('载波聚合'), cmd: 'AT+GTCAINFO?' },
	{ label: _('锁频状态'), cmd: 'AT+GTACT?' },
	{ label: _('锁小区状态'), cmd: 'AT+EMMCHLCK?' },
	{ label: _('读取 IMEI（只读）'), cmd: 'AT+EGMREXT=0,7' }
];

return view.extend({
	render: function() {
		api.injectCss();

		var output = E('div', { 'class': 'fm350-term' }, _('等待指令…'));
		var pristine = true;

		function append(line, cls) {
			if (pristine) {
				output.innerHTML = '';
				pristine = false;
			}
			output.appendChild(E('div', { 'class': cls || '' }, line));
			output.scrollTop = output.scrollHeight;
		}

		function run(cmd) {
			if (!cmd)
				return;
			append('> ' + cmd, 'fm350-echo');
			btnRun.disabled = true;

			api.at(cmd).then(function(res) {
					btnRun.disabled = false;
					if (res && res.ok)
						append(String(res.value || '').replace(/\r/g, ''));
					else
						append(_('错误：') + ((res && res.error) || _('未知错误')), 'fm350-err');
					input.value = '';
					input.focus();
				});
		}

		var input = E('input', {
			'class': 'cbi-input-text fm350-input',
			'type': 'text',
			'placeholder': _('输入 AT 指令，例如 AT+CSQ')
		});
		input.addEventListener('keydown', function(ev) {
			if (ev.key === 'Enter') {
				ev.preventDefault();
				run(input.value.trim());
			}
		});

		var btnRun = E('button', {
			'class': 'btn cbi-button cbi-button-apply',
			'click': function(ev) { ev.preventDefault(); run(input.value.trim()); }
		}, _('执行'));

		var wrap = E('div', { 'class': 'fm350-wrap' }, [
			E('h2', { 'name': 'content' }, _('AT 终端')),
			E('div', { 'class': 'fm350-notice fm350-notice-danger' },
				_('安全策略：IMEI / 串号写入类指令（AT+EGMREXT=1,*、AT+EGMR=1,*、AT+SIMEI=*、AT+CGSN=*）'
					+ '默认被拒绝。如需写入，请到「服务设置」开启写入开关后，使用 IMEI 专用接口操作。')),
			E('div', { 'class': 'fm350-toolbar' }, [input, btnRun]),
			E('h3', {}, _('常用指令'))
		]);

		var presetBar = E('div', { 'class': 'fm350-toolbar' });
		PRESETS.forEach(function(p) {
			presetBar.appendChild(E('button', {
				'class': 'btn',
				'click': function(ev) { ev.preventDefault(); input.value = p.cmd; run(p.cmd); }
			}, p.label));
		});
		wrap.appendChild(presetBar);

		wrap.appendChild(E('h3', {}, _('响应')));
		wrap.appendChild(output);

		return wrap;
	}
});
