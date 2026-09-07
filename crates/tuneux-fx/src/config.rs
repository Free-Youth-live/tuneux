//! # 配置模块
//!
//! 负责 tuneux-fx 运行时配置的加载与持久化。配置以 TOML 格式存储，
//! 文件名固定为 `tuneux-fx.toml`（与基础版 tuneux 的 `tuneux.toml` 分离，避免互覆盖）。
//!
//! ## 配置文件位置策略（便携优先）
//!
//! 1. **默认**：与可执行文件同目录的 `tuneux-fx.toml`。
//!    这样把 `tuneux-fx`（或 `tuneux-fx.exe`）+ `tuneux-fx.toml` 一起拷到 U 盘，
//!    单文件就是一个完整播放器，配置随身携带。
//! 2. **回退**：若 exe 同目录不可写（只读 U 盘、装到 `/usr/local/bin` 等系统位置），
//!    配置改存到系统标准配置目录：
//!    - Windows：`%APPDATA%\tuneux-fx\tuneux-fx.toml`
//!    - macOS：`~/Library/Application Support/tuneux-fx/tuneux-fx.toml`
//!    - Linux：`~/.config/tuneux-fx/tuneux-fx.toml`
//!
//! ## 容错原则
//!
//! 配置加载失败（文件不存在、解析错误、IO 错误）**绝不 panic**，
//! 一律回退到默认值，保证程序始终可启动。
//! 保存失败仅打印警告，不阻塞退出。

// 本模块与基础版 config 同源，本产品线已分化出主题/首启继承/歌词偏移/书签等；
// 尚未接入或不启用的公开接口，临时豁免 dead_code 警告。
#![allow(dead_code)]
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::playlist::PlaylistItem;

/// 配置操作结果别名。
///
/// 错误聚合为 trait 对象，避免为每种底层错误（IO、TOML 解析）单独定义类型，
/// 简化代码。配置模块的错误对用户均不致命，调用方据此回退默认值。
pub type ConfigResult<T> = Result<T, Box<dyn std::error::Error>>;

/// 循环播放模式。
///
/// 三态循环：关闭 → 单曲 → 列表，循环切换。
/// 用枚举而非魔法数字，配合 serde 以可读字符串存入 TOML（如 `repeat = "single"`），
/// 配置文件对人友好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    /// 不循环：播放到列表末尾即停止。
    #[default]
    Off,
    /// 单曲循环：当前曲目无限重复。
    Single,
    /// 列表循环：整列表循环播放。
    List,
}

impl RepeatMode {
    /// 循环切换到下一个模式：Off → Single → List → Off。
    /// 用于按键 `r` 的行为。
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::Single,
            RepeatMode::Single => RepeatMode::List,
            RepeatMode::List => RepeatMode::Off,
        }
    }

    /// 中文字幕，用于 TUI 状态条显示。
    pub fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "顺序",
            RepeatMode::Single => "单曲",
            RepeatMode::List => "循环",
        }
    }
}

/// 播放列表的显示模式。
///
/// `g` 键切换，存入配置，下次启动恢复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistView {
    /// 按专辑分组显示。
    ByAlbum,
    /// 平铺多列大排行（曲名/艺术家/时长，参照 foobar2000，fx 默认）。
    #[default]
    Flat,
}

/// 可视化面板的显示模式（`v` 键切换：关 → 频谱半屏 → 频谱全屏 → 示波器 → 关）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpectrumMode {
    /// 不显示可视化。
    #[default]
    Hidden,
    /// 频谱占主区一半（与播放列表上下分屏）。
    Half,
    /// 频谱占满整个主区。
    Full,
    /// 示波器（左右声道时域波形）占满整个主区。
    Oscilloscope,
}

impl SpectrumMode {
    /// 循环到下一模式：Hidden → Half → Full → Oscilloscope → Hidden。
    pub fn next(self) -> Self {
        match self {
            SpectrumMode::Hidden => SpectrumMode::Half,
            SpectrumMode::Half => SpectrumMode::Full,
            SpectrumMode::Full => SpectrumMode::Oscilloscope,
            SpectrumMode::Oscilloscope => SpectrumMode::Hidden,
        }
    }
}

/// 歌词面板的显示模式（`l` 键切换：隐藏 ↔ 显示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LyricsMode {
    /// 不显示歌词。
    #[default]
    Hidden,
    /// 歌词显示在播放列表右侧。
    Visible,
}

impl LyricsMode {
    /// 循环到下一模式：Hidden ↔ Visible。
    pub fn next(self) -> Self {
        match self {
            LyricsMode::Hidden => LyricsMode::Visible,
            LyricsMode::Visible => LyricsMode::Hidden,
        }
    }
}

/// 左侧面板的显示状态（`b` 浏览器 / `c` 封面/封面浏览，三者互斥）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LeftPanel {
    /// 左侧不显示面板（播放列表占满主区）。
    #[default]
    Hidden,
    /// 左侧显示文件浏览器。
    Browser,
    /// 左侧显示单张专辑封面。
    Cover,
    /// 左侧显示封面网格浏览（按专辑）。
    CoverBrowser,
}

