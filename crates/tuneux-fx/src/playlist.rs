//! # 播放列表（产品层：导航核心 + UI 呈现状态）
//!
//! 播放导航核心（items/current/history/shuffle/显示序/next/prev/去重）已
//! 下沉 `tuneux-mediax` 的 [`PlaylistCore`]；本模块保留 **UI 呈现状态**：
//! 选中对象（`Selection`）、滚动偏移、显示视图、专辑折叠、显示行构造。
//! 这些都是 ratatui 行渲染 / 面板焦点的产物，max 的 egui 另写。
//!
//! ## 排序 vs 显示序
//!
//! `items` 按**插入序**存储（播放序），`display_order()` 返回**显示序**
//! （按专辑-曲序排序的 indices，专为 TUI 渲染）。导航（next/prev）与
//! 显示行（display_rows/visible_rows）均以显示序为基准。

use std::collections::HashSet;

use tuneux_mediax::{PlaylistCore, PlaylistView, RepeatMode};

// 播放列表条目 / CUE 分轨引用 / 导航结果 / 分轨区间钳制：领域类型在 mediax。
pub use tuneux_mediax::{clamp_seek_to_cue, CueRef, NavOutcome, PlaylistItem};

/// 播放列表面板的选中对象。
///
/// 平铺（Flat）视图只可能选中曲目；按专辑（ByAlbum）视图还可以选中
/// 组头（用于折叠/展开）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// 选中曲目（item index）。
    Track(usize),
    /// 选中专辑组头（专辑名）。
    Album(String),
}

/// 播放列表的一条显示行。
///
/// 渲染层遍历这个列表绘制，移动选中、滚动偏移都以它为基准。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistRow {
    /// 专辑组头（可折叠）。
    AlbumHeader {
        /// 专辑名（无专辑的曲目归入"未知专辑"）。
        album: String,
        /// 该专辑包含的曲目索引（按曲序排列）。
        track_indices: Vec<usize>,
    },
    /// 曲目。
    Track {
        /// 条目索引。
        item_index: usize,
    },
}

/// 焦点面板（用于 Tab 切换）。当前只有浏览器和播放列表两个面板，
/// 元数据面板只读不参与焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    Browser,
    Playlist,
}

/// 播放列表（导航核心 + UI 呈现状态）。
#[derive(Debug, Clone)]
pub struct Playlist {
    /// 导航核心（领域层）：条目、current、history、shuffle、显示序。
    core: PlaylistCore,
    /// 面板选中对象（曲目或专辑组头）。
    selected: Option<Selection>,
    /// 显示行滚动偏移：渲染时从 `visible_rows[scroll]` 开始画。
    scroll: usize,
    /// 当前显示视图（Flat 平铺 / ByAlbum 分组）。
    view: PlaylistView,
    /// 折叠的专辑（按专辑名）。只在 ByAlbum 视图下生效。
    collapsed_albums: HashSet<String>,
}

impl Default for Playlist {
    fn default() -> Self {
        Self::new()
    }
}

impl Playlist {
    /// 新建空播放列表（基础版默认按专辑分组）。
    pub fn new() -> Self {
        Self {
            core: PlaylistCore::new(),
            selected: None,
            scroll: 0,
            view: PlaylistView::Flat,
            collapsed_albums: HashSet::new(),
        }
    }

    // =========================================================================
    // 导航核心（委托给 PlaylistCore）
    // =========================================================================

    /// 加一个条目（按插入序追加）。
    pub fn add(&mut self, item: PlaylistItem) {
        self.core.add(item);
    }

    /// 批量加入。
    pub fn add_many(&mut self, items: impl IntoIterator<Item = PlaylistItem>) {
        self.core.add_many(items);
    }

    /// 去重加入单个条目。
    pub fn add_dedup(&mut self, item: PlaylistItem) -> bool {
        self.core.add_dedup(item)
    }

    /// 去重批量加入，返回实际加入数量。
    pub fn add_many_dedup(&mut self, items: impl IntoIterator<Item = PlaylistItem>) -> usize {
        self.core.add_many_dedup(items)
    }

    /// 清空列表，重置导航与 UI 状态。
    pub fn clear(&mut self) {
        self.core.clear();
        self.selected = None;
        self.collapsed_albums.clear();
    }

