//! CUE 分轨：读取整轨旁同名 `.cue`，转码、解析并展开为播放列表条目。
//!
//! 依赖 media（取整轨元数据），被 actions 引用。

use crate::playlist;
use encoding_rs;
use tuneux_mediax::metadata;

use super::App;

/// 将 `.cue` 文件字节解码为 UTF-8 文本。
///
/// 解码顺序：UTF-8（最快路径）→ BOM 探测（UTF-16LE/BE）→ GB18030
/// （中文 Windows 生成的 `.cue` 常见编码，GBK 为其子集）。
fn decode_cue_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode(&bytes[bom_len..]).0.into_owned();
    }
    encoding_rs::GB18030.decode(bytes).0.into_owned()
}

/// 用已取得的元数据把整轨展开为 CUE 分轨条目（纯函数，无 App 状态）。
///
/// 无同名 `.cue` / 读取或解析失败返回空（调用方回退普通条目）。
/// 展开后的条目：path 同整轨文件、track_number = CUE 曲目号、
/// cue = 起点/终点毫秒 + 标题 + 表演者，播放时从对应 INDEX 位置起播、
/// 到达终点自动切下一曲。
fn cue_items_from(
    path: &std::path::Path,
    md: &metadata::TrackMetadata,
) -> Vec<playlist::PlaylistItem> {
    let cue_path = path.with_extension("cue");
    if !cue_path.is_file() {
        return Vec::new();
    }
    // 读取字节并转码：UTF-8 优先，BOM（UTF-16）与 GB18030（中文 Windows .cue 常见）
    let bytes = match std::fs::read(&cue_path) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let content = decode_cue_bytes(&bytes);
    let tracks = match tuneux_corex::cue::parse_cue(&content) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let track_duration_ms = md.duration.map(|d| (d * 1000.0) as u64);
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            // 终点 = 下一曲 INDEX 起点；末曲 = 整轨总时长（未知则 None，播到文件尾）
            let end_ms = if i + 1 < tracks.len() {
                Some((tracks[i + 1].start_secs * 1000.0) as u64)
            } else {
                track_duration_ms
            };
            playlist::PlaylistItem {
                path: path.to_path_buf(),
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
        .collect()
}

/// 把一批已收集的音乐文件路径构建为播放列表条目：整轨 + 同名 `.cue`
/// 展开为分轨，其余按普通条目；元数据**缓存优先**（未命中才探测文件）
/// ——与 App::get_or_extract_metadata 同口径，仅返回新探测的元数据
/// （封面字节已剥离）供主线程合入缓存。纯函数（只读磁盘、无 App 状态），
/// 可在后台线程运行——`a` 键加目录 / open-dir 的递归收集、逐文件探测
/// 都在这里完成，UI 线程只做结果合入（大目录加入不再卡界面）。
#[allow(clippy::type_complexity)]
pub(crate) fn build_items_for_paths(
    paths: &[std::path::PathBuf],
    cache: &std::collections::HashMap<std::path::PathBuf, metadata::TrackMetadata>,
) -> (
    Vec<playlist::PlaylistItem>,
    Vec<(std::path::PathBuf, metadata::TrackMetadata)>,
) {
    let mut items = Vec::new();
    let mut probed = Vec::new();
    for path in paths {
        // 缓存优先：命中免探测（与同步路径的 get_or_extract_metadata 语义一致）。
        let (md, from_probe) = match cache.get(path) {
            Some(cached) => (cached.clone(), false),
            None => (metadata::TrackMetadata::from_file(path), true),
        };
        let expanded = cue_items_from(path, &md);
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
    (items, probed)
}

impl App {
    /// 若整轨文件旁存在同名 `.cue`，解析并展开为 CUE 曲目条目。
    ///
    /// 无 `.cue` / 解析失败时返回空（调用方回退为普通条目）。
    /// 元数据走缓存（单文件路径：浏览器 Enter / a 键文件分支等同步场景）；
    /// 批量后台构建见 [`build_items_for_paths`]。
    pub(crate) fn cue_items_for(&mut self, path: &std::path::Path) -> Vec<playlist::PlaylistItem> {
        let md = self.get_or_extract_metadata(path);
        cue_items_from(path, &md)
    }
}
