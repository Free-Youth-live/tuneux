//! # tuneux-displayx：TUI 共享渲染层
//!
//! 仅供 tuneux 基础版与 tuneux-fx 使用，承载两端一致的 ratatui 绘制。
//! 产品侧负责面板边框、引擎状态与配色选择；本层不负责产品布局。
//! GUI 产品不使用本层。

#![cfg_attr(test, allow(clippy::unwrap_used))]
#![deny(missing_docs)]

pub mod cover;
pub mod terminal;

pub use cover::{color_for_terminal, cover_pixel, CoverStyle};
pub use terminal::{env_truecolor, truecolor_from_signals};

// —— Unicode 显示宽度列布局辅助（TUI 列对齐用）——
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 按显示宽度截断到 max_w（CJK 全角字符按 2 计）。超出部分直接丢弃；
/// 如需尾部省略号请先用 [`truncate_to_width`]。pub 供双 TUI 共用。
pub fn truncate_to_width(s: &str, max_w: usize) -> String {
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
pub fn pad_to_width(s: &str, max_w: usize) -> String {
    let t = truncate_to_width(s, max_w);
    let w = UnicodeWidthStr::width(t.as_str());
    if w >= max_w {
        t
    } else {
        format!("{}{}", t, " ".repeat(max_w - w))
    }
}

/// 截断并右对齐到 max_w 显示宽度（时长列用）。
pub fn pad_left_to_width(s: &str, max_w: usize) -> String {
    let t = truncate_to_width(s, max_w);
    let w = UnicodeWidthStr::width(t.as_str());
    if w >= max_w {
        t
    } else {
        format!("{}{}", " ".repeat(max_w - w), t)
    }
}

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

/// 主题切换顺序中的一项：稳定 ID 与中文显示名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeOrderEntry {
    /// 配置与代码使用的稳定 ID。
    pub id: &'static str,
    /// 随包皮肤与主题提示使用的中文名。
    pub name: &'static str,
}

/// 基础版与 fx 共用的主题切换顺序；终端原生居首，内置藏青位于深蓝之后。
pub const THEME_ORDER: &[ThemeOrderEntry] = &[
    ThemeOrderEntry {
        id: "terminal",
        name: "终端原生",
    },
    ThemeOrderEntry {
        id: "phosphor",
        name: "磷光绿",
    },
    ThemeOrderEntry {
        id: "classic",
        name: "经典青",
    },
    ThemeOrderEntry {
        id: "deepblue",
        name: "深蓝",
    },
    ThemeOrderEntry {
        id: "navy",
        name: "藏青",
    },
    ThemeOrderEntry {
        id: "blackwhite",
        name: "黑白",
    },
    ThemeOrderEntry {
        id: "graywhite",
        name: "灰白",
    },
    ThemeOrderEntry {
        id: "dimcolor",
        name: "暗彩",
    },
];

/// 频谱绘制样式：字符池、柱宽、实心柱开关与三种功能色。
#[derive(Debug, Clone, Copy)]
pub struct SpectrumStyle {
    /// 非实心柱使用的字符池；空池回落为单个中点字符。
    pub pool: &'static [&'static str],
    /// 非实心柱的单个字符列宽。
    pub col_width: usize,
    /// 是否使用背景色填充的实心柱。
    pub is_solid: bool,
    /// 当前频谱柱体颜色。
    pub bar_color: Color,
    /// 峰值保持帽颜色。
    pub peak_color: Color,
    /// 底部基线颜色。
    pub grid_color: Color,
}

/// 将左右声道频谱逐段平均并钳制到 `0.0..=1.0`。
///
/// 两个输入使用相同定长数组，返回值可直接交给 [`draw_spectrum`]。
pub fn merge_spectrum_channels<const N: usize>(left: &[f32; N], right: &[f32; N]) -> [f32; N] {
    std::array::from_fn(|i| ((left[i] + right[i]) / 2.0).clamp(0.0, 1.0))
}

