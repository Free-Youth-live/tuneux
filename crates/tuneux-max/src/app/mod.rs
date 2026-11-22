//! # 应用状态与主循环
//!
//! 布局参考 tuneux-fx：菜单栏 → 顶部三栏条（当前曲目 / 三桶频段 / 电平表）→
//! 工作区（左面板浏览器或封面 + 播放列表 + 可选歌词）+ 频谱面板 + 状态栏 + 功能键栏。
//! 配色为终端原生：跟随系统深浅色，选中反白。

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver};
use eframe::egui;
use tuneux_commonx::fs_browser::{collect_recursive_entries, Entry, FsBrowserConfig};
use tuneux_commonx::media_key::{
    drain_media_key_events, spawn_media_key_listener, MediaKeyHandle, MEDIA_KEY_DRAIN_LIMIT,
};
use tuneux_commonx::{sanitize_f32, FsBrowser};
use tuneux_corex::spectrum::{SpectrumPeakHold, N_BANDS};
use tuneux_corex::{
    AudioCmd, Engine, PlaybackMedium, PreloadTarget, EQ_BANDS, EQ_FREQS, EQ_GAIN_MAX_DB,
    EQ_GAIN_MIN_DB, KNOWN_AUDIO_EXTS,
};
use tuneux_mediax::lyrics::Lyrics;
use tuneux_mediax::m3u;
use tuneux_mediax::metadata::TrackMetadata;
use tuneux_mediax::{NavOutcome, PlaylistCore, PlaylistItem, PlaylistState, SavedList};

use std::collections::{HashMap, HashSet};

use crate::command::{self, Command};
use crate::config::{self, Config};
use crate::i18n;
use crate::playlist_rows::{self, PlaylistRow};
use crate::plugins::{self, PluginSet};
use crate::theme;
use crate::theme::Palette;
use crate::ui;

/// 元数据暖缓存每帧预算：至多读这么多未缓存曲目，避免大列表同步读盘冻结 UI。
const METADATA_WARM_PER_FRAME: usize = 12;

/// 频谱面板显示档位（循环切换：关 → 半屏 → 全屏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpectrumView {
    Hidden,
    Half,
    Full,
}

impl SpectrumView {
    fn next(self) -> Self {
        match self {
            Self::Hidden => Self::Half,
            Self::Half => Self::Full,
            Self::Full => Self::Hidden,
        }
    }

    fn label_key(self) -> &'static str {
        match self {
            Self::Hidden => "view.spectrum_hidden",
            Self::Half => "view.spectrum_half",
            Self::Full => "view.spectrum_full",
        }
    }

    fn height_ratio(self) -> f32 {
        match self {
            Self::Hidden => 0.0,
            Self::Half => 0.28,
            Self::Full => 0.5,
        }
    }
}

/// 左侧面板视图：浏览器或封面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeftView {
    Browser,
    Cover,
}

/// 工作区视图状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewState {
    spectrum: SpectrumView,
    lyrics: bool,
    left: LeftView,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            spectrum: SpectrumView::Hidden,
            lyrics: false,
            left: LeftView::Browser,
        }
    }
}

/// 一次用户动作（菜单/按钮），在绘制结束后统一执行，避免闭包内借用冲突。
/// 含皮肤名载荷（String），故不派生 Copy，按值移交执行。
#[derive(Clone, PartialEq, Eq)]
enum Action {
    None,
    Quit,
    TogglePlay,
    Stop,
    Prev,
    Next,
    AddSelected,
    RemoveSelected,
    ClearPlaylist,
    SetLangZh,
    SetLangEn,
    About,
    ShowBrowser,
    ToggleCover,
    ToggleLyrics,
    CycleSpectrum,
    ToggleMediaKeys,
    OpenCommand,
    OpenEq,
    OpenCompressor,
    OpenPlugins,
    ReloadPlugins,
    SetSkinTerminal,
    SetSkin(String),
    ToggleReplayGain,
    SetMedium(PlaybackMedium),
    SetVisualBuiltin,
    SetVisual(usize),
}

/// 浏览器右键菜单动作（绘制期收集、绘制后统一执行，避免闭包借用冲突）。
enum BrowserCtx {
    /// 加入单个文件（CUE 自动展开）。
    Add(PathBuf),
    /// 加入并播放。
    AddPlay(PathBuf),
    /// 进入目录。
    EnterDir(PathBuf),
    /// 递归加入整个目录。
    AddDir(PathBuf),
}

/// 播放列表右键菜单动作。
enum PlaylistCtx {
    /// 播放指定下标。
    Play(usize),
    /// 移除指定下标。
    Remove(usize),
}

/// 浏览器 → 播放列表的拖拽载荷（路径 + 是否目录）。
#[derive(Clone)]
struct DragEntry {
    path: PathBuf,
    is_dir: bool,
}

/// 后台递归搜索的结果接收端（带代次防陈旧结果）。
struct SearchJob {
    rx: Receiver<(u64, Vec<Entry>, bool)>,
}

/// 浏览器递归搜索状态。
#[derive(Default)]
struct BrowserSearch {
    query: String,
    results: Vec<Entry>,
    selected: usize,
    truncated: bool,
    input_focus: bool,
    generation: u64,
    job: Option<SearchJob>,
}

impl BrowserSearch {
    fn active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    fn searching(&self) -> bool {
        self.active() && self.job.is_some()
    }
}

/// 当前播放的 CUE 分轨区间（相对文件时间轴）。
#[derive(Debug, Clone)]
struct ActiveCue {
    start: f64,
    end: Option<f64>,
    file_duration: Option<f64>,
    title: String,
    performer: Option<String>,
}

/// 应用主状态。
pub struct MaxApp {
    cfg: Config,
    i18n: tuneux_commonx::I18n,
    engine: Option<Engine>,
    browser: FsBrowser,
    playlist: PlaylistCore,
    state: PlaylistState,
    current: Option<PathBuf>,
    metadata: Option<TrackMetadata>,
    /// 播放列表曲目元数据缓存（path → 标签；标题列显示元数据标题而非文件名）。
    metadata_cache: HashMap<PathBuf, TrackMetadata>,
    /// 元数据暖缓存是否仍有未读项（按帧预算推进，期间需持续重绘）。
    metadata_warming: bool,
    /// 浏览器键盘导航后把选中行滚入可视区（一次性，绘后清除）。
    browser_scroll_pending: bool,
    /// 播放列表起播/切曲后把当前行滚入可视区（一次性，绘后清除）。
    playlist_scroll_pending: bool,
    cover_texture: Option<egui::TextureHandle>,
    lyrics: Option<Lyrics>,
    media_path: Option<PathBuf>,
    playlist_sel: usize,
    position: f64,
    duration: f64,
    playing: bool,
    view: ViewState,
    status: Option<(String, Instant)>,
    about_open: bool,
    browser_search: BrowserSearch,
    search_debounce: Option<Instant>,
    playlist_filter: String,
    command_open: bool,
    command_input: String,
    search_focus: bool,
    command_focus: bool,
    media_keys: Option<MediaKeyHandle>,
    plugins: PluginSet,
    visual_index: Option<usize>,
    visual_text: String,
    /// 频谱峰值保持状态（corex 统一算法；换曲 / 停播清零）。
    spectrum_peaks: SpectrumPeakHold,
    /// 折叠的专辑组（空串哨兵 = 未知专辑；切换语言不漂移）。
    collapsed_albums: HashSet<String>,
    active_cue: Option<ActiveCue>,
    replay_gain_cache: std::collections::BTreeMap<PathBuf, f64>,
    consecutive_failures: u32,
    resume_pending: bool,
    eq_open: bool,
    comp_open: bool,
    plugins_open: bool,
    last_save: Instant,
    last_title: String,
}

