# luci-app-fm350

Fibocom FM350-GL 5G 模组的 OpenWrt / ImmortalWrt LuCI 管理插件。

后端由 **Rust** 编写（`fm350d`，单个静态二进制），前端采用 **LuCI JS view/form**
（`htdocs/luci-static/resources/view/`）与 **rpcd ucode 后端插件**
（`/usr/share/rpcd/ucode/fm350.uc`），样式全部集中在独立 CSS 文件中，
不使用 Lua 页面与控制器，不使用内联样式。

本插件为**全新实现**，拥有独立的数据与配置体系：

- 配置：UCI `/etc/config/fm350`（section `main`）
- 后端：`/usr/sbin/fm350d`（Rust 单二进制）
- 界面：LuCI 原生页面，无独立 HTTP 服务、无独立端口
- 不依赖 Python，不依赖任何其他框架形态的文件或配置

---

## 1. 功能

| 分类 | 能力 |
| --- | --- |
| 模组信息 | 厂商、型号、固件版本、IMEI、序列号、IMSI、ICCID、USB 模式、SIM 卡槽、短信中心 |
| 状态 | 注册状态、运营商、接入技术、RSRP / RSRQ / SINR、频段 / PCI / ARFCN / TAC / 小区 ID、载波聚合、模组温度（含 23 路传感器明细）；综合信号（SIGNAL）与 RSRQ 质量等级为派生指标 |
| 拨号 | APN / PDP 类型 / CID / 认证、拨号与断开、IPv4 / IPv6 地址与 DNS 展示 |
| 网络 | IPv4 静态 /32 接口、IPv6 dhcpv6 子接口、默认路由与路由守护、路由优先级（metric） |
| 锁频 | `AT+GTACT` 锁制式与频段、`AT+EMMCHLCK` 锁小区 PCI、制式优先级 |
| 短信 | PDU 模式收发（中文自动 UCS2、英文 GSM 7-bit）、列表、删除、存储位置查询 |
| 控制 | 重启模组、飞行/在线模式、SIM 卡槽切换、USB 模式切换 |
| IMEI | 读取、备份、写入 / 更换（写入需显式开启开关 + 二次确认） |
| AT 终端 | 任意 AT 指令透传 |
| 串口 | AT 端口自主选择（扫描候选、显示驱动与占用、支持手填） |

#### 状态页首屏（网络概览）

「状态 → FM350-GL」首屏是八张卡片，把最常看的网络信息前置：

| 卡片 | 主值 | 副标题 |
| --- | --- | --- |
| 综合信号（SIGNAL） | 信号条 + `63.1 / 100` + 等级标签 | 参与计算的指标（`RSRP + RSRQ + SINR`） |
| 运营商 | `中国移动 (46000)` | 注册状态 |
| 网络类型 | `5G NR` | 频段 / 带宽档位 |
| 信号强度（RSRP） | 信号条 + `-81 dBm` | `RSSI … dBm` / `CSQ …` |
| 信号质量（RSRQ） | `-11.0 dB` + 质量等级标签 | 阈值（优 ≥ -10 dB，良 ≥ -15 dB，中 ≥ -19.5 dB） |
| 信噪比（SINR） | `15.0 dB` | 等级 / 阈值（优 ≥ 20 dB，良 ≥ 13 dB） |
| 服务小区 | `PCI 128` | `ARFCN` / `TAC` / `CID` |
| 模组温度 | `45.5 ℃`（`md_5g`） | SoC 峰值 / 全片最高 / 传感器路数 |

页面其余部分依次为「连接与接口」「信号细节」「温度传感器」「识别信息」「网络接口」。

**三个派生指标**（均只用于展示，**不参与任何链路判定**；阈值与公式集中在一处便于核对）：

1. **RSRQ 质量等级**：`优 ≥ -10 dB`、`良 ≥ -15 dB`、`中 ≥ -19.5 dB`，其余为 `差`。
2. **信号等级**：优先按 SINR（`≥ 20 / ≥ 13 / ≥ 0 / < 0` dB），
   无 SINR 时退回 RSRP（`≥ -80 / ≥ -90 / ≥ -100 / < -100` dBm）。
3. **综合信号（SIGNAL）**：把 RSRP / RSRQ / SINR 各自线性归一化到 0..100
   后加权平均，权重 `SINR 0.40 > RSRP 0.35 > RSRQ 0.25`：

   | 指标 | 标称区间 | 归一化 |
   | --- | --- | --- |
   | RSRP | -140 ~ -44 dBm | `(v + 140) / 96 × 100` |
   | RSRQ | -19.5 ~ -3 dB | `(v + 19.5) / 16.5 × 100` |
   | SINR | -23 ~ 30 dB | `(v + 23) / 53 × 100` |

   超出标称区间即截断到 0 / 100；得分 → 信号条格数
   `≥ 80 → 5`、`≥ 60 → 4`、`≥ 40 → 3`、`≥ 20 → 2`、`> 0 → 1`、`0 → 0`；
   文字等级 `≥ 75 优`、`≥ 50 良`、`≥ 25 中`、其余 `差`。

   **缺失的指标不参与计算，并按可用权重重新归一化**，而不是按 0 分计入
   —— 否则 LTE 下没有 SINR 时得分会被系统性拉低。
   后端的 `overall_used` 字段回传实际参与计算的指标名，
   卡片副标题与「信号细节」表的「综合信号构成」一行都会显示，
   便于区分「模组上报值」与「派生值」。

   实测实例：RSRP `-81 dBm` / RSRQ `-11.0 dB` / SINR `15.0 dB`
   → 归一化 `61.5`、`51.5`、`71.7` → 加权 **`63.1 / 100`，等级 4，「良」**。

