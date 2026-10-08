//! # 音频引擎模块集合
//!
//! 内部分两层：
//!
//! - codec/：纯解码（字节解析 + 文件/进程 IO），零线程、零 cpal、零 DSP；
//! - engine/：实时线程 + DSP + 分析（cpal 回调、频谱、EQ、压缩、介质）。
//!
//! 分层纪律：codec 不依赖 engine（方向断言由 guard 与单测守护）；
//! engine 仅经 codec 的窄腰（AudioParams / DecodeError /
//! DecoderBackend / open_backend / probe_sample_rate）使用解码能力。

pub(crate) mod codec;
pub(crate) mod engine;

// —— lib.rs 公共白名单的唯一中继（audio 是唯一可见的中间层）——
pub use codec::decoder::{AudioParams, KNOWN_AUDIO_EXTS};
pub use codec::probe::{probe_metadata, ProbeTags};
pub use engine::compressor::{CompressorParams, COMP_SLOTS};
pub use engine::equalizer::{
    EqParams, EQ_BANDS, EQ_FREQS, EQ_GAIN_MAX_DB, EQ_GAIN_MIN_DB, EQ_SLOTS,
};
pub use engine::filter::{FilterParams, FILTER_SLOTS};
pub use engine::playback_medium::PlaybackMedium;
pub use engine::spectrum;
pub use engine::{AudioCmd, Engine, PlaybackStatus, PreloadTarget};
