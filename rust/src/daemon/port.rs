//! AT 口看护：配置热切换、端口消失改选、独占事实巡检。
//!
//! ## 三件事，各自独立
//!
//! | 关注点 | 触发条件 | 动作 |
//! |---|---|---|
//! | 配置热切换 | 用户在 LuCI 改了 `at_port` | 释放旧句柄，下一轮按新路径重新独占打开（免重启服务） |
//! | 消失改选 | 配置的 tty 路径**已不存在**（模组重启 / USB 重枚举导致编号漂移）且连续多轮打不开 | 主动探测仍应答 AT 的 Fibocom 口并写回 UCI |
//! | 独占事实巡检 | 每轮 | 观测除本进程外是否还有别的 pid 打开该 tty，仅在集合变化时告警 |
//!
//! ## 为什么「独占」只能靠观测
//!
//! `TIOCEXCL` 对具备 `CAP_SYS_ADMIN` 的进程（root 就是）不生效，内核层面没有
//! 强制排他。因此「是否真被抢占」没有系统调用可问，只能读 `/proc` 反查。
//! 这条巡检不是为了修复，而是为了让故障**可见**：AT 会话被第三方进程插话时，
//! 现象是状态乱跳、拨号随机失败，配置看起来完全正常，极难定位。

use crate::at::{identify_at_port, AtHandle, AtStats};
use crate::config::Config;
use crate::{infof, warnf};

/// 当前 AT 口路径消失后，连续多少轮打不开才尝试自动改选。
///
/// **必须大于 1**：模组重启瞬间 tty 节点会短暂消失，单轮判定会在 USB 重新
/// 枚举完成前就改选，把一个正在恢复的口判死。
const AT_RESELECT_DOWN_ROUNDS: u32 = 3;

/// AT 口看护的跨轮状态。
#[derive(Debug, Default)]
pub struct PortWatch {
    /// 当前 AT 口路径已消失的连续轮数（达到阈值触发改选）。
    down_rounds: u32,
    /// 上一轮观测到的「其他占用者」。只在集合变化时打印，避免每轮刷日志。
    last_intruders: Vec<u64>,
}

impl PortWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// 配置热切换：UCI 里的 `at_port` 与本进程实际持有的路径不一致时释放句柄。
    ///
    /// 只释放、不立即打开：下一轮访问时按新路径自然打开。这样改端口不需要
    /// 重启服务，也不会在本轮巡检中途换口导致后续 AT 命令打到错的设备。
    ///
    /// 比对必须在**同一形态**下进行：配置里写的是稳定标识（`usb-…` /
    /// by-id），而持有路径是解析后的 `/dev/ttyUSB*`。直接拿原文比对恒不
    /// 相等 —— 实机曾因此每 30 s 释放重开一次串口，把拨号会话反复掐断
    /// （2026-11 实验固件：写稳定标识后日志每轮「AT 端口配置变更」）。
    pub fn release_on_change(&self, at: &AtHandle, cfg: &Config) {
        let Some(cur) = at.current_path() else {
            return;
        };
        let want = crate::at::resolve_stable(cfg, &cfg.at_port);
        if cur == want {
            return;
        }
        infof!(format_args!(
            "AT 端口配置变更 {} → {}，释放旧句柄",
            cur, want
        ));
        at.close();
    }

    /// 端口消失改选。仅在**路径确实不存在**时切换 —— 路径还在时宁可等待。
    ///
    /// 同一张 FM350 会导出多个 `2cb7` 口（DIAG / GNSS / AT 等），盲切有选错
    /// 口的风险，而选错口的后果是彻底失联，比多等两轮严重得多。
    ///
    /// 存在性检查同样先解析稳定标识：`usb-…` 形态本身不是文件，
    /// 直接 metadata 恒失败会让本函数每轮误判「端口已消失」。
    pub fn reselect_if_down(&mut self, at: &AtHandle, cfg: &Config) {
        let want = crate::at::resolve_stable(cfg, &cfg.at_port);
        if std::fs::metadata(&want).is_ok() {
            self.down_rounds = 0;
            return;
        }
        self.down_rounds += 1;
        if self.down_rounds < AT_RESELECT_DOWN_ROUNDS {
            return;
        }
        self.down_rounds = 0;
        // 先释放可能残留的句柄，否则探测会被自己的独占状态挡住
        at.close();
        let Some(newp) = identify_at_port(cfg) else {
            warnf!(format_args!(
                "AT 口 {} 已不存在且未探测到可用替代口，保持原配置等待恢复",
                cfg.at_port
            ));
            return;
        };
        infof!(format_args!(
            "AT 口 {} 已不存在，自动改选 {} 并在下一轮独占打开",
            cfg.at_port, newp
        ));
        if let Err(e) = crate::config::save(&serde_json::json!({ "at_port": newp })) {
            warnf!(format_args!("写入新 AT 口失败: {}", e));
        }
    }

    /// 独占事实巡检：占用者集合变化时打印一次。
    pub fn observe_exclusivity(&mut self, at: &AtHandle, cfg: &Config) -> AtStats {
        let st = at.stats(cfg);
        if !st.open || st.other_pids == self.last_intruders {
            return st;
        }
        if st.other_pids.is_empty() {
            if !self.last_intruders.is_empty() {
                infof!(format_args!(
                    "AT 口 {} 恢复独占（此前被 pid {:?} 占用）",
                    st.path, self.last_intruders
                ));
            }
        } else {
            warnf!(format_args!(
                "警告：AT 口 {} 另有进程打开（pid {:?}），独占事实上已被破坏",
                st.path, st.other_pids
            ));
        }
        self.last_intruders = st.other_pids.clone();
        st
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 阈值必须 ≥ 2：单轮判定会在 USB 重枚举完成前误判。
    #[test]
    fn reselect_threshold_tolerates_transient_disappearance() {
        assert!(AT_RESELECT_DOWN_ROUNDS >= 2);
    }

    /// 看护状态初值：未观察到任何占用者、未累计消失轮数。
    #[test]
    fn watch_starts_clean() {
        let w = PortWatch::new();
        assert_eq!(w.down_rounds, 0);
        assert!(w.last_intruders.is_empty());
    }
}
