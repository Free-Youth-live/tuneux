//! # 加载记录（核查日志）
//!
//! 以**追加**方式记录每个插件的加载与授权事实：插件标识、信任三态、
//! 授予的能力集、是否首见。定位是核查用的记录（减责证据的卫生措施），
//! **不是防篡改机制**——插件沙箱没有文件系统、宿主是唯一写者，
//! 这两条才是真正的防护；导出时由发布方密钥签连续性的做法随实装引入。
//!
//! 当前只记录加载 / 授权事实，不记录网络请求（网络能力默认关闭）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::caps::Capability;
use crate::verify::Tristate;

/// 一条加载 / 授权记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRecord {
    /// 记录时刻（Unix 秒）。
    pub unix_ts: u64,
    /// 插件标识。
    pub plugin_id: String,
    /// 信任三态。
    pub tristate: Tristate,
    /// 实际授予的能力集。
    pub granted: Vec<Capability>,
    /// 是否为本插件的首条记录（首次加载）。
    pub first_seen: bool,
}

impl LoadRecord {
    /// 以当前时间构造一条记录（时间取不到时记 0，保证仍可写入）。
    pub fn now(
        plugin_id: String,
        tristate: Tristate,
        granted: Vec<Capability>,
        first_seen: bool,
    ) -> Self {
        Self {
            unix_ts: unix_now(),
            plugin_id,
            tristate,
            granted,
            first_seen,
        }
    }

    /// 格式化为一行 `key=value` 记录（不含换行符）。
    ///
    /// 字段名与取值保持稳定（历史记录可比）：
    /// `ts=… plugin=… trust=… caps=… first_seen=…`。
    pub fn to_line(&self) -> String {
        let caps = self
            .granted
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "ts={} plugin={} trust={} caps={} first_seen={}",
            self.unix_ts,
            self.plugin_id,
            self.tristate.name(),
            caps,
            self.first_seen
        )
    }
}

/// 当前 Unix 秒（取不到时返回 0）。
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 追加式记录器：唯一写者约定（宿主），只追加不覆盖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// 指向记录文件路径构造记录器（文件可以尚不存在；父目录须已存在）。
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// 追加一条记录（文件不存在则创建）。失败原样返回输入输出错误。
    pub fn append(&self, record: &LoadRecord) -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", record.to_line())
    }

    /// 记录文件路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_line_format_is_stable() {
        let rec = LoadRecord {
            unix_ts: 1700000000,
            plugin_id: "eq-ten-band".to_string(),
            tristate: Tristate::SignedUnknown,
            granted: vec![Capability::AudioPlay, Capability::UiWrite],
            first_seen: true,
        };
        assert_eq!(
            rec.to_line(),
            "ts=1700000000 plugin=eq-ten-band trust=signed_unknown caps=audio_play,ui_write first_seen=true"
        );
    }

    #[test]
    fn append_creates_and_appends_lines() {
        let dir = std::env::temp_dir().join("tuneux_pinx_journal_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let journal = Journal::new(dir.join("load.log"));

        let first = LoadRecord::now(
            "demo".to_string(),
            Tristate::Unsigned,
            vec![Capability::UiWrite],
            true,
        );
        let second = LoadRecord::now(
            "demo".to_string(),
            Tristate::Unsigned,
            vec![Capability::UiWrite],
            false,
        );
        journal.append(&first).unwrap();
        journal.append(&second).unwrap();

        let content = std::fs::read_to_string(journal.path()).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("ts="));
        assert!(lines[0].contains("plugin=demo trust=unsigned caps=ui_write first_seen=true"));
        assert!(lines[1].contains("first_seen=false"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unix_now_is_non_decreasing() {
        let a = unix_now();
        let b = unix_now();
        assert!(b >= a);
    }
}
