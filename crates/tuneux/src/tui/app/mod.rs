//! # 应用状态与按键处理
//!
//! 持有 TUI 需要的所有可变状态：文件浏览器、音频引擎、播放列表、
//! 频谱数据、搜索、错误显示等。所有键盘输入也在此处理。
//!
//! 本目录按领域拆分为子模块（每个子模块一个 `impl App` 扩展块）：
//! - [`media`]：元数据缓存、歌词加载、封面解码
//! - [`cue`]：CUE 分轨解析与展开
//! - [`search`]：搜索与过滤（含 SearchTarget 枚举）
//! - [`playback`]：播放控制（engine 交互）
//! - [`actions`]：列表与浏览器操作
//! - [`keys`]：按键处理（keymap、动作分派）

mod actions;
mod cue;
mod keys;
mod media;
mod playback;
mod search;

use std::path::PathBuf;

use crossbeam_channel;
use image;

use crate::config::{Config, LeftPanel, LyricsMode, PlaylistState, SpectrumMode};
use crate::fs_browser;
use crate::playlist;
use tuneux_corex as audio;
use tuneux_mediax::lyrics;
use tuneux_mediax::metadata;

// 搜索目标重导出：render/layout.rs 依赖 `crate::tui::app::SearchTarget` 路径。
pub use self::search::SearchTarget;

/// 后台目录加入的结果消息：目录 + 构建好的条目 + 新探测的元数据
/// （封面字节已剥离，仅缓存未命中的项）。
#[allow(clippy::type_complexity)]
pub type DirAddResult = (
    PathBuf,
    Vec<playlist::PlaylistItem>,
    Vec<(PathBuf, tuneux_mediax::metadata::TrackMetadata)>,
);

