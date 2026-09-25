# f22-atproxy — F22 AP 侧 EMBIND 强制绑定守护 + USB 通道 ADB 启用 + dropbear SSH（v3）

FM350-F22 固件（root.squashfs）改造包。三个部件：

1. **atproxy v2**（`/usr/bin/atproxy`）：由模组自身在内部下发
   `AT+EMBIND=1,"M-RNDIS",<cid>`，把 RNDIS 数据通道强制绑定到激活 PDN
   （主机侧发同一条 AT 不生效，见下节）。**v2 起不再订阅 EIF_IND**，原因见下。
2. **usb.init ADB 补丁**：三个 boot_mode 分支统一启用 `adbd_usb`，
   使 AP 域免认证 root ADB 在任何启动模式下可用。
3. **dropbear SSH**（v3 新增，embind4）：静态编译 dropbear 2024.86 +
   空密码放行补丁 + procd init，经 `adb forward` 隧道转出 AP 域 SSH。

**透传原始 AT：否。** atproxy 唯一的下行写命令是 EMBIND；不碰 APN（CGDCONT）、
不碰 IMEI（EGMREXT）相关命令，不暴露 NetIF id。

## v2/v3 变更摘要（相对 v1）

| 项 | v1 | v2 | v3 | 原因 |
|---|---|---|---|---|
| EIF_IND(0x4205) 订阅 | 开启，转发为 `+GT*` URC | **关闭**（`ENABLE_EIF_SUBSCRIBE 0`） | 同 v2 | MIPC IND 疑似单注册者：atproxy 与 mtk_netagent 注册同一 msg_id 时互顶，atproxy（START=99，晚启动）抢注导致 netagent 失聪，RNDIS 数据面装配流程死（1.0.11 无 atproxy 正常，刷 atproxy 后 eth2 tx=3/rx=0 全哑，时间线吻合） |
| EMBIND 强制绑定 | 事件 + 保活 | **保留**（启动 20s 首绑 + 120s 保活 + 15s 最小间隔） | 同 v2 | 数据面恢复主路径 |
| usb.init | 原厂（仅 boot_mode 0000 启动 adbd_usb） | **三分支统一启用 adbd_usb**；0001 分支补 `ffs.adb` function | 同 v2 | 调试通道随固件常开 |
| SSH 服务 | 无 | 无 | **植入 dropbear**（静态 2024.86 + `-B` 空密码补丁） | AP 域 shell 经 USB 转出（SSH 协议） |
| 产物 | `fm350-f22-root-embind.squashfs` | `fm350-f22-root-embind3.squashfs` | `fm350-f22-root-embind4.squashfs` | — |

v2 下 URC 输出仅剩 `+GTEMBIND: rc=<n>,cid=<n>,why=<startup|keepalive>`；
v1 的 `+GTNORA/+GTIFST/+GTIFADDR` 系列（EIF 事件降维）随订阅一并停用。
源码内 `ENABLE_EIF_SUBSCRIBE` 改回 `1` 可恢复订阅（排查 IND 互顶问题时用）。

## USB 通道（M-RNDIS）强制绑定

### 为什么必须由模组自己做

主机侧实测（2026-09-25，192.168.10.1 / FM350-GL）：

| 现象 | 证据 |
|---|---|
| 绑定表为空 | `AT+EMBIND?` 只回 `OK`，无 `+EMBIND:` 行 |
| 主机侧写入不生效 | `AT+EMBIND=1,"M-RNDIS",1` 首次回 `OK`、再次回**空响应**，回读依旧为空 |
| 数据面全哑 | eth2 上全目的 IP ARP 均 `FAILED`；DHCP Discover 无任何应答；memset 静态邻居后 ICMP 仍 100% 丢包 |
| 模组不给主机 DHCP/RA | 无 DHCPv4 应答；无 RA（主机侧 `Icmp6InRouterAdvertisements=0`） |
| 写入成功条件 | 仅当模组已附网（CEREG=0,1）后写入成功并回读 `+EMBIND: 1,"M-RNDIS",1` |

