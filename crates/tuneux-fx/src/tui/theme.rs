//! # 调色板（皮肤数据）
//!
//! 界面配色 = 一份调色板数据（各界面角色的颜色 + 边框线型），本身不含
//! 任何渲染逻辑；渲染层按调色板画。调色板只来自**皮肤插件**（插件向
//! 宿主注册，插件只供色、不碰渲染）；无皮肤 / 未选皮肤时用内置藏青
//! [`Palette::default`]（藏青配色）；终端原生观感由随包皮肤承担，
//! [`Palette::terminal`] 同时作为皮肤解析的缺省基底。

use ratatui::style::Color;
use ratatui::widgets::BorderType;

/// 一套皮肤（调色板）：各界面角色的颜色与边框线型。
///
/// 字段为 `Option<Color>`——`None` 表示沿用终端默认色（跟随用户终端
/// 配色）。角色划分：屏幕底 / 正文 / 边框 / 菜单栏 / 功能键号 / 频谱
///（柱体、峰值帽、基线）/ 电平表三档；另含字符风格枚举（bar_style）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 屏幕底色；`None` = 用终端默认底色。
    pub bg: Option<Color>,
    /// 正文颜色；`None` = 用终端默认字色。
    pub fg: Option<Color>,
    /// 边框颜色；`None` = 用终端默认。
    pub border: Option<Color>,
    /// 边框线型（单线 / 双线）。
    pub border_type: BorderType,
    /// 菜单栏底色；`None` = 终端默认。
    pub menu_bg: Option<Color>,
    /// 菜单栏字色；`None` = 终端默认。
    pub menu_fg: Option<Color>,
    /// 功能键号颜色；`None` = 功能键栏用暗淡纯文本（不着色）。
    pub fkey_num: Option<Color>,
    /// 语义色：bit-perfect 直通（技术参数行亮色，白）。
    pub passthrough_fg: Color,
    /// 语义色：软件重采样降级（技术参数行暗色，深灰）。
    pub resample_fg: Color,
    /// 频谱柱体颜色；`None` = 默认亮绿（LightGreen）。
    pub bar_fg: Option<Color>,
    /// 频谱峰值帽（白帽）颜色；`None` = 默认白。
    pub peak_fg: Option<Color>,
    /// 频谱基线 / 网格颜色；`None` = 默认深灰（DarkGray）。
    pub grid_fg: Option<Color>,
    /// 电平表低档（<60%）颜色；`None` = 默认绿。
    pub level_low: Option<Color>,
    /// 电平表中档（60–85%）颜色；`None` = 默认黄。
    pub level_mid: Option<Color>,
    /// 电平表高档（≥85%）颜色；`None` = 默认红。
    pub level_high: Option<Color>,
    /// 三桶能量（频）低频桶颜色；`None` = 回落 bar_fg。
    pub band_low: Option<Color>,
    /// 三桶能量中频桶颜色；`None` = 回落 bar_fg。
    pub band_mid: Option<Color>,
    /// 三桶能量高频桶颜色；`None` = 回落 bar_fg。
    pub band_high: Option<Color>,
    /// 频谱柱字符风格（皮肤键 `bar_style`）。
    pub bar_style: BarStyle,
    /// 实心色块频谱（皮肤键 `block_spectrum`）。
    pub block_spectrum: bool,
    /// 单色封面渲染（皮肤键 `monochrome`）。
    pub monochrome: bool,
    /// 反射式 TFT 封面渲染（皮肤键 `dim_screen`）。
    pub dim_screen: bool,
}

pub use tuneux_mediax::BarStyle;

