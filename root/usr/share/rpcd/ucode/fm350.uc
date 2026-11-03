#!/usr/bin/env ucode
/*
 * rpcd ucode 插件：把 LuCI 前端的 ubus 调用转发给 Rust 后端 fm350d。
 *
 * ## 传输层（v1.2 起）：本地 TCP newline-JSON，一请求一连接
 *
 *   LuCI JS → ubus → 本文件 → `timeout N nc 127.0.0.1 <port> < /tmp/请求`
 *   → Rust rpc 层（见 rust/src/rpc/mod.rs）→ 业务 dispatch → 一行 JSON 回来。
 *
 * 与旧「每次 popen 一个 fm350d CLI 进程再连 HTTP」相比：少一层 fork 与
 * JSON 重编码，协议从自实现 HTTP/1.1 换成一行 JSON（半包/粘包由 \n 定界
 * 天然解决）。CLI（/usr/sbin/fm350d）保留，供 SSH 手工调试。
 *
 * ## 设计要点
 *
 *   1. 所有 AT 口操作由 fm350d 独占持有（工作线程 + 邮箱），这里只是薄代理；
 *   2. **不依赖 json 模块**：部分固件的 ucode（libucode20230711）不含 json
 *      模块，因此响应行原样放进 raw 字段返回，由 LuCI JS 端做 JSON.parse。
 *      请求侧用 sprintf('%J') 序列化（ucode 内建，输出合法 JSON）。
 *   3. **必须为带参方法声明 args**：rpcd 依据方法里的 args 字段生成 ubus
 *      方法签名。声明中默认值的类型即签名类型；一律字符串（items/all 例外，
 *      与移植的 MT5700 短信中心前端绑定顺序严格一致）。
 *   4. **LuCI 卡死防御（rpcd 侧的多道闸）**：rpcd 的 ucode 插件在 rpcd
 *      主循环内**同步**执行——本文件堵多久，整个 ubus 通道就堵多久。
 *        a) **存活门**：AT 依赖类调用先做零 fork 的守护存活检查；
 *        b) **端口预检**：读 /proc/net/tcp 判断回环端口是否在监听，
 *           未监听直接快速失败（零子进程、零临时文件）；
 *        c) **熔断标记**：探明「后端不应答」后写 /tmp 标记（15 s TTL），
 *           窗口内同页面的几十个并发请求毫秒级返回，rpcd worker 不被占满；
 *        d) **硬超时**：nc 一律包 busybox timeout（popen 句柄没有 kill
 *           语义，只能靠 timeout 在子进程侧自杀）；
 *        e) **读缓存**：轮询类读端点缓存 6 s（/tmp 下 ts+raw 文本文件）。
 *   5. **rid 白名单**：客户端可传 _rid 用于请求标识，拼进临时文件路径前
 *      必须过 [A-Za-z0-9_-] 白名单（杜绝路径穿越与 shell 注入）。
 *
 * 暴露的 ubus 对象：
 *   luci.fm350      状态、信息、信号、PDP、网络、锁频、拨号、透传 AT、IMEI、端口枚举
 *   luci.fm350.sms  短信列表 / 发送 / 删除 / 存储（旧细粒度）+ 聚合动作（移植页用）
 */

'use strict';

import * as fs from 'fs';
import * as uci from 'uci';

/* ---- RPC 目标 ---- */

const PID_FILE = '/var/run/fm350d.pid';

/* 读缓存目录与 TTL（秒）。轮询类读端点在 TTL 内直接回缓存，
 * 不 fork、不碰串口。TTL 取 6 s：小于 LuCI 常见轮询间隔的 2 倍。 */
const CACHE_TTL = 6;
const CACHE_DIR = '/tmp';

/* 兜底超时（秒）：读类必须大于后端 daemon 的读类预算（api::budget READ=10s）、
 * 小于前端护栏（api.js 30s）；写类（拨号全流程、短信发送等）给足余量
 * （后端写类上限 85s）。 */
const TMO_READ = 12;
const TMO_WRITE = 90;

