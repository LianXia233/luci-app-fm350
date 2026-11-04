# FM350-GL F27 修改固件发布说明（v29.24.02 F27）

> 本固件由 [LianXia233/luci-app-fm350](https://github.com/LianXia233/luci-app-fm350) 项目维护发布，为 Fibocom FM350-GL（MediaTek MT6880）官方 F27 固件的修改版本。

> 本文仅适用于 **29.24.02 F27（Generic / no-OTA 基线）** 这一条固件线。仓库另有基于
> **F31 29.23.24n** 的 AP + MD 自愈完整线刷包，属于不同分支、不同修改范围，
> 请勿交叉套用两份说明的校验值、镜像与刷入步骤（见第 6 节）。

## 下载（本 Release 附件）

| 文件 | 说明 | 大小 | 下载 |
|---|---|---|---|
| `Generic_29.24.02_F27.zip` | 修改版固件本体 | 约 48.2 MiB | [⬇ 下载固件包](https://github.com/LianXia233/luci-app-fm350/releases/download/firmware-29.24.02_F27/Generic_29.24.02_F27.zip) |
| `MTK_Windows_USB_Driver_v17.6.0.6.exe` | MediaTek Windows USB 驱动（线刷识别模组用） | 约 5.4 MiB | [⬇ 下载驱动](https://github.com/LianXia233/luci-app-fm350/releases/download/firmware-29.24.02_F27/MTK_Windows_USB_Driver_v17.6.0.6.exe) |
| `FM350-GL-Flash-Guide-240812.pptx` | SP Flash Tool 刷机方法演示文档 | 约 2.1 MiB | [⬇ 下载刷机文档](https://github.com/LianXia233/luci-app-fm350/releases/download/firmware-29.24.02_F27/FM350-GL-Flash-Guide-240812.pptx) |

> [!WARNING]
> 刷入前务必：**① 校验哈希**（见第 2 节）→ **② 备份 IMEI / SN**（第 3 节警告必须先读）→ **③ 按第 4 节与刷机文档操作**。

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

### 技术依据：为什么必须由模组内部下发 EMBIND

逆向分析（[RNDIS 逆向分析报告](RNDIS-Analysis-Report.md) 第 2 节）在 F31 29.23.24n
镜像上确认：RNDIS 是 **MD 域**实现的 USB 类 —— 该样本 AP 内核 4.19.205 的 configfs
只启用 `acm` / `ffs.adb` / `mass_storage` 等，`CONFIG_USB_CONFIGFS_RNDIS/NCM/ECM`
均为 not set，AP 侧也没有 DHCP 服务端；MD 镜像内含
`middleware/hif/usbclass/rndis/*` 与 `ipcore` 的完整实现，`M-RNDIS` 是 MD 内部
L2P 实例名。外部 AT 经 USB ACM → atcid → MIPC 到达 MD，缺少「模组内部、PDN 已激活」
的上下文，因此主机侧直接发 `AT+EMBIND` 不会落地。这就是 atproxy v2 必须运行在模组
内部、并在 PDN 就绪时机下发的原因。

> 取证样本来自 F31 分支；两条分支的 AP/MD 架构同源，但 F27 未单独做镜像级取证，
> 且下列 F31 的其它改动**不属于本文固件**，请以各自固件包为准。

## 2. 适用范围与校验信息

| 项目 | 值 |
|---|---|
| 适用模组 | Fibocom FM350-GL（MT6880，k6880v1_mdot2_datacard） |
| 基线固件版本 | 29.24.02 F27（官方 no-OTA 变体） |
| 修改类型 | AP 侧：atproxy v2 EMBIND 强制绑定 + usb.init ADB 补丁 + dropbear SSH（见第 1 节） |
| 文件名 | `Generic_29.24.02_F27.zip` |
| 大小 | 50,525,259 字节（约 48.2 MiB） |
| MD5 | `92059c8243a309d3aba2f777b4fdc355` |
| SHA-256 | `2abf8670dd202b9c323c0f5ea3bc30c9d37ddffd3cd4cb03f82dff816a384326` |

刷入前请务必校验上述哈希值，不匹配则停止操作。关键镜像分项校验：

| 文件 | MD5 |
|---|---|
| `modem.img` | `9dfb1f11d206fd32b93e8384b62d3c63` |
| `root.squashfs` | `677b62c6b251a2d7459df5ec1053b6bb` |

### 配套工具与文档（随本 Release 附件提供）

| 文件 | 说明 | 大小 | MD5 | SHA-256 |
|---|---|---|---|---|
| `MTK_Windows_USB_Driver_v17.6.0.6.exe` | MediaTek Windows USB 驱动，供主机在刷机模式（BootROM / DA 模式）下识别模组 | 5,674,427 字节（约 5.4 MiB） | `f6c6b674d72c4f3a7b08c27926dcf259` | `9726952e95bc501bc3778203a6d7b38bd68a7e124bbdb5fb6be0452d4397e4b1` |
| `FM350-GL-Flash-Guide-240812.pptx` | FM350-GL 刷机方法演示文档（SP Flash Tool 线刷流程；原文件名 `FM350-GL刷机方法-240812改.pptx`，因 GitHub 附件不支持中文文件名而重命名） | 2,191,498 字节（约 2.1 MiB） | `559e6b745edca2194eb205719e8f8d7a` | `cca4b68b9353856d6a89e9ceddc5e4d620e63d0652ac1d7d301608eb2e9ca84b` |

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

提交前请先确认所用固件线（F27 还是 F31）并附上 `AT+CGMR` 输出；跨分支问题请同时
说明 `download_agent/` 的来源。技术背景见 [RNDIS 逆向分析报告](RNDIS-Analysis-Report.md)
与 [架构迁移文档](ARCHITECTURE_MIGRATION.md)。

## 6. 与 F31 29.23.24n 分支的关系（重要）

本项目同时维护两条**互不通用**的固件线：

| 项目 | 本文：29.24.02 F27 | F31 29.23.24n 修改包 |
|---|---|---|
| 基线 | 官方 `no-OTA_29.24.02_F27` | 官方 `F31_29.23.24n` |
| modem 版本串 | `81600.0000.00.29.24.02` | `81600.0000.00.29.23.24` |
| modem 构建日期 | 2023-12-23 | 2025-02-18 |
| DA / auth | `MTK_DA_v6_2024-08-20` | `MTK_DA_v6_2025-02-18`（需整组配套） |
| 修改范围 | 仅 AP 侧 `root.squashfs` | AP 侧 rootfs 加固 + MD nvdata 自愈脚本等 |
| atproxy | v2：仅 EMBIND 首绑（20 s）+ 120 s 保活 | 2.2.0：另含 AT 网关（硬性 >=25 ms 间隔、优先级队列）、自动拨号、APN 候选重试 |
| APN | **不下发 CGDCONT**，原厂 APN 库保持原样 | 补齐国内四大运营商 `ia` APN，IMS cid 收窄为 20 |
| `dipc_config` 自愈 / 禁用 PCIE | 无 | `mdcfg` + `pcieoff`（强制 `dual_ipc_mode:4`） |
| Release | [firmware-29.24.02_F27](https://github.com/LianXia233/luci-app-fm350/releases/tag/firmware-29.24.02_F27) | [firmware-29.23.24n_F31_mod](https://github.com/LianXia233/luci-app-fm350/releases/tag/firmware-29.23.24n_F31_mod) |

**版本高低怎么理解**：按点分数值 `29.24.02 > 29.23.24`，但 F31 的 modem 与 DA
构建时间晚约 14 个月 —— 这是两条并行发布分支，版本号不共享同一条递增序列，
不能用编号大小推断「哪个更新、哪个更好」。设备侧真实版本以 `AT+CGMR` / `ATI`
的 `Revision` 字段为准。

**不要跨分支混刷**：

- `download_agent/`（`DA_BR.bin` + `auth_sv5.auth`）必须与固件包整组配套，SV5
  安全认证下交叉混用会在 BROM 认证阶段被拒；
- 单独替换 `modem.img` 会形成「旧 modem + 新 AP」组合，拨号与 RNDIS 绑定行为
  均未经验证；
- no-OTA 基线包里的三个 mcf/OTA 分区镜像是 8 MiB 全 `0xFF` 空白镜像，而 F31
  官方包本来就不含 `DEV_OTA.img`；除空白分区镜像外，请勿互相借用任何镜像。

## 7. 已知限制与使用建议（依据逆向分析）

以下条目来自 [RNDIS 逆向分析报告](RNDIS-Analysis-Report.md)，用于解释刷完 F27 后
最常见的几类现象。其中标注「F31 才有」的自愈与修复能力**不属于本文固件**。

| 现象 | 原因 | F27 上的处理 |
|---|---|---|
| 插卡能注册但上不了网 / 主机显示「未识别网络」 | 原厂 APN 库中国区 14 个号段 0 个带正确 `ia` APN：移动只有空 APN，联通/电信只有 ims，广电 46015 文件缺失；附着 APN 为空时 PDP 激活常失败或拿不到地址 | **必须由插件侧显式设置 APN**（`AT+CGDCONT` 或插件 APN 配置），如电信 `ctnet`、联通 `3gnet`、移动 `cmnet`；F27 红线决定固件不会代写 APN |
| 打开 LuCI 页面后 AT 无响应，随后 USB 数据面掉线 | 多路客户端并发写 `/dev/atci-service`，MD 的 atcid 解析任务在一次调度里收到十几条命令会卡死；AT 通道与 RNDIS 绑定共用同一 MD 任务组 | 插件侧的 AT 优先级调度器只保证**主机侧**串行，不保护模组内部；避免多进程同时轮询查询。硬性 25 ms 间隔的 AT 网关只在 F31 的 atproxy 2.2.0 里，F31 才有 |
| 刷机或异常掉电后 USB 完全不枚举（看似变砖） | `nvdata/md_cmn/dipc_config` 缺 `dual_ipc_mode` 时原厂 `usb.init` 走 NO USB 分支直接 `exit 0`；`pcie_link_state` 非字符串 `0` 也会让整机数据面误判交给 PCIE | F27 **不含** `mdcfg` 自愈与 `pcieoff`。先确认该 nvdata 文件完整，或改用带自愈的 F31 包；这不属于插件可恢复范围，F31 才有 |
| 想改用 NCM / ECM 网卡 | 三重硬阻塞：AP 内核无 NCM/ECM、无 TUN/NAT；MD `usbclass/` 只有 acm/adb/msd/rndis/niccommon；md1rom 是带校验的签名容器，无法安全注入 | 维持 `M-RNDIS`（主机兼容性最好）并把 EMBIND 保活做扎实；NCM 需要 MTK BSP 重编内核，当前两条分支都做不到 |

插件侧 AT 调度器与固件侧 AT 网关是**两层、互不替代**，边界说明见
[架构迁移文档](ARCHITECTURE_MIGRATION.md) 第 12 节。