impl Default for Palette {
    /// 内置藏青：藏青配色（深蓝底 + 双线青框 + 灰字 + 灰底菜单栏 + 黄键号）。
    /// 这是无皮肤 / 未选皮肤时的开箱观感；频谱与电平表字段为 None = 各自
    /// 内置藏青（亮绿柱 / 白帽 / 深灰基线 / 绿黄红阈值）；终端原生观感由
    /// 随包皮肤承担，不再另设同名内置选择项。
    fn default() -> Self {
        Self {
            bg: Some(Color::Rgb(16, 22, 58)),
            fg: Some(Color::Gray),
            border: Some(Color::Cyan),
            border_type: BorderType::Double,
            menu_bg: Some(Color::Gray),
            menu_fg: Some(Color::Black),
            fkey_num: Some(Color::Yellow),
            passthrough_fg: Color::White,
            resample_fg: Color::DarkGray,
            bar_fg: None,
            peak_fg: None,
            grid_fg: None,
            level_low: None,
            level_mid: None,
            level_high: None,
            band_low: None,
            band_mid: None,
            band_high: None,
            bar_style: BarStyle::Matrix,
            block_spectrum: false,
            monochrome: false,
            dim_screen: false,
        }
    }
}

impl Palette {
    /// 终端原生配色（全 None 沿用终端默认色 + 单线框），也是皮肤解析基底。
    /// 选择器里的终端原生观感由随包皮肤提供。
    pub fn terminal() -> Self {
        Self {
            bg: None,
            fg: None,
            border: None,
            border_type: BorderType::Plain,
            menu_bg: None,
            menu_fg: None,
            fkey_num: None,
            passthrough_fg: Color::White,
            resample_fg: Color::DarkGray,
            bar_fg: None,
            peak_fg: None,
            grid_fg: None,
            level_low: None,
            level_mid: None,
            level_high: None,
            band_low: None,
            band_mid: None,
            band_high: None,
            bar_style: BarStyle::Matrix,
            block_spectrum: false,
            monochrome: false,
            dim_screen: false,
        }
    }

    /// 按终端色深调整实际输出；皮肤文本解析仍保留原始 RGB 值。
    pub fn for_terminal(mut self, is_truecolor: bool) -> Self {
        if !is_truecolor {
            let adapt = |color| tuneux_displayx::color_for_terminal(color, false);
            self.bg = self.bg.map(adapt);
            self.fg = self.fg.map(adapt);
            self.border = self.border.map(adapt);
            self.menu_bg = self.menu_bg.map(adapt);
            self.menu_fg = self.menu_fg.map(adapt);
            self.fkey_num = self.fkey_num.map(adapt);
            self.bar_fg = self.bar_fg.map(adapt);
            self.peak_fg = self.peak_fg.map(adapt);
            self.grid_fg = self.grid_fg.map(adapt);
            self.level_low = self.level_low.map(adapt);
            self.level_mid = self.level_mid.map(adapt);
            self.level_high = self.level_high.map(adapt);
            self.band_low = self.band_low.map(adapt);
            self.band_mid = self.band_mid.map(adapt);
            self.band_high = self.band_high.map(adapt);
            self.passthrough_fg = adapt(self.passthrough_fg);
            self.resample_fg = adapt(self.resample_fg);
        }
        self
    }

