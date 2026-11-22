//! # 可选行控件
//!
//! 稳定 Id（不依赖行号/文字）+ 选中反白 + 悬停淡底；返回的响应可直接
//! 接右键菜单。参考共享 UI 骨架的 `selectable_row` 契约。

use eframe::egui;

use crate::theme::Palette;

/// 绘制一行可选条目并返回交互响应。
///
/// `id` 必须由调用方稳定唯一指定（如 `("browser", index)`）。
/// `sense` 由调用方指定：仅选中用 `click()`；需同时拖拽用 `click_and_drag()`
/// （单一 widget 同时感知单击/双击/右键/拖拽，避免重叠 widget 互相吞事件）。
pub fn selectable_row(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: egui::Vec2,
    selected: bool,
    label: &str,
    sense: egui::Sense,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let response = ui.interact(rect, id, sense);
    let palette = Palette::current(ui.ctx());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 2.0, palette.sel_bg);
    } else if response.hovered() {
        painter.rect_filled(rect, 2.0, palette.fg.gamma_multiply(0.08));
    }
    let text_color = if selected { palette.sel_fg } else { palette.fg };
    painter.text(
        rect.left_center() + egui::vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(14.0),
        text_color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, label)
    });
    response
}

/// 播放列表四列行控件：序号 | 歌名 | 表演者 | 时长（右对齐）。
///
/// 与 [`selectable_row`] 的区别：四列分别绘制在固定 x 偏移处，
/// 保证跨行对齐（比例字体下拼接字符串无法对齐）；表演者列用 dim 色、
/// 时长右对齐。歌名/表演者超宽时用 egui 布局引擎截断（尾部省略号）。
#[allow(clippy::too_many_arguments)]
pub fn playlist_track_row(
    ui: &mut egui::Ui,
    id: egui::Id,
    size: egui::Vec2,
    selected: bool,
    seq: usize,
    icon: &str,
    title: &str,
    artist: &str,
    duration: &str,
    sense: egui::Sense,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let response = ui.interact(rect, id, sense);
    let palette = Palette::current(ui.ctx());
    let painter = ui.painter();

    if selected {
        painter.rect_filled(rect, 2.0, palette.sel_bg);
    } else if response.hovered() {
        painter.rect_filled(rect, 2.0, palette.fg.gamma_multiply(0.08));
    }
    let text_color = if selected { palette.sel_fg } else { palette.fg };
    let dim_color = if selected {
        palette.sel_fg
    } else {
        palette.dim
    };
    let font = egui::FontId::monospace(14.0);

    // —— 列 x 布局（像素）：序号 36 | 图标 18 | 歌名弹性 | 表演者 25% | 时长 76 右对齐 ——
    let seq_x = 6.0;
    let icon_x = 42.0;
    let dur_w = 76.0;
    let artist_w = (rect.width() - icon_x - dur_w - 36.0) * 0.28;
    let artist_x = rect.width() - dur_w - artist_w - 12.0;
    // 歌名实际起点 = icon_x + 18；宽度预算到表演者列左缘再留 8px 间隙，
    // 避免截断后的歌名尾部压进表演者列。
    let title_x = icon_x + 18.0;
    let title_w = (artist_x - title_x - 8.0).max(0.0);

    // 序号（左对齐，右对齐补空格）
    painter.text(
        rect.left_center() + egui::vec2(seq_x, 0.0),
        egui::Align2::LEFT_CENTER,
        format!("{seq:>3}."),
        font.clone(),
        text_color,
    );
    // 图标（▶/‖/空）
    if !icon.is_empty() {
        painter.text(
            rect.left_center() + egui::vec2(icon_x, 0.0),
            egui::Align2::LEFT_CENTER,
            icon,
            font.clone(),
            text_color,
        );
    }
    // 歌名（弹性宽度，超宽截断加省略号）
    let title_tr = truncate_to_px(painter, title, title_w, &font);
    painter.text(
        rect.left_center() + egui::vec2(title_x, 0.0),
        egui::Align2::LEFT_CENTER,
        title_tr,
        font.clone(),
        text_color,
    );
    // 表演者（固定宽度，dim 色，空则不画）
    if !artist.is_empty() {
        let artist_tr = truncate_to_px(painter, artist, artist_w, &font);
        painter.text(
            rect.left_center() + egui::vec2(artist_x, 0.0),
            egui::Align2::LEFT_CENTER,
            artist_tr,
            font.clone(),
            dim_color,
        );
    }
    // 时长（右对齐）
    if !duration.is_empty() {
        painter.text(
            rect.right_center() + egui::vec2(-6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            duration,
            font,
            dim_color,
        );
    }

    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, title)
    });
    response
}

/// 用 egui 布局引擎测量并截断到 `max_px` 像素内（尾部加省略号）。
fn truncate_to_px(painter: &egui::Painter, s: &str, max_px: f32, font: &egui::FontId) -> String {
    if s.is_empty() || max_px <= 0.0 {
        return String::new();
    }
    let galley = painter.layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::WHITE);
    if galley.size().x <= max_px {
        return s.to_owned();
    }
    // 估算截断点后逐字回退，直到「截断 + 省略号」适配宽度（宽字符也收敛）。
    let ratio = max_px / galley.size().x;
    let char_count = (s.chars().count() as f32 * ratio * 0.92) as usize;
    let mut result: String = s.chars().take(char_count.max(1)).collect();
    loop {
        let probe = format!("{result}…");
        let g = painter.layout_no_wrap(probe.clone(), font.clone(), egui::Color32::WHITE);
        if g.size().x <= max_px {
            return probe;
        }
        if result.pop().is_none() {
            return "…".to_string();
        }
    }
}

/// 绘制一行可点击的专辑组头（富文本粗体、整行可点用于折叠/展开）。
///
/// 与 [`selectable_row`] 不同：组头不参与选中反白，文本样式由调用方给定
/// （粗体专辑名 + 折叠箭头 + 曲数），点击响应交调用方处理折叠。
pub fn header_row(ui: &mut egui::Ui, size: egui::Vec2, text: egui::RichText) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    // 悬停淡底先于文本绘制，避免遮盖粗体字。
    let hovered = ui
        .ctx()
        .input(|input| input.pointer.hover_pos())
        .is_some_and(|pos| rect.contains(pos));
    if hovered {
        let palette = Palette::current(ui.ctx());
        ui.painter_at(rect)
            .rect_filled(rect, 2.0, palette.fg.gamma_multiply(0.08));
    }
    ui.put(rect, egui::Label::new(text).sense(egui::Sense::click()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_ids_are_caller_owned() {
        // 同一行在同一 Id 下不因文字变化而丢失交互身份。
        let a = egui::Id::new(("playlist", 3));
        let b = egui::Id::new(("playlist", 3));
        assert_eq!(a, b);
        assert_ne!(a, egui::Id::new(("playlist", 4)));
    }
}