    /// 删除指定项（item index），同步调整 current/selected/history/折叠。
    pub fn remove(&mut self, index: usize) -> bool {
        if !self.core.remove(index) {
            return false;
        }
        // 回收 scroll：删除条目后行数减少，防止 skip(scroll) 越过新行数致面板空白。
        self.scroll = self.scroll.min(self.len().saturating_sub(1));

        // 调整 selected
        self.selected = match self.selected.take() {
            Some(Selection::Track(i)) if i == index => None,
            Some(Selection::Track(i)) if i > index => Some(Selection::Track(i - 1)),
            other => other,
        };

        // 清理折叠状态：只保留 items 中仍存在的专辑（无专辑曲目 = 空串哨兵）
        let existing: HashSet<String> = self
            .core
            .items()
            .iter()
            .map(|it| it.album.clone().unwrap_or_default())
            .collect();
        self.collapsed_albums.retain(|a| existing.contains(a));
        true
    }

    /// 条目数量。
    pub fn len(&self) -> usize {
        self.core.len()
    }

    /// 播放列表是否为空。
    pub fn is_empty(&self) -> bool {
        self.core.is_empty()
    }

    /// 全部条目（只读视图）。
    /// 可变访问全部条目（排序用）。
    pub fn items_mut(&mut self) -> &mut [PlaylistItem] {
        self.core.items_mut()
    }

    pub fn items(&self) -> &[PlaylistItem] {
        self.core.items()
    }

    /// 当前选中条目下标（空列表为 None）。
    pub fn current_index(&self) -> Option<usize> {
        self.core.current_index()
    }

    /// 是否随机播放。
    pub fn is_shuffle(&self) -> bool {
        self.core.is_shuffle()
    }

    /// 切换随机模式。
    pub fn set_shuffle(&mut self, on: bool) {
        self.core.set_shuffle(on);
    }

    /// 跳到指定项（用户手动点选）。
    pub fn jump_to(&mut self, index: usize) -> bool {
        self.core.jump_to(index)
    }

    /// 仅设置当前项，不修改 history。
    pub fn set_current(&mut self, index: usize) -> bool {
        self.core.set_current(index)
    }

    /// 预览下一曲条目（Gapless 预载用）。
    pub fn peek_next_item(&self, repeat: RepeatMode) -> Option<&PlaylistItem> {
        self.core.peek_next_item(repeat)
    }

    /// 进入下一曲。
    pub fn next(&mut self, repeat: RepeatMode) -> NavOutcome {
        self.core.next(repeat)
    }

    /// 进入上一曲。
    pub fn prev(&mut self, repeat: RepeatMode) -> NavOutcome {
        self.core.prev(repeat)
    }

    /// 按 (album, track_number, path) 物理排序条目切片。
    pub fn sort_items(items: &mut [PlaylistItem]) {
        PlaylistCore::sort_items(items);
    }

    /// 计算显示序（委托核心）。
    pub fn display_order(&self) -> Vec<usize> {
        self.core.display_order()
    }

    // =========================================================================
    // UI 呈现状态（产品层）
    // =========================================================================

    /// 面板选中对象（曲目或组头）。
    pub fn selected(&self) -> Option<&Selection> {
        self.selected.as_ref()
    }

    /// 选中的曲目索引（若选中的是曲目；选中组头时返回 None）。
    pub fn selected_track(&self) -> Option<usize> {
        match &self.selected {
            Some(Selection::Track(i)) => Some(*i),
            _ => None,
        }
    }

    /// 当前显示视图。
    pub fn view(&self) -> PlaylistView {
        self.view
    }

    /// 设置显示视图。
    pub fn set_view(&mut self, view: PlaylistView) {
        self.view = view;
    }

