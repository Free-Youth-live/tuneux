//! # TUI 模块
//!
//! 终端界面：`app` 持有状态与按键分派，`render` 负责绘制。
//! 布局自上而下：菜单栏 → 工作区 → 状态栏 → 功能键栏（参照 foobar2000 组织）。

pub mod app;
pub mod render;
pub mod theme;
