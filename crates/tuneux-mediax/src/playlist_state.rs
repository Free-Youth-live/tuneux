//! # 播放列表持久化状态（多列表格式，三产品共用的规范形态）
//!
//! 从 tuneux-max 下沉（2026-10-02）：max 的多列表标签页格式作为规范格式，
//! tuneux / fx 后续迁移时包装为单列表。旧版单列表字段保留作迁移入口
//! （lists 为空且旧字段有值时，启动迁移为单列表）。
//!
//! 文件路径与加载/保存是产品行为（文件名、目录策略各产品不同），
//! 留在各产品的 config.rs；本模块只定义数据结构。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::PlaylistItem;

/// 单个播放列表的持久化形态（多列表：标签页口径）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SavedList {
    /// 列表名（用户可重命名；数据快照，不随语言回溯翻译）。
    pub name: String,
    /// 条目（插入序 / 用户排序后序）。
    pub items: Vec<PlaylistItem>,
    /// 上次退出时的当前曲路径（启动自动选中，不自动播放）。
    pub current: Option<PathBuf>,
    /// 随机开关。
    pub shuffle: bool,
}

/// 播放状态（多播放列表 + 每曲进度）。
///
/// 与配置分离：体积较大且频繁变化，不与偏好混存。
/// 旧版单列表字段（items/current/shuffle）保留作迁移入口：
/// lists 为空且旧字段有值时，启动迁移为单列表。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PlaylistState {
    /// 多播放列表（标签页序）。
    pub lists: Vec<SavedList>,
    /// 活跃列表索引（越界启动时钳制）。
    pub active: usize,
    /// 每曲播放进度（路径 → 秒，跨列表共享）；≤0.5s 清旧记录。
    pub positions: BTreeMap<PathBuf, f64>,
    /// ReplayGain 实测增益缓存（路径 → dB）：播完一首归档测量值，
    /// 下次播放直接应用，免重测（与 fx 的 replay_gain 状态同口径）。
    #[serde(default)]
    pub replay_gain: BTreeMap<PathBuf, f64>,
    /// 旧版：上次退出时正在播放的路径（迁移入口，保存时清空）。
    pub current: Option<PathBuf>,
    /// 旧版：单列表条目（迁移入口，保存时清空）。
    pub items: Vec<PlaylistItem>,
    /// 旧版：随机开关（迁移入口，保存时清空）。
    pub shuffle: bool,
}

impl PlaylistState {
    /// 记录进度：>0.5s 记录；≤0.5s 清除该曲旧记录（与双 TUI 同口径）。
    pub fn save_position(&mut self, path: &std::path::Path, secs: f64) {
        if secs > 0.5 {
            self.positions.insert(path.to_path_buf(), secs);
        } else {
            self.positions.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 序列化往返：字段不丢失、不重命名（serde 契约冻结）。
    #[test]
    fn toml_roundtrip() {
        let state = PlaylistState {
            lists: vec![SavedList {
                name: "默认".to_string(),
                items: vec![PlaylistItem {
                    path: PathBuf::from("/music/a.flac"),
                    album: None,
                    track_number: None,
                    cue: None,
                }],
                current: Some(PathBuf::from("/music/a.flac")),
                shuffle: true,
            }],
            active: 0,
            positions: BTreeMap::from([(PathBuf::from("/music/a.flac"), 123.45)]),
            replay_gain: BTreeMap::from([(PathBuf::from("/music/a.flac"), -6.2)]),
            current: None,
            items: Vec::new(),
            shuffle: false,
        };
        let text = toml::to_string_pretty(&state).expect("序列化");
        let back: PlaylistState = toml::from_str(&text).expect("反序列化");
        assert_eq!(back.lists.len(), 1);
        assert_eq!(back.lists[0].name, "默认");
        assert_eq!(back.active, 0);
        assert!(back.positions.contains_key(&PathBuf::from("/music/a.flac")));
        assert!((back.replay_gain[&PathBuf::from("/music/a.flac")] + 6.2).abs() < 1e-9);
    }

    /// 旧版单列表字段可独立加载（迁移入口：lists 为空 + items 有值）。
    #[test]
    fn legacy_single_list_fields_loadable() {
        let text = r#"
current = "/music/song.flac"
shuffle = true
[[items]]
path = "/music/song.flac"
"#;
        let state: PlaylistState = toml::from_str(text).expect("旧格式应可加载");
        assert_eq!(state.current, Some(PathBuf::from("/music/song.flac")));
        assert_eq!(state.items.len(), 1);
        assert!(state.shuffle);
        assert!(state.lists.is_empty(), "lists 应为空（待迁移）");
    }

    /// save_position：>0.5s 记录，≤0.5s 清除。
    #[test]
    fn save_position_threshold() {
        let mut st = PlaylistState::default();
        let p = PathBuf::from("/a.flac");
        st.save_position(&p, 1.0);
        assert_eq!(st.positions.get(&p), Some(&1.0));
        st.save_position(&p, 0.3);
        assert!(!st.positions.contains_key(&p), "≤0.5s 应清除");
    }
}
