//! # 终端配色（基础版主题）
//!
//! 基础版不接皮肤插件，配色走内置主题：`terminal`（终端原生，默认）、
//! `phosphor`（磷光绿）、`classic`（经典青框）、`deepblue`（深蓝）、
//! `navy`（藏青）、`blackwhite`（黑白）、`graywhite`（灰白）、
//! `dimcolor`（暗彩）。启动按配置选定，
//! 运行中可按 `t` 循环切换（即时生效并写回配置）。除语义阈值（直通 / 降级 /
//! 错误 / 电平档位）外，界面颜色一律从调色板字段读取。
//!
//! 状态含义不受主题影响（跨产品约定）：直通 / 降级 / 错误及电平档位的语义
//! 不变；具体颜色可按主题与可读性调整。

use ratatui::style::Color;
use std::cell::Cell;

/// 一套终端调色板（各界面角色的颜色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 屏幕底色；`None` = 沿用终端默认底色（经典 / 终端原生等透底主题）。
    pub bg: Option<Color>,
    /// 正文颜色；`None` = 沿用终端默认字色（经典 / 终端原生等透底主题）。
    pub fg: Option<Color>,
    /// 边框 / 面板标题 / 弹窗框线（界面骨架色）。
    pub chrome: Color,
    /// 次要文字：提示、空态、帮助栏、网格。
    pub dim: Color,
    /// 正常 / 播放态（状态文本 `[播]`、电平低档、能量桶）。
    pub ok: Color,
    /// 信息提示（目录名等）。
    pub info: Color,
    /// 选中项反白底色（默认配黑字；反白主题经 `sel_fg` 覆写文字色）。
    pub sel_bg: Color,
    /// 频谱柱体。
    pub bar: Color,
    /// 获得焦点的面板标题色（经典 = 黄；磷光绿 = 与其它标题同色；
    /// 浅色主题按底色取深色）。
    pub title_focus: Color,
    /// 帮助栏里的快捷键键名色（磷光绿 = 白；经典 / 浅色主题沿用可读色）。
    pub key_fg: Color,
    /// 曲名标题色覆写；`None` = 固定黄（跨产品「曲名黄色最醒目」约定）。
    /// 浅色主题（黑白 / 灰白 / 暗彩）覆写为深色，避免黄字压浅底低对比。
    pub title_fg: Option<Color>,
    /// 直通技术参数行色覆写；`None` = 固定白（深底亮色约定）。
    /// 浅色主题覆写为深色保可读；降级（重采样）行仍用 [`Palette::dim`]。
    pub pass_fg: Option<Color>,
    /// 选中项反白文字色覆写；`None` = 黑字（配浅色 sel_bg 的既有约定）。
    /// 「深底浅字」反白主题（黑白 / 灰白 / 暗彩）覆写为浅色。
    pub sel_fg: Option<Color>,
    /// 频谱峰值帽颜色覆写；`None` = 白（深底「白帽」约定）。浅色主题
    /// 白帽压浅底会隐身，覆写为可见且与柱身区分的颜色。
    pub peak_fg: Option<Color>,
    /// 频谱柱体用实心色块而非风格字符池；默认 false。
    /// 实现为「背景色填充空格」（每柱 2 格 + 1 格缝隙），纵向无字符间隙；
    /// 黑白单色主题启用，频谱观感与频段一致。
    pub block_spectrum: bool,
    /// 单色渲染：封面按亮度映射到主题明暗、电平表去阈值彩色用柱身色。
    /// 黑白 / 灰白等单色液晶主题启用。
    pub monochrome: bool,
    /// 反射式 TFT 渲染：封面黑位抬高、亮部压回中间调并带灰绿色调，电平阈值
    /// 彩色压暗。暗彩主题启用，模拟无背光屏的柔和低对比观感。
    pub dim_screen: bool,
}

/// 经典青框：与改造前的观感逐项一致（青框、灰字、深灰次要、浅青反白、
/// 蓝目录、亮绿频谱柱）。
pub const CLASSIC: Palette = Palette {
    bg: None,
    fg: None,
    chrome: Color::Cyan,
    dim: Color::DarkGray,
    ok: Color::Green,
    info: Color::Blue,
    sel_bg: Color::LightCyan,
    bar: Color::LightGreen,
    title_focus: Color::Yellow,
    key_fg: Color::DarkGray,
    title_fg: None,
    pass_fg: None,
    sel_fg: None,
    peak_fg: None,
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};

/// 磷光绿：黑底绿字、亮绿框线、次级绿做次要文字、反白选中——早期绿屏
/// 终端的观感（单色绿 + 反色区块做层级）。
///
/// 两级绿的取法（ratatui 无「深绿」变体）：正文/框线用 `LightGreen`
///（#55FF55 档），次要文字用 `Green`（#00AA00 档）——均为 16 色基础色，
/// 不依赖真彩终端。
///
/// 底色取真彩 `Rgb(0,0,0)`：主流终端主题几乎都把 ANSI 0 号黑重定义
/// 为深灰（Tango #2E3436 / Gruvbox #282828 等），铺出来的"黑底"会呈
/// 深灰，破坏绿屏参照物。**但 24 位色转义序列在不支持真彩的终端上
/// 典型行为是整条被忽略（背景干脆不铺）**，故 [`init`] / [`pal`] 会
/// 按 `COLORTERM` 自适应降级（见 [`adapt_for_terminal`]）。
pub const PHOSPHOR: Palette = Palette {
    bg: Some(Color::Rgb(0, 0, 0)),
    fg: Some(Color::LightGreen),
    chrome: Color::LightGreen,
    dim: Color::Green,
    ok: Color::LightGreen,
    info: Color::Green,
    sel_bg: Color::LightGreen,
    bar: Color::LightGreen,
    title_focus: Color::LightGreen,
    key_fg: Color::White,
    title_fg: None,
    pass_fg: None,
    sel_fg: None,
    peak_fg: None,
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};

