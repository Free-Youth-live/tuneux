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
//! WavPack / FFmpeg / 自研 WAV / 自研 FLAC / 自研 CDDA 七个后端。FFmpeg 后端以子进程 IPC 方式
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

use super::cdda::{is_bin_path, CddaBackend};
use super::ffmpeg::{is_ffmpeg_path, FfmpegBackend};
use super::flac::{is_flac_path, FlacBackend};
use super::opus::{is_opus_path, OpusBackend};
use super::wav::{is_wav_path, WavBackend};
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

/// 后端注册表条目：一个后端在「打开 / 采样率探测 / 参数探测」上的能力。
/// 新增/删除后端只改 [`BACKENDS`] 一张表；`open_backend` / `probe_sample_rate` /
/// `probe_metadata`（经 `probe_params`）三处分派都从它推导，不再各写 if-else。
struct BackendEntry {
    /// 路径判定（扩展名 / 魔数；各后端谓词互斥，一个路径至多命中一项）。
    is_path: fn(&Path) -> bool,
    /// 打开解码后端（已装箱为 `Box<dyn DecoderBackend>`）。
    open: fn(&Path) -> Result<Box<dyn DecoderBackend>, DecodeError>,
    /// 打开返回 `Unsupported` 时是否回退 symphonia（仅 flac / wav 回退，存量零退化）。
    fallback_on_unsupported: bool,
    /// 轻量采样率探测（原生直通用）；None = 本后端不提供（走 symphonia / 降级）。
    probe_sample_rate: Option<fn(&Path) -> Option<u32>>,
    /// 技术参数探测（返回不含标签的 `AudioParams`）；None = 不参与元数据探测
    ///（走 symphonia 全标签）。
    probe_params: Option<fn(&Path) -> Option<AudioParams>>,
}

/// 把后端 `open` 的 `Result<Self, _>` 统一装箱为 `Box<dyn DecoderBackend>`。
fn boxed<T: DecoderBackend + 'static>(
    r: Result<T, DecodeError>,
) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    r.map(|b| Box::new(b) as Box<dyn DecoderBackend>)
}

fn open_opus(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    boxed(OpusBackend::open(path))
}
fn open_wavpack(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    boxed(WavPackBackend::open(path))
}
fn open_flac(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    boxed(FlacBackend::open(path))
}
fn open_wav(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    boxed(WavBackend::open(path))
}
fn open_cdda(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    boxed(CddaBackend::open(path))
}
fn open_ffmpeg(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    boxed(FfmpegBackend::open(path))
}

/// Opus 技术参数探测：开自建后端取 params（标签留空由媒体层兜底）。
fn opus_probe_params(path: &Path) -> Option<AudioParams> {
    OpusBackend::open(path).ok().map(|b| b.params().clone())
}
/// CDDA 技术参数探测：开自建后端取 params（时长精确；标签留空）。
fn cdda_probe_params(path: &Path) -> Option<AudioParams> {
    CddaBackend::open(path).ok().map(|b| b.params().clone())
}

/// 后端注册表（顺序无关：各后端谓词互斥；symphonia 是最终回退、不在表内）。
static BACKENDS: &[BackendEntry] = &[
    BackendEntry {
        is_path: is_opus_path,
        open: open_opus,
        fallback_on_unsupported: false,
        probe_sample_rate: Some(super::opus::probe_sample_rate),
        probe_params: Some(opus_probe_params),
    },
    BackendEntry {
        is_path: is_wavpack_path,
        open: open_wavpack,
        fallback_on_unsupported: false,
        probe_sample_rate: Some(super::wavpack::probe_sample_rate),
        probe_params: Some(super::wavpack::probe_params),
    },
    BackendEntry {
        is_path: is_flac_path,
        open: open_flac,
        fallback_on_unsupported: true,
        probe_sample_rate: Some(super::flac::probe_sample_rate),
        probe_params: None, // flac 元数据走 symphonia（全标签）
    },
    BackendEntry {
        is_path: is_wav_path,
        open: open_wav,
        fallback_on_unsupported: true,
        probe_sample_rate: Some(super::wav::probe_sample_rate),
        probe_params: None, // wav 元数据走 symphonia（全标签）
    },
    BackendEntry {
        is_path: is_bin_path,
        open: open_cdda,
        fallback_on_unsupported: false,
        probe_sample_rate: Some(super::cdda::probe_sample_rate),
        probe_params: Some(cdda_probe_params),
    },
    BackendEntry {
        is_path: is_ffmpeg_path,
        open: open_ffmpeg,
        fallback_on_unsupported: false,
        probe_sample_rate: None, // ffmpeg 长尾无原生直通
        probe_params: None,      // ffmpeg 长尾不参与元数据探测
    },
];

