//! # 弹窗绘制
//!
//! 职责：覆盖在主界面之上的弹窗组件。目前只有 `draw_about`
//! （"关于"弹窗，`?` 键唤起，任意键关闭）。
//!
//! 说明：浏览器/播放列表的"/ 关键词"搜索框不是弹窗，而是内嵌在
//! 各面板顶部的 1 行输入框，随所属面板放在 panels.rs 中。

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// "关于"弹窗：居中显示版本号、简介、特性、开源声明与版权。
///
/// 由 `?` 键唤起，覆盖在主界面之上；任意键关闭（见 `App::handle_key`）。
/// 版本号取编译期的 `CARGO_PKG_VERSION`，与 Cargo.toml 保持一致，
/// 避免手工写死导致版本漂移。
pub(super) fn draw_about(frame: &mut ratatui::Frame, area: Rect, i18n: &tuneux_commonx::I18n) {
    // 弹窗尺寸：宽度不超过 66 列，高度不超过 19 行（内容恰好放得下）。
    let box_w = area.width.min(66);
    let box_h = area.height.min(19);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    // Clear 清空弹窗区域，避免下层的界面内容透出来造成叠字。
    frame.render_widget(ratatui::widgets::Clear, box_area);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("panel.about")),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let version = env!("CARGO_PKG_VERSION");
    let lines = vec![
        Line::from(vec![
            // 商标：tuneux 为**未注册**商标 → \u{2122}（™，普通文本商标符号）；
            // 用转义写法与紧随的作者标记 \u{00AE}（®）保持同族，避免文件编码差异。
            Span::styled(
                format!("tuneux\u{2122} v{version}"),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            // 商标：紧随版本号之后，青色加粗；\u{00AE} 是普通文本注册商标符号 ®
            Span::styled(
                "  不羁的青春\u{00AE} FreeYouth\u{00AE}",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(i18n.t("about.description").into_owned()),
        Line::from(""),
        Line::from(i18n.t("about.formats").into_owned()),
        Line::from(i18n.t("about.offline").into_owned()),
        Line::from(""),
        Line::from(i18n.t("about.license").into_owned()),
        Line::from(i18n.t("about.deps").into_owned()),
        Line::from(i18n.t("about.dep_audio").into_owned()),
        Line::from(i18n.t("about.dep_ui").into_owned()),
        Line::from(i18n.t("about.dep_common").into_owned()),
        Line::from(i18n.t("about.dep_plugin").into_owned()),
        Line::from(i18n.t("about.dep_platform").into_owned()),
        Line::from(""),
        // \u{00A9} 是普通文本版权符号（非 emoji 变体 \u{00A9}\u{FE0F}），
        // 与周围文字同号同宽，避免在部分终端上被渲染成大号 emoji 图标。
        Line::from("\u{00A9} 不羁的青春（FreeYouth）"),
        Line::from(""),
        Line::from(Span::styled(
            i18n.t("about.close").into_owned(),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}