结论：RNDIS 通道的绑定必须由**模组内部**在正确的时机（PDN 就绪）下发。

### 实现

- 通道：`mipc_sys_at_sync(ps_id=0, &st, cmd)` —— `libmipc_api.so` 导出符号。
  反汇编实证：`@0x1a6b4` 校验参数后跳公共调用器 `@0x186e0`，
  流程为 `mipc_msg_init` → `mipc_msg_add_tlv(msg, 0x8100, len(cmd), cmd)` → `sync_timeout`，
  结果写回 `st`（首 4 字节为结果码）。
- **C++ ABI 坑**：`libmipc_api.so` 符号带 Itanium mangling，`libmipc_msg.so` 是 C ABI。
  C 源码须用 asm label 绑定 mangled 名，否则链接期 `undefined reference`：

```c
extern int mipc_sys_at_sync(int ps_id, void *at_st, const char *cmd)
    __asm__("_Z16mipc_sys_at_sync19mipc_sim_ps_id_enumP18mipc_sys_at_structPKc");
```

- 触发时机：启动后 20s（等 gadget 与 PDN 就绪）、120s 周期保活
  （v2 事件触发随订阅关闭，`ENABLE_EIF_SUBSCRIBE=1` 时恢复 EIF 事件触发，
  事件触发受 15s 最小间隔约束防风暴）。
- **线程安全**：EIF 回调（如启用）运行在 MIPC 接收线程内，回调里再调同步 API
  有自锁风险，因此回调只置标志，真正的下发由主循环执行。
- 绑定结果以 `+GTEMBIND: rc=<n>,cid=<n>,why=<...>` 吐给主机侧（URC 写 `/dev/ttyGS1`，
  v1 时段实测 ttyGS1 对主机侧映射不成立，主机侧捕获见「已知问题」）。
- 现场可调：导出 `EMBIND_CID`（1..16）覆盖默认 cid=1；非法值回落默认。

### 红线

命令字符串只含 EMBIND（`strings atproxy` 断言：无 CGDCONT / EGMREXT / register_ind
痕迹，`REDLINE-CLEAN`）。**不下发 CGDCONT（APN）**（换卡场景下会把模组 APN 改写错，
直接断数据），**不下发任何 IMEI 相关命令**。

## ADB 启用补丁（usb.init 三分支）

### 背景：F22 的两条 ADB 通道

| 通道 | 归属 | 启用条件 | 调试对象 |
|---|---|---|---|
| MD 域 ADB | MD 侧组合自带（`usbclass/adb`，`adb_core.c`） | 随 `GTUSBMODE` 组合存在；实机枚举接口 `2-1:1.5 cls=ff/42/01`（标准 ADB 签名） | MD RTOS 内部 shell |
| AP 域 ADB（adbd_usb） | AP Linux rootfs（`/sbin/adbd_usb` + configfs `ffs.adb`） | usb.init 建组合（`dipc_mode` 3/4 或 PCIe 链路失败路径）且 boot_mode 分支启动 adbd_usb | **AP Linux root shell（免认证 root，本补丁目标）** |

当前实机为 MD master 模式（GTUSBMODE 41，PID 0x7127：RNDIS + 7 serial + ADB）：
USB 控制器归 MD，`start_usb.sh` 检测 MD 枚举完成后仅在 UDC 空闲时绑定 AP gadget
——此模式下 AP 域 ADB 不经 USB 暴露，MD 域 ADB（接口 1.5）是唯一可用通道。

### 补丁内容（`scripts/patch_usbinit_adb.py`，共 5 行新增）

原厂 `usb.init`（`/etc/init.d/usb.init`，START=90）boot_mode 分支：

