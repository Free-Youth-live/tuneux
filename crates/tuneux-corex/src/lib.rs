//! # tuneux-corex：tuneux 系列共享音频内核
//!
//! 心脏：纯音频引擎，零 UI。所有产品（tuneux / tuneux-fx / tuneux-max）共用。
//!
//! 契约化纪律：
//! - `#![forbid(unsafe)]`（workspace.lints 统一禁止）
//! - 零 UI 依赖（ratatui/crossterm/egui 严禁引入）
//! - 零网络、零插件
//! - 公共 API 面冻结（pub use 白名单），内部实现可重构不破坏外部
//!
//! # 公共 API 稳定性承诺
//!
//! 1.0 前可变更但需 CHANGELOG 标注；`engine_thread`/`resample`/`DecoderCmd`
//! 为内部实现（`pub(crate)`），不属公共契约。

#![deny(missing_docs)]

mod audio;
pub mod cue;
pub mod rg;

// 显式白名单（收官）：杜绝 glob 泄漏，公开面逐项冻结
pub use audio::compressor::{CompressorParams, COMP_SLOTS};
pub use audio::decoder::{
    codec_name_or_ext, open_backend, AudioParams, DecodeError, DecoderBackend, KNOWN_AUDIO_EXTS,
};
pub use audio::engine::{AudioCmd, Engine};
pub use audio::equalizer::{
    EqParams, EQ_BANDS, EQ_FREQS, EQ_GAIN_MAX_DB, EQ_GAIN_MIN_DB, EQ_SLOTS,
};
pub use audio::playback_medium::{PlaybackMedium, UnknownMedium};
pub use audio::probe::{probe_metadata, ProbeTags};
pub use audio::spectrum;
