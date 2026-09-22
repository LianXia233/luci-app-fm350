<div align="center">

# luci-app-fm350

**适用于 Fibocom FM350-GL 5G 模组的高性能、现代化 OpenWrt / ImmortalWrt LuCI 管理插件**

[![OpenWrt](https://img.shields.io/badge/OpenWrt-24.10%2B-0099e5.svg?style=flat-square&logo=openwrt)](https://openwrt.org/)
[![ImmortalWrt](https://img.shields.io/badge/ImmortalWrt-24.x%20%7C%2025.x-ff6600.svg?style=flat-square)](https://immortalwrt.org/)
[![Rust](https://img.shields.io/badge/Backend-Rust%202021-dea584.svg?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![LuCI JS](https://img.shields.io/badge/Frontend-LuCI%20JS%20(No%20Lua)-brightgreen.svg?style=flat-square)](https://github.com/openwrt/luci)
[![License](https://img.shields.io/badge/License-GPL--3.0-blueviolet.svg?style=flat-square)](LICENSE)

<p align="center">
  <b>单静态二进制</b> • <b>零运行时依赖</b> • <b>串口排他持久持有</b> • <b>纯原生 LuCI JS</b>
</p>

---

</div>

## 📑 目录索引

- [💡 核心定位与架构概览](#-核心定位与架构概览)
- [✨ 功能特性清单](#-功能特性清单)
- [📊 状态仪表盘与派生算法](#-状态仪表盘与派生算法)
  - [首屏 8 张核心卡片](#首屏-8-张核心卡片)
  - [派生指标计算模型](#派生指标计算模型)
- [🧭 菜单路由与目录规范](#-菜单路由与目录规范)
  - [菜单入口映射](#菜单入口映射)
  - [仓库目录结构](#仓库目录结构)
- [🏗️ 系统架构与数据流](#️-系统架构与数据流)
  - [通信拓扑结构](#通信拓扑结构)
  - [AT 串口独占与主动可观测机制](#at-串口独占与主动可观测机制)
- [🛡️ IMEI / 串号安全体系](#️-imei--串号安全体系)
- [⚙️ UCI 配置全景参考](#️-uci-配置全景参考)
- [💻 CLI 命令行运维速查](#-cli-命令行运维速查)
- [🔨 交叉编译与构建指引](#-交叉编译与构建指引)
  - [使用 SDK 编译](#使用-sdk-编译)
  - [架构映射参考](#架构映射参考)
  - [CI 与发布流水线](#ci-与发布流水线)
- [🚀 部署验证与验收清单](#-部署验证与验收清单)
  - [安装与启动](#安装与启动)
  - [功能验收清单](#功能验收清单)
- [🔍 深度踩坑与技术避坑指北](#-深度踩坑与技术避坑指北)
- [⚖️ 架构设计取舍与考量](#️-架构设计取舍与考量)

---

## 💡 核心定位与架构概览

本插件针对广和通 **Fibocom FM350-GL** 5G 模组全新独立实现，彻底抛弃旧式脚本堆砌方案，拥有高可靠的数据与配置架构：

* 🦀 **Rust 单二进制后端 (`fm350d`)**：单一可执行文件运行于 `/usr/sbin/fm350d`，零运行时解释器依赖（**不依赖 Python**、无额外守护进程库）。
* ⚡ **原生 LuCI JS 前端**：遵循 LuCI 现代规范，全基于 `view/form` 与独立 CSS 样式表，不使用内联样式，不使用已废弃的 Lua 页面与控制器。
* 🔒 **严格串口独占持有**：采用持久持有 + 排他文件锁，杜绝频繁开关串口握手开销；辅以进程级 `/proc/*/fd` 扫描，实现独占状态真实可观测。
* 🌐 **精简系统集成**：通过标准 `/usr/share/rpcd/ucode/fm350.uc` 提供 ubus 代理，无额外 HTTP 守护进程，不占用额外独立网络端口。
* 📁 **规范配置管理**：统一纳入 OpenWrt 原生 UCI 体系（`/etc/config/fm350`，section `main`）。

---

## ✨ 功能特性清单

| 业务领域 | 具体能力支持 |
| :--- | :--- |
| 🏷️ **模组识别** | 厂商、型号、固件版本、IMEI、序列号 (SN)、IMSI、ICCID、USB 模式、SIM 卡槽状态、短信中心 (SMSC) |
| 📶 **网络状态** | 网络注册状态、运营商名称与代号、接入制式 (NR/LTE)、RSRP / RSRQ / SINR、频段 / PCI / ARFCN / TAC / 小区 ID、载波聚合状态 (CA) |
| 🌡️ **温控监测** | 5G 基带核心温度 (`md_5g`)、SoC 峰值温度、**全量 23 路独立硬件传感器明细表** |
| 🌐 **蜂窝拨号** | APN 配置、PDP 协议类型 (IPv4/IPv6/IPv4v6)、CID、认证协议、状态侦测、实时 IPv4/IPv6 地址与 DNS 提取 |
| 🔀 **路由守护** | 自动配置 IPv4 `/32` 静态接口与 IPv6 `dhcpv6` 子接口；把上行 `/64` 前缀委派给 LAN，自愈补齐缺失的默认路由 (`onlink`)，支持 Metric 跃点微调 |
| 🔒 **锁网锁频** | 支持 `AT+GTACT` 频段与制式锁定、`AT+EMMCHLCK` 物理小区 (PCI) 锁定，支持指定网络搜索优先级 |
| 📩 **短信中心** | 规范 PDU 编解码（中文 UCS2、英文 GSM 7-bit 自动适配）、收件箱列表、单条/批量删除、短信存储容量查询 |
| 🎛️ **硬件控制** | 软件重启模组、飞行模式与在线模式切换、实体双 SIM 卡槽软件倒换、USB 复合模式调整 |
| 🛡️ **IMEI 管理** | 串号读取、出厂原始数据镜像备份、受控安全写入（15 位格式校验与双重确认锁，Luhn 仅提示） |
| 💻 **交互终端** | 网页端内置交互式 AT 透传终端，提供安全半双工通信，拦截越权破坏性指令 |
| 🔌 **智能串口** | 自动枚举并探测 `/dev/ttyUSB*` 与 `/dev/ttyACM*`，呈现驱动内核信息、VID:PID，高亮识别 Fibocom 候选口 |

---

## 📊 状态仪表盘与派生算法

### 首屏 8 张核心卡片

「状态 $\rightarrow$ FM350-GL」首屏前置呈现高频网络指标卡片：

```text
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│   综合信号评价   │  │    运营商信息    │  │    网络制式      │  │  信号强度 (RSRP) │
│   63.1 / 100 [良]│  │  中国移动 (46000)│  │   5G NR (n41)    │  │     -81 dBm      │
└──────────────────┘  └──────────────────┘  └──────────────────┘  └──────────────────┘
┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐
│  信号质量 (RSRQ) │  │   信噪比 (SINR)  │  │     当前小区     │  │     模组温度     │
│   -11.0 dB [优]  │  │      15.0 dB     │  │     PCI: 128     │  │ 45.5 ℃ (md_5g)   │
└──────────────────┘  └──────────────────┘  └──────────────────┘  └──────────────────┘
```

| 卡片名称 | 主显数值 | 辅助说明内容 |
| :--- | :--- | :--- |
| **综合信号（SIGNAL）** | 信号条 + `63.1 / 100` + 等级标签 | 实际参与合成计算的指标项（如 `RSRP + RSRQ + SINR`） |
| **运营商** | `中国移动 (46000)` | 注册状态与漫游标识 |
| **网络类型** | `5G NR` | 频段号与带宽档位（如 `n41` / `-`） |
| **信号强度（RSRP）** | 信号条 + `-81 dBm` | 模组原始 `RSSI … dBm` 及兼容标识 `CSQ 99` |
| **信号质量（RSRQ）** | `-11.0 dB` + 质量标签 | 质量门限（优 $\ge -10\text{ dB}$，良 $\ge -15\text{ dB}$，中 $\ge -19.5\text{ dB}$） |
| **信噪比（SINR）** | `15.0 dB` | 等级划分与门限（优 $\ge 20\text{ dB}$，良 $\ge 13\text{ dB}$） |
| **服务小区** | `PCI 128` | 基站核心参数：`ARFCN` / `TAC` / `CID` |
| **模组温度** | `45.5 ℃`（`md_5g`） | SoC 核心峰值 / 全片最高温 / 活跃传感器路数 |

> 页面下方依次按流式卡片呈现：**连接与接口** $\rightarrow$ **信号细节** $\rightarrow$ **温度传感器（23 路）** $\rightarrow$ **模组识别信息** $\rightarrow$ **网络接口状态**。

### 派生指标计算模型

> [!NOTE]
> 派生指标**仅用于前端可视化呈现**，**绝对不参与任何内核链路判断与路由决策**。

1. **RSRQ 质量等级**：
   * **优**： $\ge -10\text{ dB}$
   * **良**： $\ge -15\text{ dB}$
   * **中**： $\ge -19.5\text{ dB}$
   * **差**： $\lt -19.5\text{ dB}$

2. **信号质量等级**：
   * 首选评估 SINR： $\ge 20\text{ dB}$（优）/ $\ge 13\text{ dB}$（良）/ $\ge 0\text{ dB}$（中）/ $\lt 0\text{ dB}$（差）。
   * 若当前制式无 SINR 上报，退回判定 RSRP： $\ge -80\text{ dBm}$（优）/ $\ge -90\text{ dBm}$（良）/ $\ge -100\text{ dBm}$（中）/ $\lt -100\text{ dBm}$（差）。

3. **综合信号评分（SIGNAL 合成公式）**：
   将三个关键指标分别做线性归一化（映射至 $0 \sim 100$），并按以下权重加权计算：

$$\text{SIGNAL} = 0.40 \times \text{SINR}_{\text{norm}} + 0.35 \times \text{RSRP}_{\text{norm}} + 0.25 \times \text{RSRQ}_{\text{norm}}$$

   | 指标项 | 标称物理量区间 $[V_{\min}, V_{\max}]$ | 归一化公式 |
   | :--- | :--- | :--- |
   | **RSRP** | $-140 \sim -44\text{ dBm}$ | $\text{RSRP}_{\text{norm}} = \frac{v - (-140)}{-44 - (-140)} \times 100 = \frac{v + 140}{96} \times 100$ |
   | **RSRQ** | $-19.5 \sim -3\text{ dB}$ | $\text{RSRQ}_{\text{norm}} = \frac{v - (-19.5)}{-3 - (-19.5)} \times 100 = \frac{v + 19.5}{16.5} \times 100$ |
   | **SINR** | $-23 \sim 30\text{ dB}$ | $\text{SINR}_{\text{norm}} = \frac{v - (-23)}{30 - (-23)} \times 100 = \frac{v + 23}{53} \times 100$ |

   * **边界保护**：超出标称区间自动截断（Clamp）至 0 或 100。
   * **信号条格数映射**：综合得分越高，点亮的信号条格数越多。

   | 综合得分 | 信号条格数 |
   | :--- | :--- |
   | $\ge 80$ | 5 格 |
   | $\ge 60$ | 4 格 |
   | $\ge 40$ | 3 格 |
   | $\ge 20$ | 2 格 |
   | $\gt 0$ | 1 格 |
   | $= 0$ | 0 格 |
   * **缺失指标智能重归一化**：当 LTE 模式下缺少 SINR 时，**缺失指标自动剔除，剩余指标按权重比例重新归一化计算**（避免因缺项直接归零导致整体得分被系统性拉低）。后端回传 `overall_used` 字段标明参算指标。

---

## 🧭 菜单路由与目录规范

### 菜单入口映射

```text
网络 (Network) ──► 移动网络 (Mobile Network) ──► FM350-GL
```

| 选项卡 Tab | 访问路由 (URL Path) | 说明 |
| :--- | :--- | :--- |
| **状态** | `/cgi-bin/luci/admin/modem/fm350/status` | 8 张仪表盘卡片、频段详情、23 路温度列表 |
| **拨号与 APN** | `/cgi-bin/luci/admin/modem/fm350/dial` | PDP 协议、APN、CID 及拨号控制表单 |
| **网络与锁频** | `/cgi-bin/luci/admin/modem/fm350/network` | 邻区扫描与诊断、频段锁定、小区/PCI 锁定、制式选择 |
| **短信** | `/cgi-bin/luci/admin/modem/fm350/sms` | PDU 短信收发展示、短信草稿与存储清理 |
| **AT 终端** | `/cgi-bin/luci/admin/modem/fm350/at` | 交互式命令行，具备内置危险指令拦截网关 |
| **服务设置** | `/cgi-bin/luci/admin/modem/fm350/settings` | 守护进程参数、串口重新探测、IMEI 权限控制 |

菜单在 `/usr/share/luci/menu.d/luci-app-fm350.json` 中声明，父节点 `admin/modem/fm350` 的 action 为 `firstchild`，六个子节点自动由 LuCI 渲染为页面顶部的 **Tab 切换条**。

### 仓库目录结构

```text
luci-app-fm350/
├── Makefile                                 # 打包与 Rust 交叉编译集成配方
├── README.md                                # 项目规范文档
├── CHANGELOG.md                             # 版本变更日志
├── .github/workflows/
│   ├── ci.yml                               # 静态代码门禁 + Rust 单测（推送 / PR 触发）
│   └── release.yml                          # 自动化发布流水线（标签或手动触发）
├── .gitignore                               # 忽略 target、ipk/apk、发布目录等产物
├── .gitattributes                           # 强制文本文件采用 LF 行尾
├── scripts/
│   └── build-release.sh                     # 基于官方 SDK 的发布构建脚本
├── htdocs/luci-static/resources/
│   ├── fm350/api.js                         # 前端统一 RPC 客户端中间件
│   ├── fm350/css/fm350.css                  # 独立样式表（严禁内联 CSS）
│   └── view/fm350/                          # LuCI JS 页面视图
│       ├── status.js                        # 状态仪表盘视图
│       ├── dial.js                          # 拨号配置视图
│       ├── network.js                       # 邻区扫描 / 锁频段 / 锁小区 / 制式
│       ├── sms.js                           # 短信控制台
│       ├── at.js                            # AT 指令终端
│       └── settings.js                      # 系统与端口设置
├── root/
│   ├── etc/
│   │   ├── config/fm350                     # 默认 UCI 配置文件
│   │   ├── init.d/fm350d                    # procd 系统自启管理脚本
│   │   └── uci-defaults/
│   │       └── 99-fm350-network             # 首次固件启动初始化脚本（注入网口骨架）
│   └── usr/share/
│       ├── luci/menu.d/luci-app-fm350.json  # 菜单节点注册
│       └── rpcd/
│           ├── acl.d/luci-app-fm350.json    # RPCD 鉴权访问控制列表
│           └── ucode/fm350.uc               # ucode 代理层（桥接 UBUS 到 fm350d CLI）
└── rust/                                    # fm350d 核心后端源码 (Rust 2021)
    ├── Cargo.toml / Cargo.lock              # 依赖声明与锁版本定义
    ├── .cargo/config.toml                   # 交叉链接器配置
    └── src/
        ├── at.rs                            # 串口持有器与 AT 问答通道
        ├── config.rs                        # UCI 读写与本地 2s 缓存机制
        ├── imei.rs                          # IMEI 备份/校验/写入防护
        ├── modem.rs                         # 核心业务逻辑（信号换算/温度探测）
        ├── sms.rs                           # PDU 编解码与短信协议驱动
        ├── net.rs                           # 网络状态同步与路由自愈线程
        ├── api.rs                           # 本地轻量级 JSON-RPC API（监听 127.0.0.1）
        └── main.rs                          # 守护进程主循环与 CLI 解析分发
```

> [!IMPORTANT]
> **代码仓库规范约定**：
> 1. **文本文件行尾必须为 LF**：由 `.gitattributes` 的 `* text=auto eol=lf` 保证。若 `/etc/init.d/fm350d` 包含 CRLF，procd 将报错 `can't open /etc/rc.common`。
> 2. **执行权限保留**：`root/etc/init.d/` 与 `scripts/` 下的脚本在 Git 索引中必须带 `100755` 权限位，避免在设备上手工部署或直接调用时失败。

---

## 🏗️ 系统架构与数据流

### 通信拓扑结构

```text
┌─────────────────────────────────────────────────────────────┐
│                      Web 浏览器 (LuCI)                      │
│            resources/fm350/api.js (统一 RPC 调度)           │
└──────────────────────────────┬──────────────────────────────┘
                               │  uhttpd / ubus JSON-RPC
                               ▼
┌─────────────────────────────────────────────────────────────┐
│            rpcd 代理插件 (/usr/share/rpcd/ucode/fm350.uc)   │
│             - 负责参数检验与单引号指令转义                  │
│             - 桥接并分发给后台 CLI 工具                     │
└──────────────────────────────┬──────────────────────────────┘
                               │  exec: fm350d <cmd>
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                 fm350d (Rust 高性能守护服务)                │
│  - 维持 127.0.0.1:8766 本地高速 API 通信通道                │
│  - 串口持久持有 + 半双工 AT 互斥队列调度                    │
│  - Linux 系统默认路由守护检测                               │
└──────────────────────────────┬──────────────────────────────┘
                               │  /dev/ttyUSBx (ioctl + flock 独占持有)
                               ▼
┌─────────────────────────────────────────────────────────────┐
│               Fibocom FM350-GL 硬件模组                     │
└─────────────────────────────────────────────────────────────┘
```

### AT 串口独占与主动可观测机制

1. **AT 口独占（整个运行期持续持有）**：
   * **排他锁控制**：端口以 `exclusive(true)` 打开，串口层据此执行 `ioctl(TIOCEXCL)` + 排他 `flock`。
     * `flock` 是**建议锁**：凡同样申请锁的程序（本插件的端口探测、第二个 `fm350d` 实例）都会被拒绝（`EWOULDBLOCK`）；
     * `TIOCEXCL` 虽是内核级强制排他，但**对持有 `CAP_SYS_ADMIN` 的进程无效**。OpenWrt 上几乎一切程序都以 `root` 运行，因此它挡不住外部未加锁的 root 进程。
   * **独占状态主动观测**：既然内核不提供对 root 绝对有效的强制排他，系统通过 `other_openers()` 主动扫描 `/proc/*/fd`，检查除自身外还打开了该 TTY 的外部进程，并在 Web 界面中实时告警。
2. **持有生命周期**：
   * 首次操作时懒加载打开，此后**一直持有到进程退出**，消除反复开关串口的握手开销；
   * 仅在两种情况下释放：串口底层读写破损异常；或在配置中修改了 `at_port`。
3. **配置热重载（零重启）**：
   * 每一次 AT 操作都会先比对当前持有路径与配置项，不一致时当场切换端口，**无需重启后台服务**。
   * 守护进程内置 2 秒配置原子缓存，修改 APN 或其他开关同样免重启生效。
4. **拨号与路由守护**：
   * FM350 的 RNDIS 通道**不提供 DHCP**，IPv4 必须静态 `/32`。
   * netifd 不会为无网关接口下发设备路由，daemon 周期性补齐 `default dev <dev> metric <metric> onlink`。

---

## 🛡️ IMEI / 串号安全体系

> [!WARNING]
> 修改设备 IMEI 涉及底层射频配置与合规性。不当修改可能导致模组无法在公网注册，部分地区受法律约束。**仅供设备维护与原厂数据恢复使用。**

为杜绝误触与非法覆盖，系统内置三重防护：

```text
                    [ 发起 IMEI 写入请求 ]
                               │
                               ▼
               ┌───────────────────────────────┐
               │ 校验 UCI: imei_write == 1 ?   │────► [拒绝执行 (默认防护)]
               └───────────────┬───────────────┘
                               │ 是
                               ▼
               ┌───────────────────────────────┐
               │ 请求携带 confirm=1 校验标？   │────► [拒绝执行 (缺少确认标)]
               └───────────────┬───────────────┘
                               │ 是
                               ▼
               ┌───────────────────────────────┐
               │ 符合 15 位纯数字格式？        │────► [格式不合规拦截]
               └───────────────┬───────────────┘
                               │ 是（Luhn 校验仅作提示，不阻断）
                               ▼
               ┌───────────────────────────────┐
               │ 备份当前串号到 /etc/fm350/    │
               │        imei.backup            │
               └───────────────┬───────────────┘
                               │
                               ▼
               ┌───────────────────────────────┐
               │ 下发原子写入并立即回读校验    │────► 写入 syslog 系统审计日志
               └───────────────────────────────┘
```

| 运维操作 | 触发条件 | 终端命令示范 |
| :--- | :--- | :--- |
| **读取** | 始终可用（`AT+EGMREXT=0,7`） | `fm350d imei read` |
| **备份** | 始终可用，写入 `/etc/fm350/imei.backup` | `fm350d imei backup` |
| **写入** | 需 `imei_write=1` + `confirm=1` + 15 位数字格式校验（Luhn 仅提示，不阻断） | `fm350d imei write <15 位> --confirm` |

> 透传 AT 终端中的写入指令同样被强制拦截（`AT+EGMREXT=1,*`、`AT+EGMR=1,*`、`AT+SIMEI=`、`AT+CGSN=` 一律拒绝）。

---

## ⚙️ UCI 配置全景参考

配置文件路径：`/etc/config/fm350`（配置段：`main`）

| 配置键名 | 默认值 | 作用说明 |
| :--- | :--- | :--- |
| `enabled` | `1` | 是否开启后台常驻守护服务 |
| `at_port` | `/dev/ttyUSB1` | 指定 AT 串口通信节点（必须填写绝对路径） |
| `baudrate` | `115200` | 串口波特率 |
| `at_timeout` | `10` | 单条 AT 指令交互超时时间（秒） |
| `apn` | `cmiot5g` | 蜂窝接入点名称 (APN) |
| `pdp_type` | `IPV4V6` | PDP 协议类型：`IP` / `IPV6` / `IPV4V6` |
| `cid` | `1` | 激活的目标 PDP Context ID |
| `auth` | `none` | 认证协议：`none` / `pap` / `chap` |
| `iface` | `fm350` | IPv4 接口名称（生成于 `/etc/config/network`） |
| `iface_v6` | `fm350v6` | IPv6 接口名称（关联 `device=@fm350`） |
| `data_dev` | `auto` | 蜂窝数据通道网卡名称，`auto` 依驱动自动探测 |
| `metric` | `30` | 下发默认路由的跃点优先级数值 |
| `ipv6` | `1` | 是否创建并接管 IPv6 子接口 |
| `extendprefix` | `1` | 是否把上行 IPv6 前缀（通常为 /64）委派给 LAN；关闭后局域网设备无法获得 IPv6 |
| `auto_dial` | `1` | 服务启动后是否自动发起网络拨号 |
| `route_guard` | `1` | 是否启用默认路由自愈守护 |
| `poll_interval` | `30` | 状态轮询与路由守护周期（秒，最低 5） |
| `api_port` | `8766` | 本地 JSON API 监听端口（仅监听 127.0.0.1） |
| `imei_write` | `0` | **IMEI 写入全局安全开关**（为 0 时拒绝一切写入操作） |

---

## 💻 CLI 命令行运维速查

`fm350d` 不仅支持作为系统常驻守护进程运行，还提供了功能完备的命令行运维指令：

```bash
# 1. 基础状态查看
fm350d status              # 输出模组整体状态快照（JSON 格式，包含温控与信号）
fm350d info                # 输出固件版本、SN、ICCID 等基本识别信息
fm350d signal              # 查看实时信号强度（RSRP/RSRQ/SINR）与来源分支
fm350d pdp                 # 查询当前 PDP 上下文激活状态与 IP 分配
fm350d net                 # 输出接口与内核默认路由状态
fm350d ports               # 扫描所有候选串口并探测占用（--no-probe 仅读 sysfs）

# 2. 拨号与网络控制
fm350d dial                # 主动触发网络拨号并配置静态/路由参数
fm350d hangup              # 挂断蜂窝网络并拆除对应路由

# 3. 锁网锁频管理
fm350d lock-band 14        # 锁定指定制式与频段
fm350d lock-cell 1,11,0,627264,280,3  # 锁定指定基站物理小区 PCI
fm350d lock-cell 0         # 解除物理小区锁定
fm350d rat NR:LTE:WCDMA    # 配置网络制式搜索优先级
fm350d lock                # 查询当前制式/频段与小区锁定状态
# 注：高通 / Quectel 风格的 AT+QNWPREFCFG 在 FM350-GL 上确实不存在，
#     实机会返回 +CME ERROR: unknown。FM350 的官方对应命令是
#     AT+EPRATL=<RAT num>,[<rat1>,<rat2>…]（官方手册 11.1.13，
#     编码 2 = UMTS、4 = LTE、128 = NR，越靠前优先级越高），本 CLI 已改用该命令。
#     手册 11.1.13 只列了写形式；但实机（FM350-GL）实测 AT+EPRATL? 可用，
#     回读形如 +EPRATL:2,128,4（2 个优先制式，NR 优先于 LTE），
#     故 fm350d rat（不带参数）与写入同用 +EPRATL，读写对称。
#     查询锁网锁频状态请用 fm350d lock（走 AT+GTACT?），两者语义不同。

# 4. 短信中心
fm350d sms list            # 列出收件箱短信列表
fm350d sms send 10086 "CXLL" # 发送中文/英文短信
fm350d sms delete <id>     # 删除指定序号短信

# 5. IMEI 安全维护
fm350d imei read           # 安全读取当前串号
fm350d imei backup         # 备份当前 IMEI 镜像至本地
fm350d imei write <15 位数字> --confirm # 写入新串号（需 imei_write=1）

# 6. 模组与系统控制
fm350d reboot              # 软重启蜂窝模组
fm350d sim 0               # 切换 SIM 卡槽（0 或 1）
fm350d cfun 0              # 进入飞行模式（1 为在线模式）
fm350d at "AT+CESQ"        # 透传执行任意 AT 指令
```

---

## 🔨 交叉编译与构建指引

### 使用 SDK 编译

本插件 Makefile 采用自包含设计（仅 `include rules.mk` 与 `package.mk`），无需依赖 `feeds/luci/luci.mk`。

```bash
# 1. 准备并解压目标 SDK (以 ImmortalWrt filogic aarch64 为例)
tar --zstd -xf immortalwrt-sdk-mediatek-filogic_gcc-14.4.0_musl.Linux-x86_64.tar.zst
cd immortalwrt-sdk-*/

# 2. 放入插件源码包
cp -r /path/to/luci-app-fm350 package/

# 3. 安装 Rust 对应架构目标
rustup target add aarch64-unknown-linux-musl

# 4. 生成构建配置
make defconfig

# 5. 编译软件包
export PATH="$HOME/.cargo/bin:$PATH"
make package/luci-app-fm350/compile V=s
```

> **构建产物路径**：`bin/packages/aarch64_cortex-a53/base/luci-app-fm350-<版本>.apk`

### 架构映射参考

| OpenWrt ARCH | Rust Target Triple |
| :--- | :--- |
| `aarch64` | `aarch64-unknown-linux-musl` |
| `x86_64` | `x86_64-unknown-linux-musl` |
| `i386` | `i686-unknown-linux-musl` |
| `arm` | `armv7-unknown-linux-musleabihf` |
| `mipsel` | `mipsel-unknown-linux-musl` |
| `riscv64` | `riscv64gc-unknown-linux-musl` |

### CI 与发布流水线

* **`.github/workflows/ci.yml`**：在代码推送与 PR 时触发，拦截静态语法错误、CRLF 行尾、JSON 校验、Shell 语法，并运行全部 Rust 单元测试。
* **`.github/workflows/release.yml`**：推送 `v*` 标签或手动触发，基于 ImmortalWrt SDK 自动交叉编译产出安装包，附带 `SHA256SUMS` 与 SDK 构建签名公钥。

---

## 🚀 部署验证与验收清单

### 安装与启动

```bash
# 1. 安装软件包
# apk 格式 (ImmortalWrt 25.x / OpenWrt 24.10+)
apk add --allow-untrusted luci-app-fm350-*.apk
# opkg 格式 (传统 OpenWrt 固件)
opkg install luci-app-fm350_*.ipk

# 2. 清理 LuCI 缓存并重载 rpcd
rm -f /tmp/luci-indexcache*
rm -rf /tmp/luci-modulecache/
/etc/init.d/rpcd reload

# 3. 启用并启动服务
/etc/init.d/fm350d enable
/etc/init.d/fm350d start
```

### 功能验收清单

- [ ] **菜单展示**：导航访问「网络 $\rightarrow$ Mobile Network $\rightarrow$ FM350-GL」，六个 Tab 页面切换流畅且无 404 报错。
- [ ] **串口独占正常**：运行 `ls -l /proc/$(pgrep -f 'fm350d daemon')/fd | grep ttyUSB`，确认句柄常驻；`fm350d ports` 中 `stats.other_pids` 为空。
- [ ] **仪表盘卡片正常**：首屏「网络概览」8 张卡片数据完整，综合评分形如 `63.1 / 100`。
- [ ] **温度与传感器正常**：首屏显示基带实测温度（如 `45.5 ℃`），传感器明细表完整展开 23 路，不包含 `[object Object]` 或单一异常的 `1`。
- [ ] **拨号网络连通**：点击拨号后，IPv4 地址成功呈现，路由表生成 `default dev <dev> metric 30 onlink`，通过 `ping -I <蜂窝 IP> 223.5.5.5` 测试连通正常。

---

## 🔍 深度踩坑与技术避坑指北

<details>
<summary><b>🛠️ 点击展开：AT 响应异构与信号解析避坑（CESQ、CSQ、GTCCINFO）</b></summary>

1. **CSQ 盲区与 CESQ 索引换算**：
   * FM350-GL 的 `AT+CSQ` 固定返回 `99,99`（3GPP 定义为「不可用」）。
   * 信号必须采用 `AT+CESQ` 获取，返回序列格式为：`<rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<ss_rsrq>,<ss_rsrp>,<ss_sinr>`。
   * **返回值为 3GPP 阶梯索引**，必须按区间下界换算：
     * **5G NR**：取索引 7（SS-RSRP，范围 $-156 \sim -31\text{ dBm}$，步长 1）、索引 6（SS-RSRQ，范围 $-43 \sim 20\text{ dB}$，步长 0.5）、索引 8（SS-SINR，范围 $-23 \sim 40\text{ dB}$，步长 0.5）。
     * **LTE**：取索引 5（RSRP，范围 $-140 \sim -44\text{ dBm}$）、索引 4（RSRQ，范围 $-19.5 \sim -3\text{ dB}$）。
2. **`AT+GTCCINFO?` 无字段名文本**：
   * 实机返回如 `1,9,460,0,149002,C2840C002,504990,128,,,16,69,69,64` 的纯数字流，**不包含任何键名文本**。
   * 必须按预定位置进行定点解析，字段 0 为小区类型标识（`1` = 服务小区，`2` = 邻区）。
   * **末尾四位的语义必须靠交叉验证确定，不能照抄手册**：背靠背采样（先 `AT+CESQ` 紧接着 `AT+GTCCINFO?`）
     实测 `+CESQ: 99,99,255,255,255,255,65,70,77`（信噪比 15.0 dB、强度 -87 dBm、质量 -11.0 dB）
     对应服务小区行尾部 `16,69,69,64`：
     - 倒数第 4 位 `16` 是**信噪比原值，单位就是 dB，不是索引**（对上 15.0 dB）；
     - 倒数第 3 位 `69` 是 RSRP 索引（对上 -87 dBm）；
     - 倒数第 2 位与倒数第 3 位**恒等**（实机 8 行小区全部如此），是同一个量的重复上报，**不是信噪比**；
     - 倒数第 1 位 `64` 是 RSRQ 索引（对上 -11.0 dB）。
     早期版本曾把倒数第 2 位当信噪比索引换算，得到 11 dB，比真值低约 5 dB。
   * 邻区行没有信噪比字段（对应位置是 `126` 这类哨兵），应如实显示「未测量」。
3. **`AT+GTACT` 的频段编号有三套编码**：
   * LTE：`1..88` 直接就是 `B1/B3/B5/B8`；
   * NR：既有 `5000+nXX`（`5041` → n41），也有 `500+nXX`（`501` → n1），还有 `100+nXX`（`141` → n41）。
   * 编码 `100+nXX` 的存在有一条硬证据：实机返回的 `101..171` 序列里
     `106/109/110/111/115/116/121~124/127` 全部缺席，而 3GPP 恰好不存在
     n6/n9/n10/n11/n15/n16/n21~n24/n27 —— 按 LTE 解读会出现根本不存在的 `B101`。
   * 同一个 5G 频段会在多套编码里重复出现，界面必须**按频段名去重**后再展示。
4. **频段缺失与 ARFCN 动态反推**：
   * 实机服务小区数据的 Band 字段可能为空。系统内置频率栅格反推算法：
     * NR：依据 3GPP TS 38.104 频率栅格公式反算频率，再匹配 TS 38.101-1 频段表；
     * LTE：按 TS 36.101 的 EARFCN 区间表判定；
     * 重叠区间（如 n38 与 n41）明确标注为 `n38/n41`，不随意假定。
</details>

<details>
<summary><b>🌡️ 点击展开：温度传感器踩坑（AT+GTSENRDTEMP vs AT+GTTHERMAL）</b></summary>

* `AT+GTTHERMAL?` 返回的 `+GTTHERMAL: 1` 仅是**温控健康标志**（1 表示未发生降频限流），误将其作为温度会使读数固定为恒定的 `1 ℃`。
* `AT+GTSENRDTEMP?` 查询形式不被模组支持，会直接报错 `+CME ERROR: phone failure`。
* **正确做法**：必须下发带参数的执行指令 `AT+GTSENRDTEMP=0`，模组将逐行返回 23 路传感器的原始千分位读数（单位： $0.001\text{ ℃}$，实际读数需除以 1000）。常用传感器：`1` = soc_max，`10` = md_5g，`14` = ltepa_ntc，`15` = nrpa_ntc，`16` = rf_ntc。
</details>

<details>
<summary><b>🌐 点击展开：拨号链路判据与 IPv6 伪地址陷阱</b></summary>

* **`AT+CGACT?` 不能作为拨号判据**：该模组响应 `AT+CGACT?` 仅返回裸 `OK`，不输出上下文状态行。若将其作为唯一判据会导致误判未激活，重复下发引发 `+CME ERROR: 5847`。系统以 `AT+CGPADDR=<cid>` 是否获取到有效非零 IP 为主判据。
* **模组拒绝去激活**：模组对 `AT+CGACT=0,<cid>` 会返回 `+CME ERROR: 54015`，且去激活失败后 `AT+CGPADDR` 仍保留原 IP，此为固件真实行为。
* **IPv6 以点分十进制上报**：FM350 在 `AT+CGPADDR` / `AT+CGCONTRDP` 中把 IPv6 写成 16 个十进制数，每个数为一个字节，相邻两数按大端合成一个 16 位组。实机 `+CGCONTRDP: 1,,"cmiot5g","","","36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8",...` 中的 `<PDP_addr>` 解码后即 `2409:8057:2000::8`。
  * 仅「拒绝 16 段串」并不够：真实 IPv6 也走这一形态，一律拒绝会让 `pdp.ipv6` 恒为空；而只认冒号形式还会在仅有 IPv6 可用时误判「未激活」并触发重复拨号。后端统一经 `normalize_ipv6()` 解码成标准冒号写法再入库。
  * IPv4 提取仍按「严格 4 段十进制且非 `0.0.0.0`」判定，因此 16 段串不会被误收为 IPv4。
* **IPv6 占位地址必须显式排除**：IPv6 未分配时模组回 `0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1`（即 `::1`）。它与真实地址结构完全一致，只判「是否 16 段」会把它当成已分配，从而把拨号失败误报为在线。后端排除全零与「仅最低字节为 1」两种占位。
* **IPv6 前缀默认不会下发到 LAN**：运营商通常只在 PDP 上给一个 /64、不额外下发独立 PD 前缀，而 netifd 的 dhcpv6 协议默认**不**把该 /64 当作可委派前缀 —— 于是出现「WAN 口有 IPv6、局域网设备一律没有」的半残状态。
  * 修复：v6 子接口置 `extendprefix=1`，由 `/lib/netifd/proto/dhcpv6.sh` 导出 `EXTENDPREFIX`，`/lib/netifd/dhcpv6.script` 再把该 /64 登记为委派前缀并 assign 给 lan。
  * 前置条件：LAN 侧需启用 IPv6 分配（`network.lan` 的 `ip6assign '64'`）。本插件遵循最小干预原则不代改 lan 配置。
* **链路本地地址不等于「拿到了 IPv6」**：任何 UP 的网卡都自带 `fe80::`。把它计入状态会让前端长期显示有 IPv6，并让在线判定出现假阳性，故 `status()` 按 `fe80::/10` 过滤。
</details>

<details>
<summary><b>💻 点击展开：OpenWrt 新版 ucode 与前端环境兼容指南</b></summary>

* **缺少独立 JSON 模块**：目标固件的 `libucode` 不包含独立 `json` 模块，使用 `import * as json from 'json'` 会报错导致 rpcd 插件加载失败。JSON 解析全部交由浏览器端 JS 完成。
* **全局内置函数变更**：ucode 运行环境中移除了全局 `String()`，需用 `'' + v` 转换；移除了内置 `floor()`，需显式声明 `import { floor } from 'math'`。
* **ubus.js 404 问题**：当前发行版不再内置 `/www/luci-static/resources/ubus.js`，强制引入会引发 404。统一改用 `'require rpc'` 并通过 `rpc.declare()` 进行远程方法绑定。
* **方法参数声明**：rpcd ucode 必须显式声明带参方法的 `args`（如 `args: { cmd: '' }`），否则会被 ubus 以 `UBUS_STATUS_INVALID_ARGUMENT` 拒绝。
</details>

---

## ⚖️ 架构设计取舍与考量

| 维度 | 本插件设计选型 | 传统/常见做法 | 选型优势分析 |
| :--- | :--- | :--- | :--- |
| **前端体系** | LuCI JS (View/Form) + 独立 CSS | 传统 Lua 控制器 + 内联样式 | 契合 LuCI 现代演进方向，无须兼容层，表单校验与主题渲染更规范。 |
| **后端架构** | Rust 单二进制静态程序 | Python / 复杂 Shell 脚本链 | 启动速度达到微秒级，内存开销极低，杜绝运行时缺失依赖导致的故障。 |
| **串口策略** | 持久打开 + `flock` + 进程观测 | 随用随关 / 轮询临时打开 | 杜绝并发 AT 冲突与 USB 频繁握手抖动，独占状态真实透明且可观测。 |
| **配置生效** | 逐请求重读（2 秒热缓存） | 重启守护进程生效 | 用户在 Web 端调整 APN 或修改端口后立即生效，告别传统服务重启断网。 |
| **AT 通信节拍** | 强制实施最小 30 ms 下发硬下限 | 无限制全速串行下发 | 防止高频并发导致模组内部基带处理队列假死或丢指令。 |
| **系统路由** | netifd 结合动态路由巡检自愈 | 依赖 netifd 单次下发 | 解决 RNDIS 无网关状态下未自动下发默认链路的问题，确保出网无忧。 |

---

<div align="center">
  <sub>Released under the GNU General Public License v3.0. Designed for OpenWrt / ImmortalWrt & Fibocom FM350-GL.</sub>
</div>
