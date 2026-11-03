'use strict';
/*
 * LuCI core 兼容垫片。
 *
 * 背景：
 *   luci.js 的 bootstrap 自身依赖 String.prototype.format，例如
 *
 *     // luci.js: raise()
 *     const msg = fmt ? String.prototype.format.call(fmt, ...args) : null;
 *     // luci.js: require()
 *     url = '%s/%s.js%s'.format(env.base_url, name.replace(/\./g,'/'), ...);
 *
 *   而 luci.js 的启动流程是
 *
 *     Promise.all([domReady, this.require('ui'), this.require('rpc'), this.require('form'), ...])
 *         .then(this.setupDOM.bind(this)).catch(this.error)
 *
 *   其中 this.require('ui') 在 DOMContentLoaded 之前就会执行，也就早于
 *   主题脚本注入 format 定义的时机。在部分 LuCI master 构建里，format 的
 *   定义被拆分/摇树优化掉后（本机 luci.js 全文只出现一次 prototype.format，
 *   且位于调用点内部），第一个 this.require() 就会抛
 *
 *     TypeError: "%s/%s.js%s".format is not a function
 *
 *   异常被 Promise.all 的 .catch(this.error) 吞掉 → setupDOM() 永不执行 →
 *   所有视图的 load()/render() 都不运行 → 页面只显示 HTML 骨架（含表单容器），
 *   表现为「页面能打开但功能全不可用」，例如 AT 调试终端没有任何交互。
 *
 * 处理：
 *   1) 本模块由 rpc.js（所有视图的共同依赖）最先 require，其顶层副作用在
 *      luci.js 完成启动前就补齐 format 定义；
 *   2) 与 luci 自带实现保持一致的语义：
 *        - %s 参数替换，%d 数字替换，%j JSON 序列化
 *        - %% 转义为字面 %
 *        - 参数不足时保留占位符，参数多余时追加到末尾
 *   3) 仅当 String.prototype.format 尚未定义时才写入，不覆盖上游实现。
 *
 *   4) 给 LuCI 资源加载器补插件版本参数（v1.27.0 起）：
 *      luci.js 的 require() 只有在 env.resource_version 非空时才给模块 URL
 *      追加 ?v= 缓存参数，而 resource_version 取自主题模板输出的
 *      `luci.js?v=` 标签 —— 部分构建下为空，且即使非空也只随 luci-base
 *      变化，插件升级时不变，浏览器会继续命中旧 view 模块缓存
 *      （luci.js 对 require 一律 `Request.get(url, { cache: true })`）。
 *      本模块是所有插件页面的最深公共依赖、先于任何视图工厂求值，
 *      在这里把插件版本并入 resource_version：
 *        - 此后的每次模块加载（含 SPA 内跳转到的其它 view 模块、
 *          fm350/* 公共模块）URL 都带上 ?v=，插件发版 bump
 *          MT5700_UI_VERSION 即整体失效；
 *        - 已有值（主题/luci-base 给的）做追加而不是覆盖，luci-base
 *          自身的缓存失效语义保持不变。
 *      已知边界：文档首次加载时，入口页的 view 模块取回发生在本模块
 *      求值之前，这一条覆盖不到 —— 升级后对入口页硬刷新一次（Ctrl+F5）
 *      仍是必要兜底。
 *
 * 返回 Class 子类以满足 LuCI 对模块返回值的约定。
 */

'require baseclass';

/* 插件 UI 整体版本：与 Makefile 的 PKG_VERSION 同步 bump，
 * 作为 resource_version 的插件分量驱动所有 JS 模块的缓存失效。 */
var MT5700_UI_VERSION = '1.30.11';

var AtCompat = (function () {
	var api = {};

	api.install = function () {
		if (typeof String.prototype.format === 'function') return false;

		String.prototype.format = function () {
			var args = arguments;
			var index = 0;
			var str = String(this);

			var out = str.replace(/%(?:%|s|d|j)/g, function (m) {
				if (m === '%%') return '%';
				if (index >= args.length) return m;
				var v = args[index++];
				if (v == null) return '';
				if (m === '%d') {
					var n = parseInt(v, 10);
					return isNaN(n) ? String(v) : String(n);
				}
				if (m === '%j') {
					try { return JSON.stringify(v); } catch (e) { return String(v); }
				}
				return String(v);
			});

			// 多余实参按 luci 惯例追加（便于调试时看到全部上下文）
			for (; index < args.length; index++) {
				out += ' ' + args[index];
			}
			return out;
		};
		return true;
	};



	// 模块求值即安装，早于任何视图 load/render。
	api.installed = api.install();

	/* 给 resource_version 并入插件版本（见文件头第 4 点）。
	 * luci.js 的 require() 每次构造 URL 时都会读 env.resource_version，
	 * 这里改一次，此后所有模块加载（view/* 与 fm350/*）都生效。
	 * 仅追加不覆盖：主题或 luci-base 已给的版本分量原样保留在前。 */
	api.resourceVersionInstalled = (function () {
		try {
			if (typeof L === 'undefined' || !L.env || typeof L.env !== 'object') return false;
			var tag = 'mt5700-' + MT5700_UI_VERSION;
			var cur = L.env.resource_version;
			if (cur && typeof cur === 'string') {
				if (cur.indexOf(tag) !== -1) return false; /* 已并入过（如 SPA 内二次求值） */
				L.env.resource_version = cur + '.' + tag;
			} else {
				L.env.resource_version = tag;
			}
			return true;
		} catch (e) {
			/* 老版本 luci.js 未暴露 env 或结构不符时静默跳过，不影响页面功能 */
			return false;
		}
	})();

	return api;
})();

var AtCompatClass = L.Class.extend(AtCompat);
if (typeof window !== 'undefined') {
	window.AtCompat = new AtCompatClass();
}
return AtCompatClass;
