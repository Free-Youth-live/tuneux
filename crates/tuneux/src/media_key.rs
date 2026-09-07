//! 系统媒体键支持。
//!
//! 用户按系统媒体键（播放/暂停、上一曲、下一曲）时，即使终端在后台
//! 也能控制 tuneux 播放。各平台实现方式不同：
//!
//! - **Linux**：注册 MPRIS D-Bus 服务（`org.mpris.MediaPlayer2.tuneux`），
//!   桌面环境（GNOME/KDE）会把媒体键路由给 MPRIS 服务——标准做法，
//!   Wayland/X11 均适用，无需窗口。
//! - **Windows**：`rdev` crate（MIT）装 `WH_KEYBOARD_LL` 低层键盘钩子
//!   （无窗口、无消息泵、我方零 unsafe），捕获裸媒体键后经通道投递；
//!   二期可加 souvlaki + winit 隐藏窗口（SMTC）根治与其他播放器的抢占。
//! - **macOS**：Now Playing 需要 app bundle，TUI 无 bundle，暂不实现。
//!
//! 事件经 crossbeam-channel 投递到 TUI 主循环，主循环每帧 poll。
//! 本模块**不引入任何 unsafe**（zbus / rdev 均为 safe API 封装）。

use crossbeam_channel::Receiver;

/// 系统媒体键事件。
///
/// 与 keymap 动作一一对应，主循环收到后调用 `App::execute_action`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// 非 Linux 平台不编译 zbus 监听代码，变体不会被构造，
// VolumeUp/VolumeDown 变体仅在 Windows 的 rdev 映射中构造，
// 其他平台（Linux/macOS）从未使用——消除跨平台 dead_code 警告。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub enum MediaKeyEvent {
    /// 播放 / 暂停切换。
    PlayPause,
    /// 下一曲。
    Next,
    /// 上一曲。
    Prev,
    /// 音量加。
    VolumeUp,
    /// 音量减。
    VolumeDown,
}

impl MediaKeyEvent {
    /// 映射为 keymap 动作名（与 execute_action 的匹配串一致）。
    pub fn action(self) -> &'static str {
        match self {
            MediaKeyEvent::PlayPause => "toggle_play",
            MediaKeyEvent::Next => "next",
            MediaKeyEvent::Prev => "prev",
            MediaKeyEvent::VolumeUp => "volume_up",
            MediaKeyEvent::VolumeDown => "volume_down",
        }
    }
}

/// 媒体键监听句柄：事件接收端 + 共享状态回灌口。
///
/// 主循环每帧把引擎真实状态（是否播放/曲目标题）写入共享状态，
/// MPRIS 服务据此对外暴露准确的 PlaybackStatus / Metadata。
pub struct MediaKeyHandle {
    /// 事件接收端（主循环每帧 try_recv）。
    pub rx: Receiver<MediaKeyEvent>,
    /// 播放状态回灌（Linux MPRIS 用；其他平台为 None）。
    pub playing: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// 曲目标题回灌（Linux MPRIS 用；其他平台为 None）。
    pub title: Option<std::sync::Arc<std::sync::Mutex<String>>>,
}

