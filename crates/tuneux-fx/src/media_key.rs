//! 系统媒体键（fx 接线）。
//!
//! 实现已下沉 `tuneux-commonx`（Linux MPRIS / Windows rdev 后端、
//! macOS 形状预留）；本模块只做产品策略注入——MPRIS well-known 服务名
//! 与播放器 identity 是 fx 自己的策略，与其他产品隔离。

pub use tuneux_commonx::media_key::{MediaKeyEvent, MediaKeyHandle};

/// 启动 fx 系统媒体键监听线程。
///
/// 平台不支持（macOS）或 D-Bus 会话不可用（headless）时返回 `None`，
/// 调用方优雅忽略（媒体键功能静默缺失，不影响播放）。
pub fn spawn_media_key_listener() -> Option<MediaKeyHandle> {
    tuneux_commonx::media_key::spawn_media_key_listener(
        "org.mpris.MediaPlayer2.tuneuxfx",
        "tuneux-fx",
    )
}
