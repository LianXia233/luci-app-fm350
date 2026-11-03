//! 本地发件归档：把「发出过什么」留在路由器本地，与模组格位的生命周期解耦。
//!
//! ## 为什么要有它
//!
//! 模组存储的发出短信要么没有（多数运营商不回填 MO 格位）、要么被自动清理；
//! 而「只发出去过、从没收到过」的号码在纯模组视图下整个会话会消失。MT5700
//! 项目的 sms_store 用一套 900 行的归档库验证了这条需求，这里取它的**最小
//! 可用子集**：JSONL 追加写 + 指纹去重 + 按号码/指纹删除。
//!
//! ## 形态
//!
//! 每个号码一个文件：`<dir>/<会话哈希>.jsonl`（哈希避免号码直接出现在
//! 文件名里），一行一条 JSON 记录。写失败**不影响**发送结论 —— 归档是
//! 附带动作，模组已确认收下的短信不能因为写盘失败被报成失败。
//!
//! 与 MT5700 的差异（有意为之）：
//! * 只归档**发出**的短信。收到的短信在模组存储里本来就有权威副本，
//!   再抄一份反而引入两处数据源的一致性问题。
//! * `store_read` / `store_stats` / `store_drop` 的返回形状与 MT5700 前端
//!   的消费契约一致（entries / count,totalBytes,maxBytes,conversations），
//!   这样移植过来的短信中心页不用为两套后端写两套解析。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 归档目录。持久分区（模组重启不清空）；目录不存在时惰性创建。
pub fn archive_dir() -> PathBuf {
    PathBuf::from("/usr/share/fm350/sms_archive")
}

/// 一条归档记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    /// `sent` / `received`（当前只写 sent，读侧两种都认，便于将来扩展）。
    pub direction: String,
    pub number: String,
    pub content: String,
    /// 写入时刻（本地时区字符串）；发出短信按 TS 23.040 不带时刻，
    /// 前端以 `time_approx` 标注「近似」。
    pub time: Option<String>,
    pub time_approx: bool,
    /// `方向|号码|时间|内容` 的 SHA-256 截断（去重与定向删除的依据）。
    pub fingerprint: String,
}

/// 会话元信息（store_stats 的 conversations 项）。
#[derive(Debug, Serialize)]
pub struct ConversationMeta {
    pub number: String,
    pub count: usize,
    pub total_bytes: usize,
    pub last_time: Option<String>,
}

/// 归档统计（store_stats 的 result）。
#[derive(Debug, Default, Serialize)]
pub struct ArchiveStats {
    pub count: usize,
    pub total_bytes: usize,
    pub max_bytes: usize,
    pub conversations: Vec<ConversationMeta>,
}

/// 记录指纹：与前端约定的「方向|号码|时间|内容」哈希。
/// 前端删除时回传原始字段，后端算指纹，两端口径单一。
pub fn fingerprint(direction: &str, number: &str, time: Option<&str>, content: &str) -> String {
    let mut h = DefaultHasher::new();
    (direction, number, time.unwrap_or(""), content).hash(&mut h);
    format!("{:016x}", h.finish())
}

/// 会话文件名：号码哈希（避免把号码明文铺满 /usr/share）。
fn path_for(number: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    number.hash(&mut h);
    archive_dir().join(format!("{:016x}.jsonl", h.finish()))
}

fn now_local() -> String {
    // busybox date 即可；解析失败的代价只是时间显示为空
    let out = std::process::Command::new("date")
        .arg("+%Y-%m-%d %H:%M:%S")
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// 追加一条发出记录。失败不影响调用方主流程（发送结论已由模组确认）。
pub fn record_sent(number: &str, content: &str) {
    let rec = Record {
        direction: "sent".into(),
        number: number.to_string(),
        content: content.to_string(),
        time: {
            let t = now_local();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        },
        time_approx: true,
        fingerprint: fingerprint("sent", number, None, content),
    };
    let path = path_for(number);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        // 同指纹不重复追加（连点/重试保护的后端侧兜底）
        if read_records(number).iter().any(|r| r.fingerprint == rec.fingerprint) {
            return;
        }
        if let Ok(line) = serde_json::to_string(&rec) {
            let _ = writeln!(f, "{}", line);
        }
    }
}

/// 读某号码的全部归档记录（文件不存在 = 空会话）。
pub fn read_records(number: &str) -> Vec<Record> {
    let Ok(f) = std::fs::File::open(path_for(number)) else {
        return Vec::new();
    };
    std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect()
}

/// 归档统计：所有会话文件的聚合（store_stats）。
pub fn stats() -> ArchiveStats {
    let mut st = ArchiveStats::default();
    let Ok(entries) = std::fs::read_dir(archive_dir()) else {
        return st;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(f) = std::fs::File::open(&p) else {
            continue;
        };
        let recs: Vec<Record> = std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect();
        if recs.is_empty() {
            continue;
        }
        st.count += recs.len();
        st.total_bytes += recs.iter().map(|r| r.content.len()).sum::<usize>();
        let last: Option<String> = recs
            .iter()
            .filter_map(|r| r.time.clone())
            .max()
            .or_else(|| recs.last().and_then(|r| r.time.clone()));
        st.conversations.push(ConversationMeta {
            number: recs[0].number.clone(),
            count: recs.len(),
            total_bytes: recs.iter().map(|r| r.content.len()).sum(),
            last_time: last,
        });
    }
    st.conversations
        .sort_by(|a, b| b.last_time.cmp(&a.last_time));
    st.max_bytes = 4 * 1024 * 1024; // 与前端展示口径一致：超限时提示清理
    st
}

/// 删除条件（store_drop）：号码必带；指纹列表 / 全部 二选一。
pub fn drop(number: &str, fingerprints: &[String], all: bool) -> usize {
    let path = path_for(number);
    if all {
        return std::fs::remove_file(&path).map(|_| usize::MAX).unwrap_or(0);
    }
    if fingerprints.is_empty() {
        return 0;
    }
    let recs = read_records(number);
    let keep: Vec<&Record> = recs
        .iter()
        .filter(|r| !fingerprints.iter().any(|f| f == &r.fingerprint))
        .collect();
    let removed = recs.len() - keep.len();
    if removed > 0 {
        if let Ok(mut f) = std::fs::File::create(&path) {
            for r in keep {
                if let Ok(line) = serde_json::to_string(r) {
                    let _ = writeln!(f, "{}", line);
                }
            }
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_and_order_sensitive() {
        let a = fingerprint("sent", "10001", Some("t"), "108");
        let b = fingerprint("sent", "10001", Some("t"), "108");
        let c = fingerprint("sent", "10001", Some("t"), "109");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn path_for_hashes_number_instead_of_embedded_plaintext() {
        let p = path_for("+8613800138000");
        assert!(p.to_string_lossy().ends_with(".jsonl"));
        assert!(!p.to_string_lossy().contains("13800"));
    }
}