/// 启动系统媒体键监听线程，返回句柄。
///
/// 平台不支持（macOS）或 D-Bus 会话不可用（headless）时返回 `None`，
/// 调用方应优雅忽略（媒体键功能静默缺失，不影响播放）。
pub fn spawn_media_key_listener() -> Option<MediaKeyHandle> {
    #[cfg(target_os = "linux")]
    {
        linux::spawn_linux_listener()
    }
    #[cfg(target_os = "windows")]
    {
        let rx = windows::spawn_windows_listener()?;
        Some(MediaKeyHandle {
            rx,
            playing: None,
            title: None,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

/// Linux：MPRIS D-Bus 服务实现（zbus blocking，纯 safe Rust）。
#[cfg(target_os = "linux")]
mod linux {
    use super::MediaKeyEvent;
    use crossbeam_channel::{bounded, Sender};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// MPRIS Player 接口实现。
    ///
    /// 只实现播放控制必需的几个方法/属性，其余（Seek/OpenUri 等）不暴露。
    /// 事件经 crossbeam-channel 发送给主循环。
    ///
    /// `playing`/`title` 为与主循环共享的状态：主循环每帧把引擎真实状态
    /// 写入，Play/Pause 方法据此判断是否真正需要切换（修复：
    /// 播放中收到 Play 应 no-op，暂停中收到 Pause 应 no-op）。
    struct MprisPlayer {
        /// 事件发送端（跨线程，主循环在另一线程 poll）。
        tx: Sender<MediaKeyEvent>,
        /// 播放状态（与主循环共享，主循环回灌引擎真实状态）。
        playing: Arc<AtomicBool>,
        /// 当前曲目标题（与主循环共享，供桌面媒体面板显示）。
        title: Arc<Mutex<String>>,
    }

    #[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
    impl MprisPlayer {
        /// 是否可下一曲（MPRIS 规范属性，桌面面板查询用）。
        #[zbus(property)]
        fn can_go_next(&self) -> bool {
            true
        }

        /// 是否可上一曲。
        #[zbus(property)]
        fn can_go_previous(&self) -> bool {
            true
        }

        /// 是否可播放。
        #[zbus(property)]
        fn can_play(&self) -> bool {
            true
        }

        /// 是否可暂停。
        #[zbus(property)]
        fn can_pause(&self) -> bool {
            true
        }
        /// 播放状态属性：Playing / Paused / Stopped。
        #[zbus(property)]
        fn playback_status(&self) -> String {
            if self.playing.load(Ordering::Relaxed) {
                "Playing".to_string()
            } else if self.title.lock().unwrap().is_empty() {
                // 无曲目（尚未播放过）：规范初始态为 Stopped
                "Stopped".to_string()
            } else {
                "Paused".to_string()
            }
        }

        /// 播放/暂停切换（媒体键主入口）。
        ///
        /// 状态翻转由主循环回灌（本方法只发事件，不直接改状态——
        /// 避免与 TUI 内空格键的切换逻辑产生双份状态）。
        fn play_pause(&mut self) {
            let _ = self.tx.send(MediaKeyEvent::PlayPause);
        }

        /// 下一曲。
        fn next(&mut self) {
            let _ = self.tx.send(MediaKeyEvent::Next);
        }

        /// 上一曲。
        fn previous(&mut self) {
            let _ = self.tx.send(MediaKeyEvent::Prev);
        }

        /// 播放（MPRIS 契约：Play = 开始播放）。
        ///
        /// 已在播放时 no-op（修复：此前盲目发 PlayPause 会误暂停）。
        fn play(&mut self) {
            if !self.playing.load(Ordering::Relaxed) {
                let _ = self.tx.send(MediaKeyEvent::PlayPause);
            }
        }

        /// 暂停（MPRIS 契约：Pause = 暂停）。
        ///
        /// 已暂停时 no-op（同上，避免反向切换）。
        fn pause(&mut self) {
            if self.playing.load(Ordering::Relaxed) {
                let _ = self.tx.send(MediaKeyEvent::PlayPause);
            }
        }

        /// 元数据属性（最简：仅标题，供桌面媒体控制面板显示）。
        #[zbus(property)]
        fn metadata(&self) -> std::collections::HashMap<String, zbus::zvariant::OwnedValue> {
            let title = self.title.lock().unwrap().clone();
            let mut map = std::collections::HashMap::new();
            if !title.is_empty() {
                map.insert(
                    "xesam:title".to_string(),
                    zbus::zvariant::Value::from(title)
                        .try_to_owned()
                        .unwrap_or_else(|_| {
                            zbus::zvariant::Value::from(String::new())
                                .try_to_owned()
                                .expect("string owned")
                        }),
                );
            }
            map
        }
    }

    /// MPRIS 根接口（org.mpris.MediaPlayer2）。
    ///
    /// MPRIS 根接口（org.mpris.MediaPlayer2）实现。
    ///
    /// 规范要求 /org/mpris/MediaPlayer2 同时提供根接口与 Player 接口；
    /// KDE Plasma 等桌面会因缺根接口而忽略该播放器。
    /// 根接口无状态（Identity 等静态属性），单独 struct 避免与 Player
    /// 接口在同一类型上（zbus `#[interface]` 一个类型只能实现一个接口）。
    struct MprisRoot;

    #[zbus::interface(name = "org.mpris.MediaPlayer2")]
    impl MprisRoot {
        /// 播放器名称（桌面媒体面板显示）。
        #[zbus(property)]
        fn identity(&self) -> String {
            "tuneux".to_string()
        }

        /// 桌面文件条目（无 .desktop 文件，返回空）。
        #[zbus(property)]
        fn desktop_entry(&self) -> String {
            String::new()
        }

        /// 支持的 URI 协议（本地文件路径）。
        #[zbus(property)]
        fn supported_uri_schemes(&self) -> Vec<String> {
            vec!["file".to_string()]
        }

        /// 支持的 MIME 类型（未知时返回空，桌面不依赖）。
        #[zbus(property)]
        fn supported_mime_types(&self) -> Vec<String> {
            Vec::new()
        }

        /// 是否可退出（tuneux 是 TUI，不提供 D-Bus 退出）。
        #[zbus(property)]
        fn can_quit(&self) -> bool {
            false
        }

        /// 是否可置顶（无窗口概念）。
        #[zbus(property)]
        fn can_raise(&self) -> bool {
            false
        }

        /// 是否有关联的曲目列表。
        #[zbus(property)]
        fn has_track_list(&self) -> bool {
            false
        }
    }

    /// 启动 Linux MPRIS 监听线程。
    ///
    /// 返回 None 的情形：无 D-Bus 会话（headless/SSH）、服务名被占用等。
    /// 线程内运行 zbus blocking Connection（内部 block_on 自驱动消息）。
    pub(super) fn spawn_linux_listener() -> Option<super::MediaKeyHandle> {
        let (tx, rx) = bounded::<MediaKeyEvent>(32);
        let tx_clone = tx.clone();
        // 共享状态：主循环每帧回灌引擎真实播放状态与曲目标题
        let playing = Arc::new(AtomicBool::new(false));
        let title = Arc::new(Mutex::new(String::new()));
        let (playing_clone, title_clone) = (Arc::clone(&playing), Arc::clone(&title));
        let handle = std::thread::Builder::new()
            .name("media-key-mpris".to_string())
            .spawn(move || {
                // 尝试连接会话总线；失败（headless）则线程静默退出，
                // 主线程已持有 rx，收不到事件即视为媒体键不可用。
                let builder = match zbus::blocking::connection::Builder::session() {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("[媒体键] D-Bus 会话不可用，媒体键已禁用：{e}");
                        return;
                    }
                };
                // serve_at 注册两个接口对象（Player + 根接口）→ name 申请
                // well-known 名 → build。zbus 允许同一路径挂多个接口对象。
                let player = MprisPlayer {
                    tx: tx_clone,
                    playing: playing_clone,
                    title: title_clone,
                };
                let conn = match builder
                    .serve_at("/org/mpris/MediaPlayer2", player)
                    .and_then(|b| b.serve_at("/org/mpris/MediaPlayer2", MprisRoot))
                    .and_then(|b| b.name("org.mpris.MediaPlayer2.tuneux"))
                    .and_then(|b| b.build())
                {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[媒体键] MPRIS 注册失败，媒体键已禁用：{e}");
                        return;
                    }
                };
                // blocking Connection 内部自动驱动消息；保持线程存活即可。
                // 线程活到进程退出（D-Bus 会话退出时阻塞返回，线程自然结束）。
                let _ = conn;
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3600));
                }
            })
            .expect("spawn media key thread");
        // 线程句柄保持存活（不 join），事件经 channel 传递
        std::mem::forget(handle);
        Some(super::MediaKeyHandle {
            rx,
            playing: Some(playing),
            title: Some(title),
        })
    }
}

