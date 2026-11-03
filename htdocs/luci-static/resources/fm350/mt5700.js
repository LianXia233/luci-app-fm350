'use strict';
'require baseclass';
'require fm350/compat';
'require fm350/theme';
'require fm350/tabmenu';
/* global L, AtTheme */
/**
 * MT5700 LuCI 前端公共组件库：页面骨架、卡片/面板、表单控件、
 * 信号仪表与网络能力总评（木桶原理取最差项），以及实时速率折线图。
 */

// 注入新样式
var MT5700_CSS_VERSION = '2.46.2';
// 全局视觉修订层：弹窗文字可见性 + 后端未就绪提示条。
// 这两项是**插件级组件**，不属于某个页面；本模块被 12 个视图全部 require，
// 因此在这里注入即可一次覆盖所有页面（含只依赖 mt5700 的服务配置页）。
// 独立版本号：以后只调这两块样式时不必连带刷新体积更大的 mt5700.css。
var GLOBAL_CSS_VERSION = '1.0.0';
// 先落主题明暗结论（html.at-dark / at-light），再注入样式，避免首屏明暗闪烁
if (window.AtTheme) window.AtTheme.apply();
if (window.AtTheme) window.AtTheme.injectCss('/luci-static/resources/fm350/mt5700.css?v=' + MT5700_CSS_VERSION);
// 放在 mt5700.css 之后加载，作为最终修订层（同特异性时后者胜出）
if (window.AtTheme) window.AtTheme.injectCss('/luci-static/resources/fm350/global.css?v=' + GLOBAL_CSS_VERSION);
// 全局弹窗样式（MT5700 骨架）：旧页由 fm350/api.js 注入同一文件，短信页在此补齐，
// 两类页面的弹窗长相完全一致（PC + 手机端响应式）。
var DIALOG_CSS_VERSION = '1.0.0';
if (window.AtTheme) window.AtTheme.injectCss('/luci-static/resources/fm350/css/dialog.css?v=' + DIALOG_CSS_VERSION);
else if (!document.getElementById('fm350-dialog-css')) {
	var __dl = document.createElement('link');
	__dl.id = 'fm350-dialog-css';
	__dl.rel = 'stylesheet';
	__dl.type = 'text/css';
	__dl.href = '/luci-static/resources/fm350/css/dialog.css?v=' + DIALOG_CSS_VERSION;
	document.head.appendChild(__dl);
}

/*
 * 插件全局背景：三角网格铺底（网格明暗由 CSS html.at-dark 切换），两个角色每次
 * 刷新随机显示其一、随机落位。PC 与手机各用一套角色图，显示哪张由 CSS 断点决定
 * （不依赖 UA）；角色立绘分 light / dark 两套变体，随主题明暗选择
 * （fm350/theme.js 判定），运行时切换主题自动换图。
 * 网格层 fixed + pointer-events:none，应用内容经 .mt5700-app{z-index:1} 抬到其上。
 */
(function () {
	function mountGlobalBg() {
		if (document.getElementById('mt5700-global-bg')) return;
		var layer = document.createElement('div');
		layer.id = 'mt5700-global-bg';

		var base = '/luci-static/resources/fm350/bg/';
		var variant = function () {
			return (window.AtTheme && typeof window.AtTheme.dark === 'function' && window.AtTheme.dark()) ? 'dark' : 'light';
		};
		var mk = function (cls, kind) {
			var img = document.createElement('img');
			img.className = 'mt-bg-char ' + cls;
			img.alt = '';
			img.setAttribute('data-kind', kind);
			img.src = base + 'character-' + kind + '-' + variant() + '.webp';
			layer.appendChild(img);
			return img;
		};
		var pc = mk('mt-bg-pc', 'pc');
		var mobile = mk('mt-bg-mobile', 'mobile');

		/* 主题运行时切换（Aurora 顶部按钮等）→ 换对应明暗变体立绘 */
		if (window.AtTheme && typeof window.AtTheme.onChange === 'function') {
			window.AtTheme.onChange(function () {
				var v = variant();
				if (pc) pc.src = base + 'character-pc-' + v + '.webp';
				if (mobile) mobile.src = base + 'character-mobile-' + v + '.webp';
			});
		}

		/* 随机落位：一律右侧，仅垂直偏移 / 旋转 / 透明度随机，水平只做少量抖动。
		 * 层级由 CSS 保证：#mt5700-global-bg(z:0) 恒在 #maincontent(z:1) 之下，
		 * 角色只出现在文字与数据的下层，绝不遮挡正文。 */
		var rand = function (a, b) { return a + Math.random() * (b - a); };
		var chars = layer.querySelectorAll('.mt-bg-char');
		for (var i = 0; i < chars.length; i++) {
			var el = chars[i];
			el.style.right = rand(0, 3).toFixed(1) + 'vw';
			el.style.bottom = rand(-2, 2).toFixed(1) + 'vh';
			el.style.transform = 'rotate(' + rand(-5, 5).toFixed(1) + 'deg)';
			el.style.opacity = rand(0.82, 0.96).toFixed(2);
		}

		(document.body || document.documentElement).appendChild(layer);
	}

	function startGlobalBg() {
		/* 壁纸总开关：UCI at-webserver.config.wallpaper_enabled（服务配置页「界面」卡片），
		 * 缺省视为开启；uci 读取失败（异常/超时）也按开启兜底，避免配置问题弄丢背景。 */
		var boot = function (on) { if (on !== false) mountGlobalBg(); };
		try {
			if (window.L && L.uci && L.resolveDefault) {
				L.resolveDefault(L.uci.load('at-webserver')).then(function () {
					boot((L.uci.get('at-webserver', 'config', 'wallpaper_enabled') || '1') !== '0');
				}).catch(function () { boot(true); });
			} else {
				boot(true);
			}
		} catch (e) {
			boot(true);
		}
	}

	if (document.readyState === 'loading') {
		document.addEventListener('DOMContentLoaded', startGlobalBg);
	} else {
		startGlobalBg();
	}
})();

