//! 封面像素主题映射与非真彩终端的 ANSI 16 色量化。

use ratatui::style::Color;

/// 封面绘制所需的主题参数，由产品调色板注入。
#[derive(Debug, Clone, Copy)]
pub struct CoverStyle {
    /// 单色明暗映射的亮端，也是单色主题的透明像素底色。
    pub bg: Option<Color>,
    /// 单色明暗映射的暗端。
    pub fg: Option<Color>,
    /// 是否按原图亮度映射为主题单色。
    pub is_monochrome: bool,
    /// 是否使用抬高黑位、低饱和的反射屏映射。
    pub is_dim_screen: bool,
    /// 是否允许输出 24 位 RGB；否则量化为 ANSI 16 色。
    pub is_truecolor: bool,
}

/// 将封面像素映射到主题，并按终端能力控制输出色深。
pub fn cover_pixel(style: &CoverStyle, r: u8, g: u8, b: u8, a: u8) -> Color {
    let color = if style.is_monochrome {
        let bg = style.bg.unwrap_or(Color::White);
        if a <= 128 {
            bg
        } else {
            let (dr, dg, db) = rgb_of(style.fg.unwrap_or(Color::Black));
            let (lr, lg, lb) = rgb_of(bg);
            let luminance = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0;
            Color::Rgb(
                (dr as f32 + (lr as f32 - dr as f32) * luminance).round() as u8,
                (dg as f32 + (lg as f32 - dg as f32) * luminance).round() as u8,
                (db as f32 + (lb as f32 - db as f32) * luminance).round() as u8,
            )
        }
    } else if a <= 128 {
        if style.is_dim_screen {
            style.bg.unwrap_or(Color::Black)
        } else {
            Color::Black
        }
    } else if style.is_dim_screen {
        let luminance = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0;
        let shade = 0.22 + 0.68 * luminance;
        let lifted = |value: u8, dark: u8, light: u8| {
            (dark as f32
                + f32::from(light - dark) * shade
                + (value as f32 / 255.0 - luminance) * 0.5)
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Color::Rgb(lifted(r, 45, 202), lifted(g, 45, 202), lifted(b, 43, 190))
    } else {
        Color::Rgb(r, g, b)
    };
    color_for_terminal(color, style.is_truecolor)
}

/// 非真彩终端把 RGB 量化到最接近的 ANSI 16 色；具名色与索引色保持不变。
pub fn color_for_terminal(color: Color, is_truecolor: bool) -> Color {
    if is_truecolor {
        return color;
    }
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    const ANSI: &[Color] = &[
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ];
    let mut best = Color::Black;
    let mut best_distance = u32::MAX;
    for &candidate in ANSI {
        let (cr, cg, cb) = rgb_of(candidate);
        let distance = squared_distance(r, cr) + squared_distance(g, cg) + squared_distance(b, cb);
        if distance < best_distance {
            best = candidate;
            best_distance = distance;
        }
    }
    best
}

fn squared_distance(left: u8, right: u8) -> u32 {
    let difference = left as i32 - right as i32;
    (difference * difference) as u32
}

fn rgb_of(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Gray => (170, 170, 170),
        Color::DarkGray => (85, 85, 85),
        Color::Red => (170, 0, 0),
        Color::Green => (0, 170, 0),
        Color::Yellow => (255, 255, 85),
        Color::Blue => (0, 0, 170),
        Color::Magenta => (170, 0, 170),
        Color::Cyan => (0, 170, 170),
        Color::LightRed => (255, 85, 85),
        Color::LightGreen => (85, 255, 85),
        Color::LightYellow => (255, 255, 170),
        Color::LightBlue => (85, 85, 255),
        Color::LightMagenta => (255, 85, 255),
        Color::LightCyan => (85, 255, 255),
        Color::Indexed(_) | Color::Reset => (0, 0, 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(is_truecolor: bool) -> CoverStyle {
        CoverStyle {
            bg: Some(Color::Rgb(255, 255, 255)),
            fg: Some(Color::Rgb(0, 0, 0)),
            is_monochrome: true,
            is_dim_screen: false,
            is_truecolor,
        }
    }

    #[test]
    fn truecolor_preserves_exact_cover_values() {
        assert_eq!(cover_pixel(&style(true), 0, 0, 0, 255), Color::Rgb(0, 0, 0));
        assert_eq!(
            cover_pixel(&style(true), 255, 255, 255, 255),
            Color::Rgb(255, 255, 255)
        );
    }

    #[test]
    fn ansi_cover_quantizes_all_rgb_branches() {
        for input in [0, 64, 128, 192, 255] {
            let color = cover_pixel(&style(false), input, input, input, 255);
            assert!(!matches!(color, Color::Rgb(..)), "仍输出 RGB：{color:?}");
        }
        assert_eq!(cover_pixel(&style(false), 255, 255, 255, 0), Color::White);
        let dim = CoverStyle {
            is_monochrome: false,
            is_dim_screen: true,
            ..style(false)
        };
        assert!(!matches!(
            cover_pixel(&dim, 200, 100, 50, 255),
            Color::Rgb(..)
        ));
    }

    #[test]
    fn ansi_quantization_keeps_named_colors() {
        assert_eq!(color_for_terminal(Color::Reset, false), Color::Reset);
        assert_eq!(color_for_terminal(Color::Green, false), Color::Green);
        assert_eq!(
            color_for_terminal(Color::Rgb(250, 250, 250), false),
            Color::White
        );
        assert_eq!(color_for_terminal(Color::Rgb(2, 3, 4), false), Color::Black);
    }
}