/// 在指定区域绘制单声道频谱，区域最后一行作为基线。
///
/// 输入频段与峰值按显示列做最大值池化，窄屏不会丢掉区间内峰值。
/// 非实心柱每格从字符池随机取字符，峰值帽只占一行；实心柱以两格背景色
/// 填充加一格缝隙绘制，峰值帽与柱顶之间纵向连续。
/// 两组输入长度不同时只绘制共同前缀；空输入、零尺寸与空字符池均安全返回。
pub fn draw_spectrum(
    frame: &mut ratatui::Frame,
    area: Rect,
    spectrum: &[f32],
    peaks: &[f32],
    frame_tick: u64,
    style: SpectrumStyle,
) {
    const FALLBACK_POOL: &[&str] = &["·"];

    let total_w = area.width as usize;
    let h = area.height as usize;
    let bar_max_h = h.saturating_sub(1);
    let col_width = if style.is_solid {
        3
    } else {
        style.col_width.max(1)
    };
    let total_cols = total_w / col_width;
    let n_bands = spectrum.len().min(peaks.len());
    let active = total_cols.min(n_bands);
    let display_spectrum = pool_to_columns(&spectrum[..n_bands], total_cols);
    let display_peaks = pool_to_columns(&peaks[..n_bands], total_cols);
    let mut bar_hs = vec![0usize; active];
    let mut peak_hs = vec![0usize; active];
    for i in 0..active {
        bar_hs[i] = (display_spectrum[i] * bar_max_h as f32).ceil() as usize;
        peak_hs[i] = (display_peaks[i] * bar_max_h as f32).ceil() as usize;
    }

    let pool = if style.pool.is_empty() {
        FALLBACK_POOL
    } else {
        style.pool
    };
    let blank: &'static str = if col_width == 2 { "  " } else { " " };
    let baseline_ch: &'static str = if style.is_solid {
        "── "
    } else if col_width == 2 {
        "──"
    } else {
        "─"
    };
    let mut rng = frame_tick;
    let mut lines: Vec<Line> = Vec::with_capacity(h);
    for row in 0..h {
        let mut spans: Vec<Span> = Vec::with_capacity(total_cols + 1);
        if row == h - 1 {
            for _ in 0..total_cols {
                spans.push(Span::styled(
                    baseline_ch,
                    Style::default().fg(style.grid_color),
                ));
            }
        } else {
            let from_bottom = h - 1 - row;
            for i in 0..active {
                let bar_h = bar_hs[i];
                let peak_h = peak_hs[i];
                if style.is_solid {
                    if peak_h > 0 && from_bottom > bar_h && from_bottom <= peak_h {
                        spans.push(Span::styled("  ", Style::default().bg(style.peak_color)));
                    } else if bar_h > 0 && from_bottom <= bar_h {
                        spans.push(Span::styled("  ", Style::default().bg(style.bar_color)));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    spans.push(Span::raw(" "));
                } else if peak_h > 0 && from_bottom == peak_h {
                    let ch = pool[(rng_next(&mut rng) as usize) % pool.len()];
                    spans.push(Span::styled(
                        pad_col(ch, col_width),
                        Style::default().fg(style.peak_color),
                    ));
                } else if bar_h > 0 && from_bottom <= bar_h {
                    let ch = pool[(rng_next(&mut rng) as usize) % pool.len()];
                    spans.push(Span::styled(
                        pad_col(ch, col_width),
                        Style::default().fg(style.bar_color),
                    ));
                } else {
                    spans.push(Span::raw(blank));
                }
            }
            for _ in active..total_cols {
                spans.push(Span::raw(blank));
            }
        }
        if total_cols * col_width < total_w {
            spans.push(Span::raw(" ".repeat(total_w - total_cols * col_width)));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// 把单字符补齐到 col_width 个显示单元格（宽字符按 2 格计）。
fn pad_col(ch: &str, col_width: usize) -> String {
    // 单元格宽度：宽字符（CJK 等）按 2 格计，其余按字节数近似。
    let cells = if ch.chars().any(|c| (c as u32) > 0x2E7F) {
        2
    } else {
        ch.len()
    };
    let mut s = ch.to_string();
    if cells < col_width {
        s.push_str(&" ".repeat(col_width - cells));
    }
    s
}

fn pool_to_columns(bands: &[f32], total_w: usize) -> Vec<f32> {
    let n = bands.len();
    if total_w >= n {
        let mut values = bands.to_vec();
        values.resize(total_w, 0.0);
        values
    } else {
        let bands_per_col = n as f32 / total_w as f32;
        let mut values = Vec::with_capacity(total_w);
        for col in 0..total_w {
            let start = ((col as f32 * bands_per_col) as usize).min(n);
            let end = (((col + 1) as f32 * bands_per_col) as usize)
                .min(n)
                .max(start + 1);
            let mut max_v = 0.0f32;
            for &band in &bands[start..end] {
                if band > max_v {
                    max_v = band;
                }
            }
            values.push(max_v);
        }
        values
    }
}

/// 推进 splitmix64 状态并返回下一个伪随机数，供随机字符渲染共用。
pub fn rng_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D049BB133111EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn solid_style() -> SpectrumStyle {
        SpectrumStyle {
            pool: &["·"],
            col_width: 1,
            is_solid: true,
            bar_color: Color::Black,
            peak_color: Color::Gray,
            grid_color: Color::DarkGray,
        }
    }

    #[test]
    fn stereo_merge_averages_and_clamps_each_band() {
        assert_eq!(
            merge_spectrum_channels(&[0.2, 1.2, -0.4], &[0.6, 1.2, -0.2]),
            [0.4, 1.0, 0.0]
        );
    }

    #[test]
    fn solid_spectrum_draws_continuous_peak_and_separate_columns() {
        let backend = TestBackend::new(7, 5);
        let mut terminal = Terminal::new(backend).expect("建终端");
        terminal
            .draw(|frame| {
                draw_spectrum(
                    frame,
                    frame.area(),
                    &[0.5, 0.25],
                    &[1.0, 0.5],
                    0,
                    solid_style(),
                );
            })
            .expect("绘制");
        let buffer = terminal.backend().buffer();
        for y in 0..2 {
            assert_eq!(buffer[(0, y)].bg, Color::Gray);
            assert_eq!(buffer[(1, y)].bg, Color::Gray);
        }
        assert_eq!(buffer[(0, 2)].bg, Color::Black);
        assert_eq!(buffer[(3, 2)].bg, Color::Gray);
        assert_eq!(buffer[(0, 3)].bg, Color::Black);
        assert_eq!(buffer[(3, 3)].bg, Color::Black);
        for y in 0..4 {
            assert_eq!(buffer[(2, y)].symbol(), " ");
            assert_eq!(buffer[(2, y)].bg, Color::Reset);
        }
        assert_eq!(buffer[(0, 4)].symbol(), "─");
        assert_eq!(buffer[(0, 4)].fg, Color::DarkGray);
    }

    #[test]
    fn character_spectrum_keeps_peak_on_one_row() {
        let backend = TestBackend::new(1, 4);
        let mut terminal = Terminal::new(backend).expect("建终端");
        let style = SpectrumStyle {
            is_solid: false,
            ..solid_style()
        };
        terminal
            .draw(|frame| {
                draw_spectrum(frame, frame.area(), &[0.25], &[1.0], 0, style);
            })
            .expect("绘制");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].fg, Color::Gray);
        assert_eq!(buffer[(0, 1)].symbol(), " ");
        assert_eq!(buffer[(0, 2)].fg, Color::Black);
        assert_eq!(buffer[(0, 3)].symbol(), "─");
    }

    #[test]
    fn empty_pool_and_mismatched_inputs_do_not_panic() {
        let backend = TestBackend::new(3, 3);
        let mut terminal = Terminal::new(backend).expect("建终端");
        let style = SpectrumStyle {
            pool: &[],
            col_width: 0,
            is_solid: false,
            ..solid_style()
        };
        terminal
            .draw(|frame| {
                draw_spectrum(frame, frame.area(), &[1.0], &[], 0, style);
                draw_spectrum(frame, frame.area(), &[1.0], &[1.0], 0, style);
            })
            .expect("绘制");
    }
}
