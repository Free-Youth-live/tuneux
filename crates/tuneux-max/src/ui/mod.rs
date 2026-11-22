//! # 可复用 GUI 控件
//!
//! 参考共享 UI 骨架的分层：`dialog`（居中可拖弹窗）、`row`（稳定 Id 的
//! 可选行）。组件只依赖 egui 与主题令牌，不依赖产品状态。

pub mod dialog;
pub mod row;
