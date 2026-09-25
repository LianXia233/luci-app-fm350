# f22-atproxy — F22 AP 侧 EIF 语义代理 + USB 通道强制绑定

FM350-F22 固件（root.squashfs）内的用户态守护进程。两件事：

1. 订阅 MIPC `EIF_IND(0x4205)`，把网络事件降维成告知性 `+GT*` URC 写到
   `/dev/ttyGS1`（主机侧对应 `/dev/ttyUSB1`），供 luci-app-fm350 被动消费。
2. 由模组自身在内部下发 `AT+EMBIND=1,"M-RNDIS",<cid>`，把 RNDIS 数据通道
   强制绑定到激活 PDN（主机侧发同一条 AT 不生效，见下节）。

**透传原始 AT：否。** 唯一的下行写命令是 EMBIND；不碰 APN（CGDCONT）、
不碰 IMEI（EGMREXT）相关命令，不暴露 NetIF id。

## 设计边界

- 只订阅 IND（`mipc_msg_register_ind_api(0x10, 0x4205, cb, NULL, NULL)`）。
- 下行仅一条白名单命令 `AT+EMBIND=1,"M-RNDIS",<cid>`，经 `mipc_sys_at_sync` 走 MIPC 内部通道。
- 输出全部为告知性 URC，reason 字段剥离 CR/LF 防注入。
- 仅链接固件自带库：`libmipc_msg.so` / `libmipc_api.so` + musl libc，rpath `/usr/lib`。

## USB 通道（M-RNDIS）强制绑定

### 为什么必须由模组自己做

主机侧实测（2026-09-25，192.168.10.1 / FM350-GL）：

| 现象 | 证据 |
|---|---|
| 绑定表为空 | `AT+EMBIND?` 只回 `OK`，无 `+EMBIND:` 行 |
| 主机侧写入不生效 | `AT+EMBIND=1,"M-RNDIS",1` 首次回 `OK`、再次回**空响应**，回读依旧为空 |
| 数据面全哑 | eth2 上全目的 IP ARP 均 `FAILED`；DHCP Discover 无任何应答；memset 静态邻居后 ICMP 仍 100% 丢包 |
| 模组不给主机 DHCP/RA | 无 DHCPv4 应答；无 RA（主机侧 `Icmp6InRouterAdvertisements=0`） |

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

- 触发时机：启动后 20s（等 gadget 与 PDN 就绪）、每次 EIF `ifst`/`ipadd`/`ipdel`
  事件、以及 120s 周期保活；事件触发受 15s 最小间隔约束防风暴。
- **线程安全**：EIF 回调运行在 MIPC 接收线程内，回调里再调同步 API 有自锁风险，
  因此回调只置标志，真正的下发由主循环执行。
- 绑定结果以 `+GTEMBIND: rc=<n>,cid=<n>,why=<startup|eif-event|keepalive>` 吐给主机侧。
- 现场可调：导出 `EMBIND_CID`（1..16）覆盖默认 cid=1；非法值回落默认。

### 红线

命令字符串只含 EMBIND。**不下发 CGDCONT（APN）**（换卡场景下会把模组 APN 改写错，
直接断数据），**不下发任何 IMEI 相关命令**。


## URC 输出表（EIF reason 降维映射）

| EIF reason            | URC 输出                                          | 含义           |
|-----------------------|---------------------------------------------------|----------------|
| `no_ra_initial`       | `+GTNORA: initial`                                | 首次 RA 到达   |
| `no_ra_refresh`       | `+GTNORA: refresh`                                | RA 刷新        |
| `mtu`                 | `+GTIFMTU: <mtu>`                                 | MTU 变更       |
| `ifst`                | `+GTIFST: cause=<n>`                              | 接口状态变化   |
| `ipadd` / `ipdel`     | `+GTIFADDR: <verb>,cause=<n>,v4cnt=<n>,v6cnt=<n>` | 地址事件汇总   |
| （随附）              | `+GTIFADDR4: <a.b.c.d>`（每个 V4 地址一行）        | 具体地址       |
| （随附）              | `+GTIFADDR6: <v6addr>`（每个 V6 地址一行）         | 具体地址       |
| 其他                  | `+GTIFEVT: <reason>,cause=<n>`                    | 兜底原始 reason|
| （强制绑定）          | `+GTEMBIND: rc=<n>,cid=<n>,why=<...>`             | 每次下发绑定后的结果 |

