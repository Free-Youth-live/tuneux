//! # 播放列表显示行（分组 / 平铺呈现模型）
//!
//! 镜像 fx 的呈现语义：以 mediax `PlaylistCore` 的**显示序**（专辑-曲序）
//! 为基准构建行——按专辑视图每专辑一个组头 + 该组曲目，平铺视图全曲目。
//! 无专辑名以**空串哨兵**归组（渲染层译「未知专辑」，折叠状态存哨兵值，
//! 切换语言不漂移）。本模块只做纯函数行构建 / 折叠 / 序号，不持 UI 状态。

use std::collections::{HashMap, HashSet};

use tuneux_mediax::{PlaylistCore, PlaylistView};

/// 播放列表的一条显示行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistRow {
    /// 专辑组头（可折叠）；`album` 为空串表示「未知专辑」哨兵。
    AlbumHeader {
        /// 专辑名（哨兵空串 = 无专辑）。
        album: String,
        /// 该专辑曲目索引（按显示序排列）。
        track_indices: Vec<usize>,
    },
    /// 曲目行（插入序索引）。
    Track {
        /// 条目索引。
        item_index: usize,
    },
}

/// 折叠前的显示行：ByAlbum = 组头 + 组内曲目（按显示序连续归组），
/// Flat = 显示序全曲目。
pub fn display_rows(core: &PlaylistCore, view: PlaylistView) -> Vec<PlaylistRow> {
    match view {
        PlaylistView::Flat => core
            .display_order()
            .into_iter()
            .map(|item_index| PlaylistRow::Track { item_index })
            .collect(),
        PlaylistView::ByAlbum => {
            let mut rows = Vec::new();
            let mut current_album: Option<String> = None;
            let mut current_tracks: Vec<usize> = Vec::new();
            for idx in core.display_order() {
                let album = core.items()[idx].album.clone().unwrap_or_default();
                if current_album.as_deref() != Some(album.as_str()) {
                    if let Some(a) = current_album.take() {
                        let tracks = std::mem::take(&mut current_tracks);
                        rows.push(PlaylistRow::AlbumHeader {
                            album: a,
                            track_indices: tracks.clone(),
                        });
                        for t in tracks {
                            rows.push(PlaylistRow::Track { item_index: t });
                        }
                    }
                    current_album = Some(album);
                }
                current_tracks.push(idx);
            }
            if let Some(a) = current_album {
                let tracks = std::mem::take(&mut current_tracks);
                rows.push(PlaylistRow::AlbumHeader {
                    album: a,
                    track_indices: tracks.clone(),
                });
                for t in tracks {
                    rows.push(PlaylistRow::Track { item_index: t });
                }
            }
            rows
        }
    }
}

/// 折叠后的可见行：折叠专辑只保留组头，其余行原样；平铺行无组头不受影响。
pub fn visible_rows(rows: Vec<PlaylistRow>, collapsed: &HashSet<String>) -> Vec<PlaylistRow> {
    let mut out = Vec::new();
    let mut skip = false;
    for row in rows {
        match row {
            PlaylistRow::AlbumHeader {
                album,
                track_indices,
            } => {
                skip = collapsed.contains(&album);
                out.push(PlaylistRow::AlbumHeader {
                    album,
                    track_indices,
                });
            }
            PlaylistRow::Track { .. } => {
                if !skip {
                    out.push(row);
                }
            }
        }
    }
    out
}

/// 组头预扫描：item_index → **专辑内序号 1..N**（每专辑从头计，
/// 不是 metadata 曲号，是专辑内顺序号）。
pub fn album_track_seq(rows: &[PlaylistRow]) -> HashMap<usize, usize> {
    let mut map = HashMap::new();
    for row in rows {
        if let PlaylistRow::AlbumHeader { track_indices, .. } = row {
            for (pos, &idx) in track_indices.iter().enumerate() {
                map.insert(idx, pos + 1);
            }
        }
    }
    map
}