/// 深蓝：经典 DOS / VGA 文本模式蓝底（`#0000AA`）+ 浅灰字、亮青边框、
/// 黄高亮——Norton / DOS edit 那股复古蓝。非真彩终端降级 ANSI 蓝。
pub const DEEPBLUE: Palette = Palette {
    bg: Some(Color::Rgb(0, 0, 170)),
    fg: Some(Color::Rgb(170, 170, 170)),
    chrome: Color::Rgb(85, 255, 255),
    dim: Color::Rgb(153, 153, 153),
    ok: Color::Rgb(85, 255, 85),
    info: Color::Rgb(85, 255, 255),
    sel_bg: Color::Rgb(0, 170, 170),
    bar: Color::Rgb(85, 255, 255),
    title_focus: Color::Rgb(255, 255, 85),
    key_fg: Color::Rgb(255, 255, 85),
    title_fg: None,
    pass_fg: None,
    sel_fg: None,
    peak_fg: None,
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};

/// 藏青：与 fx 内置回退配色同源，蓝黑底 `#10163A` + 灰字 + 青框 + 黄键号。
/// 选中项按 fx 反白效果使用灰底和蓝黑字。
pub const NAVY: Palette = Palette {
    bg: Some(Color::Rgb(16, 22, 58)),
    fg: Some(Color::Gray),
    chrome: Color::Cyan,
    dim: Color::DarkGray,
    ok: Color::Green,
    info: Color::Blue,
    sel_bg: Color::Gray,
    bar: Color::LightGreen,
    title_focus: Color::Yellow,
    key_fg: Color::Yellow,
    title_fg: None,
    pass_fg: None,
    sel_fg: Some(Color::Rgb(16, 22, 58)),
    peak_fg: None,
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};

/// 终端原生：不铺底色 / 字色，其余角色用 `Color::Reset` 跟随终端自身
/// 配色（给想用自己终端主题的用户留的退路，与 fx 的「终端原生」同义）。
/// 选中反白底例外取白色——选中行恒配黑字，须保证任意终端底下可读。
pub const TERMINAL: Palette = Palette {
    bg: None,
    fg: None,
    chrome: Color::Reset,
    dim: Color::Reset,
    ok: Color::Reset,
    info: Color::Reset,
    sel_bg: Color::White,
    bar: Color::Reset,
    title_focus: Color::Reset,
    key_fg: Color::Reset,
    // 浅色终端下固定黄/白会失去对比：显式 Reset 跟随终端默认字色。
    title_fg: Some(Color::Reset),
    pass_fg: Some(Color::Reset),
    sel_fg: None,
    peak_fg: Some(Color::Reset),
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};

/// 黑白单色：纯白底、纯黑字 / 框线 / 频谱柱，次要文字用 50% 灰；
/// 选中为「黑底白字」反白（sel_fg 覆写）。曲名 / 直通参数覆写黑色保浅底可读。
pub const BLACKWHITE: Palette = Palette {
    bg: Some(Color::Rgb(255, 255, 255)),
    fg: Some(Color::Rgb(0, 0, 0)),
    chrome: Color::Rgb(0, 0, 0),
    dim: Color::Rgb(128, 128, 128),
    ok: Color::Rgb(0, 0, 0),
    info: Color::Rgb(0, 0, 0),
    sel_bg: Color::Rgb(0, 0, 0),
    bar: Color::Rgb(0, 0, 0),
    title_focus: Color::Rgb(0, 0, 0),
    key_fg: Color::Rgb(0, 0, 0),
    title_fg: Some(Color::Rgb(0, 0, 0)),
    pass_fg: Some(Color::Rgb(0, 0, 0)),
    sel_fg: Some(Color::Rgb(255, 255, 255)),
    peak_fg: Some(Color::Rgb(128, 128, 128)),
    block_spectrum: true,
    monochrome: true,
    dim_screen: false,
};

/// 灰白：仿 FSTN 反射式液晶（掌机改良屏）——中性灰白浅底 + 近黑深灰字，
/// 绿调已补偿掉、以灰为主。单色渲染（封面按亮度映射、电平去彩色），
/// 频谱用经典字符池（非实心块），选中「深底浅字」反白。
pub const GRAYWHITE: Palette = Palette {
    bg: Some(Color::Rgb(200, 205, 197)),
    fg: Some(Color::Rgb(27, 27, 25)),
    chrome: Color::Rgb(55, 55, 53),
    dim: Color::Rgb(130, 130, 126),
    ok: Color::Rgb(27, 27, 25),
    info: Color::Rgb(55, 55, 53),
    sel_bg: Color::Rgb(45, 45, 43),
    bar: Color::Rgb(27, 27, 25),
    title_focus: Color::Rgb(27, 27, 25),
    key_fg: Color::Rgb(27, 27, 25),
    title_fg: Some(Color::Rgb(27, 27, 25)),
    pass_fg: Some(Color::Rgb(92, 92, 88)),
    sel_fg: Some(Color::Rgb(217, 217, 213)),
    peak_fg: Some(Color::Rgb(130, 130, 126)),
    block_spectrum: false,
    monochrome: true,
    dim_screen: false,
};

