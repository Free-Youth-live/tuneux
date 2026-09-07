//! # 终端界面渲染（主入口与调度）
//!
//! 职责：`draw` 主入口 + 各区域渲染的调度分发。
//! 具体绘制实现按职责拆分到 `render/` 子模块：
//!
//! - [`layout`]：布局度量单一真相源（`layout_metrics`，main.rs 事件循环共用）；
//! - [`spectrum`]：可视化（电平表 + Matrix 风频谱 + splitmix64 随机数）；
//! - [`status`]：信息显示（当前曲目 / 状态条 / 进度条 / 帮助栏）；
//! - [`panels`]：内容面板（文件浏览器 / 播放列表 / 歌词 / 专辑封面）；
//! - [`popup`]：弹窗（"关于"）。

use ratatui::layout::{Constraint, Layout, Rect};

use crate::config;
use crate::config::{LeftPanel, LyricsMode, SpectrumMode};
use crate::playlist;
use crate::tui::app::App;
use tuneux_corex as audio;

mod layout;
mod panels;
mod popup;
mod spectrum;
mod status;

// 对外接口保持拆分前的路径不变：main.rs 仍从
// `crate::tui::render::{draw, layout_metrics}` 导入。
pub use self::layout::{layout_metrics, LayoutMetrics};

use self::panels::{draw_browser, draw_cover_panel, draw_lyrics_panel, draw_playlist};
use self::popup::draw_about;
use self::spectrum::{draw_audio_panel, draw_level_meter};
use self::status::{draw_help_bar, draw_now_playing, draw_status_bar};

// 布局（4 段，浏览器默认隐藏，b 键唤出左侧；频谱通过 v 键在半屏/全屏间切换）：
//   ┌ 当前曲目 ────────────┐ ┌ 电平 ─────────┐
//   │ 曲名 / 歌手 / 专辑 … │ │ 随机乱码字符   │   ← 顶部条（左信息|右电平）
//   ├─────────────────────┤ ├───────────────┤
//   │ [浏览器 |] 播放列表  │   [频谱（半/全屏）]              ← 主区
//   ├─────────────────────┴───────────────────────────────────┤
//   │ [播] 曲名  0:03 ━━●━━ 3:43  100% 列表循环+随机           │  ← 状态条
//   ├─────────────────────────────────────────────────────────┤
//   │ ↑↓选曲 · Enter播放 · ←→±5s · v频谱 · q退出              │  ← 帮助
//   └─────────────────────────────────────────────────────────┘

// （历史遗留：下述 splitmix64 说明与 rng_next 上的文档重复，且曾与
//  布局说明意外拼接、悬空附着在 draw 上。现统一转为普通注释，避免
//  悬挂 /// 错误附着到下方的布局常量。rng_next 现位于 spectrum.rs。）
// splitmix64 一步：给定 u64 状态，原地推进并返回一个伪随机 u64。
// 用于电平乱码区的随机字符生成，避免引入 rand 依赖。详见 spectrum.rs
// 中 rng_next 处的正式文档。
// - 顶部条 6 行：当前曲信息（左 70%）+ 电平柱图（右 30%）；
// - 主区填满：浏览器可见时左（config.browser_ratio 比例）+ 右列表/频谱；
//   频谱模式通过 v 键循环：关 → 半屏（与列表分屏）→ 全屏（占满列表区）；
// - 状态条 4 行：进度/时间/音量/循环/错误信息；
// - 帮助栏 3 行：按焦点动态显示快捷键。
//
// 各段高度用 Length(n)（含边框）固定，主区用 Fill(1) 吃掉剩余空间，
// 保证终端高度变化时只有列表区伸缩，其他区稳定。

pub fn draw(frame: &mut ratatui::Frame, app: &mut App, config: &config::Config) {
    let area = frame.area();

    // 布局单一真相源：所有尺寸从 layout_metrics 取，禁止在 draw 里
    // 手写高度/宽度公式（main.rs 事件循环也调同一个函数）。
    let metrics = layout_metrics(
        (area.width, area.height),
        app.spectrum_mode,
        app.lyrics_mode,
        app.left_panel,
        app.search_mode,
        app.search_target,
        config.browser_ratio,
    );

    // 四段垂直布局：顶部条、主区、状态条、帮助栏。
    // 频谱不再独立占一段——它通过 spectrum_mode 嵌入主区的列表侧，
    // 这样 Half/Full 模式下频谱能就近替换播放列表，不需要在底部
    // 另开一段（节省纵向空间，视觉上也更直观）。
    let vertical = Layout::vertical([
        Constraint::Length(metrics.top_bar_h),
        Constraint::Fill(1),
        Constraint::Length(metrics.status_bar_h),
        Constraint::Length(metrics.help_bar_h),
    ])
    .split(area);

    // —— 顶部条（左信息 + 右电平）——
    // 触发封面解码（_ 丢弃返回值，释放 &mut self 借用），
    // 然后通过 &self 读 cover_cache 取引用（左侧封面面板复用同一缓存）。
    let frame_tick = app.frame_tick;
    let _ = app.current_decoded_cover();
    let cover = app.cover_cache.as_ref().map(|(_, img)| img);

    draw_top_bar(
        frame,
        vertical[0],
        &app.current_metadata,
        &app.engine,
        frame_tick,
    );
    // 帧计数 +1，供电平/频谱的乱码字符用。
    app.frame_tick = app.frame_tick.wrapping_add(1);

    // —— 主区：左侧面板（浏览器/封面，互斥）+ 列表（频谱按模式嵌入列表侧）——
    // 宽度百分比与搜索状态全部取自 metrics（单一真相源）：
    // 浏览器比封面窄（约 70%），封面保持 config.browser_ratio 的比例，
    // 均 clamp 防止极端值导致某侧完全不可用（clamp 在 layout_metrics 内）。
    // playlist_rows() 在此统一计算一次，避免三个分支重复算。
    let rows = app.playlist_rows();
    match app.left_panel {
        LeftPanel::Browser => {
            let main = Layout::horizontal([
                Constraint::Percentage(metrics.browser_pct),
                Constraint::Percentage(100 - metrics.browser_pct),
            ])
            .split(vertical[1]);
            draw_browser(
                frame,
                main[0],
                &app.browser,
                metrics.browser_searching,
                &app.search_query,
                app.focus == playlist::Panel::Browser,
            );
            draw_playlist_or_spectrum(frame, main[1], app, &metrics, &rows);
        }
        LeftPanel::Cover => {
            let main = Layout::horizontal([
                Constraint::Percentage(metrics.cover_pct),
                Constraint::Percentage(100 - metrics.cover_pct),
            ])
            .split(vertical[1]);
            draw_cover_panel(frame, main[0], cover);
            draw_playlist_or_spectrum(frame, main[1], app, &metrics, &rows);
        }
        LeftPanel::Hidden => {
            draw_playlist_or_spectrum(frame, vertical[1], app, &metrics, &rows);
        }
    }

    // —— 状态条 + 帮助栏 ——
    draw_status_bar(
        frame,
        vertical[2],
        &app.current_metadata,
        &app.engine,
        config.repeat,
        app.playlist.is_shuffle(),
        app.last_error.as_deref(),
    );
    draw_help_bar(frame, vertical[3], app.focus);

    // —— 关于弹窗：最后绘制，覆盖在其它内容之上 ——
    if app.about_visible {
        draw_about(frame, area);
    }
}