    /// 切换显示视图：Flat ↔ ByAlbum。
    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            PlaylistView::Flat => PlaylistView::ByAlbum,
            PlaylistView::ByAlbum => PlaylistView::Flat,
        };
    }

    /// 某专辑是否处于折叠状态。
    pub fn is_album_collapsed(&self, album: &str) -> bool {
        self.collapsed_albums.contains(album)
    }

    /// 折叠/展开某专辑。
    pub fn toggle_album(&mut self, album: &str) {
        if self.collapsed_albums.contains(album) {
            self.collapsed_albums.remove(album);
        } else {
            self.collapsed_albums.insert(album.to_string());
        }
    }

    /// 展开某专辑（播放/切歌时确保当前曲目可见）。
    pub fn expand_album(&mut self, album: &str) {
        self.collapsed_albums.remove(album);
    }

    /// 显示序滚动偏移（首行对应的显示行索引）。
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// 当前视图的"展开前"显示行：ByAlbum 时每专辑一个组头+该组全部曲目，
    /// Flat 时只有曲目。
    pub fn display_rows(&self, view: PlaylistView) -> Vec<PlaylistRow> {
        match view {
            PlaylistView::Flat => self
                .display_order()
                .into_iter()
                .map(|i| PlaylistRow::Track { item_index: i })
                .collect(),
            PlaylistView::ByAlbum => {
                let mut rows = Vec::new();
                let mut current_album: Option<String> = None;
                let mut current_tracks: Vec<usize> = Vec::new();
                for idx in self.display_order() {
                    // 无专辑曲目以空串作分组哨兵：渲染层翻译为「未知专辑」，
                    // 折叠/选中状态存哨兵值，切换语言不漂移。
                    let album = self.items()[idx].album.clone().unwrap_or_default();
                    match &current_album {
                        Some(a) if *a == album => current_tracks.push(idx),
                        _ => {
                            if let Some(a) = current_album.take() {
                                rows.push(PlaylistRow::AlbumHeader {
                                    album: a.clone(),
                                    track_indices: current_tracks.clone(),
                                });
                                for &t in &current_tracks {
                                    rows.push(PlaylistRow::Track { item_index: t });
                                }
                                current_tracks.clear();
                            }
                            current_album = Some(album);
                            current_tracks.push(idx);
                        }
                    }
                }
                if let Some(a) = current_album {
                    rows.push(PlaylistRow::AlbumHeader {
                        album: a.clone(),
                        track_indices: current_tracks.clone(),
                    });
                    for &t in &current_tracks {
                        rows.push(PlaylistRow::Track { item_index: t });
                    }
                }
                rows
            }
        }
    }

    /// 当前视图的"折叠后"可见行（折叠的专辑只保留组头）。
    pub fn visible_rows(&self, view: PlaylistView) -> Vec<PlaylistRow> {
        match view {
            PlaylistView::Flat => self.display_rows(view),
            PlaylistView::ByAlbum => {
                let mut out = Vec::new();
                let mut skip = false;
                for row in self.display_rows(view) {
                    match row {
                        PlaylistRow::AlbumHeader {
                            album,
                            track_indices,
                        } => {
                            out.push(PlaylistRow::AlbumHeader {
                                album: album.clone(),
                                track_indices: track_indices.clone(),
                            });
                            skip = self.collapsed_albums.contains(&album);
                        }
                        PlaylistRow::Track { item_index } => {
                            if !skip {
                                out.push(PlaylistRow::Track { item_index });
                            }
                        }
                    }
                }
                out
            }
        }
    }

    /// 显示行 → 选中对象。
    fn selection_of(row: &PlaylistRow) -> Selection {
        match row {
            PlaylistRow::AlbumHeader { album, .. } => Selection::Album(album.clone()),
            PlaylistRow::Track { item_index } => Selection::Track(*item_index),
        }
    }

    /// 判断显示行是否等于选中对象。
    fn row_matches(row: &PlaylistRow, sel: &Selection) -> bool {
        match (row, sel) {
            (PlaylistRow::AlbumHeader { album, .. }, Selection::Album(a)) => album == a,
            (PlaylistRow::Track { item_index }, Selection::Track(i)) => item_index == i,
            _ => false,
        }
    }

    /// 按给定的显示行调整滚动偏移，确保选中项始终可见。
    pub fn ensure_visible_in_rows(&mut self, rows: &[PlaylistRow], visible: usize) {
        let Some(sel) = self.selected.clone() else {
            return;
        };
        if visible == 0 {
            return;
        }
        let Some(pos) = rows.iter().position(|r| Self::row_matches(r, &sel)) else {
            return;
        };
        if pos < self.scroll {
            self.scroll = pos;
        }
        if pos >= self.scroll + visible {
            self.scroll = pos.saturating_sub(visible) + 1;
        }
    }

    /// 面板选择上移（不修改 current），按当前视图的可见行移动。
    pub fn move_selection_up(&mut self) {
        let rows = self.visible_rows(self.view);
        if rows.is_empty() {
            return;
        }
        let cur_pos = self
            .selected
            .as_ref()
            .and_then(|sel| rows.iter().position(|r| Self::row_matches(r, sel)));
        match cur_pos {
            Some(pos) if pos > 0 => self.selected = Some(Self::selection_of(&rows[pos - 1])),
            Some(_) => {}
            None => {
                if let Some(last) = rows.last() {
                    self.selected = Some(Self::selection_of(last));
                }
            }
        }
    }

    /// 面板选择下移（不修改 current）。
    pub fn move_selection_down(&mut self) {
        let rows = self.visible_rows(self.view);
        if rows.is_empty() {
            return;
        }
        let cur_pos = self
            .selected
            .as_ref()
            .and_then(|sel| rows.iter().position(|r| Self::row_matches(r, sel)));
        match cur_pos {
            Some(pos) if pos + 1 < rows.len() => {
                self.selected = Some(Self::selection_of(&rows[pos + 1]))
            }
            Some(_) => {}
            None => {
                if let Some(first) = rows.first() {
                    self.selected = Some(Self::selection_of(first));
                }
            }
        }
    }

    /// 面板选择移到可见行第一行。
    pub fn move_selection_to_top(&mut self) {
        if let Some(first) = self.visible_rows(self.view).first() {
            self.selected = Some(Self::selection_of(first));
        }
    }

    /// 面板选择移到可见行最后一行。
    pub fn move_selection_to_bottom(&mut self) {
        if let Some(last) = self.visible_rows(self.view).last() {
            self.selected = Some(Self::selection_of(last));
        }
    }

    /// 外部设置选中曲目（播放/跳转时让选中跟随当前曲目）。
    pub fn set_selected(&mut self, index: usize) {
        if index < self.len() {
            self.selected = Some(Selection::Track(index));
        }
    }
}

