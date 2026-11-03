'use strict';
'require baseclass';
'require fm350/compat';
'require fm350/theme';
'require fm350/rpc';
/* global L, AtWs, AtTheme */
/**
 * LuCI 页面公共 UI 辅助：连接状态条、面板、字段、提示、加载态。
 * 统一各页面的交互语义：加载态、错误提示、确认弹窗、自动刷新。
 */

// 注入公共样式（at.css 与 ui.js 同目录）。
// uhttpd 给静态资源的 Last-Modified 为 1970，浏览器会长期复用缓存；LuCI 主题同样
// 采用「css?v=版本」的做法（aurora: main.css?v=1.3.1）。样式版本随包版本递增，
// 保证升级/修复样式后用户刷新即可生效。
var AT_CSS_VERSION = '1.15.1';
// 先落主题明暗结论（html.at-dark / at-light），再注入样式，避免首屏明暗闪烁
if (window.AtTheme) window.AtTheme.apply();
if (window.AtTheme) window.AtTheme.injectCss('/luci-static/resources/fm350/at.css?v=' + AT_CSS_VERSION);
// 后端未就绪提示条的样式不在这里：它与弹窗可读性修订统一放在 global.css，
// 由 mt5700.js 注入（该模块被所有视图 require），保证每个页面都有同一条提示条。

