#!/usr/bin/env ucode
/*
 * rpcd ucode 插件：把 LuCI 前端的 ubus 调用转发给 Rust 后端 fm350d。
 *
 * 设计要点：
 *   1. 所有 AT 口操作由 fm350d 独占持有（工作线程 + FIFO 邮箱），这里只是薄代理；
 *   2. 所有参数一律经 quote() 单引号转义后拼进 shell，杜绝注入；
 *   3. **不依赖 json 模块**：部分固件的 ucode（libucode20230711）不含 json 模块，
 *      因此这里只把后端的标准输出原样放进 raw 字段返回，
 *      由 LuCI JS 端做 JSON.parse。这样插件在任何 ucode 版本上都能加载。
 *   4. **必须为带参方法声明 args**：rpcd 依据方法里的 args 字段生成 ubus 方法签名。
 *      未声明时签名为空 {}，ubus 会以 UBUS_STATUS_INVALID_ARGUMENT
 *      （"Invalid argument"）拒绝任何带参调用——包括前端经 rpc.declare 发来的调用。
 *      声明中默认值的类型即签名类型；此处一律用字符串，前端也一律传字符串。
 *   5. **LuCI 卡死防御（rpcd 侧的三道闸）**：rpcd 的 ucode 插件在 rpcd 主循环内
 *      **同步**执行——本文件 popen 挂多久，整个 ubus 通道（菜单、session、所有
 *      应用的 LuCI 调用）就堵多久。因此：
 *        a) **存活门**：AT 依赖类调用先做零 fork 的守护存活检查
 *           （读 pidfile + /proc/<pid>/cmdline），守护不在时亚毫秒返回明确错误，
 *           绝不 popen 后干等 CLI 的本地直连回退（那要 15~90 s）；
 *        b) **读缓存**：轮询类读端点缓存 6 s（/tmp 下 ts+raw 文本文件），
 *           LuCI 高频轮询期间大幅减少 fork 与串口占用；
 *        c) **预算收紧**：读类 popen 包 timeout 12 s（须大于后端读类预算 10 s、
 *           小于前端 30 s 护栏），写类 90 s（用户主动操作，不参与轮询）。
 *
 * 暴露的 ubus 对象：
 *   luci.fm350      状态、信息、信号、PDP、网络、锁频、拨号、透传 AT、IMEI、端口枚举
 *   luci.fm350.sms  短信列表 / 发送 / 删除 / 存储
 */

'use strict';

import * as fs from 'fs';

/* 后端可执行文件绝对路径。
 * 不能写相对名：rpcd 由 procd 拉起，其环境 PATH 不一定包含 /usr/sbin，
 * 相对调用在部分固件上会静默失败（fs.popen 返回 null 或空输出），
 * 前端看到的就是「端口扫描不到 / 全部接口无输出」。绝对路径消除这一依赖。
 */
const FM350D = '/usr/sbin/fm350d';

/* 守护 pidfile：procd 管理（init.d 的 procd_set_param pidfile）。
 * 存活门据此判断守护是否在跑——纯文件读取，无 fork，亚毫秒。 */
const PID_FILE = '/var/run/fm350d.pid';

/* 读缓存目录与 TTL（秒）。轮询类读端点在 TTL 内直接回缓存，
 * 不 fork、不碰串口。TTL 取 6 s：小于 LuCI 常见轮询间隔的 2 倍，
 * 页面数据最多滞后一个 TTL，操作类端点一律不缓存。 */
const CACHE_TTL = 6;
const CACHE_DIR = '/tmp';

/* 兜底超时（秒）：被 timeout 杀掉时输出为空，走「后端无输出」错误分支，
 * 前端得到明确错误而非无限转圈。
 * 读类必须大于后端 daemon 的读类预算（api::budget READ=10s）、
 * 小于前端护栏（api.js 30s）；写类（拨号全流程、短信发送等）给足余量
 * （后端写类上限 85s）。 */
const TMO_READ = 12;
const TMO_WRITE = 90;

/* 单引号包裹，内部单引号转义为 '\'' —— POSIX shell 安全的强引用 */
function quote(s) {
	return "'" + replace(s, "'", "'\\''") + "'";
}

