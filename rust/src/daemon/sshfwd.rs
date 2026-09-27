//! SSH 转发巡检器（`SshFwdWatch`）。
//!
//! ## 功能与边界
//!
//! 把「模组内 dropbear:22」经 ADB 通道转发到路由器 LAN：
//!
//! ```text
//! LAN 客户端 -> 0.0.0.0:<lan_port> (socat, fork)
//!            -> 127.0.0.1:<fwd_port> (adb forward)
//!            -> 模组 adbd (USB) -> 模组内 127.0.0.1:22 (dropbear)
//! ```
//!
//! ## 设计约束（必须一直成立）
//!
//! 1. **默认关闭**：`sshfwd_enable=0` 时本巡检器除「停掉自己拉起的 socat」外
//!    不做任何动作 —— 在没有 ADB 通道的模组/固件上，开启前连一次 shell 都不会
//!    多执行（首次检测发现缺 `adb`/`socat` 只置 `supported=false` 并返回）。
//! 2. **全部幂等**：forward 重建、socat 拉起、pid 检测重复执行无副作用；
//!    断电/重启/USB 重枚举后下一轮自动收敛。
//! 3. **不碰串口**：本巡检器不需要 AT 句柄，与拨号/AT 巡检完全正交。
//! 4. **下游独立于上游**：ADB 离线时 socat 监听保持（LAN 侧连接会被立即
//!    关闭而不是无响应），设备恢复后 15 s 内自动恢复转发。
//! 5. **端口冲突不抢占**：LAN 端口被外部进程占用（例如旧版独立
//!    fm350-ssh-fwd 服务）时只上报 `port_conflict`，绝不 kill 他人进程。
//!
//! ## 实现要点
//!
//! - socat 以后台 `&` 拉起后由 init 收养（daemon 退出它继续活着），包装 sh
//!   由本进程 `try_wait` 回收，不产生 zombie；**严禁 nohup**（精简 BusyBox
//!   普遍缺失，见 [`ensure_socat`] 内注释）。spawn 后强制 `kill -0` 存活校验。
//! - pid 记账用 `/var/run/fm350-sshfwd.pid`，避免 `pgrep/pkill -f` 的
//!   「模式串匹配到自身命令行」自杀陷阱。
//! - 每 15 s 一轮（内部节流，独立于主循环 poll_interval），状态只在
//!   发生变化时打一条 log，避免刷屏。

use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::config::Config;
use crate::net::shell::real;

/// socat 记账文件：记录本巡检器拉起的 socat PID。
const PID_FILE: &str = "/var/run/fm350-sshfwd.pid";

/// 巡检节流间隔：`adb devices` 每次要经 USB 往返，太快无意义。
const RUN_INTERVAL: Duration = Duration::from_secs(15);

/// 全局状态快照：daemon 巡检侧写入，API/CLI 侧只读。
static STATE: Mutex<SshFwdState> = Mutex::new(SshFwdState::disabled());

#[derive(Debug, Clone, serde::Serialize)]
pub struct SshFwdState {
    /// UCI 开关当前值。
    pub enabled: bool,
    /// 环境支持（adb + socat 都存在、端口配置合法）。
    pub supported: bool,
    /// ADB 设备在线（`adb devices` 出现 `device` 行）。
    pub adb_online: bool,
    /// adb forward 条目存在。
    pub forward_ok: bool,
    /// 本巡检器拉起的 socat 存活。
    pub socat_running: bool,
    /// LAN 端口被外部进程占用（未抢占）。
    pub port_conflict: bool,
    pub lan_port: u16,
    pub fwd_port: u16,
    /// 人话说明（空串 = 一切正常）。
    pub detail: String,
    /// 快照时间（unix 秒；0 = 尚未巡检过）。
    pub updated: u64,
}

impl SshFwdState {
    /// 禁用/未巡检的初始态。必须是 const：供 static 初始化。
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            supported: true,
            adb_online: false,
            forward_ok: false,
            socat_running: false,
            port_conflict: false,
            lan_port: 0,
            fwd_port: 0,
            detail: String::new(),
            updated: 0,
        }
    }
}