### 菜单位置

```
网络 → Mobile Network → FM350-GL
```

对应 URL：

```
/cgi-bin/luci/admin/modem/fm350/status     状态
/cgi-bin/luci/admin/modem/fm350/dial       拨号与 APN
/cgi-bin/luci/admin/modem/fm350/network    网络与锁频
/cgi-bin/luci/admin/modem/fm350/sms        短信
/cgi-bin/luci/admin/modem/fm350/at         AT 终端
/cgi-bin/luci/admin/modem/fm350/settings   服务设置
```

菜单在 `/usr/share/luci/menu.d/luci-app-fm350.json` 中声明，
父节点 `admin/modem/fm350` 的 action 为 `firstchild`，
六个子节点因此被 LuCI 渲染为页面顶部的 **tab 条**。

---

## 2. 目录结构

```
luci-app-fm350/
├── Makefile                       打包定义（含 Rust 交叉编译钩子）
├── README.md
├── CHANGELOG.md
├── htdocs/luci-static/resources/
│   ├── fm350/api.js               统一后端调用层（rpc.declare 声明全部方法）
│   ├── fm350/css/fm350.css        独立样式表（页面不使用内联样式）
│   └── view/fm350/                LuCI JS 页面
│       ├── status.js              状态总览（真实快照）
│       ├── dial.js                APN / 拨号（form）
│       ├── network.js             接口参数 / 锁频 / 锁小区
│       ├── sms.js                 短信收发
│       ├── at.js                  AT 终端
│       └── settings.js            服务参数 / AT 串口 / 模组控制 / IMEI（form）
├── root/
│   ├── etc/config/fm350           默认 UCI 配置
│   ├── etc/init.d/fm350d          procd 守护脚本
│   └── usr/share/
│       ├── luci/menu.d/luci-app-fm350.json   菜单注册
│       └── rpcd/
│           ├── acl.d/luci-app-fm350.json     ACL 权限
│           └── ucode/fm350.uc                rpcd ucode 代理（ubus → fm350d）
└── rust/                          Rust 后端
    ├── Cargo.toml
    ├── .cargo/config.toml         交叉链接器配置
    └── src/
        ├── at.rs                  AT 串口独占访问层 + 端口枚举
        ├── config.rs              UCI 读写
        ├── imei.rs                IMEI 读取 / 备份 / 写入守卫
        ├── modem.rs               状态 / 信息 / 信号 / PDP / 锁频
        ├── sms.rs                 PDU 编解码与收发
        ├── net.rs                 UCI 接口与路由守护
        ├── api.rs                 本地 JSON API（仅 127.0.0.1）
        └── main.rs                CLI 与 daemon
```

---

## 3. 架构与数据流

```
浏览器
  │  LuCI JS (view/form) → resources/fm350/api.js
  │  rpc.declare({ object: 'luci.fm350', method: ... })
  ▼
uhttpd → /admin/ubus (JSON-RPC) → rpcd
  │  call luci.fm350.<method>
  ▼
rpcd ucode 插件  /usr/share/rpcd/ucode/fm350.uc
  │  exec: fm350d <子命令>          （参数一律单引号转义）
  │  原样返回后端 stdout（raw），由 JS 端 JSON.parse
  ▼
fm350d（Rust 守护，独占持有 AT 口）
  │  若 daemon 在运行 → 走 127.0.0.1:8766 本地 API
  │  否则            → 本进程临时打开 AT 口执行
  ▼
/dev/ttyUSBx  ←→  FM350-GL
  （默认 /dev/ttyUSB1，可在「服务设置 → AT 串口」中改选）
```

关键点：