/* 熔断标记：探明「后端不应答」后写下，TTL 内所有请求毫秒级快速失败。
 * 对抗并发风暴的关键——第一个请求花 12s 探明后端死了之后，同页面其余
 * 几十个并发请求不该各自再花 12s（那会把 rpcd worker 耗尽）。 */
const BACKEND_DOWN_FLAG = '/tmp/fm350-backend-down';
const BACKEND_DOWN_TTL = 15;

/* 读 RPC 配置：端口来自 UCI（fm350.main.api_port），缺省 8766。 */
let _rpcPort = null;
function rpcPort() {
	if (_rpcPort == null) {
		_rpcPort = 8766;
		try {
			const cursor = uci.cursor();
			const v = cursor.get('fm350', 'main', 'api_port');
			if (v != null && match('' + v, /^[0-9]+$/) != null && int(v) > 0)
				_rpcPort = int(v);
		} catch (e) { /* 无游标/无节 → 缺省 */ }
	}
	return _rpcPort;
}

/* ---- 熔断标记 ---- */

function backendDownRemain() {
	let txt = fs.readfile(BACKEND_DOWN_FLAG);
	if (txt == null)
		return 0;
	let m = match('' + txt, /^[0-9]+/);
	if (m == null)
		return 0;
	const left = BACKEND_DOWN_TTL - (time() - int(m[0]));
	return left > 0 ? left : 0;
}

function noteBackendDown() {
	fs.writefile(BACKEND_DOWN_FLAG, '' + time());
}

function clearBackendDown() {
	try { fs.unlink(BACKEND_DOWN_FLAG); } catch (e) { /* 不存在即目标态 */ }
}

/* ---- rid 白名单：临时文件名防注入 ---- */

function safeRid(v) {
	if (v == null)
		return null;
	const s = '' + v;
	if (length(s) > 32 || match(s, /^[A-Za-z0-9_-]+$/) == null)
		return null;
	return s;
}

/* _rid 兜底自增（同一秒内多次调用也能区分） */
let ridSeq = 0;

function findBinary(cands) {
	for (let i = 0; i < length(cands); i++) {
		if (fs.stat(cands[i]) != null)
			return cands[i];
	}
	return '';
}

/* /proc/net/tcp 是否有 127.0.0.1:<port> 的 LISTEN(0A) 行。
 * 端口转十六进制大写（8766 → 223D），本地地址列形如 "0100007F:223D"；
 * 状态列必须带空格的 ' 0A '，避免匹配到 inode/计时列里的 0A 子串。 */
function toHexUpper(n) {
	const digits = '0123456789ABCDEF';
	let v = int(n);
	if (v < 0)
		v = 0;
	if (v == 0)
		return '0';
	let out = '';
	while (v > 0) {
		out = substr(digits, v % 16, 1) + out;
		v = int(v / 16);
	}
	if (length(out) % 2 == 1)
		out = '0' + out;
	return out;
}

function tcpHasListen(tcpContent, port) {
	const key = '0100007F:' + toHexUpper(port);
	let lines = split(tcpContent, '\n');
	for (let i = 0; i < length(lines); i++) {
		let line = lines[i];
		if (index(line, key) < 0)
			continue;
		if (index(line, ' 0A ') >= 0)
			return true;
	}
	return false;
}
/* ---- 单次 RPC 调用：一行 JSON 进、一行 JSON 出 ---- */