/// 读取当前快照（API `/api/sshfwd` 与 CLI `fm350d sshfwd` 共用）。
pub fn snapshot() -> serde_json::Value {
    let s = STATE.lock().map(|g| g.clone());
    match s {
        Ok(s) => json!(s),
        Err(_) => json!({ "error": "状态锁被占（巡检线程持锁异常）" }),
    }
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// SSH 转发巡检器。每轮主循环调用 [`tick`]，内部自行节流。
pub struct SshFwdWatch {
    last_run: Option<Instant>,
}

impl SshFwdWatch {
    pub fn new() -> Self {
        Self { last_run: None }
    }

    /// 主循环入口。关闭态每轮都走（保证停机语义即时），启用态 15 s 一轮。
    pub fn tick(&mut self, cfg: &Config) {
        if !cfg.sshfwd_enable {
            self.stop();
            self.apply(SshFwdState::disabled());
            self.last_run = None;
            return;
        }
        if let Some(t) = self.last_run {
            if t.elapsed() < RUN_INTERVAL {
                return;
            }
        }
        self.last_run = Some(Instant::now());
        self.run_once(cfg);
    }

    /// 一轮完整巡检（启用态）。
    fn run_once(&mut self, cfg: &Config) {
        let lan = cfg.sshfwd_lan_port;
        let fwd = cfg.sshfwd_fwd_port;
        let mut s = SshFwdState {
            enabled: true,
            supported: true,
            adb_online: false,
            forward_ok: false,
            socat_running: false,
            port_conflict: false,
            lan_port: lan,
            fwd_port: fwd,
            detail: String::new(),
            updated: now_ts(),
        };

        if lan == 0 || fwd == 0 {
            s.supported = false;
            s.detail = "SSH 转发端口配置非法（0）".into();
            self.apply(s);
            return;
        }

        // 1) 环境支持：缺 adb/socat 时到此为止，零副作用。
        if !real("command -v adb >/dev/null 2>&1 && command -v socat >/dev/null 2>&1").0 {
            s.supported = false;
            s.detail = "系统缺少 adb 或 socat，SSH 转发在此设备上不可用".into();
            self.apply(s);
            return;
        }

        // 2) LAN 监听（独立于 ADB 状态：下游断了监听仍保持，恢复即通）。
        self.ensure_socat(&mut s, lan, fwd);

        // 3) ADB 设备在线？
        let (_, devs) = real("adb devices 2>/dev/null");
        if !devs.lines().any(|l| l.ends_with("\tdevice")) {
            s.adb_online = false;
            s.forward_ok = false;
            s.detail = if s.socat_running {
                "ADB 设备离线，等待重连（LAN 监听保持）".into()
            } else {
                "ADB 设备离线".into()
            };
            self.apply(s);
            return;
        }
        s.adb_online = true;

        // 4) forward 幂等重建。
        let (_, list) = real("adb forward --list 2>/dev/null");
        let want = format!("tcp:{}", fwd);
        if list.lines().any(|l| l.contains(&want) && l.contains(" tcp:22")) {
            s.forward_ok = true;
        } else {
            let (ok, out) = real(&format!("adb forward tcp:{} tcp:22 2>&1", fwd));
            s.forward_ok = ok;
            if !ok {
                s.detail = format!("adb forward 建立失败：{}", out);
            }
        }

        self.apply(s);
    }

    /// 保证「本巡检器拉起的 socat」存活；被外部占用则不抢占。
    fn ensure_socat(&self, s: &mut SshFwdState, lan: u16, fwd: u16) {
        // pid 文件里的进程还活着 → 复用。
        let (alive, out) = real(&format!(
            "[ -s {f} ] && kill -0 $(cat {f}) 2>/dev/null && echo ALIVE",
            f = PID_FILE
        ));
        if alive && out.contains("ALIVE") {
            s.socat_running = true;
            return;
        }

        // LAN 端口被其它进程占用（旧独立转发服务等）→ 只上报，不抢占。
        let (occupied, _) = real(&format!(
            "netstat -tln 2>/dev/null | grep ':{} ' | grep -q LISTEN",
            lan
        ));
        if occupied {
            s.port_conflict = true;
            s.detail = format!(
                "端口 {} 已被其它进程占用（如旧版独立转发服务），内置转发未启动",
                lan
            );
            return;
        }

        // 后台启动 + pid 记账。**不能加 nohup**：BusyBox 精简环境普遍没有
        // nohup，`nohup socat ... &` 会让后台命令变成 nohup 本身并立即以
        // "not found" 退出 —— pid 文件里记下的是一个从未活过的 pid
        // （1.0.15-r1 实机实锤）。stdout/stderr 已重定向，sh 退出后 socat
        // 被残缺进程组之外无 HUP 来源，孤儿由 init 收养，无需 nohup。
        let script = format!(
            "socat TCP4-LISTEN:{l},fork,reuseaddr TCP4:127.0.0.1:{p} >/dev/null 2>&1 & echo $! > {f}",
            l = lan,
            p = fwd,
            f = PID_FILE
        );
        match Command::new("sh")
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                std::thread::sleep(Duration::from_millis(500));
                let _ = child.try_wait();
                // spawn 后强制存活性校验：不能乐观置 true，否则「状态说
                // 运行、端口无人监听」的假绿会掩盖真实故障。
                let (alive, out) = real(&format!(
                    "kill -0 $(cat {f}) 2>/dev/null && echo ALIVE",
                    f = PID_FILE
                ));
                if alive && out.contains("ALIVE") {
                    s.socat_running = true;
                } else {
                    s.detail = "socat 拉起后立即退出（见 logread 中 socat 输出）".into();
                }
            }
            Err(e) => {
                s.detail = format!("socat 拉起失败：{}", e);
            }
        }
    }

    /// 关闭态：只停自己拉起的 socat（pid 文件之外的进程绝不碰）。
    fn stop(&self) {
        real(&format!(
            "[ -s {f} ] && kill $(cat {f}) 2>/dev/null; rm -f {f}",
            f = PID_FILE
        ));
    }

    /// 写入全局快照；关键位变化时打一条 log。
    fn apply(&self, s: SshFwdState) {
        let mut g = match STATE.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let changed = g.updated != 0
            && (g.enabled != s.enabled
                || g.supported != s.supported
                || g.adb_online != s.adb_online
                || g.forward_ok != s.forward_ok
                || g.socat_running != s.socat_running
                || g.port_conflict != s.port_conflict);
        let first = g.updated == 0 && s.updated != 0;
        *g = s;
        drop(g);
        if changed || first {
            crate::infof!(format_args!(
                "SSH 转发巡检: {}",
                serde_json::to_string(&snapshot()).unwrap_or_default()
            ));
        }
    }
}

