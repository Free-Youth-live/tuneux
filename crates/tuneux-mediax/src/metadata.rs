//! # 元数据提取模块
//!
//! 从音频文件中提取两类元数据：
//!
//! 1. **标签元数据**（来自 ID3/Vorbis Comment/MP4 atom 等）：
//!    标题、艺术家、专辑、曲序；
//! 2. **技术参数**（来自容器/编码头）：编码格式、采样率、位深、通道数、
//!    时长、码率。
//!
//! 这些信息在 TUI 元数据面板展示，帮助用户识别当前播放的曲目。
//!
//! ## 码率估算
//!
//! 底层解码库不直接提供码率。本模块用标准做法估算：
//! `码率 = 文件大小(字节) × 8 / 时长(秒)`，得到平均码率（bps）。
//! 对 CBR 文件较准；VBR 文件反映整首曲目的平均值。
//!
//! ## 时长计算
//!
//! 容器记录的 `Track.duration`（time_base 单位）+ `Track.time_base` 换算为秒。
//! 部分格式（如某些 MP3 无头部时长信息）可能为 None，此时显示"未知"。

use std::path::Path;

use tuneux_corex::{probe_metadata, AudioParams};

/// 一首曲目的完整元数据（标签 + 技术参数）。
///
/// 字符串字段用 `Option<String>`：None 表示该标签缺失（如现场录音可能无专辑名），
/// 显示时降级为"未知"。技术参数同理。
#[derive(Debug, Clone, Default)]
pub struct TrackMetadata {
    // —— 标签元数据 ——
    /// 曲目标题。缺失时显示文件名。
    pub title: Option<String>,
    /// 艺术家/演奏者。
    pub artist: Option<String>,
    /// 专辑名。
    pub album: Option<String>,
    /// 曲序（专辑内的曲目编号，从 1 起）。
    pub track_number: Option<u32>,
    /// 内嵌歌词文本（来自标签，如 ID3v2 USLT / Vorbis LYRICS / MP4 ©lyr）。
    ///
    /// 底层探测把这些统统归一为内嵌歌词字段，因此这里拿到的是原始歌词字符串
    ///（可能是带时间戳的 LRC，也可能是纯文本）。解析与展示交给 lyrics 模块
    ///（Lyrics::from_embedded）。
    /// 注意：ID3v2 的 SYLT（同步歌词）帧底层暂不支持，属后续工作。
    pub lyrics: Option<String>,

    // —— 技术参数 ——
    /// 编码格式名（如 "Mp3"、"Flac"），用户可读。
    pub codec: Option<String>,
    /// 采样率（Hz）。
    pub sample_rate: Option<u32>,
    /// 位深（bit）。
    pub bits_per_sample: Option<u32>,
    /// 通道数。
    pub channels: Option<u16>,
    /// 总时长（秒）。未知为 None。
    pub duration: Option<f64>,
    /// 平均码率（bps）。未知为 None。
    pub bitrate: Option<u64>,

    /// 封面图原始字节 + MIME（如 "image/jpeg" / "image/png"）。
    /// 来自 FLAC PICTURE block 或 MP3 ID3v2 APIC frame。
    /// 存原始字节而非解码后的 DynamicImage：避免在 metadata 模块拉 `image`
    /// crate 依赖；解码延迟到 UI 真要画时再做（带缓存）。
    pub cover: Option<CoverImage>,
}

/// 封面图原始数据。
///
/// 字节 + MIME 一并保存——解码时按 MIME 选 image crate loader
///（image 0.25 关闭默认 feature、显式开启 jpeg/png 即可解 PNG/JPEG；WebP 需额外开 feature）。
#[derive(Debug, Clone)]
pub struct CoverImage {
    /// 原始编码字节（PNG/JPEG/etc.）。
    pub bytes: Vec<u8>,
    /// MIME 类型，如 "image/jpeg"、"image/png"。
    pub mime: String,
}