/// 应用运行时状态。
///
/// 集中持有 TUI 需要的所有可变状态：文件浏览器与音频引擎。
pub struct App {
    /// 文件浏览器（浏览目录、选中音乐）。
    pub browser: fs_browser::FsBrowser,
    /// 目录异步载入发送端：后台线程读目录后发回 (代次, 目录, 结果)。
    pub dir_load_tx:
        crossbeam_channel::Sender<(u64, PathBuf, Result<Vec<fs_browser::Entry>, String>)>,
    /// 目录异步载入接收端。
    pub dir_load_rx:
        crossbeam_channel::Receiver<(u64, PathBuf, Result<Vec<fs_browser::Entry>, String>)>,
    /// 目录载入代次：每次导航 +1，用于丢弃陈旧结果（快速连按只应用最新）。
    pub dir_load_gen: u64,
    /// 浏览器搜索的异步收集：后台线程递归收集目录树后发回 (代次, 条目, 是否截断)。
    pub search_load_tx: crossbeam_channel::Sender<(u64, Vec<fs_browser::Entry>, bool)>,
    /// 浏览器搜索收集接收端。
    pub search_load_rx: crossbeam_channel::Receiver<(u64, Vec<fs_browser::Entry>, bool)>,
    /// 搜索收集代次：每次进入搜索 +1，用于丢弃陈旧收集（重进搜索 / 切目录后）。
    pub search_load_gen: u64,
    /// 目录递归加入的异步构建：后台线程收集 + CUE 展开 + 元数据探测后
    /// 发回 [`DirAddResult`]。加入是累积语义，多批次全部合入
    /// （与导航的「最新生效」不同，不用代次丢弃）。
    pub add_load_tx: crossbeam_channel::Sender<DirAddResult>,
    /// 目录加入接收端。
    pub add_load_rx: crossbeam_channel::Receiver<DirAddResult>,
    /// 音频引擎（播放控制、状态读取）。Option 因为引擎初始化可能失败，
    /// 失败时退化为"只能浏览不能播放"，不让整个程序崩溃。
    pub engine: Option<audio::Engine>,
    /// 当前播放曲目的元数据（标题/艺术家/专辑/技术参数）。
    /// 未播放时为 None。播放新曲时由 Enter 触发更新。
    pub current_metadata: Option<metadata::TrackMetadata>,
    /// 元数据缓存：path → 已提取的 TrackMetadata。
    ///
    /// 用途：避免对同一文件重复 probe。`from_file` 内部会读文件头取标签
    /// 和时长，对一首 50MB 的 FLAC 大概要几毫秒。用户反复按 Enter 切歌
    /// 切回同一首时直接走缓存。
    ///
    /// 不做失效处理：用户播放期间不会修改文件（编辑器的 lock 也不可能命中
    /// 音乐文件）。如果以后有"重新读取文件元数据"需求再补 mtime 校验。
    pub metadata_cache: std::collections::HashMap<PathBuf, metadata::TrackMetadata>,
    /// ReplayGain 增益缓存：path → 整曲增益（dB）。
    /// 首次播放无缓存（增益 1.0），播放中分析完成后缓存，下次播放生效。
    pub replay_gain_cache: std::collections::BTreeMap<PathBuf, f64>,
    /// 播放列表（按插入序存，显示时按专辑-曲序排序）
    pub playlist: playlist::Playlist,
    /// 播放状态（播放列表 + 每首进度），退出时保存到独立文件 playlist.toml。
    pub playlist_state: PlaylistState,
    /// 当前焦点面板（Tab 切换）
    pub focus: playlist::Panel,
    /// 左侧面板状态（`b` 键浏览器 / `c` 键封面，二者互斥；隐藏时给列表更多空间）。
    /// 默认 Hidden（列表为主），按 b 在左侧弹出浏览器，按 c 弹出专辑封面。
    pub left_panel: LeftPanel,
    /// 频谱显示模式（`v` 键切换：关 → 半屏 → 全屏 → 示波器 → 关）。
    /// 默认 Hidden——开屏给完整的播放列表，用户想看的时再 v 键唤出。
    pub spectrum_mode: SpectrumMode,
    /// 播放介质风格（`m` 键循环：无 → 4 磁带 → 4 黑胶 → 无）。
    /// 只作用于声音（corex DSP 修饰），不改变界面布局。
    pub playback_medium: audio::PlaybackMedium,
    /// 歌词显示模式（`l` 键切换：隐藏 ↔ 显示）。
    /// 默认 Hidden，可与频谱共存（歌词控制横向分屏，频谱控制纵向分屏）。
    pub lyrics_mode: LyricsMode,
    /// 当前曲目的歌词。优先同目录同名 .lrc，无则用内嵌歌词标签。无则 None。
    pub current_lyrics: Option<lyrics::Lyrics>,
    /// 封面图解码缓存：(track_path, decoded DynamicImage)。
    /// 切歌时清空；同一首歌内复用——避免每帧重新解码。
    pub cover_cache: Option<(PathBuf, image::DynamicImage)>,
    /// 当前封面面板缩略图缓存：(path, 请求宽, 请求高, 实际宽, 实际高, RGBA)。
    /// 面板尺寸不变时复用，避免每帧 resize_exact + to_rgba8（回灌 fx）。
    pub cover_thumb: Option<(PathBuf, u32, u32, u32, u32, image::RgbaImage)>,
    /// 封面解码负缓存：记录解码失败的曲目路径，命中后不再重试、不刷屏。
    pub cover_failed_path: Option<PathBuf>,
    /// 无封面负标记：已确认标签里没有封面的文件路径，不再重复整文件标签探测。
    pub coverless_files: std::collections::HashSet<PathBuf>,
    /// 封面浏览：选中的专辑索引（0 起）。
    pub cover_browser_sel: usize,
    /// 封面浏览：网格滚动偏移（第一个可见专辑的索引）。
    pub cover_browser_scroll: usize,
    /// 封面浏览：当前可视格数（渲染时写入，按键层据此让选中滚入可视窗）。
    pub cover_browser_visible: usize,
    /// 封面网格缩略图缓存：(路径, 目标宽, 目标高) → RGBA 缩略图（None=无封面/解码失败）。
    /// 封面浏览按需解码一次，避免每帧重复 from_file + 解码 + 缩放。
    pub cover_thumb_cache: std::collections::HashMap<(PathBuf, u32, u32), Option<image::RgbaImage>>,
    /// 当前正在播放的文件路径（items 索引对应的 .path）。
    /// 用于：
    /// 1. **记住播放进度**：切曲前/退出前用这个 path 存 position
    /// 2. **接着上次听**：重新播放同一文件时查表 resume
    pub current_path: Option<PathBuf>,
    /// 搜索模式：true 时输入框收字符、键全被吃，不再走全局快捷键。
    pub search_mode: bool,
    /// 搜索关键词（空 = 不过滤）。
    pub search_query: String,
    /// 搜索目标面板（播放列表 / 文件浏览器）。仅 search_mode 为 true 时有意义。
    pub search_target: SearchTarget,
    /// 最近一次错误（解码/打开失败），由 engine.take_last_error() 喂入。
    /// 显示 N 秒后自动清除（防止错误信息永久占屏）。
    pub last_error: Option<String>,
    /// last_error 的显示起始时间，用于 5 秒后自动清。
    pub last_error_at: Option<std::time::Instant>,
    /// 连续播放失败计数：列表全损坏时避免自动跳曲无限循环（达到阈值即停止）。
    pub consecutive_failures: u32,
    /// 切曲生效守卫帧数：Play/Seek 异步生效期间主循环不做 CUE 终点判定。
    pub switch_guard: u32,
    /// 渲染帧计数（u64 取模防溢出）。每帧 draw 时 +1，
    /// 传给电平表的乱码生成器，让柱图字符随帧变化（看起来"活"）。
    pub frame_tick: u64,
    /// 频谱峰值保持状态机（频段维度，0.0-1.0），用于"峰值保持白帽"：
    /// 绿柱实时跟随能量，白帽从峰值缓慢下落。RefCell 内部可变，便于渲染侧持 `&App` 更新。
    pub spectrum_peaks: std::cell::RefCell<audio::spectrum::SpectrumPeakHold>,
    /// "关于"弹窗是否可见（`?` 键切换，任意键关闭）。
    /// 显示版本号、开源声明、版权声明，覆盖在主界面之上。
    pub about_visible: bool,
    /// 清空播放列表的确认状态：true 时再按 x 才真正清空（防误触）。
    pub pending_clear: bool,
    /// 清空确认的发起时刻：与提示同寿命（5 秒过期），防止提示消失后误清空。
    pub pending_clear_at: Option<std::time::Instant>,
}