impl MaxApp {
    /// 从 eframe 上下文创建应用。
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (mut cfg, config_status) = config::load_with_status();
        cfg.volume = sanitize_f32(cfg.volume, 1.0).clamp(0.0, 1.0);
        cfg.browser_ratio = sanitize_f32(cfg.browser_ratio, 0.3).clamp(0.15, 0.6);
        let i18n = i18n::build(&cfg.lang);
        let config_corrupt_msg = tuneux_commonx::config_status::corrupt_flash_msg(
            &config_status,
            &i18n.t("msg.config_corrupt"),
            &i18n.t("msg.config_corrupt_nobackup"),
        );
        let (mut state, state_status) = config::load_state_with_status();
        // 越界 active 加载期钳制：PlaylistState 契约「越界启动时钳制」，
        // 否则 initial_items 落到空 legacy items、5 秒后 persist 覆盖已存列表。
        state.active = state.active.min(state.lists.len().saturating_sub(1));
        let state_corrupt_msg = tuneux_commonx::config_status::corrupt_flash_msg(
            &state_status,
            &i18n.t("msg.state_corrupt"),
            &i18n.t("msg.state_corrupt_nobackup"),
        );
        let engine = Engine::new(cfg.volume).ok();
        if let Some(engine) = &engine {
            engine.set_replay_gain_enabled(cfg.replay_gain);
            engine.set_medium(
                cfg.playback_medium
                    .parse()
                    .unwrap_or(tuneux_corex::PlaybackMedium::None),
            );
        }
        let start_dir = cfg
            .last_dir
            .clone()
            .filter(|path| path.is_dir())
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let browser = FsBrowser::open(
            &start_dir,
            FsBrowserConfig {
                audio_exts: KNOWN_AUDIO_EXTS,
            },
        );
        let mut playlist = PlaylistCore::new_random();
        playlist.add_many(initial_items(&state));
        playlist.set_shuffle_mode(tuneux_mediax::ShuffleMode::parse(&cfg.shuffle_mode));
        let current = state
            .lists
            .get(state.active)
            .and_then(|list| list.current.clone())
            .or_else(|| state.current.clone())
            .filter(|path| path.exists());
        let current_cue = state
            .lists
            .get(state.active)
            .and_then(|list| list.current_cue);
        let replay_gain_cache = state.replay_gain.clone();
        let mut playlist_sel = 0;
        let mut resume_pending = false;
        if let Some(path) = &current {
            let saved = state.positions.get(path).copied();
            if let Some(index) = playlist.items().iter().position(|item| {
                item.path.as_path() == path
                    && match current_cue {
                        Some(index) => item.cue.as_ref().map(|cue| cue.index) == Some(index),
                        None => cue_contains(item, saved),
                    }
            }) {
                playlist.set_current(index);
                playlist_sel = index;
                resume_pending = true;
            }
        }
        let active_cue = playlist
            .current_index()
            .and_then(|index| playlist.items().get(index))
            .and_then(|item| item.cue.as_ref())
            .map(|cue| ActiveCue {
                start: cue.start_ms as f64 / 1000.0,
                end: cue.end_ms.map(|ms| ms as f64 / 1000.0),
                file_duration: None,
                title: cue.title.clone(),
                performer: cue.performer.clone(),
            });
        let media_keys = if cfg.media_keys_enabled {
            spawn_media_key_listener("org.mpris.MediaPlayer2.tuneux-max", "tuneux-max")
        } else {
            None
        };
        let plugins = plugins::load_all(engine.as_ref());
        let mut app = Self {
            cfg,
            i18n,
            engine,
            browser,
            playlist,
            state,
            current,
            metadata: None,
            cover_texture: None,
            lyrics: None,
            media_path: None,
            playlist_sel,
            position: 0.0,
            duration: 0.0,
            playing: false,
            view: ViewState::default(),
            status: None,
            about_open: false,
            browser_search: BrowserSearch::default(),
            search_debounce: None,
            playlist_filter: String::new(),
            command_open: false,
            command_input: String::new(),
            search_focus: false,
            command_focus: false,
            media_keys,
            plugins,
            visual_index: None,
            visual_text: String::new(),
            spectrum_peaks: SpectrumPeakHold::default(),
            collapsed_albums: HashSet::new(),
            metadata_cache: HashMap::new(),
            metadata_warming: false,
            browser_scroll_pending: false,
            playlist_scroll_pending: false,
            active_cue,
            replay_gain_cache,
            consecutive_failures: 0,
            resume_pending,
            eq_open: false,
            comp_open: false,
            plugins_open: false,
            last_save: Instant::now(),
            last_title: String::new(),
        };
        if app.engine.is_none() {
            app.flash("msg.engine_unavailable");
        }
        let startup_messages = [config_corrupt_msg, state_corrupt_msg]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("；");
        if !startup_messages.is_empty() {
            app.flash_text(startup_messages);
        }
        app.apply_configured_skin(&cc.egui_ctx);
        // 启动恢复播放列表后补元数据缓存（标题列显示元数据标题）。
        app.ensure_metadata_cache();
        app
    }

    fn flash(&mut self, key: &str) {
        self.status = Some((self.i18n.t(key).into_owned(), Instant::now()));
    }

    fn flash_text(&mut self, text: String) {
        self.status = Some((text, Instant::now()));
    }

    /// 当前曲目变化时一次性载入元数据 / 封面纹理 / 歌词。
    fn ensure_media_loaded(&mut self, ctx: &egui::Context) {
        let current = self.current.clone();
        if self.media_path == current {
            return;
        }
        self.media_path = current.clone();
        self.metadata = None;
        self.cover_texture = None;
        self.lyrics = None;
        let Some(path) = current else {
            return;
        };
        let metadata = TrackMetadata::from_file(&path);
        if let Some(cover) = &metadata.cover {
            if let Some(image) =
                tuneux_mediax::decode_cover_thumb(&cover.bytes, Some(&cover.mime), 640, 640)
            {
                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [image.width as usize, image.height as usize],
                    &image.rgba,
                );
                self.cover_texture = Some(ctx.load_texture(
                    "tuneux-max-cover",
                    color_image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        let lrc_path = path.with_extension("lrc");
        self.lyrics = Lyrics::load_from_file(&lrc_path)
            .filter(|lyrics| !lyrics.is_empty())
            .or_else(|| metadata.lyrics.as_deref().and_then(Lyrics::from_embedded));
        if let Some(cue) = self.active_cue.as_mut() {
            cue.file_duration = metadata.duration;
        }
        self.duration = metadata.duration.unwrap_or(0.0);
        self.metadata = Some(metadata);
    }

    /// 轮询音频引擎状态与错误。
    fn poll_engine(&mut self) {
        let (finished, failed, switched, error) = {
            let Some(engine) = &self.engine else {
                return;
            };
            self.position = engine.position();
            self.duration = engine.duration();
            self.playing = engine.is_playing();
            (
                engine.poll_finished().is_some(),
                engine.poll_failed().is_some(),
                engine.poll_track_switched().is_some(),
                engine.take_last_error(),
            )
        };
        if let Some(cue) = &self.active_cue {
            (self.position, self.duration) =
                cue_relative_progress(self.position, cue, self.duration);
        }
        if switched {
            self.cache_measured_gain();
            self.advance_ui_on_gapless();
        }
        if finished {
            self.cache_measured_gain();
            self.on_track_finished();
        }
        if failed {
            self.handle_playback_failure();
        }
        if let Some(error) = error {
            let prefix = self.i18n.t("msg.engine_error").into_owned();
            self.flash_text(format!("{prefix}：{error}"));
        }
    }

    /// 曲目自然结束：清进度并按顺序自动下一曲（无下一曲则停止）。
    fn on_track_finished(&mut self) {
        self.position = 0.0;
        self.playing = false;
        self.consecutive_failures = 0;
        if self.active_cue.is_none() {
            if let Some(path) = self.current.clone() {
                self.state.save_position(&path, 0.0);
            }
        }
        let _ = self.next_track_impl(false);
    }

    fn play_index(&mut self, index: usize) {
        self.play_index_inner(index, true);
    }

    fn play_index_auto(&mut self, index: usize) {
        self.play_index_inner(index, false);
    }

    fn play_index_inner(&mut self, index: usize, manual: bool) {
        let Some(item) = self.playlist.items().get(index).cloned() else {
            self.flash("msg.empty_selection");
            return;
        };
        if manual {
            self.playlist.jump_to(index);
            self.consecutive_failures = 0;
        } else {
            self.playlist.set_current(index);
        }
        self.playlist_sel = index;
        self.resume_pending = false;
        self.playlist_scroll_pending = true;
        self.play_item(&item);
    }

    fn cache_measured_gain(&mut self) {
        let measured = self
            .engine
            .as_ref()
            .and_then(|engine| engine.take_measured_gain_db());
        if let (Some(db), Some(path)) = (measured, self.current.clone()) {
            self.replay_gain_cache.insert(path, db);
        }
    }

    fn drain_residual_events(&mut self) {
        let Some(engine) = &self.engine else {
            return;
        };
        let stale_gain = engine.take_measured_gain_db();
        while engine.poll_track_switched().is_some() {}
        while engine.poll_finished().is_some() {}
        while engine.poll_failed().is_some() {}
        if let (Some(db), Some(path)) = (stale_gain, self.current.clone()) {
            self.replay_gain_cache.insert(path, db);
        }
    }

    fn play_item(&mut self, item: &PlaylistItem) {
        if self.engine.is_none() {
            self.flash("msg.engine_unavailable");
            return;
        }
        // 换曲清零峰帽，白帽不跨曲目残留（与 fx 同口径）。
        self.spectrum_peaks.reset();
        // 展开当前曲目所在专辑，确保分组视图下可见（与 fx 同口径）。
        self.collapsed_albums
            .remove(&item.album.clone().unwrap_or_default());
        self.drain_residual_events();
        let metadata = TrackMetadata::from_file(&item.path);
        let resume = if item.cue.is_some() {
            0.0
        } else {
            tuneux_mediax::resume_secs(
                self.state.positions.get(&item.path).copied(),
                metadata.duration,
            )
            .unwrap_or(0.0)
        };
        let Some(engine) = &self.engine else {
            return;
        };
        if let Some(cue) = &item.cue {
            engine.send(AudioCmd::PlayRange {
                path: item.path.clone(),
                start_secs: cue.start_ms as f64 / 1000.0,
                end_secs: cue.end_ms.map(|ms| ms as f64 / 1000.0),
            });
        } else if resume > 0.5 {
            engine.send(AudioCmd::PlayResume {
                path: item.path.clone(),
                secs: resume,
            });
        } else {
            engine.send(AudioCmd::Play(item.path.clone()));
        }
        let gain = self
            .replay_gain_cache
            .get(&item.path)
            .copied()
            .map(|db| 10f64.powf(db / 20.0) as f32)
            .unwrap_or(1.0);
        engine.set_replay_gain(gain);
        self.current = Some(item.path.clone());
        self.active_cue = item.cue.as_ref().map(|cue| ActiveCue {
            start: cue.start_ms as f64 / 1000.0,
            end: cue.end_ms.map(|ms| ms as f64 / 1000.0),
            file_duration: metadata.duration,
            title: cue.title.clone(),
            performer: cue.performer.clone(),
        });
        self.position = resume;
        self.playing = true;
        self.refresh_preload();
    }

    fn refresh_preload(&mut self) {
        let next = self
            .playlist
            .peek_next_item(self.cfg.repeat)
            .map(|item| match &item.cue {
                Some(cue) => PreloadTarget::range(
                    item.path.clone(),
                    cue.start_ms as f64 / 1000.0,
                    cue.end_ms.map(|ms| ms as f64 / 1000.0),
                ),
                None => PreloadTarget::whole(item.path.clone()),
            });
        if let Some(engine) = &self.engine {
            engine.send(AudioCmd::PreloadNext(next));
        }
    }

    fn handle_playback_failure(&mut self) {
        let (count, keep_skipping) =
            tuneux_mediax::advance_failure_count(self.consecutive_failures);
        self.consecutive_failures = count;
        if !keep_skipping {
            self.flash("msg.consecutive_fail");
            self.stop();
            return;
        }
        let repeat = if self.cfg.repeat == tuneux_mediax::RepeatMode::Single {
            tuneux_mediax::RepeatMode::Off
        } else {
            self.cfg.repeat
        };
        if let NavOutcome::Switch(index) = self.playlist.next(repeat) {
            self.play_index_auto(index);
        } else {
            self.stop();
        }
    }

    fn advance_ui_on_gapless(&mut self) {
        let NavOutcome::Switch(index) = self.playlist.next(self.cfg.repeat) else {
            return;
        };
        let Some(item) = self.playlist.items().get(index).cloned() else {
            return;
        };
        self.playlist.set_current(index);
        self.playlist_sel = index;
        self.consecutive_failures = 0;
        self.resume_pending = false;
        self.playlist_scroll_pending = true;
        // 与 play_item 的「换曲清峰帽」同口径，无缝切曲不残留上一曲峰帽。
        self.spectrum_peaks.reset();
        let same_file = self.current.as_ref() == Some(&item.path);
        let file_duration = same_file
            .then(|| {
                self.metadata
                    .as_ref()
                    .and_then(|metadata| metadata.duration)
            })
            .flatten();
        self.current = Some(item.path.clone());
        self.active_cue = item.cue.as_ref().map(|cue| ActiveCue {
            start: cue.start_ms as f64 / 1000.0,
            end: cue.end_ms.map(|ms| ms as f64 / 1000.0),
            file_duration,
            title: cue.title.clone(),
            performer: cue.performer.clone(),
        });
        let gain = self
            .replay_gain_cache
            .get(&item.path)
            .copied()
            .map(|db| 10f64.powf(db / 20.0) as f32)
            .unwrap_or(1.0);
        if let Some(engine) = &self.engine {
            engine.set_replay_gain(gain);
        }
        self.refresh_preload();
    }

    fn next_track(&mut self) -> bool {
        self.next_track_impl(true)
    }

    fn next_track_impl(&mut self, manual: bool) -> bool {
        if manual {
            self.consecutive_failures = 0;
        }
        match self.playlist.next(self.cfg.repeat) {
            NavOutcome::Switch(index) => {
                self.play_index_inner(index, manual);
                true
            }
            NavOutcome::Repeat => {
                let index = self.playlist.current_index().unwrap_or(self.playlist_sel);
                if index < self.playlist.len() {
                    self.play_index_inner(index, manual);
                    true
                } else {
                    false
                }
            }
            NavOutcome::End => {
                self.stop();
                false
            }
        }
    }

    fn prev_track(&mut self) -> bool {
        self.consecutive_failures = 0;
        match self.playlist.prev(self.cfg.repeat) {
            NavOutcome::Switch(index) => {
                self.play_index(index);
                true
            }
            NavOutcome::Repeat => {
                let index = self.playlist.current_index().unwrap_or(self.playlist_sel);
                if index < self.playlist.len() {
                    self.play_index(index);
                    true
                } else {
                    false
                }
            }
            NavOutcome::End => false,
        }
    }

    /// 把一个文件展开为播放列表条目：`.cue` 文件按清单展开；整轨音频若同名
    /// `.cue` 存在也展开为分轨；否则单条。返回条目、跳过数和实际音频路径。
    fn expand_items(&self, path: &Path) -> (Vec<PlaylistItem>, usize, PathBuf) {
        if tuneux_commonx::fs_browser::is_cue_file(path) {
            return match tuneux_mediax::cue::cue_items_from_cue_file(path) {
                Some((items, skipped, audio)) => (items, skipped, audio),
                None => (Vec::new(), 0, path.to_path_buf()),
            };
        }
        let metadata = TrackMetadata::from_file(path);
        let (items, skipped) = tuneux_mediax::cue::cue_items_from(path, &metadata);
        if items.is_empty() {
            (
                vec![PlaylistItem {
                    path: path.to_path_buf(),
                    album: metadata.album.clone(),
                    track_number: metadata.track_number,
                    cue: None,
                }],
                0,
                path.to_path_buf(),
            )
        } else {
            (items, skipped, path.to_path_buf())
        }
    }

    /// 追加条目（按 路径 + CUE 轨号 去重），返回新增数量。
    fn add_items(&mut self, items: Vec<PlaylistItem>) -> usize {
        let mut added = 0;
        for item in items {
            let exists = self.playlist.items().iter().any(|existing| {
                existing.path == item.path
                    && existing.cue.as_ref().map(|cue| cue.index)
                        == item.cue.as_ref().map(|cue| cue.index)
            });
            if !exists {
                self.playlist.add(item);
                added += 1;
            }
        }
        // 加入统一入口补元数据缓存（覆盖文件 / 目录 / 拖拽 / 菜单加入）。
        self.ensure_metadata_cache();
        added
    }

    /// 为播放列表中未缓存的曲目读元数据入缓存。
    ///
    /// **按帧预算**：每次至多读 [`METADATA_WARM_PER_FRAME`] 个未缓存项。
    /// 旧实现一次性同步读全部缺失项——启动恢复大列表 / 目录批量加入时会在
    /// UI 线程逐文件读盘，冻结界面数秒。改为每帧少量、逐帧推进（标题列先显
    /// 文件名、暖缓存后回填元数据标题），与 fx / 基础版的后台分批口径一致。
    /// 仍有未读项时置 `metadata_warming`，update 据此持续请求重绘。
    fn ensure_metadata_cache(&mut self) {
        let mut missing: Vec<PathBuf> = Vec::new();
        for item in self.playlist.items() {
            if !self.metadata_cache.contains_key(&item.path) {
                missing.push(item.path.clone());
                if missing.len() > METADATA_WARM_PER_FRAME {
                    break;
                }
            }
        }
        let more = missing.len() > METADATA_WARM_PER_FRAME;
        for path in missing.into_iter().take(METADATA_WARM_PER_FRAME) {
            self.metadata_cache
                .insert(path.clone(), TrackMetadata::from_file(&path));
        }
        self.metadata_warming = more;
    }

    /// 加入并播放一个文件（CUE 展开后从第一轨开始）。
    fn add_and_play(&mut self, path: &Path) {
        let (items, skipped, audio_path) = self.expand_items(path);
        if items.is_empty() {
            self.flash("msg.cue_invalid");
            return;
        }
        let first_start_ms = items
            .first()
            .and_then(|item| item.cue.as_ref().map(|cue| cue.start_ms));
        self.add_items(items);
        if skipped > 0 {
            let prefix = self.i18n.t("msg.cue_skipped").into_owned();
            self.flash_text(format!("{prefix}：{skipped}"));
        }
        let index = first_track_index(self.playlist.items(), &audio_path, first_start_ms);
        if let Some(index) = index {
            self.play_index(index);
        } else {
            self.flash("msg.cue_invalid");
        }
    }

    fn browser_go_parent(&mut self) {
        let Some(parent) = self.browser.cwd().parent().map(Path::to_path_buf) else {
            return;
        };
        if !self.browser.navigate_to(&parent) {
            let prefix = self.i18n.t("msg.open_failed").into_owned();
            self.flash_text(format!("{prefix}：{}", parent.display()));
        }
    }

    fn activate_browser_entry(&mut self, index: usize) {
        let Some(entry) = self.browser.entries().get(index).cloned() else {
            return;
        };
        match entry {
            Entry::Dir { path, .. } => {
                if !self.browser.navigate_to(&path) {
                    let prefix = self.i18n.t("msg.open_failed").into_owned();
                    self.flash_text(format!("{prefix}：{}", path.display()));
                }
            }
            Entry::File { path, .. } => self.add_and_play(&path),
        }
    }

    fn add_selected_browser(&mut self) {
        let Some(entry) = self.browser.current().cloned() else {
            self.flash("msg.empty_selection");
            return;
        };
        if let Entry::File { path, .. } = entry {
            self.add_path(&path);
        }
    }

    /// 加入一个文件（CUE 展开、去重）并提示结果。快捷键 a / 右键「加入」/
    /// 拖拽落地共用同一口径。
    fn add_path(&mut self, path: &Path) {
        let (items, skipped, _audio_path) = self.expand_items(path);
        if items.is_empty() {
            self.flash("msg.cue_invalid");
            return;
        }
        let added = self.add_items(items);
        if skipped > 0 {
            let prefix = self.i18n.t("msg.cue_skipped").into_owned();
            self.flash_text(format!("{prefix}：{skipped}"));
        } else if added > 0 {
            self.flash("msg.added");
        } else {
            self.flash("msg.already_added");
        }
    }

    /// 递归加入一个目录下的全部音频文件（右键「加入文件夹」/ 拖拽目录）。
    fn add_dir_recursive(&mut self, path: &Path) {
        let (entries, _truncated) = collect_recursive_entries(
            path,
            FsBrowserConfig {
                audio_exts: KNOWN_AUDIO_EXTS,
            },
        );
        let mut items = Vec::new();
        for entry in entries {
            if let Entry::File { path: file, .. } = entry {
                let (expanded, _skipped, _audio) = self.expand_items(&file);
                items.extend(expanded);
            }
        }
        if items.is_empty() {
            self.flash("msg.empty_selection");
            return;
        }
        let added = self.add_items(items);
        if added > 0 {
            self.flash("msg.added");
        } else {
            self.flash("msg.already_added");
        }
    }

    /// 移除指定下标（右键「移除」）；移除的是当前播放项则停播。
    fn remove_at(&mut self, index: usize) {
        if index >= self.playlist.len() {
            return;
        }
        // 用「当前播放条目下标」判定，CUE 同文件多分轨不会误停。
        let removed_current = self.playlist.current_index() == Some(index);
        if self.playlist.remove(index) {
            if index < self.playlist_sel {
                self.playlist_sel = self.playlist_sel.saturating_sub(1);
            }
            self.playlist_sel = self.playlist_sel.min(self.playlist.len().saturating_sub(1));
            if removed_current {
                self.stop();
                self.current = None;
                self.resume_pending = false;
            }
        }
    }

    /// 执行浏览器右键菜单动作（绘制后统一调用）。
    fn apply_browser_ctx(&mut self, action: BrowserCtx) {
        match action {
            BrowserCtx::Add(path) => self.add_path(&path),
            BrowserCtx::AddPlay(path) => self.add_and_play(&path),
            BrowserCtx::EnterDir(path) => {
                if !self.browser.navigate_to(&path) {
                    let prefix = self.i18n.t("msg.open_failed").into_owned();
                    self.flash_text(format!("{prefix}：{}", path.display()));
                }
            }
            BrowserCtx::AddDir(path) => self.add_dir_recursive(&path),
        }
    }

    /// 执行播放列表右键菜单动作（绘制后统一调用）。
    fn apply_playlist_ctx(&mut self, action: PlaylistCtx) {
        match action {
            PlaylistCtx::Play(index) => self.play_index(index),
            PlaylistCtx::Remove(index) => self.remove_at(index),
        }
    }

    /// 拖拽落地：目录递归加入，文件按 CUE 展开加入。
    fn add_dropped(&mut self, entry: DragEntry) {
        if entry.is_dir {
            self.add_dir_recursive(&entry.path);
        } else {
            self.add_path(&entry.path);
        }
    }

    fn set_volume(&mut self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        self.cfg.volume = volume;
        if let Some(engine) = &self.engine {
            engine.send(AudioCmd::SetVolume(volume));
        }
    }

    fn toggle_play(&mut self) {
        if self.engine.is_none() {
            self.flash("msg.engine_unavailable");
            return;
        }
        // 播放列表非空但尚未装载当前曲（例如重启后从未起播）：直接起播选中项，
        // 不能对空闲引擎发 Resume（会显示播放中却无声）。
        if self.current.is_none() && !self.playlist.is_empty() {
            let index = self.playlist_sel.min(self.playlist.len() - 1);
            self.play_index(index);
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let cue = self
            .playlist
            .current_index()
            .and_then(|index| self.playlist.items().get(index))
            .and_then(|item| item.cue.as_ref());
        // 末尾判定必须用绝对时间轴：engine.duration() 在区间播放下是区间长度，
        // 与绝对坐标的 engine.position() 量纲不同（末轨无 end_ms 时误判「已到末尾」）。
        let end_secs = self
            .active_cue
            .as_ref()
            .and_then(|cue| cue.end)
            .or_else(|| self.active_cue.as_ref().and_then(|cue| cue.file_duration))
            .unwrap_or(0.0);
        let at_end = self.resume_pending
            || engine.at_eof()
            || (end_secs > 0.0 && engine.position() >= end_secs - 0.1);
        match tuneux_mediax::toggle_decision(engine.is_playing(), at_end, cue) {
            tuneux_mediax::PlaybackDecision::Pause => {
                if self.active_cue.is_none() {
                    if let Some(path) = &self.current {
                        self.state.save_position(path, engine.position());
                    }
                }
                engine.send(AudioCmd::Pause);
                self.playing = false;
            }
            tuneux_mediax::PlaybackDecision::Resume => {
                engine.send(AudioCmd::Resume);
                self.playing = true;
            }
            tuneux_mediax::PlaybackDecision::Play
            | tuneux_mediax::PlaybackDecision::PlayRange { .. } => {
                let index = self
                    .playlist
                    .current_index()
                    .unwrap_or(self.playlist_sel)
                    .min(self.playlist.len().saturating_sub(1));
                if self.playlist.is_empty() {
                    self.flash("msg.empty_selection");
                } else {
                    self.play_index(index);
                }
            }
            _ => {}
        }
    }

    fn stop(&mut self) {
        if let Some(engine) = &self.engine {
            if self.active_cue.is_none() {
                if let Some(path) = &self.current {
                    self.state.save_position(path, engine.position());
                }
            }
            engine.send(AudioCmd::Stop);
            engine.send(AudioCmd::PreloadNext(None));
        }
        self.playing = false;
        self.position = 0.0;
        self.duration = 0.0;
        self.active_cue = None;
        self.resume_pending = self.current.is_some();
        self.spectrum_peaks.reset();
    }

    fn seek_by(&mut self, delta: f64) {
        if self.duration <= 0.0 {
            return;
        }
        let target = (self.position + delta).clamp(0.0, self.duration);
        let absolute = target + self.active_cue.as_ref().map_or(0.0, |cue| cue.start);
        if let Some(engine) = &self.engine {
            engine.send(AudioCmd::Seek(absolute));
            self.position = target;
        }
    }

    fn remove_selected(&mut self) {
        if self.playlist.is_empty() {
            return;
        }
        let index = self.playlist_sel.min(self.playlist.len() - 1);
        self.remove_at(index);
    }

    fn clear_playlist(&mut self) {
        self.playlist.clear();
        self.playlist_sel = 0;
        self.stop();
        self.current = None;
        self.resume_pending = false;
        self.metadata_cache.clear();
    }

    /// 启动浏览器递归后台搜索（代次防陈旧结果覆盖新查询）。
    fn start_browser_search(&mut self) {
        self.browser_search.generation = self.browser_search.generation.wrapping_add(1);
        let generation = self.browser_search.generation;
        let cwd = self.browser.cwd().to_path_buf();
        let query = self.browser_search.query.trim().to_lowercase();
        let (tx, rx) = bounded(1);
        std::thread::spawn(move || {
            let (entries, truncated) = collect_recursive_entries(
                &cwd,
                FsBrowserConfig {
                    audio_exts: KNOWN_AUDIO_EXTS,
                },
            );
            let entries = entries
                .into_iter()
                .filter(|entry| entry_matches_query(entry, &query))
                .collect::<Vec<_>>();
            let _ = tx.send((generation, entries, truncated));
        });
        self.browser_search.results.clear();
        self.browser_search.selected = 0;
        self.browser_search.truncated = false;
        self.browser_search.job = Some(SearchJob { rx });
        self.search_debounce = None;
    }

    /// 轮询后台搜索结果（只接受代次匹配的结果）。
    fn poll_browser_search(&mut self) {
        let Some(job) = &self.browser_search.job else {
            return;
        };
        match job.rx.try_recv() {
            Ok((generation, entries, truncated))
                if generation == self.browser_search.generation =>
            {
                self.browser_search.results = entries;
                self.browser_search.selected = 0;
                self.browser_search.truncated = truncated;
                self.browser_search.job = None;
            }
            Ok(_) | Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.browser_search.job = None;
            }
        }
    }

    /// 激活搜索结果条目（目录进入 / 文件加入并播放），并清空搜索态。
    fn activate_browser_search_result(&mut self, index: usize) {
        let Some(entry) = self.browser_search.results.get(index).cloned() else {
            return;
        };
        self.browser_search.query.clear();
        self.browser_search.results.clear();
        self.browser_search.job = None;
        match entry {
            Entry::Dir { path, .. } => {
                let _ = self.browser.navigate_to(&path);
            }
            Entry::File { path, .. } => self.add_and_play(&path),
        }
    }

    /// 播放列表过滤结果的真实下标（保持插入序）。
    fn playlist_filtered_indices(&self) -> Vec<usize> {
        let filter = self.playlist_filter.trim().to_lowercase();
        self.playlist
            .items()
            .iter()
            .enumerate()
            .filter(|(_, item)| playlist_matches(&filter, item))
            .map(|(index, _)| index)
            .collect()
    }

    /// 轮询系统媒体键：回灌播放状态/标题，并按动作分派。
    fn poll_media_keys(&mut self) {
        let events = {
            let Some(handle) = &self.media_keys else {
                return;
            };
            if let Some(playing) = &handle.playing {
                playing.store(self.playing, Ordering::Relaxed);
            }
            if let Some(title) = &handle.title {
                if let Ok(mut guard) = title.lock() {
                    *guard = self
                        .active_cue
                        .as_ref()
                        .map(|cue| cue.title.clone())
                        .or_else(|| {
                            self.current
                                .as_ref()
                                .and_then(|path| path.file_name())
                                .map(|name| name.to_string_lossy().into_owned())
                        })
                        .unwrap_or_default();
                }
            }
            drain_media_key_events(&handle.rx, MEDIA_KEY_DRAIN_LIMIT)
        };
        for event in events {
            self.apply_action_name(event.action());
        }
    }

    /// 按键位动作名执行（自定义键位与媒体键共用）。
    fn apply_action_name(&mut self, name: &str) {
        match name {
            "toggle_play" => self.toggle_play(),
            "next" => {
                self.next_track();
            }
            "prev" => {
                self.prev_track();
            }
            "volume_up" => self.set_volume(self.cfg.volume + 0.05),
            "volume_down" => self.set_volume(self.cfg.volume - 0.05),
            "cycle_repeat" => self.cycle_repeat(),
            "toggle_shuffle" => self.toggle_shuffle(),
            "toggle_browser" => self.view.left = LeftView::Browser,
            "toggle_cover" => {
                self.view.left = match self.view.left {
                    LeftView::Browser => LeftView::Cover,
                    LeftView::Cover => LeftView::Browser,
                };
            }
            "toggle_lyrics" => self.view.lyrics = !self.view.lyrics,
            "cycle_spectrum" => self.view.spectrum = self.view.spectrum.next(),
            "cycle_medium" => self.cycle_medium(),
            "toggle_group_view" => self.toggle_group_view(),
            _ => {}
        }
    }

    fn cycle_repeat(&mut self) {
        self.cfg.repeat = self.cfg.repeat.next();
        self.refresh_preload();
        config::save(&self.cfg);
    }

    fn toggle_shuffle(&mut self) {
        let mode = self.playlist.shuffle_mode().next();
        self.playlist.set_shuffle_mode(mode);
        self.cfg.shuffle_mode = mode.as_str().to_string();
        self.refresh_preload();
        config::save(&self.cfg);
    }

    /// g 键：分组 ↔ 平铺切换（持久化；序号口径随视图切换）。
    fn toggle_group_view(&mut self) {
        self.cfg.playlist_view = match self.cfg.playlist_view {
            tuneux_mediax::PlaylistView::ByAlbum => tuneux_mediax::PlaylistView::Flat,
            tuneux_mediax::PlaylistView::Flat => tuneux_mediax::PlaylistView::ByAlbum,
        };
        config::save(&self.cfg);
    }

    fn toggle_replay_gain(&mut self) {
        self.cfg.replay_gain = !self.cfg.replay_gain;
        if let Some(engine) = &self.engine {
            engine.set_replay_gain_enabled(self.cfg.replay_gain);
        }
        let state = if self.cfg.replay_gain {
            self.i18n.t("msg.rg_on")
        } else {
            self.i18n.t("msg.rg_off")
        };
        self.flash_text(format!("{}：{state}", self.i18n.t("msg.rg_state")));
        config::save(&self.cfg);
    }

    fn cycle_medium(&mut self) {
        let all = PlaybackMedium::ALL;
        let current = self
            .cfg
            .playback_medium
            .parse()
            .unwrap_or(PlaybackMedium::None);
        let index = all
            .iter()
            .position(|medium| *medium == current)
            .unwrap_or(0);
        let next = all[(index + 1) % all.len()];
        self.apply_medium(next);
    }

    fn apply_medium(&mut self, medium: PlaybackMedium) {
        self.cfg.playback_medium = medium.as_str().to_string();
        if let Some(engine) = &self.engine {
            engine.set_medium(medium);
        }
        self.flash_text(format!(
            "{}：{}",
            self.i18n.t("msg.medium"),
            self.i18n.t(medium_label(medium))
        ));
        config::save(&self.cfg);
    }

    /// 执行命令模式输入。
    fn execute_command(&mut self, ctx: &egui::Context, input: &str) {
        match command::parse(input) {
            Ok(Command::Quit) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Ok(Command::Volume(volume)) => self.set_volume(volume),
            Ok(Command::Repeat(mode)) => {
                self.cfg.repeat = mode;
                self.refresh_preload();
                config::save(&self.cfg);
            }
            Ok(Command::Shuffle(mode)) => {
                self.playlist.set_shuffle_mode(mode);
                self.cfg.shuffle_mode = mode.as_str().to_string();
                self.refresh_preload();
                config::save(&self.cfg);
            }
            Ok(Command::Clear) => self.clear_playlist(),
            Ok(Command::About) => self.about_open = true,
            Ok(Command::Help) => {
                let text = format!(
                    "{}：{}",
                    self.i18n.t("command.help_title").into_owned(),
                    command::HELP
                );
                self.flash_text(text);
            }
            Ok(Command::SaveM3u(path)) => {
                let mut paths = Vec::new();
                for item in self.playlist.items() {
                    if !paths.contains(&item.path) {
                        paths.push(item.path.clone());
                    }
                }
                let (content, skipped) = m3u::serialize_with_report(&paths);
                match std::fs::write(&path, content) {
                    Ok(()) => {
                        let mut message = self.i18n.t("msg.m3u_saved").into_owned();
                        if skipped > 0 {
                            let suffix = self
                                .i18n
                                .t("msg.m3u_skipped")
                                .replace("{}", &skipped.to_string());
                            message = format!("{message}（{suffix}）");
                        }
                        self.flash_text(message);
                    }
                    Err(error) => {
                        let prefix = self.i18n.t("msg.save_failed").into_owned();
                        self.flash_text(format!("{prefix}：{error}"));
                    }
                }
            }
            Ok(Command::LoadM3u(path)) => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let paths = m3u::parse(&text);
                    let base = path.parent().unwrap_or(Path::new(""));
                    // 先 stop（保存当前曲断点），再清列表/当前曲。
                    self.stop();
                    self.playlist.clear();
                    self.current = None;
                    self.resume_pending = false;
                    let mut added = 0;
                    for entry in paths {
                        let resolved = if entry.is_absolute() {
                            entry
                        } else {
                            base.join(entry)
                        };
                        let (items, _, _) = self.expand_items(&resolved);
                        added += self.add_items(items);
                    }
                    let prefix = self.i18n.t("msg.m3u_loaded").into_owned();
                    self.flash_text(format!("{prefix}：{added}"));
                }
                Err(error) => {
                    let prefix = self.i18n.t("msg.open_failed").into_owned();
                    self.flash_text(format!("{prefix}：{error}"));
                }
            },
            Err(message) => self.flash_text(message),
        }
    }

    fn toggle_media_keys(&mut self) {
        self.cfg.media_keys_enabled = !self.cfg.media_keys_enabled;
        if self.cfg.media_keys_enabled {
            // Windows 下钩子线程无停止通道、阻塞至进程退出：重复开启不得
            // 再 spawn（否则累积多个全局键盘钩子）。
            if self.media_keys.is_none() {
                self.media_keys =
                    spawn_media_key_listener("org.mpris.MediaPlayer2.tuneux-max", "tuneux-max");
            }
            self.flash("msg.media_keys_on");
        } else {
            self.media_keys = None;
            self.flash("msg.media_keys_off");
        }
        config::save(&self.cfg);
    }

    /// 应用配置记录的皮肤（按名匹配；缺失时回落清单首项「终端原生」，
    /// 无皮肤清单则回落跟随系统深浅色的终端原生视觉）。
    fn apply_configured_skin(&self, ctx: &egui::Context) {
        let selected = self
            .plugins
            .skins
            .iter()
            .find(|loaded| loaded.name == self.cfg.skin)
            .or_else(|| self.plugins.skins.first());
        if let Some(loaded) = selected {
            theme::apply_skin(ctx, &loaded.skin);
        } else {
            theme::apply(ctx);
        }
    }

    /// 重载全部插件（释放旧槽位后重新验签装载）。
    fn reload_plugins(&mut self, ctx: &egui::Context) {
        if let Some(engine) = &self.engine {
            if let Some(eq) = &self.plugins.eq {
                engine.free_eq_slot(eq.slot);
            }
            if let Some(comp) = &self.plugins.comp {
                engine.free_compressor_slot(comp.slot);
            }
        }
        self.plugins = plugins::load_all(self.engine.as_ref());
        self.visual_index = self
            .visual_index
            .filter(|index| *index < self.plugins.visuals.len());
        self.apply_configured_skin(ctx);
        self.flash("msg.plugins_reloaded");
    }

    fn select_default_skin(&mut self, ctx: &egui::Context) {
        self.cfg.skin = "终端原生".to_string();
        self.apply_configured_skin(ctx);
        config::save(&self.cfg);
    }

    /// 选择并应用指定皮肤（按名持久化，重启 / 重载后恢复同一选择）。
    fn select_skin(&mut self, ctx: &egui::Context, name: &str) {
        self.cfg.skin = name.to_string();
        self.apply_configured_skin(ctx);
        config::save(&self.cfg);
    }

    /// 当前可视化插件逐帧 tick（未选 / 频谱隐藏时跳过）。
    fn tick_visual(&mut self) {
        if self.view.spectrum == SpectrumView::Hidden {
            return;
        }
        let Some(index) = self.visual_index else {
            return;
        };
        let Some(engine) = &self.engine else {
            return;
        };
        let spectrum = engine.spectrum_lr();
        let mut bands = [0.0f32; N_BANDS];
        for (index, band) in bands.iter_mut().enumerate() {
            *band = (spectrum[0][index] + spectrum[1][index]) * 0.5;
        }
        let Some(visual) = self.plugins.visuals.get_mut(index) else {
            return;
        };
        visual.plugin.set_meter(&bands);
        if visual.plugin.call_tick().is_ok() {
            if let Some(text) = visual.plugin.read_visual() {
                self.visual_text = text;
            }
        }
    }

    /// 均衡器面板（引擎槽位直控；插件只负责装载验签与默认参数）。
    fn draw_eq_dialog(&mut self, ctx: &egui::Context) {
        if !self.eq_open {
            return;
        }
        let palette = Palette::current(ctx);
        let i18n = &self.i18n;
        let engine = self.engine.as_ref();
        let slot = self.plugins.eq.as_ref().map(|plugin| plugin.slot);
        let mut close = false;
        let shown = ui::dialog::dialog(i18n.t("panel.eq").into_owned())
            .collapsible(false)
            .min_width(520.0)
            .show(ctx, |ui| {
                let Some(engine) = engine else {
                    ui.label(i18n.t("msg.engine_unavailable").into_owned());
                    return;
                };
                let Some(slot) = slot else {
                    ui.label(i18n.t("plugin.none").into_owned());
                    return;
                };
                let slots = engine.eq_slots();
                let Some(params) = slots.get(slot as usize) else {
                    ui.label(i18n.t("plugin.none").into_owned());
                    return;
                };
                for band in 0..EQ_BANDS {
                    let frequency = EQ_FREQS.get(band).copied().unwrap_or(0.0);
                    let mut gain = params.band(band);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{frequency:>6.0} Hz"))
                                .monospace()
                                .color(palette.dim),
                        );
                        let slider = egui::Slider::new(&mut gain, EQ_GAIN_MIN_DB..=EQ_GAIN_MAX_DB);
                        if ui.add(slider).changed() {
                            engine.set_eq_band(slot, band as u32, gain);
                        }
                    });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button(i18n.t("eq.reset").into_owned()).clicked() {
                        for band in 0..EQ_BANDS {
                            engine.set_eq_band(slot, band as u32, 0.0);
                        }
                    }
                    let enabled = params.enabled();
                    if ui.button(i18n.t("eq.bypass").into_owned()).clicked() {
                        engine.set_eq_enabled(slot, !enabled);
                    }
                    if ui.button(i18n.t("common.close").into_owned()).clicked() {
                        close = true;
                    }
                });
            });
        if shown.is_none() || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.eq_open = false;
        }
    }

    /// 压缩器面板（引擎槽位直控）。
    fn draw_comp_dialog(&mut self, ctx: &egui::Context) {
        if !self.comp_open {
            return;
        }
        let i18n = &self.i18n;
        let engine = self.engine.as_ref();
        let slot = self.plugins.comp.as_ref().map(|plugin| plugin.slot);
        let mut close = false;
        let shown = ui::dialog::dialog(i18n.t("panel.compressor").into_owned())
            .collapsible(false)
            .min_width(420.0)
            .show(ctx, |ui| {
                let Some(engine) = engine else {
                    ui.label(i18n.t("msg.engine_unavailable").into_owned());
                    return;
                };
                let Some(slot) = slot else {
                    ui.label(i18n.t("plugin.none").into_owned());
                    return;
                };
                let slots = engine.compressor_slots();
                let Some(params) = slots.get(slot as usize) else {
                    ui.label(i18n.t("plugin.none").into_owned());
                    return;
                };
                let mut threshold = params.threshold();
                if ui
                    .add(
                        egui::Slider::new(&mut threshold, -60.0..=0.0)
                            .text(i18n.t("comp.threshold").into_owned()),
                    )
                    .changed()
                {
                    params.set_threshold(threshold);
                }
                let mut ratio = params.ratio();
                if ui
                    .add(
                        egui::Slider::new(&mut ratio, 1.0..=20.0)
                            .text(i18n.t("comp.ratio").into_owned()),
                    )
                    .changed()
                {
                    params.set_ratio(ratio);
                }
                let mut attack = params.attack_ms();
                if ui
                    .add(
                        egui::Slider::new(&mut attack, 1.0..=200.0)
                            .text(i18n.t("comp.attack").into_owned()),
                    )
                    .changed()
                {
                    params.set_attack_ms(attack);
                }
                let mut release = params.release_ms();
                if ui
                    .add(
                        egui::Slider::new(&mut release, 10.0..=1000.0)
                            .text(i18n.t("comp.release").into_owned()),
                    )
                    .changed()
                {
                    params.set_release_ms(release);
                }
                let mut makeup = params.makeup();
                if ui
                    .add(
                        egui::Slider::new(&mut makeup, 0.0..=24.0)
                            .text(i18n.t("comp.makeup").into_owned()),
                    )
                    .changed()
                {
                    params.set_makeup(makeup);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let enabled = params.enabled();
                    if ui.button(i18n.t("comp.enabled").into_owned()).clicked() {
                        params.set_enabled(!enabled);
                    }
                    if ui.button(i18n.t("common.close").into_owned()).clicked() {
                        close = true;
                    }
                });
            });
        if shown.is_none() || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.comp_open = false;
        }
    }

    /// 插件管理：清单、授予能力、重载入口。
    fn draw_plugins_dialog(&mut self, ctx: &egui::Context) {
        if !self.plugins_open {
            return;
        }
        let i18n = &self.i18n;
        let plugins = &self.plugins;
        let none = i18n.t("plugin.none").into_owned();
        let granted_label = i18n.t("plugin.granted").into_owned();
        let mut close = false;
        let mut reload = false;
        let shown = ui::dialog::dialog(i18n.t("panel.plugins").into_owned())
            .collapsible(false)
            .min_width(460.0)
            .show(ctx, |ui| {
                let mut eq_lines = Vec::new();
                if let Some(plugin) = &plugins.eq {
                    let caps = plugin
                        .granted
                        .iter()
                        .map(|capability| capability.name())
                        .collect::<Vec<_>>()
                        .join(", ");
                    eq_lines.push(format!("{} · {granted_label}: {caps}", plugin.stem));
                }
                plugin_section(ui, i18n.t("plugin.eq").as_ref(), &none, &eq_lines);
                let mut comp_lines = Vec::new();
                if let Some(plugin) = &plugins.comp {
                    let caps = plugin
                        .granted
                        .iter()
                        .map(|capability| capability.name())
                        .collect::<Vec<_>>()
                        .join(", ");
                    comp_lines.push(format!("{} · {granted_label}: {caps}", plugin.stem));
                }
                plugin_section(ui, i18n.t("plugin.compressor").as_ref(), &none, &comp_lines);
                let skins: Vec<String> = plugins
                    .skins
                    .iter()
                    .map(|skin| format!("{} ({})", skin.name, skin.stem))
                    .collect();
                plugin_section(ui, i18n.t("plugin.skins").as_ref(), &none, &skins);
                let visuals: Vec<String> = plugins
                    .visuals
                    .iter()
                    .map(|visual| visual.stem.clone())
                    .collect();
                plugin_section(ui, i18n.t("plugin.visuals").as_ref(), &none, &visuals);
                ui.horizontal(|ui| {
                    if ui
                        .button(i18n.t("menu.reload_plugins").into_owned())
                        .clicked()
                    {
                        reload = true;
                    }
                    if ui.button(i18n.t("common.close").into_owned()).clicked() {
                        close = true;
                    }
                });
            });
        if shown.is_none() || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.plugins_open = false;
        }
        if reload {
            self.reload_plugins(ctx);
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // 命令输入条优先：Enter 执行、Esc 取消，其余按键交给文本输入。
        if self.command_open {
            let enter = ctx.input(|input| input.key_pressed(egui::Key::Enter));
            let escape = ctx.input(|input| input.key_pressed(egui::Key::Escape));
            if escape {
                self.command_open = false;
                self.command_input.clear();
            } else if enter {
                let input = std::mem::take(&mut self.command_input);
                self.command_open = false;
                self.execute_command(ctx, &input);
            }
            return;
        }
        // 文本输入态不触发全局快捷键；搜索框的 Enter 只激活当前结果。
        if ctx.wants_keyboard_input() && !ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            if self.browser_search.active() && self.browser_search.input_focus {
                if ctx.input(|input| input.key_pressed(egui::Key::Enter)) {
                    self.activate_browser_search_result(self.browser_search.selected);
                } else if ctx.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                    self.browser_search.selected = self.browser_search.selected.saturating_sub(1);
                    self.browser_scroll_pending = true;
                } else if ctx.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                    let max = self.browser_search.results.len().saturating_sub(1);
                    self.browser_search.selected = (self.browser_search.selected + 1).min(max);
                    self.browser_scroll_pending = true;
                }
            }
            return;
        }
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            if repeat {
                continue;
            }
            // 保留键：Ctrl/Cmd+Q 退出，不受 keymap 覆盖。
            if key == egui::Key::Q && (modifiers.ctrl || modifiers.command) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                continue;
            }
            // 自定义键位优先于内置默认。
            if let Some(desc) = key_desc(key, modifiers) {
                let action = self
                    .cfg
                    .keymap
                    .iter()
                    .find_map(|(action, mapped)| (mapped == &desc).then(|| action.clone()));
                if let Some(action) = action {
                    self.apply_action_name(&action);
                    continue;
                }
            }
            if modifiers.ctrl || modifiers.command || modifiers.alt {
                continue;
            }
            match key {
                egui::Key::Space => self.toggle_play(),
                egui::Key::Enter => {
                    if self.browser_search.active() {
                        self.activate_browser_search_result(self.browser_search.selected);
                    } else {
                        self.activate_browser_entry(self.browser.selected());
                    }
                }
                egui::Key::ArrowUp | egui::Key::K => {
                    if self.browser_search.active() {
                        self.browser_search.selected =
                            self.browser_search.selected.saturating_sub(1);
                    } else {
                        self.browser.move_up();
                    }
                    self.browser_scroll_pending = true;
                }
                egui::Key::ArrowDown | egui::Key::J => {
                    if self.browser_search.active() {
                        let max = self.browser_search.results.len().saturating_sub(1);
                        self.browser_search.selected = (self.browser_search.selected + 1).min(max);
                    } else {
                        self.browser.move_down();
                    }
                    self.browser_scroll_pending = true;
                }
                egui::Key::Backspace => self.browser_go_parent(),
                egui::Key::Delete | egui::Key::D => self.remove_selected(),
                egui::Key::ArrowLeft => self.seek_by(-5.0),
                egui::Key::ArrowRight => self.seek_by(5.0),
                egui::Key::Plus | egui::Key::Equals => self.set_volume(self.cfg.volume + 0.05),
                egui::Key::Minus => self.set_volume(self.cfg.volume - 0.05),
                egui::Key::P => {
                    self.prev_track();
                }
                egui::Key::N => {
                    self.next_track();
                }
                egui::Key::R => self.cycle_repeat(),
                egui::Key::S => self.toggle_shuffle(),
                egui::Key::M => self.cycle_medium(),
                egui::Key::G => self.toggle_group_view(),
                egui::Key::A => self.add_selected_browser(),
                egui::Key::X => self.clear_playlist(),
                egui::Key::Q => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                egui::Key::B => self.view.left = LeftView::Browser,
                egui::Key::C => {
                    self.view.left = match self.view.left {
                        LeftView::Browser => LeftView::Cover,
                        LeftView::Cover => LeftView::Browser,
                    };
                }
                egui::Key::L => self.view.lyrics = !self.view.lyrics,
                egui::Key::E => self.eq_open = !self.eq_open,
                egui::Key::V => self.view.spectrum = self.view.spectrum.next(),
                egui::Key::Questionmark => self.about_open = !self.about_open,
                egui::Key::Slash if modifiers.shift => self.about_open = true,
                egui::Key::Slash => {
                    self.view.left = LeftView::Browser;
                    self.search_focus = true;
                }
                egui::Key::Colon => {
                    self.command_open = true;
                    self.command_input.clear();
                    self.command_focus = true;
                }
                egui::Key::Escape => {
                    self.browser_search.query.clear();
                    self.browser_search.results.clear();
                    self.browser_search.job = None;
                    self.playlist_filter.clear();
                }
                _ => {}
            }
        }
    }

    fn apply_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::None => {}
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::TogglePlay => self.toggle_play(),
            Action::Stop => self.stop(),
            Action::Prev => {
                self.prev_track();
            }
            Action::Next => {
                self.next_track();
            }
            Action::AddSelected => self.add_selected_browser(),
            Action::RemoveSelected => self.remove_selected(),
            Action::ClearPlaylist => self.clear_playlist(),
            Action::SetLangZh => self.set_lang("zh"),
            Action::SetLangEn => self.set_lang("en"),
            Action::About => self.about_open = true,
            Action::ShowBrowser => self.view.left = LeftView::Browser,
            Action::ToggleCover => {
                self.view.left = match self.view.left {
                    LeftView::Browser => LeftView::Cover,
                    LeftView::Cover => LeftView::Browser,
                };
            }
            Action::ToggleLyrics => self.view.lyrics = !self.view.lyrics,
            Action::CycleSpectrum => self.view.spectrum = self.view.spectrum.next(),
            Action::ToggleMediaKeys => self.toggle_media_keys(),
            Action::OpenCommand => {
                self.command_open = true;
                self.command_input.clear();
                self.command_focus = true;
            }
            Action::OpenEq => self.eq_open = true,
            Action::OpenCompressor => self.comp_open = true,
            Action::OpenPlugins => self.plugins_open = true,
            Action::ReloadPlugins => self.reload_plugins(ctx),
            Action::SetSkinTerminal => self.select_default_skin(ctx),
            Action::SetSkin(name) => self.select_skin(ctx, &name),
            Action::ToggleReplayGain => self.toggle_replay_gain(),
            Action::SetMedium(medium) => self.apply_medium(medium),
            Action::SetVisualBuiltin => {
                self.visual_index = None;
                self.visual_text.clear();
            }
            Action::SetVisual(index) => {
                self.visual_index = Some(index);
                self.visual_text.clear();
            }
        }
    }

    fn set_lang(&mut self, lang: &str) {
        if self.cfg.lang == lang {
            return;
        }
        self.cfg.lang = lang.to_string();
        self.i18n = i18n::build(lang);
        config::save(&self.cfg);
    }

    fn persist(&mut self) {
        if let Some(engine) = &self.engine {
            self.cfg.volume = engine.volume();
        }
        self.cfg.last_dir = Some(self.browser.cwd().to_path_buf());
        self.cfg.shuffle_mode = self.playlist.shuffle_mode().as_str().to_string();
        config::save(&self.cfg);
        let items = self.playlist.items().to_vec();
        let mut state = self.state.clone();
        // 保留其它已保存列表，只更新活跃列表（旧实现整体覆盖会丢列表）。
        let active = state.active.min(state.lists.len().saturating_sub(1));
        let name = state
            .lists
            .get(active)
            .map(|list| list.name.clone())
            .unwrap_or_else(|| "默认".to_string());
        let saved = SavedList {
            name,
            items,
            current: self.current.clone(),
            current_cue: self
                .playlist
                .current_index()
                .and_then(|index| self.playlist.items().get(index))
                .and_then(|item| item.cue.as_ref())
                .map(|cue| cue.index),
            shuffle: self.playlist.is_shuffle(),
            shuffle_mode: self.playlist.shuffle_mode().as_str().to_string(),
        };
        let mut lists = std::mem::take(&mut state.lists);
        if let Some(slot) = lists.get_mut(active) {
            *slot = saved;
        } else {
            lists.push(saved);
        }
        state.lists = lists;
        state.active = active;
        state.current = None;
        state.items.clear();
        state.shuffle = false;
        // 只有确实在播放或已有有效进度时才覆盖断点；启动/暂停在 0 位置
        // 定时保存不得把上次断点清掉（与 fx 的落盘口径一致）。
        if self.active_cue.is_none() && (self.playing || self.position > 0.5) {
            if let Some(path) = &self.current {
                state.save_position(path, self.position);
            }
        }
        state.positions.retain(|path, _| path.exists());
        self.replay_gain_cache.retain(|path, _| path.exists());
        state.replay_gain = self.replay_gain_cache.clone();
        self.state = state.clone();
        config::save_state(&state);
    }

    /// 顶部三栏条：当前曲目 60% / 三桶频段 15% / 电平表 25%。
    fn draw_top_strip(&mut self, ctx: &egui::Context) {
        let palette = Palette::current(ctx);
        let i18n = &self.i18n;
        let spectrum = self
            .engine
            .as_ref()
            .map(|engine| engine.spectrum_lr())
            .unwrap_or([[0.0f32; N_BANDS]; 2]);
        let levels = self
            .engine
            .as_ref()
            .map(|engine| engine.level_lr())
            .unwrap_or((0.0, 0.0));
        let (title, artist, album, tech) = self.track_info_lines();
        let progress = if self.duration > 0.0 {
            (self.position / self.duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let time_text = format!("{} / {}", fmt_time(self.position), fmt_time(self.duration));
        let state_text = if self.playing {
            i18n.t("play.playing").into_owned()
        } else if self.position > 0.0 {
            i18n.t("play.paused").into_owned()
        } else {
            i18n.t("play.stopped").into_owned()
        };
        egui::TopBottomPanel::top("top_strip")
            .exact_height(104.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                let width = ui.available_width();
                ui.horizontal(|ui| {
                    let w1 = (width * 0.60).max(200.0);
                    let w2 = (width * 0.15).max(80.0);
                    let w3 = (width - w1 - w2 - 16.0).max(120.0);
                    column(ui, w1, |ui| {
                        ui.label(
                            egui::RichText::new(title)
                                .monospace()
                                .color(palette.fg)
                                .size(16.0),
                        );
                        ui.label(
                            egui::RichText::new(format!("{artist}    {album}"))
                                .monospace()
                                .color(palette.dim),
                        );
                        ui.label(egui::RichText::new(tech).monospace().color(palette.dim));
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(state_text)
                                    .monospace()
                                    .color(palette.dim),
                            );
                            ui.label(
                                egui::RichText::new(time_text)
                                    .monospace()
                                    .color(palette.dim),
                            );
                        });
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .desired_width(ui.available_width().max(10.0))
                                .text(""),
                        );
                    });
                    column(ui, w2, |ui| {
                        ui.label(
                            egui::RichText::new(i18n.t("panel.bands").into_owned())
                                .monospace()
                                .color(palette.dim),
                        );
                        let size = ui.available_size();
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        // 三桶频段（与 fx 同语义）：高在上 / 中 / 低在下，
                        // 每桶取最大值、按桶配色，横向能量条呈现。
                        let maxima = band_maxima(&spectrum);
                        let rows = [
                            (
                                i18n.t("spectrum.high").into_owned(),
                                maxima[2],
                                palette.band_high,
                            ),
                            (
                                i18n.t("spectrum.mid").into_owned(),
                                maxima[1],
                                palette.band_mid,
                            ),
                            (
                                i18n.t("spectrum.low").into_owned(),
                                maxima[0],
                                palette.band_low,
                            ),
                        ];
                        paint_bands(ui, rect, &rows, palette);
                    });
                    column(ui, w3, |ui| {
                        ui.label(
                            egui::RichText::new(i18n.t("panel.level").into_owned())
                                .monospace()
                                .color(palette.dim),
                        );
                        let size = ui.available_size();
                        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                        let half = rect.height() / 2.0;
                        let left_rect =
                            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), half));
                        let right_rect = egui::Rect::from_min_size(
                            rect.min + egui::vec2(0.0, half),
                            egui::vec2(rect.width(), half),
                        );
                        paint_meter(ui, left_rect, "L", levels.0, palette);
                        paint_meter(ui, right_rect, "R", levels.1, palette);
                    });
                });
            });
    }

    fn track_info_lines(&self) -> (String, String, String, String) {
        let fallback = self
            .current
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.i18n.t("msg.empty_selection").into_owned());
        let Some(metadata) = &self.metadata else {
            let title = self
                .active_cue
                .as_ref()
                .map(|cue| cue.title.clone())
                .unwrap_or(fallback);
            return (title, String::new(), String::new(), String::new());
        };
        let mut title = metadata.title.clone().unwrap_or(fallback);
        let mut artist = metadata.artist.clone().unwrap_or_default();
        if let Some(cue) = &self.active_cue {
            title = cue.title.clone();
            if let Some(performer) = &cue.performer {
                artist = performer.clone();
            }
        }
        let album = metadata.album.clone().unwrap_or_default();
        // 仿 fx / 基础版口径：编码 · 码率 · 采样率 · 位深/有损。元数据优先，
        // 缺失段回落引擎实测源参数（FFmpeg 长尾格式探测兜底时仍有真实参数）。
        // 无损显 "N bit"、有损显 "有损"，码率恒显示；时长在进度区单独呈现，
        // 不入 tech 行（与 fx 一致）。
        let eng_status = self.engine.as_ref().map(|e| e.status());
        let eng_codec = self.engine.as_ref().and_then(|e| e.src_codec());
        let tech = metadata.tech_line_with_engine(
            eng_codec.as_deref(),
            eng_status.and_then(|s| s.src_sample_rate),
            eng_status.and_then(|s| s.src_bits),
            &self.i18n.t("msg.unknown_format"),
        );
        (title, artist, album, tech)
    }

    /// 工作区底部频谱面板（关 / 半屏 / 全屏）。
    fn draw_spectrum_panel(&mut self, ctx: &egui::Context) {
        if self.view.spectrum == SpectrumView::Hidden {
            return;
        }
        let palette = Palette::current(ctx);
        let i18n = &self.i18n;
        let spectrum = self
            .engine
            .as_ref()
            .map(|engine| engine.spectrum_lr())
            .unwrap_or([[0.0f32; N_BANDS]; 2]);
        let height = (ctx.screen_rect().height() * self.view.spectrum.height_ratio()).max(140.0);
        let label = format!(
            "{} · {}",
            i18n.t("panel.spectrum").into_owned(),
            i18n.t(self.view.spectrum.label_key()).into_owned()
        );
        let visual_active = self.visual_index.is_some();
        let visual_text = self.visual_text.clone();
        let visual_none = i18n.t("plugin.none").into_owned();
        egui::TopBottomPanel::bottom("spectrum_view")
            .exact_height(height)
            .show(ctx, |ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(label).monospace().color(palette.dim));
                if visual_active {
                    if visual_text.is_empty() {
                        ui.label(
                            egui::RichText::new(visual_none)
                                .monospace()
                                .color(palette.dim),
                        );
                    } else {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for line in visual_text.lines() {
                                ui.label(
                                    egui::RichText::new(line)
                                        .monospace()
                                        .size(12.0)
                                        .color(palette.fg),
                                );
                            }
                        });
                    }
                } else {
                    let size = ui.available_size();
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    // L/R 平均合并单声道（与 fx / 共享 TUI 渲染同口径），
                    // 峰帽经 corex 按秒衰减更新（dt 取 egui 帧间隔）。
                    let merged = merge_spectrum_channels(&spectrum);
                    let dt = ctx.input(|input| input.unstable_dt).max(0.0);
                    let peaks = self
                        .spectrum_peaks
                        .update(&merged, std::time::Duration::from_secs_f32(dt));
                    paint_spectrum_chars(
                        ui,
                        rect,
                        &merged,
                        &peaks,
                        ctx.cumulative_frame_nr(),
                        palette,
                    );
                }
            });
    }

    /// 左侧面板：浏览器或封面。
    fn draw_left_panel(
        &mut self,
        ctx: &egui::Context,
        select: &mut Option<usize>,
        activate: &mut Option<usize>,
        browser_ctx: &mut Option<BrowserCtx>,
    ) {
        match self.view.left {
            LeftView::Browser => self.draw_browser(ctx, select, activate, browser_ctx),
            LeftView::Cover => self.draw_cover(ctx),
        }
    }

    fn draw_browser(
        &mut self,
        ctx: &egui::Context,
        select: &mut Option<usize>,
        activate: &mut Option<usize>,
        ctx_action: &mut Option<BrowserCtx>,
    ) {
        let palette = Palette::current(ctx);
        let ctx_add_label = self.i18n.t("action.add").into_owned();
        let ctx_add_play_label = self.i18n.t("ctx.add_play").into_owned();
        let ctx_open_label = self.i18n.t("ctx.open").into_owned();
        let ctx_add_folder_label = self.i18n.t("ctx.add_folder").into_owned();
        let cwd = self.browser.cwd().to_path_buf();
        let browser_title = format!(
            "{} · {}",
            self.i18n.t("panel.browser").into_owned(),
            cwd.display()
        );
        let search_hint = self.i18n.t("search.hint").into_owned();
        let searching_label = self.i18n.t("search.searching").into_owned();
        let none_label = self.i18n.t("search.none").into_owned();
        let truncated_label = self.i18n.t("search.truncated").into_owned();
        let empty_label = self.i18n.t("empty.browser").into_owned();
        let width = (ctx.screen_rect().width() * self.cfg.browser_ratio).max(220.0);
        let search_active = self.browser_search.active();
        let search_results = self.browser_search.results.clone();
        let search_selected = self.browser_search.selected;
        let browser_selected = self.browser.selected();
        let browser_scroll = self.browser_scroll_pending;
        let browser_entries = self.browser.entries().to_vec();
        let searching = self.browser_search.searching();
        let truncated = self.browser_search.truncated;
        let mut query_changed = false;
        let mut clear_search = false;
        let mut go_parent = false;
        let mut focus_pending = self.search_focus;
        self.search_focus = false;
        egui::SidePanel::left("browser_panel")
            .exact_width(width)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button(self.i18n.t("browser.up").into_owned()).clicked() {
                        go_parent = true;
                    }
                    ui.label(
                        egui::RichText::new(browser_title.clone())
                            .monospace()
                            .color(palette.dim),
                    );
                });
                ui.horizontal(|ui| {
                    // 先画清空按钮，再让输入框吃掉“按钮之后”的剩余宽度：
                    // 输入框宽度若按整行可用宽度计算，加上按钮会超出面板；
                    // egui 的 SidePanel 会把内容实际宽度存为下一帧面板宽度，
                    // 每次重绘就向右膨胀一次。
                    if ui.button("×").clicked() {
                        clear_search = true;
                    }
                    let edit_width = ui.available_width().max(40.0);
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.browser_search.query)
                            .hint_text(search_hint.clone())
                            .desired_width(edit_width),
                    );
                    if focus_pending {
                        response.request_focus();
                        focus_pending = false;
                    }
                    self.browser_search.input_focus = response.has_focus();
                    if response.changed() {
                        query_changed = true;
                    }
                });
                ui.separator();
                if search_active {
                    if searching {
                        ui.label(egui::RichText::new(searching_label.clone()).color(palette.dim));
                    } else if search_results.is_empty() {
                        ui.label(egui::RichText::new(none_label.clone()).color(palette.dim));
                    } else {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for (index, entry) in search_results.iter().enumerate() {
                                    let path = entry.path();
                                    let path_buf = path.to_path_buf();
                                    let is_dir = entry.is_dir();
                                    let label = match path.strip_prefix(&cwd) {
                                        Ok(relative) => relative.display().to_string(),
                                        Err(_) => path.display().to_string(),
                                    };
                                    let label = if is_dir {
                                        format!("▸ {label}")
                                    } else {
                                        label
                                    };
                                    let row_id = egui::Id::new(("browser_search_row", index));
                                    let response = ui::row::selectable_row(
                                        ui,
                                        row_id,
                                        egui::vec2(ui.available_width(), 22.0),
                                        index == search_selected,
                                        &label,
                                        egui::Sense::click_and_drag(),
                                    );
                                    if browser_scroll && index == search_selected {
                                        response.scroll_to_me(Some(egui::Align::Center));
                                    }
                                    if ui.ctx().is_being_dragged(row_id) {
                                        egui::DragAndDrop::set_payload(
                                            ui.ctx(),
                                            DragEntry {
                                                path: path_buf.clone(),
                                                is_dir,
                                            },
                                        );
                                    }
                                    if response.clicked() {
                                        *select = Some(index);
                                    }
                                    if response.double_clicked() {
                                        *activate = Some(index);
                                    }
                                    response.context_menu(|ui| {
                                        if is_dir {
                                            if ui.button(ctx_open_label.as_str()).clicked() {
                                                *ctx_action =
                                                    Some(BrowserCtx::EnterDir(path_buf.clone()));
                                                ui.close();
                                            }
                                            if ui.button(ctx_add_folder_label.as_str()).clicked() {
                                                *ctx_action =
                                                    Some(BrowserCtx::AddDir(path_buf.clone()));
                                                ui.close();
                                            }
                                        } else {
                                            if ui.button(ctx_add_label.as_str()).clicked() {
                                                *ctx_action =
                                                    Some(BrowserCtx::Add(path_buf.clone()));
                                                ui.close();
                                            }
                                            if ui.button(ctx_add_play_label.as_str()).clicked() {
                                                *ctx_action =
                                                    Some(BrowserCtx::AddPlay(path_buf.clone()));
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                            });
                        if truncated {
                            ui.label(
                                egui::RichText::new(truncated_label.clone()).color(palette.dim),
                            );
                        }
                    }
                } else {
                    if browser_entries.is_empty() {
                        ui.label(egui::RichText::new(empty_label.clone()).color(palette.dim));
                        return;
                    }
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for (index, entry) in browser_entries.iter().enumerate() {
                                let is_dir = entry.is_dir();
                                let path = entry.path().to_path_buf();
                                let label = if is_dir {
                                    format!("▸ {}", entry.name())
                                } else {
                                    entry.name().to_string()
                                };
                                // 单一 click_and_drag widget：单击选中/双击激活/
                                // 右键菜单/左键拖拽互不吞事件。
                                let row_id = egui::Id::new(("browser_row", index));
                                let response = ui::row::selectable_row(
                                    ui,
                                    row_id,
                                    egui::vec2(ui.available_width(), 22.0),
                                    index == browser_selected,
                                    &label,
                                    egui::Sense::click_and_drag(),
                                );
                                if browser_scroll && index == browser_selected {
                                    response.scroll_to_me(Some(egui::Align::Center));
                                }
                                // 拖拽中：设置 dnd 载荷，供播放列表放置区接收。
                                if ui.ctx().is_being_dragged(row_id) {
                                    egui::DragAndDrop::set_payload(
                                        ui.ctx(),
                                        DragEntry {
                                            path: path.clone(),
                                            is_dir,
                                        },
                                    );
                                }
                                if response.clicked() {
                                    *select = Some(index);
                                }
                                if response.double_clicked() {
                                    *activate = Some(index);
                                }
                                // 右键菜单：与快捷键 a / 拖拽并行的第三种加入方式。
                                response.context_menu(|ui| {
                                    if is_dir {
                                        if ui.button(ctx_open_label.as_str()).clicked() {
                                            *ctx_action = Some(BrowserCtx::EnterDir(path.clone()));
                                            ui.close();
                                        }
                                        if ui.button(ctx_add_folder_label.as_str()).clicked() {
                                            *ctx_action = Some(BrowserCtx::AddDir(path.clone()));
                                            ui.close();
                                        }
                                    } else {
                                        if ui.button(ctx_add_label.as_str()).clicked() {
                                            *ctx_action = Some(BrowserCtx::Add(path.clone()));
                                            ui.close();
                                        }
                                        if ui.button(ctx_add_play_label.as_str()).clicked() {
                                            *ctx_action = Some(BrowserCtx::AddPlay(path.clone()));
                                            ui.close();
                                        }
                                    }
                                });
                            }
                        });
                }
            });
        // 滚动跟随为一次性：绘后清除，避免之后手动滚动被逐帧拉回。
        self.browser_scroll_pending = false;
        if query_changed {
            if self.browser_search.active() {
                // 防抖：每敲一个字符就全树递归扫描会并发堆积 IO 线程；
                // 200ms 无输入才真正启动（update 中检查到期）。
                self.search_debounce = Some(Instant::now());
            } else {
                self.browser_search.results.clear();
                self.browser_search.job = None;
            }
        }
        if clear_search {
            self.browser_search.query.clear();
            self.browser_search.results.clear();
            self.browser_search.job = None;
        }
        if go_parent {
            self.browser_go_parent();
        }
    }

    fn draw_cover(&mut self, ctx: &egui::Context) {
        let palette = Palette::current(ctx);
        let i18n = &self.i18n;
        let width = (ctx.screen_rect().width() * self.cfg.browser_ratio).max(220.0);
        egui::SidePanel::left("cover_panel")
            .exact_width(width)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(i18n.t("panel.cover").into_owned())
                        .monospace()
                        .color(palette.dim),
                );
                ui.separator();
                let Some(texture) = &self.cover_texture else {
                    ui.label(
                        egui::RichText::new(i18n.t("empty.cover").into_owned()).color(palette.dim),
                    );
                    return;
                };
                let available = ui.available_size();
                let size = fit_size(texture.size_vec2(), available);
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    texture.id(),
                    size,
                )));
            });
    }

    /// 右侧歌词面板。
    fn draw_lyrics_panel(&mut self, ctx: &egui::Context) {
        if !self.view.lyrics {
            return;
        }
        let palette = Palette::current(ctx);
        let i18n = &self.i18n;
        let position = self.position;
        let current = self
            .lyrics
            .as_ref()
            .map(|lyrics| lyrics.current_line(position));
        egui::SidePanel::right("lyrics_panel")
            .exact_width(320.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(i18n.t("panel.lyrics").into_owned())
                        .monospace()
                        .color(palette.dim),
                );
                ui.separator();
                let Some(lyrics) = &self.lyrics else {
                    ui.label(
                        egui::RichText::new(i18n.t("empty.lyrics").into_owned()).color(palette.dim),
                    );
                    return;
                };
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for (index, line) in lyrics.lines.iter().enumerate() {
                            let highlighted = current == Some(index);
                            let color = if highlighted { palette.fg } else { palette.dim };
                            let response = ui.label(
                                egui::RichText::new(&line.text)
                                    .monospace()
                                    .color(color)
                                    .size(if highlighted { 16.0 } else { 14.0 }),
                            );
                            if highlighted {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                        }
                    });
            });
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_playlist(
        &mut self,
        ctx: &egui::Context,
        action: &mut Action,
        select: &mut Option<usize>,
        activate: &mut Option<usize>,
        ctx_action: &mut Option<PlaylistCtx>,
        dropped: &mut Option<DragEntry>,
        collapse: &mut Option<String>,
    ) {
        let palette = Palette::current(ctx);
        // 只补未缓存项（暖缓存后无读盘），保证任意加入入口的曲目都有标题。
        self.ensure_metadata_cache();
        let i18n = &self.i18n;
        let ctx_play_label = self.i18n.t("ctx.play").into_owned();
        let ctx_remove_label = self.i18n.t("ctx.remove").into_owned();
        let unknown_album = self.i18n.t("group.unknown_album").into_owned();
        let track_count_fmt = self.i18n.t("group.track_count").into_owned();
        let items = self.playlist.items().to_vec();
        let selected = self.playlist_sel;
        let playlist_scroll = self.playlist_scroll_pending;
        let filter_active = !self.playlist_filter.trim().is_empty();
        // 过滤态退化为平铺：分组下命中曲目可能被折叠 / 空组隐藏，平铺保证
        // 命中全可见；序号仍读全局预扫描（与过滤 / 折叠无关）。
        let effective_view = if filter_active {
            tuneux_mediax::PlaylistView::Flat
        } else {
            self.cfg.playlist_view
        };
        let mut rows = playlist_rows::visible_rows(
            playlist_rows::display_rows(&self.playlist, effective_view),
            &self.collapsed_albums,
        );
        let filtered = self.playlist_filtered_indices();
        if filter_active {
            let keep: HashSet<usize> = filtered.iter().copied().collect();
            rows.retain(|row| match row {
                PlaylistRow::Track { item_index } => keep.contains(item_index),
                PlaylistRow::AlbumHeader { .. } => false,
            });
        }
        let sequences = playlist_rows::track_sequences(&self.playlist, effective_view);
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}（{}）",
                        i18n.t("panel.playlist").into_owned(),
                        items.len()
                    ))
                    .monospace()
                    .color(palette.dim),
                );
                if ui.button(i18n.t("action.add").into_owned()).clicked() {
                    *action = Action::AddSelected;
                }
                if ui.button(i18n.t("action.remove").into_owned()).clicked() {
                    *action = Action::RemoveSelected;
                }
                if ui.button(i18n.t("action.clear").into_owned()).clicked() {
                    *action = Action::ClearPlaylist;
                }
                let filter_width = ui.available_width().clamp(60.0, 200.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.playlist_filter)
                        .hint_text(i18n.t("playlist.filter").into_owned())
                        .desired_width(filter_width),
                );
            });
            ui.separator();
            // 拖拽放置区：浏览器条目拖到此处即加入（空列表也可放置）。
            let (_drop_inner, dropped_payload) =
                ui.dnd_drop_zone::<DragEntry, _>(egui::Frame::default().inner_margin(0.0), |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if items.is_empty() {
                                ui.label(
                                    egui::RichText::new(i18n.t("empty.playlist").into_owned())
                                        .color(palette.dim),
                                );
                            } else if rows.is_empty() {
                                ui.label(
                                    egui::RichText::new(i18n.t("search.none").into_owned())
                                        .color(palette.dim),
                                );
                            } else {
                                for row in &rows {
                                    match row {
                                        PlaylistRow::AlbumHeader {
                                            album,
                                            track_indices,
                                        } => {
                                            // 组头：折叠箭头 + 粗体专辑名 + 曲数；
                                            // 点击折叠 / 展开（不参与选中）。
                                            let collapsed = self.collapsed_albums.contains(album);
                                            let name = if album.is_empty() {
                                                unknown_album.clone()
                                            } else {
                                                album.clone()
                                            };
                                            let count = track_count_fmt
                                                .replace("{}", &track_indices.len().to_string());
                                            let arrow = if collapsed { "▸" } else { "▾" };
                                            let rich = egui::RichText::new(format!(
                                                "{arrow} {name} ({count})"
                                            ))
                                            .monospace()
                                            .size(14.0)
                                            .strong()
                                            .color(palette.fg);
                                            let response = ui::row::header_row(
                                                ui,
                                                egui::vec2(ui.available_width(), 22.0),
                                                rich,
                                            );
                                            if response.clicked() {
                                                *collapse = Some(album.clone());
                                            }
                                        }
                                        PlaylistRow::Track { item_index } => {
                                            let index = *item_index;
                                            let Some(item) = items.get(index) else {
                                                continue;
                                            };
                                            // 标题仿 fx：CUE 用 cue.title、非 CUE
                                            // 用元数据标题回落文件名去扩展名。
                                            let title = track_title(
                                                item,
                                                self.metadata_cache.get(&item.path),
                                            );
                                            // 序号：分组 = 专辑内 1..N，平铺 = 全局
                                            // 大排行；右对齐 + "." 仿 fx 列观感。
                                            let seq = sequences.get(&index).copied().unwrap_or(0);
                                            // 艺术家与时长（仿 fx 列口径）：CUE 用
                                            // PERFORMER / 区间长度；非 CUE 用元数据。
                                            let md = self.metadata_cache.get(&item.path);
                                            let artist = if let Some(cue) = &item.cue {
                                                cue.performer
                                                    .clone()
                                                    .or_else(|| md.and_then(|m| m.artist.clone()))
                                            } else {
                                                md.and_then(|m| m.artist.clone())
                                            }
                                            .unwrap_or_default();
                                            let duration = if let Some(cue) = &item.cue {
                                                match cue.end_ms {
                                                    Some(end) => fmt_time(
                                                        (end.saturating_sub(cue.start_ms)) as f64
                                                            / 1000.0,
                                                    ),
                                                    None => md
                                                        .and_then(|m| m.duration)
                                                        .map(|d| {
                                                            fmt_time(
                                                                (d - cue.start_ms as f64 / 1000.0)
                                                                    .max(0.0),
                                                            )
                                                        })
                                                        .unwrap_or_default(),
                                                }
                                            } else {
                                                md.and_then(|m| m.duration)
                                                    .map(fmt_time)
                                                    .unwrap_or_default()
                                            };
                                            let is_current =
                                                self.playlist.current_index() == Some(index);
                                            let icon = if is_current {
                                                if self.playing {
                                                    "▶"
                                                } else {
                                                    "‖"
                                                }
                                            } else {
                                                ""
                                            };
                                            let response = ui::row::playlist_track_row(
                                                ui,
                                                egui::Id::new(("playlist_row", index)),
                                                egui::vec2(ui.available_width(), 22.0),
                                                index == selected,
                                                seq,
                                                icon,
                                                &title,
                                                &artist,
                                                &duration,
                                                egui::Sense::click(),
                                            );
                                            if playlist_scroll && index == selected {
                                                response.scroll_to_me(Some(egui::Align::Center));
                                            }
                                            if response.clicked() {
                                                *select = Some(index);
                                            }
                                            if response.double_clicked() {
                                                *activate = Some(index);
                                            }
                                            // 右键菜单：播放 / 移除（作用于曲目行）。
                                            response.context_menu(|ui| {
                                                if ui.button(ctx_play_label.as_str()).clicked() {
                                                    *ctx_action = Some(PlaylistCtx::Play(index));
                                                    ui.close();
                                                }
                                                if ui.button(ctx_remove_label.as_str()).clicked() {
                                                    *ctx_action = Some(PlaylistCtx::Remove(index));
                                                    ui.close();
                                                }
                                            });
                                        }
                                    }
                                }
                            }
                        });
                    if filter_active {
                        ui.label(
                            egui::RichText::new(format!("{} / {}", filtered.len(), items.len()))
                                .monospace()
                                .color(palette.dim),
                        );
                    }
                });
            if let Some(payload) = dropped_payload {
                *dropped = Some(payload.as_ref().clone());
            }
        });
        // 滚动跟随一次性：绘后清除。
        self.playlist_scroll_pending = false;
    }

    fn draw_menu(&mut self, ctx: &egui::Context, action: &mut Action) {
        let i18n = &self.i18n;
        let view = self.view;
        let media_keys_enabled = self.cfg.media_keys_enabled;
        let plugins = &self.plugins;
        let visual_index = self.visual_index;
        let skin_name = self.cfg.skin.clone();
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(i18n.t("menu.file").into_owned(), |ui| {
                    if ui.button(i18n.t("action.add").into_owned()).clicked() {
                        *action = Action::AddSelected;
                        ui.close();
                    }
                    if ui.button(i18n.t("action.remove").into_owned()).clicked() {
                        *action = Action::RemoveSelected;
                        ui.close();
                    }
                    if ui.button(i18n.t("action.clear").into_owned()).clicked() {
                        *action = Action::ClearPlaylist;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(i18n.t("menu.quit").into_owned()).clicked() {
                        *action = Action::Quit;
                        ui.close();
                    }
                });
                ui.menu_button(i18n.t("menu.playback").into_owned(), |ui| {
                    if ui.button(i18n.t("menu.play_pause").into_owned()).clicked() {
                        *action = Action::TogglePlay;
                        ui.close();
                    }
                    if ui.button(i18n.t("menu.stop").into_owned()).clicked() {
                        *action = Action::Stop;
                        ui.close();
                    }
                    if ui.button(i18n.t("menu.prev").into_owned()).clicked() {
                        *action = Action::Prev;
                        ui.close();
                    }
                    if ui.button(i18n.t("menu.next").into_owned()).clicked() {
                        *action = Action::Next;
                        ui.close();
                    }
                });
                ui.menu_button(i18n.t("menu.effects").into_owned(), |ui| {
                    if ui.button(i18n.t("menu.eq").into_owned()).clicked() {
                        *action = Action::OpenEq;
                        ui.close();
                    }
                    if ui.button(i18n.t("menu.compressor").into_owned()).clicked() {
                        *action = Action::OpenCompressor;
                        ui.close();
                    }
                    ui.menu_button(i18n.t("menu.medium").into_owned(), |ui| {
                        for medium in PlaybackMedium::ALL {
                            let mark = if self.cfg.playback_medium == medium.as_str() {
                                "√ "
                            } else {
                                "  "
                            };
                            if ui
                                .button(format!("{mark}{}", i18n.t(medium_label(medium))))
                                .clicked()
                            {
                                *action = Action::SetMedium(medium);
                                ui.close();
                            }
                        }
                    });
                    let replay_mark = if self.cfg.replay_gain { "√ " } else { "  " };
                    if ui
                        .button(format!("{replay_mark}{}", i18n.t("menu.replaygain")))
                        .clicked()
                    {
                        *action = Action::ToggleReplayGain;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(i18n.t("menu.plugins").into_owned()).clicked() {
                        *action = Action::OpenPlugins;
                        ui.close();
                    }
                    if ui
                        .button(i18n.t("menu.reload_plugins").into_owned())
                        .clicked()
                    {
                        *action = Action::ReloadPlugins;
                        ui.close();
                    }
                });
                ui.menu_button(i18n.t("menu.view").into_owned(), |ui| {
                    let browser_mark = if view.left == LeftView::Browser {
                        "√ "
                    } else {
                        "  "
                    };
                    if ui
                        .button(format!(
                            "{browser_mark}{}",
                            i18n.t("menu.view_browser").into_owned()
                        ))
                        .clicked()
                    {
                        *action = Action::ShowBrowser;
                        ui.close();
                    }
                    let cover_mark = if view.left == LeftView::Cover {
                        "√ "
                    } else {
                        "  "
                    };
                    if ui
                        .button(format!(
                            "{cover_mark}{}",
                            i18n.t("menu.view_cover").into_owned()
                        ))
                        .clicked()
                    {
                        *action = Action::ToggleCover;
                        ui.close();
                    }
                    let lyrics_mark = if view.lyrics { "√ " } else { "  " };
                    if ui
                        .button(format!(
                            "{lyrics_mark}{}",
                            i18n.t("menu.view_lyrics").into_owned()
                        ))
                        .clicked()
                    {
                        *action = Action::ToggleLyrics;
                        ui.close();
                    }
                    let spectrum_label = format!(
                        "{}：{}",
                        i18n.t("menu.view_spectrum").into_owned(),
                        i18n.t(view.spectrum.label_key()).into_owned()
                    );
                    if ui.button(spectrum_label).clicked() {
                        *action = Action::CycleSpectrum;
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button(i18n.t("menu.visual").into_owned(), |ui| {
                        let builtin_mark = if visual_index.is_none() { "√ " } else { "  " };
                        if ui
                            .button(format!(
                                "{builtin_mark}{}",
                                i18n.t("menu.visual_builtin").into_owned()
                            ))
                            .clicked()
                        {
                            *action = Action::SetVisualBuiltin;
                            ui.close();
                        }
                        for (index, visual) in plugins.visuals.iter().enumerate() {
                            let mark = if visual_index == Some(index) {
                                "√ "
                            } else {
                                "  "
                            };
                            if ui.button(format!("{mark}{}", visual.stem)).clicked() {
                                *action = Action::SetVisual(index);
                                ui.close();
                            }
                        }
                    });
                });
                ui.menu_button(i18n.t("menu.settings").into_owned(), |ui| {
                    ui.menu_button(i18n.t("menu.skin").into_owned(), |ui| {
                        // 全量皮肤清单（与 fx 同名同序），当前选中项打 √。
                        for loaded in &plugins.skins {
                            let mark = if loaded.name == skin_name {
                                "√ "
                            } else {
                                "  "
                            };
                            if ui.button(format!("{mark}{}", loaded.name)).clicked() {
                                *action = Action::SetSkin(loaded.name.clone());
                                ui.close();
                            }
                        }
                        // 无皮肤清单时保留终端原生兜底项（跟随系统深浅色）。
                        if plugins.skins.is_empty()
                            && ui
                                .button(format!("√ {}", i18n.t("menu.skin_terminal").into_owned()))
                                .clicked()
                        {
                            *action = Action::SetSkinTerminal;
                            ui.close();
                        }
                    });
                    ui.menu_button(i18n.t("menu.lang").into_owned(), |ui| {
                        if ui.button(i18n.t("menu.lang_zh").into_owned()).clicked() {
                            *action = Action::SetLangZh;
                            ui.close();
                        }
                        if ui.button(i18n.t("menu.lang_en").into_owned()).clicked() {
                            *action = Action::SetLangEn;
                            ui.close();
                        }
                    });
                    ui.separator();
                    let mark = if media_keys_enabled { "√ " } else { "  " };
                    if ui
                        .button(format!("{mark}{}", i18n.t("menu.media_keys").into_owned()))
                        .clicked()
                    {
                        *action = Action::ToggleMediaKeys;
                        ui.close();
                    }
                    if ui.button(i18n.t("menu.command").into_owned()).clicked() {
                        *action = Action::OpenCommand;
                        ui.close();
                    }
                });
                ui.menu_button(i18n.t("menu.help").into_owned(), |ui| {
                    if ui.button(i18n.t("menu.about").into_owned()).clicked() {
                        *action = Action::About;
                        ui.close();
                    }
                });
            });
        });
    }

    fn draw_status(&mut self, ctx: &egui::Context, action: &mut Action, volume: &mut Option<f32>) {
        // 提示超时即清除（旧实现只停显不清，status 永驻）。
        if self
            .status
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(3))
        {
            self.status = None;
        }
        let palette = Palette::current(ctx);
        let mut volume_value = self.cfg.volume;
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("⏮").clicked() {
                    *action = Action::Prev;
                }
                if ui.button(if self.playing { "⏸" } else { "▶" }).clicked() {
                    *action = Action::TogglePlay;
                }
                if ui.button("⏹").clicked() {
                    *action = Action::Stop;
                }
                if ui.button("⏭").clicked() {
                    *action = Action::Next;
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} / {}",
                        fmt_time(self.position),
                        fmt_time(self.duration)
                    ))
                    .monospace()
                    .color(palette.dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{}%", (volume_value * 100.0) as u32))
                            .monospace()
                            .color(palette.dim),
                    );
                    let slider = egui::Slider::new(&mut volume_value, 0.0..=1.0)
                        .show_value(false)
                        .trailing_fill(true);
                    if ui.add(slider).changed() {
                        *volume = Some(volume_value);
                    }
                });
            });
            if let Some((text, at)) = &self.status {
                if at.elapsed() < Duration::from_secs(3) {
                    ui.label(egui::RichText::new(text).monospace().color(palette.dim));
                }
            }
        });
    }

    fn draw_function_bar(&mut self, ctx: &egui::Context) {
        let palette = Palette::current(ctx);
        let hint = self.i18n.t("fbar.hint").into_owned();
        egui::TopBottomPanel::bottom("function_bar").show(ctx, |ui| {
            ui.label(egui::RichText::new(hint).monospace().color(palette.dim));
        });
    }

    /// 命令输入条（`:` 唤起，Enter 执行、Esc 取消）。
    fn draw_command_bar(&mut self, ctx: &egui::Context) {
        if !self.command_open {
            return;
        }
        let palette = Palette::current(ctx);
        let i18n = &self.i18n;
        egui::TopBottomPanel::bottom("command_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(":").monospace().color(palette.fg));
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.command_input)
                        .hint_text(i18n.t("command.hint").into_owned())
                        .desired_width(f32::INFINITY),
                );
                if self.command_focus {
                    response.request_focus();
                    self.command_focus = false;
                }
            });
        });
    }

    fn draw_about(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let i18n = &self.i18n;
        let engine_ok = self.engine.is_some();
        let mut close = false;
        let shown = ui::dialog::dialog(i18n.t("menu.about").into_owned())
            .collapsible(false)
            .min_width(420.0)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(format!("tuneux-max {}", env!("CARGO_PKG_VERSION")));
                ui.label(i18n.t("about.desc").into_owned());
                ui.separator();
                ui.label(i18n.t("about.license").into_owned());
                ui.label(if engine_ok {
                    i18n.t("about.engine").into_owned()
                } else {
                    i18n.t("msg.engine_unavailable").into_owned()
                });
                ui.add_space(6.0);
                if ui.button(i18n.t("common.close").into_owned()).clicked() {
                    close = true;
                }
            });
        if shown.is_none() || close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.about_open = false;
        }
    }
}

