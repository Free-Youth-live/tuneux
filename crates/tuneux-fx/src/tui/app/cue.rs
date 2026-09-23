//! CUE 分轨：解析 `.cue` 并展开为播放列表条目。
//!
//! 两个入口：整轨音频文件旁同名 `.cue`（老路径）；`.cue` 文件本身
//!（按 FILE 字段引用真实音频，cue+bin / cue+flac 镜像场景）。
//! 数据轨（MODE1/MODE2 等）跳过并计数。依赖 media（取整轨元数据），
//! 被 actions 引用。

use crate::playlist;
use tuneux_corex::cue::{decode_cue_bytes, parse_cue_sheet};
use tuneux_mediax::metadata;

use super::App;

/// 展开结果：分轨条目 + 跳过的数据轨数（供 UI 提示）。
type CueExpansion = (Vec<playlist::PlaylistItem>, usize);

/// 把解析好的 CueSheet 展开为分轨条目（纯函数）。数据轨跳过并计数。
///
/// 条目：path = 实际音频文件（整轨或镜像）、track_number = CUE 曲目号、
/// cue = 起点/终点毫秒 + 标题 + 表演者；播放时按区间起播，终点引擎判定。
fn expand_sheet(
    sheet: &tuneux_corex::cue::CueSheet,
    audio_path: &std::path::Path,
    md: &metadata::TrackMetadata,
) -> CueExpansion {
    let track_duration_ms = md.duration.map(|d| (d * 1000.0) as u64);
    let mut skipped = 0usize;
    let items = sheet
        .tracks
        .iter()
        .filter(|t| {
            // 数据轨（MODE1/MODE2 等）不可播：跳过并计数，不列进播放列表。
            if t.is_audio {
                true
            } else {
                skipped += 1;
                false
            }
        })
        .map(|t| {
            // 终点 = 解析期已推算（下一曲 INDEX 01 起点）；末曲/缺省时
            // 兜底整轨总时长（仍未知则 None，播到文件尾）。
            let end_ms = t
                .end_secs
                .map(|e| (e * 1000.0) as u64)
                .or(track_duration_ms);
            playlist::PlaylistItem {
                path: audio_path.to_path_buf(),
                album: md.album.clone(),
                track_number: Some(t.index),
                cue: Some(playlist::CueRef {
                    index: t.index,
                    title: t.title.clone(),
                    performer: t.performer.clone(),
                    start_ms: (t.start_secs * 1000.0) as u64,
                    end_ms,
                }),
            }
        })
        .collect();
    (items, skipped)
}

/// 读 + 解析 .cue 文件（编码链 UTF-8 → BOM → GB18030，corex 提供）。
fn read_cue_sheet(cue_path: &std::path::Path) -> Option<tuneux_corex::cue::CueSheet> {
    let bytes = std::fs::read(cue_path).ok()?;
    let content = decode_cue_bytes(&bytes);
    parse_cue_sheet(&content).ok()
}

/// FILE 字段解析为实际音频路径：cue 所在目录 + FILE 名；文件不存在返回 None。
fn resolve_audio_path(
    cue_path: &std::path::Path,
    sheet: &tuneux_corex::cue::CueSheet,
) -> Option<std::path::PathBuf> {
    let name = sheet.audio_file.as_deref()?;
    let dir = cue_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let p = dir.join(name);
    p.is_file().then_some(p)
}

/// 老入口：整轨音频文件 + 同名 `.cue`。返回空 = 调用方回退普通条目。
///
/// FILE 字段优先：cue 是索引权威——FILE 指向别的文件（如镜像 .bin）时
/// 以 FILE 为准并重新探测其元数据；FILE 缺失 / 指向本路径 / 文件不存在
/// 时回落传入的整轨路径（沿用调用方已探测的元数据，缓存语义不破）。
fn cue_items_from(path: &std::path::Path, md: &metadata::TrackMetadata) -> CueExpansion {
    let cue_path = path.with_extension("cue");
    if !cue_path.is_file() {
        return (Vec::new(), 0);
    }
    let Some(sheet) = read_cue_sheet(&cue_path) else {
        return (Vec::new(), 0);
    };
    match resolve_audio_path(&cue_path, &sheet) {
        Some(p) if p.as_path() != path => {
            let md2 = metadata::TrackMetadata::from_file(&p);
            expand_sheet(&sheet, &p, &md2)
        }
        _ => expand_sheet(&sheet, path, md),
    }
}