1. **AT 口独占（整个运行期持续持有）**：由 `fm350d` 单点持有。
   - **排他锁**：端口一律以 `exclusive(true)` 打开，串口层据此执行
     `ioctl(TIOCEXCL)` + 排他 `flock`。两条机制的效力必须分清：

     - `flock` 是**建议锁**：凡同样申请锁的程序（本插件自己的端口探测、
       第二个 `fm350d` 实例）都会被拒绝（`EWOULDBLOCK`），一定进不来；
     - `TIOCEXCL` 虽是内核级强制排他，但**对持有 `CAP_SYS_ADMIN` 的进程
       无效**。`tty_ioctl(2)` 原文：`They fail with EBUSY, except for a
       process with the CAP_SYS_ADMIN capability`。OpenWrt 上几乎一切
       （LuCI / rpcd / cat / 串口终端）都以 root 运行，所以它**挡不住**
       root 进程 —— 这一点已在实机用 `cat` / `dd` / `head` 逐一验证。
   - **独占可观测**：既然内核不提供对 root 有效的强制排他，"是否真的独占"
     就由 `other_openers()` 主动观测——扫描 `/proc/*/fd`，找出除自己以外
     还打开了该 tty 的进程。结果经 `fm350d ports` 的 `stats.other_pids`
     下发：daemon 在占用者出现 / 消失时打日志告警，「服务设置」页显示
     「独占有效」或「另有 N 个进程也在打开该端口」。上层另有 flock 单实例
     守卫（见 `main.rs`），保证 `fm350d` 自身不会出现两个实例争抢。
   - **持久持有**：首次访问时惰性打开，此后**一直持有到进程退出**，
     不做空闲释放。因此 AT 口在整个服务生命周期内只属于本插件：
     既满足"必须独占"的要求，也免去了反复开关串口的握手开销
     （一次状态查询会串起二十余条 AT 指令）。
   - **仅在两种情况下释放**：串口读写异常（丢弃句柄，下次调用重新独占打开）；
     或在 LuCI 改了 `at_port`（释放旧句柄，下次访问按新端口打开，
     无需重启服务）。
   - **改完即生效**：每一次 AT 操作都会先比对「当前持有的路径」与配置里的
     `at_port`，不一致就当场换端口，因此改完端口**下一次查询**即走新端口，
     不必等巡检周期；daemon 巡检只作为兜底（长时间无访问时也会释放旧句柄）。
     同理，daemon 的 API **每个请求都重新读取配置**（内部 2 秒缓存），
     所以 `apn`、各类开关的改动同样不需要重启服务。
   - **端口可选**：`at_port` 不写死 `/dev/ttyUSB1`。「服务设置 → AT 串口」
     会扫描 `/dev/ttyUSB*` 与 `/dev/ttyACM*`，读出内核驱动、USB `VID:PID`、
     厂商与产品名，并逐个尝试独占打开以探测占用情况；疑似 Fibocom 设备的
     端口会被标注。排序为「当前配置端口 → 疑似 Fibocom → 设备名」，
     也支持手填绝对路径。命令行等价物为 `fm350d ports`。
   - 进程内用互斥锁串行化一问一答：AT 通道是半双工问答语义，模组会回显
     命令、URC 会异步插入下行流，因此下发前清空输入缓冲、按结果码
     （`OK` / `ERROR` / `+CME ERROR` ...）判定响应结束，并剥离回显行。
2. **CLI 自动转发**：命令行调用时若 daemon 在跑，会通过本地 API 转发，
   避免两个进程争抢同一个串口。
3. **拨号链路**：`AT+CGDCONT`（APN）→ `AT+CGACT=1,<cid>` → `AT+CGPADDR`（IPv4）
   → `AT+GTDNS`（DNS）。FM350 的 RNDIS 通道**不提供 DHCP**，IPv4 必须静态 /32。
   已激活且已有 IPv4 时直接返回当前状态，不重复下发 `AT+CGACT`，避免 `+CME ERROR`。
   **激活判据为三源合并**（`AT+CGPADDR` 有效地址 / `AT+CGCONTRDP` 返回
   `+CGCONTRDP:` 行 / `AT+CGACT?` 显式回 `<cid>,1`），不可只用 `AT+CGACT?`——
   本模组对该命令只回裸 `OK`（详见「已知注意事项」）。
4. **路由守护**：netifd 不会为无网关接口下发设备路由，
   daemon 每 `poll_interval` 秒补齐 `default dev <dev> metric <metric> onlink`。
5. **ucode 兼容性**：rpcd 插件不 `import` 任何第三方模块，只依赖 `fs`，
   也不使用 `String()`、`floor()` 等目标固件 libucode 中不存在的全局函数
   （详见「已知注意事项」）。
6. **前端调用层**：全部后端调用集中在 `resources/fm350/api.js`，
   用 `rpc.declare()` 声明，并把后端 CLI 的两条输出路径
   （经守护进程 API 的 `<键名>` 与本地直连的 `result`）归一为
   `{ ok, value }`，页面只需判断 `res.ok` 并读 `res.value`。
7. **信号与温度的取数来源**（均为实机验证后的正确命令，不要改回旧写法）：
   - 信号数值来自 `AT+CESQ`（`AT+CSQ` 在本模组恒为 `99,99`），
     按 `AT+GTCCINFO?` 服务小区的 `<rat>` 分支选择索引并做 3GPP 阶梯换算；
     频段 / PCI / ARFCN / TAC / 小区 ID 同样取自 `AT+GTCCINFO?` 的服务小区行。
     本次数值的实际来源会随 `Signal.source` 一起下发，在状态页展示，
     便于排障时确认走的是哪条分支。
   - 温度来自 `AT+GTSENRDTEMP=0`（单位 0.001 ℃，23 路），
     **不是** `AT+GTTHERMAL?`（那是温控状态标志）。
   - 载波聚合来自 `AT+GTCAINFO?`，本模组可能只回 `OK`，此时为空向量。

---

## 4. IMEI / 串号功能

插件**提供 IMEI 的读取、写入与更换能力**，但由于写入属高风险不可逆操作，
设置了多重保护，避免误触：

| 操作 | 条件 |
| --- | --- |
| 读取 | 始终可用（`AT+EGMREXT=0,7`） |
| 备份 | 始终可用，结果写入 `/etc/fm350/imei.backup` |
| 写入 / 更换 | 需同时满足：① UCI `fm350.main.imei_write=1`（默认 0）；② 请求带 `confirm=1`；③ 目标值为 15 位数字 |

写入流程中的保护措施：

1. 下发前自动读取并备份当前 IMEI 到 `/etc/fm350/imei.backup`；
2. 校验格式（15 位数字，并检查 Luhn 校验位）；
3. 写入后自动回读校验，操作全程记录到系统日志（`logger -t fm350d`）；
4. 未开启开关时，写入请求被后端拒绝；透传 AT 中的 IMEI 写入指令同样被拦截
   （`AT+EGMREXT=1,*`、`AT+EGMR=1,*`、`AT+SIMEI=`、`AT+CGSN=` 一律拒绝，
   只读形式 `AT+EGMREXT=0,<n>` / `AT+CGSN?` 放行）。

