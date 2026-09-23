//! # tuneux-mediax：tuneux 系列共享媒体数据层
//!
//! 介于音频内核（tuneux-corex）与各产品 UI（tuneux / tuneux-fx / tuneux-max）
//! 之间的音频领域数据层（依赖 corex 探测产出）：m3u 播放列表、书签、歌词
//! 数据、元数据等。供所有产品共享。

// 测试代码允许 unwrap：断言失败即测试失败，语义与生产路径不同
//（生产路径零 unwrap 由 workspace lints 强制）。
#![cfg_attr(test, allow(clippy::unwrap_used))]
#![deny(missing_docs)]

pub mod bar_style;
pub mod bookmark;
pub mod lyrics;
pub mod m3u;
pub mod metadata;
pub mod playlist_item;
pub mod repeat;
pub mod time;

pub use bar_style::BarStyle;
pub use playlist_item::{clamp_seek_to_cue, CueRef, NavOutcome, PlaylistItem};
pub use repeat::RepeatMode;