impl TrackMetadata {
    /// 从音频文件提取完整元数据。
    ///
    /// 内部打开文件读取标签与技术参数后立即关闭（不做完整解码）。
    /// 失败字段降级为 None，绝不返回 Err——元数据缺失不应阻塞播放。
    ///
    /// **标题兜底**：`title` 字段无论打开成功与否（底层探测没读到或文件
    /// 根本打不开）都会用 file_stem 兜底——告诉用户"是哪个文件"，不至于
    /// 看到"（无标题）"以为是首歌叫这个名字。TUI 层用 is_playing 状态
    /// 区分"能播"和"打不开"两种场景。
    pub fn from_file(path: &Path) -> Self {
        let mut md = Self::try_extract(path).unwrap_or_default();
        if md.title.is_none() {
            md.title = Self::fallback_title_from_path(path);
        }
        md
    }

    /// 尝试读取元数据（经 corex 的 `probe_metadata` 探测）。失败（文件不存在/损坏/无音频轨）返回 None。
    ///
    /// 与 [`Self::from_file`] 的区别：本函数不应用任何兜底策略——调用方拿到
    /// `None` 后可决定下一步行为（from_file 的策略是套上默认 + file_stem）。
    fn try_extract(path: &Path) -> Option<Self> {
        let (params, tags) = probe_metadata(path)?;
        // from_params 已把 params.duration 填入 md.duration，这里只补标签与封面。
        let mut md = Self::from_params(&params);
        md.title = tags.title;
        md.artist = tags.artist;
        md.album = tags.album;
        md.track_number = tags.track_number;
        md.lyrics = tags.lyrics;
        md.cover = tags.cover.map(|(bytes, mime)| CoverImage { bytes, mime });

        // 码率 = 文件大小 × 8 / 时长
        // 注意：必须全程浮点计算，不能用 `dur as u64` 作除数——
        // 时长在 (0, 1) 秒时截断为 0 会触发整数除零 panic。
        if let Some(dur) = params.duration {
            if dur > 0.0 {
                if let Ok(meta) = std::fs::metadata(path) {
                    let size = meta.len();
                    md.bitrate = Some(((size as f64 * 8.0) / dur) as u64);
                }
            }
        }

        Some(md)
    }

