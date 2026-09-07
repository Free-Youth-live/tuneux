//! # 音频解码模块
//!
//! 基于 symphonia 0.6 实现：打开音频文件、选择音轨、建立解码器、
//! 循环解码数据包为 PCM 样本。本模块只负责"把文件变成样本"，
//! 不关心样本如何被消费（消费由音频引擎的播放线程负责）。
//!
//! ## 可插拔后端
//!
//! 解码职责抽象为 [DecoderBackend] trait，调用方（解码线程）只面向
//! `Box<dyn DecoderBackend>`，不感知具体后端。当前有 symphonia / Opus /
//! WavPack / FFmpeg 四个后端。FFmpeg 后端以子进程 IPC 方式
//! 隔离 LGPL/GPL 许可传染，仅在桌面平台可选启用。所有后端经
//! [open_backend] 工厂分发接入，无需改动调用方。
//!
//! ## 关键点
//!
//! - symphonia 0.6 解码出的样本用 copy_to_slice_interleaved 直接转成
//!   交错（interleaved，L,R,L,R,...）的目标格式样本，无需手动平面→交错转换；
//! - 解码产出统一归一化为 **f32**（-1.0~1.0），后续重采样、音量、cpal 都基于 f32；
//! - 元数据（技术参数）在打开时一并提取，供 TUI 显示。

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use super::ffmpeg::{is_ffmpeg_path, FfmpegBackend};
use super::opus::{is_opus_path, OpusBackend};
use super::wavpack::{is_wavpack_path, WavPackBackend};

/// 内核认识的音频文件扩展名（小写，不含点）：原生进程内解码 + ffmpeg 桥接长尾。
///
/// 供产品侧文件浏览器决定"显示哪些文件"——凡是认识的格式都列出；
/// 能否真正播放由 [`open_backend`] 在解码时判定（不支持 / 缺 ffmpeg 时返回错误，
/// 产品据此提示并跳过）。清单是"认识"而非"保证可播"。
pub const KNOWN_AUDIO_EXTS: &[&str] = &[
    // 原生进程内：symphonia / Opus / WavPack
    "mp3", "flac", "wav", "ogg", "m4a", "aac", "alac", "opus", "wv",
    // ffmpeg 桥接长尾：装了 ffmpeg 才能播
    "ape", "wma", "flv", "tak", "ofr", "mpc", "shn", "ac3", "dts", "tta", "dsf", "dff",
];

/// 把解码器给出的编码名（字符串）映射为用户可读的编码名称。
///
/// 优先匹配已知格式；未知名称返回空字符串，由调用方结合文件扩展名兜底。
/// pub(crate)：内部工具（外部经白名单 codec_name_or_ext 使用）。
pub(crate) fn codec_display_name(codec_name: &str) -> String {
    match codec_name {
        "flac" => return "FLAC".into(),
        "mp3" => return "MP3".into(),
        "aac" => return "AAC".into(),
        "alac" => return "ALAC".into(),
        "vorbis" => return "Vorbis".into(),
        "pcm" => return "PCM".into(),
        _ => {}
    }
    // 十六进制未知 codec：返回空，让调用方用扩展名兜底
    if codec_name.starts_with("0x") {
        return String::new();
    }
    codec_name.to_string()
}

/// 取编码名称，未知时用文件扩展名兜底（如 .flac → FLAC）。
pub fn codec_name_or_ext(codec_name: &str, path: &std::path::Path) -> String {
    let name = codec_display_name(codec_name);
    if !name.is_empty() {
        return name;
    }
    // symphonia 未识别：用文件扩展名兜底
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase())
        .unwrap_or_else(|| "?".into())
}

/// 音频文件的技术参数。
///
/// 在播放开始前确定，用于配置 cpal 输出流、决定是否重采样、TUI 元数据显示。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AudioParams {
    /// 采样率（Hz），如 44100。部分格式可能为 None。
    pub sample_rate: Option<u32>,
    /// 通道数（2 = 立体声）。用 u16 与 cpal 的 ChannelCount 对齐。
    pub channels: Option<u16>,
    /// 每样本位数（位深），如 16、24。有损格式通常为 None。
    pub bits_per_sample: Option<u32>,
    /// 编码格式名称（如 "MP3"、"FLAC"），用户可读。
    pub codec_name: String,
    /// 音轨 ID，用于 seek 时指定目标轨、过滤数据包。
    pub track_id: u32,
    /// 总时长（秒）。从容器头读取，部分格式（如无 Xing 头的 MP3）可能为 None。
    /// 引擎打开文件后会把这里传给 SharedState，draw_metadata 据此显示时长。
    pub duration: Option<f64>,
}

