//! # 指针表模块（GUI 仿 TUI）
//!
//! 与 fx 终端版共用 [`tuneux_commonx::gauge`] 的几何与针物理——同一块
//! 字符画布（单线制框 / ▒ 盘壁 / · 弧刻度 / ▪ 红区 / 一条线指针 /
//! ▄█▀▀ 针帽），在 egui 里以等宽字符网格渲染（与经典频谱的字符路径
//! 同款手法）。
//! 经典模式：tui_box 包裹；现代模式：卡片内直接渲染（内容本身即 TUI 风）。

use super::*;
use egui::CornerRadius;
use tuneux_commonx::gauge::{gauge_cells, GaugeKind, GAUGE_MIN_H, GAUGE_MIN_W};

impl MaxAppV2 {
    /// 指针表模块入口（经典 + 现代两路 dispatch 汇到此处）。
    pub(super) fn render_gauge_module(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.gauge").into_owned();

        // dt（与频谱面板同款的帧间隔法）
        let now = Instant::now();
        let dt = self
            .gauge_last
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(0.0)
            .min(0.1);
        self.gauge_last = Some(now);

        // 电平 → dB → 针位（-20..+3dB 域）
        let (lv_l, lv_r) = self
            .engine
            .as_ref()
            .map(|e| e.level_lr())
            .unwrap_or((0.0, 0.0));
        let to_frac = |lv: f32| -> f32 {
            let db = if lv <= 1e-7 {
                -20.0
            } else {
                (20.0 * lv.log10()).clamp(-20.0, 3.0)
            };
            (db + 20.0) / 23.0
        };
        let fracs = [
            self.gauge_needles[0].update(to_frac(lv_l), dt),
            self.gauge_needles[1].update(to_frac(lv_r), dt),
        ];
        let db_txt = |lv: f32| -> String {
            if lv <= 1e-7 {
                "-\u{221e} dB".to_string()
            } else {
                format!("{:+.1} dB", 20.0 * lv.log10())
            }
        };

        if self.config.modern {
            // 现代：卡片底 + 网格（内容即 TUI 风，无需 tui_box 框饰）
            let area = ui.max_rect();
            let p0 = ui.painter_at(area);
            p0.rect_filled(area, CornerRadius::same(4), pal.bg);
            self.draw_gauge_pair(ui, &pal, area, (lv_l, lv_r), fracs, db_txt);
        } else {
            Self::tui_box(ui, &pal, &title, false, |ui| {
                let area = ui.max_rect();
                self.draw_gauge_pair(ui, &pal, area, (lv_l, lv_r), fracs, db_txt);
            });
        }
    }

    /// L/R 双表 + 读数（闭包内零 &mut self——状态已在入口推进完）。
    #[allow(clippy::too_many_arguments)]
    fn draw_gauge_pair(
        &self,
        ui: &mut Ui,
        pal: &Palette,
        area: Rect,
        (lv_l, lv_r): (f32, f32),
        fracs: [f32; 2],
        db_txt: impl Fn(f32) -> String,
    ) {
        let cw = Self::char_w(ui);
        let line_h = LINE_H;
        // 画布尺寸：L/R 各半，留读数行
        let half_w_px = area.width() * 0.5;
        let canvas_w = ((half_w_px / cw) as u16)
            .saturating_sub(3)
            .clamp(GAUGE_MIN_W, 40);
        let canvas_h = ((area.height() / line_h) as u16)
            .saturating_sub(2)
            .clamp(GAUGE_MIN_H, 12);
        let cells_l = gauge_cells(canvas_w, canvas_h, fracs[0]);
        let cells_r = gauge_cells(canvas_w, canvas_h, fracs[1]);
        let grid_w_px = canvas_w as f32 * cw;
        let grid_h_px = canvas_h as f32 * line_h;
        let oy = area.top() + (area.height() - grid_h_px - line_h).max(0.0) * 0.5;

        for (k, cells) in [cells_l, cells_r].iter().enumerate() {
            let ox = area.left() + half_w_px * k as f32 + (half_w_px - grid_w_px).max(0.0) * 0.5;
            // 逐行渲染（同行 Span 拼接；空档补空格——LayoutJob 手动排布）
            for row in 0..canvas_h {
                let row_cells: Vec<&tuneux_commonx::gauge::GaugeCell> =
                    cells.iter().filter(|c| c.y == row).collect();
                if row_cells.is_empty() {
                    continue;
                }
                let y = oy + row as f32 * line_h;
                let p = ui.painter_at(Rect::from_min_size(
                    Pos2::new(ox, y),
                    Vec2::new(grid_w_px, line_h),
                ));
                let mut cursor = 0u16;
                for c in row_cells {
                    let gap = c.x - cursor;
                    if gap > 0 {
                        p.text(
                            Pos2::new(ox + cursor as f32 * cw, y + line_h * 0.5),
                            Align2::LEFT_CENTER,
                            " ".repeat(gap as usize),
                            mono(FONT),
                            Color32::TRANSPARENT,
                        );
                    }
                    p.text(
                        Pos2::new(ox + c.x as f32 * cw, y + line_h * 0.5),
                        Align2::LEFT_CENTER,
                        c.ch.to_string(),
                        mono(FONT),
                        gauge_kind_color(c.kind, pal),
                    );
                    cursor = c.x + 1;
                }
            }
            // 读数行（画布下方）
            let (tag, lv) = if k == 0 { ("L", lv_l) } else { ("R", lv_r) };
            let txt = format!("{tag}  {}", db_txt(lv));
            let tw = ui.fonts(|f| {
                f.layout_no_wrap(txt.clone(), mono(FONT - 1.0), pal.fg)
                    .size()
                    .x
            });
            let p = ui.painter_at(area);
            p.text(
                Pos2::new(
                    ox + (grid_w_px - tw).max(0.0) * 0.5,
                    oy + grid_h_px + line_h * 0.5,
                ),
                Align2::LEFT_CENTER,
                txt,
                mono(FONT - 1.0),
                pal.fg,
            );
        }
    }
}

/// 图层 → 调色板色（灰阶明暗由字符承担，这里分「家族」+ 投影压暗）。
fn gauge_kind_color(kind: GaugeKind, pal: &Palette) -> Color32 {
    match kind {
        GaugeKind::RedZone => pal.level_high,
        GaugeKind::Needle | GaugeKind::Pivot | GaugeKind::TickMajor => pal.fg,
        GaugeKind::NeedleShadow | GaugeKind::PivotShadow => pal.grid,
        GaugeKind::BezelTop => pal.grid.linear_multiply(0.75),
        GaugeKind::BezelBottom => pal.grid.linear_multiply(1.25),
        GaugeKind::RecessWall => pal.grid.linear_multiply(0.9),
        GaugeKind::TickMinor | GaugeKind::Label => pal.fg_weak,
    }
}
