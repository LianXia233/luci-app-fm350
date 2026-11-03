'use strict';
'require baseclass';
'require rpc';
/* global L */
/*
 * FM350 LuCI 前端统一后端调用层（MT5700 rpc.js 的移植适配层）。
 *
 * 移植的短信中心页（view/fm350/sms.js）按 MT5700 的 AtWs 接口面编程：
 *   AtWs.client.sms(action, params)   语义化短信动作（一个动作 = 后端一次事务）
 *   AtWs.client.connect()             健康检查 + 状态机迁移
 *   AtWs.client.subscribe(fn)         新短信事件订阅（后端当前回空列表，见下）
 *   AtWs.client.onConnectionStateChange(fn)
 *   AtWs.formatFlow(bytes)            字节数人话化
 *
 * 与 MT5700 的差异（有意为之）：
 *   - 传输是 ubus → rpcd ucode → 本地 newline-JSON RPC，没有 WebSocket 层；
 *   - 事件轮询调 luci.fm350 的 events 方法，后端当前恒回空列表——新短信
 *     通知链路保留接线，后端接上 CNMI 后无需改前端；
 *   - ucode 侧返回 {ok, raw}（raw 是一行 JSON 文本），本层负责解析并把
 *     业务信封归一成 {success, result|error, cause} 供页面消费。
 */

/* 各短信动作的超时（毫秒）。与 ucode 的 TMO_READ/TMO_WRITE、后端 budget
 * 对齐：send/delete 走写预算（后端 85s 上限），list/capacity/归档走读预算。 */
var SMS_ACTION_TIMEOUT_MS = {
	send: 95000,
	delete: 45000,
	list: 45000,
	capacity: 45000,
	set_storage: 45000,
	get_smsc: 45000,
	set_smsc: 45000,
	session_open: 15000,
	session_renew: 15000,
	session_release: 15000,
	store_stats: 22000,
	store_read: 22000,
	store_drop: 22000
};

var rpcPing = L.rpc.declare({
	object: 'luci.fm350',
	method: 'ping',
	expect: { ok: false }
});

var rpcEvents = L.rpc.declare({
	object: 'luci.fm350',
	method: 'events',
	params: ['since'],
	expect: { events: [] }
});

var rpcSms = L.rpc.declare({
	object: 'luci.fm350.sms',
	method: 'sms',
	/* 与 fm350.uc 的 sms 方法 args 声明**顺序严格一致**（LuCI 按位置绑定）。 */
	params: ['action', 'destination', 'message', 'smsc', 'mem', 'indices', 'session_id',
		'ttl_ms', 'dedup_key', 'enable', 'number', 'items', 'all'],
	expect: {}
});

var rpcAt = L.rpc.declare({
	object: 'luci.fm350',
	method: 'at',
	params: ['cmd'],
	expect: {}
});

/* ubus 调用失败（rpcd 返回错误码）时 L.rpc 会 reject：统一转成
 * {success:false,...} 形态，页面按后端不可用降级处理。 */
function ubusError(err) {
	return {
		success: false,
		error: (err && err.message) ? String(err.message) : 'ubus 调用失败',
		cause: 'ubus-error'
	};
}

/* 解析 ucode 的 {ok, raw} 信封：raw 是后端一行 JSON。
 * 归一成 MT5700 页面契约：{success:true, result:<业务信封>} 或
 * {success:false, error, cause}。 */
function unwrapEnvelope(resp) {
	if (!resp || resp.ok !== true) {
		return {
			success: false,
			error: (resp && resp.error) || '后端无应答',
			cause: (resp && resp.cause) || 'not-connected'
		};
	}
	var body;
	try {
		body = JSON.parse(resp.raw);
	} catch (e) {
		return { success: false, error: '后端应答解析失败', cause: 'parse-failed' };
	}
	if (body && body.ok === true) {
		return { success: true, result: body };
	}
	return {
		success: false,
		error: (body && body.error) || '后端返回错误',
		cause: 'backend-error'
	};
}

function withTimeout(p, ms, msg) {
	return new Promise(function (resolve, reject) {
		var timer = setTimeout(function () { reject(new Error(msg || '请求超时')); }, ms);
		p.then(function (v) { clearTimeout(timer); resolve(v); },
			function (e) { clearTimeout(timer); reject(e); });
	});
}

function formatFlow(bytes) {
	var b = Number(bytes);
	if (!isFinite(b) || b < 0) b = 0;
	if (b >= 1099511627776) return (b / 1099511627776).toFixed(2) + ' TB';
	if (b >= 1073741824) return (b / 1073741824).toFixed(2) + ' GB';
	if (b >= 1048576) return (b / 1048576).toFixed(2) + ' MB';
	if (b >= 1024) return (b / 1024).toFixed(2) + ' KB';
	return Math.round(b) + ' B';
}

