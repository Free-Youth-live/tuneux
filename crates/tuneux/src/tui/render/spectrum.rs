//! # 可视化绘制（电平表 + 频谱）
//!
//! 职责：所有"随音频信号实时变化"的可视化组件——
//! - 顶部条右侧的实时电平柱图（`draw_level_meter` / `draw_vu_row`）；
//! - 主区的 Matrix 风格频谱面板（`draw_audio_panel`）。
//!
//! 随机字符序列与频谱主体绘制由 `tuneux-displayx` 共享实现。

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::palette;
use tuneux_corex as audio;
use tuneux_displayx::rng_next;
use tuneux_mediax::BarStyle;

/// 实时电平柱图：左右声道分别用一条**水平**柱显示。
///
/// 数据来源：音频回调每 ~10ms 累计 L/R peak amplitude 写入 `SharedState`，
/// 这里从 `Engine::level_lr()` 读出（0.0-1.0）。
///
/// 视觉设计：
/// - **上下两行**：L 在上、R 在下（典型 VU 表布局，便于同时扫视左右声道差异）；
/// - 每行布局：`标签 + 乱码柱`。`L 65%` 之类的标签占左侧 ~6 字符，
///   右侧剩余空间全部用来画水平柱；
/// - 柱体：每格填一个"乱码"字符（从 `░▒▓█#@*+` 池子里取），由 `frame_tick`
///   作种子——每帧乱码变，营造"数据流"质感；
/// - 活跃格数 = level × 柱宽（向上取整，让微弱信号也能看到至少 1 格）；
/// - 颜色按电平阈值：<60% 绿（安全）、60-85% 黄（警示）、≥85% 红（过载）。
///
/// # 参数
/// - `engine`：从 `Engine::level_lr()` 读 L/R；未初始化时显示 0%；
/// - `frame_tick`：每帧 +1 的计数器，让乱码字符随帧变化。
pub(super) fn draw_level_meter(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
    style: BarStyle,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.level")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 2 {
        return;
    }

    // 读左右电平（peak 0.0-1.0）。未初始化时 (0.0, 0.0)。
    let (level_l, level_r) = engine.as_ref().map(|e| e.level_lr()).unwrap_or((0.0, 0.0));

    // 拆 L/R 上下两行（固定行高：高度 2 时 Percentage 分配会把第二行挤成
    // 0 行导致 R 不显示——fx 同款回归测试守护）。
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    draw_vu_row(frame, rows[0], level_l, "L", frame_tick, style);
    draw_vu_row(
        frame,
        rows[1],
        level_r,
        "R",
        frame_tick.wrapping_add(0xC0FFEE),
        style,
    );
}

/// 三桶默认（Matrix 风格）字符池：纯方块渐进 █▓▒░（从实到虚，每字符连占
/// 2 格）；非 Matrix 风格用 bar_style 风格池（与电平 / 频谱同族）。
const BAND_MATRIX_POOL: &[&str] = &["█", "▓", "▒", "░"];

