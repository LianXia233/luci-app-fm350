# f22-atproxy — F22 AP 侧 EMBIND 强制绑定守护 + USB 通道 ADB 启用补丁（v2）

FM350-F22 固件（root.squashfs）改造包。两个部件：

1. **atproxy v2**（`/usr/bin/atproxy`）：由模组自身在内部下发
   `AT+EMBIND=1,"M-RNDIS",<cid>`，把 RNDIS 数据通道强制绑定到激活 PDN
   （主机侧发同一条 AT 不生效，见下节）。**v2 起不再订阅 EIF_IND**，原因见下。
2. **usb.init ADB 补丁**：三个 boot_mode 分支统一启用 `adbd_usb`，
   使 AP 域免认证 root ADB 在任何启动模式下可用。

**透传原始 AT：否。** atproxy 唯一的下行写命令是 EMBIND；不碰 APN（CGDCONT）、
不碰 IMEI（EGMREXT）相关命令，不暴露 NetIF id。

## v2 变更摘要（相对 v1）

| 项 | v1 | v2 | 原因 |
|---|---|---|---|
| EIF_IND(0x4205) 订阅 | 开启，转发为 `+GT*` URC | **关闭**（`ENABLE_EIF_SUBSCRIBE 0`） | MIPC IND 疑似单注册者：atproxy 与 mtk_netagent 注册同一 msg_id 时互顶，atproxy（START=99，晚启动）抢注导致 netagent 失聪，RNDIS 数据面装配流程死（1.0.11 无 atproxy 正常，刷 atproxy 后 eth2 tx=3/rx=0 全哑，时间线吻合） |
| EMBIND 强制绑定 | 事件 + 保活 | **保留**（启动 20s 首绑 + 120s 保活 + 15s 最小间隔） | 数据面恢复主路径 |
| usb.init | 原厂（仅 boot_mode 0000 启动 adbd_usb） | **三分支统一启用 adbd_usb**；0001 分支补 `ffs.adb` function | 调试通道随固件常开 |
| 产物 | `fm350-f22-root-embind.squashfs` | `fm350-f22-root-embind3.squashfs` | — |

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

## 构建（云服务器，musl 交叉编译）

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
- 重打包（与出厂参数对齐）：

```sh
mksquashfs rootfs fm350-f22-root-embind3.squashfs \
    -noappend -comp xz -b 262144 -no-xattrs -all-root -no-exports -no-progress
```

产物链：`fm350-f22-root-embind.squashfs`（v1）→ `embind2`（atproxy v2）→
**`embind3`（atproxy v2 + ADB 三分支启用，本次交付）**，
md5 `b8e6a8a62c468b11a06439219ab8d3af`（20738048 B）。
抽包校验：atproxy md5 `b0ea125a82a536130f2ab00557dd345a`（v2 不变）、
usb.init md5 `d43fbe5fbd3b878a5a8d726dc3475f92`、文件清单与基线 identical。

### 已知问题

- ttyGS1 对主机侧 ttyUSB 映射不成立：130s 全 ttyUSB 监听未捕获任何 `+GTEMBIND`/
  `+GTEIF`（仅 ttyUSB1 有 GNSS URC）。v2 URC 的主机侧观测途径待定；
  EMBIND 生效以 `AT+EMBIND?` 回读与 eth2 数据面为准。
- EIF_IND 互顶为最强嫌疑假设（时间线与症状吻合），`ENABLE_EIF_SUBSCRIBE=1`
  的复现开关保留在源码中，供后续刷入对照实验。