/// 暗彩：仿无背光反射式 TFT 彩屏——灰绿浅底、黑位抬高、低对比、低饱和。
/// 青绿、灰蓝、琥珀与灰紫作为功能色，增加色相但不提高鲜艳度；曲名 / 直通 /
/// 峰值帽使用与浅底相衬的柔和深色，选中项用深底浅字。
pub const DIMCOLOR: Palette = Palette {
    bg: Some(Color::Rgb(156, 164, 148)),
    fg: Some(Color::Rgb(45, 53, 45)),
    chrome: Color::Rgb(77, 87, 78),
    dim: Color::Rgb(102, 110, 95),
    ok: Color::Rgb(72, 115, 78),
    info: Color::Rgb(76, 101, 130),
    sel_bg: Color::Rgb(72, 84, 68),
    bar: Color::Rgb(65, 112, 108),
    title_focus: Color::Rgb(133, 101, 55),
    key_fg: Color::Rgb(65, 72, 62),
    title_fg: Some(Color::Rgb(126, 85, 63)),
    pass_fg: Some(Color::Rgb(73, 87, 69)),
    sel_fg: Some(Color::Rgb(210, 218, 196)),
    peak_fg: Some(Color::Rgb(117, 91, 112)),
    block_spectrum: false,
    monochrome: false,
    dim_screen: true,
};

const PHOSPHOR_ANSI: Palette = Palette {
    bg: Some(Color::Black),
    ..PHOSPHOR
};
const DEEPBLUE_ANSI: Palette = Palette {
    bg: Some(Color::Blue),
    fg: Some(Color::Gray),
    chrome: Color::LightCyan,
    dim: Color::DarkGray,
    ok: Color::LightGreen,
    info: Color::LightCyan,
    sel_bg: Color::Cyan,
    bar: Color::LightCyan,
    title_focus: Color::Yellow,
    key_fg: Color::Yellow,
    title_fg: None,
    pass_fg: None,
    sel_fg: None,
    peak_fg: None,
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};
const NAVY_ANSI: Palette = Palette {
    bg: Some(Color::Blue),
    sel_fg: Some(Color::Black),
    ..NAVY
};
const BLACKWHITE_ANSI: Palette = Palette {
    bg: Some(Color::White),
    fg: Some(Color::Black),
    chrome: Color::Black,
    dim: Color::DarkGray,
    ok: Color::Black,
    info: Color::Black,
    sel_bg: Color::Black,
    bar: Color::Black,
    title_focus: Color::Black,
    key_fg: Color::Black,
    title_fg: Some(Color::Black),
    pass_fg: Some(Color::Black),
    sel_fg: Some(Color::White),
    peak_fg: Some(Color::DarkGray),
    block_spectrum: true,
    monochrome: true,
    dim_screen: false,
};
const GRAYWHITE_ANSI: Palette = Palette {
    bg: Some(Color::White),
    fg: Some(Color::Black),
    chrome: Color::Black,
    dim: Color::DarkGray,
    ok: Color::Black,
    info: Color::Black,
    sel_bg: Color::Black,
    bar: Color::Black,
    title_focus: Color::Black,
    key_fg: Color::Black,
    title_fg: Some(Color::Black),
    pass_fg: Some(Color::DarkGray),
    sel_fg: Some(Color::White),
    peak_fg: Some(Color::DarkGray),
    block_spectrum: false,
    monochrome: true,
    dim_screen: false,
};
const DIMCOLOR_ANSI: Palette = Palette {
    bg: Some(Color::Gray),
    fg: Some(Color::Black),
    chrome: Color::DarkGray,
    dim: Color::DarkGray,
    ok: Color::Green,
    info: Color::Blue,
    sel_bg: Color::Black,
    bar: Color::Cyan,
    title_focus: Color::DarkGray,
    key_fg: Color::Black,
    title_fg: Some(Color::Black),
    pass_fg: Some(Color::DarkGray),
    sel_fg: Some(Color::White),
    peak_fg: Some(Color::DarkGray),
    block_spectrum: false,
    monochrome: false,
    dim_screen: false,
};

struct ThemeSpec {
    id: &'static str,
    truecolor: Palette,
    ansi: Palette,
}

/// 内置主题表：id 顺序、真彩与 16 色调色板共用同一事实源。
const THEMES: &[ThemeSpec] = &[
    ThemeSpec {
        id: "terminal",
        truecolor: TERMINAL,
        ansi: TERMINAL,
    },
    ThemeSpec {
        id: "phosphor",
        truecolor: PHOSPHOR,
        ansi: PHOSPHOR_ANSI,
    },
    ThemeSpec {
        id: "classic",
        truecolor: CLASSIC,
        ansi: CLASSIC,
    },
    ThemeSpec {
        id: "deepblue",
        truecolor: DEEPBLUE,
        ansi: DEEPBLUE_ANSI,
    },
    ThemeSpec {
        id: "navy",
        truecolor: NAVY,
        ansi: NAVY_ANSI,
    },
    ThemeSpec {
        id: "blackwhite",
        truecolor: BLACKWHITE,
        ansi: BLACKWHITE_ANSI,
    },
    ThemeSpec {
        id: "graywhite",
        truecolor: GRAYWHITE,
        ansi: GRAYWHITE_ANSI,
    },
    ThemeSpec {
        id: "dimcolor",
        truecolor: DIMCOLOR,
        ansi: DIMCOLOR_ANSI,
    },
];

thread_local! {
    static CURRENT: Cell<Option<Palette>> = const { Cell::new(None) };
}

