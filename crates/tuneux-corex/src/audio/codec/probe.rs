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
use symphonia::core::meta::{
    MetadataOptions, MetadataRevision, StandardTag, StandardVisualKey, Visual,
};

use super::decoder::{bits_per_sample_with_alac, codec_name_resolved, AudioParams};

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
    // 自建后端（opus / wavpack / cdda）经注册表取技术参数，标签留空由媒体层兜底；
    // flac / wav / mp3 等走下方 symphonia 全标签探测；ffmpeg 长尾返回 None。
    if let Some(params) = super::decoder::probe_params(path) {
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
        bits_per_sample_with_alac(audio_params),
        codec_name_resolved(audio_params.codec, path),
        track_id,
        duration_secs,
    );

    // 标签：先把 Metadata 绑定到局部变量，避开"临时值 drop 后还在用"的借用问题。
    let metadata = reader.metadata();
    let mut tags = ProbeTags::default();
    if let Some(rev) = metadata.current() {
        fill_tags(&mut tags, rev);
        tags.cover = select_cover(&rev.media.visuals);
    }

    Some((params, tags))
}

/// 封面三级回退（FrontCover → BackCover → 任意）：每一级都要求 visual
/// 既有非空 data 又有 media_type——缺 MIME 的候选不再拦住整条回退链
/// （旧缺陷：先选出 visual 再取 media_type，Front 有数据无 MIME 时整链
/// 得 None，带 MIME 的 Back 不被尝试）。
fn select_cover(visuals: &[Visual]) -> Option<(Vec<u8>, String)> {
    let usable = |v: &Visual| {
        if v.data.is_empty() {
            return None;
        }
        v.media_type.as_ref().map(|m| (v.data.to_vec(), m.clone()))
    };
    visuals
        .iter()
        .find(|v| matches!(v.usage, Some(StandardVisualKey::FrontCover)))
        .and_then(usable)
        .or_else(|| {
            visuals
                .iter()
                .find(|v| matches!(v.usage, Some(StandardVisualKey::BackCover)))
                .and_then(usable)
        })
        .or_else(|| {
            visuals
                .iter()
                .find(|v| !v.data.is_empty() && v.media_type.is_some())
                .and_then(usable)
        })
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

#[cfg(test)]
mod tests {
    //! 封面三级回退选择的单元测试（直接构造 symphonia Visual，
    //! 不依赖真实媒体文件）。
    use super::*;

    /// 构造一个测试用 Visual。
    fn visual(data: &[u8], media_type: Option<&str>, usage: Option<StandardVisualKey>) -> Visual {
        Visual {
            media_type: media_type.map(str::to_string),
            dimensions: None,
            color_mode: None,
            usage,
            tags: Vec::new(),
            data: data.into(),
        }
    }

    /// 主缺陷回归：FrontCover 有数据但缺 MIME，BackCover 有数据带 MIME——
    /// 旧实现先选 visual 再取 media_type，整链得 None；修复后回退选中
    /// BackCover。
    #[test]
    fn cover_fallback_skips_front_without_mime() {
        let visuals = vec![
            visual(&[1, 2, 3], None, Some(StandardVisualKey::FrontCover)),
            visual(
                &[4, 5, 6],
                Some("image/jpeg"),
                Some(StandardVisualKey::BackCover),
            ),
        ];
        let cover = select_cover(&visuals).expect("应回退选中 BackCover");
        assert_eq!(cover.0, vec![4, 5, 6]);
        assert_eq!(cover.1, "image/jpeg");
    }

    /// Front 有数据带 MIME：优先 Front，不回退。
    #[test]
    fn cover_prefers_front_with_mime() {
        let visuals = vec![
            visual(&[1], Some("image/png"), Some(StandardVisualKey::FrontCover)),
            visual(&[2], Some("image/jpeg"), Some(StandardVisualKey::BackCover)),
        ];
        let cover = select_cover(&visuals).expect("应选中 FrontCover");
        assert_eq!(cover.0, vec![1]);
        assert_eq!(cover.1, "image/png");
    }

    /// Front 缺 MIME、Back 空数据带 MIME、第三个无用途但有数据带 MIME：
    /// 回退链逐级下探，最终落到任意级。
    #[test]
    fn cover_fallback_reaches_any_tier() {
        let visuals = vec![
            visual(&[1], None, Some(StandardVisualKey::FrontCover)),
            visual(&[], Some("image/jpeg"), Some(StandardVisualKey::BackCover)),
            visual(&[9], Some("image/png"), None),
        ];
        let cover = select_cover(&visuals).expect("应落到任意级");
        assert_eq!(cover.0, vec![9]);
        assert_eq!(cover.1, "image/png");
    }

    /// 全部候选都缺 MIME：None。
    #[test]
    fn cover_none_when_all_missing_mime() {
        let visuals = vec![
            visual(&[1], None, Some(StandardVisualKey::FrontCover)),
            visual(&[2], None, None),
        ];
        assert!(select_cover(&visuals).is_none());
    }
}