| boot_mode | PID | 原厂组合 | 原厂 adbd_usb | 补丁后 |
|---|---|---|---|---|
| `0000` | 0x7127 | acm.gs0-2 + ffs.adb (+ mass_storage) | 启动 | 不变 |
| `0001` | 0x7128 | acm.gs0-3 | **不启动，无 ADB** | 补 `f5=ffs.adb` 链接 + 启动 adbd_usb |
| 其他 | 0x7127 | acm.gs0-2 + ffs.adb | **不启动**（接口枚举但功能死） | 补启动 adbd_usb |

补丁 diff（对原厂文件，仅新增行）：

```diff
             ln -sf /config/usb_gadget/g1/functions/acm.gs3 /config/usb_gadget/g1/configs/b.1/f4
+            ln -sf /config/usb_gadget/g1/functions/ffs.adb /config/usb_gadget/g1/configs/b.1/f5
+            echo "start adbd_usb"
+            /sbin/adbd_usb 1000>&- &
             ;;
             ln -sf /config/usb_gadget/g1/functions/ffs.adb /config/usb_gadget/g1/configs/b.1/f4
+            echo "start adbd_usb"
+            /sbin/adbd_usb 1000>&- &
             ;;
```

functionfs 挂载（`mount -t functionfs adb /dev/usb-ffs/adb`）在三分支共享的
公共路径（原厂第 99 行），无需改动。

### adbd_usb 免认证 root 逆向定案

F22 固件自带 `/sbin/adbd_usb`（aarch64，静态链接 musl），反汇编定案：

1. **免认证已内建，无需任何补丁。**
   `property_get("ro.adb.secure", buf, "0")` 默认值编译为 `"0"`（vaddr 0x410210 附近），
   `auth_flag = (strcmp(buf, "1") == 0)` 恒为 false。OpenWrt 无 property service，
   属性永远取默认值，认证分支永不激活。
2. **adb shell 天然就是 root shell，无需 `adb root` 提权。**
   - adbd_usb 全二进制无 setuid/setgid 调用（仅 1 处 setgroups @0x407970，
     属 tcp 服务重启路径，非降权）。
   - 进程由 init 以 uid 0 拉起，shell 服务子进程继承 root。
   - `adb root` 命令处理（`restart_root_service` @0x40b670）与 AOSP 原版一致：
     `getuid()==0` 时直接回写 `adbd is already running as root`。
3. 二进制自包含：打开 `/dev/usb-ffs/adb/ep0..ep2`，日志走 `/tmp/adb.log`。
4. **完整 AOSP service 面**（v3 补充静态确认）：`shell:` / `forward:` / `reverse:` /
   `tcp:` / `localabstract:` / `sync:` / `exec:` / `root:` / `dev:` / `framebuffer:` /
   `jdwp:` 全在，shell 落地 `/bin/sh`，另有 `persist.adb.tcp.port`（网络监听能力，
   RNDIS 修好前无路径）。`adb shell` / `adb push|pull` / `adb forward` 均可用。

### 刷入后验证

```sh
# AP 域（需 AP 拿到 USB 属主的模式；MD master 模式下此口不经 USB 暴露）
adb shell                     # 应免认证直接进入 root shell
id                            # 期望 uid=0(root)
logread | grep -E 'atproxy|EMBIND'
/etc/init.d/atproxy status

# MD 域（MD master 模式下即可用，接口 1.5，零固件依赖）
adb devices                   # 应列出模组（0e8d:7127）
adb shell                     # MD 侧 shell
```

主机侧（OpenWrt/ImmortalWrt）需安装 adb 客户端（`apk add adb` / `opkg install adb`）；
ADB 走用户态 usbfs，无需内核驱动，接口 `ff/42/01` 显示未绑定内核驱动属正常。

## SSH 服务植入（dropbear，v3 / embind4）

### 定位与通道

