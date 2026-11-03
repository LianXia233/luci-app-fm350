'use strict';
'require baseclass';
/* global L */
/**
 * fm350/theme.js —— LuCI 主题明暗检测（兼容夜间模式）
 *
 * 背景（issue #26）：
 *   插件样式的暗色适配此前只跟随 prefers-color-scheme（系统/浏览器），
 *   而 LuCI 各主题的明暗状态各有来源，二者不一致时页面明暗混杂：
 *     - 系统暗色 + 主题亮色：插件把组件切暗，主题 Tab 文字仍是深灰
 *       → 顶部选择栏深底深字看不清；
 *     - 系统亮色 + 主题暗色：插件保持亮色 → 深色页面里浮着白色组件。
 *   另有大量暗色覆盖写死 [data-theme="dark"]/.dark-theme 选择器，
 *   但从未有任何代码设置它们，等于暗色适配从未生效。
 *
 * 各主题暗色的真实来源（检测必须逐一覆盖）：
 *   - Aurora：html[data-darkmode="true|false"]（header 内联脚本同步设置，
 *     顶部按钮可运行时切换）；
 *   - Argon 3.x：html[data-argon-mode="dark|light"]，未设置时跟随系统；
 *   - Argon 2.x：手动暗色时 dark.css 直接内联，**没有任何 DOM 标记**，
 *     只能实测 body 背景亮度识别；auto 模式跟随系统媒体查询；
 *   - 官方 bootstrap / openwrt-2020：纯 prefers-color-scheme，无标记。
 *
 * 判定优先级（高 → 低）：
 *   1. 主题显式标记（用户手动选的明暗，主题自身一定已适配）；
 *   2. color-scheme 计算值（Argon 3 等在 CSS 里显式声明）；
 *   3. 亮度探测：实测 body/html 背景色（Argon 2.x 唯一可识别途径），
 *      背景不透明度不足时退看正文文字颜色（浅字=暗色主题）；
 *   4. prefers-color-scheme 媒体查询兜底（与官方主题自动暗色一致）。
 *
 * 结论写到 <html>：class at-dark / at-light + data-at-theme="dark|light"。
 * mt5700.css / at.css 据此切换变量与覆盖样式；本模块必须在注入这两份
 * CSS 之前求值（require 顺序保证），避免首屏闪烁。
 * 运行时切换（Aurora 顶部按钮等）由 MutationObserver 跟随，并通过
 * onChange 通知关注方（如 tabmenu.js 复查主题 Tab 栏对比度）。
 */

