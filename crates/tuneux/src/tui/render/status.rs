//! # 信息显示组件（当前曲目 / 状态条 / 帮助栏）
//!
//! 职责：所有"文本状态信息"类组件——
//! - `draw_now_playing`：顶部条左侧的当前曲信息面板（曲名/歌手/专辑/技术参数）；
//! - `draw_status_bar`：底部状态条（播放状态/进度条/时间/音量/循环模式/错误）；
//! - `draw_progress_chars`：纯字符进度条（`━━━●━━━`），被状态条调用；
//! - `draw_help_bar`：底部帮助栏，按焦点动态显示快捷键。

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use crate::config;
use crate::playlist;
use tuneux_corex as audio;
use tuneux_mediax::metadata;

/// 当前曲信息面板。纯文字标签（无 emoji），缺失字段降级显示。
///
/// 布局为 4 行：
/// 1. 曲名（黄色加粗，最醒目）—— 行首加 `[播]`/`[停]` 状态标记；
/// 2. 歌手；
/// 3. 专辑 · 曲序（二者组合显示，节省纵向空间）；
/// 4. 技术参数（编码·码率·采样率·位深，暗色弱化）。
///
/// # 降级策略
/// 元数据字段缺失时显示"未知xxx"而非空白，让用户知道是"没有标签信息"
/// 而非"渲染出错"。这与播放列表里 title 降级为文件名的策略不同：
/// 当前曲面板应明确告知信息缺失，列表则追求可读。
///
/// # 状态标记用 `[播]`/`[停]` 而非 emoji
/// 终端对 emoji 宽度（1 或 2）处理不一致，会导致后续文字错位。
/// 中文字符宽度稳定（恒为 2），用 `[播]/[停]` 对齐可靠。
pub(super) fn draw_now_playing(
    frame: &mut ratatui::Frame,
    area: Rect,
    metadata: &Option<metadata::TrackMetadata>,
    engine: &Option<audio::Engine>,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " 当前曲目 ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 未播放（无 metadata）：居中提示
    let md = match metadata {
        Some(m) => m,
        None => {
            let hint = Paragraph::new("（未播放）")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Center);
            frame.render_widget(hint, inner);
            return;
        }
    };

    // 字段降级：标签缺失时显示"未知xxx"
    let title = md.title.clone().unwrap_or_else(|| "未知曲目".to_string());
    let artist = md.artist.clone().unwrap_or_else(|| "未知艺人".to_string());
    // 专辑与曲序组合显示：四种组合各有合理文案
    let album_line = match (&md.album, md.track_number) {
        (Some(a), Some(n)) => format!("{a} · 曲目 {n}"),
        (Some(a), None) => a.clone(),
        (None, Some(n)) => format!("曲目 {n}"),
        (None, None) => "未知专辑".to_string(),
    };
    let tech = format!(
        "{} · {} · {} · {}",
        md.codec.as_deref().unwrap_or("未知格式"),
        md.bitrate_label(),
        md.sample_rate_label(),
        md.bits_label(),
    );

    // 技术参数行颜色按直通状态：直通（bit-perfect @ 文件采样率）用亮色，
    // 降级（软件重采样）用暗色。让用户一眼看出当前是否原汁原味播放。
    // engine.bitstream() 由音频线程换曲时设置（设备支持文件采样率→true）。
    let bitstream = engine.as_ref().is_some_and(|e| e.bitstream());
    let tech_color = if bitstream {
        Color::White
    } else {
        Color::DarkGray
    };

    let lines = vec![
        Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(format!("歌手: {artist}"))),
        Line::from(Span::raw(format!("专辑: {album_line}"))),
        Line::from(Span::styled(tech, Style::default().fg(tech_color))),
    ];
    // 在第一行行首插入播放状态标记，让用户一眼看到播放/暂停
    let playing = engine.as_ref().is_some_and(|e| e.is_playing());
    let mut lines = lines;
    if let Some(first) = lines.first_mut() {
        let status = if playing { "[播] " } else { "[停] " };
        let color = if playing {
            Color::Green
        } else {
            Color::DarkGray
        };
        first
            .spans
            .insert(0, Span::styled(status, Style::default().fg(color)));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 状态条：单行紧凑展示播放状态、进度、时间、音量、循环/随机。
///
/// 区域高度 4 行（含边框）：错误行（无错时为空）+ 状态行，纵向两段。
/// 状态行横向三段（Layout 分割）：左图标 | 中进度条 | 右时间/音量/模式。
///
/// # 格式
/// `[播/停] ━━━●━━━ 0:03/3:43  80%  列表+随机`
///
/// # 进度计算
/// - `pos`：音频引擎报告的已播放秒数（基于 frames_played / sample_rate）；
/// - `dur`：曲目总时长（来自元数据，可能为 None）；
/// - `ratio = pos/dur`：进度比例，时长未知时 ratio=0（不画进度）。
///
/// # 三段布局
/// 横向三段用 Layout 分割，宽度按 unicode-width 计算；中段进度条用
/// Min(0) 兜底，窄终端下可被压成 0（不画进度条也不报错）。
// 参数较多（渲染上下文各取所需）；打包成结构体属后续清理项，先显式豁免。
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_status_bar(
    frame: &mut ratatui::Frame,
    area: Rect,
    metadata: &Option<metadata::TrackMetadata>,
    engine: &Option<audio::Engine>,
    repeat: config::RepeatMode,
    shuffle: bool,
    last_error: Option<&str>,
    cue: Option<&playlist::CueRef>,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " 状态 ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // 内部布局：错误行（无错时空）+ 状态行
    // 错误行优先显示（5s 后自动消失，所以是临时通知）
    let row_chunks = Layout::vertical([
        Constraint::Length(1), // 错误行
        Constraint::Length(1), // 状态行
    ])
    .split(inner);
    if let Some(err) = last_error {
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("⚠ {err}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            row_chunks[0],
        );
    }

    // 引擎未就绪（初始化失败）：提示并退出，不渲染正常状态
    let eng = match engine {
        Some(e) => e,
        None => {
            frame.render_widget(
                Paragraph::new("音频引擎未就绪").style(Style::default().fg(Color::Red)),
                row_chunks[1],
            );
            return;
        }
    };

    // 播放状态：[播] 绿 / [停] 暗灰，整行文字也用此色
    let playing = eng.is_playing();
    let status_icon = if playing { "[播]" } else { "[停]" };
    let status_color = if playing {
        Color::Green
    } else {
        Color::DarkGray
    };

    // 分轨内进度（CUE 分轨按区间换算）。
    let (pos, dur) = super::track_progress(eng, metadata.as_ref().and_then(|m| m.duration), cue);
    let ratio = if dur > 0.0 {
        (pos / dur).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // 时间格式化：秒 → "M:SS"
    fn fmt_time(s: f64) -> String {
        let total = s.max(0.0) as u64;
        format!("{}:{:02}", total / 60, total % 60)
    }

    // 组装单行状态文本：左 [状态] 标题  中 进度条  右 时间/音量/模式
    // 用 Layout 三段式分割，避免手工算宽度被中英文混排绕进去。
    // 中段进度条用 Min(0) 兜底——窄终端下可被压成 0，不画进度条也不报错。
    // 宽度用 unicode-width 计算（CJK 字符算 2 单元），防止 Layout 欠分配
    // 导致右段文字被截断。
    use unicode_width::UnicodeWidthStr;
    let pos_label = fmt_time(pos);
    let dur_label = if dur > 0.0 {
        fmt_time(dur)
    } else {
        "??:??".to_string()
    };
    let right_text = format!(
        "{}/{}  {:3.0}%  {}{}",
        pos_label,
        dur_label,
        eng.volume() * 100.0,
        repeat.label(),
        if shuffle { "+随机" } else { "" },
    );
    let left_text = status_icon.to_string();
    let left_w = left_text.width() as u16 + 2; // +2 留分隔空格
    let right_w = right_text.width() as u16;

    let chunks = Layout::horizontal([
        Constraint::Length(left_w),
        Constraint::Min(0),
        Constraint::Length(right_w),
    ])
    .split(row_chunks[1]); // 状态行（不是 inner 整体——只占第二行）

    frame.render_widget(
        Paragraph::new(Span::styled(left_text, Style::default().fg(status_color))),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            draw_progress_chars(ratio, chunks[1].width as usize),
            Style::default().fg(status_color),
        )),
        chunks[1],
    );
    frame.render_widget(
        Paragraph::new(Span::styled(right_text, Style::default().fg(status_color)))
            .alignment(Alignment::Right),
        chunks[2],
    );
}

