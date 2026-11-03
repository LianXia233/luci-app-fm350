'use strict';
'require baseclass';
/**
 * 补充解析库：运行状态 / 扫频 / USSD / 短信 / 锁频 / 定时锁频 DTO。
 * 各解析函数与 Rust 后端的应答格式严格一致。
 * 本模块不引用其它全局库（rpc.js 反而依赖这里的 Parse），保持单向依赖。
 */

var Parse = (function () {
	var api = {};

	/* ================= 状态类 ================= */

	// AT^LENDC?
	api.parseLendc = function (text) {
		var match = text.match(/\^LENDC:\s*([\d,\s]+)/);
		if (!match) return null;
		var f = match[1].split(',').map(function (v) { return Number(v.trim()); })
			.filter(function (v) { return isFinite(v); });
		var v = f.length >= 5 ? f.slice(1) : f;
		if (v.length < 4) return null;
		return {
			available: v[0] === 1,
			plmnAvailable: v[1] === 1,
			restricted: v[2] === 0,
			established: v[3] === 1
		};
	};

	var INVALID_POWER = 999;
	var power = function (v) {
		var n = Number((v || '').trim());
		if (!isFinite(n) || n === INVALID_POWER) return null;
		return n;
	};

	// AT^TXPOWER? —— 仅 GUL(2G/3G/4G) 有效；NR 下模组返回 ERROR，需改用 AT^NTXPOWER?
	api.parseTxPower = function (text) {
		var match = text.match(/\^TXPOWER:\s*(.+)/);
		if (!match) return null;
		var f = match[1].split(',');
		if (f.length < 5) return null;
		var total = power(f[0]);
		return {
			total: total === null ? null : Number((total / 10).toFixed(1)),
			pusch: power(f[1]), pucch: power(f[2]), srs: power(f[3]), prach: power(f[4])
		};
	};

	// AT^NTXPOWER? —— NR 发射功率，最多 4 个载波，每 5 字段一组：
	// <PPusch>,<PPucch>,<PSrs>,<PPrach>,<Freq(kHz)>，无效值 999（统一交给 power() 归 null）
	api.parseNrTxPower = function (text) {
		var match = text.match(/\^NTXPOWER:\s*(.+)/);
		if (!match) return [];
		var f = match[1].split(',').map(function (s) { return s.trim(); });
		var carriers = [];
		for (var i = 0; i + 4 < f.length && carriers.length < 4; i += 5) {
			var freq = Number(f[i + 4]);
			carriers.push({
				pusch: power(f[i]), pucch: power(f[i + 1]), srs: power(f[i + 2]), prach: power(f[i + 3]),
				freq: isFinite(freq) && freq !== 0 ? freq : null
			});
		}
		return carriers;
	};


	var REG_STATES = { 0: '未注册，未搜网', 1: '已注册本地网络', 2: '未注册，搜网中', 3: '注册被拒绝', 4: '未知原因', 5: '已注册漫游网络', 8: '仅紧急业务' };
	var ACT_TYPES = { 10: 'EUTRAN-5GC', 11: 'NR-5GC' };

	// AT+C5GREG?
	api.parseC5greg = function (text) {
		var match = text.match(/\+C5GREG:\s*(.+)/);
		if (!match) return null;
		var f = match[1].split(',').map(function (s) { return s.trim().replace(/^"|"$/g, ''); });
		if (f.length < 2) return null;
		var stat = Number(f[1]);
		if (!isFinite(stat)) return null;
		var act = Number(f[4]);
		return {
			stat: stat,
			statText: REG_STATES[stat] || ('状态 ' + stat),
			registered: stat === 1 || stat === 5,
			tac: f[2] || '', ci: f[3] || '',
			act: ACT_TYPES[act] || '',
			nssai: f[6] || ''
		};
	};

	var compressIPv6 = function (groups) {
		var bestStart = -1, bestLen = 0, start = -1, len = 0;
		groups.forEach(function (g, i) {
			if (g === '0') {
				if (start < 0) start = i;
				len += 1;
				if (len > bestLen) { bestLen = len; bestStart = start; }
			} else { start = -1; len = 0; }
		});
		if (bestLen < 2) return groups.join(':');
		var head = groups.slice(0, bestStart).join(':');
		var tail = groups.slice(bestStart + bestLen).join(':');
		return head + '::' + tail;
	};

	var formatPdpAddress = function (raw) {
		var parts = raw.split('.').map(function (v) { return Number(v); });
		if (parts.length === 4 && parts.every(function (n) { return Number.isInteger(n) && n >= 0 && n <= 255; })) {
			return { address: parts.join('.'), family: 'IPv4' };
		}
		if (parts.length === 16 && parts.every(function (n) { return Number.isInteger(n) && n >= 0 && n <= 255; })) {
			var groups = [];
			for (var i = 0; i < 16; i += 2) groups.push(((parts[i] << 8) | parts[i + 1]).toString(16));
			return { address: compressIPv6(groups), family: 'IPv6' };
		}
		return { address: raw, family: raw.indexOf(':') >= 0 ? 'IPv6' : '未知' };
	};

	// AT+CGPADDR
	/*
	 * AT+CGPADDR：一行可携带**多个地址**（双栈时为 IPv4 + IPv6 各一个）。
	 * 实测：+CGPADDR: 1,"198.51.100.10","32.1.13.184.0.0.0.0.0.0.0.0.0.0.0.1"
	 * 旧实现每行只取第一个地址，IPv6 会被静默丢弃（页面显示 —）。
	 * 现解析行内全部引号包裹的地址，分别格式化后逐条返回（cid 相同）。
	 */
	api.parseCgpaddr = function (text) {
		var out = [];
		String(text).split(/\r?\n/).forEach(function (line) {
			var m = line.match(/\+CGPADDR:\s*(\d+)/);
			if (!m) return;
			var cid = Number(m[1]);
			var addrRe = /"([^"]*)"/g;
			var a;
			while ((a = addrRe.exec(line)) !== null) {
				var raw = a[1].trim();
				if (raw === '') continue;
				var fmt = formatPdpAddress(raw);
				out.push({ cid: cid, address: fmt.address, family: fmt.family });
			}
		});
		return out;
	};

	/* ================= 温度 ================= */

	/*
	 * 手册 17.1.3：^CHIPTEMP 单位 0.1℃，查询失败返回 65535；实际温度超过
	 * +120℃ 或低于 -40℃ 时模组不再给出准确读数。
	 * 失败值必须归 null 而不是 0 —— 早先返回 0 会让「没读到的传感器」在界面上
	 * 显示 0.0℃，并被温度面板计入最高温/平均温，读起来像一块极冷的芯片。
	 * 这里用 ±150℃（原始值 1500 / -400）作为粗上限挡掉明显越界的读数。
	 */
	api.parseTemperature = function (v) {
		var n = typeof v === 'number' ? v : parseInt(v, 10);
		if (isNaN(n) || n >= 65535 || n > 1500 || n < -400) return null;
		return parseFloat((n / 10).toFixed(1));
	};

	/*
	 * 手册 17.1.1/17.1.3：^CHIPTEMP 固定 12 个字段，顺序为
	 *   <sub3G PA>,<sub6G PA>,<MIMO PA>,<TCXO>,<peri1>,<peri2>,
	 *   <ap1>,<ap2>,<modem1>,<modem2>,<bbp1>,<bbp2>
	 * 单位 0.1℃（100 → 10℃），查询失败回 65535。
	 * 下标不能连着往下排：ap1/modem1 在 peri1/peri2 之后（6/8），
	 * 早先按 4/5/6 取数会把 peri1/peri2/ap1 当成 ap1/ap2/modem1 显示。
	 */
	api.parseCHIPTEMP = function (text) {
		var match = text.match(/\^CHIPTEMP:\s*(.+)/);
		if (!match) return null;
		var f = match[1].split(',').map(function (s) { return s.trim(); });
		var g = function (i) { return f[i] !== undefined ? api.parseTemperature(f[i]) : null; };
		return {
			sub3GPA: g(0), sub6GPA: g(1), mimoPa: g(2), tcxo: g(3),
			peri1: g(4), peri2: g(5), ap1: g(6), ap2: g(7),
			modem1: g(8), modem2: g(9), bbp1: g(10), bbp2: g(11)
		};
	};

	/* ================= 拨号接口配置（AT^TDCFG） ================= */

	/*
	 * 手册 16.23.1：AT^TDCFG? 的应答不是逗号串，而是「^TDCFG:」空行头 + 5 行键值：
	 *   Mode : <mode_value>   1=USB Stick+网口 E5 数传（缺省） 2=USB E5+网口 E5 数传
	 *   Dmz : <dmz_str>       IPv4 目的主机；未配置显示 "not cfg"（16.23.3）
	 *   PostRoute：<pr_value> 0=未配置（缺省） 1=打开 2=关闭
	 *   LHCM: <lanIP>,<mask>,<start>,<end>
	 *   Share-pdp：<sp_value> 0=关闭（缺省） 1=开启（仅 USB Stick 模式）
	 * 而 16.23.5 的示例又写成半角冒号且键名后无空格（"Dmz: 192.168.224.102"），
	 * 手册两处口径不一致，所以两种冒号与任意空白都要容忍 —— 否则真机的
	 * "Dmz : not cfg" 永远匹配不上，界面把已配置的 DMZ 显示成「未配置」。
	 */
	api.parseTdcfg = function (text) {
		var s = String(text);
		function field(re) {
			var m = s.match(re);
			return m ? m[1].trim() : undefined;
		}
		var mode = field(/Mode\s*[：:]\s*(\d+)/);
		var postRoute = field(/PostRoute\s*[：:]\s*(\d+)/);
		/* "not cfg" 里带空格，取到行尾而不是取到下一个空白 */
		var dmz = field(/Dmz\s*[：:]\s*([^\r\n]+)/);
		if (dmz !== undefined) dmz = dmz.replace(/^"|"$/g, '');
		var unset = dmz === undefined || dmz === 'not cfg';
		return {
			mode: mode !== undefined ? parseInt(mode, 10) : undefined,
			postRoute: postRoute !== undefined ? parseInt(postRoute, 10) : undefined,
			dmz: { enabled: !unset, host: unset ? '' : dmz }
		};
	};

	/* ================= MCS ================= */

	// AT^MCS=0 / =1
	/*
	 * 手册 13.22.1/13.22.3 的权威布局（EN-DC 下模组连回两行，LTE 在前 NR 在后）：
	 *   ^MCS: <direction>,<rat>,<mcsTableIndex>,<code0>,<code1>
	 *         [, <mcsTableIndex>,<code0>,<code1>]*     ← 从辅载波起每 3 个字段一组
	 *   direction 0=上行 1=下行；rat 0=LTE 1=NR；MCS 取值 0~31，其余为非法值
	 *   （例：LTE 上行 "^MCS: 0,0,0,6,255"、NR 下行 "^MCS: 1,1,1,6,5"）。
	 *
	 * 两个此前被误读的点：
	 *   1) f1 是 <rat>（制式标志），不是 MIMO 层数/rank —— ^MCS 根本没有层数
	 *      字段，把它当 rank 会让 NR 下恒定显示「1 层」、LTE 下显示「0 层」。
	 *      空间流信息改由手册 13.31 的 AT^TRANSMODE?（LTE 传输模式）与码字数提供。
	 *   2) 多载波时后续组是 3 个字段（table,code0,code1），不能再按 5 个切。
	 */
	api.parseMCS = function (text) {
		var lines = String(text || '').replace(/\r/g, '').split('\n');
		var entries = [];
		var direction = null;
		for (var i = 0; i < lines.length; i++) {
			var m = lines[i].match(/\^MCS:\s*(.+)/);
			if (!m) continue;
			var f = m[1].split(',').map(function (s) { return s.trim(); });
			var d = parseInt(f[0], 10);
			if (isNaN(d)) continue;
			if (direction === null) direction = d;
			var rat = parseInt(f[1], 10);
			var carriers = [];
			// f2..f4 为首载波组（table,code0,code1），其后每 3 字段一个辅载波
			for (var j = 2; j + 2 < f.length; j += 3) {
				carriers.push({
					table: parseInt(f[j], 10),
					mcs: validMcs(f[j + 1]),
					mcs2: validMcs(f[j + 2])
				});
			}
			if (!carriers.length) continue;
			entries.push({ rat: rat === 1 ? 'NR' : (rat === 0 ? 'LTE' : ''), carriers: carriers });
		}
		return entries.length ? { direction: direction, entries: entries } : null;
	};

	function validMcs(s) {
		if (s === undefined || s === '') return null;
		var n = parseInt(s, 10);
		return (n >= 0 && n <= 31) ? n : null;
	}

	/*
	 * 取指定制式的载波组；rat 传 'LTE' / 'NR'，传空则退回第一行
	 * （EN-DC 下第一行是 LTE，单制式下只有一行）。
	 */
	api.mcsEntry = function (parsed, rat) {
		if (!parsed || !parsed.entries || !parsed.entries.length) return null;
		if (rat) {
			for (var i = 0; i < parsed.entries.length; i++) {
				if (parsed.entries[i].rat === rat) return parsed.entries[i];
			}
		}
		return parsed.entries[0];
	};

	/* 主载波码字数：code1 合法（0~31）即为 2 码字空间复用，否则单码字 */
	api.mcsCodewords = function (entry) {
		if (!entry || !entry.carriers || !entry.carriers.length) return null;
		return entry.carriers[0].mcs2 != null ? 2 : 1;
	};

	/*
	 * MCS → 调制方式显示映射（3GPP TS 38.214 MCS 表 2 口径，兼顾两例锚点）：
	 *   0-9   → QPSK
	 *   10-16 → 16QAM
	 *   17-25 → 64QAM   （上行 MCS 20 → 64QAM）
	 *   26-31 → 256QAM  （下行 MCS 27 → 256QAM）
	 * 无效值返回 null，由调用方回退既有占位符。
	 */
	api.mcsModulation = function (mcs) {
		if (mcs == null || isNaN(mcs) || mcs < 0 || mcs > 31) return null;
		if (mcs <= 9) return 'QPSK';
		if (mcs <= 16) return '16QAM';
		if (mcs <= 25) return '64QAM';
		return '256QAM';
	};

	/* ================= LTE 传输模式 ================= */

	/*
	 * 手册 13.31「AT^TRANSMODE? - LTE 传输模式查询」：
	 *   ^TRANSMODE: <mode>，mode ∈ {1,2,3,4,5,6,8,9,10} → TM1…TM10（无 TM7）。
	 * 该命令只在 LTE 下有效，WCDMA/NR 下模组回 ERROR，此时返回 null 由界面显示占位。
	 */
	var TRANSMODES = { 1: 'TM1', 2: 'TM2', 3: 'TM3', 4: 'TM4', 5: 'TM5', 6: 'TM6', 8: 'TM8', 9: 'TM9', 10: 'TM10' };

	api.parseTransMode = function (text) {
		var match = String(text || '').match(/\^TRANSMODE:\s*(\d+)/);
		if (!match) return null;
		return TRANSMODES[parseInt(match[1], 10)] || null;
	};

	/* ================= IPv6 CAP ================= */


	/* ================= 扫频 ================= */

	var RAT_NAMES = { 0: 'GSM', 1: 'WCDMA', 2: 'LTE', 3: 'NR' };
	var INVALID_MEASURE = 99;
	var MAX_BAND = 512;

	var dec = function (v) { var t = v.trim(); if (t === '') return null; var n = Number(t); return isFinite(n) ? n : null; };
	var hexv = function (v) { var t = v.trim(); if (t === '') return null; var n = parseInt(t, 16); return isNaN(n) ? null : n; };
	var measure = function (v, scale) {
		var n = dec(v);
		if (n === null || n === INVALID_MEASURE) return null;
		return scale === 1 ? n : n * scale;
	};
	var parseBand = function (v) {
		var n = hexv(v);
		if (n === null || n <= 0 || n > MAX_BAND) return null;
		return n;
	};

	api.parseScanLine = function (line) {
		var idx = line.indexOf('^CELLSCAN:');
		if (idx < 0) return null;
		var body = line.slice(idx + '^CELLSCAN:'.length).trim();
		if (body === '' || /^(STARTED|OK)$/i.test(body)) return null;
		var f = body.split(',').map(function (s) { return s.trim(); });
		while (f.length < 15) f.push('');
		var rat = dec(f[0]);
		if (rat === null || !(rat in RAT_NAMES)) return null;
		return {
			rat: rat, ratName: RAT_NAMES[rat], plmn: f[1].replace(/"/g, ''),
			freq: dec(f[2]), pci: dec(f[3]), band: parseBand(f[4]),
			lac: f[5].trim(), cid: f[6].trim(),
			rxlev: dec(f[7]), bsic: dec(f[8]), psc: dec(f[9]),
			scs: dec(f[10]), rsrp: measure(f[11]),
			rsrq: measure(f[12], 0.5),
			sinr: rat === 2 ? measure(f[14], 0.125) : measure(f[13], 0.5),
			raw: line.trim()
		};
	};

	api.parseScanLines = function (lines) {
		var out = [];
		for (var i = 0; i < lines.length; i++) {
			var c = api.parseScanLine(lines[i]);
			if (c) out.push(c);
		}
		return out;
	};

	// 1<<(band-1) 十六进制位图
	api.scanBandMask = function (band) {
		return (BigInt(1) << BigInt(band - 1)).toString(16).toUpperCase();
	};

	api.buildScanCommand = function (f) {
		var rat = f.rat || '', plmn = (f.plmn || '').trim(), freq = (f.freq || '').trim();
		var pci = (f.pci || '').trim(), band = (f.band || '').trim(), scs = (f.scs || '').trim();
		if ((freq || pci) && rat === '') return { command: '', error: '指定频点或 PCI 时必须选择接入技术' };
		if (pci && !freq) return { command: '', error: '指定 PCI 时必须同时指定频点' };
		if (band && freq) return { command: '', error: '频段与频点不能同时指定' };
		if (pci && rat !== '2' && rat !== '3') return { command: '', error: '只有 LTE 与 NR 支持指定 PCI' };
		if (rat === '3' && (freq || pci) && scs === '') return { command: '', error: 'NR 指定频点或 PCI 时必须同时选择子载波间隔' };

		var bandArg = '';
		if (band) {
			var n = Number(band);
			if (!Number.isInteger(n) || n < 1 || n > MAX_BAND) return { command: '', error: '频段号超出范围（1-' + MAX_BAND + '）' };
			bandArg = api.scanBandMask(n);
		}
		var args = [rat, plmn ? '"' + plmn + '"' : '', freq, pci, bandArg, scs];
		while (args.length > 0 && args[args.length - 1] === '') args.pop();
		if (args.length === 0) return { command: 'AT^CELLSCAN' };
		return { command: 'AT^CELLSCAN=' + args.join(',') };
	};

	api.SCAN_ABORT_COMMAND = 'AT^CELLSCAN=ABORT';
	api.SCAN_STATE_COMMAND = 'AT^CELLSCAN=STATE';
	api.isScanRunning = function (text) { return /\^CELLSCAN:\s*RUNNING/i.test(text); };

	/* ================= USSD ================= */

	var packGsm7 = function (text) {
		var octets = [], acc = 0, bits = 0;
		for (var i = 0; i < text.length; i++) {
			acc |= (text.charCodeAt(i) & 0x7f) << bits;
			bits += 7;
			while (bits >= 8) { octets.push(acc & 0xff); acc >>= 8; bits -= 8; }
		}
		if (bits > 0) octets.push(acc & 0xff);
		return octets.map(function (b) { return b.toString(16).padStart(2, '0'); }).join('').toUpperCase();
	};

	var unpackGsm7 = function (hex) {
		var out = '', acc = 0, bits = 0;
		for (var i = 0; i + 1 < hex.length; i += 2) {
			var byte = parseInt(hex.slice(i, i + 2), 16);
			if (isNaN(byte)) return out;
			acc |= byte << bits;
			bits += 8;
			while (bits >= 7) { out += String.fromCharCode(acc & 0x7f); acc >>= 7; bits -= 7; }
		}
		return out.replace(/\0+$/, '');
	};

	var decodeUcs2 = function (hex) {
		var out = '';
		for (var i = 0; i + 3 < hex.length; i += 4) out += String.fromCharCode(parseInt(hex.slice(i, i + 4), 16));
		return out;
	};

	var isHex = function (s) { return /^[0-9a-fA-F]+$/.test(s) && s.length % 2 === 0; };

	api.buildUssdCommand = function (code) {
		var text = code.trim();
		if (!text) return { command: '', error: '请输入 USSD 代码，例如 *133#' };
		if (text.length > 160) return { command: '', error: 'USSD 字符串最长 160 个字符' };
		if (!/^[0-9*#+]+$/.test(text)) return { command: '', error: 'USSD 代码只能包含数字与 * # +' };
		return { command: 'AT+CUSD=1,"' + packGsm7(text) + '",15' };
	};

	api.USSD_CANCEL_COMMAND = 'AT+CUSD=2';

	var USSD_RESULT_TYPES = { 0: '网络无需回复', 1: '网络等待进一步输入', 2: '会话已被网络释放', 3: '其他客户端已响应', 4: '操作不支持', 5: '网络超时' };

	api.parseUssd = function (line) {
		var match = line.match(/\+CUSD:\s*(\d+)(?:\s*,\s*"?([^"]*)"?\s*(?:,\s*(\d+))?)?/);
		if (!match) return null;
		var m = Number(match[1]);
		var dcs = match[3] !== undefined ? Number(match[3]) : 15;
		var raw = (match[2] || '').trim();
		var text = raw;
		if (isHex(raw)) {
			if (dcs === 72) text = decodeUcs2(raw);
			else if (dcs === 68) { var a = ''; for (var i = 0; i + 1 < raw.length; i += 2) a += String.fromCharCode(parseInt(raw.slice(i, i + 2), 16)); text = a; }
			else text = unpackGsm7(raw);
		}
		return { m: m, mText: USSD_RESULT_TYPES[m] || ('状态 ' + m), text: text, needsReply: m === 1 };
	};

	/* ================= 短信 ================= */

	api.normalizePhoneNumber = function (phoneNumber) {
		if (!phoneNumber) return '';
		var normalized = phoneNumber.replace(/^\+/, '');
		if (normalized.indexOf('86') === 0 && normalized.length > 2) {
			var rest = normalized.substring(2);
			if (/^\d+$/.test(rest)) return rest;
		}
		if (/^\d+$/.test(normalized)) return normalized;
		var digits = normalized.replace(/\D/g, '');
		return digits || normalized;
	};

	api.isValidPhoneNumber = function (number) {
		return /^\d{5,19}$/.test(api.normalizePhoneNumber(number));
	};

	/*
	 * 时间串 → Date；无法识别返回 **null**（不是「此刻」）。
	 *
	 * 支持两种来源：
	 *   - `YY/MM/DD,HH:MM:SS` —— 后端 `pdu.rs` 把 TP-SCTS / TP-VP 解成这个格式
	 *     （与手册 9.9 文本模式给的时间字段同形），界面显示与排序用的都是它；
	 *   - 任何 `new Date()` 能识别的串（兜底容错）。
	 *
	 * 为什么必须返回 null：旧实现在两种都失败时返回 `new Date()`。
	 * 后果有两层：一是时间未知的老短信永远排在最新，二是 `now` 每次刷新都在变，
	 * 排序结果随之抖动，表现为会话列表「自己乱跳」。如实返回 null 后，
	 * 由调用方按「时间未知」处理，排序才是稳定的。
	 */
	api.parseMessageTime = function (timeStr) {
		if (!timeStr) return null;
		var match = String(timeStr).trim().match(/(\d{2})\/(\d{2})\/(\d{2}),(\d{2}):(\d{2}):(\d{2})/);
		if (match) {
			return new Date(2000 + parseInt(match[1], 10), parseInt(match[2], 10) - 1, parseInt(match[3], 10),
				parseInt(match[4], 10), parseInt(match[5], 10), parseInt(match[6], 10));
		}
		var parsed = new Date(timeStr);
		return isNaN(parsed.getTime()) ? null : parsed;
	};

	/* 时间串 → 毫秒时间戳；时间未知返回 null。排序与「最近一条」判定统一走这里。 */
	api.messageTimeMs = function (timeStr) {
		var d = api.parseMessageTime(timeStr);
		return d ? d.getTime() : null;
	};

	/* ================= 短信 PDU 解码（3GPP TS 23.040） ================= */

	var GSM7_ALPHABET = '@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞ\x1bÆæßÉ !"#¤%&\'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà';

	function gsm7Extension(c) {
		var table = {
			0x0A: '\u000C', 0x14: '^', 0x28: '{', 0x29: '}', 0x2F: '\\',
			0x3C: '[', 0x3D: '~', 0x3E: ']', 0x40: '|', 0x65: '€'
		};
		return table[c] !== undefined ? table[c] : null;
	}

	function unpackSeptets(data, count) {
		var out = [];
		var acc = 0, bits = 0;
		for (var i = 0; i < data.length && out.length < count; i++) {
			acc |= data[i] << bits;
			bits += 8;
			while (bits >= 7 && out.length < count) {
				out.push(acc & 0x7F);
				acc >>= 7;
				bits -= 7;
			}
		}
		if (bits > 0 && out.length < count) out.push(acc & 0x7F);
		return out;
	}

	function septetsToString(septets) {
		var sb = '';
		var i = 0;
		while (i < septets.length) {
			var c = septets[i];
			if (c === 0x1B && i + 1 < septets.length) {
				var ext = gsm7Extension(septets[i + 1]);
				if (ext !== null) { sb += ext; i += 2; continue; }
			}
			if (c < GSM7_ALPHABET.length) sb += GSM7_ALPHABET[c];
			else sb += '?';
			i += 1;
		}
		return sb;
	}

	/*
	 * UCS2 字节解码（PDU 用，入参为字节数组）。
	 * 注意：上方另有一个入参为 hex 字符串的 var decodeUcs2（USSD 用），
	 * var 赋值会遮蔽函数声明——若重名会让 PDU 正文整体乱码，故这里改名 _Bytes。
	 */
	function decodeUcs2Bytes(data) {
		var out = '';
		for (var i = 0; i + 1 < data.length; i += 2) {
			out += String.fromCharCode((data[i] << 8) | data[i + 1]);
		}
		return out;
	}

	function bcdDigit(b) { return (b & 0x0F) * 10 + (b >> 4); }

	/*
	 * 7 字节 BCD 时间戳（TP-SCTS / TP-VP 绝对格式）→ Date；字段非法返回 null。
	 *
	 * 同样不再回落 `new Date()`：时间戳坏了就是「不知道」，
	 * 编一个当前时刻会让排序与界面都在说谎。
	 */
	function decodeTimestamp(ts) {
		if (!ts || ts.length < 7) return null;
		var year = 2000 + bcdDigit(ts[0]);
		var month = bcdDigit(ts[1]);
		var day = bcdDigit(ts[2]);
		var hour = bcdDigit(ts[3]);
		var minute = bcdDigit(ts[4]);
		var second = bcdDigit(ts[5]);
		if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59 || second > 60) {
			return null;
		}
		return new Date(year, month - 1, day, hour, minute, second);
	}

	function decodeAddress(raw, offset) {
		var lenNibbles = raw[offset];
		var ton = (raw[offset + 1] >> 4) & 0x07;
		var start = offset + 2;
		var digits = '';
		var nibbles = lenNibbles;
		var i = start;
		while (nibbles > 0 && i < raw.length) {
			var b = raw[i];
			var d1 = b & 0x0F;
			var d2 = (b >> 4) & 0x0F;
			if (d1 < 0x0F) digits += String(d1);
			if (d2 < 0x0F) digits += String(d2);
			nibbles -= 2;
			i += 1;
		}
		if (ton === 5) {
			var alphaBytes = raw.slice(start, i);
			/* 半字节长度折算 septet 数：向下取整（多余填充位不构成字符） */
			var septets = unpackSeptets(alphaBytes, Math.floor(lenNibbles * 4 / 7));
			return { address: septetsToString(septets), ton: ton };
		}
		if (ton === 1) return { address: '+' + digits, ton: ton };
		return { address: digits, ton: ton };
	}

	/*
	 * TP-DCS 编码判定（3GPP TS 23.038）：
	 *   0x00-0x3F 一般数据编码组：bit3-2 选编码（00=7bit 01=8bit 10=UCS2）
	 *   0x40-0xBF 保留/自动丢弃组：按 bit3-2 判定
	 *   0xC0-0xDF 消息等待组：bit3=1 为 UCS2，否则 GSM7
	 *   0xF0-0xFF 数据编码/消息类别组：固定 GSM7（bit3-2 是类别不是编码！）
	 * 返回 0=GSM7 / 1=8bit / 2=UCS2。
	 * 注意：若用 dcs & 0x0C 判断，会把 F 组的类别位误读成编码位导致乱码。
	 */
	function dcsEncoding(dcs) {
		var group = dcs >> 4;
		if (group === 0xC || group === 0xD || group === 0xE) {
			return (dcs & 0x08) ? 2 : 0;
		}
		if (group === 0xF) {
			return 0;
		}
		return (dcs >> 2) & 0x03;
	}

	/*
	 * 解析一条短信 PDU（hex），返回
	 * { direction, number, sender, content, date, timeApprox, partial }；无法解析返回 null。
	 *
	 * 支持两种 PDU 类型（3GPP TS 23.040）：
	 *
	 *   TP-MTI = 0  SMS-DELIVER —— 对方发来的。地址字段是 TP-OA（发件号码），
	 *               随后 7 字节 TP-SCTS 是服务中心盖的**收信时刻**，时间可信。
	 *
	 *   TP-MTI = 1  SMS-SUBMIT  —— 本机发出的。模组若把已发短信存在存储里，
	 *               AT+CMGL 就会以这种类型列出来。地址字段是 TP-DA（收件号码），
	 *               前面多一个 TP-MR（1 字节）。SUBMIT **没有发送时刻字段**，
	 *               只有一个 TP-VP 有效期：VPF=01/11（7 字节）时能解出时间，
	 *               但语义是「有效期截止」而非「发送时刻」，故标记 timeApprox；
	 *               VPF=10（1 字节相对格式，本插件发信时用的就是它）拿不到绝对时间，
	 *               此时 date 返回 null —— 宁可显示「时间未知」，也不编一个时刻出来。
	 *
	 * 为什么必须支持 SUBMIT：旧实现开头就是 `if (mti !== 0) return null`，整条 SUBMIT
	 * 被丢掉；而当年的前端 CMGL 解析（api.parseCMGL，已随「列表一律以后端事务返回值
	 * 为准」一起删掉）又把凡是解出来的统统标成 'received'。
	 * 两者叠加会把「本机发出的短信」变成一条假的 incoming —— 号码是自己的收件人，
	 * 方向却是「收到」。这里由 PDU 的 MTI 决定方向，不再靠调用方猜。
	 *
	 * MTI = 2/3（SMS-STATUS-REPORT / SMS-COMMAND）本页不展示，返回 null 跳过。
	 */
	api.decodeIncomingPdu = function (hex) {
		var raw = [];
		for (var i = 0; i + 1 < hex.length; i += 2) {
			raw.push(parseInt(hex.slice(i, i + 2), 16));
		}
		if (raw.length < 2) return null;
		var smscLen = raw[0];
		var pos = 1 + smscLen;
		if (pos >= raw.length) return null;
		var firstOctet = raw[pos];
		var mti = firstOctet & 0x03;
		if (mti !== 0 && mti !== 1) return null;
		var isSubmit = mti === 1;
		pos += 1;

		var addr;
		if (isSubmit) {
			/* TP-MR 占 1 字节，跳过；随后才是与 DELIVER 同构的地址字段 */
			if (pos >= raw.length) return null;
			pos += 1;
			var daLen = raw[pos];
			if (pos + 1 + Math.ceil(daLen / 2) > raw.length) return null;
			addr = decodeAddress(raw, pos);
			pos += 2 + Math.ceil(daLen / 2);
		} else {
			var oaLen = raw[pos];
			if (pos + 1 + Math.ceil(oaLen / 2) > raw.length) return null;
			addr = decodeAddress(raw, pos);
			pos += 2 + Math.ceil(oaLen / 2);
		}
		if (pos + 1 >= raw.length) return null;
		pos += 1;  // 跳过 pid 字段
		var dcs = raw[pos]; pos += 1;

		/*
		 * TP-VP 长度由首字节的 bit4:bit3（TP-VPF）决定（TS 23.040）：
		 *   bit4 bit3
		 *     0    0   → 无 TP-VP
		 *     1    0   → 相对格式，1 字节（只有「还有多久过期」，无绝对时间）
		 *     0    1   → 增强格式，7 字节
		 *     1    1   → 绝对格式，7 字节
		 * (firstOctet >> 3) & 0x03 正好得到上表的行号 0/1/2/3，
		 * 故长度表为 [0, 7, 1, 7]（下标 1 = 增强、下标 2 = 相对）。
		 */
		var vpf = (firstOctet >> 3) & 0x03;
		var vpLen = [0, 7, 1, 7][vpf];
		var date = null;
		var timeApprox = false;
		if (isSubmit) {
			if (pos + vpLen > raw.length) return null;
			if (vpLen === 7) { date = decodeTimestamp(raw.slice(pos, pos + 7)); timeApprox = true; }
			pos += vpLen;
		} else {
			if (pos + 7 > raw.length) return null;
			date = decodeTimestamp(raw.slice(pos, pos + 7));
			pos += 7;
		}
		if (pos >= raw.length) return null;
		var udl = raw[pos]; pos += 1;
		var udLen;
		if ((dcs & 0x0C) === 0x04) udLen = udl;
		else if ((dcs & 0x0C) === 0x08) udLen = udl * 2;
		else udLen = Math.ceil(udl * 7 / 8);
		var ud = raw.slice(pos, Math.min(pos + udLen, raw.length));

		var content = '';
		var partial = null;
		var udhi = (firstOctet & 0x40) !== 0;
		var userData = ud;
		var headerLen = 0;

		if (udhi && ud.length > 0) {
			headerLen = ud[0];
			var hdr = ud.slice(1, 1 + headerLen);
			userData = ud.slice(1 + headerLen);
			var p = 0;
			while (p + 1 < hdr.length) {
				var iei = hdr[p];
				var iel = hdr[p + 1];
				if (p + 2 + iel > hdr.length) break;
				var ieData = hdr.slice(p + 2, p + 2 + iel);
				if (iei === 0x00 && iel >= 3) {
					partial = { reference: ieData[0], parts_count: ieData[1], part_number: ieData[2] };
				} else if (iei === 0x08 && iel >= 4) {
					partial = { reference: ((ieData[0] << 8) | ieData[1]), parts_count: ieData[2], part_number: ieData[3] };
				}
				p += 2 + iel;
			}
		}

		var enc = dcsEncoding(dcs);
		if (enc === 2) {
			content = decodeUcs2Bytes(userData);
		} else if (enc === 1) {
			var c8 = '';
			for (var k = 0; k < userData.length; k++) c8 += String.fromCharCode(userData[k]);
			content = c8;
		} else {
			/*
			 * 7-bit：UDH 占用的 septet 数按其八位组长度折算，
			 * 必须先解「完整 UD 字节流」的 septet 序列、再跳过头部码位；
			 * 注意：若先按字节切掉 UDH 再解包，位流会错位，导致长短信正文乱码。
			 */
			var udhSeptets = (udhi && ud.length > 0) ? Math.ceil((ud[0] + 1) * 8 / 7) : 0;
			var totalSeptets = Math.max(udl, udhSeptets);
			var septets = unpackSeptets(ud, totalSeptets);
			content = udhSeptets <= septets.length
				? septetsToString(septets.slice(udhSeptets))
				: '';
		}

		return {
			direction: isSubmit ? 'sent' : 'received',
			/* SUBMIT 的地址是收件人、DELIVER 的是发件人，统一用 number 表示「对方号码」 */
			number: addr.address,
			sender: addr.address,
			content: content,
			/* Date 或 null（时间未知） */
			date: date,
			timeApprox: timeApprox,
			partial: partial
		};
	};

	/*
	 * 【已废弃的数据源】localStorage sms_sent_messages_cache
	 *
	 * 旧版本把已发短信存在浏览器本地、并在渲染短信列表时合并进来。它带来两个
	 * 实打实的问题：
	 *   1) 本地条目没有模组序号（用负数代替），删除时按着不存在的 index 下发
	 *      AT+CMGD，模组一律回 ERROR；
	 *   2) 模组里已经删掉的短信仍会显示在界面上，用旧数据掩盖了模组的真实状态。
	 *
	 * 现在短信列表一律以模组实时返回为准，本文件只保留「清除历史残留」所需的读取、
	 * 计数、删除三个函数，任何页面都不应再把本地缓存当数据源。
	 */
	api.SMS_CACHE_KEY = 'sms_sent_messages_cache';

	api.getCachedSentMessages = function () {
		try {
			var raw = localStorage.getItem(api.SMS_CACHE_KEY);
			if (!raw) return [];
			var parsed = JSON.parse(raw);
			if (Array.isArray(parsed)) return parsed;
		} catch (e) { /* ignore */ }
		return [];
	};

	api.clearSentMessageCache = function () {
		try { localStorage.removeItem(api.SMS_CACHE_KEY); } catch (e) { /* ignore */ }
	};

	/* 历史遗留条数（仅供「清除残留」显示，不作为数据源） */
	api.countLegacySentMessages = function () {
		try {
			return api.getCachedSentMessages().length;
		} catch (e) {
			return 0;
		}
	};

	/* ================= 锁频 ================= */

	api.LOCK_TYPES = [
		{ label: '关闭', value: 0 }, { label: '锁定频点', value: 1 },
		{ label: '锁定小区', value: 2 }, { label: '锁定 Band', value: 3 }
	];
	api.LTE_BANDS = [
		{ label: 'B1 2100MHz', value: 1 }, { label: 'B3 1800MHz', value: 3 }, { label: 'B5 850MHz', value: 5 },
		{ label: 'B8 900MHz', value: 8 }, { label: 'B34 2100MHz TDD', value: 34 }, { label: 'B38 2600MHz TDD', value: 38 },
		{ label: 'B39 1900MHz TDD', value: 39 }, { label: 'B40 2300MHz TDD', value: 40 }, { label: 'B41 2500MHz TDD', value: 41 }
	];
	api.NR_BANDS = [
		{ label: 'n1 2100MHz', value: 1 }, { label: 'n3 1800MHz', value: 3 }, { label: 'n5 850MHz', value: 5 },
		{ label: 'n8 900MHz', value: 8 }, { label: 'n28 700MHz', value: 28 }, { label: 'n41 2500MHz', value: 41 },
		{ label: 'n77 3700MHz', value: 77 }, { label: 'n78 3500MHz', value: 78 }, { label: 'n79 4700MHz', value: 79 }
	];
	api.SCS_TYPES = [
		{ label: '15 kHz', value: 0 }, { label: '30 kHz', value: 1 }, { label: '60 kHz', value: 2 },
		{ label: '120 kHz', value: 3 }, { label: '240 kHz', value: 4 }
	];
	api.MAX_LOCK_ITEMS = 20;
	api.MAX_ARFCN = 4294967295;
	/* 手册 13.12.3 / 13.13.3：<band> 为逗号分隔字符串（0~65535，页面只从固定频段表
	 * 取值，故不再单设上限）；<pci> LTE 0~503、NR 0~1007，<num> 1~20。 */
	api.MAX_PCI = { lte: 503, nr: 1007 };

	api.getDefaultScsType = function (band) {
		// mmWave: 257/258/260/261 -> 3 (120kHz)，TDD 常用 -> 1 (30kHz)，其余 0
		if ([257, 258, 260, 261].indexOf(Number(band)) >= 0) return 3;
		if ([41, 48, 77, 78, 79].indexOf(Number(band)) >= 0) return 1;
		return 0;
	};

	/* ARFCN → 频段号反推（邻区展示与一键锁频共用，单一来源）。
	 * 区间依据 3GPP TS 38.104 Table 5.4.2.3-1 / TS 36.101。
	 * n77 是 620000–680000（3.3–4.2GHz），n78 是 620000–653333（3.3–3.8GHz），
	 * 两者在 620000–653333 上重叠，单凭频点不可区分。按国内现网部署约定，
	 * 重叠区统一判为 n78，故把 78 排在 77 之前（旧版 network_settings 的表把
	 * 77 排在前面，78 分支实际不可达）；653334–680000 为 n77 独有，仍判 n77。 */
	var LTE_ARFCN_BANDS = [
		[1, 0, 599], [3, 1200, 1949], [5, 2400, 2649], [8, 3450, 3799],
		[34, 36200, 36349], [38, 37750, 38249], [39, 38250, 38649], [40, 38650, 39649], [41, 39650, 41589]
	];
	var NR_ARFCN_BANDS = [
		[1, 422000, 434000], [3, 361000, 376000], [5, 173800, 178800], [8, 185000, 192000],
		[28, 151600, 160600], [41, 499200, 537999],
		[78, 620000, 653333], [77, 620000, 680000], [79, 693334, 733333]
	];
	api.bandFromArfcn = function (kind, arfcn) {
		if (arfcn == null || arfcn === '') return '';
		var ch = Number(arfcn);
		if (isNaN(ch)) return '';
		var map = kind === 'LTE' ? LTE_ARFCN_BANDS : NR_ARFCN_BANDS;
		for (var i = 0; i < map.length; i++) {
			if (ch >= map[i][1] && ch <= map[i][2]) return String(map[i][0]);
		}
		return '';
	};

	var checkArfcn = function (label, raw) {
		if (!/^\d+$/.test(raw)) throw new Error(label + ' 频点必须为 0-' + api.MAX_ARFCN + ' 的整数');
		if (Number(raw) > api.MAX_ARFCN) throw new Error(label + ' 频点超出范围（0-' + api.MAX_ARFCN + '）');
		return raw;
	};
	var checkPci = function (kind, raw) {
		var max = api.MAX_PCI[kind];
		var label = kind === 'lte' ? 'LTE' : 'NR';
		if (!/^\d+$/.test(raw)) throw new Error(label + ' PCI 必须为 0-' + max + ' 的整数');
		if (Number(raw) > max) throw new Error(label + ' PCI 超出范围（0-' + max + '）');
		return raw;
	};

		/*
		 * 取值转字符串：**只有 null / undefined 才算空**。
		 *
		 * 原来写的是 `String(i.arfcn || '')`，`0` 是合法 EARFCN/PCI（LTE band 1 的
		 * 最低频点就是 0，PCI 0 也是真实小区），却被 `||` 判成空值，导致用户
		 * 明明填了 0 却提示「频点必须为 0-4294967295 的整数」，保存不了。
		 * Codex 在 PR #29 上标了 P2，这里按「空值只认 null」修掉。
		 */
		var cellText = function (v) { return v == null ? '' : String(v).trim(); };

		api.toLockLists = function (kind, type, items) {
			var empty = { type: type, bands: '', arfcns: '', scs_types: '', pcis: '' };
			if (type === 0) return empty;
			var label = kind === 'lte' ? 'LTE' : 'NR';
			var valid = items.filter(function (i) { return i.band != null; });
			if (!valid.length) throw new Error('请至少输入一个有效的 ' + label + ' 频段');
			if (valid.length > api.MAX_LOCK_ITEMS) throw new Error(label + ' 最多只能锁 ' + api.MAX_LOCK_ITEMS + ' 组');
			var bands = valid.map(function (i) { return String(i.band); }).join(',');
			if (type === 3) return { type: type, bands: bands, arfcns: '', scs_types: '', pcis: '' };
			var arfcns = valid.map(function (i) { return checkArfcn(label, cellText(i.arfcn)); }).join(',');
			var scs_types = kind === 'nr' ? valid.map(function (i) { return String(i.scs != null ? i.scs : api.getDefaultScsType(i.band)); }).join(',') : '';
			if (type === 1) return { type: type, bands: bands, arfcns: arfcns, scs_types: scs_types, pcis: '' };
			var pcis = valid.map(function (i) { return checkPci(kind, cellText(i.pci)); }).join(',');
			return { type: type, bands: bands, arfcns: arfcns, scs_types: scs_types, pcis: pcis };
		};

	// 拼接锁频命令（即时生效场景，如扫频结果一键锁定）
	/*
	 * 手册 13.12.1 / 13.13.1：`AT^xxFREQLOCK=<operatetype>[,<forbidFlag>,<num>[,...]]`
	 * forbidFlag 只认 0（不允许重选和切换）/ 1（允许重选和切换）；
	 * 锁频点(1)与锁小区(2)与锁band(3) 携带的参数组不同，num 必须等于组数。
	 */
	api.buildLockCommand = function (kind, type, mobility, items) {
		var cmd = kind === 'lte' ? 'AT^LTEFREQLOCK' : 'AT^NRFREQLOCK';
		if (type === 0) return cmd + '=0';
		if (mobility !== 0 && mobility !== 1) throw new Error('移动性标志只能取 0（不允许重选和切换）或 1（允许）');
		var l = api.toLockLists(kind, type, items);
		var num = l.bands.split(',').length;
		var head = cmd + '=' + type + ',' + mobility + ',' + num;
		if (type === 3) return head + ',"' + l.bands + '"';
		if (kind === 'lte') {
			return type === 1
				? head + ',"' + l.bands + '","' + l.arfcns + '"'
				: head + ',"' + l.bands + '","' + l.arfcns + '","' + l.pcis + '"';
		}
		return type === 1
			? head + ',"' + l.bands + '","' + l.arfcns + '","' + l.scs_types + '"'
			: head + ',"' + l.bands + '","' + l.arfcns + '","' + l.scs_types + '","' + l.pcis + '"';
	};

	api.fromLockLists = function (kind, lists) {
		var split = function (v) { return (v || '').split(',').map(function (s) { return s.trim(); }).filter(Boolean); };
		var bands = split(lists.bands);
		if (!bands.length) return [{}];
		var arfcns = split(lists.arfcns);
		var scs = split(lists.scs_types);
		var pcis = split(lists.pcis);
		return bands.map(function (band, i) {
			return {
				band: Number(band), arfcn: arfcns[i], pci: pcis[i],
				scs: kind === 'nr' ? (scs[i] != null ? Number(scs[i]) : api.getDefaultScsType(Number(band))) : undefined
			};
		});
	};

	/* ================= 定时锁频 DTO ================= */

	// AT+SCHED? -> "+SCHED: {json}\r\nOK"
	api.parseScheduleResponse = function (data) {
		var json = String(data).match(/\+SCHED:\s*(\{[\s\S]*\})/);
		if (!json) return null;
		try { return JSON.parse(json[1]); } catch (e) { return null; }
	};

	api.buildScheduleSetCommand = function (cfg) {
		var payload = {};
		Object.keys(cfg).forEach(function (k) { if (k !== 'status') payload[k] = cfg[k]; });
		return 'AT+SCHED=' + JSON.stringify(payload);
	};

	/*
	 * `^NDISSTATQRY?` 回包：`^NDISSTATQRY: <cid>,<stat>,…`
	 *
	 * 手册 16.4.1/16.4.5：首字段是 CID、第二字段才是状态（1=已连接）。
	 * 按首字段判「已激活」的话，只要模组带 CID 1 这一行就永远认为是激活态。
	 *
	 * 拨号页（判断是否已拨上）与扫频页（扫频会断网，需二次确认）原先各写了一份
	 * 同一条正则，这里统一收口，避免两边对「第几个字段才是状态」再产生分歧。
	 * 返回第二字段的字符串；无回包或格式不符时返回 null（调用方按「未激活」处理）。
	 */
	api.parseNdisStat = function (text) {
		var m = String(text == null ? '' : text).replace(/\r/g, '')
			.match(/\^NDISSTATQRY:\s*1\s*,\s*(\d+)/i);
		return m ? m[1] : null;
	};

	/* ================= 辅载波/辅站小区 ================= */
	// 手册 13.27 AT^MONSSC — NSA 下 5G 辅连接服务小区（最多 8CC）
	// 手册 13.18 AT^CASCELLINFO? — LTE CA 的辅小区（最多 4 个 SCELL）
	// ^HFREQINFO 只给频点与带宽，这两条补每个辅载波各自的信号质量。

	var MEAS_TYPES = { 0: 'SSB', 1: 'CSI-RS' };
	var NR_INVALID = { rsrp: -1256, rsrq: -348, sinr: -188 };
	// 手册 13.18.3 <ulbw>/<dlbw>
	var LTE_BANDWIDTHS = { 0: 1.4, 1: 3, 2: 5, 3: 10, 4: 15, 5: 20 };

	var descale = function (value, min, max) {
		return value < min || value > max ? Number((value / 8).toFixed(1)) : value;
	};
	var numOrNull = function (v) {
		var t = String(v == null ? '' : v).trim();
		if (t === '') return null;
		var n = Number(t);
		return isFinite(n) ? n : null;
	};

	// 解析一行 ^MONSSC。NONE（非 ENDC）或解析失败返回 null。
	api.parseMonssc = function (line) {
		var match = line.match(/\^MONSSC:\s*(.+)/);
		if (!match) return null;
		var f = match[1].split(',').map(function (s) { return s.trim(); });
		var rat = String(f[0] || '').replace(/"/g, '').toUpperCase();
		if (rat !== 'NR' || f.length < 3) return null; // NONE 表示非 ENDC，LTE 分支手册标注暂不支持
		var arfcn = numOrNull(f[1]);
		// 手册 13.27.3：<PCI> 十六进制，取值范围 0~0x3EF
		var pci = f[2] === '' ? null : parseInt(f[2], 16);
		if (arfcn === null || pci === null || isNaN(pci)) return null;
		var rawRsrp = numOrNull(f[3]), rawRsrq = numOrNull(f[4]), rawSinr = numOrNull(f[5]);
		var meas = numOrNull(f[6]);
		return {
			arfcn: arfcn,
			pci: pci,
			rsrp: rawRsrp === null || rawRsrp === NR_INVALID.rsrp ? null : descale(rawRsrp, -156, -31),
			rsrq: rawRsrq === null || rawRsrq === NR_INVALID.rsrq ? null : descale(rawRsrq, -43, 20),
			sinr: rawSinr === null || rawSinr === NR_INVALID.sinr ? null : descale(rawSinr, -23, 40),
			measType: meas !== null && MEAS_TYPES[meas] ? MEAS_TYPES[meas] : '—'
		};
	};

	api.parseMonsscAll = function (text) {
		return String(text).split(/\r?\n/).map(api.parseMonssc).filter(Boolean);
	};

	// 解析一行 ^CASCELLINFO。CA 未配置时模组直接回 ERROR，这里自然解析不到。
	api.parseCascell = function (line) {
		var match = line.match(/\^CASCELLINFO:\s*(.+)/);
		if (!match) return null;
		var f = match[1].split(',').map(function (s) { return s.trim(); });
		if (f.length < 12) return null;
		var index = numOrNull(f[0]), pci = numOrNull(f[1]);
		if (index === null || pci === null) return null;
		var bw = function (v) {
			var n = numOrNull(v);
			return n === null ? null : (LTE_BANDWIDTHS[n] != null ? LTE_BANDWIDTHS[n] : null);
		};
		var freq = function (v) {
			var n = numOrNull(v);
			// 手册：<ulfreq>/<dlfreq> 单位 100kHz，换成 MHz 显示
			return n === null ? null : Number((n / 10).toFixed(1));
		};
		return {
			index: index,
			pci: pci,
			rssi: numOrNull(f[2]),
			rsrp: numOrNull(f[3]),
			rsrq: numOrNull(f[4]),
			band: numOrNull(f[5]) || 0,
			ulArfcn: numOrNull(f[6]),
			dlArfcn: numOrNull(f[7]),
			ulFreq: freq(f[8]),
			dlFreq: freq(f[9]),
			ulBandwidth: bw(f[10]),
			dlBandwidth: bw(f[11])
		};
	};

	api.parseCascellAll = function (text) {
		return String(text).split(/\r?\n/).map(api.parseCascell).filter(Boolean);
	};

	/*
	 * 频点归一化：^HFREQINFO 的 dl_fcn 与 ^MONSSC 的 arfcn 都是字符串，
	 * 空串会被 Number() 变成 0，进而把 arfcn=0 的辅小区误判成「已匹配」。
	 * 这里统一把空值/非法值折成 null，让调用方显式处理。
	 */
	function normalizeArfcn(value) {
		if (value === undefined || value === null) return null;
		var txt = String(value).trim();
		if (txt === '') return null;
		var n = Number(txt);
		return isFinite(n) ? n : null;
	}

	// 把 ^MONSSC / ^CASCELLINFO 的信号质量对应到 ^HFREQINFO 报出来的某个载波上。
	api.carrierSignalFor = function (carrier, nr, lte) {
		if (!carrier) return null;
		var arfcn = normalizeArfcn(carrier.dlFcn);
		if (arfcn === null) return null;
		var nrList = nr || [];
		var lteList = lte || [];
		if (carrier.sysMode === 'NR') {
			var hit = nrList.find(function (c) { return normalizeArfcn(c && c.arfcn) === arfcn; });
			return hit
				? { pci: hit.pci, rsrp: hit.rsrp, rsrq: hit.rsrq, sinr: hit.sinr, measType: hit.measType }
				: null;
		}
		var lHit = lteList.find(function (c) { return normalizeArfcn(c && c.dlArfcn) === arfcn; });
		return lHit ? { pci: lHit.pci, rsrp: lHit.rsrp, rsrq: lHit.rsrq, sinr: null, rssi: lHit.rssi } : null;
	};

	// 找出没能对应到任何载波的辅小区，合并后不把数据悄悄丢掉。
	api.unmatchedSecondaries = function (carriers, nr, lte) {
		var arfcns = {};
		(carriers || []).forEach(function (c) {
			var a = normalizeArfcn(c && c.dlFcn);
			if (a !== null) arfcns[a] = true;
		});
		var inList = function (a) { return a !== null && arfcns[a] === true; };
		return {
			nr: (nr || []).filter(function (c) {
				var a = normalizeArfcn(c && c.arfcn);
				return a === null || !inList(a);
			}),
			lte: (lte || []).filter(function (c) {
				var a = normalizeArfcn(c && c.dlArfcn);
				return a === null || !inList(a);
			})
		};
	};

	/* ================= 网络拒绝原因 ^REJINFO ================= */
	// 手册 13.14：注册或业务请求或网络 DETACH 过程被网络拒绝时主动上报。
	// 锁频锁错小区导致掉网时，这条上报能直接区分"被网络拒绝"和"根本没覆盖"。

	var REJ_DOMAINS = { 0: 'CS 域', 1: 'PS 域', 2: 'CS+PS 域' };
	var REJ_RATS = {
		0: 'GERAN(2G)', 1: 'UTRAN(3G)', 2: 'E-UTRAN(4G)',
		5: 'NR-5GC(5G SA)', 6: '其他'
	};
	var REJ_TYPES = {
		0: 'LAU 被拒', 1: '鉴权失败', 2: '业务请求被拒', 3: '网络 detach 被拒',
		4: 'ATTACH 被拒', 5: 'RAU 被拒', 6: 'TAU 被拒'
	};
	// 原因值来自 3GPP TS 24.008 / 24.301 / 24.501，未收录的原样显示编号
	var REJ_CAUSES = {
		2: 'IMSI 未在 HSS 登记', 3: '非法终端', 5: 'IMEI 不被接受', 6: '非法设备',
		7: '不允许使用分组域业务', 8: '不允许使用分组域和非分组域业务', 9: '网络无法识别终端身份',
		10: '已被网络隐式分离', 11: '不允许使用该 PLMN', 12: '不允许在该跟踪区注册',
		13: '该跟踪区不允许漫游', 14: '该 PLMN 不提供分组域业务', 15: '跟踪区内没有合适的小区',
		16: 'MSC 暂时不可达', 17: '网络故障', 18: 'CS 域不可用', 19: 'ESM 流程失败',
		20: 'MAC 校验失败', 21: '同步失败', 22: '网络拥塞', 23: '终端安全能力不匹配',
		24: '安全模式被拒绝', 25: '未授权接入该 CSG', 26: '非 EPS 鉴权不可接受',
		27: '不允许使用 N1 模式', 28: '受限的服务区域', 31: '需要重定向到 4G 核心网',
		35: '该 PLMN 未授权所请求的业务', 39: 'CS 业务暂时不可用', 40: '没有激活的 EPS 承载',
		42: '严重网络故障', 43: 'LADN 不可用', 62: '没有可用的网络切片',
		65: '已达到 PDU 会话数量上限', 67: '切片与 DNN 资源不足',
		71: '不允许通过非 3GPP 接入 5G 核心网', 72: '服务网络未授权',
		95: '消息语义错误', 96: '必选信元无效', 97: '消息类型不存在或未实现',
		98: '消息类型与协议状态不匹配', 99: '信元不存在或未实现',
		100: '条件信元错误', 101: '消息与协议状态不匹配', 111: '协议错误',
		256: '鉴权失败（模组内部扩展）', 258: '联合注册中 CS 失败（其他原因）',
		301: 'CS/PS 注册网络无响应', 302: 'CS/PS 注册建链异常', 303: 'CS/PS 注册建链异常'
	};
	// 手册 13.14.2：USIM 鉴权失败的原因值从 65537 开始，共 65537~65543
	var USIM_CAUSE_MIN = 65537, USIM_CAUSE_MAX = 65543;

	api.rejectCauseText = function (cause) {
		if (REJ_CAUSES[cause]) return REJ_CAUSES[cause];
		if (cause >= USIM_CAUSE_MIN && cause <= USIM_CAUSE_MAX) return 'USIM 鉴权失败（#' + cause + '）';
		return '未知原因（#' + cause + '）';
	};

	// 解析一行 ^REJINFO 上报，不匹配时返回 null。手册正文用了全角冒号，两种都收。
	api.parseRejInfo = function (line) {
		var match = line.match(/\^REJINFO[：:]\s*(.+)/);
		if (!match) return null;
		var f = match[1].split(',').map(function (v) { return v.trim(); });
		if (f.length < 6) return null;
		var unquote = function (v) { return String(v == null ? '' : v).trim().replace(/^"|"$/g, ''); };
		var num = function (v) {
			var n = Number(unquote(v));
			return isFinite(n) ? n : 0;
		};
		var domain = num(f[1]), cause = num(f[2]), rat = num(f[3]), rejectType = num(f[4]);
		return {
			plmn: unquote(f[0]),
			domain: domain,
			domainText: REJ_DOMAINS[domain] != null ? REJ_DOMAINS[domain] : '域 ' + domain,
			cause: cause,
			causeText: api.rejectCauseText(cause),
			rat: rat,
			ratText: REJ_RATS[rat] != null ? REJ_RATS[rat] : '制式 ' + rat,
			rejectType: rejectType,
			rejectTypeText: REJ_TYPES[rejectType] != null ? REJ_TYPES[rejectType] : '类型 ' + rejectType,
			originalCause: num(f[5]),
			lac: unquote(f[6] || ''),
			rac: unquote(f[7] || ''),
			cellId: unquote(f[8] || ''),
			// 手册：当 LNAS 注册被拒绝 #19 时才会带上这个值
			esmCause: f[9] !== undefined ? num(f[9]) : undefined,
			raw: line.trim(),
			at: Date.now()
		};
	};

	/* ================= SIM 信号质量 ^SIMSQ ================= */
	// 手册 6.6：^SIMSQ 能区分卡不在位 / 被锁 / PUK 锁死，+CPIN 看不出来。
	var SIM_STATUS = {
		0: '卡不在位', 1: '卡已插入', 2: '卡被 PIN/PUK 锁定', 3: 'SIMLOCK 锁定',
		10: '卡文件初始化中', 11: '卡初始化完成，可接入网络',
		12: '卡就绪，短信与电话本可用', 98: '卡已失效（PUK 锁死或物理损坏）',
		99: '卡已移除', 100: '卡初始化失败'
	};

	api.parseSimsq = function (text) {
		var match = String(text).match(/\^SIMSQ:\s*(\d+)\s*,\s*(\d+)/);
		if (!match) return null;
		var status = Number(match[2]);
		return {
			status: status,
			label: SIM_STATUS[status] != null ? SIM_STATUS[status] : '状态 ' + status,
			dead: status === 98,
			present: status !== 0 && status !== 99
		};
	};

	/* ================= 5GS QoS 动态参数 +C5GQOSRDP ================= */
	/*
	 * 3GPP TS 27.007 +C5GQOSRDP（只读动态参数，取一次即可，无需等待业务触发）：
	 *   +C5GQOSRDP: <cid>,<5QI>,[<GFBR_uplink>],[<GFBR_downlink>],
	 *               [<MFBR_uplink>],[<MFBR_downlink>],[<Session-AMBR 上行>],[<Session-AMBR 下行>]
	 * 所有比特率单位均为 **kbps**。
	 *
	 * 注意（实测本机 AT+C5GQOSRDP=1 → "1,6,,,,,102400,102400"）：
	 *   - 5QI=6 是非 GBR 类，故 GFBR/MFBR 四个字段按规范留空；
	 *   - 末尾两个非空值即核心网下发的 **Session-AMBR**，也就是这张卡的
	 *     **签约上下行速率**（102400 kbps = 100 Mbps）。
	 *   - 标准 3GPP 定义只有 7 个字段（末位是 averaging_window，毫秒级），
	 *     而本固件是 8 字段变体。为避免把 averaging_window 误当成速率，
	 *     只在字段数 ≥8 时才把第 7/8 位解析为签约速率，否则留空。
	 */
	api.parseC5gQosRdp = function (text) {
		var lines = String(text).split(/\r?\n/);
		for (var i = 0; i < lines.length; i++) {
			var m = lines[i].match(/\+?C5GQOSRDP:\s*([^\r\n]*)/i);
			if (!m) continue;
			var f = m[1].split(',');
			var num = function (v) {
				var n = parseInt(String(v == null ? '' : v).trim(), 10);
				return isNaN(n) ? null : n;
			};
			var hasAmbr = f.length >= 8;
			return {
				cid: num(f[0]),
				qci: num(f[1]),
				gfbrUl: num(f[2]),
				gfbrDl: num(f[3]),
				mfbrUl: num(f[4]),
				mfbrDl: num(f[5]),
				ambrUl: hasAmbr ? num(f[6]) : null,
				ambrDl: hasAmbr ? num(f[7]) : null,
				raw: lines[i].trim(),
				at: Date.now()
			};
		}
		return null;
	};

	/* ================= 签约速率 AT^DSAMBR（手册 16.17） ================= */
	/*
	 * 手册 16.17「AT^DSAMBR - 签约速率查询」：
	 *   AT^DSAMBR=<cid>
	 *   <CR><LF>^DSAMBR: <cid>,<DlApnAmbr>,<UlApnAmbr>[,...]<CR><LF>OK
	 *   <DlApnAmbr> 下行签约速率，单位 kbps
	 *   <UlApnAmbr> 上行签约速率，单位 kbps
	 *
	 * 实机返回比手册多两个字段（APN 与一个状态码）：
	 *   NR: ^DSAMBR: 1,<dl>,<ul>,"<APN>",<n>
	 *   LTE: ^DSAMBR: 1,<dl>,<ul>,"<APN>",<n>
	 * 因此只按**前三个位置**取值，后面的原样保留在 extra 里，不做臆测。
	 *
	 * 该命令 4G / 5G 下均可用，是签约速率的首选来源；取不到时才回退
	 * 到标准命令 AT+C5GQOSRDP 的 Session-AMBR（见 parseC5gQosRdp）。
	 */
	api.parseDsambr = function (text) {
		var lines = String(text).split(/\r?\n/);
		for (var i = 0; i < lines.length; i++) {
			var m = lines[i].match(/\^DSAMBR:\s*([0-9]+)\s*,\s*([0-9]*)\s*,\s*([0-9]*)/);
			if (!m) continue;
			var rest = lines[i].slice(lines[i].indexOf(m[0]) + m[0].length);
			var apnMatch = rest.match(/"([^"]*)"/);
			return {
				cid: parseInt(m[1], 10),
				dlAmbr: m[2] === '' ? null : parseInt(m[2], 10),
				ulAmbr: m[3] === '' ? null : parseInt(m[3], 10),
				apn: apnMatch ? apnMatch[1] : '',
				raw: lines[i].trim(),
				at: Date.now()
			};
		}
		return null;
	};

	/* ================= FOTA 状态机（手册 15.3~15.7 + FOTA 升级指南） ================= */

	/*
	 * 手册 15.5.3 的 <status> 定义：10 Idle / 11 查询 / 12 发现新版本 /
	 * 13 查询新版本失败 / 14 未发现版本 / 20 下载失败 / 30 正在下载 /
	 * 31 下载挂起 / 40 下载完成 / 50 准备升级。
	 * 70 只出现在 15.4.5「若升级失败」的示例里（参数表未列），按升级失败处理。
	 */
	var FOTA_STATE_META = {
		10: { color: '', text: '空闲（IDLE）' },
		11: { color: 'accent', text: '正在查询新版本' },
		12: { color: 'success', text: '发现新版本' },
		13: { color: 'danger', text: '查询新版本失败' },
		14: { color: '', text: '服务器无新版本' },
		20: { color: 'danger', text: '下载失败' },
		30: { color: 'warning', text: '下载中' },
		31: { color: 'warning', text: '下载挂起' },
		40: { color: 'success', text: '下载完成，待升级' },
		50: { color: 'danger', text: '准备升级' },
		70: { color: 'danger', text: '升级失败' }
	};

	api.fotaStateMeta = function (s) {
		return FOTA_STATE_META[s] || { color: '', text: (s == null ? '未知' : '状态 ' + s) };
	};

	/* 手册 15.3.3：30/31/40 可用 AT^FOTADL=0 取消并回到 IDLE */
	api.isFotaCancellable = function (s) { return s === 30 || s === 31 || s === 40; };

	/*
	 * 手册 15.5.1：查询态为 `^FOTASTATE: <status>`；
	 * <status> 为 13 / 20 时带 <error_code>；12 时（主动上报）带
	 * <version>,<packet_size>,<description>。
	 * 12 的 <version> 两种形态都要认：手册示例带引号（"MC…"），《FOTA 升级指南》
	 * 4.1.1 的实机示例**不带引号**（`12,V100R001C00B002,6129664,1.product_name=…`），
	 * 只认带引号会把指南机型的新版本号与包大小整段丢掉。
	 */
	api.parseFotaState = function (text) {
		var str = String(text || '').replace(/\r/g, '');
		var m = str.match(/\^FOTASTATE:\s*([0-9]+)\s*(?:,\s*([0-9]+))?/);
		if (!m) return null;
		var state = parseInt(m[1], 10);
		var out = { state: state, errorCode: null, version: '', packetSize: null };
		if (m[2] != null && (state === 13 || state === 20)) out.errorCode = parseInt(m[2], 10);
		if (state === 12) {
			var v = str.match(/\^FOTASTATE:\s*12\s*,\s*(?:"([^"]*)"|([^",]*))\s*(?:,\s*([0-9]+))?/);
			if (v) {
				out.version = (v[1] != null ? v[1] : v[2]).trim();
				out.packetSize = v[3] != null ? parseInt(v[3], 10) : null;
			}
		}
		return out;
	};

	/*
	 * `+CEREG: <n>,<stat>[,<tac>,<ci>,<AcT>]`（指南 4.1.1 用它确认已注册网络，
	 * §5【建议】注册成功后再进行 FOTA）。stat：1 本网注册 / 5 漫游注册，
	 * 其余（0 未注册 2 搜网中 3 拒绝 4 未知）都视为不可升级。
	 */
	api.parseCeregStat = function (text) {
		var m = String(text || '').replace(/\r/g, '').match(/\+CEREG:\s*([0-9]+)\s*,\s*([0-9]+)/);
		return m ? parseInt(m[2], 10) : null;
	};

	/*
	 * 手册 15.5.3 错误码表（只在 status=13/20 有效）。这里保留中文释义，
	 * 未收录的码值回退成「错误码 N」，不猜含义。
	 */
	var FOTA_ERRORS = {
		1: '未知错误', 2: '上一条命令未完成', 3: '命令参数错误', 4: '不支持该操作',
		5: '系统错误', 11: '网络尚未打开', 12: '网络已打开', 13: '打开网络失败',
		14: '链路尚未建立', 15: '链路已建立', 16: '建立链路失败', 17: '绑定端口失败',
		18: '连接指定地址失败', 19: '域名无效', 20: 'DNS 解析失败', 21: 'HTTP 服务器错误',
		22: '文件类型不正确', 23: '文件来源不正确', 51: '获取升级文件列表失败（检查 URL/端口/证书）',
		52: 'MD5 校验失败', 54: 'FOTA 处于冲突状态',
		101: 'TE 取消导致发送失败', 102: '重试次数超过 10 次', 103: '文件大小错误',
		104: '包序号错误', 105: '协议非 1K-Xmodem', 106: '端口无效',
		107: '文件或子文件 CRC 错误', 108: '升级类型错误', 109: '产品型号不匹配'
	};

	api.fotaErrorText = function (code) {
		if (code == null) return '';
		return FOTA_ERRORS[code] || ('错误码 ' + code);
	};

	/*
	 * 手册 15.6.1：`^FOTADLQ: <index>,"<file_type>",<dl_size>,<file_size>`，
	 * 多文件时逐行上报，index 1~10。
	 * 已下载字节数是 **第三个** 字段、文件总字节数是 **第四个** —— 顺序读反
	 * 会让进度恒为 100%（dl < file 时算出 >100 再被截断）。
	 * 文件类型 "FIRMWARE1" 自带数字，必须先按整行正则取数，
	 * 不能「剥掉非数字字符后按逗号切」。
	 */
	api.parseFotaDlq = function (text) {
		var lines = String(text || '').replace(/\r/g, '').split('\n');
		var files = [];
		for (var i = 0; i < lines.length; i++) {
			var m = lines[i].match(/\^FOTADLQ:\s*([0-9]+)\s*,\s*"?([^",]*)"?\s*,\s*([0-9]+)\s*,\s*([0-9]+)/);
			if (!m) continue;
			files.push({
				index: parseInt(m[1], 10),
				type: m[2].trim(),
				downloaded: parseInt(m[3], 10),
				total: parseInt(m[4], 10)
			});
		}
		if (!files.length) return null;
		var dl = 0, total = 0;
		files.forEach(function (f) { dl += f.downloaded; total += f.total; });
		return {
			files: files,
			downloaded: dl,
			total: total,
			percent: total > 0 ? Math.max(0, Math.min(100, Math.floor(dl / total * 100))) : null
		};
	};

	/*
	 * FOTA 地址校验（手册 15.7.3）：http 形式 http://<host>:<port>/<path>，
	 * 指向**路径**而非文件名且必须以 / 结尾，整串最长 255 字节；
	 * HTTPS 的证书方案在《FOTA 升级指南》3.2 标注「暂未支持」，因此只放 http://。
	 * 引号与 CR/LF 会把指令拼成另一条 AT，必须拒绝。
	 * 返回归一化后的 URL；不合法时返回 null，由调用方给出提示。
	 */
	api.normalizeFotaUrl = function (raw) {
		var url = String(raw || '').trim();
		if (!/^http:\/\/[^\s]+$/.test(url)) return null;
		if (url.indexOf('"') >= 0 || /[\r\n]/.test(url)) return null;
		if (url.charAt(url.length - 1) !== '/') url += '/';
		var bytes = 0;
		for (var i = 0; i < url.length; i++) {
			var c = url.charCodeAt(i);
			bytes += c <= 0x7f ? 1 : (c <= 0x7ff ? 2 : 3);
		}
		return bytes <= 255 ? url : null;
	};

	return api;
})();

/*
 * LuCI requires factory to return a Class subclass (instantiated once).
 * Also set window.Parse for page-side global.
 */
var ParseClass = L.Class.extend(Parse);
if (typeof window !== 'undefined') {
	window.Parse = new ParseClass();
}
return ParseClass;