/// 应用配置。
///
/// 所有字段都派生 serde，整体序列化为 TOML。
/// 数值字段加 `#[serde(default)]`，保证旧版本配置文件缺少新字段时
/// 仍能正常加载（前向兼容）。
///
/// 注意：手动实现 `Default`（不派生），因为 f32 的派生默认值是 0.0，
/// 而音量默认应为满音量 1.0；serde 的 `default = "..."` 只影响反序列化，
/// 不影响 `Default` trait，二者需分开处理。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 播放音量，范围 0.0（静音）~ 1.0（满音量）。
    /// 存为 f32 而非百分比，避免整数除法；TUI 显示时再 ×100。
    #[serde(default = "default_volume")]
    pub volume: f32,

    /// ReplayGain 响度归一开关（默认关 = 不做任何增益改动）。
    /// 开启后播放中测量响度（-14 LUFS 目标），后续播放按缓存增益归一。
    #[serde(default)]
    pub replay_gain: bool,

    /// 循环模式。
    #[serde(default)]
    pub repeat: RepeatMode,

    /// 是否随机播放。
    #[serde(default)]
    pub shuffle: bool,

    /// 播放列表显示模式（平铺 / 按专辑分组）。
    #[serde(default)]
    pub playlist_view: PlaylistView,

    /// 频谱显示模式（关 / 半屏 / 全屏），退出时保留。
    #[serde(default)]
    pub spectrum_mode: SpectrumMode,

    /// 歌词显示模式（隐藏 / 显示），退出时保留。
    #[serde(default)]
    pub lyrics_mode: LyricsMode,

    /// 歌词时间偏移（秒）：正值 = 歌词延后、负值 = 提前。按键 `[` / `]` 调整。
    #[serde(default)]
    pub lyrics_offset: f64,

    /// 左侧面板状态（隐藏 / 浏览器 / 封面），退出时保留。
    #[serde(default)]
    pub left_panel: LeftPanel,

    /// 播放介质风格（corex 全部 9 档的字符串名；兼容旧短名 tape/vinyl）。
    /// 只作用于声音（DSP 修饰），界面布局不受影响。
    /// 退出时保留，下次启动沿用。
    #[serde(default = "default_playback_medium")]
    pub playback_medium: String,

    /// 配色主题（Dos / Clean），退出时保留。
    #[serde(default)]
    pub theme: crate::tui::theme::Theme,

    /// 文件浏览器占终端宽度的比例（0.1 ~ 0.9）。
    /// 例如 0.4 表示左侧浏览器占 40%，右侧元数据/播放列表占 60%。
    /// 用 f32 比例而非固定字符数，以自适应不同终端宽度。
    #[serde(default = "default_browser_ratio")]
    pub browser_ratio: f32,

    /// 上次浏览的目录，下次启动时恢复，免去重新导航。
    /// 用 Option 而非 PathBuf，区分"首次启动"（None，用系统音乐目录）
    /// 与"曾经打开过某目录"（Some）。
    #[serde(default)]
    pub last_dir: Option<PathBuf>,

    /// 加入播放列表时是否自动去重（默认开启）。
    ///
    /// 开启后，`add` / `add_many` 会跳过已存在的条目。
    /// 唯一性判定：`(path, cue.index)` 元组——同 path 的普通曲目视为重复，
    /// 但 CUE 分轨（同 path、不同 cue.index）不算重复。
    /// 旧配置文件无此字段时默认为 true（serde default）。
    #[serde(default = "default_dedup_on_add")]
    pub dedup_on_add: bool,

    /// 自定义按键映射：动作名 → 键描述（如 `"toggle_play" = "space"`）。
    ///
    /// **动作名**（map 的键，即"要做什么"）：`toggle_play`（播放/暂停）、
    /// `next`（下一曲）、`prev`（上一曲）、`volume_up`（音量 +5%）、
    /// `volume_down`（音量 −5%）。
    ///
    /// **键描述**（map 的值，即"按哪个键"）语法见 [`parse_key_desc`]，
    /// 例：`"space"`、`"n"`、`"shift+n"`、`"+"`、`"-"`。
    ///
    /// 默认空 map：全部沿用内置默认键（空格/n/p/+/-），旧配置无需迁移。
    /// 未知动作名或无法解析的键描述会被忽略并回退默认键，绝不 panic。
    ///
    /// 注意：当前键处理尚未读取此映射（配置了也暂不生效），字段保留供后续
    /// 接入自定义键位；在此之前请使用内置默认键。
    #[serde(default)]
    pub keymap: HashMap<String, String>,
    /// 系统媒体键开关（默认开启）。
    ///
    /// 关闭场景：Windows 低层键盘钩子是系统级全局行为、Linux MPRIS 服务名
    /// 可能与其他播放器冲突——用户可设 `media_keys_enabled = false` 关闭。
    #[serde(default = "default_media_keys_enabled")]
    pub media_keys_enabled: bool,
}

/// 默认音量：1.0（满音量）。
/// 作为 serde default 函数，仅在配置文件缺该字段时使用。
fn default_volume() -> f32 {
    1.0
}

/// 播放介质风格的默认值（字符串）。
fn default_playback_medium() -> String {
    "none".to_string()
}

/// 默认浏览器比例：0.4（左 40%）。
fn default_browser_ratio() -> f32 {
    0.4
}

/// 去重开关默认值：true（加入时自动跳过重复条目）。
fn default_dedup_on_add() -> bool {
    true
}

/// 系统媒体键默认开启。
fn default_media_keys_enabled() -> bool {
    true
}

