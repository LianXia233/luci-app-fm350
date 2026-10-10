# FM350-GL F27 修改固件发布说明（v29.24.02 F27）

> 本固件由 [LianXia233/luci-app-fm350](https://github.com/LianXia233/luci-app-fm350) 项目维护发布，为 Fibocom FM350-GL（MediaTek MT6880）官方 F27 固件的修改版本。

## 1. 说明

本固件基于 Fibocom 官方 **`no-OTA_29.24.02_F27`** 固件包修改（"no-OTA" 为官方发布的固件包变体名称，指该包不含 OTA 升级镜像，非本项目的修改），**修改目的为更适配 [luci-app-fm350](https://github.com/LianXia233/luci-app-fm350) 插件**，让数据面拨号与插件功能开箱可用。

### 修改内容（参照仓库固件改造记录）

修改范围仅限 AP 侧系统分区（`root.squashfs`），modem 侧（`modem.img` / MOLY NR15）保持官方原版。共三个部件：

1. **atproxy v2**（`/usr/bin/atproxy`）：由模组内部在 PDN 就绪时机下发
   `AT+EMBIND=1,"M-RNDIS",<cid>`，把 RNDIS 数据通道强制绑定到激活 PDN
   （启动 20s 首绑 + 120s 周期保活）。主机侧直接发同一条 AT 不生效，
   此为 RNDIS 数据面恢复的主路径。绑定结果以 `+GTEMBIND: rc=<n>,cid=<n>` URC 上报。
2. **usb.init ADB 补丁**：对原厂 `/etc/init.d/usb.init` 的 boot_mode 三分支统一启用
   `adbd_usb`（`0001` 分支补 `ffs.adb` function 链接），使 AP 域免认证 root ADB
   在任何启动模式下随 USB 常开，便于调试。
3. **dropbear SSH**：静态编译 dropbear 2024.86 + `-B` 空密码配置 + procd init，
   经 `adb forward` 隧道转出 AP 域 shell（SSH-over-USB）。

### 红线（与仓库改造记录一致）

- **不下发 CGDCONT（APN）**：避免换卡场景改写模组 APN 导致断数据。
- **不下发任何 IMEI 相关命令**：IMEI 操作仅限合法 NV 恢复场景。
- atproxy 不透传原始 AT，唯一下行写命令是 EMBIND；不暴露 NetIF id。

## 2. 适用范围与校验信息

| 项目 | 值 |
|---|---|
| 适用模组 | Fibocom FM350-GL（MT6880，k6880v1_mdot2_datacard） |
| 基线固件版本 | 29.24.02 F27（官方 no-OTA 变体） |
| 修改类型 | AP 侧：atproxy v2 EMBIND 强制绑定 + usb.init ADB 补丁 + dropbear SSH（见第 1 节） |
| 文件名 | `Generic_29.24.02_F27.zip` |
| 大小 | 50,525,259 字节（约 48.2 MiB） |
| MD5 | `60e04e7dddb15df9bdc7d339b17505f7` |
| SHA-256 | `9f9159efc5e6dd530fc61e9b3439f00289fd905a95b5bda4d036adfb47f779f0` |

刷入前请务必校验上述哈希值，不匹配则停止操作。关键镜像分项校验：

| 文件 | MD5 |
|---|---|
| `modem.img` | `9dfb1f11d206fd32b93e8384b62d3c63` |
| `root.squashfs` | `677b62c6b251a2d7459df5ec1053b6bb` |

## 3. ⚠️ 刷入前必须备份 IMEI / SN 分区

> **⚠️ 警告：刷入本固件前，必须先记录/备份存放 IMEI 和 SN 的 NV 数据。**
> **IMEI / SN 丢失后模块将无法正常注册运营商网络（无信号、无串号）。丢失后可通过 AT 命令回写恢复，但前提是掌握原始值——请务必在刷机前先记录。**

- **风险**：刷机过程或分区布局变化可能导致 NV 分区被擦除或校验失败。IMEI 丢失后模块插卡无服务；SN 丢失影响售后与质保。
- **恢复方式**：丢失后可通过 AT 命令回写 IMEI / SN 恢复（相关 AT 命令详见官方 AT 手册 `fibocom_fm350_at_commands_v2.2.pdf`，或经由诊断/工程通道写入）。但建议仍以**刷前备份优先**——回写恢复依赖正确记录原始值，若未备份则无从得知原始 IMEI / SN。
- **备份方法（刷入前执行）**：通过 AT 命令导出并记录当前 IMEI、SN（`AT+GSN`、`AT+CGSN` 等，SN 查询命令以官方 AT 手册为准），单独留存；有条件时再用 SP Flash Tool readback 读取 NVDATA 分区做完整备份。

**刷入前请务必通过 AT 命令记录原始 IMEI / SN；未记录前，请不要执行第 4 节的任何刷入操作。**

## 4. 刷入步骤与注意事项

### 前置准备

1. 完成第 3 节 NV 备份，并确认备份文件可读、已另存。
2. 下载固件包并校验 MD5 / SHA-256。
3. 准备 Windows 主机、USB 数据线（模组直连）、SP Flash Tool（或等价的 MT6880 线刷工具）。

### 刷入步骤

1. 将模组进入刷机模式（BootROM / DA 模式），通过 USB 连接主机。
2. 在 SP Flash Tool 中加载固件包内 `Scatter.xml`。
3. **仅勾选需刷写的分区**：AP 侧 `root.squashfs`（及必要的 boot/lk 等），**切勿勾选 NVDATA / nvram / protect 分区**。
4. 点击 Download 开始刷写，等待完成提示，不要中途断电或拔线。
5. 重启模组，用 `AT+CGMR` 等命令确认固件版本为 `29.24.02 F27`，插卡验证拨号与网络。

### 注意事项

- 本固件基于官方 no-OTA 变体，**请勿尝试混刷其他 OTA 包或对模组执行在线固件更新**。
- 本固件含免认证 root ADB 与空密码 dropbear（均为 USB 通道调试用途），**模组 USB 口不要接入不受信任的主机/网络环境**。
- 刷写过程断电、拔线可能导致模组变砖。
- 刷写前确认模组电量/供电稳定（USB 直连建议使用带独立供电的口或短质量好的线）。
- 严禁任何形式的 IMEI 篡改；IMEI 相关操作仅限合法的 NV 恢复场景，且需分步确认。
- 刷入后如信号/注册异常，优先排查 NV 分区是否完好，再排查 modem 配置。

## 5. 问题反馈

使用中遇到问题请在 [luci-app-fm350 Issues](https://github.com/LianXia233/luci-app-fm350/issues) 提交，附上模组型号、固件版本、AT 响应日志。
