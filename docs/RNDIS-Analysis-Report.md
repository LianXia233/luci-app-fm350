# FM350 / F31 29.23.24n 逆向分析：RNDIS 域归属、APN 审计、稳定性与拨号成功率

> 分析对象：`F31_29.23.24n.zip`（MT6880 / k6880v1_mdot2_datacard，AP 内核 4.19.205，MD 固件 81600.0000.00.29.23.24）
> 方法：root.squashfs 解包 + AP 内核 vmlinux 提取 IKCONFIG/字符串 + MD 镜像 md1rom 容器解析 + 关键二进制静态分析

---

## 一、结论速览

| 问题 | 结论 |
|---|---|
| RNDIS 由哪个域创建 | **MD 域**。AP 内核根本没有 RNDIS/ECM/NCM 功能，只有 ACM/ADB/MSC；RNDIS 的 USB 类实现、数据面、主机侧 DHCP 全在 MD 的 `middleware/hif/usbclass/rndis` |
| AP 域在 USB 里干什么 | 只做控制器（mtu3 @11201000.usb）、`dipc_mode` 模式切换、枚举骨架与 ACM/ADB/MSC 端口 |
| 主机侧 `AT+EMBIND` 为什么无效 | 它进的是 USB ACM → atcid → MIPC 的“外部 AT”通道，与 MD 内部 RNDIS 实例所需的上下文/时机不一致；只有模组内部在 PDN 就绪后下发才会绑定成功 |
| 预置国内四大运营商 APN | **有问题，而且很严重**：移动 4 个号段只有「空 APN」的 ia 条目，联通/电信只有 ims，广电 46015 整个文件不存在 |
| APN 缺失的后果 | `AT+CGDCONT` 未显式设置时附着 APN 为空 → 电信/联通 PDP 激活经常直接失败，移动即使激活也常拿不到地址 |
| 稳定性最大的坑 | `nvdata/md_cmn/dipc_config` 缺 `dual_ipc_mode` 时，`usb.init` 直接走「NO USB」分支退出 → 模组完全不枚举，看起来像砖 |
| NCM 能不能加 | **不能**，三重硬阻塞（见第五节），需要重编内核或改 MD 固件源码 |

---

## 二、RNDIS 到底是谁创建的

### 2.1 证据链

**(1) AP 内核没有 RNDIS/ECM/NCM 功能**

从 `boot-verified.img` 提取的内核配置（`IKCFG_ST` 段）：

```
CONFIG_USB_GADGET=y
CONFIG_USB_F_ACM=y
CONFIG_USB_F_FS=y
CONFIG_USB_CONFIGFS=y
CONFIG_USB_CONFIGFS_ACM=y
CONFIG_USB_CONFIGFS_MASS_STORAGE=y
CONFIG_USB_CONFIGFS_F_FS=y
CONFIG_USB_CONFIGFS_F_HID=y
# CONFIG_USB_CONFIGFS_NCM is not set
# CONFIG_USB_CONFIGFS_ECM is not set
# CONFIG_USB_CONFIGFS_ECM_SUBSET is not set
# CONFIG_USB_CONFIGFS_RNDIS is not set
```

内核里能实例化的 gadget 功能名只有：`acm`、`gser`、`hid`、`loopback`、`mass_storage`、`obex`、`ptp`（+`ffs`）。
**没有 `rndis`/`ecm`/`ncm` 任何一个** —— AP 域物理上无法创建 RNDIS 功能。

**(2) AP 侧的 `usb.init` 只挂了串口类功能**

原厂 `/etc/init.d/usb.init` 用 configfs 建的是：

```
functions/ffs.adb  acm.gs0  acm.gs1  acm.gs2  acm.gs3  mass_storage.usb0
```

没有任何网络功能。AP 侧也**没有 DHCP 服务端**（全盘只有 `udhcpc` 客户端脚本、`lib/netifd/proto/dhcp.sh`，没有 `udhcpd`/`dnsmasq`）。

**(3) MD 镜像里有完整的 RNDIS 类实现**

对 `modem-verified.img` 做字符串提取，MD 的 HIF 层源码路径清清楚楚：

```
middleware/hif/usbclass/rndis/src/rndis_ctrl.c
middleware/hif/usbclass/rndis/src/rndis_data_rb.c
middleware/hif/usbclass/rndis/src/rndis_if.c
middleware/hif/usbclass/rndis/src/rndis_oid.c
middleware/hif/usbclass/rndis/src/rndis_sm.c
middleware/hif/usbclass/niccommon/src/nicc_ctrl.c
middleware/hif/usbcore/src/usbcore_stdreq.c
middleware/hif/usbcore/src/usbcore_dual_owner.c
middleware/hif/ipcore/src/ipc_dhcp_adp.c
```