var ATClient = (function () {
	var api = {};

	api.state = 'disconnected';
	api.error = null;
	api.backendKnownDown = false;
	api.backendDownError = null;
	api.circuitOpenUntil = 0;
	api.pollInterval = 20000;
	api.healthTimeout = 12000;
	api.commandTimeout = 45000;

	var eventSeq = 0;
	var handlers = [];
	var pollTimer = null;
	var stateListeners = [];

	api.setState = function (state, err) {
		var prev = api.state;
		api.state = state;
		api.error = err || null;
		if (prev !== state) {
			stateListeners.forEach(function (fn) {
				try { fn(state, api.error); } catch (e) { /* 监听器异常不扩散 */ }
			});
		}
	};

	api.onConnectionStateChange = function (fn) {
		if (typeof fn === 'function')
			stateListeners.push(fn);
		try { fn(api.state, api.error); } catch (e) { /* 立即回调一次当前状态 */ }
	};

	api.offConnectionStateChange = function (fn) {
		var i = stateListeners.indexOf(fn);
		if (i >= 0) stateListeners.splice(i, 1);
	};

	/* 健康检查：ping（读类零门槛）。后端活着 → connected；熔断/失败 → error。
	 * 注意 rpcPing 用 expect:{ok:false} 声明 —— LuCI 会把应答里的 ok 字段
	 * **解包成布尔**直接返回（true/false），不是对象；两种形态都要认。 */
	api.connect = function () {
		return rpcPing().then(function (resp) {
			var alive = resp === true || (resp && typeof resp === "object" && resp.ok === true);
			if (alive) {
				api.backendKnownDown = false;
				api.backendDownError = null;
				api.setState("connected", null);
				api.startPolling();
			} else {
				api.setState("degraded", "后端未就绪");
			}
			return api.state;
		}).catch(function () {
			api.setState("error", "无法连接 FM350 后端服务");
			return api.state;
		});
	};

	/* 事件轮询：后端当前恒回空 events。轮询保持接线（将来 CNMI 新短信
	 * 事件接上后，页面无需改动即可收到推送）。 */
	api.startPolling = function () {
		if (pollTimer) return;
		pollTimer = setInterval(function () {
			if (api.state !== 'connected') return;
			Promise.resolve(rpcEvents(eventSeq)).then(function (resp) {
				var events = (resp && resp.events) || [];
				var seq = (resp && resp.seq) || 0;
				if (seq > eventSeq) eventSeq = seq;
				events.forEach(function (ev) {
					handlers.forEach(function (fn) {
						try { fn(ev); } catch (e) { /* 监听器异常不扩散 */ }
					});
				});
			}).catch(function () { /* 轮询失败静默，下一轮再试 */ });
		}, api.pollInterval);
	};

	api.subscribe = function (fn) {
		if (typeof fn === 'function' && handlers.indexOf(fn) < 0)
			handlers.push(fn);
	};

	api.unsubscribe = function (fn) {
		var i = handlers.indexOf(fn);
		if (i >= 0) handlers.splice(i, 1);
	};

	/* 语义化短信动作。params 的键名与 MT5700 页面契约一致；未定义的项
	 * 不进 JSON（LuCI 会省略 undefined），避免触发 rpcd 类型校验。 */
	api.sms = function (action, params) {
		params = params || {};
		var budget = SMS_ACTION_TIMEOUT_MS[action] || api.commandTimeout;
		var argv = [action, params.destination, params.message, params.smsc,
			params.mem, params.indices, params.session_id, params.ttl_ms,
			params.dedup_key, params.enable, params.number, params.items, params.all];
		return withTimeout(Promise.resolve(rpcSms.apply(null, argv)), budget, '请求超时')
			.then(unwrapEnvelope)
			.catch(function (err) { return ubusError(err); });
	};

	/* 透传 AT（ui.js 的通用命令助手用；短信页不直接走这里）。 */
	api.sendCommand = function (command, timeoutMs) {
		return withTimeout(
			Promise.resolve(rpcAt(command)).then(unwrapEnvelope),
			timeoutMs || api.commandTimeout,
			'命令执行超时'
		).catch(function (err) {
			return (err && err.cause) ? err : ubusError(err);
		});
	};

	return api;
})();

var AtWs = {
	client: ATClient,
	formatFlow: formatFlow,
	withTimeout: withTimeout
};

var AtWsClass = L.Class.extend(AtWs);
if (typeof window !== 'undefined') {
	window.AtWs = new AtWsClass();
}
return AtWsClass;
