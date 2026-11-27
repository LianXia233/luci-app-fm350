# FM350-GL F32 修改固件发布说明（29.24.02 F32，RW-OTA）

> 由 [LianXia233/luci-app-fm350](https://github.com/LianXia233/luci-app-fm350) 项目维护发布，基于 Fibocom 官方 `RW-OTA_29.24.02_F32`，仅替换 AP 侧 `root.squashfs`。

> 本文仅适用于 **29.24.02 F32（RW-OTA 基线）** 这一条固件线。仓库另有 **F27（no-OTA）** 与 **F31** 分支，修改范围与镜像不同，请勿交叉套用校验值与刷机步骤。

## 下载（本 Release 附件）

| 文件 | 说明 | 大小 |
| --- | --- | --- |
| `RW-OTA_29.24.02_F32.zip` | 修改版固件本体（22 个文件，扁平结构） | 50,817,497 B |
| `SP_Flash_Tool_v6.2124_Win.rar` | SP Flash Tool V6（MTK 线刷工具） | 34,797,992 B |

Release：<https://github.com/LianXia233/luci-app-fm350/releases/tag/firmware-29.24.02_F32_patched>

> [!WARNING]
> 刷入前请先记录原始 IMEI / SN（`AT+CGSN`、`AT+CFSN`）。本包只写 `rootfs` 分区，不会清 NV，但任何线刷操作都存在误操作风险。

## 0. 与 F27 固件线的区别

| 项目 | F27（no-OTA） | F32（本版，RW-OTA） |
| :--- | :--- | :--- |
| 基线 | 官方 `no-OTA_29.24.02_F27` | 官方 `RW-OTA_29.24.02_F32` |
| atproxy v2（RNDIS 绑定） | 有 | **无** |
| dropbear SSH | 有 | **无** |
| LuCI 相关组件 | 有 | **无**（AP 侧保持原厂功能集） |
| `usb.init` ADB 修复 | boot_mode 三分支启用 `adbd_usb` | dipc 回退写序 + functionfs 无条件补挂 + `adbd_usb` 清理重拉 |
| `fibocom_modem_at_app` FD 泄漏 | 未修 | **已修（两处）** |
| `fm350-guard` / `fm350-diag` | 无 | **有**（Rust + Tokio，静态 musl） |
| OTA 分区 | `OEM/OP_OTA` 为空白镜像 | 保留官方 RW-OTA 定制内容 |

需要 RNDIS 数据面绑定与 AP 域 shell 的设备请继续使用 F27 线；本版聚焦于 AP 侧稳定性与可观测性。

## 1. 修改内容

修改范围仅限 AP 侧 `root.squashfs`。`modem.img`、`dsp.bin`、`boot.img`、`lk.img`、preloader、TEE 与 OEM/OP OTA 分区保持官方原样。

### 1.1 USB / adb 三层修复

1. **dipc 回退写序**：原逻辑先把（可能为空/非法的）`dipc_mode` 写进
   `/sys/devices/platform/11201000.usb/dipc_mode`，再仅修改变量；现改为回退到 `1`
   之后再写 sysfs，避免驱动拿到空值。
2. **functionfs 无条件补挂**：`/config/usb_gadget/g1` 已存在时（`usb.init restart`
   场景），原脚本会整段跳过建 gadget 与 `mount -t functionfs adb`，导致 `adbd_usb`
   打不开 `/dev/usb-ffs/adb`、主机侧 adb 长期 `offline`。现每次启动都按
   `/proc/mounts` 检查并补挂。
3. **adbd 清理重拉**：启动前 `killall adbd_usb`，清除卡在失效 functionfs 上的实例，
   再由 boot_mode 分支重新拉起；`START=99` 与 `S99usb.init` 保持一致。

配套脚本：`/etc/start_usb.sh`（180s 超时后仍继续绑定 UDC、UDC 自检重试、adbd 兜底）、
`/etc/init.d/usbdiag`（早启状态采集，写入 `/tmp/usbdiag.log` 与 `/overlay/usbdiag.log`）。

### 1.2 `fibocom_modem_at_app` 文件描述符泄漏

原厂 F32 的 `fibocom_modem_at_app` 存在两处 `fopen` 后不 `fclose` 的路径：

| 地址 | 文件 | 触发条件 |
| :--- | :--- | :--- |
| `0x4280b8` | `/etc/CarrierIDMappingTable.xml` | 运营商映射匹配成功即返回，跳过 `fclose` |
| `0x41f97c` | `/etc/vendor_info` | 读取 `AP_VERSION` 后所有返回路径均未 `fclose`（6 个调用点） |

高频轮询 `AT+CGMR` / 状态查询会持续累积 fd，最终 `EMFILE`，模组打开温度文件失败、
状态读数丢失。修复方式：在 RX 段之后的零填充空洞放置 trampoline，并把 `PT_LOAD`
扩至 `0x3ff60`，**不搬移、不改写任何原有指令**。

### 1.3 `fm350-guard` / `fm350-diag`

Rust + Tokio 实现的 AP 侧特权组件，AArch64 静态 musl，无运行时依赖。

**守护（`/usr/bin/fm350-guard`，procd `S99fm350-guard`）**

- UDC 长时间未绑定 → `/etc/init.d/usb.init restart`（限流：5 分钟 1 次、每小时 3 次）
- adbd 健康判据为「进程存在 **且** functionfs 已挂载」，不健康时
  `killall adbd_usb` → `mount` → `respawn`
- 映射表 fd 超阈值告警；超高阈值重启 `fibocom_modem_at_app`（限流）
- 日志 `/overlay/fm350-guard.log`，带大小上限与轮转

**控制通道**

- Unix socket `/var/run/fm350-guard.sock`，权限 `0600`（仅 root 可连）
- 自有帧协议 `FMG1` / `FMR1`：`ping` / `status` / `fd` / `exec` / `log`
- 默认 `require_token=0`（无鉴权）；可改为 `require_token=1` 并放置 32 字节 token
- 所有 `exec` 写入 `/overlay/fm350-audit.log`（退出码、耗时、输出长度、命令前 200 字符）

**客户端（`/usr/bin/fm350-diag`）**

```sh
fm350-diag status                    # 本地快照（无需守护）
fm350-diag fd                        # fibocom_modem_at_app 的 fd 明细
fm350-diag carrier [查询词]          # 解析 CarrierIDMappingTable.xml
fm350-diag ping | exec "..." | ctl-log -n 50
```

配置文件：`/etc/fm350-guard.conf`。

## 2. 刷机

1. 解压 `RW-OTA_29.24.02_F32.zip`（扁平结构，`Scatter.xml` 与 `root.squashfs` 同级）
2. 运行 `SPFlashToolV6.exe`，进入 **Download** 页，Scatter 选择 `Scatter.xml`
3. Download-Agent 选 `download_agent/DA_BR.bin`，Authentication 选 `download_agent/auth_sv5.auth`
4. 模式选 **Download Only**；只刷 AP 侧时可取消除 `rootfs` 外的分区（`rootfs` 位于 `SYS31`，
   `type=NORMAL_ROM`，`partition_size=0x2000000`）
5. 模组进入 Preloader/EDL（断电 → 短接/按住 BOOT → 上电，PC 出现 `MediaTek USB Port`）后点击 Download
6. 完成后断电重启，等待 30–60 秒

> 只写 `rootfs` 分区不会影响 MD 固件、IMEI/SN、bootloader 与 OEM/OP OTA 分区。

## 3. 校验

| 文件 | 大小 | 校验 |
| :--- | ---: | :--- |
| `root.squashfs` | 20,340,736 B | MD5 `c8f7c2a4d71d7ad024e39836beed5c3e` |
| `RW-OTA_29.24.02_F32.zip` | 50,817,497 B | SHA256 `afc4ab994447d697d01b7740033da9de25b760753087ec2641e0e52389b04b66` |
| `SP_Flash_Tool_v6.2124_Win.rar` | 34,797,992 B | SHA256 `94053f7fdaa1255881e71e8c2946f837e17a68572e95a9a3930714c0393c87bf` |

镜像信息：squashfs 4.0，xz，block 262144，全 root 属主，含 `dev/console` 字符设备；
分区余量约 12.6 MiB。

## 4. 回滚

原厂 `root.squashfs`：20,340,736 B 中对应原版为 **19,922,948 B，MD5 `4ca6145560a6eacafb409310ce92dbe5`**，
可用同一份 `Scatter.xml` 回刷 `rootfs`。

## 5. 已知限制

- **AP 域 adb 仍可能 `offline`**：本版已覆盖 dipc 回退顺序、functionfs 未挂载、
  `adbd_usb` 卡死三类问题，但最终状态受设备 `boot_mode`、NV 与主机驱动影响。
  若刷后仍为 `offline`，请反馈：
  ```sh
  cat /proc/mounts | grep usb-ffs
  ps | grep -E "adbd|usb.init"
  cat /tmp/usbdiag.log 2>/dev/null | tail -30
  cat /overlay/fm350-guard.log 2>/dev/null | tail -20
  ```
- **不含 atproxy**：RNDIS 数据面绑定仍依赖 F27 线或后续合并版本。
- **控制通道默认无鉴权**：socket 为 `0600`，仅模组内 root 可连；如设备会暴露给
  非 root 进程，请开启 `require_token=1`。
- `fm350-guard` 的 fd 自愈重启 `fibocom_modem_at_app` 会导致 AT 通道短暂中断，
  已做限流；两处泄漏修复后正常不会触发。

## 6. 风险提示

- 修改固件按原样提供，刷机风险自负；请先备份 IMEI/SN 与重要 NV 数据。
- 本包不修改 IMEI/SN，也不下发 APN/CGDCONT。
- `SP_Flash_Tool` 版权归 MediaTek 所有，此处仅作刷机便利分发。