/// 曲目序号口径：分组 = 专辑内 1..N；平铺 = 显示序全局大排行 1..N。
/// 序号与过滤 / 折叠无关（恒按完整显示行预扫描），过滤态仍读全局序号。
pub fn track_sequences(core: &PlaylistCore, view: PlaylistView) -> HashMap<usize, usize> {
    match view {
        PlaylistView::Flat => core
            .display_order()
            .into_iter()
            .enumerate()
            .map(|(pos, idx)| (idx, pos + 1))
            .collect(),
        PlaylistView::ByAlbum => album_track_seq(&display_rows(core, view)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuneux_mediax::PlaylistItem;

    fn item(album: Option<&str>, name: &str) -> PlaylistItem {
        PlaylistItem {
            path: std::path::PathBuf::from(format!("/music/{name}.flac")),
            album: album.map(|a| a.to_string()),
            track_number: None,
            cue: None,
        }
    }

    fn core_with(items: Vec<PlaylistItem>) -> PlaylistCore {
        let mut core = PlaylistCore::new_random();
        for it in items {
            core.add(it);
        }
        core
    }

    #[test]
    fn by_album_groups_headers_and_tracks_in_display_order() {
        let core = core_with(vec![
            item(Some("B"), "b1"),
            item(Some("A"), "a1"),
            item(Some("A"), "a2"),
            item(None, "x1"),
        ]);
        let rows = display_rows(&core, PlaylistView::ByAlbum);
        // 显示序按专辑-曲序：空专辑哨兵排最前还是最后取决于 compare_items；
        // 这里只断言结构：组头数 = 专辑数（含哨兵），曲目行总数 = 条目数。
        let headers: Vec<&String> = rows
            .iter()
            .filter_map(|r| match r {
                PlaylistRow::AlbumHeader { album, .. } => Some(album),
                _ => None,
            })
            .collect();
        assert_eq!(headers.len(), 3, "两个具名专辑 + 一个空名哨兵");
        assert!(headers.iter().any(|a| a.is_empty()), "空专辑名以哨兵归组");
        let tracks = rows
            .iter()
            .filter(|r| matches!(r, PlaylistRow::Track { .. }))
            .count();
        assert_eq!(tracks, 4);
        // 每个组头后紧随其曲目行。
        for (i, row) in rows.iter().enumerate() {
            if let PlaylistRow::AlbumHeader { track_indices, .. } = row {
                for (k, &idx) in track_indices.iter().enumerate() {
                    assert_eq!(rows[i + 1 + k], PlaylistRow::Track { item_index: idx });
                }
            }
        }
    }

    #[test]
    fn flat_rows_are_display_order_tracks_only() {
        let core = core_with(vec![item(Some("A"), "a1"), item(Some("B"), "b1")]);
        let rows = display_rows(&core, PlaylistView::Flat);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| matches!(r, PlaylistRow::Track { .. })));
        let order = core.display_order();
        for (pos, row) in rows.iter().enumerate() {
            assert_eq!(
                row,
                &PlaylistRow::Track {
                    item_index: order[pos]
                }
            );
        }
    }

    #[test]
    fn album_seq_restarts_per_album_not_metadata() {
        let core = core_with(vec![
            item(Some("A"), "a1"),
            item(Some("A"), "a2"),
            item(Some("B"), "b1"),
        ]);
        let rows = display_rows(&core, PlaylistView::ByAlbum);
        let seq = album_track_seq(&rows);
        // 每专辑从头计 1..N。
        let a: Vec<usize> = core
            .display_order()
            .into_iter()
            .filter(|&i| core.items()[i].album.as_deref() == Some("A"))
            .map(|i| seq[&i])
            .collect();
        assert_eq!(a, vec![1, 2]);
        let b: Vec<usize> = core
            .display_order()
            .into_iter()
            .filter(|&i| core.items()[i].album.as_deref() == Some("B"))
            .map(|i| seq[&i])
            .collect();
        assert_eq!(b, vec![1], "新专辑序号从头计");
    }

    #[test]
    fn flat_seq_is_global_rank() {
        let core = core_with(vec![
            item(Some("A"), "a1"),
            item(Some("B"), "b1"),
            item(Some("A"), "a2"),
        ]);
        let seq = track_sequences(&core, PlaylistView::Flat);
        let order = core.display_order();
        for (pos, idx) in order.iter().enumerate() {
            assert_eq!(seq[idx], pos + 1, "平铺 = 全局大排行");
        }
        let grouped = track_sequences(&core, PlaylistView::ByAlbum);
        assert_ne!(seq, grouped, "分组与平铺序号口径不同");
    }

    #[test]
    fn collapsed_album_keeps_header_only() {
        let core = core_with(vec![item(Some("A"), "a1"), item(Some("B"), "b1")]);
        let rows = display_rows(&core, PlaylistView::ByAlbum);
        let mut collapsed = HashSet::new();
        collapsed.insert("A".to_string());
        let visible = visible_rows(rows, &collapsed);
        // A 组只剩组头，B 组组头 + 曲目保留。
        let a_tracks = visible
            .iter()
            .filter(|r| matches!(r, PlaylistRow::Track { .. }))
            .count();
        assert_eq!(a_tracks, 1, "折叠专辑曲目隐藏");
        assert_eq!(
            visible
                .iter()
                .filter(|r| matches!(r, PlaylistRow::AlbumHeader { .. }))
                .count(),
            2,
            "组头均保留"
        );
        // 空哨兵折叠同样生效。
        let core2 = core_with(vec![item(None, "x1")]);
        let rows2 = display_rows(&core2, PlaylistView::ByAlbum);
        let mut collapsed2 = HashSet::new();
        collapsed2.insert(String::new());
        assert_eq!(visible_rows(rows2, &collapsed2).len(), 1);
    }
}
