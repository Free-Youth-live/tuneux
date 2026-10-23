//! # 弹窗绘制
//!
//! 覆盖在主界面之上的弹窗组件：关于（draw_about，? 键唤起，任意键关闭）、
//! 均衡器（draw_equalizer）、压缩器（draw_compressor）、皮肤/语言选择器、
//! 属性、快捷键一览（菜单表生成）、书签列表、插件管理页。

use ratatui::{
    layout::{Alignment, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use unicode_width::UnicodeWidthStr;

use super::{draw_shadow, pal_style, panel_border};
use crate::tui::app::App;
use crate::tui::theme::Palette;
use tuneux_corex as audio;

/// 关于弹窗的像素字标（6 行 × 57 列，等宽已校验）。
const LOGO: [&str; 6] = [
    "████████╗ ██╗   ██╗ ███╗  ██╗ ███████╗ ██╗   ██╗ ██╗  ██╗",
    "╚══██╔══╝ ██║   ██║ ████╗ ██║ ██╔════╝ ██║   ██║ ╚██╗██╔╝",
    "   ██║    ██║   ██║ ██╔██╗██║ █████╗   ██║   ██║  ╚███╔╝ ",
    "   ██║    ██║   ██║ ██║╚████║ ██╔══╝   ██║   ██║  ██╔██╗ ",
    "   ██║    ╚██████╔╝ ██║ ╚███║ ███████╗ ╚██████╔╝ ██╔╝╚██╗",
    "   ╚═╝     ╚═════╝  ╚═╝  ╚══╝ ╚══════╝  ╚═════╝  ╚═╝  ╚═╝",
];

/// 皮肤选择器弹窗：清单 =「默认（DOS 风）+ 终端原生」+ 已加载皮肤；高亮项即时预览，
/// 当前生效项标 √；Enter 确认并持久化、Esc 取消恢复原样。
pub(super) fn draw_skin_picker(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let rows = app.skins.len() + 2; // +2 = 内置默认 / 终端原生
    let box_w = area.width.min(44);
    let box_h = area.height.min(rows as u16 + 6);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.skin")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let fg = pal_style(pal.fg, pal.bg);
    let accent = pal_style(pal.border, pal.bg).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = vec![Line::from("")];
    for i in 0..rows {
        // 行首光标：高亮行 ▶；行尾 √ 标记当前生效项。
        let cursor = if app.skin_picker_sel == i {
            "▶ "
        } else {
            "  "
        };
        let name = match i {
            0 => app.i18n.t("skin.default_dos").into_owned().to_string(),
            1 => app.i18n.t("skin.terminal_native").into_owned().to_string(),
            _ => app.skins[i - 2].name.clone(),
        };
        let cur = if app.skin_sel == i { " √" } else { "" };
        let mut style = fg;
        if app.skin_picker_sel == i {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{cursor}{name}"), style),
            Span::styled(cur, accent),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.i18n.t("skin.hint").into_owned(),
        fg.add_modifier(Modifier::DIM),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// "关于"弹窗：居中显示像素字标、品牌全称、版本、插件自检、快捷键速查、
/// 开源宣言与许可。由 `?` 键唤起，覆盖在主界面之上；任意键关闭。
///
/// 内容按可用高度分级裁剪（先砍字标，再砍速查）：小终端至少保留品牌、
/// 许可与关闭提示。
pub(super) fn draw_about(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    pal: &Palette,
    _i18n: &tuneux_commonx::I18n,
) {
    // 弹窗尺寸：宽度不超过 66 列，高度不超过 31 行（完整版内容 29 行——
    // 含字标 6 行 + 空行 1；收尾的许可/版权行在窄终端会拆成两行，即 29 行的由来）。
    let box_w = area.width.min(66);
    let box_h = area.height.min(31);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    // Clear 清空弹窗区域，避免下层界面透出来造成叠字。
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.about")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let version = env!("CARGO_PKG_VERSION");
    let h = inner.height as usize;
    let fg = pal_style(pal.fg, pal.bg);
    let dim = fg.add_modifier(Modifier::DIM);
    let accent = pal_style(pal.border, pal.bg).add_modifier(Modifier::BOLD);
    let sep = |lines: &mut Vec<Line>| {
        lines.push(Line::from(Span::styled(
            "─".repeat(inner.width as usize),
            dim,
        )))
    };

    // 插件自检标记：已加载 √，未加载 —。
    let mark = |ok: bool| if ok { "√" } else { "—" };
    let selfcheck = format!(
        "{}：{} {}  {} {}  {} {}",
        app.i18n.t("about.plugins_cat"),
        app.i18n.t("panel.eq"),
        mark(app.eq_plugin.is_some()),
        app.i18n.t("panel.compressor"),
        mark(app.comp_plugin.is_some()),
        app.i18n.t("panel.skin"),
        mark(app.skin.is_some())
    );

    let mut lines: Vec<Line> = Vec::new();
    // 第一段：像素字标（终端足够高时）。阈值 = 完整内容行数（29），
    // 保证"字标显示时其余内容必然放得下"。
    if h >= 29 {
        for l in LOGO {
            lines.push(Line::from(Span::styled(l, accent)));
        }
        lines.push(Line::from(""));
    }
    // 品牌全称与版本（必备）。
    // 注：tuneux-fx 是带连字符的产品名，按商标口径**不加** ™（只有裸名 tuneux 标）。
    lines.push(Line::from(Span::styled(
        app.i18n.t("about.brand").into_owned(),
        accent,
    )));
    lines.push(Line::from(Span::styled(
        format!("v{version} · {} · host ABI v1", app.i18n.t("about.tagline")),
        fg,
    )));
    // 第二段：运行态（自检 + 格式）。
    if h >= 12 {
        lines.push(Line::from(""));
        sep(&mut lines);
        lines.push(Line::from(Span::styled(selfcheck, fg)));
        lines.push(Line::from(Span::styled(app.i18n.t("about.formats"), fg)));
    }
    // 第三段：快捷键速查（与菜单项「关于 / 快捷键速查」名实相符）。
    if h >= 16 {
        sep(&mut lines);
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.keys1").into_owned(),
            fg,
        )));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.keys2").into_owned(),
            fg,
        )));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.keys3").into_owned(),
            fg,
        )));
    }
    // 第四段：第三方开源库（高度充足时）。
    if h >= 22 {
        sep(&mut lines);
        lines.push(Line::from(Span::styled(app.i18n.t("about.deps"), fg)));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.dep_audio").into_owned(),
            fg,
        )));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.dep_ui").into_owned(),
            fg,
        )));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.dep_common").into_owned(),
            fg,
        )));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.dep_plugin").into_owned(),
            fg,
        )));
        lines.push(Line::from(Span::styled(
            app.i18n.t("about.dep_platform").into_owned(),
            fg,
        )));
    }
    // 收尾：商标行 + 许可/版权 + 关闭提示（各段均已按 Alignment::Center 居中）。
    if h >= 12 {
        sep(&mut lines);
    }
    lines.push(Line::from(Span::styled(
        "tuneux™  不羁的青春®   FreeYouth®",
        fg,
    )));
    // 许可 + 版权：合并后 58 列，弹窗最大内宽 64 列（66 减左右边框）——
    // Paragraph 默认不换行、超宽直接截断，故按可用内宽决定合并还是拆三行。
    let license = app.i18n.t("about.license").into_owned();
    let copyright = app.i18n.t("about.copyright").into_owned();
    let combined = format!("{license}{copyright}");
    if UnicodeWidthStr::width(combined.as_str()) <= inner.width as usize {
        lines.push(Line::from(Span::styled(combined, fg)));
    } else {
        lines.push(Line::from(Span::styled(license, fg)));
        lines.push(Line::from(Span::styled(copyright, fg)));
    }
    lines.push(Line::from(Span::styled(
        app.i18n.t("about.close").into_owned(),
        dim,
    )));
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}
/// 均衡器面板：10 段增益显示、选中段反白；↑/↓ 调、←/→ 切、e 旁路、u 卸载/加载、Esc 关闭。
pub(super) fn draw_equalizer(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let box_w = area.width.min(56);
    let box_h = area.height.min(17);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.eq")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line> = Vec::new();
    match &app.eq_plugin {
        Some(plugin) => {
            let eq = &plugin.state().eq_slots[app.eq_slot as usize];
            let enabled = eq.enabled();
            lines.push(Line::from(Span::styled(
                if enabled {
                    app.i18n.t("msg.eq_enabled").into_owned()
                } else {
                    app.i18n.t("msg.eq_bypassed").into_owned()
                },
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
            )));
            for i in 0..audio::EQ_BANDS {
                let gain = eq.band(i);
                let selected = i == app.eq_band_sel;
                let text = format!("{:>5}  {}  {:+5.1}", eq_freq_label(i), eq_bar(gain), gain);
                let st = if selected {
                    pal_style(pal.bg, pal.fg)
                } else {
                    pal_style(pal.fg, pal.bg)
                };
                lines.push(Line::from(Span::styled(text, st)));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                app.i18n.t("msg.plugin_eq_missing").into_owned(),
                pal_style(pal.fg, pal.bg),
            )));
        }
    }
    lines.push(Line::from(""));
    let hint = if app.eq_plugin.is_some() {
        app.i18n.t("msg.eq_hint").into_owned()
    } else {
        app.i18n.t("msg.eq_hint_load").into_owned()
    };
    lines.push(Line::from(Span::styled(
        hint,
        pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 段频率显示：<1kHz 显示整数 Hz，≥1kHz 显示 x.xk。
fn eq_freq_label(i: usize) -> String {
    let f = audio::EQ_FREQS[i];
    if f >= 1000.0 {
        format!("{:.1}k", f / 1000.0)
    } else {
        format!("{:.0}", f)
    }
}

/// 增益条：-12..+12 dB 映射到 13 格 ASCII（中线 |，正向 =，负向 -，每格 2 dB）。
fn eq_bar(gain: f32) -> String {
    let clamped = gain.clamp(-12.0, 12.0);
    let units = (clamped / 2.0).round() as i32; // -6..=6
    let mut s = String::with_capacity(13);
    for i in -6i32..=6 {
        s.push(if i == 0 {
            '|'
        } else if i > 0 && i <= units {
            '='
        } else if i < 0 && i >= units {
            '-'
        } else {
            ' '
        });
    }
    s
}
/// 压缩器面板：5 个参数显示与选中高亮；↑/↓ 调、←/→ 切、e 旁路、u 卸载/加载、Esc 关闭。
pub(super) fn draw_compressor(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let box_w = area.width.min(44);
    let box_h = area.height.min(11);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.compressor")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line> = Vec::new();
    match &app.comp_plugin {
        Some(plugin) => {
            let c = &plugin.state().comp_slots[app.comp_slot as usize];
            let enabled = c.enabled();
            lines.push(Line::from(Span::styled(
                if enabled {
                    app.i18n.t("msg.eq_enabled").into_owned()
                } else {
                    app.i18n.t("msg.eq_bypassed").into_owned()
                },
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
            )));
            let rows: [(String, String); 5] = [
                (
                    app.i18n.t("msg.comp_threshold").into_owned(),
                    format!("{:.1} dB", c.threshold()),
                ),
                (
                    app.i18n.t("msg.comp_ratio").into_owned(),
                    format!("{:.1} :1", c.ratio()),
                ),
                (
                    app.i18n.t("msg.comp_attack").into_owned(),
                    format!("{:.1} ms", c.attack_ms()),
                ),
                (
                    app.i18n.t("msg.comp_release").into_owned(),
                    format!("{:.1} ms", c.release_ms()),
                ),
                (
                    app.i18n.t("msg.comp_makeup").into_owned(),
                    format!("{:.1} dB", c.makeup()),
                ),
            ];
            for (i, (name, val)) in rows.iter().enumerate() {
                let selected = i == app.comp_param_sel;
                let text = format!("  {name:<4} {val:>10}");
                let st = if selected {
                    pal_style(pal.bg, pal.fg)
                } else {
                    pal_style(pal.fg, pal.bg)
                };
                lines.push(Line::from(Span::styled(text, st)));
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                app.i18n.t("msg.plugin_comp_missing").into_owned(),
                pal_style(pal.fg, pal.bg),
            )));
        }
    }
    lines.push(Line::from(""));
    let hint = if app.comp_plugin.is_some() {
        app.i18n.t("msg.comp_hint").into_owned()
    } else {
        app.i18n.t("msg.eq_hint_load").into_owned()
    };
    lines.push(Line::from(Span::styled(
        hint,
        pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 语言选择器弹窗：↑/↓ 选语言、Enter 确认立即生效、Esc 取消。
pub(super) fn draw_lang_picker(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let count = app.lang_list.len();
    let box_h = (count as u16 + 4).min(area.height); // 标题+提示+条目+边框
    let box_w = 42.min(area.width);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    frame.render_widget(ratatui::widgets::Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(pal_style(pal.fg, pal.bg))
        .title(format!(" {} ", app.i18n.t("menu.lang_select")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let current_lang = &app
        .lang_list
        .get(app.lang_picker_sel)
        .map(|(id, _)| id.clone())
        .unwrap_or_default();

    let mut y = inner.y;
    for (i, (lang_id, display)) in app.lang_list.iter().enumerate() {
        if y >= inner.y + inner.height {
            break;
        }
        let cursor = if i == app.lang_picker_sel {
            "▸ "
        } else {
            "  "
        };
        let is_current = lang_id == current_lang;
        let text = format!("{cursor}{display}");
        let style = if i == app.lang_picker_sel {
            pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD)
        } else {
            pal_style(pal.fg, pal.bg)
        };
        let _ = is_current;
        frame.render_widget(
            Paragraph::new(Span::styled(text, style)),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
        y += 1;
    }
    // 底部提示
    if y < inner.y + inner.height {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!(" {} ", app.i18n.t("skin.hint")),
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM),
            )),
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
        );
    }
}

/// 按 unicode 显示宽截断（CJK 算 2 列）。
fn trunc_w(s: &str, w: usize) -> String {
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

/// 当前曲目属性弹窗（i / Alt+Enter）：标签 + 技术参数 + 路径。
pub(super) fn draw_properties(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &crate::tui::app::App,
    pal: &Palette,
) {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};
    let box_w = area.width.min(64);
    let box_h = area.height.min(15);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    frame.render_widget(ratatui::widgets::Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(ratatui::style::Style::default().fg(pal.border.unwrap_or(Color::Cyan)))
        .title(format!(" {} ", app.i18n.t("popup.properties")));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let i18n = &app.i18n;
    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    // 引擎回落（与基础版 / max 同口径）：FFmpeg 长尾曲目的元数据兜底
    // 记录缺技术字段时，以引擎实测源参数补缺。弹窗只针对当前播放曲目。
    let src_params = app.engine.as_ref().and_then(|e| e.src_params());
    let overlaid = match src_params {
        Some(p) => app
            .current_metadata
            .clone()
            .map(|md| md.overlaid_with_engine_params(Some(p))),
        None => app.current_metadata.clone(),
    };
    match &overlaid {
        None => lines.push(Line::styled(
            i18n.t("msg.empty").into_owned(),
            Style::default().fg(Color::DarkGray),
        )),
        Some(md) => {
            let unknown = |k: &str| i18n.t(k).into_owned();
            lines.push(Line::styled(
                trunc_w(
                    &md.title
                        .clone()
                        .unwrap_or_else(|| unknown("metadata.unknown_title")),
                    w,
                ),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(Span::raw(format!(
                "{}{}",
                i18n.t("metadata.artist_label"),
                trunc_w(
                    &md.artist
                        .clone()
                        .unwrap_or_else(|| unknown("metadata.unknown_artist")),
                    w
                )
            ))));
            lines.push(Line::from(Span::raw(format!(
                "{}{}",
                i18n.t("metadata.album_label"),
                trunc_w(
                    &md.album
                        .clone()
                        .unwrap_or_else(|| unknown("metadata.unknown_album")),
                    w
                )
            ))));
            let tech = format!(
                "{} {} · {} · {} · {} · {} {}",
                i18n.t("prop.codec"),
                md.codec.as_deref().unwrap_or(&i18n.t("msg.unknown_format")),
                md.bitrate_label(),
                md.sample_rate_label(),
                md.bits_label(),
                i18n.t("prop.channels"),
                md.channels.unwrap_or(0),
            );
            lines.push(Line::styled(
                trunc_w(&tech, w),
                Style::default().fg(Color::DarkGray),
            ));
            let dur = md
                .duration
                .map(super::fmt_time)
                .unwrap_or_else(|| "??:??".into());
            lines.push(Line::styled(
                format!("{}{}", i18n.t("prop.duration"), dur),
                Style::default().fg(Color::DarkGray),
            ));
            if let Some(idx) = app.playlist.current_index() {
                if let Some(it) = app.playlist.items().get(idx) {
                    lines.push(Line::styled(
                        format!(
                            "{}{}",
                            i18n.t("prop.path"),
                            trunc_w(&it.path.display().to_string(), w)
                        ),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }
        }
    }
    lines.push(Line::styled(
        i18n.t("about.close").into_owned(),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 快捷键一览弹窗：内容从菜单静态表生成（`menu::shortcut_rows`），
/// 键位定义只有菜单表一处真相源，本弹窗不手写第二份清单。任意键关闭。
pub(super) fn draw_shortcuts(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let groups = crate::tui::app::menu::shortcut_rows();
    let box_w = area.width.min(46);
    let box_h = area.height.min(30);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.shortcuts")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let fg = pal_style(pal.fg, pal.bg);
    let dim = fg.add_modifier(Modifier::DIM);
    let accent = pal_style(pal.border, pal.bg).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = Vec::new();
    for (title_key, rows) in &groups {
        lines.push(Line::from(Span::styled(
            app.i18n.t(title_key).into_owned(),
            accent,
        )));
        for (key, label_key) in rows {
            lines.push(Line::from(vec![
                Span::styled(format!(" {key:>6}  "), fg),
                Span::styled(app.i18n.t(label_key).into_owned(), fg),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.i18n.t("about.close").into_owned(),
        dim,
    )));
    // 超高内容在小终端自然截断（Paragraph 不换行、超出裁剪）。
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 书签列表弹窗：↑/↓ 选中、Enter 跳转、d 删除、Esc 关闭。
pub(super) fn draw_bookmarks(frame: &mut ratatui::Frame, area: Rect, app: &App, pal: &Palette) {
    let bms = &app.playlist_state.bookmarks.items;
    let box_w = area.width.min(56);
    let box_h = area.height.min((bms.len() as u16 + 4).max(6));
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.bookmarks")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let fg = pal_style(pal.fg, pal.bg);
    let dim = fg.add_modifier(Modifier::DIM);
    let mut lines: Vec<Line> = Vec::new();
    if bms.is_empty() {
        lines.push(Line::from(Span::styled(
            app.i18n.t("msg.bm_empty").into_owned(),
            dim,
        )));
    } else {
        let w = inner.width as usize;
        for (i, b) in bms.iter().enumerate() {
            let cursor = if i == app.bookmark_sel { "▶ " } else { "  " };
            let text = trunc_w(
                &format!("{cursor}{}: {} @ {:.0}s", i, b.label, b.position_secs),
                w,
            );
            let st = if i == app.bookmark_sel {
                fg.add_modifier(Modifier::REVERSED)
            } else {
                fg
            };
            lines.push(Line::from(Span::styled(text, st)));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.i18n.t("msg.bm_hint").into_owned(),
        dim,
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 插件管理页（只读）：plugins/ 目录扫描结果 + 类型 + 加载状态；
/// 页内 u 重载已装插件、Esc 关闭。空态显示目录路径与装载方式提示。
pub(super) fn draw_plugin_manager(
    frame: &mut ratatui::Frame,
    area: Rect,
    app: &App,
    pal: &Palette,
) {
    let box_w = area.width.min(64);
    let box_h = area.height.min((app.plugin_rows.len() as u16 + 6).max(8));
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    draw_shadow(frame, box_area);
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(format!(" {} ", app.i18n.t("panel.plugin_mgr")))
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let fg = pal_style(pal.fg, pal.bg);
    let dim = fg.add_modifier(Modifier::DIM);
    let accent = pal_style(pal.border, pal.bg);
    let w = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        trunc_w(
            &app.i18n.t("msg.pm_dir").replace("{}", &app.plugin_dir_text),
            w,
        ),
        dim,
    )));
    lines.push(Line::from(""));
    if app.plugin_rows.is_empty() {
        lines.push(Line::from(Span::styled(
            app.i18n.t("msg.pm_empty").into_owned(),
            fg,
        )));
    } else {
        for row in &app.plugin_rows {
            let state = if row.loaded {
                app.i18n.t("msg.pm_loaded").into_owned()
            } else {
                app.i18n.t("msg.pm_not_loaded").into_owned()
            };
            let state_style = if row.loaded { accent } else { dim };
            let text = trunc_w(
                &format!("{}  [{}]", row.name, app.i18n.t(row.kind_key)),
                w.saturating_sub(8),
            );
            lines.push(Line::from(vec![
                Span::styled(text, fg),
                Span::styled(format!("  {state}"), state_style),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.i18n.t("msg.pm_hint").into_owned(),
        dim,
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}