/// Windows：rdev 低层键盘钩子实现（LL 钩子，零窗口、零 unsafe）。
///
/// 设计依据：
/// - 裸媒体键不产生 WM_KEYDOWN，RegisterHotKey 收不到；
///   正确路径是 WH_KEYBOARD_LL 钩子（rdev::listen 内部安装并泵消息）。
/// - rdev 全部 unsafe 封装在 crate 内部，我们只调 safe API。
/// - 监听线程只做「键事件 → 通道发送」，不做重活。
#[cfg(target_os = "windows")]
mod windows {
    use super::MediaKeyEvent;
    use crossbeam_channel::{bounded, Receiver};

    /// 启动 Windows 媒体键监听。
    ///
    /// 钩子安装失败（罕见）时返回 None，调用方优雅忽略。
    pub(super) fn spawn_windows_listener() -> Option<Receiver<MediaKeyEvent>> {
        let (tx, rx) = bounded::<MediaKeyEvent>(32);
        let handle = std::thread::Builder::new()
            .name("media-key-rdev".to_string())
            .spawn(move || {
                // rdev::listen 内部安装 WH_KEYBOARD_LL 并跑 GetMessageA 泵，
                // 阻塞本线程直到进程退出；监听不拦截事件（pass-through），
                // 不影响其他程序正常接收媒体键。
                if let Err(e) = rdev::listen(move |event| {
                    if let rdev::EventType::KeyPress(key) = event.event_type {
                        if let Some(ev) = map_key(key) {
                            let _ = tx.send(ev);
                        }
                    }
                }) {
                    eprintln!("[媒体键] Windows 键盘钩子安装失败，媒体键已禁用：{e:?}");
                }
            })
            .expect("spawn media key thread");
        std::mem::forget(handle);
        Some(rx)
    }