fn first_track_index(
    items: &[PlaylistItem],
    audio_path: &Path,
    first_start_ms: Option<u64>,
) -> Option<usize> {
    match first_start_ms {
        Some(start_ms) => items.iter().position(|item| {
            item.path == audio_path && item.cue.as_ref().map(|cue| cue.start_ms) == Some(start_ms)
        }),
        None => items.iter().position(|item| item.path == audio_path),
    }
}

fn entry_matches_query(entry: &Entry, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || entry.name().to_lowercase().contains(&query)
}

fn cue_relative_progress(position: f64, cue: &ActiveCue, fallback_end: f64) -> (f64, f64) {
    let file_end = cue.end.or(cue.file_duration).unwrap_or(fallback_end);
    let duration = (file_end - cue.start).max(0.0);
    ((position - cue.start).clamp(0.0, duration), duration)
}

fn cue_contains(item: &PlaylistItem, saved: Option<f64>) -> bool {
    let Some(cue) = &item.cue else {
        return true;
    };
    let Some(position) = saved.filter(|position| *position > 0.5) else {
        return true;
    };
    let start = cue.start_ms as f64 / 1000.0;
    let end = cue.end_ms.map(|ms| ms as f64 / 1000.0);
    position >= start && end.is_none_or(|end| position < end)
}

