//! # tuneux-commonx —— 发行版通用机制层
//!
//! 与「音频领域」无关、与「UI 呈现」无关、≥2 个发行版共用的应用机制。
//! 三产品（tuneux / tuneux-fx / tuneux-max）共享；max 用 egui 自写呈现，
//! 但目录浏览机制、键位解析、配置路径、媒体键事件这三类通用机制直接复用。
//!
//! # 红线（红线以依赖图为事实，不靠文档自觉）
//!
//! - **零 workspace 内部依赖**：不依赖 tuneux-corex / mediax / pinx——
//!   平级叶子，防循环；仅 std + 平台 cfg 依赖（zbus / rdev / dirs /
//!   crossbeam-channel）。
//! - **零 UI 依赖**：ratatui / crossterm / egui / image 严禁引入。
//! - **零网络、零插件**。
//! - **事实与策略构造注入**：音频扩展名清单、MPRIS 服务名与 identity、
//!   keymap 合法动作与保留键集合，全部由调用方（发行版）作为参数传入；
//!   本层只提供机制，不持有产品策略。
//!
//! # 模块
//!
//! - [`keymap`]：键位描述解析（`parse_key_desc` / `canonical_key_name`）
//!   与 keymap 校验（保留键不可重映射的退出底线）；
//! - [`path`]：exe 同目录优先、系统配置目录回退的便携路径 + 可写性探测；
//! - [`float`]：浮点净化助手（NaN / Inf 回退默认，堵住「手写 nan 静音」）；
//! - `media_key`：系统媒体键事件 + Linux MPRIS / Windows rdev 后端
//!   （服务名 / identity 构造注入，macOS 预留形状；仅 media-key feature
//!   启用时编译，故不用链接语法）；
//! - [`fs_browser`]：目录浏览的数据 / 导航内核（`Entry` / `FsBrowser` /
//!   递归收集 / 符号链接环防护 / Windows 盘符与隐藏目录过滤）；
//!   「什么算可播」经 [`fs_browser::FsBrowserConfig`] 注入，不依赖内核。

#![deny(missing_docs)]
// 测试代码允许 unwrap：断言失败即测试失败，语义与生产路径不同
//（生产路径零 unwrap 由 workspace lints 强制）。
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod float;
pub mod fs_browser;
pub mod gauge;
pub mod i18n;
pub mod keymap;
// 媒体键（Linux MPRIS / Windows rdev）默认不启用：rdev 是系统级低层键盘钩子。
// 需要媒体键的产品经 `features = ["media-key"]` 显式启用。
#[cfg(feature = "media-key")]
pub mod media_key;
pub mod path;
pub mod ui;

pub use float::{sanitize_f32, sanitize_f64};
pub use fs_browser::FsBrowser;
pub use gauge::{gauge_cells, GaugeCell, GaugeKind, NeedlePhys, GAUGE_MIN_H, GAUGE_MIN_W};
pub use i18n::{build_i18n, scan_langs, I18n, LangTable};
pub use keymap::{canonical_key_name, parse_key_desc, validate_keymap};
pub use path::{is_writable, portable_path};
pub use ui::{FocusTarget, UiAction, UiMode, UiModel};
