//! CUE 分轨展开（fx 接线）。
//!
//! 解析与展开已下沉 `tuneux-mediax`；本模块只保留依赖 App 元数据缓存的入口。

use tuneux_mediax::cue::cue_items_from;

use super::App;

/// 展开结果：分轨条目 + 跳过的数据轨数（供 UI 提示）。
pub(crate) type CueExpansion = tuneux_mediax::cue::CueExpansion;

// 供 mod.rs 使用（点中 .cue 文件展开；批量后台构建已由共享扫描池接管）。
pub(crate) use tuneux_mediax::cue::cue_items_from_cue_file;

/// 按「path + 第一轨起点」在播放列表定位 CUE 第一分轨。
///
/// 部分去重命中时新加入的分轨追加在已有分轨之后，「首个新加入分轨」
/// 的索引不等于第一轨位置——直接按起点定位第一轨；定位失败返回 None
/// （调用方提示，不再静默回退第 0 首）。
pub(crate) fn cue_first_track_index(
    items: &[tuneux_mediax::PlaylistItem],
    first_start_ms: Option<u64>,
    audio_path: &std::path::Path,
) -> Option<usize> {
    items.iter().position(|it| {
        it.path == audio_path
            && first_start_ms.is_some_and(|ms| it.cue.as_ref().map(|c| c.start_ms) == Some(ms))
    })
}

impl App {
    /// 若整轨文件旁存在同名 `.cue`，解析并展开为 CUE 曲目条目。
    ///
    /// 无 `.cue` / 解析失败时返回空（调用方回退为普通条目）。
    /// 元数据走缓存（单文件路径：浏览器 Enter / a 键文件分支等同步场景）。
    /// 批量后台构建已由共享扫描池接管（tuneux_mediax::scan_pool）。
    pub(crate) fn cue_items_for(&mut self, path: &std::path::Path) -> CueExpansion {
        let md = self.get_or_extract_metadata(path);
        cue_items_from(path, &md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue_item(path: &str, index: u32, start_ms: u64) -> tuneux_mediax::PlaylistItem {
        tuneux_mediax::PlaylistItem {
            path: std::path::PathBuf::from(path),
            album: None,
            track_number: None,
            cue: Some(tuneux_mediax::CueRef {
                index,
                title: format!("Track {index}"),
                performer: None,
                start_ms,
                end_ms: None,
            }),
        }
    }

    /// 部分去重命中——列表已有第一轨（起点 0），本次只追加了
    /// 第二、三轨；定位必须返回第一轨（索引 0），而非首个新加入分轨（1）。
    #[test]
    fn cue_first_track_found_after_partial_dedup() {
        let items = [
            cue_item("/m/album.flac", 1, 0),
            cue_item("/m/album.flac", 2, 60_000),
            cue_item("/m/album.flac", 3, 120_000),
        ];
        assert_eq!(
            cue_first_track_index(&items, Some(0), std::path::Path::new("/m/album.flac")),
            Some(0),
            "应按第一轨起点定位，而不是追加位置"
        );
    }

    /// 定位失败返回 None（第一轨起点已漂移或路径不一致），
    /// 调用方提示而非回退第 0 首。
    #[test]
    fn cue_first_track_miss_returns_none() {
        let items = [
            cue_item("/m/album.flac", 1, 5_000),
            cue_item("/m/album.flac", 2, 65_000),
        ];
        assert_eq!(
            cue_first_track_index(&items, Some(0), std::path::Path::new("/m/album.flac")),
            None,
            "第一轨起点不一致应定位失败"
        );
        // 路径过滤：另一张碟的同起点分轨不算命中。
        let other = [cue_item("/m/other.flac", 1, 0)];
        assert_eq!(
            cue_first_track_index(&other, Some(0), std::path::Path::new("/m/album.flac")),
            None,
            "只匹配同路径条目"
        );
    }
}