命令行：

```bash
fm350d imei read                   # 只读，安全
fm350d imei backup                 # 备份当前值
fm350d imei write <15位> --confirm # 写入（需先开启开关）
```

> **风险提示**：修改 IMEI 可能导致设备无法入网，或在部分地区违反当地法规。
> 仅应在设备维修、恢复原厂串号等合法场景下使用。
> 日常运维请保持 `imei_write=0`。

---

## 5. 编译

### 5.1 前置条件

| 依赖 | 说明 |
| --- | --- |
| OpenWrt / ImmortalWrt SDK 或完整 buildroot | 提供交叉工具链与打包框架 |
| `cargo` + 对应 target | 例如 `rustup target add aarch64-unknown-linux-musl` |

本 Makefile 自包含（只 `include` `rules.mk` 与 `package.mk`），
**不需要** `feeds/luci/luci.mk`，因此在没有 LuCI feed 的 SDK 中也能构建。

### 5.2 使用 SDK 编译

```bash
# 1) 准备 SDK（以 ImmortalWrt aarch64_cortex-a53 为例）
tar --zstd -xf immortalwrt-sdk-mediatek-filogic_gcc-14.4.0_musl.Linux-x86_64.tar.zst
cd immortalwrt-sdk-*/

# 2) 放入插件源码
cp -r luci-app-fm350 package/

# 3) 准备 Rust 交叉环境
rustup target add aarch64-unknown-linux-musl

# 4) 让构建系统识别新包（首次或改动 Makefile 后需要）
make defconfig

# 5) 编译
export PATH=/root/.cargo/bin:$PATH
make package/luci-app-fm350/compile V=s
```

产物位于：

```
bin/packages/aarch64_cortex-a53/base/luci-app-fm350-<版本>.apk
```

### 5.3 关键变量

| 变量 | 默认值 | 作用 |
| --- | --- | --- |
| `CARGO` | `cargo` | 指定 cargo 路径（非默认安装位置时必须设置） |
| `ARCH` | 由 SDK 决定 | 决定 Rust target triple |
| `RUST_TARGET` | 按 ARCH 推导 | 例如 `aarch64-unknown-linux-musl` |
| `RUST_LINKER_VAR` | 自动 | `CARGO_TARGET_<TRIPLE>_LINKER`，指向 `$(TARGET_CROSS)gcc` |

架构映射：

| OpenWrt ARCH | Rust target |
| --- | --- |
| `aarch64` | `aarch64-unknown-linux-musl` |
| `x86_64` | `x86_64-unknown-linux-musl` |
| `i386` | `i686-unknown-linux-musl` |
| `arm` | `armv7-unknown-linux-musleabihf` |
| `mipsel` | `mipsel-unknown-linux-musl` |
| `riscv64` | `riscv64gc-unknown-linux-musl` |

### 5.4 关于多语言

界面文案（菜单标题、页面标题、提示语）**直接使用简体中文**，
`_()` 调用在无 `.lmo` 时原样返回源字符串，因此包内不安装任何 `.lmo`，
在中文环境下显示完全正常。

若需要英文界面，做法是把 JS / JSON 中的源字符串换成英文，
再补一份 `po/zh_Hans/*.po`（英文 → 中文）并用 `po2lmo` 生成
`/usr/lib/lua/luci/i18n/fm350.zh-cn.lmo` 随包安装。

---

## 6. 部署与验证

```bash
# 安装（apk 包管理，ImmortalWrt 25.x / OpenWrt 24.10+）
apk add --allow-untrusted luci-app-fm350-<版本>.apk

# 安装（opkg 包管理）
opkg install luci-app-fm350_<版本>_*.ipk

# 确认 AT 设备节点存在
ls -l /dev/ttyUSB*

# 清理菜单与模块缓存（否则新菜单可能不出现）
rm -f /tmp/luci-indexcache*
rm -rf /tmp/luci-modulecache/
/etc/init.d/rpcd reload

# 启动服务
/etc/init.d/fm350d enable
/etc/init.d/fm350d start
/etc/init.d/fm350d status

# 命令行验证
fm350d status
fm350d info
fm350d signal
fm350d pdp
fm350d net
fm350d ports                # 枚举候选 AT 口（只探测，不改动配置）
fm350d dial
fm350d sms list

# 验证独占：查看后端持有的 fd，应有 ttyUSB 一项
ls -l /proc/$(pgrep -f 'fm350d daemon')/fd | grep ttyUSB

# 验证独占：查看后端持有状态与其他进程占用情况
fm350d ports | tr -d '\n' | grep -o '"stats":{[^}]*}'
#   期望: {"held_secs":N,"open":true,"other_pids":[],"path":"/dev/ttyUSB1",...}
#   open=true  -> 后端在整个运行期持续持有
#   held_secs  -> 随时间递增，证明中间没有释放重开
#   releases   -> 0，证明没有发生过空闲释放
#   other_pids -> 空，证明当前没有别的进程抢占

# 注意：不要用 `cat /dev/ttyUSB1` 是否报错来判断独占。
# OpenWrt 上一切以 root 运行，TIOCEXCL 对它无效，root 一定打得开。
# 要实测"能否发现抢占"，在另一进程故意打开该节点后看 other_pids：
#   sleep 30 < /dev/ttyUSB1 &
#   fm350d ports | tr -d '\n' | grep -o '"other_pids":\[[^]]*\]'
#   期望: "other_pids":["<该进程 pid>"]   <- 插件如实报告被抢占
```

