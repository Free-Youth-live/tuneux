//! # 弹窗绘制
//!
//! 覆盖在主界面之上的弹窗组件：关于（draw_about，? 键唤起，任意键关闭）、
//! 均衡器（draw_equalizer）、压缩器（draw_compressor）。

use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::{pal_style, panel_border};
use crate::tui::app::App;
use crate::tui::theme::Palette;
use tuneux_corex as audio;

/// "关于"弹窗：居中显示版本号、简介、特性、开源声明与版权。
///
/// 由 `?` 键唤起，覆盖在主界面之上；任意键关闭（见 `App::handle_key`）。
pub(super) fn draw_about(frame: &mut ratatui::Frame, area: Rect, pal: &Palette) {
    // 弹窗尺寸：宽度不超过 66 列，高度不超过 15 行。
    let box_w = area.width.min(66);
    let box_h = area.height.min(15);
    let box_area = Rect {
        x: area.x + (area.width.saturating_sub(box_w)) / 2,
        y: area.y + (area.height.saturating_sub(box_h)) / 2,
        width: box_w,
        height: box_h,
    };
    // Clear 清空弹窗区域，避免下层界面透出来造成叠字。
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(" 关于 ")
        .style(pal_style(None, pal.bg));
    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let version = env!("CARGO_PKG_VERSION");
    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!("tuneux-fx v{version}"),
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  不羁的青春\u{00AE} FreeYouth\u{00AE}",
                pal_style(pal.border, pal.bg).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            "插件化命令行音乐播放器",
            pal_style(pal.fg, pal.bg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "支持 MP3 · FLAC · WAV · OGG · OPUS · WV · M4A · AAC · ALAC",
            pal_style(pal.fg, pal.bg),
        )),
        Line::from(Span::styled(
            "中文界面 · 纯离线 · 不收集任何数据",
            pal_style(pal.fg, pal.bg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "本项目采用木兰宽松许可证 v2（MulanPSL-2.0）",
            pal_style(pal.fg, pal.bg),
        )),
        Line::from(Span::styled(
            "基于 symphonia 等开源库构建，详见 README",
            pal_style(pal.fg, pal.bg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "\u{00A9} 不羁的青春（FreeYouth）",
            pal_style(pal.fg, pal.bg),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "按任意键关闭",
            pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
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
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(" 均衡器 ")
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
                    "已启用 · e 旁路"
                } else {
                    "已旁路 · e 恢复"
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
                "均衡器插件未加载 · 按 u 加载",
                pal_style(pal.fg, pal.bg),
            )));
        }
    }
    lines.push(Line::from(""));
    let hint = if app.eq_plugin.is_some() {
        "↑↓ ±1dB · ←→ 切段 · e 旁路 · r 恢复默认 · u 卸载 · Esc 关闭"
    } else {
        "u 加载插件 · Esc 关闭"
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
    frame.render_widget(Clear, box_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(pal.border_type)
        .border_style(panel_border(pal, true))
        .title(" 压缩器 ")
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
                    "已启用 · e 旁路"
                } else {
                    "已旁路 · e 恢复"
                },
                pal_style(pal.fg, pal.bg).add_modifier(Modifier::BOLD),
            )));
            let rows: [(&str, String); 5] = [
                ("阈值", format!("{:.1} dB", c.threshold())),
                ("压缩比", format!("{:.1} :1", c.ratio())),
                ("启动", format!("{:.1} ms", c.attack_ms())),
                ("释放", format!("{:.1} ms", c.release_ms())),
                ("补偿", format!("{:.1} dB", c.makeup())),
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
                "压缩器插件未加载 · 按 u 加载",
                pal_style(pal.fg, pal.bg),
            )));
        }
    }
    lines.push(Line::from(""));
    let hint = if app.comp_plugin.is_some() {
        "↑↓ 调 · ←→ 切 · e 旁路 · r 恢复默认 · u 卸载 · Esc 关闭"
    } else {
        "u 加载插件 · Esc 关闭"
    };
    lines.push(Line::from(Span::styled(
        hint,
        pal_style(pal.fg, pal.bg).add_modifier(Modifier::DIM),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}
