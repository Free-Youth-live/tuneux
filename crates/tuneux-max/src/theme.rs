//! # 新 UI 主题：调色板 + 视觉下发 + 字体链
//!
//! 视觉基准 = tuneux-fx：默认主题为 fx 开箱的 DOS 风（深蓝底、青框、
//! 灰字、黄键号、亮绿频谱、白峰帽）；另备纯终端黑白「term」。
//!
//! 纪律：调色板是唯一色彩来源——渲染层只读 Palette 字段取色，
//! 不得私藏硬编码色（语义阈值色 level_* 也入板）。
//!
//! 字体：内置文泉驿微米黑等宽面（assets/wqy-microhei-mono.ttf）作
//! CJK 主回退，系统字体二级回退；Proportional / Monospace 两族都挂
//! （egui 默认只带拉丁字体，不挂 CJK 回退中文全是豆腐块）。

use eframe::egui::{self, Color32, CornerRadius, Stroke};

/// 新 UI 调色板（egui 色彩空间）。
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    /// 窗口/面板底色。
    pub bg: Color32,
    /// 沉下面板色（菜单栏/功能键栏底）。
    pub panel_bg: Color32,
    /// 正文字色。
    pub fg: Color32,
    /// 弱字色（提示/次级/菜单数字前缀）。
    pub fg_weak: Color32,
    /// 强调色（焦点边框/当前曲目/功能键号/选中悬停）。
    pub accent: Color32,
    /// 悬停底色。
    pub sel_bg: Color32,
    /// 非焦点边框色。
    pub border: Color32,
    /// 网格/空位点色。
    pub grid: Color32,
    /// 频谱柱/三桶能量色。
    pub spec_bar: Color32,
    /// 频谱峰值白帽色。
    pub peak_fg: Color32,
    /// 直通语义色（亮）。
    pub pass_fg: Color32,
    /// 降级语义色（暗）。
    pub resample_fg: Color32,
    /// 电平阈值色：<60%。
    pub level_low: Color32,
    /// 电平阈值色：60–85%。
    pub level_mid: Color32,
    /// 电平阈值色：≥85%。
    pub level_high: Color32,
    /// 可视化字符池风格（皮肤 bar_style 键；默认 Matrix 细字雨）。
    pub bar_style: tuneux_mediax::BarStyle,
    /// 面板框饰风格：true = TUI 制框（标题嵌上边线、底色断线的
    /// ratatui Block 风）；false = GUI 头部条（完整边框 + panel_bg
    /// 标题带，彻底去 TUI 观感）。dos/term 与皮肤插件为 true；
    /// f2k 系为 false。
    pub tui_chrome: bool,
}

impl Default for Palette {
    /// 默认 = fx 开箱 DOS 风（深蓝底 + 青框 + 灰字 + 黄强调）。
    fn default() -> Self {
        Self {
            bg: Color32::from_rgb(16, 22, 58),
            panel_bg: Color32::from_rgb(12, 17, 45),
            fg: Color32::from_rgb(192, 192, 192),
            fg_weak: Color32::from_rgb(120, 122, 150),
            accent: Color32::from_rgb(255, 255, 85),
            sel_bg: Color32::from_rgb(40, 50, 120),
            border: Color32::from_rgb(0, 200, 220),
            grid: Color32::from_rgb(90, 92, 120),
            spec_bar: Color32::from_rgb(144, 238, 144),
            peak_fg: Color32::WHITE,
            pass_fg: Color32::WHITE,
            resample_fg: Color32::from_rgb(105, 105, 105),
            level_low: Color32::from_rgb(80, 200, 80),
            level_mid: Color32::from_rgb(220, 200, 80),
            level_high: Color32::from_rgb(230, 80, 80),
            bar_style: tuneux_mediax::BarStyle::Matrix,
            tui_chrome: true,
        }
    }
}