fn medium_label(medium: PlaybackMedium) -> &'static str {
    match medium {
        PlaybackMedium::None => "medium.none",
        PlaybackMedium::TapeClear => "medium.tape_clear",
        PlaybackMedium::TapeWhite => "medium.tape_white",
        PlaybackMedium::TapeClassic => "medium.tape_classic",
        PlaybackMedium::TapeAged => "medium.tape_aged",
        PlaybackMedium::VinylClean => "medium.vinyl_clean",
        PlaybackMedium::VinylDynamic => "medium.vinyl_dynamic",
        PlaybackMedium::VinylStandard => "medium.vinyl_standard",
        PlaybackMedium::VinylAged => "medium.vinyl_aged",
        _ => "medium.unknown",
    }
}

/// 取初次恢复的播放列表条目：多列表活跃列表优先，旧单列表兜底。
fn initial_items(state: &PlaylistState) -> Vec<PlaylistItem> {
    if let Some(list) = state.lists.get(state.active) {
        return list.items.clone();
    }
    state.items.clone()
}

/// 秒数格式化为 `m:ss`。
fn fmt_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// 在水平布局里放一列固定宽度、占满高度的子区。
fn column(ui: &mut egui::Ui, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    let height = ui.available_height();
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::top_down(egui::Align::LEFT),
        |ui| {
            add_contents(ui);
        },
    );
}