AP 域 rootfs 原本没有任何 SSH 组件（`etc/init.d/` 与 `usr/sbin/` 无 dropbear，
仅 LuCI 前端 `dropbear.js`/`sshkeys.js` 静态资源残留——工厂裁掉了服务端、留了页面；
用户体系则完整：`etc/passwd` root 无哈希 shell=/bin/ash、shadow/group/inittab/
ttyS0 login.sh/urngd 熵源全在）。embind4 植入静态 dropbear，经 ADB 隧道转出：

```sh
adb forward tcp:2222 tcp:22     # ADB 隧道 → AP 域 127.0.0.1:22
ssh -p 2222 root@127.0.0.1      # 空密码回车，直接进入 AP root shell
adb forward tcp:8080 tcp:80     # 附带能力：LuCI(uhttpd) 同法转出
```

为什么必须走 ADB 隧道：AP 内核（4.19.205）gadget 网络类零编译
（无 u_ether/usb_f_ncm/usb_f_ecm/usb_f_rndis/libcomposite）且 MODVERSIONS=y
拒自编 ko，「USB 网口 + SSH」路不通（详见逆向与移植方案报告三道墙定案）；
ADB forward 是唯一 TCP-over-USB 用户态隧道。

### dropbear 构建（云服务器，musl 静态交叉）

- 版本：dropbear 2024.86（tag `DROPBEAR_2024.86`）
- 注意：GitHub release 无 tarball 附件，源码经 `git clone --branch` 获取；
  **必须在 Linux 侧 clone**（Windows `core.autocrlf` 检出的 CRLF 会让
  `./configure` 报 `cannot execute: required file not found`）
- 编译：

```sh
cd dropbear-src
CC=/opt/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc \
  ./configure --host=aarch64-linux-musl --disable-zlib --disable-pam
make PROGRAMS='dropbear' STATIC=1 -j4
aarch64-linux-musl-strip dropbear
```

- 产物：347280 B，ELF64/AArch64，readelf -d 无 NEEDED（纯静态，
  bundled libtomcrypt/libtommath，零 rootfs 新依赖），
  md5 `9a2da24b56dd338dc2572bcf5023d7da`（含下述补丁）。

### 空密码补丁（`scripts/patch_dropbear_blankpass.py`）

上游两个事实：

1. `svr_auth_password()`（`src/svr-authpasswd.c`）对空 shadow 哈希
   （`passwdcrypt[0] == '\0'`）**无条件拒绝**；
2. `-B`（`svr_opts.allowblankpass`）只作用于 SSH `none` 方法
   （`src/svr-auth.c:127`），而 OpenSSH 客户端不会主动发 none。

两者叠加 = 即使加 `-B`，空哈希的 root 依然无法用 OpenSSH 登录。
补丁把 password 方法的空哈希分支改为「`-B` 开启时放行、否则维持拒绝」，
与 OpenWrt 出厂空密码直登行为对齐：

```c
	if (passwdcrypt[0] == '\0') {
		if (!svr_opts.allowblankpass) {
			/* 原行为：rejected */
			return;
		}
		/* F22: blank shadow hash + -B allows login. USB-only surface,
		 * same trust level as the always-on adbd root shell. */
		send_msg_userauth_success();
		return;
	}
```

补丁脚本为行级精确替换（`count==1` 断言 + 落盘复核），已随源码重编译验证。

### 安全模型

| 项 | 说明 |
|---|---|
| 空密码登录 | root 在 shadow 中无哈希（`root::`），`-B` 放行——暴露面与 adbd_usb 免认证 root 完全一致（仅 USB 可达），不新增攻击面 |
| 设密码后 | `passwd` 设置 root 密码后空哈希条件消失，`-B` 自动失效，密码校验接管，init 脚本无需改动 |
| host key | `-R` 首连接自动生成于 `/etc/dropbear/`（tmpfs，重启重生成，known_hosts 提示变更属预期） |
| 监听 | `-p 22` 绑 0.0.0.0；USB 隧道之外无可达路径（AP 域无下行网络口） |

### 植入清单与运行参数

