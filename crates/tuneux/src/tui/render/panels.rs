//! # 面板绘制（文件浏览器 / 播放列表 / 歌词 / 专辑封面）
//!
//! 职责：主区与左侧面板的所有内容型组件——
//! - `draw_browser`：文件浏览器面板（含内嵌搜索输入框）；
//! - `draw_playlist`：播放列表面板（平铺/按专辑分组两种视图，
//!   含内嵌搜索输入框）；
//! - `draw_lyrics_panel`：歌词面板（当前行居中高亮，随播放滚动）；
//! - `draw_cover_panel`：专辑封面面板（halfblock 半块字符渲染）；
//! - 私有辅助：`album_artists` / `album_total_duration`（播放列表分组
//!   视图的组头信息）；时长格式化用父模块共享的 `fmt_time`。
//!
//! 说明：浏览器/播放列表的搜索输入框不是独立弹窗，而是内嵌在
//! 各面板顶部的 1 行，因此跟随所属面板放在本模块（而非 popup.rs）。

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::fs_browser;
use crate::playlist;
use crate::tui::palette;
use tuneux_corex as audio;
use tuneux_mediax::lyrics;
use tuneux_mediax::metadata;

/// 在左侧面板渲染专辑封面图。
///
/// 占据主区左侧（原本文件浏览器的位置），由 `c` 键切换显示/隐藏。
///
/// **策略**：halfblock 字符（`▀` U+2580）配前景/背景色 = 1 字符显示 2 像素。
/// 终端字符宽高比约 2:1（宽：高），所以这种"双高像素"近似正方形。
///
/// 缩放由调用方按面板像素区准备（`ensure_cover_thumb` 缓存命中即跳过
/// 逐帧 resize），本函数只做 halfblock 逐行扫描渲染：每 2 个垂直像素 =
/// 1 个字符（上半 = 前景色，下半 = 背景色）。
///
/// 无图（None）或解码失败：居中显示"（无封面）"，提示按 c 隐藏。
/// 封面像素取样（含主题处理）：暗屏主题（dim_screen）先做十字邻域轻模糊
/// 模拟反射式 TFT 的柔边，再经 cover_pixel 抬高黑位、降低对比和饱和度；其余主题直接取色。
/// 越界 / 透明像素按 cover_pixel 的透明口径处理。
fn cover_px(rgba: &image::RgbaImage, px: u32, py: u32, w: u32, h: u32) -> Color {
    if py >= h || px >= w {
        return palette::cover_pixel(0, 0, 0, 0);
    }
    let [cr, cg, cb, ca] = rgba.get_pixel(px, py).0;
    if ca <= 128 {
        return palette::cover_pixel(0, 0, 0, 0);
    }
    if palette::pal().dim_screen {
        let (mut sr, mut sg, mut sb, mut n) = (0u32, 0u32, 0u32, 0u32);
        for (dx, dy) in [(0i32, 0i32), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            let x = px as i32 + dx;
            let y = py as i32 + dy;
            if x >= 0 && x < w as i32 && y >= 0 && y < h as i32 {
                let [r, g, b, a] = rgba.get_pixel(x as u32, y as u32).0;
                if a > 128 {
                    sr += r as u32;
                    sg += g as u32;
                    sb += b as u32;
                    n += 1;
                }
            }
        }
        if n > 0 {
            return palette::cover_pixel(
                sr.checked_div(n).unwrap_or_default() as u8,
                sg.checked_div(n).unwrap_or_default() as u8,
                sb.checked_div(n).unwrap_or_default() as u8,
                255,
            );
        }
        return palette::cover_pixel(0, 0, 0, 0);
    }
    palette::cover_pixel(cr, cg, cb, ca)
}