/// 用纯字符画进度条：`━━━●━━━`。
///
/// - 已播放段用 `━`（粗横线）；
/// - 游标位置用 `●`（实心圆，醒目）；
/// - 未播放段用 `─`（细横线）。
///
/// 用纯字符而非 ratatui 的 Gauge 组件，是为了与终端整体风格一致
/// （Gauge 的填充块在某些终端渲染异常）。`width` 为 0 时返回空串，
/// 避免极端窄终端下除零。
fn draw_progress_chars(ratio: f64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    // 游标位置 = 比例 × 宽度，clamp 防止越界
    let pos = (ratio.clamp(0.0, 1.0) * width as f64) as usize;
    let pos = pos.min(width);
    let mut s = String::with_capacity(width);
    for i in 0..width {
        if i < pos {
            s.push('━');
        } else if i == pos {
            s.push('●');
        } else {
            s.push('─');
        }
    }
    s
}

/// 帮助栏：按焦点动态显示快捷键。
pub(super) fn draw_help_bar(frame: &mut ratatui::Frame, area: Rect, focus: playlist::Panel) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " 帮助 ",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let help = match focus {
        playlist::Panel::Browser => {
            " ↑↓ 浏览 · Enter 进入/播放 · a 加入列表 · / 搜索(Esc退) · b 关闭浏览器 · Backspace 上级 · ←→ ±5s · 空格 暂停 · +/- 音量 · v 频谱 · c 封面 · m 介质 · ? 关于 · q 退出 "
        }
        playlist::Panel::Playlist => {
            " ↑↓ 选曲 · Enter 播放/折叠 · g 分组 · a 加入 · d 删除 · x 清空 · r 循环 · s 随机 · n/p 上下首 · ←→ ±5s · / 搜索(Esc退) · b 浏览器 · 空格 暂停 · v 频谱 · l 歌词 · c 封面 · m 介质 · ? 关于 · q 退出 "
        }
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        inner,
    );
}