/// 解析并规范化一条自定义键描述（`keymap` 的值），返回规范形式；非法返回 None。
///
/// # 语法
///
/// 键描述 = `[shift+][ctrl+][alt+]<键名>`。修饰符大小写不敏感、顺序不敏感
/// （输出统一为 shift → ctrl → alt），段间可含空白。`<键名>` 为：
///
/// - **单个字符**：直接写字符本身，如 `"n"`、`"+"`、`"-"`、`"="`。
///   `" "`（单空格）等价于 `"space"`。
/// - **具名键**（小写、别名见下）：`"space"`、`"tab"`、`"enter"`（别名
///   `"return"`）、`"esc"`（别名 `"escape"`）、`"backspace"`（别名 `"delete"`）、
///   `"up"`、`"down"`、`"left"`、`"right"`、`"home"`、`"end"`。
///
/// 修饰符 **shift 仅对字母键有意义**（如 `"shift+n"`）：符号键的 Shift 是
/// 打出符号本身所需，键描述里不写 shift（`"+"` 即代表加号键）。
///
/// 示例：`"n"` → `"n"`；`"space"` → `"space"`；`"Shift+N"` → `"shift+n"`；
/// `"ctrl+c"` → `"ctrl+c"`；`"+"` → `"+"`。
///
/// # 返回 None（该映射被忽略，回退内置默认键）的情形
///
/// 空串、纯修饰符（如 `"shift+"`）、多个键名（如 `"ab"`）、未知键名
/// （如 `"f1"`、`"foo"`）。
pub fn parse_key_desc(desc: &str) -> Option<String> {
    // 单空格直接代表空格键
    if desc == " " {
        return Some("space".to_string());
    }

    // 单字符键（含 '+'、'-' 等会被 split('+') 拆散的特殊字符）直接规范化；
    // 单字符不可能携带修饰符。
    let trimmed = desc.trim();
    if trimmed.chars().count() == 1 {
        return canonical_key_name(trimmed, false);
    }

    let mut shift = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut base: Option<&str> = None;

    // 按 '+' 拆分修饰符与键名；'+' 键本身已在上面的单字符分支处理。
    for part in trimmed.split('+') {
        let p = part.trim();
        if p.is_empty() {
            return None; // 空段（如 "shift++n"）
        }
        match p.to_ascii_lowercase().as_str() {
            "shift" => shift = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "meta" => alt = true,
            _ => {
                // 非修饰符段即键名；键名只能出现一次
                if base.is_some() {
                    return None;
                }
                base = Some(p);
            }
        }
    }

    let base = base?; // 纯修饰符（无键名）
    let canonical = canonical_key_name(base, shift)?;

    // 按固定顺序拼出规范形式
    let mut out = String::new();
    if shift {
        out.push_str("shift+");
    }
    if ctrl {
        out.push_str("ctrl+");
    }
    if alt {
        out.push_str("alt+");
    }
    out.push_str(&canonical);
    Some(out)
}

/// 把键名部分规范化为小写具名键或单字符（字母 + shift 时统一小写）。
///
/// 输出格式与按键事件侧的规范键描述保持同一格式，二者对齐后才能
/// 命中映射（自定义键位接入按键处理后生效，见 Config.keymap 字段说明）。
fn canonical_key_name(name: &str, shift: bool) -> Option<String> {
    // 具名键：大小写不敏感
    let named = match name.to_ascii_lowercase().as_str() {
        "space" => Some("space"),
        "tab" => Some("tab"),
        "enter" | "return" => Some("enter"),
        "esc" | "escape" => Some("esc"),
        "backspace" | "delete" => Some("backspace"),
        "up" => Some("up"),
        "down" => Some("down"),
        "left" => Some("left"),
        "right" => Some("right"),
        "home" => Some("home"),
        "end" => Some("end"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_string());
    }

    // 单字符键：字母 + shift 时统一小写（与 key_to_desc 一致），其余原样
    let mut chars = name.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None; // 多字符且非具名键 → 未知
    }
    let c = if shift && c.is_ascii_alphabetic() {
        c.to_ascii_lowercase()
    } else {
        c
    };
    Some(c.to_string())
}

/// 手动实现 Default：音量默认满音量 1.0（而非 f32 派生的 0.0 静音），
/// 浏览器比例默认 0.4，其余字段用类型的自然默认。
impl Default for Config {
    fn default() -> Self {
        Self {
            volume: default_volume(),
            replay_gain: false,
            repeat: RepeatMode::default(),
            shuffle: false,
            playlist_view: PlaylistView::Flat,
            spectrum_mode: SpectrumMode::Hidden,
            lyrics_mode: LyricsMode::Hidden,
            lyrics_offset: 0.0,
            // -fx 默认浏览器常显；tuneux 默认隐藏，按 b 唤出。
            left_panel: LeftPanel::Browser,
            playback_medium: "none".to_string(),
            theme: crate::tui::theme::Theme::default(),
            browser_ratio: default_browser_ratio(),
            last_dir: None,
            dedup_on_add: default_dedup_on_add(),
            keymap: HashMap::new(),
            media_keys_enabled: default_media_keys_enabled(),
        }
    }
}

impl Config {
    /// 获取上次浏览目录；若为首次启动（None），回退到系统音乐目录或主目录。
    ///
    /// 返回值保证非空（最坏回退到当前目录 "."），调用方无需再判空。
    pub fn effective_last_dir(&self) -> PathBuf {
        if let Some(d) = &self.last_dir {
            return d.clone();
        }
        // 优先用系统"音乐"目录（Windows/Mac/Linux 均有标准位置），
        // 其次用户主目录，最后当前目录兜底。
        dirs::audio_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// 播放状态（保存到独立文件 `playlist.toml`）。
///
/// 与 `tuneux.toml`（配置偏好）分离：这里存播放列表与每首曲目的
/// 播放进度（断点续播），体积可能较大且频繁变化。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlaylistState {
    /// 上次退出时正在播放的歌曲路径（下次启动自动选中）。
    #[serde(default)]
    pub current: Option<PathBuf>,
    /// 上次退出时正在播放的 CUE 分轨号（配合 `current` 定位同一整轨文件内的
    /// 具体分轨，避免恢复时只落到首分轨）。`#[serde(default)]`
    /// 保证旧版（无此字段）正常加载。
    #[serde(default)]
    pub current_cue: Option<u32>,
    /// 播放列表条目（按插入序）。
    pub items: Vec<PlaylistItem>,
    /// 每首曲目的播放进度（路径 → 秒）。
    pub positions: BTreeMap<PathBuf, f64>,
    /// ReplayGain 测量结果缓存（路径 → 增益 dB）。
    ///
    /// 与断点续播进度一同持久化到 `playlist.toml`：同一首歌曲
    /// 首次播放时流式分析测得整曲响度，之后每次播放直接套用缓存
    /// 的增益，免去重复分析（跨会话有效）。`#[serde(default)]`
    /// 保证旧版 `playlist.toml`（无此字段）仍可正常加载。
    #[serde(default)]
    pub replay_gain: BTreeMap<PathBuf, f64>,
    /// 书签列表（路径 + 位置 + 标签），随 playlist.toml 持久化。
    #[serde(default)]
    pub bookmarks: tuneux_mediax::bookmark::BookmarkList,
}

impl PlaylistState {
    /// 记录某曲目的播放位置（秒）。重复保存同一曲目会覆盖旧值。
    pub fn save_position(&mut self, path: &Path, secs: f64) {
        // 0 附近的位置不值得记录（刚开播/几乎从头）——避免噪声占满 map
        if secs > 0.5 {
            self.positions.insert(path.to_path_buf(), secs);
        } else {
            // 接近 0 时清掉旧记录（防止"上次听了 200s 的歌被覆盖成 0.1"）
            self.positions.remove(path);
        }
    }

