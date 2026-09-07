//! # 渲染主入口与调度
//!
//! 四段布局（参照 foobar2000 组织）：菜单栏 → 工作区 → 状态栏 → 功能键栏。
//! 工作区为多面板拼合：左侧面板（文件浏览器/专辑封面，互斥）+ 播放列表/频谱
//!（纵向）+ 歌词（横向），承 tuneux 的面板模型。配色完全由【调色板】驱动
//!（见 [`super::theme`]）：渲染只认调色板里的颜色，不写死任何色值——将来插件
//! 换皮肤就是换一份调色板，零返工。

mod layout;
mod popup;
mod spectrum;

pub use self::layout::{layout_metrics, LayoutMetrics};

use std::path::Path;

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tuneux_corex as audio;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::menu::{menus, MenuAction};
use super::app::App;
use super::theme::Palette;
use crate::config::{Config, LeftPanel, LyricsMode, PlaylistView, SpectrumMode};
use crate::fs_browser::Entry;
use crate::playlist::{Panel, PlaylistRow, Selection};
use tuneux_mediax::lyrics;

use self::popup::{draw_about, draw_compressor, draw_equalizer};
use self::spectrum::{draw_audio_panel, draw_level_meter, draw_oscilloscope};

// —— 样式与排版小工具 ——

/// 由可选前景/背景拼一个样式；`None` = 沿用终端默认色。
fn pal_style(fg: Option<Color>, bg: Option<Color>) -> Style {
    let mut s = Style::default();
    if let Some(f) = fg {
        s = s.fg(f);
    }
    if let Some(b) = bg {
        s = s.bg(b);
    }
    s
}

/// 面板边框样式：聚焦的面板加粗突出。
fn panel_border(pal: &Palette, focused: bool) -> Style {
    let s = pal_style(pal.border, None);
    if focused {
        s.add_modifier(Modifier::BOLD)
    } else {
        s
    }
}