/// 三桶频段最大值：低频 / 中频 / 高频（L/R 先平均合并为单声道，再逐桶取 MAX）。
/// 与 fx 频段面板同口径——取最大值保留桶内峰值能量，平均会把尖峰抹平。
fn band_maxima(spectrum: &[[f32; N_BANDS]; 2]) -> [f32; 3] {
    let mut buckets = [0.0f32; 3];
    let third = N_BANDS / 3;
    for (bucket, value) in buckets.iter_mut().enumerate() {
        let start = bucket * third;
        let end = if bucket == 2 { N_BANDS } else { start + third };
        for (left, right) in spectrum[0][start..end].iter().zip(&spectrum[1][start..end]) {
            *value = value.max(((left + right) * 0.5).clamp(0.0, 1.0));
        }
    }
    buckets
}

/// L/R 平均合并为单声道频谱（与 fx / 共享 TUI 渲染同口径），钳制到 0–1。
fn merge_spectrum_channels(spectrum: &[[f32; N_BANDS]; 2]) -> [f32; N_BANDS] {
    std::array::from_fn(|i| ((spectrum[0][i] + spectrum[1][i]) * 0.5).clamp(0.0, 1.0))
}

/// 频谱 / 峰值按显示列池化：列数不少于段数时补零直传；否则每列取区间
/// **最大值**（保留峰值，窄屏不丢尖峰）。
fn pool_to_columns(bands: &[f32], columns: usize) -> Vec<f32> {
    let n = bands.len();
    if columns >= n {
        let mut values = bands.to_vec();
        values.resize(columns, 0.0);
        return values;
    }
    let bands_per_col = n as f32 / columns as f32;
    (0..columns)
        .map(|col| {
            let start = ((col as f32 * bands_per_col) as usize).min(n);
            let end = (((col + 1) as f32 * bands_per_col) as usize)
                .min(n)
                .max(start + 1);
            bands[start..end].iter().copied().fold(0.0f32, f32::max)
        })
        .collect()
}