`usbclass/` 下只有 `acm / adb / msd / rndis / niccommon` 五类 —— **RNDIS 是 MD 自己实现的一个 USB 类**，连标准请求（`usbcore_stdreq`）和双所有者切换（`usbcore_dual_owner`）都在 MD 里。

**(4) 绑定关系只能存在于 MD 内部**

MD 的 AT 面里有：

```
AT+EMBIND=1,"M-RNDIS",<cid>
+EMBIND: (0,1),(list of supported <L2P>s),(1-16)
支持的 L2P： "M-CCMNI" / "M-RNDIS" / "M-MBIM" / "M-LHIF"
```

`M-RNDIS` 是 L2P（Layer-2 Protocol）名字，`<cid>` 是 PDN 上下文号。这条命令的语义就是「把 RNDIS 这个 USB 网络实例绑到第 cid 个 PDN 上」，**被绑的实例在 MD 里**，AP 侧没有任何对应对象。

**(5) AP 侧只在 M-CCMNI 路径下才有 netdev**

AP 的 `mtk_netagent` 打开 `/dev/radio/pttynwcmd`、`/dev/radio/pttynwurc`，处理 `AT+EIF=...`（接口 up + IP），把地址配到 `ccmni0..5`。也就是说：
- 绑 `M-CCMNI` → AP 出现 `ccmniX` 带 PDN 地址；
- 绑 `M-RNDIS` → AP 侧**什么都不出现**，数据从 MD 的 USB 端点直接进主机。

### 2.2 数据流

```
       主机 (Windows / Linux / OpenWrt)
            │  USB  RNDIS(数据) + CDC-ACM(AT 口)
            ▼
   ┌──────────────────────────────────────────┐
   │  mtu3 控制器  /sys/devices/platform/11201000.usb │
   │  dipc_mode: 0/2 = AP 独占 | 1/3/4 = 与 MD 共享 | 11 = PCIe 模式 │
   └───────────────┬──────────────────────────┘
                   │
      ┌────────────┴─────────────┐
      ▼                          ▼
 AP 域 (Linux 4.19.205)      MD 域 (modem RTOS)
 configfs gadget:            usbcore + usbclass/rndis
   acm.gs0..3  ← AT 口         ├─ rndis_sm / rndis_ctrl / rndis_oid
   ffs.adb     ← ADB           ├─ ipcore (数据转发 + DHCP 服务)
   mass_storage                └─ EMBIND → cid → PDN → 5G 核心网
      │
      ▼
 atcid / atci_service ──MIPC──▶ MD AT 引擎（+EMBIND 在这里执行）
 mtk_netagent        ──ptty──▶ MD（+EIF → 配置 ccmni0..5）
```

### 2.3 为什么主机侧发同一条 AT 不生效

主机侧 `AT+EMBIND` 的路径是：USB ACM(`ttyGS0`) → `atcid` → `/dev/atci-service` → MIPC → MD AT 引擎。这条路径能执行普通 AT（`AT+CSQ`、`AT+CGACT`），但 EMBIND 绑定的是 **MD 内部 RNDIS 实例 + 当前 PDN 上下文**，它需要：

1. PDN 已经 `ACTIVE`（cid 存在且有 IP）；
2. 命令在模组内部上下文里执行（与 RNDIS 实例同域），而不是从外部 AT 端口进来。

实际现象就是：主机在枚举完、拨号前发 → 没有 cid 可绑，返回错误；拨号后发 → 通道/上下文不对，绑定不落地。所以正确做法是：**在模组内部、PDN 就绪的那一刻由 AP 域程序下发**，这就是 `atproxy` 存在的意义。

> 附带一个容易忽略的点：原厂 `usb.init` 对 `boot_mode` 只处理了 `0000` 和 `0001`，其它值（比如某些工厂/OTA 模式）落进 `esac` 空分支 —— 结果是 configuration 里**一个功能都没链接**，USB 枚举出来是个空设备。这版已经补了 default 分支。

---

## 三、预置国内四大运营商 APN 审计

### 3.1 数据库在哪

MD 的 APN 数据库以「MCC_MNC.conf」形式随 rootfs 下发：

```
/vendor/etc/md/apncfg/        510 个运营商配置
/vendor/etc/md/plmn/TS25      运营商名映射
```

格式（每行一个 APN profile，冒号分隔）：