> 注意：只读枚举与占用探测**不会**抢占正在使用的端口；探测对已被排他锁
> 占用的端口会直接失败 —— 这证明 `flock` 生效（本插件的探测与第二实例
> 都会被挡住）。但 `flock` 是建议锁，挡不住不申请锁的 root 进程，
> 那部分由 `stats.other_pids` 观测，见上文验证命令。

### 验收清单

| 项目 | 期望 |
| --- | --- |
| 菜单 | 网络 → Mobile Network → FM350-GL 可见，六个 tab 正常切换 |
| rpcd | `ubus list \| grep fm350` 显示 `luci.fm350` 与 `luci.fm350.sms` |
| 前端模块 | 浏览器控制台无 404；`/luci-static/resources/ubus.js` 在本固件不存在，页面改由 `rpc.js` 调用 |
| 状态页 | 首屏「网络概览」八张卡片齐全：综合信号、运营商（`中国移动 (46000)`）、网络类型（`5G NR`）、信号强度（RSRP）、信号质量（RSRQ）、信噪比（SINR）、服务小区、模组温度 |
| 综合信号 | 主值形如 `63.1 / 100`，副标题列出参与计算的指标；`fm350d status` 的 `overall` / `overall_level` / `overall_grade` / `overall_used` 四个字段齐全 |
| RSRQ 信号质量 | 主值形如 `-11.0 dB` 并带等级标签；`fm350d status` 的 `rsrq_grade` 非空 |
| 温度精度 | `fm350d status` 的 `temperature` 数值不超过 1 位小数（不应出现 `47.20000076293945` 这类尾数） |
| 信号细节 | RSRP / RSRQ / SINR 有数值（NR 下如 SS-RSRP -81 dBm、RSRQ -11.0 dB、SINR 17.0 dB），PCI / ARFCN / TAC / 小区 ID 非空；`CSQ` 显示 `99（模组不支持）`；频段显示 `n41` 且来源标注为「由 ARFCN 推算（模组未上报）」 |
| 运营商 | 显示 `中国移动 (46000)` 形式（名称为本地查表，数字码为模组上报） |
| 温度 | 「模组温度」显示 `md_5g` 实测值（如 46.1 ℃），明细表列出各路传感器；不应出现 `1` 或 `[object Object]` |
| 拨号 | 点击「保存并拨号」后 PDP 激活，IPv4 出现且 `default dev eth2 metric 30` 路由存在 |
| 出网 | `ping -I <蜂窝 IP> 223.5.5.5` 通 |
| 短信 | 能列出 PDU 短信，中文发送成功 |
| AT 终端 | `AT+CSQ` 正常返回；IMEI 写入类指令被拒绝并给出提示 |
| AT 端口选择 | 「服务设置 → AT 串口」下拉列出系统候选端口（含驱动 / VID:PID / 占用状态），`fm350d ports` 输出与之一致；切换端口后状态页仍有真实数值 |
| AT 口独占 | 后端 `/proc/<pid>/fd` 中**始终**存在 ttyUSB 项；`fm350d ports` 的 `stats.open` 为 `true`、`held_secs` 随时间递增、`releases` 为 `0`；`settings` 页「AT 口当前状态」显示已持续独占的秒数与「独占有效」；人为在另一进程打开该节点后，`stats.other_pids` 能列出该 pid |
| 配置改完即生效 | 在「服务设置」改 `at_port` 后**不重启服务**，下一次 `fm350d status` 即走新端口；改回后同样立即恢复 |
| 服务 | `logread -e fm350d` 无崩溃循环 |

### 已知注意事项

- **行尾必须为 LF**：`root/etc/init.d/fm350d` 若带 CRLF，procd 会报
  `can't open /etc/rc.common`。上传前请确认。
- **AT 口占用的两个方向**（务必分清，二者结论相反）：
  - **本插件申请不到端口**：若启动时端口已被别的服务以排他锁占用，后端打开
    会失败，状态页提示「AT 通道不可用」并给出「端口已被其他程序独占」的原因。
    请在「服务设置 → AT 串口」换用模组导出的其他 AT 口（用「重新探测」查看
    哪些端口空闲），或先停用占用该端口的服务。
  - **别人闯进来**：`TIOCEXCL` 对 root 无效，因此**不保证**挡住 `cat`、
    串口终端这类以 root 运行且不申请 `flock` 的程序。这种情况不会静默发生 ——
    `stats.other_pids` 会列出抢占者，daemon 日志会打「独占事实上已被破坏」，
    设置页也会显示告警。发现后停掉该程序即可。
  - 一句话概括：本插件在整个运行期不与任何其他程序协商共享 AT 口；
    同机制（同样使用排他锁）的程序一定进不来；不申请锁的程序进得来，
    但会被立刻看见。
- **断电后小区锁定丢失**：`AT+EMMCHLCK` 的锁定在断电后大概率不保存，需重新下发。
- **锁频建议先离线**：后端下发 `AT+GTACT` / `AT+EMMCHLCK` 时会自动
  先 `AT+CFUN=0` 再恢复 `AT+CFUN=1`。
