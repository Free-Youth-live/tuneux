//! # TUI 模块
//!
//! 终端界面相关代码，拆分自 main.rs 以减少单文件体积。
//!
//! - [`app`]：应用状态、按键处理、所有 `App` 方法
//! - [`render`]：渲染主入口 `draw` 与调度；具体绘制按职责拆分在
//!   `render/layout.rs`（布局度量）、`render/spectrum.rs`（电平/频谱）、
//!   `render/status.rs`（当前曲目/状态条/帮助栏）、
//!   `render/panels.rs`（浏览器/播放列表/歌词/封面）、
//!   `render/popup.rs`（"关于"弹窗）中

pub mod app;
pub mod render;
