//! # 现代界面呈现层（modern mode）
//!
//! 与「经典 TUI 观感」（mod.rs 主体）并存的第二套呈现层，`config.modern`
//! 一键开关（视图菜单 / `:modern` 命令），默认关闭：
//!
//! - **零字符图形**：模块框饰 = 圆角卡片 + 头部条；频谱 / 示波器 / 电平 /
//!   波形 / 三桶频段一律矢量绘制（复用 f2k 图形路径）；传输 / 导航 /
//!   折叠 / 排序等一切图标用 painter 三角、圆、弧线、折线自绘——不出现
//!   `▶ ▮▮ █▓▒░ ▸ ━●─` 之类字符网格（普通文本内容除外）。
//! - **控件现代化**：顶栏 = 封面缩略 + 曲目信息 + 传输按钮 + seek 滑条 +
//!   音量滑条 + 静音；EQ = 竖直矢量推子；压缩器 = 水平矢量滑条；滤波器 =
//!   矢量旋钮；开关 = 现代 Switch。
//! - **鼠标全覆盖**：拖拽（seek / 音量 / EQ 推子 / 分割条比例 / 模块换位 /
//!   浏览器→列表投递 / 列表行重排 / 歌词点行跳转）与右键（行菜单 / 空白
//!   菜单 / 叶子模块菜单 / 状态栏模块菜单）可达全部操作；键盘快捷键照常。
//! - **可逆**：关闭开关即回到经典观感；两套呈现共享同一份状态（播放列表 /
//!   dock 树 / 搜索 / 命令 / 弹层 / 快捷键），互切零丢失。
//!
//! 纪律（与 mod.rs 同源）：调色板是唯一色彩来源，渲染层只读 Palette 字段；
//! 生产路径零 unwrap / expect。
//!
//! `allocate_ui_at_rect` 弃用告警与经典路径（mod.rs）同源同口径，待整体迁移
//! `allocate_new_ui` 时一并处理，此处显式豁免保持告警面干净。
#![allow(deprecated)]

use super::*;
use egui::{CornerRadius, RichText};

// =============================================================================
// 常量与字体
// =============================================================================

/// 现代模式行高（列表行 / 工具条行）。
const ROW_H: f32 = 26.0;
/// 卡片圆角半径（px）。
const CARD_R: u8 = 6;
/// 控件圆角半径（px）。
const CTRL_R: u8 = 4;
/// 顶栏固定高度（px：封面 64 + 三排控制簇 + 上下留白）。
const TOPBAR_H: f32 = 104.0;

/// 比例字体（现代模式正文；WQY 为两族主字体，字形与等宽一致）。
pub(super) fn pf(sz: f32) -> FontId {
    FontId::proportional(sz)
}

// =============================================================================
// 矢量图标（纯 painter 绘制，无字符字形）
// =============================================================================

/// 传输类图标种类。
enum TrIcon {
    /// 上一曲（竖条 + 左三角）。
    Prev,
    /// 播放（右三角）。
    Play,
    /// 暂停（双竖条）。
    Pause,
    /// 停止（圆角方块；测试冒烟覆盖，生产按钮组未排布——保留供后续传输条）。
    #[allow(dead_code)]
    Stop,
    /// 下一曲（右三角 + 竖条）。
    Next,
}

/// 传输图标：以 c 为中心、s 为半宽绘制。全部折线 / 三角 / 矩形原语。
fn tr_icon(p: &egui::Painter, c: Pos2, s: f32, kind: TrIcon, col: Color32) {
    let th = s * 0.62; // 三角形半高
    let tw = s * 0.55; // 三角形宽
    match kind {
        TrIcon::Prev => {
            let x_bar = c.x - s * 0.85;
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x_bar - s * 0.14, c.y - th),
                    Pos2::new(x_bar + s * 0.14, c.y + th),
                ),
                CornerRadius::same(1),
                col,
            );
            let x_tip = c.x + s * 0.75;
            p.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x_tip, c.y),
                    Pos2::new(x_tip - tw, c.y - th),
                    Pos2::new(x_tip - tw, c.y + th),
                ],
                col,
                Stroke::NONE,
            ));
        }
        TrIcon::Play => {
            let x_tip = c.x + s * 0.72;
            p.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x_tip, c.y),
                    Pos2::new(x_tip - tw * 1.5, c.y - th * 1.15),
                    Pos2::new(x_tip - tw * 1.5, c.y + th * 1.15),
                ],
                col,
                Stroke::NONE,
            ));
        }
        TrIcon::Pause => {
            let bw = s * 0.22; // 条宽
            let bh = s * 0.95;
            for dx in [-s * 0.42, s * 0.42] {
                p.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(c.x + dx - bw * 0.5, c.y - bh * 0.5),
                        Pos2::new(c.x + dx + bw * 0.5, c.y + bh * 0.5),
                    ),
                    CornerRadius::same(1),
                    col,
                );
            }
        }
        TrIcon::Stop => {
            let q = s * 0.62;
            p.rect_filled(
                Rect::from_min_max(Pos2::new(c.x - q, c.y - q), Pos2::new(c.x + q, c.y + q)),
                CornerRadius::same(2),
                col,
            );
        }
        TrIcon::Next => {
            let x_tip = c.x - s * 0.75;
            p.add(egui::Shape::convex_polygon(
                vec![
                    Pos2::new(x_tip, c.y),
                    Pos2::new(x_tip + tw, c.y - th),
                    Pos2::new(x_tip + tw, c.y + th),
                ],
                col,
                Stroke::NONE,
            ));
            let x_bar = c.x + s * 0.85;
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x_bar - s * 0.14, c.y - th),
                    Pos2::new(x_bar + s * 0.14, c.y + th),
                ),
                CornerRadius::same(1),
                col,
            );
        }
    }
}

/// 随机播放图标：两条交叉折线 + 箭头。
fn icon_shuffle(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let st = Stroke::new(2.0_f32, col);
    // 下→上交叉线
    p.line_segment(
        [
            Pos2::new(c.x - s, c.y + s * 0.55),
            Pos2::new(c.x + s * 0.35, c.y - s * 0.55),
        ],
        st,
    );
    // 上→下交叉线
    p.line_segment(
        [
            Pos2::new(c.x - s, c.y - s * 0.55),
            Pos2::new(c.x + s * 0.35, c.y + s * 0.55),
        ],
        st,
    );
    // 右端竖箭头（两线汇入同一终点）
    p.line_segment(
        [
            Pos2::new(c.x + s * 0.35, c.y - s * 0.55),
            Pos2::new(c.x + s, c.y - s * 0.55),
        ],
        st,
    );
    p.line_segment(
        [
            Pos2::new(c.x + s * 0.35, c.y + s * 0.55),
            Pos2::new(c.x + s, c.y + s * 0.55),
        ],
        st,
    );
    for dy in [-s * 0.55, s * 0.55] {
        p.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(c.x + s, c.y + dy),
                Pos2::new(c.x + s * 0.45, c.y + dy - s * 0.32),
                Pos2::new(c.x + s * 0.45, c.y + dy + s * 0.32),
            ],
            col,
            Stroke::NONE,
        ));
    }
}

/// 循环图标：上下横线 + 两端半圆弧 + 箭头；`one` 加中心圆点（单曲）。
fn icon_repeat(p: &egui::Painter, c: Pos2, s: f32, one: bool, col: Color32) {
    let st = Stroke::new(2.0_f32, col);
    let w = s * 0.8; // 半宽
    let h = s * 0.55; // 半高
    p.line_segment(
        [
            Pos2::new(c.x - w * 0.35, c.y - h),
            Pos2::new(c.x + w * 0.5, c.y - h),
        ],
        st,
    );
    p.line_segment(
        [
            Pos2::new(c.x + w * 0.35, c.y + h),
            Pos2::new(c.x - w * 0.5, c.y + h),
        ],
        st,
    );
    // 两端半圆弧（16 段折线逼近）
    let arc_r = w * 0.35;
    let steps = 12;
    let mut prev = Pos2::new(c.x + w * 0.5, c.y - h + arc_r);
    for i in 1..=steps {
        let t = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / steps as f32;
        let pt = Pos2::new(
            c.x + w * 0.5 + t.cos() * arc_r,
            c.y - h + arc_r + t.sin() * arc_r,
        );
        p.line_segment([prev, pt], st);
        prev = pt;
    }
    let mut prev = Pos2::new(c.x - w * 0.5, c.y + h - arc_r);
    for i in 1..=steps {
        let t = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / steps as f32;
        let pt = Pos2::new(
            c.x - w * 0.5 + t.cos() * arc_r,
            c.y + h - arc_r + t.sin() * arc_r,
        );
        p.line_segment([prev, pt], st);
        prev = pt;
    }
    // 箭头（右上角指向左）
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(c.x + w * 0.5 + arc_r * 0.2, c.y - h + arc_r),
            Pos2::new(c.x + w * 0.5 - arc_r * 0.9, c.y - h + arc_r * 0.45),
            Pos2::new(c.x + w * 0.5 - arc_r * 0.55, c.y - h + arc_r * 1.5),
        ],
        col,
        Stroke::NONE,
    ));
    if one {
        p.circle_filled(c, s * 0.14, col);
    }
}

/// 音量图标：扬声器多边形 + 声波弧；`muted` 画叉线替代声波。
fn icon_volume(p: &egui::Painter, c: Pos2, s: f32, muted: bool, col: Color32) {
    // 扬声器本体（矩形 + 三角锥口）
    let body = Rect::from_min_max(
        Pos2::new(c.x - s, c.y - s * 0.28),
        Pos2::new(c.x - s * 0.45, c.y + s * 0.28),
    );
    p.rect_filled(body, CornerRadius::same(1), col);
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(c.x - s * 0.45, c.y - s * 0.28),
            Pos2::new(c.x - s * 0.45, c.y + s * 0.28),
            Pos2::new(c.x + s * 0.05, c.y + s * 0.72),
            Pos2::new(c.x + s * 0.05, c.y - s * 0.72),
        ],
        col,
        Stroke::NONE,
    ));
    if muted {
        let st = Stroke::new(2.0_f32, col);
        p.line_segment(
            [
                Pos2::new(c.x + s * 0.25, c.y - s * 0.5),
                Pos2::new(c.x + s, c.y + s * 0.5),
            ],
            st,
        );
        p.line_segment(
            [
                Pos2::new(c.x + s, c.y - s * 0.5),
                Pos2::new(c.x + s * 0.25, c.y + s * 0.5),
            ],
            st,
        );
    } else {
        let st = Stroke::new(1.8_f32, col);
        for (k, rr) in [s * 0.45, s * 0.8].iter().enumerate() {
            let cx0 = c.x + s * 0.18 + k as f32 * s * 0.22;
            let steps = 8;
            let mut prev = Pos2::new(cx0, c.y - rr * 0.6);
            for i in 1..=steps {
                let t = -std::f32::consts::FRAC_PI_3
                    + (std::f32::consts::FRAC_PI_2 * 1.2) * i as f32 / steps as f32;
                let pt = Pos2::new(cx0 + t.sin() * rr * 0.55, c.y + t.cos() * rr * 0.6);
                p.line_segment([prev, pt], st);
                prev = pt;
            }
        }
    }
}

/// 介质图标：唱片（同心圆 + 中心孔）。
fn icon_disc(p: &egui::Painter, c: Pos2, s: f32, col: Color32, hole: Color32) {
    p.circle_filled(c, s, col);
    p.circle_filled(c, s * 0.62, hole);
    p.circle_filled(c, s * 0.16, col);
}

/// 文件夹图标（左上小舌 + 圆角主体，填充色）。
fn icon_folder(p: &egui::Painter, min: Pos2, sz: Vec2, col: Color32, bg: Color32) {
    let tab = Rect::from_min_size(min, Vec2::new(sz.x * 0.42, sz.y * 0.22));
    p.rect_filled(tab, CornerRadius::same(1), col);
    let body = Rect::from_min_max(
        Pos2::new(min.x, min.y + sz.y * 0.18),
        Pos2::new(min.x + sz.x, min.y + sz.y),
    );
    p.rect_filled(body, CornerRadius::same(CTRL_R), col);
    // 主体上一条浅色横杠（标签位观感）
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(min.x + sz.x * 0.12, min.y + sz.y * 0.42),
            Pos2::new(min.x + sz.x * 0.62, min.y + sz.y * 0.56),
        ),
        CornerRadius::same(1),
        bg,
    );
}

/// 音频文件图标：小音符（符头椭圆 + 符干 + 旗）。
fn icon_note(p: &egui::Painter, min: Pos2, sz: Vec2, col: Color32) {
    let cx = min.x + sz.x * 0.42;
    let cy = min.y + sz.y * 0.72;
    p.circle_filled(Pos2::new(cx, cy), sz.x * 0.22, col);
    p.line_segment(
        [
            Pos2::new(cx + sz.x * 0.2, cy),
            Pos2::new(cx + sz.x * 0.2, min.y + sz.y * 0.18),
        ],
        Stroke::new(2.0_f32, col),
    );
    p.line_segment(
        [
            Pos2::new(cx + sz.x * 0.2, min.y + sz.y * 0.18),
            Pos2::new(min.x + sz.x * 0.88, min.y + sz.y * 0.34),
        ],
        Stroke::new(2.0_f32, col),
    );
}