var Ui = (function () {
	var api = {};





	// 所有按钮都经由此卡口创建。cls 接收视图传入的 cbi-button-* 变体，
	// 通过映射附加 at-btn-* 系列类：外观完全由 at.css 决定，不受 LuCI 主题影响。
	api.button = function (label, cls, onClick) {
		var map = {
			'cbi-button-positive': 'at-btn-primary',
			'cbi-button-negative': 'at-btn-danger',
			'cbi-button-neutral': 'at-btn-neutral',
			'cbi-button-action': 'at-btn-action'
		};
		var extra = (cls || '').split(/\s+/).map(function (c) { return map[c] || ''; }).join(' ');
		var btn = E('button', { 'class': 'at-btn ' + extra + ' ' + (cls || '') }, label);
		btn.addEventListener('click', onClick);
		return btn;
	};






	/**
	 * 开关（checkbox 拨杆），带回滚约定（Codex P2：模组拒绝 AT 指令时必须还原勾选状态）。
	 *
	 * - onChange 返回 Promise 时，以 resolve 值判定成败：false 代表模组拒绝、
	 *   超时或请求异常，此时统一把 checkbox 恢复到操作前的状态；
	 * - handler 内部只负责「发指令 + 提示」，不再各自写回滚，避免漏写；
	 * - 返回 undefined 的老式 handler 不触发回滚，保持向后兼容。
	 */
	api.switch = function (onChange) {
		var wrap = E('div', { 'class': 'mt5700-switch' });
		var input = E('input', { type: 'checkbox' });
		input.addEventListener('change', function () {
			var checked = input.checked;
			var result;
			try {
				result = onChange(checked, input);
			} catch (e) {
				input.checked = !checked;
				return;
			}
			if (!result || typeof result.then !== 'function') return;
			result.then(function (ok) {
				if (ok === false) input.checked = !checked;
			}).catch(function () {
				input.checked = !checked;
			});
		});
		wrap.appendChild(input);
		return wrap;
	};

	api.promptModal = function (title, fields, onOk) {
		var mask = E('div', { 'class': 'at-modal-mask' });
		var box = E('div', { 'class': 'at-modal at-modal-wide' });
		var h = E('h4', { 'class': 'at-modal-title' }, title);
		var body = E('div', { 'class': 'at-modal-body' });
		var inputs = {};
		for (var i = 0; i < fields.length; i++) {
			var f = fields[i];
			var row = E('div', { 'class': 'at-field' });
			row.appendChild(E('div', { 'class': 'at-field-label' }, f.label));
			var ctrl = E('div', { 'class': 'at-field-control' });
			var input;
			if (f.type === 'select') {
				input = document.createElement('select');
				input.className = 'cbi-input-select';
				for (var j = 0; j < f.options.length; j++) {
					var opt = document.createElement('option');
					opt.value = String(f.options[j].value);
					opt.textContent = f.options[j].label;
					if (String(f.options[j].value) === String(f.value)) opt.selected = true;
					input.appendChild(opt);
				}
			} else {
				input = document.createElement('input');
				input.type = f.type === 'password' ? 'password' : 'text';
				input.className = 'cbi-input-text';
				if (f.value != null) input.value = String(f.value);
				if (f.placeholder) input.placeholder = f.placeholder;
			}
			ctrl.appendChild(input);
			if (f.hint) ctrl.appendChild(E('div', { 'class': 'at-field-hint' }, f.hint));
			row.appendChild(ctrl);
			body.appendChild(row);
			inputs[f.key] = input;
		}
		var actions = E('div', { 'class': 'at-modal-actions' });
		var cancel = api.button('取消', 'cbi-button-neutral', function () {
			if (mask.parentNode) mask.parentNode.removeChild(mask);
		});
		var ok = api.button('确定', 'cbi-button-positive', function () {
			var values = {};
			for (var key in inputs) values[key] = inputs[key].value;
			if (mask.parentNode) mask.parentNode.removeChild(mask);
			if (onOk) onOk(values);
		});
		actions.appendChild(cancel);
		actions.appendChild(ok);
		box.appendChild(h);
		box.appendChild(body);
		box.appendChild(actions);
		mask.appendChild(box);
		mask.addEventListener('click', function (e) {
			if (e.target === mask && mask.parentNode) mask.parentNode.removeChild(mask);
		});
		document.body.appendChild(mask);
		return mask;
	};

	/* ---------- 连接状态条 ---------- */


	/* ---------- 后端降级提示（统一入口） ---------- */

	/**
	 * 把悬浮提示条挪到「吸顶头部」下方。
	 *
	 * 提示条挂在 #maincontent 里，而 #maincontent 只有 z-index:1；Argon 一类主题
	 * 把头部做成 position:fixed 且层级更高，提示条自己的 z-index 再大也压不过
	 * 头部，会被整个盖在下面（用户看到的就是「提示条没出现」）。
	 * 所以这里实测一次头部高度，把结果写进 style.top；窗口变化时再量。
	 *
	 * 只认「贴顶 + 够宽 + 不占半屏」的头部：侧边栏（#mainmenu）同样是 fixed，
	 * 但它贴左不满宽，三条里「够宽」这条会把它筛掉。
	 */
	function placeNotice(el) {
		if (!el) return;
		var top = 12;
		var vw = window.innerWidth || 0;
		var vh = window.innerHeight || 0;
		var heads = document.querySelectorAll('header, .main-header, .main-header-inner, #topbar, .navbar');

		for (var i = 0; i < heads.length; i++) {
			var cs = window.getComputedStyle(heads[i]);
			if (cs.position !== 'fixed' && cs.position !== 'sticky') continue;
			var r = heads[i].getBoundingClientRect();
			if (r.top > 4 || r.height < 16 || r.height > vh * 0.4) continue;
			if (r.width < vw * 0.55) continue;
			top = Math.max(top, Math.round(r.bottom) + 12);
			break;
		}

		el.style.top = top + 'px';
	}

	/**
	 * 在页面顶部插入「后端未就绪」提示条，并在真正恢复后自动移除 + 重新加载数据。
	 *
	 * 背景：connect() 现在「乐观放行 + 后台探活」，后端不可用时会在稍后
	 * 派发 'degraded' 状态。各页面若不做提示，用户看到的是空控件，
	 * 会以为页面坏了。这里把「提示 + 自愈」收敛成一个函数，
	 * 避免 10 个页面各写一份样式与逻辑。
	 *
	 * 关键实现约束（实测踩过的坑）：**不能一看到 connected 就移除提示条**。
	 * connect() 是乐观放行，重连时它会先派发 connecting→connected，
	 * 探活结论 10s 后才到。若在 connected 时移除，提示条会「闪一下就没了」，
	 * 而重连只要还失败就会反复出现这个闪烁。因此这里的移除条件是
	 * 「恢复到 connected 且后端已知可用（未处于熔断/已知不可用状态）」。
	 *
	 * 视觉上是一条**视口悬浮**的提示条（`position: fixed`），不占文档流：
	 * 原来插的是 LuCI 原生 `.alert-message`，在流里会把整页内容整体推下去，
	 * 探活抖动一次页面就上下跳一次，正在填表时尤其难受。
	 *
	 * @param {HTMLElement} body   页面根节点（提示条挂在它里面，随页面一起销毁）
	 * @param {Function}    onReady 后端恢复后要执行的数据加载函数
	 * @returns {HTMLElement|null} 提示条元素
	 */
	api.degradedNotice = function (body, onReady) {
		if (!body || !AtWs || !AtWs.client) return null;

		/* 幂等：已有提示条就复用它，避免重连抖动导致插入多个 */
		if (body.__atwsDegradedBanner && body.__atwsDegradedBanner.parentNode) {
			return body.__atwsDegradedBanner;
		}

		var err = AtWs.client.error || AtWs.client.backendDownError
			|| '正在等待 at-webserver 服务启动，页面会在恢复后自动刷新。';
		var banner = E('div', {
			'class': 'alert-message warning mt5700-float-notice',
			'role': 'status',
			'aria-live': 'polite'
		}, [
			E('strong', {}, '模组后端未就绪：'),
			E('span', {}, err)
		]);

		/* 悬浮条一直挂在视野里，给一个「知道了」的出口：
		 * 只是收起本次提示，后端再降级时会按下面的状态回调重新出现。 */
		var dismiss = E('button', {
			'class': 'mt5700-float-notice-x',
			'type': 'button',
			'aria-label': '关闭提示',
			'title': '关闭提示'
		}, '×');
		dismiss.addEventListener('click', function () {
			if (banner.parentNode) banner.parentNode.removeChild(banner);
			if (body.__atwsDegradedBanner === banner) body.__atwsDegradedBanner = null;
		});
		banner.appendChild(dismiss);

		body.insertBefore(banner, body.firstChild);
		body.__atwsDegradedBanner = banner;
		placeNotice(banner);

		/* 窗口尺寸变化后头部高度会变（移动端横竖屏切换、侧栏折叠），重新量一次 */
		var onResize = function () {
			if (!banner.parentNode) {
				window.removeEventListener('resize', onResize);
				return;
			}
			placeNotice(banner);
		};
		window.addEventListener('resize', onResize);

		/* 一次注册、长期生效：依据「后端是否真的可用」决定去留 */
		var onState = function (state) {
			var genuinelyUp = (state === 'connected' && !AtWs.client.backendKnownDown
				&& AtWs.client.circuitOpenUntil <= Date.now());
			if (genuinelyUp) {
				if (banner.parentNode) banner.parentNode.removeChild(banner);
				if (body.__atwsDegradedBanner === banner) body.__atwsDegradedBanner = null;
				if (typeof onReady === 'function') onReady();
			} else if ((state === 'degraded' || state === 'error')
				&& !banner.parentNode) {
				/* 又降级了：提示条若已被移除则重新插回（重新量一次头部高度） */
				body.insertBefore(banner, body.firstChild);
				placeNotice(banner);
			}
		};
		AtWs.client.onConnectionStateChange(onState);

		/* 离开本页时把这一条也摘掉：ATClient 是单例，回调数组不清理的话
		 * 每进一次页面就多一个闭包（连同本页 body 一起留在内存里）。 */
		api.onLeave(function () {
			window.removeEventListener('resize', onResize);
			AtWs.client.offConnectionStateChange(onState);
			if (banner.parentNode) banner.parentNode.removeChild(banner);
			if (body.__atwsDegradedBanner === banner) body.__atwsDegradedBanner = null;
		});

		return banner;
	};

	/**
	 * 各页面启动时的统一入口：连接 → 降级则提示 → 否则加载数据。
	 *
	 * 因为 connect() 采用「乐观放行 + 后台探活」，这里要处理两种情况：
	 *   a) 调用时刻已经是 degraded（例如重连过程中再次进入页面）-> 立刻提示；
	 *   b) 调用时刻还是 connected，但探活随后失败 -> 由 state 回调异步提示。
	 *
	 * 实现要点：**监听必须在 onReady 之前注册**。
	 * 若先加载数据、后挂监听，探活 12s 后才报 degraded 时，
	 * 数据请求早已失败并被各自的 catch 吞掉，页面拿到不到任何提示
	 * （实测就是这样：页面不卡了，但用户看不到「后端未就绪」）。
	 *
	 * degradedNotice 内部已做成幂等且自管理去留，
	 * 因此这里只需在「曾进入降级」时调用一次即可，无需守卫重复调用。
	 *
	 * 用法：Ui.startup(body, function () { loadAll(); });
	 */
	api.startup = function (body, onReady) {
		/* 先挂监听，再启动：保证任何时刻进入 degraded 都能被捕获。
		 * onConnectionStateChange 注册时会立刻回调一次当前状态，
		 * 因此场景 a 无需额外分支。 */
		var onState = function (state) {
			if (state === 'degraded' || state === 'error') {
				api.degradedNotice(body, onReady);
			}
		};
		AtWs.client.onConnectionStateChange(onState);

		/* 页面离场注销：ATClient 是单例，不摘的话每进一次页面就多一个
		 * 捕获 body/onReady 的闭包，切页次数多了会一直积累。 */
		api.onLeave(function () { AtWs.client.offConnectionStateChange(onState); });

		return AtWs.client.connect().then(function () {
			if (AtWs.client.state === 'degraded') {
				api.degradedNotice(body, onReady);
				return;
			}
			if (typeof onReady === 'function') onReady();
		}).catch(function (err) {
			/* connect 自身已保证不 reject；这里是最后一道兜底，保证页面不白屏 */
			console.warn('启动流程异常', err);
			if (typeof onReady === 'function') {
				try { onReady(); } catch (e) { /* ignore */ }
			}
		});
	};

	/* ---------- 常用 AT 辅助 ---------- */

	api.sleep = function (ms) {
		return new Promise(function (resolve) { window.setTimeout(resolve, ms); });
	};

	// 发命令前固定等待 100ms，保证命令之间的发送间隔
	api.sendCmd = function (command) {
		return api.sleep(100).then(function () { return AtWs.client.sendCommand(command); });
	};

	// 错误文本还原：把 CME ERROR 等原始错误码转成可读中文提示
	api.atErrorText = function (res, fallback) {
		if (!res || res.success) return fallback;
		var msg = res.error || fallback;
		if (typeof msg === 'string' && msg.indexOf('CME ERROR') >= 0) {
			var code = msg.match(/CME ERROR:\s*(\d+)/);
			if (code) {
				var map = {
					'1': '未指定的错误', '3': '操作不允许', '4': '操作不支持', '10': '模组忙',
					'11': 'PIN 未输入或错误', '13': 'SIM 故障', '14': 'SIM 已满', '21': '无效字符',
					'22': '无效索引', '100': '未知错误', '513': 'SIM 锁定', '515': 'SIM 错误',
					'516': '内存满', '517': '无效内存索引', '518': '内存错误'
				};
				return map[code[1]] || msg;
			}
		}
		return msg;
	};

	// 飞行模式：CFUN=0 开启（关射频），CFUN=1 关闭。返回是否成功。
	api.setFlightMode = function (on) {
		return api.sendCmd('AT+CFUN=' + (on ? '0' : '1')).then(function (res) { return !!res.success; });
	};


	/* ---------- 通用渲染 ---------- */



	/* ---------- 自动刷新 ---------- */

	// 自动刷新开关 + 间隔选择。返回 { el, setEnabled, setInterval, getInterval, isEnabled }
	api.autoRefresh = function (onChange) {
		var wrap = E('div', { 'class': 'at-autorefresh' });
		var enabled = true;
		var interval = 5;
		var chk = document.createElement('input');
		chk.type = 'checkbox';
		chk.checked = true;
		var label = E('label', { 'class': 'at-autorefresh-label' }, '自动刷新 ');
		label.insertBefore(chk, label.firstChild);
		var sel = document.createElement('select');
		sel.className = 'cbi-input-select';
		sel.style.width = '90px';
		[3, 5, 10, 15, 30, 60].forEach(function (s) {
			var opt = document.createElement('option');
			opt.value = String(s);
			opt.textContent = s + ' 秒';
			if (s === interval) opt.selected = true;
			sel.appendChild(opt);
		});
		sel.addEventListener('change', function () {
			interval = parseInt(sel.value, 10);
			if (onChange) onChange(enabled, interval);
		});
		chk.addEventListener('change', function () {
			enabled = chk.checked;
			if (onChange) onChange(enabled, interval);
		});
		label.appendChild(sel);
		wrap.appendChild(label);
		return {
			el: wrap,
			setEnabled: function (v) { enabled = v; chk.checked = v; },
			setInterval: function (s) { interval = s; sel.value = String(s); },
			getInterval: function () { return interval; },
			isEnabled: function () { return enabled; }
		};
	};

	/* ---------- 定时器/订阅生命周期（切页自动清理） ---------- */

	var _hooks = [];

	function _installHashHook() {
		if (_installHashHook._done) return;
		_installHashHook._done = true;
		window.addEventListener('hashchange', function () {
			/*
			 * 同步快照，延后执行。
			 *
			 * 快照必须在本 tick 内取：此刻路由派发刚起步、目标视图尚未 load/render，
			 * _hooks 里只可能是离场视图的钩子。若连快照一起推迟到回调里，凡是
			 * load() 能纯微任务走完的视图（无网络边界，例如命中本地缓存）会抢先
			 * 注册完毕，新页的钩子就会被 old 一并捕获并立刻执行 —— 后果是刚进页面
			 * 就把本页的轮询/监听摘掉，以及误调 closeSession() 之类的离场清理。
			 *
			 * 执行仍延后一拍，保留「等路由派发完成再清理」的原有意图：
			 * 清理回调里常要摘 document/window 监听器、释放会话租约，晚一点跑可以
			 * 避开与 LuCI 自身 DOM 更新的竞争。
			 */
			var leaving = _hooks;
			_hooks = [];
			setTimeout(function () {
				leaving.forEach(function (fn) { try { fn(); } catch (e) { /* ignore */ } });
			}, 0);
		});
	}

	/**
	 * set interval，路由切换（hashchange）时自动 clear。
	 * 返回定时器 id。
	 */
	api.interval = function (ms, fn) {
		_installHashHook();
		var id = setInterval(fn, ms);
		_hooks.push(function () { clearInterval(id); });
		return id;
	};

	/**
	 * 注册「离开本页」回调：路由切换（hashchange）时执行。
	 * LuCI view 没有 dispose 钩子；用于退订推送、释放会话/租约、
	 * 摘除挂在 document/window 上的监听器等 interval 覆盖不到的清理。
	 */
	api.onLeave = function (fn) {
		_installHashHook();
		_hooks.push(fn);
	};


	return api;
})();

var UiClass = L.Class.extend(Ui);
if (typeof window !== 'undefined') {
	window.Ui = new UiClass();
}
return UiClass;