/// 柱高 / 峰高：能量 × 可用行数向上取整（非零能量至少占一行）。
fn column_height(value: f32, max_h: usize) -> usize {
    (value.clamp(0.0, 1.0) * max_h as f32).ceil() as usize
}

/// 频谱格类型（纯函数，单测守护峰帽行 / 柱身 / 实心填充语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpectrumCell {
    /// 留白。
    Blank,
    /// 柱身字符。
    Bar,
    /// 峰帽字符（只占峰高一行）。
    Peak,
    /// 实心柱身填充。
    SolidBar,
    /// 实心峰帽区间填充（柱顶与峰顶之间）。
    SolidPeak,
}

/// 按「距底行距」判定格类型：峰帽只占峰高行、柱身自底填到柱高；
/// 实心模式峰帽区间亦填充（与共享 TUI 渲染逐格同语义）。
fn spectrum_cell(bar_h: usize, peak_h: usize, from_bottom: usize, solid: bool) -> SpectrumCell {
    if solid {
        if peak_h > 0 && from_bottom > bar_h && from_bottom <= peak_h {
            SpectrumCell::SolidPeak
        } else if bar_h > 0 && from_bottom <= bar_h {
            SpectrumCell::SolidBar
        } else {
            SpectrumCell::Blank
        }
    } else if peak_h > 0 && from_bottom == peak_h {
        SpectrumCell::Peak
    } else if bar_h > 0 && from_bottom <= bar_h {
        SpectrumCell::Bar
    } else {
        SpectrumCell::Blank
    }
}

