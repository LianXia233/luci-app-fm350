# 后端重构说明（1.0.11）

本轮重构以 **F22 原厂固件逆向结果**为最高依据（AP 域 OpenWrt 19.07 + MD 域
MOLY NR15），把原来 8 个大文件拆成按职责划分的模块树。产物不变：仍是
单一静态二进制 `/usr/sbin/fm350d`；页面路径、UCI 配置项、依赖、安装结构
零改动；IMEI 相关代码逻辑零改动。

## 目录结构

```text
rust/src/
├── main.rs            二进制入口（仅转发到 cli::run）
├── lib.rs             crate 根：分层图、两条硬约束
├── cli.rs             命令行派发（daemon 转发优先，其次本地直连）
├── api/               本地 JSON API（仅 127.0.0.1）
│   ├── mod.rs         serve 主循环、回环绑定
│   ├── http.rs        报文读写、统一响应信封、percent-decoding
│   └── route.rs       路由表与参数提取
├── daemon/            守护：一轮巡检的编排与各巡检器
│   ├── mod.rs         编排（巡检顺序与原因见模块头注释）
│   ├── singleton.rs   flock 单实例守卫
│   ├── dial.rs        PDP 状态机：未激活就拨 / 地址偏了就补 / 没地址就重拨
│   ├── v6.rs          IPv6 维护工位：模组轮询、刷新时机、恢复分级
│   ├── sig.rs         参数签名巡检（uci set 热生效）
│   ├── guard.rs       数据面自愈 + 双栈连通性保活 + 接口冲突告警
│   └── port.rs        AT 口看护：热切换、消失改选、独占事实巡检
├── at/                AT 层：独占持有 + 协议 + 命令字面量
│   ├── mod.rs         AtPort / AtHandle（进程内串行、TIOCEXCL）
│   ├── port.rs        端口探测与识别（sysfs vid/pid/driver）
│   ├── parse.rs       响应解析原语
│   ├── cmd.rs         全部 AT 命令字面量的唯一出处
│   └── guard.rs       IMEI 写入拦截
├── modem/             模组状态与动作（AT 的业务语义）
│   ├── mod.rs         status / run / f 等公共入口
│   ├── info.rs        模组信息（厂商/型号/固件/IMSI/ICCID）
│   ├── signal.rs      信号解析（CSQ/CESQ/GTCCINFO/GTCAINFO）
│   ├── pdp.rs         PDP 状态机与拨号（激活判据只有 CGACT?）
│   ├── bind.rs        AT+EMBIND 数据通道绑定体检
│   ├── lock.rs        锁频段 / 锁小区 / 制式 / SIM / CFUN / USB 模式
│   ├── band.rs        频段换算与运营商名
│   └── temp.rs        传感器温度（GTSENRDTEMP）
├── net/               主机侧网络：把模组地址变成能上网的接口
│   ├── mod.rs         边界说明（数据面终止在 MD 内）与子模块表
│   ├── shell.rs       外部命令唯一出口（转义、失败判定、dry-run）
│   ├── iface.rs       UCI 接口骨架与防火墙登记
│   ├── gateway.rs     网关规划、ARP 实测、参数签名
│   ├── v6.rs          IPv6 取址方式、RA 主动索取、刷新判据
│   ├── heal.rs        路由守卫、自愈动作、apply_after_dial
│   └── probe.rs       数据网卡探测、连通性探测、停滞判定
├── sms/               短信（PDU 模式）
│   ├── mod.rs         AT 交互与对外操作
│   ├── pdu.rs         PDU 字段解析与构造
│   └── gsm7.rs        GSM 03.38 / UCS2 编解码
├── imei.rs            IMEI 读取 / 备份 / 写入（写号守护不变）
├── addr.rs            地址解析工具
├── config.rs          /etc/config/fm350 读写
└── log.rs             统一日志出口（stderr，格式与重构前逐字一致）
```

## 与原实现的行为差异

| 差异 | 原实现 | 重构后 | 依据 |
|---|---|---|---|
| v6 刷新判据 | 模组上报地址必须出现在接口地址列表（dhcpv6/ra 下恒不成立，导致 fm350v6 每周期重启） | 仅 static 模式比对相等，其余只看有无全局地址 | odhcp6c IA_NA / 内核 SLAAC 地址来源与 CGPADDR 不同 |
| ra 模式判定 | 非 static/dhcpv6/off 的**一切取值**（含空值）都按 ra | 仅显式 `v6_mode=ra`；未识别取值回落 dhcpv6 | 缺省配置不应走用户没选过的路径 |
| ra 无 RA 时 | 回落把 CGPADDR 地址写成静态 `/128` | 主动索取 RS（5 轮 x 3 s），未果如实记录跳过本轮 | 原厂 `AT+EIF "ipadd",2,"<v6>/%lu"` 恒带前缀长度；CGPADDR 去激活后仍回残留地址 |
| RA 触发方式 | 被动等周期性 RA | 照抄原厂 netagent 硬动作：写 `router_solicitations` | MD 侧 `d2cm_ipv6_no_ra_cb_hdl` 对应 AP 侧主动索取 |
| 服务小区解析 | `+GTCCINFO:` 行先被整体跳过（dead code），LTE SINR/频段/带宽恒空 | 先剥前缀再解析，只取服务小区（第 1 字段 = 1）行 | 实机 `AT+GTCCINFO?` 每行都以该前缀开头 |
| 错误响应 | 仅裸 `ERROR` 被跳过 | `+CME ERROR` / `+CMS ERROR` / `ERROR` 一律视为空值 | `AT+GTSENRDTEMP?` 传感器不可用时回 `+CME ERROR` |
| 拨号失败诊断 | 只报 AT 错误 | 追加 `AT+EMBIND?` 体检结论（如 `ccmni-only`） | 绑定落在 `M-CCMNI` 时数据进模组自身，主机必然无地址 |

除上表以外的一切行为（拨号流程、接口骨架、网关选择、自愈分级、冷却时长、
日志文案）逐字保留。

## 回归验证清单

1. **拨号**：`fm350d dial` 后接口拿到 v4 + v6，`/api/dial` 返回 `net` 非空。
2. **断线重连**：拔掉 USB 重插；AT 口 tty 编号漂移后 3 轮内自动改选。
3. **稳态无重启**：dhcpv6 与 ra 模式各观察 ≥ 10 分钟，`fm350v6` 不应周期性
   down/up（本轮修复的核心场景）；`logread -f | grep fm350d` 无每 30 s 一条的
   刷新日志。
4. **RA 模式取址**：`uci set fm350.main.v6_mode='ra'` 后重拨，接口应通过
   SLAAC 拿到**带前缀**的全局地址；RA 不下发时日志提示主动索取未果且**不写** /128。
5. **信号页**：LTE 下 SINR / 频段 / 带宽有值（dead code 修复的验证点）。
6. **温度页**：无传感器槽位显示为空而非 `phone failure` 字样。
7. **net_guard**：`iptables -I OUTPUT -p icmp -j DROP` 之类手段模拟断网，
   确认 v4 第 1 级重建 / v6 不重拨（v4 正常时）的分级行为不变。
8. **数据面自愈**：保持 1.0.10-r2 的三级升级（复位网卡 → 重拨 → 重启模组）。
9. **IMEI**：读取正常；写入在 `imei_write=0` 时被拒，开启后仍需 `--confirm`。

## 测试

```sh
cd rust && cargo test      # 159 项单元测试
cargo build --release      # 产物 target/<triple>/release/fm350d
```

目标机交叉编译沿用 Makefile 既有逻辑（musl 静态链接）。