```
<APN>:<类型列表>:<PDP v4>:<PDP v6>:<认证>:<用户>:<密码>:<承载列表>:<GID类型>:<GID>:<保留>
```

### 3.2 中国区现状（原厂）

| PLMN | 运营商 | 原厂内容 | 判定 |
|---|---|---|---|
| 46000 / 46002 / 46007 / 46008 | 中国移动 | `:ia:IPV4V6:IPV4V6:::::::`（**APN 为空**）+ `ims` | ✗ 无 `cmnet` |
| 46001 / 46009 | 中国联通 | 只有 `ims` | ✗ 无 `3gnet`，连 ia 条目都没有 |
| 46003 / 46011 / 46012 / 46013 | 中国电信 | 只有 `ims` | ✗ 无 `ctnet`，连 ia 条目都没有 |
| 46015 | 中国广电 | **文件不存在** | ✗ 完全无条目 |
| 46004 / 46005 / 46006 | 移动/电信/联通旧号段 | **文件不存在** | ✗ |

对照：全库 510 个配置里只有 50 个带 `ia` 条目；中国区 14 个号段里 **0 个**带正确的 `ia` APN。

### 3.3 这会造成什么

- **电信 / 联通**：MD 侧没有默认数据 APN，宿主不显式 `AT+CGDCONT=1,"IP","ctnet"` 时，`AT+CGACT=1,1` 大概率 `+CME ERROR`（常见 30/33/50），或者激活成功但没地址。
- **移动**：ia APN 为空 → 附着用空 APN。部分省份核心网接受空 APN 并下发默认承载，部分直接拒绝；即使激活，`CGCONTRDP` 也常拿不到 IPv4 地址 → RNDIS 侧 DHCP 无地址可发 → 主机显示「未识别网络」。**这就是「插卡能注册、就是上不了网」的高频原因。**
- **广电**：连条目都没有，只能靠宿主手写 APN。

### 3.4 已修复内容（打进两个固件包）

| 文件 | 改动 |
|---|---|
| 460_00 / 02 / 04 / 07 / 08 | 空 ia → `cmnet:default,ia:...`；**新增 `cmiot` / `cmiot5g` 两条物联卡 profile**；IMS 行 cid 收窄为 `20` |
| 460_01 / 06 / 09 | 新增 `3gnet:default,ia:...`；IMS 行 cid 收窄为 `20` |
| 460_03 / 05 / 11 / 12 / 13 | 新增 `ctnet:default,ia:...`；IMS 行 cid 收窄为 `20` |
| 460_15 | **新建**，`cbnet:default,ia:...` + `ims`（cid 20） |

移动号段的最终形态：

```
cmnet:default,ia:IPV4V6:IPV4V6:::::::      ← 公网卡，兜底
cmiot:default:IPV4V6:IPV4V6:::::::         ← 移动物联卡
cmiot5g:default:IPV4V6:IPV4V6:::::::       ← 移动 5G 物联卡
ims:ims:IPV4V6:IPV4V6::::20:::             ← IMS 只占 cid 20
```

### 3.5 IMS 不再抢数据 cid

原厂每条 IMS 行都是 `...:1|2|3|...|20:::`——第 9 个字段是**该 APN 允许占用的 cid 集合**，写成 1~20 等于告诉 MD「IMS 可以落在任意承载上」。数据拨号固定用 cid 1，于是 cid 1 经常被 IMS 先占住，数据面要么激活失败，要么拿到的是 IMS 承载（有 IP、没路由）。

证据：`310_410.conf`（T-Mobile）里 emergency 行的 cid 列表刻意跳过 18，而 `sos` 行正好用 18——说明这个字段就是用来给不同 APN 分配互斥 cid 的。

现在 IMS 只声明 cid 20，数据留在 cid 1。另外 `atproxy` 启动拨号前会做一次 `AT+CGDCONT?` 探测：

- 若 `dial_cid` 被 IMS 占用 → 自动从 1..7 里挑一个没被 IMS 占的 cid，并写日志；
- 这是兜底，防止 MD 内部仍按老规则把 IMS 放到 cid 1。

### 3.6 物联网卡：不用再手改配置

`atproxy` 的移动 APN 现在是候选列表，按顺序试，成功即停：

```
option apn_cmcc 'cmnet,cmiot,cmiot5g'
```

拨号日志会明确写出试了哪个、哪个失败：

```
AT< AT+CGDCONT=1,"IPV4V6","cmnet"
AT< AT+CGACT=1,1 => +CME ERROR: 30
APN cmnet 激活失败：+CME ERROR: 30
AT< AT+CGDCONT=1,"IPV4V6","cmiot"
AT< AT+CGACT=1,1 => OK
PDN 激活成功 cid=1 apn=cmiot
```