/// 播放列表区（或频谱/歌词）渲染分发器。
///
/// 歌词与频谱可共存：
/// - 歌词 **Visible**：左（播放列表 + 频谱）+ 右歌词（横向分屏）；
///   频谱开启时显示在左侧区域的下半部分；
/// - 歌词 **Hidden**：走原频谱逻辑（Hidden/Half/Full）。
fn draw_playlist_or_spectrum(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    metrics: &LayoutMetrics,
    rows: &[playlist::PlaylistRow],
) {
    match app.lyrics_mode {
        // 歌词显示时：频谱 Full 全屏（覆盖歌词），否则左 + 右歌词
        LyricsMode::Visible => {
            if app.spectrum_mode == SpectrumMode::Full {
                // 全屏频谱（优先于歌词）
                draw_audio_panel(frame, area, &app.engine, app.frame_tick);
            } else {
                // 列表与歌词横向分屏比例取自 metrics（单一真相源）
                let split = Layout::horizontal([
                    Constraint::Percentage(metrics.playlist_w_pct),
                    Constraint::Percentage(metrics.lyrics_w_pct),
                ])
                .split(area);
                match app.spectrum_mode {
                    // 无频谱：左侧全是播放列表
                    SpectrumMode::Hidden => {
                        draw_playlist(
                            frame,
                            split[0],
                            &app.playlist,
                            &app.metadata_cache,
                            &app.engine,
                            app.focus,
                            rows,
                            app.search_mode,
                            &app.search_query,
                        );
                    }
                    // 半屏：左侧区域下半显示频谱
                    SpectrumMode::Half => {
                        let left_split = Layout::vertical([
                            Constraint::Percentage(50),
                            Constraint::Percentage(50),
                        ])
                        .split(split[0]);
                        draw_playlist(
                            frame,
                            left_split[0],
                            &app.playlist,
                            &app.metadata_cache,
                            &app.engine,
                            app.focus,
                            rows,
                            app.search_mode,
                            &app.search_query,
                        );
                        draw_audio_panel(frame, left_split[1], &app.engine, app.frame_tick);
                    }
                    // Full 已在上方分支处理
                    SpectrumMode::Full => {}
                }
                draw_lyrics_panel(frame, split[1], &app.engine, app.current_lyrics.as_ref());
            }
        }
        // 歌词关闭：走原频谱逻辑
        LyricsMode::Hidden => match app.spectrum_mode {
            SpectrumMode::Hidden => {
                draw_playlist(
                    frame,
                    area,
                    &app.playlist,
                    &app.metadata_cache,
                    &app.engine,
                    app.focus,
                    rows,
                    app.search_mode,
                    &app.search_query,
                );
            }
            SpectrumMode::Half => {
                let split =
                    Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .split(area);
                draw_playlist(
                    frame,
                    split[0],
                    &app.playlist,
                    &app.metadata_cache,
                    &app.engine,
                    app.focus,
                    rows,
                    app.search_mode,
                    &app.search_query,
                );
                draw_audio_panel(frame, split[1], &app.engine, app.frame_tick);
            }
            SpectrumMode::Full => {
                draw_audio_panel(frame, area, &app.engine, app.frame_tick);
            }
        },
    }
}

/// 顶部条：左侧当前曲信息（约 70%）+ 右侧电平（约 30%）。
///
/// 参数全部按值传入（不是 &App），让调用方先算好 cover 再传入，
/// 避免本函数和后续 `app.frame_tick += 1` 之间的可变借用冲突。
fn draw_top_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    metadata: &Option<crate::metadata::TrackMetadata>,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
) {
    let cols =
        Layout::horizontal([Constraint::Percentage(70), Constraint::Percentage(30)]).split(area);
    draw_now_playing(frame, cols[0], metadata, engine);
    draw_level_meter(frame, cols[1], engine, frame_tick);
}