pub(super) fn draw_cover_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    thumb: Option<(u32, u32, &image::RgbaImage)>,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} · c ", i18n.t("panel.cover")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 内框太小：直接退出
    if inner.width < 2 || inner.height < 2 {
        return;
    }

    // 缩略图由调用方按本面板内框像素区备好（ensure_cover_thumb 缓存命中
    // 不做逐帧 resize），这里只做 halfblock 逐行渲染。
    let Some((dst_w, dst_h, rgba)) = thumb else {
        // 无封面：居中显示灰色提示
        let msg = Paragraph::new(i18n.t("empty.cover").into_owned())
            .style(Style::default().fg(palette::pal().dim))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    };

    // 把图像居中放进 inner（水平、垂直都居中：可用空间两侧均分）
    let x_offset = ((inner.width as u32).saturating_sub(dst_w)) / 2;
    // 图像占 dst_h.div_ceil(2) 个字符行（2 像素行 = 1 字符行）；
    // 垂直居中：可用行数两侧均分（除以 2）
    let y_offset = (inner.height as u32).saturating_sub(dst_h.div_ceil(2)) / 2;

    // 逐行扫描：每 2 行像素 = 1 行字符
    let lines: Vec<Line> = (0..inner.height as u32)
        .map(|row| {
            let mut spans: Vec<Span> = Vec::new();
            // 前 x_offset 空格（左侧居中）
            for _ in 0..x_offset {
                spans.push(Span::raw(" "));
            }
            // 该行对应原始像素 y = (row - y_offset) * 2。
            // row < y_offset 是上方留白：checked_sub 得 None 时置为 dst_h，
            // 让下方 if 不成立（留白行不渲染图像）。
            let py_top = row.checked_sub(y_offset).map_or(dst_h, |r| r * 2);
            let py_bot = py_top + 1;
            if py_top < dst_h {
                for px in 0..dst_w {
                    // 经 cover_px：单色映射 / 暗屏压暗 + 轻模糊，透明落底色。
                    let top_color = cover_px(rgba, px, py_top, dst_w, dst_h);
                    // 恒用 ▀（上半块）：图像奇数高时末行下半越界，仍画 ▀、
                    // 下半取背景色，不丢最后一像素行。
                    let ch = "▀";
                    let bot_color = cover_px(rgba, px, py_bot, dst_w, dst_h);
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(top_color).bg(bot_color),
                    ));
                }
            }
            // 后填充空格（右对齐）
            let drawn = x_offset + dst_w;
            for _ in drawn..inner.width as u32 {
                spans.push(Span::raw(" "));
            }
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// 文件浏览器面板。
///
/// # 视觉规则
/// - **标题**显示当前目录路径，让用户始终知道自己在哪；
/// - **焦点反馈**：`focused=true`（当前操作此面板）时标题转黄色，否则青色；
/// - **目录**加 `▸` 前缀并染蓝，与文件视觉区分（目录可进入、文件可播放）；
/// - **选中项**反白高亮（LightCyan 底黑字加粗），无论是否聚焦都显示，
///   方便用户切焦点回来仍能看到上次选到哪；
/// - **错误/空目录**特殊提示，避免误以为"渲染崩了"。
///
/// # 滚动
/// `browser.scroll()` 是首行对应的条目索引（在事件循环 ensure_visible
/// 时算好），这里 `skip(scroll)` 跳过不可见部分，实现长列表滚动。
pub(super) fn draw_browser(
    frame: &mut ratatui::Frame,
    area: Rect,
    browser: &fs_browser::FsBrowser,
    search_mode: bool,
    search_query: &str,
    focused: bool,
    i18n: &tuneux_commonx::I18n,
) {
    let title_color = if focused {
        palette::pal().title_focus
    } else {
        palette::pal().chrome
    };
    let title = format!(
        " {} · {} ",
        i18n.t("panel.browser"),
        browser.cwd().display()
    );
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 三种特殊状态优先处理：错误 / 空目录 / 正常列表
    if let Some(err) = browser.last_error() {
        let msg = Paragraph::new(format!("{}: {err}", i18n.t("msg.error")))
            .style(Style::default().fg(palette::error_fg()));
        frame.render_widget(msg, inner);
        return;
    }
    // 空目录 / 搜索零匹配的提示移到搜索框渲染**之后**（见下方）：搜索态下
    // entries 是过滤结果，零匹配时同样为空；若在此提前 return，搜索输入框与
    // 「Esc 退出」提示会被一起顶掉，而按键仍被 handle_key 的搜索分支全量吞掉。

    // 搜索模式下：顶部 1 行给搜索输入框，剩余给列表
    let list_area = if search_mode {
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        // 搜索框："/ " 前缀 + query + 光标 + Esc 退出提示；
        // 截断时追加提示，让用户知道大目录下只搜索了前 2 万条。
        let prompt = if browser.search_truncated() {
            format!(
                "/ {}█  {}  {}",
                search_query,
                i18n.t("search.esc_exit"),
                i18n.t("search.truncated")
            )
        } else {
            format!("/ {}█  {}", search_query, i18n.t("search.esc_exit"))
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                prompt,
                Style::default()
                    .fg(palette::pal().title_fg.unwrap_or(Color::Yellow))
                    .add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );
        chunks[1]
    } else {
        inner
    };

    if browser.entries().is_empty() {
        // 空目录（未搜索）或搜索零匹配：提示画在 list_area 内——搜索框已在
        // 上方渲染，不会再被这条提示遮住。
        let msg = if search_mode {
            i18n.t("search.no_match").into_owned()
        } else {
            i18n.t("empty.dir").into_owned()
        };
        let msg = Paragraph::new(msg)
            .style(Style::default().fg(palette::pal().dim))
            .alignment(Alignment::Center);
        frame.render_widget(msg, list_area);
        return;
    }

    // 正常列表：entries 本身就是当前显示列表（搜索时为递归过滤结果），
    // 从 scroll 偏移开始渲染，最多画可视行数。
    let scroll = browser.scroll();
    let selected = browser.selected();
    let visible_rows = list_area.height as usize;
    let lines: Vec<Line> = browser
        .entries()
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_rows)
        .map(|(idx, entry)| {
            let prefix = if entry.is_dir() { "▸ " } else { "  " };
            let text = format!("{prefix}{}", entry.name());
            if idx == selected {
                // 选中项：反白高亮
                Line::from(Span::styled(
                    text,
                    Style::default()
                        .bg(palette::pal().sel_bg)
                        .fg(palette::pal().sel_fg.unwrap_or(Color::Black))
                        .add_modifier(Modifier::BOLD),
                ))
            } else if entry.is_dir() {
                // 目录：蓝色，与前缀 ▸ 一起标识可进入
                Line::from(Span::styled(text, Style::default().fg(palette::pal().info)))
            } else {
                Line::from(Span::raw(text))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// 组头歌手：收集专辑内所有曲目的 artist（去重、去空、保持曲序）。
fn album_artists(
    cache: &std::collections::HashMap<std::path::PathBuf, metadata::TrackMetadata>,
    playlist: &playlist::Playlist,
    track_indices: &[usize],
) -> Vec<String> {
    let mut artists: Vec<String> = Vec::new();
    for &i in track_indices {
        if let Some(item) = playlist.items().get(i) {
            if let Some(md) = cache.get(&item.path) {
                if let Some(a) = &md.artist {
                    if !a.is_empty() && !artists.contains(a) {
                        artists.push(a.clone());
                    }
                }
            }
        }
    }
    artists
}

/// 组头总时长：累加专辑内所有曲目的时长（秒）。
fn album_total_duration(
    cache: &std::collections::HashMap<std::path::PathBuf, metadata::TrackMetadata>,
    playlist: &playlist::Playlist,
    track_indices: &[usize],
) -> f64 {
    let mut total = 0.0;
    for &i in track_indices {
        if let Some(item) = playlist.items().get(i) {
            if let Some(md) = cache.get(&item.path) {
                total += md.duration.unwrap_or(0.0);
            }
        }
    }
    total
}

/// 播放列表面板。
///
/// # 两种视图
/// - **平铺（Flat）**：`序号. 歌名 - 歌手`，序号按显示顺序（1 起）；
/// - **按专辑（ByAlbum）**：组头 `▸/▾ 专辑名 - 歌手 (N首) (总时间)`，
///   曲目缩进 4 格，组头可折叠/展开。
///
/// # 元数据降级
/// 标题/艺术家优先从 `metadata_cache` 取（同一首只 probe 一次）。
/// - 缓存命中但字段缺失：标题降级为文件名，艺术家显示i18n.t("group.unknown_artist")；
/// - 缓存未命中（极少，理论不应发生，因为加入列表时已 probe）：
///   标题降级为文件名，艺术家留空。
///
/// # 视觉区分
/// - 当前播放项（`▶`）：绿色加粗——即使不在选中状态也醒目；
/// - 面板选中项 + 焦点在 Playlist：反白高亮；
/// - 组头：青色（选中时反白）；
/// - 普通项：默认色。
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_playlist(
    frame: &mut ratatui::Frame,
    area: Rect,
    playlist: &playlist::Playlist,
    cache: &std::collections::HashMap<std::path::PathBuf, metadata::TrackMetadata>,
    engine: &Option<audio::Engine>,
    focus: playlist::Panel,
    rows: &[playlist::PlaylistRow],
    search_mode: bool,
    search_query: &str,
    i18n: &tuneux_commonx::I18n,
) {
    let focused = focus == playlist::Panel::Playlist;
    let title_color = if focused {
        palette::pal().title_focus
    } else {
        palette::pal().chrome
    };
    // 标题：搜索时显示 "匹配 N/M" 让用户知道过滤效果
    let title = if search_mode && !search_query.is_empty() {
        format!(
            " {} ({}/{}) ",
            i18n.t("panel.playlist"),
            rows.len(),
            playlist.len()
        )
    } else {
        format!(" {} ({}) ", i18n.t("panel.playlist"), playlist.len())
    };
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default()
            .fg(title_color)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 空列表 / 搜索零匹配的提示移到搜索框渲染**之后**（见下方）：若在此提前
    // return，空列表下按 `/` 进入搜索态时输入框与「Esc 退出」提示不可见，而
    // handle_key 的搜索分支仍会吞掉全部按键（隐形模态态，连 q 都出不去）。

    // 搜索模式下：顶部 1 行给搜索输入框，剩余给列表
    let list_area = if search_mode {
        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
        // 输入框："/ " 前缀 + query + 闪烁光标 + Esc 退出提示
        let prompt = format!("/ {}█  {}", search_query, i18n.t("search.esc_exit"));
        frame.render_widget(
            Paragraph::new(Span::styled(
                prompt,
                Style::default()
                    .fg(palette::pal().title_fg.unwrap_or(Color::Yellow))
                    .add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );
        chunks[1]
    } else {
        inner
    };

    // 无可渲染行 = 列表为空（未搜索）或搜索零匹配——两者都是 rows 为空，
    // 提示画在 list_area 内，搜索框已在上面占掉 1 行，不会被遮住。
    if rows.is_empty() {
        let msg = if search_mode {
            i18n.t("search.no_match").into_owned()
        } else {
            i18n.t("empty.playlist").into_owned()
        };
        let msg = Paragraph::new(msg)
            .style(Style::default().fg(palette::pal().dim))
            .alignment(Alignment::Center);
        frame.render_widget(msg, list_area);
        return;
    }

    // 分组视图（rows 含组头）时曲目缩进（列宽计算要用，先判定）
    let grouped = rows
        .iter()
        .any(|r| matches!(r, playlist::PlaylistRow::AlbumHeader { .. }));

    // 列布局宽度（仿 fx 同款口径）：▶(1)+空格 + 序号(右对齐+.) + 空格 + 歌名 + 空格
    // + 表演者 + 空格 + 时长(右对齐 8 列)。窄面板时表演者列先收缩。
    let w = list_area.width as usize;
    let dur_w = 8usize;
    let icon_w = 2usize;
    let digits = rows.len().to_string().len().max(1);
    let num_w = digits + 1;
    let indent_w = if grouped { 4 } else { 0 };
    let gaps = 4usize;
    let flexible = w.saturating_sub(icon_w + num_w + gaps + dur_w + indent_w);
    let artist_w = flexible * 30 / 100;
    let title_w = flexible.saturating_sub(artist_w);

    let current = playlist.current_index();
    let selected = playlist.selected();

    // 滚动：按 playlist.scroll() 跳过不可见行，且最多只渲染可视高度。
    // scroll 由事件循环的 ensure_visible 维护，保证选中项在可视区内。
    let scroll = playlist.scroll();
    let visible_rows = list_area.height as usize;

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(scroll) // 跳过滚动偏移之前的行
        .take(visible_rows) // 只渲染可视区能容纳的行数
        .map(|(row_idx, row)| match row {
            playlist::PlaylistRow::AlbumHeader {
                album,
                track_indices,
            } => {
                // 组头：▸/▾ 专辑名 - 歌手 (N首) (总时间)
                let arrow = if playlist.is_album_collapsed(album) {
                    "▸"
                } else {
                    "▾"
                };
                let artists = album_artists(cache, playlist, track_indices);
                let artist_label = if artists.len() > 3 {
                    i18n.t("group.various").to_string()
                } else if artists.is_empty() {
                    i18n.t("group.unknown_artist").to_string()
                } else {
                    artists.join("、")
                };
                let total = album_total_duration(cache, playlist, track_indices);
                // 空专辑名 = 分组哨兵：显示层翻译为「未知专辑」（语言切换不漂移）。
                let album_label = if album.is_empty() {
                    i18n.t("group.unknown_album")
                } else {
                    std::borrow::Cow::Borrowed(album.as_str())
                };
                let count_label = i18n
                    .t("group.track_count")
                    .replace("{}", &track_indices.len().to_string());
                let text = format!(
                    "{arrow} {album_label} - {artist_label} ({count_label}) ({})",
                    super::fmt_time(total),
                );
                let is_selected =
                    matches!(selected, Some(playlist::Selection::Album(a)) if a == album);
                if is_selected && focused {
                    // 选中组头：反白
                    Line::from(Span::styled(
                        text,
                        Style::default()
                            .bg(palette::pal().sel_bg)
                            .fg(palette::pal().sel_fg.unwrap_or(Color::Black))
                            .add_modifier(Modifier::BOLD),
                    ))
                } else {
                    // 组头：青色
                    Line::from(Span::styled(
                        text,
                        Style::default().fg(palette::pal().chrome),
                    ))
                }
            }
            playlist::PlaylistRow::Track { item_index } => {
                let item = &playlist.items()[*item_index];
                // CUE 分轨曲目：标题用 .cue 的 TITLE，歌手优先 .cue 的 PERFORMER，
                // 缺失时回退整轨元数据
                let (title, artist) = if let Some(cue) = &item.cue {
                    let artist = cue
                        .performer
                        .clone()
                        .or_else(|| cache.get(&item.path).and_then(|md| md.artist.clone()))
                        .unwrap_or_default();
                    (cue.title.clone(), artist)
                } else {
                    // 普通曲目：取标题/艺术家，优先元数据缓存，降级文件名
                    match cache.get(&item.path) {
                        Some(md) => (
                            md.title.clone().unwrap_or_else(|| {
                                item.path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_default()
                            }),
                            md.artist.clone().unwrap_or_default(),
                        ),
                        None => (
                            // 缓存未命中：标题降级文件名，艺术家留空
                            item.path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(|s| s.to_string())
                                .unwrap_or_default(),
                            String::new(),
                        ),
                    }
                };
                // "歌名 | 艺术家 | 时长" 多列布局（仿 fx 同款列宽与截断口径）
                let dur = if let Some(cue) = &item.cue {
                    match cue.end_ms {
                        Some(end) => {
                            super::fmt_time((end.saturating_sub(cue.start_ms)) as f64 / 1000.0)
                        }
                        None => cache
                            .get(&item.path)
                            .and_then(|md| md.duration)
                            .map(|d| super::fmt_time((d - cue.start_ms as f64 / 1000.0).max(0.0)))
                            .unwrap_or_default(),
                    }
                } else {
                    cache
                        .get(&item.path)
                        .and_then(|md| md.duration)
                        .map(super::fmt_time)
                        .unwrap_or_default()
                };

                // 序号：平铺 = 大排行，分组 = track_number（专辑内序号）。
                let is_current = Some(*item_index) == current;
                let prefix = if is_current {
                    if engine.as_ref().is_some_and(|e| e.is_playing()) {
                        "▶"
                    } else {
                        "‖"
                    }
                } else {
                    " "
                };
                let seq_str = if grouped {
                    match item.track_number {
                        Some(n) => format!("{n}."),
                        None => String::new(),
                    }
                } else {
                    format!("{}.", row_idx + 1)
                };

                let indent = if grouped { "    " } else { "" };
                let title_tr = tuneux_displayx::truncate_to_width(&title, title_w);
                let artist_tr = tuneux_displayx::truncate_to_width(&artist, artist_w);
                let text = format!(
                    "{indent}{prefix} {} {} {} {}",
                    tuneux_displayx::pad_left_to_width(&seq_str, num_w),
                    tuneux_displayx::pad_to_width(&title_tr, title_w),
                    tuneux_displayx::pad_to_width(&artist_tr, artist_w),
                    tuneux_displayx::pad_left_to_width(&dur, dur_w),
                );

                let is_selected =
                    matches!(selected, Some(playlist::Selection::Track(i)) if *i == *item_index);
                if is_selected && focused {
                    // 焦点选中项：反白
                    Line::from(Span::styled(
                        text,
                        Style::default()
                            .bg(palette::pal().sel_bg)
                            .fg(palette::pal().sel_fg.unwrap_or(Color::Black))
                            .add_modifier(Modifier::BOLD),
                    ))
                } else if is_current {
                    // 当前播放项：绿色加粗（不依赖焦点）
                    Line::from(Span::styled(
                        text,
                        Style::default()
                            .fg(palette::pal().ok)
                            .add_modifier(Modifier::BOLD),
                    ))
                } else {
                    Line::from(Span::raw(text))
                }
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), list_area);
}

/// 歌词面板：当前行居中高亮，随播放进度滚动。
///
/// - 当前行：黄色加粗；其他行：暗灰；
/// - 无歌词：居中提示「（无歌词）」；
/// - 当前行位置由 `engine.position()` 计算，始终居中显示（卡拉 OK 式）。
pub(super) fn draw_lyrics_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    lyrics_opt: Option<&lyrics::Lyrics>,
    cue: Option<&tuneux_mediax::CueRef>,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.lyrics")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(lyrics) = lyrics_opt.filter(|l| !l.is_empty()) else {
        let msg = Paragraph::new(i18n.t("empty.lyrics").into_owned())
            .style(Style::default().fg(palette::pal().dim))
            .alignment(Alignment::Center);
        frame.render_widget(msg, inner);
        return;
    };

    // CUE 分轨的歌词时间以分轨起点为零点。
    let pos = engine.as_ref().map(|e| e.position()).unwrap_or(0.0);
    let current = lyrics.current_line(tuneux_mediax::cue_relative_position(pos, cue));
    let visible = inner.height as usize;

    // 当前行居中：起始行 = current - 可视行数/2
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
                    Style::default()
                        .fg(palette::pal().title_fg.unwrap_or(Color::Yellow))
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(
                    line.text.clone(),
                    Style::default().fg(palette::pal().dim),
                ))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 按显示宽度截断字符串（CJK 字符算 2 单元），与封面网格单元格宽度对齐。
fn truncate_to_width(s: &str, max_w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
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

/// 封面网格浏览：按专辑去重，每格显示封面缩略图 + 专辑名，选中高亮。
/// Enter 播放选中专辑第一首；↑↓/jk 移动；PageUp/PageDown 翻页。
pub(super) fn draw_cover_browser(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &mut crate::tui::app::App,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.cover_browser")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let albums = app.cover_browser_albums();
    if albums.is_empty() {
        let msg = Paragraph::new(i18n.t("empty.playlist_short").into_owned());
        frame.render_widget(msg, inner);
        return;
    }

    const CELL_W: u16 = 14;
    const COVER_H: u16 = 3;
    let cell_h = COVER_H + 1;
    // 面板放不下一个整格（终端过窄 / 过矮）时直接不画：`.max(1)` 会让
    // cell_x + CELL_W 越过 inner 右边界，而 ratatui 只按整块 buffer 裁剪，
    // 于是封面像素覆盖到面板边框甚至状态条上。
    if inner.width < CELL_W || inner.height < cell_h {
        app.cover_browser_visible = 0;
        return;
    }
    let cols = (inner.width / CELL_W) as usize;
    let rows = (inner.height / cell_h) as usize;
    let visible = cols * rows;
    app.cover_browser_visible = visible;
    // 列表变短（删曲/清空）后 sel/scroll 可能越界——渲染时钳到有效范围，
    // 否则没有格子被高亮、Enter 走 albums.get(sel) 得 None 无反应。
    let count = albums.len();
    if app.cover_browser_sel >= count {
        app.cover_browser_sel = count.saturating_sub(1);
    }
    if app.cover_browser_scroll >= count {
        app.cover_browser_scroll = count.saturating_sub(1);
    }
    let start = app.cover_browser_scroll.min(albums.len().saturating_sub(1));
    let end = (start + visible).min(albums.len());

    for (i, (album, path)) in albums[start..end].iter().enumerate() {
        let idx = start + i;
        let row = (i / cols) as u16;
        let col = (i % cols) as u16;
        let cell_x = inner.x + col * CELL_W;
        let cell_y = inner.y + row * cell_h;
        let selected = idx == app.cover_browser_sel;

        let thumb = app.cover_grid_thumb(path, CELL_W as u32, (COVER_H as u32) * 2);
        if let Some(rgba) = thumb {
            let (dw, dh) = (rgba.width(), rgba.height());
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
                        // 经 cover_px：单色映射 / 暗屏压暗 + 轻模糊，透明落底色。
                        let top = cover_px(rgba, px, py_top, dw, dh);
                        let bot = cover_px(rgba, px, py_bot, dw, dh);
                        // 恒用 ▀（上半=顶像素，下半=底像素或背景）。
                        let st = Style::default().fg(top).bg(bot);
                        spans.push(Span::styled("▀".to_string(), st));
                    }
                }
                let a = Rect {
                    x: cell_x,
                    y: cell_y + cy,
                    width: CELL_W,
                    height: 1,
                };
                frame.render_widget(Paragraph::new(Line::from(spans)), a);
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
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        frame.render_widget(Paragraph::new(Span::styled(name, name_style)), name_area);
    }
}
