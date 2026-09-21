'use strict';
'require baseclass';
'require rpc';
'require ui';

/*
 * luci-app-fm350 —— 前端统一后端调用层
 *
 * 一、为什么必须用 baseclass.extend 导出
 *   LuCI 的模块加载器（luci.js compileClass）对每个 class 文件会执行：
 *       _class = _factory.apply(...);  if (!Class.isSubclass(_class)) 报错
 *   即模块**必须 return 一个 L.Class 的子类（构造器）**。
 *   直接 `return { ... }` 会触发
 *       TypeError: "fm350.api" factory yields invalid constructor
 *   导致该模块（及所有依赖它的 view）整体加载失败。
 *   本固件把 baseclass 预置在 luci.js 内：
 *       const classes = { baseclass: Class, dom: DOM, poll: Poll, ... }
 *   因此 'require baseclass' 得到的就是 L.Class，用它 extend 即可。
 *
 * 二、为什么不用 ubus.js
 *   本固件的 LuCI 未提供 /luci-static/resources/ubus.js（ubus 能力在 rpc.js 中），
 *   因此统一使用官方公开 API rpc.declare() 声明后端方法。
 *
 * 三、调用链
 *   rpc.declare → JSON-RPC /admin/ubus → rpcd
 *   → luci.fm350.*（ucode 薄代理，固件 ucode 无 json 模块，故原样回传 raw）
 *   → fm350d（Rust，独占 AT 口）
 *
 * 四、为什么这里要做"归一"
 *   fm350d 的 CLI 有两条输出路径，键名不完全一致：
 *     - 转发给守护进程：GET /api/<x>      → { ok, <x>: ... }
 *     - 本地直连 AT 口：ok_value(...)     → { ok, result: ... }
 *   为避免页面同时关心两种形态，这里按方法声明期望的键名统一取出，
 *   result 作为通用兜底。页面只需判断 res.ok 并读取 res.value。
 */

var OBJ = 'luci.fm350';
var OBJ_SMS = 'luci.fm350.sms';

function decl(object, method, params) {
	return rpc.declare({
		object: object,
		method: method,
		params: params || [],
		expect: { '': {} }
	});
}

/* 把 rpcd 代理返回的 raw 字符串解析为后端 JSON */
function parseRaw(res) {
	if (res == null)
		return { ok: false, error: _('后端无响应') };
	if (typeof res.raw === 'string') {
		try {
			return JSON.parse(res.raw);
		} catch (e) {
			return { ok: false, error: _('后端输出解析失败'), raw: res.raw };
		}
	}
	if (res.ok === undefined)
		return { ok: false, error: _('响应格式异常') };
	return res;
}

/* 按期望键名取出数据，统一收敛为 { ok, value } / { ok:false, error } */
function extract(key) {
	return function(res) {
		var r = parseRaw(res);
		if (r == null)
			return { ok: false, error: _('后端无响应') };
		if (r.ok === false)
			return { ok: false, error: r.error || _('调用失败') };
		if (key && r[key] != null)
			return { ok: true, value: r[key] };
		if (r.result != null)
			return { ok: true, value: r.result };
		return { ok: true, value: r };
	};
}

function wrap(object, method, params, key) {
	var fn = decl(object, method, params);
	var ex = extract(key);
	return function() {
		var args = arguments;
		return fn.apply(null, args).then(ex, function(e) {
			return { ok: false, error: '' + ((e && e.message) || e || _('调用失败')) };
		});
	};
}

return baseclass.extend({
	/* ---- 只读 ---- */
	status: wrap(OBJ, 'status', [], 'status'),
	info: wrap(OBJ, 'info', [], 'info'),
	signal: wrap(OBJ, 'signal', [], 'signal'),
	pdp: wrap(OBJ, 'pdp', [], 'pdp'),
	net: wrap(OBJ, 'net', [], 'net'),
	cell: wrap(OBJ, 'cell', [], 'cell'),
	lock: wrap(OBJ, 'lock', [], 'lock'),
	config: wrap(OBJ, 'config', [], 'config'),
	/* 候选 AT 口：key 传 null，保留整包（ports/current/idle_ttl/stats） */
	ports: wrap(OBJ, 'ports', [ 'probe' ], null),

	/* ---- 拨号 ---- */
	dial: wrap(OBJ, 'dial', [ 'apn' ], 'pdp'),
	hangup: wrap(OBJ, 'hangup', [], 'result'),

	/* ---- AT 透传 ---- */
	at: wrap(OBJ, 'at', [ 'cmd' ], 'result'),

	/* ---- 锁频 / 锁小区 / 制式 ---- */
	lockBand: wrap(OBJ, 'lock_band', [ 'args' ], 'result'),
	lockCell: wrap(OBJ, 'lock_cell', [ 'args' ], 'result'),
	rat: wrap(OBJ, 'rat', [ 'order' ], 'result'),

	/* ---- 模组控制 ---- */
	reboot: wrap(OBJ, 'reboot', [], 'result'),
	cfun: wrap(OBJ, 'cfun', [ 'mode' ], 'result'),
	sim: wrap(OBJ, 'sim', [ 'slot' ], 'result'),
	usbmode: wrap(OBJ, 'usbmode', [ 'mode' ], 'result'),
	set: wrap(OBJ, 'set', [ 'key', 'value' ], 'result'),

	/* ---- IMEI（写入需后端开关 + 二次确认） ---- */
	imeiRead: wrap(OBJ, 'imei_read', [], 'imei'),
	imeiBackup: wrap(OBJ, 'imei_backup', [], 'result'),
	imeiWrite: wrap(OBJ, 'imei_write', [ 'value', 'confirm' ], 'result'),

	/* ---- 短信 ---- */
	smsList: wrap(OBJ_SMS, 'list', [], 'messages'),
	smsStorage: wrap(OBJ_SMS, 'storage', [ 'mem' ], 'storage'),
	smsSend: wrap(OBJ_SMS, 'send', [ 'number', 'text' ], 'result'),
	smsDelete: wrap(OBJ_SMS, 'delete', [ 'index' ], 'result'),

	/* 载入独立样式表（只注入一次），页面本身不写内联样式 */
	injectCss: function() {
		if (document.getElementById('fm350-css'))
			return;
		var link = document.createElement('link');
		link.id = 'fm350-css';
		link.rel = 'stylesheet';
		link.type = 'text/css';
		link.href = L.resource('fm350/css/fm350.css');
		document.head.appendChild(link);
	},

	/* 统一的成功/失败提示 */
	notify: function(res, okMsg) {
		if (res && res.ok)
			ui.addNotification(null, E('p', okMsg || _('操作完成')), 'success');
		else
			ui.addNotification(null, E('p', _('操作失败：') + ((res && res.error) || _('未知错误'))), 'danger');
	}
});
