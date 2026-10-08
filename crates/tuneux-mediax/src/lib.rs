//! # tuneux-mediax：tuneux 系列共享媒体数据层
//!
//! 介于音频内核（tuneux-corex）与各产品 UI（tuneux / tuneux-fx / tuneux-max）
//! 之间的音频领域数据层（依赖 corex 探测产出）：m3u 播放列表、书签、歌词
//! 数据、元数据等。供所有产品共享。

// 测试代码允许 unwrap：断言失败即测试失败，语义与生产路径不同
//（生产路径零 unwrap 由 workspace lints 强制）。
#![cfg_attr(test, allow(clippy::unwrap_used))]
#![deny(missing_docs)]

pub mod artwork;
pub mod bar_style;
pub mod bookmark;
pub mod cue;
pub mod lyrics;
pub mod m3u;
pub mod metadata;
pub mod playback_decision;
pub mod playlist;
pub mod playlist_item;
pub mod playlist_state;
pub mod repeat;
pub mod scan_pool;
pub mod time;

pub use artwork::{decode as decode_cover, decode_thumb as decode_cover_thumb, CoverImage};
pub use bar_style::BarStyle;
pub use cue::{
    build_items_for_paths, cue_items_from, cue_items_from_cue_file, decode_cue_bytes, parse_cue,
    parse_cue_sheet, CueExpansion, CueParseError, CueSheet, CueTrack,
};
pub use metadata::{BitsDisplay, ChannelsDisplay, SampleRateDisplay};
pub use playback_decision::{
    resume_secs, single_repeat_decision, toggle_decision, PlaybackDecision,
};
pub use playlist::{PlaylistCore, PlaylistView, SplitMix64};
pub use playlist_item::{clamp_seek_to_cue, CueRef, NavOutcome, PlaylistItem};
pub use playlist_state::{PlaylistState, SavedList};
pub use repeat::RepeatMode;
