//! # 音频引擎模块集合
//!
//! 整合 symphonia 解码、rubato 重采样、cpal 输出，提供统一的播放控制。
//!
//! 子模块：
//! - [`decoder`]：可插拔解码后端（DecoderBackend trait + open_backend 工厂，
//!   当前 symphonia + Opus + WavPack 三个后端；另含采样率探测）；
//! - [`opus`]：Opus 解码后端（Ogg Opus 解封装 + opus-decoder 包解码）；
//! - [`ffmpeg`]：FFmpeg 进程外解码后端（子进程 IPC 隔离 LGPL）；
//! - [`wavpack`]：WavPack 解码后端（基于 wavicle，`.wv` 无损流）；
//! - [`ogg_opus`]：Ogg 页解析与逻辑包重组（opus 后端的解封装底层）；
//! - [`resample`]：rubato 重采样（采样率不匹配时使用）；
//! - [`engine`]：音频引擎句柄（主线程接口，下发命令、读取状态）；
//! - [`engine_thread`]：音频线程与解码线程的具体实现（含原生采样率直通）；
//! - [`spectrum`]：FFT 频谱分析（256-pt R2C，64 段对数分布）。

// 契约化纪律：engine_thread/resample 是内部实现，
// 不进入公共 API 面；ringbuf/线程细节不外泄，保拆仓期权。
pub mod decoder;
pub mod engine;
pub(crate) mod engine_thread;
pub(crate) mod ffmpeg;
pub(crate) mod ogg_opus;
pub(crate) mod opus;
pub(crate) mod resample;
pub mod spectrum;
pub(crate) mod wavpack;

// 对外暴露常用类型。
#[allow(unused_imports)]
pub use decoder::{open_backend, AudioParams, DecodeError, DecoderBackend};
#[allow(unused_imports)]
pub use engine::{AudioCmd, Engine};