/// 三桶能量面板（顶部条中栏）：高 / 中 / 低自上而下各一行，柱长随面板宽度伸缩。
///
/// 数据 = 引擎实时频谱（对数 256 段）每桶取最大值（平均会把纯音 /
/// 稀疏频谱峰值稀释为零）；字符池判定与电平表同口径：Matrix（默认）
/// 用方块渐进池（按柱位均匀分布，越左越实），其余风格随 bar_style 池
/// 逐格随机取用（与电平 / 频谱同观感）。`seed` 决定随机序列（每帧不同）。
pub(super) fn draw_band_column(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    seed: u64,
    style: BarStyle,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.bands")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 8 || inner.height < 3 {
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
    // 无引擎时画全零桶（空桶框架在位，栏位不留白）。
    let bands = bands.unwrap_or([0.0; audio::spectrum::N_BANDS]);
    let bucket_max = |start: usize, end: usize| -> f32 {
        bands[start..end].iter().copied().fold(0.0f32, f32::max)
    };
    let n = audio::spectrum::N_BANDS;
    let b1 = n / 3;
    let b2 = n * 2 / 3;
    let (low_name, mid_name, high_name) = (
        i18n.t("spectrum.low"),
        i18n.t("spectrum.mid"),
        i18n.t("spectrum.high"),
    );
    // 显示序：高频在上、低频在下（与刻度直觉一致：越往上频率越高）。
    let buckets = [
        (high_name.as_ref(), bucket_max(b2, n)),
        (mid_name.as_ref(), bucket_max(b1, b2)),
        (low_name.as_ref(), bucket_max(0, b1)),
    ];
    // Matrix（默认）= 纯方块渐进（█▓▒░ 从实到虚，按柱宽均匀分布）；
    // 其余风格随 bar_style 池逐格随机取用（与电平 / 频谱同观感）。
    let matrix = style == BarStyle::Matrix;
    let pool: &[&str] = if matrix {
        BAND_MATRIX_POOL
    } else {
        style.pool()
    };
    let col_w = style.col_width();
    // 每行布局：标签（低/中/高，各 2 列）+ 柱体；柱体填满剩余宽度，
    // 面板越宽柱越长。
    let label_w = 2usize;
    let bar_cols = (inner.width as usize).saturating_sub(label_w) / col_w;
    if bar_cols == 0 {
        return;
    }
    let mut rng = seed;
    let mut lines: Vec<Line> = Vec::with_capacity(buckets.len());
    // 三桶配色：统一取主题柱体色；未达到的格位仍是暗点。
    // 显示序为高频在上、低频在下。
    let pal = palette::pal();
    for (name, energy) in buckets {
        let active = (energy * bar_cols as f32).ceil() as usize;
        let mut spans: Vec<Span> =
            vec![Span::styled(name, Style::default().fg(palette::pal().dim))];
        for i in 0..bar_cols {
            if i < active {
                // Matrix 池按柱位均匀渐变（越左越实）；风格池逐格随机。
                let ch = if matrix {
                    pool[((i * pool.len()) / bar_cols).min(pool.len() - 1)]
                } else {
                    pool[(rng_next(&mut rng) as usize) % pool.len()]
                };
                spans.push(Span::styled(ch, Style::default().fg(pal.bar)));
            } else {
                let blank: &str = if col_w == 2 { "· " } else { "·" };
                spans.push(Span::styled(blank, Style::default().fg(palette::pal().dim)));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 单个声道的横向 VU 柱。`seed` 决定乱码字符的随机性，每帧不同。
///
/// 行布局：`{label} {百分比}  {乱码柱体}` —— 标签 + 柱。
/// 柱体部分：active 部分填乱码字符，剩余是空格。
fn draw_vu_row(
    frame: &mut ratatui::Frame,
    area: Rect,
    level: f32,
    label: &str,
    seed: u64,
    style: BarStyle,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // 乱码字符池：方块渐进（░▒▓█）+ 散点符号（#@*+）——0.5.0 原版观感；
    // 仅 Matrix（默认）风格用本池，其余风格电平与频谱同族（下方分支）。
    const POOL_CHARS: &[char] = &['░', '▒', '▓', '█', '#', '@', '*', '+'];
    let col_w = style.col_width();

    let w = area.width as usize;
    let level = level.clamp(0.0, 1.0);

    // 颜色按电平阈值（<60% 绿、60-85% 黄、≥85% 红）；单色主题去阈值
    // 彩色，恒用柱身色（液晶观感不出现绿黄红）。非真彩终端统一量化输出。
    let pal = palette::pal();
    let dim = pal.dim_screen;
    let color = if pal.monochrome {
        pal.bar
    } else if level < 0.6 {
        pal.ok
    } else if level < 0.85 {
        if dim {
            Color::Rgb(154, 138, 58)
        } else {
            Color::Yellow
        }
    } else if dim {
        Color::Rgb(160, 64, 56)
    } else {
        Color::Red
    };
    let color = tuneux_displayx::color_for_terminal(color, tuneux_displayx::env_truecolor());

    // —— 标签："{L|R} {百分比} " ——
    // 固定宽度，方便柱体起点对齐
    let label_text = format!("{} {:3}% ", label, (level * 100.0) as u32);
    let label_w = label_text.chars().count();

    // —— 柱体 ——
    // 柱宽 = w - label_w（电平恒 1 列一格）；可被宽度挤压到 0（窄终端不报错）
    let bar_cols = w.saturating_sub(label_w) / col_w; // 柱位数（hanzi 2 列一格）
    let active_cells = (level * bar_cols as f32).ceil() as usize;

    let mut spans: Vec<Span> = Vec::with_capacity(bar_cols + 1);
    spans.push(Span::styled(
        label_text,
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));

    let mut rng = seed;
    for col in 0..bar_cols {
        if col < active_cells {
            let r = rng_next(&mut rng);
            if style == BarStyle::Matrix {
                let ch = POOL_CHARS[(r as usize) % POOL_CHARS.len()];
                spans.push(Span::styled(ch.to_string(), Style::default().fg(color)));
            } else {
                let pool = style.pool();
                let ch = pool[(r as usize) % pool.len()];
                spans.push(Span::styled(ch, Style::default().fg(color)));
            }
        } else {
            // 不活跃部分：暗灰短横线（让"空"也可见但不抢眼）
            let blank: &str = if col_w == 2 { "· " } else { "·" };
            spans.push(Span::styled(blank, Style::default().fg(palette::pal().dim)));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// 频谱面板：单声道竖向密柱，Matrix 风格（纯绿 + 细字符）。
///
/// 频谱数据来自 `Engine::spectrum_lr()`，左右声道取平均值合并显示。
///
/// ## 自适应显示
///
/// `N_BANDS=256` 在 4K 屏（≥256 列）能 1:1 显示每段一格。终端更窄时
/// 按显示列数做 **max-pooling** 降采样：每显示列对应一段连续频段，
/// 取该段的最大值——保证任意宽度下都画满、不留空、峰值不丢。
///
/// 视觉：
/// - 每显示列 1 格，柱高 = value × 可用高度；
/// - 字符池：37 个 Matrix 风细字符（`1 l i I | · • ◦ : ; , ' ` ´ . j J
///   ~ / \ ⁄ - – — = │ ┆ ┊ ¦ ∣ ! ? + × ∗ ˖ ˗`），无填充块——
///   每帧每格随机换，营造"数据雨"的跳跃质感（Matrix 致敬）；
/// - 柱身单色亮绿（LightGreen），柱顶一格白色高亮
///   让眼睛能跟上"条顶位置"；
/// - 无信号区域留空，底部一行 `─` 基线。
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_audio_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
    peaks: &std::cell::RefCell<audio::spectrum::SpectrumPeakHold>,
    dt: std::time::Duration,
    style: BarStyle,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.spectrum")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 2 || inner.height < 2 {
        return;
    }

    // 频谱数据：L/R 平均
    let spectrum = engine
        .as_ref()
        .map(|e| {
            let [left, right] = e.spectrum_lr();
            tuneux_displayx::merge_spectrum_channels(&left, &right)
        })
        .unwrap_or([0.0; audio::spectrum::N_BANDS]);

    // 峰值在频段维度保持，窗口改宽不重置；显示列池化与绘制由双端共享层完成。
    let peaks_out = peaks.borrow_mut().update(&spectrum, dt);
    let pal = palette::pal();
    tuneux_displayx::draw_spectrum(
        frame,
        inner,
        &spectrum,
        &peaks_out,
        frame_tick,
        tuneux_displayx::SpectrumStyle {
            pool: style.pool(),
            col_width: style.col_width(),
            is_solid: pal.block_spectrum,
            bar_color: pal.bar,
            peak_color: pal.peak_fg.unwrap_or(Color::White),
            grid_color: pal.dim,
        },
    );
}

/// 示波器：左右声道时域波形（双边，中线上下摆动）。
///
/// 数据来自 [`audio::Engine::waveform_lr`]（每通道 WAVEFORM_LEN 点，-1.0~1.0）。
/// 每个声道占一半高度：中线基线 '-'，波形点 '#' 上下摆动，幅度为半高。
pub(super) fn draw_oscilloscope(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
    style: BarStyle,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.oscilloscope")),
        Style::default()
            .fg(palette::pal().chrome)
            .add_modifier(Modifier::BOLD),
    ));
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
    draw_wave_band(frame, rows[0], &l, "L", frame_tick, style);
    draw_wave_band(frame, rows[1], &r, "R", frame_tick, style);
}

/// 单个声道的双边波形：中线基线，波形点用风格池字符（随帧变化）。
fn draw_wave_band(
    frame: &mut ratatui::Frame,
    area: Rect,
    wave: &[f32],
    label: &str,
    frame_tick: u64,
    style: BarStyle,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let h = area.height as usize;
    let w = area.width as usize;
    // 中线行索引与上下摆动幅度（行数）。
    let mid = h / 2;
    let amp = (mid.saturating_sub(1)).max(1) as f32;
    // 左侧标签占 2 列（"L " / "R "），其余画波形。
    let label_text = format!("{label} ");
    let col_w = style.col_width();
    let bar_cols = w.saturating_sub(label_text.chars().count()) / col_w;
    if bar_cols == 0 {
        return;
    }
    // 字符池随 bar_style（与频谱同风格）；中线 / 空白按列宽对齐。
    let pool = style.pool();
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
                    .fg(palette::pal().chrome)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("  "));
        }
        for col in 0..bar_cols {
            let idx = col * audio::spectrum::WAVEFORM_LEN / bar_cols;
            // 增益 ×3 让低电平也撑满；波形点行：v=1 到顶、v=0 中线、v=-1 到底。
            let v = (wave[idx] * 3.0).clamp(-1.0, 1.0);
            let wave_row = mid as f32 - v * amp;
            if (wave_row - row as f32).abs() < 0.5 {
                // 波形点用风格池字符（随帧变化）。
                let ch = pool[(rng_next(&mut rng) as usize) % pool.len()];
                spans.push(Span::styled(ch, Style::default().fg(palette::pal().ok)));
            } else if row == mid {
                spans.push(Span::styled(
                    mid_ch,
                    Style::default().fg(palette::pal().dim),
                ));
            } else {
                spans.push(Span::raw(blank));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三桶中栏渲染守护：无引擎时也画出带边框标题的空桶框架，
    /// 低 / 中 / 高三行齐备，每桶占满「内宽 - 标签 2 列」个占位符。
    #[test]
    fn band_column_renders_empty_buckets_without_engine() {
        let backend = ratatui::backend::TestBackend::new(24, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("建终端");
        terminal
            .draw(|f| {
                super::draw_band_column(
                    f,
                    f.area(),
                    &None,
                    0,
                    BarStyle::Matrix,
                    &crate::config::build_i18n("zh"),
                );
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
            assert!(text.contains(name), "{name} 应在：{text:?}");
        }
        // 标题：zh 内置表命中「频段」（CJK 占两格，逐字符断言）。
        assert!(
            text.contains('频') && text.contains('段'),
            "标题应在：{text:?}"
        );
        // 无引擎 = 三桶全空：每桶占满内宽（24 列终端：内宽 22 - 标签 2 = 20 格），
        // 三桶共 60 个占位符「·」（Matrix 单列池）。
        assert_eq!(text.matches('·').count(), 60, "每桶应占满内宽：{text:?}");
    }

    /// 电平表阈值色契约：<60% 主题绿、60–85% 黄、≥85% 红。
    /// 主题化（裸色值收口到调色板）之后仍必须成立——这是「电平表变色」的守护。
    #[test]
    fn level_meter_threshold_colors_survive_theme() {
        fn bar_colors(level: f32) -> Vec<Color> {
            let backend = ratatui::backend::TestBackend::new(40, 1);
            let mut terminal = ratatui::Terminal::new(backend).expect("建终端");
            terminal
                .draw(|f| {
                    super::draw_vu_row(f, f.area(), level, "L", 0, BarStyle::Matrix);
                })
                .expect("绘制");
            let buf = terminal.backend().buffer();
            (0..buf.area.width).map(|x| buf[(x, 0)].fg).collect()
        }
        let low = bar_colors(0.3);
        assert!(
            low.iter().any(|c| *c == palette::pal().ok),
            "低档应为主题绿：{low:?}"
        );
        let mid = bar_colors(0.7);
        assert!(mid.contains(&Color::Yellow), "中档应为黄：{mid:?}");
        let high = bar_colors(0.95);
        assert!(high.contains(&Color::Red), "高档应为红：{high:?}");
    }

    /// 三桶显示序：高频在上、低频在下。
    #[test]
    fn band_rows_ordered_high_to_low() {
        let backend = ratatui::backend::TestBackend::new(24, 5);
        let mut terminal = ratatui::Terminal::new(backend).expect("建终端");
        terminal
            .draw(|f| {
                super::draw_band_column(
                    f,
                    f.area(),
                    &None,
                    0,
                    BarStyle::Matrix,
                    &crate::config::build_i18n("zh"),
                );
            })
            .expect("绘制");
        let buf = terminal.backend().buffer();
        let row_of = |ch: char| {
            (0..buf.area.height)
                .find(|&y| (0..buf.area.width).any(|x| buf[(x, y)].symbol().starts_with(ch)))
                .expect("标签应存在")
        };
        let (high, mid, low) = (row_of('高'), row_of('中'), row_of('低'));
        assert!(
            high < mid && mid < low,
            "高频应在上：高{high} 中{mid} 低{low}"
        );
    }
}
