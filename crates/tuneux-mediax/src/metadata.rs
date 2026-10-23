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
//! ## 文件夹封面回退
//!
//! 封面来源优先级：内嵌封面（FLAC PICTURE / ID3v2 APIC）＞ 同目录约定
//! 图片文件。无内嵌封面时，按业界惯例文件名（cover / folder / front /
//! album × jpg / jpeg / png，大小写不敏感）在音频文件所在目录取第一个
//! 命中项，读原始字节、按扩展名推断 MIME。只在无内嵌封面时列目录一次，
//! 开销可忽略。
//!
//! ## 时长计算
//!
//! 容器记录的 `Track.duration`（time_base 单位）+ `Track.time_base` 换算为秒。
//! 部分格式（如某些 MP3 无头部时长信息）可能为 None，此时显示"未知"。

use std::path::Path;

use tuneux_corex::{probe_metadata, AudioParams};

/// 采样率显示标签（语义枚举，产品本地化）。
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg(test)]
pub enum SampleRateDisplay {
    /// 采样率（kHz，如 44.1）。
    KHz(f64),
    /// 未知。
    Unknown,
}

/// 位深显示标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum BitsDisplay {
    /// 确定位深（如 16、24）。
    Bits(u32),
    /// 有损格式（MP3/AAC/Vorbis/Opus 等位深无意义，比"未知"更准确）。
    Lossy,
    /// 未知（探测失败或格式无法判定）。
    Unknown,
}

/// 通道数显示标签。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub enum ChannelsDisplay {
    /// 单声道。
    Mono,
    /// 立体声。
    Stereo,
    /// 多声道（具体数量）。
    Multi(u16),
    /// 未知。
    Unknown,
}