联通物联卡（`scuiot` / `unim2m.njm2mapn`）、电信物联卡（`ctiot`）暂时仍要手改 `apn_cucc` / `apn_ctcc`，因为各地 APN 名不统一，没法给一个可靠默认值。

---

## 四、稳定性与拨号成功率：改了什么

### 4.1 数据面（本次主修）

新增 `/usr/bin/atproxy`（静态 aarch64，66 KB）：

| 能力 | 实现 |
|---|---|
| PDN 就绪检测 | netlink 监听 `ccmni*/rndis*/wwan*/usb*` 的 addr/link 事件 + `AT+CGACT?` 兜底轮询 |
| 首绑 | 开机 20 s（可配） |
| 保活 | 每 120 s 重绑一次（可配） |
| 失败退避 | 5 s 起，连续 6 次失败重开 AT 通道 |
| 结果上报 | 解析 `+GTEMBIND: rc=<n>,cid=<n>` 与 `+EMBIND: <n>`，写 `/var/log/atproxy.log` 与 `/var/run/atproxy.json` |
| AT 通道 | 优先 `/dev/atci-service`（unix socket，能读回响应），失败退化到 `/dev/radio/pttynwcmd`（只写） |
| AT 网关 | 独占通道 + 单队列串行 + **25 ms 硬性间隔**，外部经 `/var/run/atproxy.sock` 接入（见 4.6） |
| 现场诊断 | `atproxy at "AT+CSQ"`（带 hexdump）、`atproxy probe`、`atproxy status` |

### 4.2 拨号（`dial_enabled=1` 时随主进程启动）

拨号不再是独立进程，而是 `atproxy run` 里的一个线程，与绑定共用同一条 AT 通道
（`init.d` 只拉起一个实例，见 4.6 的单实例锁）。

```
AT+CPIN?     → READY
AT+CIMI      → 取前 5 位选 APN 候选（移动：cmnet,cmiot,cmiot5g）
AT+CGDCONT?  → 探测 dial_cid 是否被 IMS 占用，占用则换到空闲 cid
AT+CEREG?    → stat 1/5 才继续
按候选顺序：AT+CGDCONT=<cid>,"IPV4V6","<apn>" → AT+CGACT=1,<cid>
             失败则 AT+CGACT=0,<cid> 清承载，换下一个候选
每 30 s 复查 +CGACT?，连续 2 次查不到才判定掉线（md 承载表刷新有延迟）
检测到其它非 IMS cid 已激活 → 认为外部插件接管，60 s 内不抢拨
```

### 4.3 AT 网关：25 ms 硬性间隔，拨号不再堵插件

#### 问题

`/dev/atci-service` 是流式 socket，谁都能连。原厂状态下有至少三路并发：

| 来源 | 典型行为 |
|---|---|
| Luci 拨号插件 | 打开页面时连发 `AT+CSQ`、`AT+CEREG?`、`AT+CGDCONT?`、`AT+CGACT?` 十几条 |
| fibo 框架 / ubus 脚本 | 按事件触发，瞬间补一轮状态查询 |
| atproxy 自己 | 绑定 + 拨号 |

这些请求各自独立 TCP 式写入，MD 侧 `atcid` 的 AT 解析任务在一次调度里收到十几条
命令，就会直接卡死：表现是先 `AT` 无响应，紧接着 USB 数据面也掉（因为 AT 通道和
RNDIS 绑定共用同一个 MD 任务组）。

#### 改法

`atproxy` 变成唯一持有 atci 通道的进程，内部一个 worker 线程串行下发：

```
外部（插件/CLI） ─┐
                  ├─→ 高优先级队列 ─┐
绑定线程 ─────────┤                 ├─→ worker ─→ 25ms 闸门 ─→ atci_service
拨号线程 ─────────┘─→ 低优先级队列 ─┘                  (串行、单条 in-flight)
```

关键点：

| 机制 | 作用 |
|---|---|
| `at_gap_ms` 硬间隔 | 两条 AT 之间至少 25 ms，**下限写死**，配置填 0 也按 25 执行 |
| 优先级队列 | 外部请求 = 高，拨号/绑定 = 低 |
| 老化 | 连续放行 8 个外部请求后强制服务一次内部，防止插件轮询把拨号饿死 |
| 队列上限 | 64 条，满了直接回 `+ATPROXY: BUSY`，不堆积 |
| 拨号不占通道 | `sleep(30)`、注册轮询、APN 候选退避全在 worker 之外 |
| 长命令限时 | `CGACT` 超时 6 s，这也是外部请求被阻塞的上限 |
| 单实例锁 | `/var/run/atproxy.lock` 的 `flock`，第二个实例直接退出 |