/* 执行后端 CLI，把标准输出原样返回（不做 JSON 解析）。
 * to：timeout 上限（秒），缺省读类 12；被 timeout 杀掉时输出为空，
 * 走「后端无输出」错误分支，前端得到明确错误而非无限转圈。
 *
 * 历史坑：本文件多数调用点写的是 exec('fm350d xxx')，而下方已拼接
 * FM350D 前缀，实际执行变成 `/usr/sbin/fm350d fm350d xxx` —— CLI 把
 * "fm350d" 当成未知子命令，打印 usage 以退出码 2 结束。前端拿到的是
 * usage 文本而非 JSON，JSON.parse 失败后所有字段显示为空。这里在
 * 拼接前剥掉重复前缀，两种写法都兼容。 */
function exec(cmd, to) {
	let t = (to != null) ? to : TMO_READ;
	if (substr(cmd, 0, 7) == 'fm350d ')
		cmd = substr(cmd, 7);
	let f = fs.popen('timeout ' + t + ' ' + FM350D + ' ' + cmd + ' 2>/dev/null');
	if (!f)
		return { ok: false, error: '无法执行 ' + FM350D + '（二进制缺失或不可执行）' };

	let data = f.read(4194304);
	f.close();

	if (data == null || length(data) == 0)
		return { ok: false, error: '后端无输出（服务未运行、AT 口被占用或处理超时 ' + t + 's）' };

	return { ok: true, raw: data };
}

/* ---- LuCI 卡死防御：三道闸的实现 ---- */

/* 存活门：守护进程是否在跑。读 pidfile → /proc/<pid>/cmdline 验证进程身份，
 * 全程零 fork（亚毫秒）。pidfile 缺失（procd 重启窗口）或 pid 已被复用
 * （cmdline 不含 fm350d）都判为不在。 */
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

/* 缓存键白名单（只允许这些字符，杜绝路径穿越）；超长键直接不缓存。 */
function cache_key_ok(key) {
	return length(key) <= 40 && match(key, /^[a-zA-Z0-9_.-]+$/) != null;
}

/* 读缓存：命中返回 { ok, raw }，未命中/过期/损坏返回 null。
 * 文件格式：第一行 unix 时间戳，其余为后端原始输出。 */
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

/* 写缓存：仅缓存成功响应；失败（含超时空输出）绝不缓存，
 * 保证故障状态最多一个 TTL 后就能反映出来。 */
function cache_put(key, raw) {
	if (!cache_key_ok(key))
		return;
	fs.writefile(CACHE_DIR + '/fm350.cache.' + key, '' + time() + '\n' + raw);
}

/* 转发入口：可选存活门（gate）+ 可选读缓存（key）。
 *   gate=true  —— AT 依赖类：守护不在时立即报错，不 popen；
 *   gate=false —— 守护无关类（ports / net / config / set）：照常执行；
 *   key 非 null —— 读缓存生效（只用于轮询类读端点，写类恒 null）。 */
function exec_cached(cmd, to, key, gate) {
	if (gate && !daemon_alive())
		return { ok: false, error: '后端守护未运行：请执行 /etc/init.d/fm350d start，或检查插件是否完整安装' };
	if (key != null) {
		let c = cache_get(key);
		if (c != null)
			return c;
	}
	let r = exec(cmd, to);
	if (r.ok && key != null)
		cache_put(key, r.raw);
	return r;
}

function get_arg(req, name, dflt) {
	if (req && req.args && req.args[name] != null)
		return req.args[name];
	return dflt;
}

/* 参数统一转字符串。
 * 注意：目标固件的 ucode（libucode20230711）不提供 String() 全局构造器
 *       （调用会抛 "left-hand side is not a function"），因此统一用字符串拼接。
 *       ubus 声明的参数类型一律为 String；转成字符串后再比较，
 *       可避免 '0' 这类字符串在 ucode 中作为真值造成的判断失误。
 */
function s(v) {
	return '' + v;
}