    /// 从皮肤文本解析调色板（插件皮肤）。
    ///
    /// 格式：每行 `键=值`，`#` 开头为注释、空行忽略。颜色为 `#rrggbb`
    /// 十六进制、`none`（沿用终端默认）或具名色；未知键 / 非法值宽松忽略——
    /// 坏皮肤不阻断插件加载，缺失字段回落终端原生基底（同 [`Palette::terminal`]）。
    /// 只要存在任意含 `=` 的行即视为皮肤已注册（返回 `Some`，哪怕全是
    /// 未知键）；文本完全无键值对（空 / 纯注释）才返回 `None`。
    pub fn from_skin(text: &str) -> Option<Self> {
        // 皮肤在终端原生底上覆盖（缺失字段 = 沿用终端默认）——与「默认项 =
        // 藏青回退」分离：内置回退由 Palette::default() 承担，皮肤只负责改色。
        let mut pal = Palette::terminal();
        let mut any = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            any = true;
            match k {
                "bg" => pal.bg = parse_color(v),
                "fg" => pal.fg = parse_color(v),
                "border" => pal.border = parse_color(v),
                "border_type" => {
                    pal.border_type = if v == "double" {
                        BorderType::Double
                    } else {
                        BorderType::Plain
                    };
                }
                "menu_bg" => pal.menu_bg = parse_color(v),
                "menu_fg" => pal.menu_fg = parse_color(v),
                "fkey_num" => pal.fkey_num = parse_color(v),
                "bar_fg" => pal.bar_fg = parse_color(v),
                "peak_fg" => pal.peak_fg = parse_color(v),
                "grid_fg" => pal.grid_fg = parse_color(v),
                "level_low" => pal.level_low = parse_color(v),
                "level_mid" => pal.level_mid = parse_color(v),
                "level_high" => pal.level_high = parse_color(v),
                "band_low" => pal.band_low = parse_color(v),
                "band_mid" => pal.band_mid = parse_color(v),
                "band_high" => pal.band_high = parse_color(v),
                "bar_style" => {
                    pal.bar_style = BarStyle::from_name(v).unwrap_or_default();
                }
                "block_spectrum" => pal.block_spectrum = parse_bool(v),
                "monochrome" => pal.monochrome = parse_bool(v),
                "dim_screen" => pal.dim_screen = parse_bool(v),
                "passthrough" => {
                    if let Some(c) = parse_color(v) {
                        pal.passthrough_fg = c;
                    }
                }
                "resample" => {
                    if let Some(c) = parse_color(v) {
                        pal.resample_fg = c;
                    }
                }
                _ => {}
            }
        }
        any.then_some(pal)
    }
}

