//! # 布局度量（单一真相源）
//!
//! 职责：集中定义 TUI 各面板的尺寸常量与 `layout_metrics` 纯函数，
//! 给出每个面板的高度/宽度/可视行数。render.rs 的 `draw` 与
//! main.rs 的事件循环（滚动跟随）都必须从这里取数，
//! 禁止在别处手写高度/宽度公式。

use crate::config::{LeftPanel, LyricsMode, SpectrumMode};
use crate::tui::app::SearchTarget;

/// 顶部条高度（含边框，固定 6 行）：左当前曲信息 + 右电平。
const TOP_BAR_H: u16 = 6;
/// 状态条高度（含边框，固定 4 行）：进度/时间/音量/循环/错误。
const STATUS_BAR_H: u16 = 4;
/// 帮助栏高度（含边框，固定 3 行）：按焦点动态显示快捷键。
const HELP_BAR_H: u16 = 3;
/// 三个固定段的总高（6 + 4 + 3）：主区 = 终端高 - 此值。
const CHROME_H: u16 = TOP_BAR_H + STATUS_BAR_H + HELP_BAR_H;
/// 播放列表块上下边框合计占 2 行（Block::Borders::ALL）。
const PANEL_BORDERS_H: u16 = 2;
/// 搜索模式下，列表 inner 顶部让 1 行给 "/ 关键词" 输入框。
const SEARCH_LINE_H: u16 = 1;

/// 布局度量结果：一次 layout_metrics 调用得到的全部面板尺寸。
///
/// 这是渲染布局的**单一真相源**——render.rs 的 draw 与 main.rs 的
/// 事件循环（滚动跟随）都必须从这里取数，禁止各自手写高度公式。
///
/// 高度字段均为"含边框的面板外框高度"；`*_visible_rows` 是去掉
/// 边框与搜索行后**实际能容纳的内容行数**（直接喂给 ensure_visible）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutMetrics {
    /// 终端总列数。
    pub term_w: u16,
    /// 终端总行数。
    pub term_h: u16,
    /// 顶部条高度（固定 6，见 TOP_BAR_H）。
    pub top_bar_h: u16,
    /// 状态条高度（固定 4，见 STATUS_BAR_H）。
    pub status_bar_h: u16,
    /// 帮助栏高度（固定 3，见 HELP_BAR_H）。
    pub help_bar_h: u16,
    /// 主区高度 = 终端高 - 13（顶部条+状态条+帮助栏）。
    /// 频谱 Half 时上下各分一半（main_h / 2）。
    pub main_h: u16,
    /// 播放列表面板外框高度：
    /// - 无频谱：占满主区；
    /// - 半屏频谱：主区上半（main_h / 2，与 draw 内 50/50 分割一致）；
    /// - 全屏频谱：0（列表不渲染）。
    pub playlist_panel_h: u16,
    /// 播放列表实际可视行数（去掉上下边框 2 行；列表搜索时再去 1 行输入框）。
    pub playlist_visible_rows: usize,
    /// 频谱面板外框高度（Half 时为主区下半 = main_h - main_h / 2，
    /// Full 时占满主区，Hidden 时 0）。
    pub spectrum_panel_h: u16,
    /// 浏览器实际可视行数（左侧非浏览器时为 0；否则主区高 - 2 边框，
    /// 浏览器搜索时再去 1 行输入框）。
    pub browser_visible_h: usize,
    /// 浏览器占主区宽度的百分比（(browser_ratio * 70).clamp(8, 70)）。
    pub browser_pct: u16,
    /// 封面占主区宽度的百分比（(browser_ratio * 100).clamp(10, 90)）。
    pub cover_pct: u16,
    /// 歌词可见时，播放列表在左半区宽度的百分比（固定 60）。
    pub playlist_w_pct: u16,
    /// 歌词可见时，歌词面板宽度的百分比（固定 40，与列表互补）。
    pub lyrics_w_pct: u16,
    /// 当前是否处于"浏览器搜索"状态（搜索框占浏览器顶部 1 行）。
    pub browser_searching: bool,
    /// 当前是否处于"播放列表搜索"状态（搜索框占列表顶部 1 行）。
    pub playlist_searching: bool,
}