/// 新入口：`.cue` 文件本身被点中 → 按 FILE 引用展开为分轨条目。
///
/// 返回（条目, 跳过数据轨数, 实际音频路径）；cue 解析失败 / FILE 缺失 /
/// 音频文件不存在 / 全是数据轨 → None（调用方提示用户）。
pub(crate) fn cue_items_from_cue_file(
    cue_path: &std::path::Path,
) -> Option<(Vec<playlist::PlaylistItem>, usize, std::path::PathBuf)> {
    let sheet = read_cue_sheet(cue_path)?;
    let audio_path = resolve_audio_path(cue_path, &sheet)?;
    let md = metadata::TrackMetadata::from_file(&audio_path);
    let (items, skipped) = expand_sheet(&sheet, &audio_path, &md);
    if items.is_empty() {
        return None;
    }
    Some((items, skipped, audio_path))
}

/// 把一批已收集的音乐文件路径构建为播放列表条目：整轨 + 同名 `.cue`
/// 展开为分轨，其余按普通条目；元数据**缓存优先**（未命中才探测文件）
/// ——与 App::get_or_extract_metadata 同口径，仅返回新探测的元数据
/// （封面字节已剥离）供主线程合入缓存。纯函数（只读磁盘、无 App 状态），
/// 可在后台线程运行——`a` 键加目录 / open-dir 的递归收集、逐文件探测
/// 都在这里完成，UI 线程只做结果合入（大目录加入不再卡界面）。
/// 第三元返回值 = 本批跳过的数据轨总数（供 UI 一次性提示）。
#[allow(clippy::type_complexity)]
pub(crate) fn build_items_for_paths(
    paths: &[std::path::PathBuf],
    cache: &std::collections::HashMap<std::path::PathBuf, metadata::TrackMetadata>,
) -> (
    Vec<playlist::PlaylistItem>,
    Vec<(std::path::PathBuf, metadata::TrackMetadata)>,
    usize,
) {
    let mut items = Vec::new();
    let mut probed = Vec::new();
    let mut skipped_total = 0usize;
    for path in paths {
        // 缓存优先：命中免探测（与同步路径的 get_or_extract_metadata 语义一致）。
        let (md, from_probe) = match cache.get(path) {
            Some(cached) => (cached.clone(), false),
            None => (metadata::TrackMetadata::from_file(path), true),
        };
        let (expanded, skipped) = cue_items_from(path, &md);
        skipped_total += skipped;
        if !expanded.is_empty() {
            items.extend(expanded);
        } else {
            items.push(playlist::PlaylistItem {
                path: path.clone(),
                album: md.album.clone(),
                track_number: md.track_number,
                cue: None,
            });
        }
        // 只回传新探测的元数据（缓存命中项无需重写）；封面字节不入缓存。
        if from_probe {
            let mut cached = md;
            cached.cover = None;
            probed.push((path.clone(), cached));
        }
    }
    (items, probed, skipped_total)
}

