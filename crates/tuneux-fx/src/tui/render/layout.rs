//! # 布局度量（单一真相源）
//!
//! 集中定义四段布局（菜单栏 / 工作区 / 状态栏 / 功能键栏）的高度常量与
//! `layout_metrics` 纯函数，并给出工作区内各面板（浏览器/封面/播放列表/
//! 频谱/歌词）的尺寸。render 的 draw 与事件循环（滚动跟随）都从这里取数，
//! 禁止在别处手写高度/宽度公式。

use crate::config::{LeftPanel, LyricsMode, SpectrumMode};
use crate::tui::app::SearchTarget;

/// 菜单栏高度（1 行，无边框）。
const MENU_BAR_H: u16 = 1;
/// 状态栏高度（含边框 3 行：图标 + 进度条 + 时间/音量/循环）。
const STATUS_BAR_H: u16 = 3;
/// 功能键栏高度（1 行，无边框）。
const FKEY_BAR_H: u16 = 1;
/// 当前曲目框高度（含边框 6 行：曲名 / 演唱者 / 专辑 / 技术参数）。
const NOW_PLAYING_H: u16 = 6;
/// 四段固定高度之和；工作区 = 终端高 - 此值。
const CHROME_H: u16 = MENU_BAR_H + NOW_PLAYING_H + STATUS_BAR_H + FKEY_BAR_H;
/// 面板块上下边框合计占 2 行（Block::Borders::ALL）。
const PANEL_BORDERS_H: u16 = 2;
/// 搜索模式下，面板 inner 顶部让 1 行给 "/ 关键词" 输入框。
const SEARCH_LINE_H: u16 = 1;

/// 布局度量结果：一次 layout_metrics 调用得到的各段/各面板尺寸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutMetrics {
    /// 终端总列数。
    pub term_w: u16,
    /// 终端总行数。
    pub term_h: u16,
    /// 菜单栏高度（固定 1）。
    pub menu_bar_h: u16,
    /// 当前曲目框高度（固定 6，含边框）。
    pub now_playing_h: u16,
    /// 状态栏高度（固定 3，含边框）。
    pub status_bar_h: u16,
    /// 功能键栏高度（固定 1）。
    pub fkey_bar_h: u16,
    /// 工作区高度 = 终端高 - 11（菜单 1 + 当前曲目 6 + 状态 3 + 功能键 1）。
    pub workspace_h: u16,
    /// 播放列表面板外框高度（频谱决定纵向分配；Full 时 0 不渲染）。
    pub playlist_panel_h: u16,
    /// 播放列表实际可视行数（去边框；列表搜索时再去 1 行输入框）。
    pub playlist_visible_rows: usize,
    /// 频谱面板外框高度（Half 为工作区下半，Full 占满，Hidden 0）。
    pub spectrum_panel_h: u16,
    /// 浏览器实际可视行数（左侧非浏览器时为 0）。
    pub browser_visible_h: usize,
    /// 浏览器占工作区宽度百分比。
    pub browser_pct: u16,
    /// 封面占工作区宽度百分比。
    pub cover_pct: u16,
    /// 歌词可见时播放列表在左半区宽度百分比。
    pub playlist_w_pct: u16,
    /// 歌词可见时歌词面板宽度百分比。
    pub lyrics_w_pct: u16,
    /// 当前是否"浏览器搜索"态（搜索框占浏览器顶部 1 行）。
    pub browser_searching: bool,
    /// 当前是否"播放列表搜索"态（搜索框占列表顶部 1 行）。
    pub playlist_searching: bool,
}

/// 布局度量纯函数：给定终端尺寸与界面状态，算出各段/各面板尺寸。
///
/// 每个数值都镜像 draw 内对应 ratatui Constraint 的效果，保证渲染与
/// 事件循环（滚动跟随）取数一致。
pub fn layout_metrics(
    term_size: (u16, u16),
    spectrum_mode: SpectrumMode,
    lyrics_mode: LyricsMode,
    left_panel: LeftPanel,
    search_mode: bool,
    search_target: SearchTarget,
    browser_ratio: f32,
) -> LayoutMetrics {
    let (term_w, term_h) = term_size;
    // 工作区：终端高 - 四段固定（1 + 6 + 3 + 1 = 11）。
    let workspace_h = term_h.saturating_sub(CHROME_H);

    // 播放列表/频谱纵向分配（50/50 取下取整，与 draw 内 Percentage 一致）。
    let playlist_panel_h = match spectrum_mode {
        SpectrumMode::Hidden => workspace_h,
        SpectrumMode::Half => workspace_h / 2,
        SpectrumMode::Full | SpectrumMode::Oscilloscope => 0,
    };
    let spectrum_panel_h = match spectrum_mode {
        SpectrumMode::Hidden => 0,
        SpectrumMode::Half => workspace_h - workspace_h / 2,
        SpectrumMode::Full | SpectrumMode::Oscilloscope => workspace_h,
    };

    let browser_searching = search_mode && search_target == SearchTarget::Browser;
    let playlist_searching = search_mode && search_target == SearchTarget::Playlist;

    // 播放列表可视行数：外框 - 上下边框；列表搜索时再让 1 行输入框。
    let playlist_visible_rows = playlist_panel_h
        .saturating_sub(PANEL_BORDERS_H)
        .saturating_sub(u16::from(playlist_searching) * SEARCH_LINE_H)
        as usize;

    // 浏览器可视行数：左侧非浏览器时为 0；否则占满工作区（-2 边框），
    // 浏览器搜索时再让 1 行输入框（与 draw_browser 内布局一致）。
    let browser_visible_h = if left_panel == LeftPanel::Browser {
        workspace_h
            .saturating_sub(PANEL_BORDERS_H)
            .saturating_sub(u16::from(browser_searching) * SEARCH_LINE_H) as usize
    } else {
        0
    };

    // 左侧面板宽度百分比：浏览器比封面窄（约 70%），封面占满比例值。
    // 均 clamp 防止极端配置值导致某侧完全不可用。
    let browser_pct = (f64::from(browser_ratio) * 70.0).clamp(8.0, 70.0) as u16;
    let cover_pct = (f64::from(browser_ratio) * 100.0).clamp(10.0, 90.0) as u16;

    // 歌词可见时，列表与歌词横向 60/40 分屏（对高度无影响）。
    let (playlist_w_pct, lyrics_w_pct) = match lyrics_mode {
        LyricsMode::Hidden => (100, 0),
        LyricsMode::Visible => (60, 40),
    };

    LayoutMetrics {
        term_w,
        term_h,
        menu_bar_h: MENU_BAR_H,
        now_playing_h: NOW_PLAYING_H,
        status_bar_h: STATUS_BAR_H,
        fkey_bar_h: FKEY_BAR_H,
        workspace_h,
        playlist_panel_h,
        playlist_visible_rows,
        spectrum_panel_h,
        browser_visible_h,
        browser_pct,
        cover_pct,
        playlist_w_pct,
        lyrics_w_pct,
        browser_searching,
        playlist_searching,
    }
}