impl AudioParams {
    /// 构造完整技术参数。
    ///
    /// `#[non_exhaustive]` 后外部无法字面量构造，此构造器为契约化唯一入口
    /// （字段冻结可演进，未来加字段不破坏外部构造）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sample_rate: Option<u32>,
        channels: Option<u16>,
        bits_per_sample: Option<u32>,
        codec_name: String,
        track_id: u32,
        duration: Option<f64>,
    ) -> Self {
        Self {
            sample_rate,
            channels,
            bits_per_sample,
            codec_name,
            track_id,
            duration,
        }
    }
}

/// 解码后端抽象 trait：把"文件 → 交错 f32 样本"这一职责抽成可插拔接口。
///
/// 未来 Opus/WavPack/ffmpeg 等后端只需实现本 trait 并经 [open_backend]
/// 分发接入，调用方（解码线程）只面向 `Box<dyn DecoderBackend>`，
/// 不感知具体后端。Send 超 trait 保证后端可跨线程移动（解码线程独占）。
pub trait DecoderBackend: Send {
    /// 取技术参数只读引用（采样率/通道数/位深/编码名/音轨/时长）。
    fn params(&self) -> &AudioParams;

    /// 读取并解码下一批数据，产出交错（interleaved，L,R,L,R,...）f32 样本。
    ///
    /// 返回 Ok(Some(samples)) 成功；Ok(None) EOF；Err 解码错误。
    ///
    /// 用 copy_to_slice_interleaved 把解码缓冲（可能 planar）一次性转成
    /// 交错 f32，目标格式由 `Vec<f32>` 的元素类型推断。
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError>;

    /// Seek 到指定秒数。返回请求的秒数（实际落点由底层格式决定，
    /// 可能略早于请求值；底层实际时间戳未透传，调用方按请求值使用即可，
    /// 进度显示偏差在可接受范围）。
    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError>;
}