/// CUE 文件图标：圆角矩形 + 折线（轨道列表观感）。
fn icon_cue(p: &egui::Painter, min: Pos2, sz: Vec2, col: Color32) {
    p.rect_stroke(
        Rect::from_min_size(min, sz),
        CornerRadius::same(2),
        Stroke::new(1.6_f32, col),
        egui::StrokeKind::Middle,
    );
    let st = Stroke::new(1.4_f32, col);
    for k in 1..=3 {
        let y = min.y + sz.y * (0.25 + 0.25 * k as f32);
        p.line_segment(
            [
                Pos2::new(min.x + sz.x * 0.2, y),
                Pos2::new(min.x + sz.x * (if k == 3 { 0.6 } else { 0.8 }), y),
            ],
            st,
        );
    }
}

/// 折叠箭头（chevron 三角）：`down` = 展开（朝下），否则朝右。
fn icon_chevron(p: &egui::Painter, c: Pos2, s: f32, down: bool, col: Color32) {
    let (a, b, d) = if down {
        (
            Pos2::new(c.x - s, c.y - s * 0.5),
            Pos2::new(c.x + s, c.y - s * 0.5),
            Pos2::new(c.x, c.y + s * 0.6),
        )
    } else {
        (
            Pos2::new(c.x - s * 0.5, c.y - s),
            Pos2::new(c.x - s * 0.5, c.y + s),
            Pos2::new(c.x + s * 0.6, c.y),
        )
    };
    p.add(egui::Shape::convex_polygon(
        vec![a, b, d],
        col,
        Stroke::NONE,
    ));
}

/// 关闭 ×（两条交叉线）。
fn icon_close(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    let st = Stroke::new(1.8_f32, col);
    p.line_segment(
        [Pos2::new(c.x - s, c.y - s), Pos2::new(c.x + s, c.y + s)],
        st,
    );
    p.line_segment(
        [Pos2::new(c.x + s, c.y - s), Pos2::new(c.x - s, c.y + s)],
        st,
    );
}

/// 上级目录图标：向上箭头 + 底部横线。
fn icon_up_dir(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(c.x, c.y - s),
            Pos2::new(c.x + s * 0.75, c.y),
            Pos2::new(c.x + s * 0.28, c.y),
            Pos2::new(c.x + s * 0.28, c.y + s * 0.75),
            Pos2::new(c.x - s * 0.28, c.y + s * 0.75),
            Pos2::new(c.x - s * 0.28, c.y),
            Pos2::new(c.x - s * 0.75, c.y),
        ],
        col,
        Stroke::NONE,
    ));
}

/// 主目录图标：房顶三角 + 方体。
fn icon_home(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(c.x, c.y - s),
            Pos2::new(c.x + s, c.y - s * 0.1),
            Pos2::new(c.x + s * 0.75, c.y - s * 0.1),
            Pos2::new(c.x + s * 0.75, c.y + s),
            Pos2::new(c.x - s * 0.75, c.y + s),
            Pos2::new(c.x - s * 0.75, c.y - s * 0.1),
            Pos2::new(c.x - s, c.y - s * 0.1),
        ],
        col,
        Stroke::NONE,
    ));
    // 门洞（底色矩形）
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(c.x - s * 0.2, c.y + s * 0.25),
            Pos2::new(c.x + s * 0.2, c.y + s),
        ),
        CornerRadius::same(1),
        Color32::TRANSPARENT,
    );
}

/// 搜索图标：圆 + 柄。
fn icon_search(p: &egui::Painter, c: Pos2, s: f32, col: Color32) {
    p.circle_stroke(
        Pos2::new(c.x - s * 0.15, c.y - s * 0.15),
        s * 0.62,
        Stroke::new(1.8_f32, col),
    );
    p.line_segment(
        [
            Pos2::new(c.x + s * 0.3, c.y + s * 0.3),
            Pos2::new(c.x + s * 0.85, c.y + s * 0.85),
        ],
        Stroke::new(1.8_f32, col),
    );
}

/// 列头排序箭头：`asc` 朝上 / 否则朝下；`active` 用强调色。
fn icon_sort(p: &egui::Painter, c: Pos2, s: f32, asc: bool, active: bool, pal: &Palette) {
    let col = if active { pal.accent } else { pal.fg_weak };
    let pts = if asc {
        vec![
            Pos2::new(c.x - s, c.y + s * 0.5),
            Pos2::new(c.x + s, c.y + s * 0.5),
            Pos2::new(c.x, c.y - s * 0.6),
        ]
    } else {
        vec![
            Pos2::new(c.x - s, c.y - s * 0.5),
            Pos2::new(c.x + s, c.y - s * 0.5),
            Pos2::new(c.x, c.y + s * 0.6),
        ]
    };
    p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
}

/// 播放中指示（三根动画竖条，随帧起伏）。
fn eq_bars(p: &egui::Painter, min: Pos2, sz: Vec2, tick: u64, col: Color32) {
    let n = 3.0f32;
    let gap = sz.x * 0.12;
    let bw = (sz.x - gap * (n - 1.0)) / n;
    for k in 0..3 {
        let phase = (tick.wrapping_add(k as u64 * 7) as f32 * 0.35).sin();
        let h = sz.y * (0.35 + 0.6 * (0.5 + 0.5 * phase));
        let x = min.x + k as f32 * (bw + gap);
        let y1 = min.y + sz.y - h;
        p.rect_filled(
            Rect::from_min_max(Pos2::new(x, y1), Pos2::new(x + bw, min.y + sz.y)),
            CornerRadius::same(1),
            col,
        );
    }
}

/// 暂停指示（两根静态竖条）。
fn pause_bars(p: &egui::Painter, min: Pos2, sz: Vec2, col: Color32) {
    let bw = sz.x * 0.28;
    for dx in [0.0f32, sz.x * 0.72] {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(min.x + dx, min.y + sz.y * 0.15),
                Pos2::new(min.x + dx + bw, min.y + sz.y * 0.85),
            ),
            CornerRadius::same(1),
            col,
        );
    }
}

// =============================================================================
// 通用控件（圆形图标按钮 / Switch / 滑条 / seek 条）
// =============================================================================

/// 圆形图标按钮（图标颜色显式传入版——激活反白由调用方决定）。
fn round_btn_col(
    ui: &mut Ui,
    pal: &Palette,
    size: f32,
    active: bool,
    tooltip: &str,
    draw: impl FnOnce(&egui::Painter, Pos2, f32, Color32),
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let p = ui.painter_at(rect);
    let c = rect.center();
    let hover = resp.hovered();
    let bg = if active {
        pal.accent
    } else if hover {
        pal.sel_bg
    } else {
        pal.panel_bg
    };
    p.circle_filled(c, size * 0.5, bg);
    if active {
        p.circle_stroke(c, size * 0.5 + 1.5, Stroke::new(1.0_f32, pal.accent));
    }
    let icon_col = if active { pal.bg } else { pal.fg };
    draw(&p, c, size * 0.36, icon_col);
    resp.on_hover_text(tooltip).clicked()
}

/// 现代 Switch：圆角轨道 + 圆形滑块。返回值变化时 true。
fn switch(ui: &mut Ui, pal: &Palette, on: &mut bool, label: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 18.0), Sense::click());
        let p = ui.painter_at(rect);
        let track = Rect::from_min_size(
            rect.left_center() - Vec2::new(0.0, 9.0),
            Vec2::new(34.0, 18.0),
        );
        let r = CornerRadius::same(9);
        p.rect_filled(track, r, if *on { pal.accent } else { pal.grid });
        let cx = if *on {
            track.right() - 9.0
        } else {
            track.left() + 9.0
        };
        p.circle_filled(
            Pos2::new(cx, track.center().y),
            7.0,
            if *on { pal.bg } else { pal.fg },
        );
        if resp.clicked() {
            *on = !*on;
            changed = true;
        }
        if !label.is_empty() {
            ui.label(RichText::new(label).font(pf(13.0)).color(pal.fg));
        }
    });
    changed
}

/// seek 滑条：细轨 + 填充 + 圆滑块；拖动 / 点击 / 释出目标秒。
/// 悬停与拖动时滑块跟随指针（ghost），释放才真正 seek（不逐帧发命令）。
fn seek_bar(ui: &mut Ui, pal: &Palette, pos: f64, dur: f64) -> Option<f64> {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 20.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let cy = rect.center().y;
    let track = Rect::from_min_max(
        Pos2::new(rect.left(), cy - 2.5),
        Pos2::new(rect.right(), cy + 2.5),
    );
    p.rect_filled(track, CornerRadius::same(2), pal.grid);
    let frac = if dur > 0.0 {
        (pos / dur).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // 指针位置（悬停 / 拖动时作 ghost）
    let hover_pt = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
    let ghost = hover_pt.filter(|pt| rect.contains(*pt) && (resp.hovered() || resp.dragged()));
    let show_frac = ghost
        .map(|pt| ((pt.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0))
        .unwrap_or(frac as f32);
    // 填充：拖动中的 ghost 段用半透明强调（预览）
    let fill_col = if ghost.is_some() && resp.dragged() {
        pal.accent.linear_multiply(0.75)
    } else {
        pal.accent
    };
    if show_frac > 0.0 {
        let px = rect.left() + rect.width() * show_frac;
        p.rect_filled(
            Rect::from_min_max(Pos2::new(rect.left(), cy - 2.5), Pos2::new(px, cy + 2.5)),
            CornerRadius::same(2),
            fill_col,
        );
    }
    // 滑块圆
    let tx = rect.left() + rect.width() * show_frac;
    let tr_r = if resp.hovered() || resp.dragged() {
        6.5
    } else {
        5.0
    };
    p.circle_filled(Pos2::new(tx, cy), tr_r, pal.accent);
    p.circle_filled(Pos2::new(tx, cy), tr_r * 0.45, pal.bg);
    // 时间 ghost 提示（拖动中显示于滑块上方）
    if let (Some(_), true) = (&ghost, resp.dragged()) {
        if dur > 0.0 {
            let t = show_frac as f64 * dur;
            p.text(
                Pos2::new(tx, cy - 16.0),
                Align2::CENTER_BOTTOM,
                fmt_time(t),
                pf(11.0),
                pal.fg,
            );
        }
    }
    if dur > 0.0 && (resp.drag_stopped() || resp.clicked()) {
        if let Some(pt) = resp.interact_pointer_pos() {
            let rel = ((pt.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            return Some(rel as f64 * dur);
        }
    }
    None
}

/// 水平滑条（单行控件）：label + 轨 + 圆滑块 + 值。拖 / 点调值、
/// 双击回默认、滚轮微调（Shift 加速）。返回新值。
struct HSliderRow<'a> {
    label: &'a str,
    min: f32,
    max: f32,
    val: f32,
    def: f32,
    unit: &'a str,
    /// 小数位数（0 = 整数显示）。
    dec: usize,
}

fn hslider_row(ui: &mut Ui, pal: &Palette, s: HSliderRow<'_>) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 30.0),
        Sense::click_and_drag(),
    );
    let p = ui.painter_at(rect);
    let cy = rect.center().y;
    let x_label = rect.left();
    let x_val = rect.right();
    let x_track = rect.left() + 96.0;
    let val_w = 76.0; // 值列预留（右对齐文本不压轨道）
    let track_w = (x_val - val_w - 12.0 - x_track).max(40.0);
    // 标签
    p.text(
        Pos2::new(x_label, cy),
        Align2::LEFT_CENTER,
        s.label,
        pf(13.0),
        pal.fg,
    );
    // 轨
    let track = Rect::from_min_max(
        Pos2::new(x_track, cy - 2.5),
        Pos2::new(x_track + track_w, cy + 2.5),
    );
    p.rect_filled(track, CornerRadius::same(2), pal.grid);
    let prog = ((s.val - s.min) / (s.max - s.min)).clamp(0.0, 1.0);
    if prog > 0.0 {
        p.rect_filled(
            Rect::from_min_max(
                track.left_top(),
                Pos2::new(track.left() + track_w * prog, track.bottom()),
            ),
            CornerRadius::same(2),
            pal.spec_bar,
        );
    }
    // 默认位刻度
    let dprog = ((s.def - s.min) / (s.max - s.min)).clamp(0.0, 1.0);
    let dx = track.left() + track_w * dprog;
    p.line_segment(
        [Pos2::new(dx, cy - 6.0), Pos2::new(dx, cy + 6.0)],
        Stroke::new(1.0_f32, pal.fg_weak),
    );
    // 滑块
    let tx = track.left() + track_w * prog;
    p.circle_filled(Pos2::new(tx, cy), 6.5, pal.accent);
    p.circle_filled(Pos2::new(tx, cy), 2.8, pal.bg);
    // 值
    let text = if s.dec == 0 {
        format!("{:.0} {}", s.val, s.unit)
    } else {
        format!("{:.*} {}", s.dec, s.val, s.unit)
    };
    p.text(
        Pos2::new(x_val, cy),
        Align2::RIGHT_CENTER,
        text,
        pf(13.0),
        pal.fg,
    );
    // 交互
    if resp.dragged() || resp.clicked() {
        if let Some(pt) = resp.interact_pointer_pos() {
            let rel = ((pt.x - x_track) / track_w).clamp(0.0, 1.0);
            return Some(s.min + rel * (s.max - s.min));
        }
    }
    if resp.double_clicked() {
        return Some(s.def);
    }
    if resp.hovered() {
        let scroll: f32 = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                    _ => None,
                })
                .sum()
        });
        if scroll.abs() > 0.5 {
            let range = s.max - s.min;
            let step = if ui.input(|i| i.modifiers.shift) {
                range * 0.05
            } else {
                range * 0.02
            };
            let dir = if scroll > 0.0 { step } else { -step };
            return Some((s.val + dir).clamp(s.min, s.max));
        }
    }
    None
}

