//! # 弹窗绘制
//!
//! 覆盖在主界面之上的弹窗组件：关于（draw_about，? 键唤起，任意键关闭）、
//! 均衡器（draw_equalizer）、压缩器（draw_compressor）。

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
