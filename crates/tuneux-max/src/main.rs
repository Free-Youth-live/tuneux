//! # tuneux-max 入口
//!
//! eframe 窗口创建、深色主题锁定、CJK 字体加载。
//!
//! 用法（站长裁决口径：max 仅以应用程序形态交付，命令行不是用户入口）：
//!   常规入口 = 双击启动（macOS .app / Windows exe / Linux 桌面集成）；
//!   交付二进制不编译任何命令行参数解析，双击/桌面入口是唯一启动面。

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
// Windows 双击启动不弹控制台黑窗（GUI 子系统）；CI / 重定向场景 stdout
// 句柄仍被继承——两种启动形态兼得。

// 测试模块放行 unwrap（规范：生产路径零 unwrap 机器强制，测试例外）。
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod app_v2;
mod config;
mod dock;
mod theme;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("tuneux-max")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "tuneux-max",
        options,
        Box::new(move |cc| {
            setup_style_and_fonts(&cc.egui_ctx);
            Ok(Box::new(app_v2::MaxAppV2::new(cc)))
        }),
    )
}

/// 深色主题锁定 + 中文字体链：内置文泉驿微米黑等宽面为主回退，
/// 系统 CJK 字体为二级回退（补内置面罕缺字形）。
///
/// 两个实锤坑（spike 阶段踩过，勿回退）：
/// 1. 光 set_visuals 不够——macOS 浅色外观下 egui 会跟随系统主题把面板
///    刷回浅色，必须 set_theme 锁死 Dark；
/// 2. egui 默认只带拉丁字体，不挂 CJK 回退则中文全是豆腐块；
///    Proportional 与 Monospace 两个族都要挂。
fn setup_style_and_fonts(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals(egui::Visuals::dark());
    // 字体定义收归 theme 层（Proportional/Monospace 两族同源构建）。
    ctx.set_fonts(theme::build_fonts());
}