地址行自带完整信息，不依赖与汇总行的行序归属；URC 流被 atcid 插行也不破坏解析。

## EIF 地址块布局（netagent 反汇编实证，0x4049c8 / 0x404904）

- tag `0x810c` = V4 地址块、`0x810e` = V6 地址块；tag 以 int16_t 语义传参
  （netagent 对 0x81xx 一律 MOVN 符号扩展加载 `0xFFFF810C`），atproxy 同形。
- 条目 stride `0x14` = 20 字节；地址起始偏移 4：
  V4 取 `entry[4..7]`（netagent `sprintf("%u.%u.%u.%u", e[4],e[5],e[6],e[7])`），
  V6 取 `entry[4..19]`（16 字节）。
- `entry[0..3]` 头部语义 netagent 未使用，不外发。
- 单事件最多外发 16 个地址（`EIF_ADDR_MAX`，防异常计数刷屏）；计数与块缺失
  不一致时只发汇总行，绝不编造地址。

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
- 重打包（与出厂参数对齐，产物见 `outputs/fm350-f22-root-embind.squashfs`）：

```sh
mksquashfs rootfs fm350-f22-root-embind.squashfs \
    -noappend -comp xz -b 262144 -no-xattrs -all-root -no-exports -no-progress
```

### 刷入后验证（adb shell，免认证 root）

```sh
logread | grep -E 'atproxy|EMBIND'     # 期望见 EMBIND(...) rc=... resp="...OK..."
cat /dev/ttyGS1 &                      # 观察 +GTEMBIND URC（主机侧 /dev/ttyUSB1）
# 主机侧（OpenWrt）：
#   AT+EMBIND? 应开始返回 +EMBIND: <n>[,"M-RNDIS",<cid>]
#   eth2 上 ARP/DHCP 应开始有应答，默认路由可解析网关
```

## 实机验证前置条件：adbd_usb 逆向结论（已定案）

F22 固件自带 `/sbin/adbd_usb`（aarch64，静态链接 musl），反汇编定案如下：

1. **免认证已内建，无需任何补丁。**
   `property_get("ro.adb.secure", buf, "0")` 默认值编译为 `"0"`（vaddr 0x410210 附近），
   `auth_flag = (strcmp(buf, "1") == 0)` 恒为 false。OpenWrt 无 property service，
   属性永远取默认值，认证分支永不激活。

2. **adb shell 天然就是 root shell，无需 `adb root` 提权。**
   - adbd_usb 全二进制**无 setuid/setgid 调用**（仅 1 处 setgroups @0x407970，
     属 tcp 服务重启路径，非降权）。
   - 进程由 procd init 以 uid 0 拉起，shell 服务子进程继承 root。
   - `adb root` 命令处理（`restart_root_service` @0x40b670）与 AOSP 原版一致：
     `getuid()==0` 时直接回写 `adbd is already running as root` 并关闭连接。
   - `ro.debuggable` 判定链（@0x40ae90）仅在 `getuid()!=0` 时可达，root 启动场景不触发；
     其 `property_get` 默认值为空串（vaddr 0x410c70），即使触发也不会通过 `"1"` 比对——
     但这不影响本场景。

3. **验证步骤（刷入修改后的 rootfs 后）**：

```sh
adb shell                     # 应免认证直接进入 root shell
id                            # 期望 uid=0(root)
cat /dev/ttyGS1 &             # 挂后台观察 atproxy URC 输出
/etc/init.d/atproxy status    # procd 服务状态
# 触发 EIF_IND：重新拨号 / 等待 RA 刷新，观察 +GTNORA / +GTIFADDR 等 URC
```

4. **注意**：usb.init 的 `boot_mode` 分支需确认 `0001` 模式下 adb function 是否注入；
   若设备默认 `0000` 模式无 adb，则改用 `0000` 模式验证或补 usb.init 配置。
