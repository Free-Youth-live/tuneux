//! 搜索与过滤：搜索目标枚举、播放列表过滤显示序、搜索态导航。
//!
//! 叶子模块（仅依赖 playlist 类型与 App 字段）。

use crate::playlist;

use super::App;

/// 搜索的目标面板（`/` 键在当前焦点下唤出搜索框）。
///
/// 搜索模式期间所有按键都被搜索框吃掉，`Tab` 无法切换焦点，因此
/// 用独立字段记录搜索针对哪个面板，避免靠 `focus` 推断带来的歧义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTarget {
    /// 在播放列表中搜索（按曲名/歌手/专辑/路径）。
    Playlist,
    /// 在文件浏览器中搜索（按文件名）。
    Browser,
}

impl App {
    /// 当前是否处于「浏览器搜索」态（搜索框可见且作用于浏览器）。
    /// 主循环判定异步导航结果是否需要连带退出搜索态用。
    pub(crate) fn in_browser_search(&self) -> bool {
        self.search_mode && self.search_target == SearchTarget::Browser
    }

    /// 当前 search_query 下的过滤显示序。
    pub fn filter_playlist(&self) -> Vec<usize> {
        let order = self.playlist.display_order();
        if self.search_query.is_empty() {
            return order;
        }
        let q = self.search_query.to_lowercase();
        order
            .into_iter()
            .filter(|&orig_idx| {
                let item = &self.playlist.items()[orig_idx];
                if item.path.to_string_lossy().to_lowercase().contains(&q) {
                    return true;
                }
                if item
                    .album
                    .as_deref()
                    .is_some_and(|a| a.to_lowercase().contains(&q))
                {
                    return true;
                }
                // CUE 分轨：匹配 .cue 的标题/表演者（否则搜分轨名零命中）。
                if let Some(cue) = &item.cue {
                    if cue.title.to_lowercase().contains(&q) {
                        return true;
                    }
                    if cue
                        .performer
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(&q))
                    {
                        return true;
                    }
                }
                if let Some(md) = self.metadata_cache.get(&item.path) {
                    if md
                        .title
                        .as_deref()
                        .is_some_and(|t| t.to_lowercase().contains(&q))
                    {
                        return true;
                    }
                    if md
                        .artist
                        .as_deref()
                        .is_some_and(|a| a.to_lowercase().contains(&q))
                    {
                        return true;
                    }
                }
                false
            })
            .collect()
    }

    /// 渲染播放列表用的显示行（考虑视图、折叠、搜索）。
    ///
    /// 搜索时临时平铺（过滤逻辑见 [`App::filter_playlist`]），退出搜索后
    /// 恢复原视图的分组/折叠状态。
    pub fn playlist_rows(&self) -> Vec<playlist::PlaylistRow> {
        if self.search_mode && self.search_target == SearchTarget::Playlist {
            self.filter_playlist()
                .into_iter()
                .map(|i| playlist::PlaylistRow::Track { item_index: i })
                .collect()
        } else {
            self.playlist.visible_rows(self.playlist.view())
        }
    }

    /// 调整播放列表滚动，确保选中项可见（使用渲染实际使用的行）。
    ///
    /// 搜索时渲染行是过滤后的平铺行，与 visible_rows(view) 不同，
    /// 故统一从 playlist_rows() 取行再调 ensure_visible_in_rows。
    pub fn ensure_playlist_visible(&mut self, rows: &[playlist::PlaylistRow], visible: usize) {
        self.playlist.ensure_visible_in_rows(rows, visible);
    }

    /// 搜索关键字变化后，让当前搜索目标面板刷新过滤结果并跳到首个匹配项。
    pub fn jump_selected_to_filter_first(&mut self) {
        let filter = self.filter_playlist();
        match filter.first() {
            Some(&i) => self.playlist.set_selected(i),
            None => self.playlist.set_selected(0),
        }
    }

    /// 在过滤结果内上下移动选中项（delta>0 下移，delta<0 上移）。
    fn move_selection_in_filter(&mut self, delta: i64) {
        let filter = self.filter_playlist();
        if filter.is_empty() {
            return;
        }
        let cur_pos = self
            .playlist
            .selected_track()
            .and_then(|s| filter.iter().position(|&i| i == s));
        let new_pos = match cur_pos {
            Some(p) if delta > 0 && p + 1 < filter.len() => Some(p + 1),
            Some(p) if delta < 0 && p > 0 => Some(p - 1),
            Some(p) => Some(p),
            None => {
                if delta > 0 {
                    Some(0)
                } else {
                    Some(filter.len() - 1)
                }
            }
        };
        if let Some(p) = new_pos {
            self.playlist.set_selected(filter[p]);
        }
    }

    /// 搜索关键字变化后，按当前目标分派刷新：Playlist 跳到首个匹配项，
    /// Browser 应用过滤关键字。
    pub(crate) fn apply_search_query(&mut self) {
        match self.search_target {
            SearchTarget::Playlist => self.jump_selected_to_filter_first(),
            SearchTarget::Browser => self.browser.set_filter(&self.search_query),
        }
    }

    /// 搜索模式下在过滤结果内上下移动选中项（delta>0 下移，delta<0 上移）。
    pub(crate) fn search_nav(&mut self, delta: i64) {
        match self.search_target {
            SearchTarget::Playlist => self.move_selection_in_filter(delta),
            SearchTarget::Browser => {
                if delta > 0 {
                    self.browser.move_down();
                } else {
                    self.browser.move_up();
                }
            }
        }
    }
}