/// 按 id 取内置主题；未知 id 返回 None（调用方回退默认）。
pub fn builtin(id: &str) -> Option<Palette> {
    match id {
        "dos" => Some(Palette::default()),
        // 纯终端黑白：与终端原生配色观感一致。
        "term" => Some(Palette {
            bg: Color32::from_rgb(12, 12, 12),
            panel_bg: Color32::from_rgb(17, 17, 17),
            fg: Color32::from_rgb(204, 204, 204),
            fg_weak: Color32::from_rgb(118, 118, 118),
            accent: Color32::from_rgb(86, 156, 214),
            sel_bg: Color32::from_rgb(0, 55, 218),
            border: Color32::from_rgb(118, 118, 118),
            grid: Color32::from_rgb(51, 51, 51),
            spec_bar: Color32::from_rgb(204, 204, 204),
            peak_fg: Color32::WHITE,
            pass_fg: Color32::WHITE,
            resample_fg: Color32::from_rgb(105, 105, 105),
            ..Palette::default()
        }),
        // f2k 风格：Win 经典浅灰底 + 蓝色高亮（亮色皮肤；
        // apply_visuals 按底色亮度自动切换亮/暗控件基底）。
        // f2k 暗色风格（参照真实 f2k 暗色配置截图）：近黑底、
        // 浅灰字、蓝色强调/分组头、银灰反色选中、列头带、技术状态栏、
        // 频谱坐标轴——结构特征全部调色板驱动，dos/term 不受影响。
        "f2k" => Some(Palette {
            bg: Color32::from_rgb(28, 28, 28),
            panel_bg: Color32::from_rgb(38, 38, 38),
            fg: Color32::from_rgb(214, 214, 214),
            fg_weak: Color32::from_rgb(138, 138, 138),
            accent: Color32::from_rgb(91, 155, 213),
            sel_bg: Color32::from_rgb(52, 52, 52),
            border: Color32::from_rgb(74, 74, 74),
            grid: Color32::from_rgb(58, 58, 58),
            spec_bar: Color32::from_rgb(74, 159, 216),
            peak_fg: Color32::from_rgb(224, 224, 224),
            pass_fg: Color32::from_rgb(120, 200, 120),
            resample_fg: Color32::from_rgb(130, 130, 130),
            level_low: Color32::from_rgb(80, 200, 80),
            level_mid: Color32::from_rgb(220, 200, 80),
            level_high: Color32::from_rgb(230, 80, 80),
            bar_style: tuneux_mediax::BarStyle::Matrix,
            tui_chrome: false,
        }),
        // f2k 经典浅色：参照 f2k v1.x 默认 DUI（Win32 经典控件风）——
        // 白色播放列表为主体、#F0F0F0 经典 3D 面板灰作工具/沉底、深蓝强调、
        // 淡蓝选中（经典“选中但未聚焦”色）、绿色频谱柱。亮色皮肤，
        // apply_visuals 按底色亮度自动切亮色控件基底，与 f2k 暗色互为表里；
        // dos/term 不受影响。
        "f2k-light" => Some(Palette {
            bg: Color32::from_rgb(255, 255, 255),
            panel_bg: Color32::from_rgb(240, 240, 240),
            fg: Color32::from_rgb(0, 0, 0),
            fg_weak: Color32::from_rgb(128, 128, 128),
            accent: Color32::from_rgb(0, 60, 143),
            sel_bg: Color32::from_rgb(192, 216, 240),
            border: Color32::from_rgb(160, 160, 160),
            grid: Color32::from_rgb(212, 208, 200),
            spec_bar: Color32::from_rgb(46, 158, 68),
            peak_fg: Color32::from_rgb(128, 128, 128),
            pass_fg: Color32::from_rgb(0, 128, 0),
            resample_fg: Color32::from_rgb(128, 128, 128),
            level_low: Color32::from_rgb(0, 160, 0),
            level_mid: Color32::from_rgb(192, 160, 0),
            level_high: Color32::from_rgb(208, 0, 0),
            bar_style: tuneux_mediax::BarStyle::Blocks,
            tui_chrome: false,
        }),
        _ => None,
    }
}

/// 内置主题目录（id, 显示名）——皮肤选择菜单用；显示名为专名不译。
pub fn builtin_list() -> [(&'static str, &'static str); 4] {
    [
        ("dos", "fx DOS"),
        ("term", "Terminal"),
        ("f2k", "f2k 暗色"),
        ("f2k-light", "f2k 经典浅色"),
    ]
}

/// 皮肤默认模块集（结构模块开关）：dos/term = fx 极简集（全关，
/// 开箱即 fx 观感）；f2k = 现代 GUI 集（全开）。用户经右键/菜单的
/// 加装记录存配置 mod_over（按皮肤分桶），换肤自动套用该皮肤默认+覆盖。
pub fn default_modules(id: &str) -> &'static [(&'static str, bool)] {
    match id {
        // f2k 双肤同默认集：现代 GUI 结构模块全开
        "f2k" | "f2k-light" => &[
            ("transport", true),
            ("col_header", true),
            ("status_tech", true),
            ("spec_axis", true),
            ("group_rule", true),
            ("group_covers", true),
        ],
        _ => &[
            ("transport", false),
            ("col_header", false),
            ("status_tech", false),
            ("spec_axis", false),
            ("group_rule", false),
            ("group_covers", false),
        ],
    }
}