impl App {
    /// 引擎启动失败时 engine 为 None，程序仍可浏览。
    pub fn new(config: &Config) -> Self {
        let initial = config.effective_last_dir();
        // 解析配置里的介质风格（同源解析一次，供引擎下发与 App 字段共用）。
        let medium: audio::PlaybackMedium = config
            .playback_medium
            .parse()
            .unwrap_or(audio::PlaybackMedium::None);
        // 启动音频引擎；失败不阻塞程序，错误经 last_error 通道显示
        //（避免 raw 模式下 eprintln 花屏）。
        let (engine, engine_init_error) = match audio::Engine::new(config.volume) {
            Ok(e) => (Some(e), None),
            Err(e) => (
                None,
                Some(format!("音频引擎初始化失败，播放功能不可用：{e}")),
            ),
        };
        let engine_init_error_at = engine_init_error
            .as_ref()
            .map(|_| std::time::Instant::now());
        // 启动即下发介质风格，避免配置与声音不一致（介质纯声音修饰、无画面）。
        // 一并下发 ReplayGain 开关（默认关）。
        if let Some(e) = &engine {
            e.set_medium(medium);
            e.set_replay_gain_enabled(config.replay_gain);
        }
        // 加载播放状态（独立文件 playlist.toml）
        let playlist_state = crate::config::load_playlist_state();

        let mut playlist = playlist::Playlist::new();
        playlist.set_shuffle(config.shuffle);
        playlist.set_view(config.playlist_view);

        // 恢复上次播放列表，并预提取 metadata（使组头歌手/时长立即正确）。
        // 失效路径（文件已删除/移动）自动跳过。
        let mut metadata_cache = std::collections::HashMap::new();
        for item in &playlist_state.items {
            if !item.path.exists() {
                continue;
            }
            playlist.add(item.clone());
            if !metadata_cache.contains_key(&item.path) {
                let md = metadata::TrackMetadata::from_file(&item.path);
                metadata_cache.insert(item.path.clone(), md);
            }
        }

        // 自动选中上次播放的歌曲（高亮落在它上，用户按 Enter 才播放）。
        if let Some(current_path) = &playlist_state.current {
            if let Some(index) = playlist
                .items()
                .iter()
                .position(|it| &it.path == current_path)
            {
                playlist.set_selected(index);
                // 展开其所在专辑，保证选中项在 ByAlbum 视图下可见。
                if let Some(album) = playlist.items().get(index).and_then(|it| it.album.clone()) {
                    playlist.expand_album(&album);
                }
            }
        }

        // 目录异步载入通道：后台线程读目录，主循环轮询后应用（大目录不卡 UI）。
        let (dir_load_tx, dir_load_rx) = crossbeam_channel::unbounded();
        // 浏览器搜索收集与目录加入的后台通道（同机制，见对应字段说明）。
        let (search_load_tx, search_load_rx) = crossbeam_channel::unbounded();
        let (add_load_tx, add_load_rx) = crossbeam_channel::unbounded();

        Self {
            browser: fs_browser::FsBrowser::open(&initial),
            dir_load_tx,
            dir_load_rx,
            dir_load_gen: 0,
            search_load_tx,
            search_load_rx,
            search_load_gen: 0,
            add_load_tx,
            add_load_rx,
            engine,
            current_metadata: None,
            metadata_cache,
            // ReplayGain 缓存：从 playlist.toml 载入上次会话的测量结果
            //（路径 → dB），本次会话内新测量结果经 save_playlist_to_state 回写。
            replay_gain_cache: playlist_state.replay_gain.clone(),
            playlist,
            playlist_state,
            focus: playlist::Panel::Playlist,
            left_panel: config.left_panel,
            spectrum_mode: config.spectrum_mode,
            playback_medium: medium,
            lyrics_mode: config.lyrics_mode,
            current_lyrics: None,
            cover_cache: None,
            cover_thumb: None,
            cover_failed_path: None,
            coverless_files: std::collections::HashSet::new(),
            cover_browser_sel: 0,
            cover_browser_scroll: 0,
            cover_browser_visible: 0,
            cover_thumb_cache: std::collections::HashMap::new(),
            current_path: None,
            search_mode: false,
            search_query: String::new(),
            search_target: SearchTarget::Playlist,
            last_error: engine_init_error,
            last_error_at: engine_init_error_at,
            consecutive_failures: 0,
            switch_guard: 0,
            frame_tick: 0,
            spectrum_peaks: std::cell::RefCell::new(audio::spectrum::SpectrumPeakHold::default()),
            about_visible: false,
            pending_clear: false,
            pending_clear_at: None,
        }
    }

