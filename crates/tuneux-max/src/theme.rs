//! # 主题：终端原生 + 皮肤插件
//!
//! 默认是「终端原生」：不铺品牌底色、跟随系统深浅色，选中反白。
//! 皮肤插件（WASM，theme 能力）注册的调色板文本可切换为显式配色；
//! 两者共用同一套取色令牌 [`Palette`]。
//!
//! 参考共享 UI 骨架的做法：主题**双槽位**写入（Dark/Light 各一套）。

use std::sync::Mutex;

use eframe::egui::{self, Color32, Stroke, Theme, ThemePreference, Visuals};
use tuneux_mediax::BarStyle;

/// 当前生效的皮肤（全局，供渲染层取色；`None` = 终端原生）。
static SKIN: Mutex<Option<Skin>> = Mutex::new(None);

/// 一套皮肤配色（插件文本解析结果；缺省字段沿用默认）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skin {
    /// 显示名（`name=` 键；缺省用插件文件名兜底）。
    pub name: String,
    /// 面板底色。
    pub bg: Option<Color32>,
    /// 正文色。
    pub fg: Option<Color32>,
    /// 边框色。
    pub border: Option<Color32>,
    /// 菜单/浮层底色。
    pub menu_bg: Option<Color32>,
    /// 菜单文字。
    pub menu_fg: Option<Color32>,
    /// 快捷键键名色。
    pub key: Option<Color32>,
    /// 直通技术参数色。
    pub passthrough: Option<Color32>,
    /// 次要文字 / 降级参数色。
    pub dim: Option<Color32>,
    /// 频谱柱体色。
    pub bar: Option<Color32>,
    /// 频谱峰值帽色。
    pub peak: Option<Color32>,
    /// 电平低档色。
    pub level_low: Option<Color32>,
    /// 电平中档色。
    pub level_mid: Option<Color32>,
    /// 电平高档色。
    pub level_high: Option<Color32>,
    /// 频段低频桶色。
    pub band_low: Option<Color32>,
    /// 频段中频桶色。
    pub band_mid: Option<Color32>,
    /// 频段高频桶色。
    pub band_high: Option<Color32>,
    /// 频谱基线 / 空位弱色。
    pub grid: Option<Color32>,
    /// 频谱柱字符风格（皮肤键 `bar_style`，非法回落 Matrix）。
    pub bar_style: BarStyle,
    /// 频谱实心色块模式（皮肤键 `block_spectrum`，单色皮肤启用）。
    pub block_spectrum: bool,
}

impl Skin {
    /// 解析皮肤插件注册的调色板文本（`key=value` 行）。
    ///
    /// 未知键忽略；`bar_style` 经风格名解析（非法回落 Matrix）、
    /// `block_spectrum` 按布尔解析；一个可识别键都没有时返回 `None`。
    pub fn parse(text: &str, fallback_name: &str) -> Option<Self> {
        let mut skin = Skin {
            name: fallback_name.to_string(),
            bg: None,
            fg: None,
            border: None,
            menu_bg: None,
            menu_fg: None,
            key: None,
            passthrough: None,
            dim: None,
            bar: None,
            peak: None,
            level_low: None,
            level_mid: None,
            level_high: None,
            band_low: None,
            band_mid: None,
            band_high: None,
            grid: None,
            bar_style: BarStyle::default(),
            block_spectrum: false,
        };
        let mut any = false;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            let parsed = parse_color(value);
            match key {
                "name" => {
                    if !value.is_empty() {
                        skin.name = value.to_string();
                        any = true;
                    }
                }
                "bg" => {
                    skin.bg = parsed;
                    any = true;
                }
                "fg" => {
                    skin.fg = parsed;
                    any = true;
                }
                "border" => {
                    skin.border = parsed;
                    any = true;
                }
                "menu_bg" => {
                    skin.menu_bg = parsed;
                    any = true;
                }
                "menu_fg" => {
                    skin.menu_fg = parsed;
                    any = true;
                }
                "fkey_num" => {
                    skin.key = parsed;
                    any = true;
                }
                "passthrough" => {
                    skin.passthrough = parsed;
                    any = true;
                }
                "resample" => {
                    skin.dim = parsed;
                    any = true;
                }
                "bar_fg" => {
                    skin.bar = parsed;
                    any = true;
                }
                "peak_fg" => {
                    skin.peak = parsed;
                    any = true;
                }
                "grid_fg" => {
                    skin.grid = parsed;
                    any = true;
                }
                "bar_style" => {
                    skin.bar_style = BarStyle::from_name(value).unwrap_or_default();
                    any = true;
                }
                "block_spectrum" => {
                    skin.block_spectrum = parse_bool(value);
                    any = true;
                }
                "level_low" => {
                    skin.level_low = parsed;
                    any = true;
                }
                "level_mid" => {
                    skin.level_mid = parsed;
                    any = true;
                }
                "level_high" => {
                    skin.level_high = parsed;
                    any = true;
                }
                "band_low" => {
                    skin.band_low = parsed;
                    any = true;
                }
                "band_mid" => {
                    skin.band_mid = parsed;
                    any = true;
                }
                "band_high" => {
                    skin.band_high = parsed;
                    any = true;
                }
                _ => {}
            }
        }
        any.then_some(skin)
    }

    /// 由皮肤推导取色令牌。
    pub fn palette(&self) -> Palette {
        let bg = self.bg.unwrap_or(Color32::BLACK);
        let fg = self.fg.unwrap_or(Color32::WHITE);
        let bar = self.bar.unwrap_or(fg);
        let dim = self
            .dim
            .or(self.menu_fg)
            .unwrap_or(fg.gamma_multiply(0.6_f32));
        Palette {
            bg,
            fg,
            dim,
            border: self.border.unwrap_or(fg),
            bar,
            peak: self.peak.unwrap_or(fg),
            level_low: self.level_low.unwrap_or(bar),
            level_mid: self.level_mid.unwrap_or(fg),
            level_high: self.level_high.unwrap_or(fg),
            // 频段三桶色缺省回落柱体色（再缺省正文色），与 fx 同口径。
            band_low: self.band_low.unwrap_or(bar),
            band_mid: self.band_mid.unwrap_or(bar),
            band_high: self.band_high.unwrap_or(bar),
            // 基线 / 空位弱色缺省回落次要文字色。
            grid: self.grid.unwrap_or(dim),
            bar_style: self.bar_style,
            block_spectrum: self.block_spectrum,
            sel_bg: fg,
            sel_fg: bg,
        }
    }
}