function rpcCall(method, params, timeoutSec) {
	const port = rpcPort();

	/* 熔断窗口内直接快速失败 */
	let downRemain = backendDownRemain();
	if (downRemain > 0) {
		return {
			ok: false,
			error: 'Rust 后端无应答，已暂停请求（约 ' + downRemain + 's 后自动恢复）',
			cause: 'circuit-open'
		};
	}

	/* 端口预检：/proc/net/tcp 是内核 TCP 表，行格式 "sl local_address …"，
	 * 监听态 st=0A，回环 127.0.0.1 编码为 0100007F（小端），端口十六进制大写。
	 * 全程零子进程、零副作用；读取失败时保守放行（宁可多走一次流程，
	 * 绝不误判成「服务没起来」——误判的代价是用户以为插件坏了）。 */
	let portUp = false;
	let portChecked = false;
	try {
		const tcp = fs.readfile('/proc/net/tcp');
		if (tcp != null && length(tcp) > 0) {
			portChecked = true;
			portUp = tcpHasListen('' + tcp, port);
		}
	} catch (e) {
		portChecked = false;
	}
	if (portChecked && !portUp) {
		noteBackendDown();
		return {
			ok: false,
			error: 'Rust 后端未运行（127.0.0.1:' + port + ' 未监听）：请确认 fm350d 服务已启动',
			cause: 'not-listening'
		};
	}

	/* 请求行写临时文件（%J 输出合法 JSON），nc 以 `< file` 一次送出。
	 * 文件名唯一由 rid 保证：调用方 _rid 过白名单，没传则时间戳+自增。 */
	let rid = safeRid((params != null) ? params._rid : null);
	if (rid == null) {
		rid = sprintf('%d-%d', time(), ridSeq++);
	}
	const tmp = '/tmp/fm350-rpc-' + rid + '.json';
	const payload = { id: 1, method: method, params: params };
	if (!fs.writefile(tmp, sprintf('%J', payload) + '\n')) {
		return { ok: false, error: '无法写 RPC 请求临时文件（/tmp 满？）', cause: 'tmp-write' };
	}

	/* busybox nc 候选路径：rpcd 由 procd 拉起，PATH 未必全 */
	const ncBin = findBinary(['/usr/bin/nc', '/bin/nc', '/usr/sbin/nc', '/sbin/nc']);
	if (ncBin == '') {
		try { fs.unlink(tmp); } catch (e) { /* 忽略 */ }
		return {
			ok: false,
			error: '系统缺少 nc（busybox 未编译 nc applet），无法连接后端：请安装 netcat 后重试',
			cause: 'no-nc'
		};
	}
	const timeoutBin = findBinary(['/usr/bin/timeout', '/bin/timeout', '/usr/sbin/timeout', '/sbin/timeout']);

	/* nc 硬超时：popen 句柄没有 kill 语义，子进程不退出就永久挂在 rpcd
	 * 上下文里——这是「LuCI 卡死」最经典的根因。busybox timeout 杀掉 nc
	 * 后管道关闭，read('line') 返回 null，下方把它明确区分为超时。 */
	let raw = ncBin + " 127.0.0.1 " + port + " < '" + tmp + "'";
	const budgetSec = (timeoutSec != null && timeoutSec > 0) ? timeoutSec : TMO_READ;
	if (timeoutBin != '') {
		raw = timeoutBin + ' ' + budgetSec + ' ' + raw;
	}

	let p;
	try {
		p = fs.popen(raw, 'r');
	} catch (e) {
		try { fs.unlink(tmp); } catch (e2) { /* 清不掉不影响主流程 */ }
		return { ok: false, error: '无法连接 Rust 后端: ' + e.message, cause: 'spawn-failed' };
	}
	if (!p) {
		try { fs.unlink(tmp); } catch (e2) { /* 忽略 */ }
		return { ok: false, error: '无法连接 Rust 后端', cause: 'spawn-failed' };
	}

	const line = p.read('line');
	p.close();
	try { fs.unlink(tmp); } catch (e) { /* 用完即删 */ }

	if (line == null || length(line) == 0) {
		/* 三种可能：超时被杀 / nc 连不上 / 后端进程崩了。无论哪种都写熔断
		 * 标记——同页面其余请求不再各自重复这 12s。 */
		noteBackendDown();
		return {
			ok: false,
			error: 'Rust 后端应答超时（超过 ' + budgetSec
				+ 's 未返回）：模组可能正在重连或执行长命令，请稍后重试',
			cause: timeoutBin != '' ? 'timeout' : 'no-reply'
		};
	}
	/* 后端能正常应答：清除熔断标记。注意清标记放在「拿到应答行」之后，
	 * 而不是「端口在监听」时——端口监听但进程卡死（SIGSTOP）的场景
	 * 正是要持续熔断的。 */
	clearBackendDown();
	const s = '' + line;
	if (substr(s, 0, 1) != '{') {
		return { ok: false, error: 'Rust 后端应答异常: ' + substr(s, 0, 160), cause: 'bad-reply' };
	}
	return { ok: true, raw: s };
}