/// 竖直推子（EQ 段）：轨道 + 零线 + 增益填充 + 滑块帽。
/// 由 `eq_faders` 批量布局；本函数只画一段并处理交互。
#[allow(clippy::too_many_arguments)]
fn eq_fader(ui: &mut Ui, pal: &Palette, rect: Rect, resp: &egui::Response, gain: f32, freq: f32) {
    let p = ui.painter_at(rect);
    let cx = rect.center().x;
    let zero_y = rect.center().y;
    // 轨道（圆角竖条）
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(cx - 2.5, rect.top() + 4.0),
            Pos2::new(cx + 2.5, rect.bottom() - 4.0),
        ),
        CornerRadius::same(2),
        pal.grid,
    );
    // 零线
    p.line_segment(
        [Pos2::new(cx - 7.0, zero_y), Pos2::new(cx + 7.0, zero_y)],
        Stroke::new(1.0_f32, pal.fg_weak),
    );
    // 增益填充（0 → gain）
    let half = (rect.height() * 0.5 - 8.0).max(8.0);
    let gy = zero_y - (gain / 12.0) * half;
    let fill_col = if gain >= 0.0 {
        pal.spec_bar
    } else {
        pal.level_mid
    };
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(cx - 2.5, zero_y.min(gy)),
            Pos2::new(cx + 2.5, zero_y.max(gy)),
        ),
        CornerRadius::same(2),
        fill_col,
    );
    // 滑块帽（圆角横条）
    let hw = if resp.hovered() { 9.0 } else { 7.0 };
    p.rect_filled(
        Rect::from_min_max(Pos2::new(cx - hw, gy - 2.5), Pos2::new(cx + hw, gy + 2.5)),
        CornerRadius::same(2),
        pal.accent,
    );
    // dB 值（非零显示于帽侧）
    if gain.abs() >= 0.5 {
        p.text(
            Pos2::new(cx, gy - 12.0),
            Align2::CENTER_BOTTOM,
            format!("{:+.0}", gain),
            pf(10.0),
            pal.fg_weak,
        );
    }
    // 频率标（底部）
    let label = if freq >= 1000.0 {
        format!("{:.0}k", freq / 1000.0)
    } else {
        format!("{:.0}", freq)
    };
    p.text(
        Pos2::new(cx, rect.bottom() - 2.0),
        Align2::CENTER_BOTTOM,
        label,
        pf(10.0),
        pal.fg_weak,
    );
}

// =============================================================================
// impl MaxAppV2：现代呈现入口
// =============================================================================

impl MaxAppV2 {
    /// 现代顶栏：封面 + 曲目信息 + seek + 传输按钮 + 音量（整条 TopBottomPanel）。
    pub(super) fn render_now_playing_modern(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        // 换曲检测与媒体加载（与经典 render_now_playing 同源，抽出共用）
        self.ensure_track_media(ctx);

        let cur_cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.clone());
        let (title, artist, album_line, tech) = self
            .metadata
            .as_ref()
            .map(|md| {
                let title = cur_cue
                    .as_ref()
                    .map(|c| c.title.clone())
                    .or_else(|| md.title.clone())
                    .unwrap_or_else(|| self.i18n.t("metadata.unknown_title").into_owned());
                let artist = cur_cue
                    .as_ref()
                    .and_then(|c| c.performer.clone())
                    .or_else(|| md.artist.clone())
                    .unwrap_or_else(|| self.i18n.t("metadata.unknown_artist").into_owned());
                let album_line = match (&md.album, md.track_number) {
                    (Some(a), Some(n)) => {
                        format!("{a} · {} {n}", self.i18n.t("metadata.track_label"))
                    }
                    (Some(a), None) => a.clone(),
                    (None, Some(n)) => format!("{} {n}", self.i18n.t("metadata.track_label")),
                    (None, None) => self.i18n.t("metadata.unknown_album").into_owned(),
                };
                let tech = format!(
                    "{} · {} · {} · {}",
                    md.codec.as_deref().unwrap_or("?"),
                    md.bitrate_label(),
                    md.sample_rate_label(),
                    md.bits_label(),
                );
                (title, artist, album_line, tech)
            })
            .unwrap_or_else(|| {
                (
                    self.i18n.t("msg.empty").into_owned(),
                    String::new(),
                    String::new(),
                    String::new(),
                )
            });

        let status = self.status;
        let (pos, dur, playing) = match status {
            Some(s) => (s.position, s.duration, s.playing),
            None => (0.0, 0.0, false),
        };
        let vol = self
            .engine
            .as_ref()
            .map(|e| e.volume())
            .unwrap_or(self.config.volume);
        let muted = self.volume_before_mute.is_some();
        let repeat = self.config.repeat;
        let shuffle = self.playlist.is_shuffle();
        let medium = self
            .engine
            .as_ref()
            .map(|e| e.medium())
            .unwrap_or(PlaybackMedium::None);
        let tick = self.frame_tick;

        let t_shuffle = self.i18n.t("btn.shuffle").into_owned();
        let t_repeat = self.i18n.t("menu.repeat").into_owned();
        let t_medium = self.i18n.t("menu.medium").into_owned();
        let t_vol = self.i18n.t("modern.volume").into_owned();
        let t_mute = self.i18n.t("modern.mute").into_owned();
        let t_prev = self.i18n.t("menu.prev").into_owned();
        let t_next = self.i18n.t("menu.next").into_owned();
        let t_pp = self.i18n.t("menu.play_pause").into_owned();

        // 交互结果（闭包外执行）
        let mut seek_req: Option<f64> = None;
        let mut vol_new: Option<f32> = None;
        let mut toggle_mute = false;
        let mut cmd: Option<u8> = None; // 1=prev 2=pp 3=next 4=shuffle 5=repeat 6=medium
        let cover_tex = self.cover_tex.clone();

