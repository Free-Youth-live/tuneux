//! # tuneux-mediax：tuneux 系列共享媒体数据层
//!
//! 介于音频内核（tuneux-corex）与各产品 UI（tuneux / tuneux-fx / tuneux-max）
//! 之间的"非音频、非 UI"领域逻辑：m3u 播放列表、书签、歌词数据、元数据等。
//! 供所有产品共享。

#![deny(missing_docs)]

pub mod bookmark;
pub mod lyrics;
pub mod m3u;
pub mod metadata;