impl App {
    /// 若整轨文件旁存在同名 `.cue`，解析并展开为 CUE 曲目条目。
    ///
    /// 无 `.cue` / 解析失败时返回空（调用方回退为普通条目）。
    /// 元数据走缓存（单文件路径：浏览器 Enter / a 键文件分支等同步场景）；
    /// 批量后台构建见 [`build_items_for_paths`]。返回含跳过数据轨数。
    pub(crate) fn cue_items_for(&mut self, path: &std::path::Path) -> CueExpansion {
        let md = self.get_or_extract_metadata(path);
        cue_items_from(path, &md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    /// 临时目录 + 合成 .bin（2 扇区已知 PCM）+ 指定内容的 .cue。
    /// 返回（临时目录, bin 路径, cue 路径）。
    fn make_fixture(
        cue_content: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("tuneux-cue-test-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        // 2 扇区 2352 字节/扇的合法 .bin
        let bin = dir.join("CDImage.bin");
        let mut f = std::fs::File::create(&bin).expect("建 bin");
        f.write_all(&vec![0u8; 2 * 2352]).expect("写 bin");
        drop(f);
        let cue = dir.join("album.cue");
        let mut f = std::fs::File::create(&cue).expect("建 cue");
        f.write_all(cue_content.as_bytes()).expect("写 cue");
        (dir, bin, cue)
    }

    /// FILE 指向另一个文件（.bin 镜像）：条目以 FILE 为准。
    #[test]
    fn file_field_points_to_bin() {
        let (dir, bin, _cue) = make_fixture(
            "FILE \"CDImage.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        );
        // 入口：点一个与 cue 同名的整轨 flac（内容无关——FILE 权威）。
        let flac = dir.join("album.flac");
        std::fs::write(&flac, b"not really audio").unwrap();
        let md = metadata::TrackMetadata::from_file(&flac);
        let (items, skipped) = cue_items_from(&flac, &md);
        assert_eq!(skipped, 0);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, bin, "条目路径应以 cue 的 FILE 为准");
        let cue0 = items[0].cue.as_ref().unwrap();
        assert_eq!(cue0.start_ms, 0);
        assert_eq!(cue0.end_ms, Some(60_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 数据轨（MODE1）跳过并计数。
    #[test]
    fn data_tracks_skipped_and_counted() {
        let (dir, _bin, _cue) = make_fixture(
            "FILE \"CDImage.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        );
        let flac = dir.join("album.flac");
        std::fs::write(&flac, b"x").unwrap();
        let md = metadata::TrackMetadata::from_file(&flac);
        let (items, skipped) = cue_items_from(&flac, &md);
        assert_eq!(skipped, 1, "数据轨应计入跳过数");
        assert_eq!(items.len(), 1, "数据轨不进播放列表");
        assert_eq!(items[0].cue.as_ref().unwrap().index, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// .cue 文件直接展开：成功 / 缺 FILE / FILE 指向不存在文件。
    #[test]
    fn cue_file_entry_paths() {
        let (dir, bin, cue) =
            make_fixture("FILE \"CDImage.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n");
        let Some((items, skipped, audio_path)) = cue_items_from_cue_file(&cue) else {
            panic!("应展开成功");
        };
        assert_eq!(skipped, 0);
        assert_eq!(items.len(), 1);
        assert_eq!(audio_path, bin);
        // cue 无 FILE 字段 → None。
        let no_file_cue = dir.join("nofile.cue");
        std::fs::write(&no_file_cue, "TRACK 01 AUDIO\n  INDEX 01 00:00:00\n").unwrap();
        assert!(cue_items_from_cue_file(&no_file_cue).is_none());
        // FILE 指向不存在 → None。
        let missing_cue = dir.join("missing.cue");
        std::fs::write(
            &missing_cue,
            "FILE \"nope.wav\" WAVE\n  TRACK 01 AUDIO\n  INDEX 01 00:00:00\n",
        )
        .unwrap();
        assert!(cue_items_from_cue_file(&missing_cue).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 老行为回落：FILE 与被点整轨同名 → 沿用传入元数据与路径。
    #[test]
    fn fallback_same_file_keeps_legacy_behavior() {
        let (dir, _bin, _cue) = make_fixture(
            "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"老路径\"\n    INDEX 01 00:00:00\n",
        );
        let flac = dir.join("album.flac");
        std::fs::write(&flac, b"x").unwrap();
        let md = metadata::TrackMetadata::from_file(&flac);
        let (items, _) = cue_items_from(&flac, &md);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, flac, "同名时沿用被点文件路径");
        assert_eq!(items[0].cue.as_ref().unwrap().title, "老路径");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无同名 cue → 空（调用方回退普通条目）。
    #[test]
    fn no_cue_returns_empty() {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let lonely = std::env::temp_dir().join(format!(
            "tuneux-cue-lonely-{}-{seq}.flac",
            std::process::id()
        ));
        std::fs::write(&lonely, b"x").unwrap();
        let md = metadata::TrackMetadata::from_file(&lonely);
        let (items, skipped) = cue_items_from(&lonely, &md);
        assert!(items.is_empty() && skipped == 0);
        let _ = std::fs::remove_file(&lonely);
    }
}
