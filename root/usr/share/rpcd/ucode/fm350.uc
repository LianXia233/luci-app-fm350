#!/usr/bin/env ucode
/*
 * rpcd ucode 插件：把 LuCI 前端的 ubus 调用转发给 Rust 后端 fm350d。
 *
 * 设计要点：
 *   1. 所有 AT 口操作由 fm350d 独占持有，这里只是薄代理，避免多处争抢串口；
 *   2. 所有参数一律经 quote() 单引号转义后拼进 shell，杜绝注入；
 *   3. **不依赖 json 模块**：部分固件的 ucode（libucode20230711）不含 json 模块，
 *      因此这里只把后端的标准输出原样放进 raw 字段返回，
 *      由 LuCI JS 端做 JSON.parse。这样插件在任何 ucode 版本上都能加载。
 *   4. **必须为带参方法声明 args**：rpcd 依据方法里的 args 字段生成 ubus 方法签名。
 *      未声明时签名为空 {}，ubus 会以 UBUS_STATUS_INVALID_ARGUMENT
 *      （"Invalid argument"）拒绝任何带参调用——包括前端经 rpc.declare 发来的调用。
 *      声明中默认值的类型即签名类型；此处一律用字符串，前端也一律传字符串。
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

/* 兜底超时（秒）：rpcd 的 ucode 插件在 rpcd 主循环内**同步**执行，
 * fs.popen 挂多久，整个 ubus 通道就堵多久 —— 后端一旦卡死（如 AT 口
 * 无应答），LuCI 登录、session、uci 等所有 ubus 调用全部排队，表现为
 * 整个 Web 管理界面瘫痪。timeout 包裹保证单次调用最多占用 rpcd 这么久。
 * 读类必须大于 CLI 读类转发超时（cli.rs CLI_READ_TIMEOUT=15s）；
 * 写类（拨号全流程、短信发送等）给足余量。 */
const TMO_READ = 20;
const TMO_WRITE = 90;

/* 单引号包裹，内部单引号转义为 '\'' —— POSIX shell 安全的强引用 */
function quote(s) {
	return "'" + replace(s, "'", "'\\''") + "'";
}

