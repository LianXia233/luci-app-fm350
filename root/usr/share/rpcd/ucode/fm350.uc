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

/* 单引号包裹，内部单引号转义为 '\'' —— POSIX shell 安全的强引用 */
function quote(s) {
	return "'" + replace(s, "'", "'\\''") + "'";
}

/* 执行后端 CLI，把标准输出原样返回（不做 JSON 解析） */
function exec(cmd) {
	let f = fs.popen(cmd + ' 2>/dev/null');
	if (!f)
		return { ok: false, error: '无法执行命令' };

	let data = f.read(4194304);
	f.close();

	if (data == null || length(data) == 0)
		return { ok: false, error: '后端无输出（服务未运行或 AT 口被占用）' };

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
					return exec('fm350d set apn ' + quote(apn) + ' && fm350d dial');
				return exec('fm350d dial');
			}
		},
		'hangup': { 'call': function(req) { return exec('fm350d hangup'); } },

		'at': {
			'args': { 'cmd': '' },
			'call': function(req) {
				let cmd = s(get_arg(req, 'cmd', ''));
				if (cmd == '')
					return { ok: false, error: '缺少 cmd 参数' };
				return exec('fm350d at ' + quote(cmd));
			}
		},

		'lock_band': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
				if (args == '')
					return { ok: false, error: '缺少 args，例如 14 / 2 / 20 / 20,6,3,5078' };
				return exec('fm350d lock-band ' + quote(args));
			}
		},
		'lock_cell': {
			'args': { 'args': '' },
			'call': function(req) {
				let args = s(get_arg(req, 'args', ''));
				if (args == '')
					return { ok: false, error: '缺少 args，例如 1,11,0,627264,280,3 或 0 取消' };
				return exec('fm350d lock-cell ' + quote(args));
			}
		},

		'rat': {
			'args': { 'order': '' },
			'call': function(req) {
				let order = s(get_arg(req, 'order', ''));
				if (order == '')
					return exec('fm350d rat');
				return exec('fm350d rat ' + quote(order));
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
				return exec('fm350d imei write ' + value + ' --confirm');
			}
		},
		'imei_backup': { 'call': function(req) { return exec('fm350d imei backup'); } },

		'sim': {
			'args': { 'slot': '' },
			'call': function(req) {
				let slot = s(get_arg(req, 'slot', '0'));
				if (slot != '0' && slot != '1')
					return { ok: false, error: '卡槽只能是 0 或 1' };
				return exec('fm350d sim ' + slot);
			}
		},
		'cfun': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '1'));
				if (mode != '0' && mode != '1')
					return { ok: false, error: '模式只能是 0 或 1' };
				return exec('fm350d cfun ' + mode);
			}
		},
		'usbmode': {
			'args': { 'mode': '' },
			'call': function(req) {
				let mode = s(get_arg(req, 'mode', '40'));
				if (!match(mode, /^[0-9]+$/))
					return { ok: false, error: 'USB 模式必须为数字' };
				return exec('fm350d usbmode ' + mode);
			}
		},
		'reboot': { 'call': function(req) { return exec('fm350d reboot'); } },

		'set': {
			'args': { 'key': '', 'value': '' },
			'call': function(req) {
				let key = s(get_arg(req, 'key', ''));
				let val = s(get_arg(req, 'value', ''));
				if (key == '')
					return { ok: false, error: '缺少 key' };
				if (!match(key, /^[a-z_][a-z0-9_]*$/))
					return { ok: false, error: '键名不合法' };
				return exec('fm350d set ' + key + ' ' + quote(val));
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
				return exec('fm350d sms send ' + quote(number) + ' ' + quote(text));
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