    /// 从文件路径兜底提取 title：取 `file_stem`（去扩展名），仅当是有效
    /// UTF-8 时返回 `Some`。用于 `from_file` 的 title 兜底路径。
    ///
    /// 设计要点：
    /// - `file_stem` 对 "song.mp3" 返回 Some("song")；对 "track"（无扩展名）
    ///   也返回 Some("track")；对 ".gitignore" 这种隐藏文件返回
    ///   Some(".gitignore")（Rust Path 将其整体视为 stem，无扩展名）；
    /// - `to_str()` 失败（非 UTF-8 文件名）时返回 None，让调用方走
    ///   "（无标题）"占位而非乱码——与 fs_browser 的 UTF-8 严格策略一致。
    fn fallback_title_from_path(path: &Path) -> Option<String> {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_owned())
    }

    /// 仅从 AudioParams 构造技术参数部分（标签留空）。
    /// 用于已打开的解码后端复用其参数，避免重复打开文件。
    pub fn from_params(params: &AudioParams) -> Self {
        Self {
            title: None,
            artist: None,
            album: None,
            track_number: None,
            lyrics: None,
            codec: Some(params.codec_name.clone()),
            sample_rate: params.sample_rate,
            bits_per_sample: params.bits_per_sample,
            channels: params.channels,
            duration: params.duration,
            bitrate: None,
            cover: None,
        }
    }

    // —— 中文格式化方法（供 TUI 显示）——

    /// 时长格式化为 "分:秒"（如 "3:45"），未知返回 "??:??"。
    pub fn duration_label(&self) -> String {
        match self.duration {
            Some(d) => format!("{}:{:02}", (d as u64) / 60, (d as u64) % 60),
            None => "??:??".to_string(),
        }
    }

    /// 采样率格式化（如 "44.1 kHz"、"48 kHz"），未知返回 "未知"。
    pub fn sample_rate_label(&self) -> String {
        match self.sample_rate {
            Some(sr) => {
                // 44100 → "44.1 kHz"，48000 → "48 kHz"
                let khz = sr as f64 / 1000.0;
                if (khz.fract()).abs() < 1e-3 {
                    format!("{:.0} kHz", khz)
                } else {
                    format!("{:.1} kHz", khz)
                }
            }
            None => "未知".to_string(),
        }
    }

    /// 位深格式化（如 "16 bit"），未知返回 "未知"。
    /// 有损格式（MP3/AAC）位深为 None，显示"有损"更准确。
    pub fn bits_label(&self) -> String {
        match self.bits_per_sample {
            Some(b) => format!("{b} bit"),
            None => "有损".to_string(),
        }
    }

    /// 码率格式化（如 "320 kbps"），未知返回 "未知"。
    pub fn bitrate_label(&self) -> String {
        match self.bitrate {
            Some(b) => format!("{} kbps", b / 1000),
            None => "未知".to_string(),
        }
    }

    /// 通道格式化（如 "立体声"、"单声道"），未知返回 "未知"。
    pub fn channels_label(&self) -> String {
        match self.channels {
            Some(1) => "单声道".to_string(),
            Some(2) => "立体声".to_string(),
            Some(n) => format!("{n} 声道"),
            None => "未知".to_string(),
        }
    }

    /// 一行式技术摘要（如 "MP3 · 320 kbps · 44.1 kHz · 立体声"）。
    /// 用于控制条等紧凑显示位置。
    pub fn tech_summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(c) = &self.codec {
            parts.push(c.clone());
        }
        if self.bitrate.is_some() {
            parts.push(self.bitrate_label());
        }
        if self.sample_rate.is_some() {
            parts.push(self.sample_rate_label());
        }
        if self.channels.is_some() {
            parts.push(self.channels_label());
        }
        parts.join(" · ")
    }
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    // 测试里故意反复改字段验证不同取值，Default::default() 后逐字段赋值
    // 是刻意的可读性选择，压制 clippy 的 field_reassign_with_default 建议。
    #![allow(clippy::field_reassign_with_default)]
    use super::*;

    #[test]
    fn format_sample_rate() {
        let mut md = TrackMetadata::default();
        md.sample_rate = Some(44100);
        assert_eq!(md.sample_rate_label(), "44.1 kHz");
        md.sample_rate = Some(48000);
        assert_eq!(md.sample_rate_label(), "48 kHz");
        md.sample_rate = Some(96000);
        assert_eq!(md.sample_rate_label(), "96 kHz");
        md.sample_rate = None;
        assert_eq!(md.sample_rate_label(), "未知");
    }

    #[test]
    fn format_duration() {
        let mut md = TrackMetadata::default();
        md.duration = Some(225.0); // 3 分 45 秒
        assert_eq!(md.duration_label(), "3:45");
        md.duration = Some(59.0);
        assert_eq!(md.duration_label(), "0:59");
        md.duration = None;
        assert_eq!(md.duration_label(), "??:??");
    }

    #[test]
    fn format_bitrate() {
        let mut md = TrackMetadata::default();
        md.bitrate = Some(320_000);
        assert_eq!(md.bitrate_label(), "320 kbps");
        md.bitrate = Some(1_411_200); // CD 质量
        assert_eq!(md.bitrate_label(), "1411 kbps");
        md.bitrate = None;
        assert_eq!(md.bitrate_label(), "未知");
    }

    #[test]
    fn format_bits_and_channels() {
        let mut md = TrackMetadata::default();
        md.bits_per_sample = Some(16);
        assert_eq!(md.bits_label(), "16 bit");
        md.bits_per_sample = None;
        assert_eq!(md.bits_label(), "有损");

        md.channels = Some(1);
        assert_eq!(md.channels_label(), "单声道");
        md.channels = Some(2);
        assert_eq!(md.channels_label(), "立体声");
        md.channels = Some(6);
        assert_eq!(md.channels_label(), "6 声道");
        md.channels = None;
        assert_eq!(md.channels_label(), "未知");
    }

    #[test]
    fn tech_summary_combines() {
        let md = TrackMetadata {
            codec: Some("MP3".to_string()),
            bitrate: Some(320_000),
            sample_rate: Some(44100),
            channels: Some(2),
            ..Default::default()
        };
        assert_eq!(md.tech_summary(), "MP3 · 320 kbps · 44.1 kHz · 立体声");
    }

    #[test]
    fn title_falls_back_to_filename() {
        let path = std::path::Path::new("/music/我的歌.mp3");
        let md = TrackMetadata::from_file(path);
        // 文件打不开时仍走 file_stem 兜底，让用户至少知道是哪个文件
        assert_eq!(md.title.as_deref(), Some("我的歌"));
    }

    /// `fallback_title_from_path` 的纯函数单测：不依赖文件系统、音频探测。
    #[test]
    fn fallback_title_normal() {
        // 普通文件：去扩展名
        assert_eq!(
            TrackMetadata::fallback_title_from_path(Path::new("/music/song.mp3")),
            Some("song".into())
        );
        // 无扩展名：整个文件名就是 stem
        assert_eq!(
            TrackMetadata::fallback_title_from_path(Path::new("/music/track")),
            Some("track".into())
        );
        // 多个点：只去最后一个扩展名
        assert_eq!(
            TrackMetadata::fallback_title_from_path(Path::new("/x/archive.tar.gz")),
            Some("archive.tar".into())
        );
        // 隐藏文件（Unix 习惯以 . 开头）：在 Rust Path 看来没有"扩展名"，
        // file_stem 返回整个文件名。保留为"显示原文件名"是合理行为
        // （fs_browser 已经过滤了隐藏文件，理论上走不到这里）。
        assert_eq!(
            TrackMetadata::fallback_title_from_path(Path::new(".gitignore")),
            Some(".gitignore".into())
        );
        // 空路径：file_stem 返回 None
        assert_eq!(TrackMetadata::fallback_title_from_path(Path::new("")), None);
    }

    /// `from_file` 在打开失败时也走 file_stem 兜底（这是这次改动的关键回归点）。
    /// 用一个肯定打不开的路径（不存在的文件）触发 `try_extract` 返回 None。
    #[test]
    fn from_file_fallback_when_open_fails() {
        let path = Path::new("/this/path/does/not/exist/我的歌.flac");
        let md = TrackMetadata::from_file(path);
        // 打开失败 → title 用 file_stem 兜底
        assert_eq!(md.title.as_deref(), Some("我的歌"));
        // 其他字段保持 default 状态
        assert!(md.artist.is_none());
        assert!(md.album.is_none());
        assert!(md.duration.is_none());
    }

    /// 回归测试：亚秒时长（0.5s）的音频文件计算码率时不能触发整数
    /// 除零 panic。旧实现 `(size * 8) / dur as u64` 对 dur ∈ (0, 1) 秒时
    /// `dur as u64` 截断为 0 → 除零崩溃；修复后全程浮点计算。
    /// 测试文件用代码内生成的 RIFF WAV（0.5s 静音），不依赖外部夹具，
    /// 因此不需要 #[ignore]。
    #[test]
    fn subsecond_audio_no_div_zero_panic() {
        let dir = std::env::temp_dir().join("tuneux_metadata_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("subsecond.wav");

        // 构造 44.1kHz / 16-bit / mono / 0.5 秒的 RIFF WAV 头 + 静音样本
        let sample_rate: u32 = 44100;
        let channels: u16 = 1;
        let bits: u16 = 16;
        let num_samples: u32 = sample_rate / 2; // 0.5 秒
        let data_size: u32 = num_samples * (bits / 8) as u32 * channels as u32;
        let byte_rate: u32 = sample_rate * channels as u32 * (bits / 8) as u32;
        let block_align: u16 = channels * (bits / 8);

        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_size).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // fmt 块大小
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.resize(wav.len() + data_size as usize, 0); // 静音样本

        std::fs::write(&path, &wav).unwrap();

        // 核心断言：亚秒文件不 panic，且码率正确（≈44100B×8/0.5s≈705kbps）
        let md = TrackMetadata::from_file(&path);
        assert!(md.duration.is_some(), "亚秒 WAV 应能读出时长");
        let bitrate = md.bitrate.expect("亚秒 WAV 应能算出码率（不得除零）");
        assert!(
            (bitrate as f64 - 705_600.0).abs() < 50_000.0,
            "码率应在 705kbps 附近，实际 {bitrate}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