- **目标固件 ucode 的坑**（`libucode20230711` / ucode 2026.x）：
  - 没有 `json` 模块，`import * as json from 'json'` 会报
    `Unable to resolve path for module 'json'`，导致整个 rpcd 插件加载失败、
    所有 `luci.fm350.*` 方法消失。解析交给浏览器端 JS 完成即可。
  - 没有 `String()` 全局构造器，调用会抛
    `Type error: left-hand side is not a function`，请用 `'' + v` 转换。
  - `floor()` 等数学函数已移出内置（`min` / `max` 仍在），需要 `import` `math`。
  - 内置 `json()`（解析）与 `sprintf('%J', ...)`（序列化）可用。
- **LuCI 模板形态**：LuCI 22+ 的模板是**编译后的二进制 `.ut`**，位于
  `/usr/share/ucode/luci/template/`；直接投放文本 `.uc` 模板不会被渲染。
  本插件的页面全部使用 JS view，不涉及该机制。
- **本固件没有 `ubus.js`**：`/www/luci-static/resources/` 下只有
  `rpc.js`、`uci.js`、`form.js`、`view.js`、`ui.js`、`fs.js` 等，
  写 `'require ubus'` 会 404（`HTTP error 404 while loading module ubus`），
  导致整个页面加载失败。正确做法是用 `'require rpc'` 加
  `rpc.declare({ object, method, params, expect: { '': {} } })`。
  可用 `ls /www/luci-static/resources/*.js` 先确认目标固件提供了哪些模块。
- **rpcd ucode 插件必须为带参方法声明 `args`**：rpcd 依据方法定义里的
  `args` 字段生成 ubus 方法签名，未声明时签名为空 `{}`。本固件的 ubus
  **严格校验参数**：类型不符、存在未声明的键，都会被以
  `UBUS_STATUS_INVALID_ARGUMENT`（`Invalid argument`）拒绝。
  因此带参方法必须写成：

  ```js
  'at': {
      args: { cmd: '' },      // 默认值的类型即签名类型（'' -> String，0 -> Integer）
      call: function(req) {
          let cmd = '' + req.args.cmd;
          ...
      }
  }
  ```

  自检命令：`ubus -v list luci.fm350`，应显示每个方法的参数签名
  （如 `"at":{"cmd":"String"}`）；若显示 `{}` 即为漏声明。
  另外注意 ucode 里 `'0'` 是**真值**，数值型参数不要用 `if (!x)` 判断，
  应先转字符串再显式比较（如 `if (x != '1')`）。
- **`rpc.declare` 的 params 与 ucode 的 args 键名必须一一对应**：
  前端 `params: ['cmd']` 对应后端 `args: { cmd: '' }`；
  名字不一致或前端传了多余键，同样会被 ubus 拒绝。
- **FM350 的 `AT+CGACT?` 不可作为激活判据**：实机对该命令只回裸 `OK`，
  不下发 `+CGACT:` 行，因此无法据此判断 PDP 是否已激活。若把它当唯一判据，
  会连锁导致：不读 `AT+CGPADDR` → 误报未激活 → 重复下发 `AT+CGACT=1,<cid>`
  → 收到 `+CME ERROR: 5847`（重复激活）→ 拨号被判失败。
  正确做法是以 `AT+CGPADDR=<cid>` 的有效非零地址为主判据，
  `AT+CGCONTRDP=<cid>` 的 `+CGCONTRDP:` 行为辅（3GPP 标准，仅激活时下发）。
  另注意本模组**拒绝**去激活（`AT+CGACT=0,<cid>` 回 `+CME ERROR: 54015`），
  且去激活失败后 `AT+CGPADDR` 仍返回原地址——这是模组的真实行为，不是缓存。
- **IPv6 未分配时的伪地址**：FM350 会在 `AT+CGPADDR` / `AT+GTDNS` 中回形如
  `0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.1` 或 `36.9.128.87.32.0.0.0.0.0.0.0.0.0.0.8`
  的点分十进制伪地址（16 段）。解析时必须按「4 段且非 0.0.0.0」过滤，
  否则会被误当成 IPv4 地址写入接口。
- **FM350 的 `AT+CSQ` 恒为 `99,99`，信号必须读 `AT+CESQ`**：本模组不支持
  CSQ 形式的信号强度（`99` 在 3GPP 中即"不可用"）。正确来源是
  `AT+CESQ`（3GPP TS 27.007 §8.69），字段顺序为
  `<rxlev>,<ber>,<rscp>,<ecno>,<rsrq>,<rsrp>,<ss_rsrq>,<ss_rsrp>,<ss_sinr>`；
  实测 NR 下为 `99,99,255,255,255,255,65,75,75`（前 6 位不可用，
  后 3 位才是 SS-RSRQ / SS-RSRP / SS-SINR）。
  注意 CESQ 返回的是 **3GPP 阶梯索引**，不是 dBm/dB，必须按制式换算：

  | 制式 | `<rat>` | 指标 | 索引位 | 换算（取区间下界） |
  | --- | --- | --- | --- | --- |
  | NR | 9 | SS-RSRP | 7 | 1 → -156 dBm，步长 1；126 → ≥ -31 |
  | NR | 9 | SS-RSRQ | 6 | 1 → -43 dB，步长 0.5；126 → [19.5, 20) |
  | NR | 9 | SS-SINR | 8 | 1 → -23 dB，步长 0.5；127 → ≥ 40 |
  | LTE | 4 | RSRP | 5 | 1 → -140 dBm，步长 1；97 → ≥ -44 |
  | LTE | 4 | RSRQ | 4 | 1 → -19.5 dB，步长 0.5；34 → ≥ -3 |
  | WCDMA | 2 | RSCP | 2 | 1 → -120 dBm，步长 1；96 → ≥ -25 |
  | WCDMA | 2 | Ec/Io | 3 | 1 → -24 dB，步长 0.5；49 → ≥ 0 |
  | GERAN | — | rxlev | 0 | 1 → -110 dBm，步长 1；63 → ≥ -48 |

  即索引 `n` 对应区间 `[base + (n-1)·step, base + n·step)`，实现取**下界**；
  索引 `0` 表示"低于可测下限"。

  `255` 一律视为未检出。LTE 的 SINR 不在 CESQ 中，需另取服务小区的
  `<rssnr_value>`（该值本身即 dB，有效范围 -100..100）。