/// 工厂：按 BACKENDS 注册表（私有常量，见本文件）分发到具体解码后端；
/// symphonia 是最终回退。
///
/// 当前有 symphonia / Opus / WavPack / FFmpeg / 自研 WAV / 自研 FLAC / 自研 CDDA 七个后端。
/// FFmpeg 后端为进程外 IPC，仅桌面可选。返回 `Box<dyn DecoderBackend>` 使调用方
/// 与具体后端解耦——这是"可插拔"的关键接缝。
pub fn open_backend(path: &Path) -> Result<Box<dyn DecoderBackend>, DecodeError> {
    for entry in BACKENDS {
        if (entry.is_path)(path) {
            match (entry.open)(path) {
                Ok(backend) => return Ok(backend),
                // flac / wav 自研不支持时回退 symphonia（存量文件零退化）；
                // 其余后端的 Unsupported 原样上抛（如 ffmpeg 未安装）。
                Err(DecodeError::Unsupported(_)) if entry.fallback_on_unsupported => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(Box::new(SymphoniaBackend::open(path)?))
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
        let sought = self.reader.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.params.track_id),
            },
        )?;
        // 透传实际落点：粗粒度 seek（MP3 无 Xing 头）的实际位置可能与
        // 请求值偏差数百毫秒；用实际值让进度条与音频对齐。
        let track = self
            .reader
            .tracks()
            .iter()
            .find(|t| t.id == self.params.track_id);
        let actual_secs = match track {
            Some(t) => t.time_base.map_or(secs, |tb| {
                tb.calc_time_saturating(sought.actual_ts).as_secs_f64()
            }),
            // 找不到轨（极端）：回退请求值，不阻断
            None => secs,
        };
        Ok(actual_secs)
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
    // 注册表：命中自建后端的轻量探测（cdda/flac/wav/wavpack/opus）。
    // 与旧实现同口径：命中即 return（即使返回 None 也不再回退 symphonia）。
    for entry in BACKENDS {
        if let Some(probe) = entry.probe_sample_rate {
            if (entry.is_path)(path) {
                return probe(path);
            }
        }
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

/// 探测技术参数（不含标签）：供 `probe_metadata` 使用。
///
/// 命中自建后端（opus / wavpack / cdda）返回 `AudioParams`；其余（flac / wav /
/// mp3 / ffmpeg 长尾等）返回 None，由调用方走 symphonia 全标签探测。
pub(crate) fn probe_params(path: &Path) -> Option<AudioParams> {
    for entry in BACKENDS {
        if let Some(probe) = entry.probe_params {
            if (entry.is_path)(path) {
                return probe(path);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模糊化烟雾测试：任意字节容器经 open_backend → probe → decode/seek
    /// 不得 panic。覆盖自研后端（WAV / FLAC / WavPack / Ogg-Opus / CD-DA）
    /// 与 symphonia 桥接的解析健壮性——恶意音频文件是威胁模型里的最高
    /// 价值攻击面，任何 panic 都是缺陷。部分轮次注入真魔数（RIFF / fLaC /
    /// wvpk / OggS）以走进更深的容器解析分支。（真 fuzz 用 cargo-fuzz +
    /// 消毒器；此处用确定性伪随机做「永不 panic」轻量守护，进常规测试跑；
    /// ffmpeg 系扩展名不入目标——进程外后端另行处理。）
    #[test]
    fn backends_never_panic_on_garbage_files() {
        // (扩展名, 可选魔数)：魔数轮写入文件头，其余轮纯随机字节。
        let targets: [(&str, Option<&[u8]>); 7] = [
            ("wav", Some(b"RIFF\0\0\0\0WAVE")),
            ("flac", Some(b"fLaC")),
            ("wv", Some(b"wvpk")),
            ("ogg", Some(b"OggS")),
            ("bin", None),
            ("mp3", Some(b"ID3\x03\0\0\0\0")),
            ("m4a", Some(b"\0\0\0 ftypM4A ")),
        ];
        let dir =
            std::env::temp_dir().join(format!("tuneux_fuzz_lite_decoder_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录可建");
        let mut state: u64 = 0x2026_0927_5eed_beef;
        let mut rng = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..48usize {
            for (ext, magic) in targets {
                let len = (rng() % 8192) as usize;
                let mut bytes: Vec<u8> = (0..len).map(|_| (rng() & 0xff) as u8).collect();
                // 隔轮注入魔数：同一长度分布下交替「有头/无头」两态。
                if round % 2 == 0 {
                    if let Some(m) = magic {
                        let n = m.len().min(bytes.len());
                        bytes[..n].copy_from_slice(&m[..n]);
                    }
                }
                let path = dir.join(format!("f{round}.{ext}"));
                std::fs::write(&path, &bytes).expect("临时文件可写");
                // 结果 Ok/Err/None 均可接受，唯一红线是不得 panic。
                let _ = probe_sample_rate(&path);
                let _ = super::super::probe::probe_metadata(&path);
                if let Ok(mut b) = open_backend(&path) {
                    let _ = b.params();
                    for _ in 0..3 {
                        let _ = b.decode_next();
                    }
                    let _ = b.seek(1.0);
                    let _ = b.decode_next();
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// probe_sample_rate 能从测试 WAV 读出 44100。
    /// 不建 decoder、不解码，只读容器头。
    #[test]
    #[ignore = "需要 test_tone.wav 测试夹具；运行：cargo test -- --ignored"]
    fn probe_reads_sample_rate() {
        // 夹具位于工作区外层 测试音频/（仓库外、物理隔离）；按
        // CARGO_MANIFEST_DIR 定位，与测试运行 cwd 无关。
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../测试音频/test_tone.wav");
        let path = fixture.as_path();
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

    /// 后端注册表对拍：六个原生后端齐备；按扩展名判定时各谓词互斥、
    /// 命中唯一；mp3 不在表内（走 symphonia 回退）。
    ///（cdda 的 is_bin_path 需要真实文件尺寸，故不在本扩展名级测试内。）
    #[test]
    fn backend_registry_dispatch_is_exclusive() {
        assert_eq!(BACKENDS.len(), 6, "注册表应含六个原生后端");

        // (文件名, 期望命中的后端索引)
        let cases = [
            ("a.opus", 0usize),
            ("a.wv", 1usize),
            ("a.flac", 2usize),
            ("a.wav", 3usize),
            ("a.ape", 5usize),
        ];
        for (name, expect) in cases {
            let matched: Vec<usize> = BACKENDS
                .iter()
                .enumerate()
                .filter(|(_, e)| (e.is_path)(std::path::Path::new(name)))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(matched, vec![expect], "{name} 应只命中后端 {expect}");
        }
        // mp3 不在注册表：走 symphonia 回退。
        assert!(
            BACKENDS
                .iter()
                .all(|e| !(e.is_path)(std::path::Path::new("a.mp3"))),
            "mp3 不应命中任何注册表后端"
        );
    }

    /// 对拍：flac / wav 的元数据走 symphonia（注册表 probe_params 不接）；
    /// 它们的采样率探测由自研头解析接（probe_sample_rate 有值）。
    #[test]
    fn backend_registry_probe_capabilities() {
        let flac = &BACKENDS[2];
        assert!(flac.probe_sample_rate.is_some(), "flac 应有自研采样率探测");
        assert!(flac.probe_params.is_none(), "flac 元数据应走 symphonia");
        assert!(
            flac.fallback_on_unsupported,
            "flac 自研不支持应回退 symphonia"
        );

        let wav = &BACKENDS[3];
        assert!(wav.probe_sample_rate.is_some(), "wav 应有自研采样率探测");
        assert!(wav.probe_params.is_none(), "wav 元数据应走 symphonia");
        assert!(
            wav.fallback_on_unsupported,
            "wav 自研不支持应回退 symphonia"
        );

        let ffmpeg = &BACKENDS[5];
        assert!(ffmpeg.probe_sample_rate.is_none(), "ffmpeg 长尾无原生直通");
        assert!(ffmpeg.probe_params.is_none(), "ffmpeg 长尾不参与元数据探测");
        assert!(
            !ffmpeg.fallback_on_unsupported,
            "ffmpeg 缺失不应回退 symphonia"
        );
    }
}
