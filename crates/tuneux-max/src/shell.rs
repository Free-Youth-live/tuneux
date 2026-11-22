//! # 应用壳：窗口规格、字体/主题安装、窗口图标与启动样板。
//!
//! 参考共享 UI 骨架的「纯构造 + 副作用薄包装」模式：`native_options` 是纯
//! 函数（可单测），`run` 只负责装字体、主题并创建 App。窗口图标必须显式
//! 注入 `ViewportBuilder::with_icon`——eframe/winit 不会读 exe 资源里的图标。

use eframe::egui;

use crate::{app, fonts, theme};

/// 窗口标题。
pub const APP_TITLE: &str = "tuneux-max";
/// 默认窗口内尺寸（逻辑像素）。
pub const DEFAULT_SIZE: [f32; 2] = [1280.0, 800.0];
/// 最小窗口内尺寸。
pub const MIN_SIZE: [f32; 2] = [960.0, 640.0];

/// 由固定规格构造 eframe 选项（纯构造，可单测）。
pub fn native_options() -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(APP_TITLE)
        .with_inner_size(DEFAULT_SIZE)
        .with_min_inner_size(MIN_SIZE);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    eframe::NativeOptions {
        viewport,
        ..Default::default()
    }
}

/// 解码随包 PNG 作为窗口图标；解码失败时返回 None（不阻塞启动）。
fn window_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../../../assets/tuneux-ico.png");
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(egui::IconData {
        rgba: image.clone().into_raw(),
        width: image.width(),
        height: image.height(),
    })
}

/// 启动应用：装字体 → 应用终端原生主题 → 创建 App。
pub fn run() -> eframe::Result<()> {
    let options = native_options();
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| {
            fonts::install(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app::MaxApp::new(cc)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_options_carry_title_and_sizes() {
        let options = native_options();
        assert_eq!(options.viewport.title.as_deref(), Some(APP_TITLE));
        assert_eq!(
            options.viewport.inner_size,
            Some(egui::vec2(DEFAULT_SIZE[0], DEFAULT_SIZE[1]))
        );
        assert_eq!(
            options.viewport.min_inner_size,
            Some(egui::vec2(MIN_SIZE[0], MIN_SIZE[1]))
        );
    }

    #[test]
    fn window_icon_decodes() {
        let icon = window_icon().expect("随包 PNG 图标应可解码");
        assert!(icon.width > 0 && icon.height > 0);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }
}
