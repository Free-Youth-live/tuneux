//! # 频谱可视化（频谱面板 / 电平表 / 示波器）
//!
//! 柱体颜色与字符风格随调色板（皮肤键 bar_fg / peak_fg / grid_fg /
//! level_low / level_mid / level_high / bar_style）；面板边框/标题走调色板。
//! 字符集宿主内置（[`crate::tui::theme::BarStyle`] 枚举），hanzi 风格为
//! 2 列宽、横向分辨率减半。
//! 数据来自 `Engine::spectrum_lr()`，左右声道取平均合并显示。

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tuneux_corex as audio;

use super::{pal_style, panel_border};
use crate::tui::theme::{BarStyle, Palette};

/// splitmix64 一步：给定 u64 状态，原地推进并返回一个伪随机 u64。
///
/// 用于频谱乱码字符生成，避免引入 `rand` 依赖。
fn rng_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// 把 N_BANDS 频段 max-pool 到 `total_w` 显示列：每显示列取对应连续频段的
/// 最大值，保证任意宽度下画满、峰值不丢（宽屏剩余列补 0）。
fn pool_to_columns(bands: &[f32; audio::spectrum::N_BANDS], total_w: usize) -> Vec<f32> {
    let n = audio::spectrum::N_BANDS;
    if total_w >= n {
        let mut v = bands.to_vec();
        v.resize(total_w, 0.0);
        v
    } else {
        let bands_per_col = n as f32 / total_w as f32;
        let mut v = Vec::with_capacity(total_w);
        for col in 0..total_w {
            let start = ((col as f32 * bands_per_col) as usize).min(n);
            let end = (((col + 1) as f32 * bands_per_col) as usize)
                .min(n)
                .max(start + 1);
            let mut max_v = 0.0f32;
            for &b in &bands[start..end] {
                if b > max_v {
                    max_v = b;
                }
            }
            v.push(max_v);
        }
        v
    }
}