#### 实测

用假 atci 服务端（记录每条命令的到达时间）+ 50 条一次性突发：

```
$ atproxy at ... ; # 另一侧同时灌 50 条 AT+CSQ
响应：50/50，总耗时 1.24 s
服务端收到的相邻间隔：min 25.07 ms  mean 64.06 ms  低于 24 ms 的：0 条
```

拨号运行期间（含 `CGACT=1`）外部 AT 的实测延迟：

| 时刻 | 外部 `AT+CSQ` 延迟 |
|---|---|
| 拨号激活中 | 0.001 s |
| 激活成功后 5 s | 0.001 s |
| 链路巡检 sleep(30) 期间 | 0.001 s |

#### 插件怎么接

把 AT 端点从 `/dev/atci-service` 换成 `/var/run/atproxy.sock` 即可，协议完全一致
（裸 AT 文本，`
` 结尾，响应原样回传）。shell 里：

```sh
# 老写法
printf 'AT+CSQ\r' | socat - UNIX-CONNECT:/dev/atci-service

# 新写法（受 25ms 限速与优先级保护）
printf 'AT+CSQ\r' | socat - UNIX-CONNECT:/var/run/atproxy.sock

# 或者直接用 atproxy 自己的 CLI（守护在跑时自动走网关）
atproxy at "AT+CSQ"
```

网关自身错误码：

| 响应 | 含义 |
|---|---|
| `+ATPROXY: BUSY` | 队列满或排队超时，稍后重试 |
| `+ATPROXY: TIMEOUT` | 命令在 MD 侧超时 |
| `+ATPROXY: TOOLONG` | 单条超过 255 字节，整条丢弃（不截断转发） |
| `+ATPROXY: NOCHAN` | atci 通道打不开 |

#### 参数

```uci
option at_gap_ms '25'          # 硬间隔，下限 25
option at_sock '/var/run/atproxy.sock'
option at_max_queue '64'
option at_cmd_timeout '3000'   # 外部单条超时
option at_long_timeout '6000'  # 内部 CGACT 超时 = 外部最长阻塞
option at_ext_wait '15000'     # 外部请求最长排队等待
```

#### 前提

这套限速只对**经过网关**的 AT 生效。如果某个插件仍直连 `/dev/atci-service`，
它在 MD 侧仍然能和 atproxy 并发。两个办法：

1. 改插件的 AT 路径（推荐，一行配置）；
2. 临时把 `atproxy` 停掉，让插件独占（`/etc/init.d/atproxy stop`），至少不会三方并发。

注意别同时手工起 `atproxy run` 和 `atproxy dial`——单实例锁会让第二个直接退出，
日志里会写"已有 atproxy 实例在跑"。

### 4.4 启动顺序上的坑（已修）

原厂 `usb.init`：

```sh
dipc_mode=`grep dual_ipc_mode /mnt/vendor/nvdata/md_cmn/dipc_config | ...`
if [ "$dipc_mode" != "1" -a != "3" -a != "4" ]; then
    echo "DIPC mode with NO USB" > /dev/ttyS0
    exit 0
fi
```

`nvdata` 是 yaffs2 可写分区。一旦 `dipc_config` 丢失/损坏/被误清（换卡、异常掉电、刷机），`dipc_mode` 为空 → 直接 `exit 0` → **USB 完全不枚举，主机看不到任何设备**，现场表现和砖头一样。

`MOD_AP_MD` 变体带 `/etc/init.d/mdcfg`（S005，在 `dipcd` 之后、`usb.init` 之前）：
- 文件缺失 → 建最小可用 `dual_ipc_mode:4`（USB only，见第五章）；
- 键缺失 → 追加，保留其它键；
- 其它值 → 交给 `S97pcieoff_late` 统一修正并重启 `dipcd`。

### 4.5 可调试性（这次一起补的）

- **ADB 常开**：`usb.init` 三个分支统一挂 `ffs.adb` 并拉起 `adbd_usb`（0001 分支原来连 `ffs.adb` 链接都没有）。
- **SSH-over-USB**：静态 `dropbear 2024.86`（343 KB，musl，`-B` 空口令）+ procd 托管，配合 `adb forward tcp:2222 tcp:22` 直接进 AP 域 shell。