/// 解码错误：后端无关的统一错误类型。
///
/// 不直接暴露 symphonia 的错误类型，避免未来换 ffmpeg 后端时调用方的
/// 错误处理跟着改。字符串携带人可读的原始错误描述。
#[derive(Debug)]
pub enum DecodeError {
    /// 底层 IO 失败（打开/读取文件等）。
    Io(String),
    /// 容器或编码特性不受支持（如无可用音频轨、缺编解码参数）。
    Unsupported(String),
    /// 解码/解封装失败（数据损坏或畸形流）。
    Decode(String),
    /// Seek 失败（不可 seek / 目标越界 / 音轨无效等）。
    Seek(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Io(msg) => write!(f, "IO 错误：{msg}"),
            DecodeError::Unsupported(msg) => write!(f, "不支持：{msg}"),
            DecodeError::Decode(msg) => write!(f, "解码错误：{msg}"),
            DecodeError::Seek(msg) => write!(f, "Seek 错误：{msg}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// 把 std::io::Error 映射为 [DecodeError::Io]，使 File::open 等
/// 可直接用 ? 传播到后端方法的返回类型。
impl From<std::io::Error> for DecodeError {
    fn from(e: std::io::Error) -> Self {
        DecodeError::Io(e.to_string())
    }
}

/// 把 symphonia 的内部错误映射为后端无关的 [DecodeError]。
///
/// 分类规则：
/// - IoError → [DecodeError::Io]
/// - DecodeError → [DecodeError::Decode]
/// - Unsupported → [DecodeError::Unsupported]
/// - SeekError → [DecodeError::Seek]（SeekErrorKind 仅 Debug，取 Debug 文本）
/// - 其余（LimitError/ResetRequired 及未来新增，因 `#[non_exhaustive]`）
///   → [DecodeError::Decode] 兜底
impl From<SymphoniaError> for DecodeError {
    fn from(e: SymphoniaError) -> Self {
        match e {
            SymphoniaError::IoError(io) => DecodeError::Io(io.to_string()),
            SymphoniaError::DecodeError(msg) => DecodeError::Decode(msg.to_string()),
            SymphoniaError::Unsupported(feature) => DecodeError::Unsupported(feature.to_string()),
            SymphoniaError::SeekError(kind) => DecodeError::Seek(format!("{kind:?}")),
            other => DecodeError::Decode(other.to_string()),
        }
    }
}

/// 工厂：按文件扩展名/内容探测分发到具体解码后端。
///
/// 当前有 symphonia / Opus / WavPack / FFmpeg 四个后端，在此按扩展名或
/// 内容探测追加分支。FFmpeg 后端为进程外 IPC，仅桌面可选。
/// 返回 `Box<dyn DecoderBackend>` 使调用方与具体后端解耦——
/// 这是"可插拔"的关键接缝。
pub fn open_backend(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    // 分发点：.opus 扩展名或 Ogg 首包 OpusHead 魔数 → Opus 后端；
    // .wv 扩展名 → WavPack 后端；其余统一走 symphonia（按容器探测格式）。
    if is_opus_path(path) {
        let backend = OpusBackend::open(path)?;
        return Ok(Box::new(backend));
    }
    if is_wavpack_path(path) {
        let backend = WavPackBackend::open(path)?;
        return Ok(Box::new(backend));
    }
    // FFmpeg 进程外后端——仅桌面可选。
    // is_ffmpeg_path 按扩展名匹配（.ape/.wma/.flv 等 symphonia 不支持的格式）。
    // FfmpegBackend::open 内部检查 ffmpeg 二进制存在性，不存在时返回
    // DecodeError::Unsupported，调用方可据此向用户提示安装或降级处理。
    if is_ffmpeg_path(path) {
        let backend = FfmpegBackend::open(path)?;
        return Ok(Box::new(backend));
    }
    let backend = SymphoniaBackend::open(path)?;
    Ok(Box::new(backend))
}

/// symphonia 后端：封装 symphonia 的 FormatReader + AudioDecoder。
///
/// 内部实现，不进入公共 API 面——外部经 [open_backend] 获取
/// Box<dyn DecoderBackend>。一个实例对应一个已打开文件；换曲时丢弃
/// 旧实例、新建一个。字段保持私有。
pub(crate) struct SymphoniaBackend {
    /// 格式读取器：从文件解封装出数据包。
    reader: Box<dyn FormatReader>,
    /// 音频解码器：把数据包解码为 PCM 缓冲。AudioDecoder 是 0.6 的 trait。
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    /// 技术参数。
    params: AudioParams,
}

impl SymphoniaBackend {
    /// 打开音频文件并初始化 symphonia 解码器。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // hint 携带扩展名，帮助 probe 选择正确格式
        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        // probe 探测容器格式，返回 Box<dyn FormatReader>
        let reader = symphonia::default::get_probe().probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )?;

        // 选默认音频轨
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or_else(|| DecodeError::Unsupported("无可用音频轨".to_string()))?;
        let track_id = track.id;

        // 取音频编解码参数（codec_params.audio() 返回 Option<&AudioCodecParameters>）
        let audio_params = track
            .codec_params
            .as_ref()
            .and_then(|cp| cp.audio())
            .ok_or_else(|| DecodeError::Unsupported("缺少音频编解码参数".to_string()))?;

        // 提取技术参数
        let params = AudioParams {
            sample_rate: audio_params.sample_rate,
            // channels 是 Copy 类型，直接 copy 即可，无需 move
            // Option<Channels> 的 map 会消费 self，但 audio_params 是借用。
            // 用 as_ref() 先转成 Option<&Channels> 再映射。
            channels: audio_params.channels.as_ref().map(|c| c.count() as u16),
            bits_per_sample: audio_params.bits_per_sample,
            // AudioCodecId 的 Display 给出格式名（MP3/FLAC/AAC 等）
            codec_name: codec_name_or_ext(&audio_params.codec.to_string(), path),
            track_id,
            // 时长从 track.duration (time_base tick) + time_base 换算
            duration: track.duration.and_then(|dur| {
                track.time_base.map(|tb| {
                    let time = tb.calc_time_saturating(symphonia::core::units::Timestamp::from(
                        dur.get() as i64,
                    ));
                    time.as_secs_f64()
                })
            }),
        };

        // 建立解码器
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(audio_params, &AudioDecoderOptions::default())?;

        Ok(Self {
            reader,
            decoder,
            params,
        })
    }
}

impl DecoderBackend for SymphoniaBackend {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        loop {
            match self.reader.next_packet()? {
                Some(packet) => {
                    // 0.6 的 Packet 用字段 track_id（非方法）
                    if packet.track_id != self.params.track_id {
                        continue;
                    }
                    let decoded = self.decoder.decode(&packet)?;
                    // 预分配交错样本数，填 0.0（f32 的中值/静音）
                    let n = decoded.samples_interleaved();
                    let mut samples: Vec<f32> = vec![0.0; n];
                    decoded.copy_to_slice_interleaved(&mut samples);
                    return Ok(Some(samples));
                }
                None => return Ok(None),
            }
        }
    }

    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        use symphonia::core::formats::{SeekMode, SeekTo};
        use symphonia::core::units::Time;
        // 0.6 的 Time 用 try_from_secs_f64（可能因精度溢出返回 None）
        let time = Time::try_from_secs_f64(secs)
            .ok_or_else(|| DecodeError::Unsupported("秒数超出可表示范围".to_string()))?;
        let _sought = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.params.track_id),
            },
        )?;
        // 实际落点 (sought.actual_ts) 未透传——见 trait 方法文档说明。
        Ok(secs)
    }
}