var AtTheme = (function () {
	var api = {};

	/* ================= 纯函数（可单测） ================= */

	// 解析 CSS 颜色为 {r,g,b,a}；支持 #rgb/#rrggbb/rgb()/rgba()，其余返回 null
	function parseColor(str) {
		if (typeof str !== 'string') return null;
		var s = str.trim().toLowerCase();
		var m;
		if ((m = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/.exec(s))) {
			return { r: parseInt(m[1] + m[1], 16), g: parseInt(m[2] + m[2], 16), b: parseInt(m[3] + m[3], 16), a: 1 };
		}
		if ((m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/.exec(s))) {
			return { r: parseInt(m[1], 16), g: parseInt(m[2], 16), b: parseInt(m[3], 16), a: 1 };
		}
		m = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)(?:[\s,\/]+([\d.%]+))?\s*\)$/.exec(s);
		if (m) {
			var a = 1;
			if (m[4] != null) {
				if (m[4].charAt(m[4].length - 1) === '%') a = parseFloat(m[4]) / 100;
				else a = parseFloat(m[4]);
			}
			return { r: +m[1], g: +m[2], b: +m[3], a: a };
		}
		return null;
	}
	api.parseColor = parseColor;

	// WCAG 相对亮度（0=黑 1=白）
	function relLuma(c) {
		function lin(v) {
			v = v / 255;
			return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
		}
		return 0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b);
	}
	api.relLuma = relLuma;

	// WCAG 对比度（1..21）
	function contrastRatio(c1, c2) {
		var l1 = relLuma(c1), l2 = relLuma(c2);
		var hi = Math.max(l1, l2), lo = Math.min(l1, l2);
		return (hi + 0.05) / (lo + 0.05);
	}
	api.contrastRatio = contrastRatio;

	// 半透明 c 叠加到底色 base 之上（alpha 预乘近似）
	function composite(c, base) {
		var a = (c && c.a != null) ? c.a : 1;
		if (a >= 1) return c;
		var b = base || { r: 255, g: 255, b: 255, a: 1 };
		return {
			r: Math.round(c.r * a + b.r * (1 - a)),
			g: Math.round(c.g * a + b.g * (1 - a)),
			b: Math.round(c.b * a + b.b * (1 - a)),
			a: 1
		};
	}
	api.composite = composite;

	// 已知主题明暗标记 → 'dark' | 'light' | null（无显式标记）
	// htmlEl/bodyEl 只读 class 与 data-* 属性，兼容无 DOM 环境（单测传桩）
	var DARK_CLASS_TOKENS = { dark: 1, 'dark-theme': 1, 'theme-dark': 1, night: 1, nightmode: 1, darkmode: 1 };
	var LIGHT_CLASS_TOKENS = { light: 1, 'light-theme': 1, 'theme-light': 1 };

	function attrVerdict(el, names) {
		if (!el || typeof el.getAttribute !== 'function') return null;
		for (var i = 0; i < names.length; i++) {
			var v = (el.getAttribute(names[i]) || '').trim().toLowerCase();
			if (!v) continue;
			if (v === 'true' || v === 'dark' || v.indexOf('dark') >= 0) return 'dark';
			if (v === 'false' || v === 'light' || v.indexOf('light') >= 0) return 'light';
		}
		return null;
	}

	function classVerdict(el) {
		if (!el || typeof el.className !== 'string') return null;
		var tokens = el.className.trim().split(/\s+/);
		for (var i = 0; i < tokens.length; i++) {
			if (DARK_CLASS_TOKENS.hasOwnProperty(tokens[i])) return 'dark';
		}
		for (var j = 0; j < tokens.length; j++) {
			if (LIGHT_CLASS_TOKENS.hasOwnProperty(tokens[j])) return 'light';
		}
		return null;
	}

	// 显式标记：Aurora(data-darkmode) / Argon3(data-argon-mode) / 通用(data-theme) / class 约定
	function markerVerdict(htmlEl, bodyEl) {
		var v = attrVerdict(htmlEl, ['data-darkmode', 'data-argon-mode', 'data-theme', 'data-luci-theme']);
		if (v) return v;
		v = attrVerdict(bodyEl, ['data-darkmode', 'data-theme']);
		if (v) return v;
		v = classVerdict(htmlEl);
		if (v) return v;
		return classVerdict(bodyEl);
	}
	api.markerVerdict = markerVerdict;

	// 实测探测：color-scheme → 背景亮度 → 正文文字色
	// win 需提供 getComputedStyle / document；返回 'dark'|'light'|null
	function probeVerdict(win) {
		var doc = win && win.document;
		if (!doc || typeof win.getComputedStyle !== 'function') return null;

		try {
			// 1) color-scheme：主题在 CSS 里显式声明时最可靠
			var cs = win.getComputedStyle(doc.documentElement).colorScheme;
			if (cs) {
				var t = String(cs).trim().toLowerCase();
				if (t === 'dark') return 'dark';
				if (t === 'light') return 'light';
			}

			var bodyStyle = doc.body ? win.getComputedStyle(doc.body) : null;
			var htmlStyle = win.getComputedStyle(doc.documentElement);

			var bodyBg = bodyStyle ? parseColor(bodyStyle.backgroundColor) : null;
			var htmlBg = parseColor(htmlStyle.backgroundColor);

			// 2) 背景亮度（body 半透明时叠加 html 底色）
			var base = (htmlBg && htmlBg.a > 0) ? htmlBg : { r: 255, g: 255, b: 255, a: 1 };
			var effective = null;
			if (bodyBg && bodyBg.a > 0) effective = composite(bodyBg, base);
			else if (htmlBg && htmlBg.a > 0) effective = htmlBg;

			if (effective) {
				return relLuma(effective) < 0.35 ? 'dark' : 'light';
			}

			// 3) 背景不透明（壁纸/渐变主题）：正文浅字 → 暗色主题
			if (bodyStyle) {
				var tc = parseColor(bodyStyle.color);
				if (tc) {
					if (relLuma(tc) > 0.5) return 'dark';
					if (relLuma(tc) < 0.3) return 'light';
				}
			}
		} catch (e) { /* 老引擎/异常样式表降级 */ }
		return null;
	}
	api.probeVerdict = probeVerdict;

	// 媒体查询兜底
	function mediaVerdict(win) {
		try {
			if (win && win.matchMedia && win.matchMedia('(prefers-color-scheme: dark)').matches) return 'dark';
		} catch (e) { /* ignore */ }
		return 'light';
	}
	api.mediaVerdict = mediaVerdict;

	// 综合判定（优先级：标记 > color-scheme/探测 > 媒体查询）
	function decide(htmlEl, bodyEl, win) {
		return markerVerdict(htmlEl, bodyEl) || probeVerdict(win) || mediaVerdict(win);
	}
	api.decide = decide;

	/* ================= 应用到 <html> ================= */

	var current = null;          // 'dark' | 'light'
	var listeners = [];          // 主题变化回调
	var watching = false;

	function readVerdict() {
		if (typeof document === 'undefined') return null;
		return decide(document.documentElement, document.body, window);
	}

	// 写入 <html> 的 class 与 data-at-theme；结论未变时零写、零通知
	function apply() {
		if (typeof document === 'undefined' || !document.documentElement) return current;
		var v = readVerdict();
		if (v === current) return current;
		current = v;

		var root = document.documentElement;
		try {
			root.setAttribute('data-at-theme', v);
			root.classList.toggle('at-dark', v === 'dark');
			root.classList.toggle('at-light', v === 'light');
		} catch (e) { /* 极老引擎降级为仅属性 */ }

		for (var i = 0; i < listeners.length; i++) {
			try { listeners[i](v); } catch (e) { /* 单个回调异常不阻断 */ }
		}
		return v;
	}
	api.apply = apply;

	/**
	 * 注入一份样式表（幂等：同 href 已存在则跳过）。
	 * mt5700.js 与 ui.js 的样式注入共用此入口，调用前各自先 apply() 落主题结论。
	 */
	api.injectCss = function (cssPath) {
		if (typeof document === 'undefined') return;
		var links = document.querySelectorAll('link[rel="stylesheet"]');
		for (var i = 0; i < links.length; i++) {
			if (links[i].getAttribute('href') === cssPath) return;
		}
		var link = document.createElement('link');
		link.rel = 'stylesheet';
		link.href = cssPath;
		document.head.appendChild(link);
	};

	api.dark = function () { return current === 'dark'; };
	api.current = function () { return current; };

	api.onChange = function (fn) {
		if (typeof fn === 'function' && listeners.indexOf(fn) < 0) listeners.push(fn);
		return function () {
			var i = listeners.indexOf(fn);
			if (i >= 0) listeners.splice(i, 1);
		};
	};

	/* ================= 运行时跟随 =================
	 * Aurora 顶部按钮 / Argon 菜单切换主题时重判：
	 *   - 自身写 class/attr 会触发 observer，靠「结论未变零写」断回环；
	 *   - prefers-color-scheme 变化（auto 类主题）用 matchMedia 监听。
	 */

	var reapplyScheduled = false;
	function scheduleReapply() {
		if (reapplyScheduled) return;
		reapplyScheduled = true;
		setTimeout(function () {
			reapplyScheduled = false;
			apply();
		}, 50);
	}

	function startWatch() {
		if (watching || typeof window === 'undefined') return;
		watching = true;

		try {
			var mq = window.matchMedia('(prefers-color-scheme: dark)');
			if (mq.addEventListener) mq.addEventListener('change', scheduleReapply);
			else if (mq.addListener) mq.addListener(scheduleReapply); /* Safari<14 */
		} catch (e) { /* ignore */ }

		if (typeof MutationObserver === 'function') {
			try {
				var mo = new MutationObserver(scheduleReapply);
				if (document.documentElement) {
					mo.observe(document.documentElement, { attributes: true, attributeFilter: ['class', 'data-darkmode', 'data-argon-mode', 'data-theme'] });
				}
				if (document.body) {
					mo.observe(document.body, { attributes: true, attributeFilter: ['class', 'data-darkmode', 'data-theme'] });
				}
			} catch (e) { /* 老引擎降级为 matchMedia */ }
		}

		// 主题脚本晚于本模块设置标记的场景补一枪
		setTimeout(apply, 500);
		setTimeout(apply, 2000);
	}

	// 模块求值即判定（require 链保证先于 mt5700.css / at.css 注入执行）
	if (typeof document !== 'undefined') {
		apply();
		if (document.readyState === 'loading') {
			document.addEventListener('DOMContentLoaded', function () { apply(); startWatch(); });
		} else {
			startWatch();
		}
	}

	return api;
})();

// 导出（遵循本仓库模块约定：L.Class 子类 + window 挂载）
var AtThemeClass = L.Class.extend(AtTheme);
if (typeof window !== 'undefined') {
	window.AtTheme = new AtThemeClass();
}
return AtThemeClass;