    /// 取某曲目的上次播放位置。无记录返回 None。
    pub fn get_position(&self, path: &Path) -> Option<f64> {
        self.positions.get(path).copied()
    }
}

/// 计算"exe 同目录优先、系统目录回退"的某文件路径。
///
/// 处理逻辑：
/// 1. 取 exe 所在目录，若该目录下已存在该文件（说明此前用过便携模式），
///    或该目录可写（尝试创建临时探测文件验证），则使用 `exe_dir/文件`。
/// 2. 否则回退到系统配置目录 `config_dir/tuneux-fx/文件`，并自动创建中间目录。
///
/// 注：探测可写性而非依赖权限判断，是因为跨平台权限模型差异大
/// （Windows ACL、Unix umask），实测最可靠。
fn portable_path(filename: &str) -> PathBuf {
    // 尝试路径 1：exe 同目录
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(filename);
            // 文件已存在 → 沿用便携模式（即使目录现变为只读，也尊重既有文件）
            // 文件不存在但目录可写 → 新建便携文件
            if candidate.exists() || is_writable(dir) {
                return candidate;
            }
        }
    }

    // 回退路径 2：系统配置目录（tuneux-fx 独立目录，与基础版隔离）
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("tuneux-fx");
    // 回退目录不存在时自动创建（首次运行），失败忽略——save 时再处理错误
    let _ = std::fs::create_dir_all(&path);
    path.push(filename);
    path
}

/// 配置文件 `tuneux-fx.toml` 的存储路径。
pub fn config_path() -> PathBuf {
    portable_path("tuneux-fx.toml")
}

/// 播放状态文件 `tuneux-fx-playlist.toml` 的存储路径（与基础版分离，避免互覆盖）。
pub fn playlist_state_path() -> PathBuf {
    portable_path("tuneux-fx-playlist.toml")
}

/// 插件目录解析（只读：加载 .wasm / .manifest / .sig）。
///
/// 三级候选，命中「已存在的目录」即返回：
/// 1. exe 同目录的 plugins/（便携，随发布包分发）；
/// 2. 系统配置目录 tuneux-fx/plugins/（用户自装第三方插件）；
/// 3. 仅 debug 构建：仓库顶层 plugins/（开发时 cargo run 也能找到插件）。
pub fn plugins_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("plugins");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("tuneux-fx");
    path.push("plugins");
    if path.is_dir() {
        return path;
    }
    #[cfg(debug_assertions)]
    {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        if repo.is_dir() {
            return repo;
        }
    }
    path
}

/// 插件加载核查日志文件 `tuneux-fx-plugins.log` 的存储路径（追加式核查记录）。
pub fn plugin_log_path() -> PathBuf {
    portable_path("tuneux-fx-plugins.log")
}

/// 探测目录是否可写：尝试创建并删除一个临时探测文件。
///
/// 返回 true 表示可写。任何 IO 错误（权限不足、只读文件系统、路径不存在）
/// 均视为不可写，返回 false。
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".tuneux_write_probe");
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
        .is_ok();
    // 探测成功后清理临时文件；失败也无妨，下次启动会覆盖
    if writable {
        let _ = std::fs::remove_file(&probe);
    }
    writable
}

/// keymap 合法动作名白名单（与 tui/app/mod.rs 键处理支持的动作一致）。
const KEYMAP_ACTIONS: &[&str] = &["toggle_play", "next", "prev", "volume_up", "volume_down"];

/// 校验并清理 keymap：剔除非法动作名（未知动作 / 空白键描述），
/// 避免用户误配导致快捷键静默失效或与退出键（q/Ctrl+C）冲突。
///
/// 非法映射打警告并剔除；保留键冲突由文档警示。
pub(crate) fn validate_keymap(keymap: &mut HashMap<String, String>) {
    keymap.retain(|action, desc| {
        let action_ok = KEYMAP_ACTIONS.contains(&action.as_str());
        let desc_ok = !desc.trim().is_empty();
        if !action_ok || !desc_ok {
            eprintln!(
                "[配置] 忽略非法快捷键映射：动作 {action}（描述 {desc}）——合法动作：{}",
                KEYMAP_ACTIONS.join(" / ")
            );
            return false;
        }
        true
    });
}

/// 始终返回有效 Config：加载失败时打印警告并回退默认值，
/// 保证程序在任何情况下都能启动。
/// 前代同名产品（tuneux）同名文件的候选路径：exe 同目录 → 系统配置目录。
/// 仅供首次启动继承使用（本产品自身配置文件不存在时）。
fn legacy_tuneux_candidates(filename: &str) -> Vec<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join(filename));
        }
    }
    let mut p = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("tuneux");
    p.push(filename);
    cands.push(p);
    cands
}