/// 布局度量纯函数：给定终端尺寸与界面状态，算出所有面板的高度/宽度。
///
/// # 参数
/// - term_size：(宽, 高)，即 frame.area() 或 terminal.size()；
/// - spectrum_mode：频谱模式（决定列表区纵向怎么分）；
/// - lyrics_mode：歌词模式（决定列表区横向 60/40 分屏；对高度无影响，
///   因为歌词与列表的分工是横向的——唯一例外是频谱 Full 覆盖一切，
///   此时列表高度为 0，与歌词无关）；
/// - left_panel：左侧面板（浏览器/封面/隐藏，决定浏览器可视行数与
///   左侧宽度百分比）；
/// - search_mode / search_target：搜索状态（浏览器或列表的顶部
///   搜索框各占 1 行）；
/// - browser_ratio：配置中的左侧面板宽度比例（0.0-1.0）。
///
/// # 一致性保证
/// 本函数的每个数值都**镜像 draw 内对应 ratatui Constraint 的效果**：
/// - 主区 = Fill(1) 于 [Length(6), Fill(1), Length(4), Length(3)]
///   → term_h.saturating_sub(13)；
/// - Half 频谱 = vertical([50%, 50%]) 的上段 → main_h / 2
///   （下半段为 main_h - main_h / 2）；
/// - 面板内容高 = 外框高 - 上下边框 2 行（Block::inner）；
/// - 搜索框 = vertical([Length(1), Min(1)]) 的第一段 → 再减 1 行。
///
/// 注意：ratatui 的 50% 上下分割在奇数高度下可能把多出的 1 行分给
/// 任一段（求解器行为），这里的 main_h / 2 按下取整——与改造前
/// main.rs 的推算完全一致（行为不变优先）。
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

    // 主区：终端高 - 三个固定段（6 + 4 + 3 = 13）。saturating 防止
    // 极矮终端（< 13 行）减出负数——此时主区为 0，各面板自然全空。
    let main_h = term_h.saturating_sub(CHROME_H);

    // 播放列表面板高度：频谱模式决定纵向分配。
    // Half 时 50/50 分割取上段（下取整，见函数级注释）。
    let playlist_panel_h = match spectrum_mode {
        SpectrumMode::Hidden => main_h,
        SpectrumMode::Half => main_h / 2,
        // 全屏频谱：列表根本不渲染
        SpectrumMode::Full => 0,
    };
    // 频谱面板高度：与播放列表互补。
    let spectrum_panel_h = match spectrum_mode {
        SpectrumMode::Hidden => 0,
        SpectrumMode::Half => main_h - main_h / 2,
        SpectrumMode::Full => main_h,
    };

    // 搜索状态拆分：浏览器搜索占浏览器顶部 1 行；列表搜索占列表顶部 1 行。
    let browser_searching = search_mode && search_target == SearchTarget::Browser;
    let playlist_searching = search_mode && search_target == SearchTarget::Playlist;

    // 播放列表可视行数：外框 - 上下边框 2 行；列表搜索时再让 1 行输入框。
    // 注意：列表搜索的 -1 是 render.rs（draw_playlist）的实际行为，
    // 改造前 main.rs 漏算了这一行（见任务记录），此处以 render.rs 为准。
    let playlist_visible_rows = playlist_panel_h
        .saturating_sub(PANEL_BORDERS_H)
        .saturating_sub(if playlist_searching { SEARCH_LINE_H } else { 0 })
        as usize;

    // 浏览器可视行数：左侧非浏览器时为 0；否则占满主区高（-2 边框），
    // 浏览器搜索时再让 1 行输入框（与 draw_browser 内布局一致）。
    let browser_visible_h = if left_panel == LeftPanel::Browser {
        main_h
            .saturating_sub(PANEL_BORDERS_H)
            .saturating_sub(if browser_searching { SEARCH_LINE_H } else { 0 }) as usize
    } else {
        0
    };

    // 左侧面板宽度百分比：浏览器比封面窄（约 70%），封面占满比例值。
    // 均 clamp 防止极端配置值导致某侧完全不可用。
    let browser_pct = ((browser_ratio * 70.0) as f64).clamp(8.0, 70.0) as u16;
    let cover_pct = ((browser_ratio * 100.0) as f64).clamp(10.0, 90.0) as u16;

    // 歌词可见时，列表与歌词横向 60/40 分屏（对高度无影响）。
    let (playlist_w_pct, lyrics_w_pct) = match lyrics_mode {
        LyricsMode::Hidden => (100, 0),
        LyricsMode::Visible => (60, 40),
    };

    LayoutMetrics {
        term_w,
        term_h,
        top_bar_h: TOP_BAR_H,
        status_bar_h: STATUS_BAR_H,
        help_bar_h: HELP_BAR_H,
        main_h,
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