/// 当前主题的取色令牌。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 面板底色。
    pub bg: Color32,
    /// 正文/边框主色。
    pub fg: Color32,
    /// 次要文字。
    pub dim: Color32,
    /// 面板边框色。
    pub border: Color32,
    /// 频谱柱体色。
    pub bar: Color32,
    /// 频谱峰值帽色。
    pub peak: Color32,
    /// 电平低档色。
    pub level_low: Color32,
    /// 电平中档色。
    pub level_mid: Color32,
    /// 电平高档色。
    pub level_high: Color32,
    /// 频段低频桶色。
    pub band_low: Color32,
    /// 频段中频桶色。
    pub band_mid: Color32,
    /// 频段高频桶色。
    pub band_high: Color32,
    /// 频谱基线 / 空位弱色。
    pub grid: Color32,
    /// 频谱柱字符风格。
    pub bar_style: BarStyle,
    /// 频谱实心色块模式。
    pub block_spectrum: bool,
    /// 选中行底色（反白：等于主色）。
    pub sel_bg: Color32,
    /// 选中行文字色（反白：等于底色）。
    pub sel_fg: Color32,
}

impl Palette {
    /// 从一组 `Visuals` 推导令牌（终端原生主题）。
    pub fn from_visuals(visuals: &Visuals) -> Self {
        let fg = visuals.widgets.noninteractive.fg_stroke.color;
        Self {
            bg: visuals.panel_fill,
            fg,
            dim: fg.gamma_multiply(0.6_f32),
            border: fg,
            bar: fg,
            peak: fg,
            level_low: fg,
            level_mid: fg,
            level_high: fg,
            band_low: fg,
            band_mid: fg,
            band_high: fg,
            grid: fg.gamma_multiply(0.6_f32),
            bar_style: BarStyle::default(),
            block_spectrum: false,
            sel_bg: fg,
            sel_fg: visuals.panel_fill,
        }
    }

    /// 当前上下文的取色令牌（皮肤优先，其次终端原生）。
    pub fn current(ctx: &egui::Context) -> Self {
        match active_skin() {
            Some(skin) => skin.palette(),
            None => Self::from_visuals(&ctx.style().visuals),
        }
    }
}

/// 当前生效皮肤（克隆快照）。
pub fn active_skin() -> Option<Skin> {
    SKIN.lock().ok().and_then(|guard| guard.clone())
}