var Mt5700 = (function () {
	var api = {};

	/* ================= 工具函数 ================= */

	// 创建 DOM 元素
	function E(tag, attrs, text) {
		var el = document.createElement(tag);
		if (attrs) {
			for (var k in attrs) {
				if (k === 'class' || k === 'className') {
					el.className = attrs[k];
				} else if (k === 'style') {
					el.style.cssText = attrs[k];
				} else if (k.startsWith('on')) {
					el.addEventListener(k.substring(2).toLowerCase(), attrs[k]);
				} else {
					el.setAttribute(k, attrs[k]);
				}
			}
		}
		// 文本内容：字符串走 textContent，DOM 节点/数组直接挂载
		if (text != null) {
			if (Array.isArray(text)) {
				for (var j = 0; j < text.length; j++) {
					if (text[j] != null) {
						if (typeof text[j] === 'object' && text[j].nodeType) {
							el.appendChild(text[j]);
						} else {
							el.appendChild(document.createTextNode(String(text[j])));
						}
					}
				}
			} else if (typeof text === 'object' && text.nodeType) {
				el.appendChild(text);
			} else {
				el.textContent = String(text);
			}
		}
		return el;
	}

	// SVG 创建
	function svgEl(tag, attrs) {
		var el = document.createElementNS('http://www.w3.org/2000/svg', tag);
		if (attrs) {
			for (var k in attrs) {
				el.setAttribute(k, attrs[k]);
			}
		}
		return el;
	}

	/*
	 * QAM 星座图 (v2.41 重绘)
	 * 与空间流图共用 320×190 视窗，保证两图同比例、渲染后高度一致、样式对齐。
	 * 状态表达：按模组实际上报的调制方式绘制对应阶数的星座点阵
	 *   QPSK 2×2=4 点 / 16QAM 4×4=16 点 / 64QAM 6×6=36 点 / 256QAM 16×16=256 点，
	 * 点阵越密代表单符号承载比特越多；每种调制一个主题色，角标实时显示当前调制。
	 */
	api.constellationSvg = function (modType) {
		var W = 320, H = 190, CX = 160, CY = 100;
		var svg = svgEl('svg', {
			'class': 'mt-constellation-svg',
			viewBox: '0 0 ' + W + ' ' + H,
			fill: 'none',
			xmlns: 'http://www.w3.org/2000/svg'
		});

		var mod = String(modType || '').toUpperCase();

		/* 按调制阶数选点阵、点径与主题色 (点间距随阶数收敛，模拟真实判决域收缩) */
		var spec = { color: '#a78bfa', levels: [-48, -16, 16, 48], r: '2.4', glow: true };   // 16QAM
		if (mod.indexOf('QPSK') >= 0) {
			spec = { color: '#38bdf8', levels: [-36, 36], r: '3.4', glow: true };
		} else if (mod.indexOf('256QAM') >= 0) {
			/* 256QAM 必须画 16×16 = 256 个映射点，角标与图必须一致 */
			var lv256 = [];
			for (var k = 0; k < 16; k++) lv256.push(-60 + k * 8);
			spec = { color: '#34d399', levels: lv256, r: '1.3', glow: false };
		} else if (mod.indexOf('64QAM') >= 0) {
			spec = { color: '#f472b6', levels: [-60, -36, -12, 12, 36, 60], r: '2', glow: true };
		}

		var defs = svgEl('defs');
		defs.innerHTML =
			'<radialGradient id="mtqam-bg" cx="50%" cy="42%" r="82%">' +
			'<stop offset="0%" stop-color="#1d3050"/>' +
			'<stop offset="55%" stop-color="#131c30"/>' +
			'<stop offset="100%" stop-color="#0b1120"/>' +
			'</radialGradient>' +
			'<radialGradient id="mtqam-glow" cx="50%" cy="50%" r="50%">' +
			'<stop offset="0%" stop-color="' + spec.color + '" stop-opacity="0.5"/>' +
			'<stop offset="100%" stop-color="' + spec.color + '" stop-opacity="0"/>' +
			'</radialGradient>' +
			'<linearGradient id="mtqam-badge" x1="0" y1="0" x2="1" y2="1">' +
			'<stop offset="0%" stop-color="#8b5cf6"/><stop offset="100%" stop-color="#38bdf8"/>' +
			'</linearGradient>';
		svg.appendChild(defs);

		// 深空面板底
		svg.appendChild(svgEl('rect', { x: '2', y: '2', width: String(W - 4), height: String(H - 4), rx: '14', fill: 'url(#mtqam-bg)', stroke: 'rgba(167, 139, 250, 0.28)', 'stroke-width': '1' }));

		// 参考同心环 (SNR 越高可用判决域越大)
		[22, 44, 66].forEach(function (r) {
			svg.appendChild(svgEl('circle', { cx: CX, cy: CY, r: r, stroke: 'rgba(148, 163, 184, 0.15)', 'stroke-width': '1', 'stroke-dasharray': '3 5' }));
		});

		// I/Q 坐标轴与箭头
		svg.appendChild(svgEl('line', { x1: '20', y1: CY, x2: W - 18, y2: CY, stroke: 'rgba(255,255,255,0.22)', 'stroke-width': '1' }));
		svg.appendChild(svgEl('line', { x1: CX, y1: '18', x2: CX, y2: H - 14, stroke: 'rgba(255,255,255,0.22)', 'stroke-width': '1' }));
		svg.appendChild(svgEl('path', { d: 'M ' + (W - 25) + ' ' + (CY - 3.5) + ' L ' + (W - 18) + ' ' + CY + ' L ' + (W - 25) + ' ' + (CY + 3.5), stroke: 'rgba(255,255,255,0.4)', 'stroke-width': '1', fill: 'none' }));
		svg.appendChild(svgEl('path', { d: 'M ' + (CX - 3.5) + ' 25 L ' + CX + ' 18 L ' + (CX + 3.5) + ' 25', stroke: 'rgba(255,255,255,0.4)', 'stroke-width': '1', fill: 'none' }));
		var tI = svgEl('text', { x: W - 28, y: CY - 7, fill: '#94a3b8', 'font-size': '9.5', 'font-style': 'italic', 'font-family': 'sans-serif' });
		tI.textContent = 'I';
		svg.appendChild(tI);
		var tQ = svgEl('text', { x: CX + 7, y: '27', fill: '#94a3b8', 'font-size': '9.5', 'font-style': 'italic', 'font-family': 'sans-serif' });
		tQ.textContent = 'Q';
		svg.appendChild(tQ);

		// 星座点: 光晕 + 实点 (256QAM 点太多不再逐点光晕，避免 500+ 节点卡顿)
		var seq = 0;
		spec.levels.forEach(function (lvx) {
			spec.levels.forEach(function (lvy) {
				var px = CX + lvx, py = CY + lvy;
				if (spec.glow) {
					svg.appendChild(svgEl('circle', { cx: px, cy: py, r: '7', fill: 'url(#mtqam-glow)' }));
				}
				var dot = svgEl('circle', { cx: px, cy: py, r: spec.r, fill: spec.color, 'class': 'mt-anim-pulse' });
				if (spec.glow) dot.style.animationDelay = ((seq % 9) * 0.24) + 's';
				seq++;
				svg.appendChild(dot);
			});
		});

		// 左上角调制角标 (渐变胶囊，显示当前真实调制)
		var label = mod || '16QAM';
		var bw = 16 + label.length * 6.8;
		svg.appendChild(svgEl('rect', { x: '12', y: '10', width: bw, height: '20', rx: '10', fill: 'url(#mtqam-badge)', opacity: '0.95' }));
		var badge = svgEl('text', { x: 12 + bw / 2, y: '24', 'text-anchor': 'middle', fill: '#ffffff', 'font-size': '10', 'font-weight': 'bold', 'font-family': 'sans-serif' });
		badge.textContent = label;
		svg.appendChild(badge);

		return svg;
	};

	/*
	 * MIMO 空间流图 (v2.41 重绘)
	 * 与星座图共用 320×190 视窗、同款深空面板，保证样式对齐。
	 * 状态表达：按下行码字数点亮对应数量的空间流 (基站 TX → 模组 RX)，
	 * 流上叠加流动光点表示数据传输；未参与的天线位灰显，真实反映当前 MIMO 阶数。
	 */
	api.mimoSvg = function (rank) {
		var W = 320, H = 190;
		var svg = svgEl('svg', {
			'class': 'mt-mimo-svg',
			viewBox: '0 0 ' + W + ' ' + H,
			fill: 'none',
			xmlns: 'http://www.w3.org/2000/svg'
		});

		var numRank = parseInt(rank, 10) || 4;
		if (numRank < 1) numRank = 1;
		if (numRank > 4) numRank = 4;
		var streamColors = ['#8b5cf6', '#38bdf8', '#f472b6', '#34d399'];

		var defs = svgEl('defs');
		defs.innerHTML =
			'<radialGradient id="mtmimo-bg" cx="50%" cy="42%" r="82%">' +
			'<stop offset="0%" stop-color="#2a2444"/>' +
			'<stop offset="55%" stop-color="#171326"/>' +
			'<stop offset="100%" stop-color="#0d0a18"/>' +
			'</radialGradient>' +
			'<linearGradient id="mtmimo-badge" x1="0" y1="0" x2="1" y2="1">' +
			'<stop offset="0%" stop-color="#8b5cf6"/><stop offset="100%" stop-color="#f472b6"/>' +
			'</linearGradient>';
		svg.appendChild(defs);

		// 与星座图同款深空面板底
		svg.appendChild(svgEl('rect', { x: '2', y: '2', width: String(W - 4), height: String(H - 4), rx: '14', fill: 'url(#mtmimo-bg)', stroke: 'rgba(167, 139, 250, 0.28)', 'stroke-width': '1' }));

		// 列标题: 基站发射 / 模组接收
		var ht = svgEl('text', { x: '46', y: '30', 'text-anchor': 'middle', fill: '#94a3b8', 'font-size': '9', 'font-family': 'sans-serif', 'letter-spacing': '1' });
		ht.textContent = '基站 TX';
		svg.appendChild(ht);
		var hr = svgEl('text', { x: '274', y: '30', 'text-anchor': 'middle', fill: '#94a3b8', 'font-size': '9', 'font-family': 'sans-serif', 'letter-spacing': '1' });
		hr.textContent = '模组 RX';
		svg.appendChild(hr);

		// 4 个天线位，未激活的灰显 (真实反映参与复用的流数)
		var antY = [56, 88, 120, 152];
		for (var i = 0; i < 4; i++) {
			var active = i < numRank;
			var dotFill = active ? streamColors[i] : 'rgba(148, 163, 184, 0.30)';
			var lineStroke = active ? streamColors[i] : 'rgba(148, 163, 184, 0.30)';
			var txtFill = active ? '#cbd5e1' : '#475569';

			svg.appendChild(svgEl('line', { x1: '38', y1: antY[i], x2: '56', y2: antY[i], stroke: lineStroke, 'stroke-width': '2', 'stroke-linecap': 'round' }));
			var tc = svgEl('circle', { cx: '44', cy: antY[i], r: active ? '4.5' : '3.2', fill: dotFill });
			if (active) tc.setAttribute('class', 'mt-anim-pulse');
			svg.appendChild(tc);
			var tt = svgEl('text', { x: '22', y: antY[i] + 3.5, fill: txtFill, 'font-size': '8.5', 'font-family': 'monospace' });
			tt.textContent = 'T' + (i + 1);
			svg.appendChild(tt);

			svg.appendChild(svgEl('line', { x1: '264', y1: antY[i], x2: '282', y2: antY[i], stroke: lineStroke, 'stroke-width': '2', 'stroke-linecap': 'round' }));
			var rc = svgEl('circle', { cx: '276', cy: antY[i], r: active ? '4.5' : '3.2', fill: dotFill });
			if (active) rc.setAttribute('class', 'mt-anim-pulse');
			svg.appendChild(rc);
			var rt = svgEl('text', { x: '289', y: antY[i] + 3.5, fill: txtFill, 'font-size': '8.5', 'font-family': 'monospace' });
			rt.textContent = 'R' + (i + 1);
			svg.appendChild(rt);
		}

		// 激活的空间流: 曲线 + 流动光点
		for (var s = 0; s < numRank; s++) {
			var y = antY[s];
			var d = 'M 56 ' + y + ' C 120 ' + (y - 18) + ', 200 ' + (y + 18) + ', 264 ' + y;
			var path = svgEl('path', {
				d: d,
				stroke: streamColors[s],
				'stroke-width': '2.4',
				'stroke-linecap': 'round',
				'stroke-dasharray': '7 5',
				opacity: '0.9',
				'class': 'mt-anim-stream'
			});
			path.setAttribute('id', 'mtmimo-path-' + s);
			svg.appendChild(path);

			/* 沿曲线流动的数据光点 (SMIL animateMotion) */
			var fly = svgEl('circle', { r: '2.8', fill: '#ffffff', opacity: '0.95' });
			var motion = svgEl('animateMotion', { dur: (2.1 + s * 0.4) + 's', repeatCount: 'indefinite' });
			motion.appendChild(svgEl('mpath', { href: '#mtmimo-path-' + s }));
			fly.appendChild(motion);
			svg.appendChild(fly);
		}

		// 左上角 Rank 角标 (与星座图角标同款式)
		var label = 'MIMO ' + numRank + '×' + numRank;
		var bw = 16 + label.length * 6.4;
		svg.appendChild(svgEl('rect', { x: '12', y: '10', width: bw, height: '20', rx: '10', fill: 'url(#mtmimo-badge)', opacity: '0.95' }));
		var badge = svgEl('text', { x: 12 + bw / 2, y: '24', 'text-anchor': 'middle', fill: '#ffffff', 'font-size': '10', 'font-weight': 'bold', 'font-family': 'sans-serif' });
		badge.textContent = label;
		svg.appendChild(badge);

		return svg;
	};

	/*
	 * 看板娘：保留头像本体（图片 + 悬浮动效），对话气泡功能已移除。
	 * 之前的气泡会挤占英雄栏标题并在窄屏下遮挡内容，按需求不再提供。
	 */
	api.mascot = function () {
		var wrap = E('div', { 'class': 'mt-mascot-speech-wrap' });

		// 二次元蓝发猫耳看板娘头像图片，外圈带光环与悬浮动效
		var avatar = E('div', { 'class': 'mt-mascot-avatar' });
		var img = E('img', {
			'class': 'mt-mascot-img',
			'src': '/luci-static/resources/fm350/mascot.png?v=' + MT5700_CSS_VERSION,
			'alt': '可可的看板娘'
		});
		avatar.appendChild(img);
		wrap.appendChild(avatar);

		return { el: wrap };
	};

	/* ================= 四格动态信号图标 (cellular signal) =================
	 *
	 * 移植自 cellular-signal-preview.html：状态由 data-state 属性驱动，
	 * 颜色与动画全部交给 mt5700.css 的 .cellular[data-state=...] 规则。
	 *
	 * 状态语义（与 CSS 一一对应，页面必须按真实数据选择，不得凭空指定）：
	 *   none          无信号（0 格 + 斜杠）
	 *   weak/medium/strong/full   1 / 2 / 3 / 4 格
	 *   searching     正在搜索网络（信号格依次扫描）
	 *   connecting    正在连接网络（右上角圆点闪烁）
	 *   disconnected  网络已断开（0 格 + 斜杠）
	 *   error         连接错误（感叹号角标）
	 */
	var CELLULAR_LEVELS = {
		none: 0, weak: 1, medium: 2, strong: 3, full: 4,
		searching: 0, connecting: 0, disconnected: 0, error: 0
	};

	var CELLULAR_LABELS = {
		none: '无信号', weak: '弱信号', medium: '中等信号', strong: '强信号', full: '信号满格',
		searching: '正在搜索网络', connecting: '正在连接网络',
		disconnected: '网络已断开', error: '网络连接错误'
	};


	api.cellularSignal = function () {
		var wrap = E('span', {
			'class': 'cellular',
			'data-state': 'searching',
			'role': 'img',
			'aria-label': CELLULAR_LABELS.searching
		});

		var svg = svgEl('svg', {
			viewBox: '0 0 32 32',
			xmlns: 'http://www.w3.org/2000/svg',
			'aria-hidden': 'true'
		});

		/* 四格信号柱（高度递增） */
		[[3, 22, 5, 7], [10, 17, 5, 12], [17, 11, 5, 18], [24, 5, 5, 24]].forEach(function (b) {
			svg.appendChild(svgEl('rect', { 'class': 'bar', x: b[0], y: b[1], width: b[2], height: b[3], rx: 1 }));
		});

		/* 无信号 / 已断开：斜杠 */
		var slash = svgEl('g', {
			'class': 'overlay slash', fill: 'none', stroke: 'currentColor',
			'stroke-width': '2.4', 'stroke-linecap': 'round'
		});
		slash.appendChild(svgEl('path', { d: 'M5 5 L27 27' }));
		svg.appendChild(slash);

		/* 连接中：右上角圆点 */
		svg.appendChild(svgEl('circle', { 'class': 'overlay connecting-dot', cx: 26, cy: 5, r: 3, fill: 'currentColor' }));

		/* 错误：感叹号角标 */
		var errMark = svgEl('g', { 'class': 'overlay error-mark' });
		errMark.appendChild(svgEl('circle', { cx: 25, cy: 7, r: 6, fill: 'currentColor' }));
		errMark.appendChild(svgEl('path', { d: 'M25 3.8 V7.5 M25 10 V10.1', stroke: '#ffffff', 'stroke-width': '1.6', 'stroke-linecap': 'round' }));
		svg.appendChild(errMark);

		wrap.appendChild(svg);

		var bars = svg.querySelectorAll('.bar');

		/* 按状态更新格数与可访问标签；未知状态退到 searching，绝不静默留空 */
		wrap.setCellularState = function (state) {
			if (!Object.prototype.hasOwnProperty.call(CELLULAR_LEVELS, state)) state = 'searching';
			wrap.setAttribute('data-state', state);
			var level = CELLULAR_LEVELS[state];
			for (var i = 0; i < bars.length; i++) {
				if (i < level) bars[i].classList.add('active');
				else bars[i].classList.remove('active');
			}
			wrap.setAttribute('aria-label', CELLULAR_LABELS[state]);
			return state;
		};

		return wrap;
	};

	api.page = function (title, subtitle, pageKey, badgeText) {
		var node = E('div', { 'class': 'mt5700-page' });

		/* pageKey/badgeText 由各视图显式传入（路由映射见 menu.d） */
		var hero = E('div', { 'class': 'mt5700-hero-card' });
		var content = E('div', { 'class': 'mt5700-hero-content' });

		var badge = E('div', { 'class': 'mt5700-hero-badge' }, [
			E('span', { 'class': 'mt5700-hero-badge-dot' }),
			E('span', {}, badgeText || 'MT5700 5G 模组')
		]);
		content.appendChild(badge);

		content.appendChild(E('h2', { 'class': 'mt5700-hero-title' }, title || ''));
		if (subtitle) {
			content.appendChild(E('p', { 'class': 'mt5700-hero-desc' }, subtitle));
		}
		hero.appendChild(content);

		var art = E('div', { 'class': 'mt5700-hero-art' });

		/* 看板娘头像（图片 + 悬浮动效）；对话气泡已按需求移除，见 api.mascot 注释 */
		var mascot = api.mascot();
		art.appendChild(mascot.el);
		hero.appendChild(art);

		node.appendChild(hero);

		var body = E('div', { 'class': 'mt5700-page-body' });
		node.appendChild(body);
		node._body = body;
		node._hero = hero;
		return node;
	};

	/* ================= 卡片 ================= */

	api.card = function (title, subtitle, extra) {
		var card = E('div', { 'class': 'mt5700-card' });
		var header = E('div', { 'class': 'mt5700-card-header' });
		header.appendChild(E('h3', { 'class': 'mt5700-card-title' }, title || ''));
		if (subtitle) header.appendChild(E('p', { 'class': 'mt5700-card-subtitle' }, subtitle));
		if (extra) {
			var ex = E('div', { 'class': 'mt5700-card-extra' });
			ex.appendChild(extra);
			header.appendChild(ex);
		}
		card.appendChild(header);
		var body = E('div', { 'class': 'mt5700-card-body' });
		card.appendChild(body);
		card._body = body;
		return card;
	};

	/* ================= 指标卡片 ================= */

	/*
	 * 单位白名单：显式枚举。
	 * 只有命中白名单才做「数字 + 单位」分段渲染，否则整段原样输出。
	 */
	var METRIC_UNITS = [
		'Mbps', 'Gbps', 'kbps', 'Kbps', 'Bps',
		'dBm', 'dB', 'MHz', 'GHz', 'kHz', 'Hz',
		'ms', 'us', 'ns', 's',
		'℃', '°C', '%',
		'MB', 'GB', 'KB', 'TB', 'B',
		'次', '个', '台'
	];

	api.metricValue = function (el, value) {
		var text = (value == null || value === '') ? '—' : String(value);
		/* 形如「102.40 Mbps」：数字 + 空格 + 白名单单位 */
		var m = /^([-+]?[\d.,]+)\s*(\S+)$/.exec(text);
		if (m && METRIC_UNITS.indexOf(m[2]) >= 0) {
			el.appendChild(document.createTextNode(m[1]));
			el.appendChild(E('span', { 'class': 'unit' }, m[2]));
			/* 数字位数多时同步降字号，避免超出卡片 */
			if (m[1].replace(/[.,]/g, '').length >= 7) el.classList.add('len-md');
			return el;
		}

		el.appendChild(document.createTextNode(text));

		/*
		 * 纯文本值的宽度分级：按「显示宽度」估算（CJK 记 1，ASCII 记 0.55）。
		 * 分档阈值经 176px 卡宽实测校准：≤6 全尺寸，>6 逐级降到 0.62em。
		 */
		var w = 0;
		for (var i = 0; i < text.length; i++) {
			w += /[\u2e80-\u9fff\uff00-\uffef]/.test(text.charAt(i)) ? 1 : 0.55;
		}
		if (w > 6 && w <= 9) el.classList.add('len-md');
		else if (w > 9 && w <= 12) el.classList.add('len-lg');
		else if (w > 12) el.classList.add('len-xl');
		return el;
	};

	api.metric = function (label, value, color) {
		var m = E('div', { 'class': 'mt5700-metric' });
		m.appendChild(E('div', { 'class': 'mt5700-metric-label', title: label || '' }, label));
		var v = E('div', { 'class': 'mt5700-metric-value', title: (value == null ? '' : String(value)) });
		api.metricValue(v, value);
		if (color) v.classList.add(color);
		m.appendChild(v);
		return m;
	};

	/*
	 * 制式代际档位。代际决定的是「理论峰值与调度效率」的量级差异，
	 * 与当前信号强弱无关。
	 *   NR   5G：Sub-6 单载波 100MHz 可跑数百 Mbps
	 *   LTE  4G：20MHz 典型 100~150Mbps，Cat 等级决定上限
	 *   WCDMA 3G：个位数到十余 Mbps
	 */
	var RAT_CAP = {
		'NR': { level: 'exc', label: '5G', note: '5G 制式，单载波带宽上限最高' },
		'LTE': { level: 'good', label: '4G', note: '4G 制式，理论峰值明显低于 5G' },
		'WCDMA': { level: 'poor', label: '3G', note: '3G 制式，仅够轻量上网' },
		'GSM': { level: 'bad', label: '2G', note: '2G 制式，无法承载数据业务' }
	};

	/* 制式名归一：^HFREQINFO 返回 6/7，^MONSC 返回 LTE/NR，页面 state 可能给任意形式 */
	function normRat(v) {
		if (v == null || v === '') return null;
		var s = String(v).trim().toUpperCase();
		if (s === '6' || s.indexOf('LTE') === 0) return 'LTE';
		if (s === '7' || s === '11' || s.indexOf('NR') === 0) return 'NR';
		if (s.indexOf('WCDMA') === 0 || s === '3') return 'WCDMA';
		if (s.indexOf('GSM') === 0 || s === '1') return 'GSM';
		return null;
	}
	/* 制式代号 → 可读标签（供页面展示与无障碍属性使用） */
	api.ratLabel = function (v) {
		var r = normRat(v);
		return r && RAT_CAP[r] ? RAT_CAP[r].label : (v ? String(v) : '—');
	};

	/* ================= 按钮 ================= */

	api.button = function (label, onClick, variant) {
		variant = variant || 'secondary';
		var btn = E('button', { 'class': 'mt5700-btn mt5700-btn-' + variant }, label);
		if (onClick) btn.addEventListener('click', onClick);
		return btn;
	};

	api.primaryButton = function (label, onClick) {
		return api.button(label, onClick, 'primary');
	};


	api.dangerButton = function (label, onClick) {
		return api.button(label, onClick, 'danger');
	};

	api.ghostButton = function (label, onClick) {
		return api.button(label, onClick, 'ghost');
	};

	/* ================= 状态标签 ================= */

	api.badge = function (text, variant) {
		variant = variant || 'neutral';
		return E('span', { 'class': 'mt5700-badge mt5700-badge-' + variant }, text);
	};


	/* ================= 模态框 ================= */

	api.confirm = function (message, onOk, okText) {
		var mask = E('div', { 'class': 'mt5700-modal-mask' });
		var box = E('div', { 'class': 'mt5700-modal' });
		var text = E('div', { 'class': 'mt5700-modal-body' }, message);
		var actions = E('div', { 'class': 'mt5700-modal-footer' });
		var cancel = api.ghostButton('取消', function () {
			if (mask.parentNode) mask.parentNode.removeChild(mask);
		});
		var ok = api.primaryButton(okText || '确定', function () {
			if (mask.parentNode) mask.parentNode.removeChild(mask);
			if (onOk) onOk();
		});
		actions.appendChild(cancel);
		actions.appendChild(ok);
		box.appendChild(text);
		box.appendChild(actions);
		mask.appendChild(box);
		mask.addEventListener('click', function (e) {
			if (e.target === mask && mask.parentNode) mask.parentNode.removeChild(mask);
		});
		document.body.appendChild(mask);
		return mask;
	};

	/* ================= Toast 通知 ================= */

	api.toast = function (message, type) {
		type = type || 'info';
		var t = E('div', { 'class': 'mt5700-toast mt5700-toast-' + type }, message);
		document.body.appendChild(t);
		setTimeout(function () { t.classList.add('mt5700-toast-hide'); }, 3000);
		setTimeout(function () { if (t.parentNode) t.parentNode.removeChild(t); }, 3500);
	};

	api.success = function (m) { api.toast(m, 'success'); };
	api.error = function (m) { api.toast(m, 'error'); };
	api.warning = function (m) { api.toast(m, 'warning'); };
	api.info = function (m) { api.toast(m, 'info'); };

	/* ================= Loading / Error / Empty ================= */

	api.loading = function (text) {
		var el = E('div', { 'class': 'mt5700-loading' });
		var sp = E('div', { 'class': 'mt-anime-spinner-box' });
		var svg = svgEl('svg', {
			'class': 'mt-anime-spinner-svg mt-anim-spin',
			viewBox: '0 0 50 50',
			width: '38',
			height: '38',
			fill: 'none',
			xmlns: 'http://www.w3.org/2000/svg'
		});
		svg.appendChild(svgEl('circle', {
			cx: '25', cy: '25', r: '20',
			stroke: 'rgba(167, 139, 250, 0.25)',
			'stroke-width': '4'
		}));
		svg.appendChild(svgEl('path', {
			d: 'M25 5 A20 20 0 0 1 45 25',
			stroke: '#8b5cf6',
			'stroke-width': '4',
			'stroke-linecap': 'round'
		}));
		svg.appendChild(svgEl('circle', { cx: '25', cy: '5', r: '3.5', fill: '#f472b6' }));
		sp.appendChild(svg);
		el.appendChild(sp);
		el.appendChild(E('div', { 'class': 'mt5700-loading-text' }, text || '正在全力加载中喵~ ♡'));
		return el;
	};

	/*
	 * 空态立绘源码（小吱-喜欢，1000x1000 方图）。
	 *
	 * 整段 innerHTML 注入而不是逐个 appendChild：这幅画有 20 多个渐变 /
	 * 滤镜定义，用 DOM API 拼既不可读也没法维护。defs 里的 id 在注入时
	 * 按实例加后缀（见下方 replace），避免同页多个空态互相串色 ——
	 * 重复 id 会让 url(#...) 全部解析到文档里第一份，渲染虽然一致，
	 * 但调试时根本分不清是哪一份。
	 */
		
	/*
	 * 空态伙伴立绘 mint（777×1000 竖图）：与主立绘同时显示，
	 * 每次渲染位置随机（见 api.empty），渐变 id 走同一套 uid 改名防冲突。
	 */
	
	api.empty = function (text) {
		/* 空态：纯文本提示（原 MT5700 版本在此渲染随机落位的角色插画 SVG，
		 * 移植时按需求去掉 —— 占位大、体积大，且与 FM350 的页面风格无关）。 */
		return E('div', { 'class': 'mt5700-empty' },
			E('div', { 'class': 'mt5700-empty-text' }, text || '暂无数据'));
	};

	/* ================= 表格 ================= */

	api.table = function (headers, rows, options) {
		options = options || {};
		var wrapper = E('div', { 'class': 'mt5700-table-wrapper' });
		var table = E('table', { 'class': 'mt5700-table' });
		if (options.striped) table.classList.add('mt5700-table-striped');

		/*
		 * 手机端（<=600px）表格改为「卡片式堆叠」：隐藏表头，每个单元格
		 * 用 ::before 显示自己的列名。因此这里把表头文案写进每个 td 的
		 * data-label，CSS 侧用 content: attr(data-label) 读取。
		 * 桌面端 data-label 不显示，对既有布局零影响。
		 */
		var labelOf = function (i) {
			var h = headers[i];
			return (h == null) ? '' : String(h);
		};

		/*
		 * 只有两列的键值表：手机端不适用「一行一卡」的堆叠（会把每项
		 * 拆成上下两行，项数多时反而更高），改由 .mt5700-table-kv
		 * 走「标签左 / 值右」的紧凑分栏行。
		 */
		if (headers.length === 2) table.classList.add('mt5700-table-kv');

		// 表头
		var thead = E('thead');
		var tr = E('tr');
		headers.forEach(function (h) {
			tr.appendChild(E('th', {}, h));
		});
		thead.appendChild(tr);
		table.appendChild(thead);

		// 表体
		var tbody = E('tbody');
		if (!rows || !rows.length) {
			var tr0 = E('tr');
			tr0.appendChild(E('td', { colspan: headers.length, 'class': 'mt5700-empty' }, '暂无数据'));
			tbody.appendChild(tr0);
		} else {
			rows.forEach(function (row) {
				var tr = E('tr');
				row.forEach(function (cell, ci) {
					var td;
					if (typeof cell === 'object' && cell.nodeType) {
						td = E('td');
						td.appendChild(cell);
					} else {
						td = E('td', {}, cell || '—');
					}
					var lbl = labelOf(ci);
					if (lbl) td.setAttribute('data-label', lbl);
					tr.appendChild(td);
				});
				tbody.appendChild(tr);
			});
		}
		table.appendChild(tbody);
		wrapper.appendChild(table);
		return wrapper;
	};

	/* ================= 表单 ================= */

	api.formGroup = function (label, input, hint, required) {
		var group = E('div', { 'class': 'mt5700-form-group' });
		var lbl = E('label', { 'class': 'mt5700-label' + (required ? ' mt5700-label-required' : '') }, label);
		group.appendChild(lbl);
		group.appendChild(input);
		if (hint) group.appendChild(E('div', { 'class': 'mt5700-hint' }, hint));
		return group;
	};

	api.input = function (type, placeholder, value) {
		var input = E('input', { 'class': 'mt5700-input', type: type || 'text' });
		if (placeholder) input.placeholder = placeholder;
		if (value != null) input.value = value;
		return input;
	};

	api.select = function (options, value) {
		var sel = E('select', { 'class': 'mt5700-input mt5700-select' });
		options.forEach(function (opt) {
			var o = E('option', { value: opt.value }, opt.label);
			if (opt.value === value) o.selected = true;
			sel.appendChild(o);
		});
		return sel;
	};

	/* ================= 面板操作区 ================= */

	api.panelActions = function () {
		var el = E('div', { 'class': 'mt5700-panel-actions' });
		for (var i = 0; i < arguments.length; i++) {
			el.appendChild(arguments[i]);
		}
		return el;
	};

	/* ================= 高级控件 ================= */

	/*
	 * 卡片式单选组
	 *
	 * 用途：把「用户看不懂的编码」变成「用户可以按影响面直接选」的选项。
	 * 与原生 <select> 的区别：每个选项都有独立标题与说明文案，说明始终可见，
	 * 不需要展开下拉才能比较。适合选项数量少（<=8）且每项都需要解释的场景。
	 *
	 * options: [{ value, label, desc, badge }]
	 *   value —— 真实下发值（如 "080302"），onChange 收到它
	 *   label —— 选项主标题（通俗中文）
	 *   desc  —— 选项说明，讲清「选了会怎样」
	 *   badge —— 可选角标，如「推荐」「仅搜索」
	 *
	 * 返回值为容器元素，额外挂载：
	 *   el.getValue()         取当前值
	 *   el.setValue(v, silent) 设当前值（silent=true 时不触发 onChange）
	 *   el.setDisabled(v, off) 单项禁用（用于「含 LTE 时不允许 CS_ONLY」这类互斥约束）
	 */
	api.radioCards = function (name, options, value, onChange) {
		var wrap = E('div', { 'class': 'mt5700-radio-cards' });
		var items = [];

		options.forEach(function (opt, idx) {
			var id = 'mt5700-rc-' + name + '-' + idx;
			var label = E('label', { 'class': 'mt5700-radio-card', 'for': id });
			var input = E('input', { type: 'radio', name: 'mt5700-rc-' + name, id: id, value: opt.value });
			var body = E('div', { 'class': 'mt5700-radio-card-body' });
			var head = E('div', { 'class': 'mt5700-radio-card-head' });
			head.appendChild(E('span', { 'class': 'mt5700-radio-card-title' }, opt.label));
			if (opt.badge) head.appendChild(E('span', { 'class': 'mt5700-radio-card-badge' }, opt.badge));
			body.appendChild(head);
			if (opt.desc) body.appendChild(E('div', { 'class': 'mt5700-radio-card-desc' }, opt.desc));
			if (opt.code) body.appendChild(E('code', { 'class': 'mt5700-radio-card-code' }, opt.code));
			label.appendChild(input);
			label.appendChild(E('span', { 'class': 'mt5700-radio-card-dot' }));
			label.appendChild(body);
			wrap.appendChild(label);
			items.push({ input: input, label: label, option: opt });
			input.addEventListener('change', function () {
				if (input.checked && typeof onChange === 'function') onChange(opt.value, opt);
			});
		});

		function apply(v) {
			items.forEach(function (it) { it.input.checked = (it.option.value === v); });
		}
		apply(value);

		wrap.getValue = function () {
			for (var i = 0; i < items.length; i++) if (items[i].input.checked) return items[i].option.value;
			return '';
		};
		wrap.setValue = function (v, silent) {
			apply(v);
			if (!silent && typeof onChange === 'function') {
				var hit = items.filter(function (it) { return it.option.value === v; })[0];
				if (hit) onChange(v, hit.option);
			}
		};
		/* 单项禁用：用于官方约束（如 srvdomain 含 LTE/NR 时不允许 0 或 3） */
		wrap.setDisabled = function (v, off) {
			items.forEach(function (it) {
				if (it.option.value !== v) return;
				it.input.disabled = !!off;
				it.label.classList.toggle('mt5700-radio-card-disabled', !!off);
				if (off && it.input.checked) it.input.checked = false;
			});
		};
		wrap.setAllDisabled = function (off) {
			items.forEach(function (it) {
				it.input.disabled = !!off;
				it.label.classList.toggle('mt5700-radio-card-disabled', !!off);
			});
		};
		return wrap;
	};

	/*
	 * 底层参数展示行
	 *
	 * 用途：界面用通俗文案，但底层编码仍要可见可审计（便于排障与对标 AT 手册）。
	 * 用等宽灰底呈现编码值，不参与编辑，用户不会误改。
	 */
	api.rawValue = function (label, value) {
		var row = E('div', { 'class': 'mt5700-raw-row' });
		row.appendChild(E('span', { 'class': 'mt5700-raw-label' }, label));
		var val = E('code', { 'class': 'mt5700-raw-value' }, (value === '' || value == null) ? '—' : String(value));
		row.appendChild(val);
		row.setValue = function (v) { val.textContent = (v === '' || v == null) ? '—' : String(v); };
		return row;
	};

	/*
	 * 字段说明块
	 *
	 * 比 formGroup 的 hint 更结构化：支持一个主说明 + 若干条要点。
	 * 用于把一个技术字段的作用、影响面、注意事项讲清楚。
	 */
	api.fieldNote = function (summary, points) {
		var box = E('div', { 'class': 'mt5700-field-note' });
		if (summary) box.appendChild(E('div', { 'class': 'mt5700-field-note-summary' }, summary));
		if (points && points.length) {
			var ul = E('ul', { 'class': 'mt5700-field-note-list' });
			points.forEach(function (p) { ul.appendChild(E('li', {}, p)); });
			box.appendChild(ul);
		}
		return box;
	};


	/* ================= 未保存更改（暂存 / 保存并应用 / 撤销） ================= */

	/*
	 * OpenWrt 标准「保存并应用」语义的前端实现：
	 * - 页面修改不立即下发，先调用 staged.set(key, label, run) 暂存
	 * - 同一 key 重复修改自动覆盖（只应用最后一次）
	 * - 悬浮条实时显示未保存项数量，提供「保存并应用」「撤销更改」两个入口
	 * - 应用时逐项执行 run()（返回 Promise），成功/失败后回调刷新实际状态
	 */
	api.staged = function (options) {
		options = options || {};
		var items = []; // { key, label, run }
		var bar = E('div', { 'class': 'mt5700-applybar mt5700-applybar-hidden' });
		var text = E('span', { 'class': 'mt5700-applybar-text' });
		var actions = E('div', { 'class': 'mt5700-applybar-actions' });
		var revertBtn = api.ghostButton('撤销更改', doRevert);
		var applyBtn = api.primaryButton('保存并应用', doApply);
		actions.appendChild(revertBtn);
		actions.appendChild(applyBtn);
		bar.appendChild(text);
		bar.appendChild(actions);

		function refresh() {
			if (items.length) {
				text.textContent = '有 ' + items.length + ' 项未保存的更改';
				bar.classList.remove('mt5700-applybar-hidden');
			} else {
				bar.classList.add('mt5700-applybar-hidden');
			}
		}

		function doApply() {
			if (!items.length) return;
			var queue = items.slice();
			applyBtn.disabled = true;
			revertBtn.disabled = true;
			text.textContent = '正在应用更改（' + queue.length + ' 项）…';
			var chain = Promise.resolve();
			queue.forEach(function (it) {
				chain = chain.then(function () { return it.run(); });
			});
			return chain.then(function () {
				items = [];
				api.success('更改已应用');
			}).catch(function (err) {
				api.error((err && err.message) || '应用更改失败');
			}).then(function () {
				applyBtn.disabled = false;
				revertBtn.disabled = false;
				refresh();
				/* 成败都重新拉取，让界面与实际状态对齐 */
				if (options.onChanged) options.onChanged();
			});
		}

		function doRevert() {
			api.confirm('确定放弃全部未保存的更改？', function () {
				items = [];
				refresh();
				if (options.onChanged) options.onChanged();
			}, '放弃更改');
		}

		return {
			el: bar,
			/* key 相同的暂存项会被覆盖，避免重复下发同一配置 */
			set: function (key, label, run) {
				for (var i = 0; i < items.length; i++) {
					if (items[i].key === key) {
						items[i].label = label;
						items[i].run = run;
						refresh();
						return;
					}
				}
				items.push({ key: key, label: label, run: run });
				refresh();
			},
			remove: function (key) {
				items = items.filter(function (it) { return it.key !== key; });
				refresh();
			},
			clear: function () { items = []; refresh(); },
			count: function () { return items.length; }
		};
	};

	/* ================= 速率显示 (L3 浮动) ================= */


	return api;
})();

// 导出
var Mt5700Class = L.Class.extend(Mt5700);
if (typeof window !== 'undefined') {
	window.Mt5700 = new Mt5700Class();
}
return Mt5700Class;