/// 推进 splitmix64 状态（与共享 TUI 渲染同源，字符随机口径一致）。
fn rng_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D049BB133111EB);
    value ^ (value >> 31)
}

/// 字符格频谱（egui painter 复刻共享 TUI 渲染语义）：逐列 max-pool、
/// 柱高 / 峰高、非实心逐格取池字符（帧号种 splitmix64）、峰帽占一行、
/// 底行基线；实心模式背景填充竖柱 + 峰帽区间填充（单色皮肤）。
/// 字符风格 / 列宽 / 柱色 / 峰色 / 基线色均随皮肤令牌变化。
fn paint_spectrum_chars(
    ui: &egui::Ui,
    rect: egui::Rect,
    spectrum: &[f32],
    peaks: &[f32],
    frame_tick: u64,
    palette: Palette,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let font = egui::FontId::monospace(12.0);
    let advance = ui.fonts(|fonts| fonts.glyph_width(&font, 'M')).max(1.0);
    let row_h = 12.0;
    // 实心模式柱 2 格 + 缝 1 格；字符模式列宽随风格（汉字 2 其余 1）。
    let col_width = if palette.block_spectrum {
        3
    } else {
        palette.bar_style.col_width().max(1)
    };
    let cell_w = advance * col_width as f32;
    let total_cols = (rect.width() / cell_w) as usize;
    let rows = (rect.height() / row_h) as usize;
    if total_cols == 0 || rows < 2 {
        return;
    }
    let bar_max_h = rows - 1;
    let n = spectrum.len().min(peaks.len());
    let active = total_cols.min(n);
    let display = pool_to_columns(&spectrum[..n], total_cols);
    let display_peaks = pool_to_columns(&peaks[..n], total_cols);
    let pool = palette.bar_style.pool();
    let mut rng = frame_tick;
    let painter = ui.painter_at(rect);
    for row in 0..rows {
        let y = rect.top() + row as f32 * row_h;
        if row == rows - 1 {
            // 基线：实心 / 汉字列宽 2 画双格横線，其余单格。
            let baseline = if palette.block_spectrum || col_width == 2 {
                "──"
            } else {
                "─"
            };
            for col in 0..total_cols {
                painter.text(
                    egui::pos2(rect.left() + col as f32 * cell_w, y),
                    egui::Align2::LEFT_TOP,
                    baseline,
                    font.clone(),
                    palette.grid,
                );
            }
            continue;
        }
        let from_bottom = rows - 1 - row;
        for col in 0..active {
            let x = rect.left() + col as f32 * cell_w;
            let cell = spectrum_cell(
                column_height(display[col], bar_max_h),
                column_height(display_peaks[col], bar_max_h),
                from_bottom,
                palette.block_spectrum,
            );
            match cell {
                SpectrumCell::Blank => {}
                SpectrumCell::SolidBar | SpectrumCell::SolidPeak => {
                    let color = if cell == SpectrumCell::SolidBar {
                        palette.bar
                    } else {
                        palette.peak
                    };
                    painter.rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(x, y),
                            egui::vec2(advance * 2.0, row_h),
                        ),
                        0.0,
                        color,
                    );
                }
                SpectrumCell::Bar | SpectrumCell::Peak => {
                    let ch = pool[(rng_next(&mut rng) as usize) % pool.len()];
                    let color = if cell == SpectrumCell::Peak {
                        palette.peak
                    } else {
                        palette.bar
                    };
                    painter.text(
                        egui::pos2(x, y),
                        egui::Align2::LEFT_TOP,
                        ch,
                        font.clone(),
                        color,
                    );
                }
            }
        }
    }
}

/// 在给定矩形内画竖条频谱。
/// 一行电平表（标签 + 填充条）。
fn paint_meter(ui: &egui::Ui, rect: egui::Rect, label: &str, value: f32, palette: Palette) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let painter = ui.painter_at(rect);
    painter.text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.0),
        palette.dim,
    );
    let bar = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 18.0, rect.center().y - 4.0),
        egui::pos2(rect.right(), rect.center().y + 4.0),
    );
    if bar.width() <= 0.0 {
        return;
    }
    painter.rect_filled(bar, 0.0, palette.fg.gamma_multiply(0.12));
    let filled = egui::Rect::from_min_max(
        bar.min,
        egui::pos2(
            bar.left() + bar.width() * value.clamp(0.0, 1.0),
            bar.bottom(),
        ),
    );
    let color = if value < 0.55 {
        palette.level_low
    } else if value < 0.85 {
        palette.level_mid
    } else {
        palette.level_high
    };
    painter.rect_filled(filled, 0.0, color);
}

/// 三行横向频段条（标签 + 填充条，与电平表同一画法）：显示序由调用方
/// 给定（高在上 / 中 / 低在下），颜色随桶走；能量条按数值横向填充。
fn paint_bands(
    ui: &egui::Ui,
    rect: egui::Rect,
    rows: &[(String, f32, egui::Color32)],
    palette: Palette,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 || rows.is_empty() {
        return;
    }
    let painter = ui.painter_at(rect);
    let row_height = rect.height() / rows.len() as f32;
    for (index, (label, value, color)) in rows.iter().enumerate() {
        let row = egui::Rect::from_min_size(
            rect.min + egui::vec2(0.0, index as f32 * row_height),
            egui::vec2(rect.width(), row_height),
        );
        painter.text(
            row.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::monospace(11.0),
            palette.dim,
        );
        let bar = egui::Rect::from_min_max(
            egui::pos2(row.left() + 18.0, row.center().y - 4.0),
            egui::pos2(row.right(), row.center().y + 4.0),
        );
        if bar.width() <= 0.0 {
            continue;
        }
        painter.rect_filled(bar, 0.0, palette.fg.gamma_multiply(0.12));
        let filled = egui::Rect::from_min_max(
            bar.min,
            egui::pos2(
                bar.left() + bar.width() * value.clamp(0.0, 1.0),
                bar.bottom(),
            ),
        );
        painter.rect_filled(filled, 0.0, *color);
    }
}

/// 插件管理页的分组展示（标题 + 若干行 / 空态）。
fn plugin_section(ui: &mut egui::Ui, label: &str, none: &str, lines: &[String]) {
    ui.label(egui::RichText::new(label).monospace());
    if lines.is_empty() {
        ui.label(none.to_string());
    } else {
        for line in lines {
            ui.label(egui::RichText::new(line).monospace());
        }
    }
    ui.separator();
}

/// 把 egui 按键事件规范化为与 `commonx::keymap` 同口径的键描述。
fn key_desc(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    let base = match key {
        egui::Key::Space => "space",
        egui::Key::Tab => "tab",
        egui::Key::Enter => "enter",
        egui::Key::Escape => "esc",
        egui::Key::Backspace => "backspace",
        egui::Key::ArrowUp => "up",
        egui::Key::ArrowDown => "down",
        egui::Key::ArrowLeft => "left",
        egui::Key::ArrowRight => "right",
        egui::Key::Home => "home",
        egui::Key::End => "end",
        egui::Key::Plus => "+",
        egui::Key::Equals => "=",
        egui::Key::Minus => "-",
        other => {
            let name = format!("{other:?}");
            if name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic()) {
                return Some(format_key_desc(&name.to_ascii_lowercase(), modifiers));
            }
            return None;
        }
    };
    Some(format_key_desc(base, modifiers))
}

/// 组合修饰符与键名（shift 仅对单字母有意义，与 commonx 口径一致）。
fn format_key_desc(base: &str, modifiers: egui::Modifiers) -> String {
    let mut out = String::new();
    let letter = base.chars().count() == 1 && base.chars().all(|c| c.is_ascii_alphabetic());
    if modifiers.shift && letter {
        out.push_str("shift+");
    }
    if modifiers.ctrl || modifiers.command {
        out.push_str("ctrl+");
    }
    if modifiers.alt {
        out.push_str("alt+");
    }
    out.push_str(base);
    out
}

/// 播放列表过滤匹配：空过滤恒真；匹配文件名 / 路径 / 专辑 / CUE 标题。
/// 曲目标题（与 fx 同口径）：CUE 分轨用 `cue.title`（纯 cue 标题，不拼文件名）；
/// 非 CUE 用元数据标题，缺失回落**文件名去扩展名**。元数据读取与平台无关，
/// 三 OS 天然一致，不加平台分支。
fn track_title(item: &PlaylistItem, md: Option<&TrackMetadata>) -> String {
    if let Some(cue) = &item.cue {
        return cue.title.clone();
    }
    md.and_then(|meta| meta.title.clone()).unwrap_or_else(|| {
        item.path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| item.path.display().to_string())
    })
}

