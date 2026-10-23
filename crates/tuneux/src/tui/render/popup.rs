//! # 弹窗绘制
//!
//! 职责：覆盖在主界面之上的弹窗组件：`draw_about`（"关于"弹窗，
//! `?` 唤起）与 `draw_properties`（当前曲目属性，`i` / Alt+Enter 唤起）。
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

/// 按 unicode 显示宽截断（CJK 算 2 列）：超长路径 / 标题防撑破弹窗。
fn trunc(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w.saturating_sub(1) {
            out.push('…');
            return out;
        }
        out.push(ch);
        used += cw;
    }
    out
}

/// 当前曲目属性弹窗：标签（标题/歌手/专辑/曲序）+ 技术参数（编码/码率/
/// 采样率/位深/声道/时长）+ 文件路径。`i` / Alt+Enter 唤起，任意键关闭。
pub(super) fn draw_properties(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &crate::tui::app::App,
    i18n: &tuneux_commonx::I18n,
) {
    let box_w = area.width.min(64);
    let box_h = area.height.min(15);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    frame.render_widget(ratatui::widgets::Clear, box_area);
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", i18n.t("popup.properties")),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let label = |k: &str| format!("{}: ", i18n.t(k));
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    // 引擎回落：FFmpeg 长尾曲目的文件探测只有兜底记录（技术字段全
    // None）时，用后端 open 实测的源参数补缺（元数据优先，码率不补）。
    // 弹窗只针对当前播放曲目，引擎快照即此文件参数。
    let overlaid = app
        .current_metadata
        .clone()
        .map(|md| md.overlaid_with_engine_params(app.engine.as_ref().and_then(|e| e.src_params())));
    match &overlaid {
        None => lines.push(Line::styled(
            i18n.t("msg.empty").into_owned(),
            Style::default().fg(Color::DarkGray),
        )),
        Some(md) => {
            let w = inner.width as usize;
            let unknown = |k: &str| i18n.t(k).into_owned();
            lines.push(Line::styled(
                trunc(
                    &md.title
                        .clone()
                        .unwrap_or_else(|| unknown("metadata.unknown_title")),
                    w,
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                format!(
                    "{}{}",
                    label("metadata.artist_label"),
                    trunc(
                        &md.artist
                            .clone()
                            .unwrap_or_else(|| unknown("metadata.unknown_artist")),
                        w
                    )
                ),
                Style::default(),
            ));
            lines.push(Line::styled(
                format!(
                    "{}{}",
                    label("metadata.album_label"),
                    trunc(
                        &md.album
                            .clone()
                            .unwrap_or_else(|| unknown("metadata.unknown_album")),
                        w
                    )
                ),
                Style::default(),
            ));
            let channels = format!("{}: {}", i18n.t("prop.channels"), md.channels.unwrap_or(0));
            let tech = format!(
                "{} {} · {} · {} · {} {}",
                label("prop.codec"),
                md.codec.as_deref().unwrap_or(&i18n.t("msg.unknown_format")),
                md.bitrate_label(),
                md.sample_rate_label(),
                md.bits_label(),
                channels,
            );
            lines.push(Line::styled(trunc(&tech, w), dim));
            let dur = md
                .duration
                .map(super::fmt_time)
                .unwrap_or_else(|| "??:??".to_string());
            lines.push(Line::styled(
                format!("{}{}", label("prop.duration"), dur),
                dim,
            ));
            if let Some(path) = app.current_file_path() {
                lines.push(Line::styled(
                    format!(
                        "{}{}",
                        label("prop.path"),
                        trunc(&path.display().to_string(), w)
                    ),
                    dim,
                ));
            }
        }
    }
    let hint = Line::styled(
        i18n.t("about.close").into_owned(),
        Style::default().fg(Color::DarkGray),
    );
    lines.push(hint);
    frame.render_widget(Paragraph::new(lines), inner);
}