return {
	'luci.fm350': {
		/* 读类：存活门 + 6s 缓存（LuCI 状态页轮询的主路径） */
		'status': { 'call': function(req) { return exec_cached('fm350d status', null, 'status', true); } },
		'info': { 'call': function(req) { return exec_cached('fm350d info', null, 'info', true); } },
		'signal': { 'call': function(req) { return exec_cached('fm350d signal', null, 'signal', true); } },
		'pdp': { 'call': function(req) { return exec_cached('fm350d pdp', null, 'pdp', true); } },
		/* net 不碰 AT 口（仅查接口/路由），守护不在也可用：只缓存不加门 */
		'net': { 'call': function(req) { return exec_cached('fm350d net', null, 'net', false); } },

		/* 候选 AT 口枚举。probe=0 时只读 sysfs，不做独占探测（更快）。
		 * 守护拉不起来时它恰恰是排障入口，永不加存活门；两种 probe 分开缓存。 */
		'ports': {
			'args': { 'probe': '' },
			'call': function(req) {
				let probe = s(get_arg(req, 'probe', '1'));
				if (probe == '0')
					return exec_cached('fm350d ports --no-probe', null, 'ports.np', false);
				return exec_cached('fm350d ports', null, 'ports.p', false);
			}
		},
		'cell': { 'call': function(req) { return exec_cached('fm350d cell', null, 'cell', true); } },
		'lock': { 'call': function(req) { return exec_cached('fm350d lock', null, 'lock', true); } },
		'config': { 'call': function(req) { return exec_cached('fm350d config', null, null, false); } },

		'dial': {
			'args': { 'apn': '' },
			'call': function(req) {
			let apn = s(get_arg(req, 'apn', ''));
			if (apn != '') {
				/* 先单独落盘 APN：set 与 dial 各自输出一段 JSON，
				 * 若用 'set apn X && dial' 拼接执行，两段 JSON 直排
				 * 会让前端 JSON.parse 抛错（APN 保存页报错的根因）。
				 * 拆开执行：set 失败立即返回、不带旧 APN 继续拨；
				 * 成功则只回传 dial 的单段 JSON。daemon 巡检每轮
				 * 重读配置，落盘后下一轮自动生效。
				 * 写类不加缓存；拨号前后清掉状态类缓存，页面立即见新值。 */
				let r1 = exec_cached('fm350d set apn ' + quote(apn), TMO_WRITE, null, true);
				if (!r1.ok)
					return r1;
			}
			let r = exec_cached('fm350d dial', TMO_WRITE, null, true);
			fs.unlink(CACHE_DIR + '/fm350.cache.status');
			fs.unlink(CACHE_DIR + '/fm350.cache.pdp');
			fs.unlink(CACHE_DIR + '/fm350.cache.net');
			return r;
			}
		},
		'hangup': { 'call': function(req) { return exec_cached('fm350d hangup', TMO_WRITE, null, true); } },

		'at': {
			'args': { 'cmd': '' },
			'call': function(req) {
				let cmd = s(get_arg(req, 'cmd', ''));
			if (cmd == '')
				return { ok: false, error: '缺少 cmd 参数' };
			return exec_cached('fm350d at ' + quote(cmd), TMO_WRITE, null, true);
			}
		},

		'lock_band': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
			if (args == '')
				return { ok: false, error: '缺少 args，例如 14 / 2 / 20 / 20,6,3,5078' };
			return exec_cached('fm350d lock-band ' + quote(args), TMO_WRITE, null, true);
			}
		},
		'lock_cell': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
			if (args == '')
				return { ok: false, error: '缺少 args，例如 1,11,0,627264,280,3 或 0 取消' };
			return exec_cached('fm350d lock-cell ' + quote(args), TMO_WRITE, null, true);
			}
		},

		'rat': {
			'args': { 'order': '' },
			'call': function(req) {
				let order = s(get_arg(req, 'order', ''));
				if (order == '')
					return exec_cached('fm350d rat', null, 'rat', true);
				return exec_cached('fm350d rat ' + quote(order), TMO_WRITE, null, true);
			}
		},

		/* ---- IMEI / 串号 ----
		 * 读取始终允许。写入属于高风险不可逆操作，由后端加多重保护：
		 * 需先在设置页开启 imei_write，且请求必须携带 confirm=1，
		 * 后端还会校验 IMEI 格式（15 位 + Luhn）并在写入前备份原值。
		 */
		'imei_read': { 'call': function(req) { return exec_cached('fm350d imei read', null, 'imei', true); } },
		'imei_write': {
			'args': { 'value': '', 'confirm': '' },
			'call': function(req) {
				let value = s(get_arg(req, 'value', ''));
				let confirm = s(get_arg(req, 'confirm', ''));
				if (value == '')
					return { ok: false, error: '缺少 IMEI 值' };
				if (!match(value, /^[0-9]{15}$/))
					return { ok: false, error: 'IMEI 必须为 15 位数字' };
				/* 显式比较 '1'：ubus 参数一律为字符串，'0' 在 ucode 中是真值，不能用 ! */
				if (confirm != '1')
					return { ok: false, error: '缺少二次确认参数 confirm=1' };
				return exec_cached('fm350d imei write ' + value + ' --confirm', TMO_WRITE, null, true);
			}
		},
		'imei_backup': { 'call': function(req) { return exec_cached('fm350d imei backup', TMO_WRITE, null, true); } },

		'sim': {
			'args': { 'slot': '' },
			'call': function(req) {
				let slot = s(get_arg(req, 'slot', '0'));
			if (slot != '0' && slot != '1')
				return { ok: false, error: '卡槽只能是 0 或 1' };
			return exec_cached('fm350d sim ' + slot, TMO_WRITE, null, true);
			}
		},
		'cfun': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '1'));
			if (mode != '0' && mode != '1')
				return { ok: false, error: '模式只能是 0 或 1' };
			return exec_cached('fm350d cfun ' + mode, TMO_WRITE, null, true);
			}
		},
		'usbmode': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '40'));
			if (!match(mode, /^[0-9]+$/))
				return { ok: false, error: 'USB 模式必须为数字' };
			return exec_cached('fm350d usbmode ' + mode, TMO_WRITE, null, true);
		}
		},
		'reboot': { 'call': function(req) { return exec_cached('fm350d reboot', TMO_WRITE, null, true); } },

		/* SSH 转发巡检快照：daemon 内存只读，秒回不阻塞 rpcd */
		'sshfwd': { 'call': function(req) { return exec_cached('fm350d sshfwd', null, 'sshfwd', true); } },

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
				return exec_cached('fm350d modexec ' + ch + ' ' + quote(cmd), 30, null, false);
			}
		},

		/* SSH 公钥引导（幂等）：经 ADB 把 router 公钥写入模组 dropbear */
		'sshbootstrap': { 'call': function(req) { return exec_cached('fm350d sshbootstrap', TMO_WRITE, null, false); } },

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
			return exec_cached('fm350d set ' + key + ' ' + quote(val), TMO_WRITE, null, false);
			}
		}
	},

	'luci.fm350.sms': {
		'list': { 'call': function(req) { return exec_cached('fm350d sms list', null, null, true); } },
		'send': {
			'args': { 'number': '', 'text': '' },
			'call': function(req) {
				let number = s(get_arg(req, 'number', ''));
				let text = s(get_arg(req, 'text', ''));
				if (number == '')
					return { ok: false, error: '缺少收件人号码' };
			if (text == '')
				return { ok: false, error: '缺少短信内容' };
			return exec_cached('fm350d sms send ' + quote(number) + ' ' + quote(text), TMO_WRITE, null, true);
			}
		},
		'delete': {
			'args': { 'index': '' },
			'call': function(req) {
				let index = s(get_arg(req, 'index', ''));
				if (!match(index, /^[0-9]+$/))
					return { ok: false, error: '序号必须为数字' };
				return exec_cached('fm350d sms delete ' + index, null, null, true);
			}
		},
		'storage': {
			'args': { 'mem': '' },
			'call': function(req) {
				let mem = s(get_arg(req, 'mem', ''));
				if (mem == '')
					return exec_cached('fm350d sms storage', null, null, true);
				if (!match(mem, /^[A-Za-z]{2}$/))
					return { ok: false, error: '存储名不合法（ME / SM）' };
				return exec_cached('fm350d sms storage ' + mem, null, null, true);
			}
		}
	}
};
