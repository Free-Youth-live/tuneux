//! # max 应用 v2：fx TUI 风格的 egui 复刻
//!
//! 视觉基准 = tuneux-fx（002 手册 + P0 规格冻结）：
//! - 所有面板 = 边框盒 + 标题嵌入上边框（ratatui Block 同款观感）
//! - 全局等宽字体、直角、密排间距、选中行反色
//! - 进度条 / 频谱 / 电平 / 三桶频段全部字符渲染（━●─ / █▓▒░ / 白帽峰值）
//! - 垂直结构：菜单栏(1行) → 当前曲目(6行,三栏) → 工作区 → 状态栏(3行) → 功能键栏(1行)
//!
//! 交互白名单（相对 fx 仅两项新增）：拖拽（浏览器→播放列表、面板分隔线）、右键菜单。
//! 老 UI（app.rs）保留，切换点在 main.rs。
//!
//! 现代呈现层（[`modern`]）：`config.modern = true` 时切换为纯图形观感
//! （圆角卡片 / 矢量控件 / 鼠标全覆盖），默认关闭、一键可逆。

mod gauge;
mod modern;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use tuneux_commonx::fs_browser::{FsBrowser, FsBrowserConfig};
use tuneux_commonx::ui::{FocusTarget, UiAction, UiMode, UiModel};
use tuneux_commonx::I18n;
use tuneux_corex::{Engine, PlaybackMedium, PlaybackStatus, KNOWN_AUDIO_EXTS};
use tuneux_mediax::lyrics::Lyrics;
use tuneux_mediax::metadata::TrackMetadata;
use tuneux_mediax::{PlaylistCore, PlaylistItem, PlaylistView, RepeatMode};

use crate::config::MaxConfig;
use crate::dock;
use crate::theme::{self, Palette};

/// 全局等宽字号（TUI 观感基准）。
const FONT: f32 = 15.0;
/// 行高（列表行 / 字符画行距）。
const LINE_H: f32 = 19.0;
/// 现代模式按钮圆角。
const CTRL_R_MODERN: u8 = 5;

fn mono(sz: f32) -> FontId {
    FontId::monospace(sz)
}

/// 显示宽度（CJK 全角 = 2 列，与 fx pad_to_width 同口径）。
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| {
            let u = c as u32;
            if matches!(u,
                0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD)
            {
                2
            } else {
                1
            }
        })
        .sum()
}

/// 截断到显示宽度（超出加 …）。
fn truncate_to_width(s: &str, w: usize) -> String {
    if display_width(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = display_width(&c.to_string());
        if used + cw > w.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

/// 左侧补空格到指定显示宽度（右对齐）。
fn pad_left_to_width(s: &str, w: usize) -> String {
    let mut out = String::new();
    let used = display_width(s);
    for _ in used..w {
        out.push(' ');
    }
    out.push_str(s);
    out
}

/// 分类单个文件到 音频 / cue 两桶（后台线程用）。
fn classify_file(p: &std::path::Path, files: &mut Vec<PathBuf>, cues: &mut Vec<PathBuf>) {
    let Some(ext) = p.extension().and_then(|x| x.to_str()) else {
        return;
    };
    if ext.eq_ignore_ascii_case("cue") {
        cues.push(p.to_path_buf());
        return;
    }
    let lower = ext.to_ascii_lowercase();
    if KNOWN_AUDIO_EXTS.contains(&lower.as_str()) {
        files.push(p.to_path_buf());
    }
}

/// 后台线程：递归收集音频条目（读标签填排序键）+ 展开 .cue。
/// 同名整轨镜像被 .cue 覆盖时跳过（防重复）。单文件根同样适用。
fn collect_items(root: &std::path::Path) -> Vec<PlaylistItem> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut cues: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        if p.is_file() {
            classify_file(&p, &mut files, &mut cues);
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            let fp = e.path();
            if fp.is_dir() {
                stack.push(fp);
            } else {
                classify_file(&fp, &mut files, &mut cues);
            }
        }
    }
    files.sort();
    cues.sort();
    let cue_keys: std::collections::HashSet<(PathBuf, String)> = cues
        .iter()
        .filter_map(|c| {
            Some((
                c.parent()?.to_path_buf(),
                c.file_stem()?.to_str()?.to_ascii_lowercase(),
            ))
        })
        .collect();
    let mut out = Vec::new();
    for fp in files {
        let shadowed = fp
            .parent()
            .zip(fp.file_stem().and_then(|st| st.to_str()))
            .is_some_and(|(dir, stem)| {
                cue_keys.contains(&(dir.to_path_buf(), stem.to_ascii_lowercase()))
            });
        if shadowed {
            continue;
        }
        let md = TrackMetadata::from_file(&fp);
        out.push(PlaylistItem {
            path: fp,
            album: md.album,
            track_number: md.track_number,
            cue: None,
        });
    }
    for c in cues {
        if let Some((items, _, _)) = tuneux_mediax::cue::cue_items_from_cue_file(&c) {
            out.extend(items);
        }
    }
    out
}

/// 第一方插件签名验证公钥（与 tuneux-fx 同源同值——两端共用插件产物）。
const OFFICIAL_PUBKEY: [u8; 32] = [
    0x9a, 0xc3, 0x5b, 0x76, 0xa9, 0xaf, 0xbe, 0x5f, 0xfc, 0xab, 0x74, 0x35, 0xeb, 0xaf, 0xb2, 0x83,
    0x46, 0x56, 0x55, 0xda, 0x84, 0x58, 0xd9, 0xec, 0x3f, 0x4d, 0xa3, 0xb8, 0x4e, 0xb8, 0xcb, 0x29,
];