/// 终端真彩能力判定（纯函数，可单测）：返回是否按 24 位色输出。
///
/// 探测信号（任一成立即真彩）：
/// - `TUNEUX_COLOR_DEPTH` 显式覆盖：`truecolor`/`24bit`/`1` → 是，
///   `16`/`256`/`0` → 否（给不支持真彩的终端留强制降级开关）；
/// - `WT_SESSION` 存在（Windows Terminal 不设 COLORTERM，但支持真彩）；
/// - `TERM_PROGRAM` 属于常见真彩终端（iTerm2 / WezTerm / VS Code / Hyper 等）；
/// - `ConEmuANSI=ON`；
/// - `TERM` 含 `direct` / `truecolor`；
/// - `COLORTERM` 含 `truecolor` / `24bit`；
/// - 以上都没有时：**Windows 平台默认按真彩**（现代 Windows Terminal / conhost
///   均支持 24 位 SGR；旧终端忽略该序列只会退回终端默认色，比整表错位降级更轻），
///   非 Windows 默认 16 色（macOS Terminal.app 至今不支持真彩且不宣告）。
#[cfg(test)]
fn truecolor_from_signals(
    depth_override: Option<&str>,
    wt_session: bool,
    term_program: Option<&str>,
    conemu_ansi: Option<&str>,
    term: Option<&str>,
    colorterm: Option<&str>,
    is_windows: bool,
) -> bool {
    tuneux_displayx::truecolor_from_signals(
        depth_override,
        wt_session,
        term_program,
        conemu_ansi,
        term,
        colorterm,
        is_windows,
    )
}

/// 按终端真彩能力自适应调色板（纯函数，可单测）：
///
/// 真彩底色 `Rgb(0,0,0)` 在不支持 24 位色的终端上会被整条忽略（背景
/// 不铺），故终端能力探测未确认真彩时降级为 ANSI 黑——至少保证铺底；
/// 被终端主题重定义为深灰也只是色彩偏差，不会失去背景。
fn adapt_for_terminal(p: Palette, truecolor: bool) -> Palette {
    if !truecolor && p.bg == Some(Color::Rgb(0, 0, 0)) {
        Palette {
            bg: Some(Color::Black),
            ..p
        }
    } else {
        p
    }
}

/// 读环境变量并自适应（`init` / `pal` 共用；一次会话内终端能力不变）。
fn adapt_to_env(p: Palette) -> Palette {
    adapt_for_terminal(p, env_truecolor())
}

/// 读环境变量判定真彩能力（Windows 默认按真彩）（一次会话内能力不变）。
fn env_truecolor() -> bool {
    tuneux_displayx::env_truecolor()
}

/// 错误提示色：深色主题用常规红；浅色 / 反射屏主题（黑白、灰白、暗彩）
/// 用压暗红保证浅底可读（常规亮红压白/灰绿底对比度不足）。
pub fn error_fg() -> Color {
    let p = pal();
    let light_bg = match p.bg {
        Some(Color::White) => true,
        Some(Color::Rgb(r, g, b)) => {
            (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) > 140.0
        }
        _ => false,
    };
    let color = if light_bg {
        Color::Rgb(150, 30, 30)
    } else {
        Color::Red
    };
    tuneux_displayx::color_for_terminal(color, env_truecolor())
}
fn theme_spec(id: &str) -> &'static ThemeSpec {
    let id = id.trim().to_ascii_lowercase();
    THEMES
        .iter()
        .find(|theme| theme.id == id)
        .unwrap_or(&THEMES[0])
}

/// 规范化主题 id（去首尾空白、大小写不敏感）：未知 / 空串 → 终端原生默认。
pub fn normalize(id: &str) -> &'static str {
    theme_spec(id).id
}

/// 按配置值选择主题（纯函数，可单测），返回**真彩**调色板；
/// 未知 / 空串 → 终端原生默认。非真彩终端的降级见 `ansi_fallback`。
pub fn select(theme: &str) -> Palette {
    theme_spec(theme).truecolor
}

/// 循环切换的下一个主题 id（纯函数，可单测）：按双端共享顺序表，
/// 末位回绕到首位；未知当前值先规范化为终端原生。
pub fn next_theme_id(current: &str) -> &'static str {
    let current = normalize(current);
    let order = tuneux_displayx::THEME_ORDER;
    let index = order
        .iter()
        .position(|theme| theme.id == current)
        .unwrap_or(0);
    order[(index + 1) % order.len()].id
}

/// 非真彩终端的降级调色板，与真彩主题共用主题表。
fn ansi_fallback(id: &str) -> Palette {
    theme_spec(id).ansi
}

/// 启动时按配置初始化当前主题（等价 [`set_theme`]；运行中可再次调用切换）。
pub fn init(theme: &str) {
    set_theme(theme);
}

/// 运行时设置当前 UI 线程主题（即时生效）：按终端能力取真彩或降级
/// 调色板写入线程本地快照，渲染下一帧即读到新色。
pub fn set_theme(id: &str) {
    let norm = normalize(id);
    let p = if env_truecolor() {
        select(norm)
    } else {
        ansi_fallback(norm)
    };
    CURRENT.set(Some(p));
}

/// 当前 UI 线程主题（未初始化时 = 终端原生按环境自适应），读取无需加锁。
pub fn pal() -> Palette {
    CURRENT.get().unwrap_or_else(|| adapt_to_env(TERMINAL))
}

/// 封面像素取色：单色主题按亮度映射到主题明暗，反射屏主题抬黑位并压低
/// 对比度；实际输出按终端能力保留 RGB 或量化为 ANSI 16 色。
pub fn cover_pixel(r: u8, g: u8, b: u8, a: u8) -> Color {
    cover_pixel_with_capability(&pal(), r, g, b, a, env_truecolor())
}

/// 真彩纯函数内核：显式传入调色板，便于单测且不依赖全局环境。
#[cfg(test)]
fn cover_pixel_impl(p: &Palette, r: u8, g: u8, b: u8, a: u8) -> Color {
    cover_pixel_with_capability(p, r, g, b, a, true)
}