        egui::TopBottomPanel::top("np_modern")
            .exact_height(TOPBAR_H)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // —— 封面缩略（圆角，无封面画唱片图标）——
                    let cover_sz = 64.0;
                    let (crect, _cresp) =
                        ui.allocate_exact_size(Vec2::splat(cover_sz), Sense::hover());
                    let cp = ui.painter_at(crect);
                    match &cover_tex {
                        Some(tex) => {
                            cp.image(
                                tex.id(),
                                crect.shrink(1.0),
                                egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        None => {
                            cp.rect_filled(crect, CornerRadius::same(CTRL_R), pal.bg);
                            icon_disc(&cp, crect.center(), cover_sz * 0.28, pal.grid, pal.panel_bg);
                        }
                    }
                    // 封面圆角遮罩感：四角盖面板底色（简化圆角裁切）
                    // —— 用描边统一边界
                    cp.rect_stroke(
                        crect,
                        CornerRadius::same(CTRL_R),
                        Stroke::new(1.0_f32, pal.border),
                        egui::StrokeKind::Middle,
                    );

                    ui.add_space(10.0);

                    // —— 中列：标题 / 艺人·专辑 / 技术参数 / seek ——
                    ui.vertical(|ui| {
                        ui.set_min_width((ui.available_width() - 300.0).max(40.0));
                        ui.horizontal(|ui| {
                            // 播放状态指示（矢量，无字符）
                            let (ir, _) =
                                ui.allocate_exact_size(Vec2::new(16.0, 14.0), Sense::hover());
                            let ip = ui.painter_at(ir);
                            if playing {
                                eq_bars(&ip, ir.min, ir.size(), tick, pal.accent);
                            } else {
                                pause_bars(&ip, ir.min, ir.size(), pal.fg_weak);
                            }
                            let title_fit =
                                Self::fit_px(ui, &title, ui.available_width() - 8.0, &pf(16.0));
                            ui.label(
                                RichText::new(title_fit)
                                    .font(pf(16.0))
                                    .color(pal.fg)
                                    .strong(),
                            );
                        });
                        ui.label(
                            RichText::new(Self::fit_px(
                                ui,
                                &format!("{} · {}", artist, album_line),
                                ui.available_width(),
                                &pf(12.0),
                            ))
                            .font(pf(12.0))
                            .color(pal.fg_weak),
                        );
                        ui.label(RichText::new(&tech).font(pf(11.0)).color(pal.fg_weak));
                        ui.add_space(2.0);
                        // seek 条 + 时间
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(fmt_time(pos))
                                    .font(pf(11.0))
                                    .color(pal.fg_weak),
                            );
                            let w = ui.available_width() - 84.0;
                            ui.allocate_ui_with_layout(
                                Vec2::new(w.max(40.0), 20.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    if let Some(t) = seek_bar(ui, &pal, pos, dur) {
                                        seek_req = Some(t);
                                    }
                                },
                            );
                            ui.label(
                                RichText::new(fmt_time(dur))
                                    .font(pf(11.0))
                                    .color(pal.fg_weak),
                            );
                        });
                    });

                    ui.add_space(8.0);

                    // —— 右列：控制簇 ——
                    ui.vertical(|ui| {
                        // 上排：随机 / 循环 / 介质
                        ui.horizontal(|ui| {
                            if round_btn_col(ui, &pal, 22.0, shuffle, &t_shuffle, |p, c, s, col| {
                                icon_shuffle(p, c, s, col)
                            }) {
                                cmd = Some(4);
                            }
                            if round_btn_col(
                                ui,
                                &pal,
                                22.0,
                                repeat != RepeatMode::Off,
                                &t_repeat,
                                |p, c, s, col| {
                                    icon_repeat(p, c, s, repeat == RepeatMode::Single, col)
                                },
                            ) {
                                cmd = Some(5);
                            }
                            if round_btn_col(
                                ui,
                                &pal,
                                22.0,
                                medium != PlaybackMedium::None,
                                &format!("{} · {}", t_medium, self.i18n.t(medium_key(medium))),
                                |p, c, s, col| icon_disc(p, c, s, col, pal.bg),
                            ) {
                                cmd = Some(6);
                            }
                        });
                        // 中排：上一曲 / 播放（大） / 下一曲
                        ui.horizontal(|ui| {
                            if round_btn_col(ui, &pal, 26.0, false, &t_prev, |p, c, s, col| {
                                tr_icon(p, c, s, TrIcon::Prev, col)
                            }) {
                                cmd = Some(1);
                            }
                            if round_btn_col(ui, &pal, 34.0, false, &t_pp, |p, c, s, col| {
                                tr_icon(
                                    p,
                                    c,
                                    s,
                                    if playing { TrIcon::Pause } else { TrIcon::Play },
                                    col,
                                )
                            }) {
                                cmd = Some(2);
                            }
                            if round_btn_col(ui, &pal, 26.0, false, &t_next, |p, c, s, col| {
                                tr_icon(p, c, s, TrIcon::Next, col)
                            }) {
                                cmd = Some(3);
                            }
                        });
                        // 下排：静音 + 音量滑条
                        ui.horizontal(|ui| {
                            if round_btn_col(
                                ui,
                                &pal,
                                20.0,
                                muted,
                                if muted { &t_mute } else { &t_vol },
                                |p, c, s, col| icon_volume(p, c, s, muted, col),
                            ) {
                                toggle_mute = true;
                            }
                            let w = 96.0;
                            ui.allocate_ui_with_layout(
                                Vec2::new(w, 18.0),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    if let Some(v) = volume_bar(ui, &pal, vol) {
                                        vol_new = Some(v);
                                    }
                                },
                            );
                            ui.label(
                                RichText::new(format!("{}%", (vol * 100.0) as u32))
                                    .font(pf(11.0))
                                    .color(pal.fg_weak),
                            );
                        });
                    });
                });
            });

        // —— 应用交互结果 ——
        if toggle_mute {
            match self.volume_before_mute.take() {
                Some(v) => {
                    if let Some(e) = &self.engine {
                        e.send(tuneux_corex::AudioCmd::SetVolume(v));
                        self.config.volume = v;
                    }
                }
                None => {
                    if vol > 0.0 {
                        self.volume_before_mute = Some(vol);
                        if let Some(e) = &self.engine {
                            e.send(tuneux_corex::AudioCmd::SetVolume(0.0));
                        }
                    }
                }
            }
        }
        if let Some(v) = vol_new {
            // 手动拖动音量 = 解除静音记忆
            self.volume_before_mute = None;
            if let Some(e) = &self.engine {
                e.send(tuneux_corex::AudioCmd::SetVolume(v));
                self.config.volume = v;
            }
        }
        if let Some(t) = seek_req {
            let cue = self
                .playlist
                .current_index()
                .and_then(|i| self.playlist.items().get(i))
                .and_then(|it| it.cue.clone());
            let t = tuneux_mediax::clamp_seek_to_cue(t, cue.as_ref());
            if let Some(e) = &self.engine {
                e.send(tuneux_corex::AudioCmd::Seek(t));
            }
        }
        match cmd {
            Some(1) => {
                let o = self.playlist.prev(self.config.repeat);
                self.handle_nav_outcome(o);
            }
            Some(2) => self.toggle_play(),
            Some(3) => {
                let o = self.playlist.next(self.config.repeat);
                self.handle_nav_outcome(o);
            }
            Some(4) => {
                let on = !self.playlist.is_shuffle();
                self.playlist.set_shuffle(on);
                self.refresh_preload();
            }
            Some(5) => {
                self.config.repeat = match self.config.repeat {
                    RepeatMode::Off => RepeatMode::List,
                    RepeatMode::List => RepeatMode::Single,
                    RepeatMode::Single => RepeatMode::Off,
                };
                self.refresh_preload();
            }
            Some(6) => {
                if let Some(e) = &self.engine {
                    let all = PlaybackMedium::ALL;
                    let idx = all.iter().position(|&m| m == medium).unwrap_or(0);
                    e.set_medium(all[(idx + 1) % all.len()]);
                }
            }
            _ => {}
        }
    }

    /// 现代状态栏：单行，信息 + 提示体系 + 右键模块菜单（正确赋值
    /// status_rect——经典路径维持原状）。
    pub(super) fn render_status_bar_modern(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        let playing = self.status.as_ref().is_some_and(|s| s.playing);
        let (pos, dur) = self
            .status
            .as_ref()
            .map(|s| (s.position, s.duration))
            .unwrap_or((0.0, 0.0));
        let scanning = self.scan_pool.pending() > 0 || self.tag_added > 0;
        let count = self.playlist.items().len();
        let vol = (self.config.volume * 100.0) as u32;
        let rep = match self.config.repeat {
            RepeatMode::Off => String::new(),
            RepeatMode::List => self.i18n.t("repeat.list").into_owned(),
            RepeatMode::Single => self.i18n.t("repeat.single").into_owned(),
        };
        let shuf = if self.playlist.is_shuffle() {
            self.i18n.t("btn.shuffle").into_owned()
        } else {
            String::new()
        };
        let scanning_txt = self.i18n.t("msg.scanning").into_owned();
        let err = self.last_error.clone();
        let flash = self.flash.clone();
        let x_hint = self
            .x_confirm_at
            .is_some_and(|t| t.elapsed().as_secs() < 3)
            .then(|| self.i18n.t("msg.clear_confirm").into_owned());
        let tick = self.frame_tick;
        let title_now = self
            .metadata
            .as_ref()
            .and_then(|m| m.title.clone())
            .unwrap_or_default();

        let mut rect_out = egui::Rect::NOTHING;
        egui::TopBottomPanel::bottom("status_modern")
            .exact_height(30.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(10, 2)),
            )
            .show(ctx, |ui| {
                rect_out = ui.max_rect();
                let p = ui.painter().clone();
                let rect = ui.max_rect();
                let cy = rect.center().y;
                // 顶部分隔线
                p.line_segment(
                    [
                        Pos2::new(rect.left(), rect.top() + 0.5),
                        Pos2::new(rect.right(), rect.top() + 0.5),
                    ],
                    Stroke::new(1.0_f32, pal.border.linear_multiply(0.5)),
                );
                // 左：状态指示 + 标题
                let (ir, _) = ui.allocate_exact_size(Vec2::new(16.0, 14.0), Sense::hover());
                let ip = ui.painter_at(ir);
                if playing {
                    eq_bars(&ip, ir.min, ir.size(), tick, pal.accent);
                } else {
                    pause_bars(&ip, ir.min, ir.size(), pal.fg_weak);
                }
                let left_txt = if !title_now.is_empty() {
                    title_now
                } else {
                    self.i18n.t("panel.current_track").into_owned()
                };
                p.text(
                    Pos2::new(ir.right() + 6.0, cy),
                    Align2::LEFT_CENTER,
                    Self::fit_px(ui, &left_txt, rect.width() * 0.35, &pf(12.0)),
                    pf(12.0),
                    pal.fg,
                );
                // 右：信息
                let right = format!(
                    "{} · {} · {}{}{} · {}%",
                    count,
                    fmt_time(pos),
                    rep,
                    if rep.is_empty() || shuf.is_empty() {
                        String::new()
                    } else {
                        " · ".to_string()
                    },
                    shuf,
                    vol,
                );
                p.text(
                    Pos2::new(rect.right() - 6.0, cy),
                    Align2::RIGHT_CENTER,
                    right,
                    pf(12.0),
                    pal.fg_weak,
                );
                // 中：提示体系（错误 > x 确认 > 扫描 > flash）
                let mid = if let Some(e) = &err {
                    (e.clone(), Color32::from_rgb(230, 80, 80))
                } else if let Some(h) = &x_hint {
                    (h.clone(), pal.accent)
                } else if scanning {
                    (scanning_txt, pal.accent)
                } else if let Some(f) = &flash {
                    (f.clone(), pal.accent)
                } else if dur > 0.0 {
                    (fmt_time(dur), pal.fg_weak)
                } else {
                    (String::new(), pal.fg_weak)
                };
                if !mid.0.is_empty() {
                    p.text(
                        Pos2::new(rect.center().x, cy),
                        Align2::CENTER_CENTER,
                        Self::fit_px(ui, &mid.0, rect.width() * 0.4, &pf(12.0)),
                        pf(12.0),
                        mid.1,
                    );
                }
            });
        self.status_rect = rect_out;
    }

    /// 现代 dock 叶子：圆角卡片 + 头部条（标题 / 关闭 × / 拖拽换位 /
    /// 右键模块菜单）+ 模块内容。
    pub(super) fn render_leaf_modern(&mut self, ui: &mut Ui, rect: Rect, module: dock::ModuleId) {
        let pal = self.palette.clone();
        // 卡片底
        let p = ui.painter_at(rect);
        p.rect_filled(rect, CornerRadius::same(CARD_R), pal.panel_bg);
        p.rect_stroke(
            rect,
            CornerRadius::same(CARD_R),
            Stroke::new(1.0_f32, pal.border.linear_multiply(0.55)),
            egui::StrokeKind::Middle,
        );
        // 头部条
        let head = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 26.0));
        let hp = ui.painter_at(head);
        // 底色（与卡片同色但略沉的边线区分）
        hp.line_segment(
            [
                Pos2::new(head.left() + 4.0, head.bottom() - 0.5),
                Pos2::new(head.right() - 4.0, head.bottom() - 0.5),
            ],
            Stroke::new(1.0_f32, pal.border.linear_multiply(0.4)),
        );
        let title = self.i18n.t(module.label_key()).into_owned();
        hp.text(
            Pos2::new(head.left() + 10.0, head.center().y),
            Align2::LEFT_CENTER,
            &title,
            pf(12.0),
            pal.fg_weak,
        );
        // 关闭 ×（命中即关闭本叶）
        let close_rect = Rect::from_min_size(
            Pos2::new(head.right() - 26.0, head.center().y - 9.0),
            Vec2::splat(18.0),
        );
        let close_resp = ui
            .allocate_rect(close_rect, Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let close_col = if close_resp.hovered() {
            pal.accent
        } else {
            pal.fg_weak
        };
        icon_close(&hp, close_rect.center(), 4.0, close_col);
        // 内容区
        let inner = Rect::from_min_max(
            Pos2::new(rect.left() + 2.0, head.bottom() + 2.0),
            Pos2::new(rect.right() - 2.0, rect.bottom() - 4.0),
        );
        ui.allocate_ui_at_rect(inner, |ui| {
            ui.push_id(format!("mdock_{module:?}"), |ui| {
                self.render_module_modern(module, ui);
            });
        });
        // —— 头部交互：右键模块菜单 / 拖拽换位（与经典同语义）——
        if !self.menu_open_at_frame_start
            && ui.input(|i| i.pointer.secondary_clicked())
            && ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pp| head.contains(pp) && !close_rect.contains(pp))
        {
            self.dock_leaf_menu = Some((
                module,
                ui.input(|i| i.pointer.hover_pos()).unwrap_or_default(),
                self.frame_tick,
            ));
            self.leaf_page = 0;
        }
        let band_resp = ui
            .allocate_rect(
                Rect::from_min_max(head.min, Pos2::new(close_rect.left(), head.bottom())),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        if band_resp.drag_started() {
            self.dock_drag = Some(module);
        }
        if let Some(d) = self.dock_drag {
            if d != module
                && ui.input(|i| i.pointer.primary_released())
                && ui
                    .input(|i| i.pointer.hover_pos())
                    .is_some_and(|pp| rect.contains(pp))
            {
                self.dock_swap = Some((d, module));
            }
        }
        if close_resp.clicked() {
            if let Some(tree) = self.config.dock.take() {
                self.config.dock = tree.prune(module);
            }
        }
    }

    /// 现代模式模块分发。
    pub(super) fn render_module_modern(&mut self, m: dock::ModuleId, ui: &mut Ui) {
        match m {
            dock::ModuleId::Browser => self.render_browser_modern(ui),
            dock::ModuleId::Playlist => self.render_playlist_modern(ui),
            dock::ModuleId::Spectrum => match self.spectrum_mode {
                SpectrumMode::Oscilloscope => self.render_oscilloscope_modern(ui),
                _ => self.render_spectrum_modern(ui),
            },
            dock::ModuleId::Cover => self.render_cover_modern(ui),
            dock::ModuleId::Lyrics => self.render_lyrics_modern(ui),
            dock::ModuleId::Eq => self.render_eq_modern(ui),
            dock::ModuleId::Comp => self.render_comp_modern(ui),
            dock::ModuleId::Waveform => self.render_waveform_modern(ui),
            dock::ModuleId::Filter => self.render_filter_modern(ui),
            dock::ModuleId::Visualizer => self.render_visualizer_modern(ui),
            dock::ModuleId::DspChain => self.render_dspchain_modern(ui),
            // 指针表：内容即 TUI 风，两模式共渲染（经典入口另有 tui_box 框饰）
            dock::ModuleId::VuGauge => self.render_gauge_module(ui),
        }
    }

    // ==================== 浏览器（现代） ====================

    fn render_browser_modern(&mut self, ui: &mut Ui) {
        self.browser_rect = ui.max_rect();
        let pal = self.palette.clone();
        let cwd = self.browser.cwd().to_path_buf();
        let search_here = self.ui.mode == UiMode::Search && self.search_target_browser;
        let mut query_local = self.ui.search_query.clone();
        let want_focus = self.search_focus_req;
        let mut search_blur = false;
        let empty_hint = self.i18n.t("empty.dir").into_owned();
        let empty_search_hint = self.i18n.t("search.esc_exit_search").into_owned();
        let esc_hint = self.i18n.t("search.esc_exit").into_owned();
        let t_up = self.i18n.t("modern.up").into_owned();
        let t_home = self.i18n.t("modern.home").into_owned();

        // 行数据快照
        let entries: Vec<(usize, String, bool)> = self
            .browser
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| (i, e.name().to_string(), e.is_dir()))
            .collect();
        let sel = self.browser.selected();

        // 交互收集
        let mut go_up = false;
        let mut go_home = false;
        let mut nav: Option<PathBuf> = None;
        let mut click = None;
        let mut dbl_dir = false;
        let mut dbl_file = None;
        let mut rmb_row: Option<(usize, Pos2)> = None;
        let mut drag_src = None;

        // —— 工具条：上级 / 主目录 / 面包屑 ——
        ui.horizontal(|ui| {
            if round_btn_col(ui, &pal, 22.0, false, &t_up, |p, c, s, col| {
                icon_up_dir(p, c, s, col)
            }) {
                go_up = true;
            }
            if round_btn_col(ui, &pal, 22.0, false, &t_home, |p, c, s, col| {
                icon_home(p, c, s, col)
            }) {
                go_home = true;
            }
            ui.add_space(4.0);
            // 面包屑（可点路径段；过长只留尾部 4 段 + 省略号）
            let mut comps: Vec<(String, PathBuf)> = Vec::new();
            let mut acc = PathBuf::new();
            for c in cwd.components() {
                acc.push(c);
                let label = acc
                    .components()
                    .next_back()
                    .map(|x| x.as_os_str().to_string_lossy().into_owned())
                    .unwrap_or_default();
                comps.push((label, acc.clone()));
            }
            let skip = comps.len().saturating_sub(4);
            if skip > 0 {
                ui.label(RichText::new("…").font(pf(12.0)).color(pal.fg_weak));
            }
            for (label, path) in comps.iter().skip(skip) {
                let resp = ui.add(
                    egui::Button::new(RichText::new(label).font(pf(12.0)).color(pal.accent))
                        .fill(Color32::TRANSPARENT),
                );
                if resp.clicked() {
                    nav = Some(path.clone());
                }
                ui.label(RichText::new("/").font(pf(12.0)).color(pal.fg_weak));
            }
        });
        ui.add_space(4.0);

        // —— 搜索行（搜索态）——
        if search_here {
            ui.horizontal(|ui| {
                let (ir, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                let ip = ui.painter_at(ir);
                icon_search(&ip, ir.center(), 5.0, pal.fg_weak);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut query_local)
                        .font(pf(13.0))
                        .desired_width(ui.available_width() - 90.0),
                );
                ui.label(RichText::new(esc_hint).font(pf(11.0)).color(pal.fg_weak));
                if want_focus {
                    resp.request_focus();
                }
                if !want_focus
                    && !resp.has_focus()
                    && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                {
                    search_blur = true;
                }
            });
            ui.add_space(2.0);
        }

        if entries.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                let hint = if search_here {
                    empty_search_hint
                } else {
                    empty_hint
                };
                ui.label(RichText::new(hint).font(pf(12.0)).color(pal.fg_weak));
            });
        } else {
            let scroll_follow = self.scroll_to_sel && self.ui.focus == FocusTarget::Browser;
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, entries.len(), |ui, range| {
                    ui.set_min_width(ui.available_width());
                    for idx in range {
                        let Some((i, name, is_dir)) = entries.get(idx) else {
                            continue;
                        };
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_H),
                            Sense::click_and_drag(),
                        );
                        let is_sel = *i == sel;
                        let p = ui.painter_at(rect);
                        let row = rect.shrink2(Vec2::new(2.0, 1.0));
                        if is_sel {
                            p.rect_filled(row, CornerRadius::same(CTRL_R), pal.accent);
                        } else if resp.hovered() {
                            p.rect_filled(row, CornerRadius::same(CTRL_R), pal.sel_bg);
                        }
                        let fg = if is_sel { pal.bg } else { pal.fg };
                        // 矢量图标（目录 / 音频 / CUE）
                        let irect = Rect::from_min_size(
                            Pos2::new(row.left() + 6.0, row.center().y - 8.0),
                            Vec2::splat(16.0),
                        );
                        if *is_dir {
                            icon_folder(&p, irect.min, irect.size(), pal.spec_bar, pal.panel_bg);
                        } else if name.to_lowercase().ends_with(".cue") {
                            icon_cue(&p, irect.min, irect.size(), pal.accent);
                        } else {
                            icon_note(&p, irect.min, irect.size(), pal.fg_weak);
                        }
                        let name_fit =
                            Self::fit_px(ui, name, row.right() - irect.right() - 8.0, &pf(13.0));
                        p.text(
                            Pos2::new(irect.right() + 6.0, row.center().y),
                            Align2::LEFT_CENTER,
                            name_fit,
                            pf(13.0),
                            fg,
                        );
                        if resp.clicked() {
                            click = Some(*i);
                        }
                        if resp.double_clicked() {
                            if *is_dir {
                                dbl_dir = true;
                            } else {
                                dbl_file = Some(*i);
                            }
                        }
                        if resp.secondary_clicked() {
                            if let Some(pos) = resp.hover_pos() {
                                rmb_row = Some((*i, pos));
                            }
                        }
                        if resp.drag_started() {
                            drag_src = Some(*i);
                        }
                        if scroll_follow && *i == sel {
                            ui.scroll_to_rect(resp.rect, Some(egui::Align::Center));
                        }
                    }
                });
        }

        // —— 应用交互 ——
        if search_here {
            self.search_focus_req = false;
            if query_local != self.ui.search_query {
                self.ui.search_query = query_local.clone();
                self.browser.set_filter(&query_local);
            }
        }
        if search_blur {
            self.end_search();
        }
        if go_up {
            self.browser.go_up();
        }
        if go_home {
            if let Some(h) = dirs::home_dir() {
                self.browser.navigate_to(&h);
            }
        }
        if let Some(t) = nav {
            self.browser.navigate_to(&t);
        }
        if let Some(i) = click {
            self.browser.select(i);
            self.ui.focus = FocusTarget::Browser;
        }
        if dbl_dir {
            self.browser.enter_selected();
        }
        if let Some(i) = dbl_file {
            if let Some(entry) = self.browser.entries().get(i) {
                let p = entry.path().to_path_buf();
                if let Some(idx) = self.add_file_sync(&p) {
                    self.play_item(idx);
                }
            }
        }
        if let Some((i, pos)) = rmb_row {
            if !self.menu_open_at_frame_start {
                self.browser_menu = Some((i, pos, self.frame_tick));
            }
            self.blank_menu = None;
        }
        if let Some(i) = drag_src {
            self.drag_browser = Some(i);
        }
    }

    // ==================== 播放列表（现代） ====================

    fn render_playlist_modern(&mut self, ui: &mut Ui) {
        self.playlist_rect = ui.max_rect();
        let pal = self.palette.clone();
        let focused = self.ui.focus == FocusTarget::Playlist;
        let empty_hint = self.i18n.t("empty.playlist").into_owned();
        let search_here = self.ui.mode == UiMode::Search && !self.search_target_browser;
        let want_focus = self.search_focus_req;
        let mut search_blur = false;
        let mut query_local = self.ui.search_query.clone();
        let esc_hint = self.i18n.t("search.esc_exit").into_owned();
        let empty_search_hint = self.i18n.t("search.esc_exit_search").into_owned();
        let unknown_album = self.i18n.t("group.unknown_album").into_owned();
        let count_tpl = self.i18n.t("group.track_count").into_owned();
        let col_title = self.i18n.t("col.title").into_owned();
        let col_artist = self.i18n.t("col.artist").into_owned();
        let col_album = self.i18n.t("col.album").into_owned();
        let col_dur = self.i18n.t("col.dur").into_owned();
        let flat = self.config.view == PlaylistView::Flat;
        let sel = self.ui.playlist_selected;
        let current = self.playlist.current_index();
        let playing = self.status.as_ref().is_some_and(|s| s.playing);
        let sort_key = self.pl_sort;
        let sort_desc = self.pl_sort_desc;
        let tick = self.frame_tick;

        let rows = self.build_rows();

        // 元数据惰性加载（经典同口径）
        let pending: Vec<PathBuf> = rows
            .iter()
            .filter_map(|r| match r {
                PlRow::Track { item, .. } => {
                    self.playlist.items().get(*item).map(|it| it.path.clone())
                }
                PlRow::Header { .. } => None,
            })
            .filter(|pp| !self.metadata_cache.contains_key(pp))
            .collect();
        for pp in pending {
            let md = TrackMetadata::from_file(&pp);
            self.metadata_cache.insert(pp, md);
        }

        // 交互收集
        let mut click = None;
        let mut dbl = None;
        let mut rmb_row: Option<(usize, Pos2)> = None;
        let mut drop_here = false;
        let mut sort_req: Option<u8> = None; // 0=默认 1=标题 2=艺术家 3=专辑 4=时长
        let mut drag_from: Option<usize> = None;
        let mut move_req: Option<(usize, usize)> = None;

        // 搜索行
        if search_here {
            ui.horizontal(|ui| {
                let (ir, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                let ip = ui.painter_at(ir);
                icon_search(&ip, ir.center(), 5.0, pal.fg_weak);
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut query_local)
                        .font(pf(13.0))
                        .desired_width(ui.available_width() - 90.0),
                );
                ui.label(RichText::new(esc_hint).font(pf(11.0)).color(pal.fg_weak));
                if want_focus {
                    resp.request_focus();
                }
                if !want_focus
                    && !resp.has_focus()
                    && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                {
                    search_blur = true;
                }
            });
            ui.add_space(2.0);
        }

        if rows.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                let hint = if search_here {
                    empty_search_hint
                } else {
                    empty_hint
                };
                ui.label(RichText::new(hint).font(pf(12.0)).color(pal.fg_weak));
            });
        } else {
            // —— 列头（可点排序；`#` 恢复默认序）——
            if !search_here {
                let (hrect, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
                let hp = ui.painter_at(hrect);
                hp.line_segment(
                    [
                        Pos2::new(hrect.left() + 2.0, hrect.bottom() - 0.5),
                        Pos2::new(hrect.right() - 2.0, hrect.bottom() - 0.5),
                    ],
                    Stroke::new(1.0_f32, pal.border.linear_multiply(0.5)),
                );
                // 列几何：# 36 | 标题 40% | 艺术家 25% | 专辑 18%(平铺) | 时长 52 右
                let x_num = hrect.left() + 4.0;
                let dur_w = 52.0;
                let flex = hrect.width() - 36.0 - dur_w - 8.0;
                let album_w = if flat { flex * 0.18 } else { 0.0 };
                let title_w = flex * 0.40;
                let artist_w = flex * 0.25;
                let x_title = x_num + 36.0;
                let x_artist = x_title + title_w;
                let x_album = x_artist + artist_w;
                let x_dur = hrect.right() - dur_w;
                let f = pf(12.0);
                let mut mk_col =
                    |ui: &mut Ui, hp: &egui::Painter, x: f32, w: f32, label: &str, key: u8| {
                        let crect = Rect::from_min_size(
                            Pos2::new(x, hrect.top()),
                            Vec2::new(w, hrect.height()),
                        );
                        let cresp = ui.allocate_rect(crect, Sense::click());
                        let col = if sort_key == key && key > 0 {
                            pal.accent
                        } else {
                            pal.fg_weak
                        };
                        hp.text(
                            Pos2::new(crect.left() + 4.0, hrect.center().y),
                            Align2::LEFT_CENTER,
                            label,
                            f.clone(),
                            col,
                        );
                        if key > 0 {
                            // 排序方向箭头（激活列显示）
                            let active = sort_key == key;
                            let asc = if active { !sort_desc } else { true };
                            icon_sort(
                                hp,
                                Pos2::new(crect.right() - 8.0, hrect.center().y),
                                3.5,
                                asc,
                                active,
                                &pal,
                            );
                            if cresp.clicked() {
                                // 点击同列 = 翻转方向；换列 = 升序
                                if sort_key == key {
                                    sort_req = Some(100 + key); // 翻转编码
                                } else {
                                    sort_req = Some(key);
                                }
                            }
                        } else if cresp.clicked() {
                            sort_req = Some(0);
                        }
                        let _ = cresp;
                    };
                mk_col(ui, &hp, x_num, 36.0, "#", 0);
                mk_col(ui, &hp, x_title, title_w, &col_title, 1);
                mk_col(ui, &hp, x_artist, artist_w, &col_artist, 2);
                if flat {
                    mk_col(ui, &hp, x_album, album_w, &col_album, 3);
                }
                mk_col(ui, &hp, x_dur, dur_w, &col_dur, 4);
                let _ = x_album;
            }

            // 浏览器拖放（经典同口径）
            let released = ui.input(|i| i.pointer.primary_released());
            let pointer_in = ui
                .max_rect()
                .contains(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()));
            if released && pointer_in && self.drag_browser.is_some() {
                drop_here = true;
            }

            let scroll_follow = self.scroll_to_sel && focused;
            let drag_active = self.pl_drag_from;

            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, rows.len(), |ui, range| {
                    ui.set_min_width(ui.available_width());
                    for ri in range {
                        let Some(row) = rows.get(ri) else {
                            continue;
                        };
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), ROW_H),
                            Sense::click_and_drag(),
                        );
                        let p = ui.painter_at(rect);
                        let rowr = rect.shrink2(Vec2::new(2.0, 1.0));
                        let is_sel = sel == Some(ri);
                        match row {
                            PlRow::Header { album, count } => {
                                let is_c = self.collapsed.contains(album);
                                if resp.hovered() {
                                    p.rect_filled(rowr, CornerRadius::same(CTRL_R), pal.sel_bg);
                                }
                                // chevron + 专辑名 + 曲数
                                let cc = Pos2::new(rowr.left() + 12.0, rowr.center().y);
                                icon_chevron(&p, cc, 4.5, !is_c, pal.accent);
                                let label = if album.is_empty() {
                                    unknown_album.as_str()
                                } else {
                                    album.as_str()
                                };
                                let text = format!(
                                    "{}  ({})",
                                    label,
                                    count_tpl.replace("{}", &count.to_string())
                                );
                                p.text(
                                    Pos2::new(cc.x + 14.0, rowr.center().y),
                                    Align2::LEFT_CENTER,
                                    Self::fit_px(ui, &text, rowr.width() - 34.0, &pf(13.0)),
                                    pf(13.0),
                                    pal.accent,
                                );
                            }
                            PlRow::Track { item, seq } => {
                                let (item_idx, seq) = (*item, *seq);
                                let items = self.playlist.items();
                                let path = &items[item_idx].path;
                                let md = self.metadata_cache.get(path);
                                let cue_title =
                                    items[item_idx].cue.as_ref().map(|c| c.title.clone());
                                let name = cue_title
                                    .or_else(|| md.and_then(|m| m.title.clone()))
                                    .unwrap_or_else(|| {
                                        path.file_stem()
                                            .and_then(|st| st.to_str())
                                            .unwrap_or("?")
                                            .to_string()
                                    });
                                let artist = md.and_then(|m| m.artist.clone()).unwrap_or_default();
                                let album = md.and_then(|m| m.album.clone()).unwrap_or_default();
                                let dur = md
                                    .and_then(|m| m.duration)
                                    .map(fmt_time)
                                    .unwrap_or_default();
                                let is_cur = current == Some(item_idx);
                                // 行底色
                                if is_sel {
                                    p.rect_filled(rowr, CornerRadius::same(CTRL_R), pal.accent);
                                } else if resp.hovered() {
                                    p.rect_filled(rowr, CornerRadius::same(CTRL_R), pal.sel_bg);
                                } else if is_cur {
                                    p.rect_filled(
                                        rowr,
                                        CornerRadius::same(CTRL_R),
                                        pal.sel_bg.linear_multiply(1.3),
                                    );
                                }
                                // 拖拽重排：目标行上缘高亮线
                                if drag_active.is_some()
                                    && resp.contains_pointer()
                                    && drag_active != Some(item_idx)
                                {
                                    p.line_segment(
                                        [
                                            Pos2::new(rowr.left(), rowr.top()),
                                            Pos2::new(rowr.right(), rowr.top()),
                                        ],
                                        Stroke::new(2.0_f32, pal.accent),
                                    );
                                }
                                let fg = if is_sel { pal.bg } else { pal.fg };
                                // 状态列：当前曲 = 播放/暂停指示；否则序号
                                let x_num = rowr.left() + 4.0;
                                if is_cur {
                                    let irect = Rect::from_min_size(
                                        Pos2::new(x_num, rowr.center().y - 7.0),
                                        Vec2::new(14.0, 12.0),
                                    );
                                    if playing {
                                        eq_bars(&p, irect.min, irect.size(), tick, pal.accent);
                                    } else {
                                        pause_bars(&p, irect.min, irect.size(), pal.accent);
                                    }
                                } else {
                                    p.text(
                                        Pos2::new(x_num + 8.0, rowr.center().y),
                                        Align2::RIGHT_CENTER,
                                        seq.to_string(),
                                        pf(12.0),
                                        pal.fg_weak,
                                    );
                                }
                                // 列几何（与列头同式）
                                let dur_w = 52.0;
                                let flex = rowr.width() - 36.0 - dur_w - 8.0;
                                let album_w = if flat { flex * 0.18 } else { 0.0 };
                                let title_w = flex * 0.40;
                                let artist_w = flex * 0.25;
                                let x_title = x_num + 36.0;
                                let x_artist = x_title + title_w;
                                let x_album = x_artist + artist_w;
                                let x_dur = rowr.right() - dur_w;
                                p.text(
                                    Pos2::new(x_title, rowr.center().y),
                                    Align2::LEFT_CENTER,
                                    Self::fit_px(ui, &name, title_w - 8.0, &pf(13.0)),
                                    pf(13.0),
                                    fg,
                                );
                                p.text(
                                    Pos2::new(x_artist, rowr.center().y),
                                    Align2::LEFT_CENTER,
                                    Self::fit_px(ui, &artist, artist_w - 8.0, &pf(12.0)),
                                    pf(12.0),
                                    if is_sel { fg } else { pal.fg_weak },
                                );
                                if flat {
                                    p.text(
                                        Pos2::new(x_album, rowr.center().y),
                                        Align2::LEFT_CENTER,
                                        Self::fit_px(ui, &album, album_w - 8.0, &pf(12.0)),
                                        pf(12.0),
                                        if is_sel { fg } else { pal.fg_weak },
                                    );
                                }
                                p.text(
                                    Pos2::new(x_dur, rowr.center().y),
                                    Align2::LEFT_CENTER,
                                    &dur,
                                    pf(12.0),
                                    if is_sel { fg } else { pal.fg_weak },
                                );
                                // —— 交互 ——
                                // 拖拽重排（平铺 + 非搜索）：起点
                                if resp.drag_started() && flat && !search_here {
                                    drag_from = Some(item_idx);
                                }
                                // 拖拽重排：终点（在目标行上释放）
                                if let Some(from) = drag_active {
                                    if from != item_idx
                                        && resp.contains_pointer()
                                        && ui.input(|i| i.pointer.primary_released())
                                    {
                                        move_req = Some((from, item_idx));
                                    }
                                }
                            }
                        }
                        if resp.clicked() {
                            click = Some(ri);
                        }
                        if resp.double_clicked() {
                            dbl = Some(ri);
                        }
                        if resp.secondary_clicked() {
                            if let Some(pos) = resp.hover_pos() {
                                rmb_row = Some((ri, pos));
                            }
                        }
                        if scroll_follow && is_sel {
                            ui.scroll_to_rect(resp.rect, Some(egui::Align::Center));
                        }
                    }
                });
        }

        // —— 应用交互 ——
        if search_here {
            self.search_focus_req = false;
            if query_local != self.ui.search_query {
                self.ui.search_query = query_local;
            }
        }
        if search_blur {
            self.end_search();
        }
        if let Some(ri) = click {
            self.ui.playlist_selected = Some(ri);
            self.ui.focus = FocusTarget::Playlist;
        }
        if let Some(ri) = dbl {
            self.ui.playlist_selected = Some(ri);
            let rows = self.build_rows();
            match rows.get(ri) {
                Some(PlRow::Track { item, .. }) => {
                    self.play_item(*item);
                }
                Some(PlRow::Header { album, .. }) => {
                    let a = album.clone();
                    self.toggle_collapse(&a);
                }
                None => {}
            }
        }
        if let Some((ri, pos)) = rmb_row {
            if !self.menu_open_at_frame_start {
                self.playlist_menu = Some((ri, pos, self.frame_tick));
            }
            self.blank_menu = None;
        }
        if drop_here {
            if let Some(src) = self.drag_browser.take() {
                if let Some(entry) = self.browser.entries().get(src) {
                    let pp = entry.path().to_path_buf();
                    if pp.is_dir() {
                        self.enqueue_scan(pp);
                    } else {
                        self.add_file_sync(&pp);
                    }
                }
            }
        }
        if let Some(f) = drag_from {
            self.pl_drag_from = Some(f);
        }
        if let Some((from, to)) = move_req {
            self.playlist.move_item(from, to);
            self.pl_drag_from = None;
        }
        // 全局释放兜底清拖拽态
        if ui.input(|i| i.pointer.primary_released()) {
            self.pl_drag_from = None;
        }
        // 排序请求
        if let Some(code) = sort_req {
            if code == 0 {
                self.pl_sort = 0;
                self.sort_playlist();
            } else if code >= 100 {
                // 同列翻转：翻方向并立即重排（pl_sort 已是该列）
                self.pl_sort_desc = !self.pl_sort_desc;
                self.sort_playlist();
            } else {
                self.pl_sort = code;
                self.pl_sort_desc = false;
                self.sort_playlist();
            }
            self.ui.playlist_selected = None;
        }
    }

    /// 按当前 `pl_sort` 重排播放列表（保留当前曲：按路径找回）。
    fn sort_playlist(&mut self) {
        let cur_path = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .map(|it| it.path.clone());
        match self.pl_sort {
            1 => {
                // 标题（缓存标题 > 文件名）
                self.playlist.items_mut().sort_by(|a, b| {
                    let key = |it: &PlaylistItem| {
                        self.metadata_cache
                            .get(&it.path)
                            .and_then(|m| m.title.clone())
                            .unwrap_or_else(|| {
                                it.path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("")
                                    .to_string()
                            })
                    };
                    let (ka, kb) = (key(a), key(b));
                    if self.pl_sort_desc {
                        kb.cmp(&ka)
                    } else {
                        ka.cmp(&kb)
                    }
                });
            }
            2 => {
                // 艺术家
                self.playlist.items_mut().sort_by(|a, b| {
                    let key = |it: &PlaylistItem| {
                        self.metadata_cache
                            .get(&it.path)
                            .and_then(|m| m.artist.clone())
                            .unwrap_or_default()
                    };
                    let (ka, kb) = (key(a), key(b));
                    if self.pl_sort_desc {
                        kb.cmp(&ka)
                    } else {
                        ka.cmp(&kb)
                    }
                });
            }
            3 => {
                // 专辑（专辑内按曲号）
                self.playlist.items_mut().sort_by(|a, b| {
                    let ord = if self.pl_sort_desc {
                        b.album
                            .clone()
                            .unwrap_or_default()
                            .cmp(&a.album.clone().unwrap_or_default())
                    } else {
                        a.album
                            .clone()
                            .unwrap_or_default()
                            .cmp(&b.album.clone().unwrap_or_default())
                    };
                    ord.then(
                        a.track_number
                            .unwrap_or(u32::MAX)
                            .cmp(&b.track_number.unwrap_or(u32::MAX)),
                    )
                });
            }
            4 => {
                // 时长（缓存元数据，缺省排末尾）
                self.playlist.items_mut().sort_by(|a, b| {
                    let key = |it: &PlaylistItem| {
                        self.metadata_cache
                            .get(&it.path)
                            .and_then(|m| m.duration)
                            .unwrap_or(f64::MAX)
                    };
                    let (ka, kb) = (key(a), key(b));
                    if self.pl_sort_desc {
                        kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
                    } else {
                        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
                    }
                });
            }
            _ => {
                // 默认序：专辑 + 曲号（mediax 标准序）
                PlaylistCore::sort_items(self.playlist.items_mut());
            }
        }
        // 排序后历史失效（索引全部漂移）——清空；当前曲按路径找回。
        self.playlist.clear_history();
        if let Some(p) = cur_path {
            if let Some(idx) = self.playlist.items().iter().position(|it| it.path == p) {
                self.playlist.set_current(idx);
            }
        }
        self.refresh_preload();
    }

    // ==================== 频谱 / 示波器（现代：图形路径） ====================

    fn render_spectrum_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        // dt + 峰值保持（经典同口径：corex SpectrumPeakHold）
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|t| now.duration_since(t))
            .unwrap_or_default();
        self.last_frame = Some(now);
        let spectrum = if let Some(e) = &self.engine {
            let [l, r] = e.spectrum_lr();
            let mut m = [0.0f32; tuneux_corex::spectrum::N_BANDS];
            for i in 0..tuneux_corex::spectrum::N_BANDS {
                m[i] = ((l[i] + r[i]) / 2.0).clamp(0.0, 1.0);
            }
            m
        } else {
            [0.0; tuneux_corex::spectrum::N_BANDS]
        };
        let peaks = self.spectrum_peaks.update(&spectrum, dt);
        let show_axis = true;
        let mut zoomed = false;
        let content = ui.max_rect();
        Self::draw_spectrum_gui(ui, &pal, content, &spectrum, &peaks, show_axis);
        // 右键循环放大（经典同款白名单交互）
        if ui.input(|i| {
            i.pointer.secondary_clicked()
                && content.contains(i.pointer.hover_pos().unwrap_or_default())
        }) {
            zoomed = true;
        }
        if zoomed {
            const STEPS: [f32; 5] = [15.0, 18.0, 22.0, 26.0, 32.0];
            let cur = STEPS
                .iter()
                .position(|&x| (x - self.spectrum_font).abs() < 0.5)
                .unwrap_or(0);
            self.spectrum_font = STEPS[(cur + 1) % STEPS.len()];
        }
    }

    fn render_oscilloscope_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let waves = self
            .engine
            .as_ref()
            .map(|e| e.waveform_lr())
            .unwrap_or([[0.0; tuneux_corex::spectrum::WAVEFORM_LEN]; 2]);
        Self::draw_oscilloscope_gui(ui, &pal, &waves);
    }

    // ==================== 封面 / 歌词（现代） ====================

    fn render_cover_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let none_hint = self.i18n.t("cover.none").into_owned();
        if let Some(tex) = &self.cover_tex {
            let avail = ui.available_size();
            let tsz = tex.size_vec2();
            let scale = (avail.x / tsz.x).min(avail.y / tsz.y).min(1.0);
            let display = (tsz * scale).max(Vec2::splat(16.0));
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::new(tex)
                        .max_size(display)
                        .corner_radius(CornerRadius::same(CARD_R)),
                );
            });
        } else {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                let (ir, _) = ui.allocate_exact_size(Vec2::splat(64.0), Sense::hover());
                let ip = ui.painter_at(ir);
                icon_disc(&ip, ir.center(), 26.0, pal.grid, pal.panel_bg);
                ui.label(RichText::new(none_hint).font(pf(12.0)).color(pal.fg_weak));
            });
        }
    }

    fn render_lyrics_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let empty_hint = self.i18n.t("empty.lyrics").into_owned();
        let pos = self.status.as_ref().map(|s| s.position).unwrap_or(0.0);
        let lines: Vec<(f64, String)> = self
            .lyrics
            .as_ref()
            .map(|ly| {
                ly.lines
                    .iter()
                    .map(|l| (l.timestamp, l.text.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let cur_line = self
            .lyrics
            .as_ref()
            .map(|ly| ly.current_line(pos))
            .unwrap_or(0);
        if lines.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.label(RichText::new(empty_hint).font(pf(12.0)).color(pal.fg_weak));
            });
            return;
        }
        let mut seek_line: Option<f64> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H + 4.0, lines.len(), |ui, range| {
                for idx in range {
                    let Some((ts, text)) = lines.get(idx) else {
                        continue;
                    };
                    let is_cur = idx == cur_line;
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), ROW_H + 4.0),
                        Sense::click(),
                    );
                    let p = ui.painter_at(rect);
                    if resp.hovered() && !is_cur {
                        p.rect_filled(
                            rect.shrink2(Vec2::new(2.0, 1.0)),
                            CornerRadius::same(CTRL_R),
                            pal.sel_bg,
                        );
                    }
                    let col = if is_cur { pal.accent } else { pal.fg_weak };
                    let sz = if is_cur { 15.0 } else { 13.0 };
                    p.text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        Self::fit_px(ui, text, rect.width() - 16.0, &pf(sz)),
                        pf(sz),
                        col,
                    );
                    // 点击带时间戳的行 = seek 到该行
                    if resp.clicked() && *ts > 0.0 {
                        seek_line = Some(*ts);
                    }
                    if is_cur {
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    }
                }
            });
        if let Some(t) = seek_line {
            let cue = self
                .playlist
                .current_index()
                .and_then(|i| self.playlist.items().get(i))
                .and_then(|it| it.cue.clone());
            let base = cue
                .as_ref()
                .map(|c| c.start_ms as f64 / 1000.0)
                .unwrap_or(0.0);
            let cue_end = cue.as_ref().and_then(|c| c.end_ms);
            let target = if cue.is_some() { base + t } else { t };
            let target = match cue_end {
                Some(e) => target.min(e as f64 / 1000.0),
                None => target,
            };
            if let Some(e) = &self.engine {
                e.send(tuneux_corex::AudioCmd::Seek(target));
            }
        }
    }

    // ==================== EQ / 压缩器 / 滤波器（现代） ====================

    fn render_eq_modern(&mut self, ui: &mut Ui) {
        if self.eq_plugin.is_none() {
            let hint = self.i18n.t("msg.plugin_eq_missing").into_owned();
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(hint)
                        .font(pf(12.0))
                        .color(self.palette.fg_weak),
                );
            });
            return;
        }
        if self.eq_slot.is_none() {
            if let Some(e) = &self.engine {
                self.eq_slot = e.alloc_eq_slot();
            }
        }
        let Some((_, params)) = self.eq_slot.clone() else {
            return;
        };
        let pal = self.palette.clone();
        let t_on = self.i18n.t("eq.on").into_owned();
        let t_preset = self.i18n.t("eq.preset").into_owned();
        let t_reset = self.i18n.t("eq.reset").into_owned();

        // 顶部：开关
        let mut on = params.enabled();
        if switch(ui, &pal, &mut on, &t_on) {
            params.set_enabled(on);
        }
        ui.add_space(4.0);

        // 十段竖推子
        let n_bands = tuneux_corex::EQ_BANDS;
        let fader_h = (ui.available_height() - 46.0).max(80.0);
        let col_w = (ui.available_width() / n_bands as f32).max(28.0);
        let (frect, fresp) = ui.allocate_exact_size(
            Vec2::new(col_w * n_bands as f32, fader_h),
            Sense::click_and_drag(),
        );
        // 交互（拖 / 点调增益、双击归零、滚轮微调）
        let mut set_band: Option<(usize, f32)> = None;
        if fresp.dragged() || fresp.clicked() {
            if let Some(pt) = fresp.interact_pointer_pos() {
                let band = (((pt.x - frect.left()) / col_w).floor() as usize).clamp(0, n_bands - 1);
                // 与 eq_fader 绘制公式互逆：gy = zero_y - gain/12*half
                let zero_y = frect.top() + frect.height() * 0.5;
                let half = (frect.height() * 0.5 - 8.0).max(8.0);
                let gain = ((zero_y - pt.y) / half * 12.0).clamp(-12.0, 12.0);
                set_band = Some((band, gain));
            }
        }
        if fresp.double_clicked() {
            if let Some(pt) = fresp.interact_pointer_pos() {
                let band = (((pt.x - frect.left()) / col_w).floor() as usize).clamp(0, n_bands - 1);
                set_band = Some((band, 0.0));
            }
        }
        // dB 刻度（左侧）
        let p = ui.painter_at(frect);
        for db in [-12i32, -6, 0, 6, 12] {
            let frac = 0.5 - db as f32 / 24.0;
            let y = frect.top() + frect.height() * frac;
            p.text(
                Pos2::new(frect.left() - 2.0, y),
                Align2::RIGHT_CENTER,
                format!("{:+}", db),
                pf(9.0),
                pal.fg_weak,
            );
        }
        for bi in 0..n_bands {
            let seg = Rect::from_min_size(
                Pos2::new(frect.left() + bi as f32 * col_w, frect.top()),
                Vec2::new(col_w, frect.height()),
            );
            // 段级命中（悬停态给推子加宽）
            let hovered = ui
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|hp| seg.contains(hp));
            let hr = ui
                .allocate_rect(seg, Sense::hover())
                .on_hover_cursor(egui::CursorIcon::ResizeVertical);
            let gain = params.band(bi);
            eq_fader(ui, &pal, seg, &hr, gain, tuneux_corex::EQ_FREQS[bi]);
            let _ = hovered;
            // 滚轮微调（悬停段）
            if hr.hovered() {
                let scroll: f32 = ui.input(|i| {
                    i.events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                            _ => None,
                        })
                        .sum()
                });
                if scroll.abs() > 0.5 {
                    let step = if ui.input(|i| i.modifiers.shift) {
                        3.0
                    } else {
                        1.0
                    };
                    let dir = if scroll > 0.0 { step } else { -step };
                    set_band = Some((bi, (gain + dir).clamp(-12.0, 12.0)));
                }
            }
        }
        if let Some((band, gain)) = set_band {
            params.set_band(band, gain);
        }

        // 预设行
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t_preset).font(pf(12.0)).color(pal.fg_weak));
            for (name, vals) in EQ_PRESETS {
                if ui.button(name).clicked() {
                    for (i, v) in vals.iter().enumerate() {
                        params.set_band(i, *v);
                    }
                }
            }
            if ui.button(&t_reset).clicked() {
                params.reset();
            }
        });
    }

    fn render_comp_modern(&mut self, ui: &mut Ui) {
        if self.comp_plugin.is_none() {
            let hint = self.i18n.t("msg.plugin_comp_missing").into_owned();
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(hint)
                        .font(pf(12.0))
                        .color(self.palette.fg_weak),
                );
            });
            return;
        }
        if self.comp_slot.is_none() {
            if let Some(e) = &self.engine {
                self.comp_slot = e.alloc_compressor_slot();
            }
        }
        let Some((_, params)) = self.comp_slot.clone() else {
            return;
        };
        let pal = self.palette.clone();
        let (l_thresh, l_ratio, l_attack, l_release, l_makeup) = (
            self.i18n.t("msg.comp_threshold").into_owned(),
            self.i18n.t("msg.comp_ratio").into_owned(),
            self.i18n.t("msg.comp_attack").into_owned(),
            self.i18n.t("msg.comp_release").into_owned(),
            self.i18n.t("msg.comp_makeup").into_owned(),
        );
        let mut on = params.enabled();
        if switch(ui, &pal, &mut on, "") {
            params.set_enabled(on);
        }
        ui.add_space(4.0);
        let rows = [
            HSliderRow {
                label: &l_thresh,
                min: -60.0,
                max: 0.0,
                val: params.threshold(),
                def: -20.0,
                unit: "dB",
                dec: 0,
            },
            HSliderRow {
                label: &l_ratio,
                min: 1.0,
                max: 20.0,
                val: params.ratio(),
                def: 4.0,
                unit: ":1",
                dec: 1,
            },
            HSliderRow {
                label: &l_attack,
                min: 1.0,
                max: 200.0,
                val: params.attack_ms(),
                def: 25.0,
                unit: "ms",
                dec: 0,
            },
            HSliderRow {
                label: &l_release,
                min: 10.0,
                max: 1000.0,
                val: params.release_ms(),
                def: 250.0,
                unit: "ms",
                dec: 0,
            },
            HSliderRow {
                label: &l_makeup,
                min: 0.0,
                max: 24.0,
                val: params.makeup(),
                def: 0.0,
                unit: "dB",
                dec: 0,
            },
        ];
        for (ri, r) in rows.iter().enumerate() {
            if let Some(v) = hslider_row(
                ui,
                &pal,
                HSliderRow {
                    label: r.label,
                    min: r.min,
                    max: r.max,
                    val: r.val,
                    def: r.def,
                    unit: r.unit,
                    dec: r.dec,
                },
            ) {
                match ri {
                    0 => {
                        params.set_threshold(v);
                    }
                    1 => {
                        params.set_ratio(v);
                    }
                    2 => {
                        params.set_attack_ms(v);
                    }
                    3 => {
                        params.set_release_ms(v);
                    }
                    _ => {
                        params.set_makeup(v);
                    }
                }
            }
        }
    }

    /// 现代滤波器：两个矢量旋钮（弧 + 指针 + 中心帽），拖竖向调值。
    fn render_filter_modern(&mut self, ui: &mut Ui) {
        if self.filter_slot.is_none() {
            if let Some(e) = &self.engine {
                self.filter_slot = e.alloc_filter_slot();
            }
        }
        let Some((_, params)) = self.filter_slot.clone() else {
            return;
        };
        let pal = self.palette.clone();
        let l_cutoff = self.i18n.t("filter.cutoff").into_owned();
        let l_reson = self.i18n.t("filter.resonance").into_owned();
        let l_off = self.i18n.t("filter.off").into_owned();

        let mut on = params.enabled();
        let label = if on { String::new() } else { l_off.clone() };
        if switch(ui, &pal, &mut on, &label) {
            params.set_enabled(on);
        }
        ui.add_space(4.0);

        // 两旋钮参数
        let knobs: [(String, f32, f32, f32, f32, bool, usize); 2] = [
            (
                l_cutoff,
                20.0,
                20000.0,
                20000.0,
                params.cutoff_hz(),
                true,
                0,
            ),
            (l_reson, 0.1, 20.0, 0.707, params.resonance_q(), false, 1),
        ];
        let mut set_param: Option<(usize, f32)> = None;
        let avail_w = ui.available_width();
        let knob_r = (avail_w * 0.16).clamp(22.0, 44.0);
        let area_h = knob_r * 2.0 + 46.0;
        let (area, _) = ui.allocate_exact_size(Vec2::new(avail_w, area_h), Sense::hover());
        let p = ui.painter_at(area);
        let gap = avail_w * 0.2;
        let total_w = knob_r * 4.0 + gap;
        let start_x = (avail_w - total_w) * 0.5;
        for (ki, (label, min, max, dflt, val, is_log, pidx)) in knobs.iter().enumerate() {
            let cx = area.left() + start_x + ki as f32 * (knob_r * 2.0 + gap) + knob_r;
            let cy = area.top() + knob_r + 10.0;
            let norm = if *is_log {
                ((val.log2() - min.log2()) / (max.log2() - min.log2())).clamp(0.0, 1.0)
            } else {
                ((val - min) / (max - min)).clamp(0.0, 1.0)
            };
            // 旋钮体
            p.circle_filled(Pos2::new(cx + 2.0, cy + 2.0), knob_r, pal.bg);
            p.circle_filled(Pos2::new(cx, cy), knob_r, pal.panel_bg);
            p.circle_stroke(
                Pos2::new(cx, cy),
                knob_r,
                Stroke::new(1.5_f32, pal.border.linear_multiply(0.6)),
            );
            // 值弧（-225° → +45°）
            let start_angle = -225.0_f32.to_radians();
            let sweep = 270.0_f32.to_radians();
            let arc_r = knob_r - 6.0;
            let steps = 24;
            let mut prev = Pos2::new(
                cx + start_angle.cos() * arc_r,
                cy + start_angle.sin() * arc_r,
            );
            for si in 1..=steps {
                let t = start_angle + sweep * si as f32 / steps as f32;
                let pt = Pos2::new(cx + t.cos() * arc_r, cy + t.sin() * arc_r);
                p.line_segment([prev, pt], Stroke::new(2.0_f32, pal.grid));
                prev = pt;
            }
            if norm > 0.01 {
                let value_angle = start_angle + norm * sweep;
                let mut prev = Pos2::new(
                    cx + start_angle.cos() * arc_r,
                    cy + start_angle.sin() * arc_r,
                );
                for si in 1..=steps {
                    let t = start_angle + (value_angle - start_angle) * si as f32 / steps as f32;
                    let pt = Pos2::new(cx + t.cos() * arc_r, cy + t.sin() * arc_r);
                    p.line_segment([prev, pt], Stroke::new(3.0_f32, pal.spec_bar));
                    prev = pt;
                }
            }
            // 指针 + 中心帽
            let value_angle = start_angle + norm * sweep;
            let nx = cx + value_angle.cos() * arc_r * 0.8;
            let ny = cy + value_angle.sin() * arc_r * 0.8;
            p.line_segment(
                [Pos2::new(cx, cy), Pos2::new(nx, ny)],
                Stroke::new(2.5_f32, pal.accent),
            );
            p.circle_filled(Pos2::new(cx, cy), 4.0, pal.grid);
            p.circle_filled(Pos2::new(cx - 1.0, cy - 1.0), 3.0, pal.accent);
            // 标签 + 值
            p.text(
                Pos2::new(cx, cy + knob_r + 12.0),
                Align2::CENTER_CENTER,
                label,
                pf(12.0),
                pal.fg,
            );
            let val_text = if *pidx == 0 {
                if *val >= 1000.0 {
                    format!("{:.1} kHz", val / 1000.0)
                } else {
                    format!("{:.0} Hz", val)
                }
            } else {
                format!("Q {:.2}", val)
            };
            p.text(
                Pos2::new(cx, cy + knob_r + 26.0),
                Align2::CENTER_CENTER,
                val_text,
                pf(11.0),
                pal.fg_weak,
            );
            // 交互区
            let krect =
                Rect::from_center_size(Pos2::new(cx, cy), Vec2::new(knob_r * 2.4, knob_r * 2.4));
            let resp = ui
                .allocate_rect(krect, Sense::click_and_drag())
                .on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if resp.dragged() {
                let dy = resp.drag_delta().y;
                let sensitivity = if *is_log { 0.005 } else { 0.003 };
                let new_norm = (norm - dy * sensitivity).clamp(0.0, 1.0);
                let new_val = if *is_log {
                    2.0f32.powf(min.log2() + new_norm * (max.log2() - min.log2()))
                } else {
                    min + new_norm * (max - min)
                };
                set_param = Some((*pidx, new_val));
            }
            if resp.double_clicked() {
                set_param = Some((*pidx, *dflt));
            }
            if resp.hovered() {
                let scroll: f32 = ui.input(|i| {
                    i.events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                            _ => None,
                        })
                        .sum()
                });
                if scroll.abs() > 0.5 {
                    let step = if ui.input(|i| i.modifiers.shift) {
                        0.05
                    } else {
                        0.02
                    };
                    let dir = if scroll > 0.0 { step } else { -step };
                    let new_norm = (norm + dir).clamp(0.0, 1.0);
                    let new_val = if *is_log {
                        2.0f32.powf(min.log2() + new_norm * (max.log2() - min.log2()))
                    } else {
                        min + new_norm * (max - min)
                    };
                    set_param = Some((*pidx, new_val));
                }
            }
        }
        if let Some((idx, v)) = set_param {
            match idx {
                0 => {
                    params.set_cutoff_hz(v);
                }
                _ => {
                    params.set_resonance_q(v);
                }
            }
        }
    }

    // ==================== 波形 / 可视化 / DSP 链（现代） ====================

    fn render_waveform_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let (pos, dur) = match &self.status {
            Some(s) => (s.position, s.duration),
            None => (0.0, 0.0),
        };
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let p = ui.painter_at(rect);
        let mid = rect.center().y;
        let progress = if dur > 0.0 {
            (pos / dur).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let play_x = rect.left() + progress as f32 * rect.width();
        let env: Vec<f32> = match &self.track_envelope {
            Some((_, e)) => e.clone(),
            None => vec![0.5; 256],
        };
        let n = env.len();
        let bar_w = rect.width() / n as f32;
        for (i, peak) in env.iter().enumerate() {
            let x = rect.left() + i as f32 * bar_w;
            let boosted = (peak * 1.5).clamp(0.03, 1.0);
            let h = (boosted * rect.height() * 0.45).max(1.5);
            let played = x < play_x;
            let color = if played { pal.spec_bar } else { pal.grid };
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x, mid - h),
                    Pos2::new(x + bar_w.max(1.0) - 0.5, mid + h),
                ),
                CornerRadius::same(1),
                color,
            );
        }
        // 播放头
        p.line_segment(
            [
                Pos2::new(play_x, rect.top() + 4.0),
                Pos2::new(play_x, rect.bottom() - 4.0),
            ],
            Stroke::new(2.0_f32, pal.accent),
        );
        p.text(
            Pos2::new(rect.left() + 2.0, rect.bottom() - 8.0),
            Align2::LEFT_BOTTOM,
            fmt_time(pos),
            pf(10.0),
            pal.fg_weak,
        );
        if dur > 0.0 {
            p.text(
                Pos2::new(rect.right() - 2.0, rect.bottom() - 8.0),
                Align2::RIGHT_BOTTOM,
                fmt_time(dur),
                pf(10.0),
                pal.fg_weak,
            );
            if let Some(pt) = resp.interact_pointer_pos() {
                let rel = ((pt.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let target = rel as f64 * dur;
                if resp.dragged() {
                    let px = rect.left() + rel * rect.width();
                    p.text(
                        Pos2::new(px, rect.top() + 4.0),
                        Align2::CENTER_TOP,
                        fmt_time(target),
                        pf(10.0),
                        pal.peak_fg,
                    );
                } else if resp.drag_stopped() || resp.clicked() {
                    if let Some(e) = &self.engine {
                        e.send(tuneux_corex::AudioCmd::Seek(target));
                    }
                }
            }
        }
    }

    fn render_visualizer_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        if self.visual_plugins.is_empty() {
            let hint = self.i18n.t("msg.no_visual").into_owned();
            ui.vertical_centered(|ui| {
                ui.add_space(20.0);
                ui.label(RichText::new(hint).font(pf(12.0)).color(pal.fg_weak));
            });
            return;
        }
        // 插件产出的是字符画（插件内容约定），保持等宽渲染
        let text = self.visual_text.clone();
        egui::ScrollArea::both().show(ui, |ui| {
            for line in text.lines() {
                ui.label(RichText::new(line).monospace().color(pal.spec_bar));
            }
        });
    }

    fn render_dspchain_modern(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let (l_eq, l_comp, l_filt) = (
            self.i18n.t("dsp.eq").into_owned(),
            self.i18n.t("dsp.comp").into_owned(),
            self.i18n.t("dsp.filter").into_owned(),
        );
        let (l_on, l_off) = (
            self.i18n.t("dsp.active").into_owned(),
            self.i18n.t("dsp.bypassed").into_owned(),
        );
        // 状态行数据：(名称, 已装载, 启用, 切换目标)
        let rows: Vec<(String, bool, bool)> = vec![
            (
                l_eq,
                self.eq_plugin.is_some(),
                self.eq_slot.as_ref().is_some_and(|(_, p)| p.enabled()),
            ),
            (
                l_comp.clone(),
                self.comp_plugin.is_some(),
                self.comp_slot.as_ref().is_some_and(|(_, p)| p.enabled()),
            ),
            (
                l_filt.clone(),
                true,
                self.filter_slot.as_ref().is_some_and(|(_, p)| p.enabled()),
            ),
        ];
        let mut toggle: Option<usize> = None;
        for (ri, (name, loaded, on)) in rows.iter().enumerate() {
            let (rect, resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::click());
            let p = ui.painter_at(rect);
            let rowr = rect.shrink2(Vec2::new(2.0, 1.0));
            if resp.hovered() {
                p.rect_filled(rowr, CornerRadius::same(CTRL_R), pal.sel_bg);
            }
            // 状态点：装载+启用 = 绿（level_low）；装载未启用 = 灰；未装载 = 暗
            let dot_col = if !*loaded {
                pal.grid
            } else if *on {
                pal.level_low
            } else {
                pal.fg_weak
            };
            p.circle_filled(Pos2::new(rowr.left() + 12.0, rowr.center().y), 4.0, dot_col);
            p.text(
                Pos2::new(rowr.left() + 24.0, rowr.center().y),
                Align2::LEFT_CENTER,
                name,
                pf(13.0),
                pal.fg,
            );
            let status = if !*loaded {
                l_off.clone()
            } else if *on {
                l_on.clone()
            } else {
                l_off.clone()
            };
            p.text(
                Pos2::new(rowr.right() - 8.0, rowr.center().y),
                Align2::RIGHT_CENTER,
                status,
                pf(12.0),
                if *loaded && *on {
                    pal.level_low
                } else {
                    pal.fg_weak
                },
            );
            if resp.clicked() && *loaded {
                toggle = Some(ri);
            }
        }
        if let Some(ri) = toggle {
            match ri {
                0 => {
                    if let Some((_, p)) = &self.eq_slot {
                        let on = !p.enabled();
                        p.set_enabled(on);
                    }
                }
                1 => {
                    if let Some((_, p)) = &self.comp_slot {
                        let on = !p.enabled();
                        p.set_enabled(on);
                    }
                }
                _ => {
                    if let Some((_, p)) = &self.filter_slot {
                        let on = !p.enabled();
                        p.set_enabled(on);
                    }
                }
            }
        }
    }
}