以前出问题只能靠 `AT+GT...` 猜，现在能进去看 `dmesg`、`logread`、`ip -s link`、`cat /sys/class/net/*/statistics/*`。

### 4.6 还没做、建议在实机上验证的

| 项目 | 说明 | 优先级 |
|---|---|---|
| USB 自动挂起 | 确认 `/sys/bus/usb/devices/*/power/control` 与 mtu3 runtime PM 未把设备挂起（RNDIS 掉线的经典原因） | 高 |
| `usbwatch` 自愈 | UDC 空绑定时自动 `usb.init restart`（默认不敢开，怕抖） | 中 |
| 温度/降频 | `thermal_core` 在高温下会限速，弱信号场景容易表现为「速率骤降」而非掉线 | 中 |
| 更换 APN 后的 PDP 复用 | 同一 cid 改 APN 需先 `CGACT=0` 再 `CGDCONT`，否则旧 profile 残留 | 中 |

---

## 五、禁用 PCIE，优先保 USB

这一版把 PCIE 数据面整体关掉，DIPC 只走 USB。原因不是"PCIE 不好"，而是这台设备的使用形态就是插 USB，PCIE 那条路只会带来误判和启动延迟。

### 5.1 原厂是怎么把 USB 弄丢的

`/etc/init.d/usb.init` 开头这三步：

```sh
dipc_config=`grep dual_ipc_mode /mnt/vendor/nvdata/md_cmn/dipc_config`
dipc_mode=${dipc_config#dual_ipc_mode:}
pcie_link_state=`cat /sys/class/pcie_ep/pcie_ep/link_state`
echo $dipc_mode > /sys/devices/platform/11201000.usb/dipc_mode
```

然后：

| `dual_ipc_mode` | 行为 |
|---|---|
| 1 且 `pcie_link_state != 0` | 写 `11` 到 `dipc_mode`，**`exit 0`，USB gadget 根本不建** |
| 1 且 link 为 0 | 继续走 USB |
| 3 / 4 | 直接走 USB |
| 其它（含空值） | `DIPC mode with NO USB`，`exit 0` |

问题在第二行：`pcie_link_state` 只要不是字符串 `0`（空、`-1`、读取失败留下的旧值都算），就判定"PCIE link 成功"，把整机数据面交给 PCIE。现场没有 PCIE 主机、但 EP 侧 PHY 有残余状态时，这就是**USB 完全不枚举**的直接原因。

### 5.2 `dual_ipc_mode=4` 到底做了什么

反汇编 `dipcd` 的模式分发表（`0x401078` 起，4 项跳转表）可以还原：

| 模式 | 动作 |
|---|---|
| 1 / 2 | `ioctl(PCIE_DIPC_IOC_SET_MODE, 1)` |
| 3 | `ioctl(PCIE_DIPC_IOC_SET_MODE, 2)` |
| **4** | `ioctl(PCIE_DIPC_IOC_SET_USB_ONLY_MODE)`，日志 `pcie_dipc mode don't care: MODE_USB_ONLY` |

模式 4 就是驱动里写死的 **USB only**：`pcie_dipc` 不再参与 DIPC 端口枚举，AT / LOG / 数据面全部走 USB 通道（`/dev/ccci_hsapif_dipc` 的 CLDMA 分支照常执行）。

而 `libdipc.so` 的 `generate_default_config()` 里，出厂默认是：

```
dual_ipc_mode         = 1
ap_logging_interface  = 2
md_logging_interface  = 2
md_at_interface       = 2
ap_pcie_port_config   = 23   (0b10111)
md_pcie_port_config   = 13   (0b01101)
```

`ap_pcie_port_config=23` 右移 1 位再取模 2 得 **1**，所以 `/etc/init.d/adbd_pcie` 默认是启用的 —— 出厂状态就是"PCIE 优先 + PCIE 上跑 ADB"，USB 只是备胎。

### 5.3 改了哪三层

| 层 | 文件 | 动作 |
|---|---|---|
| 配置 | `dipc_config`（nvdata） | `dual_ipc_mode` 强制 `4`，原文件备份为 `dipc_config.orig` |
| 启动 | `/etc/init.d/usb.init` | `FORCE_USB_ONLY=1` 时忽略 `pcie_link_state`，固定按 4 启动 gadget |
| 服务 | `/etc/init.d/adbd_pcie` | 直接短路，不再拉起；ADB 统一走 USB 侧 `adbd_usb` |

配置改写由新脚本 `/etc/init.d/pcieoff` 完成，挂两个启动点：