/// 频谱面板：单声道竖向密柱，Matrix 风格（亮绿 + 细字符，每帧随机换）。
///
/// `N_BANDS` 频段按显示列数做 max-pooling 降采样：每显示列取对应频段最大值，
/// 保证任意宽度下都画满、峰值不丢。
pub(super) fn draw_audio_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
    pal: &Palette,
    peaks: &std::cell::RefCell<audio::spectrum::SpectrumPeakHold>,
    dt: std::time::Duration,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 频 谱 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let n_bands = audio::spectrum::N_BANDS;
    if inner.width < 2 || inner.height < 2 {
        return;
    }

    // 频谱数据：L/R 平均。
    let spectrum: [f32; audio::spectrum::N_BANDS] = engine
        .as_ref()
        .map(|e| {
            let [l, r] = e.spectrum_lr();
            let mut merged = [0.0f32; audio::spectrum::N_BANDS];
            for i in 0..n_bands {
                merged[i] = ((l[i] + r[i]) / 2.0).clamp(0.0, 1.0);
            }
            merged
        })
        .unwrap_or([0.0; audio::spectrum::N_BANDS]);

    let total_w = inner.width as usize;
    let h = inner.height as usize;
    let bar_max_h = h.saturating_sub(1); // 最底行是基线
    let col_w = pal.bar_style.col_width(); // hanzi 风格 2 列一柱，横向分辨率减半
    let total_cols = total_w / col_w;
    let active = total_cols.min(n_bands);

    // —— 峰值保持白帽 ——
    // 绿柱实时跟随当前能量；白帽保存每频段历史峰值，能量低于峰值时按固定
    // 每秒速率线性缓落（明显慢于绿柱），且永不低于当前能量。峰值在频段维度
    // 保持，窗口改宽不重置；当前能量与峰值分别 max-pool 到显示列后绘制。
    let peaks_out = peaks.borrow_mut().update(&spectrum, dt);
    let display_spectrum = pool_to_columns(&spectrum, total_cols);
    let display_peaks = pool_to_columns(&peaks_out, total_cols);
    let mut bar_hs = vec![0usize; active];
    let mut peak_hs = vec![0usize; active];
    for i in 0..active {
        bar_hs[i] = (display_spectrum[i] * bar_max_h as f32).ceil() as usize;
        peak_hs[i] = (display_peaks[i] * bar_max_h as f32).ceil() as usize;
    }

    // 字符池与颜色均来自调色板（皮肤），缺省回落内置默认（亮绿柱 / 白帽 /
    // 深灰基线）。池为 &'static str：Span 直接借用，避免每格 to_string() 分配。
    let pool = pal.bar_style.pool();
    let bar_color = pal.bar_fg.unwrap_or(Color::LightGreen);
    let peak_color = pal.peak_fg.unwrap_or(Color::White);
    let grid_color = pal.grid_fg.unwrap_or(Color::DarkGray);
    let blank: &'static str = if col_w == 2 { "  " } else { " " };
    let baseline_ch: &'static str = if col_w == 2 { "──" } else { "─" };

    let mut rng = frame_tick;
    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for row in 0..h {
        let mut spans: Vec<Span> = Vec::with_capacity(total_cols + 1);
        if row == h - 1 {
            // 基线行：每个柱位画基线字符。
            for _ in 0..total_cols {
                spans.push(Span::styled(baseline_ch, Style::default().fg(grid_color)));
            }
        } else {
            let from_bottom = h - 1 - row;
            for i in 0..active {
                let bar_h = bar_hs[i];
                let peak_h = peak_hs[i];
                if peak_h > 0 && from_bottom == peak_h {
                    // 峰值帽（可能悬浮在柱体上方）：与柱体同池随机字符，颜色区分。
                    let r = rng_next(&mut rng);
                    let ch = pool[(r as usize) % pool.len()];
                    spans.push(Span::styled(ch, Style::default().fg(peak_color)));
                } else if bar_h > 0 && from_bottom <= bar_h {
                    // 柱体：每格随机取池字符。
                    let r = rng_next(&mut rng);
                    let ch = pool[(r as usize) % pool.len()];
                    spans.push(Span::styled(ch, Style::default().fg(bar_color)));
                } else {
                    spans.push(Span::raw(blank));
                }
            }
            for _ in active..total_cols {
                spans.push(Span::raw(blank));
            }
        }
        // 列宽整除不尽的右缘余量补空（仅 2 列风格遇奇数宽度时出现）。
        if total_cols * col_w < total_w {
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 插件可视化面板：显示「可视化-*」插件 tick 产出的字符画（v 循环的
///「插件」态）。插件只产文本、宿主负责贴上；无插件 / 无画面时显示提示。
pub(super) fn draw_visual_panel(frame: &mut ratatui::Frame, area: Rect, text: &str, pal: &Palette) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 插 件 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let body = if text.is_empty() {
        "无可视化插件画面（plugins/ 下放「可视化-*」插件并经签名后重启）"
    } else {
        text
    };
    frame.render_widget(Paragraph::new(body).style(pal_style(pal.fg, None)), inner);
}

/// 三桶默认（Matrix 风格）字符池：纯方块渐进 █▓▒░（从实到虚，每字符连占
/// 2 格）；非 Matrix 风格用 bar_style 风格池（与电平 / 频谱同族）。
const BAND_MATRIX_POOL: &[&str] = &["█", "▓", "▒", "░"];

/// 三桶能量竖排（顶部条中栏，内置组件）：低 / 中 / 高各一行 8 格。
///
/// 数据 = 引擎实时频谱（对数 256 段 ≈ 低 20-200Hz / 中 200Hz-2kHz /
/// 高 2k-20kHz）每桶取最大值（平均会把纯音 / 稀疏频谱峰值稀释为零）。
/// 字符池判定与电平表同口径：Matrix（默认）用方块渐进池，其余风格随
/// bar_style 池逐格随机取用（与电平 / 频谱同观感）；颜色随
/// band_low / band_mid / band_high（缺省回落 bar_fg 频谱柱色）。
/// `seed` 决定随机序列（每帧不同）。
pub(super) fn draw_band_column(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    seed: u64,
    pal: &Palette,
) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    // L/R 平均合并为单声道频谱（与频谱面板同口径）。
    let bands = engine.as_ref().map(|e| {
        let [l, r] = e.spectrum_lr();
        let mut merged = [0.0f32; audio::spectrum::N_BANDS];
        for i in 0..audio::spectrum::N_BANDS {
            merged[i] = ((l[i] + r[i]) / 2.0).clamp(0.0, 1.0);
        }
        merged
    });
    let bands = bands.unwrap_or([0.0; audio::spectrum::N_BANDS]);
    let bucket_max = |start: usize, end: usize| -> f32 {
        bands[start..end].iter().copied().fold(0.0f32, f32::max)
    };
    let n = audio::spectrum::N_BANDS;
    let b1 = n / 3;
    let b2 = n * 2 / 3;
    let buckets = [
        ("低", bucket_max(0, b1), pal.band_low),
        ("中", bucket_max(b1, b2), pal.band_mid),
        ("高", bucket_max(b2, n), pal.band_high),
    ];
    // Matrix（默认）= 纯方块渐进（█▓▒░ 从实到虚，每字符连占 2 格）；
    // 其余风格随 bar_style 池逐格随机取用（与电平 / 频谱同观感）。
    let matrix = pal.bar_style == BarStyle::Matrix;
    let pool: &[&str] = if matrix {
        BAND_MATRIX_POOL
    } else {
        pal.bar_style.pool()
    };
    let fallback_bar = pal.bar_fg.unwrap_or(Color::LightGreen);
    let grid = pal.grid_fg.unwrap_or(Color::DarkGray);
    let col_w = pal.bar_style.col_width();
    let mut rng = seed;
    // 首行留空：与右侧电平表的边框标题行对齐（低 / 中 / 高分别对齐
    // 电平 inner 的 L / R 内容行）。
    let mut lines: Vec<Line> = vec![Line::from("")];
    for (name, energy, key_color) in buckets {
        let cells = (energy * 8.0 + 0.5) as usize; // 每桶 8 格
        let color = key_color.unwrap_or(fallback_bar);
        let mut spans: Vec<Span> = vec![Span::styled(name, Style::default().fg(grid))];
        for i in 0..8 {
            if i < cells {
                // Matrix 池每字符连占 2 格；风格池逐格随机。
                let ch = if matrix {
                    pool[i / 2]
                } else {
                    pool[(rng_next(&mut rng) as usize) % pool.len()]
                };
                spans.push(Span::styled(ch, Style::default().fg(color)));
            } else {
                let blank: &str = if col_w == 2 { "· " } else { "·" };
                spans.push(Span::styled(blank, Style::default().fg(grid)));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// 实时电平柱图（L/R 上下两行水平柱）。
///
/// 数据来自 [`audio::Engine::level_lr`]（音频回调实时累计的 L/R peak，0.0-1.0）。
/// 柱体用乱码字符（随帧变化），颜色按电平阈值：<60% 绿、60-85% 黄、≥85% 红。
pub(super) fn draw_level_meter(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
    pal: &Palette,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 电平 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 2 {
        return;
    }

    let (level_l, level_r) = engine.as_ref().map(|e| e.level_lr()).unwrap_or((0.0, 0.0));
    // 拆 L/R 上下两行（固定行高：高度 2 时 Percentage 分配会把第二行挤成
    // 0 行导致 R 不显示——回归测试 level_meter_renders_both_channels 守护）。
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
    draw_vu_row(frame, rows[0], level_l, "L", frame_tick, pal);
    draw_vu_row(
        frame,
        rows[1],
        level_r,
        "R",
        frame_tick.wrapping_add(0xC0FFEE),
        pal,
    );
}

/// 单个声道的横向 VU 柱：`{label} {百分比}  乱码柱体`。
/// `seed` 决定乱码字符的随机性，每帧不同。
fn draw_vu_row(
    frame: &mut ratatui::Frame,
    area: Rect,
    level: f32,
    label: &str,
    seed: u64,
    pal: &Palette,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // 电平字符口径：bar_style 为 Matrix（默认 / 原版风格）时用 0.5.0 方块渐进
    // 池（独立成族）；皮肤选非 Matrix 风格（blocks/ascii/hanzi）时与频谱同池。
    // 阈值色随皮肤（level_low/mid/high），缺省回落绿/黄/红。
    const POOL_CHARS: &[char] = &['░', '▒', '▓', '█', '#', '@', '*', '+'];
    let bar_style = pal.bar_style;
    let col_w = bar_style.col_width();

    let w = area.width as usize;
    let level = level.clamp(0.0, 1.0);
    let color = if level < 0.6 {
        pal.level_low.unwrap_or(Color::Green)
    } else if level < 0.85 {
        pal.level_mid.unwrap_or(Color::Yellow)
    } else {
        pal.level_high.unwrap_or(Color::Red)
    };
    let dot_color = pal.grid_fg.unwrap_or(Color::DarkGray);

    let label_text = format!("{} {:3}% ", label, (level * 100.0) as u32);
    let label_w = label_text.chars().count();
    let bar_cols = w.saturating_sub(label_w) / col_w; // 柱位数（hanzi 2 列一格）
    let active_cells = (level * bar_cols as f32).ceil() as usize;

    let mut spans: Vec<Span> = Vec::with_capacity(bar_cols + 1);
    spans.push(Span::styled(
        label_text,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let mut rng = seed;
    for col in 0..bar_cols {
        if col < active_cells {
            let r = rng_next(&mut rng);
            if bar_style == BarStyle::Matrix {
                let ch = POOL_CHARS[(r as usize) % POOL_CHARS.len()];
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            } else {
                let pool = bar_style.pool();
                let ch = pool[(r as usize) % pool.len()];
                spans.push(Span::styled(ch, Style::default().fg(color)));
            }
        } else {
            let blank: &str = if col_w == 2 { "· " } else { "·" };
            spans.push(Span::styled(blank, Style::default().fg(dot_color)));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// 示波器：左右声道时域波形（双边，中线上下摆动）。
///
/// 数据来自 [`audio::Engine::waveform_lr`]（每通道 WAVEFORM_LEN 点，-1.0~1.0）。
/// 每个声道占一半高度：中线基线 '-'，波形点 '#' 上下摆动，幅度为半高。
pub(super) fn draw_oscilloscope(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    pal: &Palette,
    frame_tick: u64,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(" 示波器 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 8 || inner.height < 4 {
        return;
    }

    let [l, r] = engine
        .as_ref()
        .map(|e| e.waveform_lr())
        .unwrap_or([[0.0; audio::spectrum::WAVEFORM_LEN]; 2]);
    let rows =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(inner);
    draw_wave_band(frame, rows[0], &l, "L", frame_tick, pal);
    draw_wave_band(frame, rows[1], &r, "R", frame_tick, pal);
}

/// 单个声道的双边波形：中线基线，波形点用风格池字符（随帧变化）。
/// 波形点颜色随皮肤 bar_fg（与频谱柱同色），中线随 grid_fg；hanzi 风格
/// 下波形点 2 列宽，横向采样点数减半。
fn draw_wave_band(
    frame: &mut ratatui::Frame,
    area: Rect,
    wave: &[f32],
    label: &str,
    frame_tick: u64,
    pal: &Palette,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let h = area.height as usize;
    let w = area.width as usize;
    let mid = h / 2;
    let amp = (mid.saturating_sub(1)).max(1) as f32;
    let label_text = format!("{label} ");
    let col_w = pal.bar_style.col_width();
    let bar_cols = w.saturating_sub(label_text.chars().count()) / col_w;
    if bar_cols == 0 {
        return;
    }
    // 字符池随 bar_style（与频谱同风格）；中线 / 空白按列宽对齐。
    let pool = pal.bar_style.pool();
    let wave_color = pal.bar_fg.unwrap_or(Color::LightGreen);
    let mid_color = pal.grid_fg.unwrap_or(Color::DarkGray);
    let mid_ch: &str = if col_w == 2 { "──" } else { "-" };
    let blank: &str = if col_w == 2 { "  " } else { " " };
    let mut rng = frame_tick ^ 0x9E37_79B9;
    let mut lines = Vec::with_capacity(h);
    for row in 0..h {
        let mut spans = Vec::with_capacity(bar_cols + 1);
        if row == 0 {
            spans.push(Span::styled(
                label_text.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("  "));
        }
        for col in 0..bar_cols {
            let idx = col * audio::spectrum::WAVEFORM_LEN / bar_cols;
            let v = (wave[idx] * 3.0).clamp(-1.0, 1.0); // 增益 ×3，低电平也撑满
            let wave_row = mid as f32 - v * amp;
            if (wave_row - row as f32).abs() < 0.5 {
                let ch = pool[(rng_next(&mut rng) as usize) % pool.len()];
                spans.push(Span::styled(ch, Style::default().fg(wave_color)));
            } else if row == mid {
                spans.push(Span::styled(mid_ch, Style::default().fg(mid_color)));
            } else {
                spans.push(Span::raw(blank));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use crate::tui::theme::{BarStyle, Palette};
    use unicode_width::UnicodeWidthStr;

    /// 电平表 L/R 两行渲染守护（曾有 R 行被挤没的布局回归）：
    /// 电平表皮肤键生效守护：自定义 level_low 时低电平柱使用该颜色。
    ///（Matrix 风格电平字符用 0.5.0 方块池；非 Matrix 随风格池，此处只验颜色。）
    #[test]
    fn level_meter_uses_palette_threshold_color() {
        let pal = Palette {
            level_low: Some(ratatui::style::Color::Magenta), // 显著区别于默认绿
            ..Palette::default()
        };
        let backend = ratatui::backend::TestBackend::new(40, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("建终端");
        terminal
            .draw(|f| {
                // 直接调 draw_vu_row，level=0.5（低档）——柱体应使用 level_low 色。
                super::draw_vu_row(f, f.area(), 0.5, "L", 0, &pal);
            })
            .expect("绘制");
        let buf = terminal.backend().buffer().content().to_vec();
        let bar_cells: Vec<_> = buf
            .iter()
            .filter(|c| ["░", "▒", "▓", "█", "#", "@", "*", "+"].contains(&c.symbol()))
            .collect();
        assert!(!bar_cells.is_empty(), "应有电平柱字符");
        assert!(
            bar_cells
                .iter()
                .all(|c| c.fg == ratatui::style::Color::Magenta),
            "低档柱体应使用 level_low 配置色（Magenta）"
        );
    }

    /// TestBackend 实际渲染后，缓冲里应同时含 L 与 R 行标签。
    #[test]
    fn level_meter_renders_both_channels() {
        // 真实链路高度：电平 block 收到 4 行 area（顶部条 inner），
        // 边框后 inner 2 行，L / R 各 1 行——必须两行都在。
        let backend = ratatui::backend::TestBackend::new(40, 4);
        let mut terminal = ratatui::Terminal::new(backend).expect("建终端");
        terminal
            .draw(|f| {
                super::draw_level_meter(f, f.area(), &None, 0, &Palette::default());
            })
            .expect("绘制");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains('L'), "L 行应在：{text:?}");
        assert!(text.contains('R'), "R 行应在：{text:?}");
    }

    /// 三桶中栏渲染守护：首行留空对齐电平边框行，低 / 中 / 高三行齐备。
    #[test]
    fn band_column_renders_three_buckets() {
        let backend = ratatui::backend::TestBackend::new(24, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("建终端");
        terminal
            .draw(|f| {
                super::draw_band_column(f, f.area(), &None, 0, &Palette::default());
            })
            .expect("绘制");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        for name in ["低", "中", "高"] {
            assert!(text.contains(name), "{name} 行应在：{text:?}");
        }
    }

    /// 各风格字符池宽度守护：逐字符宽度 == 风格列宽（1 列 / 2 列）；
    /// 白帽字符同检。这是对位渲染的前提，防字符集误加全角 / 零宽字符。
    #[test]
    fn style_pools_match_col_width() {
        for style in [
            BarStyle::Matrix,
            BarStyle::Blocks,
            BarStyle::Ascii,
            BarStyle::Hanzi,
        ] {
            for ch in style.pool() {
                assert_eq!(
                    UnicodeWidthStr::width(*ch),
                    style.col_width(),
                    "{style:?} 池字符 {ch:?} 宽度与列宽不符"
                );
            }
        }
    }
}