/// 音量滑条（内联短版）：返回新值。
fn volume_bar(ui: &mut Ui, pal: &Palette, vol: f32) -> Option<f32> {
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 18.0), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    let cy = rect.center().y;
    p.rect_filled(
        Rect::from_min_max(
            Pos2::new(rect.left(), cy - 2.0),
            Pos2::new(rect.right(), cy + 2.0),
        ),
        CornerRadius::same(2),
        pal.grid,
    );
    let frac = vol.clamp(0.0, 1.0);
    if frac > 0.0 {
        p.rect_filled(
            Rect::from_min_max(
                Pos2::new(rect.left(), cy - 2.0),
                Pos2::new(rect.left() + rect.width() * frac, cy + 2.0),
            ),
            CornerRadius::same(2),
            pal.accent,
        );
    }
    let tx = rect.left() + rect.width() * frac;
    p.circle_filled(Pos2::new(tx, cy), 5.0, pal.accent);
    p.circle_filled(Pos2::new(tx, cy), 2.0, pal.bg);
    if resp.dragged() || resp.clicked() {
        if let Some(pt) = resp.interact_pointer_pos() {
            let rel = ((pt.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            return Some(rel);
        }
    }
    if resp.double_clicked() {
        return Some(0.8);
    }
    None
}

#[cfg(test)]
mod tests {
    //! 现代层的纯函数级测试（交互路径由 egui 驱动，此处覆盖可独立
    //! 断言的部分：图标函数不 panic + 离屏 painter 冒烟）。

    #[test]
    fn icons_smoke_no_panic() {
        // 图标函数全部是 painter 原语组合，任何尺寸/中心不得 panic。
        // 离屏 painter（虚拟画布）跑一轮冒烟。
        use eframe::egui::{self, Context, Painter, Pos2, Rect, Vec2};
        let ctx = Context::default();
        let painter = Painter::new(
            ctx,
            egui::LayerId::background(),
            Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 200.0)),
        );
        let c = Pos2::new(100.0, 100.0);
        super::tr_icon(&painter, c, 10.0, super::TrIcon::Prev, egui::Color32::WHITE);
        super::tr_icon(&painter, c, 10.0, super::TrIcon::Play, egui::Color32::WHITE);
        super::tr_icon(
            &painter,
            c,
            10.0,
            super::TrIcon::Pause,
            egui::Color32::WHITE,
        );
        super::tr_icon(&painter, c, 10.0, super::TrIcon::Stop, egui::Color32::WHITE);
        super::tr_icon(&painter, c, 10.0, super::TrIcon::Next, egui::Color32::WHITE);
        super::icon_shuffle(&painter, c, 8.0, egui::Color32::WHITE);
        super::icon_repeat(&painter, c, 8.0, false, egui::Color32::WHITE);
        super::icon_repeat(&painter, c, 8.0, true, egui::Color32::WHITE);
        super::icon_volume(&painter, c, 8.0, false, egui::Color32::WHITE);
        super::icon_volume(&painter, c, 8.0, true, egui::Color32::WHITE);
        super::icon_disc(&painter, c, 8.0, egui::Color32::WHITE, egui::Color32::BLACK);
        super::icon_folder(
            &painter,
            Pos2::new(10.0, 10.0),
            Vec2::new(16.0, 12.0),
            egui::Color32::WHITE,
            egui::Color32::BLACK,
        );
        super::icon_note(
            &painter,
            Pos2::new(10.0, 10.0),
            Vec2::new(12.0, 16.0),
            egui::Color32::WHITE,
        );
        super::icon_cue(
            &painter,
            Pos2::new(10.0, 10.0),
            Vec2::new(12.0, 16.0),
            egui::Color32::WHITE,
        );
        super::icon_chevron(&painter, c, 4.0, true, egui::Color32::WHITE);
        super::icon_chevron(&painter, c, 4.0, false, egui::Color32::WHITE);
        super::icon_close(&painter, c, 4.0, egui::Color32::WHITE);
        super::icon_up_dir(&painter, c, 8.0, egui::Color32::WHITE);
        super::icon_home(&painter, c, 8.0, egui::Color32::WHITE);
        super::icon_search(&painter, c, 6.0, egui::Color32::WHITE);
        super::icon_sort(
            &painter,
            c,
            4.0,
            true,
            true,
            &crate::theme::Palette::default(),
        );
        super::icon_sort(
            &painter,
            c,
            4.0,
            false,
            false,
            &crate::theme::Palette::default(),
        );
        super::eq_bars(
            &painter,
            Pos2::new(10.0, 10.0),
            Vec2::new(14.0, 12.0),
            7,
            egui::Color32::WHITE,
        );
        super::pause_bars(
            &painter,
            Pos2::new(10.0, 10.0),
            Vec2::new(14.0, 12.0),
            egui::Color32::WHITE,
        );
    }
}