/// 从候选路径读取第一个存在且可解析的文件（配置/播放状态共用）。
/// 全部不存在或解析失败返回 None。
fn load_first_existing<T: serde::de::DeserializeOwned>(candidates: &[PathBuf]) -> Option<T> {
    for cand in candidates {
        if !cand.exists() {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(cand) {
            if let Ok(value) = toml::from_str(&text) {
                return Some(value);
            }
        }
    }
    None
}

pub fn load() -> Config {
    let path = config_path();
    let mut cfg = if path.exists() {
        match load_from(&path) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("[配置] 配置解析失败，使用默认值（{}）：{e}", path.display());
                Config::default()
            }
        }
    } else if let Some(inherited) =
        load_first_existing::<Config>(&legacy_tuneux_candidates("tuneux.toml"))
    {
        // 首次启动：继承前代同名产品的配置（字段兼容），免去重新设置。
        eprintln!("[配置] 首次启动，已继承 tuneux 的配置");
        inherited
    } else {
        eprintln!("[配置] 首次启动，使用默认配置（{}）", path.display());
        Config::default()
    };
    // 校验并清理自定义快捷键（剔除非法动作，避免静默失效）
    validate_keymap(&mut cfg.keymap);
    // 浮点字段净化：手写成 nan/inf 时 clamp 失效（NaN 比较全 false），
    // 会导致 0 宽度浏览器不可见 / 音量归零等，回退默认值。
    if !cfg.browser_ratio.is_finite() {
        cfg.browser_ratio = default_browser_ratio();
    }
    if !cfg.lyrics_offset.is_finite() {
        cfg.lyrics_offset = 0.0;
    }
    if !cfg.volume.is_finite() {
        cfg.volume = default_volume();
    }
    cfg
}

/// 从指定路径加载配置（内部接口，便于单元测试）。
///
/// 文件不存在视为非错误：返回默认配置（首次启动的常见情况）。
/// 其他错误（IO 错误、TOML 解析错误）向上传播。
fn load_from(path: &Path) -> ConfigResult<Config> {
    // 文件不存在 → 默认配置（不当作错误，避免首次启动报警告）
    if !path.exists() {
        return Ok(Config::default());
    }
    let text = std::fs::read_to_string(path)?;
    // toml::from_str 解析失败会返回 toml::de::Error，自动装进 Box<dyn Error>
    let cfg: Config = toml::from_str(&text)?;
    Ok(cfg)
}

/// 保存配置到默认路径（对外接口）。
///
/// 保存失败静默吞掉、不向上传播——配置丢失不影响音频播放，不应阻塞
/// 程序退出流程；且运行期定期保存时 eprintln 会弄脏 raw-mode 屏幕。
pub fn save(cfg: &Config) {
    let path = config_path();
    let _ = save_to(&path, cfg);
}