- **`AT+GTCCINFO?` 没有字段名，不能按关键字匹配**：实机返回形如
  `1,9,460,0,149002,C2840C002,504990,128,,,15,74,74,64` 的纯数字行，
  响应行中**不存在** `PCI` / `band` 之类的字面量。若按关键字匹配将永远解析失败。
  正确做法是按位置解析，并用首个字段 `<IsServiceCell>`（1 = 服务小区，
  2 = 邻区）定位服务小区所在行。前 9 个字段布局对三种制式一致：
  `<IsServiceCell>,<rat>,<mcc>,<mnc>,<tac|lac>,<cellid>,<arfcn>,<pci|psc>,<band>`。
- **`AT+GTCCINFO?` 的 `<band>` 字段可能为空，此时按 ARFCN 反推**：
  三种制式的服务小区第 9 个字段都是 `<band>`，但实机在该小区下没有输出。
  插件在此情况下按 ARFCN / EARFCN 反推频段号，并在界面单独给出
  「频段来源」一行，明确区分模组上报与推算值：
  - NR 按 3GPP TS 38.104 全局频率栅格换算（`N_REF ≤ 599999` 时 `F = 5 kHz × N_REF`，
    `600000 ≤ N_REF ≤ 2016666` 时 `F = 3000 MHz + 15 kHz × (N_REF - 600000)`），
    再查 TS 38.101-1 的频段范围；
  - LTE 按 3GPP TS 36.101 的各频段 EARFCN 区间判定（`B1` 0–599、`B3` 1200–1949、
    `B5` 2400–2649、`B8` 3450–3799、`B34` 36200–36349、`B38` 37750–38249、
    `B39` 38250–38649、`B40` 38650–39649、`B41` 39650–41589）；
  - `n38`（2570–2620 MHz）完全落在 `n41`（2496–2690 MHz）内，`n77` 与 `n78`
    也重叠，此时标注为 `n38/n41` / `n77/n78`，不猜；
  - 落在本地表未覆盖的范围则显示 `-`，WCDMA 的 UARFCN 不参与推算。
- **运营商名称是本地查表，不是模组上报**：`AT+COPS?` 在本模组返回
  `+COPS: 0,2,"46000",11`（格式位 `2` = 数字格式），因此 `Signal.operator`
  是数字码。界面上的「中国移动」等名称来自插件内置的 MCC/MNC 表
  （`operator_name` 字段），数字码始终一并显示；未收录的编码只显示数字码。
- **`<bandwidth>`（GTCCINFO 第 10 个字段）也可能为空**：实机同样不上报，
  此时界面「带宽档位」显示 `-`。
- **模组温度必须用 `AT+GTSENRDTEMP=0`，不要用 `AT+GTTHERMAL?`**：
  `AT+GTTHERMAL?` 返回 `+GTTHERMAL: 1`，那是**温控状态标志**（1 = 正常），
  不是温度；把它当温度显示会得到恒定的 `1`。
  而温度命令的**读形式不支持**：`AT+GTSENRDTEMP?` 回
  `+CME ERROR: phone failure`，必须带参数下发 `AT+GTSENRDTEMP=0`，
  才会逐路返回 `+GTSENRDTEMP:<id>,<raw>`（共 23 路）。
  读数单位为 **0.001 ℃**（`raw / 1000`），`raw == 0` 表示该路未装配。
  常用传感器编号：`1` = soc_max，`10` = md_5g（5G 基带），
  `14` = ltepa_ntc，`15` = nrpa_ntc，`16` = rf_ntc。
- **载波聚合信息可能为空**：本模组 `AT+GTCAINFO?` 只回 `OK`（无 CA 信息），
  此时状态页「载波聚合」显示为 `-`，属正常现象。

---

## 7. UCI 配置参考（`/etc/config/fm350`）