/// 均衡器预设（业界通行曲线；名称为专名不译）。
const EQ_PRESETS: [(&str, [f32; 10]); 5] = [
    ("Flat", [0.0; 10]),
    ("Rock", [5.0, 4.0, 3.0, 1.0, -1.0, -1.0, 0.0, 2.0, 3.0, 4.0]),
    ("Pop", [-1.0, 1.0, 3.0, 4.0, 3.0, 1.0, -1.0, -1.0, 1.0, 2.0]),
    (
        "Classical",
        [4.0, 3.0, 2.0, 0.0, 0.0, 0.0, -1.0, 1.0, 2.0, 3.0],
    ),
    ("Bass", [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
];

/// 由条目构造预载目标（fx preload_target_of 同款：CUE 按区间，普通整文件）。
fn preload_target_of(item: &PlaylistItem) -> tuneux_corex::PreloadTarget {
    match &item.cue {
        Some(cue) => tuneux_corex::PreloadTarget::range(
            item.path.clone(),
            cue.start_ms as f64 / 1000.0,
            cue.end_ms.map(|e| e as f64 / 1000.0),
        ),
        None => tuneux_corex::PreloadTarget::whole(item.path.clone()),
    }
}

/// 时间格式化 "3:45" / "1:02:03"（与 fx fmt_time 同源）。
fn fmt_time(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    if t >= 3600 {
        format!("{}:{:02}:{:02}", t / 3600, (t % 3600) / 60, t % 60)
    } else {
        format!("{}:{:02}", t / 60, t % 60)
    }
}

/// splitmix64 一步（与 fx rng_next 同源：频谱/电平乱码字符）。
fn rng_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// 介质的 i18n key（与 fx medium_menu_label 同源）。
fn medium_key(m: PlaybackMedium) -> &'static str {
    match m {
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

/// 频谱面板模式（v 键循环，与 fx SpectrumMode 同源；Oscilloscope/Plugin 属 P2）。
/// 播放列表可见行（平铺 = 全 Track；专辑分组 = Header+Track 交替）。
#[derive(Clone)]
enum PlRow {
    /// 专辑组头（album 空串 = 未知专辑哨兵，语言切换不漂移）。
    Header { album: String, count: usize },
    /// 曲目行（item = 底层索引，seq = 显示序号：平铺大排行/专辑内排行）。
    Track { item: usize, seq: usize },
}

/// dock 叶子菜单动作（f2k Change layout 同款）。
enum LeafAct {
    Close(dock::ModuleId),
    /// 在 target 处拆分插入 new（side_by_side = 水平/垂直）。
    Split(dock::ModuleId, dock::ModuleId, bool),
    /// 原位更换。
    Replace(dock::ModuleId, dock::ModuleId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpectrumMode {
    Hidden,
    Full,
    /// 示波器（时域波形，fx v 循环第四态）。
    Oscilloscope,
}

/// 菜单动作缓冲（闭包内记录、循环外执行——避免 egui 借用冲突）。
enum MenuAct {
    Ui(UiAction),
    Medium(PlaybackMedium),
    ReplayGain,
    PickFiles,
    PickDir,
    Font(usize),
    Lang(String),
    ToggleComp,
    NotYet,
    DevFollow,
    Theme(String),
    ToggleMod(String),
    ToggleModule(dock::ModuleId),
    PresetSave,
    PresetLoad(String),
    /// 切换现代 / 经典界面（可逆开关）。
    ModernUi,
    /// 在当前位置添加书签（与 :bm 命令同语义）。
    Bookmark,
}

/// v2 应用状态。
pub struct MaxAppV2 {
    ui: UiModel,
    engine: Option<Engine>,
    browser: FsBrowser,
    playlist: PlaylistCore,
    config: MaxConfig,
    i18n: I18n,
    status: Option<PlaybackStatus>,
    palette: Palette,
    replay_gain: bool,
    /// 浏览器右键菜单（行索引 + 屏幕位置）。
    browser_menu: Option<(usize, Pos2, u64)>,
    /// 播放列表右键菜单。
    playlist_menu: Option<(usize, Pos2, u64)>,
    /// 浏览器拖拽源行。
    drag_browser: Option<usize>,
    /// x 键二次确认时间戳。
    x_confirm_at: Option<Instant>,
    /// 当前曲目元数据。
    metadata: Option<TrackMetadata>,
    /// 封面纹理。
    cover_tex: Option<egui::TextureHandle>,
    /// 封面/元数据对应路径（换曲检测）。
    cover_path: Option<PathBuf>,
    /// 当前歌词。
    lyrics: Option<Lyrics>,
    lyrics_path: Option<PathBuf>,
    /// 当前播放路径。
    current_path: Option<PathBuf>,
    spectrum_mode: SpectrumMode,
    /// 峰值保持白帽（corex 算法，007 约定）。
    spectrum_peaks: tuneux_corex::spectrum::SpectrumPeakHold,
    last_frame: Option<Instant>,
    frame_tick: u64,
    /// 播放列表元数据缓存（路径 → 标签，渲染可见行时惰性加载，与 fx 同口径）。
    metadata_cache: HashMap<PathBuf, TrackMetadata>,
    /// 键盘导航后请求滚动到选中行（渲染时消费）。
    scroll_to_sel: bool,
    /// 折叠的专辑（键 = 专辑哨兵串，与语言无关）。
    collapsed: std::collections::HashSet<String>,
    /// 连续播放失败计数（≥10 熔断自动跳曲，防全损列表无限循环）。
    consecutive_failures: u32,
    /// 瞬时错误提示（状态栏红字，5 秒过期）。
    last_error: Option<String>,
    /// 错误提示时间戳（过期清理用）。
    last_error_at: Option<Instant>,
    /// 断点续播位置（路径 → 秒；退出/切曲/暂停时写入，随状态持久化）。
    positions: std::collections::BTreeMap<PathBuf, f64>,
    /// ReplayGain 实测增益缓存（路径 → dB，随状态持久化）。
    replay_gain_cache: std::collections::BTreeMap<PathBuf, f64>,
    /// 扫描池：目录/文件请求发送端（UI → 目录walker 线程）。
    scan_pool: tuneux_mediax::scan_pool::ScanPool,
    /// 扫描池：结果接收端（标签工人 → UI；每项一批）。

    /// 在途扫描条目数（>0 = 状态栏显示「扫描中」）。

    /// 本轮后台加入的条目累计（完成时一次性提示）。
    tag_added: usize,
    /// 瞬时提示（操作反馈，3 秒过期；错误红字优先于它）。
    flash: Option<String>,
    /// 瞬时提示时间戳。
    flash_at: Option<Instant>,
    /// 搜索目标是否为浏览器（false = 播放列表）。
    search_target_browser: bool,
    /// 搜索框请求焦点（进入搜索态的一次性标志）。
    search_focus_req: bool,
    /// 命令输入框请求焦点（进入命令态的一次性标志）。
    command_focus_req: bool,
    /// 系统媒体键监听（Linux/Windows；macOS 无实现，None 静默缺失）。
    media_keys: Option<tuneux_commonx::media_key::MediaKeyHandle>,
    /// 分组封面纹理缓存（组首曲路径 → 封面；None = 已确认无封面）。
    album_covers: std::collections::HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// 全曲波形包络（路径 → 256 桶峰值；后台线程计算，当前曲切换时触发）。
    track_envelope: Option<(PathBuf, Vec<f32>)>,
    /// 待打开的菜单索引（数字键 1-8 / F10 触发，菜单栏渲染时消费）。
    pending_menu: Option<usize>,
    /// dock 叶子右键菜单（目标模块 + 弹出位 + 打开帧号）。
    dock_leaf_menu: Option<(dock::ModuleId, Pos2, u64)>,
    /// 叶子菜单当前页（0=根页 1=水平拆分 2=垂直拆分 3=更换为）。
    leaf_page: u8,
    /// 拖拽中的模块（标题带 drag 开始；落到别的叶子松手 = 换位）。
    dock_drag: Option<dock::ModuleId>,
    /// 待应用的叶子交换（渲染后做树手术）。
    dock_swap: Option<(dock::ModuleId, dock::ModuleId)>,
    /// 浏览器面板矩形（每帧更新；空白区右键命中判定）。
    browser_rect: egui::Rect,
    /// 播放列表面板矩形（每帧更新；频谱独占时为 NOTHING 不误判）。
    playlist_rect: egui::Rect,
    /// 空白区右键菜单（true = 播放列表 / false = 浏览器）。
    blank_menu: Option<(bool, Pos2, u64)>,
    /// 帧首是否有菜单开启（该帧的右键只关不开——防「关了又弹」观感）。
    menu_open_at_frame_start: bool,
    /// 上一帧的浏览器目录（目录变化 = 行索引失效，清行菜单）。
    last_cwd: PathBuf,
    /// 状态栏矩形（每帧更新；右键开模块菜单的命中区）。
    status_rect: egui::Rect,
    /// 模块加装菜单弹出位置（状态栏右键触发）。
    mod_menu: Option<(Pos2, u64)>,
    /// EQ 槽位（插件装载时由插件分配；关闭后音效保持）。
    eq_slot: Option<(u32, std::sync::Arc<tuneux_corex::EqParams>)>,
    /// 均衡器插件（第一方 wasm；验签装载失败 → 面板显示「未加载」提示）。
    eq_plugin: Option<tuneux_pinx::LoadedPlugin>,
    /// 压缩器插件。
    comp_plugin: Option<tuneux_pinx::LoadedPlugin>,
    filter_slot: Option<(u32, std::sync::Arc<tuneux_corex::FilterParams>)>,
    visual_plugins: Vec<tuneux_pinx::LoadedPlugin>,
    visual_text: String,
    /// 皮肤插件清单（名 → 调色板；与内置三套并存于皮肤菜单）。
    skin_plugins: Vec<(String, theme::Palette)>,
    /// 压缩器槽位（同上）。
    comp_slot: Option<(u32, std::sync::Arc<tuneux_corex::CompressorParams>)>,
    /// 输出设备清单快照（启动时枚举一次；选择仅提示——输出跟随系统）。
    output_devices: Vec<String>,
    /// 字体目录（内置文泉驿 + fonts/ 目录扫描）。
    fonts: Vec<theme::FontEntry>,
    /// 当前字体索引。
    font_idx: usize,
    /// 字体待应用（闭包外生效）。
    fonts_dirty: bool,
    /// 频谱字符字号（右键步进放大，独立于全局字号）。
    spectrum_font: f32,
    /// 静音前的音量记忆（现代顶栏静音按钮恢复用；None = 未静音）。
    volume_before_mute: Option<f32>,
    /// 现代播放列表列头排序键：0=默认（专辑+曲号）1=标题 2=艺术家
    /// 3=专辑 4=时长（会话级，不持久化）。
    pl_sort: u8,
    /// 列头排序方向（true = 降序）。
    pl_sort_desc: bool,
    /// 播放列表行拖拽重排的源条目索引（平铺视图）。
    pl_drag_from: Option<usize>,
    /// 指针表针物理（L/R 双针）。
    gauge_needles: [tuneux_commonx::gauge::NeedlePhys; 2],
    /// 指针表帧时刻（dt 计算）。
    gauge_last: Option<Instant>,
}

impl MaxAppV2 {
    /// 创建应用。
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = crate::config::load();
        let i18n = tuneux_commonx::build_i18n(&config.lang, crate::config::zh_table());
        let engine = match Engine::new(config.volume) {
            Ok(e) => Some(e),
            Err(err) => {
                eprintln!("[引擎] 初始化失败：{err}");
                None
            }
        };
        // 第一方插件装载（fx 同链路；失败静默降级——面板显示「未加载」）
        let eq_loaded = engine.as_ref().and_then(Self::load_eq_plugin);
        let comp_loaded = engine.as_ref().and_then(Self::load_comp_plugin);
        let skin_plugins = engine
            .as_ref()
            .map(Self::load_skin_plugins)
            .unwrap_or_default();
        let visual_loaded = engine
            .as_ref()
            .map(Self::load_visual_plugins)
            .unwrap_or_default();
        // 槽位 Arc 预取（Self{} 里 engine 会被 move，不能在字面量里再借）；
        // and_then+map 替代 unwrap：装载成功 ⇒ engine 必在，None 稳健传播零 panic
        let eq_slot_init = eq_loaded.as_ref().and_then(|(s, _)| {
            engine
                .as_ref()
                .map(|eng| (*s, eng.eq_slots()[*s as usize].clone()))
        });
        let comp_slot_init = comp_loaded.as_ref().and_then(|(s, _)| {
            engine
                .as_ref()
                .map(|eng| (*s, eng.compressor_slots()[*s as usize].clone()))
        });
        let scan_pool = tuneux_mediax::scan_pool::ScanPool::new();
        let palette = theme::builtin(&config.theme)
            .or_else(|| {
                skin_plugins
                    .iter()
                    .find(|(n, _)| *n == config.theme)
                    .map(|(_, p)| p.clone())
            })
            .unwrap_or_default();

        let start_dir = config
            .last_dir
            .clone()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
        let browser = FsBrowser::open(
            &start_dir,
            FsBrowserConfig {
                audio_exts: KNOWN_AUDIO_EXTS,
            },
        );

        // 字体目录：内置文泉驿 + fonts/ 扫描；按配置恢复上次选择
        let fonts = theme::font_catalog();
        let font_idx = fonts
            .iter()
            .position(|f| f.name == config.font)
            .unwrap_or(0);
        cc.egui_ctx
            .set_fonts(theme::build_fonts_with(&fonts[font_idx].data));

        // 播放列表状态恢复（mediax PlaylistState 规范格式）：
        // 多列表取活跃列表；旧版单列表字段（items/current/shuffle）作迁移入口。
        let mut playlist = PlaylistCore::new();
        let st = crate::config::load_state();
        let (items, shuffle, current) = if !st.lists.is_empty() {
            let active = st.active.min(st.lists.len() - 1);
            let l = &st.lists[active];
            (l.items.clone(), l.shuffle, l.current.clone())
        } else {
            (st.items.clone(), st.shuffle, st.current.clone())
        };
        let positions = st.positions.clone();
        let replay_gain_cache = st.replay_gain.clone();
        let last_cwd_init = browser.cwd().to_path_buf();
        playlist.add_many(items);
        playlist.set_shuffle(shuffle);
        if let Some(cur) = &current {
            if let Some(idx) = playlist.items().iter().position(|it| &it.path == cur) {
                playlist.set_current(idx);
            }
        }

        Self {
            ui: UiModel::default(),
            engine,
            browser,
            playlist,
            config,
            i18n,
            status: None,
            palette,
            replay_gain: false,
            browser_menu: None,
            playlist_menu: None,
            drag_browser: None,
            x_confirm_at: None,
            metadata: None,
            cover_tex: None,
            cover_path: None,
            lyrics: None,
            lyrics_path: None,
            current_path: None,
            spectrum_mode: SpectrumMode::Hidden,
            spectrum_peaks: tuneux_corex::spectrum::SpectrumPeakHold::new(
                tuneux_corex::spectrum::DEFAULT_PEAK_FALL_PER_SEC,
            ),
            last_frame: None,
            frame_tick: 0,
            metadata_cache: HashMap::new(),
            scroll_to_sel: false,
            collapsed: std::collections::HashSet::new(),
            consecutive_failures: 0,
            last_error: None,
            last_error_at: None,
            positions,
            replay_gain_cache,
            scan_pool,

            tag_added: 0,
            flash: None,
            flash_at: None,
            search_target_browser: false,
            search_focus_req: false,
            command_focus_req: false,
            media_keys: tuneux_commonx::media_key::spawn_media_key_listener(
                "org.mpris.MediaPlayer2.tuneux-max",
                "tuneux-max",
            ),
            album_covers: std::collections::HashMap::new(),
            track_envelope: None,
            pending_menu: None,
            dock_leaf_menu: None,
            leaf_page: 0,
            dock_drag: None,
            dock_swap: None,
            browser_rect: egui::Rect::NOTHING,
            playlist_rect: egui::Rect::NOTHING,
            blank_menu: None,
            menu_open_at_frame_start: false,
            last_cwd: last_cwd_init,
            status_rect: egui::Rect::NOTHING,
            mod_menu: None,
            eq_slot: eq_slot_init,
            comp_slot: comp_slot_init,
            eq_plugin: eq_loaded.map(|(_, p)| p),
            filter_slot: None,
            visual_plugins: visual_loaded,
            visual_text: String::new(),
            comp_plugin: comp_loaded.map(|(_, p)| p),
            skin_plugins,
            output_devices: tuneux_corex::Engine::output_devices(),
            fonts,
            font_idx,
            fonts_dirty: false,
            spectrum_font: FONT,
            volume_before_mute: None,
            pl_sort: 0,
            pl_sort_desc: false,
            pl_drag_from: None,
            gauge_needles: [
                tuneux_commonx::gauge::NeedlePhys::default(),
                tuneux_commonx::gauge::NeedlePhys::default(),
            ],
            gauge_last: None,
        }
    }

    /// 键盘输入 → UiAction（与 fx 键位表逐键一致）。
    fn handle_input(&mut self, ctx: &egui::Context) {
        // 搜索态：Esc 退出 / Enter 执行首个匹配；字符输入交给搜索框 widget
        if self.ui.mode == UiMode::Search {
            let mut end = false;
            let mut enter = false;
            ctx.input(|i| {
                if i.key_pressed(egui::Key::Escape) {
                    end = true;
                }
                if i.key_pressed(egui::Key::Enter) {
                    enter = true;
                }
            });
            if enter {
                self.search_enter();
            } else if end {
                self.end_search();
            }
            return;
        }
        // 命令态：Esc 取消 / Enter 执行；字符交给命令输入条 widget
        if self.ui.mode == UiMode::Command {
            let mut exec = false;
            let mut cancel = false;
            ctx.input(|i| {
                if i.key_pressed(egui::Key::Escape) {
                    cancel = true;
                }
                if i.key_pressed(egui::Key::Enter) {
                    exec = true;
                }
            });
            if exec {
                let c = self.ui.command_input.clone();
                self.execute(UiAction::CommandExecute(c));
            } else if cancel {
                self.execute(UiAction::CommandCancel);
            }
            return;
        }
        if self.ui.mode != UiMode::Normal {
            return;
        }
        // 字符键统一走 Event::Text（egui Key::X 在部分平台不稳定）
        let text_char = |i: &egui::InputState, c: char| -> bool {
            i.events
                .iter()
                .any(|e| matches!(e, egui::Event::Text(t) if t == &c.to_string()))
        };

        let mut nav = false;
        ctx.input(|i| {
            use egui::Key;
            let ctrl = i.modifiers.ctrl;
            let action = if text_char(i, 'q') || text_char(i, 'Q') {
                Some(UiAction::Quit)
            } else if i.key_pressed(Key::Space) {
                Some(UiAction::PlayToggle)
            } else if text_char(i, 'p') && !ctrl {
                Some(UiAction::PrevTrack)
            } else if text_char(i, 'n') && !ctrl {
                Some(UiAction::NextTrack)
            } else if text_char(i, 'r') && !ctrl {
                Some(UiAction::CycleRepeat)
            } else if text_char(i, 's') && !ctrl {
                Some(UiAction::ToggleShuffle)
            } else if i.key_pressed(Key::ArrowLeft) {
                Some(UiAction::SeekBack(5.0))
            } else if i.key_pressed(Key::ArrowRight) {
                Some(UiAction::SeekForward(5.0))
            } else if text_char(i, '+') || text_char(i, '=') {
                Some(UiAction::VolumeUp)
            } else if text_char(i, '-') {
                Some(UiAction::VolumeDown)
            } else if text_char(i, '[') {
                Some(UiAction::MediumPrev)
            } else if text_char(i, ']') {
                Some(UiAction::MediumNext)
            } else if text_char(i, 'm') && !ctrl {
                Some(UiAction::MediumCycle)
            } else if text_char(i, 'a') && !ctrl {
                Some(UiAction::AddCurrent)
            } else if text_char(i, 'b') && !ctrl {
                Some(UiAction::ToggleBrowser)
            } else if text_char(i, 'v') && !ctrl {
                // v 循环频谱模式：隐藏 → 半屏 → 全屏 → 隐藏
                Some(UiAction::ToggleSpectrum)
            } else if text_char(i, 'l') && !ctrl {
                Some(UiAction::ToggleLyrics)
            } else if text_char(i, 'x') && !ctrl {
                // 二次确认清空（3 秒窗口，与 fx 同口径）
                let now = Instant::now();
                if let Some(t) = self.x_confirm_at {
                    if now.duration_since(t).as_secs() < 3 {
                        self.x_confirm_at = None;
                        Some(UiAction::ClearPlaylist)
                    } else {
                        self.x_confirm_at = Some(now);
                        None
                    }
                } else {
                    self.x_confirm_at = Some(now);
                    None
                }
            } else if text_char(i, 'd') && !ctrl {
                Some(UiAction::DeleteSelected)
            } else if text_char(i, 'g') && !ctrl {
                Some(UiAction::ToggleGroupView)
            } else if text_char(i, 'c') && !ctrl {
                Some(UiAction::ToggleCover)
            } else if text_char(i, 'u') && !ctrl {
                self.reload_plugins();
                None
            } else if text_char(i, ':') {
                Some(UiAction::CommandStart)
            } else if text_char(i, '/') {
                Some(UiAction::SearchStart)
            } else if text_char(i, '?') {
                Some(UiAction::About)
            } else if i.key_pressed(Key::Tab) {
                Some(UiAction::CycleFocus)
            } else if i.key_pressed(Key::Escape) {
                Some(UiAction::NavBack)
            } else if i.key_pressed(Key::Backspace) {
                match self.ui.focus {
                    FocusTarget::Browser => {
                        self.browser.go_up();
                        None
                    }
                    FocusTarget::Playlist => {
                        self.ui.focus = FocusTarget::Browser;
                        None
                    }
                }
            } else if i.key_pressed(Key::Enter) {
                Some(UiAction::NavEnter)
            } else if i.key_pressed(Key::ArrowUp) || (text_char(i, 'k') && !ctrl) {
                nav = true;
                Some(UiAction::NavUp)
            } else if i.key_pressed(Key::ArrowDown) || (text_char(i, 'j') && !ctrl) {
                nav = true;
                Some(UiAction::NavDown)
            } else if i.key_pressed(Key::Home) {
                nav = true;
                Some(UiAction::NavHome)
            } else if i.key_pressed(Key::End) {
                nav = true;
                Some(UiAction::NavEnd)
            } else if i.key_pressed(Key::F5) {
                Some(UiAction::ToggleBrowser)
            } else if i.key_pressed(Key::F6) {
                Some(UiAction::ToggleCover)
            } else if i.key_pressed(Key::F7) {
                Some(UiAction::ToggleLyrics)
            } else if i.key_pressed(Key::F8) {
                Some(UiAction::ToggleSpectrum)
            } else if i.key_pressed(Key::F9) {
                Some(UiAction::ToggleEq)
            } else if i.key_pressed(Key::F10) {
                self.pending_menu = Some(0);
                None
            } else if !ctrl {
                // 数字键 1-8：直接打开对应菜单（fx 同款）
                match (1..=8u8).find(|d| text_char(i, char::from(b'0' + d))) {
                    Some(d) => {
                        self.pending_menu = Some((d - 1) as usize);
                        None
                    }
                    None => None,
                }
            } else {
                None
            };
            if let Some(a) = action {
                self.execute(a);
            }
        });
        if nav {
            self.scroll_to_sel = true;
        }
    }

    /// 执行 UI 动作（引擎/数据/dock 侧效果；UiModel::apply 处理焦点/模式/选中）。
    fn execute(&mut self, action: UiAction) {
        use UiAction::*;
        self.ui.apply(&action);

        match action {
            Quit => std::process::exit(0),
            PlayToggle => self.toggle_play(),
            NextTrack => {
                let outcome = self.playlist.next(self.config.repeat);
                self.handle_nav_outcome(outcome);
            }
            PrevTrack => {
                let outcome = self.playlist.prev(self.config.repeat);
                self.handle_nav_outcome(outcome);
            }
            SeekBack(secs) => self.seek_by(-secs),
            SeekForward(secs) => self.seek_by(secs),
            VolumeUp | VolumeDown => {
                if let Some(e) = &self.engine {
                    let step = if matches!(action, VolumeUp) {
                        0.05
                    } else {
                        -0.05
                    };
                    let v = (e.volume() + step).clamp(0.0, 1.0);
                    e.send(tuneux_corex::AudioCmd::SetVolume(v));
                    self.config.volume = v;
                }
            }
            CycleRepeat => {
                self.config.repeat = match self.config.repeat {
                    RepeatMode::Off => RepeatMode::List,
                    RepeatMode::List => RepeatMode::Single,
                    RepeatMode::Single => RepeatMode::Off,
                };
                // 预载目标随新策略刷新（fx 同口径）
                self.refresh_preload();
            }
            ToggleShuffle => {
                let on = !self.playlist.is_shuffle();
                self.playlist.set_shuffle(on);
                self.refresh_preload();
            }
            MediumCycle | MediumPrev | MediumNext => {
                if let Some(e) = &self.engine {
                    let all = PlaybackMedium::ALL;
                    let cur = e.medium();
                    let idx = all.iter().position(|&m| m == cur).unwrap_or(0);
                    let next = match action {
                        MediumPrev => (idx + all.len() - 1) % all.len(),
                        _ => (idx + 1) % all.len(),
                    };
                    e.set_medium(all[next]);
                }
            }
            // 指定介质直选（菜单/命令入口；越界索引忽略）
            MediumSet(i) => {
                if let Some(e) = &self.engine {
                    if let Some(&m) = PlaybackMedium::ALL.get(i as usize) {
                        e.set_medium(m);
                    }
                }
            }
            // 压缩器面板（与菜单 ToggleComp 同路径）
            ToggleCompressor => self.toggle_dock_module(dock::ModuleId::Comp),
            // 插件三面板：打开对应 dock 面板（插件在否由面板内部自示提示）
            PluginEq => self.toggle_dock_module(dock::ModuleId::Eq),
            PluginComp => self.toggle_dock_module(dock::ModuleId::Comp),
            PluginVisual => self.toggle_dock_module(dock::ModuleId::Visualizer),
            // 搜索结果导航：搜索激活时行集已过滤，复用列表导航（递归一次）
            SearchNavigate(down) => {
                self.execute(if down { NavDown } else { NavUp });
            }
            AddCurrent => {
                let entry = self
                    .browser
                    .entries()
                    .get(self.browser.selected())
                    .map(|e| (e.path().to_path_buf(), e.is_dir()));
                match entry {
                    Some((p, true)) => self.enqueue_scan(p),
                    Some((p, false)) => {
                        self.add_file_sync(&p);
                    }
                    None => {}
                }
            }
            DeleteSelected => {
                let rows = self.build_rows();
                if let Some(ri) = self.ui.playlist_selected {
                    if let Some(PlRow::Track { item, .. }) = rows.get(ri) {
                        let item = *item;
                        self.playlist.remove(item);
                        let after = self.build_rows();
                        self.ui.playlist_selected = if after.is_empty() {
                            None
                        } else {
                            Some(ri.min(after.len() - 1))
                        };
                    }
                }
            }
            UiAction::ToggleBrowser => self.toggle_dock_module(dock::ModuleId::Browser),
            UiAction::ToggleCover => self.toggle_dock_module(dock::ModuleId::Cover),
            UiAction::ToggleLyrics => self.toggle_dock_module(dock::ModuleId::Lyrics),
            UiAction::ToggleEq => self.toggle_dock_module(dock::ModuleId::Eq),
            UiAction::ToggleSpectrum => {
                let has = self
                    .config
                    .dock
                    .as_ref()
                    .is_some_and(|d| d.contains(dock::ModuleId::Spectrum));
                if !has {
                    self.spectrum_mode = SpectrumMode::Full;
                    self.toggle_dock_module(dock::ModuleId::Spectrum);
                } else if self.spectrum_mode == SpectrumMode::Full {
                    self.spectrum_mode = SpectrumMode::Oscilloscope;
                } else {
                    self.toggle_dock_module(dock::ModuleId::Spectrum);
                    self.spectrum_mode = SpectrumMode::Full;
                }
            }
            CommandStart => {
                self.command_focus_req = true;
            }
            CommandExecute(cmd) => self.execute_command(&cmd),
            CommandCancel => {}
            SearchStart => {
                self.search_target_browser = self.ui.focus == FocusTarget::Browser;
                if self.search_target_browser {
                    self.browser.begin_search();
                }
                self.search_focus_req = true;
            }
            SearchExit => self.end_search(),
            ClearPlaylist => {
                self.playlist.clear();
                self.ui.playlist_selected = None;
                self.collapsed.clear();
            }
            ToggleGroupView => {
                self.config.view = match self.config.view {
                    PlaylistView::Flat => PlaylistView::ByAlbum,
                    PlaylistView::ByAlbum => PlaylistView::Flat,
                };
                self.ui.playlist_selected = None;
            }
            NavUp | NavDown | NavHome | NavEnd => {
                let len = match self.ui.focus {
                    FocusTarget::Browser => self.browser.entries().len(),
                    FocusTarget::Playlist => self.build_rows().len(),
                };
                if len == 0 {
                    return;
                }
                let cur = match self.ui.focus {
                    FocusTarget::Browser => self.browser.selected(),
                    FocusTarget::Playlist => self.ui.playlist_selected.unwrap_or(0),
                };
                let next = match action {
                    NavUp => cur.saturating_sub(1),
                    NavDown => (cur + 1).min(len - 1),
                    NavHome => 0,
                    NavEnd => len - 1,
                    _ => cur,
                };
                match self.ui.focus {
                    FocusTarget::Browser => self.browser.select(next),
                    FocusTarget::Playlist => self.ui.playlist_selected = Some(next),
                }
            }
            NavEnter => match self.ui.focus {
                FocusTarget::Browser => {
                    let idx = self.browser.selected();
                    let entry = self
                        .browser
                        .entries()
                        .get(idx)
                        .map(|e| (e.path().to_path_buf(), e.is_dir()));
                    if let Some((p, is_dir)) = entry {
                        if is_dir {
                            self.browser.enter_selected();
                        } else if let Some(idx) = self.add_file_sync(&p) {
                            self.play_item(idx);
                        }
                    }
                }
                FocusTarget::Playlist => {
                    let rows = self.build_rows();
                    if let Some(ri) = self.ui.playlist_selected {
                        match rows.get(ri) {
                            Some(PlRow::Track { item, .. }) => {
                                self.play_item(*item);
                            }
                            Some(PlRow::Header { album, .. }) => {
                                let a = album.clone();
                                self.toggle_collapse(&a);
                            }
                            None => {}
                        }
                    }
                }
            },
            _ => {}
        }
    }

    /// 构建可见行（fx display_rows/visible_rows 同款）：
    /// 平铺 = 显示序（专辑+曲号排序）全曲目，序号 = 大排行；
    /// 分组 = 按专辑连续段分组（组头 + 专辑内序号 1..N），折叠专辑只留组头。
    fn build_rows(&self) -> Vec<PlRow> {
        let order = self.playlist.display_order();
        let items = self.playlist.items();
        // 播放列表搜索态：平铺过滤行（文件名/标题/艺术家 子串匹配，忽略大小写）
        if self.ui.mode == UiMode::Search
            && !self.search_target_browser
            && !self.ui.search_query.is_empty()
        {
            let q = self.ui.search_query.to_lowercase();
            let mut seq = 0usize;
            return order
                .into_iter()
                .filter_map(|i| {
                    let it = &items[i];
                    let stem = it
                        .path
                        .file_stem()
                        .and_then(|st| st.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    let md = self.metadata_cache.get(&it.path);
                    let title_hit = md
                        .and_then(|m| m.title.as_ref())
                        .is_some_and(|t| t.to_lowercase().contains(&q));
                    let artist_hit = md
                        .and_then(|m| m.artist.as_ref())
                        .is_some_and(|a| a.to_lowercase().contains(&q));
                    if stem.contains(&q) || title_hit || artist_hit {
                        seq += 1;
                        Some(PlRow::Track { item: i, seq })
                    } else {
                        None
                    }
                })
                .collect();
        }
        if self.config.view == PlaylistView::Flat {
            return order
                .iter()
                .enumerate()
                .map(|(n, &i)| PlRow::Track {
                    item: i,
                    seq: n + 1,
                })
                .collect();
        }
        let mut rows = Vec::new();
        let mut album = String::new();
        let mut group: Vec<usize> = Vec::new();
        let mut have = false;
        for &i in &order {
            let a = items[i].album.clone().unwrap_or_default();
            if have && a == album {
                group.push(i);
                continue;
            }
            if have {
                Self::push_group(&mut rows, &album, &group, &self.collapsed);
            }
            album = a;
            group = vec![i];
            have = true;
        }
        if have {
            Self::push_group(&mut rows, &album, &group, &self.collapsed);
        }
        rows
    }

    fn push_group(
        rows: &mut Vec<PlRow>,
        album: &str,
        group: &[usize],
        collapsed: &std::collections::HashSet<String>,
    ) {
        rows.push(PlRow::Header {
            album: album.to_string(),
            count: group.len(),
        });
        if collapsed.contains(album) {
            return;
        }
        for (seq, &item) in group.iter().enumerate() {
            rows.push(PlRow::Track { item, seq: seq + 1 });
        }
    }

    /// 切换专辑折叠态。
    fn toggle_collapse(&mut self, album: &str) {
        if !self.collapsed.remove(album) {
            self.collapsed.insert(album.to_string());
        }
    }

    /// 退出搜索态（浏览器恢复未过滤列表）。
    fn end_search(&mut self) {
        if self.search_target_browser {
            self.browser.end_search();
        }
        self.ui.mode = UiMode::Normal;
        self.ui.search_query.clear();
    }

    /// 搜索态 Enter：对首个可见项执行进入/播放，随后退出搜索（fx 同口径）。
    fn search_enter(&mut self) {
        if self.search_target_browser {
            let first = self
                .browser
                .entries()
                .first()
                .map(|e| (e.path().to_path_buf(), e.is_dir()));
            self.end_search();
            if let Some((p, is_dir)) = first {
                if is_dir {
                    self.browser.navigate_to(&p);
                } else if let Some(idx) = self.add_file_sync(&p) {
                    self.play_item(idx);
                }
            }
        } else {
            let rows = self.build_rows();
            let first_track = rows.iter().find_map(|r| match r {
                PlRow::Track { item, .. } => Some(*item),
                PlRow::Header { .. } => None,
            });
            self.end_search();
            if let Some(item) = first_track {
                self.play_item(item);
            }
        }
    }

    // ==================== EQ / 压缩器面板（fx draw_equalizer/draw_compressor 对应） ====================

    /// 传输工具条（f2k 现代模块）：传输键 + seek 滑条 + 音量滑条。
    fn render_transport(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        let (dur, pos, playing) = match &self.status {
            Some(s) => (s.duration, s.position, s.playing),
            None => (0.0, 0.0, false),
        };
        let vol = self
            .engine
            .as_ref()
            .map(|e| e.volume())
            .unwrap_or(self.config.volume);
        let mut seek = pos;
        let mut volc = vol;
        let mut cmd: u8 = 0;
        egui::TopBottomPanel::top("transport")
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::same(3)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("◀◀").clicked() {
                        cmd = 1;
                    }
                    let pp = if playing { "▮▮" } else { "▶" };
                    if ui.button(pp).clicked() {
                        cmd = 2;
                    }
                    if ui.button("▶▶").clicked() {
                        cmd = 3;
                    }
                    if ui.button("■").clicked() {
                        cmd = 4;
                    }
                    ui.add_space(10.0);
                    ui.add(egui::Slider::new(&mut seek, 0.0..=dur.max(0.1)).show_value(false));
                    ui.label(format!("{} / {}", fmt_time(pos), fmt_time(dur)));
                    ui.add_space(10.0);
                    ui.add(egui::Slider::new(&mut volc, 0.0..=1.0).show_value(false));
                });
            });
        let seek_delta = (seek - pos).abs() > 0.05;
        let vol_delta = (volc - vol).abs() > 1e-4;
        if seek_delta || vol_delta || cmd == 4 {
            if let Some(e) = &self.engine {
                if seek_delta {
                    e.send(tuneux_corex::AudioCmd::Seek(seek));
                }
                if vol_delta {
                    e.send(tuneux_corex::AudioCmd::SetVolume(volc));
                }
                if cmd == 4 {
                    e.send(tuneux_corex::AudioCmd::Pause);
                    e.send(tuneux_corex::AudioCmd::Seek(0.0));
                }
            }
        }
        if vol_delta {
            self.config.volume = volc;
        }
        match cmd {
            1 => {
                let o = self.playlist.prev(self.config.repeat);
                self.handle_nav_outcome(o);
            }
            2 => self.toggle_play(),
            3 => {
                let o = self.playlist.next(self.config.repeat);
                self.handle_nav_outcome(o);
            }
            _ => {}
        }
    }

    // ==================== 播放生命周期（fx playback.rs 同款语义） ====================

    /// 取走引擎滞留的测量增益并排空残留事件（防旧事件触发额外自动切曲）。
    fn drain_residual_events(&self) -> Option<f64> {
        let e = self.engine.as_ref()?;
        let gain = e.take_measured_gain_db();
        while e.poll_track_switched().is_some() {}
        while e.poll_finished().is_some() {}
        while e.poll_failed().is_some() {}
        gain
    }

    /// 保存当前曲目进度（CUE 分轨不写——断点表以文件路径为键，
    /// 整轨位置会覆盖该文件作为普通曲目的续播点）。
    fn save_current_position(&mut self) {
        let is_cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .is_some_and(|it| it.cue.is_some());
        if is_cue {
            return;
        }
        if let (Some(p), Some(e)) = (&self.current_path, &self.engine) {
            let pos = e.position();
            if pos > 0.5 {
                self.positions.insert(p.clone(), pos);
            } else {
                self.positions.remove(p);
            }
        }
    }

    /// RG 缓存查表：dB → 线性增益（无缓存 = 1.0）。
    fn rg_linear(&self, path: &PathBuf) -> f32 {
        self.replay_gain_cache
            .get(path)
            .copied()
            .map(|db| 10f64.powf(db / 20.0) as f32)
            .unwrap_or(1.0)
    }

    /// 切到列表指定项并播放（fx play_and_update_current 同款全流程）：
    /// 存旧位置 → set_current → 排空残留 → CUE 区间/断点续播/整文件三选一
    /// → 应用 RG 缓存 → 预载下一曲（gapless）→ 归档旧曲测量增益。
    fn play_item(&mut self, index: usize) {
        self.save_current_position();
        if !self.playlist.set_current(index) {
            return;
        }
        let Some(item) = self.playlist.items().get(index) else {
            return;
        };
        let path = item.path.clone();
        let cue = item.cue.clone();
        let album = item.album.clone();
        // 时长供断点判定（距结尾 5 秒内视为听完，从头播）
        let md = self
            .metadata_cache
            .entry(path.clone())
            .or_insert_with(|| TrackMetadata::from_file(&path));
        let duration = md.duration;
        let prev_path = self.current_path.clone();
        self.current_path = Some(path.clone());
        // 切曲清零频谱白帽（007 跨产品约定）
        self.spectrum_peaks.reset();
        // 展开所在专辑（分组视图下当前曲可见）
        if let Some(a) = &album {
            self.collapsed.remove(a);
        }
        let stale_gain = self.drain_residual_events();
        if let Some(e) = &self.engine {
            if let Some(c) = cue.as_ref() {
                e.send(tuneux_corex::AudioCmd::PlayRange {
                    path: path.clone(),
                    start_secs: c.start_ms as f64 / 1000.0,
                    end_secs: c.end_ms.map(|ms| ms as f64 / 1000.0),
                });
            } else {
                let resume =
                    tuneux_mediax::resume_secs(self.positions.get(&path).copied(), duration);
                match resume {
                    Some(secs) => e.send(tuneux_corex::AudioCmd::PlayResume {
                        path: path.clone(),
                        secs,
                    }),
                    None => e.send(tuneux_corex::AudioCmd::Play(path.clone())),
                }
            }
            e.set_replay_gain(self.rg_linear(&path));
            let next = self
                .playlist
                .peek_next_item(self.config.repeat)
                .map(preload_target_of);
            e.send(tuneux_corex::AudioCmd::PreloadNext(next));
        }
        if let (Some(db), Some(old)) = (stale_gain, prev_path) {
            self.replay_gain_cache.insert(old, db);
        }
    }

    /// NavOutcome → 播放操作（fx handle_nav_outcome 同款，含单曲循环决策）。
    fn handle_nav_outcome(&mut self, outcome: tuneux_mediax::NavOutcome) {
        use tuneux_mediax::{NavOutcome, PlaybackDecision};
        match outcome {
            NavOutcome::Switch(idx) => self.play_item(idx),
            NavOutcome::Repeat => {
                let cur = self
                    .playlist
                    .current_index()
                    .and_then(|i| self.playlist.items().get(i))
                    .map(|it| (it.path.clone(), it.cue.clone()));
                if let Some((path, cue)) = cur {
                    let stale = self.drain_residual_events();
                    if let Some(e) = &self.engine {
                        let d = tuneux_mediax::single_repeat_decision(e.is_playing(), cue.as_ref());
                        match d {
                            PlaybackDecision::Seek { secs } => {
                                e.send(tuneux_corex::AudioCmd::Seek(secs))
                            }
                            PlaybackDecision::PlayRange {
                                start_secs,
                                end_secs,
                            } => e.send(tuneux_corex::AudioCmd::PlayRange {
                                path: path.clone(),
                                start_secs,
                                end_secs,
                            }),
                            PlaybackDecision::Play => {
                                e.send(tuneux_corex::AudioCmd::Play(path.clone()))
                            }
                            _ => {}
                        }
                        // 单曲循环无「下一曲」：显式取消残留预载
                        e.send(tuneux_corex::AudioCmd::PreloadNext(None));
                        e.set_replay_gain(self.rg_linear(&path));
                    }
                    if let Some(db) = stale {
                        self.replay_gain_cache.insert(path, db);
                    }
                }
            }
            NavOutcome::End => {}
        }
    }

    /// Gapless 无缝切曲后的前端同步（fx advance_ui_on_gapless 同款）：
    /// 解码线程已切到预载曲目——只更新 UI 状态，绝不重发 Play。
    fn advance_ui_on_gapless(&mut self) {
        let outcome = self.playlist.next(self.config.repeat);
        let tuneux_mediax::NavOutcome::Switch(idx) = outcome else {
            return;
        };
        if !self.playlist.set_current(idx) {
            return;
        }
        let meta = self
            .playlist
            .items()
            .get(idx)
            .map(|it| (it.path.clone(), it.album.clone()));
        if let Some((path, album)) = meta {
            self.current_path = Some(path.clone());
            if let Some(a) = &album {
                self.collapsed.remove(a);
            }
            if let Some(e) = &self.engine {
                e.set_replay_gain(self.rg_linear(&path));
            }
        }
        if let Some(e) = &self.engine {
            let next = self
                .playlist
                .peek_next_item(self.config.repeat)
                .map(preload_target_of);
            e.send(tuneux_corex::AudioCmd::PreloadNext(next));
        }
    }

    /// 播放/暂停切换（fx toggle_play 同款：mediax 纯函数决策 + 末尾态双判据）。
    fn toggle_play(&mut self) {
        if self.current_path.is_none() {
            return;
        }
        let cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.clone());
        let (is_playing, at_end) = match &self.engine {
            Some(e) => {
                let end_secs = match cue.as_ref().and_then(|c| c.end_ms) {
                    Some(ms) => ms as f64 / 1000.0,
                    None => e.duration(),
                };
                (
                    e.is_playing(),
                    e.at_eof() || (end_secs > 0.0 && e.position() >= end_secs - 0.1),
                )
            }
            None => return,
        };
        let decision = tuneux_mediax::toggle_decision(is_playing, at_end, cue.as_ref());
        use tuneux_mediax::PlaybackDecision as PD;
        match decision {
            PD::Pause => {
                self.save_current_position();
                if let Some(e) = &self.engine {
                    e.send(tuneux_corex::AudioCmd::Pause);
                }
            }
            PD::Resume => {
                if let Some(e) = &self.engine {
                    e.send(tuneux_corex::AudioCmd::Resume);
                }
            }
            PD::Play => {
                let stale = self.drain_residual_events();
                let p = self.current_path.clone();
                if let (Some(e), Some(p)) = (&self.engine, p) {
                    e.send(tuneux_corex::AudioCmd::Play(p.clone()));
                    if let Some(db) = stale {
                        self.replay_gain_cache.insert(p, db);
                    }
                }
            }
            PD::PlayRange {
                start_secs,
                end_secs,
            } => {
                let stale = self.drain_residual_events();
                let p = self.current_path.clone();
                if let (Some(e), Some(p)) = (&self.engine, p) {
                    e.send(tuneux_corex::AudioCmd::PlayRange {
                        path: p.clone(),
                        start_secs,
                        end_secs,
                    });
                    if let Some(db) = stale {
                        self.replay_gain_cache.insert(p, db);
                    }
                }
            }
            _ => {}
        }
    }

    /// 相对 seek（钳制 [0, dur]，CUE 分轨再钳到本曲区间）。
    fn seek_by(&mut self, delta: f64) {
        let (cur, dur) = match &self.engine {
            Some(e) => (e.position(), e.duration()),
            None => return,
        };
        let mut np = cur + delta;
        if np < 0.0 {
            np = 0.0;
        }
        if dur > 0.0 && np > dur {
            np = dur;
        }
        let cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.clone());
        let np = tuneux_mediax::clamp_seek_to_cue(np, cue.as_ref());
        if let Some(e) = &self.engine {
            e.send(tuneux_corex::AudioCmd::Seek(np));
        }
    }

    /// 重发预载（循环/随机模式切换后调用，保证预载目标与新策略一致）。
    fn refresh_preload(&mut self) {
        if let Some(e) = &self.engine {
            let next = self
                .playlist
                .peek_next_item(self.config.repeat)
                .map(preload_target_of);
            e.send(tuneux_corex::AudioCmd::PreloadNext(next));
        }
    }

    /// 拉取引擎错误 + 清理过期提示（每帧调用，fx refresh_last_error 同款）。
    fn refresh_last_error(&mut self) {
        if let Some(e) = &self.engine {
            if let Some(err) = e.take_last_error() {
                self.last_error = Some(err);
                self.last_error_at = Some(Instant::now());
            }
        }
        if let Some(at) = self.last_error_at {
            if at.elapsed() > std::time::Duration::from_secs(5) {
                self.last_error = None;
                self.last_error_at = None;
            }
        }
    }

    /// 模块可见性 = 皮肤默认集 + 该皮肤的用户覆盖（右键/菜单写入 mod_over）。
    fn mod_on(&self, key: &str) -> bool {
        let skin = self.config.theme.as_str();
        let dflt = theme::default_modules(skin)
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .unwrap_or(false);
        self.config
            .mod_over
            .get(skin)
            .and_then(|m| m.get(key))
            .copied()
            .unwrap_or(dflt)
    }

    /// 模块菜单条目（key, 显示名）。
    fn mod_entries(&self) -> Vec<(&'static str, String)> {
        theme::default_modules(&self.config.theme)
            .iter()
            .map(|(k, _)| (*k, self.i18n.t(&format!("mod.{k}")).into_owned()))
            .collect()
    }

    /// 切换模块开关（写入当前皮肤的覆盖表）。
    fn toggle_mod(&mut self, key: &str) {
        let on = self.mod_on(key);
        self.config
            .mod_over
            .entry(self.config.theme.clone())
            .or_default()
            .insert(key.to_string(), !on);
    }

    /// 分割树递归：叶 = 模块盒；内节点 = 可拖分割条（双击回 50/50）。
    fn render_node(
        &mut self,
        ui: &mut Ui,
        pal: &Palette,
        rect: egui::Rect,
        node: &mut dock::DockNode,
        depth: u8,
    ) {
        if depth > 8 || rect.width() < 40.0 || rect.height() < 40.0 {
            return;
        }
        match node {
            dock::DockNode::Leaf { module } => {
                let module = *module;
                // 现代模式：圆角卡片叶（头部条 + 关闭× + 拖拽/右键，语义与经典同源）
                if self.config.modern {
                    self.render_leaf_modern(ui, rect, module);
                    return;
                }
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    // 独立 id 作用域：ScrollArea/Slider 等状态按 ui id 键控，
                    // 不 push_id 则各叶子的滚动偏移共用同一键 = 同步滚动 bug
                    ui.push_id(format!("dock_{module:?}"), |ui| {
                        self.render_module(module, ui);
                    });
                });
                // 标题带：右键 = 模块菜单（f2k 同款：关闭/拆分加装/更换）；
                // 拖拽 = 换位（拖到别的叶子松手交换）
                let band = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), 16.0));
                if !self.menu_open_at_frame_start
                    && ui.input(|i| i.pointer.secondary_clicked())
                    && ui
                        .input(|i| i.pointer.hover_pos())
                        .is_some_and(|pp| band.contains(pp))
                {
                    self.dock_leaf_menu = Some((
                        module,
                        ui.input(|i| i.pointer.hover_pos()).unwrap_or_default(),
                        self.frame_tick,
                    ));
                    self.leaf_page = 0;
                }
                let band_resp = ui
                    .allocate_rect(band, egui::Sense::drag())
                    .on_hover_cursor(egui::CursorIcon::Grab);
                if band_resp.drag_started() {
                    self.dock_drag = Some(module);
                }
                if let Some(d) = self.dock_drag {
                    if d != module
                        && ui.input(|i| i.pointer.primary_released())
                        && ui
                            .input(|i| i.pointer.hover_pos())
                            .is_some_and(|pp| rect.contains(pp))
                    {
                        self.dock_swap = Some((d, module));
                    }
                }
            }
            dock::DockNode::Split {
                side_by_side,
                ratio,
                a,
                b,
            } => {
                let r = ratio.clamp(0.08, 0.92);
                // 分割条宽度：现代模式更细（10px）更轻；经典 18px
                let handle_sz = if self.config.modern { 10.0 } else { 18.0 };
                let (a_rect, handle, b_rect) = if *side_by_side {
                    let aw = (rect.width() - handle_sz) * r;
                    let ar = egui::Rect::from_min_size(rect.min, Vec2::new(aw, rect.height()));
                    let h = egui::Rect::from_min_size(
                        Pos2::new(ar.right(), rect.top()),
                        Vec2::new(handle_sz, rect.height()),
                    );
                    let br = egui::Rect::from_min_size(
                        Pos2::new(h.right(), rect.top()),
                        Vec2::new((rect.right() - h.right()).max(0.0), rect.height()),
                    );
                    (ar, h, br)
                } else {
                    let ah = (rect.height() - handle_sz) * r;
                    let ar = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width(), ah));
                    let h = egui::Rect::from_min_size(
                        Pos2::new(rect.left(), ar.bottom()),
                        Vec2::new(rect.width(), handle_sz),
                    );
                    let br = egui::Rect::from_min_size(
                        Pos2::new(rect.left(), h.bottom()),
                        Vec2::new(rect.width(), (rect.bottom() - h.bottom()).max(0.0)),
                    );
                    (ar, h, br)
                };
                self.render_node(ui, pal, a_rect, a, depth + 1);
                let hresp = ui.allocate_rect(handle, egui::Sense::click_and_drag());
                {
                    let pt = ui.painter_at(handle.expand(2.0));
                    // 手柄底色：现代 = 透明（背景透出，悬停才亮）；经典 = 沉底色
                    if !self.config.modern {
                        pt.rect_filled(handle, 0.0, pal.panel_bg);
                    }
                    // 悬停/拖拽高亮仅现代模式（经典路径观感零变化）
                    let grip_col = if self.config.modern && (hresp.hovered() || hresp.dragged()) {
                        pal.accent
                    } else {
                        pal.border
                    };
                    let c = handle.center();
                    if *side_by_side {
                        pt.line_segment(
                            [Pos2::new(c.x, c.y - 14.0), Pos2::new(c.x, c.y + 14.0)],
                            egui::Stroke::new(
                                if self.config.modern { 3.0_f32 } else { 1.0_f32 },
                                grip_col,
                            ),
                        );
                    } else {
                        pt.line_segment(
                            [Pos2::new(c.x - 14.0, c.y), Pos2::new(c.x + 14.0, c.y)],
                            egui::Stroke::new(
                                if self.config.modern { 3.0_f32 } else { 1.0_f32 },
                                grip_col,
                            ),
                        );
                    }
                }
                let hresp = hresp.on_hover_cursor(if *side_by_side {
                    egui::CursorIcon::ResizeHorizontal
                } else {
                    egui::CursorIcon::ResizeVertical
                });
                if hresp.dragged() {
                    if let Some(pp) = hresp.interact_pointer_pos() {
                        *ratio = if *side_by_side {
                            ((pp.x - rect.left()) / rect.width().max(1.0)).clamp(0.08, 0.92)
                        } else {
                            ((pp.y - rect.top()) / rect.height().max(1.0)).clamp(0.08, 0.92)
                        };
                    }
                }
                if hresp.double_clicked() {
                    *ratio = 0.5;
                }
                self.render_node(ui, pal, b_rect, b, depth + 1);
                // 右键（两侧都渲染完再处理，避免当帧半旧半新）：
                // 交换两侧 = 镜像（子树换位 + 比例翻转，各模块保持原尺寸）；
                // Shift+右键 = 改分割方向（原操作保留）
                if hresp.secondary_clicked() {
                    if ui.input(|i| i.modifiers.shift) {
                        *side_by_side = !*side_by_side;
                    } else {
                        std::mem::swap(a, b);
                        *ratio = 1.0 - *ratio;
                    }
                }
            }
        }
    }

    /// 模块分发（dock 叶子渲染入口）：现代模式走 modern 层，经典走原路径。
    fn render_module(&mut self, m: dock::ModuleId, ui: &mut Ui) {
        if self.config.modern {
            self.render_module_modern(m, ui);
            return;
        }
        match m {
            dock::ModuleId::Browser => self.render_browser_in(ui),
            dock::ModuleId::Playlist => self.render_playlist_in(ui),
            dock::ModuleId::Spectrum => match self.spectrum_mode {
                SpectrumMode::Oscilloscope => self.render_oscilloscope_in(ui),
                _ => self.render_spectrum_in(ui),
            },
            dock::ModuleId::Cover => self.render_cover_in(ui),
            dock::ModuleId::Lyrics => self.render_lyrics_in(ui),
            dock::ModuleId::Eq => self.render_eq_in(ui),
            dock::ModuleId::Comp => self.render_comp_in(ui),
            dock::ModuleId::Waveform => self.render_waveform_in(ui),
            dock::ModuleId::Filter => self.render_filter_in(ui),
            dock::ModuleId::Visualizer => self.render_visualizer_in(ui),
            dock::ModuleId::DspChain => self.render_dsp_chain_in(ui),
            dock::ModuleId::VuGauge => self.render_gauge_module(ui),
        }
    }

    /// 加装/移除 dock 模块（右键关闭 / 菜单切换 / 快捷键共用）。
    fn toggle_dock_module(&mut self, m: dock::ModuleId) {
        let tree = self.config.dock.take();
        self.config.dock = match tree {
            None => Some(dock::DockNode::leaf(m)),
            Some(t) => {
                if t.contains(m) {
                    t.prune(m)
                } else if m.prefers_vertical() {
                    Some(t.append_small(m))
                } else {
                    Some(t.append_right(m))
                }
            }
        };
    }

    // ==================== 命令模式（fx execute_command 同款语义） ====================

    // ==================== 插件装载（fx 同链路：验签 + 能力 + journal） ====================

    /// 插件装载核查日志（首次/重复各记一条；与 fx 分文件）。
    fn record_plugin_load(
        log_id: &str,
        tristate: tuneux_pinx::Tristate,
        granted: Vec<tuneux_pinx::Capability>,
    ) {
        let journal = tuneux_pinx::journal::Journal::new(crate::config::plugin_log_path());
        let seen = std::fs::read_to_string(journal.path())
            .map(|c| c.lines().any(|l| l.contains(&format!("plugin={log_id} "))))
            .unwrap_or(false);
        let rec =
            tuneux_pinx::journal::LoadRecord::now(log_id.to_string(), tristate, granted, !seen);
        let _ = journal.append(&rec);
    }

    /// 装载前置（读 .wasm/.manifest/.sig → 清单 → 验签 → 宿主装载）。
    /// 任一步失败返回 None（调用方静默降级）。
    fn load_first_party_core(
        engine: &tuneux_corex::Engine,
        name: &str,
        id: &str,
        allowed: Option<&[tuneux_pinx::Capability]>,
    ) -> Option<(tuneux_pinx::LoadedPlugin, Vec<tuneux_pinx::Capability>)> {
        let dir = crate::config::plugins_dir();
        let wasm = std::fs::read(dir.join(format!("{name}.wasm"))).ok()?;
        let manifest = std::fs::read_to_string(dir.join(format!("{name}.manifest"))).ok()?;
        let sig: [u8; 64] = std::fs::read(dir.join(format!("{name}.sig")))
            .ok()?
            .try_into()
            .ok()?;
        let requested = tuneux_pinx::parse_manifest(&manifest).ok()?;
        let allowed = allowed.unwrap_or(&requested);
        let granted: Vec<tuneux_pinx::Capability> = requested
            .iter()
            .filter(|c| allowed.contains(c))
            .copied()
            .collect();
        let host =
            tuneux_pinx::WasmHost::new(100_000, 4, engine.eq_slots(), engine.compressor_slots());
        let trust = tuneux_pinx::TrustList::from_parts(vec![OFFICIAL_PUBKEY]);
        let plugin = host
            .load_enforced(
                &wasm,
                id,
                manifest.as_bytes(),
                Some((&OFFICIAL_PUBKEY, &sig)),
                &trust,
                &requested,
                allowed,
                tuneux_pinx::LoadPolicy::Silent,
            )
            .ok()?;
        Some((plugin, granted))
    }

    /// 装载效果器插件（槽位在验签通过后才分配；init 携带槽位号）。
    fn load_fx_plugin(
        engine: &tuneux_corex::Engine,
        name: &str,
        id: &str,
        alloc: impl FnOnce(&tuneux_corex::Engine) -> Option<u32>,
    ) -> Option<(u32, tuneux_pinx::LoadedPlugin)> {
        let (mut plugin, granted) = Self::load_first_party_core(engine, name, id, None)?;
        let slot = alloc(engine)?;
        plugin.call_init(slot).ok()?;
        Self::record_plugin_load(id, plugin.tristate, granted);
        Some((slot, plugin))
    }

    /// 装载均衡器插件（共享引擎均衡器槽位）。
    fn load_eq_plugin(engine: &tuneux_corex::Engine) -> Option<(u32, tuneux_pinx::LoadedPlugin)> {
        Self::load_fx_plugin(engine, "equalizer", tuneux_pinx::EQ_ID, |e| {
            e.alloc_eq_slot().map(|(s, _)| s)
        })
    }

    /// 装载压缩器插件（共享引擎压缩器槽位）。
    fn load_comp_plugin(engine: &tuneux_corex::Engine) -> Option<(u32, tuneux_pinx::LoadedPlugin)> {
        Self::load_fx_plugin(engine, "compressor", tuneux_pinx::COMP_ID, |e| {
            e.alloc_compressor_slot().map(|(s, _)| s)
        })
    }

    /// 装载全部皮肤插件（皮肤-*.wasm，Theme 能力窄集；term 基底覆盖）。
    fn load_skin_plugins(engine: &tuneux_corex::Engine) -> Vec<(String, theme::Palette)> {
        let allowed = [tuneux_pinx::Capability::Theme];
        let dir = crate::config::plugins_dir();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let pp = e.path();
                        let stem = pp.file_stem()?.to_str()?.to_owned();
                        (pp.extension()?.to_str()? == "wasm" && stem.starts_with("皮肤-"))
                            .then_some(stem)
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let mut out = Vec::new();
        for stem in names {
            let Some((mut plugin, granted)) =
                Self::load_first_party_core(engine, &stem, tuneux_pinx::SKIN_ID, Some(&allowed))
            else {
                continue;
            };
            if plugin.call_init(0).is_err() {
                continue;
            }
            Self::record_plugin_load(&format!("tuneux-skin:{stem}"), plugin.tristate, granted);
            let Some(text) = plugin.theme().and_then(|b| std::str::from_utf8(b).ok()) else {
                continue;
            };
            let Some(pal) = theme::from_skin(text) else {
                continue;
            };
            let fallback = stem.trim_start_matches("皮肤-").to_string();
            let name = theme::skin_name(text).unwrap_or(fallback);
            out.push((name, pal));
        }
        out
    }

    /// 装载可视化插件（可视化-*.wasm，MeterRead 能力窄集）。
    fn load_visual_plugins(engine: &tuneux_corex::Engine) -> Vec<tuneux_pinx::LoadedPlugin> {
        let allowed = [tuneux_pinx::Capability::MeterRead];
        let dir = crate::config::plugins_dir();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let pp = e.path();
                        let stem = pp.file_stem()?.to_str()?.to_owned();
                        (pp.extension()?.to_str()? == "wasm" && stem.starts_with("可视化-"))
                            .then_some(stem)
                    })
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        let mut out = Vec::new();
        for stem in names {
            let Some((mut plugin, granted)) =
                Self::load_first_party_core(engine, &stem, "tuneux-vis", Some(&allowed))
            else {
                continue;
            };
            if plugin.call_init(0).is_err() {
                continue;
            }
            Self::record_plugin_load(&format!("tuneux-visual:{stem}"), plugin.tristate, granted);
            out.push(plugin);
        }
        out
    }

    /// u 键：重新装载插件（fx 同款；已装载的效果器跳过防重复占槽）。
    fn reload_plugins(&mut self) {
        // Engine 非 Clone（含原始线程 JoinHandle）——先在借用块内完成全部
        // 装载，再统一写入字段，避免「借 self.engine 同时写 self.eq_plugin」
        let eq_new = self
            .engine
            .as_ref()
            .filter(|_| self.eq_plugin.is_none())
            .and_then(Self::load_eq_plugin)
            // 装载成功 ⇒ engine 必在；map 替代 unwrap，None 稳健传播零 panic
            .and_then(|(slot, plugin)| {
                self.engine
                    .as_ref()
                    .map(|eng| (slot, eng.eq_slots()[slot as usize].clone(), plugin))
            });
        let comp_new = self
            .engine
            .as_ref()
            .filter(|_| self.comp_plugin.is_none())
            .and_then(Self::load_comp_plugin)
            // 装载成功 ⇒ engine 必在；map 替代 unwrap，None 稳健传播零 panic
            .and_then(|(slot, plugin)| {
                self.engine
                    .as_ref()
                    .map(|eng| (slot, eng.compressor_slots()[slot as usize].clone(), plugin))
            });
        let skins_new = if self.skin_plugins.is_empty() {
            self.engine
                .as_ref()
                .map(Self::load_skin_plugins)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if let Some((slot, arc, plugin)) = eq_new {
            self.eq_slot = Some((slot, arc));
            self.eq_plugin = Some(plugin);
        }
        if let Some((slot, arc, plugin)) = comp_new {
            self.comp_slot = Some((slot, arc));
            self.comp_plugin = Some(plugin);
        }
        if !skins_new.is_empty() {
            self.skin_plugins = skins_new;
        }
    }

    /// 取专辑封面纹理（缓存命中优先；无封面记 None 不再重读）。
    fn album_cover_tex(
        &mut self,
        ctx: &egui::Context,
        path: &PathBuf,
    ) -> Option<egui::TextureHandle> {
        if let Some(c) = self.album_covers.get(path) {
            return c.clone();
        }
        // 元数据缓存优先（含封面字节），未缓存则现读并回填
        let cover = match self.metadata_cache.get(path) {
            Some(md) => md.cover.clone(),
            None => {
                let md = TrackMetadata::from_file(path);
                let c = md.cover.clone();
                self.metadata_cache.insert(path.clone(), md);
                c
            }
        };
        let tex = cover.and_then(|cv| {
            let img = tuneux_mediax::decode_cover(&cv.bytes, Some(cv.mime.as_str()))?;
            let ci = egui::ColorImage::from_rgba_unmultiplied(
                [img.width as usize, img.height as usize],
                &img.rgba,
            );
            Some(ctx.load_texture("album_cover", ci, egui::TextureOptions::LINEAR))
        });
        self.album_covers.insert(path.clone(), tex.clone());
        tex
    }

    /// 瞬时提示（命令反馈走状态栏 flash 通道）。
    fn flash_msg(&mut self, msg: String) {
        self.flash = Some(msg);
        self.flash_at = Some(Instant::now());
    }

    /// 在当前播放位置添加书签（菜单与 :bm 命令共用）。
    fn add_bookmark_now(&mut self) {
        let Some(path) = self.current_path.clone() else {
            let m = self.i18n.t("msg.bm_no_file").into_owned();
            self.flash_msg(m);
            return;
        };
        let cue_ms = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.as_ref())
            .map(|c| c.start_ms);
        let label = self
            .metadata
            .as_ref()
            .and_then(|m| m.title.clone())
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|x| x.to_str())
                    .unwrap_or("?")
                    .to_string()
            });
        let pos = self.engine.as_ref().map(|e| e.position()).unwrap_or(0.0);
        let added = self.config.bookmarks.add(&path, cue_ms, pos, &label);
        let key = if added {
            "msg.bm_added"
        } else {
            "msg.bm_updated"
        };
        let m = self.i18n.t(key).into_owned();
        self.flash_msg(m);
    }

    /// 音量微调（媒体键/命令共用；钳 0–1 并落配置）。
    fn nudge_volume(&mut self, d: f32) {
        if let Some(e) = &self.engine {
            let v = (e.volume() + d).clamp(0.0, 1.0);
            e.send(tuneux_corex::AudioCmd::SetVolume(v));
            self.config.volume = v;
        }
    }

    /// 布局预设名解析：名字精确匹配或 1 基序号（与列表展示同序）。
    fn resolve_preset(&self, key: &str) -> Option<String> {
        if let Some(nm) = self.config.dock_presets.keys().find(|k| *k == key) {
            return Some(nm.clone());
        }
        let idx = key.parse::<usize>().ok().filter(|v| *v >= 1)?;
        self.config.dock_presets.keys().nth(idx - 1).cloned()
    }

    /// 保存当前布局为预设（None = 自动命名「布局N」）。
    fn preset_save(&mut self, name: Option<String>) {
        let Some(tree) = self.config.dock.clone() else {
            return;
        };
        let name = name
            .unwrap_or_else(|| format!("\u{5e03}\u{5c40}{}", self.config.dock_presets.len() + 1));
        self.config.dock_presets.insert(name.clone(), tree);
        let m = self.i18n.t("msg.preset_saved").replace("{}", &name);
        self.flash_msg(m);
    }

    /// 载入布局预设。
    fn preset_load(&mut self, key: &str) {
        match self.resolve_preset(key) {
            Some(nm) => {
                if let Some(t) = self.config.dock_presets.get(&nm).cloned() {
                    self.config.dock = Some(t);
                    let m = self.i18n.t("msg.preset_loaded").replace("{}", &nm);
                    self.flash_msg(m);
                }
            }
            None => {
                let m = self.i18n.t("msg.preset_not_found").into_owned();
                self.flash_msg(m);
            }
        }
    }

    /// 删除布局预设。
    fn preset_del(&mut self, key: &str) {
        match self.resolve_preset(key) {
            Some(nm) => {
                self.config.dock_presets.remove(&nm);
                let m = self.i18n.t("msg.preset_deleted").replace("{}", &nm);
                self.flash_msg(m);
            }
            None => {
                let m = self.i18n.t("msg.preset_not_found").into_owned();
                self.flash_msg(m);
            }
        }
    }

    /// 执行一条命令（fx 同款命令集 + 布局预设扩展；未知命令 flash 提示）。
    /// 支持：vol 0-100 / repeat off|list|single / save-m3u / load-m3u [path] /
    /// bm / bm-list / bm-jump n / bm-del n / preset-save [名] /
    /// preset-load 名|序号 / preset-del 名|序号 / preset-list / help。
    fn execute_command(&mut self, raw: &str) {
        let line = raw.trim();
        if line.is_empty() {
            return;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_lowercase();
        match cmd.as_str() {
            "quit" | "q" | "exit" => {
                let m = self.i18n.t("msg.quit_hint").into_owned();
                self.flash_msg(m);
            }
            "volume" | "vol" => {
                // is_finite 校验："nan"/"inf" 能 parse 但 clamp 对 NaN 失效
                if let Some(v) = parts
                    .next()
                    .and_then(|x| x.parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                {
                    let v = v.clamp(0.0, 100.0) / 100.0;
                    if let Some(e) = &self.engine {
                        e.send(tuneux_corex::AudioCmd::SetVolume(v));
                    }
                    self.config.volume = v;
                    let m = self
                        .i18n
                        .t("msg.vol_set")
                        .replace("{}", &format!("{}", (v * 100.0).round() as u32));
                    self.flash_msg(m);
                } else {
                    let m = self.i18n.t("msg.usage_vol").into_owned();
                    self.flash_msg(m);
                }
            }
            "repeat" => match parts.next().map(|x| x.to_lowercase()) {
                Some(x) if x == "off" => {
                    self.config.repeat = RepeatMode::Off;
                    self.refresh_preload();
                    let m = self.i18n.t("msg.repeat_off").into_owned();
                    self.flash_msg(m);
                }
                Some(x) if x == "list" || x == "all" => {
                    self.config.repeat = RepeatMode::List;
                    self.refresh_preload();
                    let m = self.i18n.t("msg.repeat_list").into_owned();
                    self.flash_msg(m);
                }
                Some(x) if x == "single" || x == "one" => {
                    self.config.repeat = RepeatMode::Single;
                    self.refresh_preload();
                    let m = self.i18n.t("msg.repeat_single").into_owned();
                    self.flash_msg(m);
                }
                _ => {
                    let m = self.i18n.t("msg.usage_repeat").into_owned();
                    self.flash_msg(m);
                }
            },
            "save-m3u" | "m3u-save" => {
                if self.playlist.items().is_empty() {
                    let m = self.i18n.t("msg.m3u_empty").into_owned();
                    self.flash_msg(m);
                } else {
                    let path = self.browser.cwd().join("playlist.m3u");
                    let paths: Vec<PathBuf> = self
                        .playlist
                        .items()
                        .iter()
                        .map(|it| it.path.clone())
                        .collect();
                    let content = tuneux_mediax::m3u::serialize(&paths);
                    match std::fs::write(&path, content) {
                        Ok(_) => {
                            let m = format!(
                                "{} {}",
                                self.i18n
                                    .t("msg.saved_m3u")
                                    .replace("{}", &paths.len().to_string()),
                                path.display()
                            );
                            self.flash_msg(m);
                        }
                        Err(e) => {
                            let m = format!("{}\u{ff1a}{e}", self.i18n.t("msg.save_fail"));
                            self.flash_msg(m);
                        }
                    }
                }
            }
            "load-m3u" | "m3u-load" => {
                let path = match parts.next() {
                    Some(pp) => PathBuf::from(pp),
                    None => self.browser.cwd().join("playlist.m3u"),
                };
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        let paths = tuneux_mediax::m3u::parse(&content);
                        let cnt = paths.len();
                        for pp in paths {
                            self.enqueue_scan(pp);
                        }
                        let m = self.i18n.t("msg.loaded").replace("{}", &cnt.to_string());
                        self.flash_msg(m);
                    }
                    Err(e) => {
                        let m = format!("{}\u{ff1a}{e}", self.i18n.t("msg.load_fail"));
                        self.flash_msg(m);
                    }
                }
            }
            "bookmark" | "bm" => {
                self.add_bookmark_now();
            }
            "bookmarks" | "bm-list" => {
                if self.config.bookmarks.items.is_empty() {
                    let m = self.i18n.t("msg.bm_empty").into_owned();
                    self.flash_msg(m);
                } else {
                    let lines: Vec<String> = self
                        .config
                        .bookmarks
                        .items
                        .iter()
                        .enumerate()
                        .map(|(i, b)| {
                            format!("{}. {} [{}]", i + 1, b.label, fmt_time(b.position_secs))
                        })
                        .collect();
                    self.flash_msg(lines.join("\n"));
                }
            }
            "bookmark-jump" | "bm-jump" => {
                let Some(nm) = parts.next().and_then(|x| x.parse::<usize>().ok()) else {
                    let m = self.i18n.t("msg.usage_bm_jump").into_owned();
                    self.flash_msg(m);
                    return;
                };
                let bm = self.config.bookmarks.items.get(nm.wrapping_sub(1)).cloned();
                match bm {
                    None => {
                        let m = self.i18n.t("msg.bm_not_found").into_owned();
                        self.flash_msg(m);
                    }
                    Some(b) => {
                        let idx = match self
                            .playlist
                            .items()
                            .iter()
                            .position(|it| it.path == b.path)
                        {
                            Some(i) => i,
                            None => self.add_file_sync(&b.path).unwrap_or(0),
                        };
                        self.play_item(idx);
                        let seek = b.position_secs
                            + b.cue_start_ms.map(|m| m as f64 / 1000.0).unwrap_or(0.0);
                        if let Some(e) = &self.engine {
                            e.send(tuneux_corex::AudioCmd::Seek(seek));
                        }
                        let m = self.i18n.t("msg.bm_jump").replace("{}", &nm.to_string());
                        self.flash_msg(m);
                    }
                }
            }
            "bookmark-del" | "bm-del" => {
                let Some(nm) = parts.next().and_then(|x| x.parse::<usize>().ok()) else {
                    let m = self.i18n.t("msg.usage_bm_del").into_owned();
                    self.flash_msg(m);
                    return;
                };
                if nm >= 1 && nm <= self.config.bookmarks.items.len() {
                    self.config.bookmarks.items.remove(nm - 1);
                    let m = self.i18n.t("msg.bm_deleted").into_owned();
                    self.flash_msg(m);
                } else {
                    let m = self.i18n.t("msg.bm_not_found").into_owned();
                    self.flash_msg(m);
                }
            }
            "preset-save" | "layout-save" => {
                let name = parts.next().map(|x| x.to_string());
                self.preset_save(name);
            }
            "preset-load" | "layout-load" => match parts.next() {
                Some(key) => self.preset_load(key),
                None => {
                    let m = self.i18n.t("msg.preset_empty").into_owned();
                    self.flash_msg(m);
                }
            },
            "preset-del" | "layout-del" => match parts.next() {
                Some(key) => self.preset_del(key),
                None => {
                    let m = self.i18n.t("msg.preset_not_found").into_owned();
                    self.flash_msg(m);
                }
            },
            "preset" | "preset-list" | "layout" => {
                if self.config.dock_presets.is_empty() {
                    let m = self.i18n.t("msg.preset_empty").into_owned();
                    self.flash_msg(m);
                } else {
                    let lines: Vec<String> = self
                        .config
                        .dock_presets
                        .keys()
                        .enumerate()
                        .map(|(i, k)| format!("{}. {}", i + 1, k))
                        .collect();
                    self.flash_msg(lines.join("\n"));
                }
            }
            "modern" => {
                // 现代界面开关：:modern 切换 / :modern on|off 显式设定
                let want = match parts.next().map(|x| x.to_lowercase()) {
                    Some(x) if x == "on" || x == "1" || x == "true" => Some(true),
                    Some(x) if x == "off" || x == "0" || x == "false" => Some(false),
                    _ => None,
                };
                let new_v = want.unwrap_or(!self.config.modern);
                self.config.modern = new_v;
                let m = if new_v {
                    self.i18n.t("msg.modern_on").into_owned()
                } else {
                    self.i18n.t("msg.modern_off").into_owned()
                };
                self.flash_msg(m);
            }
            "help" | "about" => {
                self.ui.about_visible = true;
            }
            _ => {
                let m = self.i18n.t("msg.unknown_cmd").replace("{}", &cmd);
                self.flash_msg(m);
            }
        }
    }

    /// 均衡器面板：十段字符推子（fx TUI 风）+ 预设 + 开关。
    fn render_eq_in(&mut self, ui: &mut Ui) {
        // 插件缺失 = 功能缺失（fx 同口径）
        if self.eq_plugin.is_none() {
            let hint = self.i18n.t("msg.plugin_eq_missing").into_owned();
            let pal = self.palette.clone();
            let title = self.i18n.t("panel.eq").into_owned();
            Self::tui_box(ui, &pal, &title, false, |ui| {
                ui.add_space(20.0);
                ui.colored_label(pal.fg_weak, &hint);
            });
            return;
        }
        if self.eq_slot.is_none() {
            if let Some(e) = &self.engine {
                self.eq_slot = e.alloc_eq_slot();
            }
        }
        let Some((_, params)) = self.eq_slot.clone() else {
            return;
        };
        let (t_title, t_preset, t_on, t_reset) = (
            self.i18n.t("panel.eq").into_owned(),
            self.i18n.t("eq.preset").into_owned(),
            self.i18n.t("eq.on").into_owned(),
            self.i18n.t("eq.reset").into_owned(),
        );
        let pal = self.palette.clone();
        let bs = pal.bar_style;
        let f = mono(FONT);
        let small = mono(FONT - 3.0);

        Self::tui_box(ui, &pal, &t_title, false, |ui| {
            // ── 开关 ──
            let mut on = params.enabled();
            if ui.toggle_value(&mut on, &t_on).changed() {
                params.set_enabled(on);
            }

            // ── 十段字符推子 ──
            let n_bands = tuneux_corex::EQ_BANDS;
            let cw = Self::char_w(ui);
            let avail_w = ui.available_width() - 8.0;
            let col_w = (avail_w / n_bands as f32).max(cw * 2.0);
            let track_rows = 13usize; // +12..-12, 2 dB/行
            let avail_h = ui.available_height() - 50.0; // 预留频标+预设行
            let row_h = (avail_h / track_rows as f32).clamp(LINE_H * 0.6, LINE_H);
            let fader_h = row_h * track_rows as f32;

            let (frect, fresp) = ui.allocate_exact_size(
                Vec2::new(col_w * n_bands as f32 + cw * 3.0, fader_h + row_h * 2.0),
                egui::Sense::click_and_drag(),
            );
            let p = ui.painter_at(frect);
            let zero_row = track_rows / 2;
            let fader_left = frect.left() + cw * 2.5; // 给刻度留位

            // dB 刻度标尺（左侧）
            for db in [-12i32, -6, 0, 6, 12] {
                let row = (zero_row as i32 - db / 2).clamp(0, track_rows as i32 - 1) as usize;
                let y = frect.top() + row as f32 * row_h + row_h * 0.5;
                p.text(
                    Pos2::new(frect.left() + cw * 0.5, y),
                    Align2::LEFT_CENTER,
                    format!("{:+}", db),
                    small.clone(),
                    pal.fg_weak,
                );
            }

            // 悬停检测（哪一列被鼠标悬停）
            let hover_band = ui.input(|i| i.pointer.hover_pos()).and_then(|hp| {
                if frect.contains(hp) {
                    let b = ((hp.x - fader_left) / col_w).floor() as usize;
                    (b < n_bands).then_some(b)
                } else {
                    None
                }
            });

            for bi in 0..n_bands {
                let cx = fader_left + (bi as f32 + 0.5) * col_w;
                let gain = params.band(bi);

                // 悬停列：整列背景条（醒目高亮）
                let hovered = hover_band == Some(bi);
                if hovered {
                    let bg_rect = egui::Rect::from_min_max(
                        Pos2::new(cx - col_w * 0.45, frect.top()),
                        Pos2::new(cx + col_w * 0.45, frect.bottom() - row_h),
                    );
                    p.rect_filled(bg_rect, 0.0, pal.sel_bg);
                }
                // 轨道（·）：悬停列用更亮的色
                let track_color = if hovered { pal.fg } else { pal.grid };
                for row in 0..track_rows {
                    let y = frect.top() + row as f32 * row_h + row_h * 0.5;
                    p.text(
                        Pos2::new(cx, y),
                        Align2::CENTER_CENTER,
                        "\u{00b7}",
                        f.clone(),
                        track_color,
                    );
                }

                // 零线（─，贯穿）
                let zy = frect.top() + zero_row as f32 * row_h + row_h * 0.5;
                p.line_segment(
                    [
                        Pos2::new(cx - col_w * 0.35, zy),
                        Pos2::new(cx + col_w * 0.35, zy),
                    ],
                    egui::Stroke::new(1.0_f32, pal.fg_weak),
                );

                // 填充（█ 提升 / ▓ 衰减）
                let gain_rows = ((gain / 2.0).round() as i32)
                    .clamp(-(track_rows as i32 - 1), track_rows as i32 - 1);
                let fill_ch = if bs == tuneux_mediax::BarStyle::Hanzi {
                    bs.pool().first().copied().unwrap_or("\u{2588}")
                } else {
                    "\u{2588}"
                };
                let cut_ch = "\u{2593}";
                let boost_color = pal.spec_bar;
                let cut_color = pal.resample_fg;

                if gain_rows > 0 {
                    for step in 1..=gain_rows {
                        let row = zero_row - step as usize;
                        let y = frect.top() + row as f32 * row_h + row_h * 0.5;
                        p.text(
                            Pos2::new(cx, y),
                            Align2::CENTER_CENTER,
                            fill_ch,
                            f.clone(),
                            boost_color,
                        );
                    }
                } else if gain_rows < 0 {
                    for step in 1..=(-gain_rows) as usize {
                        let row = zero_row + step;
                        if row < track_rows {
                            let y = frect.top() + row as f32 * row_h + row_h * 0.5;
                            p.text(
                                Pos2::new(cx, y),
                                Align2::CENTER_CENTER,
                                cut_ch,
                                f.clone(),
                                cut_color,
                            );
                        }
                    }
                }

                // 推子帽（━，强调色）
                let cap_row =
                    (zero_row as i32 - gain_rows).clamp(0, track_rows as i32 - 1) as usize;
                let cap_y = frect.top() + cap_row as f32 * row_h + row_h * 0.5;
                let cap_half = if hovered { col_w * 0.42 } else { col_w * 0.3 };
                p.line_segment(
                    [
                        Pos2::new(cx - cap_half, cap_y),
                        Pos2::new(cx + cap_half, cap_y),
                    ],
                    egui::Stroke::new(if hovered { 3.5_f32 } else { 2.5_f32 }, pal.accent),
                );

                // dB 值（非零时显示在帽上方）
                if gain.abs() >= 0.5 {
                    let vy = cap_y - row_h * 0.8;
                    p.text(
                        Pos2::new(cx, vy),
                        Align2::CENTER_CENTER,
                        format!("{:+.0}", gain),
                        small.clone(),
                        pal.fg_weak,
                    );
                }

                // 频率标（底部）
                let freq = tuneux_corex::EQ_FREQS[bi];
                let label = if freq >= 1000.0 {
                    format!("{:.0}k", freq / 1000.0)
                } else {
                    format!("{:.0}", freq)
                };
                let fy = frect.bottom() - row_h * 0.5;
                p.text(
                    Pos2::new(cx, fy),
                    Align2::CENTER_CENTER,
                    &label,
                    small.clone(),
                    if hovered { pal.accent } else { pal.fg_weak },
                );
            }

            // ── 交互：拖/点调增益，双击归零 ──
            if fresp.dragged() || fresp.clicked() {
                if let Some(pt) = fresp.interact_pointer_pos() {
                    let band = ((pt.x - fader_left) / col_w)
                        .floor()
                        .clamp(0.0, (n_bands - 1) as f32) as usize;
                    let rel = 1.0 - (pt.y - frect.top()) / fader_h;
                    let gain = (rel * 24.0 - 12.0).clamp(-12.0, 12.0);
                    params.set_band(band, (gain / 2.0).round() * 2.0);
                }
            }
            // 滚轮微调（悬停列 ±1 dB；Shift 加速 ±3 dB）
            if let Some(bi) = hover_band {
                let scroll: f32 = ui.input(|i| {
                    i.events
                        .iter()
                        .filter_map(|e| match e {
                            egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                            _ => None,
                        })
                        .sum()
                });
                if scroll.abs() > 0.5 {
                    let step = if ui.input(|i| i.modifiers.shift) {
                        3.0
                    } else {
                        1.0
                    };
                    let dir = if scroll > 0.0 { step } else { -step };
                    let cur = params.band(bi);
                    params.set_band(bi, (cur + dir).clamp(-12.0, 12.0));
                }
            }
            if fresp.double_clicked() {
                if let Some(pt) = fresp.interact_pointer_pos() {
                    let band = ((pt.x - fader_left) / col_w)
                        .floor()
                        .clamp(0.0, (n_bands - 1) as f32) as usize;
                    params.set_band(band, 0.0);
                }
            }

            // ── 预设行 ──
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(&t_preset).monospace());
                for (name, vals) in EQ_PRESETS {
                    if ui.button(name).clicked() {
                        for (i, v) in vals.iter().enumerate() {
                            params.set_band(i, *v);
                        }
                    }
                }
                if ui.button(&t_reset).clicked() {
                    params.reset();
                }
            });
        });
    }

    /// 压缩器面板：五参数字符推子（水平条 + ● 滑块头）。
    fn render_comp_in(&mut self, ui: &mut Ui) {
        // 插件缺失 = 功能缺失
        if self.comp_plugin.is_none() {
            let hint = self.i18n.t("msg.plugin_comp_missing").into_owned();
            let pal = self.palette.clone();
            let title = self.i18n.t("panel.compressor").into_owned();
            Self::tui_box(ui, &pal, &title, false, |ui| {
                ui.add_space(20.0);
                ui.colored_label(pal.fg_weak, &hint);
            });
            return;
        }
        if self.comp_slot.is_none() {
            if let Some(e) = &self.engine {
                self.comp_slot = e.alloc_compressor_slot();
            }
        }
        let Some((_, params)) = self.comp_slot.clone() else {
            return;
        };
        let t_title = self.i18n.t("panel.compressor").into_owned();
        let (l_thresh, l_ratio, l_attack, l_release, l_makeup) = (
            format!("{} dB", self.i18n.t("msg.comp_threshold")),
            format!("{} :1", self.i18n.t("msg.comp_ratio")),
            format!("{} ms", self.i18n.t("msg.comp_attack")),
            format!("{} ms", self.i18n.t("msg.comp_release")),
            format!("{} dB", self.i18n.t("msg.comp_makeup")),
        );
        let pal = self.palette.clone();
        let f = mono(FONT);

        Self::tui_box(ui, &pal, &t_title, false, |ui| {
            let cw = Self::char_w(ui);
            let label_w = 90.0;
            let value_w = 80.0;
            let track_w = (ui.available_width() - label_w - value_w - 24.0).max(60.0);
            let row_h = LINE_H * 1.6;
            let track_cols = ((track_w / cw) as usize).clamp(8, 80);

            // 五参数数据（标签, min, max, 默认值, 当前值, 单位后缀）
            let rows: Vec<(&str, f32, f32, f32, f32, &str)> = vec![
                (&l_thresh, -60.0, 0.0, -20.0, params.threshold(), "dB"),
                (&l_ratio, 1.0, 20.0, 4.0, params.ratio(), ":1"),
                (&l_attack, 1.0, 200.0, 25.0, params.attack_ms(), "ms"),
                (&l_release, 10.0, 1000.0, 250.0, params.release_ms(), "ms"),
                (&l_makeup, 0.0, 24.0, 0.0, params.makeup(), "dB"),
            ];

            let mut set_param: Option<(usize, f32)> = None;

            for (ri, (label, min, max, dflt, val, unit)) in rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_h),
                    egui::Sense::click_and_drag(),
                );
                let p = ui.painter_at(rect);
                let cy = rect.center().y;
                let x_label = rect.left();
                let x_track = rect.left() + label_w;
                let x_value = rect.right() - 4.0;

                // 标签
                p.text(
                    Pos2::new(x_label, cy),
                    Align2::LEFT_CENTER,
                    label,
                    f.clone(),
                    pal.fg,
                );

                // 轨道（· 字符）
                let progress = ((val - min) / (max - min)).clamp(0.0, 1.0);
                let thumb_col = (progress * track_cols as f32).round() as usize;
                for c in 0..track_cols {
                    let x = x_track + c as f32 * cw + cw * 0.5;
                    let ch = if c == thumb_col {
                        "\u{25cf}"
                    } else if (c as f32) < progress * track_cols as f32 {
                        "\u{2588}"
                    } else {
                        "\u{00b7}"
                    };
                    let color = if c == thumb_col {
                        pal.accent
                    } else if (c as f32) < progress * track_cols as f32 {
                        pal.spec_bar
                    } else {
                        pal.grid
                    };
                    p.text(
                        Pos2::new(x, cy),
                        Align2::CENTER_CENTER,
                        ch,
                        f.clone(),
                        color,
                    );
                }

                // 默认位置标记（│ 竖线）
                let dflt_progress = ((dflt - min) / (max - min)).clamp(0.0, 1.0);
                let dflt_x = x_track + dflt_progress * track_w;
                p.line_segment(
                    [
                        Pos2::new(dflt_x, cy - row_h * 0.3),
                        Pos2::new(dflt_x, cy + row_h * 0.3),
                    ],
                    egui::Stroke::new(1.0_f32, pal.grid),
                );

                // 值：偏离默认时变色（近默认绿 / 偏离黄 / 大幅偏离红）
                let dev = ((val - dflt) / (max - min)).abs();
                let val_color = if dev < 0.05 {
                    pal.level_low
                } else if dev < 0.25 {
                    pal.fg
                } else {
                    pal.level_mid
                };
                let val_text = if ri == 1 {
                    format!("{:.1} {}", val, unit) // 压缩比 1 位小数
                } else {
                    format!("{:.0} {}", val, unit)
                };
                p.text(
                    Pos2::new(x_value, cy),
                    Align2::RIGHT_CENTER,
                    &val_text,
                    f.clone(),
                    val_color,
                );

                // 交互：拖/点调值，双击回默认
                if resp.dragged() || resp.clicked() {
                    if let Some(pt) = resp.interact_pointer_pos() {
                        let rel = ((pt.x - x_track) / track_w).clamp(0.0, 1.0);
                        let new_val = min + rel * (max - min);
                        set_param = Some((ri, new_val));
                    }
                }
                if resp.double_clicked() {
                    set_param = Some((ri, *dflt));
                }
                // 滚轮微调（Shift 加速）
                if resp.hovered() {
                    let scroll: f32 = ui.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                                _ => None,
                            })
                            .sum()
                    });
                    if scroll.abs() > 0.5 {
                        let range = max - min;
                        let step = if ui.input(|i| i.modifiers.shift) {
                            range * 0.05
                        } else {
                            range * 0.02
                        };
                        let dir = if scroll > 0.0 { step } else { -step };
                        set_param = Some((ri, (val + dir).clamp(*min, *max)));
                    }
                }
            }

            if let Some((ri, v)) = set_param {
                match ri {
                    0 => {
                        params.set_threshold(v);
                    }
                    1 => {
                        params.set_ratio(v);
                    }
                    2 => {
                        params.set_attack_ms(v);
                    }
                    3 => {
                        params.set_release_ms(v);
                    }
                    4 => {
                        params.set_makeup(v);
                    }
                    _ => {}
                }
            }
        });
    }

    /// 滤波器面板：截止 + 谐振两个字符旋钦（弧线盘面 + 指针）。
    fn render_filter_in(&mut self, ui: &mut Ui) {
        if self.filter_slot.is_none() {
            if let Some(e) = &self.engine {
                self.filter_slot = e.alloc_filter_slot();
            }
        }
        let Some((_, params)) = self.filter_slot.clone() else {
            return;
        };
        let t_title = self.i18n.t("panel.filter").into_owned();
        let l_cutoff = self.i18n.t("filter.cutoff").into_owned();
        let l_reson = self.i18n.t("filter.resonance").into_owned();
        let l_off = self.i18n.t("filter.off").into_owned();
        let pal = self.palette.clone();
        let small = mono(FONT - 3.0);

        Self::tui_box(ui, &pal, &t_title, false, |ui| {
            // 开关
            let mut on = params.enabled();
            let label = if on { t_title.clone() } else { l_off.clone() };
            if ui.toggle_value(&mut on, label).changed() {
                params.set_enabled(on);
            }

            // 两个旋钦（截止频率（对数）+ 谐振 Q（线性））
            let avail_w = ui.available_width();
            let knob_r = (avail_w * 0.18).clamp(24.0, 50.0);
            let knob_h = knob_r * 2.0 + 40.0;
            let gap = avail_w * 0.15;
            let total_w = knob_r * 4.0 + gap;
            let start_x = (avail_w - total_w) * 0.5;

            let (area_rect, _area_resp) =
                ui.allocate_exact_size(Vec2::new(avail_w, knob_h), egui::Sense::hover());
            let p = ui.painter_at(area_rect);
            let cy = area_rect.top() + knob_r + 8.0;

            // 参数定义（标签，最小/最大/默认/当前值，是否对数）
            let knobs: Vec<(&str, f32, f32, f32, f32, bool, usize)> = vec![
                (
                    &l_cutoff,
                    20.0,
                    20000.0,
                    20000.0,
                    params.cutoff_hz(),
                    true,
                    0,
                ),
                (&l_reson, 0.1, 20.0, 0.707, params.resonance_q(), false, 1),
            ];

            let mut set_param: Option<(usize, f32)> = None;

            for (ki, (label, min, max, dflt, val, is_log, param_idx)) in knobs.iter().enumerate() {
                let cx = area_rect.left() + start_x + ki as f32 * (knob_r * 2.0 + gap) + knob_r;
                // 值归一化到 0-1（对数参数用 log2 比例）
                let norm = if *is_log {
                    ((val.log2() - min.log2()) / (max.log2() - min.log2())).clamp(0.0, 1.0)
                } else {
                    ((val - min) / (max - min)).clamp(0.0, 1.0)
                };

                // 旋钦角度：-225° 到 +45°（总扫描 270°）
                let start_angle = -225.0_f32.to_radians();
                let sweep = 270.0_f32.to_radians();
                let value_angle = start_angle + norm * sweep;

                // 画弧线轨道（背景弧）
                // ═══ 3D 立体旋钮（光源左上 → 右下阴影） ═══

                // 1) 投影（右下偏移的暗圆——深度感）
                let shadow_off = 3.0;
                p.circle_filled(
                    egui::Pos2::new(cx + shadow_off, cy + shadow_off),
                    knob_r + 2.0,
                    pal.bg, // 最暗底色
                );

                // 2) 旋钮体（深色圆盘）
                p.circle_filled(egui::Pos2::new(cx, cy), knob_r, pal.panel_bg);

                // 3) 外圈斜面——上半亮 / 下半暗（金属边框立体感）
                let bezel_steps = 40;
                let mut prev = egui::Pos2::new(cx + knob_r, cy);
                for si in 1..=bezel_steps {
                    let t = std::f32::consts::TAU * si as f32 / bezel_steps as f32;
                    let pt = egui::Pos2::new(cx + t.cos() * knob_r, cy + t.sin() * knob_r);
                    // 上半（0 到 π）亮——光源方向；下半暗
                    let is_top = t.cos() + t.sin() < 0.0; // 左上象限更亮
                    let stroke_col = if is_top { pal.border } else { pal.grid };
                    p.line_segment([prev, pt], egui::Stroke::new(2.0_f32, stroke_col));
                    prev = pt;
                }

                // 4) 内高光环（左上弧——模拟光源反射）
                let highlight_r = knob_r - 4.0;
                let hl_start = std::f32::consts::PI * 0.75; // 左上 135°
                let hl_sweep = std::f32::consts::PI * 0.5; // 90° 弧
                let mut prev = egui::Pos2::new(
                    cx + hl_start.cos() * highlight_r,
                    cy + hl_start.sin() * highlight_r,
                );
                for si in 1..=16 {
                    let t = hl_start + hl_sweep * si as f32 / 16.0;
                    let pt =
                        egui::Pos2::new(cx + t.cos() * highlight_r, cy + t.sin() * highlight_r);
                    p.line_segment([prev, pt], egui::Stroke::new(1.5_f32, pal.fg_weak));
                    prev = pt;
                }

                // 5) 内阴影环（右下弧——背光面）
                let shadow_r = knob_r - 4.0;
                let sh_start = std::f32::consts::PI * 1.75; // 右下 315°
                let sh_sweep = std::f32::consts::PI * 0.5;
                let mut prev = egui::Pos2::new(
                    cx + sh_start.cos() * shadow_r,
                    cy + sh_start.sin() * shadow_r,
                );
                for si in 1..=16 {
                    let t = sh_start + sh_sweep * si as f32 / 16.0;
                    let pt = egui::Pos2::new(cx + t.cos() * shadow_r, cy + t.sin() * shadow_r);
                    p.line_segment([prev, pt], egui::Stroke::new(1.5_f32, pal.bg));
                    prev = pt;
                }

                // 6) 值弧（已调部分——谱色，画在斜面内侧）
                let arc_r = knob_r - 8.0;

                // 背景弧（全程——暗灰参考线）
                {
                    let steps = 24;
                    let mut prev = egui::Pos2::new(
                        cx + start_angle.cos() * arc_r,
                        cy + start_angle.sin() * arc_r,
                    );
                    for si in 1..=steps {
                        let t = start_angle + sweep * si as f32 / steps as f32;
                        let pt = egui::Pos2::new(cx + t.cos() * arc_r, cy + t.sin() * arc_r);
                        p.line_segment([prev, pt], egui::Stroke::new(2.0_f32, pal.grid));
                        prev = pt;
                    }
                }

                // 已调弧（谱色）
                if norm > 0.01 {
                    let steps = 24;
                    let mut prev = egui::Pos2::new(
                        cx + start_angle.cos() * arc_r,
                        cy + start_angle.sin() * arc_r,
                    );
                    for si in 1..=steps {
                        let t =
                            start_angle + (value_angle - start_angle) * si as f32 / steps as f32;
                        let pt = egui::Pos2::new(cx + t.cos() * arc_r, cy + t.sin() * arc_r);
                        p.line_segment([prev, pt], egui::Stroke::new(3.0_f32, pal.spec_bar));
                        prev = pt;
                    }
                }

                // 指针线（从中心到弧边缘）
                let needle_len = arc_r * 0.85;
                let nx = cx + value_angle.cos() * needle_len;
                let ny = cy + value_angle.sin() * needle_len;
                // 指针阴影（右下偏移 1px）
                p.line_segment(
                    [
                        egui::Pos2::new(cx + 1.0, cy + 1.0),
                        egui::Pos2::new(nx + 1.0, ny + 1.0),
                    ],
                    egui::Stroke::new(1.5_f32, pal.bg),
                );
                // 指针本体
                p.line_segment(
                    [egui::Pos2::new(cx, cy), egui::Pos2::new(nx, ny)],
                    egui::Stroke::new(2.5_f32, pal.accent),
                );

                // 中心轴帽（外暗内亮——铆钉立体感）
                p.circle_filled(egui::Pos2::new(cx, cy), 5.0, pal.grid);
                p.circle_filled(egui::Pos2::new(cx - 1.0, cy - 1.0), 4.0, pal.accent);

                // 标签（旋钦下方）
                p.text(
                    egui::Pos2::new(cx, cy + knob_r + 14.0),
                    Align2::CENTER_CENTER,
                    label,
                    small.clone(),
                    pal.fg,
                );

                // 值（标签下方）
                let val_text = if *param_idx == 0 {
                    if val >= &1000.0 {
                        format!("{:.1} kHz", val / 1000.0)
                    } else {
                        format!("{:.0} Hz", val)
                    }
                } else {
                    format!("Q {:.2}", val)
                };
                p.text(
                    egui::Pos2::new(cx, cy + knob_r + 28.0),
                    Align2::CENTER_CENTER,
                    &val_text,
                    small.clone(),
                    pal.fg_weak,
                );

                // 交互区（旋钦周围圆形区域）
                let knob_rect = egui::Rect::from_center_size(
                    egui::Pos2::new(cx, cy),
                    Vec2::new(knob_r * 2.4, knob_r * 2.4),
                );
                let resp = ui.allocate_rect(knob_rect, egui::Sense::click_and_drag());

                // 拖拽：垂直方向调值（往上增加）
                if resp.dragged() {
                    // 用拖拽起始点与当前点的 Y 差调值
                    let dy = resp.drag_delta().y;
                    let sensitivity = if *is_log { 0.005 } else { 0.003 };
                    let new_norm = (norm - dy * sensitivity).clamp(0.0, 1.0);
                    let new_val = if *is_log {
                        2.0f32.powf(min.log2() + new_norm * (max.log2() - min.log2()))
                    } else {
                        min + new_norm * (max - min)
                    };
                    set_param = Some((*param_idx, new_val));
                }
                // 双击回默认
                if resp.double_clicked() {
                    set_param = Some((*param_idx, *dflt));
                }
                // 滚轮微调
                if resp.hovered() {
                    let scroll: f32 = ui.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                                _ => None,
                            })
                            .sum()
                    });
                    if scroll.abs() > 0.5 {
                        let step = if ui.input(|i| i.modifiers.shift) {
                            0.05
                        } else {
                            0.02
                        };
                        let dir = if scroll > 0.0 { step } else { -step };
                        let new_norm = (norm + dir).clamp(0.0, 1.0);
                        let new_val = if *is_log {
                            2.0f32.powf(min.log2() + new_norm * (max.log2() - min.log2()))
                        } else {
                            min + new_norm * (max - min)
                        };
                        set_param = Some((*param_idx, new_val));
                    }
                }
            }

            if let Some((idx, v)) = set_param {
                match idx {
                    0 => {
                        params.set_cutoff_hz(v);
                    }
                    1 => {
                        params.set_resonance_q(v);
                    }
                    _ => {}
                }
            }
        });
    }

    /// 波形 Seekbar 模块：全曲包络 + 播放位置 + 点击跳转。
    fn render_waveform_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.waveform").into_owned();
        let (pos, dur) = match &self.status {
            Some(s) => (s.position, s.duration),
            None => (0.0, 0.0),
        };

        Self::tui_box(ui, &pal, &title, false, |ui| {
            let avail = ui.available_size();
            let (rect, resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
            let p = ui.painter_at(rect);
            let mid = rect.center().y;
            let progress = if dur > 0.0 {
                (pos / dur).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let play_x = rect.left() + progress as f32 * rect.width();

            // 无包络时画均匀条
            let env: Vec<f32> = match &self.track_envelope {
                Some((_, e)) => e.clone(),
                None => vec![0.5; 256],
            };
            let n = env.len();
            let bar_w = rect.width() / n as f32;

            for (i, peak) in env.iter().enumerate() {
                let x = rect.left() + i as f32 * bar_w;
                // 增益 1.5x：安静段也可见（原始峰值偏低时波形扁平不可辨）
                let boosted = (peak * 1.5).clamp(0.03, 1.0);
                let h = (boosted * rect.height() * 0.45).max(1.5);
                let played = x < play_x;
                let color = if played { pal.spec_bar } else { pal.grid };
                p.rect_filled(
                    egui::Rect::from_min_max(
                        Pos2::new(x, mid - h),
                        Pos2::new(x + bar_w.max(1.0) - 0.5, mid + h),
                    ),
                    0.0,
                    color,
                );
            }

            // 播放位置竖线
            p.line_segment(
                [
                    Pos2::new(play_x, rect.top() + 4.0),
                    Pos2::new(play_x, rect.bottom() - 4.0),
                ],
                egui::Stroke::new(2.0_f32, pal.accent),
            );

            // 时间标注
            let small = mono(FONT - 4.0);
            p.text(
                Pos2::new(rect.left() + 2.0, rect.bottom() - 8.0),
                Align2::LEFT_BOTTOM,
                fmt_time(pos),
                small.clone(),
                pal.fg_weak,
            );
            if dur > 0.0 {
                p.text(
                    Pos2::new(rect.right() - 2.0, rect.bottom() - 8.0),
                    Align2::RIGHT_BOTTOM,
                    fmt_time(dur),
                    small.clone(),
                    pal.fg_weak,
                );
            }

            // 交互：点击/拖拽跳转
            if dur > 0.0 {
                if let Some(pt) = resp.interact_pointer_pos() {
                    let rel = ((pt.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    let target = rel as f64 * dur;
                    if resp.dragged() {
                        let px = rect.left() + rel * rect.width();
                        p.line_segment(
                            [
                                Pos2::new(px, rect.top() + 2.0),
                                Pos2::new(px, rect.bottom() - 2.0),
                            ],
                            egui::Stroke::new(1.5_f32, pal.peak_fg),
                        );
                        p.text(
                            Pos2::new(px, rect.top() + 4.0),
                            Align2::CENTER_TOP,
                            fmt_time(target),
                            small.clone(),
                            pal.peak_fg,
                        );
                    } else if resp.drag_stopped() || resp.clicked() {
                        if let Some(e) = &self.engine {
                            e.send(tuneux_corex::AudioCmd::Seek(target));
                        }
                    }
                }
            }
        });
    }

    /// 可视化面板：插件 tick 每帧产出字符画。
    fn render_visualizer_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.visualizer").into_owned();

        Self::tui_box(ui, &pal, &title, false, |ui| {
            if self.visual_plugins.is_empty() {
                let hint = self.i18n.t("msg.no_visual").into_owned();
                ui.add_space(20.0);
                ui.colored_label(pal.fg_weak, hint);
                return;
            }
            // 逐行渲染插件产出的字符画
            let text = self.visual_text.clone();
            egui::ScrollArea::both().show(ui, |ui| {
                for line in text.lines() {
                    ui.label(egui::RichText::new(line).monospace().color(pal.spec_bar));
                }
            });
        });
    }

    /// DSP 链面板：EQ → 压缩器 → 滤波器 链路总览。
    fn render_dsp_chain_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.dspchain").into_owned();
        let (l_eq, l_comp, l_filt) = (
            self.i18n.t("dsp.eq").into_owned(),
            self.i18n.t("dsp.comp").into_owned(),
            self.i18n.t("dsp.filter").into_owned(),
        );
        let (l_on, l_off) = (
            self.i18n.t("dsp.active").into_owned(),
            self.i18n.t("dsp.bypassed").into_owned(),
        );

        Self::tui_box(ui, &pal, &title, false, |ui| {
            // 链路图
            let chain = format!(
                "[{}] {} \u{2192} [{}] {} \u{2192} [{}] {} \u{2192} OUT",
                if self.eq_plugin.is_some() { "*" } else { " " },
                l_eq,
                if self.comp_plugin.is_some() { "*" } else { " " },
                l_comp,
                if self.filter_slot.as_ref().is_some_and(|(_, p)| p.enabled()) {
                    "*"
                } else {
                    " "
                },
                l_filt,
            );
            ui.label(egui::RichText::new(&chain).monospace().color(pal.fg));
            ui.add_space(8.0);

            // 各效果器状态
            let eq_status = if let Some((_, params)) = &self.eq_slot {
                if params.enabled() {
                    &l_on
                } else {
                    &l_off
                }
            } else {
                &l_off
            };
            let comp_status = if let Some((_, params)) = &self.comp_slot {
                if params.enabled() {
                    &l_on
                } else {
                    &l_off
                }
            } else {
                &l_off
            };
            let filt_status = if let Some((_, params)) = &self.filter_slot {
                if params.enabled() {
                    &l_on
                } else {
                    &l_off
                }
            } else {
                &l_off
            };

            ui.label(
                egui::RichText::new(format!("{}: {}", l_eq, eq_status))
                    .monospace()
                    .color(pal.fg),
            );
            ui.label(
                egui::RichText::new(format!("{}: {}", l_comp, comp_status))
                    .monospace()
                    .color(pal.fg),
            );
            ui.label(
                egui::RichText::new(format!("{}: {}", l_filt, filt_status))
                    .monospace()
                    .color(pal.fg),
            );
        });
    }

    /// 同步加入单个路径（文件/.cue——标签读取毫秒级）。
    /// 返回最后加入项索引（供「加入并播放」）。
    fn add_file_sync(&mut self, p: &std::path::Path) -> Option<usize> {
        let items = collect_items(p);
        let added = items.len();
        if added == 0 {
            return None;
        }
        self.playlist.add_many(items);
        self.set_flash_added(added);
        Some(self.playlist.items().len() - 1)
    }

    /// 路径入后台扫描队列（递归 + 读标签；再大的目录也不阻塞 UI）。
    /// 在途计数统一由 walker 管理（每弹出项 +1 / 处理完 -1）——
    /// UI 侧若再加一笔会永久泄漏 +1 = 「扫描中」卡死不灭（实测教训）。
    fn enqueue_scan(&mut self, p: PathBuf) {
        self.scan_pool.submit(p);
    }

    /// 「已加入 N 首」瞬时提示。
    fn set_flash_added(&mut self, count: usize) {
        self.flash = Some(
            self.i18n
                .t("msg.added")
                .into_owned()
                .replace("{}", &count.to_string()),
        );
        self.flash_at = Some(Instant::now());
    }

    // ==================== TUI 绘制原语 ====================

    /// 边框盒：TUI 皮肤 = ratatui Block 风（标题嵌上边线，底色断线）；
    /// GUI 皮肤（tui_chrome=false，f2k 系）= 完整边框 + 标题头部条。
    /// focused = 强调色边框（fx 的焦点面板高亮）。
    fn tui_box<R>(
        ui: &mut Ui,
        pal: &Palette,
        title: &str,
        focused: bool,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> R {
        let rect = ui.available_rect_before_wrap();
        Self::tui_box_at(ui, pal, title, rect, focused, add)
    }

    /// ratatui Block 同款盒：底色/边框/标题画在给定矩形整块上，高宽
    /// 完全由 rect 决定（与内容行数解耦）；内容经 scope_builder
    /// 塞入内矩形（越界自动裁剪，不会画出边框）。
    /// 画笔解除裁剪：子 ui 的 clip 等于自身矩形，骑线描边会被吃掉
    /// （= 边框消失），必须显式放宽到无限再画。
    fn tui_box_at<R>(
        ui: &mut Ui,
        pal: &Palette,
        title: &str,
        rect: Rect,
        focused: bool,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> R {
        let bc = if focused { pal.accent } else { pal.border };
        let p = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), Rect::EVERYTHING);
        p.rect_filled(rect, egui::CornerRadius::same(0), pal.bg);
        p.rect_stroke(
            rect,
            egui::CornerRadius::same(0),
            Stroke::new(1.0_f32, bc),
            egui::StrokeKind::Middle,
        );
        if pal.tui_chrome {
            Self::paint_box_title(&p, pal, rect, title);
        } else {
            Self::paint_gui_header(&p, pal, rect, title, focused);
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(Self::box_inner(rect)), add)
            .inner
    }

    /// 盒内矩形：顶部 24px（标题嵌在顶边线、下半延伸约 10px，滚动时
    /// 行内容不得贴标题——用户规格）+ 左右 9 / 底 10。
    fn box_inner(rect: Rect) -> Rect {
        Rect::from_min_max(
            Pos2::new(rect.left() + 9.0, rect.top() + 24.0),
            Pos2::new(rect.right() - 9.0, rect.bottom() - 10.0),
        )
    }

    /// 标题嵌在盒顶边线上（底色打断边线，ratatui Block title 同款）。
    fn paint_box_title(p: &egui::Painter, pal: &Palette, rect: Rect, title: &str) {
        if title.is_empty() {
            return;
        }
        let text = format!(" {} ", title);
        let galley = p.layout_no_wrap(text, mono(FONT), pal.fg);
        let sz = galley.size();
        let center = Pos2::new(rect.left() + 12.0 + sz.x / 2.0, rect.top());
        let bg_rect = Rect::from_center_size(center, sz + Vec2::new(6.0, 4.0));
        p.rect_filled(bg_rect, 0.0, pal.bg);
        p.galley(bg_rect.left_center() + Vec2::new(3.0, 0.0), galley, pal.fg);
    }

    /// GUI 标题（f2k 系）：透明背景——不铺任何底色补块，标题文字直接
    /// 置于面板底色上（边框完整走通；内容自 24px 起，不与文字重叠）。
    /// 焦点时标题提亮为 fg，否则 fg_weak。
    fn paint_gui_header(p: &egui::Painter, pal: &Palette, rect: Rect, title: &str, focused: bool) {
        if title.is_empty() {
            return;
        }
        let col = if focused { pal.fg } else { pal.fg_weak };
        let galley = p.layout_no_wrap(format!(" {title} "), mono(FONT), col);
        let sz = galley.size();
        // 垂直居中于顶部 24px 标题区（与 box_inner 顶留白同源），左侧 10px
        let y = rect.top() + (24.0 - sz.y) / 2.0;
        p.galley(Pos2::new(rect.left() + 10.0, y), galley, col);
    }

    /// max-pool 频段到显示列（fx pool_to_columns 同款；字符/GUI 两路共用）。
    fn max_pool_cols(data: &[f32], total_cols: usize) -> Vec<f32> {
        let n_bands = tuneux_corex::spectrum::N_BANDS;
        if total_cols >= n_bands {
            let mut v = data.to_vec();
            v.resize(total_cols, 0.0);
            v
        } else {
            let per = n_bands as f32 / total_cols as f32;
            (0..total_cols)
                .map(|c| {
                    let st = ((c as f32 * per) as usize).min(n_bands);
                    let en = (((c + 1) as f32 * per) as usize).min(n_bands).max(st + 1);
                    data[st..en].iter().copied().fold(0.0f32, f32::max)
                })
                .collect()
        }
    }

    /// GUI 频谱（f2k 经典风）：实心条 + 缓落峰帽 + dB 网格线 + 频率轴，
    /// 纯矩形/文本绘制，无字符网格；条色按高度走 level 三段阈值色
    /// （f2k 经典绿→黄→红观感，调色板驱动；峰帽缓落由 corex
    /// SpectrumPeakHold 驱动，与字符版同一数据链）。
    fn draw_spectrum_gui(
        ui: &Ui,
        pal: &Palette,
        rect: Rect,
        bands: &[f32; tuneux_corex::spectrum::N_BANDS],
        peaks: &[f32; tuneux_corex::spectrum::N_BANDS],
        axis: bool,
    ) {
        let p = ui.painter_at(rect);
        let axis_h = if axis { 14.0 } else { 0.0 };
        let axis_w = if axis { 28.0 } else { 0.0 };
        let plot = Rect::from_min_max(
            Pos2::new(rect.left() + 2.0, rect.top() + 2.0),
            Pos2::new(rect.right() - axis_w - 2.0, rect.bottom() - axis_h - 2.0),
        );
        if axis {
            // dB 网格线（0/-20/-40/-60，三等分）
            for frac in [1.0 / 3.0, 2.0 / 3.0] {
                let y = plot.top() + plot.height() * frac;
                p.line_segment(
                    [Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)],
                    Stroke::new(1.0_f32, pal.grid),
                );
            }
        }
        p.line_segment(
            [plot.left_bottom(), plot.right_bottom()],
            Stroke::new(1.0_f32, pal.grid),
        );
        let n_cols = (((plot.width() + 2.0) / 7.0) as usize).clamp(8, 256);
        let spec = Self::max_pool_cols(bands, n_cols);
        let pk = Self::max_pool_cols(peaks, n_cols);
        let step = plot.width() / n_cols as f32;
        let bar_w = (step - 2.0).max(1.5);
        for (c, v) in spec.iter().enumerate() {
            let x = plot.left() + c as f32 * step;
            let frac = v.clamp(0.0, 1.0);
            let h = frac * plot.height();
            if h >= 1.0 {
                let col = if frac < 0.6 {
                    pal.level_low
                } else if frac < 0.85 {
                    pal.level_mid
                } else {
                    pal.level_high
                };
                p.rect_filled(
                    Rect::from_min_size(Pos2::new(x, plot.bottom() - h), Vec2::new(bar_w, h)),
                    0.0,
                    col,
                );
            }
            let ph = pk[c].clamp(0.0, 1.0) * plot.height();
            if ph >= 1.0 {
                let py = (plot.bottom() - ph - 3.0).max(plot.top());
                p.rect_filled(
                    Rect::from_min_size(Pos2::new(x, py), Vec2::new(bar_w, 2.0)),
                    0.0,
                    pal.peak_fg,
                );
            }
        }
        if axis {
            let lspan = (20000f32.ln() - 20f32.ln()).max(1e-6);
            for (fq, lab) in [
                (50.0f32, "50"),
                (100.0, "100"),
                (500.0, "500"),
                (1000.0, "1k"),
                (5000.0, "5k"),
                (10000.0, "10k"),
                (20000.0, "20k"),
            ] {
                let frac = (fq.ln() - 20f32.ln()) / lspan;
                let x = plot.left() + plot.width() * frac;
                p.line_segment(
                    [
                        Pos2::new(x, plot.bottom()),
                        Pos2::new(x, plot.bottom() + 3.0),
                    ],
                    Stroke::new(1.0_f32, pal.grid),
                );
                p.text(
                    Pos2::new(x, plot.bottom() + 4.0),
                    Align2::CENTER_TOP,
                    lab,
                    mono(FONT * 0.8),
                    pal.fg_weak,
                );
            }
            for (frac, lab) in [
                (0.0, "0dB"),
                (1.0 / 3.0, "-20"),
                (2.0 / 3.0, "-40"),
                (1.0, "-60"),
            ] {
                let y = plot.top() + plot.height() * frac;
                p.text(
                    Pos2::new(plot.right() + 2.0, y),
                    Align2::LEFT_CENTER,
                    lab,
                    mono(FONT * 0.8),
                    pal.fg_weak,
                );
            }
        }
    }

    /// GUI 示波器（f2k 风）：L/R 双波形折线 + 中线基线（纯矢量绘制）。
    fn draw_oscilloscope_gui(
        ui: &Ui,
        pal: &Palette,
        waves: &[[f32; tuneux_corex::spectrum::WAVEFORM_LEN]; 2],
    ) {
        let rect = ui.max_rect();
        let p = ui.painter_at(rect);
        let n = tuneux_corex::spectrum::WAVEFORM_LEN;
        for (bi, wave) in waves.iter().enumerate() {
            let cy = rect.top() + rect.height() * (0.25 + 0.5 * bi as f32);
            let half = rect.height() * 0.22;
            p.line_segment(
                [
                    Pos2::new(rect.left() + 2.0, cy),
                    Pos2::new(rect.right() - 2.0, cy),
                ],
                Stroke::new(1.0_f32, pal.grid),
            );
            p.text(
                Pos2::new(rect.left() + 4.0, cy - 2.0),
                Align2::LEFT_BOTTOM,
                if bi == 0 { "L" } else { "R" },
                mono(FONT * 0.8),
                pal.fg_weak,
            );
            let w = (rect.width() - 8.0).max(4.0);
            let pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let x = rect.left() + 4.0 + w * i as f32 / (n - 1) as f32;
                    Pos2::new(x, cy - wave[i].clamp(-1.0, 1.0) * half)
                })
                .collect();
            p.add(egui::Shape::line(pts, Stroke::new(1.5_f32, pal.spec_bar)));
        }
    }

    fn fit_px(ui: &Ui, text: &str, budget: f32, font: &FontId) -> String {
        if budget <= 0.0 || text.is_empty() {
            return String::new();
        }
        let measure = |t: &str| {
            ui.fonts(|f| {
                f.layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
                    .size()
                    .x
            })
        };
        let full = measure(text);
        if full <= budget {
            return text.to_string();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut cut = ((budget / full.max(1.0)) * chars.len() as f32) as usize;
        cut = cut.clamp(1, chars.len());
        loop {
            let mut t: String = chars[..cut].iter().collect();
            t.push('…');
            if measure(&t) <= budget || cut <= 1 {
                return t;
            }
            cut -= 1;
        }
    }

    /// 每字符列宽（等宽字体 M 的字形宽）。
    /// 给定字号下字形宽度是否接近拉丁字宽（±8%）：滤掉全角/非整数
    /// 宽字符（CJK 全角、░ ▸ ‖ 之类），保证字符柱横向不抖。
    fn is_narrow_at(ui: &Ui, ch: char, size: f32) -> bool {
        let id = mono(size);
        let (cw, w) = ui.fonts(|f| (f.glyph_width(&id, 'M'), f.glyph_width(&id, ch)));
        (w - cw).abs() <= cw * 0.08
    }

    /// 字符池按实测宽度过滤：只留接近拉丁字宽的字形；滤空时调用方
    /// 回退 ASCII 池（跨字体对齐的运行时保障）。
    fn narrow_pool<'a>(ui: &Ui, pool: &[&'a str]) -> Vec<&'a str> {
        pool.iter()
            .copied()
            .filter(|sg| {
                sg.chars()
                    .next()
                    .is_some_and(|ch| Self::is_narrow_at(ui, ch, FONT))
            })
            .collect()
    }

    fn char_w(ui: &Ui) -> f32 {
        ui.fonts(|f| f.glyph_width(&mono(FONT), 'M')).max(4.0)
    }

    // ==================== 菜单栏（1 行，fx 同款：数字弱化 + 标题） ====================

    fn render_menu_bar(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        let t = |k: &str| self.i18n.t(k).into_owned();
        let titles: Vec<(String, String)> = [
            ("menu.file", "1"),
            ("menu.play", "2"),
            ("menu.medium", "3"),
            ("menu.view", "4"),
            ("menu.tools", "5"),
            ("menu.settings", "6"),
            ("menu.plugins", "7"),
            ("menu.help", "8"),
        ]
        .iter()
        .map(|(k, n)| (t(k), n.to_string()))
        .collect();
        let (l_quit, l_open, l_add_dir) = (t("menu.quit"), t("menu.open_files"), t("menu.add_dir"));
        let (l_pp, l_prev, l_next) = (t("menu.play_pause"), t("menu.prev"), t("menu.next"));
        let (l_rep, l_shuf, l_vu, l_vd) = (
            t("menu.repeat"),
            t("menu.shuffle"),
            t("menu.vol_up"),
            t("menu.vol_down"),
        );
        let (l_br, l_cov, l_lyr, l_spec, l_grp) = (
            t("menu.panel_browser"),
            t("menu.panel_cover"),
            t("menu.lyrics"),
            t("menu.panel_spectrum"),
            t("menu.group"),
        );
        let (l_eq, l_comp) = (t("menu.eq"), t("menu.compressor"));
        let (l_dsp, l_tag, l_cvt, l_cvm) = (
            t("menu.dsp"),
            t("menu.tag_edit"),
            t("menu.convert"),
            t("menu.cover_mgmt"),
        );
        let (l_dev, l_rg, l_skin, l_lang) = (
            t("menu.output_device"),
            t("menu.replaygain"),
            t("menu.skin_select"),
            t("menu.lang_select"),
        );
        let l_about = t("menu.about");
        let l_font = t("menu.font");
        let font_names: Vec<String> = self.fonts.iter().map(|f| f.name.clone()).collect();
        let font_cur = self.font_idx;
        // 语言清单：scan_langs 首项即内置 zh/中文，勿再手动前置（会重复）
        let lang_list: Vec<(String, String)> = tuneux_commonx::scan_langs();
        let dev_list = self.output_devices.clone();
        let cur_theme = self.config.theme.clone();
        let l_mods = t("menu.modules");
        let mod_entries = self.mod_entries();
        let l_presets = t("menu.presets");
        let l_preset_save = t("menu.preset_save");
        let cur_lang = self.config.lang.clone();
        let mediums: Vec<(PlaybackMedium, String)> = PlaybackMedium::ALL
            .iter()
            .map(|&m| (m, t(medium_key(m))))
            .collect();
        // 菜单 √ 读 dock 树（模块在树里即在显示——dock 是唯一事实源）
        let dock_has = |m: dock::ModuleId| self.config.dock.as_ref().is_some_and(|d| d.contains(m));
        let (on_br, on_cov, on_lyr, on_spec) = (
            dock_has(dock::ModuleId::Browser),
            dock_has(dock::ModuleId::Cover),
            dock_has(dock::ModuleId::Lyrics),
            dock_has(dock::ModuleId::Spectrum),
        );
        let cur_medium = self
            .engine
            .as_ref()
            .map(|e| e.medium())
            .unwrap_or(PlaybackMedium::None);
        let rg_on = self.replay_gain;
        let modern_on = self.config.modern;
        let l_modern = t("menu.modern_ui");
        let l_bm = t("menu.add_bookmark");

        let mut acts: VecDeque<MenuAct> = VecDeque::new();
        // 设备/预设子菜单点击标志（acts 已被 item 闭包捕获，不能二次可变借用）
        let mut dev_pick = false;
        let mut preset_pick: Option<MenuAct> = None;
        // 菜单栏 id + 各菜单按钮位置（数字键程序化打开用）
        let mut bar_id: Option<egui::Id> = None;
        let mut rects: Vec<(String, Pos2)> = Vec::new();

        egui::TopBottomPanel::top("menu_bar")
            .exact_height(26.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(4, 2)),
            )
            .show(ctx, |ui| {
                let mk_label = |num: &str, title: &str| -> LayoutJob {
                    let mut job = LayoutJob::default();
                    // 现代模式：无数字前缀、比例字体（菜单观感去 TUI 化）
                    if modern_on {
                        job.append(
                            title,
                            0.0,
                            TextFormat::simple(FontId::proportional(FONT), pal.fg),
                        );
                        return job;
                    }
                    job.append(
                        &format!("{num} "),
                        0.0,
                        TextFormat::simple(mono(FONT), pal.fg_weak),
                    );
                    job.append(
                        &format!("{title}  "),
                        0.0,
                        TextFormat::simple(mono(FONT), pal.fg),
                    );
                    job
                };
                let mut item =
                    |ui: &mut Ui, label: &str, act: Option<MenuAct>, on: Option<bool>| {
                        let prefix = match on {
                            Some(true) => "√ ",
                            Some(false) => "  ",
                            None => "",
                        };
                        if ui.button(format!("{prefix}{label}")).clicked() {
                            if let Some(a) = act {
                                acts.push_back(a);
                            }
                            ui.close();
                        }
                    };
                let disabled = |ui: &mut Ui, label: &str| {
                    ui.add_enabled(false, egui::Button::new(label.to_string()));
                };

                egui::MenuBar::new().ui(ui, |ui| {
                    bar_id = Some(ui.id());
                    let r0 = ui.menu_button(mk_label(&titles[0].1, &titles[0].0), |ui| {
                        item(ui, &l_open, Some(MenuAct::PickFiles), None);
                        item(ui, &l_add_dir, Some(MenuAct::PickDir), None);
                        ui.separator();
                        item(ui, &l_quit, Some(MenuAct::Ui(UiAction::Quit)), None);
                    });
                    let r1 = ui.menu_button(mk_label(&titles[1].1, &titles[1].0), |ui| {
                        item(ui, &l_pp, Some(MenuAct::Ui(UiAction::PlayToggle)), None);
                        item(ui, &l_prev, Some(MenuAct::Ui(UiAction::PrevTrack)), None);
                        item(ui, &l_next, Some(MenuAct::Ui(UiAction::NextTrack)), None);
                        ui.separator();
                        item(ui, &l_rep, Some(MenuAct::Ui(UiAction::CycleRepeat)), None);
                        item(
                            ui,
                            &l_shuf,
                            Some(MenuAct::Ui(UiAction::ToggleShuffle)),
                            None,
                        );
                        ui.separator();
                        item(ui, &l_vu, Some(MenuAct::Ui(UiAction::VolumeUp)), None);
                        item(ui, &l_vd, Some(MenuAct::Ui(UiAction::VolumeDown)), None);
                        ui.separator();
                        item(ui, &l_bm, Some(MenuAct::Bookmark), None);
                    });
                    let r2 = ui.menu_button(mk_label(&titles[2].1, &titles[2].0), |ui| {
                        for (m, label) in &mediums {
                            item(ui, label, Some(MenuAct::Medium(*m)), Some(*m == cur_medium));
                        }
                    });
                    let r3 = ui.menu_button(mk_label(&titles[3].1, &titles[3].0), |ui| {
                        item(ui, &l_modern, Some(MenuAct::ModernUi), Some(modern_on));
                        ui.separator();
                        item(
                            ui,
                            &l_br,
                            Some(MenuAct::Ui(UiAction::ToggleBrowser)),
                            Some(on_br),
                        );
                        ui.menu_button(&l_mods, |ui| {
                            for (key, label) in &mod_entries {
                                item(
                                    ui,
                                    label,
                                    Some(MenuAct::ToggleMod((*key).to_string())),
                                    Some(self.mod_on(key)),
                                );
                            }
                            ui.separator();
                            for m in dock::ModuleId::ALL {
                                let label = self.i18n.t(m.label_key()).into_owned();
                                let on = self.config.dock.as_ref().is_some_and(|d| d.contains(m));
                                item(ui, &label, Some(MenuAct::ToggleModule(m)), Some(on));
                            }
                        });
                        ui.menu_button(&l_presets, |ui| {
                            if ui.button(&l_preset_save).clicked() {
                                preset_pick = Some(MenuAct::PresetSave);
                            }
                            if self.config.dock_presets.is_empty() {
                                ui.weak(self.i18n.t("msg.preset_empty"));
                            }
                            for nm in self.config.dock_presets.keys() {
                                if ui.button(nm).clicked() {
                                    preset_pick = Some(MenuAct::PresetLoad(nm.clone()));
                                }
                            }
                        });
                        item(
                            ui,
                            &l_cov,
                            Some(MenuAct::Ui(UiAction::ToggleCover)),
                            Some(on_cov),
                        );
                        item(
                            ui,
                            &l_lyr,
                            Some(MenuAct::Ui(UiAction::ToggleLyrics)),
                            Some(on_lyr),
                        );
                        item(
                            ui,
                            &l_spec,
                            Some(MenuAct::Ui(UiAction::ToggleSpectrum)),
                            Some(on_spec),
                        );
                        item(
                            ui,
                            &l_grp,
                            Some(MenuAct::Ui(UiAction::ToggleGroupView)),
                            None,
                        );
                    });
                    let r4 = ui.menu_button(mk_label(&titles[4].1, &titles[4].0), |ui| {
                        item(ui, &l_eq, Some(MenuAct::Ui(UiAction::ToggleEq)), None);
                        item(ui, &l_comp, Some(MenuAct::ToggleComp), None);
                        ui.separator();
                        disabled(ui, &l_dsp);
                        disabled(ui, &l_tag);
                        disabled(ui, &l_cvt);
                        disabled(ui, &l_cvm);
                    });
                    let r5 = ui.menu_button(mk_label(&titles[5].1, &titles[5].0), |ui| {
                        ui.menu_button(&l_dev, |ui| {
                            for d in &dev_list {
                                if ui.button(d).clicked() {
                                    dev_pick = true;
                                }
                            }
                        });
                        item(ui, &l_rg, Some(MenuAct::ReplayGain), Some(rg_on));
                        ui.separator();
                        ui.menu_button(&l_skin, |ui| {
                            for (tid, tname) in theme::builtin_list() {
                                item(
                                    ui,
                                    tname,
                                    Some(MenuAct::Theme(tid.to_string())),
                                    Some(tid == cur_theme),
                                );
                            }
                            if !self.skin_plugins.is_empty() {
                                ui.separator();
                                for (nm, _) in &self.skin_plugins {
                                    item(
                                        ui,
                                        nm,
                                        Some(MenuAct::Theme(nm.clone())),
                                        Some(*nm == cur_theme),
                                    );
                                }
                            }
                        });
                        ui.menu_button(&l_lang, |ui| {
                            for (lid, lname) in &lang_list {
                                item(
                                    ui,
                                    lname,
                                    Some(MenuAct::Lang(lid.clone())),
                                    Some(*lid == cur_lang),
                                );
                            }
                        });
                        ui.separator();
                        ui.menu_button(&l_font, |ui| {
                            for (fi, fname) in font_names.iter().enumerate() {
                                item(ui, fname, Some(MenuAct::Font(fi)), Some(fi == font_cur));
                            }
                        });
                    });
                    let r6 = ui.menu_button(mk_label(&titles[6].1, &titles[6].0), |ui| {
                        item(ui, &l_eq, Some(MenuAct::NotYet), None);
                        item(ui, &l_comp, Some(MenuAct::NotYet), None);
                    });
                    let r7 = ui.menu_button(mk_label(&titles[7].1, &titles[7].0), |ui| {
                        item(ui, &l_about, Some(MenuAct::Ui(UiAction::About)), None);
                    });
                    for (k, r) in [&r0, &r1, &r2, &r3, &r4, &r5, &r6, &r7]
                        .into_iter()
                        .enumerate()
                    {
                        rects.push((
                            format!("{} {}  ", titles[k].1, titles[k].0),
                            r.response.rect.left_bottom(),
                        ));
                    }
                });
            });

        if dev_pick {
            acts.push_back(MenuAct::DevFollow);
        }
        if let Some(a) = preset_pick {
            acts.push_back(a);
        }
        // 数字键 1-8 / F10：程序化打开对应菜单（BarState 直设，等效点击；
        // menu_id = bar_id.with(按钮文本)，与 egui menu_button 内部同式）。
        // 定点豁免 deprecated：egui 0.32 新 menu API（MenuBar/menu_button）
        // 没有程序化开菜单的等价入口，仅此处的 BarState/MenuRoot 直设可复用；
        // 升级 egui 时重访（若新 API 补齐副入口则迁移）。
        #[allow(deprecated)]
        if let Some(i) = self.pending_menu.take() {
            if let (Some(bid), Some((text, pos))) = (bar_id, rects.get(i)) {
                let mut bs = egui::menu::BarState::load(ctx, bid);
                **bs = Some(egui::menu::MenuRoot::new(*pos, bid.with(text)));
                bs.store(ctx, bid);
            }
        }

        while let Some(a) = acts.pop_front() {
            match a {
                MenuAct::Ui(u) => self.execute(u),
                MenuAct::Medium(m) => {
                    if let Some(e) = &self.engine {
                        e.set_medium(m);
                    }
                }
                MenuAct::ReplayGain => {
                    self.replay_gain = !self.replay_gain;
                    if let Some(e) = &self.engine {
                        e.set_replay_gain_enabled(self.replay_gain);
                    }
                }
                MenuAct::PickFiles => {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter(
                            "音频",
                            &["flac", "wav", "mp3", "ogg", "opus", "wv", "m4a", "aiff"],
                        )
                        .pick_files()
                    {
                        for p in paths {
                            self.enqueue_scan(p);
                        }
                    }
                    // 原生对话框关闭后取回窗口焦点（macOS 有时不自动归还）
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                MenuAct::PickDir => {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.enqueue_scan(dir);
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                MenuAct::Font(i) => {
                    self.font_idx = i;
                    self.fonts_dirty = true;
                    if let Some(f) = self.fonts.get(i) {
                        self.config.font = f.name.clone();
                    }
                }
                MenuAct::Lang(id) => {
                    self.config.lang = id.clone();
                    self.i18n = tuneux_commonx::build_i18n(&id, crate::config::zh_table());
                }
                MenuAct::ToggleComp => {
                    self.toggle_dock_module(dock::ModuleId::Comp);
                }
                MenuAct::NotYet => {
                    self.flash = Some(self.i18n.t("msg.not_yet").into_owned());
                    self.flash_at = Some(Instant::now());
                }
                MenuAct::DevFollow => {
                    self.flash = Some(self.i18n.t("msg.dev_follow").into_owned());
                    self.flash_at = Some(Instant::now());
                }
                MenuAct::ToggleModule(m) => {
                    self.toggle_dock_module(m);
                }
                MenuAct::PresetSave => {
                    self.preset_save(None);
                }
                MenuAct::PresetLoad(name) => {
                    self.preset_load(&name);
                }
                MenuAct::Theme(id) => {
                    self.config.theme = id.clone();
                    self.palette = theme::builtin(&id)
                        .or_else(|| {
                            self.skin_plugins
                                .iter()
                                .find(|(n, _)| *n == id)
                                .map(|(_, p)| p.clone())
                        })
                        .unwrap_or_default();
                    theme::apply_visuals(ctx, &self.palette);
                }
                MenuAct::ToggleMod(key) => {
                    self.toggle_mod(&key);
                }
                MenuAct::ModernUi => {
                    self.config.modern = !self.config.modern;
                    let m = if self.config.modern {
                        self.i18n.t("msg.modern_on").into_owned()
                    } else {
                        self.i18n.t("msg.modern_off").into_owned()
                    };
                    self.flash_msg(m);
                }
                MenuAct::Bookmark => {
                    self.add_bookmark_now();
                }
            }
        }
    }

    // ==================== 当前曲目（6 行三栏，fx draw_now_playing 同款） ====================

    /// 换曲检测：元数据 / 封面纹理 / 歌词 / 波形包络一次性重载
    /// （经典与现代顶栏共用；渲染前统一做，闭包内零 IO 判断）。
    fn ensure_track_media(&mut self, ctx: &egui::Context) {
        let cur = self.current_path.clone();
        if cur != self.cover_path {
            self.cover_path = cur.clone();
            self.metadata = cur.as_ref().map(|p| TrackMetadata::from_file(p));
            self.cover_tex = None;
            if let Some(ref md) = self.metadata {
                if let Some(ref cv) = md.cover {
                    if let Some(img) =
                        tuneux_mediax::decode_cover(&cv.bytes, Some(cv.mime.as_str()))
                    {
                        let ci = egui::ColorImage::from_rgba_unmultiplied(
                            [img.width as usize, img.height as usize],
                            &img.rgba,
                        );
                        self.cover_tex =
                            Some(ctx.load_texture("album_cover", ci, egui::TextureOptions::LINEAR));
                    }
                }
            }
            // 歌词：同名 .lrc
            self.track_envelope = cur.as_ref().map(|pp| (pp.clone(), vec![0.0; 256]));
            self.lyrics_path = cur.clone();
            self.lyrics = cur.as_ref().and_then(|p| {
                let lrc = p.with_extension("lrc");
                if lrc.exists() {
                    Lyrics::load_from_file(&lrc)
                } else {
                    None
                }
            });
        }
    }

    fn render_now_playing(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.current_track").into_owned();

        // 换曲检测：元数据 / 封面 / 歌词一次性重载（渲染前统一做，闭包内零 IO 判断）
        self.ensure_track_media(ctx);

        // 信息栏数据快照（闭包内只读 locals）
        // CUE 分轨：标题/表演者优先取分轨信息（与列表口径一致）
        let cur_cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.clone());
        let info = self.metadata.as_ref().map(|md| {
            let title = cur_cue
                .as_ref()
                .map(|c| c.title.clone())
                .or_else(|| md.title.clone())
                .unwrap_or_else(|| self.i18n.t("metadata.unknown_title").into_owned());
            let artist = cur_cue
                .as_ref()
                .and_then(|c| c.performer.clone())
                .or_else(|| md.artist.clone())
                .unwrap_or_else(|| self.i18n.t("metadata.unknown_artist").into_owned());
            let album_line = match (&md.album, md.track_number) {
                (Some(a), Some(n)) => {
                    format!("{a} · {} {n}", self.i18n.t("metadata.track_label"))
                }
                (Some(a), None) => a.clone(),
                (None, Some(n)) => format!("{} {n}", self.i18n.t("metadata.track_label")),
                (None, None) => self.i18n.t("metadata.unknown_album").into_owned(),
            };
            let tech = format!(
                "{} · {} · {} · {}",
                md.codec.as_deref().unwrap_or("?"),
                md.bitrate_label(),
                md.sample_rate_label(),
                md.bits_label(),
            );
            (title, artist, album_line, tech)
        });
        let empty_hint = self.i18n.t("msg.empty").into_owned();
        let (l_artist, l_album) = (
            self.i18n.t("metadata.artist_label").into_owned(),
            self.i18n.t("metadata.album_label").into_owned(),
        );
        let playing = self.status.as_ref().is_some_and(|s| s.playing);
        let bitstream = self.status.as_ref().is_some_and(|s| s.bitstream);
        let (s_play, s_stop) = (
            self.i18n.t("status.playing").into_owned(),
            self.i18n.t("status.stopped").into_owned(),
        );

        // 三桶频段数据（L/R 平均 → 低/中/高各取段内最大值，fx 同口径）
        let bands = if let Some(e) = &self.engine {
            let [l, r] = e.spectrum_lr();
            let mut m = [0.0f32; tuneux_corex::spectrum::N_BANDS];
            for i in 0..tuneux_corex::spectrum::N_BANDS {
                m[i] = ((l[i] + r[i]) / 2.0).clamp(0.0, 1.0);
            }
            m
        } else {
            [0.0; tuneux_corex::spectrum::N_BANDS]
        };
        let n = tuneux_corex::spectrum::N_BANDS;
        let bucket = |s: usize, e: usize| bands[s..e].iter().copied().fold(0.0f32, f32::max);
        let buckets = [
            bucket(0, n / 3),
            bucket(n / 3, n * 2 / 3),
            bucket(n * 2 / 3, n),
        ];
        let (b_low, b_mid, b_high) = (
            self.i18n.t("spectrum.low").into_owned(),
            self.i18n.t("spectrum.mid").into_owned(),
            self.i18n.t("spectrum.high").into_owned(),
        );

        // 电平数据
        let (lv_l, lv_r) = self
            .engine
            .as_ref()
            .map(|e| e.level_lr())
            .unwrap_or((0.0, 0.0));
        let tick = self.frame_tick;
        let l_level = self.i18n.t("panel.level").into_owned();
        let band_title = self.i18n.t("panel.bands").into_owned();

        egui::TopBottomPanel::top("now_playing")
            .default_height(118.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    // 上边距加大：模块标题不压到上面的面板（用户规格）
                    .inner_margin(egui::Margin {
                        left: 2,
                        right: 2,
                        top: 14,
                        bottom: 2,
                    }),
            )
            .show(ctx, |ui| {
                // tuneux 式：当前曲目 / 三桶 / 电平 = 横向三个独立模块，
                // 各自带框、内容顶格、合计铺满窗宽（无嵌套盒、无顶部空隙）
                // tuneux 基础版同款：同一矩形显式切三份（60/15/25），
                // 等高由构造保证（共享同一 top/bottom），与内容行数解耦
                let area = ui.max_rect();
                let cw = Self::char_w(ui);
                let r_info =
                    Rect::from_min_size(area.min, Vec2::new(area.width() * 0.60, area.height()));
                let r_band = Rect::from_min_size(
                    Pos2::new(r_info.right(), area.top()),
                    Vec2::new(area.width() * 0.15, area.height()),
                );
                let r_level = Rect::from_min_max(Pos2::new(r_band.right(), area.top()), area.max);
                let info_w = r_info.width();
                let band_w = r_band.width();

                // 共享边框：整条一个外框 + 相邻模块之间单条竖线（用户规格：
                // 相邻模块共用竖线，不各画各的双线）；标题嵌在各段顶边线
                let bp = egui::Painter::new(ui.ctx().clone(), ui.layer_id(), Rect::EVERYTHING);
                let outer = Rect::from_min_max(r_info.min, r_level.max);
                bp.rect_filled(outer, egui::CornerRadius::same(0), pal.bg);
                bp.rect_stroke(
                    outer,
                    egui::CornerRadius::same(0),
                    Stroke::new(1.0_f32, pal.border),
                    egui::StrokeKind::Middle,
                );
                for x in [r_band.left(), r_level.left()] {
                    bp.line_segment(
                        [Pos2::new(x, outer.top()), Pos2::new(x, outer.bottom())],
                        Stroke::new(1.0_f32, pal.border),
                    );
                }
                if pal.tui_chrome {
                    Self::paint_box_title(&bp, &pal, r_info, &title);
                    Self::paint_box_title(&bp, &pal, r_band, &band_title);
                    Self::paint_box_title(&bp, &pal, r_level, &l_level);
                } else {
                    // GUI 头部条：三列各自的标题带（与整体框饰同风格）
                    Self::paint_gui_header(&bp, &pal, r_info, &title, false);
                    Self::paint_gui_header(&bp, &pal, r_band, &band_title, false);
                    Self::paint_gui_header(&bp, &pal, r_level, &l_level, false);
                }

                // ── 模块一：当前曲目 ──
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Self::box_inner(r_info)),
                    |ui| {
                        ui.vertical(|ui| match &info {
                            Some((ti, ar, al, tech)) => {
                                let info_cols = (((info_w - 14.0) / cw) as usize).max(8);
                                let icon = if playing { &s_play } else { &s_stop };
                                let icon_color = if playing { Color32::GREEN } else { pal.fg_weak };
                                let mut job = LayoutJob::default();
                                job.append(
                                    &format!("{icon} "),
                                    0.0,
                                    TextFormat::simple(mono(FONT), icon_color),
                                );
                                job.append(
                                    &truncate_to_width(ti, info_cols.saturating_sub(3)),
                                    0.0,
                                    TextFormat {
                                        font_id: mono(FONT + 2.0),
                                        color: pal.fg,
                                        ..Default::default()
                                    },
                                );
                                ui.label(job);
                                ui.label(truncate_to_width(
                                    &format!("{l_artist}: {ar}"),
                                    info_cols,
                                ));
                                ui.label(truncate_to_width(&format!("{l_album}: {al}"), info_cols));
                                let tech_color = if bitstream {
                                    pal.pass_fg
                                } else {
                                    pal.resample_fg
                                };
                                ui.colored_label(tech_color, truncate_to_width(tech, info_cols));
                            }
                            None => {
                                ui.colored_label(pal.fg_weak, &empty_hint);
                            }
                        });
                    },
                );

                // ── 模块二：三桶频段（连续色块，格间无空格，fx 同口径） ──
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Self::box_inner(r_band)),
                    |ui| {
                        ui.vertical(|ui| {
                            // 池随皮肤 bar_style（Hanzi 取尾四字做密度渐变；其余用块字符）
                            let bs = pal.bar_style;
                            let full = bs.pool();
                            let pool_owned: Vec<&str> = if bs == tuneux_mediax::BarStyle::Hanzi {
                                full.iter().rev().take(4).rev().copied().collect()
                            } else {
                                let blocks: Vec<&str> =
                                    ["\u{2588}", "\u{2593}", "\u{2592}", "\u{2591}"].to_vec();
                                let filt = Self::narrow_pool(ui, &blocks);
                                if filt.is_empty() {
                                    vec!["#", "%", "=", "-"]
                                } else {
                                    filt
                                }
                            };
                            let pool: &[&str] = &pool_owned;
                            let dot = if Self::is_narrow_at(ui, '\u{00b7}', FONT) {
                                "\u{00b7}"
                            } else {
                                "."
                            };
                            // 格数按池字符实测宽度算（Hanzi ≈2×拉丁 → 自动减半，
                            // 防止双宽字符撑爆换行导致整条抖动）
                            let pool_cw = pool
                                .first()
                                .and_then(|sg| sg.chars().next())
                                .map(|ch| ui.fonts(|f| f.glyph_width(&mono(FONT), ch)))
                                .unwrap_or(cw)
                                .max(cw);
                            let n_cells = ((((band_w - 14.0) / pool_cw) as usize)
                                .saturating_sub(3))
                            .clamp(3, 64);
                            let band_colors = [pal.level_low, pal.level_mid, pal.level_high];
                            for (bi, (name, energy)) in [
                                (&b_low, buckets[0]),
                                (&b_mid, buckets[1]),
                                (&b_high, buckets[2]),
                            ]
                            .iter()
                            .enumerate()
                            {
                                if !pal.tui_chrome {
                                    // GUI 皮肤：标签 + 实心条（f2k 电平条风，无字符）
                                    let brect = ui
                                        .allocate_exact_size(
                                            Vec2::new(ui.available_width(), LINE_H),
                                            egui::Sense::hover(),
                                        )
                                        .0;
                                    let p = ui.painter_at(brect);
                                    p.text(
                                        Pos2::new(brect.left() + 2.0, brect.center().y),
                                        Align2::LEFT_CENTER,
                                        name.as_str(),
                                        mono(FONT),
                                        pal.fg_weak,
                                    );
                                    let track = Rect::from_min_max(
                                        Pos2::new(brect.left() + 20.0, brect.center().y - 4.0),
                                        Pos2::new(brect.right() - 2.0, brect.center().y + 4.0),
                                    );
                                    p.rect_filled(track, 0.0, pal.grid);
                                    let e = energy.clamp(0.0, 1.0);
                                    if e > 0.003 {
                                        let w = (track.width() * e).round().max(2.0);
                                        p.rect_filled(
                                            Rect::from_min_max(
                                                track.left_top(),
                                                Pos2::new(track.left() + w, track.bottom()),
                                            ),
                                            0.0,
                                            band_colors[bi],
                                        );
                                    }
                                    continue;
                                }
                                let cells = (energy * n_cells as f32 + 0.5) as usize;
                                let mut job = LayoutJob::default();
                                job.append(name, 0.0, TextFormat::simple(mono(FONT), pal.fg_weak));
                                job.append(" ", 0.0, TextFormat::simple(mono(FONT), pal.fg_weak));
                                for i in 0..n_cells {
                                    if i < cells {
                                        // 连续色块：渐变字符按位置取，格间零间隔
                                        job.append(
                                            pool[(i * pool.len() / n_cells.max(1))
                                                .min(pool.len() - 1)],
                                            0.0,
                                            TextFormat::simple(mono(FONT), band_colors[bi]),
                                        );
                                    } else {
                                        job.append(
                                            dot,
                                            0.0,
                                            TextFormat::simple(mono(FONT), pal.grid),
                                        );
                                    }
                                }
                                ui.label(job);
                            }
                            // 补空行至 4 行：与曲目信息模块内容等高对齐
                            ui.add_space(LINE_H);
                        });
                    },
                );

                // ── 模块三：电平（L/R 双 VU，顶格无空隙） ──
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(Self::box_inner(r_level)),
                    |ui| {
                        ui.vertical(|ui| {
                            for (label, level, seed_off) in
                                [("L", lv_l, 0u64), ("R", lv_r, 0xC0FFEE)]
                            {
                                Self::vu_row(ui, &pal, label, level, tick, seed_off);
                            }
                            // 补空行至 4 行：与曲目信息模块内容等高对齐
                            ui.add_space(LINE_H * 2.0);
                        });
                    },
                );
            });
    }

    /// 单声道 VU 行（fx draw_vu_row 同款）：标签青色 + 随机字符柱 + 阈值变色。
    fn vu_row(ui: &mut Ui, pal: &Palette, label: &str, level: f32, tick: u64, seed_off: u64) {
        // GUI 皮肤：实心电平条 + 阈值变色 + 85% 警戒刻度（f2k peak meter 风）
        if !pal.tui_chrome {
            let avail = ui.available_width();
            let rect = ui
                .allocate_exact_size(Vec2::new(avail, LINE_H), egui::Sense::hover())
                .0;
            let p = ui.painter_at(rect);
            p.text(
                Pos2::new(rect.left() + 2.0, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                mono(FONT),
                pal.fg_weak,
            );
            let track = Rect::from_min_max(
                Pos2::new(rect.left() + 20.0, rect.center().y - 4.0),
                Pos2::new(rect.right() - 2.0, rect.center().y + 4.0),
            );
            p.rect_filled(track, 0.0, pal.grid);
            let lv = level.clamp(0.0, 1.0);
            let color = if lv < 0.6 {
                pal.level_low
            } else if lv < 0.85 {
                pal.level_mid
            } else {
                pal.level_high
            };
            if lv > 0.003 {
                let w = (track.width() * lv).round().max(2.0);
                p.rect_filled(
                    Rect::from_min_max(
                        track.left_top(),
                        Pos2::new(track.left() + w, track.bottom()),
                    ),
                    0.0,
                    color,
                );
            }
            let x85 = track.left() + track.width() * 0.85;
            p.line_segment(
                [
                    Pos2::new(x85, track.top() - 1.0),
                    Pos2::new(x85, track.bottom() + 1.0),
                ],
                Stroke::new(1.0_f32, pal.fg_weak),
            );
            return;
        }
        // 池随皮肤 bar_style（fx 同款）；Hanzi 双宽故意不过滤
        let bs = pal.bar_style;
        let pool_full = bs.pool();
        let pool_owned: Vec<&str> = if bs == tuneux_mediax::BarStyle::Hanzi {
            pool_full.to_vec()
        } else {
            let filt = Self::narrow_pool(ui, pool_full);
            if filt.is_empty() {
                vec!["#", "@", "*", "+"]
            } else {
                filt
            }
        };
        let pool: &[&str] = &pool_owned;
        let dot = if Self::is_narrow_at(ui, '·', FONT) {
            "·"
        } else {
            "."
        };
        let lv = level.clamp(0.0, 1.0);
        let color = if lv < 0.6 {
            pal.level_low
        } else if lv < 0.85 {
            pal.level_mid
        } else {
            pal.level_high
        };
        let cyan = Color32::from_rgb(0, 200, 220);

        // 柱位数按可用宽度算（减去标签 8 列）
        let total_w = ui.available_width();
        let cw = Self::char_w(ui);
        // 柱位数按池字符实测宽度算（Hanzi ≈2×拉丁 → 自动减半防溢出）
        let pool_cw = pool
            .first()
            .and_then(|sg| sg.chars().next())
            .map(|ch| ui.fonts(|f| f.glyph_width(&mono(FONT), ch)))
            .unwrap_or(cw)
            .max(cw);
        let bar_cols = ((total_w / pool_cw) as usize)
            .saturating_sub(9)
            .clamp(4, 64);
        let active = (lv * bar_cols as f32).ceil() as usize;

        let mut job = LayoutJob::default();
        job.append(
            &format!("{label} {:3}% ", (lv * 100.0) as u32),
            0.0,
            TextFormat::simple(mono(FONT), cyan),
        );
        let mut rng = tick.wrapping_add(seed_off);
        for i in 0..bar_cols {
            if i < active {
                let r = rng_next(&mut rng);
                let ch = pool[(r as usize) % pool.len()];
                job.append(ch, 0.0, TextFormat::simple(mono(FONT), color));
            } else {
                job.append(dot, 0.0, TextFormat::simple(mono(FONT), pal.grid));
            }
        }
        ui.label(job);
    }

    // ==================== 浏览器（左侧，fx draw_browser 同款） ====================

    fn render_browser_in(&mut self, ui: &mut Ui) {
        // 叶子命中矩形（空白区右键检测）
        self.browser_rect = ui.max_rect();
        let pal = self.palette.clone();
        // 标题 = 当前目录路径（截断到面板宽，fx 同口径）
        let path_str = self.browser.cwd().display().to_string();
        let focused = self.ui.focus == FocusTarget::Browser;
        let empty_hint = self.i18n.t("empty.dir").into_owned();
        let search_here = self.ui.mode == UiMode::Search && self.search_target_browser;
        let mut query_local = self.ui.search_query.clone();
        let want_focus = self.search_focus_req;
        let mut search_blur = false;
        let esc_hint = self.i18n.t("search.esc_exit").into_owned();
        let empty_search_hint = self.i18n.t("search.esc_exit_search").into_owned();

        // 行数据快照：(索引, 文本, 是否目录)——闭包内不借 self.browser
        let entries: Vec<(usize, String, bool)> = self
            .browser
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let text = if e.is_dir() {
                    format!("{}/", e.name())
                } else {
                    e.name().to_string()
                };
                (i, text, e.is_dir())
            })
            .collect();
        let sel = self.browser.selected();

        // 交互结果收集（闭包外执行）
        let mut click = None;
        let mut dbl_dir = false;
        let mut dbl_file = None;
        let mut rmb_row: Option<(usize, Pos2)> = None;
        let mut drag_src = None;

        Self::tui_box(ui, &pal, &truncate_to_width(&path_str, 40), focused, |ui| {
            if search_here {
                // 搜索输入行（/ 进入 · Esc 退出 · Enter 首个匹配）
                ui.horizontal(|ui| {
                    ui.label("/");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut query_local)
                            .font(egui::TextStyle::Monospace)
                            .frame(false)
                            .margin(egui::Margin::same(0))
                            .desired_width(120.0),
                    );
                    ui.colored_label(pal.fg_weak, &esc_hint);
                    if want_focus {
                        resp.request_focus();
                    }
                    // 点击别处 = 退出搜索（防搜索态滞留吞掉全部按键）
                    if !want_focus
                        && !resp.has_focus()
                        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                    {
                        search_blur = true;
                    }
                });
            }
            // 零匹配提示放在输入行之后（fx 同款教训：提前 return 会
            // 把输入框顶掉，按键仍被搜索分支吞掉 = 隐形模态态）
            if entries.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    let hint = if search_here {
                        empty_search_hint.as_str()
                    } else {
                        empty_hint.as_str()
                    };
                    ui.colored_label(pal.fg_weak, hint);
                });
                return;
            }
            let cw = Self::char_w(ui);
            let scroll_follow = self.scroll_to_sel && focused;
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, LINE_H, entries.len(), |ui, range| {
                    // 名字列保底 40 字符：窄盒时横向滚动而非截断
                    ui.set_min_width(cw * 43.0 + 11.0);
                    for idx in range {
                        let Some((i, text, is_dir)) = entries.get(idx) else {
                            continue;
                        };
                        // 像素定位：图标列(2字符宽) + 名字列——目录三角
                        // 与文件名起点在任何字体下都对齐。
                        let (rect, resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), LINE_H),
                            Sense::click_and_drag(),
                        );
                        let is_sel = *i == sel;
                        let p = ui.painter_at(rect);

                        if is_sel {
                            p.rect_filled(rect, 0.0, pal.fg);
                        } else if resp.hovered() {
                            p.rect_filled(rect, 0.0, pal.sel_bg);
                        }
                        let fg = if is_sel { pal.bg } else { pal.fg };
                        let cy = rect.center().y;
                        let f = mono(FONT);
                        let x0 = rect.left() + 3.0;
                        if *is_dir {
                            p.text(
                                Pos2::new(x0, cy),
                                Align2::LEFT_CENTER,
                                "▸",
                                f.clone(),
                                if is_sel { pal.bg } else { pal.accent },
                            );
                        }
                        let x_name = x0 + 2.0 * cw;
                        let name_fit = Self::fit_px(ui, text, rect.right() - x_name - 4.0, &f);
                        p.text(Pos2::new(x_name, cy), Align2::LEFT_CENTER, name_fit, f, fg);
                        if resp.clicked() {
                            click = Some(*i);
                        }
                        if resp.double_clicked() {
                            if *is_dir {
                                dbl_dir = true;
                            } else {
                                dbl_file = Some(*i);
                            }
                        }
                        if resp.secondary_clicked() {
                            if let Some(pos) = resp.hover_pos() {
                                rmb_row = Some((*i, pos));
                            }
                        }
                        if resp.drag_started() {
                            drag_src = Some(*i);
                        }
                        if scroll_follow && *i == sel {
                            ui.scroll_to_rect(resp.rect, Some(egui::Align::Center));
                        }
                    }
                });
        });
        if search_here {
            self.search_focus_req = false;
            if query_local != self.ui.search_query {
                self.ui.search_query = query_local.clone();
                self.browser.set_filter(&query_local);
            }
        }
        if search_blur {
            self.end_search();
        }
        if let Some(i) = click {
            self.browser.select(i);
            self.ui.focus = FocusTarget::Browser;
        }
        if dbl_dir {
            self.browser.enter_selected();
        }
        if let Some(i) = dbl_file {
            if let Some(entry) = self.browser.entries().get(i) {
                let p = entry.path().to_path_buf();
                if let Some(idx) = self.add_file_sync(&p) {
                    self.play_item(idx);
                }
            }
        }
        if let Some((i, pos)) = rmb_row {
            if !self.menu_open_at_frame_start {
                self.browser_menu = Some((i, pos, self.frame_tick));
            }
            self.blank_menu = None;
        }
        if let Some(i) = drag_src {
            self.drag_browser = Some(i);
        }
    }

    // ==================== 播放列表（fx draw_playlist 同款多列行） ====================

    /// 在给定 ui 内渲染播放列表盒（fx draw_playlist 同款：平铺/专辑分组）。
    fn render_playlist_in(&mut self, ui: &mut Ui) {
        // 本面板可用区 = 空白右键命中矩形（频谱全屏时本函数不被调用 → NOTHING）
        self.playlist_rect = ui.max_rect();
        let pal = self.palette.clone();
        let count = self.playlist.items().len();
        let focused = self.ui.focus == FocusTarget::Playlist;
        let empty_hint = self.i18n.t("empty.playlist").into_owned();
        let search_here = self.ui.mode == UiMode::Search && !self.search_target_browser;
        let want_focus = self.search_focus_req;
        let mut search_blur = false;
        // 分组大封面（模块开关）：分组视图非搜索态启用
        let show_covers = self.mod_on("group_covers")
            && self.config.view == PlaylistView::ByAlbum
            && self.ui.mode != UiMode::Search;
        let mut query_local = self.ui.search_query.clone();
        let mut rows_rects: Vec<(usize, egui::Rect)> = Vec::new();
        let mut cover_jobs: Vec<(egui::Rect, PathBuf)> = Vec::new();
        let esc_hint = self.i18n.t("search.esc_exit").into_owned();
        let empty_search_hint = self.i18n.t("search.esc_exit_search").into_owned();
        let unknown_album = self.i18n.t("group.unknown_album").into_owned();
        let count_tpl = self.i18n.t("group.track_count").into_owned();
        let col_title = self.i18n.t("col.title").into_owned();
        let col_artist = self.i18n.t("col.artist").into_owned();
        let col_dur = self.i18n.t("col.dur").into_owned();
        let show_col_header = self.mod_on("col_header");
        let show_group_rule = self.mod_on("group_rule");
        let sel = self.ui.playlist_selected;
        let current = self.playlist.current_index();
        let playing = self.status.as_ref().is_some_and(|s| s.playing);

        // 可见行（平铺 = 全曲目；分组 = 组头+曲目，折叠专辑只留组头；
        // 播放列表搜索态 = 过滤平铺）
        let rows = self.build_rows();
        let title = if search_here && !self.ui.search_query.is_empty() {
            format!(
                "{} ({}/{})",
                self.i18n.t("panel.playlist"),
                rows.len(),
                count
            )
        } else {
            format!("{} · {}", self.i18n.t("panel.playlist"), count)
        };

        // 可见曲目的元数据惰性加载（fx metadata_cache 同口径）
        let pending: Vec<PathBuf> = rows
            .iter()
            .filter_map(|r| match r {
                PlRow::Track { item, .. } => {
                    self.playlist.items().get(*item).map(|it| it.path.clone())
                }
                PlRow::Header { .. } => None,
            })
            .filter(|pp| !self.metadata_cache.contains_key(pp))
            .collect();
        for pp in pending {
            let md = TrackMetadata::from_file(&pp);
            self.metadata_cache.insert(pp, md);
        }

        let mut click = None;
        let mut dbl = None;
        let mut rmb_row: Option<(usize, Pos2)> = None;
        let mut drop_here = false;

        Self::tui_box(ui, &pal, &title, focused, |ui| {
            if search_here {
                ui.horizontal(|ui| {
                    ui.label("/");
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut query_local)
                            .font(egui::TextStyle::Monospace)
                            .frame(false)
                            .margin(egui::Margin::same(0))
                            .desired_width(160.0),
                    );
                    ui.colored_label(pal.fg_weak, &esc_hint);
                    if want_focus {
                        resp.request_focus();
                    }
                    if !want_focus
                        && !resp.has_focus()
                        && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary))
                    {
                        search_blur = true;
                    }
                });
            }
            if rows.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    let hint = if search_here {
                        empty_search_hint.as_str()
                    } else {
                        empty_hint.as_str()
                    };
                    ui.colored_label(pal.fg_weak, hint);
                });
                return;
            }
            let cw = Self::char_w(ui);
            let digits = rows.len().to_string().len().max(1);
            let num_w = digits + 1;
            let dur_w = 8usize;
            // 列宽保底（曲名 50 + 艺术家 22 字符）：窄盒时横向滚动，
            // 不再把列压扁截断（用户规格）
            let natural_cols = 2 + num_w + 4 + dur_w + 50 + 22;
            // 分组封面右列让位（组封面按组高成正方，钳 3–10 行）
            let cover_strip = if show_covers { LINE_H * 8.0 + 8.0 } else { 0.0 };
            let avail_w = ui.available_width() - 8.0 - cover_strip;
            let cols = ((avail_w / cw) as usize).max(natural_cols);
            let flexible = cols.saturating_sub(2 + num_w + 4 + dur_w);
            let artist_w = flexible * 30 / 100;
            let title_w = flexible.saturating_sub(artist_w);
            let title_px = title_w as f32 * cw;
            let artist_px = artist_w as f32 * cw;

            // f2k 列头带（调色板驱动；与行同列位像素对齐）。
            // 横向滚动时隐藏：置顶列头无法与滚动行对齐，宁缺毋错
            let h_scroll = ((ui.available_width() - 8.0) / cw) < natural_cols as f32;
            if show_col_header && !h_scroll {
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), LINE_H),
                    egui::Sense::hover(),
                );
                let p = ui.painter_at(rect);
                p.rect_filled(rect, 0.0, pal.panel_bg);
                let f = mono(FONT);
                let cy = rect.center().y;
                let x0 = rect.left() + 3.0;
                let x_num = x0 + 2.0 * cw;
                let x_title = x_num + (num_w + 1) as f32 * cw;
                let x_artist = x_title + title_px;
                p.text(
                    Pos2::new(x_num, cy),
                    Align2::LEFT_CENTER,
                    "#",
                    f.clone(),
                    pal.fg_weak,
                );
                p.text(
                    Pos2::new(x_title, cy),
                    Align2::LEFT_CENTER,
                    &col_title,
                    f.clone(),
                    pal.fg_weak,
                );
                p.text(
                    Pos2::new(x_artist, cy),
                    Align2::LEFT_CENTER,
                    &col_artist,
                    f.clone(),
                    pal.fg_weak,
                );
                p.text(
                    Pos2::new(rect.right() - 4.0, cy),
                    Align2::RIGHT_CENTER,
                    &col_dur,
                    f,
                    pal.fg_weak,
                );
            }

            let scroll_follow = self.scroll_to_sel && focused;
            let released = ui.input(|i| i.pointer.primary_released());
            let pointer_in = ui
                .max_rect()
                .contains(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()));
            if released && pointer_in && self.drag_browser.is_some() {
                drop_here = true;
            }

            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, LINE_H, rows.len(), |ui, range| {
                    ui.set_min_width(cols as f32 * cw + 8.0 + cover_strip);
                    for ri in range {
                        let Some(row) = rows.get(ri) else {
                            continue;
                        };
                        let f = mono(FONT);
                        match row {
                            PlRow::Header { album, count } => {
                                let (rect, resp) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), LINE_H),
                                    Sense::click_and_drag(),
                                );
                                rows_rects.push((ri, rect));
                                let is_sel = sel == Some(ri);
                                let pt = ui.painter_at(rect);

                                if is_sel {
                                    pt.rect_filled(rect, 0.0, pal.fg);
                                } else if resp.hovered() {
                                    pt.rect_filled(rect, 0.0, pal.sel_bg);
                                }
                                let fg = if is_sel { pal.bg } else { pal.accent };
                                let mark = if self.collapsed.contains(album) {
                                    "▸"
                                } else {
                                    "▾"
                                };
                                let album_label = if album.is_empty() {
                                    unknown_album.as_str()
                                } else {
                                    album.as_str()
                                };
                                let text = format!(
                                    "{mark} {album_label} ({})",
                                    count_tpl.replace("{}", &count.to_string())
                                );
                                pt.text(
                                    Pos2::new(rect.left() + 7.0, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    Self::fit_px(ui, &text, rect.width() - 12.0, &f),
                                    f,
                                    fg,
                                );
                                // f2k 分组头下划线（调色板驱动）
                                if show_group_rule {
                                    let y = rect.bottom() - 1.0;
                                    pt.line_segment(
                                        [
                                            Pos2::new(rect.left() + 7.0, y),
                                            Pos2::new(rect.right() - 7.0, y),
                                        ],
                                        egui::Stroke::new(1.0_f32, pal.accent.linear_multiply(0.6)),
                                    );
                                }
                                if resp.clicked() {
                                    click = Some(ri);
                                }
                                if resp.double_clicked() {
                                    dbl = Some(ri);
                                }
                                if resp.secondary_clicked() {
                                    if let Some(pos) = resp.hover_pos() {
                                        rmb_row = Some((ri, pos));
                                    }
                                }
                                if scroll_follow && is_sel {
                                    ui.scroll_to_rect(resp.rect, Some(egui::Align::Center));
                                }
                            }
                            PlRow::Track { item, seq } => {
                                let (item_idx, seq) = (*item, *seq);
                                let items = self.playlist.items();
                                let path = &items[item_idx].path;
                                let md = self.metadata_cache.get(path);
                                let cue_title =
                                    items[item_idx].cue.as_ref().map(|c| c.title.clone());
                                let name = cue_title
                                    .or_else(|| md.and_then(|m| m.title.clone()))
                                    .unwrap_or_else(|| {
                                        path.file_stem()
                                            .and_then(|st| st.to_str())
                                            .unwrap_or("?")
                                            .to_string()
                                    });
                                let artist = md.and_then(|m| m.artist.clone()).unwrap_or_default();
                                let dur = md
                                    .and_then(|m| m.duration)
                                    .map(fmt_time)
                                    .unwrap_or_default();
                                let is_cur = current == Some(item_idx);
                                let icon = if is_cur {
                                    if playing {
                                        "▶"
                                    } else {
                                        "|"
                                    }
                                } else {
                                    " "
                                };
                                // 像素定位列（对齐与字体宽度比无关）
                                let (rect, resp) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), LINE_H),
                                    Sense::click_and_drag(),
                                );
                                rows_rects.push((ri, rect));
                                let is_sel = sel == Some(ri);
                                let pt = ui.painter_at(rect);

                                if is_sel {
                                    pt.rect_filled(rect, 0.0, pal.fg);
                                } else if resp.hovered() {
                                    pt.rect_filled(rect, 0.0, pal.sel_bg);
                                }
                                let fg = if is_sel { pal.bg } else { pal.fg };
                                let row_color = if is_cur && !is_sel { pal.accent } else { fg };
                                let cy = rect.center().y;
                                let x0 = rect.left() + 3.0;
                                pt.text(
                                    Pos2::new(x0, cy),
                                    Align2::LEFT_CENTER,
                                    icon,
                                    f.clone(),
                                    row_color,
                                );
                                let x_num = x0 + 2.0 * cw;
                                let num_s = pad_left_to_width(&format!("{seq}."), num_w);
                                pt.text(
                                    Pos2::new(x_num, cy),
                                    Align2::LEFT_CENTER,
                                    num_s,
                                    f.clone(),
                                    row_color,
                                );
                                let x_title = x_num + (num_w + 1) as f32 * cw;
                                let title_fit = Self::fit_px(ui, &name, title_px - cw, &f);
                                pt.text(
                                    Pos2::new(x_title, cy),
                                    Align2::LEFT_CENTER,
                                    title_fit,
                                    f.clone(),
                                    row_color,
                                );
                                let x_artist = x_title + title_px;
                                let artist_fit = Self::fit_px(ui, &artist, artist_px - cw, &f);
                                pt.text(
                                    Pos2::new(x_artist, cy),
                                    Align2::LEFT_CENTER,
                                    artist_fit,
                                    f.clone(),
                                    fg,
                                );
                                pt.text(
                                    Pos2::new(rect.right() - 4.0, cy),
                                    Align2::RIGHT_CENTER,
                                    &dur,
                                    f,
                                    fg,
                                );
                                if resp.clicked() {
                                    click = Some(ri);
                                }
                                if resp.double_clicked() {
                                    dbl = Some(ri);
                                }
                                if resp.secondary_clicked() {
                                    if let Some(pos) = resp.hover_pos() {
                                        rmb_row = Some((ri, pos));
                                    }
                                }
                                if scroll_follow && is_sel {
                                    ui.scroll_to_rect(resp.rect, Some(egui::Align::Center));
                                }
                            }
                        }
                    }
                });
        });

        // 分组大封面：由可见行矩形推算各组范围，右列画组首曲封面
        if show_covers && !rows_rects.is_empty() {
            let content_right = rows_rects[0].1.right();
            let mut i = 0;
            while i < rows_rects.len() {
                let (ri, first) = rows_rects[i];
                if matches!(rows.get(ri), Some(PlRow::Header { .. })) {
                    let mut j = i + 1;
                    while j < rows_rects.len()
                        && matches!(rows.get(rows_rects[j].0), Some(PlRow::Track { .. }))
                    {
                        j += 1;
                    }
                    // 组内首个 Track 的路径 = 封面来源
                    let mut src: Option<PathBuf> = None;
                    for rr in &rows_rects[i..j] {
                        if let Some(PlRow::Track { item, .. }) = rows.get(rr.0) {
                            if let Some(it) = self.playlist.items().get(*item) {
                                src = Some(it.path.clone());
                            }
                            break;
                        }
                    }
                    if let (Some(path), Some((_, last))) = (src, rows_rects.get(j - 1)) {
                        let top = first.top();
                        let bot = last.bottom();
                        let h = (bot - top).clamp(LINE_H * 3.0, LINE_H * 10.0);
                        let rect = egui::Rect::from_min_max(
                            Pos2::new(content_right - h - 4.0, top),
                            Pos2::new(content_right - 4.0, top + h),
                        );
                        cover_jobs.push((rect, path));
                    }
                    i = j;
                } else {
                    i += 1;
                }
            }
        }
        for (rect, path) in cover_jobs {
            if let Some(tex) = self.album_cover_tex(ui.ctx(), &path) {
                ui.painter().image(
                    tex.id(),
                    rect,
                    egui::Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }
        if search_here {
            self.search_focus_req = false;
            if query_local != self.ui.search_query {
                self.ui.search_query = query_local;
            }
        }
        if search_blur {
            self.end_search();
        }
        if let Some(ri) = click {
            self.ui.playlist_selected = Some(ri);
            self.ui.focus = FocusTarget::Playlist;
        }
        if let Some(ri) = dbl {
            self.ui.playlist_selected = Some(ri);
            let rows = self.build_rows();
            match rows.get(ri) {
                Some(PlRow::Track { item, .. }) => {
                    self.play_item(*item);
                }
                Some(PlRow::Header { album, .. }) => {
                    let a = album.clone();
                    self.toggle_collapse(&a);
                }
                None => {}
            }
        }
        if let Some((ri, pos)) = rmb_row {
            if !self.menu_open_at_frame_start {
                self.playlist_menu = Some((ri, pos, self.frame_tick));
            }
            self.blank_menu = None;
        }
        if drop_here {
            if let Some(src) = self.drag_browser.take() {
                if let Some(entry) = self.browser.entries().get(src) {
                    let pp = entry.path().to_path_buf();
                    if pp.is_dir() {
                        self.enqueue_scan(pp);
                    } else {
                        self.add_file_sync(&pp);
                    }
                }
            }
        }
    }

    // ==================== 频谱（fx draw_audio_panel 同款字符柱 + 白帽） ====================

    fn render_spectrum_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.spectrum").into_owned();

        // dt + 峰值保持（007 约定：必须用 corex SpectrumPeakHold）
        let now = Instant::now();
        let dt = self
            .last_frame
            .map(|t| now.duration_since(t))
            .unwrap_or_default();
        self.last_frame = Some(now);

        let spectrum = if let Some(e) = &self.engine {
            let [l, r] = e.spectrum_lr();
            let mut m = [0.0f32; tuneux_corex::spectrum::N_BANDS];
            for i in 0..tuneux_corex::spectrum::N_BANDS {
                m[i] = ((l[i] + r[i]) / 2.0).clamp(0.0, 1.0);
            }
            m
        } else {
            [0.0; tuneux_corex::spectrum::N_BANDS]
        };
        let peaks = self.spectrum_peaks.update(&spectrum, dt);
        let tick = self.frame_tick;
        let spec_font = self.spectrum_font;
        let show_axis = self.mod_on("spec_axis");
        let mut zoomed = false;

        Self::tui_box(ui, &pal, &title, false, |ui| {
            // 整个频谱内容区（右键放大在此矩形内任意位置生效）
            let content_rect = ui.max_rect();
            if pal.tui_chrome {
                // 字符池随皮肤 bar_style 键（fx 同款）：Hanzi 汉字池为双宽
                // 故意不用窄过滤（过滤会清空全池）；其余走运行时宽度校验
                let bs = pal.bar_style;
                let pool_full = bs.pool();
                let pool_owned: Vec<&str> = if bs == tuneux_mediax::BarStyle::Hanzi {
                    pool_full.to_vec()
                } else {
                    let filt = Self::narrow_pool(ui, pool_full);
                    if filt.is_empty() {
                        vec!["1", "l", "i", "|", ":", "."]
                    } else {
                        filt
                    }
                };
                let pool: &[&str] = &pool_owned;
                let base_ch = if Self::is_narrow_at(ui, '─', spec_font) {
                    "─"
                } else {
                    "-"
                };
                let n_bands = tuneux_corex::spectrum::N_BANDS;
                let spec_font_id = mono(spec_font);
                let base_cw = ui.fonts(|f| f.glyph_width(&spec_font_id, 'M')).max(4.0);
                let cw = if bs == tuneux_mediax::BarStyle::Hanzi {
                    // 汉字双宽：用池首字符实测宽度（≈2×拉丁列宽，横向分辨率减半）
                    let probe = pool.first().copied().unwrap_or("音");
                    let ch = probe.chars().next().unwrap_or('音');
                    ui.fonts(|f| f.glyph_width(&spec_font_id, ch))
                        .max(base_cw * 2.0)
                        .max(4.0)
                } else {
                    base_cw
                };
                // f2k 结构特征：右侧 dB 刻度列 + 底部频率轴行（调色板驱动）
                let axis = show_axis;
                let db_w = if axis { 6 } else { 0 };
                let axis_row = if axis { 1 } else { 0 };
                let total_cols =
                    (((ui.available_width() - 4.0) / cw).max(8.0) as usize).saturating_sub(db_w);
                let total_rows = (((ui.available_height() - 4.0) / (spec_font + 2.0)).max(3.0)
                    as usize)
                    .saturating_sub(axis_row)
                    .max(3);
                let bar_max_h = total_rows - 1;

                // max-pool 到显示列（fx pool_to_columns 同款）
                let pool_cols = |data: &[f32]| -> Vec<f32> {
                    if total_cols >= n_bands {
                        let mut v = data.to_vec();
                        v.resize(total_cols, 0.0);
                        v
                    } else {
                        let per = n_bands as f32 / total_cols as f32;
                        (0..total_cols)
                            .map(|c| {
                                let st = ((c as f32 * per) as usize).min(n_bands);
                                let en = (((c + 1) as f32 * per) as usize).min(n_bands).max(st + 1);
                                data[st..en].iter().copied().fold(0.0f32, f32::max)
                            })
                            .collect()
                    }
                };
                let disp_spec = pool_cols(&spectrum);
                let disp_peaks = pool_cols(&peaks);

                let mut rng = tick;
                let mut job = LayoutJob::default();
                let fmt_bar = TextFormat::simple(mono(spec_font), pal.spec_bar);
                let fmt_peak = TextFormat::simple(mono(spec_font), pal.peak_fg);
                let fmt_grid = TextFormat::simple(mono(spec_font), pal.grid);

                for row in 0..total_rows {
                    if row == total_rows - 1 {
                        for _ in 0..total_cols {
                            job.append(base_ch, 0.0, fmt_grid.clone());
                        }
                    } else {
                        let from_bottom = total_rows - 1 - row;
                        for col in 0..total_cols {
                            let bar_h = (disp_spec[col] * bar_max_h as f32).ceil() as usize;
                            let peak_h = (disp_peaks[col] * bar_max_h as f32).ceil() as usize;
                            if peak_h > 0 && from_bottom == peak_h {
                                let r = rng_next(&mut rng);
                                job.append(pool[(r as usize) % pool.len()], 0.0, fmt_peak.clone());
                            } else if bar_h > 0 && from_bottom <= bar_h {
                                let r = rng_next(&mut rng);
                                job.append(pool[(r as usize) % pool.len()], 0.0, fmt_bar.clone());
                            } else {
                                job.append(" ", 0.0, fmt_bar.clone());
                            }
                        }
                    }
                    if row < total_rows - 1 {
                        job.append("\n", 0.0, fmt_bar.clone());
                    }
                }
                if axis {
                    // 底部频率轴：对数刻度 50/100/500/1k/5k/10k/20k（截图同款）
                    job.append("\n", 0.0, fmt_grid.clone());
                    let fmt_axis = TextFormat::simple(mono(spec_font), pal.fg_weak);
                    let ticks: [(f32, &str); 7] = [
                        (50.0, "50"),
                        (100.0, "100"),
                        (500.0, "500"),
                        (1000.0, "1k"),
                        (5000.0, "5k"),
                        (10000.0, "10k"),
                        (20000.0, "20k"),
                    ];
                    let mut line = vec![' '; total_cols + db_w];
                    let lspan = (20000f32.ln() - 20f32.ln()).max(1e-6);
                    for (fq, lab) in ticks {
                        let frac = (fq.ln() - 20f32.ln()) / lspan;
                        let col = (frac * (total_cols.saturating_sub(1)) as f32) as usize;
                        for (k, ch) in lab.chars().enumerate() {
                            let c = col + k;
                            if c < line.len() {
                                line[c] = ch;
                            }
                        }
                    }
                    job.append(&line.iter().collect::<String>(), 0.0, fmt_axis);
                    // 右侧 dB 刻度（0 / -20 / -40 / -60，叠画在刻度列上）
                    let p = ui.painter_at(content_rect);
                    let bar_top = content_rect.top() + 2.0;
                    let bar_bot = content_rect.bottom() - (spec_font + 2.0) * 2.0;
                    for (frac, lab) in [
                        (0.0, "0dB"),
                        (1.0 / 3.0, "-20"),
                        (2.0 / 3.0, "-40"),
                        (1.0, "-60"),
                    ] {
                        let y = bar_top + (bar_bot - bar_top).max(1.0) * frac;
                        p.text(
                            Pos2::new(content_rect.right() - 2.0, y),
                            Align2::RIGHT_CENTER,
                            lab,
                            mono(spec_font * 0.8),
                            pal.fg_weak,
                        );
                    }
                }
                ui.label(job);
            } else {
                // GUI 皮肤：实心条 + 峰帽 + 网格/轴（f2k 经典风，无字符）
                Self::draw_spectrum_gui(ui, &pal, content_rect, &spectrum, &peaks, show_axis);
            }
            // 右键 = 放大频谱字符（白名单交互）。直接查询指针状态而非依赖
            // widget 命中测试——ui.interact 大矩形的 CLICKED 标志在嵌套面板
            // 里不可靠（实测不触发）；「secondary_clicked + rect.contains」
            // 是老 UI 模块菜单验证过的稳定手法。
            if ui.input(|i| {
                i.pointer.secondary_clicked()
                    && content_rect.contains(i.pointer.hover_pos().unwrap_or_default())
            }) {
                zoomed = true;
            }
        });
        if zoomed {
            const STEPS: [f32; 5] = [15.0, 18.0, 22.0, 26.0, 32.0];
            let cur = STEPS
                .iter()
                .position(|&x| (x - self.spectrum_font).abs() < 0.5)
                .unwrap_or(0);
            self.spectrum_font = STEPS[(cur + 1) % STEPS.len()];
        }
    }

    /// 示波器（fx draw_oscilloscope 同款：L/R 双声道时域波形，
    /// 中线基线 + 风格池字符随帧变化）。
    fn render_oscilloscope_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.oscilloscope").into_owned();
        let waves = self
            .engine
            .as_ref()
            .map(|e| e.waveform_lr())
            .unwrap_or([[0.0; tuneux_corex::spectrum::WAVEFORM_LEN]; 2]);
        let tick = self.frame_tick;

        Self::tui_box(ui, &pal, &title, false, |ui| {
            if pal.tui_chrome {
                const POOL_ALL: [&str; 6] = ["█", "▓", "▒", "#", "*", "+"];
                let filt = Self::narrow_pool(ui, &POOL_ALL);
                let pool: &[&str] = if filt.is_empty() {
                    &["#", "*", "+"]
                } else {
                    &filt
                };
                let cw = Self::char_w(ui);
                let cols = (((ui.available_width() - 4.0) / cw) as usize).clamp(16, 400);
                let rows_total = (((ui.available_height() - 4.0) / (FONT + 2.0)) as usize).max(8);
                let band_rows = (rows_total / 2).max(3);
                let mid = band_rows / 2;
                let n = tuneux_corex::spectrum::WAVEFORM_LEN;

                let mut rng = tick;
                let mut job = LayoutJob::default();
                let fmt_wave = TextFormat::simple(mono(FONT), pal.spec_bar);
                let fmt_grid = TextFormat::simple(mono(FONT), pal.grid);
                for (bi, wave) in waves.iter().enumerate() {
                    for row in 0..band_rows {
                        for col in 0..cols {
                            let si = (col * n / cols).min(n - 1);
                            let v = wave[si].clamp(-1.0, 1.0);
                            let off = (v * (mid.max(1) as f32 - 1.0)).round() as i32;
                            let cell = mid as i32 - off;
                            let is_wave = cell == row as i32 && v.abs() > 0.02;
                            if is_wave {
                                let r = rng_next(&mut rng);
                                job.append(pool[(r as usize) % pool.len()], 0.0, fmt_wave.clone());
                            } else if row == mid && col == 0 {
                                job.append(if bi == 0 { "L" } else { "R" }, 0.0, fmt_grid.clone());
                            } else if row == mid {
                                job.append("-", 0.0, fmt_grid.clone());
                            } else {
                                job.append(" ", 0.0, fmt_grid.clone());
                            }
                        }
                        // L 带末行也换行（隔开 R 带）；R 带末行不加
                        if row < band_rows - 1 || bi == 0 {
                            job.append(
                                "
",
                                0.0,
                                fmt_grid.clone(),
                            );
                        }
                    }
                }
                ui.label(job);
            } else {
                // GUI 皮肤：L/R 波形折线（f2k 风，无字符）
                Self::draw_oscilloscope_gui(ui, &pal, &waves);
            }
        });
    }

    // ==================== 封面 / 歌词（右侧面板） ====================

    fn render_cover_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.cover").into_owned();
        let none_hint = self.i18n.t("cover.none").into_owned();
        let has_tex = self.cover_tex.is_some();

        Self::tui_box(ui, &pal, &title, false, |ui| {
            if has_tex {
                if let Some(tex) = &self.cover_tex {
                    let avail = ui.available_size();
                    let tsz = tex.size_vec2();
                    let scale = (avail.x / tsz.x).min(avail.y / tsz.y).min(1.0);
                    let display = (tsz * scale).max(Vec2::splat(16.0));
                    ui.vertical_centered(|ui| {
                        ui.add(egui::Image::new(tex).max_size(display));
                    });
                }
            } else {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.colored_label(pal.fg_weak, &none_hint);
                });
            }
        });
    }

    fn render_lyrics_in(&mut self, ui: &mut Ui) {
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.lyrics").into_owned();
        let empty_hint = self.i18n.t("empty.lyrics").into_owned();
        let pos = self.status.as_ref().map(|s| s.position).unwrap_or(0.0);
        // 歌词行快照（闭包内不借 self.lyrics）
        let lines: Vec<(f64, String)> = self
            .lyrics
            .as_ref()
            .map(|ly| {
                ly.lines
                    .iter()
                    .map(|l| (l.timestamp, l.text.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let cur_line = self
            .lyrics
            .as_ref()
            .map(|ly| ly.current_line(pos))
            .unwrap_or(0);

        Self::tui_box(ui, &pal, &title, false, |ui| {
            if lines.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.colored_label(pal.fg_weak, &empty_hint);
                });
                return;
            }
            egui::ScrollArea::both()
                .auto_shrink([false, false])
                .show_rows(ui, LINE_H + 5.0, lines.len(), |ui, range| {
                    // 内容宽 = 可见行最长文本：长行横向滚动而非裁掉
                    let max_w = range
                        .clone()
                        .filter_map(|k| lines.get(k))
                        .map(|(_, t)| {
                            ui.fonts(|fo| fo.layout_no_wrap(t.clone(), mono(FONT), pal.fg).size().x)
                        })
                        .fold(0.0_f32, f32::max);
                    ui.set_min_width(max_w + 16.0);
                    for idx in range {
                        let Some((_, text)) = lines.get(idx) else {
                            continue;
                        };
                        let is_cur = idx == cur_line;
                        let (rect, _resp) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), LINE_H + 5.0),
                            Sense::hover(),
                        );
                        let p = ui.painter_at(rect);
                        if is_cur {
                            // 当前行反色高亮（fx 同款）
                            p.rect_filled(rect, 0.0, pal.fg);
                            p.text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                text,
                                mono(FONT + 1.0),
                                pal.bg,
                            );
                        } else {
                            p.text(
                                rect.center(),
                                Align2::CENTER_CENTER,
                                text,
                                mono(FONT),
                                pal.fg_weak,
                            );
                        }
                        if is_cur {
                            ui.scroll_to_rect(rect, Some(egui::Align::Center));
                        }
                    }
                });
        });
    }

    // ==================== 状态栏（3 行盒，fx draw_status_bar 同款） ====================

    fn render_status_bar(&mut self, ctx: &egui::Context) {
        // 现代模式：单行状态栏（信息 + 提示体系 + 右键模块菜单）
        if self.config.modern {
            self.render_status_bar_modern(ctx);
            return;
        }
        let pal = self.palette.clone();
        let title = self.i18n.t("panel.status").into_owned();
        let show_tech = self.mod_on("status_tech");
        let playing = self.status.as_ref().is_some_and(|s| s.playing);
        let (pos, dur) = self
            .status
            .as_ref()
            .map(|s| (s.position, s.duration))
            .unwrap_or((0.0, 0.0));
        let vol = (self.config.volume * 100.0).round() as u32;
        let (icon, icon_color) = if playing {
            (self.i18n.t("status.playing").into_owned(), Color32::GREEN)
        } else {
            (self.i18n.t("status.stopped").into_owned(), pal.fg_weak)
        };
        let time_str = if dur > 0.0 {
            format!("{}/{}", fmt_time(pos), fmt_time(dur))
        } else {
            "--:--/--:--".to_string()
        };
        let rep = match self.config.repeat {
            RepeatMode::Off => String::new(),
            RepeatMode::List => self.i18n.t("repeat.list").into_owned(),
            RepeatMode::Single => self.i18n.t("repeat.single").into_owned(),
        };
        let shuf = if self.playlist.is_shuffle() {
            self.i18n.t("btn.shuffle").into_owned()
        } else {
            String::new()
        };
        let vol_label = self.i18n.t("btn.volume").into_owned();
        let ratio = if dur > 0.0 {
            (pos / dur).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let err_flash = self.last_error.clone();
        let x_hint = self
            .x_confirm_at
            .is_some_and(|t| t.elapsed().as_secs() < 3)
            .then(|| self.i18n.t("msg.clear_confirm").into_owned());
        let scanning = self.scan_pool.pending() > 0 || self.tag_added > 0;
        let scan_msg = self.i18n.t("msg.scanning").into_owned();
        let flash_msg = self.flash.clone();

        egui::TopBottomPanel::bottom("status_bar")
            .default_height(56.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(egui::Margin::same(2)),
            )
            .show(ctx, |ui| {
                Self::tui_box(ui, &pal, &title, false, |ui| {
                    // 瞬时提示优先级（fx 同口径）：错误红字 > x 二次确认 >
                    // 后台扫描中 > 操作反馈 > 正常进度行
                    if let Some(err) = &err_flash {
                        ui.colored_label(Color32::from_rgb(230, 80, 80), err);
                        return;
                    }
                    if let Some(hint) = &x_hint {
                        ui.colored_label(pal.accent, hint);
                        return;
                    }
                    if scanning {
                        ui.colored_label(pal.accent, &scan_msg);
                        return;
                    }
                    if let Some(fm) = &flash_msg {
                        ui.colored_label(pal.accent, fm);
                        return;
                    }
                    // f2k 技术行：codec | 码率 | 采样率 | 声道 | 位置 / 时长
                    if show_tech {
                        let md = self.metadata.as_ref();
                        let codec = md
                            .and_then(|m| m.codec.clone())
                            .unwrap_or_else(|| "---".to_string());
                        let kbps = md
                            .and_then(|m| m.bitrate)
                            .map(|b| format!("{} kbps", b / 1000))
                            .unwrap_or_default();
                        let hz = md
                            .and_then(|m| m.sample_rate)
                            .map(|r| format!("{} Hz", r))
                            .unwrap_or_default();
                        let ch = md
                            .and_then(|m| m.channels)
                            .map(|c| {
                                if c >= 2 {
                                    "stereo".to_string()
                                } else {
                                    "mono".to_string()
                                }
                            })
                            .unwrap_or_default();
                        let left = [codec, kbps, hz, ch]
                            .into_iter()
                            .filter(|seg| !seg.is_empty())
                            .collect::<Vec<_>>()
                            .join(" | ");
                        let right = format!("{} / {}", fmt_time(pos), fmt_time(dur));
                        ui.horizontal(|ui| {
                            ui.colored_label(pal.fg, &left);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&right).monospace().color(pal.fg),
                                        )
                                        .wrap_mode(egui::TextWrapMode::Truncate),
                                    );
                                },
                            );
                        });
                        return;
                    }
                    // 全像素测量：图标/右段实测宽 + 进度字符实测宽 → 柱数
                    //（任何字体的宽度比都精确，不再依赖显示宽度估算）。
                    let right = format!("{time_str}  {rep}{shuf}  {vol}% {vol_label}");
                    let f = mono(FONT);
                    let icon_px = ui.fonts(|ff| {
                        ff.layout_no_wrap(format!("{icon} "), f.clone(), Color32::WHITE)
                            .size()
                            .x
                    });
                    let right_px = ui.fonts(|ff| {
                        ff.layout_no_wrap(right.clone(), f.clone(), Color32::WHITE)
                            .size()
                            .x
                    });
                    let bar_ch_w = ui.fonts(|ff| ff.glyph_width(&f, '━')).max(1.0);
                    let avail = ui.available_width() - 8.0;
                    // 右段优先：进度条吃剩余空间（窄窗口可缩到 0——
                    // 音量/时间完整显示优先于进度条可见性）
                    let bar_w =
                        (((avail - icon_px - right_px) / bar_ch_w - 2.0) as usize).clamp(0, 400);

                    // 进度条字符（fx draw_progress_chars 同款：━ 已播 ● 当前位置 ─ 未播）
                    let bar_pos = (ratio * bar_w as f64) as usize;
                    let mut bar = String::with_capacity(bar_w * 3 + 2);
                    bar.push(' ');
                    for i in 0..bar_w {
                        if i < bar_pos {
                            bar.push('━');
                        } else if i == bar_pos {
                            bar.push('●');
                        } else {
                            bar.push('─');
                        }
                    }
                    bar.push(' ');

                    let mut job = LayoutJob::default();
                    job.append(
                        &format!("{icon} "),
                        0.0,
                        TextFormat::simple(mono(FONT), icon_color),
                    );
                    job.append(&bar, 0.0, TextFormat::simple(mono(FONT), pal.fg));
                    job.append(" ", 0.0, TextFormat::simple(mono(FONT), pal.fg));
                    job.append(&right, 0.0, TextFormat::simple(mono(FONT), pal.fg));
                    // Extend 模式：永不折行——首帧可用宽度不准时 widget 记住
                    // 错误尺寸导致「量」掉行又恢复；Extend 直接排除折行可能
                    ui.add(egui::Label::new(job).wrap_mode(egui::TextWrapMode::Extend));
                });
            });
    }

    // ==================== 功能键栏（1 行，fx draw_fkey_bar 同款） ====================

    fn render_fkey_bar(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        let keys: Vec<(&'static str, String)> = vec![
            ("1-8", self.i18n.t("fkey.menu").into_owned()),
            ("F5-F8", self.i18n.t("fkey.panels").into_owned()),
            ("F9", self.i18n.t("fkey.eq").into_owned()),
            ("F10", self.i18n.t("fkey.menu").into_owned()),
            ("?", self.i18n.t("fkey.help").into_owned()),
        ];

        egui::TopBottomPanel::bottom("fkey_bar")
            .exact_height(24.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(pal.panel_bg)
                    .inner_margin(egui::Margin::symmetric(6, 1)),
            )
            .show(ctx, |ui| {
                let mut job = LayoutJob::default();
                job.append(" ", 0.0, TextFormat::simple(mono(FONT - 1.0), pal.fg));
                for (k, label) in &keys {
                    job.append(
                        k,
                        0.0,
                        TextFormat {
                            font_id: mono(FONT - 1.0),
                            color: pal.accent,
                            ..Default::default()
                        },
                    );
                    job.append(
                        &format!("{label}   "),
                        0.0,
                        TextFormat::simple(mono(FONT - 1.0), pal.fg),
                    );
                }
                ui.label(job);
            });
    }

    // ==================== 弹出层：关于 / 右键菜单 ====================

    fn render_popups(&mut self, ctx: &egui::Context) {
        let pal = self.palette.clone();
        let _pal = self.palette.clone();

        // —— 浏览器右键菜单 ——
        if let Some((i, pos, opened_tick)) = self.browser_menu {
            let entry = self
                .browser
                .entries()
                .get(i)
                .map(|e| (e.name().to_string(), e.is_dir(), e.path().to_path_buf()));
            let (l_play, l_add, l_dir, l_enter) = (
                self.i18n.t("menu.ctx_add_play").into_owned(),
                self.i18n.t("menu.ctx_add_only").into_owned(),
                self.i18n.t("menu.ctx_add_dir").into_owned(),
                self.i18n.t("menu.ctx_open").into_owned(),
            );
            let mut close = false;
            let mut act: Option<fn(&mut MaxAppV2, &std::path::Path)> = None;
            let mut target: Option<PathBuf> = None;
            egui::Area::new(egui::Id::new("browser_rmb"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(2))
                        .shadow(egui::epaint::Shadow::NONE)
                        .show(ui, |ui| {
                            ui.spacing_mut().button_padding = egui::Vec2::new(6.0, 2.0);
                            ui.set_min_width(170.0);
                            if let Some((_, is_dir, path)) = &entry {
                                if *is_dir {
                                    if ui.button(&l_enter).clicked() {
                                        self.browser.select(i);
                                        self.browser.enter_selected();
                                        close = true;
                                    }
                                    if ui.button(&l_dir).clicked() {
                                        act = Some(|s: &mut MaxAppV2, p: &std::path::Path| {
                                            s.enqueue_scan(p.to_path_buf())
                                        });
                                        target = Some(path.clone());
                                        close = true;
                                    }
                                } else {
                                    if ui.button(&l_play).clicked() {
                                        act = Some(|s: &mut MaxAppV2, p: &std::path::Path| {
                                            if let Some(idx) = s.add_file_sync(p) {
                                                s.play_item(idx);
                                            }
                                        });
                                        target = Some(path.clone());
                                        close = true;
                                    }
                                    if ui.button(&l_add).clicked() {
                                        act = Some(|s, p| {
                                            s.playlist.add(PlaylistItem {
                                                path: p.to_path_buf(),
                                                album: None,
                                                track_number: None,
                                                cue: None,
                                            });
                                        });
                                        target = Some(path.clone());
                                        close = true;
                                    }
                                }
                            }
                        });
                });
            if let (Some(f), Some(p)) = (act, target) {
                f(self, &p);
            }
            if close {
                self.browser_menu = None;
            }
            // 只认左键关闭：右键释放的那一帧 any_click 也为真，会把刚弹出
            // 的菜单同帧杀掉（只活一帧 = 肉眼不可见）。左键点击别处才关；
            // 右键换行 = 重定位菜单，不关闭。
            // 左键任意处、或非打开帧的右键 → 关闭（打开帧的右键是开启
            // 事件本身，必须排除——同帧自杀教训）
            if !close {
                let pri = ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Primary));
                let sec =
                    ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Secondary));
                if pri || (sec && opened_tick != self.frame_tick) {
                    self.browser_menu = None;
                }
            }
        }

        // —— 播放列表右键菜单（行级：曲目行 = 播放/移除；组头行 = 折叠/展开/展开全部）——
        if let Some((ri, pos, opened_tick)) = self.playlist_menu {
            let rows = self.build_rows();
            let target: Option<PlRow> = rows.get(ri).cloned();
            let (l_play, l_rm, l_collapse, l_expand, l_expand_all) = (
                self.i18n.t("menu.ctx_play").into_owned(),
                self.i18n.t("menu.ctx_remove").into_owned(),
                self.i18n.t("menu.ctx_collapse").into_owned(),
                self.i18n.t("menu.ctx_expand").into_owned(),
                self.i18n.t("menu.ctx_expand_all").into_owned(),
            );
            let is_collapsed = match &target {
                Some(PlRow::Header { album, .. }) => self.collapsed.contains(album),
                _ => false,
            };
            let mut close = false;
            let mut do_play: Option<usize> = None;
            let mut do_rm: Option<usize> = None;
            let mut do_toggle: Option<String> = None;
            let mut do_expand_all = false;
            egui::Area::new(egui::Id::new("playlist_rmb"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(2))
                        .shadow(egui::epaint::Shadow::NONE)
                        .show(ui, |ui| match &target {
                            Some(PlRow::Track { item, .. }) => {
                                if ui.button(&l_play).clicked() {
                                    do_play = Some(*item);
                                    close = true;
                                }
                                ui.separator();
                                if ui.button(&l_rm).clicked() {
                                    do_rm = Some(*item);
                                    close = true;
                                }
                            }
                            Some(PlRow::Header { album, .. }) => {
                                let label = if is_collapsed { &l_expand } else { &l_collapse };
                                if ui.button(label).clicked() {
                                    do_toggle = Some(album.clone());
                                    close = true;
                                }
                                ui.separator();
                                if ui.button(&l_expand_all).clicked() {
                                    do_expand_all = true;
                                    close = true;
                                }
                            }
                            None => close = true,
                        });
                });
            if let Some(idx) = do_play {
                self.play_item(idx);
            }
            if let Some(item) = do_rm {
                self.playlist.remove(item);
                let after = self.build_rows();
                self.ui.playlist_selected = if after.is_empty() {
                    None
                } else {
                    Some(ri.min(after.len() - 1))
                };
            }
            if let Some(a) = do_toggle {
                self.toggle_collapse(&a);
            }
            if do_expand_all {
                self.collapsed.clear();
            }
            if close {
                self.playlist_menu = None;
            }
            // 只认左键关闭（防右键同帧自杀）。
            // 左键任意处、或非打开帧的右键 → 关闭（打开帧的右键是开启
            // 事件本身，必须排除——同帧自杀教训）
            if !close {
                let pri = ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Primary));
                let sec =
                    ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Secondary));
                if pri || (sec && opened_tick != self.frame_tick) {
                    self.playlist_menu = None;
                }
            }
        }

        // —— 空白区右键：未命中任何行菜单时弹「加入」菜单（用户诉求：
        //    不必先选中一行也能右键添加）——
        if self.blank_menu.is_none()
            && self.browser_menu.is_none()
            && self.playlist_menu.is_none()
            && !self.menu_open_at_frame_start
            && ctx.input(|i| i.pointer.secondary_clicked())
        {
            if let Some(hp) = ctx.input(|i| i.pointer.hover_pos()) {
                if self.browser_rect.contains(hp) {
                    self.blank_menu = Some((false, hp, self.frame_tick));
                } else if self.playlist_rect.contains(hp) {
                    self.blank_menu = Some((true, hp, self.frame_tick));
                }
            }
        }
        if let Some((is_pl, pos, opened_tick)) = self.blank_menu {
            let (l_files, l_dir, l_clear) = (
                self.i18n.t("menu.open_files").into_owned(),
                self.i18n.t("menu.add_dir").into_owned(),
                self.i18n.t("menu.clear_playlist").into_owned(),
            );
            let mut close = false;
            let mut act: Option<u8> = None;
            egui::Area::new(egui::Id::new("blank_rmb"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(2))
                        .shadow(egui::epaint::Shadow::NONE)
                        .show(ui, |ui| {
                            ui.spacing_mut().button_padding = egui::Vec2::new(6.0, 2.0);
                            ui.set_min_width(170.0);
                            if ui.button(&l_files).clicked() {
                                act = Some(0);
                                close = true;
                            }
                            if ui.button(&l_dir).clicked() {
                                act = Some(1);
                                close = true;
                            }
                            if is_pl {
                                ui.separator();
                                if ui.button(&l_clear).clicked() {
                                    act = Some(2);
                                    close = true;
                                }
                            }
                        });
                });
            match act {
                Some(0) => {
                    if let Some(paths) = rfd::FileDialog::new().pick_files() {
                        for pp in paths {
                            self.enqueue_scan(pp);
                        }
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Some(1) => {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.enqueue_scan(dir);
                    }
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Some(2) => {
                    self.playlist.clear();
                    self.collapsed.clear();
                    self.ui.playlist_selected = None;
                }
                _ => {}
            }
            if close {
                self.blank_menu = None;
            }
            // 只认左键关闭（防右键同帧自杀）。
            // 左键任意处、或非打开帧的右键 → 关闭（打开帧的右键是开启
            // 事件本身，必须排除——同帧自杀教训）
            if !close {
                let pri = ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Primary));
                let sec =
                    ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Secondary));
                if pri || (sec && opened_tick != self.frame_tick) {
                    self.blank_menu = None;
                }
            }
        }

        // —— 状态栏右键：模块加装菜单 ——
        if self.mod_menu.is_none()
            && !self.menu_open_at_frame_start
            && ctx.input(|i| i.pointer.secondary_clicked())
            && ctx
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|pp| self.status_rect.contains(pp))
        {
            self.mod_menu = ctx
                .input(|i| i.pointer.hover_pos())
                .map(|pp| (pp, self.frame_tick));
        }
        if let Some((pos, opened_tick)) = self.mod_menu {
            let entries = self.mod_entries();
            let mut close = false;
            let mut toggle: Option<String> = None;
            egui::Area::new(egui::Id::new("mod_rmb"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(2))
                        .shadow(egui::epaint::Shadow::NONE)
                        .show(ui, |ui| {
                            ui.spacing_mut().button_padding = egui::Vec2::new(6.0, 2.0);
                            ui.set_min_width(170.0);
                            for (key, label) in &entries {
                                let on = self.mod_on(key);
                                let text =
                                    format!("{} {}", if on { "\u{221a}" } else { " " }, label);
                                if ui.button(text).clicked() {
                                    toggle = Some((*key).to_string());
                                    close = true;
                                }
                            }
                        });
                });
            if let Some(key) = toggle {
                self.toggle_mod(&key);
            }
            if close {
                self.mod_menu = None;
            }
            // 左键任意处、或非打开帧的右键 → 关闭
            if !close {
                let pri = ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Primary));
                let sec =
                    ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Secondary));
                if pri || (sec && opened_tick != self.frame_tick) {
                    self.mod_menu = None;
                }
            }
        }

        // —— 命令输入条（':' 进入 · Esc 取消 · Enter 执行；浮在状态栏上方）——
        if self.ui.mode == UiMode::Command {
            let mut cmd_local = self.ui.command_input.clone();
            let hint = self.i18n.t("cmd.execute").into_owned();
            let want_focus = self.command_focus_req;
            let pos = Pos2::new(10.0, (self.status_rect.top() - 36.0).max(60.0));
            egui::Area::new(egui::Id::new("cmd_bar"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(3))
                        .shadow(egui::epaint::Shadow::NONE)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(":");
                                let resp = ui.add(
                                    egui::TextEdit::singleline(&mut cmd_local)
                                        .font(egui::TextStyle::Monospace)
                                        .frame(false)
                                        .desired_width(280.0),
                                );
                                ui.colored_label(pal.fg_weak, &hint);
                                if want_focus {
                                    resp.request_focus();
                                }
                            });
                        });
                });
            self.command_focus_req = false;
            if cmd_local != self.ui.command_input {
                self.ui.command_input = cmd_local;
            }
        }

        // —— dock 叶子右键菜单（f2k 同款操作 + 钻取分页：根页四项，
        //     拆分/更换进子页选模块。全部久经考验原语（Area + button +
        //     ScrollArea）——模块再多子页也只是滚动列表，弹层永不超屏）——
        if let Some((target, pos, opened_tick)) = self.dock_leaf_menu {
            let present =
                |mm: dock::ModuleId| self.config.dock.as_ref().is_some_and(|d| d.contains(mm));
            let names: Vec<(dock::ModuleId, String)> = dock::ModuleId::ALL
                .into_iter()
                .filter(|&mm| mm != target && !present(mm))
                .map(|mm| (mm, self.i18n.t(mm.label_key()).into_owned()))
                .collect();
            let (l_close, l_sh, l_sv, l_ch, l_back) = (
                self.i18n.t("menu.dock_close").into_owned(),
                self.i18n.t("menu.dock_split_h").into_owned(),
                self.i18n.t("menu.dock_split_v").into_owned(),
                self.i18n.t("menu.dock_change").into_owned(),
                self.i18n.t("menu.dock_back").into_owned(),
            );
            let mut close = false;
            let mut nav: Option<u8> = None;
            let mut act: Option<LeafAct> = None;
            egui::Area::new(egui::Id::new("dock_leaf_rmb"))
                .fixed_pos(pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::new()
                        .fill(pal.panel_bg)
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .corner_radius(egui::CornerRadius::same(0))
                        .inner_margin(egui::Margin::same(2))
                        .shadow(egui::epaint::Shadow::NONE)
                        .show(ui, |ui| {
                            ui.spacing_mut().button_padding = egui::Vec2::new(6.0, 2.0);
                            ui.set_min_width(180.0);
                            match self.leaf_page {
                                pg @ (1 | 2) => {
                                    let sbs = pg == 1;
                                    let head = if sbs { &l_sh } else { &l_sv };
                                    if ui.button(format!("\u{2039} {l_back}")).clicked() {
                                        nav = Some(0);
                                    }
                                    ui.colored_label(pal.fg_weak, head);
                                    egui::ScrollArea::vertical()
                                        .max_height(340.0)
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            for (mm, nm) in &names {
                                                if ui.button(nm).clicked() {
                                                    act = Some(LeafAct::Split(target, *mm, sbs));
                                                    close = true;
                                                }
                                            }
                                        });
                                }
                                3 => {
                                    if ui.button(format!("\u{2039} {l_back}")).clicked() {
                                        nav = Some(0);
                                    }
                                    ui.colored_label(pal.fg_weak, &l_ch);
                                    egui::ScrollArea::vertical()
                                        .max_height(340.0)
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            for (mm, nm) in &names {
                                                if ui.button(nm).clicked() {
                                                    act = Some(LeafAct::Replace(target, *mm));
                                                    close = true;
                                                }
                                            }
                                        });
                                }
                                _ => {
                                    if ui.button(&l_close).clicked() {
                                        act = Some(LeafAct::Close(target));
                                        close = true;
                                    }
                                    ui.separator();
                                    if ui.button(&l_sh).clicked() {
                                        nav = Some(1);
                                    }
                                    if ui.button(&l_sv).clicked() {
                                        nav = Some(2);
                                    }
                                    if ui.button(&l_ch).clicked() {
                                        nav = Some(3);
                                    }
                                }
                            }
                        });
                });
            if let Some(pg) = nav {
                self.leaf_page = pg;
            }
            match act {
                Some(LeafAct::Close(m)) => {
                    if let Some(tree) = self.config.dock.take() {
                        self.config.dock = tree.prune(m);
                    }
                }
                Some(LeafAct::Split(t, new, sbs)) => {
                    if let Some(tree) = self.config.dock.as_mut() {
                        tree.split_insert(t, new, sbs);
                    }
                }
                Some(LeafAct::Replace(t, new)) => {
                    if let Some(tree) = self.config.dock.as_mut() {
                        tree.replace_leaf(t, new);
                    }
                }
                None => {}
            }
            if close {
                self.dock_leaf_menu = None;
                self.leaf_page = 0;
            }
            // 页面跳转帧不判关（点击在弹层内部）；左键任意处、或非打开帧
            // 的右键 → 关闭（同帧自杀教训）
            if !close && nav.is_none() {
                let pri = ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Primary));
                let sec =
                    ctx.input(|inp| inp.pointer.button_clicked(egui::PointerButton::Secondary));
                if pri || (sec && opened_tick != self.frame_tick) {
                    self.dock_leaf_menu = None;
                    self.leaf_page = 0;
                }
            }
        }

        // —— 关于弹窗（fx draw_about 同款：字标 + 品牌 + 运行态自检 +
        //    快捷键速查 + 第三方依赖 + 许可/离线声明）——
        if self.ui.about_visible {
            let pal = self.palette.clone();
            let (t_title, t_brand, t_tag, t_cat, t_fmt) = (
                self.i18n.t("panel.about").into_owned(),
                self.i18n.t("about.brand").into_owned(),
                self.i18n.t("about.tagline").into_owned(),
                self.i18n.t("about.plugins_cat").into_owned(),
                self.i18n.t("about.formats").into_owned(),
            );
            let (t_k1, t_k2, t_k3, t_deps, t_lic, t_off) = (
                self.i18n.t("about.keys1").into_owned(),
                self.i18n.t("about.keys2").into_owned(),
                self.i18n.t("about.keys3").into_owned(),
                self.i18n.t("about.deps").into_owned(),
                self.i18n.t("about.license").into_owned(),
                self.i18n.t("about.offline").into_owned(),
            );
            let (t_eq, t_comp, t_skin, ok) = (
                self.i18n.t("panel.eq").into_owned(),
                self.i18n.t("panel.compressor").into_owned(),
                self.i18n.t("panel.skin").into_owned(),
                self.i18n.t("btn.ok").into_owned(),
            );
            // 插件自检标记：已加载 √，未加载 —（fx 同口径）
            let mark = |loaded: bool| if loaded { "√" } else { "—" };
            let selfcheck = format!(
                "{}：{} {}  {} {}  {} {}",
                t_cat,
                t_eq,
                mark(self.eq_plugin.is_some()),
                t_comp,
                mark(self.comp_plugin.is_some()),
                t_skin,
                mark(
                    self.skin_plugins
                        .iter()
                        .any(|(n, _)| *n == self.config.theme)
                ),
            );
            let mut open = self.ui.about_visible;
            let mut close_req = false;
            egui::Window::new(&t_title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(
                        egui::RichText::new(&t_brand)
                            .monospace()
                            .color(pal.border)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "v{} · {} · host ABI v1",
                            env!("CARGO_PKG_VERSION"),
                            t_tag
                        ))
                        .monospace(),
                    );
                    ui.separator();
                    ui.label(egui::RichText::new(&selfcheck).monospace());
                    ui.label(egui::RichText::new(&t_fmt).monospace());
                    ui.separator();
                    ui.label(egui::RichText::new(&t_k1).monospace());
                    ui.label(egui::RichText::new(&t_k2).monospace());
                    ui.label(egui::RichText::new(&t_k3).monospace());
                    ui.separator();
                    ui.label(egui::RichText::new(&t_deps).monospace());
                    ui.label(
                        egui::RichText::new(
                            "symphonia · rubato · cpal · rustfft · egui/eframe · wasmtime",
                        )
                        .monospace()
                        .weak(),
                    );
                    ui.separator();
                    ui.label(egui::RichText::new(&t_lic).monospace().weak());
                    ui.label(egui::RichText::new(&t_off).monospace().weak());
                    ui.add_space(4.0);
                    if ui.button(&ok).clicked() {
                        close_req = true;
                    }
                });
            self.ui.about_visible = open && !close_req;
        }
    }
}

