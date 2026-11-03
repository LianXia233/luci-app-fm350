'use strict';
'require fm350/rpc';
'require fm350/parse';
'require fm350/ui';
'require fm350/mt5700';
/* global L, AtWs, Parse, Ui, Mt5700 */

/**
 * 短信中心页面。
 *
 * 本次改造两块内容：
 *
 * 一、界面重构为 IM 会话形态（视觉参照 sms_im_demo，沿用其蓝紫色系）
 *     左会话列表（头像 + 号码 + 预览 + 时间），右聊天气泡；
 *     收发双方**默认展示头像**：对方用粉色系、本机用薰衣草系。
 *     窄屏（<=820px）收成单栏，列表与会话二选一 —— 演示稿在窄屏直接隐藏
 *     侧栏，手机上进了会话就切不回去，这里补了返回按钮做切换。
 *
 * 二、发送/接收记录写入路由器本地存储
 *     落盘由后端 `sms_store` 负责（加密存放在 /etc/fm350/SMS）。
 *     本页承担两件事：
 *       - 删除短信时提供「同时删除本地记录文件」选项，删完同步刷新界面；
 *       - 展示归档占用（会话数 / 体积 / 上限），支持按会话查看与清理。
 *
 * 沿用原有三条硬约束：
 *   1. 短信优先：进页面即申请前台短信会话，离开/隐藏时释放；
 *   2. 无缓存：列表、容量、发送/删除结果全部来自本次真实下发；
 *   3. 不臆造结果：unknown（超时/无响应）明确提示可能已发走，不自动重发。
 */

/* 前台短信模式的心跳间隔：必须明显小于后端租约（90s），否则会被判为已离开 */
/*
 * 会话租约 TTL 与心跳（issue #23）。
 *
 * 原值 TTL 90s / 心跳 30s：页面崩溃或忘记释放时，其它页面要等租约自然
 * 过期（最长 90s）才能恢复，叠加旧版「一刀切等 30s」就是整页卡死。
 * 现与后端 SMS_MODE_LEASE 对齐为 TTL 30s / 心跳 10s —— 续租粒度更细，
 * 正常使用无感，异常场景最长阻塞 90s → 30s。
 */
var SMS_SESSION_TTL_MS = 30000;
var SMS_SESSION_HEARTBEAT_MS = 10000;

/*
 * 批量删除每批的槽位数。
 *
 * 后端是一句 `AT+CMGD=<槽位>` 删一条（模组不收槽位列表，实机同理），一次 RPC 的
 * 耗时与槽位数成正比；而 ucode 侧的 `nc` 超时是写死的，勾满 40 条就是 80s 以上，
 * 必然被掐断 —— 掐断不止是这次删除失败，还会把「后端不可用」的共享熔断打开 15s，
 * 别的页面一起跟着报错。按 8 个槽位一批（每批 ≤ 排队 10 + 8×2 + 回读 2 = 28s）
 * 顺序发，既对得上超时预算，也让中途失败时已经删掉的不必重来。
 */
var SMS_DELETE_CHUNK = 8;

/*
 * 会话头像：收发双方各一张（与演示稿一致）。
 * 存在资源目录下随包安装，架构无关。
 */
var SMS_AVATAR_RECV = '/luci-static/resources/fm350/avatar-recv.png';
var SMS_AVATAR_SENT = '/luci-static/resources/fm350/avatar-sent.png';

/* ================= DOM 小工具（模块级，供 render 内使用） ================= */

function smsEl(tag, attrs, children) {
	return E(tag, attrs || {}, children);
}

/* 头像：收到（对方）用粉色系，发出（本机）用薰衣草系 */
function smsAvatar(isSent, sizeCls) {
	return smsEl('img', {
		'class': 'mt5700-sms-avatar ' + (sizeCls || 'mt5700-sms-avatar-sm'),
		'src': isSent ? SMS_AVATAR_SENT : SMS_AVATAR_RECV,
		'alt': ''
	});
}

/* 字节数人话化统一走 AtWs.formatFlow（rpc.js 单一来源），本页不再自留一份 */

