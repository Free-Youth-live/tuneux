//! # 字体：内置 Sarasa Mono SC 同时挂 Proportional / Monospace 两族。
//!
//! 主副次序是列对齐关键：拉丁与 CJK 全部出自更纱等宽面，半角/全角宽度
//! 严格 2:1；若拉丁走别的面、CJK 走回退，混排行会错位。

use eframe::egui;
use std::sync::Arc;

/// 内置字体在 `FontDefinitions` 中的登记名。
const EMBEDDED_FONT_NAME: &str = "tuneux-max-sarasa-mono-sc";

/// 内置字体字节（Sarasa Mono SC Regular，SIL OFL 1.1，原样未修改）。
static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/SarasaMonoSC-Regular.ttf");

/// 安装字体到 egui 上下文。
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

/// 构造字体定义（纯构造，可单测）：内置字体置顶两族。
pub fn font_definitions() -> egui::FontDefinitions {
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert(
        EMBEDDED_FONT_NAME.to_owned(),
        Arc::new(egui::FontData::from_owned(EMBEDDED_FONT.to_vec())),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = defs.families.entry(family).or_default();
        list.retain(|name| name != EMBEDDED_FONT_NAME);
        list.insert(0, EMBEDDED_FONT_NAME.to_owned());
    }
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_font_bytes_present() {
        assert!(EMBEDDED_FONT.len() > 1_000_000, "内置字体应为完整字形文件");
    }

    #[test]
    fn font_definition_puts_embedded_first() {
        let defs = font_definitions();
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            let list = defs.families.get(&family).expect("两族都应存在");
            assert_eq!(list.first().map(String::as_str), Some(EMBEDDED_FONT_NAME));
        }
    }
}