impl eframe::App for MaxAppV2 {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 非搜索态交还控件焦点：按钮/滑条点击后不再截获空格、回车等按键
        //（「键盘失灵」根因之一：焦点滞留在上次点击的控件上）
        if self.ui.mode != UiMode::Search {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        // 面板矩形每帧重置（空白区右键命中判定用）
        self.browser_rect = egui::Rect::NOTHING;
        self.playlist_rect = egui::Rect::NOTHING;
        // 帧首菜单快照：本帧已有菜单开启时，任何右键只做关闭、
        // 不新开（否则旧菜单关闭的同帧新菜单弹出 = 「没消失」观感）
        self.menu_open_at_frame_start = self.browser_menu.is_some()
            || self.playlist_menu.is_some()
            || self.blank_menu.is_some()
            || self.mod_menu.is_some()
            || self.dock_leaf_menu.is_some();
        // 目录变化 → 行索引失效 → 清行菜单（防「进入子目录后旧菜单还在」）
        if self.browser.cwd() != self.last_cwd.as_path() {
            self.last_cwd = self.browser.cwd().to_path_buf();
            self.browser_menu = None;
        }
        self.status_rect = egui::Rect::NOTHING;
        // 主题 + TUI 紧凑风格（每帧下发，皮肤切换即时生效）
        theme::apply_visuals(ctx, &self.palette);
        if self.fonts_dirty {
            if let Some(f) = self.fonts.get(self.font_idx) {
                ctx.set_fonts(theme::build_fonts_with(&f.data));
            }
            self.fonts_dirty = false;
        }
        let mut style = (*ctx.style()).clone();
        if self.config.modern {
            // 现代模式：圆角 + 宽松间距 + 比例字体 + 可见按钮底色
            let r = egui::CornerRadius::same(6);
            style.visuals.window_corner_radius = r;
            style.visuals.menu_corner_radius = r;
            style.visuals.popup_shadow = egui::Shadow {
                offset: [0, 3],
                blur: 6,
                spread: 0,
                color: Color32::from_black_alpha(60),
            };
            style.visuals.window_shadow = egui::Shadow {
                offset: [0, 4],
                blur: 10,
                spread: 0,
                color: Color32::from_black_alpha(70),
            };
            for w in [
                &mut style.visuals.widgets.noninteractive,
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                w.corner_radius = egui::CornerRadius::same(CTRL_R_MODERN);
            }
            style.spacing.item_spacing = Vec2::new(8.0, 6.0);
            style.spacing.button_padding = Vec2::new(10.0, 4.0);
            style.spacing.menu_margin = egui::Margin::same(6);
            style.spacing.window_margin = egui::Margin::same(10);
            style.spacing.interact_size = Vec2::new(18.0, 20.0);
            style.visuals.widgets.inactive.bg_fill = self.palette.sel_bg.linear_multiply(0.5);
            style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            style.visuals.widgets.hovered.bg_fill = self.palette.sel_bg;
            style.visuals.widgets.active.bg_fill = self.palette.accent.linear_multiply(0.7);
            style.override_font_id = Some(FontId::proportional(FONT));
        } else {
            style.visuals.window_corner_radius = egui::CornerRadius::same(0);
            style.visuals.menu_corner_radius = egui::CornerRadius::same(0);
            style.visuals.popup_shadow = egui::Shadow::NONE;
            style.visuals.window_shadow = egui::Shadow::NONE;
            for w in [
                &mut style.visuals.widgets.noninteractive,
                &mut style.visuals.widgets.inactive,
                &mut style.visuals.widgets.hovered,
                &mut style.visuals.widgets.active,
                &mut style.visuals.widgets.open,
            ] {
                w.corner_radius = egui::CornerRadius::same(0);
            }
            style.spacing.item_spacing = Vec2::new(4.0, 2.0);
            style.spacing.button_padding = Vec2::new(4.0, 1.0);
            style.spacing.menu_margin = egui::Margin::same(2);
            style.spacing.window_margin = egui::Margin::same(4);
            style.spacing.interact_size = Vec2::new(14.0, 16.0);
            style.visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            style.visuals.widgets.inactive.bg_stroke = Stroke::NONE;
            style.visuals.widgets.hovered.bg_fill = self.palette.sel_bg;
            style.visuals.widgets.active.bg_fill = self.palette.sel_bg;
            style.override_font_id = Some(mono(FONT));
        }
        ctx.set_style(style);

        // 键盘输入
        self.handle_input(ctx);

        // 引擎轮询 + 状态快照 + 帧计数
        // —— 引擎事件循环（fx main.rs 同款：自动切曲/失败熔断/gapless 同步）——
        let mut ev_finished = false;
        let mut ev_failed = false;
        let mut ev_gapless = false;
        if let Some(e) = &self.engine {
            ev_finished = e.poll_finished().is_some();
            ev_failed = e.poll_failed().is_some();
            ev_gapless = e.poll_track_switched().is_some();
            if ev_finished {
                // 曲目播完：归档实测增益（RG 缓存，下次播该曲免重测）
                if let Some(db) = e.take_measured_gain_db() {
                    if let Some(pp) = self.current_path.clone() {
                        self.replay_gain_cache.insert(pp, db);
                    }
                }
            }
            self.status = Some(e.status());
        }
        if ev_finished {
            self.consecutive_failures = 0;
            let outcome = self.playlist.next(self.config.repeat);
            self.handle_nav_outcome(outcome);
        }
        if ev_failed {
            self.consecutive_failures += 1;
            if self.consecutive_failures >= 10 {
                // 连败熔断：停止自动跳曲（防全损列表无限循环刷屏）
                self.last_error = Some(self.i18n.t("msg.consecutive_fail").into_owned());
                self.last_error_at = Some(Instant::now());
                self.consecutive_failures = 0;
            } else {
                // 失败跳下一首；单曲循环按顺序语义（不重复失败曲）
                let rep = if self.config.repeat == RepeatMode::Single {
                    RepeatMode::Off
                } else {
                    self.config.repeat
                };
                let outcome = self.playlist.next(rep);
                if matches!(outcome, tuneux_mediax::NavOutcome::Switch(_)) {
                    self.handle_nav_outcome(outcome);
                }
            }
        }
        if ev_gapless {
            // Gapless：解码线程已无缝切到预载曲，只同步 UI 不重发 Play
            self.consecutive_failures = 0;
            self.advance_ui_on_gapless();
        }
        // 后台标签结果排空（每帧至多 4 批 × 32 条，海量加入也不掉帧）
        {
            // 并行工人每项一批；每帧最多消化 64 条（海量加入不掉帧）
            let mut drained_items = 0;
            while drained_items < 64 {
                match self.scan_pool.try_recv() {
                    Some(batch) => {
                        self.tag_added += batch.len();
                        drained_items += batch.len();
                        self.playlist.add_many(batch);
                    }
                    None => break,
                }
            }
            if self.scan_pool.pending() == 0 && self.tag_added > 0 {
                let total = self.tag_added;
                self.tag_added = 0;
                self.set_flash_added(total);
            }
            if let Some(at) = self.flash_at {
                if at.elapsed() > std::time::Duration::from_secs(3) {
                    self.flash = None;
                    self.flash_at = None;
                }
            }
        }
        // 系统媒体键（Linux/Windows；macOS 无实现静默缺失）
        let mk_events: Vec<tuneux_commonx::media_key::MediaKeyEvent> = {
            let mut v = Vec::new();
            if let Some(h) = &self.media_keys {
                while let Ok(ev) = h.rx.try_recv() {
                    v.push(ev);
                }
            }
            v
        };
        for ev in mk_events {
            match ev {
                tuneux_commonx::media_key::MediaKeyEvent::PlayPause => self.toggle_play(),
                tuneux_commonx::media_key::MediaKeyEvent::Next => {
                    let o = self.playlist.next(self.config.repeat);
                    self.handle_nav_outcome(o);
                }
                tuneux_commonx::media_key::MediaKeyEvent::Prev => {
                    let o = self.playlist.prev(self.config.repeat);
                    self.handle_nav_outcome(o);
                }
                tuneux_commonx::media_key::MediaKeyEvent::VolumeUp => self.nudge_volume(0.05),
                tuneux_commonx::media_key::MediaKeyEvent::VolumeDown => self.nudge_volume(-0.05),
            }
        }
        if let Some(e) = &self.engine {
            let (pos, dur) = (e.position(), e.duration());
            if dur > 0.0 {
                let [wl, wr] = e.waveform_lr();
                let peak = wl
                    .iter()
                    .chain(wr.iter())
                    .map(|v| v.abs())
                    .fold(0.0f32, f32::max);
                if peak > 0.001 {
                    let bucket = ((pos / dur) * 256.0) as usize;
                    if let Some((_, env)) = &mut self.track_envelope {
                        if bucket < env.len() {
                            env[bucket] = env[bucket].max(peak);
                        }
                    }
                }
            }
        }
        // 可视化插件每帧 tick（谱数据 → 插件 → 字符画）
        if !self.visual_plugins.is_empty() {
            if let Some(e) = &self.engine {
                let [sl, sr] = e.spectrum_lr();
                let bands: [f32; tuneux_corex::spectrum::N_BANDS] =
                    std::array::from_fn(|i| ((sl[i] + sr[i]) / 2.0).clamp(0.0, 1.0));
                for p in &mut self.visual_plugins {
                    p.set_meter(&bands);
                    if p.call_tick().is_ok() {
                        if let Some(text) = p.read_visual() {
                            self.visual_text = text;
                        }
                    }
                }
            }
        }
        self.refresh_last_error();
        self.frame_tick = self.frame_tick.wrapping_add(1);

        // 面板渲染顺序 = egui 布局顺序：
        // 顶：菜单栏 → 当前曲目；底：功能键栏（最先声明=最底）→ 状态栏
        self.render_menu_bar(ctx);
        if self.config.modern {
            // 现代模式：顶栏自带传输控制；不渲染功能键栏（菜单 + 右键全覆盖）
            self.render_now_playing_modern(ctx);
        } else {
            // 传输工具条（f2k 默认模块；右键/菜单可加装或移除）
            if self.mod_on("transport") {
                self.render_transport(ctx);
            }
            self.render_now_playing(ctx);
            self.render_fkey_bar(ctx);
        }
        self.render_status_bar(ctx);
        // 中央工作区：停靠树（模块自由组合，分割条可拖）
        let pal = self.palette.clone();
        let empty_dock = self.i18n.t("dock.empty").into_owned();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(pal.bg)
                    .inner_margin(if self.config.modern {
                        // 现代模式：卡片自带框饰，中央留白对称收紧
                        egui::Margin::same(4)
                    } else {
                        egui::Margin {
                            left: 3,
                            right: 3,
                            top: 12,
                            bottom: 3,
                        }
                    }),
            )
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let dock_pal = self.palette.clone();
                let mut dock = self.config.dock.clone();
                if let Some(node) = dock.as_mut() {
                    self.render_node(ui, &dock_pal, rect, node, 0);
                } else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(dock_pal.fg_weak, &empty_dock);
                    });
                }
                self.config.dock = dock;
            });
        // 拖拽换位：拖到目标叶子松手 = 交换两叶（树手术）
        if let Some((a, b)) = self.dock_swap.take() {
            if let Some(t) = self.config.dock.as_mut() {
                t.swap_modules(a, b);
            }
            self.dock_drag = None;
        }
        if ctx.input(|i| i.pointer.primary_released()) {
            self.dock_drag = None;
        }

        // 弹出层
        self.render_popups(ctx);

        // 键盘滚动请求已消费
        self.scroll_to_sel = false;

        ctx.request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.config.last_dir = Some(self.browser.cwd().to_path_buf());
        crate::config::save(&self.config);
        // 退出前保存当前进度（断点续播）
        self.save_current_position();
        // 播放列表持久化（规范多列表格式：单列表包一层）。
        let st = tuneux_mediax::PlaylistState {
            lists: vec![tuneux_mediax::SavedList {
                name: self.i18n.t("list.default").into_owned(),
                items: self.playlist.items().to_vec(),
                current: self.current_path.clone(),
                shuffle: self.playlist.is_shuffle(),
            }],
            active: 0,
            positions: self.positions.clone(),
            replay_gain: self.replay_gain_cache.clone(),
            current: None,
            items: Vec::new(),
            shuffle: false,
        };
        crate::config::save_state(&st);
    }
}
