//! # 播放列表模块
//!
//! 管理用户加入的曲目集合，支持按专辑-曲序排序的"专辑视图"展示，
//! 以及单曲/列表循环、随机播放等导航策略。
//!
//! ## 数据模型
//!
//! - `PlaylistItem`：单个条目，包含路径和排序键（专辑名 + 曲序）。
//! - `Playlist`：条目集合 + 导航状态（current / selected / history）+ 随机开关。
//!
//! ## 排序 vs 显示序
//!
//! `items` 按**插入序**存储（这是播放序，next/prev/jump_to 都用它），
//! `display_order()` 返回**显示序**索引（按专辑-曲序排序后的 indices，
//! 专为 TUI 渲染）。这样设计避免插入时排序导致 current/history 索引被破坏。
//!
//! ## 循环 + 随机
//!
//! - `RepeatMode::Single`：永远保持当前（`NavOutcome::Repeat`）
//! - `RepeatMode::List` + 关随机：顺序播，到尾循环到首
//! - `RepeatMode::Off` + 关随机：顺序播，到尾 `End`
//! - 任意模式 + 开随机：从"未播"集合随机抽；全播过则按 Repeat 决定
//!   `End`（Off）或重洗（List）
//!
//! `prev` 走 history 栈（shuffle 时）或 current-1（顺序时）。

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::{PlaylistView, RepeatMode};

/// CUE 分轨引用（整轨文件内的曲目片段）。
///
/// 播放整轨（整张专辑压成一个音频文件 + 同名 `.cue`）时，
/// 每个 CUE 曲目展开为一个 PlaylistItem，播放起点 = `start_ms`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CueRef {
    /// CUE 曲目号（1-based，来自 `TRACK n AUDIO`）。
    pub index: u32,
    /// 曲目标题（来自 `.cue` 的 TITLE，缺失时为 "Track {n}"）。
    pub title: String,
    /// 表演者（来自 `.cue` 的 PERFORMER，可选；渲染时优先于整轨元数据）。
    #[serde(default)]
    pub performer: Option<String>,
    /// 起始时间（毫秒，来自 `INDEX 01`，75 帧/秒换算）。
    pub start_ms: u64,
    /// 结束时间（毫秒）：下一曲 INDEX 起点；末曲为整轨总时长；未知为 None（播到文件尾）。
    #[serde(default)]
    pub end_ms: Option<u64>,
}

/// 播放列表中的一个条目。
///
/// 排序键（`album` + `track_number`）在加入时确定并随条目存储，
/// 避免每次排序时重新读取 metadata。
/// 派生 Serialize/Deserialize 用于"保存播放列表，下次启动恢复"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// 文件完整路径
    pub path: PathBuf,
    /// 专辑名（来自 ID3/Vorbis 标签），无则归入"无专辑"组
    pub album: Option<String>,
    /// 曲序（专辑内 1, 2, 3...），无则排到该专辑末尾
    pub track_number: Option<u32>,
    /// CUE 分轨引用：Some 表示这是整轨文件中的一曲（播放从 `start_ms` 起）。
    /// `#[serde(default)]` 保证旧版保存的播放列表可正常反序列化。
    #[serde(default)]
    pub cue: Option<CueRef>,
}

/// 导航操作的结果。
///
/// 调用 `next` / `prev` / `jump_to` 后由 `Playlist` 返回，
/// App 据此决定是否给音频引擎下发新曲目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavOutcome {
    /// 切到指定 item
    Switch(usize),
    /// 单曲循环：保持当前（调用方应重新发 Play 让流再起）
    Repeat,
    /// 没有下一曲/上一曲
    End,
}

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
    Track { item_index: usize },
}

/// 焦点面板（用于 Tab 切换）。当前只有浏览器和播放列表两个面板，
/// 元数据面板只读不参与焦点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    #[default]
    Browser,
    Playlist,
}

/// 播放列表。
///
/// 持有条目集合、播放状态（current）、面板选择（selected）、
/// 已播历史（history）、随机开关和 LCG 状态。
#[derive(Debug, Clone)]
pub struct Playlist {
    items: Vec<PlaylistItem>,
    /// 当前播放项索引（按插入序）。None 表示还没开始播。
    current: Option<usize>,
    /// 面板选中对象（曲目或专辑组头）。当焦点在 Playlist 面板时，
    /// ↑/↓ 移动此项；Enter 播放曲目或折叠/展开组头。
    selected: Option<Selection>,
    /// 已播历史（最近的在末尾），用于 shuffle 模式下的 prev 撤回
    history: Vec<usize>,
    shuffle: bool,
    rng: Lcg,
    /// 显示行滚动偏移：渲染时从 visible_rows[scroll] 开始画。
    /// 由 ensure_visible 根据选中项位置维护，让选中项始终在可视区内。
    scroll: usize,
    /// 当前显示视图（Flat 平铺 / ByAlbum 分组）。
    view: PlaylistView,
    /// 折叠的专辑（按专辑名）。只在 ByAlbum 视图下生效。
    collapsed_albums: HashSet<String>,
    /// 显示序缓存：`display_order()` 的结果，列表变更时置 None 失效。
    ///
    /// 避免每次渲染/导航都重算 `O(n log n)` 排序（性能项）；
    /// 用 RefCell 保持 display_order(&self) 签名不变（调用方众多）。
    display_order_cache: std::cell::RefCell<Option<Vec<usize>>>,
}