return L.view.extend({
	render: function () {
		var self = this;
		var page = Mt5700.page('短信中心', '即时短信收发、会话列表、长短信拼接与本地归档', 'sms_center', '即时消息 · 会话管理');
		page.classList.add('mt5700-sms-shell');
		/*
		 * 手机端把顶部 hero 标记出来压成一行。
		 * 聊天页最缺的是竖向空间：桌面上那块大标题占掉近三分之一屏，
		 * 结果输入框被顶到折叠线以下，看起来像「发不出去」。
		 */
		if (page._hero) {
			page._hero.classList.add('mt5700-sms-hero-compact');
		}
		var body = page._body;

		var state = {
			contacts: [],        /* [{ number, lastMessage, lastTime, messages: [] }] */
			selectedContact: '',
			messages: [],        /* 当前联系人消息 */
			storage: { used: 0, total: 0 },
			/* 本地归档统计：{ totalBytes, maxBytes, conversations: [...] } */
			archive: { totalBytes: 0, maxBytes: 0, conversations: [] },
			/* 会话搜索关键字 */
			keyword: ''
		};

		/*
		 * 本地归档消息缓存：号码 -> 该会话的归档记录（已转成界面消息）。
		 *
		 * 归档是**按会话独立文件**存的，只能一个号码一次 RPC，所以只在选中会话时
		 * 才读，不给每个会话都拉一遍。任何写操作（发送 / 删除 / 清空归档）之后
		 * 必须 invalidateArchive()，否则界面会拿着过期的归档继续渲染。
		 */
		var archiveMsgs = {};

		function invalidateArchive(number) {
			if (number) {
				delete archiveMsgs[normalizeNumber(number)];
			} else {
				archiveMsgs = {};
			}
		}

		/* ================= 布局 ================= */

		var layout = smsEl('div', { 'class': 'mt5700-sms-layout' });
		body.appendChild(layout);

		/* ---------- 左：会话列表 ---------- */

		var side = smsEl('div', { 'class': 'mt5700-sms-side' });
		layout.appendChild(side);

		var storageEl = smsEl('span', { 'class': 'mt5700-sms-storage' }, '存储：—');
		side.appendChild(smsEl('div', { 'class': 'mt5700-sms-side-head' }, [
			smsEl('div', { 'class': 'mt5700-sms-mark' }, '✦'),
			smsEl('span', {}, '短信'),
			storageEl
		]));

		/*
		 * 会话搜索：只过滤左侧列表，不改动后端请求 —— 列表仍是本次从模组读回的
		 * 全量数据，关键字仅用于界面筛选（与「无缓存」约束一致）。
		 */
		var searchInput = smsEl('input', {
			type: 'text',
			placeholder: '搜索号码或内容',
			'aria-label': '搜索会话'
		});
		searchInput.addEventListener('input', function () {
			state.keyword = String(searchInput.value || '').trim();
			renderContacts();
		});
		side.appendChild(smsEl('div', { 'class': 'mt5700-sms-search' }, [
			smsEl('span', { 'aria-hidden': 'true' }, '⌕'),
			searchInput
		]));

		var currentTab = 'all'; /* all | unread | sent */
		var filterBar = smsEl('div', { 'class': 'mt5700-sms-filters' });
		[
			{ id: 'all', title: '全部' },
			{ id: 'unread', title: '未读' },
			{ id: 'sent', title: '已发' }
		].forEach(function (td) {
			var tb = smsEl('button', {
				type: 'button',
				'class': 'mt5700-sms-filter' + (td.id === 'all' ? ' active' : '')
			}, td.title);
			tb.addEventListener('click', function () {
				currentTab = td.id;
				filterBar.querySelectorAll('.mt5700-sms-filter').forEach(function (b) {
					b.classList.remove('active');
				});
				tb.classList.add('active');
				renderContacts();
			});
			filterBar.appendChild(tb);
		});
		/* 必须先挂载：只填充不 append 的话，三个筛选按钮根本不会出现在页面上 */
		side.appendChild(filterBar);

		var convList = smsEl('div', { 'class': 'mt5700-sms-convs' });
		side.appendChild(convList);

		var sideFoot = smsEl('div', { 'class': 'mt5700-sms-side-foot' });

		var newBtn = Mt5700.primaryButton('+ 新短信', function () {
			var promptMask = Ui.promptModal('新短信', [
				{ key: 'number', label: '收件人号码', placeholder: '请输入 5-19 位手机号码' }
			], function (values) {
				var num = (values.number || '').trim();
				if (!num) {
					Mt5700.warning('请输入联系人号码');
					return;
				}
				if (!Parse.isValidPhoneNumber(num)) {
					Mt5700.warning('请输入正确的 5-19 位手机号码');
					return;
				}
				selectContact(num);
			});
			/* 仅用于移动端把「新短信」弹窗排成与其余短信弹窗一致的底部面板 */
			if (promptMask && promptMask.classList) {
				promptMask.classList.add('mt5700-sms-prompt');
			}
		});
		sideFoot.appendChild(newBtn);

		/*
		 * 归档概览：显示占用与上限，并可点开按会话查看/清理。
		 * 这里只呈现统计数字，真正的加密读写在后端完成。
		 */
		var archiveInfo = smsEl('span', {}, '本地归档：—');
		var archiveRow = smsEl('div', { 'class': 'mt5700-sms-foot-row' }, [
			archiveInfo,
			Mt5700.button('管理归档', function () { openArchiveModal(); }, 'ghost')
		]);
		sideFoot.appendChild(archiveRow);
		side.appendChild(sideFoot);

		/* ---------- 右：聊天区 ---------- */

		var chat = smsEl('div', { 'class': 'mt5700-sms-chat' });
		layout.appendChild(chat);

		var backBtn = smsEl('button', {
			type: 'button',
			'class': 'mt5700-sms-back',
			title: '返回会话列表',
			'aria-label': '返回会话列表'
		}, '‹');
		backBtn.addEventListener('click', function () { showChat(false); });

		var headAvatar = smsAvatar(false, 'mt5700-sms-avatar-md');
		var chatTitle = smsEl('h2', {}, '请选择联系人');
		var chatChannel = smsEl('span', { 'class': 'mt5700-sms-channel' }, [
			smsEl('span', { 'class': 'mt5700-sms-dot online' }),
			smsEl('span', {}, '短信')
		]);
		/*
		 * 通道状态徽章（issue #23 改动点 6）：优先模式生效期间告诉用户
		 * 「后台暂停、剩余 Ns」，其它页面的数据转圈时有明确原因可看。
		 * 由 statusTicker 每秒驱动；续租成功会跳回 TTL——租约确实被延长，
		 * 这样显示是诚实的，不是做假的平滑倒数。
		 */
		var channelStatus = smsEl('span', { 'class': 'mt5700-sms-channel-status' }, '');
		chatChannel.appendChild(channelStatus);
		var lastRenewAt = 0;
		function refreshChannelStatus() {
			if (!sessionId) {
				channelStatus.textContent = '';
				return;
			}
			var left = Math.max(0, Math.ceil((SMS_SESSION_TTL_MS - (Date.now() - lastRenewAt)) / 1000));
			channelStatus.textContent = '优先模式生效中 · 后台暂停（剩余 ' + left + 's）';
		}

		var chatHead = smsEl('div', { 'class': 'mt5700-sms-chat-head' }, [
			backBtn,
			headAvatar,
			smsEl('div', { 'class': 'mt5700-sms-chat-copy' }, [chatTitle, chatChannel]),
			smsEl('div', { 'class': 'mt5700-sms-chat-actions' }, [
				Mt5700.ghostButton('刷新', function () { refreshAll(); }),
				Mt5700.dangerButton('批量删除', function () { batchDelete(); })
			])
		]);
		chat.appendChild(chatHead);

		var msgArea = smsEl('div', { 'class': 'mt5700-sms-msgs' });
		chat.appendChild(msgArea);

		var msgInput = smsEl('textarea', {
			rows: '1',
			placeholder: '输入短信内容…',
			'aria-label': '短信内容'
		});
		var sendBtn = Mt5700.primaryButton('发送', function () { composer.requestSubmit(); });
		/* 供短信页专用样式命中（蓝底、手机端固定宽度） */
		sendBtn.classList.add('mt5700-sms-send');
		var composer = smsEl('form', { 'class': 'mt5700-sms-composer' }, [msgInput, sendBtn]);
		composer.addEventListener('submit', function (e) {
			e.preventDefault();
			send();
		});
		msgInput.addEventListener('input', function () {
			autoGrow();
			renderHint();
		});
		msgInput.addEventListener('keydown', function (e) {
			if (e.key === 'Enter' && !e.shiftKey) {
				e.preventDefault();
				composer.requestSubmit();
			}
		});
		chat.appendChild(composer);

		var hintEl = smsEl('span', {}, '');
		chat.appendChild(smsEl('div', { 'class': 'mt5700-sms-hint' }, hintEl));

		/* ================= 渲染 ================= */

		function normalizeNumber(n) {
			return Parse.normalizePhoneNumber(n);
		}

		function autoGrow() {
			msgInput.style.height = 'auto';
			msgInput.style.height = Math.min(msgInput.scrollHeight, 110) + 'px';
		}

		function renderStorage() {
			storageEl.textContent = '存储：' + state.storage.used + ' / ' + state.storage.total;
		}

		function renderArchive() {
			var a = state.archive || {};
			var list = a.conversations || [];
			if (!list.length) {
				archiveInfo.textContent = '本地归档：暂无记录';
				return;
			}
			archiveInfo.textContent = '本地归档：' + list.length + ' 个会话 · '
				+ AtWs.formatFlow(a.totalBytes) + ' / '
				+ (a.maxBytes ? AtWs.formatFlow(a.maxBytes) : '—');
		}

		/*
		 * 长短信合并：把同一号码、同一拼接参考号的各段拼成一条。
		 *
		 * 分段信息来自后端解析的 UDH（reference / 总段数 / 当前段号），
		 * 不再依赖任何本地缓存：缺段就是缺段，界面如实标注「已合并 x/y」。
		 */
		function mergeConcatenated(list) {
			var groups = {};
			var result = [];
			var i;
			for (i = 0; i < list.length; i++) {
				var m = list[i];
				if (m && m.isConcatenated && m.concatenatedRef != null
					&& m.concatenatedSeq != null) {
					var key = (m.number || '') + '|' + m.concatenatedRef;
					if (!groups[key]) {
						groups[key] = [];
					}
					groups[key].push(m);
				} else {
					result.push(m);
				}
			}
			for (var k in groups) {
				var parts = groups[k];
				parts.sort(function (a, b) {
					return (a.concatenatedSeq || 0) - (b.concatenatedSeq || 0);
				});
				var full = '';
				for (i = 0; i < parts.length; i++) {
					full += (parts[i].content || '');
				}
				var first = parts[0];
				var last = parts[parts.length - 1];
				var idxs = [];
				for (i = 0; i < parts.length; i++) {
					if (parts[i].index != null) {
						idxs.push(parts[i].index);
					}
				}
				result.push({
					index: first.index,
					partIndices: idxs,
					content: full,
					number: first.number,
					time: last.time,
					timeApprox: last.timeApprox,
					type: first.type,
					isConcatenated: false,
					concatenatedRef: first.concatenatedRef,
					partsCount: parts.length,
					partsExpected: first.concatenatedTotal
				});
			}
			return result;
		}

		/*
		 * 按当前 Tab 过滤会话。
		 *
		 * 「未读」不能用模组给的标志位：手册 9.8.2 写明 `AT+CMGL` 一旦把某条
		 * 「收到的未读短信」列出来，存储里的状态就同时变成「收到的已读」——
		 * 我们每次刷新列表都是 CMGL=4，等结果回到界面时 stat 已经是 1 了。
		 * （新短信的 +CMTI → AT+CMGR 也一样，9.10.2 同样会翻成已读。）
		 * 所以未读只能按「最新一条是收到的、且本次页面会话里还没点开过」估，
		 * 刷新后重置，不冒充持久未读状态。
		 */
		function latestMsg(c) {
			return c.messages && c.messages.length ? c.messages[c.messages.length - 1] : null;
		}

		function matchTab(c) {
			var m;
			if (currentTab === 'all') {
				return true;
			}
			m = latestMsg(c);
			if (!m) {
				return false;
			}
			if (currentTab === 'sent') {
				return m.type === 'sent';
			}
			if (currentTab === 'unread') {
				return m.type === 'received' && c.number !== state.selectedContact;
			}
			return true;
		}

		/* 搜索：号码或最近一条内容命中即保留 */
		function matchKeyword(c) {
			if (!state.keyword) {
				return true;
			}
			var kw = state.keyword.toLowerCase();
			if (String(c.number || '').toLowerCase().indexOf(kw) >= 0) {
				return true;
			}
			return String(c.lastMessage || '').toLowerCase().indexOf(kw) >= 0;
		}

		/* 会话列表右上角的时刻：只要 HH:MM */
		function shortTime(t) {
			if (!t) {
				return '';
			}
			var part = String(t).split(',')[1] || String(t);
			return String(part).slice(0, 5);
		}

		function renderContacts() {
			convList.innerHTML = '';
			var shown = (state.contacts || []).filter(function (c) {
				return matchTab(c) && matchKeyword(c);
			});
			if (!shown.length) {
				convList.appendChild(Mt5700.empty(
					state.contacts.length ? '当前筛选下没有会话' : '暂无短信'));
				return;
			}
			shown.forEach(function (c) {
				var item = smsEl('button', {
					type: 'button',
					'class': 'mt5700-sms-conv'
						+ (c.number === state.selectedContact ? ' active' : '')
				});
				item.appendChild(smsAvatar(false, 'mt5700-sms-avatar-lg'));

				var copy = smsEl('span', { 'class': 'mt5700-sms-conv-copy' });
				copy.appendChild(smsEl('span', { 'class': 'mt5700-sms-conv-name' },
					c.number || '未知号码'));
				copy.appendChild(smsEl('span', { 'class': 'mt5700-sms-conv-preview' },
					c.lastMessage || ''));
				item.appendChild(copy);

				item.appendChild(smsEl('time', { 'class': 'mt5700-sms-conv-time' },
					shortTime(c.lastTime)));

				item.addEventListener('click', function () { selectContact(c.number); });
				convList.appendChild(item);
			});
		}

		/*
		 * 时刻文案：日期已由分隔条给出，气泡内只留时刻（会话视图与归档弹窗共用）。
		 * 时间未知时如实显示「时间未知」：空白会让人以为渲染丢了，直接不显示
		 * 又会让人默认它是刚刚。近似时间（来自 TP-VP 有效期或本机时钟）额外标注，
		 * 避免被当成精确发送时刻。
		 */
		function smsTimeText(time, timeApprox) {
			var t = time ? (String(time).split(',')[1] || time) : '时间未知';
			if (time && timeApprox) {
				t += '（近似）';
			}
			return t;
		}

		/* 气泡行骨架（两处渲染路径共用）：row + 气泡按收发方向就位，对方在左、自己在右 */
		function buildSmsRow(isSent, bubble) {
			var row = smsEl('div', {
				'class': 'mt5700-sms-row ' + (isSent ? 'outgoing' : 'incoming')
			});
			if (isSent) {
				row.appendChild(bubble);
				row.appendChild(smsAvatar(true, 'mt5700-sms-avatar-sm'));
			} else {
				row.appendChild(smsAvatar(false, 'mt5700-sms-avatar-sm'));
				row.appendChild(bubble);
			}
			return row;
		}

		/*
		 * 单条消息气泡（含时间、长短信缺失提示与删除按钮）。
		 *
		 * 结构照演示稿：头像与气泡并排，收到时头像在左、发出时头像在右。
		 */
		function buildBubble(m) {
			var isSent = m.type === 'sent';
			var bubble = smsEl('div', { 'class': 'mt5700-sms-bubble' });

			bubble.appendChild(smsEl('div', { 'class': 'mt5700-sms-text' },
				m.content || '(空消息)'));

			/* 时刻与删除按钮收进同一行：否则「删除」会独占一行，
			 * 每个气泡底部都多出一块空白，手机上尤其明显。 */
			var meta = smsEl('div', { 'class': 'mt5700-sms-meta' });
			meta.appendChild(smsEl('span', { 'class': 'mt5700-sms-time' },
				smsTimeText(m.time, m.timeApprox)));
			var del = Mt5700.button('删除', function () { deleteMessage(m); }, 'danger');
			del.className = 'mt5700-sms-del';
			meta.appendChild(del);
			bubble.appendChild(meta);

			if (m.partsExpected != null && m.partsCount != null
				&& m.partsCount < m.partsExpected) {
				bubble.appendChild(smsEl('div', { 'class': 'mt5700-sms-note' },
					'长短信已合并 ' + m.partsCount + '/' + m.partsExpected + ' 段（部分缺失）'));
			}
			if (m.error) {
				bubble.appendChild(smsEl('div', { 'class': 'mt5700-sms-note' },
					'该条解析失败：' + m.error));
			}

			return buildSmsRow(isSent, bubble);
		}

		/*
		 * 日期分隔条文案（`YY/MM/DD,HH:MM:SS` → `20YY-MM-DD`）。
		 *
		 * 无时刻的消息若借到了锚点位置，日期就用锚点那条的日期 —— 否则时间轴中间
		 * 会突然冒出一条「时间未知的消息」分隔，把一次来回劈成两半。真的连锚点都
		 * 没有（它排在最前）时，才单列一档如实说明。
		 */
		function dayLabel(m) {
			var timeStr = m.time || m._anchorTime || null;
			if (!timeStr) {
				return '时间未知的消息';
			}
			var day = String(timeStr).split(',')[0];
			var seg = day.split('/');
			if (seg.length === 3) {
				return '20' + seg[0] + '-' + seg[1] + '-' + seg[2];
			}
			return day;
		}

		/*
		 * 聊天式单条时间轴：收发消息按时间顺序**混排在同一条轴上**，
		 * 靠气泡左右位置区分方向 —— 对方在左、自己在右，跨天处插一条日期分隔条。
		 */
		/* ================= 本地归档 → 消息合并 ================= */

		/*
		 * 读一个会话的本地归档。
		 *
		 * 为什么必须做这一步：3GPP 的 SMS-SUBMIT 提交成功后模组**不会**把它写进
		 * SM/ME 槽位，所以 AT+CMGL 读回来的列表里永远没有「自己发出去的那条」。
		 * 不合并归档的话，IM 界面就只剩对方的话，自己那句发完即从界面消失。
		 */
		function loadArchived(number) {
			var key = normalizeNumber(number || '');
			if (!key) {
				return Promise.resolve([]);
			}
			if (archiveMsgs[key]) {
				return Promise.resolve(archiveMsgs[key]);
			}
			return AtWs.client.sms('store_read', { number: key }).then(function (res) {
				if (!res.success) {
					return [];
				}
				var r = res.result || {};
				var list = (r.entries || []).map(function (e) {
					var m = toUiMessage(e);
					/* 标记来源：删除时据此判断「只能删本地」还是「模组 + 本地」 */
					m.archived = true;
					return m;
				});
				archiveMsgs[key] = list;
				return list;
			}).catch(function () {
				return [];
			});
		}

		/*
		 * 合并模组消息与归档消息。
		 *
		 * 收到的短信两边都有（模组槽位 + 归档），必须去重，否则每条都出现两次。
		 * 去重键用「方向 | 正文 | 时刻」：收到的时刻来自 TP-SCTS，两边完全一致。
		 *
		 * 发出的短信还要多做一件事：**时刻一律以归档为准**。归档那条是插件在
		 * 发送成功当场用 nettime（NTP 优先、取不到才退设备时钟）盖的章；
		 * 模组里若存了已发条目，它能给的只有 PDU 的 TP-VP 有效期 —— 那是
		 * 「这条短信几时作废」，不是「几时发出去」，最长能差一周，拿来当发送
		 * 时刻排障会把时间线整个带偏。所以这里把模组条目的时刻换成归档时刻，
		 * 同时**保留模组条目的 index**：换的是显示用的时间，不是删除要用的槽位。
		 *
		 * 同一正文发过两次时两条归档无法区分，宁可不换（继续显示「近似」），
		 * 也不随便挑一个时刻冒充精确时间。
		 */
		function archiveKeyOf(m) {
			return (m.type || '') + '|' + (m.content || '') + '|' + (m.time || '');
		}

		/* 正文 → 唯一的发出归档记录；正文重复出现的直接剔除，不参与换时刻 */
		function sentTimeAnchor(extra) {
			var seenContent = {};
			var anchor = {};
			(extra || []).forEach(function (m) {
				if (m.type !== 'sent' || !m.time) {
					return;
				}
				var key = m.content || '';
				if (seenContent[key]) {
					anchor[key] = null;      /* 发过不止一次，无法判定是哪一次 */
					return;
				}
				seenContent[key] = true;
				anchor[key] = m;
			});
			return anchor;
		}

		function mergeArchived(base, extra) {
			if (!extra || !extra.length) {
				return base;
			}
			var anchor = sentTimeAnchor(extra);
			var seen = {};
			var out = [];
			var i;
			for (i = 0; i < base.length; i++) {
				var m = base[i];
				if (m.type === 'sent' && (m.timeApprox || !m.time)) {
					var a = anchor[m.content || ''];
					if (a) {
						m.time = a.time;
						m.timeApprox = a.timeApprox;
					}
				}
				seen[archiveKeyOf(m)] = true;
				out.push(m);
			}
			for (i = 0; i < extra.length; i++) {
				var k = archiveKeyOf(extra[i]);
				if (seen[k]) {
					continue;
				}
				seen[k] = true;
				out.push(extra[i]);
			}
			return out;
		}

		/* 当前会话的完整消息（模组 + 归档） */
		function currentMessages() {
			return mergeArchived(state.messages,
				archiveMsgs[normalizeNumber(state.selectedContact || '')] || []);
		}

		function renderConversation() {
			chatTitle.textContent = state.selectedContact
				? state.selectedContact : '请选择联系人';
			msgArea.innerHTML = '';
			var msgs = currentMessages();
			if (!msgs.length) {
				msgArea.appendChild(Mt5700.empty(
					state.selectedContact ? '暂无消息，输入内容发送'
						: '请选择左侧会话或新建短信'));
				return;
			}
			var ordered = conversationOrder(msgs);
			var lastDay = null;
			ordered.sorted.forEach(function (m) {
				var day = dayLabel(m);
				if (day !== lastDay) {
					msgArea.appendChild(smsEl('div', { 'class': 'mt5700-sms-date' },
						[smsEl('span', {}, day)]));
					lastDay = day;
				}
				msgArea.appendChild(buildBubble(m));
			});
			msgArea.scrollTop = msgArea.scrollHeight;
		}

		function renderHint() {
			var text = msgInput.value.trim();
			if (!text) {
				hintEl.textContent = '';
				return;
			}
			/*
			 * 字数提示是**本地估算**，权威永远是后端：编码与分片在
			 * `pdu::build_submit_parts` 里决定，这里只求「大概拆成几条」符合直觉，
			 * 不让两处算出不同的片数。
			 *
			 * 上限与后端同源：单条 160 septet / 70 UCS2 字，长短信扣掉 6 字节 UDH
			 * 后 153 septet / 67 UCS2 字（6×8/7 向上取整占 7 个 septet）。
			 *
			 * 判定 UCS2 用的字符表覆盖 GSM 7bit 默认表 + 扩展表：0x20–0x7E 里
			 * **只有反引号 ` (0x60)** 两张表都没有，后端会因此转 UCS2，所以单独滤掉。
			 */
			var ucs2 = text.indexOf('`') >= 0
				|| !/^[\x20-\x7E\r\n@£$¥èéùìòÇØøÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ¡ÄÖÑÜ§¿äöñüà]*$/.test(text);
			var per = ucs2 ? 67 : 153;
			var chars = Array.from(text).length;
			var parts = chars <= (ucs2 ? 70 : 160) ? 1 : Math.ceil(chars / per);
			hintEl.textContent = chars + ' 字 · '
				+ (ucs2 ? '需按 UCS2 编码' : '可按 GSM 7bit 编码')
				+ (parts > 1 ? ' · 预计拆成 ' + parts + ' 条' : '');
		}

		/* ================= 数据：全部实时，无缓存 ================= */

		function refreshStorage() {
			return AtWs.client.sms('capacity').then(function (res) {
				if (res.success && res.result && res.result.storage) {
					state.storage = {
						used: res.result.storage.used,
						total: res.result.storage.total
					};
					renderStorage();
				}
			}).catch(function () {});
		}

		function refreshArchiveStats() {
			return AtWs.client.sms('store_stats').then(function (res) {
				if (!res.success) {
					return;
				}
				var r = res.result || {};
				state.archive = {
					totalBytes: r.totalBytes || 0,
					maxBytes: r.maxBytes || 0,
					conversations: r.conversations || []
				};
				renderArchive();
			}).catch(function () {});
		}

		function refresh() {
			/*
			 * 归档统计排在列表之前：buildContacts 要靠它把「只发出去过、从没收到过」
			 * 的号码补进会话列表 —— 那种会话在模组里一条槽位都没有，光读 list 会整个消失。
			 */
			return refreshArchiveStats().then(function () {
				return AtWs.client.sms('list');
			}).then(function (res) {
				if (!res.success) {
					Mt5700.error(String(res.error || '获取短信列表失败'));
					return null;
				}
				var r = res.result || {};
				var entries = r.entries || [];
				/*
				 * 读得出应答行、却一条都解析不出来：后端把「一条都没有」和
				 * 「一条都解析不出」都归成 entries 为空（r.totalLines 是本次应答的
				 * 原始行数）。混为一谈的后果是 SIM 卡没就绪 / PDU 形态不认识时，
				 * 页面安静地显示「暂无短信」，用户以为短信被清空了 —— 一条提示
				 * 比一句谎话强。
				 */
				if (!entries.length && (r.totalLines || 0) > 0 && (r.storage && r.storage.total > 0))
					Mt5700.warning('模组有 ' + r.storage.total + ' 条短信，但本次一条都没解析出来'
						+ '（应答 ' + r.totalLines + ' 行）。可能是短信格式异常，请刷新或查看运行日志。');
				buildContacts(entries.map(toUiMessage));
				return null;
			}).catch(function () {
				Mt5700.error('获取短信列表失败');
			});
		}

		function refreshAll() {
			/* 列表重读意味着归档也可能变了，缓存一并作废 */
			invalidateArchive();
			return refresh().then(refreshStorage);
		}

		/* ================= 会话装配 ================= */

		/*
		 * 单元格排序键：模组存储的序号。数值越大表示越新。
		 */
		function sortIndex(m) {
			return m && typeof m.index === 'number' ? m.index : 0;
		}

		/*
		 * 会话内排序：按真实时刻把收发混排成一条时间轴（一人一句的顺序）。
		 *
		 * 这里踩过两个坑，都记在下面，别再改回去：
		 *
		 * 1. **不能拿消息对象当字典的键**。`keyOf[msg]` 会把所有对象都转成
		 *    字符串 `"[object Object]"`，于是每条消息共用同一个键，比较器恒等，
		 *    排序退化成按模组槽位号 —— 本地归档里的消息没有槽位号（null），
		 *    结果全被甩到会话最前面，「自己刚发的那句」跑到一年前的短信之前。
		 *    改用与数组同下标的平行数组存排序键。
		 *
		 * 2. 发出短信按 3GPP TS 23.040 不带发送时刻。模组读回来的老短信仍然按
		 *    存储顺序借锚点；而本地归档里的发出记录自带写入时刻（界面标「近似」），
		 *    它有真实的先后，直接用自己的时刻，不再借位。
		 */
		function conversationOrder(msgs) {
			var i;

			/* 先在模组存储内部按槽位号排出先后，沿这个顺序把锚点往后传 */
			var byStore = [];
			for (i = 0; i < msgs.length; i++) {
				if (msgs[i].index != null && msgs[i].index >= 0) {
					byStore.push(i);
				}
			}
			byStore.sort(function (a, b) {
				return sortIndex(msgs[a]) - sortIndex(msgs[b]);
			});

			var sortMs = [];
			var relaxed = [];
			var lastMs = null;
			var lastTime = null;
			for (i = 0; i < byStore.length; i++) {
				var j = byStore[i];
				var t = Parse.messageTimeMs(msgs[j].time);
				if (t !== null) {
					lastMs = t;
					lastTime = msgs[j].time;
				}
				sortMs[j] = lastMs;
				relaxed[j] = (t === null);
				msgs[j]._anchorTime = lastTime;
			}

			/* 不在模组列表里的（本地归档）：用自己的时刻 */
			for (i = 0; i < msgs.length; i++) {
				if (sortMs[i] !== undefined) {
					continue;
				}
				var own = Parse.messageTimeMs(msgs[i].time);
				sortMs[i] = own;
				relaxed[i] = false;
				msgs[i]._anchorTime = own !== null ? msgs[i].time : null;
			}

			var pos = [];
			for (i = 0; i < msgs.length; i++) {
				pos.push(i);
			}
			pos.sort(function (a, b) {
				var ka = sortMs[a];
				var kb = sortMs[b];
				if (ka !== kb) {
					if (ka === null) {
						return -1;      /* 连锚点都没有 → 排最前 */
					}
					if (kb === null) {
						return 1;
					}
					return ka - kb;
				}
				/* 同一时刻：有确切时刻的在前，借位置的紧随其后 */
				if (relaxed[a] !== relaxed[b]) {
					return relaxed[a] ? 1 : -1;
				}
				return sortIndex(msgs[a]) - sortIndex(msgs[b]);
			});

			var sorted = [];
			for (i = 0; i < pos.length; i++) {
				sorted.push(msgs[pos[i]]);
			}
			return { sorted: sorted };
		}

		function buildContacts(list) {
			list = mergeConcatenated(list);
			var map = {};
			var i;
			for (i = 0; i < list.length; i++) {
				var msg = list[i];
				var num = normalizeNumber(msg.number);
				/* 解析失败的条目号码为空，归入特殊分组以便显示错误信息 */
				var key = num || '__parse_error__';
				if (!map[key]) {
					map[key] = {
						number: num,
						lastMessage: msg.content,
						lastTime: msg.time,
						messages: []
					};
				}
				map[key].messages.push(msg);
			}
			var list2 = [];
			for (var key in map) {
				var c = map[key];
				c.messages = conversationOrder(c.messages).sorted;
				/*
				 * 联系人列表里的「最近一条」取**时间最新**的那条，而不是数组末尾那条
				 * —— 排序把时间未知的消息放在了末尾，直接取末尾会拿错。
				 */
				var newest = null;
				for (i = c.messages.length - 1; i >= 0; i--) {
					if (Parse.messageTimeMs(c.messages[i].time) !== null) {
						newest = c.messages[i];
						break;
					}
				}
				if (!newest && c.messages.length) {
					newest = c.messages[c.messages.length - 1];
				}
				/* 解析失败的条目显示错误信息而非内容 */
				c.lastMessage = newest
					? (newest.error ? '短信解析失败：' + newest.error : (newest.content || ''))
					: '';
				c.lastTime = newest ? (newest.time || '') : '';
				list2.push(c);
			}
			/*
			 * 本地归档里存在、但模组里一条槽位都没有的号码，补成「只有本地记录」
			 * 的会话。典型场景：给别人发了一条短信，对方还没回 —— 模组的 SM 槽位
			 * 里根本没有这条，光靠 list 会让这次联系在界面上完全不存在。
			 */
			var arch = (state.archive && state.archive.conversations) || [];
			for (i = 0; i < arch.length; i++) {
				var an = normalizeNumber(arch[i].number || '');
				if (!an || map[an] || an === '__parse_error__') {
					continue;
				}
				list2.push({
					number: an,
					lastMessage: arch[i].lastContent || '',
					lastTime: '',
					messages: [],
					archivedOnly: true
				});
			}
			/* 联系人按最近一次联系时间倒序；时间未知的沉底 */
			list2.sort(function (a, b) {
				var ta = Parse.messageTimeMs(a.lastTime);
				var tb = Parse.messageTimeMs(b.lastTime);
				if (ta !== null && tb !== null) {
					return tb - ta;
				}
				if (ta !== null) {
					return -1;
				}
				if (tb !== null) {
					return 1;
				}
				return 0;
			});
			state.contacts = list2;
			if (state.selectedContact) {
				var sel = null;
				for (i = 0; i < list2.length; i++) {
					if (normalizeNumber(list2[i].number) === normalizeNumber(state.selectedContact)) {
						sel = list2[i];
						break;
					}
				}
				state.messages = sel ? sel.messages : [];
			}
			renderContacts();
			renderConversation();
			/* 列表重读后归档缓存可能刚好被作废，这里补读一次，发出的气泡才不会闪没 */
			if (state.selectedContact) {
				loadArchived(state.selectedContact).then(function (list) {
					if (list.length) {
						renderConversation();
					}
				});
			}
		}

		function selectContact(num) {
			state.selectedContact = num;
			state.messages = [];
			for (var i = 0; i < state.contacts.length; i++) {
				if (normalizeNumber(state.contacts[i].number) === normalizeNumber(num)) {
					state.messages = state.contacts[i].messages;
					break;
				}
			}
			renderContacts();
			renderConversation();
			/* 窄屏下点进会话就切到聊天区（演示稿缺这一步，手机上切不回去） */
			showChat(true);
			/*
			 * 归档读完再补渲染一次：先出模组消息，界面不会因为一次文件读取而空白。
			 * 期间用户可能已经切到别的会话，回来时比对号码，别把 A 的归档画到 B 上。
			 */
			loadArchived(num).then(function () {
				if (normalizeNumber(state.selectedContact || '') === normalizeNumber(num)) {
					renderConversation();
				}
			});
		}

		/* ================= 发送 ================= */

		/*
		 * 短信发送。
		 *
		 * 与旧实现的两点本质差别：
		 *   1. PDU 编码、模式切换、短信中心号读取、分片提交全部由后端在**一次
		 *      事务**里完成，前端不再连发四五条各自排队的 AT 命令；
		 *   2. 结果按 accepted / rejected / unknown 三档处理，unknown 明确提示
		 *      「可能已发走」，前端绝不会自动重发（那才是重复计费的真凶）。
		 *
		 * 发送记录由后端写入本地归档（含时间与正文），前端不参与落盘。
		 */
		function send() {
			var content = msgInput.value.trim();
			if (!content) {
				Mt5700.warning('请输入短信内容');
				return;
			}
			var target = state.selectedContact || '';
			if (!target) {
				Mt5700.warning('请输入联系人号码');
				return;
			}
			if (!Parse.isValidPhoneNumber(target)) {
				Mt5700.warning('请输入正确的 5-19 位手机号码');
				return;
			}

			sendBtn.disabled = true;
			var label = sendBtn.textContent;
			sendBtn.textContent = '发送中…';

			AtWs.client.sms('send', {
				destination: target,
				message: content,
				/* 重复提交保护键：同一号码 + 同一正文在后端 10s 窗口内只接受一次，
				 * 覆盖浏览器重试、用户连点、按钮还没来得及置灰这几种情况 */
				dedup_key: target + '|' + content
			}).then(function (res) {
				if (!res.success) {
					throw new Error(String(res.error || '发送失败'));
				}
				var r = res.result || {};
				if (r.outcome === 'accepted') {
					msgInput.value = '';
					autoGrow();
					renderHint();
					Mt5700.success('发送成功'
						+ (r.parts && r.parts.length > 1
							? '（' + r.parts.length + ' 段长短信）' : ''));
				} else if (r.outcome === 'unknown') {
					/*
					 * 结果不明：绝不自动重发。如实告诉用户可能已经发走了，
					 * 并让他用「刷新」去模组里看真实状态再决定。
					 */
					Mt5700.warning('发送结果不明：' + (r.message || '')
						+ '。请勿重复点击发送。');
				} else {
					throw new Error(r.message || '模组拒绝了这条短信');
				}
				/* 无论哪一档，都用模组的真实数据刷新界面 */
				return refreshAll();
			}).catch(function (err) {
				Mt5700.error((err && err.message) || '发送失败');
			}).then(function () {
				sendBtn.disabled = false;
				sendBtn.textContent = label;
			});
		}

		/* ================= 本地归档 ================= */

		/*
		 * 删除一批界面消息对应的本地归档记录。
		 *
		 * 指纹由**后端**按 `方向|号码|时间|正文` 计算，前端只回传原始字段 ——
		 * 前端因此不必再实现一次 SHA-256，两处口径也不会走偏。
		 *
		 * 同时带上模组槽位：收到的长短信是按段归档的，界面合并成一条气泡后
		 * 只带着各段的槽位，删它得靠槽位命中；发出的短信没有槽位，只能靠指纹。
		 */
		function dropLocalRecords(msgs) {
			if (!msgs || !msgs.length) {
				return Promise.resolve(0);
			}
			var byNumber = {};
			msgs.forEach(function (m) {
				if (!m.number) {
					return;
				}
				if (!byNumber[m.number]) {
					byNumber[m.number] = { items: [], indices: [] };
				}
				byNumber[m.number].items.push({
					direction: m.type === 'sent' ? 'sent' : 'received',
					content: m.content || '',
					time: m.time || null
				});
				indicesOf(m).forEach(function (ix) {
					if (byNumber[m.number].indices.indexOf(ix) < 0) {
						byNumber[m.number].indices.push(ix);
					}
				});
			});

			var numbers = Object.keys(byNumber);
			var chain = Promise.resolve(0);
			numbers.forEach(function (num) {
				chain = chain.then(function (acc) {
					return AtWs.client.sms('store_drop', {
						number: num,
						items: byNumber[num].items,
						indices: byNumber[num].indices
					}).then(function (res) {
						if (!res.success) {
							Mt5700.warning('本地记录文件删除失败：'
								+ String(res.error || ''));
							return acc;
						}
						return acc + ((res.result && res.result.dropped) || 0);
					});
				});
			});
			return chain.then(function (n) {
				/* 删过本地文件的号码，缓存立刻作废，否则重绘又把删掉的那条画回来 */
				numbers.forEach(function (num) { invalidateArchive(num); });
				if (n > 0) {
					Mt5700.success('已同步删除 ' + n + ' 条本地记录');
				}
				return refreshArchiveStats();
			});
		}

		/* 删除整个会话的归档文件；number 为空且 all=true 时清空全部 */
		function dropArchiveConversation(number, all) {
			return AtWs.client.sms('store_drop', {
				number: number || '',
				all: !!all
			}).then(function (res) {
				if (!res.success) {
					Mt5700.error(String(res.error || '删除失败'));
				} else {
					Mt5700.success(all ? '已清空本地归档' : '已删除该会话的本地归档');
				}
				return refreshAll();
			}).catch(function () {
				Mt5700.error('删除失败');
			});
		}

		/* 短信弹窗脚手架（本页四处弹窗共用）：建 mask/box、写标题、挂到 body、
		 * 点遮罩关闭。返回 { mask, box, close }，正文与底部按钮由调用方追加。 */
		function smsModal(title) {
			var mask = smsEl('div', { 'class': 'mt5700-modal-mask mt5700-sms-modal-mask' });
			var box = smsEl('div', { 'class': 'mt5700-modal mt5700-sms-modal' });
			box.appendChild(smsEl('div', { 'class': 'mt5700-modal-title' }, title));
			mask.appendChild(box);
			document.body.appendChild(mask);
			mask.addEventListener('click', function (e) {
				if (e.target === mask) {
					close();
				}
			});
			function close() {
				if (mask.parentNode) {
					mask.parentNode.removeChild(mask);
				}
			}
			return { mask: mask, box: box, close: close };
		}

		/* 查看某个会话的归档记录（弹窗内展示，不干扰当前会话） */
		function viewArchive(number) {
			AtWs.client.sms('store_read', { number: number }).then(function (res) {
				var modal = smsModal('归档：' + number);
				var body2 = smsEl('div', { 'class': 'mt5700-sms-modal-scroll' });

				if (!res.success) {
					body2.appendChild(Mt5700.empty('读取失败：' + String(res.error || '')));
					} else {
						var entries = (res.result && res.result.entries) || [];
						if (!entries.length) {
							body2.appendChild(Mt5700.empty('该会话没有归档记录'));
						} else {
							entries.forEach(function (e) {
								var isSent = e.direction === 'sent';
								var bubble = smsEl('div', { 'class': 'mt5700-sms-bubble' });
								bubble.appendChild(smsEl('div', { 'class': 'mt5700-sms-text' },
									e.content || '(空消息)'));
								bubble.appendChild(smsEl('div', { 'class': 'mt5700-sms-time' },
									smsTimeText(e.time, e.timeApprox)));
								body2.appendChild(buildSmsRow(isSent, bubble));
							});
						}
					}

				modal.box.appendChild(body2);

				var actions2 = smsEl('div', { 'class': 'mt5700-modal-footer' });
				actions2.appendChild(Mt5700.ghostButton('关闭', modal.close));
				modal.box.appendChild(actions2);
			}).catch(function () {
				Mt5700.error('读取归档失败');
			});
		}

		/* 归档管理弹窗：按会话列出占用，可单独查看或删除 */
		function openArchiveModal() {
			/*
			 * 归档统计是一次轻量读文件，但页面刚打开时短信通道常正忙，
			 * 首次调用偶发失败 —— 失败会让弹窗上限显示成「—」。
			 * 这里在读不到上限时补一次重试，避免用户看到半截数据。
			 */
			refreshArchiveStats().then(function () {
				if (state.archive && state.archive.maxBytes) {
					return null;
				}
				return refreshArchiveStats();
			}).then(function () {
				var modal = smsModal('本地短信归档');
				var box = modal.box;
				var listEl = smsEl('div', { 'class': 'mt5700-agree-list mt5700-sms-modal-list' });

				var a = state.archive || {};
				var list = a.conversations || [];
				box.appendChild(smsEl('p', { 'class': 'mt5700-hint' },
					'已用 ' + AtWs.formatFlow(a.totalBytes) + ' / 上限 '
					/*
					 * 上限读不到时显示「—」而不是「0 B」：0 B 会被当成
					 * 「配额为零」这种真实状态，比不知道更误导。
					 */
					+ (a.maxBytes ? AtWs.formatFlow(a.maxBytes) : '—')
					+ '（' + list.length + ' 个会话，超出后自动淘汰最旧的）'));

				if (!list.length) {
					listEl.appendChild(Mt5700.empty('暂无归档记录'));
				} else {
					list.forEach(function (c) {
						var row = smsEl('div', {
							'class': 'mt5700-inline mt5700-sms-archive-row'
						});

						var left = smsEl('div', {
							'style': 'min-width:0;display:flex;flex-direction:column;gap:2px;'
						});
						left.appendChild(smsEl('strong', {}, c.number || '未知号码'));
						left.appendChild(smsEl('span', { 'class': 'mt5700-hint' },
							(c.count || 0) + ' 条 · ' + AtWs.formatFlow(c.bytes)
							+ (c.lastContent ? ' · ' + c.lastContent : '')));
						row.appendChild(left);

						var right = smsEl('div', {
							'style': 'flex:0 0 auto;display:flex;gap:6px;'
						});
						var viewBtn = Mt5700.button('查看', function () {
					viewArchive(c.number);
				}, 'ghost');
						var delBtn = Mt5700.button('删除', function () {
							Mt5700.confirm('确定删除该会话的本地归档记录？', function () {
								dropArchiveConversation(c.number, false);
							});
						}, 'danger');
						right.appendChild(viewBtn);
						right.appendChild(delBtn);
						row.appendChild(right);

						listEl.appendChild(row);
					});
				}
				box.appendChild(listEl);

				var actions = smsEl('div', { 'class': 'mt5700-modal-footer' });
				actions.appendChild(Mt5700.ghostButton('关闭', modal.close));
				var purgeBtn = Mt5700.dangerButton('清空全部归档', function () {
					Mt5700.confirm('确定清空全部本地归档记录？', function () {
						dropArchiveConversation('', true);
					});
				});
				actions.appendChild(purgeBtn);
				box.appendChild(actions);
			});
		}

		/*
		 * 带「同时删除本地记录文件」勾选的确认框。
		 *
		 * Mt5700.confirm 只有确认/取消两个按钮，删短信要多问一句
		 * 「本地留档要不要一起删」，故单独做一个。
		 *
		 * 回调参数 `alsoLocal`：true 表示连本地归档一起删。
		 *
		 * `opts.localOnly`：这条在模组里根本没有槽位（典型是发出的短信，只在归档里），
		 * 能删的本来就只有本地文件 —— 此时藏掉勾选框，直接按「删本地」处理，
		 * 否则那个勾选框会让人以为另有一个「只删模组、保住本地」的选项。
		 */
		function confirmDeleteModal(message, onOk, opts) {
			var localOnly = !!(opts && opts.localOnly);
			var modal = smsModal(message);
			var box = modal.box;
			var chk = smsEl('input', { type: 'checkbox' });

			var optRow = smsEl('label', {
				'class': 'mt5700-inline mt5700-sms-opt-row'
			}, [chk, smsEl('span', {}, '同时删除本地记录文件')]);
			if (!localOnly) {
				box.appendChild(optRow);
			}

			var actions = smsEl('div', { 'class': 'mt5700-modal-footer' });
			actions.appendChild(Mt5700.ghostButton('取消', modal.close));
			var okBtn = Mt5700.dangerButton('删除', function () {
				var alsoLocal = localOnly ? true : !!chk.checked;
				modal.close();
				onOk(alsoLocal);
			});
			actions.appendChild(okBtn);
			box.appendChild(actions);
		}

		/* ================= 删除 ================= */

		/*
		 * 一条界面消息对应模组里的哪些序号。
		 *
		 * 判据是 `typeof === 'number'`：后端把「头行里读不出序号」的那几条报成
		 * null（不是 0 —— 实机的短信就从 0 号位开始排，拿 0 当哨兵会让最老的
		 * 一条永远删不掉），那种条目压根不给勾选。
		 */
		function indicesOf(msg) {
			if (msg.partIndices && msg.partIndices.length) {
				return msg.partIndices.slice();
			}
			return typeof msg.index === 'number' ? [msg.index] : [];
		}

		function deleteMessage(msg) {
			var idxs = indicesOf(msg);
			if (!idxs.length) {
				/*
				 * 模组里没有槽位。若这条来自本地归档（发出的短信就属于这种），
				 * 那它是**真实存在**的记录，不能像解析失败的条目那样只提示一句了事；
				 * 只删本地那一份，删完立刻重绘。
				 */
				if (msg.archived) {
					confirmDeleteModal('确定删除该条本地记录？', function () {
						dropLocalRecords([msg]).then(function () {
							invalidateArchive(msg.number);
							return refreshArchiveStats();
						}).then(function () {
							renderConversation();
						});
					}, { localOnly: true });
					return;
				}
				Mt5700.warning('该条不在模组存储中，刷新后即消失');
				return;
			}
			confirmDeleteModal('确定删除该短信？', function (alsoLocal) {
				AtWs.client.sms('delete', { indices: idxs }).then(function (res) {
					if (!res.success) {
						Mt5700.error(String(res.error || '删除失败'));
					} else if ((res.result || {}).deleted > 0) {
						Mt5700.success('删除成功');
				} else {
					Mt5700.error('模组未能删除该短信');
				}
					/* 勾选了才动本地留档；删不动只提示，不影响上面已完成的删除 */
					if (alsoLocal) {
						return dropLocalRecords([msg]).then(function () {
							invalidateArchive(msg.number);
						});
					}
					return refreshAll();
				}).catch(function () {
					Mt5700.error('删除失败');
					return refreshAll();
				});
			});
		}

		/* 批量删除：勾选条目后按批下发，并可选同步删除本地归档 */
		function batchDelete() {
			var modal = smsModal('批量删除短信');
			var box = modal.box;
			var listEl = smsEl('div', { 'class': 'mt5700-agree-list mt5700-sms-modal-list' });
			var checked = [];

			var messages = currentMessages();
			var pickBoxes = [];

			/*
			 * 全选条：吸在列表顶部，滚动时始终可点。
			 * 只对「真的能删」的条目生效 —— 没有模组槽位、又不是本地归档的
			 * 条目复选框是禁用的，全选不能把它们算进去。
			 */
			var selectAllChk = smsEl('input', { type: 'checkbox' });
			selectAllChk.setAttribute('aria-label', '全选可删除的短信');
			var selectAllCount = smsEl('span', {
				'class': 'mt5700-sms-selectall-count'
			}, '');
			var selectAllRow = smsEl('label', { 'class': 'mt5700-sms-selectall' }, [
				selectAllChk,
				smsEl('span', { 'class': 'mt5700-sms-selectall-label' }, '全选'),
				selectAllCount
			]);

			function selectablePicks() {
				return pickBoxes.filter(function (p) { return !p.chk.disabled; });
			}

			function updatePickCount() {
				var can = selectablePicks();
				var selected = 0;
				can.forEach(function (p) {
					if (p.chk.checked) selected++;
				});
				selectAllCount.textContent = can.length
					? '已选 ' + selected + ' / ' + can.length + ' 条'
					: '没有可删除的短信';
				selectAllChk.disabled = !can.length;
				selectAllChk.checked = can.length > 0 && selected === can.length;
				selectAllChk.indeterminate = selected > 0 && selected < can.length;
			}

			selectAllChk.addEventListener('change', function () {
				var on = selectAllChk.checked;
				selectablePicks().forEach(function (p) {
					p.chk.checked = on;
					var at = checked.indexOf(p.m);
					if (on && at < 0) {
						checked.push(p.m);
					}
					if (!on && at >= 0) {
						checked.splice(at, 1);
					}
				});
				updatePickCount();
			});

			if (messages.length) {
				listEl.appendChild(selectAllRow);
			}

			messages.forEach(function (m) {
				var row = smsEl('label', { 'class': 'mt5700-inline mt5700-sms-pick-row' });
				var chk = smsEl('input', { type: 'checkbox' });
				/*
				 * 能不能删**只看有没有真实槽位**，不再因为「解析失败」就禁掉。
				 *
				 * 状态报告那类 PDU 我们解析不出号码和正文，但它确实占着一个 SM
				 * 槽位、也确实吃掉了存储额度（40 条里少一条可用）。能看见却不能
				 * 删，用户只能干看着它占地方。0 号槽位是合法的，不能拿 0 当哨兵。
				 *
				 * 本地归档条目在模组里本来就没有槽位，却能删本地文件，照样给勾。
				 */
				if (typeof m.index !== 'number' && !m.archived) {
					chk.disabled = true;
				}
				pickBoxes.push({ m: m, chk: chk });
				chk.addEventListener('change', function () {
					var idx = checked.indexOf(m);
					if (chk.checked && idx < 0) {
						checked.push(m);
					}
					if (!chk.checked && idx >= 0) {
						checked.splice(idx, 1);
					}
					updatePickCount();
				});
				row.appendChild(chk);

				var label = (m.type === 'sent' ? '[发] ' : '[收] ')
					+ (m.number || '未知号码') + '：';
				label += m.error ? '短信解析失败：' + m.error : (m.content || '').slice(0, 30);
				row.appendChild(smsEl('span', {
					'class': 'mt5700-hint mt5700-sms-pick-text'
				}, label));
				listEl.appendChild(row);
			});

			if (!messages.length) {
				listEl.appendChild(Mt5700.empty('当前会话没有可删除的短信'));
			}
			updatePickCount();
			box.appendChild(listEl);

			/* 与单条删除口径一致的「同时删除本地记录文件」 */
			var alsoLocalChk = smsEl('input', { type: 'checkbox' });
			var optRow = smsEl('label', {
				'class': 'mt5700-inline mt5700-sms-opt-row'
			}, [alsoLocalChk, smsEl('span', {}, '同时删除本地记录文件')]);
			box.appendChild(optRow);

			var actions = smsEl('div', { 'class': 'mt5700-modal-footer' });
			actions.appendChild(Mt5700.ghostButton('取消', modal.close));

			var confirmBtn = Mt5700.dangerButton('删除所选', function () {
				var alsoLocal = !!alsoLocalChk.checked;
				modal.close();
				if (!checked.length) {
					Mt5700.warning('请先选择要删除的短信');
					return;
				}
				/* 一次事务里删完：后端逐条 AT+CMGD，中途不会被别的命令插进来 */
				var idxs = [];
				checked.forEach(function (m) {
					indicesOf(m).forEach(function (ix) {
						if (idxs.indexOf(ix) < 0) {
							idxs.push(ix);
						}
					});
				});
				if (!idxs.length) {
					/*
					 * 所选条目在模组里全都没有槽位，但都是本地归档记录（发出的短信
					 * 就是这种）—— 这种批次能删的只有本地文件，不必再问勾没勾。
					 */
					var hasArchived = checked.some(function (m) { return m.archived; });
					if (!hasArchived) {
						Mt5700.warning('所选条目都不在模组存储中');
						return;
					}
					dropLocalRecords(checked).then(function () {
						invalidateArchive();
						return refreshArchiveStats();
					}).then(function () {
						renderConversation();
					});
					return;
				}
				/*
				 * 分批下发（理由见 SMS_DELETE_CHUNK）；每批仍是后端的一次事务，
				 * 批内不会被别的命令插进来，批与批之间让出通道给巡检。
				 */
				var total = { deleted: 0, failed: 0 };
				var chain = Promise.resolve();
				for (var c = 0; c < idxs.length; c += SMS_DELETE_CHUNK) {
					(function (chunk) {
						chain = chain.then(function () {
							return AtWs.client.sms('delete', { indices: chunk })
						.then(function (res) {
										if (!res.success) {
							throw new Error(String(res.error || '批量删除失败'));
										}
							var r = res.result || {};
										total.deleted += (r.deleted || 0);
							total.failed += ((r.failed || []).length);
								});
							});
					})(idxs.slice(c, c + SMS_DELETE_CHUNK));
				}
				chain.then(function () {
					Mt5700.success('已删除 ' + total.deleted + ' 条'
							+ (total.failed ? '，' + total.failed + ' 条失败' : ''));
					if (alsoLocal) {
						return dropLocalRecords(checked);
					}
					return refreshAll();
				}).catch(function (e) {
					/* 半途失败也要回读一次：已经删掉的那些不该还留在界面上 */
					Mt5700.error(String((e && e.message) || '批量删除失败'));
					if (alsoLocal) {
						return dropLocalRecords(checked);
					}
					return refreshAll();
				});
			});
			/* 别漏了这一步：按钮建出来不挂到弹窗上，弹窗就只剩「取消」 */
			actions.appendChild(confirmBtn);
			box.appendChild(actions);
		}

		/*
		 * 后端条目 → 界面消息。
		 *
		 * index 一律来自本次 `list` 的真实返回，界面上每一条都对应模组里的一个
		 * 真实槽位；本地归档改由后端维护，不再混进列表参与删除。
		 */
		function toUiMessage(e) {
			return {
				index: e.index,
				content: e.content || '',
				number: normalizeNumber(e.number || ''),
				time: e.time || null,
				timeApprox: !!e.timeApprox,
				type: e.direction === 'sent' ? 'sent' : 'received',
				isConcatenated: !!e.concat,
				concatenatedRef: e.concat ? e.concat.reference : null,
				concatenatedSeq: e.concat ? e.concat.partNumber : null,
				concatenatedTotal: e.concat ? e.concat.partsCount : null,
				error: e.error || null
			};
		}

		/* ================= 新短信推送 ================= */

		var newSmsHandler = function (resp) {
			if (!resp || resp.type !== 'new_sms' || !resp.data) {
				return;
			}
			/*
			 * 收到到达通知后立刻拉一次真实列表：通知只是「有新短信」这个信号，
			 * 正文、时间、序号都必须以模组里读到的为准。
			 */
			refreshAll();
			Mt5700.info('收到新短信');
		};
		AtWs.client.subscribe(newSmsHandler);

		/* ================= 前台短信模式：生命周期 ================= */

		var sessionId = 0;
		var heartbeat = null;
		/* 徽章秒表：1s 粒度刷新剩余租约（原来只在事件点刷新，看起来钉死不动） */
		var statusTicker = null;

		function openSession() {
			/*
			 * 已有会话且心跳在跑：不重复开。
			 * visibilitychange 的 visible 分支与初始化的竞态曾导致
			 * 「会话 8 已释放 → 0.4s 后会话 9 上线」的 churn ——
			 * 模式被反复开关，后台轮询刚恢复又被暂停。
			 */
			if (sessionId && heartbeat) {
				return;
			}
			AtWs.client.sms('session_open', { ttl_ms: SMS_SESSION_TTL_MS }).then(function (res) {
				if (!res.success || !res.result) {
					return;
				}
				sessionId = res.result.session_id || 0;
				lastRenewAt = Date.now();
				refreshChannelStatus();
				/* 秒表：每秒刷新一次剩余租约，让倒计时真的在走 */
				if (statusTicker) {
					clearInterval(statusTicker);
				}
				statusTicker = setInterval(refreshChannelStatus, 1000);
				/*
				 * 心跳续期：页面还在就一直占着；后端另有租约上限兜底，
				 * 即便这里的心跳因为浏览器休眠停了，也会自动释放。
				 */
				if (heartbeat) {
					clearInterval(heartbeat);
				}
				heartbeat = setInterval(function () {
					if (!sessionId) {
						return;
					}
					AtWs.client.sms('session_renew', {
						session_id: sessionId,
						ttl_ms: SMS_SESSION_TTL_MS
					}).then(function (r) {
						/* 续不上（租约已被自动回收）→ 重新申请，不要静默失效 */
						if (!r.success || (r.result && r.result.renewed === false)) {
							sessionId = 0;
							refreshChannelStatus();
							openSession();
						} else {
							lastRenewAt = Date.now();
							refreshChannelStatus();
						}
					});
				}, SMS_SESSION_HEARTBEAT_MS);
			}).catch(function () {});
		}

		function closeSession() {
			if (heartbeat) {
				clearInterval(heartbeat);
				heartbeat = null;
			}
			if (statusTicker) {
				clearInterval(statusTicker);
				statusTicker = null;
			}
			if (!sessionId) {
				return;
			}
			var id = sessionId;
			sessionId = 0;
			lastRenewAt = 0;
			refreshChannelStatus();
			AtWs.client.sms('session_release', { session_id: id }).catch(function () {});
		}

		/* 页面隐藏（切到别的页签/熄屏）也视为离开：释放让后台巡检恢复 */
		var onVisibility = function () {
			if (document.hidden) {
				closeSession();
			} else if (!sessionId) {
				openSession();
			}
		};
		document.addEventListener('visibilitychange', onVisibility);

		/* 离开本页（hash 路由切换）时退订推送、摘除全局监听并释放短信租约。
		 * LuCI view 没有 dispose 钩子：原先写在 _dispose 里的清理从不执行，
		 * 租约心跳会在切走后长期续命（压制后台巡检），监听器也随反复进出累积。 */
		Ui.onLeave(function () {
			AtWs.client.unsubscribe(newSmsHandler);
			document.removeEventListener('visibilitychange', onVisibility);
			window.removeEventListener('resize', refit);
			window.removeEventListener('orientationchange', refit);
			closeSession();
		});

		/* ================= 窄屏：列表 / 会话 切换 ================= */

		/*
		 * 演示稿在窄屏把侧栏 `display:none` 掉，结果手机上点进会话就切不回去。
		 * 这里用一个状态类在「列表」与「会话」之间切换，并加了返回按钮。
		 */
		function showChat(on) {
			if (on) {
				page.classList.add('is-mobile-chat');
			} else {
				page.classList.remove('is-mobile-chat');
			}
		}

		/* ================= 高度贴合视口 ================= */

		/*
		 * 把会话区高度定成「视口里剩下的那点地方」。
		 *
		 * 纯 CSS 算不出来：上方的导航栏、页签高度随主题和屏宽变，用 calc
		 * 写死常量必然在某些尺寸下超出去，页面就又能整体滚动了 ——
		 * 那样「滚消息」变成了「滚网页」，输入框被推到很远。
		 * 这里直接量一次 offsetTop，剩下的全给会话区，超出的部分由消息区自己滚。
		 *
		 * 注意：不要再用「文档溢出多少就扣多少」来自适应。OpenWrt 的
		 * 「保存与应用」操作条、主题页脚都在会话区下方，扣溢出的结果就是
		 * 它们越高、聊天窗越矮，手机上直接被压成一条缝。会话区始终占满
		 * 视口剩余高度，操作条留在首屏之外，需要时整页滚一下就能看到。
		 */
		function fitHeight() {
			var top = layout.getBoundingClientRect().top + window.pageYOffset;
			/* 8px 底部留白，避免卡片贴着屏幕边缘 */
			var avail = window.innerHeight - top - 8;
			if (avail < 320) {
				avail = 320;      /* 极端矮屏：宁可整页滚，也不要把会话区压成一条缝 */
			}
			layout.style.height = avail + 'px';
		}

		var refit = function () { fitHeight(); };

		/*
		 * 延后多帧再量：render() 返回时节点还没插进文档，此刻 rect.top 是 0，
		 * 算出来的高度会把整页重新顶长；hero 里的图片/字体撑开又会改变高度。
		 * 所以量几次，最后一次一定是在页面稳定之后（fitHeight 自带溢出回扣，
		 * 多量几次只会更准，不会累积误差）。
		 */
		function scheduleFit() {
			window.requestAnimationFrame(function () {
				fitHeight();
				window.requestAnimationFrame(fitHeight);
			});
			window.setTimeout(fitHeight, 400);
			window.setTimeout(fitHeight, 1500);
		}

		/* ================= 初始化 ================= */

		Ui.startup(body, function () {
			openSession();
			refreshAll();
			scheduleFit();
		});

		scheduleFit();
		window.addEventListener('resize', refit);
		window.addEventListener('orientationchange', refit);

		renderStorage();
		renderArchive();
		renderContacts();
		renderConversation();

		return page;
	}
});