/// 采样率 kHz 格式化（如 44100 → "44.1 kHz"，352800 → "352.8 kHz"）。
/// 引擎实测回落路径与 [`TrackMetadata::sample_rate_label`] 共用本函数——
/// 三端技术参数行的采样率格式单一来源（元数据与引擎回落同格式）。
pub fn sample_rate_khz_label(rate: u32) -> String {
    let khz = rate as f64 / 1000.0;
    if (khz.fract()).abs() < 1e-3 {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

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
        // 内嵌封面优先；无内嵌时回退到同目录约定图片文件（见模块头说明）。
        md.cover = tags
            .cover
            .map(|(bytes, mime)| CoverImage { bytes, mime })
            .or_else(|| Self::folder_cover(path));

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

    /// 文件夹封面回退：音频文件无内嵌封面时，查其所在目录的约定图片文件。
    ///
    /// 约定文件名（大小写不敏感）：cover / folder / front / album ×
    /// jpg / jpeg / png，按清单序取第一个命中项（cover 优先于 folder 等）。
    /// 命中但文件超限（防误读异常大文件拖慢元数据提取）则跳过该项继续找。
    /// 读取或列举失败一律返回 None（无封面），绝不上抛错误。
    fn folder_cover(audio_path: &Path) -> Option<CoverImage> {
        const STEMS: [&str; 4] = ["cover", "folder", "front", "album"];
        const EXTS: [&str; 3] = ["jpg", "jpeg", "png"];
        /// 文件夹封面超过此字节数不读。
        const MAX_COVER_BYTES: u64 = 32 * 1024 * 1024;

        let dir = audio_path.parent()?;
        let entries = std::fs::read_dir(dir).ok()?;
        // 收集 (小写文件名, 路径)，再按清单序匹配——一次列目录换来
        // 大小写不敏感（Cover.JPG 也能命中），比逐个试路径稳健。
        let mut names: Vec<(String, std::path::PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    names.push((name.to_lowercase(), p));
                }
            }
        }
        for stem in STEMS {
            for ext in EXTS {
                let want = format!("{stem}.{ext}");
                let Some((_, p)) = names.iter().find(|(n, _)| *n == want) else {
                    continue;
                };
                let Ok(meta) = std::fs::metadata(p) else {
                    continue;
                };
                if meta.len() > MAX_COVER_BYTES {
                    continue;
                }
                let Ok(bytes) = std::fs::read(p) else {
                    continue;
                };
                let mime = if ext == "png" {
                    "image/png"
                } else {
                    "image/jpeg"
                };
                return Some(CoverImage {
                    bytes,
                    mime: mime.to_string(),
                });
            }
        }
        None
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
    #[cfg(test)]
    pub fn duration_label(&self) -> String {
        match self.duration {
            Some(d) => format!("{}:{:02}", (d as u64) / 60, (d as u64) % 60),
            None => "??:??".to_string(),
        }
    }

    /// 采样率格式化（如 "44.1 kHz"、"48 kHz"），未知返回 "未知"。
    pub fn sample_rate_label(&self) -> String {
        match self.sample_rate {
            Some(sr) => sample_rate_khz_label(sr),
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

    /// 引擎回落（纯函数口径）：FFmpeg 长尾曲目的文件探测只有兜底记录
    ///（技术字段全 None），用后端 open 实测的源参数补齐 codec / 采样率 /
    /// 声道 / 位深 / 时长。已有值不覆盖（元数据优先）；码率无引擎侧
    /// 来源不补（保持元数据单源）。与 max 属性弹窗同一语义。
    pub fn overlay_engine_params(
        mut self,
        codec: Option<String>,
        rate: Option<u32>,
        channels: Option<u16>,
        bits: Option<u32>,
        duration: Option<f64>,
    ) -> Self {
        self.codec = self.codec.or(codec);
        self.sample_rate = self.sample_rate.or(rate);
        self.channels = self.channels.or(channels);
        self.bits_per_sample = self.bits_per_sample.or(bits);
        self.duration = self.duration.or(duration);
        self
    }

    /// 引擎回落便捷封装：以引擎实测源参数快照（后端 open 时的
    /// `AudioParams`）补缺（字段语义见 [`Self::overlay_engine_params`]；
    /// 时长仅取有效正值）；无快照时原样返回。TUI 双端属性弹窗共用
    /// 本入口，避免回落逻辑双份实现。
    pub fn overlaid_with_engine_params(self, params: Option<AudioParams>) -> Self {
        let Some(p) = params else {
            return self;
        };
        self.overlay_engine_params(
            Some(p.codec_name),
            p.sample_rate,
            p.channels,
            p.bits_per_sample,
            p.duration.filter(|d| *d > 0.0),
        )
    }

    /// 技术参数行文本（编码 · 码率 · 采样率 · 位深）：元数据优先，
    /// 缺失段回落引擎实测源参数（FFmpeg 长尾格式探测为空时仍显示真实
    /// 参数）；引擎侧也无值时维持原降级文案。码率无引擎侧来源不补。
    /// `unknown_format` 为编码名缺失时的本地化占位（如 i18n
    /// `msg.unknown_format`）。tuneux / tuneux-fx 双端共用本函数，
    /// 保证回落口径一致。
    pub fn tech_line_with_engine(
        &self,
        eng_codec: Option<&str>,
        eng_rate: Option<u32>,
        eng_bits: Option<u32>,
        unknown_format: &str,
    ) -> String {
        let codec = self
            .codec
            .as_deref()
            .or(eng_codec)
            .unwrap_or(unknown_format);
        let rate = match self.sample_rate.or(eng_rate) {
            Some(r) => sample_rate_khz_label(r),
            None => self.sample_rate_label(),
        };
        let bits = match self.bits_per_sample.or(eng_bits) {
            Some(b) => format!("{b} bit"),
            None => self.bits_label(),
        };
        format!("{} · {} · {} · {}", codec, self.bitrate_label(), rate, bits)
    }
    /// 采样率语义标签（产品据此本地化，替代 *_label 的中文硬编码）。
    #[cfg(test)]
    pub fn sample_rate_display(&self) -> SampleRateDisplay {
        match self.sample_rate {
            Some(sr) => SampleRateDisplay::KHz(sr as f64 / 1000.0),
            None => SampleRateDisplay::Unknown,
        }
    }
    /// 位深语义标签。
    #[cfg(test)]
    pub fn bits_display(&self) -> BitsDisplay {
        match self.bits_per_sample {
            Some(b) => BitsDisplay::Bits(b),
            None => {
                if self.codec.as_deref().is_some_and(|c| {
                    c.eq_ignore_ascii_case("mp3")
                        || c.eq_ignore_ascii_case("aac")
                        || c.eq_ignore_ascii_case("vorbis")
                        || c.eq_ignore_ascii_case("opus")
                }) {
                    BitsDisplay::Lossy
                } else {
                    BitsDisplay::Unknown
                }
            }
        }
    }
    /// 通道数语义标签。
    #[cfg(test)]
    pub fn channels_display(&self) -> ChannelsDisplay {
        match self.channels {
            Some(1) => ChannelsDisplay::Mono,
            Some(2) => ChannelsDisplay::Stereo,
            Some(n) => ChannelsDisplay::Multi(n),
            None => ChannelsDisplay::Unknown,
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

    /// kHz 格式单一来源：DSD→PCM 出口率 352800 应为 "352.8 kHz"
    ///（与 sample_rate_label 同格式，引擎回落路径复用）。
    #[test]
    fn sample_rate_khz_label_formats() {
        assert_eq!(sample_rate_khz_label(44_100), "44.1 kHz");
        assert_eq!(sample_rate_khz_label(48_000), "48 kHz");
        assert_eq!(sample_rate_khz_label(352_800), "352.8 kHz");
        assert_eq!(sample_rate_khz_label(2_822_400), "2822.4 kHz");
    }

    /// 引擎回落：缺失字段补齐、已有字段不覆盖（元数据优先于引擎实测）。
    #[test]
    fn overlay_engine_params_fills_missing_keeps_existing() {
        let md = TrackMetadata {
            sample_rate: Some(44_100),
            ..Default::default()
        };
        let out = md.overlay_engine_params(
            Some("dsd_lsbf_planar".into()),
            Some(352_800),
            Some(2),
            Some(32),
            Some(10.0),
        );
        assert_eq!(out.codec.as_deref(), Some("dsd_lsbf_planar"));
        assert_eq!(out.channels, Some(2));
        assert_eq!(out.bits_per_sample, Some(32));
        assert_eq!(out.duration, Some(10.0));
        assert_eq!(out.sample_rate, Some(44_100), "已有字段不被覆盖");
        // 引擎全空时为 no-op
        let md = TrackMetadata {
            title: Some("x".into()),
            ..Default::default()
        };
        let out = md.overlay_engine_params(None, None, None, None, None);
        assert_eq!(out.codec, None);
        assert_eq!(out.title.as_deref(), Some("x"));
    }

    /// AudioParams 快照回落：整包字段映射、非正时长滤除、无快照原样返回。
    #[test]
    fn overlaid_with_engine_params_maps_fields() {
        let p = AudioParams::new(
            Some(352_800),
            Some(2),
            Some(32),
            "dts".into(),
            0,
            Some(12.5),
        );
        let out = TrackMetadata::default().overlaid_with_engine_params(Some(p));
        assert_eq!(out.codec.as_deref(), Some("dts"));
        assert_eq!(out.sample_rate, Some(352_800));
        assert_eq!(out.duration, Some(12.5));
        // 零时长不补（与 max 属性弹窗 >0.0 口径一致）
        let p = AudioParams::new(None, None, None, "x".into(), 0, Some(0.0));
        let out = TrackMetadata::default().overlaid_with_engine_params(Some(p));
        assert_eq!(out.duration, None);
        // 无快照：no-op
        let md = TrackMetadata {
            title: Some("t".into()),
            ..Default::default()
        };
        let out = md.overlaid_with_engine_params(None);
        assert_eq!(out.title.as_deref(), Some("t"));
        assert_eq!(out.codec, None);
    }

    /// 技术行回落：元数据缺失段用引擎实测值；码率保持元数据单源；
    /// 引擎也缺时维持原降级文案。
    #[test]
    fn tech_line_engine_fallback() {
        // FFmpeg 长尾兜底记录（全 None）→ 引擎实测值补齐
        let md = TrackMetadata::default();
        assert_eq!(
            md.tech_line_with_engine(Some("dts"), Some(96_000), Some(32), "未知格式"),
            "dts · 未知 · 96 kHz · 32 bit"
        );
        // 元数据优先：已有字段不被引擎值覆盖
        let md = TrackMetadata {
            codec: Some("FLAC".into()),
            bitrate: Some(900_000),
            sample_rate: Some(44_100),
            bits_per_sample: Some(16),
            ..Default::default()
        };
        assert_eq!(
            md.tech_line_with_engine(Some("dts"), Some(96_000), Some(32), "未知格式"),
            "FLAC · 900 kbps · 44.1 kHz · 16 bit"
        );
        // 双缺：维持原降级文案（有损 = 位深未知语义）
        let md = TrackMetadata::default();
        assert_eq!(
            md.tech_line_with_engine(None, None, None, "未知格式"),
            "未知格式 · 未知 · 未知 · 有损"
        );
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

    /// 造一个最小合法 RIFF WAV（44.1kHz / 16-bit / mono / 0.5s 静音），
    /// 无内嵌封面。供需要真实音频文件的测试复用。
    fn write_test_wav(path: &Path) {
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

        std::fs::write(path, &wav).unwrap();
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
        write_test_wav(&path);

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

    // —— 文件夹封面回退 ——

    /// 准备临时目录：一张无内嵌封面的 WAV + 可选的若干图片文件。
    /// 返回（目录, WAV 路径）；调用方负责清理目录。
    fn setup_cover_dir(
        tag: &str,
        images: &[(&str, &[u8])],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tuneux_cover_test_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("track.wav");
        write_test_wav(&wav);
        for (name, bytes) in images {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        (dir, wav)
    }

    /// 无内嵌封面时，同目录 cover.jpg 被回退读取，MIME 按扩展名推断。
    #[test]
    fn folder_cover_picks_cover_jpg() {
        let (dir, wav) = setup_cover_dir("basic", &[("cover.jpg", &[1, 2, 3])]);
        let md = TrackMetadata::from_file(&wav);
        let cover = md.cover.expect("应回退到 cover.jpg");
        assert_eq!(cover.mime, "image/jpeg");
        assert_eq!(cover.bytes, vec![1, 2, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 优先级：cover 先于 folder；jpg 先于 png。
    #[test]
    fn folder_cover_priority_order() {
        let (dir, wav) = setup_cover_dir(
            "priority",
            &[
                ("folder.png", &[9]),
                ("cover.jpg", &[1]),
                ("front.jpg", &[5]),
            ],
        );
        let md = TrackMetadata::from_file(&wav);
        let cover = md.cover.expect("应命中 cover.jpg");
        assert_eq!(cover.bytes, vec![1], "cover.jpg 应优先于 folder/front");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 大小写不敏感：Cover.JPG 同样命中。
    #[test]
    fn folder_cover_case_insensitive() {
        let (dir, wav) = setup_cover_dir("case", &[("Cover.JPG", &[7, 7])]);
        let md = TrackMetadata::from_file(&wav);
        let cover = md.cover.expect("Cover.JPG 应命中（大小写不敏感）");
        assert_eq!(cover.bytes, vec![7, 7]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目录下无约定图片 → 无封面（不回退、不报错）。
    #[test]
    fn folder_cover_none_when_absent() {
        let (dir, wav) = setup_cover_dir("absent", &[("photo.jpg", &[1])]);
        let md = TrackMetadata::from_file(&wav);
        assert!(md.cover.is_none(), "非约定文件名不应命中");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