/// 轻量探测文件的音频采样率（只读容器头，不建 decoder，不解码）。
///
/// 用途：原生采样率直通策略——换曲时音频线程需要先知道文件采样率，
/// 才能决定是否重建 Stream。完整后端 open 会建立解码器（占内存），
/// 这里只需 sample_rate，故只 probe header 后立即丢弃，开销几 ms。
///
/// 失败（打不开、无音频轨、无采样率信息）返回 None，调用方据此回退到
/// "用设备默认采样率 + 软件重采样"的降级路径。
///
/// 注意：本函数不覆盖 ffmpeg 长尾格式（ape/wma/tak 等）——这些格式恒返回
/// None、恒走软件重采样（设计可接受：长尾格式本就走进程外解码，无原生直通）。
pub fn probe_sample_rate(path: &Path) -> Option<u32> {
    // .wv 走 wavicle 首块头解析（symphonia 不识别 WavPack 容器）。
    if is_wavpack_path(path) {
        return super::wavpack::probe_sample_rate(path);
    }
    // .opus 输出率由 OpusHead 决定（8/12/16/24/48k 原生，其余按 48k）：
    // symphonia 不识别 Ogg Opus 容器，读 OpusHead 映射，不硬编码 48k。
    if is_opus_path(path) {
        return super::opus::probe_sample_rate(path);
    }
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;

    let track = reader.default_track(TrackType::Audio)?;
    track
        .codec_params
        .as_ref()
        .and_then(|cp| cp.audio())
        .and_then(|a| a.sample_rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// probe_sample_rate 能从测试 WAV 读出 44100。
    /// 不建 decoder、不解码，只读容器头。
    #[test]
    #[ignore = "需要 test_tone.wav 测试夹具；运行：cargo test -- --ignored"]
    fn probe_reads_sample_rate() {
        let path = std::path::Path::new("测试音频/test_tone.wav");
        assert!(path.exists(), "测试夹具 test_tone.wav 缺失");
        let sr = probe_sample_rate(path).expect("应能读出采样率");
        assert_eq!(sr, 44100, "测试 WAV 采样率应为 44100");
    }

    /// probe_sample_rate 对不存在的文件返回 None（不 panic）。
    #[test]
    fn probe_missing_returns_none() {
        let path = std::path::Path::new("nonexistent_test_file_12345.wav");
        assert_eq!(probe_sample_rate(path), None, "不存在文件应返回 None");
    }
}
