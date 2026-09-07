//! # 音频文件探测（读技术参数与标签，不解码）
//!
//! 打开音频文件、探测容器格式、读取技术参数（采样率 / 通道 / 位深 / 编码名 /
//! 音轨 ID / 时长）与标签（标题 / 艺人 / 专辑 / 曲号 / 歌词 / 封面）。
//! 与 [`super::decoder`] 的分工：本模块只"读"，不建立解码器、不产出样本。
//!
//! 供媒体层（tuneux-mediax）组装 `TrackMetadata` 使用；返回的 [`ProbeTags`]
//! 是核心自有结构，不泄露 symphonia 类型。

use std::path::Path;

use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTag, StandardVisualKey};

use super::decoder::{codec_name_or_ext, AudioParams, DecoderBackend};

/// 探测得到的原始标签（供媒体层组装 `TrackMetadata`，不泄露 symphonia 类型）。
#[derive(Debug, Default, Clone)]
pub struct ProbeTags {
    /// 曲目标题。
    pub title: Option<String>,
    /// 艺人（Artist 优先，缺失时退到 AlbumArtist）。
    pub artist: Option<String>,
    /// 专辑名。
    pub album: Option<String>,
    /// 曲目号（ID3 TRCK / Vorbis TRACKNUMBER，或 CUESHEET 索引）。
    pub track_number: Option<u32>,
    /// 内嵌歌词（多个标签取第一个非空）。
    pub lyrics: Option<String>,
    /// 封面（原始字节 + MIME 类型）。
    pub cover: Option<(Vec<u8>, String)>,
}

/// 探测音频文件：返回技术参数（含总时长，可能为 `None`）与原始标签。
///
/// 失败（文件打不开 / 无音频轨 / 格式不支持）返回 `None`。只读容器头与标签，
/// 不解码音频样本。
///
/// Opus / WavPack 由自建后端解码（symphonia 不识别这两种容器，probe 会失败）：
/// 分发到后端取技术参数（codec / 采样率 / 位深 / 声道），标签留空由媒体层
/// 从文件名等兜底；FFmpeg 长尾格式（ape/wma/tak 等）仍不支持、返回 `None`。
pub fn probe_metadata(path: &Path) -> Option<(AudioParams, ProbeTags)> {
    // symphonia 不识别 Opus 容器：开自建后端（只做头解析）拿技术参数。
    if super::opus::is_opus_path(path) {
        let params = super::opus::OpusBackend::open(path).ok()?.params().clone();
        return Some((params, ProbeTags::default()));
    }
    // symphonia 不识别 WavPack 容器：同 Opus 处理。
    if super::wavpack::is_wavpack_path(path) {
        let params = super::wavpack::WavPackBackend::open(path)
            .ok()?
            .params()
            .clone();
        return Some((params, ProbeTags::default()));
    }
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .ok()?;

    let track = reader.default_track(TrackType::Audio)?;
    let track_id = track.id;
    let audio_params = track.codec_params.as_ref().and_then(|cp| cp.audio())?;

    // 时长：Track.duration（time_base tick 数）+ time_base 换算为秒。
    let duration_secs = track.duration.and_then(|dur| {
        track.time_base.map(|tb| {
            // Duration 的内部 u64 是私有的，用 .get() 取；
            // Timestamp 没有 From<u64>，转 i64：时长永远非负，位转换安全。
            let time =
                tb.calc_time_saturating(symphonia::core::units::Timestamp::from(dur.get() as i64));
            time.as_secs_f64()
        })
    });

    let params = AudioParams::new(
        audio_params.sample_rate,
        audio_params.channels.as_ref().map(|c| c.count() as u16),
        audio_params.bits_per_sample,
        codec_name_or_ext(&audio_params.codec.to_string(), path),
        track_id,
        duration_secs,
    );

    // 标签：先把 Metadata 绑定到局部变量，避开"临时值 drop 后还在用"的借用问题。
    let metadata = reader.metadata();
    let mut tags = ProbeTags::default();
    if let Some(rev) = metadata.current() {
        fill_tags(&mut tags, rev);
        // 封面：symphonia 把 FLAC PICTURE / MP3 APIC 都归一为 Visual。
        // 按用途优先取 FrontCover（标准封面），退而 BackCover，
        // 再退到任意非空 visual——避免 FileIcon / Logo 之类的小图先被误取。
        tags.cover = rev
            .media
            .visuals
            .iter()
            .find(|v| !v.data.is_empty() && matches!(v.usage, Some(StandardVisualKey::FrontCover)))
            .or_else(|| {
                rev.media.visuals.iter().find(|v| {
                    !v.data.is_empty() && matches!(v.usage, Some(StandardVisualKey::BackCover))
                })
            })
            .or_else(|| rev.media.visuals.iter().find(|v| !v.data.is_empty()))
            .and_then(|v| v.media_type.clone().map(|m| (v.data.to_vec(), m)));
    }

    Some((params, tags))
}

/// 从 symphonia 的 `MetadataRevision` 填充标签字段。
fn fill_tags(tags: &mut ProbeTags, rev: &MetadataRevision) {
    for tag in &rev.media.tags {
        // 仅处理被识别的标准标签（has_std_tag）。
        if let Some(std) = &tag.std {
            match std {
                StandardTag::TrackTitle(name) => tags.title = Some((**name).clone()),
                StandardTag::Artist(name) => tags.artist = Some((**name).clone()),
                StandardTag::AlbumArtist(name) => {
                    // 优先用 Artist，缺失时退到 AlbumArtist。
                    if tags.artist.is_none() {
                        tags.artist = Some((**name).clone());
                    }
                }
                StandardTag::Album(name) => tags.album = Some((**name).clone()),
                // symphonia 0.6 把"CD 曲目索引"叫 CdTrackIndex(u8)，
                // 不是 CdTrackNumber；与 TrackNumber(u64) 互补。
                StandardTag::CdTrackIndex(n) => tags.track_number = Some(*n as u32),
                StandardTag::TrackNumber(n) => tags.track_number = Some(*n as u32),
                // 内嵌歌词：symphonia 已把各容器写法归一为 Lyrics 变体，
                // 多个取第一个非空。
                StandardTag::Lyrics(text) if tags.lyrics.is_none() && !text.is_empty() => {
                    tags.lyrics = Some((**text).clone());
                }
                _ => {}
            }
        } else {
            // 兜底：未被识别为标准标签的原始 Tag，按 key 大小写不敏感匹配歌词。
            let key = tag.raw.key.to_ascii_lowercase();
            if matches!(key.as_str(), "lyrics" | "unsyncedlyrics") {
                if let symphonia::core::meta::RawValue::String(val) = &tag.raw.value {
                    if tags.lyrics.is_none() && !val.is_empty() {
                        tags.lyrics = Some((**val).clone());
                    }
                }
            }
        }
    }
}
