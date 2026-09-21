'use strict';
'require view';
'require ui';
'require fm350.api as api';

/* 短信读写全部经 luci.fm350.sms 对象（rpcd ucode 代理 → fm350d） */
function notify(res, okMsg) {
	api.notify(res, okMsg);
}

var statusText = {
	0: _('已接收未读'),
	1: _('已接收已读'),
	2: _('已存储未发送'),
	3: _('已存储已发送')
};

return view.extend({
	load: function() {
		return Promise.all([
			api.smsList(),
			api.smsStorage()
		]);
	},

	render: function(data) {
		api.injectCss();

		var list = (data[0] && data[0].ok && data[0].value) || [];
		var storage = (data[1] && data[1].ok && data[1].value) || null;

		var wrap = E('div', { 'class': 'fm350-wrap' }, [
			E('h2', { 'name': 'content' }, _('短信')),
			E('div', { 'class': 'fm350-notice' },
				_('短信以 PDU 模式读写。中文自动使用 UCS2 编码，纯英文使用 GSM 7-bit 编码。'))
		]);

		/* ---------------- 存储信息 ---------------- */
		if (storage) {
			wrap.appendChild(E('p', { 'class': 'fm350-muted' },
				_('存储位置：') + storage.mem + '　' + _('已用：') + storage.used + ' / ' + storage.total));
		}

		/* ---------------- 发送表单 ---------------- */
		var inputNumber = E('input', {
			'class': 'cbi-input-text fm350-input-num',
			'type': 'text',
			'placeholder': _('收件人号码，如 13800138000')
		});
		var inputText = E('textarea', {
			'class': 'cbi-input-textarea fm350-input-msg',
			'rows': 3,
			'placeholder': _('短信内容')
		});

		var btnSend = E('button', { 'class': 'btn cbi-button cbi-button-apply', 'click': send }, _('发送'));

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
				notify(res, _('短信已发送'));
				btnSend.disabled = false;
				if (res && res.ok) {
					inputText.value = '';
					setTimeout(function() { window.location.reload(); }, 800);
				}
			});
		}

		wrap.appendChild(E('h3', {}, _('发送短信')));
		wrap.appendChild(E('div', { 'class': 'fm350-toolbar' }, [
			inputNumber,
			E('button', { 'class': 'btn', 'click': function(ev) { ev.preventDefault(); inputNumber.value = inputNumber.value.replace(/[^\d+]/g, ''); } }, _('清理号码'))
		]));
		wrap.appendChild(inputText);
		wrap.appendChild(E('div', { 'class': 'fm350-toolbar' }, [btnSend]));

		/* ---------------- 列表 ---------------- */
		wrap.appendChild(E('h3', {}, _('短信列表')));

		if (!list.length) {
			wrap.appendChild(E('p', { 'class': 'fm350-muted' }, _('暂无短信')));
		} else {
			var table = E('table', { 'class': 'fm350-table' });
			var thead = E('thead', {}, E('tr', {}, [
				E('th', {}, _('序号')),
				E('th', {}, _('状态')),
				E('th', {}, _('发件人')),
				E('th', {}, _('时间')),
				E('th', {}, _('内容')),
				E('th', {}, _('编码')),
				E('th', {}, _('操作'))
			]));
			table.appendChild(thead);

			var tbody = E('tbody');
			list.forEach(function(msg) {
				var btnDel = E('button', {
					'class': 'btn cbi-button cbi-button-remove',
					'click': function(ev) {
						ev.preventDefault();
						api.smsDelete('' + msg.index).then(function(res) {
							notify(res, _('已删除'));
							setTimeout(function() { window.location.reload(); }, 600);
						});
					}
				}, _('删除'));

				tbody.appendChild(E('tr', {}, [
					E('td', {}, String(msg.index)),
					E('td', {}, statusText[msg.status] || String(msg.status)),
					E('td', {}, msg.sender || '-'),
					E('td', {}, msg.timestamp || '-'),
					E('td', {}, msg.text || ''),
					E('td', {}, msg.encoding || '-'),
					E('td', {}, btnDel)
				]));
			});
			table.appendChild(tbody);
			wrap.appendChild(table);
		}

		/* ---------------- 刷新 ---------------- */
		wrap.appendChild(E('div', { 'class': 'fm350-toolbar' }, [
			E('button', {
				'class': 'btn',
				'click': function(ev) { ev.preventDefault(); window.location.reload(); }
			}, _('刷新'))
		]));

		return wrap;
	}
});