| 键 | 默认 | 说明 |
| --- | --- | --- |
| `enabled` | `1` | 是否启用守护进程 |
| `at_port` | `/dev/ttyUSB1` | AT 串口（可在设置页从探测结果中选择，也支持手填绝对路径） |
| `baudrate` | `115200` | 波特率 |
| `at_timeout` | `10` | 单条 AT 指令超时（秒） |
| `apn` | `cmiot5g` | 接入点名称 |
| `pdp_type` | `IPV4V6` | PDP 类型 |
| `cid` | `1` | PDP 上下文 ID |
| `auth` | `none` | 认证方式 |
| `iface` | `fm350` | IPv4 接口名 |
| `iface_v6` | `fm350v6` | IPv6 接口名（`device=@fm350`） |
| `data_dev` | `auto` | 数据通道网卡，auto 按驱动探测 |
| `metric` | `30` | 路由优先级 |
| `ipv6` | `1` | 是否创建 IPv6 子接口 |
| `auto_dial` | `1` | 自动拨号 |
| `route_guard` | `1` | 路由守护 |
| `poll_interval` | `30` | 巡检周期（秒，最小 5） |
| `api_port` | `8766` | 本地 API 端口（仅 127.0.0.1） |
| `imei_write` | `0` | IMEI 写入开关；为 0 时后端拒绝一切 IMEI 写入 |

`at_port` 写入前会校验必须为绝对路径，避免误填 `ttyUSB1` 这类相对值后
`open()` 在当前工作目录下静默失败，而界面却显示"保存成功"。

---

## 8. 命令行参考

```
fm350d daemon                     守护模式
fm350d status|info|signal|pdp|net|cell|lock
fm350d ports [--no-probe]         枚举候选 AT 口与占用状态（不占用 AT 口）
fm350d dial                       拨号并配置接口
fm350d hangup                     断开并拆除接口
fm350d lock-band <参数>           例如 14 / 2 / 20 / 20,6,3,5078
fm350d lock-cell <参数>           例如 1,11,0,627264,280,3 或 0
fm350d at <命令>                  透传 AT
fm350d imei read|backup|write     读 / 备份 / 写（写需 --confirm 且开关打开）
fm350d sms list|send|delete|storage
fm350d rat [顺序]                 例如 NR:LTE:WCDMA
fm350d sim <0|1> / cfun <0|1> / usbmode <模式> / reboot
fm350d config                     输出配置 JSON
fm350d set <键> <值>              写入配置项
```

说明：

- 温度与信号细节不需要单独子命令，都包含在 `fm350d status` / `fm350d signal`
  的输出中（`status` 的 `temperature` 字段为结构化对象，
  `signal` 含 `source` 字段标明数值来源）。
- `fm350d ports` 的输出字段：`path`、`name`、`driver`、`vid`、`pid`、
  `vendor`、`product`、`likely_fm350`、`current`、`busy`、`note`。
  `--no-probe` 表示只读 sysfs 不做独占探测（更快）。
- `stats` 字段（`fm350d ports` 与 `/api/ports` 都返回）：`path`、`open`、
  `held_secs`、`opens`、`releases`、`other_pids`。`other_pids` 是除本进程
  以外当前也打开了该 tty 的进程号列表，为空表示独占事实成立。
- 想直接核对 AT 层，可用 AT 终端页或 `fm350d at`：

  | 目的 | 正确命令 | 错误/不可用写法 |
  | --- | --- | --- |
  | 信号强度 | `AT+CESQ` | `AT+CSQ`（恒回 `99,99`） |
  | 服务小区 / 频段 / PCI | `AT+GTCCINFO?` | — |
  | 温度 | `AT+GTSENRDTEMP=0` | `AT+GTSENRDTEMP?`（`phone failure`）<br>`AT+GTTHERMAL?`（状态标志，非温度） |
  | 载波聚合 | `AT+GTCAINFO?` | — |

---

## 9. 设计取舍

| 项 | 选择 | 理由 |
| --- | --- | --- |
| 前端 | LuCI JS view/form | 与 LuCI 26.x 原生一致，无 Lua 兼容层依赖，表单校验与保存语义由框架保证 |
| 菜单注册 | `menu.d/*.json` | LuCI 22+ 的标准注册方式，父节点用 `firstchild` 自动渲染为 tab 条 |
| 后端 | Rust 单二进制 | 无运行时依赖，串口独占与状态机可做严格串行化 |
| AT 口持有方式 | 排他锁 + 持久持有 | 全程持有并申请排他 `flock`：同机制的程序（端口探测、第二实例）都被挡住；整个运行期不释放，杜绝交错收发，也免去反复开关串口的握手开销 |
| 独占强度 | 建议锁 + 主动观测 | `TIOCEXCL` 对 root（`CAP_SYS_ADMIN`）无效，不能当作强制手段；因此用 `other_openers()` 扫描 `/proc/*/fd`，把"是否真独占"变成可观测事实，而不是写一条站不住的保证 |
| AT 口释放时机 | 仅串口异常 / 切换端口 | 正常运行时端口不释放；改了 `at_port` 则在**下一次 AT 操作**当场换端口（daemon 巡检兜底），不必重启服务 |
| 配置生效 | 逐请求重读（2 秒缓存） | daemon API 每个请求都取最新 UCI 配置，改完即生效、不必重启；2 秒缓存避免逐请求 fork 十余次 `uci` |
| 端口来源 | 扫描 sysfs 生成候选 + 允许手填 | 不写死设备节点，兼容不同枚举顺序与不同 USB 模式导出形态 |
| 与 LuCI 通信 | rpcd ucode 薄代理 | 避免前端直接 exec；参数单引号转义防注入 |
| 样式 | 独立 CSS，无内联样式 | 便于主题适配（浅色 / 深色）与统一维护 |
| i18n | 源文案即中文，不发 `.lmo` | 避免同值条目被 po2lmo 丢弃后产生空翻译文件与无效依赖 |