/// 从皮肤文本提取皮肤名（`name=` 行）。
///
/// 皮肤名为显示用途（皮肤选择器清单）；缺省 None，调用方用文件名兜底。
pub fn skin_name(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name=") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// 解析皮肤颜色：`none` → `None`（沿用终端默认），`#rrggbb` → RGB，
/// 具名色 → 对应 [`Color`]；其余 `None`（宽松忽略，回落默认）。
fn parse_color(v: &str) -> Option<Color> {
    if v == "none" {
        return None;
    }
    if let Some(hex) = v.strip_prefix('#') {
        // is_ascii 先行：多字节字符可拼出恰好 6 字节（如 4 字节 emoji +
        // 2 字节重音字母），此时字节切片会落在字符边界中间；ASCII 才保证
        // 任意子串切片边界安全，非 ASCII 一律按非法颜色宽松忽略。
        if hex.len() == 6 && hex.is_ascii() {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }
    Some(match v {
        "white" => Color::White,
        "black" => Color::Black,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "dark_gray" => Color::DarkGray,
        "red" => Color::Red,
        "green" => Color::Green,
        "blue" => Color::Blue,
        "yellow" => Color::Yellow,
        "cyan" => Color::Cyan,
        "magenta" => Color::Magenta,
        _ => return None,
    })
}

/// 皮肤布尔键解析：大小写不敏感；`true/1/yes/on` 为真，其余（含未知值）为假。
fn parse_bool(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes" | "on")
}

/// 将封面像素映射到主题，并按终端能力保留 RGB 或量化为 ANSI 16 色。
pub fn cover_pixel(p: &Palette, r: u8, g: u8, b: u8, a: u8) -> Color {
    cover_pixel_with_capability(p, r, g, b, a, tuneux_displayx::env_truecolor())
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

    #[test]
    fn from_skin_parses_hex_colors() {
        let text = "bg=#0d1321\nborder=#5f87af\nborder_type=double\nfkey_num=#ffb000\n";
        let pal = Palette::from_skin(text).expect("应解析出调色板");
        assert_eq!(pal.bg, Some(Color::Rgb(0x0d, 0x13, 0x21)));
        assert_eq!(pal.border, Some(Color::Rgb(0x5f, 0x87, 0xaf)));
        assert_eq!(pal.border_type, BorderType::Double);
        assert_eq!(pal.fkey_num, Some(Color::Rgb(0xff, 0xb0, 0x00)));
        // 未指定字段回落中性默认。
        assert_eq!(pal.fg, None);
    }

    /// 频谱 / 电平表皮肤键：颜色 + 字符风格解析，非法值回落默认。
    #[test]
    fn from_skin_parses_spectrum_keys() {
        let text = "bar_fg=#101010\npeak_fg=#202020\ngrid_fg=#303030\nlevel_low=#00ff00\nlevel_mid=#ffff00\nlevel_high=#ff0000\nbar_style=hanzi\n";
        let pal = Palette::from_skin(text).expect("应解析出调色板");
        assert_eq!(pal.bar_fg, Some(Color::Rgb(0x10, 0x10, 0x10)));
        assert_eq!(pal.peak_fg, Some(Color::Rgb(0x20, 0x20, 0x20)));
        assert_eq!(pal.grid_fg, Some(Color::Rgb(0x30, 0x30, 0x30)));
        assert_eq!(pal.level_low, Some(Color::Rgb(0x00, 0xff, 0x00)));
        assert_eq!(pal.level_mid, Some(Color::Rgb(0xff, 0xff, 0x00)));
        assert_eq!(pal.level_high, Some(Color::Rgb(0xff, 0x00, 0x00)));
        assert_eq!(pal.bar_style, BarStyle::Hanzi);

        // 非法风格值回落 Matrix；缺键回落默认（柱体 None = 内置亮绿）。
        let pal2 = Palette::from_skin("bar_style=bogus\n").expect("风格键可解析");
        assert_eq!(pal2.bar_style, BarStyle::Matrix);
        assert_eq!(pal2.bar_fg, None);
    }

    /// 皮肤名提取：name= 键优先；缺省 / 空值 / 注释行均返回 None。
    #[test]
    fn skin_name_extraction() {
        assert_eq!(
            skin_name("name=深蓝\nbg=#000000\n"),
            Some("深蓝".to_string())
        );
        assert_eq!(skin_name("bg=#000000\n"), None);
        assert_eq!(skin_name("name=\n"), None);
        assert_eq!(skin_name("# name=注释里的不算\n"), None);
    }

    #[test]
    fn from_skin_lenient_on_empty_and_invalid() {
        assert_eq!(Palette::from_skin(""), None, "空文本应无皮肤");
        assert_eq!(Palette::from_skin("# 只有注释\n"), None);
        let pal = Palette::from_skin("bg=none\nunknown=1\n").expect("宽松解析");
        assert_eq!(pal.bg, None, "none 表示沿用终端默认");
        // 未知键也计入「已注册」：只要有含 = 的行就返回 Some（皮肤基底调色板），
        // 与函数文档口径一致。皮肤基底 = 终端原生（缺失字段沿用终端默认）。
        let pal = Palette::from_skin("unknown=1\n").expect("未知键仍算已注册");
        assert_eq!(pal, Palette::terminal());
        // 非法十六进制颜色不应 panic，回落默认。
        let pal = Palette::from_skin("fg=#zzzzzz\n").expect("非法颜色宽松忽略");
        assert_eq!(pal.fg, None);
    }

    #[test]
    fn from_skin_multibyte_hex_is_ignored_without_panic() {
        // 4 字节 emoji + 2 字节重音字母恰好 6 字节：非 ASCII 的伪颜色必须
        // 宽松忽略而非字节切片 panic（回归旧缺陷：切在字符边界中间）。
        let pal = Palette::from_skin("fg=#\u{1F600}\u{00E9}\n").expect("宽松解析");
        assert_eq!(pal.fg, None, "多字节伪颜色应被忽略且不 panic");
    }

    #[test]
    fn from_skin_parses_display_flags() {
        let mono =
            Palette::from_skin("bg=#ffffff\nfg=#000000\nmonochrome=true\nblock_spectrum=true\n")
                .expect("单色皮肤");
        assert!(mono.monochrome && mono.block_spectrum);
        assert!(!mono.dim_screen);
        assert_eq!(
            cover_pixel_with_capability(&mono, 10, 20, 30, 0, true),
            mono.bg.unwrap_or(Color::Black)
        );

        let dim =
            Palette::from_skin("bg=#9ca494\nfg=#2d352d\ndim_screen=true\n").expect("暗彩皮肤");
        assert!(dim.dim_screen);
        match cover_pixel_with_capability(&dim, 0, 0, 0, 255, true) {
            Color::Rgb(r, g, b) => {
                assert!(r > 40 && g > 40 && b > 40, "反射式封面应抬高黑位");
                assert!(g >= r && g >= b, "黑位应带灰绿调");
            }
            _ => panic!("反射式封面应为 Rgb"),
        }
        assert_eq!(
            cover_pixel_with_capability(&dim, 200, 100, 50, 0, true),
            dim.bg.unwrap_or(Color::Black)
        );
    }

    #[test]
    fn default_is_navy_style() {
        // 内置藏青 = 藏青配色（v0.5.0 观感）：深蓝底 + 双线青框 + 黄键号。
        let pal = Palette::default();
        assert_eq!(pal.bg, Some(Color::Rgb(16, 22, 58)));
        assert_eq!(pal.fg, Some(Color::Gray));
        assert_eq!(pal.border, Some(Color::Cyan));
        assert_eq!(pal.border_type, BorderType::Double);
        assert_eq!(pal.menu_bg, Some(Color::Gray));
        assert_eq!(pal.menu_fg, Some(Color::Black));
        assert_eq!(pal.fkey_num, Some(Color::Yellow));
        assert_eq!(pal.passthrough_fg, Color::White);
        assert_eq!(pal.resample_fg, Color::DarkGray);
        // 频谱 / 电平表为 None = 内置藏青（亮绿柱 / 白帽 / 绿黄红阈值）。
        assert_eq!(pal.bar_fg, None);
        assert_eq!(pal.bar_style, BarStyle::Matrix);
    }

    #[test]
    fn terminal_fallback_quantizes_every_rgb_role() {
        let original = Palette::from_skin(
            "bg=#9ca494\nfg=#2d352d\nbar_fg=#41706c\nlevel_mid=#9a8a3a\nlevel_high=#a04038\n",
        )
        .expect("皮肤");
        let pal = original.for_terminal(false);
        let optional = [
            pal.bg,
            pal.fg,
            pal.border,
            pal.menu_bg,
            pal.menu_fg,
            pal.fkey_num,
            pal.bar_fg,
            pal.peak_fg,
            pal.grid_fg,
            pal.level_low,
            pal.level_mid,
            pal.level_high,
            pal.band_low,
            pal.band_mid,
            pal.band_high,
        ];
        for color in optional
            .into_iter()
            .flatten()
            .chain([pal.passthrough_fg, pal.resample_fg])
        {
            assert!(!matches!(color, Color::Rgb(..)), "仍输出 RGB：{color:?}");
        }
        assert_eq!(original.for_terminal(true), original, "真彩保持原色");
    }

    #[test]
    fn terminal_is_native_fallback() {
        // 终端原生项：全 None 沿用终端默认色 + 单线框。
        let pal = Palette::terminal();
        assert_eq!(pal.bg, None);
        assert_eq!(pal.fg, None);
        assert_eq!(pal.border, None);
        assert_eq!(pal.border_type, BorderType::Plain);
        assert_eq!(pal.menu_bg, None);
        assert_eq!(pal.fkey_num, None);
    }
}