fn cover_pixel_with_capability(
    p: &Palette,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    is_truecolor: bool,
) -> Color {
    tuneux_displayx::cover_pixel(
        &tuneux_displayx::CoverStyle {
            bg: p.bg,
            fg: p.fg,
            is_monochrome: p.monochrome,
            is_dim_screen: p.dim_screen,
            is_truecolor,
        },
        r,
        g,
        b,
        a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn classic_keeps_legacy_colors() {
        let p = select("classic");
        assert_eq!(p.bg, None);
        assert_eq!(p.fg, None);
        assert_eq!(p.chrome, Color::Cyan);
        assert_eq!(p.dim, Color::DarkGray);
        assert_eq!(p.ok, Color::Green);
        assert_eq!(p.info, Color::Blue);
        assert_eq!(p.sel_bg, Color::LightCyan);
        assert_eq!(p.bar, Color::LightGreen);
        assert_eq!(p.title_focus, Color::Yellow);
        assert_eq!(p.key_fg, Color::DarkGray);
    }

    #[test]
    fn phosphor_is_black_and_green() {
        let p = select("phosphor");
        // 底色必须真彩纯黑：ANSI 0 号黑会被终端主题重定义为深灰
        //（回归：铺底后观感"不是纯黑"）。
        assert_eq!(p.bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(p.fg, Some(Color::LightGreen));
        assert_eq!(p.chrome, Color::LightGreen);
        assert_eq!(p.dim, Color::Green);
        assert_eq!(p.title_focus, Color::LightGreen, "聚焦标题与其它标题同色");
        assert_eq!(p.key_fg, Color::White, "帮助栏键名白色");
    }

    #[test]
    fn select_is_case_insensitive_and_falls_back() {
        assert_eq!(select("CLASSIC"), CLASSIC);
        assert_eq!(select("Classic"), CLASSIC);
        assert_eq!(select("  Classic\n"), CLASSIC, "配置值忽略首尾空白");
        assert_eq!(select(""), TERMINAL);
        assert_eq!(select("unknown"), TERMINAL);
    }

    #[test]
    fn every_theme_in_table_has_localized_name() {
        let zh = crate::config::zh_table();
        for theme in THEMES {
            let key = format!("theme.{}", theme.id);
            assert!(zh.get(&key).is_some(), "中文表缺少 {key}");
        }
    }

    /// 真彩信号判定（纯函数）：显式覆盖 > 各终端信号 > 平台默认。
    #[test]
    fn truecolor_signals() {
        // COLORTERM 经典信号（大小写不敏感）。
        assert!(truecolor_from_signals(
            None,
            false,
            None,
            None,
            None,
            Some("truecolor"),
            false
        ));
        assert!(truecolor_from_signals(
            None,
            false,
            None,
            None,
            None,
            Some("24BIT"),
            false
        ));
        assert!(!truecolor_from_signals(
            None,
            false,
            None,
            None,
            None,
            Some("256"),
            false
        ));
        // Windows Terminal 不设 COLORTERM，靠 WT_SESSION 判定。
        assert!(truecolor_from_signals(
            None, true, None, None, None, None, false
        ));
        // TERM_PROGRAM / ConEmu / TERM 信号。
        assert!(truecolor_from_signals(
            None,
            false,
            Some("iTerm.app"),
            None,
            None,
            None,
            false
        ));
        assert!(truecolor_from_signals(
            None,
            false,
            Some("WezTerm"),
            None,
            None,
            None,
            false
        ));
        assert!(truecolor_from_signals(
            None,
            false,
            None,
            Some("ON"),
            None,
            None,
            false
        ));
        assert!(truecolor_from_signals(
            None,
            false,
            None,
            None,
            Some("xterm-direct"),
            None,
            false
        ));
        // 无信号：Windows 默认真彩（现代终端支持 24 位 SGR），其他平台降级。
        assert!(truecolor_from_signals(
            None, false, None, None, None, None, true
        ));
        assert!(!truecolor_from_signals(
            None, false, None, None, None, None, false
        ));
        // 显式覆盖优先于一切信号（含 WT_SESSION / COLORTERM）。
        assert!(!truecolor_from_signals(
            Some("16"),
            true,
            Some("iTerm.app"),
            Some("ON"),
            Some("xterm-direct"),
            Some("truecolor"),
            true
        ));
        assert!(truecolor_from_signals(
            Some("truecolor"),
            false,
            None,
            None,
            None,
            None,
            false
        ));
    }

    /// 底色自适应：真彩终端保持 `Rgb(0,0,0)` 精确纯黑；未宣告真彩时
    /// 降级 ANSI 黑——保证铺底（被主题重定义也只是深灰，不会失去背景）；
    /// 经典主题透底（bg=None）不受影响。
    ///
    /// 回归：旧论证称"非真彩终端按最近色回落 0 号"，实测多数终端对
    /// 不认识的 24 位色 SGR 是整条忽略（背景干脆不铺，macOS Terminal.app
    /// 即此类）——降级分支是防止"比修前更差"的底线。
    #[test]
    fn phosphor_bg_adapts_to_truecolor_capability() {
        let full = adapt_for_terminal(PHOSPHOR, true);
        assert_eq!(full.bg, Some(Color::Rgb(0, 0, 0)), "真彩终端用精确纯黑");
        let downgraded = adapt_for_terminal(PHOSPHOR, false);
        assert_eq!(
            downgraded.bg,
            Some(Color::Black),
            "非真彩终端退 ANSI 黑保铺底"
        );
        assert_eq!(downgraded.fg, PHOSPHOR.fg, "其余角色不变");
        // 经典主题透底，两种能力下都不铺。
        assert_eq!(adapt_for_terminal(CLASSIC, false).bg, None);
        assert_eq!(adapt_for_terminal(CLASSIC, true).bg, None);
    }

    #[test]
    fn pal_returns_a_valid_palette() {
        // pal() 必须返回主题表中某个主题的真彩或降级框线色。
        let p = pal();
        let valid = THEMES
            .iter()
            .any(|theme| p.chrome == theme.truecolor.chrome || p.chrome == theme.ansi.chrome);
        assert!(valid, "pal() 框线色须属于某个注册主题");
    }

    #[test]
    fn theme_ids_cycle_in_order_and_wrap() {
        assert_eq!(next_theme_id("terminal"), "phosphor");
        assert_eq!(next_theme_id("phosphor"), "classic");
        assert_eq!(next_theme_id("classic"), "deepblue");
        assert_eq!(next_theme_id("deepblue"), "navy");
        assert_eq!(next_theme_id("navy"), "blackwhite");
        assert_eq!(next_theme_id("blackwhite"), "graywhite");
        assert_eq!(next_theme_id("graywhite"), "dimcolor");
        assert_eq!(next_theme_id("dimcolor"), "terminal", "末位回绕");
        assert_eq!(
            next_theme_id("unknown"),
            "phosphor",
            "未知规范化为终端原生再进一位"
        );
        assert_eq!(next_theme_id("DEEPBLUE"), "navy", "大小写不敏感");
    }

    #[test]
    fn select_maps_all_themes_case_insensitive() {
        assert_eq!(select("deepblue"), DEEPBLUE);
        assert_eq!(select("NAVY"), NAVY);
        assert_eq!(select("terminal"), TERMINAL);
        assert_eq!(NAVY.bg, Some(Color::Rgb(16, 22, 58)));
        assert_eq!(NAVY.chrome, Color::Cyan);
        assert_eq!(NAVY.key_fg, Color::Yellow);
        assert_eq!(ansi_fallback("navy").bg, Some(Color::Blue));
    }

    #[test]
    fn new_themes_visibly_differ_from_phosphor() {
        assert_ne!(select("deepblue").chrome, select("phosphor").chrome);
        assert_ne!(select("blackwhite").bg, select("phosphor").bg);
    }

    #[test]
    fn ansi_fallback_keeps_visible_bg() {
        assert_eq!(ansi_fallback("deepblue").bg, Some(Color::Blue));
        assert_eq!(ansi_fallback("blackwhite").bg, Some(Color::White));
        // 暗彩降级：16 色里用 Gray 近似灰绿底（旧版误用 White，整体观感差得远）。
        assert_eq!(ansi_fallback("dimcolor").bg, Some(Color::Gray));
        assert!(
            !ansi_fallback("dimcolor").dim_screen,
            "降级后关闭反射屏标志"
        );
        assert_eq!(ansi_fallback("terminal").bg, None);
        assert_eq!(ansi_fallback("phosphor").bg, Some(Color::Black));
    }

    /// 非真彩降级调色板用 16 色具名色；单色封面映射必须辨认它们，否则
    /// 黑底/白底都被当成 (0,0,0)，整幅封面全黑（非真彩终端回归）。
    #[test]
    fn mono_cover_maps_named_ansi_colors() {
        let bw = ansi_fallback("blackwhite");
        assert!(bw.monochrome, "降级黑白仍应为单色");
        assert_eq!(
            cover_pixel_impl(&bw, 0, 0, 0, 255),
            Color::Rgb(0, 0, 0),
            "黑像素映射为暗端"
        );
        assert_eq!(
            cover_pixel_impl(&bw, 255, 255, 255, 255),
            Color::Rgb(255, 255, 255),
            "白像素映射为亮端"
        );
        match cover_pixel_impl(&bw, 128, 128, 128, 255) {
            Color::Rgb(r, g, b) => {
                assert!(r > 20 && r < 235, "中灰应落在明暗之间：{r}");
                assert_eq!((r, g, b), (r, r, r), "降级单色仍应等灰");
            }
            other => panic!("单色映射应为 Rgb：{other:?}"),
        }
    }

    #[test]
    fn cover_pixel_mono_maps_luminance_and_transparent() {
        // 纯内核 + 显式调色板，不依赖全局态（避免并行测试干扰）。
        // 非单色（磷光绿）：原样 RGB，透明→黑。
        assert_eq!(
            cover_pixel_impl(&PHOSPHOR, 10, 20, 30, 255),
            Color::Rgb(10, 20, 30)
        );
        assert_eq!(cover_pixel_impl(&PHOSPHOR, 10, 20, 30, 0), Color::Black);
        // 单色（黑白）：黑像素→fg、白像素→bg、透明→bg，中灰居中。
        assert_eq!(
            cover_pixel_impl(&BLACKWHITE, 0, 0, 0, 255),
            Color::Rgb(0, 0, 0)
        );
        assert_eq!(
            cover_pixel_impl(&BLACKWHITE, 255, 255, 255, 255),
            Color::Rgb(255, 255, 255)
        );
        assert_eq!(
            cover_pixel_impl(&BLACKWHITE, 0, 0, 0, 0),
            Color::Rgb(255, 255, 255)
        );
        match cover_pixel_impl(&BLACKWHITE, 128, 128, 128, 255) {
            Color::Rgb(r, g, b) => {
                assert!(r > 0 && r < 255, "中灰应介于明暗之间");
                assert_eq!(r, g);
                assert_eq!(g, b);
            }
            _ => panic!("单色映射应为 Rgb"),
        }
    }

    #[test]
    fn cover_pixel_dim_screen_lifts_black_and_mutes_colors() {
        // 反射式 TFT：黑位抬高、亮部压回中间调，灰绿色调且不丢失色相。
        match cover_pixel_impl(&DIMCOLOR, 0, 0, 0, 255) {
            Color::Rgb(r, g, b) => {
                assert!(r > 40 && g > 40 && b > 40, "黑位应抬高为可见灰阶");
                assert!(g >= r && g >= b, "黑位应带灰绿调");
            }
            _ => panic!("反射屏封面应为 Rgb"),
        }
        let white = cover_pixel_impl(&DIMCOLOR, 255, 255, 255, 255);
        match white {
            Color::Rgb(r, g, b) => {
                assert!(r < 230 && g < 230 && b < 230, "亮部应压回中间调：{white:?}");
                assert!(g >= r && g >= b, "亮部同样带灰绿调");
            }
            _ => panic!("反射屏封面应为 Rgb"),
        }
        let color = cover_pixel_impl(&DIMCOLOR, 200, 100, 50, 255);
        match color {
            Color::Rgb(r, g, b) => {
                assert!(r > g && g > b, "原图的色相关系应保留：{color:?}");
                assert!(r < 230, "彩色也应保持柔和，不能鲜亮刺眼");
            }
            _ => panic!("反射屏封面应为 Rgb"),
        }
        assert_eq!(
            cover_pixel_impl(&DIMCOLOR, 200, 100, 50, 0),
            DIMCOLOR.bg.unwrap_or(Color::Black)
        );
    }

    #[test]
    fn dimcolor_reflective_tft_is_muted_not_backlit() {
        let d = select("dimcolor");
        if let Some(Color::Rgb(r, g, b)) = d.bg {
            assert!(
                r > 120 && g > 120 && b > 120,
                "反射屏应是浅灰底而非背光黑底"
            );
            assert!(g >= r && g >= b, "底色应带轻微灰绿调");
        } else {
            panic!("暗彩底应为 Rgb");
        }
        if let Some(Color::Rgb(r, g, b)) = d.fg {
            assert!(r < 90 && g < 90 && b < 90, "正文应使用柔和深色保证浅底可读");
        } else {
            panic!("暗彩正文应为 Rgb");
        }
        for c in [d.ok, d.info, d.bar, d.title_focus] {
            if let Color::Rgb(r, g, b) = c {
                let mx = r.max(g).max(b);
                let mn = r.min(g).min(b);
                assert!(mx - mn >= 20, "彩色保留色相但不能压成灰阶");
            } else {
                panic!("暗彩彩色应为 Rgb");
            }
        }
        assert!(d.title_fg.is_some() && d.pass_fg.is_some());
        assert!(d.sel_fg.is_some(), "选中项应为深底浅字");
        assert!(d.dim_screen, "暗彩应启用反射屏像素映射");
        assert!(!d.monochrome, "暗彩是低饱和彩色屏，非单色");
    }

    #[test]
    fn dimcolor_has_multiple_muted_color_families() {
        let d = select("dimcolor");
        assert_eq!(d.ok, Color::Rgb(72, 115, 78), "播放态使用青绿");
        assert_eq!(d.info, Color::Rgb(76, 101, 130), "信息提示使用灰蓝");
        assert_eq!(d.bar, Color::Rgb(65, 112, 108), "频谱柱使用青绿色");
        assert_eq!(
            d.title_focus,
            Color::Rgb(133, 101, 55),
            "焦点标题使用琥珀色"
        );
        assert_eq!(d.peak_fg, Some(Color::Rgb(117, 91, 112)), "峰值帽使用灰紫");
    }

    #[test]
    fn graywhite_fstn_neutral_gray_classic_chars() {
        let g = select("graywhite");
        assert!(g.bg.is_some(), "灰白为浅底");
        assert!(
            g.title_fg.is_some() && g.pass_fg.is_some(),
            "浅底曲名/直通须覆写"
        );
        assert!(g.sel_fg.is_some(), "深底浅字反白");
        assert!(!g.block_spectrum, "灰白频谱用经典字符池");
        assert!(g.monochrome, "灰白单色渲染（封面/电平）");
        assert!(g.peak_fg.is_some(), "峰值帽须覆写（浅底）");
        // FSTN 灰白微绿调：底色偏亮、绿通道略高但绿得很淡
        if let Some(Color::Rgb(r, gg, b)) = g.bg {
            assert!(r > 150 && gg > 150 && b > 150, "浅底");
            assert!(gg >= r && gg >= b, "微绿调");
            assert!(gg - r <= 10 && gg - b <= 12, "绿调要淡，不能鲜绿");
        } else {
            panic!("灰白底色应为 Rgb");
        }
    }

    #[test]
    fn blackwhite_inverted_selection() {
        let m = select("blackwhite");
        assert_eq!(m.bg, Some(Color::Rgb(255, 255, 255)));
        assert_eq!(m.fg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(m.sel_bg, Color::Rgb(0, 0, 0));
        assert_eq!(m.sel_fg, Some(Color::Rgb(255, 255, 255)), "黑底白字反白");
        assert!(
            m.title_fg.is_some() && m.pass_fg.is_some(),
            "浅底曲名/直通须覆写"
        );
    }

    #[test]
    fn other_themes_keep_black_selection_text() {
        for id in ["phosphor", "classic", "deepblue", "terminal"] {
            assert_eq!(select(id).sel_fg, None, "{id} 选中保持黑字");
        }
    }

    #[test]
    fn blackwhite_block_spectrum_and_visible_peak() {
        let m = select("blackwhite");
        assert!(m.block_spectrum, "黑白频谱用实心色块");
        assert!(m.peak_fg.is_some(), "黑白峰值帽须覆写（白底白帽会隐身）");
        assert_ne!(m.peak_fg, Some(m.bar), "峰值帽色须与柱身区分");
        for id in ["phosphor", "classic", "deepblue"] {
            assert!(!select(id).block_spectrum, "{id} 频谱保持风格字符池");
            assert_eq!(select(id).peak_fg, None, "{id} 峰值帽保持白");
        }
        assert_eq!(
            select("terminal").peak_fg,
            Some(Color::Reset),
            "终端原生跟随默认字色"
        );
    }

    #[test]
    fn blackwhite_overrides_title_and_pass_for_light_bg() {
        let p = select("blackwhite");
        assert!(p.title_fg.is_some(), "黑白曲名须覆写深色");
        assert!(p.pass_fg.is_some(), "黑白直通参数须覆写深色");
        // 深底主题不覆写，保持固定黄 / 白语义色约定；终端原生主题例外：
        // 显式 Reset 跟随终端默认字色（浅色终端下固定黄/白不可读）。
        for id in ["phosphor", "classic", "deepblue"] {
            assert_eq!(select(id).title_fg, None, "{id} 曲名保持固定黄");
            assert_eq!(select(id).pass_fg, None, "{id} 直通保持固定白");
        }
        assert_eq!(select("terminal").title_fg, Some(Color::Reset));
        assert_eq!(select("terminal").pass_fg, Some(Color::Reset));
    }

    #[test]
    fn terminal_theme_follows_terminal_colors() {
        let t = select("terminal");
        assert_eq!(t.bg, None);
        assert_eq!(t.fg, None);
        assert_eq!(t.chrome, Color::Reset);
        assert_eq!(t.sel_bg, Color::White, "选中底例外取白保黑字可读");
    }

    /// 主题铺底机制：先整屏铺底色/字色后，未显式着色的文字必须继承它
    /// （这是「黑底绿字」成立的前提；ratatui 单元格样式按字段覆盖）。
    #[test]
    fn base_fill_survives_default_text_style() {
        use ratatui::backend::TestBackend;
        use ratatui::widgets::{Block, Paragraph};
        use ratatui::Terminal;

        let mut term = Terminal::new(TestBackend::new(8, 2)).unwrap();
        term.draw(|f| {
            let area = f.area();
            f.render_widget(
                Block::default().style(Style::default().bg(Color::Black).fg(Color::LightGreen)),
                area,
            );
            f.render_widget(Paragraph::new("hi"), area);
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert_eq!(
            buf[(0, 0)].fg,
            Color::LightGreen,
            "默认文字不得覆盖铺底字色"
        );
        assert_eq!(buf[(0, 0)].bg, Color::Black, "默认文字不得覆盖铺底底色");
    }

    /// 端到端：整屏绘制时电平表仍按阈值变色（70% 黄 / 95% 红）。
    /// 音频引擎可初始化时才有意义；无设备环境自动跳过（不误报）。
    #[test]
    fn full_draw_level_meter_still_changes_color() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let config = crate::config::Config::default();
        let mut app = crate::tui::app::App::new(&config);
        let Some(engine) = app.engine.as_ref() else {
            return; // 无音频设备（CI 等）：跳过
        };
        engine.set_level_lr_for_test(0.70, 0.95);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| {
            crate::tui::render::draw(
                f,
                &mut app,
                &config,
                &[],
                std::time::Duration::from_millis(16),
            );
        })
        .unwrap();
        let buf = term.backend().buffer();
        let count = |c: Color| {
            (0..buf.area.height)
                .map(|y| (0..buf.area.width).filter(|&x| buf[(x, y)].fg == c).count())
                .sum::<usize>()
        };
        // 70% / 95% 时整条柱应转为黄 / 红，而不是只有零星几格。
        assert!(count(Color::Yellow) >= 3, "70% 应出现黄色电平柱");
        assert!(count(Color::Red) >= 3, "95% 应出现红色电平柱");
    }

    /// 离屏渲染一帧真实基础版界面，写单元格文本（开发用观感核验，默认忽略）：
    /// `cargo test -p tuneux dump_theme_frame -- --ignored --nocapture`
    #[test]
    #[ignore = "开发用：离屏渲染一帧并输出单元格供观感核验"]
    fn dump_theme_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut config = crate::config::Config::default();
        // 观感核验可指定任一 主题表 主题 id，默认走配置默认值。
        if let Ok(t) = std::env::var("TUNEUX_PREVIEW_THEME") {
            config.theme = t;
        }
        let mut app = crate::tui::app::App::new(&config);
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| {
            crate::tui::render::draw(
                f,
                &mut app,
                &config,
                &[],
                std::time::Duration::from_millis(16),
            );
        })
        .unwrap();

        fn rgb(c: Color) -> (u8, u8, u8) {
            match c {
                Color::Black => (0, 0, 0),
                Color::Red => (170, 0, 0),
                Color::Green => (0, 170, 0),
                Color::Yellow => (255, 255, 85),
                Color::Blue => (0, 0, 170),
                Color::Magenta => (170, 0, 170),
                Color::Cyan => (0, 170, 170),
                Color::Gray => (170, 170, 170),
                Color::DarkGray => (85, 85, 85),
                Color::LightRed => (255, 85, 85),
                Color::LightGreen => (85, 255, 85),
                Color::LightYellow => (255, 255, 170),
                Color::LightBlue => (85, 85, 255),
                Color::LightMagenta => (255, 85, 255),
                Color::LightCyan => (85, 255, 255),
                Color::White => (255, 255, 255),
                Color::Rgb(r, g, b) => (r, g, b),
                Color::Indexed(_) | Color::Reset => (0, 0, 0),
            }
        }
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                let (fr, fg_, fb) = rgb(cell.fg);
                let (br, bg_, bb) = rgb(cell.bg);
                out.push_str(&format!(
                    "{fr:02x}{fg_:02x}{fb:02x},{br:02x}{bg_:02x}{bb:02x},{:x};",
                    cell.symbol().chars().next().unwrap_or(' ') as u32
                ));
            }
            out.push('\n');
        }
        let path = std::env::temp_dir().join(format!("tuneux-frame-{}.txt", config.theme));
        std::fs::write(&path, out).expect("写帧文本失败");
        println!("frame -> {}", path.display());
    }
}