/// 保存配置到指定路径（内部接口，便于单元测试）。
fn save_to(path: &Path, cfg: &Config) -> ConfigResult<()> {
    // 确保父目录存在（回退路径下目录可能尚未创建）
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// 加载播放状态（播放列表 + 每首进度）。
///
/// 文件不存在或解析失败时返回默认（空），绝不 panic，保证程序可启动。
pub fn load_playlist_state() -> PlaylistState {
    let path = playlist_state_path();
    if path.exists() {
        return std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
    }
    // 首次启动：继承前代同名产品的播放状态（播放列表与断点），平滑迁移。
    load_first_existing::<PlaylistState>(&legacy_tuneux_candidates("playlist.toml"))
        .unwrap_or_default()
}

/// 保存播放状态到独立文件 `playlist.toml`。
///
/// 保存失败静默吞掉、不阻塞退出（理由同 [`save`]：运行期打印会弄脏屏幕）。
pub fn save_playlist_state(state: &PlaylistState) {
    let path = playlist_state_path();
    let _ = save_playlist_state_to(&path, state);
}

/// 保存播放状态到指定路径（内部接口，便于单元测试）。
fn save_playlist_state_to(path: &Path, state: &PlaylistState) -> ConfigResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(state)?;
    std::fs::write(path, text)?;
    Ok(())
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 循环模式切换顺序正确：Off → Single → List → Off。
    #[test]
    fn repeat_mode_cycle() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Single);
        assert_eq!(RepeatMode::Single.next(), RepeatMode::List);
        assert_eq!(RepeatMode::List.next(), RepeatMode::Off);
    }

    /// TOML 往返：保存后再加载，字段应完全一致。
    /// 验证序列化/反序列化的对称性。
    #[test]
    fn config_roundtrip() {
        let tmp = std::env::temp_dir().join("tuneux_test_roundtrip.toml");
        // 清理可能残留的旧测试文件
        let _ = std::fs::remove_file(&tmp);

        let original = Config {
            volume: 0.42,
            replay_gain: true,
            repeat: RepeatMode::Single,
            shuffle: true,
            playlist_view: PlaylistView::Flat,
            spectrum_mode: SpectrumMode::Half,
            lyrics_mode: LyricsMode::Visible,
            lyrics_offset: 0.5,
            left_panel: LeftPanel::Browser,
            playback_medium: "tape".to_string(),
            theme: crate::tui::theme::Theme::Dos,
            browser_ratio: 0.55,
            last_dir: Some(PathBuf::from("/tmp/music")),
            dedup_on_add: false,
            keymap: HashMap::from([
                ("toggle_play".to_string(), "space".to_string()),
                ("volume_up".to_string(), "+".to_string()),
            ]),
            media_keys_enabled: true,
        };
        save_to(&tmp, &original).expect("保存应成功");
        let loaded = load_from(&tmp).expect("加载应成功");

        assert!((loaded.volume - 0.42).abs() < 1e-6, "音量应一致");
        assert_eq!(loaded.repeat, RepeatMode::Single, "循环模式应一致");
        assert!(loaded.shuffle, "随机应一致");
        assert!((loaded.browser_ratio - 0.55).abs() < 1e-6, "比例应一致");
        assert_eq!(loaded.last_dir, Some(PathBuf::from("/tmp/music")));
        assert!(!loaded.dedup_on_add, "去重开关应一致");
        assert_eq!(
            loaded.keymap.get("toggle_play").map(String::as_str),
            Some("space")
        );
        assert_eq!(
            loaded.keymap.get("volume_up").map(String::as_str),
            Some("+")
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// 播放状态（playlist.toml）往返：保存后再加载，字段应完全一致。
    #[test]
    fn playlist_state_roundtrip() {
        let tmp = std::env::temp_dir().join("tuneux_test_playlist_state.toml");
        let _ = std::fs::remove_file(&tmp);

        let original = PlaylistState {
            current: Some(PathBuf::from("/tmp/music/a.mp3")),
            current_cue: None,
            items: vec![PlaylistItem {
                path: PathBuf::from("/tmp/music/a.mp3"),
                album: Some("A".to_string()),
                track_number: Some(1),
                cue: None,
            }],
            positions: BTreeMap::from([(PathBuf::from("/tmp/music/a.mp3"), 42.5)]),
            replay_gain: BTreeMap::from([(PathBuf::from("/tmp/music/a.mp3"), -8.5)]),
            bookmarks: tuneux_mediax::bookmark::BookmarkList::default(),
        };
        save_playlist_state_to(&tmp, &original).expect("保存应成功");
        let loaded: PlaylistState =
            toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).expect("解析应成功");

        assert_eq!(
            loaded.current.as_deref(),
            Some(std::path::Path::new("/tmp/music/a.mp3")),
            "当前曲目应保留"
        );
        assert_eq!(loaded.items.len(), 1, "播放列表应保留");
        assert_eq!(loaded.items[0].album.as_deref(), Some("A"));
        assert_eq!(loaded.positions.len(), 1, "进度应保留");
        assert_eq!(
            loaded.positions.get(&PathBuf::from("/tmp/music/a.mp3")),
            Some(&42.5)
        );
        assert_eq!(
            loaded.replay_gain.get(&PathBuf::from("/tmp/music/a.mp3")),
            Some(&-8.5),
            "ReplayGain 缓存应保留"
        );

        let _ = std::fs::remove_file(&tmp);
    }

    /// 加载不存在的文件应返回默认配置（首次启动场景）。
    #[test]
    fn load_missing_returns_default() {
        let missing = std::env::temp_dir().join("tuneux_test_nonexistent_12345.toml");
        let _ = std::fs::remove_file(&missing);
        let cfg = load_from(&missing).expect("缺失文件不应报错");
        assert_eq!(cfg.volume, 1.0, "默认音量应为满音量");
        assert_eq!(cfg.repeat, RepeatMode::Off);
        assert!(!cfg.shuffle);
        assert!((cfg.browser_ratio - 0.4).abs() < 1e-6);
        assert!(cfg.last_dir.is_none());
        assert!(cfg.dedup_on_add, "默认应开启去重");
    }

    /// 前向兼容：旧配置文件缺少新字段时，加载应成功并用默认值填充。
    /// 模拟一个只含 volume 字段的"旧版"配置。
    #[test]
    fn forward_compatibility_partial_config() {
        let tmp = std::env::temp_dir().join("tuneux_test_partial.toml");
        std::fs::write(&tmp, "volume = 0.7\n").expect("写入测试文件");

        let cfg = load_from(&tmp).expect("部分字段配置应能加载");
        assert!((cfg.volume - 0.7).abs() < 1e-6, "已有字段应保留");
        assert_eq!(cfg.repeat, RepeatMode::Off, "缺失字段用默认值");
        assert!(!cfg.shuffle);
        assert!(cfg.last_dir.is_none());
        assert!(cfg.dedup_on_add, "旧配置缺字段时去重应默认为 true");

        let _ = std::fs::remove_file(&tmp);
    }

    /// 首次启动继承：候选路径里存在前代配置时读取成功；全不存在返回 None。
    #[test]
    fn legacy_inheritance_loads_first_existing() {
        let tmp = std::env::temp_dir().join("tuneux_fx_legacy_inherit");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let legacy = tmp.join("tuneux.toml");
        std::fs::write(&legacy, "volume = 0.7\n").unwrap();

        // 跳过不存在的第一个候选，读到第二个
        let cfg: Option<Config> = load_first_existing(&[tmp.join("none.toml"), legacy.clone()]);
        let cfg = cfg.expect("存在且合法的候选应被读到");
        assert!((cfg.volume - 0.7).abs() < 1e-6, "继承的音量应保留");

        // 全不存在 → None
        let none: Option<Config> = load_first_existing(&[tmp.join("a.toml"), tmp.join("b.toml")]);
        assert!(none.is_none(), "无候选存在时应返回 None");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// parse_key_desc 规范化各种合法写法，并拒绝非法输入（未知键名/纯修饰符/空串）。
    #[test]
    fn parse_key_desc_cases() {
        // 合法：单字符 / 具名键 / 修饰符
        assert_eq!(parse_key_desc("n"), Some("n".to_string()));
        assert_eq!(parse_key_desc("space"), Some("space".to_string()));
        assert_eq!(parse_key_desc(" "), Some("space".to_string()));
        assert_eq!(parse_key_desc("shift+n"), Some("shift+n".to_string()));
        assert_eq!(parse_key_desc("Shift+N"), Some("shift+n".to_string()));
        assert_eq!(parse_key_desc("ctrl+c"), Some("ctrl+c".to_string()));
        assert_eq!(
            parse_key_desc("alt + ctrl + q"),
            Some("ctrl+alt+q".to_string())
        );
        assert_eq!(parse_key_desc("+"), Some("+".to_string()));
        assert_eq!(parse_key_desc("-"), Some("-".to_string()));
        assert_eq!(parse_key_desc("Enter"), Some("enter".to_string()));
        assert_eq!(parse_key_desc("return"), Some("enter".to_string()));
        assert_eq!(parse_key_desc("escape"), Some("esc".to_string()));
        // 非法：空串 / 纯修饰符 / 多字符非具名键
        assert_eq!(parse_key_desc(""), None);
        assert_eq!(parse_key_desc("shift+"), None);
        assert_eq!(parse_key_desc("ab"), None);
        assert_eq!(parse_key_desc("f1"), None);
        assert_eq!(parse_key_desc("foo"), None);
    }

    /// RepeatMode 序列化为可读字符串（如 "single"），配置文件对人友好。
    /// 注：TOML 顶层必须是表，不能直接序列化孤立枚举值，需用包装结构。
    #[test]
    fn repeat_mode_serde_readable() {
        #[derive(Serialize, Deserialize)]
        struct Wrap {
            repeat: RepeatMode,
        }
        let s = toml::to_string(&Wrap {
            repeat: RepeatMode::Single,
        })
        .expect("序列化");
        assert!(s.contains("single"), "应为小写字符串");
        let w: Wrap = toml::from_str("repeat = \"list\"\n").expect("反序列化");
        assert_eq!(w.repeat, RepeatMode::List);
    }

    /// save_position：>0.5s 的位置会被记录；≤0.5s 视为"刚开播"，清掉旧记录。
    /// 避免噪声（每首播了 0.1 秒也算 resume）+ 防止"听完一首歌再从头播
    /// 仍显示 0.1s"的怪现象。
    #[test]
    fn save_position_threshold() {
        let mut state = PlaylistState::default();
        let p = PathBuf::from("/music/song.mp3");

        state.save_position(&p, 60.0);
        assert_eq!(state.get_position(&p), Some(60.0));

        state.save_position(&p, 0.1);
        assert_eq!(state.get_position(&p), None, "<0.5s 视为刚开播，清掉记录");

        state.save_position(&p, 1.0);
        assert_eq!(state.get_position(&p), Some(1.0));
    }

    /// get_position：无记录返回 None，不 panic。
    #[test]
    fn get_position_missing_returns_none() {
        let state = PlaylistState::default();
        assert_eq!(state.get_position(Path::new("/nope.mp3")), None);
    }

    // =========================================================================
    // 断点续播（播放进度保存/恢复）补充测试
    // =========================================================================

    /// 基本往返：save_position 保存后 get_position 应取回完全相同的值（含小数部分）。
    #[test]
    fn position_save_get_roundtrip_basic() {
        let mut state = PlaylistState::default();
        let p = Path::new("/music/roundtrip.mp3");
        // 播放中常见的非整秒位置，验证 f64 精度无损
        state.save_position(p, 183.749_213_889);
        assert_eq!(
            state.get_position(p),
            Some(183.749_213_889),
            "保存后应取回相同值"
        );

        // 常规整秒值
        state.save_position(p, 42.0);
        assert_eq!(state.get_position(p), Some(42.0), "整秒值也应无损");
    }

    /// 多曲目各自独立：不同 path 的进度互不干扰，
    /// 修改其中一首不影响另一首的记录。
    #[test]
    fn positions_independent_per_track() {
        let mut state = PlaylistState::default();
        let a = Path::new("/music/a.mp3");
        let b = Path::new("/music/b.flac");
        let c = Path::new("/music/专辑/c.ogg"); // 含非 ASCII 的路径

        state.save_position(a, 10.0);
        state.save_position(b, 20.0);
        state.save_position(c, 30.0);

        // 各取各的，不串位
        assert_eq!(state.get_position(a), Some(10.0));
        assert_eq!(state.get_position(b), Some(20.0));
        assert_eq!(state.get_position(c), Some(30.0));

        // 更新 a 不影响 b/c
        state.save_position(a, 11.0);
        assert_eq!(state.get_position(a), Some(11.0), "a 应被更新");
        assert_eq!(state.get_position(b), Some(20.0), "b 不受影响");
        assert_eq!(state.get_position(c), Some(30.0), "c 不受影响");

        // map 中恰好 3 条记录
        assert_eq!(state.positions.len(), 3);
    }

    /// 覆盖保存：同一 path 保存两次，后值覆盖前值，不产生重复条目。
    #[test]
    fn position_overwrite_last_wins() {
        let mut state = PlaylistState::default();
        let p = Path::new("/music/overwrite.mp3");

        state.save_position(p, 100.0);
        state.save_position(p, 250.5);

        assert_eq!(state.get_position(p), Some(250.5), "后值应覆盖前值");
        assert_eq!(state.positions.len(), 1, "同一路径只应有一条记录");
    }

    /// 序列化往返（多曲目进度）：PlaylistState → TOML → 反序列化，
    /// 每首曲目的进度都应完整保留（数值用近似比较，容许 TOML 文本
    /// 表示引入的极小浮点误差）。
    #[test]
    fn positions_survive_toml_roundtrip() {
        let mut state = PlaylistState::default();
        // 构造多条进度，含小数、较大值、不同扩展名/层级
        let tracks = [
            ("/music/a.mp3", 12.34),
            ("/music/sub/dir/b.flac", 345.678),
            ("/music/长文件名 专辑 (2024)/03. 曲目.ogg", 0.75), // 恰好过阈值
            ("/music/c.wav", 9999.125),
        ];
        for (path, secs) in tracks {
            state.save_position(Path::new(path), secs);
        }
        state.current = Some(PathBuf::from("/music/a.mp3"));

        // 序列化 → 反序列化（纯内存，不落盘，聚焦 serde 对称性）
        let text = toml::to_string_pretty(&state).expect("序列化应成功");
        let loaded: PlaylistState = toml::from_str(&text).expect("反序列化应成功");

        // 进度条数一致
        assert_eq!(loaded.positions.len(), tracks.len(), "进度条数应一致");
        // 每首曲目的进度都在容差内还原
        for (path, secs) in tracks {
            let got = loaded.get_position(Path::new(path));
            assert_eq!(got, Some(secs), "路径 {path} 的进度应完整还原");
        }
        // 当前曲目也应还原
        assert_eq!(
            loaded.current.as_deref(),
            Some(Path::new("/music/a.mp3")),
            "current 应随进度一起还原"
        );
    }

    /// 序列化往返（文件级）：走 save_playlist_state_to / load 路径落盘再读回，
    /// 覆盖真实持久化链路（含文件 IO）。
    #[test]
    fn positions_survive_file_roundtrip() {
        let tmp = std::env::temp_dir().join("tuneux_test_positions_file.toml");
        let _ = std::fs::remove_file(&tmp);

        let mut state = PlaylistState::default();
        state.save_position(Path::new("/music/x.mp3"), 88.8);
        state.save_position(Path::new("/music/y.mp3"), 1.5);

        save_playlist_state_to(&tmp, &state).expect("保存应成功");
        let loaded: PlaylistState =
            toml::from_str(&std::fs::read_to_string(&tmp).unwrap()).expect("解析应成功");

        assert_eq!(loaded.get_position(Path::new("/music/x.mp3")), Some(88.8));
        assert_eq!(loaded.get_position(Path::new("/music/y.mp3")), Some(1.5));

        let _ = std::fs::remove_file(&tmp);
    }

    /// 不存在的 path 返回 None：空状态、有其他记录的状态两种情形。
    /// 同时验证"保存过但被 ≤0.5s 清除"的 path 也回到 None。
    #[test]
    fn get_position_absent_paths_return_none() {
        // 空状态
        let empty = PlaylistState::default();
        assert_eq!(empty.get_position(Path::new("")), None, "空路径");
        assert_eq!(empty.get_position(Path::new("/whatever.mp3")), None);

        // 有记录的状态：未记录的 path 不应误命中
        let mut state = PlaylistState::default();
        state.save_position(Path::new("/music/a.mp3"), 10.0);
        assert_eq!(
            state.get_position(Path::new("/music/a.mp3")),
            Some(10.0),
            "已记录的正常返回"
        );
        // 相似但不同的路径（前缀/后缀差异）不能串
        assert_eq!(state.get_position(Path::new("/music/a.mp3.bak")), None);
        assert_eq!(state.get_position(Path::new("/music")), None);

        // 保存过 60s 后又保存 0.3s（≤0.5s 阈值）→ 记录被清除，回到 None
        state.save_position(Path::new("/music/a.mp3"), 0.3);
        assert_eq!(
            state.get_position(Path::new("/music/a.mp3")),
            None,
            "≤0.5s 的保存应清除旧记录"
        );
    }

    /// 边界值：
    /// - 0.0 / 极小值 / 恰好 0.5：低于阈值，不记录（且会清除既有记录）；
    /// - 恰好略过阈值（0.5+ε）：记录成功；
    /// - 极大值：正常记录且取回无损。
    ///
    /// 注：save_position 的语义是"≤0.5s 视为刚开播不续播"，
    /// 因此 0.0/极小值的预期是 None（而非保存 0.0）。
    #[test]
    fn position_boundary_values() {
        let mut state = PlaylistState::default();
        let p = Path::new("/music/boundary.mp3");

        // —— 0.0：不记录 ——
        state.save_position(p, 0.0);
        assert_eq!(state.get_position(p), None, "0.0 不应被记录");

        // —— 极小正数（f64 最小正规数）：不记录 ——
        state.save_position(p, f64::MIN_POSITIVE);
        assert_eq!(state.get_position(p), None, "极小值不应被记录");

        // —— 恰好 0.5（阈值本身，条件是 secs > 0.5）：不记录 ——
        state.save_position(p, 0.5);
        assert_eq!(state.get_position(p), None, "0.5 不满足 > 0.5，不应被记录");

        // —— 恰好略过阈值：记录成功，且值精确 ——
        state.save_position(p, 0.5 + f64::EPSILON);
        assert_eq!(
            state.get_position(p),
            Some(0.5 + f64::EPSILON),
            "刚过阈值应被精确记录"
        );

        // —— 先有记录再存 0.0：旧记录被清除（防止续播显示 0） ——
        state.save_position(p, 120.0);
        assert_eq!(state.get_position(p), Some(120.0));
        state.save_position(p, 0.0);
        assert_eq!(state.get_position(p), None, "存 0.0 应清除旧记录");

        // —— 极大值：正常记录、无损取回（如 100 小时的进度） ——
        let huge = 360_000.999_999;
        state.save_position(p, huge);
        assert_eq!(state.get_position(p), Some(huge), "极大值应无损记录");

        // —— 负数：同样 ≤0.5，不记录，且会清除既有记录 ——
        state.save_position(p, -5.0);
        assert_eq!(state.get_position(p), None, "负数不应被记录（且清除旧值）");
    }

    /// 边界值的序列化往返：阈值附近的极小进度与极大进度
    /// 落盘再读回后应保持一致（不因 TOML 文本表示丢失）。
    #[test]
    fn position_boundary_survives_toml_roundtrip() {
        let mut state = PlaylistState::default();
        let tiny = Path::new("/music/tiny.mp3");
        let huge = Path::new("/music/huge.mp3");

        state.save_position(tiny, 0.5001); // 刚过阈值的最小可保存值量级
        state.save_position(huge, 9_999_999.999_999);

        let text = toml::to_string_pretty(&state).expect("序列化");
        let loaded: PlaylistState = toml::from_str(&text).expect("反序列化");

        let t = loaded.get_position(tiny).expect("极小进度应保留");
        assert!((t - 0.5001).abs() < 1e-9, "极小进度往返后应一致：{t}");
        let h = loaded.get_position(huge).expect("极大进度应保留");
        assert!(
            (h - 9_999_999.999_999).abs() < 1e-6,
            "极大进度往返后应一致：{h}"
        );
    }
}