| 文件 | 权限 | 说明 |
|---|---|---|
| `/usr/sbin/dropbear` | 755 | 静态二进制（见上） |
| `/etc/init.d/dropbear` | 755 | procd init（START=95，`respawn 3600 5 0`） |
| `/etc/config/dropbear` | 644 | UCI 占位（LuCI dropbear 页读取） |

运行参数：`dropbear -F -E -p 22 -B -R -T 3`（前台交 procd、stderr 日志、
端口 22、允许空密码、自动生成 host key、认证尝试限 3 次）。

### 刷入后验证（SSH 通道）

```sh
# 0) 前置：adb shell（路 A）确认 dropbear 已被 procd 拉起
adb shell
ps | grep dropbear                 # 应见 /usr/sbin/dropbear -F -E ...
netstat -lnt | grep ':22 '         # 0.0.0.0:22 LISTEN

# 1) 隧道 + 登录（主机侧）
adb forward tcp:2222 tcp:22
ssh -p 2222 root@127.0.0.1         # 密码提示直接回车
id                                 # uid=0(root)

# 2) 兜底
#    SSH 不通时 adb shell 永远可用；
#    /etc/init.d/dropbear restart && logread | grep dropbear 排查
```

## 构建（atproxy，云服务器，musl 交叉编译）

```sh
CC=/opt/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc
$CC -O2 -Wall -Wextra -o atproxy.new atproxy.c \
    -L<rootfs>/usr/lib -lmipc_api -lmipc_msg -Wl,-rpath,/usr/lib
aarch64-linux-musl-readelf -d atproxy.new   # 确认 NEEDED 仅 libmipc_api/libmipc_msg/libc
```

也可用仓库内 Makefile：`make MIPC_DIR=../libs-f22`。

## 部署（rootfs 注入）

- 二进制 → `/usr/bin/atproxy`
- init 脚本 → `/etc/init.d/atproxy`（`START=99`，晚于 usb.init；`respawn 3600 5 0`）
- usb.init → `scripts/patch_usbinit_adb.py`（行级精准插入，`sh -n` 校验）
- dropbear 三件套 → `files/dropbear.init`、`files/dropbear.config` +
  静态二进制（补丁编译，见上节；`scripts/patch_dropbear_blankpass.py`）
- 重打包（与出厂参数对齐）：

```sh
mksquashfs rootfs fm350-f22-root-embind4.squashfs \
    -noappend -comp xz -b 262144 -no-xattrs -all-root -no-exports -no-progress
```

产物链：`fm350-f22-root-embind.squashfs`（v1）→ `embind2`（atproxy v2）→
`embind3`（atproxy v2 + ADB 三分支启用）→
**`embind4`（+ dropbear SSH，本次交付）**，
md5 `40bb5171630564154664b64431d10a5a`（20889600 B）。
抽包校验：dropbear md5 `9a2da24b56dd338dc2572bcf5023d7da`、
atproxy md5 `b0ea125a82a536130f2ab00557dd345a`（v2 不变）、
adbd_usb `9ee3e4396b0f831f1356f4adbfdbd9b6`（不变）、
usb.init md5 `d43fbe5fbd3b878a5a8d726dc3475f92`（不变）、
文件清单 = 基线 2129 + 3 植入（2132）。

### 已知问题

- ttyGS1 对主机侧 ttyUSB 映射不成立：130s 全 ttyUSB 监听未捕获任何 `+GTEMBIND`/
  `+GTEIF`（仅 ttyUSB1 有 GNSS URC）。v2 URC 的主机侧观测途径待定；
  EMBIND 生效以 `AT+EMBIND?` 回读与 eth2 数据面为准。
- EIF_IND 互顶为最强嫌疑假设（时间线与症状吻合），`ENABLE_EIF_SUBSCRIBE=1`
  的复现开关保留在源码中，供后续刷入对照实验。
- dropbear host key 重启重生成（tmpfs），strict host key checking 的客户端
  每次刷机后首次连接需确认或用 `-o UserKnownHostsFile=/dev/null`。
