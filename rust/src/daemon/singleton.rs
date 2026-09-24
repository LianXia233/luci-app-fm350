//! 单实例守卫：同一个节点上只允许一个 `fm350d daemon` 触碰 AT 口。
//!
//! ## 为什么需要它
//!
//! AT 口是独占资源（打开时下发 `TIOCEXCL`），但内核并不保证真正的排他：
//! 对具备 `CAP_SYS_ADMIN` 的进程（root 就是）`TIOCEXCL` 形同虚设，见
//! [`crate::at::port`] 模块注释。因此「只有一个实例」这件事必须由本插件
//! 自己保证。
//!
//! 典型撞车场景是**换包升级或 procd 自动拉起**：旧进程还没退出，新进程已经
//! start，两者同时持有串口，日志里刷一长串 "Unable to acquire exclusive
//! lock"，且互相打断对方的 AT 会话 —— 表现为状态乱跳、拨号时好时坏。

use std::fs::File;

/// 运行时锁文件。放在 `/var/run`（tmpfs）下：重启自然消失，不需要清理逻辑。
pub const LOCK_FILE: &str = "/var/run/fm350d.lock";

/// 已持有的单实例锁。
///
/// 语义上是个 RAII 守卫：`Singleton` 被 drop（文件关闭）时内核自动释放
/// flock，无需显式解锁，进程崩溃也不会留下死锁文件。
pub struct Singleton {
    _file: File,
}

/// 尝试抢占单实例锁；抢不到返回 `None`。
///
/// 调用方拿到 `None` 必须**直接退出，绝不触碰 AT 口** —— 与其两个实例互相
/// 打断，不如什么都不做。
pub fn acquire() -> Option<Singleton> {
    acquire_at(LOCK_FILE)
}

/// 以指定路径抢占锁。
///
/// 生产代码固定走 [`acquire`]（[`LOCK_FILE`]）；此入口的存在是为了让单测
/// 能在**无特权的临时目录**里验证 flock 语义 —— `/var/run` 需要 root 才能
/// 建文件，CI runner 是非特权用户，直接测生产路径必然失败。
pub fn acquire_at(path: &str) -> Option<Singleton> {
    let f = open_lock_file(path)?;
    if !try_flock(&f)? {
        return None;
    }
    Some(Singleton { _file: f })
}

#[cfg(unix)]
fn open_lock_file(path: &str) -> Option<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()
}

/// 非 Unix 主机（仅本机 `cargo check` / `cargo test` 用）不做路径区分，
/// 且不加锁：目标平台始终是 Linux，该分支不会进入实际部署。
#[cfg(not(unix))]
fn open_lock_file(path: &str) -> Option<File> {
    let _ = path;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(std::env::temp_dir().join("fm350d.lock"))
        .ok()
}

/// 非阻塞独占加锁。返回 `false` 表示锁已被别人持有。
#[cfg(unix)]
fn try_flock(f: &File) -> Option<bool> {
    use std::os::unix::io::AsRawFd;

    // SAFETY: fd 来自本函数刚打开且仍持有所有权的文件，flock 是幂等的
    // 文件描述附表操作，不会越界访问。
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Some(rc == 0)
}

#[cfg(not(unix))]
fn try_flock(_f: &File) -> Option<bool> {
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁文件路径必须落在 tmpfs，绝不能是持久化目录 —— 否则一次异常退出
    /// 留下的空文件会让后续所有实例都无法启动。
    #[test]
    fn lock_file_lives_under_var_run() {
        assert!(LOCK_FILE.starts_with("/var/run/"));
    }

    /// 连续抢占两次：第二次必须失败（同一进程内 flock 也会互相排斥，
    /// 因为两次 open 得到的是不同的文件描述附表条目）。
    ///
    /// 走 `acquire_at` + 临时目录：CI runner 无权写 `/var/run`，用生产
    /// 路径测 flock 语义会先死在 open 上，而不是死在锁语义上。
    #[cfg(unix)]
    #[test]
    fn second_acquire_is_rejected() {
        let path = std::env::temp_dir().join("fm350d-test-singleton.lock");
        let p = path.to_str().expect("临时路径必须是合法 UTF-8");
        let _ = std::fs::remove_file(&path);

        let a = acquire_at(p);
        assert!(a.is_some());
        let b = acquire_at(p);
        assert!(b.is_none(), "同一进程重复抢占应当失败");
        drop(a);
        // 原锁释放后应可再次抢占
        assert!(acquire_at(p).is_some());

        let _ = std::fs::remove_file(&path);
    }
}