/// 判断 ADB 设备当前是否在线（exec 与 bootstrap 共用）。
fn adb_online() -> bool {
    let (_, devs) = real("adb devices 2>/dev/null");
    devs.lines().any(|l| l.ends_with("\tdevice"))
}

/// SSH 客户端密钥路径（router 侧，用于免密登录模组 dropbear）。
const CLIENT_KEY: &str = "/etc/fm350/modem_client_key";

/// 模组 shell 命令执行超时（秒）。adbd 离线时 `adb shell` 会挂住，必须包裹。
const EXEC_TIMEOUT: u64 = 15;

/// 经 ADB 通道向模组下发 shell 命令（USB 直连，不依赖 dropbear）。
pub fn adb_exec(cmd: &str) -> Result<String, String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err("命令为空".into());
    }
    if !adb_online() {
        return Err("ADB 设备离线，无法执行（等待重连中）".into());
    }
    let (ok, out) = real(&format!(
        "timeout {t} adb shell {c} 2>&1",
        t = EXEC_TIMEOUT,
        c = crate::net::shell::sq(cmd)
    ));
    if out.contains("error:") && (out.contains("device") || out.contains("offline")) {
        return Err(out);
    }
    if !ok {
        return Err(if out.is_empty() {
            format!("adb shell 执行失败（超时 {}s）", EXEC_TIMEOUT)
        } else {
            out
        });
    }
    Ok(out)
}

