//! # 播放列表条目与导航结果（跨产品共享类型）
//!
//! 双端（tuneux / tuneux-fx）共用的播放列表领域类型：`CueRef`（CUE 分轨
//! 引用）、`PlaylistItem`（条目）、`NavOutcome`（导航结果），以及 CUE 分轨
//! 的 seek 钳制函数。
//!
//! **持久化契约红线**：`CueRef` / `PlaylistItem` 的 serde（toml）表示冻结——
//! 它们写入用户的播放列表 / 书签文件，字段改名、改型、改默认值都破坏
//! 既有用户数据。改动前必须更新本模块的契约快照测试并经人工评审。
//!
//! CUE 解析本体在 corex（`cue.rs`），本模块只承载分轨引用的数据形态。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// CUE 分轨引用：整轨文件中的一曲。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueRef {
    /// CUE 曲目号（1-based，来自 `TRACK n AUDIO`）。
    pub index: u32,
    /// 曲目标题（来自 `.cue` 的 TITLE，缺失时为 "Track {n}"）。
    pub title: String,
    /// 表演者（来自 `.cue` 的 PERFORMER，可选；渲染时优先于整轨元数据）。
    #[serde(default)]
    pub performer: Option<String>,
    /// 起始时间（毫秒，来自 `INDEX 01`，75 帧/秒换算）。
    pub start_ms: u64,
    /// 结束时间（毫秒）：下一曲 INDEX 起点；末曲为整轨总时长；未知为 None（播到文件尾）。
    #[serde(default)]
    pub end_ms: Option<u64>,
}

/// 播放列表中的一个条目。
///
/// 排序键（`album` + `track_number`）在加入时确定并随条目存储，
/// 避免每次排序时重新读取 metadata。
/// 派生 Serialize/Deserialize 用于"保存播放列表，下次启动恢复"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// 文件完整路径
    pub path: PathBuf,
    /// 专辑名（来自 ID3/Vorbis 标签），无则归入"无专辑"组
    pub album: Option<String>,
    /// 曲序（专辑内 1, 2, 3...），无则排到该专辑末尾
    pub track_number: Option<u32>,
    /// CUE 分轨引用：Some 表示这是整轨文件中的一曲（播放从 `start_ms` 起）。
    /// `#[serde(default)]` 保证旧版保存的播放列表可正常反序列化。
    #[serde(default)]
    pub cue: Option<CueRef>,
}

/// 导航操作的结果。
///
/// 调用 `next` / `prev` / `jump_to` 后由 `Playlist` 返回，
/// App 据此决定是否给音频引擎下发新曲目。
/// 运行期枚举（不持久化），不挂 serde。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavOutcome {
    /// 切到指定 item
    Switch(usize),
    /// 单曲循环：保持当前（调用方应重新发 Play 让流再起）
    Repeat,
    /// 没有下一曲/上一曲
    End,
}

/// 把 seek 目标钳制在 CUE 分轨区间 [start, end] 内（end 未知只钳下限）；
/// 普通曲目（cue 为 None）原样返回。
///
/// 独立为自由函数便于单元测试边界（App 层携带引擎状态无法单测）。
pub fn clamp_seek_to_cue(pos: f64, cue: Option<&CueRef>) -> f64 {
    let Some(cue) = cue else {
        return pos;
    };
    let start = cue.start_ms as f64 / 1000.0;
    let p = pos.max(start);
    match cue.end_ms {
        Some(e) => p.min(e as f64 / 1000.0),
        None => p,
    }
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_item() -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from("/music/album.flac"),
            album: Some("专辑".into()),
            track_number: Some(2),
            cue: Some(CueRef {
                index: 2,
                title: "Track 2".into(),
                performer: Some("某人".into()),
                start_ms: 10_000,
                end_ms: Some(20_000),
            }),
        }
    }

    /// 契约快照：PlaylistItem（含 CueRef）的 toml 表示冻结。
    /// 它们写入用户的播放列表 / 书签文件——此测试红 = 持久化格式变了，
    /// 必须人工评审并同步迁移策略，不得顺手改快照放行。
    #[test]
    fn serde_contract_frozen() {
        let text = toml::to_string(&sample_item()).expect("序列化");
        let expected = r#"path = "/music/album.flac"
album = "专辑"
track_number = 2

[cue]
index = 2
title = "Track 2"
performer = "某人"
start_ms = 10000
end_ms = 20000
"#;
        assert_eq!(text, expected, "PlaylistItem 的 toml 表示变了（契约红线）");
        // 往返：序列化结果可无损读回
        let back: PlaylistItem = toml::from_str(&text).expect("反序列化");
        assert_eq!(back, sample_item());
    }

    /// 旧格式兼容：无 cue 字段的旧播放列表条目可正常解析（serde default）。
    #[test]
    fn legacy_item_without_cue_parses() {
        let old = r#"path = "/music/song.mp3"
album = "老专辑"
track_number = 1
"#;
        let item: PlaylistItem = toml::from_str(old).expect("旧格式应可解析");
        assert_eq!(item.cue, None);
        assert_eq!(item.album.as_deref(), Some("老专辑"));
    }

    /// seek 钳制边界（自 fx 迁入；双端共用）。
    #[test]
    fn clamp_seek_to_cue_bounds() {
        let cue = CueRef {
            index: 2,
            title: "Track 2".into(),
            performer: None,
            start_ms: 10_000,
            end_ms: Some(20_000),
        };
        // 普通曲目：原样返回（含越界值）
        assert_eq!(clamp_seek_to_cue(5.0, None), 5.0);
        // 区间内：不动
        assert_eq!(clamp_seek_to_cue(15.0, Some(&cue)), 15.0);
        // 低于起点：钳到起点
        assert_eq!(clamp_seek_to_cue(3.0, Some(&cue)), 10.0);
        // 超过终点：钳到终点
        assert_eq!(clamp_seek_to_cue(99.0, Some(&cue)), 20.0);
        // 末曲（end 未知）：只钳下限
        let last = CueRef {
            end_ms: None,
            ..cue
        };
        assert_eq!(clamp_seek_to_cue(99.0, Some(&last)), 99.0);
        assert_eq!(clamp_seek_to_cue(0.0, Some(&last)), 10.0);
    }
}