/// 简单伪随机数生成器（splitmix64），避免引入 `rand` 依赖。
/// 分布对洗牌足够用，单测用固定种子验证确定性。
#[derive(Debug, Clone)]
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new() -> Self {
        // 默认种子：固定常量。洗牌会话间不可复现但单测可设自己的种子。
        Self {
            state: 0x1234567890abcdef,
        }
    }

    #[cfg(test)]
    fn with_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// splitmix64 单步
    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// 0..n 的随机 usize（n=0 返回 0）
    fn gen_range(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() as usize) % n
        }
    }
}

impl Default for Playlist {
    fn default() -> Self {
        Self::new()
    }
}

impl Playlist {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            current: None,
            selected: None,
            history: Vec::new(),
            shuffle: false,
            rng: Lcg::new(),
            scroll: 0,
            view: PlaylistView::ByAlbum,
            collapsed_albums: HashSet::new(),
            display_order_cache: std::cell::RefCell::new(None),
        }
    }

    /// 用固定种子构造（仅测试用）
    #[cfg(test)]
    pub fn new_with_seed(seed: u64) -> Self {
        Self {
            items: Vec::new(),
            current: None,
            selected: None,
            history: Vec::new(),
            shuffle: false,
            rng: Lcg::with_seed(seed),
            scroll: 0,
            view: PlaylistView::ByAlbum,
            collapsed_albums: HashSet::new(),
            display_order_cache: std::cell::RefCell::new(None),
        }
    }

    /// 加一个条目（按插入序追加，不立即排序——排序在显示时算）
    pub fn add(&mut self, item: PlaylistItem) {
        self.items.push(item);
        self.invalidate_display_order();
    }

    /// 批量加入（如 `a` 加整个目录时一次塞一批）
    pub fn add_many(&mut self, items: impl IntoIterator<Item = PlaylistItem>) {
        self.items.extend(items);
        self.invalidate_display_order();
    }

    // =========================================================================
    // 去重加入（配合 Config::dedup_on_add 使用）
    // =========================================================================

    /// 条目的去重键：`(path, cue.index)`。
    ///
    /// - 普通曲目（cue = None）：键为 `(path, None)`，同 path 即重复。
    /// - CUE 分轨（cue = Some）：键为 `(path, Some(index))`，
    ///   同 path 但不同 cue.index 视为不同曲目（不误去重）。
    fn dedup_key(item: &PlaylistItem) -> (PathBuf, Option<u32>) {
        (item.path.clone(), item.cue.as_ref().map(|c| c.index))
    }

    /// 判断条目是否已存在于播放列表中（按去重键判定）。
    fn contains_dedup_key(&self, item: &PlaylistItem) -> bool {
        let key = Self::dedup_key(item);
        self.items
            .iter()
            .any(|existing| Self::dedup_key(existing) == key)
    }

    /// 去重加入单个条目：若 `(path, cue.index)` 已存在则跳过。
    ///
    /// 返回 true 表示实际加入了新条目，false 表示因重复被跳过。
    pub fn add_dedup(&mut self, item: PlaylistItem) -> bool {
        if self.contains_dedup_key(&item) {
            false
        } else {
            self.items.push(item);
            self.invalidate_display_order();
            true
        }
    }

    /// 去重批量加入：逐个检查 `(path, cue.index)`，跳过已存在的条目。
    ///
    /// 返回实际加入的新条目数量。
    pub fn add_many_dedup(&mut self, items: impl IntoIterator<Item = PlaylistItem>) -> usize {
        let mut added = 0;
        for item in items {
            if self.add_dedup(item) {
                added += 1;
            }
        }
        added
    }

    // =========================================================================
    // 排序增强
    // =========================================================================

    /// 按标题字母序重排条目（原地排序，修改插入序）。
    ///
    /// 标题取值优先级：`cue.title` > path 文件名（不含扩展名）。
    /// 标题相同时回退到 path 字典序，保证排序确定性。
    /// 当前未接入 UI 快捷键（保留 API 供播放列表增强接入），豁免 dead_code。
    #[allow(dead_code)]
    pub fn sort_by_title(&mut self) {
        self.items.sort_by(|a, b| {
            let title_a = Self::display_title(a);
            let title_b = Self::display_title(b);
            title_a
                .to_lowercase()
                .cmp(&title_b.to_lowercase())
                .then(a.path.cmp(&b.path))
        });
        // 排序后 current/selected/history 索引可能失效，重置以保持一致性
        self.current = None;
        self.selected = None;
        self.history.clear();
        self.invalidate_display_order();
    }

    /// 按文件路径字母序重排条目（原地排序，修改插入序）。
    ///
    /// 直接比较完整 path，排序确定且稳定。
    /// 当前未接入 UI 快捷键（保留 API 供播放列表增强接入），豁免 dead_code。
    #[allow(dead_code)]
    pub fn sort_by_path(&mut self) {
        self.items.sort_by(|a, b| a.path.cmp(&b.path));
        // 排序后 current/selected/history 索引可能失效，重置以保持一致性
        self.current = None;
        self.selected = None;
        self.history.clear();
        self.invalidate_display_order();
    }

    /// 获取条目的显示标题（用于排序和渲染）。
    ///
    /// 优先级：CUE 标题 > path 文件名（去扩展名）> 空字符串。
    /// 仅供排序使用（sort_by_title），豁免 dead_code。
    #[allow(dead_code)]
    fn display_title(item: &PlaylistItem) -> String {
        if let Some(cue) = &item.cue {
            return cue.title.clone();
        }
        item.path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// 清空列表，重置所有状态。
    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
        self.selected = None;
        self.history.clear();
        self.collapsed_albums.clear();
        self.invalidate_display_order();
    }

    /// 删除指定项（item index），并同步调整 current/selected/history 的索引。
    ///
    /// 删除后索引大于该值的项自动前移一位；删除当前项时 current 置 None
    /// （调用方应停止播放）；清理不再存在的专辑折叠状态。
    pub fn remove(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.items.remove(index);
        self.invalidate_display_order();

        // 调整 current
        self.current = match self.current {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };

        // 调整 selected
        self.selected = match self.selected.take() {
            Some(Selection::Track(i)) if i == index => None,
            Some(Selection::Track(i)) if i > index => Some(Selection::Track(i - 1)),
            other => other,
        };

        // 调整 history（去掉被删项，其余索引前移）
        self.history = self
            .history
            .iter()
            .filter(|&&i| i != index)
            .map(|&i| if i > index { i - 1 } else { i })
            .collect();

        // 清理折叠状态：只保留 items 中仍存在的专辑
        let existing: HashSet<String> = self
            .items
            .iter()
            .map(|it| it.album.clone().unwrap_or_else(|| "未知专辑".to_string()))
            .collect();
        self.collapsed_albums.retain(|a| existing.contains(a));

        true
    }

    /// 条目数量。
    // 仅单元测试使用；二进制 crate 中 pub 方法仍会触发 dead_code 警告，故保留此 allow。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }
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
    /// 显示序滚动偏移（首行对应的显示行索引）。渲染时据此 skip。
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
                    let album = self.items[idx]
                        .album
                        .clone()
                        .unwrap_or_else(|| "未知专辑".to_string());
                    match &current_album {
                        Some(a) if *a == album => current_tracks.push(idx),
                        _ => {
                            // 结束上一个组：push 组头 + 该组曲目
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

    /// 当前视图的"折叠后"可见行（折叠的专辑只保留组头，不显示其曲目）。
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
    ///
    /// 由事件循环在渲染前调用（传入渲染实际使用的行与可视行数）。
    /// 把行列表作为参数传入，是为了搜索等场景下渲染行可能与
    /// visible_rows(view) 不同（搜索时是过滤后的平铺行）。
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
        // 选中项在可视区上方 → 上移 scroll 到选中项
        if pos < self.scroll {
            self.scroll = pos;
        }
        // 选中项在可视区下方 → 下推 scroll，让选中项落在可视区最后一行
        if pos >= self.scroll + visible {
            self.scroll = pos.saturating_sub(visible) + 1;
        }
    }

    pub fn is_shuffle(&self) -> bool {
        self.shuffle
    }

    /// 切换随机模式。开 → 启用并清空 history（让"未播"从全集开始）。
    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        if on {
            self.history.clear();
        }
    }

    /// 跳到指定项（用户手动点选）。失败（越界）返回 false。
    pub fn jump_to(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        if let Some(curr) = self.current {
            self.history.push(curr);
        }
        self.current = Some(index);
        true
    }

    /// 仅设置当前项，不修改 history。
    ///
    /// 供"导航结果"（next/prev 返回的 Switch）使用：next/prev 已维护 history
    /// 与 current，此处再走 jump_to 会把当前项重复压栈，破坏 prev 的正确性。
    pub fn set_current(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.current = Some(index);
        true
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
            Some(_) => {} // 已在顶部
            None => {
                // 未选中：跳到末行
                if let Some(last) = rows.last() {
                    self.selected = Some(Self::selection_of(last));
                }
            }
        }
    }

    /// 面板选择下移（不修改 current），按当前视图的可见行移动。
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
            Some(_) => {} // 已在底部
            None => {
                // 未选中：跳到首行
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
        if index < self.items.len() {
            self.selected = Some(Selection::Track(index));
        }
    }

    /// 进入下一曲（按 repeat + shuffle 算）。
    /// 同时把旧的 current push 到 history（shuffle 模式 prev 用）。
    ///
    /// 顺序模式按 **display 顺序**走（按 album/track 排序后的顺序），
    /// 与面板上 ↑/↓ 的导航一致——用户看到啥、播下一首就是啥的下一首。
    /// shuffle 模式不受影响（随机从所有未播过项里抽，不依赖顺序）。
    /// 预览下一曲路径（不改变状态；Gapless 预载用）。
    ///
    /// 仅顺序播放可预测：单曲循环 / 随机播放返回 None（不预载，
    /// 由现有 EOF→draining→finished 流程兜底）。
    pub fn peek_next(&self, repeat: RepeatMode) -> Option<PathBuf> {
        if self.items.is_empty() {
            return None;
        }
        // 单曲循环 / 随机播放 / 单曲列表：无"下一曲"可预载（next 会 Repeat 自身）
        if matches!(repeat, RepeatMode::Single) || self.shuffle || self.items.len() == 1 {
            return None;
        }
        // 未播放（current=None）预测第一首；已播放预测 current 的下一首
        let idx = match self.current {
            Some(c) => c + 1,
            None => 0,
        };
        if idx < self.items.len() {
            self.items.get(idx).map(|i| i.path.clone())
        } else if matches!(repeat, RepeatMode::List) {
            // 列表循环：回到第一首
            self.items.first().map(|i| i.path.clone())
        } else {
            None // 顺序播放到末尾且不循环
        }
    }

    pub fn next(&mut self, repeat: RepeatMode) -> NavOutcome {
        if self.items.is_empty() {
            return NavOutcome::End;
        }
        // 单曲循环 OR 列表只有 1 首：永远保持
        if matches!(repeat, RepeatMode::Single) || self.items.len() == 1 {
            return NavOutcome::Repeat;
        }

        let next_idx = if self.shuffle {
            match self.pick_random_unplayed() {
                Some(i) => Some(i),
                None if matches!(repeat, RepeatMode::List) => {
                    // 全播过了，列表循环：清空 history 重洗
                    self.history.clear();
                    if let Some(c) = self.current {
                        self.history.push(c);
                    }
                    self.pick_random_unplayed()
                }
                None => None,
            }
        } else {
            // 顺序模式：按 display 顺序找 current 的位置，+1
            let order = self.display_order();
            match self
                .current
                .and_then(|c| order.iter().position(|&i| i == c))
            {
                Some(pos) if pos + 1 < order.len() => Some(order[pos + 1]),
                Some(_) if matches!(repeat, RepeatMode::List) => Some(order[0]),
                Some(_) => None,
                None => order.first().copied(), // 未播过：跳到 display 第一项
            }
        };

        match next_idx {
            Some(idx) => {
                if let Some(curr) = self.current {
                    self.history.push(curr);
                }
                self.current = Some(idx);
                NavOutcome::Switch(idx)
            }
            None => NavOutcome::End,
        }
    }

    /// 进入上一曲。
    /// - 单曲循环：保持
    /// - shuffle：弹 history 栈顶
    /// - 顺序：按 display 顺序 current - 1，到顶按 List/Off 决定循环或 End
    /// - 列表只有 1 首：与 next 一致，保持（永远没有"上一首"）
    pub fn prev(&mut self, repeat: RepeatMode) -> NavOutcome {
        if self.items.is_empty() {
            return NavOutcome::End;
        }
        // 单曲循环 OR 列表只有 1 首：永远保持
        if matches!(repeat, RepeatMode::Single) || self.items.len() == 1 {
            return NavOutcome::Repeat;
        }

        let prev_idx = if self.shuffle {
            self.history.pop()
        } else {
            // 顺序模式：按 display 顺序找 current 的位置，-1
            let order = self.display_order();
            match self
                .current
                .and_then(|c| order.iter().position(|&i| i == c))
            {
                Some(0) if matches!(repeat, RepeatMode::List) => order.last().copied(),
                Some(0) => None,
                Some(pos) => Some(order[pos - 1]),
                None => None, // 未播过：没有"上一首"
            }
        };

        match prev_idx {
            Some(idx) => {
                self.current = Some(idx);
                NavOutcome::Switch(idx)
            }
            None => NavOutcome::End,
        }
    }

    /// 按 (album, track_number, path) 物理排序条目切片。
    ///
    /// 供"添加目录"等批量加入场景在加入前预排序，与 [`Playlist::display_order`]
    /// 的比较逻辑完全一致，保证新增批次的插入序 = 显示序。
    pub fn sort_items(items: &mut [PlaylistItem]) {
        items.sort_by(Self::compare_items);
    }

    /// 两个条目的排序比较：(album, track_number, path) 字典序，
    /// 无专辑的项排末尾，无曲序的项排到该专辑末尾。
    fn compare_items(a: &PlaylistItem, b: &PlaylistItem) -> Ordering {
        match (a.album.as_deref(), b.album.as_deref()) {
            (Some(al), Some(bl)) => al
                .cmp(bl)
                .then(
                    a.track_number
                        .unwrap_or(u32::MAX)
                        .cmp(&b.track_number.unwrap_or(u32::MAX)),
                )
                .then(a.path.cmp(&b.path)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => a.path.cmp(&b.path),
        }
    }

    /// 使显示序缓存失效（列表内容变更后调用）。
    fn invalidate_display_order(&mut self) {
        *self.display_order_cache.borrow_mut() = None;
    }

    /// 计算显示序：按 (album, track_number) 排序，
    /// 无专辑的项排到末尾，无曲序的项排到该专辑末尾。
    /// 插入序索引保持不变——返回的是 indices 数组。
    ///
    /// 结果缓存于 `display_order_cache`：列表未变更时多次调用零开销
    /// （渲染/导航每帧调用，大列表下避免重复 `O(n log n)`）。
    pub fn display_order(&self) -> Vec<usize> {
        // 缓存命中直接返回
        if let Some(cached) = self.display_order_cache.borrow().as_ref() {
            return cached.clone();
        }
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.sort_by(|&a, &b| Self::compare_items(&self.items[a], &self.items[b]));
        *self.display_order_cache.borrow_mut() = Some(order.clone());
        order
    }

    /// 从"未播"集合（不含 current 和 history）随机抽一个
    fn pick_random_unplayed(&mut self) -> Option<usize> {
        let unplayed: Vec<usize> = (0..self.items.len())
            .filter(|i| Some(*i) != self.current && !self.history.contains(i))
            .collect();
        if unplayed.is_empty() {
            None
        } else {
            Some(unplayed[self.rng.gen_range(unplayed.len())])
        }
    }
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn item(album: Option<&str>, track: Option<u32>, name: &str) -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from(name),
            album: album.map(String::from),
            track_number: track,
            cue: None,
        }
    }

    /// peek_next：顺序播放预测下一曲（不改变状态）。
    #[test]
    fn peek_next_sequential() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/m/1.mp3"));
        p.add(item(Some("A"), Some(2), "/m/2.mp3"));
        p.add(item(Some("A"), Some(3), "/m/3.mp3"));

        // 未播放（current=None）：预测第一首
        assert_eq!(
            p.peek_next(RepeatMode::Off),
            Some(PathBuf::from("/m/1.mp3"))
        );
        // current=0 → 下一首 = 2
        p.set_current(0);
        assert_eq!(
            p.peek_next(RepeatMode::Off),
            Some(PathBuf::from("/m/2.mp3"))
        );
        // current=2（末尾）→ Off 不循环：None
        p.set_current(2);
        assert_eq!(p.peek_next(RepeatMode::Off), None);
        // 列表循环：末尾 → 回第一首
        assert_eq!(
            p.peek_next(RepeatMode::List),
            Some(PathBuf::from("/m/1.mp3"))
        );
    }

    /// peek_next：单曲循环 / 随机播放不预载（返回 None，由 EOF 流程兜底）。
    #[test]
    fn peek_next_no_preload_modes() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/m/1.mp3"));
        p.add(item(Some("A"), Some(2), "/m/2.mp3"));
        p.set_current(0);
        assert_eq!(p.peek_next(RepeatMode::Single), None);
        p.shuffle = true;
        assert_eq!(p.peek_next(RepeatMode::List), None);
        assert_eq!(p.peek_next(RepeatMode::Off), None);
    }

    /// peek_next：空列表 / 单曲列表。
    #[test]
    fn peek_next_empty_or_single() {
        let p = Playlist::new();
        assert_eq!(p.peek_next(RepeatMode::List), None);
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/m/1.mp3"));
        // 单曲列表：Single/List/Off 均无"下一曲"（next 会 Repeat 自身），不预载
        assert_eq!(p.peek_next(RepeatMode::Single), None);
        assert_eq!(p.peek_next(RepeatMode::List), None);
        assert_eq!(p.peek_next(RepeatMode::Off), None);
    }

    #[test]
    fn empty_playlist_next_is_end() {
        let mut p = Playlist::new();
        assert!(p.is_empty());
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::End);
        assert_eq!(p.next(RepeatMode::List), NavOutcome::End);
        assert_eq!(p.next(RepeatMode::Single), NavOutcome::End);
    }

    #[test]
    fn add_and_len() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/a.mp3"));
        p.add(item(None, None, "/b.mp3"));
        assert_eq!(p.len(), 2);
        assert_eq!(p.items().len(), 2);
    }

    #[test]
    fn clear_resets_everything() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/a.mp3"));
        p.jump_to(0);
        p.move_selection_down();
        p.clear();
        assert!(p.is_empty());
        assert_eq!(p.current_index(), None);
        assert_eq!(p.selected(), None);
    }

    #[test]
    fn remove_adjusts_indices() {
        let mut p = Playlist::new();
        for i in 0..5 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        // current = 3，selected = 4
        p.jump_to(3);
        p.set_selected(4);

        // 删除索引 1：后面的索引应前移
        assert!(p.remove(1));
        assert_eq!(p.items().len(), 4);
        assert_eq!(p.current_index(), Some(2), "current 3 → 2");
        assert_eq!(p.selected_track(), Some(3), "selected 4 → 3");
    }

    #[test]
    fn remove_current_clears_it() {
        let mut p = Playlist::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(1);
        // 删除当前项
        assert!(p.remove(1));
        assert_eq!(p.current_index(), None, "删除当前项后 current 应为 None");
    }

    // —— 顺序模式 ——
    #[test]
    fn next_off_advances_then_ends() {
        let mut p = Playlist::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::Switch(1));
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::Switch(2));
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::End);
    }

    #[test]
    fn next_list_wraps_to_first() {
        let mut p = Playlist::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(2);
        assert_eq!(p.next(RepeatMode::List), NavOutcome::Switch(0));
    }

    #[test]
    fn next_single_keeps_current() {
        let mut p = Playlist::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(1);
        assert_eq!(p.next(RepeatMode::Single), NavOutcome::Repeat);
        assert_eq!(p.current_index(), Some(1));
    }

    #[test]
    fn prev_off_decrements_then_ends() {
        let mut p = Playlist::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(2);
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Switch(1));
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Switch(0));
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::End);
    }

    #[test]
    fn prev_list_wraps_to_last() {
        let mut p = Playlist::new();
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        assert_eq!(p.prev(RepeatMode::List), NavOutcome::Switch(2));
    }

    #[test]
    fn single_song_always_repeats() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/only.mp3"));
        p.jump_to(0);
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::Repeat);
        assert_eq!(p.next(RepeatMode::List), NavOutcome::Repeat);
        assert_eq!(p.next(RepeatMode::Single), NavOutcome::Repeat);
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Repeat);
    }

    // —— shuffle 模式 ——
    #[test]
    fn shuffle_does_not_pick_same_twice_in_a_row() {
        let mut p = Playlist::new_with_seed(42);
        p.set_shuffle(true);
        for i in 0..5 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        let mut seen = std::collections::HashSet::new();
        // 跑 4 次 next，应得 4 个不同的非 current 项
        for _ in 0..4 {
            match p.next(RepeatMode::Off) {
                NavOutcome::Switch(i) => {
                    assert_ne!(i, 0, "不应回 current");
                    assert!(seen.insert(i), "不应重复抽到 {i}");
                }
                _ => panic!("应该返回 Switch"),
            }
        }
    }

    #[test]
    fn shuffle_with_list_reshuffles_when_exhausted() {
        let mut p = Playlist::new_with_seed(0xdeadbeef);
        p.set_shuffle(true);
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        // 跑 3 次 next 播完所有（不包括 current，shuffle 不重播 current）
        // Off 模式下，第 3 次应该 End
        p.next(RepeatMode::Off);
        p.next(RepeatMode::Off);
        assert_eq!(p.next(RepeatMode::Off), NavOutcome::End);
        // 切到 List 模式：可以重洗
        assert_ne!(p.next(RepeatMode::List), NavOutcome::End);
    }

    #[test]
    fn shuffle_prev_uses_history() {
        let mut p = Playlist::new_with_seed(7);
        p.set_shuffle(true);
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        // next 一次到 X（随机曲），只验证确实是切歌而非 End
        assert!(matches!(p.next(RepeatMode::Off), NavOutcome::Switch(_)));
        // prev 应该回到 0
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Switch(0));
        // 第二次 next 应该再去 next_idx（重洗后的随机，但 history 已清）
        // 简化：只检查 prev 正常工作
    }

    #[test]
    fn shuffle_prev_after_navigation() {
        // 回归：播放层曾对 next 结果再调 jump_to，把当前项重复压栈，
        // 导致 prev 弹回当前项（看起来 p 键无效）。现在导航结果用 set_current。
        let mut p = Playlist::new_with_seed(7);
        p.set_shuffle(true);
        for i in 0..3 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);

        // next：0 → X，next 内部 push 0，history = [0]
        let x = match p.next(RepeatMode::Off) {
            NavOutcome::Switch(i) => i,
            _ => panic!("应 Switch"),
        };
        // 播放层用 set_current（不重复 push），current = X，history 仍 [0]
        p.set_current(x);

        // prev 应回到 0，而不是停在 X
        assert_eq!(p.prev(RepeatMode::Off), NavOutcome::Switch(0));
        assert_eq!(p.current_index(), Some(0));
    }

    #[test]
    fn shuffle_distribution_is_roughly_uniform() {
        // 10 首歌，1000 次 next（List 模式跑完会自动重洗）
        // 1000 次 next / 10 首歌 ≈ 每首 100 次。允许 ±30% 偏差（70~130）。
        let mut p = Playlist::new_with_seed(0xcafe);
        p.set_shuffle(true);
        for i in 0..10 {
            p.add(item(None, None, &format!("/{i}.mp3")));
        }
        p.jump_to(0);
        let mut counts = [0usize; 10];
        for _ in 0..1000 {
            if let NavOutcome::Switch(i) = p.next(RepeatMode::List) {
                if i < 10 {
                    counts[i] += 1;
                }
            }
        }
        for (i, &c) in counts.iter().enumerate() {
            assert!(
                c > 70 && c < 130,
                "位置 {i} 被选 {c} 次，分布偏 (期望 ~100)"
            );
        }
    }

    // —— display_order ——
    #[test]
    fn display_order_groups_by_album_then_track() {
        let mut p = Playlist::new();
        // 故意乱序加入
        p.add(item(Some("B"), Some(1), "/b1.mp3"));
        p.add(item(Some("A"), Some(3), "/a3.mp3"));
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));

        let order = p.display_order();
        let names: Vec<String> = order
            .iter()
            .map(|&i| p.items()[i].path.to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["/a1.mp3", "/a2.mp3", "/a3.mp3", "/b1.mp3"]);
    }

    #[test]
    fn display_order_no_album_at_end() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a.mp3"));
        p.add(item(None, None, "/no1.mp3"));
        p.add(item(None, None, "/no2.mp3"));

        let order = p.display_order();
        let names: Vec<String> = order
            .iter()
            .map(|&i| p.items()[i].path.to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["/a.mp3", "/no1.mp3", "/no2.mp3"]);
    }

    #[test]
    fn display_order_missing_track_at_album_end() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), None, "/a_no_track.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));

        let order = p.display_order();
        let names: Vec<String> = order
            .iter()
            .map(|&i| p.items()[i].path.to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["/a1.mp3", "/a2.mp3", "/a_no_track.mp3"]);
    }

    /// 显示序缓存：列表变更（add/remove/clear）后缓存失效，结果始终最新。
    #[test]
    fn display_order_cache_invalidates_on_change() {
        let mut p = Playlist::new();
        p.add(item(Some("B"), Some(1), "/b1.mp3"));
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        // 首次计算：A 排在 B 前（按专辑名）
        let order = p.display_order();
        assert_eq!(p.items()[order[0]].path.to_string_lossy(), "/a1.mp3");
        // 再次调用：命中缓存，结果一致
        assert_eq!(p.display_order(), order);
        // 新增 C 专辑项：缓存应失效，重新排序
        p.add(item(Some("C"), Some(1), "/c1.mp3"));
        let order2 = p.display_order();
        assert_eq!(order2.len(), 3, "新增后应包含 3 项");
        assert_ne!(order2, order, "新增后缓存应已失效");
        // 删除后缓存失效
        p.remove(order2[0]);
        assert_eq!(p.display_order().len(), 2, "删除后应剩 2 项");
    }

    /// 去重加入（主路径，默认 dedup_on_add=true）也必须失效显示序缓存。
    ///
    /// 回归测试（红-1）：add_dedup 此前漏调 invalidate，
    /// 导致去重加入后播放列表空白（缓存脏读 Some([])）。
    #[test]
    fn add_dedup_invalidates_display_order_cache() {
        let mut p = Playlist::new();
        // 空列表预热缓存（渲染每帧会先调 display_order）
        assert_eq!(p.display_order().len(), 0);
        // 去重加入一条：缓存必须失效
        assert!(p.add_dedup(item(Some("A"), Some(1), "/a1.mp3")));
        let order = p.display_order();
        assert_eq!(order.len(), 1, "去重加入后应显示 1 项");
        // 重复加入被跳过：列表不变
        assert!(!p.add_dedup(item(Some("A"), Some(1), "/a1.mp3")));
        assert_eq!(p.display_order().len(), 1, "重复项不应重复显示");
        // 追加新曲：缓存再次失效
        assert!(p.add_dedup(item(Some("B"), Some(1), "/b1.mp3")));
        assert_eq!(p.display_order().len(), 2, "追加后应显示 2 项");
    }

    #[test]
    fn display_order_empty() {
        let p = Playlist::new();
        assert!(p.display_order().is_empty());
    }

    #[test]
    fn sort_items_orders_by_album_track() {
        let mut items = vec![
            item(Some("B"), Some(1), "/b1.mp3"),
            item(Some("A"), Some(3), "/a3.mp3"),
            item(Some("A"), Some(1), "/a1.mp3"),
            item(Some("A"), Some(2), "/a2.mp3"),
        ];
        Playlist::sort_items(&mut items);
        let names: Vec<&str> = items.iter().map(|i| i.path.to_str().unwrap()).collect();
        assert_eq!(names, vec!["/a1.mp3", "/a2.mp3", "/a3.mp3", "/b1.mp3"]);
    }

    // —— 分组视图（ByAlbum）——
    #[test]
    fn display_rows_groups_by_album() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));
        p.add(item(Some("B"), Some(1), "/b1.mp3"));
        p.add(item(None, None, "/no.mp3"));

        let rows = p.display_rows(PlaylistView::ByAlbum);
        // A 组头 + a1 + a2，B 组头 + b1，未知专辑组头 + no = 7 行
        assert_eq!(rows.len(), 7);
        assert!(matches!(&rows[0], PlaylistRow::AlbumHeader { album, .. } if album == "A"));
        assert!(matches!(&rows[1], PlaylistRow::Track { item_index: 0 }));
        assert!(matches!(&rows[2], PlaylistRow::Track { item_index: 1 }));
        assert!(matches!(&rows[3], PlaylistRow::AlbumHeader { album, .. } if album == "B"));
        assert!(matches!(&rows[4], PlaylistRow::Track { item_index: 2 }));
        assert!(matches!(&rows[5], PlaylistRow::AlbumHeader { album, .. } if album == "未知专辑"));
        assert!(matches!(&rows[6], PlaylistRow::Track { item_index: 3 }));
    }

    #[test]
    fn collapse_hides_tracks() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));
        p.add(item(Some("B"), Some(1), "/b1.mp3"));

        // 折叠 A
        p.toggle_album("A");
        assert!(p.is_album_collapsed("A"));

        let rows = p.visible_rows(PlaylistView::ByAlbum);
        // A 组头（折叠）+ B 组头 + b1 = 3 行
        assert_eq!(rows.len(), 3);
        assert!(matches!(&rows[0], PlaylistRow::AlbumHeader { album, .. } if album == "A"));
        assert!(matches!(&rows[1], PlaylistRow::AlbumHeader { album, .. } if album == "B"));
        assert!(matches!(&rows[2], PlaylistRow::Track { item_index: 2 }));

        // 展开 A 恢复 5 行
        p.toggle_album("A");
        assert_eq!(p.visible_rows(PlaylistView::ByAlbum).len(), 5);
    }

    #[test]
    fn selection_moves_onto_album_header() {
        let mut p = Playlist::new();
        p.add(item(Some("A"), Some(1), "/a1.mp3"));
        p.add(item(Some("A"), Some(2), "/a2.mp3"));

        // 未选中 ↓：跳到第一行（A 组头）
        p.move_selection_down();
        assert!(matches!(p.selected(), Some(Selection::Album(a)) if a == "A"));

        // 再 ↓：跳到 a1
        p.move_selection_down();
        assert_eq!(p.selected_track(), Some(0));
    }

    // —— 面板选择 ——
    #[test]
    fn move_selection_from_none_picks_endpoints() {
        // 未选中状态下：↓ → 第一项，↑ → 最后一项（Flat 平铺视图）
        let mut p_down = Playlist::new();
        p_down.set_view(PlaylistView::Flat);
        for i in 0..3 {
            p_down.add(item(None, None, &format!("/{i}.mp3")));
        }
        p_down.move_selection_down();
        assert_eq!(p_down.selected_track(), Some(0), "未选中 ↓ → 0");

        let mut p_up = Playlist::new();
        p_up.set_view(PlaylistView::Flat);
        for i in 0..3 {
            p_up.add(item(None, None, &format!("/{i}.mp3")));
        }
        p_up.move_selection_up();
        assert_eq!(p_up.selected_track(), Some(2), "未选中 ↑ → 末项");
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
        assert_eq!(p.selected_track(), Some(0), "顶部再上移不变");
        p.set_selected(2);
        p.move_selection_down();
        assert_eq!(p.selected_track(), Some(2), "底部再下移不变");
    }

    // =========================================================================
    // 去重测试
    // =========================================================================

    /// 构造带 CUE 引用的条目（用于去重测试）。
    fn cue_item(path: &str, index: u32, title: &str) -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from(path),
            album: Some("整轨专辑".to_string()),
            track_number: Some(index),
            cue: Some(CueRef {
                index,
                title: title.to_string(),
                performer: None,
                start_ms: (index as u64 - 1) * 180_000,
                end_ms: None,
            }),
        }
    }

    /// 普通曲目去重：同 path 的第二次加入应被跳过。
    #[test]
    fn dedup_skips_duplicate_path() {
        let mut p = Playlist::new();
        assert!(p.add_dedup(item(None, None, "/a.mp3")), "首次加入应成功");
        assert!(!p.add_dedup(item(None, None, "/a.mp3")), "同 path 应被去重");
        assert_eq!(p.len(), 1, "去重后应只有 1 条");
    }

    /// CUE 分轨不误去重：同 path 但不同 cue.index 应各自独立加入。
    #[test]
    fn dedup_cue_tracks_with_different_index_not_deduped() {
        let mut p = Playlist::new();
        assert!(p.add_dedup(cue_item("/album.flac", 1, "Track 1")));
        assert!(p.add_dedup(cue_item("/album.flac", 2, "Track 2")));
        assert!(p.add_dedup(cue_item("/album.flac", 3, "Track 3")));
        assert_eq!(p.len(), 3, "不同 cue.index 的 CUE 曲目不应被去重");
    }

    /// CUE 分轨去重：同 path + 同 cue.index 应被去重。
    #[test]
    fn dedup_cue_tracks_same_index_deduped() {
        let mut p = Playlist::new();
        assert!(p.add_dedup(cue_item("/album.flac", 1, "Track 1")));
        assert!(
            !p.add_dedup(cue_item("/album.flac", 1, "Track 1 dup")),
            "同 path+index 应去重"
        );
        assert_eq!(p.len(), 1);
    }

    /// 批量去重：add_many_dedup 应返回实际加入数量，跳过重复项。
    #[test]
    fn dedup_add_many_skips_duplicates() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/existing.mp3"));
        let items = vec![
            item(None, None, "/existing.mp3"), // 重复
            item(None, None, "/new1.mp3"),     // 新
            item(None, None, "/new2.mp3"),     // 新
            item(None, None, "/new1.mp3"),     // 批次内重复
        ];
        let added = p.add_many_dedup(items);
        assert_eq!(added, 2, "应只加入 2 条新条目");
        assert_eq!(p.len(), 3, "总计 3 条（1 旧 + 2 新）");
    }

    /// 普通曲目与 CUE 曲目互不干扰：同 path 的普通曲目和 CUE 曲目
    /// 去重键不同（None vs Some(index)），应各自独立存在。
    #[test]
    fn dedup_plain_and_cue_coexist() {
        let mut p = Playlist::new();
        // 先加普通曲目（cue=None → key=(path, None)）
        assert!(p.add_dedup(item(None, None, "/album.flac")));
        // 再加同 path 的 CUE 曲目（cue.index=1 → key=(path, Some(1))）
        assert!(p.add_dedup(cue_item("/album.flac", 1, "Track 1")));
        assert_eq!(p.len(), 2, "普通曲目与 CUE 曲目应共存");
    }

    // =========================================================================
    // 排序增强测试
    // =========================================================================

    /// sort_by_title：按标题字母序排列（CUE 标题优先于文件名）。
    #[test]
    fn sort_by_title_orders_correctly() {
        let mut p = Playlist::new();
        // 故意乱序加入
        p.add(item(None, None, "/z_song.mp3")); // 标题 "z_song"
        p.add(item(None, None, "/a_song.mp3")); // 标题 "a_song"
        p.add(cue_item("/album.flac", 1, "Middle")); // CUE 标题 "Middle"

        p.sort_by_title();

        let titles: Vec<String> = p.items().iter().map(Playlist::display_title).collect();
        assert_eq!(
            titles,
            vec!["a_song", "Middle", "z_song"],
            "应按标题字母序排列"
        );
    }

    /// sort_by_title：大小写不敏感（"Apple" 和 "apple" 相邻）。
    #[test]
    fn sort_by_title_case_insensitive() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/banana.mp3"));
        p.add(item(None, None, "/Apple.mp3"));
        p.add(item(None, None, "/cherry.mp3"));

        p.sort_by_title();

        let titles: Vec<String> = p.items().iter().map(Playlist::display_title).collect();
        assert_eq!(titles, vec!["Apple", "banana", "cherry"]);
    }

    /// sort_by_path：按完整路径字母序排列。
    #[test]
    fn sort_by_path_orders_correctly() {
        let mut p = Playlist::new();
        p.add(item(Some("B"), Some(1), "/music/z.mp3"));
        p.add(item(Some("A"), Some(1), "/music/a.mp3"));
        p.add(item(None, None, "/music/m.mp3"));

        p.sort_by_path();

        let paths: Vec<String> = p
            .items()
            .iter()
            .map(|it| it.path.to_string_lossy().to_string())
            .collect();
        assert_eq!(paths, vec!["/music/a.mp3", "/music/m.mp3", "/music/z.mp3"]);
    }

    /// sort_by_title / sort_by_path 排序后 current/selected/history 被重置。
    #[test]
    fn sort_resets_navigation_state() {
        let mut p = Playlist::new();
        p.add(item(None, None, "/b.mp3"));
        p.add(item(None, None, "/a.mp3"));
        p.jump_to(0);
        p.set_selected(1);

        p.sort_by_path();

        assert_eq!(p.current_index(), None, "排序后 current 应重置");
        assert_eq!(p.selected(), None, "排序后 selected 应重置");
    }
}