/// 构造指定深浅档位的终端原生 `Visuals`（纯函数，可单测）。
pub fn terminal_visuals(mode: Theme) -> Visuals {
    let mut visuals = match mode {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    let text = visuals.widgets.noninteractive.fg_stroke.color;
    // 选中反白：底色取前景色、描边取面板底色（与终端反白同义）。
    visuals.selection.bg_fill = text;
    visuals.selection.stroke = Stroke::new(1.0_f32, visuals.panel_fill);
    visuals
}

/// 应用终端原生主题：两槽位写入 + 跟随系统深浅色。
pub fn apply(ctx: &egui::Context) {
    if let Ok(mut guard) = SKIN.lock() {
        *guard = None;
    }
    ctx.set_visuals_of(Theme::Dark, terminal_visuals(Theme::Dark));
    ctx.set_visuals_of(Theme::Light, terminal_visuals(Theme::Light));
    ctx.set_theme(ThemePreference::System);
}

/// 应用皮肤：两槽位写入同一套显式配色，并登记为当前皮肤。
pub fn apply_skin(ctx: &egui::Context, skin: &Skin) {
    // 「终端原生」皮肤插件只写 none（跟随终端默认）：不铺显式底色，
    // 回落到跟随系统深浅色的终端原生视觉，而不是黑色。
    if skin.bg.is_none() && skin.fg.is_none() {
        apply(ctx);
        return;
    }
    if let Ok(mut guard) = SKIN.lock() {
        *guard = Some(skin.clone());
    }
    let visuals = skin_visuals(skin);
    ctx.set_visuals_of(Theme::Dark, visuals.clone());
    ctx.set_visuals_of(Theme::Light, visuals);
    ctx.set_theme(ThemePreference::Dark);
}

/// 由皮肤构造 `Visuals`（纯函数，可单测）。
pub fn skin_visuals(skin: &Skin) -> Visuals {
    let bg = skin.bg.unwrap_or(Color32::BLACK);
    let light = luminance(bg) > 0.5;
    let mut visuals = if light {
        Visuals::light()
    } else {
        Visuals::dark()
    };
    let fg = skin
        .fg
        .unwrap_or(visuals.widgets.noninteractive.fg_stroke.color);
    visuals.panel_fill = bg;
    visuals.window_fill = skin.menu_bg.unwrap_or(bg);
    visuals.extreme_bg_color = shade(bg, if light { -18 } else { 18 });
    visuals.faint_bg_color = skin.dim.unwrap_or(fg.gamma_multiply(0.12_f32));
    visuals.override_text_color = Some(fg);
    visuals.widgets.noninteractive.fg_stroke.color = fg;
    visuals.widgets.inactive.fg_stroke.color = fg;
    visuals.widgets.hovered.fg_stroke.color = fg;
    visuals.widgets.active.fg_stroke.color = fg;
    if let Some(border) = skin.border {
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, border);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, border);
        visuals.widgets.active.bg_stroke = Stroke::new(1.0_f32, border);
    }
    visuals.selection.bg_fill = fg;
    visuals.selection.stroke = Stroke::new(1.0_f32, bg);
    if let Some(link) = skin.bar.or(skin.key) {
        visuals.hyperlink_color = link;
    }
    visuals
}

/// 解析布尔键值（皮肤文本口径：true/1/yes/on 为真，其余为假）。
fn parse_bool(value: &str) -> bool {
    matches!(value.trim(), "true" | "1" | "yes" | "on")
}

/// 解析颜色值：`none` → `None`；`#rrggbb` → RGB；其余 `None`。
fn parse_color(value: &str) -> Option<Color32> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    let hex = value.strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

/// 颜色亮度（0.0–1.0，用于判定深/浅底）。
fn luminance(color: Color32) -> f32 {
    (0.299 * color.r() as f32 + 0.587 * color.g() as f32 + 0.114 * color.b() as f32) / 255.0
}