/// 经 ADB 通道把 router 侧公钥注入模组 dropbear 免密清单（幂等）。
///
/// 这是一次性引导：模组 dropbear 是 root 空密码（LAN 暴露），但 ssh 通道
/// 交互式输入密码在脚本环境不可行，因此统一收敛到密钥认证 —— 注入成功后
/// 建议用户在模组侧关闭空密码登录（本插件不代改模组 sshd 配置）。
pub fn ssh_bootstrap() -> Result<String, String> {
    if !real("command -v dbclient >/dev/null 2>&1 && command -v dropbearkey >/dev/null 2>&1").0 {
        return Err("router 缺少 dbclient / dropbearkey（dropbear 客户端组件）".into());
    }
    if !adb_online() {
        return Err("ADB 设备离线，无法注入公钥".into());
    }
    if !real(&format!("[ -f {k} ]", k = CLIENT_KEY)).0 {
        let (ok, out) = real(&format!(
            "mkdir -p /etc/fm350 && dropbearkey -t ed25519 -f {k}",
            k = CLIENT_KEY
        ));
        if !ok {
            return Err(format!("生成客户端密钥失败：{}", out));
        }
    }
    let (ok, pubkey) = real(&format!(
        "dropbearkey -y -f {k} 2>/dev/null | grep '^ssh-'",
        k = CLIENT_KEY
    ));
    if !ok || pubkey.trim().is_empty() {
        return Err("提取客户端公钥失败".into());
    }
    let pk = pubkey.trim();
    // 公钥内容为算法名 + base64 + 注释，不含引号；双层引号：router sh 的
    // 单引号包 adb 参数，模组 ash 侧用双引号引用变量展开结果。
    let (ok, out) = real(&format!(
        "adb shell 'mkdir -p /root/.ssh; chmod 700 /root/.ssh; grep -qF \"{pk}\" /root/.ssh/authorized_keys 2>/dev/null || echo {pk} >> /root/.ssh/authorized_keys; chmod 600 /root/.ssh/authorized_keys' 2>&1",
        pk = pk
    ));
    if !ok {
        return Err(format!("公钥注入失败：{}", out));
    }
    Ok("公钥已就位（幂等注入）".into())
}

/// 经 SSH 通道（adb forward → 模组 dropbear，密钥免密）下发 shell 命令。
pub fn ssh_exec(cfg: &crate::config::Config, cmd: &str) -> Result<String, String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err("命令为空".into());
    }
    if !real("command -v dbclient >/dev/null 2>&1").0 {
        return Err("router 缺少 dbclient".into());
    }
    if !real(&format!("[ -f {k} ]", k = CLIENT_KEY)).0 {
        return Err("尚未注入 SSH 公钥，请先点击「注入 SSH 公钥」".into());
    }
    let (ok, out) = real(&format!(
        "timeout {t} dbclient -y -i {k} root@127.0.0.1:{p} {c} 2>&1",
        t = EXEC_TIMEOUT,
        k = CLIENT_KEY,
        p = cfg.sshfwd_fwd_port,
        c = crate::net::shell::sq(cmd)
    ));
    if !ok {
        return Err(if out.is_empty() {
            format!(
                "SSH 执行失败（超时 {}s；请确认转发链已就绪）",
                EXEC_TIMEOUT
            )
        } else {
            out
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_state_is_const_and_zeroed() {
        let s = SshFwdState::disabled();
        assert!(!s.enabled);
        assert!(s.supported);
        assert!(!s.socat_running);
        assert_eq!(s.updated, 0);
    }

    #[test]
    fn snapshot_is_valid_json_object() {
        let v = snapshot();
        assert!(v.is_object());
        assert_eq!(v["enabled"], false);
    }

    #[test]
    fn disabled_tick_never_marks_state_dirty() {
        // 关闭态 tick 只写 disabled 快照：updated 保持 0（未巡检）语义。
        let mut w = SshFwdWatch::new();
        let cfg = Config::default(); // sshfwd_enable = false
        w.tick(&cfg);
        let v = snapshot();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["updated"], 0);
    }
}