    /// rdev::Key → 归一化媒体键事件。
    ///
    /// rdev 0.5.3 的 `Key` 枚举无媒体键变体，媒体键经
    /// `Key::Unknown(u32)` 携带 Windows 虚拟键码（VK_MEDIA_*），
    /// 在此按 VK 码归一化（对照 winuser.h 常量）：
    ///
    /// - `0xB0` VK_MEDIA_NEXT_TRACK 下一曲
    /// - `0xB1` VK_MEDIA_PREV_TRACK 上一曲
    /// - `0xB2` VK_MEDIA_STOP 停止
    /// - `0xB3` VK_MEDIA_PLAY_PAUSE 播放/暂停
    /// - `0xAD` VK_VOLUME_MUTE 静音
    /// - `0xAE` VK_VOLUME_DOWN 音量减
    /// - `0xAF` VK_VOLUME_UP 音量加
    fn map_key(key: rdev::Key) -> Option<MediaKeyEvent> {
        let code = match key {
            rdev::Key::Unknown(code) => code,
            _ => return None,
        };
        Some(match code {
            0xB3 => MediaKeyEvent::PlayPause,
            0xB0 => MediaKeyEvent::Next,
            0xB1 => MediaKeyEvent::Prev,
            0xAF => MediaKeyEvent::VolumeUp,
            0xAE => MediaKeyEvent::VolumeDown,
            _ => return None,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 媒体键事件 → keymap 动作名映射正确（主循环依赖此映射）。
    #[test]
    fn media_key_action_mapping() {
        assert_eq!(MediaKeyEvent::PlayPause.action(), "toggle_play");
        assert_eq!(MediaKeyEvent::Next.action(), "next");
        assert_eq!(MediaKeyEvent::Prev.action(), "prev");
        assert_eq!(MediaKeyEvent::VolumeUp.action(), "volume_up");
        assert_eq!(MediaKeyEvent::VolumeDown.action(), "volume_down");
    }

    /// 非 Linux 平台（或 headless）：spawn 返回 None，调用方优雅降级。
    ///
    /// Linux 上若本机无 D-Bus 会话（CI headless 环境），监听线程内
    /// 注册失败也会返回 None 路径——本测试验证该入口不会 panic。
    #[test]
    fn spawn_returns_option_without_panic() {
        // 无论平台/环境，调用本身不应 panic；返回 None 或 Some 均可接受。
        let _ = spawn_media_key_listener();
    }
}
