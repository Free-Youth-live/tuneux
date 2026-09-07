//! CUE 分轨：读取整轨旁同名 `.cue`，转码、解析并展开为播放列表条目。
//!
//! 依赖 media（取整轨元数据），被 actions 引用。

use crate::playlist;
use encoding_rs;

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

impl App {
    /// 若整轨文件旁存在同名 `.cue`，解析并展开为 CUE 曲目条目。
    ///
    /// 无 `.cue` / 解析失败时返回空（调用方回退为普通条目）。
    /// 展开后的条目：path 同整轨文件、track_number = CUE 曲目号、
    /// cue = 起点/终点毫秒 + 标题 + 表演者，播放时从对应 INDEX 位置起播、
    /// 到达终点自动切下一曲。
    pub(crate) fn cue_items_for(&mut self, path: &std::path::Path) -> Vec<playlist::PlaylistItem> {
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
        let md = self.get_or_extract_metadata(path);
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
}
