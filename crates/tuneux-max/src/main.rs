//! # tuneux-max 入口
//!
//! GUI 产品：只以应用程序形态交付（macOS .app / Windows GUI 子系统 exe），
//! 交付二进制不解析任何命令行参数。
//!
//! 分层：`shell`（窗口/字体/主题引导）→ `app`（状态与交互）；
//! `theme` 只提供终端原生配色令牌，`ui` 提供可复用控件。

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![cfg_attr(test, allow(clippy::unwrap_used))]

mod app;
mod command;
mod config;
mod fonts;
mod i18n;
mod playlist_rows;
mod plugins;
mod shell;
mod theme;
mod ui;

fn main() -> eframe::Result<()> {
    shell::run()
}