    /// 把当前播放列表与当前曲目写回播放状态（退出时调用，下次启动恢复）。
    pub fn save_playlist_to_state(&mut self) {
        self.playlist_state.items = self.playlist.items().to_vec();
        self.playlist_state.current = self.current_path.clone();
        // ReplayGain 测量缓存同步回持久化状态（随 playlist.toml 落盘，
        // 下次启动经 App::new 重新载入，跨会话免重复分析）。
        // 修剪：只保留当前列表曲目或文件仍在磁盘的（测量代价高，重加入免重测）。
        self.replay_gain_cache
            .retain(|k, _| self.playlist.items().iter().any(|it| it.path == *k) || k.exists());
        self.playlist_state.replay_gain = self.replay_gain_cache.clone();
        // 断点 positions 按当前列表修剪：只保留仍在播放列表中的曲目，
        // 避免已删除/清空曲目的进度残留、文件无界增长。
        let keep: std::collections::HashSet<&std::path::Path> = self
            .playlist
            .items()
            .iter()
            .map(|it| it.path.as_path())
            .collect();
        self.playlist_state
            .positions
            .retain(|k, _| keep.contains(k.as_path()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::fs;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn app_opens_browser_at_config_dir() {
        let tmp = std::env::temp_dir().join("tuneux_app_init_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("test.mp3")).unwrap();

        let config = Config {
            last_dir: Some(tmp.clone()),
            ..Config::default()
        };
        let app = App::new(&config);
        let expected = tmp.canonicalize().unwrap_or_else(|_| tmp.clone());
        assert_eq!(app.browser.cwd(), expected, "浏览器应定位在配置目录");
        assert_eq!(app.browser.entries().len(), 1, "应看到 test.mp3");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 模拟主循环轮询：等待异步目录载入结果并应用到浏览器（测试用）。
    /// 用阻塞 recv_timeout 而非 try_recv，保证后台线程已把结果送达（确定性）。
    fn drain_dir_load(app: &mut App) {
        if let Ok((gen, resolved, result)) = app
            .dir_load_rx
            .recv_timeout(std::time::Duration::from_secs(1))
        {
            if gen + 1 == app.dir_load_gen {
                if let Ok(entries) = result {
                    app.browser.apply_loaded(resolved, entries);
                }
            }
        }
    }

    /// 模拟主循环轮询搜索收集结果（测试用，阻塞等待保证确定性）。
    fn drain_search_load(app: &mut App) {
        if let Ok((gen, entries, truncated)) = app
            .search_load_rx
            .recv_timeout(std::time::Duration::from_secs(1))
        {
            if gen + 1 == app.search_load_gen {
                app.browser.apply_search_collected(entries, truncated);
            }
        }
    }

    /// 模拟主循环轮询目录加入结果（测试用，阻塞等待保证确定性）。
    fn drain_add_load(app: &mut App, config: &mut Config) {
        if let Ok((dir, items, mds)) = app
            .add_load_rx
            .recv_timeout(std::time::Duration::from_secs(1))
        {
            app.apply_dir_add(dir, items, mds, config);
        }
    }

    #[test]
    fn app_key_navigation() {
        let tmp = std::env::temp_dir().join("tuneux_app_nav_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("album1")).unwrap();
        fs::create_dir_all(tmp.join("album2")).unwrap();
        fs::File::create(tmp.join("album1/song1.mp3")).unwrap();
        fs::File::create(tmp.join("solo.wav")).unwrap();

        let mut config = Config {
            last_dir: Some(tmp.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config);
        assert!(app.handle_key(key(KeyCode::Char('b')), &mut config));
        assert_eq!(app.focus, playlist::Panel::Browser);
        assert_eq!(app.browser.entries().len(), 3);

        assert!(app.handle_key(key(KeyCode::Down), &mut config));
        assert_eq!(app.browser.selected(), 1);
        assert!(app.handle_key(key(KeyCode::Enter), &mut config));
        drain_dir_load(&mut app);
        assert_eq!(app.browser.entries().len(), 0, "album2 应为空");

        assert!(app.handle_key(key(KeyCode::Backspace), &mut config));
        drain_dir_load(&mut app);
        assert_eq!(app.browser.entries().len(), 3);

        assert!(app.handle_key(key(KeyCode::Home), &mut config));
        assert_eq!(app.browser.selected(), 0);
        assert!(app.handle_key(key(KeyCode::Enter), &mut config));
        drain_dir_load(&mut app);
        assert_eq!(app.browser.entries().len(), 1);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn app_quit_keys() {
        let mut config = Config::default();
        let mut app = App::new(&config);
        assert!(
            !app.handle_key(key(KeyCode::Char('q')), &mut config),
            "q 应请求退出"
        );
        assert!(
            !app.handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                &mut config
            ),
            "Ctrl+C 应请求退出"
        );
        assert!(
            app.handle_key(key(KeyCode::Down), &mut config),
            "↓ 不应退出"
        );
    }

    #[test]
    fn custom_keymap_overrides_default() {
        let mut config = Config::default();
        // 把"下一曲"映射到 x 键：按 x 应走 next，而非默认的"清空确认"
        config.keymap.insert("next".to_string(), "x".to_string());
        let mut app = App::new(&config);

        assert!(app.handle_key(key(KeyCode::Char('x')), &mut config));
        assert!(!app.pending_clear, "x 已被映射为 next，不应进入清空确认");
        assert_eq!(app.focus, playlist::Panel::Playlist);
    }

    #[test]
    fn browser_enter_adds_to_playlist() {
        let tmp = std::env::temp_dir().join("tuneux_enter_playlist_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("song1.mp3")).unwrap();
        fs::File::create(tmp.join("song2.mp3")).unwrap();

        let mut config = Config {
            last_dir: Some(tmp.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config);
        assert!(app.handle_key(key(KeyCode::Char('b')), &mut config));
        assert_eq!(app.focus, playlist::Panel::Browser);
        assert!(app.playlist.is_empty(), "初始列表应为空");

        assert!(app.handle_key(key(KeyCode::Enter), &mut config));
        assert_eq!(app.playlist.items().len(), 1, "Enter 后应加入 1 首");
        assert_eq!(app.playlist.current_index(), Some(0));

        assert!(app.handle_key(key(KeyCode::Down), &mut config));
        assert!(app.handle_key(key(KeyCode::Enter), &mut config));
        assert_eq!(app.playlist.items().len(), 2, "应累计 2 首");
        assert_eq!(app.playlist.current_index(), Some(1));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn add_directory_sorts_by_album_track() {
        let tmp = std::env::temp_dir().join("tuneux_add_dir_sort_test");
        let _ = fs::remove_dir_all(&tmp);
        let album_a = tmp.join("合集/albumA");
        let album_b = tmp.join("合集/albumB");
        fs::create_dir_all(&album_a).unwrap();
        fs::create_dir_all(&album_b).unwrap();
        fs::File::create(album_a.join("02 - 二.mp3")).unwrap();
        fs::File::create(album_a.join("01 - 一.mp3")).unwrap();
        fs::File::create(album_b.join("01 - 一.mp3")).unwrap();
        // canonicalize 得到真实路径（macOS 上 /var → /private/var），
        // 与浏览器递归收集时产生的路径保持一致，确保 metadata_cache 命中。
        let album_a = album_a.canonicalize().unwrap();
        let album_b = album_b.canonicalize().unwrap();

        let mut config = Config {
            last_dir: Some(tmp.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config);

        // 注入假元数据：专辑 A 曲序 1/2，专辑 B 曲序 1
        app.metadata_cache.insert(
            album_a.join("01 - 一.mp3"),
            metadata::TrackMetadata {
                album: Some("A".to_string()),
                track_number: Some(1),
                ..Default::default()
            },
        );
        app.metadata_cache.insert(
            album_a.join("02 - 二.mp3"),
            metadata::TrackMetadata {
                album: Some("A".to_string()),
                track_number: Some(2),
                ..Default::default()
            },
        );
        app.metadata_cache.insert(
            album_b.join("01 - 一.mp3"),
            metadata::TrackMetadata {
                album: Some("B".to_string()),
                track_number: Some(1),
                ..Default::default()
            },
        );

        // 选中"合集"目录并添加（浏览器 entries[0] = 合集）；
        // 目录加入已异步化：模拟主循环轮询合入结果。
        assert_eq!(app.browser.entries()[0].name(), "合集");
        app.add_current_browser_to_playlist(&mut config);
        drain_add_load(&mut app, &mut config);

        // 期望：3 个条目，按专辑-曲序 A/01, A/02, B/01
        assert_eq!(app.playlist.items().len(), 3);
        let seq: Vec<(String, u32)> = app
            .playlist
            .items()
            .iter()
            .map(|i| {
                (
                    i.album.clone().unwrap_or_default(),
                    i.track_number.unwrap_or(0),
                )
            })
            .collect();
        assert_eq!(
            seq,
            vec![
                ("A".to_string(), 1),
                ("A".to_string(), 2),
                ("B".to_string(), 1)
            ]
        );
        // 自动播放/选中第一首 = 排序后的第一首（索引 0）
        assert_eq!(app.playlist.current_index(), Some(0));
        assert_eq!(app.playlist.selected_track(), Some(0));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn left_panel_browser_cover_mutually_exclusive() {
        let mut config = Config::default();
        let mut app = App::new(&config);
        assert_eq!(app.left_panel, LeftPanel::Hidden);

        // b 开浏览器
        assert!(app.handle_key(key(KeyCode::Char('b')), &mut config));
        assert_eq!(app.left_panel, LeftPanel::Browser);
        assert_eq!(app.focus, playlist::Panel::Browser);

        // c 开封面：互斥，浏览器被关掉，焦点回到列表
        assert!(app.handle_key(key(KeyCode::Char('c')), &mut config));
        assert_eq!(app.left_panel, LeftPanel::Cover);
        assert_eq!(app.focus, playlist::Panel::Playlist);

        // 再 c 切封面浏览：仍是封面面板，焦点保持在列表
        assert!(app.handle_key(key(KeyCode::Char('c')), &mut config));
        assert_eq!(app.left_panel, LeftPanel::CoverBrowser);
        assert_eq!(app.focus, playlist::Panel::Playlist);

        // 第三次 c 关封面
        assert!(app.handle_key(key(KeyCode::Char('c')), &mut config));
        assert_eq!(app.left_panel, LeftPanel::Hidden);

        // b 再开浏览器，再 b 关闭
        assert!(app.handle_key(key(KeyCode::Char('b')), &mut config));
        assert_eq!(app.left_panel, LeftPanel::Browser);
        assert!(app.handle_key(key(KeyCode::Char('b')), &mut config));
        assert_eq!(app.left_panel, LeftPanel::Hidden);
        assert_eq!(app.focus, playlist::Panel::Playlist);
    }

    #[test]
    fn browser_search_flow() {
        let tmp = std::env::temp_dir().join("tuneux_browser_search_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("子目录")).unwrap();
        fs::File::create(tmp.join("稻香.mp3")).unwrap();
        fs::File::create(tmp.join("晴天.mp3")).unwrap();
        fs::File::create(tmp.join("子目录/稻香现场版.mp3")).unwrap();

        let mut config = Config {
            last_dir: Some(tmp.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config);
        // 打开浏览器
        assert!(app.handle_key(key(KeyCode::Char('b')), &mut config));
        assert_eq!(app.focus, playlist::Panel::Browser);

        // / 进入浏览器搜索（目录树收集已异步化：模拟主循环轮询提交结果）
        assert!(app.handle_key(key(KeyCode::Char('/')), &mut config));
        assert!(app.search_mode);
        assert_eq!(app.search_target, SearchTarget::Browser);
        drain_search_load(&mut app);

        // 输入"稻"：递归匹配一级与子目录里的文件
        assert!(app.handle_key(key(KeyCode::Char('稻')), &mut config));
        assert_eq!(app.search_query, "稻");
        assert_eq!(app.browser.entries().len(), 2, "应递归匹配到 2 项");

        // Esc 退出：清空关键字、退出搜索、恢复一级条目
        assert!(app.handle_key(key(KeyCode::Esc), &mut config));
        assert!(!app.search_mode);
        assert!(app.search_query.is_empty());
        assert_eq!(
            app.browser.entries().len(),
            3,
            "退出后应恢复一级条目（1目录+2文件）"
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn browser_search_box_renders_esc_hint() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let tmp = std::env::temp_dir().join("tuneux_browser_search_render_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("稻香.mp3")).unwrap();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let config = Config {
            last_dir: Some(tmp.clone()),
            ..Config::default()
        };
        let mut app = App::new(&config);
        app.left_panel = LeftPanel::Browser;
        app.search_mode = true;
        app.search_target = SearchTarget::Browser;
        app.search_query = "稻".to_string();

        let rows = app.playlist_rows();
        terminal
            .draw(|frame| {
                crate::tui::render::draw(frame, &mut app, &config, &rows, std::time::Duration::ZERO)
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        let compact: String = output.split_whitespace().collect();
        assert!(compact.contains("Esc退出"), "搜索框应显示 Esc 退出提示");
        assert!(compact.contains("稻"), "搜索框应显示关键字");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn about_key_toggles_and_any_key_closes() {
        let mut config = Config::default();
        let mut app = App::new(&config);
        assert!(!app.about_visible);

        // ? 打开
        assert!(app.handle_key(key(KeyCode::Char('?')), &mut config));
        assert!(app.about_visible);

        // 任意键关闭（被吃掉，不触发其它操作）
        assert!(app.handle_key(key(KeyCode::Down), &mut config));
        assert!(!app.about_visible);

        // q 在关于打开时应只关弹窗，不退出
        assert!(app.handle_key(key(KeyCode::Char('?')), &mut config));
        assert!(app.about_visible);
        assert!(
            app.handle_key(key(KeyCode::Char('q')), &mut config),
            "关于打开时 q 只关闭弹窗，不应退出"
        );
        assert!(!app.about_visible);
    }

    #[test]
    fn about_popup_renders_info() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let config = Config::default();
        let mut app = App::new(&config);
        app.about_visible = true;

        let rows = app.playlist_rows();
        terminal
            .draw(|frame| {
                crate::tui::render::draw(frame, &mut app, &config, &rows, std::time::Duration::ZERO)
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        let compact: String = output.split_whitespace().collect();
        assert!(compact.contains("关于"), "应包含关于弹窗标题");
        assert!(compact.contains("tuneux"), "应包含程序名");
        assert!(compact.contains("基于命令行的音乐播放器"), "应包含简介");
        assert!(compact.contains("MP3"), "应包含支持的格式");
        assert!(compact.contains("FLAC"), "应包含支持的格式");
        assert!(compact.contains("纯离线"), "应包含特性说明");
        assert!(compact.contains("木兰宽松许可证"), "应包含开源声明");
        assert!(compact.contains("symphonia"), "应包含第三方库声明");
        assert!(compact.contains("不羁的青春"), "应包含版权所有人");
        assert!(compact.contains("FreeYouth"), "应包含版权英文名");
        assert!(compact.contains("按任意键关闭"), "应包含关闭提示");
    }

    #[test]
    fn cover_panel_renders_title_without_cover() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let config = Config::default();
        let mut app = App::new(&config);
        app.left_panel = LeftPanel::Cover;

        let rows = app.playlist_rows();
        terminal
            .draw(|frame| {
                crate::tui::render::draw(frame, &mut app, &config, &rows, std::time::Duration::ZERO)
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        let compact: String = output.split_whitespace().collect();
        assert!(compact.contains("专辑封面"), "应包含封面面板标题");
        assert!(compact.contains("无封面"), "无封面时应显示提示");
    }

    // —— 音频引擎测试 ——
    #[test]
    #[ignore = "需要 test_tone.wav 测试夹具；运行：cargo test -- --ignored"]
    fn decoder_opens_and_decodes_wav() {
        let path = std::path::Path::new("测试音频/test_tone.wav");
        assert!(path.exists(), "测试夹具 test_tone.wav 缺失");
        let mut dec = audio::open_backend(path).expect("打开失败");
        let p = dec.params();
        assert_eq!(p.sample_rate, Some(44100));
        assert_eq!(p.channels, Some(2));
        assert_eq!(p.bits_per_sample, Some(16));
        let samples = dec.decode_next().expect("解码不应失败");
        assert!(samples.is_some());
        let samples = samples.unwrap();
        assert!(!samples.is_empty());
        assert_eq!(samples.len() % 2, 0, "立体声样本数应为偶数");
    }

    #[test]
    #[ignore = "需要 test_tone.wav 和音频设备；运行：cargo test -- --ignored"]
    fn engine_plays_until_eof() {
        let path = std::path::Path::new("测试音频/test_tone.wav");
        assert!(path.exists(), "测试夹具 test_tone.wav 缺失");
        let engine = audio::Engine::new(0.3).expect("音频设备初始化失败");
        engine.send(audio::AudioCmd::Play(path.to_path_buf()));
        let start = std::time::Instant::now();
        let mut got_eof = false;
        while start.elapsed() < std::time::Duration::from_secs(4) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if engine.poll_finished().is_some() {
                got_eof = true;
                break;
            }
        }
        assert!(got_eof, "应在 4 秒内收到 EOF 事件");
    }

    #[test]
    fn engine_volume_control() {
        let engine = match audio::Engine::new(0.5) {
            Ok(e) => e,
            Err(_) => {
                eprintln!("跳过：无音频设备");
                return;
            }
        };
        assert!((engine.volume() - 0.5).abs() < 0.01, "初始音量 0.5");
        engine.send(audio::AudioCmd::SetVolume(0.8));
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!((engine.volume() - 0.8).abs() < 0.01, "音量应更新为 0.8");
    }

    #[test]
    #[ignore = "需要 test_tone.wav 测试夹具；运行：cargo test -- --ignored"]
    fn metadata_extracts_from_real_wav() {
        let path = std::path::Path::new("测试音频/test_tone.wav");
        assert!(path.exists(), "测试夹具 test_tone.wav 缺失");
        let md = metadata::TrackMetadata::from_file(path);
        assert_eq!(md.sample_rate, Some(44100));
        assert_eq!(md.channels, Some(2));
        assert_eq!(md.bits_per_sample, Some(16));
        assert!(md.codec.is_some());
        let dur = md.duration.expect("应有时长");
        assert!(dur > 1.5 && dur < 2.5, "时长应在 2 秒左右，实际：{dur}");
        let br_kbps = md.bitrate.expect("应有码率") / 1000;
        assert!(br_kbps > 1300 && br_kbps < 1500, "码率应在 1411 kbps 附近");
        assert_eq!(md.title.as_deref(), Some("test_tone"));
        assert!(md.artist.is_none());
        assert!(md.album.is_none());
        assert_eq!(md.sample_rate_label(), "44.1 kHz");
        assert_eq!(md.channels_label(), "立体声");
        assert_eq!(md.bits_label(), "16 bit");
        let summary = md.tech_summary();
        assert!(summary.contains("44.1 kHz"));
        assert!(summary.contains("立体声"));
    }

    #[test]
    #[ignore = "需要 test_tone.wav；运行：cargo test tui_snapshot -- --ignored --nocapture"]
    fn tui_snapshot() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let path = std::path::Path::new("测试音频/test_tone.wav");
        if !path.exists() {
            eprintln!("跳过：test_tone.wav 缺失");
            return;
        }
        let mut config = Config::default();
        if let Some(parent) = path.parent() {
            config.last_dir = Some(parent.to_path_buf());
        }
        let mut app = App::new(&config);
        app.left_panel = LeftPanel::Browser;

        app.playlist.add(playlist::PlaylistItem {
            path: PathBuf::from("/music/Bob Dylan/01 - Blowin' in the Wind.mp3"),
            album: Some("Bob Dylan 经典".to_string()),
            track_number: Some(1),
            cue: None,
        });
        app.playlist.add(playlist::PlaylistItem {
            path: PathBuf::from("/music/周杰伦/01 - 稻香.mp3"),
            album: Some("魔杰座".to_string()),
            track_number: Some(1),
            cue: None,
        });
        app.playlist.add(playlist::PlaylistItem {
            path: PathBuf::from("/music/周杰伦/02 - 给我一首歌的时间.mp3"),
            album: Some("魔杰座".to_string()),
            track_number: Some(2),
            cue: None,
        });
        app.playlist.jump_to(1);
        app.playlist.set_selected(1);
        app.metadata_cache.insert(
            PathBuf::from("/music/周杰伦/01 - 稻香.mp3"),
            metadata::TrackMetadata {
                title: Some("稻香".to_string()),
                artist: Some("周杰伦".to_string()),
                album: Some("魔杰座".to_string()),
                track_number: Some(1),
                codec: Some("Mp3".to_string()),
                sample_rate: Some(44100),
                bits_per_sample: Some(16),
                channels: Some(2),
                duration: Some(223.0),
                bitrate: Some(320_000),
                cover: None,
                lyrics: None,
            },
        );
        app.current_metadata = app
            .metadata_cache
            .get(&PathBuf::from("/music/周杰伦/01 - 稻香.mp3"))
            .cloned();

        config.repeat = config::RepeatMode::List;
        config.shuffle = true;
        app.playlist.set_shuffle(true);

        if let Some(engine) = &app.engine {
            engine.set_level_lr_for_test(0.65, 0.30);
            let l_spec = [
                0.9, 0.85, 0.8, 0.7, 0.6, 0.5, 0.55, 0.5, 0.4, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05,
                0.05,
            ];
            let r_spec = [
                0.85, 0.8, 0.75, 0.65, 0.55, 0.5, 0.5, 0.45, 0.35, 0.3, 0.25, 0.2, 0.15, 0.1, 0.05,
                0.05,
            ];
            engine.set_spectrum_lr_for_test(&l_spec, &r_spec);
        }

        app.search_mode = true;
        app.search_query = "稻".to_string();
        app.jump_selected_to_filter_first();
        app.last_error = Some("打开失败 [broken.mp3]：unsupported format".to_string());
        app.last_error_at = Some(std::time::Instant::now());

        let rows = app.playlist_rows();
        terminal
            .draw(|frame| {
                crate::tui::render::draw(frame, &mut app, &config, &rows, std::time::Duration::ZERO)
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        println!("\n========== TUI 渲染快照 (120×30) ==========\n");
        println!("{output}");
        println!("==========================================\n");
        let compact: String = output.split_whitespace().collect();
        assert!(compact.contains("文件浏览器"), "应包含浏览器标题");
        assert!(compact.contains("播放列表"), "应包含播放列表标题");
        assert!(compact.contains("当前曲目"), "应包含当前曲目面板");
        assert!(compact.contains("电平"), "应包含电平面板");
        assert!(compact.contains("状态"), "应包含状态面板");
        assert!(compact.contains("帮助"), "应包含帮助面板");
        assert!(compact.contains("稻香"), "应包含当前曲目标题");
        assert!(compact.contains("周杰伦"), "应包含当前曲目歌手");
        assert!(
            compact.contains("▶") || compact.contains("‖"),
            "应包含当前播放指示符"
        );
        assert!(compact.contains("魔杰座"), "应包含当前曲目专辑");
    }

    #[test]
    #[ignore = "需要 test_tone.wav；运行：cargo test tui_compact_snapshot -- --ignored --nocapture"]
    fn tui_compact_snapshot() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let path = std::path::Path::new("测试音频/test_tone.wav");
        if !path.exists() {
            eprintln!("跳过：test_tone.wav 缺失");
            return;
        }
        let mut config = Config::default();
        if let Some(parent) = path.parent() {
            config.last_dir = Some(parent.to_path_buf());
        }
        let mut app = App::new(&config);

        app.playlist.add(playlist::PlaylistItem {
            path: PathBuf::from("/music/Bob Dylan/01 - Blowin' in the Wind.mp3"),
            album: Some("Bob Dylan 经典".to_string()),
            track_number: Some(1),
            cue: None,
        });
        app.playlist.add(playlist::PlaylistItem {
            path: PathBuf::from("/music/周杰伦/01 - 稻香.mp3"),
            album: Some("魔杰座".to_string()),
            track_number: Some(1),
            cue: None,
        });
        app.playlist.add(playlist::PlaylistItem {
            path: PathBuf::from("/music/周杰伦/02 - 给我一首歌的时间.mp3"),
            album: Some("魔杰座".to_string()),
            track_number: Some(2),
            cue: None,
        });
        app.playlist.jump_to(1);
        app.playlist.set_selected(1);
        app.metadata_cache.insert(
            PathBuf::from("/music/周杰伦/01 - 稻香.mp3"),
            metadata::TrackMetadata {
                title: Some("稻香".to_string()),
                artist: Some("周杰伦".to_string()),
                album: Some("魔杰座".to_string()),
                track_number: Some(1),
                codec: Some("Mp3".to_string()),
                sample_rate: Some(44100),
                bits_per_sample: Some(16),
                channels: Some(2),
                duration: Some(223.0),
                bitrate: Some(320_000),
                cover: None,
                lyrics: None,
            },
        );
        app.current_metadata = app
            .metadata_cache
            .get(&PathBuf::from("/music/周杰伦/01 - 稻香.mp3"))
            .cloned();

        config.repeat = config::RepeatMode::List;
        config.shuffle = true;
        app.playlist.set_shuffle(true);
        app.left_panel = LeftPanel::Hidden;

        let rows = app.playlist_rows();
        terminal
            .draw(|frame| {
                crate::tui::render::draw(frame, &mut app, &config, &rows, std::time::Duration::ZERO)
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        println!("\n========== 紧凑布局 TUI 快照 (120×30) ==========\n");
        println!("{output}");
        println!("==============================================\n");

        let compact: String = output.split_whitespace().collect();
        assert!(compact.contains("当前曲目"), "应包含'当前曲目'面板");
        assert!(compact.contains("电平"), "应包含'电平'面板");
        assert!(compact.contains("播放列表"), "应包含'播放列表'面板");
        assert!(compact.contains("状态"), "应包含'状态'面板");
        assert!(compact.contains("帮助"), "应包含'帮助'面板");
        assert!(
            !compact.contains("文件浏览器"),
            "默认布局不应有'文件浏览器'面板"
        );
    }

    #[test]
    #[ignore = "需要 测试音频/sample_voice.mp3；运行：cargo test -- --ignored"]
    fn decoder_opens_and_decodes_mp3() {
        let path = std::path::Path::new("测试音频/sample_voice.mp3");
        if !path.exists() {
            eprintln!("跳过：测试 MP3 缺失");
            return;
        }
        let mut dec = audio::open_backend(path).expect("打开 MP3 失败");
        let p = dec.params();
        assert_eq!(p.sample_rate, Some(44100));
        assert_eq!(p.channels, Some(2));
        assert!(p.bits_per_sample.is_none(), "MP3 无原始位深");
        for i in 0..10 {
            match dec.decode_next() {
                Ok(Some(samples)) => {
                    assert!(!samples.is_empty(), "第 {i} 个 packet 不应为空");
                    let max_abs = samples.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
                    assert!(max_abs > 0.0, "第 {i} 个 packet 应有非零音频");
                    for s in &samples {
                        assert!(*s >= -1.0 && *s <= 1.0, "样本 {s} 超出 [-1, 1]");
                    }
                }
                Ok(None) => panic!("不应在第 {i} 个 packet 就 EOF"),
                Err(e) => panic!("解码失败：{e:?}"),
            }
        }
    }

    #[test]
    #[ignore = "需要测试 MP3 + 音频设备；运行：cargo test -- --ignored"]
    fn engine_plays_mp3_until_eof() {
        let path = std::path::Path::new("测试音频/sample_voice.mp3");
        if !path.exists() {
            eprintln!("跳过：测试 MP3 缺失");
            return;
        }
        let engine = match audio::Engine::new(0.3) {
            Ok(e) => e,
            Err(_) => {
                eprintln!("跳过：无音频设备");
                return;
            }
        };
        engine.send(audio::AudioCmd::Play(path.to_path_buf()));
        let start = std::time::Instant::now();
        let mut got_eof = false;
        while start.elapsed() < std::time::Duration::from_secs(35) {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if engine.poll_finished().is_some() {
                got_eof = true;
                break;
            }
        }
        assert!(got_eof, "35 秒内应收到 EOF 事件");
    }

    /// 验证频谱面板在 Full 模式下能渲染（非空 buffer、面板标题存在、
    /// 降采样不崩溃）。N_BANDS=256 在 120 列终端会触发 max-pooling
    /// 降采样——这条路径需要单独覆盖。
    #[test]
    fn spectrum_full_renders_with_subsampling() {
        use crate::config::SpectrumMode;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut config = Config::default();
        let mut app = App::new(&config);
        // 无音频设备的 CI 环境跳过：set_spectrum_lr_for_test 需要 engine
        if app.engine.is_none() {
            eprintln!("[跳过] 无音频设备，频谱渲染测试需要 engine 可用");
            return;
        }
        // 注入全频段非零数据：低频满量程，高频衰减到 0.3（模拟真实
        // 音乐能量分布）——这样降采样后各显示列都有非零值，能验证
        // 频谱柱实际被画出来。
        let mut l_spec = [0.0f32; audio::spectrum::N_BANDS];
        let mut r_spec = [0.0f32; audio::spectrum::N_BANDS];
        for i in 0..audio::spectrum::N_BANDS {
            let frac = i as f32 / audio::spectrum::N_BANDS as f32;
            let v = (1.0 - frac * 0.7).clamp(0.0, 1.0);
            l_spec[i] = v;
            r_spec[i] = v * 0.85;
        }
        app.engine
            .as_ref()
            .unwrap()
            .set_spectrum_lr_for_test(&l_spec, &r_spec);
        app.spectrum_mode = SpectrumMode::Full;
        // 给音量设个有意义的值，避免状态条出现奇怪的"未定义"显示
        config.volume = 0.5;

        let rows = app.playlist_rows();
        terminal
            .draw(|frame| {
                crate::tui::render::draw(frame, &mut app, &config, &rows, std::time::Duration::ZERO)
            })
            .unwrap();

        let buffer = terminal.backend().buffer().clone();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        println!("\n========== 频谱面板快照 (120×30) ==========\n{output}\n============================================\n");
        // 用去空白后检查，避免依赖中文字符间空格数量（脆弱的渲染细节）
        let compact: String = output.split_whitespace().collect();
        assert!(compact.contains("频谱"), "应包含频谱面板标题");
        // 检查频谱区域有非空格字符（说明 max-pooling 后确实画了条）
        // Full 模式下频谱占主区大部，行 7 是顶边框、20 是基线（`─`），
        // 8..20 是频谱柱区。直接数非空格字符——避免硬编码 37 个 POOL 字符
        // （POOL 改了就破测试，没意义）。
        let mut bars_drawn = 0;
        for y in 8..20 {
            for x in 0..buffer.area.width {
                let ch = buffer[(x, y)].symbol();
                if !ch.is_empty() && ch != " " {
                    bars_drawn += 1;
                }
            }
        }
        assert!(
            bars_drawn > 50,
            "Full 模式下频谱应画出大量柱图，实际只画了 {bars_drawn} 格"
        );
    }
}