/* 执行后端 CLI，把标准输出原样返回（不做 JSON 解析）。
 * to：timeout 上限（秒），缺省读类 20；被 timeout 杀掉时输出为空，
 * 走下方「后端无输出」错误分支，前端得到明确错误而非无限转圈。
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
		'status': { 'call': function(req) { return exec('fm350d status'); } },
		'info': { 'call': function(req) { return exec('fm350d info'); } },
		'signal': { 'call': function(req) { return exec('fm350d signal'); } },
		'pdp': { 'call': function(req) { return exec('fm350d pdp'); } },
		'net': { 'call': function(req) { return exec('fm350d net'); } },

		/* 候选 AT 口枚举。probe=0 时只读 sysfs，不做独占探测（更快）。 */
		'ports': {
			'args': { 'probe': '' },
			'call': function(req) {
				let probe = s(get_arg(req, 'probe', '1'));
				if (probe == '0')
					return exec('fm350d ports --no-probe');
				return exec('fm350d ports');
			}
		},
		'cell': { 'call': function(req) { return exec('fm350d cell'); } },
		'lock': { 'call': function(req) { return exec('fm350d lock'); } },
		'config': { 'call': function(req) { return exec('fm350d config'); } },

		'dial': {
			'args': { 'apn': '' },
			'call': function(req) {
			let apn = s(get_arg(req, 'apn', ''));
			if (apn != '')
				return exec('fm350d set apn ' + quote(apn) + ' && fm350d dial', TMO_WRITE);
			return exec('fm350d dial', TMO_WRITE);
			}
		},
		'hangup': { 'call': function(req) { return exec('fm350d hangup', TMO_WRITE); } },

		'at': {
			'args': { 'cmd': '' },
			'call': function(req) {
				let cmd = s(get_arg(req, 'cmd', ''));
			if (cmd == '')
				return { ok: false, error: '缺少 cmd 参数' };
			return exec('fm350d at ' + quote(cmd), TMO_WRITE);
			}
		},

		'lock_band': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
			if (args == '')
				return { ok: false, error: '缺少 args，例如 14 / 2 / 20 / 20,6,3,5078' };
			return exec('fm350d lock-band ' + quote(args), TMO_WRITE);
			}
		},
		'lock_cell': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
			if (args == '')
				return { ok: false, error: '缺少 args，例如 1,11,0,627264,280,3 或 0 取消' };
			return exec('fm350d lock-cell ' + quote(args), TMO_WRITE);
			}
		},

		'rat': {
			'args': { 'order': '' },
			'call': function(req) {
				let order = s(get_arg(req, 'order', ''));
				if (order == '')
					return exec('fm350d rat');
				return exec('fm350d rat ' + quote(order), TMO_WRITE);
			}
		},

		/* ---- IMEI / 串号 ----
		 * 读取始终允许。写入属于高风险不可逆操作，由后端加多重保护：
		 * 需先在设置页开启 imei_write，且请求必须携带 confirm=1，
		 * 后端还会校验 IMEI 格式（15 位 + Luhn）并在写入前备份原值。
		 */
		'imei_read': { 'call': function(req) { return exec('fm350d imei read'); } },
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
				return exec('fm350d imei write ' + value + ' --confirm', TMO_WRITE);
			}
		},
		'imei_backup': { 'call': function(req) { return exec('fm350d imei backup', TMO_WRITE); } },

		'sim': {
			'args': { 'slot': '' },
			'call': function(req) {
				let slot = s(get_arg(req, 'slot', '0'));
			if (slot != '0' && slot != '1')
				return { ok: false, error: '卡槽只能是 0 或 1' };
			return exec('fm350d sim ' + slot, TMO_WRITE);
			}
		},
		'cfun': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '1'));
			if (mode != '0' && mode != '1')
				return { ok: false, error: '模式只能是 0 或 1' };
			return exec('fm350d cfun ' + mode, TMO_WRITE);
			}
		},
		'usbmode': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '40'));
			if (!match(mode, /^[0-9]+$/))
				return { ok: false, error: 'USB 模式必须为数字' };
			return exec('fm350d usbmode ' + mode, TMO_WRITE);
		}
		},
		'reboot': { 'call': function(req) { return exec('fm350d reboot', TMO_WRITE); } },

		'set': {
			'args': { 'key': '', 'value': '' },
			'call': function(req) {
				let key = s(get_arg(req, 'key', ''));
				let val = s(get_arg(req, 'value', ''));
				if (key == '')
					return { ok: false, error: '缺少 key' };
			if (!match(key, /^[a-z_][a-z0-9_]*$/))
				return { ok: false, error: '键名不合法' };
			return exec('fm350d set ' + key + ' ' + quote(val), TMO_WRITE);
			}
		}
	},

	'luci.fm350.sms': {
		'list': { 'call': function(req) { return exec('fm350d sms list'); } },
		'send': {
			'args': { 'number': '', 'text': '' },
			'call': function(req) {
				let number = s(get_arg(req, 'number', ''));
				let text = s(get_arg(req, 'text', ''));
				if (number == '')
					return { ok: false, error: '缺少收件人号码' };
			if (text == '')
				return { ok: false, error: '缺少短信内容' };
			return exec('fm350d sms send ' + quote(number) + ' ' + quote(text), TMO_WRITE);
			}
		},
		'delete': {
			'args': { 'index': '' },
			'call': function(req) {
				let index = s(get_arg(req, 'index', ''));
				if (!match(index, /^[0-9]+$/))
					return { ok: false, error: '序号必须为数字' };
				return exec('fm350d sms delete ' + index);
			}
		},
		'storage': {
			'args': { 'mem': '' },
			'call': function(req) {
				let mem = s(get_arg(req, 'mem', ''));
				if (mem == '')
					return exec('fm350d sms storage');
				if (!match(mem, /^[A-Za-z]{2}$/))
					return { ok: false, error: '存储名不合法（ME / SM）' };
				return exec('fm350d sms storage ' + mem);
			}
		}
	}
};