fn playlist_matches(filter: &str, item: &PlaylistItem) -> bool {
    let filter = filter.trim().to_lowercase();
    if filter.is_empty() {
        return true;
    }
    let mut haystack = item.path.display().to_string().to_lowercase();
    if let Some(album) = &item.album {
        haystack.push('\n');
        haystack.push_str(&album.to_lowercase());
    }
    if let Some(cue) = &item.cue {
        haystack.push('\n');
        haystack.push_str(&cue.title.to_lowercase());
        if let Some(performer) = &cue.performer {
            haystack.push('\n');
            haystack.push_str(&performer.to_lowercase());
        }
    }
    haystack.contains(&filter)
}

/// 在可用区域内等比缩放纹理。
fn fit_size(texture: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if texture.x <= 0.0 || texture.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let scale = (available.x / texture.x).min(available.y / texture.y);
    (texture * scale).max(egui::Vec2::splat(1.0))
}

impl eframe::App for MaxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ensure_media_loaded(ctx);
        self.poll_engine();
        self.poll_browser_search();
        // 搜索防抖到期且无在途任务才启动后台扫描。
        if self.browser_search.active()
            && self.browser_search.job.is_none()
            && self
                .search_debounce
                .is_some_and(|at| at.elapsed() >= Duration::from_millis(200))
        {
            self.start_browser_search();
        }
        self.poll_media_keys();
        self.tick_visual();
        self.handle_shortcuts(ctx);
        let mut action = Action::None;
        let mut select_browser = None;
        let mut activate_browser = None;
        let mut select_playlist = None;
        let mut activate_playlist = None;
        let mut volume = None;
        let mut browser_ctx = None;
        let mut playlist_ctx = None;
        let mut dropped = None;

        self.draw_function_bar(ctx);
        self.draw_status(ctx, &mut action, &mut volume);
        self.draw_command_bar(ctx);
        self.draw_spectrum_panel(ctx);
        self.draw_menu(ctx, &mut action);
        self.draw_top_strip(ctx);
        self.draw_left_panel(
            ctx,
            &mut select_browser,
            &mut activate_browser,
            &mut browser_ctx,
        );
        self.draw_lyrics_panel(ctx);
        let mut collapse_album = None;
        self.draw_playlist(
            ctx,
            &mut action,
            &mut select_playlist,
            &mut activate_playlist,
            &mut playlist_ctx,
            &mut dropped,
            &mut collapse_album,
        );
        if let Some(album) = collapse_album {
            if !self.collapsed_albums.remove(&album) {
                self.collapsed_albums.insert(album);
            }
        }
        self.draw_about(ctx);
        self.draw_eq_dialog(ctx);
        self.draw_comp_dialog(ctx);
        self.draw_plugins_dialog(ctx);

        if let Some(index) = select_browser {
            if self.browser_search.active() {
                self.browser_search.selected = index;
            } else {
                self.browser.select(index);
            }
        }
        if let Some(index) = activate_browser {
            if self.browser_search.active() {
                self.activate_browser_search_result(index);
            } else {
                self.activate_browser_entry(index);
            }
        }
        if let Some(index) = select_playlist {
            self.playlist_sel = index;
        }
        if let Some(index) = activate_playlist {
            self.play_index(index);
        }
        if let Some(ctx_action) = browser_ctx {
            self.apply_browser_ctx(ctx_action);
        }
        if let Some(ctx_action) = playlist_ctx {
            self.apply_playlist_ctx(ctx_action);
        }
        if let Some(entry) = dropped {
            self.add_dropped(entry);
        }
        if let Some(value) = volume {
            self.set_volume(value);
        }
        self.apply_action(ctx, action);

        if self.last_save.elapsed() >= Duration::from_secs(5) {
            self.persist();
            self.last_save = Instant::now();
        }
        let title = self
            .current
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| format!("{} - tuneux-max", name.to_string_lossy()))
            .unwrap_or_else(|| "tuneux-max".to_string());
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        // 播放中 100ms；空闲但媒体键监听/后台搜索在途时也要持续轮询，
        // 否则暂停后按媒体键 Play 没有 winit 事件，主循环不会醒来。
        if self.playing {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self.search_debounce.is_some() || self.metadata_warming {
            // 搜索防抖到期 / 元数据暖缓存推进都依赖持续重绘：空闲（未播放、
            // 无媒体键）时若不主动请求重绘，防抖永不触发、暖缓存停滞。
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if self.media_keys.is_some()
            || self.browser_search.job.is_some()
            || self.status.is_some()
        {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_time_formats_minutes() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(65.4), "1:05");
        assert_eq!(fmt_time(-3.0), "0:00");
    }

    #[test]
    fn initial_items_prefers_active_saved_list() {
        let state = PlaylistState {
            lists: vec![SavedList {
                name: "默认".to_string(),
                items: vec![PlaylistItem {
                    path: PathBuf::from("/music/a.flac"),
                    album: None,
                    track_number: None,
                    cue: None,
                }],
                current: None,
                current_cue: None,
                shuffle: false,
                shuffle_mode: "off".to_string(),
            }],
            ..PlaylistState::default()
        };
        assert_eq!(initial_items(&state).len(), 1);
    }

    #[test]
    fn spectrum_view_cycles_and_ratios() {
        assert_eq!(SpectrumView::Hidden.next(), SpectrumView::Half);
        assert_eq!(SpectrumView::Half.next(), SpectrumView::Full);
        assert_eq!(SpectrumView::Full.next(), SpectrumView::Hidden);
        assert!(SpectrumView::Hidden.height_ratio() < SpectrumView::Half.height_ratio());
    }

    #[test]
    fn band_maxima_split_low_to_high() {
        let mut spectrum = [[0.0f32; N_BANDS]; 2];
        let mut values = [0.0f32; N_BANDS];
        for (index, value) in values.iter_mut().enumerate() {
            *value = if index < N_BANDS / 3 {
                0.1
            } else if index < (N_BANDS * 2) / 3 {
                0.5
            } else {
                0.9
            };
        }
        spectrum[0] = values;
        spectrum[1] = values;
        let maxima = band_maxima(&spectrum);
        assert!(maxima[0] < maxima[1] && maxima[1] < maxima[2]);
    }

    #[test]
    fn band_maxima_keep_bucket_peak_not_average() {
        // 桶内单根尖峰：取 MAX 应保留尖峰能量（平均值会抹平到接近 0）。
        let mut spectrum = [[0.0f32; N_BANDS]; 2];
        spectrum[0][N_BANDS - 1] = 1.0;
        spectrum[1][N_BANDS - 1] = 1.0;
        let maxima = band_maxima(&spectrum);
        assert_eq!(maxima[0], 0.0);
        assert_eq!(maxima[1], 0.0);
        assert_eq!(maxima[2], 1.0, "高频桶尖峰必须保留");
    }

    #[test]
    fn merge_spectrum_channels_averages_and_clamps() {
        let mut spectrum = [[0.0f32; N_BANDS]; 2];
        spectrum[0][0] = 0.8;
        spectrum[1][0] = 0.4;
        spectrum[0][1] = 1.0;
        spectrum[1][1] = 1.0;
        let merged = merge_spectrum_channels(&spectrum);
        assert!((merged[0] - 0.6).abs() < 1e-6);
        assert_eq!(merged[1], 1.0);
        assert_eq!(merged[2], 0.0);
    }

    #[test]
    fn pool_to_columns_pads_or_max_pools() {
        let bands = [0.1, 0.9, 0.2, 0.3];
        // 列数 >= 段数：补零直传。
        assert_eq!(
            pool_to_columns(&bands, 6),
            vec![0.1, 0.9, 0.2, 0.3, 0.0, 0.0]
        );
        // 列数 < 段数：每列取区间最大值（尖峰保留）。
        assert_eq!(pool_to_columns(&bands, 2), vec![0.9, 0.3]);
    }

    #[test]
    fn column_height_ceils_and_clamps() {
        assert_eq!(column_height(0.0, 10), 0);
        assert_eq!(column_height(0.01, 10), 1, "非零能量至少一行");
        assert_eq!(column_height(0.5, 10), 5);
        assert_eq!(column_height(2.0, 10), 10, "越界钳制");
    }

    #[test]
    fn spectrum_cell_peak_cap_occupies_single_row() {
        // 柱高 3、峰高 6：距底 1..3 柱身、6 峰帽、其余留白。
        assert_eq!(spectrum_cell(3, 6, 1, false), SpectrumCell::Bar);
        assert_eq!(spectrum_cell(3, 6, 3, false), SpectrumCell::Bar);
        assert_eq!(spectrum_cell(3, 6, 4, false), SpectrumCell::Blank);
        assert_eq!(spectrum_cell(3, 6, 6, false), SpectrumCell::Peak);
        assert_eq!(spectrum_cell(3, 6, 7, false), SpectrumCell::Blank);
        // 峰帽不坠到柱身下方：峰高==柱高时该行归峰帽。
        assert_eq!(spectrum_cell(3, 3, 3, false), SpectrumCell::Peak);
        // 零能量全留白。
        assert_eq!(spectrum_cell(0, 0, 1, false), SpectrumCell::Blank);
    }

    #[test]
    fn spectrum_cell_solid_fills_peak_gap() {
        // 实心：柱顶与峰顶之间填峰色，柱身填柱色，峰顶之上留白。
        assert_eq!(spectrum_cell(3, 6, 2, true), SpectrumCell::SolidBar);
        assert_eq!(spectrum_cell(3, 6, 4, true), SpectrumCell::SolidPeak);
        assert_eq!(spectrum_cell(3, 6, 6, true), SpectrumCell::SolidPeak);
        assert_eq!(spectrum_cell(3, 6, 7, true), SpectrumCell::Blank);
    }

    #[test]
    fn rng_next_is_deterministic_per_seed() {
        let mut a = 42u64;
        let mut b = 42u64;
        for _ in 0..8 {
            assert_eq!(rng_next(&mut a), rng_next(&mut b));
        }
        let mut c = 43u64;
        assert_ne!(rng_next(&mut a), rng_next(&mut c));
    }

    #[test]
    fn fit_size_keeps_aspect_ratio() {
        let fitted = fit_size(egui::vec2(200.0, 100.0), egui::vec2(400.0, 400.0));
        assert!((fitted.x - 400.0).abs() < 0.1 && (fitted.y - 200.0).abs() < 0.1);
        let fitted = fit_size(egui::vec2(200.0, 100.0), egui::vec2(100.0, 400.0));
        assert!((fitted.x - 100.0).abs() < 0.1 && (fitted.y - 50.0).abs() < 0.1);
    }

    #[test]
    fn key_desc_matches_commonx_syntax() {
        assert_eq!(
            key_desc(egui::Key::Space, egui::Modifiers::NONE).as_deref(),
            Some("space")
        );
        assert_eq!(
            key_desc(egui::Key::P, egui::Modifiers::NONE).as_deref(),
            Some("p")
        );
        assert_eq!(
            key_desc(egui::Key::P, egui::Modifiers::CTRL).as_deref(),
            Some("ctrl+p")
        );
        assert_eq!(
            key_desc(egui::Key::N, egui::Modifiers::SHIFT).as_deref(),
            Some("shift+n")
        );
        assert_eq!(
            key_desc(egui::Key::Plus, egui::Modifiers::SHIFT).as_deref(),
            Some("+"),
            "符号键的 shift 不写入描述",
        );
        assert_eq!(key_desc(egui::Key::F1, egui::Modifiers::NONE), None);
    }

    #[test]
    fn track_title_cue_uses_cue_title_not_filename() {
        let item = PlaylistItem {
            path: PathBuf::from("/music/album.flac"),
            album: None,
            track_number: None,
            cue: Some(tuneux_mediax::CueRef {
                index: 1,
                title: "Cue Title".to_string(),
                performer: None,
                start_ms: 0,
                end_ms: None,
            }),
        };
        let md = TrackMetadata {
            title: Some("Meta".to_string()),
            ..Default::default()
        };
        assert_eq!(track_title(&item, Some(&md)), "Cue Title");
        assert_eq!(track_title(&item, None), "Cue Title");
    }

    #[test]
    fn track_title_non_cue_prefers_metadata_title() {
        let item = PlaylistItem {
            path: PathBuf::from("/music/01-song.flac"),
            album: None,
            track_number: None,
            cue: None,
        };
        let md = TrackMetadata {
            title: Some("Meta Title".to_string()),
            ..Default::default()
        };
        assert_eq!(track_title(&item, Some(&md)), "Meta Title");
    }

    #[test]
    fn track_title_falls_back_to_file_stem_without_extension() {
        let item = PlaylistItem {
            path: PathBuf::from("/music/01-song.flac"),
            album: None,
            track_number: None,
            cue: None,
        };
        assert_eq!(track_title(&item, None), "01-song");
        // 元数据存在但无 title 同样回落去扩展名文件名。
        assert_eq!(
            track_title(&item, Some(&TrackMetadata::default())),
            "01-song"
        );
    }

    #[test]
    fn playlist_filter_matches_path_album_and_cue() {
        let item = PlaylistItem {
            path: PathBuf::from("/music/Album/Track 01.flac"),
            album: Some("夜曲".to_string()),
            track_number: Some(1),
            cue: Some(tuneux_mediax::CueRef {
                index: 1,
                title: "第一曲".to_string(),
                performer: Some("某人".to_string()),
                start_ms: 0,
                end_ms: None,
            }),
        };
        assert!(playlist_matches("", &item));
        assert!(playlist_matches("track 01", &item));
        assert!(playlist_matches("夜曲", &item));
        assert!(playlist_matches("第一曲", &item));
        assert!(playlist_matches("某人", &item));
        assert!(!playlist_matches("不存在的词", &item));
    }

    #[test]
    fn browser_search_default_is_inactive() {
        let search = BrowserSearch::default();
        assert!(!search.active());
        assert!(!search.searching());
    }

    fn cue_item(index: u32, start_ms: u64, end_ms: Option<u64>) -> PlaylistItem {
        PlaylistItem {
            path: PathBuf::from("/music/album.flac"),
            album: None,
            track_number: Some(index),
            cue: Some(tuneux_mediax::CueRef {
                index,
                title: format!("Track {index}"),
                performer: None,
                start_ms,
                end_ms,
            }),
        }
    }

    #[test]
    fn last_cue_track_uses_file_duration_once() {
        let cue = ActiveCue {
            start: 60.0,
            end: None,
            file_duration: Some(180.0),
            title: "last".to_string(),
            performer: None,
        };
        let (position, duration) = cue_relative_progress(75.0, &cue, 0.0);
        assert_eq!(position, 15.0);
        assert_eq!(duration, 120.0);
    }

    #[test]
    fn first_cue_track_is_found_after_partial_dedup() {
        let items = [
            cue_item(1, 0, Some(60_000)),
            cue_item(2, 60_000, Some(120_000)),
            cue_item(3, 120_000, None),
        ];
        assert_eq!(
            first_track_index(&items, Path::new("/music/album.flac"), Some(0)),
            Some(0)
        );
        assert_eq!(
            first_track_index(&items, Path::new("/music/album.flac"), Some(1)),
            None
        );
    }

    #[test]
    fn cue_resume_candidate_checks_absolute_range() {
        let item = cue_item(2, 60_000, Some(120_000));
        assert!(cue_contains(&item, Some(75.0)));
        assert!(!cue_contains(&item, Some(30.0)));
        assert!(!cue_contains(&item, Some(120.0)));
    }

    #[test]
    fn browser_entry_filter_matches_name_case_insensitively() {
        let entry = Entry::File {
            name: "稻香.mp3".to_string(),
            path: PathBuf::from("/music/稻香.mp3"),
        };
        assert!(entry_matches_query(&entry, "稻香"));
        assert!(entry_matches_query(&entry, "MP3"));
        assert!(!entry_matches_query(&entry, "晴天"));
    }

    #[test]
    fn spectrum_view_cycles() {
        assert_eq!(SpectrumView::Hidden.next(), SpectrumView::Half);
    }
}