// =============================================================================
// 单元测试（UI 呈现状态）
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn item(album: Option<&str>, track: Option<u32>, name: &str) -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from(name),
            album: album.map(String::from),
            track_number: track,
            cue: None,
        }
    }

    #[test]
    fn display_rows_groups_by_album() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));
        p.add(item(Some("B"), Some(1), "/b1.mp3"));
        p.add(item(None, None, "/no.mp3"));

        let rows = p.display_rows(PlaylistView::ByAlbum);
        assert_eq!(rows.len(), 7);
        assert!(matches!(&rows[0], PlaylistRow::AlbumHeader { album, .. } if album == "A"));
        assert!(matches!(&rows[1], PlaylistRow::Track { item_index: 0 }));
        assert!(matches!(&rows[3], PlaylistRow::AlbumHeader { album, .. } if album == "B"));
        assert!(matches!(&rows[5], PlaylistRow::AlbumHeader { album, .. } if album.is_empty()));
        assert!(matches!(&rows[6], PlaylistRow::Track { item_index: 3 }));
    }

    #[test]
    fn collapse_hides_tracks() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));
        p.add(item(Some("B"), Some(1), "/b1.mp3"));

        p.toggle_album("A");
        assert!(p.is_album_collapsed("A"));

        let rows = p.visible_rows(PlaylistView::ByAlbum);
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], PlaylistRow::AlbumHeader { album, .. } if album == "A"));
        assert!(matches!(&rows[1], PlaylistRow::AlbumHeader { album, .. } if album == "B"));
        assert!(matches!(&rows[2], PlaylistRow::Track { item_index: 2 }));

        p.toggle_album("A");
        assert_eq!(p.visible_rows(PlaylistView::ByAlbum).len(), 5);
    }

    #[test]
    fn selection_moves_onto_album_header() {
        let mut p = Playlist::new();
        // fx 默认 Flat，本测试验证 ByAlbum 组头选中行为，先切到 ByAlbum。
        p.set_view(PlaylistView::ByAlbum);
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));

        p.move_selection_down();
        assert!(matches!(p.selected(), Some(Selection::Album(a)) if a == "A"));
        p.move_selection_down();
        assert_eq!(p.selected_track(), Some(0));
    }

    #[test]
    fn move_selection_from_none_picks_endpoints() {
        let mut p_down = Playlist::new();
        p_down.set_view(PlaylistView::Flat);
        for i in 0..3 {
            p_down.add(item(None, None, &format!("/{i}.mp3")));
        }
        p_down.move_selection_down();
        assert_eq!(p_down.selected_track(), Some(0));

        let mut p_up = Playlist::new();
        p_up.set_view(PlaylistView::Flat);
        for i in 0..3 {
            p_up.add(item(None, None, &format!("/{i}.mp3")));
        }
        p_up.move_selection_up();
        assert_eq!(p_up.selected_track(), Some(2));
    }

    #[test]
    fn move_selection_at_boundary_clamps() {
        let mut p = Playlist::new();
        p.set_view(PlaylistView::Flat);
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.set_selected(0);
        p.move_selection_up();
        assert_eq!(p.selected_track(), Some(0));
        p.set_selected(2);
        p.move_selection_down();
        assert_eq!(p.selected_track(), Some(2));
    }

    #[test]
    fn remove_adjusts_selected_and_scroll() {
        let mut p = Playlist::new();
        for i in 0..5 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(3);
        p.set_selected(4);
        assert!(p.remove(1));
        assert_eq!(p.current_index(), Some(2), "current 3 → 2");
        assert_eq!(p.selected_track(), Some(3), "selected 4 → 3");
    }

    #[test]
    fn clear_resets_ui_state() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/a.mp3"));
        p.jump_to(0);
        p.move_selection_down();
        p.toggle_album("未知专辑");
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.current_index(), None);
        assert_eq!(p.selected(), None);
        assert!(!p.is_album_collapsed("未知专辑"));
    }
}
