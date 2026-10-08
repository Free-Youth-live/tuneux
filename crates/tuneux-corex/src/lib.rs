//! # tuneux-corex：tuneux 系列共享音频内核
//!
//! 心脏：纯音频引擎，零 UI。所有产品（tuneux / tuneux-fx / tuneux-max）共用。
//!
//! 契约化纪律：
//! - 零 unsafe（workspace.lints 统一禁止）
//! - 零 UI 依赖（ratatui/crossterm/egui 严禁引入）
//! - 零网络、零插件
//! - 公共 API 面冻结（pub use 白名单），内部实现可重构不破坏外部
//!
//! # 内部分层（2026-10-02 起）
//!
//! audio/codec/：纯解码（字节解析 + 文件/进程 IO），零线程、零 cpal。
//! audio/engine/：实时线程 + DSP + 分析（cpal 回调、频谱、EQ、介质）。
//! 方向：engine 到 codec（经窄腰五符号），codec 不依赖 engine。

// 测试代码允许 unwrap：断言失败即测试失败，语义与生产路径不同
//（生产路径零 unwrap 由 workspace lints 强制）。
#![cfg_attr(test, allow(clippy::unwrap_used))]
#![deny(missing_docs)]

mod audio;

// 显式白名单（收官）：杜绝 glob 泄漏，公开面逐项冻结。
// 死导出已收 pub(crate)（A2）：DecoderBackend / DecodeError /
// codec_name_or_ext / UnknownMedium 零外部消费方；open_backend 仅测试用，
// 走 test-helpers feature。
pub use audio::codec::decoder::open_backend;
pub use audio::{
    probe_metadata, spectrum, AudioCmd, AudioParams, CompressorParams, Engine, EqParams,
    FilterParams, PlaybackMedium, PlaybackStatus, PreloadTarget, ProbeTags, COMP_SLOTS, EQ_BANDS,
    EQ_FREQS, EQ_GAIN_MAX_DB, EQ_GAIN_MIN_DB, EQ_SLOTS, FILTER_SLOTS, KNOWN_AUDIO_EXTS,
};

/// 测试专用入口：解码后端工厂 + 后端 trait + 错误类型。
/// 交付构建不编译（feature 门控），产品集成测试经 test-helpers 启用。
#[cfg(feature = "test-helpers")]
pub mod test_helpers {
    pub use crate::audio::codec::decoder::{
        open_backend, AudioParams, DecodeError, DecoderBackend,
    };
}