/// 解析皮肤色值：#rrggbb（ASCII 先行校验——多字节字符可拼出恰好
/// 6 字节，字节切片会落在字符边界中间；非 ASCII 一律宽松忽略）。
/// 值 none = 不覆盖该键（保持基底）。
fn parse_color(v: &str) -> Option<Color32> {
    let hex = v.strip_prefix('#')?;
    if hex.len() == 6 && hex.is_ascii() {
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some(Color32::from_rgb(r, g, b))
    } else {
        None
    }
}

/// 从皮肤文本构建调色板（fx 皮肤键规范、term 基底覆盖式——皮肤文件
/// 两端通用）。max 侧映射：menu_bg→panel_bg、menu_fg→fg、
/// fkey_num→accent、bar_fg→spec_bar、grid_fg→grid、passthrough→
/// pass_fg、resample→resample_fg、sel_bg→sel_bg；border_type /
/// bar_style / band_* 为 TUI 专有键，忽略。
pub fn from_skin(text: &str) -> Option<Palette> {
    let mut pal = builtin("term")?;
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
        if let Some(c) = parse_color(v) {
            any = true;
            match k {
                "bg" => pal.bg = c,
                "fg" => pal.fg = c,
                "border" => pal.border = c,
                "menu_bg" => pal.panel_bg = c,
                "menu_fg" => pal.fg = c,
                "fkey_num" => pal.accent = c,
                "bar_fg" => pal.spec_bar = c,
                "peak_fg" => pal.peak_fg = c,
                "grid_fg" => pal.grid = c,
                "level_low" => pal.level_low = c,
                "level_mid" => pal.level_mid = c,
                "level_high" => pal.level_high = c,
                "passthrough" => pal.pass_fg = c,
                "resample" => pal.resample_fg = c,
                "sel_bg" => pal.sel_bg = c,
                _ => {}
            }
        } else if k == "bar_style" {
            // 字符池风格（非色值；fx 皮肤同键同语义）
            if let Some(bs) = tuneux_mediax::BarStyle::from_name(v) {
                pal.bar_style = bs;
                any = true;
            }
        }
    }
    any.then_some(pal)
}

/// 从皮肤文本提取皮肤名（name= 行；缺省 None 由调用方兜底文件名）。
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

/// 调色板 → egui Visuals（每帧下发，主题切换即时生效）。
pub fn apply_visuals(ctx: &egui::Context, pal: &Palette) {
    // 亮/暗基底按底色亮度选（f2k = 亮色，dos/term = 暗色）
    let lum = (pal.bg.r() as u32 * 299 + pal.bg.g() as u32 * 587 + pal.bg.b() as u32 * 114) / 1000;
    let mut v = if lum > 140 {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };
    let square = CornerRadius::same(0);
    v.panel_fill = pal.bg;
    v.window_fill = pal.panel_bg;
    v.extreme_bg_color = pal.panel_bg;
    v.faint_bg_color = pal.panel_bg;
    v.override_text_color = Some(pal.fg);
    v.weak_text_color = Some(pal.fg_weak);
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.selection.bg_fill = pal.sel_bg.linear_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0_f32, pal.accent);
    // 扁平 TUI：去全部浮影；窗/弹层描边统一为调色板边框色
    v.window_shadow = egui::epaint::Shadow::NONE;
    v.popup_shadow = egui::epaint::Shadow::NONE;
    v.window_stroke = Stroke::new(1.0_f32, pal.border);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = square;
        w.bg_fill = Color32::TRANSPARENT;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke = Stroke::new(1.0_f32, pal.fg);
    }
    v.widgets.hovered.bg_fill = pal.sel_bg;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, pal.accent);
    v.widgets.active.bg_fill = pal.sel_bg;
    v.widgets.open.bg_fill = pal.panel_bg;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.fg_weak);
    ctx.set_visuals(v);
}

/// 内置中文字体：文泉驿微米黑等宽面（WenQuanYi Micro Hei Mono）。
pub static EMBEDDED_CJK_FONT: &[u8] = include_bytes!("../assets/wqy-microhei-mono.ttf");

/// 内置字体的显示名。
pub const EMBEDDED_FONT_NAME: &str = "文泉驿等宽微米黑";