/* ---- LuCI 卡死防御：存活门与读缓存 ---- */

/* 存活门：读 pidfile → /proc/<pid>/cmdline 验证进程身份，全程零 fork。 */
function daemon_alive() {
	let txt = fs.readfile(PID_FILE);
	if (txt == null)
		return false;
	let m = match('' + txt, /[0-9]+/);
	if (m == null)
		return false;
	let cmd = fs.readfile('/proc/' + m[0] + '/cmdline');
	if (cmd == null)
		return false;
	return index('' + cmd, 'fm350d') >= 0;
}

/* 缓存键白名单（杜绝路径穿越）；超长键直接不缓存。 */
function cache_key_ok(key) {
	return length(key) <= 40 && match(key, /^[a-zA-Z0-9_.-]+$/) != null;
}

/* 读缓存：命中返回 { ok, raw }，未命中/过期/损坏返回 null。 */
function cache_get(key) {
	if (!cache_key_ok(key))
		return null;
	let txt = fs.readfile(CACHE_DIR + '/fm350.cache.' + key);
	if (txt == null)
		return null;
	let s = '' + txt;
	let nl = index(s, '\n');
	if (nl < 0)
		return null;
	let m = match(substr(s, 0, nl), /^[0-9]+/);
	if (m == null)
		return null;
	let ts = 0 + m[0];
	if (time() - ts > CACHE_TTL)
		return null;
	return { ok: true, raw: substr(s, nl + 1) };
}

/* 写缓存：仅缓存成功响应；失败绝不缓存，故障状态最多一个 TTL 后就能反映。 */
function cache_put(key, raw) {
	if (!cache_key_ok(key))
		return;
	fs.writefile(CACHE_DIR + '/fm350.cache.' + key, '' + time() + '\n' + raw);
}

/* 转发入口：可选存活门（gate）+ 可选读缓存（key）。
 *   gate=true  —— AT 依赖类：守护不在时立即报错，不发起 RPC；
 *   gate=false —— 守护无关类（ports / net / config / set）：照常执行；
 *   key 非 null —— 读缓存生效（只用于轮询类读端点，写类恒 null）。 */
function rpc_cached(method, params, to, key, gate) {
	if (gate && !daemon_alive())
		return { ok: false, error: '后端守护未运行：请执行 /etc/init.d/fm350d start，或检查插件是否完整安装' };
	if (key != null) {
		let c = cache_get(key);
		if (c != null)
			return c;
	}
	let r = rpcCall(method, params, to);
	if (r.ok && key != null)
		cache_put(key, r.raw);
	return r;
}

function get_arg(req, name, dflt) {
	if (req && req.args && req.args[name] != null)
		return req.args[name];
	return dflt;
}

/* 参数统一转字符串（目标固件 ucode 无 String() 构造器，只能拼接）。 */
function s(v) {
	return '' + v;
}

/* ---------------------------------------------------------------- 对外方法 */