```
S001pcieoff        mount_early 之后、ccci_fsd/dipcd 之前，抢在 dipcd 读配置前改
S005mdcfg          文件缺失时补最小配置（AP_MD 变体，写的就是 4）
S97pcieoff_late    dipcd 若用内置默认值起过，这里强制重启一次 dipcd 让它重读
S99usb.init        绑定 UDC
```

`pcieoff` 的关键点：

- 幂等：已经是 4 就不重复写；
- 只改 `dual_ipc_mode` 一行，`awk` 逐行透传，其它字段（端口位图）原样保留；
- late 阶段无论文件是否改过，都重启一次 `dipcd` —— 因为文件一致不代表 `dipcd` 内存里是 4（首次开机文件根本还不存在）；
- 写盘后 `sync`，避免 yaffs2 掉电丢配置。

### 5.4 USB 侧的加固

`/etc/start_usb.sh` 原逻辑有两个坑，一并修了：

```sh
# 原版：等 aplog_usb_port 变成 "1 0"，100 秒等不到就 exit 0
if [ $tout -lt 0 ]; then echo "start_usb timeout"; exit 0; fi
...
echo 11201000.usb > /config/usb_gadget/g1/UDC   # 只绑一次，失败就没了
```

- 超时从 `exit 0` 改成"继续尝试绑定"：禁用 PCIE 后，MD 侧端口角色上报可能永远不出现 `1 0`，原逻辑会静默失败；
- UDC 绑定改成 10 次重试（每次 1s），失败返回 1，交给 procd 的 `respawn 3600 5 0` 重启整个脚本。

### 5.5 回滚

不改固件也能回退，两种方式：

```sh
# 方式一：恢复原配置（下次开机 pcieoff 还会改回来，需要同时关掉开关）
cp /mnt/vendor/nvdata/md_cmn/dipc_config.orig /mnt/vendor/nvdata/md_cmn/dipc_config
/etc/init.d/pcieoff stop      # 需要在 rc.d 里去掉 S001/S97 链接，否则重启后失效

# 方式二：只让 usb.init 按原厂逻辑跑（配置仍是 4，但 USB 一定会启）
sed -i 's/^FORCE_USB_ONLY=.*/FORCE_USB_ONLY=0/' /etc/init.d/usb.init
/etc/init.d/usb.init restart
```

### 5.6 验证

```sh
/etc/init.d/pcieoff status
# dual_ipc_mode    : 4  (4=USB only, 1=PCIE 优先)
# 内核 usb dipc_mode: 4
# pcie link_state  : <无节点> 或具体值，都不再影响 USB
# dipcd            : running

cat /tmp/pcieoff.log
# [pcieoff] dual_ipc_mode 1 -> 4（备份 .../dipc_config.orig）
# [pcieoff] dipcd 已重新拉起

cat /sys/class/udc/11201000.usb/state      # 应为 configured
ip -s link show ccmni0                      # 数据面计数在涨
```

---

## 六、NCM：为什么这次做不了

三个独立的硬阻塞，缺一不可：

**① 内核没有 NCM/ECM 功能，也没有 TUN、没有 NAT**

```
# CONFIG_USB_CONFIGFS_NCM is not set
# CONFIG_USB_CONFIGFS_ECM is not set
# CONFIG_USB_CONFIGFS_RNDIS is not set
CONFIG_IP_NF_NAT is not set
（CONFIG_TUN 在内核配置里根本不存在）
```

就算想办法让 AP 侧出一个 NCM 网卡，也没有 `TUN` 做用户态桥接、没有 `MASQUERADE` 做地址转换 —— 主机拿到 `192.168.x.x` 后流量出不去。

**② MD 侧没有 NCM 类**

MD 的 `usbclass/` 只有 `acm / adb / msd / rndis / niccommon`，EMBIND 的 L2P 白名单也只有 `M-CCMNI / M-RNDIS / M-MBIM / M-LHIF`。全镜像里 `NCM` 字符串为 0 处。

**③ MD 镜像是 md1rom 签名容器，不能安全注入代码**

```
0x000  md1rom 头（魔数 0x58881688）
0x600  MMM 块 #0  type=1 FILE_INFO      56 B
0x638  MMM 块 #1  type=2 MDBIN          33,554,736 B  (MT6880_DTC_FM350_GL_00_MDBIN_PCB01_...29_23_24.bin)
0x2000768  尾部 16,336,184 B（加载映射/校验表，非 ELF）
```

