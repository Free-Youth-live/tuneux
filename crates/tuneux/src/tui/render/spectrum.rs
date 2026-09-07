//! # 可视化绘制（电平表 + 频谱）
//!
//! 职责：所有"随音频信号实时变化"的可视化组件——
//! - 顶部条右侧的实时电平柱图（`draw_level_meter` / `draw_vu_row`）；
//! - 主区的 Matrix 风格频谱面板（`draw_audio_panel`）；
//! - 以及两者共用的 splitmix64 伪随机数 `rng_next`
//!   （用于"乱码字符"效果，避免引入 rand 依赖）。

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use tuneux_corex as audio;

/// splitmix64 一步：给定 u64 状态，原地推进并返回一个伪随机 u64。
///
/// 用于电平乱码区和频谱的随机字符生成，避免引入 `rand` 依赖。
/// splitmix64 是 Sebastiano Vigna 设计的简单且分布良好的伪随机算法。
pub(crate) fn rng_next(state: &mut u64) -> u64 {
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
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " 电平 ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 10 || inner.height < 2 {
        return;
    }

    // 读左右电平（peak 0.0-1.0）。未初始化时 (0.0, 0.0)。
    let (level_l, level_r) = engine.as_ref().map(|e| e.level_lr()).unwrap_or((0.0, 0.0));

    // 拆 L/R 上下两行
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

/// 单个声道的横向 VU 柱。`seed` 决定乱码字符的随机性，每帧不同。
///
/// 行布局：`{label} {百分比}  {乱码柱体}` —— 标签 + 柱。
/// 柱体部分：active 部分填乱码字符，剩余是空格。
fn draw_vu_row(frame: &mut ratatui::Frame, area: Rect, level: f32, label: &str, seed: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    // 乱码字符池：方块渐进（░▒▓█）+ 散点符号（#@*+），按字符宽度都算 1 单元
    const POOL_CHARS: &[char] = &['░', '▒', '▓', '█', '#', '@', '*', '+'];

    let w = area.width as usize;
    let level = level.clamp(0.0, 1.0);

    // 颜色按电平阈值（<60% 绿、60-85% 黄、≥85% 红）
    let color = if level < 0.6 {
        Color::Green
    } else if level < 0.85 {
        Color::Yellow
    } else {
        Color::Red
    };

    // —— 标签："{L|R} {百分比} " ——
    // 固定宽度，方便柱体起点对齐
    let label_text = format!("{} {:3}% ", label, (level * 100.0) as u32);
    let label_w = label_text.chars().count();

    // —— 柱体 ——
    // 柱宽 = w - label_w；柱体可被宽度挤压到 0（窄终端不报错）
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
            // 不活跃部分：暗灰短横线（让"空"也可见但不抢眼）
            spans.push(Span::styled("·", Style::default().fg(Color::DarkGray)));
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
pub(super) fn draw_audio_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    frame_tick: u64,
    peaks: &std::cell::RefCell<audio::spectrum::SpectrumPeakHold>,
    dt: std::time::Duration,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " 频 谱 ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let n_bands = audio::spectrum::N_BANDS;
    if inner.width < 2 || inner.height < 2 {
        return;
    }

    // 频谱数据：L/R 平均
    let spectrum = engine
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

    // 频段颜色 + 字符：Matrix 风格——纯绿 + 细字符。
    // 配色：从青改成 LightGreen（Matrix 标志性的亮绿），
    // 字符池全是 thin（无填充块），每格随机换——
    // 形成"数据雨"质感的频谱柱。
    const BAR_COLOR: Color = Color::LightGreen;

    // Matrix 风细字符池：37 个，全部 thin（无填充块）。
    // 按风格族分组（行注释仅作阅读，运行时无意义）：
    //   竖线   1 l i I |
    //   点    · • ◦ : ; , ' ` ´ . j J
    //   弯折  ~ / \ ⁄
    //   破折  - – — =
    //   框线  │ ┆ ┊ ¦ ∣
    //   符号  ! ? + × ∗ ˖ ˗
    // 每帧每格随机选——给眼睛"在跳"的活感。
    // 全部为 1 列宽的窄字符，宽度经 unicode-width 校验。
    #[allow(clippy::unicode_not_nfc)]
    const POOL: &[char] = &[
        // 现有：竖线 + 简单点
        '1', 'l', 'i', 'I', '|', '\'', ':', '.', // A：点/小字符
        '·', '•', '◦', ';', ',', '`', '´', 'j', 'J', // B：弯/折
        '~', '/', '\\', '⁄', // C：破折
        '-', '–', '—', '=', // D：框线
        '│', '┆', '┊', '¦', '∣', // F：符号
        '!', '?', '+', '×', '∗', '˖', '˗',
    ];

    let mut rng = frame_tick;
    let mut lines: Vec<Line> = Vec::with_capacity(h);

    // 主循环：按行从上到下渲染频谱面板的每一行。
    //   - row 0 是顶行（最远，最"矮"的柱顶能到这）
    //   - row h-1 是基线行（柱底），单独画 ─
    // 柱子是"从下往上长"的：value 越大 → bar_h 越大 → 柱顶离基线越远。
    for row in 0..h {
        let mut spans: Vec<Span> = Vec::with_capacity(total_w);

        if row == h - 1 {
            // 基线行：所有列画一个 ─（深灰，不抢眼）
            for _ in 0..total_w {
                spans.push(Span::styled("─", Style::default().fg(Color::DarkGray)));
            }
        } else {
            // 当前行距基线的"高度距离"（row 0 → h-1, from_bottom = h-1 → 1）
            let from_bottom = h - 1 - row;
            // active 列画频谱（峰值帽 + 绿柱），超宽屏尾部留空。
            for i in 0..active {
                let bar_h = bar_hs[i];
                let peak_h = peak_hs[i];
                if peak_h > 0 && from_bottom == peak_h {
                    // 白色峰值帽（可能悬浮在绿柱上方）。
                    let r = rng_next(&mut rng);
                    let ch = POOL[(r as usize) % POOL.len()];
                    spans.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(Color::White),
                    ));
                } else if bar_h > 0 && from_bottom <= bar_h {
                    // 绿色柱体。
                    let r = rng_next(&mut rng);
                    let ch = POOL[(r as usize) % POOL.len()];
                    spans.push(Span::styled(ch.to_string(), Style::default().fg(BAR_COLOR)));
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            // 4K 屏等超宽场景：多出的列留空
            for _ in active..total_w {
                spans.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
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
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " 示波器 ",
        Style::default()
            .fg(Color::Cyan)
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
    // 中线行索引与上下摆动幅度（行数）。
    let mid = h / 2;
    let amp = (mid.saturating_sub(1)).max(1) as f32;
    // 左侧标签占 2 列（"L " / "R "），其余画波形。
    let label_text = format!("{label} ");
    let bar_w = w.saturating_sub(label_text.chars().count());
    if bar_w == 0 {
        return;
    }
    // 随机字符池（纯 ASCII，1 列宽，避免歧义宽度）。
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
            let v = (wave[idx] * 3.0).clamp(-1.0, 1.0); // 增益 ×3，低电平也撑满 // 带符号 -1.0~1.0
                                                        // 波形点行：v=1 到顶、v=0 中线、v=-1 到底。
            let wave_row = mid as f32 - v * amp;
            let ch = if (wave_row - row as f32).abs() < 0.5 {
                // 波形点用随机字符（随帧变化，像老式点阵示波器）。
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
