//! # 频谱可视化（Matrix 风频谱面板）
//!
//! 柱体颜色为固定 Matrix 亮绿（内容色，不随皮肤）；面板边框/标题走调色板。
//! 数据来自 `Engine::spectrum_lr()`，左右声道取平均合并显示。

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tuneux_corex as audio;

use super::{pal_style, panel_border};
use crate::tui::theme::Palette;

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
    let active = total_w.min(n_bands);

    // —— 峰值保持白帽 ——
    // 绿柱实时跟随当前能量；白帽保存每频段历史峰值，能量低于峰值时按固定
    // 每秒速率线性缓落（明显慢于绿柱），且永不低于当前能量。峰值在频段维度
    // 保持，窗口改宽不重置；当前能量与峰值分别 max-pool 到显示列后绘制。
    let peaks_out = peaks.borrow_mut().update(&spectrum, dt);
    let display_spectrum = pool_to_columns(&spectrum, total_w);
    let display_peaks = pool_to_columns(&peaks_out, total_w);
    let mut bar_hs = vec![0usize; active];
    let mut peak_hs = vec![0usize; active];
    for i in 0..active {
        bar_hs[i] = (display_spectrum[i] * bar_max_h as f32).ceil() as usize;
        peak_hs[i] = (display_peaks[i] * bar_max_h as f32).ceil() as usize;
    }

    // Matrix 风细字符池（全部 1 列宽）：每帧每格随机换，营造"数据雨"。
    // 用 &'static str 而非 char：Span 直接借用，避免每格 to_string() 分配。
    #[allow(clippy::unicode_not_nfc)]
    const POOL: &[&str] = &[
        "1", "l", "i", "I", "|", "'", ":", ".", "·", "•", "◦", ";", ",", "`", "´", "j", "J", "~",
        "/", "\\", "⁄", "-", "–", "—", "=", "│", "┆", "┊", "¦", "∣", "!", "?", "+", "×", "∗", "˖",
        "˗",
    ];
    const BAR_COLOR: Color = Color::LightGreen;

    let mut rng = frame_tick;
    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for row in 0..h {
        let mut spans: Vec<Span> = Vec::with_capacity(total_w);
        if row == h - 1 {
            // 基线行：所有列画 ─（深灰）。
            for _ in 0..total_w {
                spans.push(Span::styled("─", Style::default().fg(Color::DarkGray)));
            }
        } else {
            let from_bottom = h - 1 - row;
            for i in 0..active {
                let bar_h = bar_hs[i];
                let peak_h = peak_hs[i];
                if peak_h > 0 && from_bottom == peak_h {
                    // 白色峰值帽（可能悬浮在绿柱上方）。
                    let r = rng_next(&mut rng);
                    let ch = POOL[(r as usize) % POOL.len()];
                    spans.push(Span::styled(ch, Style::default().fg(Color::White)));
                } else if bar_h > 0 && from_bottom <= bar_h {
                    // 绿色柱体。
                    let r = rng_next(&mut rng);
                    let ch = POOL[(r as usize) % POOL.len()];
                    spans.push(Span::styled(ch, Style::default().fg(BAR_COLOR)));
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            for _ in active..total_w {
                spans.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
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
    let rows =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(inner);
    draw_vu_row(frame, rows[0], level_l, "L", frame_tick);
    draw_vu_row(
        frame,
        rows[1],
        level_r,
        "R",
        frame_tick.wrapping_add(0xC0FFEE),
    );
}

/// 单个声道的横向 VU 柱：`{label} {百分比}  乱码柱体`。
/// `seed` 决定乱码字符的随机性，每帧不同。
fn draw_vu_row(frame: &mut ratatui::Frame, area: Rect, level: f32, label: &str, seed: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    // 乱码字符池：方块渐进 + 散点符号。
    const POOL_CHARS: &[char] = &['░', '▒', '▓', '█', '#', '@', '*', '+'];

    let w = area.width as usize;
    let level = level.clamp(0.0, 1.0);
    let color = if level < 0.6 {
        Color::Green
    } else if level < 0.85 {
        Color::Yellow
    } else {
        Color::Red
    };

    let label_text = format!("{} {:3}% ", label, (level * 100.0) as u32);
    let label_w = label_text.chars().count();
    let bar_w = w.saturating_sub(label_w);
    let active_cells = (level * bar_w as f32).ceil() as usize;

    let mut spans: Vec<Span> = Vec::with_capacity(w);
    spans.push(Span::styled(
        label_text,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let mut rng = seed;
    for col in 0..bar_w {
        if col < active_cells {
            let r = rng_next(&mut rng);
            let ch = POOL_CHARS[(r as usize) % POOL_CHARS.len()];
            spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
        } else {
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
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
    draw_wave_band(frame, rows[0], &l, "L", frame_tick);
    draw_wave_band(frame, rows[1], &r, "R", frame_tick);
}

/// 单个声道的双边波形：中线基线 '-'，波形点用随机字符（随帧变化）。
fn draw_wave_band(
    frame: &mut ratatui::Frame,
    area: Rect,
    wave: &[f32],
    label: &str,
    frame_tick: u64,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let h = area.height as usize;
    let w = area.width as usize;
    let mid = h / 2;
    let amp = (mid.saturating_sub(1)).max(1) as f32;
    let label_text = format!("{label} ");
    let bar_w = w.saturating_sub(label_text.chars().count());
    if bar_w == 0 {
        return;
    }
    // 随机字符池（纯 ASCII，1 列宽）。
    const POOL: &[char] = &['1', '0'];
    let mut rng = frame_tick ^ 0x9E37_79B9;
    let mut lines = Vec::with_capacity(h);
    for row in 0..h {
        let mut spans = Vec::with_capacity(w);
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
        for col in 0..bar_w {
            let idx = col * audio::spectrum::WAVEFORM_LEN / bar_w;
            let v = (wave[idx] * 3.0).clamp(-1.0, 1.0); // 增益 ×3，低电平也撑满
            let wave_row = mid as f32 - v * amp;
            let ch = if (wave_row - row as f32).abs() < 0.5 {
                POOL[(rng_next(&mut rng) as usize) % POOL.len()]
            } else if row == mid {
                '-'
            } else {
                ' '
            };
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(Color::Green),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}
