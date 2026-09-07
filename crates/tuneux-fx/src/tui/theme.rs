//! # 主题与调色板
//!
//! **主题 = 一份调色板数据**（各界面角色的颜色 + 边框线型），本身不含任何
//! 渲染逻辑；宿主按调色板渲染。将来「插件换皮肤」就是插件向
//! 宿主注册一份新调色板——插件只供色、不碰渲染，天然安全。
//!
//! 内置两套：[`Theme::Clean`]（现代干净，备选）与 [`Theme::Dos`]（DOS 复古）。

use ratatui::style::Color;
use ratatui::widgets::BorderType;

/// 界面配色主题。`F2` 在内置主题间切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// 现代干净渲染（备选）。
    Clean,
    /// DOS 复古渲染（默认）。
    #[default]
    Dos,
}

/// 一套皮肤（调色板）：各界面角色的颜色与边框线型。
///
/// 字段为 `Option<Color>`——`None` 表示沿用终端默认色（干净版大量用默认，
/// 跟随用户终端配色）。角色划分：屏幕底 / 正文 / 边框 / 菜单栏 / 功能键号。
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
}

impl Theme {
    /// 取当前主题对应的调色板。
    pub fn palette(self) -> Palette {
        match self {
            // 干净版：全用终端默认色 + 单线框，功能键栏暗淡纯文本。
            Theme::Clean => Palette {
                bg: None,
                fg: None,
                border: None,
                border_type: BorderType::Plain,
                menu_bg: None,
                menu_fg: None,
                fkey_num: None,
                passthrough_fg: Color::White,
                resample_fg: Color::DarkGray,
            },
            // DOS 版：深蓝护眼底 + 双线青框 + 浅灰字 + 浅色菜单栏 + 黄色功能键号。
            Theme::Dos => Palette {
                bg: Some(Color::Rgb(16, 22, 58)),
                fg: Some(Color::Gray),
                border: Some(Color::Cyan),
                border_type: BorderType::Double,
                menu_bg: Some(Color::Gray),
                menu_fg: Some(Color::Black),
                fkey_num: Some(Color::Yellow),
                passthrough_fg: Color::White,
                resample_fg: Color::DarkGray,
            },
        }
    }
}

impl Palette {
    /// 从皮肤文本解析调色板（插件皮肤，v1 只动颜色）。
    ///
    /// 格式：每行 `键=值`，`#` 开头为注释、空行忽略。颜色为 `#rrggbb`
    /// 十六进制、`none`（沿用终端默认）或具名色；未知键 / 非法值宽松忽略——
    /// 坏皮肤不阻断插件加载，缺失字段回落中性默认（同 [`Theme::Clean`]）。
    /// 返回 `None` 仅当文本里没有任何 `键=值` 行（含未知键，宽松忽略）。
    pub fn from_skin(text: &str) -> Option<Self> {
        let mut pal = Theme::Clean.palette();
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
        // 未指定字段回落中性默认（Clean 基底）。
        assert_eq!(pal.fg, None);
    }

    #[test]
    fn from_skin_lenient_on_empty_and_invalid() {
        assert_eq!(Palette::from_skin(""), None, "空文本应无皮肤");
        assert_eq!(Palette::from_skin("# 只有注释\n"), None);
        let pal = Palette::from_skin("bg=none\nunknown=1\n").expect("宽松解析");
        assert_eq!(pal.bg, None, "none 表示沿用终端默认");
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
}
