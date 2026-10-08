//! # 指针表面板（fx 呈现层）
//!
//! VU 风格指针表：L/R 双表占满主区。几何与针物理来自
//! [`tuneux_commonx::gauge`]（与 max 的 GUI 仿 TUI 版共用同一几何）——
//! 单线制框、▒ 盘壁、· 弧 + ●• 刻度、▪ 红区、一条线指针、▄█▀▀ 针帽；
//! 颜色随皮肤按图层分家族着色（框/盘壁 → grid_fg，指针/刻度 → fg，
//! 红区 → level_high）。

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tuneux_commonx::gauge::{gauge_cells, GaugeKind, GAUGE_MIN_H, GAUGE_MIN_W};
use tuneux_corex as audio;

use super::{pal_style, panel_border};
use crate::tui::theme::Palette;

/// 图层 → 皮肤色（灰阶字符自带明暗，这里只分「家族」）。
fn kind_color(kind: GaugeKind, pal: &Palette) -> Color {
    match kind {
        GaugeKind::RedZone => pal.level_high.unwrap_or(Color::Red),
        GaugeKind::Needle | GaugeKind::Pivot | GaugeKind::TickMajor => {
            pal.fg.unwrap_or(Color::Gray)
        }
        // 数字标注：次级灰（不与指针抢焦点）
        GaugeKind::Label => pal.fg.unwrap_or(Color::Gray),
        _ => pal.grid_fg.unwrap_or(Color::DarkGray),
    }
}

/// 指针表面板（整区）：标题框 + L/R 双表 + 读数行。
///
/// `needles` 以 RefCell 借用更新（draw 侧 &App 的既有口径）；`dt` 驱动
/// 针物理（300ms 弹道 + 微过冲）。
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_gauge_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    engine: &Option<audio::Engine>,
    pal: &Palette,
    needles: &std::cell::RefCell<[tuneux_commonx::gauge::NeedlePhys; 2]>,
    dt: std::time::Duration,
    i18n: &tuneux_commonx::I18n,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, false))
        .title(format!(" {} ", i18n.t("panel.gauge")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < GAUGE_MIN_W * 2 + 3 || inner.height < GAUGE_MIN_H + 2 {
        return;
    }

    // 电平 → dB → 针位 frac（-20..+3dB 域）
    let (lv_l, lv_r) = engine.as_ref().map(|e| e.level_lr()).unwrap_or((0.0, 0.0));
    let to_frac = |lv: f32| -> f32 {
        let db = if lv <= 1e-7 {
            -20.0
        } else {
            (20.0 * lv.log10()).clamp(-20.0, 3.0)
        };
        (db + 20.0) / 23.0
    };
    let dt_s = dt.as_secs_f32().min(0.1);
    let fracs = {
        let mut n = needles.borrow_mut();
        [
            n[0].update(to_frac(lv_l), dt_s),
            n[1].update(to_frac(lv_r), dt_s),
        ]
    };
    let db_txt = |lv: f32| -> String {
        if lv <= 1e-7 {
            "-\u{221e} dB".to_string()
        } else {
            format!("{:+.1} dB", 20.0 * lv.log10())
        }
    };

    // L/R 各占一半，画布在其中居中；读数行贴画布下缘
    let half_w = inner.width / 2;
    let canvas_w: u16 = half_w.saturating_sub(4).clamp(GAUGE_MIN_W, 40);
    let canvas_h: u16 = inner.height.saturating_sub(4).clamp(GAUGE_MIN_H, 12);
    for (k, (tag, lv, frac)) in [("L", lv_l, fracs[0]), ("R", lv_r, fracs[1])]
        .iter()
        .enumerate()
    {
        let half = Rect {
            x: inner.x + k as u16 * half_w,
            y: inner.y,
            width: half_w,
            height: inner.height,
        };
        let cells = gauge_cells(canvas_w, canvas_h, *frac);
        let ox = half.x + (half.width.saturating_sub(canvas_w)) / 2;
        let oy = half.y + (half.height.saturating_sub(canvas_h + 2)) / 2;
        // 逐行渲染网格（同行相邻格拼 Span，空档补空格）
        for row in 0..canvas_h {
            let row_cells: Vec<_> = cells.iter().filter(|c| c.y == row).collect();
            if row_cells.is_empty() {
                continue;
            }
            let mut spans = Vec::new();
            let mut cursor = 0u16;
            for c in &row_cells {
                let gap = c.x - cursor;
                if gap > 0 {
                    spans.push(Span::raw(" ".repeat(gap as usize)));
                }
                spans.push(Span::styled(
                    c.ch.to_string(),
                    Style::default().fg(kind_color(c.kind, pal)),
                ));
                cursor = c.x + 1;
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect {
                    x: ox,
                    y: oy + row,
                    width: canvas_w,
                    height: 1,
                },
            );
        }
        // 读数行（画布下方居中：标签 + dB）
        let txt = format!("{tag}  {}", db_txt(*lv));
        let txt_w = txt.chars().count() as u16;
        let tx = half.x + (half.width.saturating_sub(txt_w)) / 2;
        let ty = oy + canvas_h + 1;
        if ty < inner.bottom() {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    txt,
                    Style::default().fg(pal.fg.unwrap_or(Color::Gray)),
                )),
                Rect {
                    x: tx,
                    y: ty,
                    width: txt_w.min(half.right().saturating_sub(tx)),
                    height: 1,
                },
            );
        }
    }
}