要加 NCM，等于在 modem RTOS 的 USB 栈里新写一个 USB class + 改 L2P 分发 + 改 ipcore 绑定 —— 这是**改固件源码**的活，不是在镜像上打几个字节能解决的；而且 MDBIN 有校验，改了大概率起不来（变砖风险高，你手上就一台设备，不值得赌）。

**唯一的正路**：拿 MTK BSP 重编内核，打开 `CONFIG_USB_CONFIGFS_NCM` + `CONFIG_TUN` + `CONFIG_IP_NF_NAT`，然后把 PDN 绑 `M-CCMNI`，AP 侧做 NCM gadget + NAT。这条路需要 MT6880 的 kernel source（BSP 授权），当前环境拿不到。

**替代方案**：维持 `M-RNDIS`（MD 侧 RNDIS，本来就是主机兼容性最好的方案），把 EMBIND 保活做扎实 —— 这版 `atproxy` 就是干这个的。等 RNDIS 稳定了，NCM 的收益（少 4 字节/包、无 RNDIS 私有控制消息）在这个场景里其实很有限。

---

## 七、交付物与验证

| 文件 | 内容 |
|---|---|
| `F31_29.23.24n_MOD_AP.zip` | AP 域加固：atproxy 2.2.0（AT 网关限速）+ ADB 全模式 + dropbear + APN 修复 + 自动拨号 + PCIE 禁用（USB 优先） |
| `F31_29.23.24n_MOD_AP_MD.zip` | 上面全部 + `mdcfg`（MD nvdata 自愈，默认写 `dual_ipc_mode:4`） |
| `fm350-mod/` | 源码、overlay、补丁、构建脚本、md1rom 解析工具 |

刷完开机验证：

```sh
# 1) 绑定是否成功
atproxy status
cat /var/log/atproxy.log        # 应看到 "URC 上报：rc=0 cid=1"

# 2) AT 网关是否起来（应有一行"已监听 /var/run/atproxy.sock"）
ps | grep atproxy
ls -l /var/run/atproxy.sock

# 3) 拨号
atproxy at 'AT+CGACT?'
atproxy at 'AT+CGCONTRDP=1'

# 4) 限速是否生效（连续 20 条，总耗时应 >= 500 ms）
time sh -c 'i=0; while [ $i -lt 20 ]; do atproxy at "AT" >/dev/null; i=$((i+1)); done'
# 更直接：看服务端侧两条命令的间隔，或观察连续下发时 MD 不出现 AT 超时

# 5) 数据面
ip -s link show ccmni0

# 6) ADB / SSH
adb devices
adb forward tcp:2222 tcp:22 && ssh -p 2222 root@127.0.0.1
```

AT 网关相关日志：

```
AT 通道就绪（kind=1）
AT 网关已监听 /var/run/atproxy.sock（最小间隔 25ms，队列上限 64）
配置启用自动拨号，启动拨号线程（与绑定共用单一 AT 通道）
数据面 cid 固定为 2（绑定侧会跟随）
```

如果看到 `已有 atproxy 实例在跑，本进程退出`，说明 procd 之外还手工起了一个，
或者旧版 init 同时拉了 `run` + `dial` 两个 instance —— 用
`/etc/init.d/atproxy restart` 收掉。

### 7.1 已知限制

| 限制 | 说明 | 处理 |
|---|---|---|
| 只保护经过网关的 AT | 直连 `/dev/atci-service` 的插件仍能与 atproxy 并发打到 MD | 把插件 AT 路径改成 `/var/run/atproxy.sock`；或停掉 atproxy 让插件独占 |
| `atci_service` 多客户端行为未知 | 闭源，无法确认它是否把多路客户端串行转发给 atcid | 用上面的办法收口；日志里若出现 AT 超时，先查还有谁连着 atci_service |
| 外部长命令超时 | 默认 3 s，`AT+CFUN=1`、`AT+GT...` 这类可能不够 | 调大 `at_cmd_timeout`，或改用 `atproxy at` 单独执行 |
| 超长命令 | 单条 > 255 字节直接丢弃并回 `+ATPROXY: TOOLONG` | 拆分下发；不要截断转发（可能触发 NV 写坏） |

查还有谁连着 atci_service：

```sh
ino=$(awk '/atci-service/{print $(NF-1)}' /proc/net/unix)
for f in /proc/[0-9]*/fd; do
    ls -l "$f" 2>/dev/null | grep -q "socket:\[$ino\]" && \
        echo "${f%/fd} $(cat ${f%/fd}/comm 2>/dev/null)"
done
```

正常只应看到 atproxy 自己（以及 atci_service 自身）。多出来的进程就是绕过限速的那一路。
