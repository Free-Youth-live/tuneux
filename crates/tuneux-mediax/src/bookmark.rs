//! # 书签
//!
//! 记住曲目播放位置（路径 + 秒数），供快速跳回。纯数据 + 增删查，可序列化
//! 持久化。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一个书签：某曲目（可选某 CUE 分轨）的某播放位置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bookmark {
    /// 曲目路径（CUE 分轨为整轨文件路径）。
    pub path: PathBuf,
    /// 所属 CUE 分轨起点（毫秒）；`None` = 整轨书签。
    /// 分轨书签的 `position_secs` 是相对该分轨开头的偏移。
    #[serde(default)]
    pub cue_start_ms: Option<u64>,
    /// 播放位置（秒；整轨书签相对文件头，分轨书签相对分轨头）。
    pub position_secs: f64,
    /// 展示标签（一般取曲目标题；缺失时用文件名兜底）。
    pub label: String,
}

/// 书签集合。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BookmarkList {
    /// 书签列表（按添加顺序）。
    pub items: Vec<Bookmark>,
}

impl BookmarkList {
    /// 添加或更新书签：键为 (路径, CUE 分轨起点)。同键已存在则更新位置与
    /// 标签（返回 false），否则新增（返回 true）。`cue_start_ms` 为 `None`
    /// 时是整轨书签；分轨书签与整轨书签可共存于同一路径。
    pub fn add(
        &mut self,
        path: &Path,
        cue_start_ms: Option<u64>,
        position_secs: f64,
        label: &str,
    ) -> bool {
        if let Some(b) = self
            .items
            .iter_mut()
            .find(|b| b.path == path && b.cue_start_ms == cue_start_ms)
        {
            b.position_secs = position_secs;
            b.label = label.to_string();
            false
        } else {
            self.items.push(Bookmark {
                path: path.to_path_buf(),
                cue_start_ms,
                position_secs,
                label: label.to_string(),
            });
            true
        }
    }

    /// 删除某路径的书签，返回是否删除成功。
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.items.len();
        self.items.retain(|b| b.path != path);
        self.items.len() < before
    }

    /// 按路径查书签。
    pub fn get(&self, path: &Path) -> Option<&Bookmark> {
        self.items.iter().find(|b| b.path == path)
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_then_update() {
        let mut list = BookmarkList::default();
        assert!(list.add(Path::new("/a.mp3"), None, 10.0, "A"));
        assert!(!list.add(Path::new("/a.mp3"), None, 20.0, "A2"));
        let b = list.get(Path::new("/a.mp3")).unwrap();
        assert_eq!(b.position_secs, 20.0);
        assert_eq!(b.label, "A2");
        assert_eq!(list.items.len(), 1);
    }

    /// 同一路径下整轨书签与 CUE 分轨书签可共存（键含 cue 起点）。
    #[test]
    fn cue_bookmark_coexists_with_track_bookmark() {
        let mut list = BookmarkList::default();
        assert!(list.add(Path::new("/album.flac"), None, 30.0, "整轨"));
        assert!(list.add(Path::new("/album.flac"), Some(60_000), 15.0, "分轨2"));
        assert!(list.add(Path::new("/album.flac"), Some(180_000), 5.0, "分轨3"));
        // 同 cue 更新而非新增。
        assert!(!list.add(Path::new("/album.flac"), Some(60_000), 20.0, "分轨2b"));
        assert_eq!(list.items.len(), 3);
        let cue2 = list
            .items
            .iter()
            .find(|b| b.cue_start_ms == Some(60_000))
            .unwrap();
        assert_eq!(cue2.position_secs, 20.0);
        assert_eq!(cue2.label, "分轨2b");
    }

    #[test]
    fn remove_bookmark() {
        let mut list = BookmarkList::default();
        list.add(Path::new("/a.mp3"), None, 10.0, "A");
        assert!(list.remove(Path::new("/a.mp3")));
        assert!(list.is_empty());
        assert!(!list.remove(Path::new("/a.mp3")));
    }
}