/// 秒数 → "m:ss"（超过 1 小时 "h:mm:ss"）。
fn fmt_time(secs: f64) -> String {
    // 四舍五入到秒：避免 3.999 显示为 0:03 的观感问题。
    let secs = (secs.max(0.0) + 0.5) as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 按显示宽度截断到 max_w（CJK 全角字符按 2 计）。
fn truncate_to_width(s: &str, max_w: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max_w {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

/// 截断并用空格补齐到 max_w 显示宽度（左对齐，列表列用）。
fn pad_to_width(s: &str, max_w: usize) -> String {
    let t = truncate_to_width(s, max_w);
    let w = UnicodeWidthStr::width(t.as_str());
    if w >= max_w {
        t
    } else {
        format!("{}{}", t, " ".repeat(max_w - w))
    }
}

/// 截断并右对齐到 max_w 显示宽度（时长列用）。
fn pad_left_to_width(s: &str, max_w: usize) -> String {
    let t = truncate_to_width(s, max_w);
    let w = UnicodeWidthStr::width(t.as_str());
    if w >= max_w {
        t
    } else {
        format!("{}{}", " ".repeat(max_w - w), t)
    }
}

/// 取路径文件干名（去扩展名），缺标题时兜底显示。
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// —— 主入口 ——

/// 分轨内进度（CUE 分轨按分轨区间换算）：返回（位置秒, 时长秒）。
///
/// 无时长时返回（位置, 0.0）。供状态条与进度相关显示共用同一口径。
pub(super) fn track_progress(
    engine: &audio::Engine,
    duration: Option<f64>,
    cue: Option<&crate::playlist::CueRef>,
) -> (f64, f64) {
    let mut pos = engine.position();
    let mut dur = duration.unwrap_or(0.0);
    // CUE 分轨口径：进度/时长按本分轨区间显示，而非整轨时间。
    if let Some(c) = cue {
        let start_s = c.start_ms as f64 / 1000.0;
        let track_dur = match c.end_ms {
            Some(end) => (end.saturating_sub(c.start_ms)) as f64 / 1000.0,
            None => (dur - start_s).max(0.0),
        };
        pos = (pos - start_s).max(0.0);
        dur = track_dur;
    }
    (pos, dur)
}

/// 渲染主入口：四段垂直布局，自上而下；取当前主题的调色板。
pub fn draw(
    frame: &mut ratatui::Frame,
    app: &mut App,
    config: &Config,
    rows: &[PlaylistRow],
    dt: std::time::Duration,
) {
    let pal = app.skin.unwrap_or_else(|| app.theme.palette());
    let area = frame.area();
    let m = layout_metrics(
        (area.width, area.height),
        app.spectrum_mode,
        app.lyrics_mode,
        app.left_panel,
        app.search_mode,
        app.search_target,
        config.browser_ratio,
    );
    let vertical = Layout::vertical([
        Constraint::Length(m.menu_bar_h),
        Constraint::Length(m.now_playing_h),
        Constraint::Fill(1),
        Constraint::Length(m.status_bar_h),
        Constraint::Length(m.fkey_bar_h),
    ])
    .split(area);

    draw_menu_bar(frame, vertical[0], app, &pal);
    draw_now_playing(frame, vertical[1], app, &pal);

    // 工作区：左侧面板（浏览器/封面/隐藏）+ 列表/频谱/歌词。
    // 介质只作用于声音（corex DSP），不占界面：左侧面板始终按
    // 浏览器 / 封面等正常切换，频谱照常嵌入右侧。
    // rows 由主循环每帧计算一次传入（避免重复构建）。
    match app.left_panel {
        LeftPanel::Browser => {
            let main = Layout::horizontal([
                Constraint::Percentage(m.browser_pct),
                Constraint::Percentage(100 - m.browser_pct),
            ])
            .split(vertical[2]);
            draw_browser(frame, main[0], app, &pal, m.browser_searching);
            draw_playlist_or_spectrum(
                frame,
                main[1],
                app,
                &pal,
                &m,
                rows,
                dt,
                config.lyrics_offset,
            );
        }
        LeftPanel::Cover => {
            let main = Layout::horizontal([
                Constraint::Percentage(m.cover_pct),
                Constraint::Percentage(100 - m.cover_pct),
            ])
            .split(vertical[2]);
            draw_cover_panel(frame, main[0], app, &pal);
            draw_playlist_or_spectrum(
                frame,
                main[1],
                app,
                &pal,
                &m,
                rows,
                dt,
                config.lyrics_offset,
            );
        }
        LeftPanel::Hidden => {
            draw_playlist_or_spectrum(
                frame,
                vertical[2],
                app,
                &pal,
                &m,
                rows,
                dt,
                config.lyrics_offset,
            );
        }
        LeftPanel::CoverBrowser => {
            // 封面网格浏览占满整个工作区（不含播放列表）。
            draw_cover_browser(frame, vertical[2], app, &pal);
        }
    }
    // 帧计数 +1，供频谱乱码字符随帧变化。
    app.frame_tick = app.frame_tick.wrapping_add(1);

    draw_status_bar(frame, vertical[3], app, config, &pal);
    draw_fkey_bar(frame, vertical[4], &pal);

    // 菜单下拉：覆盖在工作区之上（在关于弹窗之前）。
    draw_menu_dropdown(frame, area, app, &pal);

    // 均衡器面板：覆盖在工作区之上（关于弹窗之前）。
    if app.eq_visible {
        draw_equalizer(frame, area, app, &pal);
    }

    // 压缩器面板：与均衡器同级覆盖。
    if app.comp_visible {
        draw_compressor(frame, area, app, &pal);
    }

    // 关于弹窗：最后绘制，覆盖在其它内容之上。
    if app.about_visible {
        draw_about(frame, area, &pal);
    }
}

/// 菜单栏（顶部 1 行）：顶级菜单（文件…帮助 + 介质）；激活栏（menu_top）反白，下拉见 [`draw_menu_dropdown`]。
fn draw_menu_bar(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let ms = menus();
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::styled(" ", pal_style(pal.menu_fg, pal.menu_bg)));
    for (i, menu) in ms.iter().enumerate() {
        let active = app.menu_active && app.menu_top == i;
        let st = if active {
            // 激活菜单：前后景互换（反白）。
            pal_style(pal.menu_bg, pal.menu_fg).add_modifier(Modifier::BOLD)
        } else {
            pal_style(pal.menu_fg, pal.menu_bg).add_modifier(Modifier::BOLD)
        };
        // 数字前缀（dim 弱化，与标题区分；按数字直接打开该栏）。
        spans.push(Span::styled(
            (i + 1).to_string(),
            pal_style(pal.menu_fg, pal.menu_bg).add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled(menu.title.to_string(), st));
        spans.push(Span::styled("  ", pal_style(pal.menu_fg, pal.menu_bg)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// 顶级菜单标题在菜单栏中的起始列（供下拉定位）。
fn menu_x_offset(index: usize) -> u16 {
    let ms = menus();
    let mut x = 1u16; // 行首 1 空格
    for m in ms.iter().take(index) {
        // 数字 1 列 + 标题 + 2 空格间隔。
        x += UnicodeWidthStr::width(m.title) as u16 + 3;
    }
    x
}

/// 菜单下拉框：菜单栏激活且展开时，覆盖在工作区之上渲染当前栏的菜单项。
/// 视图开关类项按运行时状态打 √；不可用项灰显；选中项反白。
fn draw_menu_dropdown(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    if !(app.menu_active && app.menu_dropdown) {
        return;
    }
    let ms = menus();
    let Some(menu) = ms.get(app.menu_top) else {
        return;
    };

    // 下拉宽度 = 最宽项（标签+快捷键）+ 边距；高度 = 项数 + 2 边框。
    let mut item_w = 0usize;
    for it in &menu.items {
        let sw = if it.shortcut.is_empty() {
            0
        } else {
            UnicodeWidthStr::width(it.shortcut) + 2
        };
        item_w = item_w.max(UnicodeWidthStr::width(it.label) + 3 + sw); // 3 = 勾选位 2 + 间隔 1
    }
    let box_w = ((item_w + 2).min(area.width as usize)) as u16;
    let box_h = ((menu.items.len() + 2).min(area.height.saturating_sub(1) as usize)) as u16;
    if box_w < 4 || box_h < 3 {
        return;
    }
    let x = area.x + menu_x_offset(app.menu_top).min(area.width.saturating_sub(box_w));
    let box_area = Rect {
        x,
        y: area.y + 1,
        width: box_w,
        height: box_h,
    };

    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(pal_style(pal.border, None))
        .style(pal_style(None, pal.menu_bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let inner_w = inner.width as usize;
    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            // 视图开关类的勾选态（按运行时状态）。
            let checked: Option<bool> = match it.action {
                MenuAction::ToggleBrowser => Some(app.left_panel == LeftPanel::Browser),
                MenuAction::ToggleCover => Some(app.left_panel == LeftPanel::Cover),
                MenuAction::ToggleLyrics => Some(app.lyrics_mode == LyricsMode::Visible),
                MenuAction::ToggleSpectrum => Some(app.spectrum_mode != SpectrumMode::Hidden),
                MenuAction::TogglePlaylistView => {
                    Some(app.playlist.view() == PlaylistView::ByAlbum)
                }
                // 介质菜单：当前生效的介质打勾。
                MenuAction::SetMedium(m) => Some(app.playback_medium == m),
                // ReplayGain：开关状态打勾。
                MenuAction::ReplayGain => Some(app.replay_gain),
                // 插件清单：√ = 该插件已加载（运行时从 plugins/ 目录装载）。
                MenuAction::PluginEq => Some(app.eq_plugin.is_some()),
                MenuAction::PluginComp => Some(app.comp_plugin.is_some()),
                _ => None,
            };
            let mark = match checked {
                Some(true) => "√ ",
                Some(false) => "  ",
                None => "  ",
            };
            // 标签 + （快捷键右对齐）。
            let mut text = format!("{mark}{}", it.label);
            if !it.shortcut.is_empty() {
                let lw = UnicodeWidthStr::width(text.as_str());
                let sw = UnicodeWidthStr::width(it.shortcut);
                let gap = inner_w.saturating_sub(lw + sw);
                text = format!("{}{}{}", text, " ".repeat(gap.max(1)), it.shortcut);
            }
            let st = if i == app.menu_item && it.enabled {
                pal_style(pal.menu_bg, pal.menu_fg).add_modifier(Modifier::BOLD)
            } else if !it.enabled {
                pal_style(pal.menu_fg, pal.menu_bg).add_modifier(Modifier::DIM)
            } else {
                pal_style(pal.menu_fg, pal.menu_bg)
            };
            Line::from(Span::styled(pad_to_width(&text, inner_w), st))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 播放列表区（或频谱/歌词）渲染分发器。
///
/// 歌词与频谱可共存：歌词 Visible 时左（列表+频谱）右（歌词）横向分屏；
/// 频谱 Full 覆盖一切。歌词 Hidden 时走频谱 Hidden/Half/Full 逻辑。
#[allow(clippy::too_many_arguments)]
fn draw_playlist_or_spectrum(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    pal: &Palette,
    m: &LayoutMetrics,
    rows: &[PlaylistRow],
    dt: std::time::Duration,
    lyrics_offset: f64,
) {
    match app.lyrics_mode {
        LyricsMode::Visible => {
            if app.spectrum_mode == SpectrumMode::Full {
                draw_audio_panel(
                    frame,
                    area,
                    &app.engine,
                    app.frame_tick,
                    pal,
                    &app.spectrum_peaks,
                    dt,
                );
            } else if app.spectrum_mode == SpectrumMode::Oscilloscope {
                draw_oscilloscope(frame, area, &app.engine, pal, app.frame_tick);
            } else {
                let split = Layout::horizontal([
                    Constraint::Percentage(m.playlist_w_pct),
                    Constraint::Percentage(m.lyrics_w_pct),
                ])
                .split(area);
                match app.spectrum_mode {
                    SpectrumMode::Hidden => {
                        draw_playlist(frame, split[0], app, pal, rows, m.playlist_searching);
                    }
                    SpectrumMode::Half => {
                        let left = Layout::vertical([
                            Constraint::Percentage(50),
                            Constraint::Percentage(50),
                        ])
                        .split(split[0]);
                        draw_playlist(frame, left[0], app, pal, rows, m.playlist_searching);
                        draw_audio_panel(
                            frame,
                            left[1],
                            &app.engine,
                            app.frame_tick,
                            pal,
                            &app.spectrum_peaks,
                            dt,
                        );
                    }
                    // Full 已被外层 if 拦截早返，此处仅为 match 穷尽性，不会执行到。
                    _ => {}
                }
                draw_lyrics_panel(
                    frame,
                    split[1],
                    &app.engine,
                    app.current_lyrics.as_ref(),
                    pal,
                    lyrics_offset,
                );
            }
        }
        LyricsMode::Hidden => match app.spectrum_mode {
            SpectrumMode::Hidden => {
                draw_playlist(frame, area, app, pal, rows, m.playlist_searching);
            }
            SpectrumMode::Half => {
                let split =
                    Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(area);
                draw_playlist(frame, split[0], app, pal, rows, m.playlist_searching);
                draw_audio_panel(
                    frame,
                    split[1],
                    &app.engine,
                    app.frame_tick,
                    pal,
                    &app.spectrum_peaks,
                    dt,
                );
            }
            SpectrumMode::Full => {
                draw_audio_panel(
                    frame,
                    area,
                    &app.engine,
                    app.frame_tick,
                    pal,
                    &app.spectrum_peaks,
                    dt,
                );
            }
            SpectrumMode::Oscilloscope => {
                draw_oscilloscope(frame, area, &app.engine, pal, app.frame_tick);
            }
        },
    }
}
/// 文件浏览器面板：目录/音乐文件列表，↑↓ 选择、Enter 进入/播放，含内嵌搜索框。
fn draw_browser(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette, searching: bool) {
    let focused = matches!(app.focus, Panel::Browser);
    // 标题按面板宽度截断，避免深/长路径撑爆边框。
    let path_str = app.browser.cwd().display().to_string();
    let title = format!(
        " {} ",
        truncate_to_width(&path_str, (area.width as usize).saturating_sub(4))
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, focused))
        .title(title)
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 特殊状态优先：错误 / 空目录。
    if let Some(err) = app.browser.last_error() {
        let msg = Paragraph::new(format!("错误: {err}"))
            .style(pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD));
        frame.render_widget(msg, inner);
        return;
    }
    if app.browser.entries().is_empty() {
        let msg = Paragraph::new("（空目录）")
            .style(pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    // 搜索模式：顶部 1 行搜索输入框，剩余给列表。
    let list_area = if searching {
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        let prompt = if app.browser.search_truncated() {
            format!(
                "/ {}█  Esc 退出  目录过大，仅搜索前 2 万条",
                app.search_query
            )
        } else {
            format!("/ {}█  Esc 退出", app.search_query)
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                prompt,
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );
        chunks[1]
    } else {
        inner
    };

    let entries = app.browser.entries();
    let scroll = app.browser.scroll();
    let sel = app.browser.selected();
    let vis_h = list_area.height as usize;
    let w = list_area.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (i, entry) in entries.iter().enumerate().skip(scroll).take(vis_h) {
        let is_sel = i == sel;
        let label = match entry {
            Entry::Dir { name, .. } => format!("▸ {name}/"),
            Entry::File { name, .. } => format!("  {name}"),
        };
        let mut st = pal_style(pal.fg, pal.bg);
        if is_sel {
            st = st.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(Span::styled(pad_to_width(&label, w), st)));
    }
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// 多列播放列表面板：曲名 | 艺术家 | 时长（参照 foobar2000），含内嵌搜索框。
fn draw_playlist(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    pal: &Palette,
    rows: &[PlaylistRow],
    searching: bool,
) {
    let focused = matches!(app.focus, Panel::Playlist);
    let count = app.playlist.len();
    // 标题：搜索时显示 "匹配 N/M" 让用户知道过滤效果。
    let title = if searching && !app.search_query.is_empty() {
        format!(" 播放列表 (匹配 {}/{}) ", rows.len(), count)
    } else {
        format!(" 播放列表 · {count} 首 ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, focused))
        .title(title)
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if count == 0 {
        let hint = "空列表：在浏览器按 a 加入，或 Enter 直接播放";
        let st = pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM);
        frame.render_widget(
            Paragraph::new(hint).style(st).alignment(Alignment::Center),
            inner,
        );
        return;
    }

    // 搜索模式：顶部 1 行搜索输入框，剩余给列表。
    let list_area = if searching {
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        let prompt = format!("/ {}█  Esc 退出", app.search_query);
        frame.render_widget(
            Paragraph::new(Span::styled(
                prompt,
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );
        chunks[1]
    } else {
        inner
    };

    let w = list_area.width as usize;
    let vis_h = list_area.height as usize;
    // 列宽：▶(1)+空格 + 曲名 + 空格 + 艺术家 + 空格 + 时长(右对齐)。
    // 8 列容纳超 1 小时的 `H:MM:SS`（如 1:23:45），避免截成 `1:23:4`。
    let dur_w = 8usize;
    let icon_w = 2usize;
    let gaps = 3usize;
    let flexible = w.saturating_sub(icon_w + gaps + dur_w);
    let artist_w = flexible * 30 / 100;
    let title_w = flexible.saturating_sub(artist_w);

    let current = app.playlist.current_index();
    let sel_track = app.playlist.selected_track();
    let playing = app.engine.as_ref().is_some_and(|e| e.is_playing());

    let mut lines: Vec<Line> = Vec::new();
    for row in rows.iter().skip(app.playlist.scroll()).take(vis_h) {
        match row {
            PlaylistRow::AlbumHeader {
                album,
                track_indices,
            } => {
                let mark = if app.playlist.is_album_collapsed(album) {
                    "▸"
                } else {
                    "▾"
                };
                let label = format!("{mark} {album} ({} 首)", track_indices.len());
                let is_sel =
                    matches!(app.playlist.selected(), Some(Selection::Album(a)) if a == album);
                let mut st = pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD);
                if is_sel {
                    st = st.add_modifier(Modifier::REVERSED);
                }
                lines.push(Line::from(Span::styled(pad_to_width(&label, w), st)));
            }
            PlaylistRow::Track { item_index } => {
                let item = &app.playlist.items()[*item_index];
                let md = app.metadata_cache.get(&item.path);
                // CUE 分轨优先用 .cue 内的标题/表演者，时长取分轨区间（end-start），
                // 而非整轨元数据（否则所有分轨显示同一整轨标题与总时长）。
                let (title, artist, dur) = if let Some(cue) = &item.cue {
                    let a = cue
                        .performer
                        .clone()
                        .or_else(|| md.and_then(|m| m.artist.clone()))
                        .unwrap_or_default();
                    let start_s = cue.start_ms as f64 / 1000.0;
                    let d = match cue.end_ms {
                        Some(end) => fmt_time((end.saturating_sub(cue.start_ms)) as f64 / 1000.0),
                        None => md
                            .and_then(|m| m.duration)
                            .map(|d| fmt_time((d - start_s).max(0.0)))
                            .unwrap_or_default(),
                    };
                    (cue.title.clone(), a, d)
                } else {
                    let t = md
                        .and_then(|m| m.title.clone())
                        .unwrap_or_else(|| file_stem(&item.path));
                    let a = md.and_then(|m| m.artist.clone()).unwrap_or_default();
                    let d = md
                        .and_then(|m| m.duration)
                        .map(fmt_time)
                        .unwrap_or_default();
                    (t, a, d)
                };
                let is_current = current == Some(*item_index);
                let is_sel = sel_track == Some(*item_index);
                let icon = if is_current {
                    if playing {
                        "▶"
                    } else {
                        "‖"
                    }
                } else {
                    " "
                };
                let mut text = String::new();
                text.push_str(icon);
                text.push(' ');
                text.push_str(&pad_to_width(&title, title_w));
                text.push(' ');
                text.push_str(&pad_to_width(&artist, artist_w));
                text.push(' ');
                text.push_str(&pad_left_to_width(&dur, dur_w));
                let mut st = pal_style(pal.fg, pal.bg);
                if is_current {
                    st = st.add_modifier(Modifier::BOLD);
                }
                if is_sel {
                    st = st.add_modifier(Modifier::REVERSED);
                }
                lines.push(Line::from(Span::styled(text, st)));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// 歌词面板：当前行高亮居中，随播放进度滚动（卡拉 OK 式）。
fn draw_lyrics_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    lyrics_opt: Option<&lyrics::Lyrics>,
    pal: &Palette,
    lyrics_offset: f64,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 歌词 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(lyrics) = lyrics_opt.filter(|l| !l.is_empty()) else {
        let msg = Paragraph::new("（无歌词）\n放置同名 .lrc 或在标签内嵌歌词（USLT/LYRICS）可显示")
            .style(pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    };

    // 当前行：按播放位置定位，始终居中显示。
    let pos = engine.as_ref().map(|e| e.position()).unwrap_or(0.0);
    let current = lyrics.current_line(pos - lyrics_offset);
    let visible = inner.height as usize;
    let start = current.saturating_sub(visible / 2);
    let lines: Vec<Line> = lyrics
        .lines
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, line)| {
            if i == current {
                Line::from(Span::styled(
                    line.text.clone(),
                    pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    line.text.clone(),
                    pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM),
                ))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}
/// 专辑封面面板：halfblock 半块字符渲染（1 字符 = 2 垂直像素，近似正方形）。
///
/// 占据左侧面板位置，`c` 键切换。无图时居中提示。
fn draw_cover_panel(frame: &mut ratatui::Frame, area: Rect, app: &mut App, pal: &Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 专辑封面 · 按 c 隐藏 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 2 || inner.height < 2 {
        return;
    }

    // 像素区：halfblock 让 1 字符 = 2 垂直像素。
    let pixel_w = inner.width as u32;
    let pixel_h = (inner.height as u32) * 2;

    // 确保缩略图就绪（按路径 + 目标像素区缓存，避免每帧 resize），再取只读借用。
    app.ensure_cover_thumb(pixel_w, pixel_h);
    let Some((dst_w, dst_h, rgba)) = app.cover_thumb() else {
        let msg = Paragraph::new("（无封面）\n按 c 隐藏")
            .style(pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    };

    // 图像居中放进 inner。
    let x_offset = ((inner.width as u32).saturating_sub(dst_w)) / 2;
    let y_offset = (inner.height as u32).saturating_sub(dst_h.div_ceil(2)) / 2;

    // 逐行扫描：每 2 行像素 = 1 行字符（上半前景、下半背景）。
    let lines: Vec<Line> = (0..inner.height as u32)
        .map(|row| {
            let mut spans: Vec<Span> = Vec::new();
            for _ in 0..x_offset {
                spans.push(Span::raw(" "));
            }
            let py_top = row.checked_sub(y_offset).map_or(dst_h, |r| r * 2);
            let py_bot = py_top + 1;
            if py_top < dst_h {
                for px in 0..dst_w {
                    let [r1, g1, b1, a1] = rgba.get_pixel(px, py_top).0;
                    let top_color = if a1 > 128 {
                        Color::Rgb(r1, g1, b1)
                    } else {
                        Color::Black
                    };
                    let bot_color = if py_bot < dst_h {
                        let [r2, g2, b2, a2] = rgba.get_pixel(px, py_bot).0;
                        if a2 > 128 {
                            Color::Rgb(r2, g2, b2)
                        } else {
                            Color::Black
                        }
                    } else {
                        Color::Black
                    };
                    // 恒用 ▀：图像奇数高时末行下半越界，仍画 ▀、下半取背景色。
                    let ch = "▀";
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(top_color).bg(bot_color),
                    ));
                }
            }
            let drawn = x_offset + dst_w;
            for _ in drawn..inner.width as u32 {
                spans.push(Span::raw(" "));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 封面网格浏览：按专辑去重，每格显示封面缩略图 + 专辑名，选中高亮。
/// Enter 播放选中专辑第一首；↑↓/jk 移动；PageUp/PageDown 翻页。
fn draw_cover_browser(frame: &mut ratatui::Frame, area: Rect, app: &mut App, pal: &Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 封面浏览 · ↑↓ 选择 · PgUp/PgDn 翻页 · Enter 播放 · c 隐藏 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let albums = app.cover_browser_albums();
    if albums.is_empty() {
        let msg = Paragraph::new("（播放列表为空）\n按 c 隐藏")
            .style(pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    }

    // 网格：每格宽 14 字符（封面缩略 14×3 行 = 14×6 像素 + 专辑名 1 行）。
    const CELL_W: u16 = 14;
    const COVER_H: u16 = 3;
    let cell_h = COVER_H + 1;
    let cols = (inner.width / CELL_W).max(1) as usize;
    let rows = (inner.height / cell_h).max(1) as usize;
    let visible = cols * rows;
    app.cover_browser_visible = visible;
    let start = app.cover_browser_scroll.min(albums.len().saturating_sub(1));
    let end = (start + visible).min(albums.len());
    let base = pal_style(pal.fg, pal.bg);

    for (i, (album, path)) in albums[start..end].iter().enumerate() {
        let idx = start + i;
        let row = (i / cols) as u16;
        let col = (i % cols) as u16;
        let cell_x = inner.x + col * CELL_W;
        let cell_y = inner.y + row * cell_h;
        let selected = idx == app.cover_browser_sel;

        // 封面缩略图（halfblock）：按路径 + 目标尺寸取缓存缩略图（首帧解码后复用）。
        let thumb = app.cover_grid_thumb(path, CELL_W as u32, (COVER_H as u32) * 2);
        if let Some(rgba) = thumb {
            let (dw, dh) = (rgba.width(), rgba.height());
            // 封面水平居中（字符列）；halfblock 让 dh 像素 = dh.div_ceil(2) 字符行，
            // 再在 COVER_H 行内垂直居中。
            let cover_x = (CELL_W as u32).saturating_sub(dw) / 2;
            let cover_rows = dh.div_ceil(2);
            let cover_y = (COVER_H as u32).saturating_sub(cover_rows) / 2;
            for cy in 0..COVER_H {
                let mut spans: Vec<Span> = Vec::new();
                for _ in 0..cover_x {
                    spans.push(Span::raw(" "));
                }
                if (cy as u32) < cover_y {
                    // 顶部留白行：整行空白，不读图（原 saturating_sub 会饱和到 0，
                    // 导致留白行误画封面首两行像素）。
                    for _ in 0..dw {
                        spans.push(Span::raw(" "));
                    }
                } else {
                    let rel = (cy as u32) - cover_y;
                    let py_top = rel * 2;
                    let py_bot = py_top + 1;
                    for px in 0..dw {
                        let top = if py_top < dh {
                            let [r, g, b, a] = rgba.get_pixel(px, py_top).0;
                            if a > 128 {
                                Some(Color::Rgb(r, g, b))
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        let bot = if py_bot < dh {
                            let [r, g, b, a] = rgba.get_pixel(px, py_bot).0;
                            if a > 128 {
                                Some(Color::Rgb(r, g, b))
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        // 恒用 ▀（上半=顶像素，下半=底像素或背景）；奇数高封面末行
                        // 底像素缺失时下半画背景色，不再丢失顶像素。
                        let mut st = Style::default();
                        if let Some(c) = top {
                            st = st.fg(c);
                        } else {
                            st = st.fg(Color::Black);
                        }
                        if let Some(c) = bot {
                            st = st.bg(c);
                        } else {
                            st = st.bg(Color::Black);
                        }
                        spans.push(Span::styled("▀".to_string(), st));
                    }
                }
                let area = Rect {
                    x: cell_x,
                    y: cell_y + cy,
                    width: CELL_W,
                    height: 1,
                };
                frame.render_widget(Paragraph::new(Line::from(spans)), area);
            }
        }

        // 专辑名（截断到 CELL_W，选中反色高亮）。
        let name = truncate_to_width(album, CELL_W as usize);
        let name_area = Rect {
            x: cell_x,
            y: cell_y + COVER_H,
            width: CELL_W,
            height: 1,
        };
        let name_style = if selected {
            pal_style(pal.menu_bg, pal.menu_fg)
        } else {
            base
        };
        frame.render_widget(Paragraph::new(Span::styled(name, name_style)), name_area);
    }
}
/// 纯字符进度条：`━━━●━━━`。已播放段 `━`、游标 `●`、未播放段 `─`。
fn draw_progress_chars(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let pos = (ratio.clamp(0.0, 1.0) * width as f64) as usize;
    let pos = pos.min(width);
    let mut s = String::with_capacity(width);
    for i in 0..width {
        if i < pos {
            s.push('━');
        } else if i == pos {
            s.push('●');
        } else {
            s.push('─');
        }
    }
    s
}

/// 当前曲目框：曲名 / 演唱者 / 专辑 / 技术参数。
///
/// 曲名加粗醒目；技术参数按直通状态亮/暗着色；未播放时居中提示。
/// 与状态栏分工：曲名/演唱者只在这里显示，状态栏只放进度/时间/音量。
fn draw_now_playing(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 当前曲目 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let base = pal_style(pal.fg, pal.bg);
    let Some(md) = app.current_metadata.as_ref() else {
        let hint = Paragraph::new("（未播放）")
            .style(base)
            .alignment(Alignment::Center);
        frame.render_widget(hint, inner);
        return;
    };

    // 两栏：左曲目信息（70%）+ 右实时电平（30%）。
    let columns =
        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)]).split(inner);
    let info_area = columns[0];

    let title = md.title.clone().unwrap_or_else(|| "未知曲目".to_string());
    let artist = md.artist.clone().unwrap_or_else(|| "未知艺人".to_string());
    let album_line = match (&md.album, md.track_number) {
        (Some(a), Some(n)) => format!("{a} · 曲目 {n}"),
        (Some(a), None) => a.clone(),
        (None, Some(n)) => format!("曲目 {n}"),
        (None, None) => "未知专辑".to_string(),
    };
    let tech = format!(
        "{} · {} · {} · {}",
        md.codec.as_deref().unwrap_or("未知格式"),
        md.bitrate_label(),
        md.sample_rate_label(),
        md.bits_label(),
    );
    let bitstream = app.engine.as_ref().is_some_and(|e| e.bitstream());
    // 直通/降级用独立语义色：直通亮（白）、降级暗（深灰），
    // 任何主题下都清晰可辨，不与装饰色联动。
    let tech_style = if bitstream {
        pal_style(Some(pal.passthrough_fg), pal.bg)
    } else {
        pal_style(Some(pal.resample_fg), pal.bg)
    };

    let playing = app.engine.as_ref().is_some_and(|e| e.is_playing());
    let status = if playing { "[播] " } else { "[停] " };
    let status_color = if playing {
        Color::Green
    } else {
        Color::DarkGray
    };
    // 按左栏宽截断，避免超长标题换行撑破固定四行布局。
    let w = info_area.width as usize;
    let title_w = w.saturating_sub(UnicodeWidthStr::width(status));
    let mut lines = vec![
        Line::from(Span::styled(
            truncate_to_width(&title, title_w),
            base.add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            truncate_to_width(&format!("歌手: {artist}"), w),
            base,
        )),
        Line::from(Span::styled(
            truncate_to_width(&format!("专辑: {album_line}"), w),
            base,
        )),
        Line::from(Span::styled(truncate_to_width(&tech, w), tech_style)),
    ];
    if let Some(first) = lines.first_mut() {
        first
            .spans
            .insert(0, Span::styled(status, Style::default().fg(status_color)));
    }
    frame.render_widget(Paragraph::new(lines), info_area);
    // 右栏：实时电平表。
    draw_level_meter(frame, columns[1], &app.engine, app.frame_tick, pal);
}

/// 状态栏：播放图标 + 进度条 + 时间/音量/循环/随机。
fn draw_status_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    config: &Config,
    pal: &Palette,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(pal_style(pal.border, None))
        .title(" 状态 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 命令模式：输入框接管状态栏（`: 命令█  Enter 执行  Esc 取消`）。
    if app.command_mode {
        let prompt = format!(": {}█  Enter 执行  Esc 取消", app.command_query);
        let st = pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(truncate_to_width(&prompt, inner.width as usize)).style(st),
            inner,
        );
        return;
    }

    // 瞬时提示（引擎错误 / 操作反馈）优先显示。
    if let Some(err) = &app.last_error {
        // 错误提示用红色（语义色），与装饰字色解耦。
        let st = pal_style(Some(Color::Red), pal.bg).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(truncate_to_width(err, inner.width as usize)).style(st),
            inner,
        );
        return;
    }

    let playing = app.engine.as_ref().is_some_and(|e| e.is_playing());
    // 文本状态标记（与 tuneux 一致）：避免 Emoji 宽度在不同终端不一致导致错位。
    let icon = if playing { "[播]" } else { "[停]" };
    // 分轨内进度（CUE 分轨按区间换算）。
    let (pos, dur) = match app.engine.as_ref() {
        Some(engine) => track_progress(
            engine,
            Some(engine.duration()),
            app.playlist
                .current_index()
                .and_then(|i| app.playlist.items().get(i))
                .and_then(|it| it.cue.as_ref()),
        ),
        None => (0.0, 0.0),
    };
    let vol = app
        .engine
        .as_ref()
        .map(|e| (e.volume() * 100.0).round() as u32)
        .unwrap_or(0);
    let time_str = if dur > 0.0 {
        format!("{}/{}", fmt_time(pos), fmt_time(dur))
    } else {
        "--:--".to_string()
    };
    let ratio = if dur > 0.0 {
        (pos / dur).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let shuffle_s = if app.playlist.is_shuffle() {
        " 随机"
    } else {
        ""
    };
    let right_main = format!(
        "{time_str}  音量{vol}%  {}{}",
        config.repeat.label(),
        shuffle_s
    );
    let base_style = pal_style(pal.fg, pal.bg);

    // 三段：图标 | 进度条 | 右段（时间/音量/循环）。
    // 曲名/演唱者已移到顶部"当前曲目"框，状态栏不再重复显示。
    let icon_w = UnicodeWidthStr::width(icon) as u16 + 1;
    let right_w = UnicodeWidthStr::width(right_main.as_str()) as u16;
    let chunks = Layout::horizontal([
        Constraint::Length(icon_w),
        Constraint::Min(0),
        Constraint::Length(right_w),
    ])
    .split(inner);

    let icon_style = if playing {
        pal_style(Some(Color::Green), pal.bg)
    } else {
        pal_style(Some(Color::DarkGray), pal.bg)
    };
    frame.render_widget(Paragraph::new(Span::styled(icon, icon_style)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Span::styled(
            draw_progress_chars(ratio, chunks[1].width as usize),
            base_style,
        )),
        chunks[1],
    );
    let right_spans = vec![Span::styled(right_main, base_style)];
    frame.render_widget(
        Paragraph::new(Line::from(right_spans)).alignment(Alignment::Right),
        chunks[2],
    );
}

/// 功能键栏（最底 1 行）：数字菜单 + 功能键提示。
fn draw_fkey_bar(frame: &mut ratatui::Frame, area: Rect, pal: &Palette) {
    const KEYS: &[(&str, &str)] = &[
        ("1-8", "菜单"),
        ("F2", "配色"),
        ("F3", "打开"),
        ("F4", "目录"),
        ("F5-F8", "面板"),
        ("F9", "均衡器"),
        ("?", "帮助"),
    ];
    match pal.fkey_num {
        None => {
            let text = " 1-8菜单  F2配色  F3打开  F4目录  F5-F8面板  F9均衡器  ?帮助 ";
            frame.render_widget(
                Paragraph::new(text).style(Style::default().add_modifier(Modifier::DIM)),
                area,
            );
        }
        Some(num_color) => {
            let mut spans: Vec<Span> = Vec::new();
            spans.push(Span::styled(" ", pal_style(None, pal.bg)));
            for (k, label) in KEYS {
                spans.push(Span::styled(
                    (*k).to_string(),
                    pal_style(Some(num_color), pal.bg).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    (*label).to_string(),
                    pal_style(pal.fg, pal.bg),
                ));
                spans.push(Span::styled("   ", pal_style(None, pal.bg)));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)).style(pal_style(None, pal.bg)),
                area,
            );
        }
    }
}