return {
	'luci.fm350': {
		/* 探活（移植前端的健康检查）：读类零门槛 */
		'ping': { 'call': function(req) { return rpc_cached('ping', {}, null, 'ping', false); } },

		/* ---- 异步任务管理（设计文档 §6）：提交即回 task_id ----
		 * submit 本身零耗时（任务体跑在 daemon 阻塞池），读类预算足够；
		 * 查询/取消同为秒回。同步 /api/dial 保留，前端默认交互不变。 */
		'task_submit': {
			'args': { 'kind': '' },
			'call': function(req) {
				let kind = s(get_arg(req, 'kind', ''));
				if (kind == '')
					return { ok: false, error: '缺少 kind' };
				return rpc_cached('task/submit', { kind: kind }, TMO_READ, null, true);
			}
		},
		'task': {
			'args': { 'id': '' },
			'call': function(req) {
				let id = s(get_arg(req, 'id', ''));
				let params = (id == '') ? {} : { id: id };
				return rpc_cached('task', params, TMO_READ, null, true);
			}
		},
		'task_cancel': {
			'args': { 'id': '' },
			'call': function(req) {
				let id = s(get_arg(req, 'id', ''));
				if (id == '')
					return { ok: false, error: '缺少 id' };
				return rpc_cached('task/cancel', { id: id }, TMO_READ, null, true);
			}
		},

		/* 事件轮询（移植前端的新短信通知链路）：后端当前恒回空列表 */
		'events': {
			'args': { 'since': '' },
			'call': function(req) { return rpc_cached('events', { since: s(get_arg(req, 'since', '0')) }, null, null, false); }
		},

		/* 读类：存活门 + 6s 缓存（LuCI 状态页轮询的主路径） */
		'status': { 'call': function(req) { return rpc_cached('status', {}, null, 'status', true); } },
		'info': { 'call': function(req) { return rpc_cached('info', {}, null, 'info', true); } },
		'signal': { 'call': function(req) { return rpc_cached('signal', {}, null, 'signal', true); } },
		'pdp': { 'call': function(req) { return rpc_cached('pdp', {}, null, 'pdp', true); } },
		/* net 不碰 AT 口（仅查接口/路由），守护不在也可用：只缓存不加门 */
		'net': { 'call': function(req) { return rpc_cached('net', {}, null, 'net', false); } },

		/* 候选 AT 口枚举。probe=0 时只读 sysfs，不做独占探测（更快）。
		 * 守护拉不起来时它恰恰是排障入口，永不加存活门；两种 probe 分开缓存。 */
		'ports': {
			'args': { 'probe': '' },
			'call': function(req) {
				let probe = s(get_arg(req, 'probe', '1'));
				if (probe == '0')
					return rpc_cached('ports', { probe: '0' }, null, 'ports.np', false);
				return rpc_cached('ports', { probe: '1' }, null, 'ports.p', false);
			}
		},
		'cell': { 'call': function(req) { return rpc_cached('cell', {}, null, 'cell', true); } },
		'lock': { 'call': function(req) { return rpc_cached('lock', {}, null, 'lock', true); } },
		'config': { 'call': function(req) { return rpc_cached('config', {}, null, null, false); } },

		'dial': {
			'args': { 'apn': '' },
			'call': function(req) {
				let apn = s(get_arg(req, 'apn', ''));
				if (apn != '') {
					/* 先单独落盘 APN：set 与 dial 各自一段 JSON，直排会让
					 * 前端 JSON.parse 抛错（APN 保存页报错的根因）。拆开执行：
					 * set 失败立即返回、不带旧 APN 继续拨。 */
					let r1 = rpc_cached('set', { key: 'apn', value: apn }, TMO_WRITE, null, true);
					if (!r1.ok)
						return r1;
				}
				let r = rpc_cached('dial', {}, TMO_WRITE, null, true);
				fs.unlink(CACHE_DIR + '/fm350.cache.status');
				fs.unlink(CACHE_DIR + '/fm350.cache.pdp');
				fs.unlink(CACHE_DIR + '/fm350.cache.net');
				return r;
			}
		},
		'hangup': { 'call': function(req) { return rpc_cached('hangup', {}, TMO_WRITE, null, true); } },

		'at': {
			'args': { 'cmd': '' },
			'call': function(req) {
				let cmd = s(get_arg(req, 'cmd', ''));
				if (cmd == '')
					return { ok: false, error: '缺少 cmd 参数' };
				return rpc_cached('at', { cmd: cmd }, TMO_WRITE, null, true);
			}
		},

		'lock_band': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
				if (args == '')
					return { ok: false, error: '缺少 args，例如 14 / 2 / 20 / 20,6,3,5078' };
				return rpc_cached('lock/band', { args: args }, TMO_WRITE, null, true);
			}
		},
		'lock_cell': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
				if (args == '')
					return { ok: false, error: '缺少 args，例如 1,11,0,627264,280,3 或 0 取消' };
				return rpc_cached('lock/cell', { args: args }, TMO_WRITE, null, true);
			}
		},

		'rat': {
			'args': { 'order': '' },
			'call': function(req) {
				let order = s(get_arg(req, 'order', ''));
				if (order == '')
					return rpc_cached('rat', {}, null, 'rat', true);
				return rpc_cached('rat', { order: order }, TMO_WRITE, null, true);
			}
		},

		/* ---- IMEI / 串号 ----
		 * 读取始终允许。写入属高风险不可逆操作，由后端加多重保护：
		 * 需先开启 imei_write，请求必须携带 confirm=1，写入前自动备份。
		 */
		'imei_read': { 'call': function(req) { return rpc_cached('imei', {}, null, 'imei', true); } },
		'imei_write': {
			'args': { 'value': '', 'confirm': '' },
			'call': function(req) {
				let value = s(get_arg(req, 'value', ''));
				let confirm = s(get_arg(req, 'confirm', ''));
				if (value == '')
					return { ok: false, error: '缺少 IMEI 值' };
				if (!match(value, /^[0-9]{15}$/))
					return { ok: false, error: 'IMEI 必须为 15 位数字' };
				/* 显式比较 '1'：ubus 参数一律为字符串，'0' 在 ucode 中是真值 */
				if (confirm != '1')
					return { ok: false, error: '缺少二次确认参数 confirm=1' };
				return rpc_cached('imei', { value: value, confirm: confirm }, TMO_WRITE, null, true);
			}
		},
		'imei_backup': { 'call': function(req) { return rpc_cached('imei/backup', {}, TMO_WRITE, null, true); } },

		'sim': {
			'args': { 'slot': '' },
			'call': function(req) {
				let slot = s(get_arg(req, 'slot', '0'));
				if (slot != '0' && slot != '1')
					return { ok: false, error: '卡槽只能是 0 或 1' };
				return rpc_cached('sim', { slot: slot }, TMO_WRITE, null, true);
			}
		},
		'cfun': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '1'));
				if (mode != '0' && mode != '1')
					return { ok: false, error: '模式只能是 0 或 1' };
				return rpc_cached('cfun', { mode: mode }, TMO_WRITE, null, true);
			}
		},
		'usbmode': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '40'));
				if (!match(mode, /^[0-9]+$/))
					return { ok: false, error: 'USB 模式必须为数字' };
				return rpc_cached('usbmode', { mode: mode }, TMO_WRITE, null, true);
			}
		},
		'reboot': { 'call': function(req) { return rpc_cached('reboot', {}, TMO_WRITE, null, true); } },

		/* SSH 转发巡检快照：daemon 内存只读，秒回不阻塞 rpcd */
		'sshfwd': { 'call': function(req) { return rpc_cached('sshfwd', {}, null, 'sshfwd', true); } },

		/* 模组 shell 双通道执行（ADB / SSH）：后端 15s 超时包裹，rpcd 代理给
		 * 30s 上限；adb 设备离线时后端快速失败，不会堵 ubus 通道 */
		'modexec': {
			'args': { 'channel': '', 'cmd': '' },
			'call': function(req) {
				let ch = s(get_arg(req, 'channel', ''));
				let cmd = s(get_arg(req, 'cmd', ''));
				if (ch != 'adb' && ch != 'ssh')
					return { ok: false, error: '通道只能是 adb 或 ssh' };
				if (cmd == '')
					return { ok: false, error: '缺少 cmd 参数' };
				return rpc_cached('modexec', { channel: ch, cmd: cmd }, 30, null, false);
			}
		},

		/* SSH 公钥引导（幂等）：经 ADB 把 router 公钥写入模组 dropbear */
		'sshbootstrap': { 'call': function(req) { return rpc_cached('sshfwd/bootstrap', {}, TMO_WRITE, null, false); } },

		'set': {
			'args': { 'key': '', 'value': '' },
			'call': function(req) {
				let key = s(get_arg(req, 'key', ''));
				let val = s(get_arg(req, 'value', ''));
				if (key == '')
					return { ok: false, error: '缺少 key' };
				if (!match(key, /^[a-z_][a-z0-9_]*$/))
					return { ok: false, error: '键名不合法' };
				/* 配置写完立刻清读缓存：改完配置的下一拍页面就能看到新值 */
				fs.unlink(CACHE_DIR + '/fm350.cache.status');
				fs.unlink(CACHE_DIR + '/fm350.cache.net');
				fs.unlink(CACHE_DIR + '/fm350.cache.signal');
				return rpc_cached('set', { key: key, value: val }, TMO_WRITE, null, false);
			}
		}
	},

	'luci.fm350.sms': {
		/* 聚合动作入口（移植自 MT5700 的短信中心页）。
		 * args 顺序与前端 rpc.declare 的 params 列表**严格一致**——LuCI 按
		 * 位置绑定实参，错位会让参数落到相邻字段。
		 * args 的**字面量类型**决定 rpcd 怎么校验入参（MT5700 实测教训）：
		 * indices/items 必须是数组、session_id/ttl_ms 数字、all 布尔——
		 * 声明成 '' 的话，类型不符的调用直接被 rpcd 拒回 result:[2]，
		 * 插件函数根本不会执行，前端表现为「全部动作拿不到数据」。 */
		'sms': {
			'args': { 'action': '', 'destination': '', 'message': '', 'smsc': '',
				'mem': '', 'indices': [], 'session_id': 0, 'ttl_ms': 0,
				'dedup_key': '', 'enable': false, 'number': '', 'items': [],
				'all': false, '_rid': '' },
			'call': function(req) {
				let a = req.args;
				let action = (a.action != null) ? s(a.action) : '';
				if (action == '')
					return { ok: false, error: '缺少 action' };
				/* 白名单分派 + 逐项取值透传（数据而非命令片段，无 AT 注入面） */
				let known = ['list', 'send', 'delete', 'capacity', 'get_smsc',
					'set_smsc', 'session_open', 'session_renew', 'session_release',
					'store_stats', 'store_read', 'store_drop'];
				if (index(known, action) < 0)
					return { ok: false, error: '不支持的短信动作: ' + action };
				let params = { action: action, _rid: (a._rid != null) ? s(a._rid) : null };
				if (a.destination != null)
					params.destination = s(a.destination);
				if (a.message != null)
					params.message = a.message;
				if (a.number != null)
					params.number = s(a.number);
				if (a.dedup_key != null)
					params.dedup_key = s(a.dedup_key);
				if (a.ttl_ms != null)
					params.ttl_ms = a.ttl_ms;
				if (a.session_id != null)
					params.session_id = a.session_id;
				if (a.all != null)
					params.all = a.all;
				if (type(a.items) == 'array')
					params.items = a.items;
				if (type(a.indices) == 'array')
					params.indices = a.indices;
				/* 发送/删除写类给写预算；list/capacity/会话/归档给读预算 */
				let to = (action == 'send' || action == 'delete')
					? TMO_WRITE : TMO_READ;
				return rpc_cached('sms', params, to, null, true);
			}
		},

		/* 旧细粒度接口（CLI 与旧页兼容） */
		'list': { 'call': function(req) { return rpc_cached('sms/list', {}, null, null, true); } },
		'send': {
			'args': { 'number': '', 'text': '' },
			'call': function(req) {
				let number = s(get_arg(req, 'number', ''));
				let text = get_arg(req, 'text', '');
				if (number == '')
					return { ok: false, error: '缺少收件人号码' };
				if (text == null || s(text) == '')
					return { ok: false, error: '缺少短信内容' };
				return rpc_cached('sms/send', { number: number, text: text }, TMO_WRITE, null, true);
			}
		},
		'delete': {
			'args': { 'index': '' },
			'call': function(req) {
				let index = s(get_arg(req, 'index', ''));
				if (!match(index, /^[0-9]+$/))
					return { ok: false, error: '序号必须为数字' };
				return rpc_cached('sms/delete', { index: int(index) }, TMO_WRITE, null, true);
			}
		},
		'storage': {
			'args': { 'mem': '' },
			'call': function(req) {
				let mem = s(get_arg(req, 'mem', ''));
				if (mem == '')
					return rpc_cached('sms/storage', {}, null, null, true);
				if (!match(mem, /^[A-Za-z]{2}$/))
					return { ok: false, error: '存储名不合法（ME / SM）' };
				return rpc_cached('sms/storage', { mem: mem }, TMO_WRITE, null, true);
			}
		}
	}
};