/// 明暗微调（用于输入框等最深层底色）。
fn shade(color: Color32, delta: i32) -> Color32 {
    let adjust = |value: u8| (value as i32 + delta).clamp(0, 255) as u8;
    Color32::from_rgb(adjust(color.r()), adjust(color.g()), adjust(color.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_inverted_in_both_modes() {
        for mode in [Theme::Dark, Theme::Light] {
            let visuals = terminal_visuals(mode);
            let text = visuals.widgets.noninteractive.fg_stroke.color;
            assert_eq!(visuals.selection.bg_fill, text, "选中底应取前景色");
            assert_eq!(
                visuals.selection.stroke.color, visuals.panel_fill,
                "选中描边应取底色"
            );
        }
    }

    #[test]
    fn palette_maps_inverted_selection() {
        let visuals = terminal_visuals(Theme::Dark);
        let palette = Palette::from_visuals(&visuals);
        assert_eq!(
            palette.sel_bg,
            visuals.widgets.noninteractive.fg_stroke.color
        );
        assert_eq!(palette.sel_fg, visuals.panel_fill);
        assert_eq!(palette.bg, visuals.panel_fill);
    }

    #[test]
    fn parse_skin_reads_name_and_colors() {
        let skin = Skin::parse(
            "name=磷光绿\nbg=#000000\nfg=#55ff55\nborder=#55ff55\nbar_fg=#55ff55\nlevel_mid=#ffff00\nblock_spectrum=true",
            "fallback",
        )
        .expect("应解析成功");
        assert_eq!(skin.name, "磷光绿");
        assert_eq!(skin.bg, Some(Color32::BLACK));
        assert_eq!(skin.fg, Some(Color32::from_rgb(0x55, 0xff, 0x55)));
        assert_eq!(skin.level_mid, Some(Color32::YELLOW));
        let palette = skin.palette();
        assert_eq!(palette.bg, Color32::BLACK);
        assert_eq!(palette.sel_bg, palette.fg, "皮肤同样选中反白");
    }

    #[test]
    fn parse_skin_reads_band_colors_and_falls_back_to_bar() {
        let skin = Skin::parse(
            "name=x\nbar_fg=#55ff55\nband_low=#00aa00\nband_high=#ffff55",
            "x",
        )
        .expect("应解析成功");
        let palette = skin.palette();
        assert_eq!(palette.band_low, Color32::from_rgb(0x00, 0xaa, 0x00));
        assert_eq!(palette.band_mid, palette.bar, "缺省桶回落柱体色");
        assert_eq!(palette.band_high, Color32::from_rgb(0xff, 0xff, 0x55));
        // 无 bar_fg 时再回落正文色。
        let bare = Skin::parse("name=x\nfg=#112233", "x").expect("应解析成功");
        let palette = bare.palette();
        assert_eq!(palette.band_low, palette.fg);
        assert_eq!(palette.band_mid, palette.fg);
        assert_eq!(palette.band_high, palette.fg);
    }

    #[test]
    fn parse_skin_rejects_empty_or_unknown() {
        assert!(Skin::parse("", "x").is_none());
        assert!(Skin::parse("# 只有注释\nunknown_key=true", "x").is_none());
    }

    #[test]
    fn parse_skin_reads_bar_style_block_and_grid() {
        let skin = Skin::parse(
            "name=x\nbar_style=blocks\nblock_spectrum=true\ngrid_fg=#808080",
            "x",
        )
        .expect("应解析成功");
        assert_eq!(skin.bar_style, BarStyle::Blocks);
        assert!(skin.block_spectrum);
        let palette = skin.palette();
        assert_eq!(palette.bar_style, BarStyle::Blocks);
        assert!(palette.block_spectrum);
        assert_eq!(palette.grid, Color32::from_rgb(0x80, 0x80, 0x80));
        // 非法风格名回落 Matrix；布尔假值 / 缺省键保持默认。
        let bogus = Skin::parse("name=x\nbar_style=bogus", "x").expect("应解析成功");
        assert_eq!(bogus.bar_style, BarStyle::Matrix);
        let bare = Skin::parse("name=x\nfg=#112233", "x").expect("应解析成功");
        assert!(!bare.block_spectrum);
        assert_eq!(
            bare.palette().grid,
            bare.palette().dim,
            "grid 缺省回落次要色"
        );
        assert!(
            Skin::parse("name=x\nblock_spectrum=yes", "x")
                .expect("应解析成功")
                .block_spectrum
        );
    }

    #[test]
    fn non_ascii_color_is_ignored_without_panic() {
        let skin = Skin::parse("name=x\nbg=#😀é", "x").expect("非法颜色应被忽略");
        assert_eq!(skin.bg, None);
    }

    #[test]
    fn skin_visuals_use_explicit_colors() {
        let skin = Skin {
            name: "test".to_string(),
            bg: Some(Color32::from_rgb(10, 20, 30)),
            fg: Some(Color32::from_rgb(200, 210, 220)),
            border: Some(Color32::from_rgb(100, 110, 120)),
            ..Skin::parse("name=x", "x").unwrap()
        };
        let visuals = skin_visuals(&skin);
        assert_eq!(visuals.panel_fill, Color32::from_rgb(10, 20, 30));
        assert_eq!(
            visuals.override_text_color,
            Some(Color32::from_rgb(200, 210, 220))
        );
        assert_eq!(visuals.selection.bg_fill, Color32::from_rgb(200, 210, 220));
    }
}