/// 一个可用字体（显示名 + 字体文件字节）。
pub struct FontEntry {
    /// 显示名（内置名或文件名主干）。
    pub name: String,
    /// 字体文件内容。
    pub data: Vec<u8>,
}

/// 字体目录扫描位置：exe 同目录 fonts/（bundle 口径）+ 用户配置目录
/// fonts/（安装到只读位置时的用户扩展口径）。将来新增字体 = 丢一个
/// .ttf/.otf 进目录，菜单自动出现，无需改代码重编译。
fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.join("fonts"));
        }
    }
    if let Some(d) = dirs::data_dir() {
        dirs.push(d.join("tuneux-max").join("fonts"));
    }
    dirs
}

/// 字体目录：内置文泉驿恒在首位，随后是 fonts/ 目录扫描结果（按名排序）。
/// 损坏 / 不可读的文件静默跳过（字体缺失不致命，回退内置）。
pub fn font_catalog() -> Vec<FontEntry> {
    let mut out = vec![FontEntry {
        name: EMBEDDED_FONT_NAME.to_string(),
        data: EMBEDDED_CJK_FONT.to_vec(),
    }];
    let mut file_fonts: Vec<FontEntry> = Vec::new();
    for dir in font_dirs() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let is_font = p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "ttf" | "otf"));
            if !is_font {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if let Ok(data) = std::fs::read(&p) {
                file_fonts.push(FontEntry {
                    name: stem.to_string(),
                    data,
                });
            }
        }
    }
    file_fonts.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(file_fonts);
    out
}

/// 以给定字体数据构建字体定义（主字体 = data，内置面退居回退）。
pub fn build_fonts_with(data: &[u8]) -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert(
        "app-cjk".to_owned(),
        std::sync::Arc::new(egui::FontData::from_owned(data.to_vec())),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = defs.families.entry(family).or_default();
        list.retain(|n| n != "app-cjk" && n != "wqy-cjk");
        list.insert(0, "app-cjk".to_owned());
    }
    defs
}

/// 构建字体定义：文泉驿微米黑等宽面为两族**主字体**，内置面退居回退。
///
/// 主副次序是列对齐的关键：全部字形（拉丁 + CJK）统一出自文泉驿等宽面，
/// 其 CJK 字宽 = 2×拉丁字宽严格成立，按显示宽度补空格的列才能像素级对齐
/// （真 TUI 规整感）。若拉丁走 Hack、CJK 走回退，两面字宽比不是精确 2:1，
/// 混排行必然错位——spike 期实测踩坑，勿回退主副次序。
pub fn build_fonts() -> egui::FontDefinitions {
    build_fonts_with(EMBEDDED_CJK_FONT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置主题全部可解析。
    #[test]
    fn from_skin_parses_and_overlays() {
        let text = "name=测试皮肤\nbg=#0d1321\nfkey_num=#ffb000\nborder_type=double\n";
        let pal = from_skin(text).expect("应解析出调色板");
        assert_eq!(pal.bg, Color32::from_rgb(0x0d, 0x13, 0x21));
        assert_eq!(pal.accent, Color32::from_rgb(0xff, 0xb0, 0x00));
        assert_eq!(skin_name(text).as_deref(), Some("测试皮肤"));
        // 无任何有效键 → None
        assert!(from_skin("# 只有注释\n").is_none());
    }

    #[test]
    fn all_builtin_ids_resolve() {
        for id in ["dos", "term", "f2k", "f2k-light"] {
            assert!(builtin(id).is_some(), "内置主题 {id} 应可解析");
        }
        // 亮色皮肤亮度走 light 分支（apply_visuals 前置条件自检）
        let pal = builtin("f2k-light").unwrap();
        let lum =
            (pal.bg.r() as u32 * 299 + pal.bg.g() as u32 * 587 + pal.bg.b() as u32 * 114) / 1000;
        assert!(lum > 140, "f2k-light 底色应为亮色基底");
    }

    /// 未知 id 返回 None（调用方回退默认）。
    #[test]
    fn unknown_id_none() {
        assert!(builtin("no-such-theme").is_none());
    }

    /// 字体链：文泉驿必须是两族主字体（列对齐的根基）。
    #[test]
    fn font_chain_wqy_is_primary() {
        let defs = build_fonts();
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let list = &defs.families[&family];
            assert_eq!(
                list[0], "app-cjk",
                "{family:?} 主字体必须居首（列对齐根基）"
            );
        }
    }
}
