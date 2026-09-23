//! # 应用状态与按键分派
//!
//! 持有 TUI 全部运行时状态：音频引擎、文件浏览器、播放列表、元数据缓存、
//! 配色主题、面板焦点、面板开合（浏览器/封面/歌词/频谱）、搜索、关于弹窗等。
//! 所有键盘输入在此分派。
//!
//! 键位与 tuneux 统一：基础层与 tuneux 完全一致，-fx 只做加法（F 键 / 菜单
//! 等扩展层）。提供面板开合（b/c/v/l）、搜索（/）、关于（?）、清空二次确认
//! （x x）、菜单栏与命令模式。
//!
//! 本目录按领域拆分子模块（每个子模块一个 `impl App` 扩展块）：
//! - [`cue`]：CUE 分轨的列表展开与起播偏移
//! - [`media`]：元数据缓存、歌词加载、封面解码
//! - [`menu`]：菜单栏静态表与介质档标签
//! - [`playback`]：播放控制（engine 交互）
//! - [`search`]：搜索与过滤（含 SearchTarget 枚举）

pub mod cue;
pub mod media;
pub mod menu;
pub mod playback;
pub mod search;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image;
use tuneux_corex as audio;
use tuneux_mediax;

use crate::config::{self, Config, LeftPanel, LyricsMode, PlaylistState, SpectrumMode};
use crate::fs_browser::{self, FsBrowser};
use crate::playlist::{self, Playlist};
use tuneux_mediax::lyrics;
use tuneux_mediax::metadata::TrackMetadata;

use super::theme::Palette;

// 搜索目标重导出：render/layout.rs 依赖 `crate::tui::app::SearchTarget` 路径。
pub use self::search::SearchTarget;

use self::menu::{menus, MenuAction};

/// 后台目录加入的结果消息：目录 + 构建好的条目 + 新探测的元数据
/// （封面字节已剥离，仅缓存未命中的项）。
#[allow(clippy::type_complexity)]
pub type DirAddResult = (
    PathBuf,
    Vec<playlist::PlaylistItem>,
    Vec<(PathBuf, TrackMetadata)>,
    // 本批跳过的数据轨总数（cue 中的 MODE1/MODE2 等不可播轨；供 UI 提示）。
    usize,
);

/// 应用运行时状态。
pub struct App {
    /// 当前生效的调色板（来自皮肤清单选中项；None = 内置默认 DOS 风配色）。
    pub skin: Option<Palette>,
    /// 已加载的第一方皮肤清单（验签 Trusted；名称 + 调色板）。
    pub skins: Vec<LoadedSkin>,
    /// 当前皮肤序号：0 = 终端原生，1..=skins.len() = 清单内皮肤。
    pub skin_sel: usize,
    /// 皮肤选择器弹窗开关。
    pub skin_picker: bool,
    /// 选择器内高亮序号（0 = 终端原生）。
    pub skin_picker_sel: usize,
    /// 打开选择器前的生效皮肤（Esc 取消时恢复）。
    skin_prev: Option<Palette>,
    /// 已装载的可视化面板插件清单（「可视化-*」前缀扫描，验签 Trusted）。
    pub visual_plugins: Vec<tuneux_pinx::LoadedPlugin>,
    /// 插件面板当前画面（每帧由插件 tick 产出；无插件 / 无画面时为空）。
    pub visual_text: String,
    /// 音频引擎（后台解码 + 播放线程）。初始化失败为 None（播放不可用）。
    pub engine: Option<audio::Engine>,
    /// 文件浏览器（左侧面板）。
    pub browser: FsBrowser,
    /// 目录异步载入通道：后台线程读目录后发回 (代次, 目录, 结果)。
    pub dir_load_tx:
        crossbeam_channel::Sender<(u64, PathBuf, Result<Vec<crate::fs_browser::Entry>, String>)>,
    pub dir_load_rx:
        crossbeam_channel::Receiver<(u64, PathBuf, Result<Vec<crate::fs_browser::Entry>, String>)>,
    /// 目录载入代次：每次导航 +1，用于丢弃陈旧结果（快速连按返回上级时不覆盖新目录）。
    pub dir_load_gen: u64,
    /// 浏览器搜索的异步收集：后台线程递归收集目录树后发回 (代次, 条目, 是否截断)。
    pub search_load_tx: crossbeam_channel::Sender<(u64, Vec<crate::fs_browser::Entry>, bool)>,
    /// 浏览器搜索收集接收端。
    pub search_load_rx: crossbeam_channel::Receiver<(u64, Vec<crate::fs_browser::Entry>, bool)>,
    /// 搜索收集代次：每次进入搜索 +1，用于丢弃陈旧收集（重进搜索 / 切目录后）。
    pub search_load_gen: u64,
    /// 目录递归加入的异步构建：后台线程收集 + CUE 展开 + 元数据探测后
    /// 发回 [`DirAddResult`]。加入是累积语义，多批次全部合入
    /// （与导航的「最新生效」不同，不用代次丢弃）。
    pub add_load_tx: crossbeam_channel::Sender<DirAddResult>,
    /// 目录加入接收端。
    pub add_load_rx: crossbeam_channel::Receiver<DirAddResult>,
    /// 播放列表（右侧主面板，多列展示）。
    pub playlist: Playlist,
    /// 播放列表持久化状态（退出/定时落盘，下次启动恢复）。
    pub playlist_state: PlaylistState,
    /// ReplayGain 增益缓存：path → 整曲增益（dB）。首次播放无缓存（增益 1.0），
    /// 播放中引擎测量完成后缓存，下次播放该曲生效。随 playlist.toml 落盘。
    pub replay_gain_cache: std::collections::BTreeMap<PathBuf, f64>,
    /// 焦点面板：浏览器 ↔ 播放列表（Tab 切换）。
    pub focus: playlist::Panel,
    /// 左侧面板状态（`b` 浏览器 / `c` 封面，二者互斥；隐藏时列表占满）。
    pub left_panel: LeftPanel,
    /// 频谱显示模式（`v` 键切换：关 → 半屏 → 全屏 → 示波器 → 插件面板 → 关）。
    pub spectrum_mode: SpectrumMode,
    /// 播放介质风格（`m` 键循环 / 菜单选择：无 → 4 磁带 → 4 黑胶 → 无）。
    /// 只作用于声音（corex DSP 修饰），不改变界面布局。
    pub playback_medium: audio::PlaybackMedium,
    /// ReplayGain 响度归一开关（镜像 config，菜单勾选 / 执行共用）。
    pub replay_gain: bool,
    /// 歌词显示模式（`l` 键切换：隐藏 ↔ 显示，可与频谱共存）。
    pub lyrics_mode: LyricsMode,
    /// 当前曲目的歌词（.lrc 优先，内嵌兜底，无则 None）。
    pub current_lyrics: Option<lyrics::Lyrics>,
    /// 封面图解码缓存：(曲目路径, 已解码图)。切歌清空，同曲复用。
    pub cover_cache: Option<(PathBuf, image::DynamicImage)>,
    /// 封面缩略图缓存：(曲目路径, 目标像素宽, 目标像素高, 实际缩放宽, 实际缩放高, RGBA)。
    /// 命中时跳过每帧 resize（仅在封面或面板尺寸变化时 resize 一次）。
    pub cover_thumb: Option<(PathBuf, u32, u32, u32, u32, image::RgbaImage)>,
    /// 封面解码负缓存：记录解码失败的曲目路径。命中后不再重试、不刷屏，
    /// 切到别的曲目（路径不同）自然失效；回到该曲目仍跳过（避免每帧重解码）。
    pub cover_failed_path: Option<PathBuf>,
    /// 无封面负标记：已确认标签里没有封面的文件路径。命中后切回该曲
    /// 不再重复做整文件标签探测（省一次重复解析）。
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
    /// 连续播放失败计数：列表全损坏时避免自动跳曲无限循环（达到阈值即停止）。
    pub consecutive_failures: u32,
    /// 当前正在播放（或已加载）的文件路径。
    pub current_path: Option<PathBuf>,
    /// 当前曲目元数据（标题/艺术家/时长/技术参数）。
    pub current_metadata: Option<TrackMetadata>,
    /// 元数据缓存（路径 → 元数据），避免重复读盘解析。
    pub metadata_cache: HashMap<PathBuf, TrackMetadata>,
    /// 搜索模式：true 时输入框收字符、键全被吃，不再走全局快捷键。
    pub search_mode: bool,
    /// 搜索关键词（空 = 不过滤）。
    pub search_query: String,
    /// 搜索目标面板（播放列表 / 文件浏览器）。
    pub search_target: SearchTarget,
    /// 命令模式：`:` 唤起，输入命令回车执行（fx 扩展层，供后续插件注册命令）。
    pub command_mode: bool,
    /// 当前命令输入内容。
    pub command_query: String,
    /// 菜单栏是否激活（数字 1-8 / F10 唤起，方向键/Enter 导航）。
    pub menu_active: bool,
    /// 激活的顶级菜单下标（0-7）。
    pub menu_top: usize,
    /// 当前顶级菜单的下拉是否展开。
    pub menu_dropdown: bool,
    /// 展开下拉中的选中项下标。
    pub menu_item: usize,
    /// 引擎最近一次错误（短暂显示后自动清除）。
    pub last_error: Option<String>,
    /// 错误产生时刻（用于到期清除）。
    pub last_error_at: Option<Instant>,
    /// 渲染帧计数（每帧 +1）：供电平/频谱的乱码字符随帧变化。
    pub frame_tick: u64,
    /// 频谱峰值保持状态机（频段维度，0.0-1.0），用于"峰值保持白帽"：
    /// 绿柱实时跟随能量，白帽从峰值缓慢下落。用 RefCell 内部可变，便于渲染侧持 `&App` 更新。
    pub spectrum_peaks: std::cell::RefCell<audio::spectrum::SpectrumPeakHold>,
    /// "关于"弹窗是否可见（`?` 键切换，任意键关闭）。
    pub about_visible: bool,
    /// 均衡器面板是否可见（「工具 › 均衡器」/ F9 打开，Esc 关闭）。
    pub eq_visible: bool,
    /// 均衡器当前选中的段（0-9，↑/↓ 调增益、←/→ 切段）。
    pub eq_band_sel: usize,
    /// 已加载的第一方均衡器插件（持有其 WASM 实例与槽位共享）。
    pub eq_plugin: Option<tuneux_pinx::LoadedPlugin>,
    /// 第一方均衡器占用的槽位号（v2 动态多槽位）。
    pub eq_slot: u32,
    /// 压缩器面板是否可见（「工具 › 压缩器」打开，Esc 关闭）。
    pub comp_visible: bool,
    /// 压缩器当前选中参数（0=阈值 1=压缩比 2=启动 3=释放 4=补偿）。
    pub comp_param_sel: usize,
    /// 已加载的第一方压缩器插件。
    pub comp_plugin: Option<tuneux_pinx::LoadedPlugin>,
    /// 第一方压缩器占用的槽位号。
    pub comp_slot: u32,
    /// 清空播放列表的确认状态：true 时再按 x 才真正清空（防误触）。
    pub pending_clear: bool,
    /// 清空确认的发起时刻：与提示同寿命（5 秒过期），防止"提示已消失、
    /// 几分钟后随手按 x 仍无提示清空"。
    pub pending_clear_at: Option<Instant>,
    /// 书签跳转 CUE 分轨的瞬时偏移（秒，相对分轨头）：`jump_bookmark` 设置，
    /// `play_and_update_current` 的 CUE 分支消费（用后即清）；平时为 `None`。
    pub pending_cue_offset: Option<f64>,
}

/// 官方（第一方）插件签名公钥（Ed25519，32 字节）：宿主内置的信任根。
///
/// 随二进制分发、运行时不可替换；用它验证随包分发的 equalizer.sig /
/// compressor.sig——验签命中 → Trusted，无 .sig / 验签失败 → Unsigned（降级）。
/// 对应私钥只存仓库外本地文件，绝不入库、绝不写进二进制。
const OFFICIAL_PUBKEY: [u8; 32] = [
    0x9a, 0xc3, 0x5b, 0x76, 0xa9, 0xaf, 0xbe, 0x5f, 0xfc, 0xab, 0x74, 0x35, 0xeb, 0xaf, 0xb2, 0x83,
    0x46, 0x56, 0x55, 0xda, 0x84, 0x58, 0xd9, 0xec, 0x3f, 0x4d, 0xa3, 0xb8, 0x4e, 0xb8, 0xcb, 0x29,
];

/// 把 KeyEvent 翻译为规范化键描述（与基础版 keys.rs 同源）。
///
/// 空值 = 不可自定义的键（F 键 / 方向外的功能键 / 修饰键本体）。
/// 两侧（事件侧与配置侧）都经同一规范化再比较，容忍大小写、别名、
/// 修饰符顺序等写法差异。
fn key_to_desc(key: &KeyEvent) -> Option<String> {
    // 先解出基础键名；空值 = 不可自定义的键
    let base = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => {
            // 字母 + SHIFT：统一小写，交给下方 shift+ 前缀（屏蔽终端大小写差异）
            if key.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_alphabetic() {
                c.to_ascii_lowercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Tab => "tab".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        _ => return None,
    };

    // 前缀修饰符；符号键的 SHIFT 是"打出符号"本身，不计入前缀
    let is_alpha = matches!(key.code, KeyCode::Char(c) if c.is_ascii_alphabetic());
    let mut desc = String::new();
    if is_alpha && key.modifiers.contains(KeyModifiers::SHIFT) {
        desc.push_str("shift+");
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        desc.push_str("ctrl+");
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        desc.push_str("alt+");
    }
    desc.push_str(&base);
    Some(desc)
}
/// 把按键事件翻译成自定义动作名：`KeyEvent → 键描述 → 反查 keymap`。
///
/// 两侧（事件侧与配置侧）都经 `parse_key_desc` 规范化后再比较，
/// 容忍大小写、别名、修饰符顺序等写法差异。找不到返回 None。
fn keycode_to_action<'a>(
    key: &KeyEvent,
    keymap: &'a std::collections::HashMap<String, String>,
) -> Option<&'a str> {
    let desc = key_to_desc(key)?;
    keymap.iter().find_map(|(action, configured)| {
        if crate::config::parse_key_desc(configured).as_deref() == Some(desc.as_str()) {
            Some(action.as_str())
        } else {
            None
        }
    })
}

/// 追加一条插件加载核查记录（首见判定：日志里是否已出现过该 **log_id**）。
/// 日志失败不阻断插件加载（核查日志是卫生措施，非功能门槛）。
///
/// `log_id` 必须**逐插件唯一**，且与插件的**签名 id 无关**：
/// - 签名 id 参与 `id ‖ wasm 字节` 的验签消息，改动会让既有 `.sig` 全部失配，
///   所以皮肤 / 可视化这类"一个 id 带多个插件"的族不能靠改签名 id 来区分；
/// - 而核查日志是按 id 做子串匹配判首见的（`plugin={id} `），同一 id 下的
///   第 2 个及以后的插件恒被判"已见过"，first_seen 语义失真。
///   故此处传日志键（如 `tuneux-vis:可视化-能量条`），签名侧仍用族 id。
fn record_plugin_load(
    log_id: &str,
    tristate: tuneux_pinx::Tristate,
    granted: Vec<tuneux_pinx::Capability>,
) {
    let journal = tuneux_pinx::journal::Journal::new(config::plugin_log_path());
    let seen = std::fs::read_to_string(journal.path())
        .map(|content| {
            content
                .lines()
                .any(|line| line.contains(&format!("plugin={log_id} ")))
        })
        .unwrap_or(false);
    let record =
        tuneux_pinx::journal::LoadRecord::now(log_id.to_string(), tristate, granted, !seen);
    let _ = journal.append(&record);
}

/// 装载一个第一方插件的前置（从插件目录读 .wasm/.manifest/.sig）：
/// 解析清单 → 官方验签 → 宿主装载 → Trusted 校验，任一步失败返回 None。
///
/// `allowed` 为宿主侧允许集（求交授予）：`None` 取申请集自身（效果器插件，
/// 清单声明即授予）；皮肤插件传固定窄集（纵深防御——清单被篡改多声明
/// 能力也拿不到）。返回 (已装载插件, 实际授予的能力集)，授予集供调用方
/// 写核查日志。init 由调用方按各自语义触发（效果器携槽位号，皮肤固定 0）。
fn load_first_party_core(
    engine: &audio::Engine,
    name: &str,
    id: &str,
    allowed: Option<&[tuneux_pinx::Capability]>,
) -> Option<(tuneux_pinx::LoadedPlugin, Vec<tuneux_pinx::Capability>)> {
    let dir = config::plugins_dir();
    let wasm = std::fs::read(dir.join(format!("{name}.wasm"))).ok()?;
    let manifest = std::fs::read_to_string(dir.join(format!("{name}.manifest"))).ok()?;
    // 官方签名 .sig（64 字节，随包分发）：验签证明「第一方 + 未被篡改」。
    let sig: [u8; 64] = std::fs::read(dir.join(format!("{name}.sig")))
        .ok()?
        .try_into()
        .ok()?;
    // MV3 式能力清单：manifest 文件声明能力（音频效果器参数写入 / 皮肤供色）。
    let requested = tuneux_pinx::parse_manifest(&manifest).ok()?;
    // 允许集缺省取申请集；授予 = 申请 ∩ 允许（保序，与 pinx 仲裁同口径）。
    let allowed = allowed.unwrap_or(&requested);
    let granted: Vec<tuneux_pinx::Capability> = requested
        .iter()
        .filter(|c| allowed.contains(c))
        .copied()
        .collect();
    let host = tuneux_pinx::WasmHost::new(100_000, 4, engine.eq_slots(), engine.compressor_slots());
    // 第一方插件：官方公钥验签（Trusted 才加载），申请集 = manifest 声明。
    let trust = tuneux_pinx::TrustList::from_parts(vec![OFFICIAL_PUBKEY]);
    let plugin = host
        .load(
            &wasm,
            id,
            Some((&OFFICIAL_PUBKEY, &sig)),
            &trust,
            &requested,
            allowed,
        )
        .ok()?;
    // 第一方插件必须验签为 Trusted，否则按加载失败处理（防 .sig 缺失/被篡改）。
    if plugin.tristate != tuneux_pinx::Tristate::Trusted {
        return None;
    }
    Some((plugin, granted))
}

/// 加载一个第一方插件（效果器形态）：装载验签通过后分配槽位并 init。
/// alloc 从对应槽位池分配一个槽位（返回槽位号）。任一文件缺失 / 编译 /
/// 实例化 / init 失败返回 None（面板据此提示）。
/// 返回 (槽位号, 已装载插件, 授予的能力集)——能力集供调用方写核查日志。
/// 槽位在装载验签通过后才分配：文件缺失 / 验签失败不再占用槽位
///（旧实现在读文件后即分配，失败路径会漏占一个槽）。
fn load_first_party_plugin(
    engine: &audio::Engine,
    name: &str,
    id: &str,
    alloc: impl FnOnce(&audio::Engine) -> Option<u32>,
) -> Option<(u32, tuneux_pinx::LoadedPlugin, Vec<tuneux_pinx::Capability>)> {
    let (mut plugin, granted) = load_first_party_core(engine, name, id, None)?;
    // 分配槽位（v2 动态多槽位）；插件 init 携带 slot id 写参数。
    let slot = alloc(engine)?;
    plugin.call_init(slot).ok()?;
    Some((slot, plugin, granted))
}

/// 加载第一方均衡器插件（共享引擎的均衡器槽位）。
fn load_eq_plugin(
    engine: &audio::Engine,
) -> Option<(u32, tuneux_pinx::LoadedPlugin, Vec<tuneux_pinx::Capability>)> {
    load_first_party_plugin(engine, "equalizer", "tuneux-eq", |e| {
        e.alloc_eq_slot().map(|(s, _)| s)
    })
}

/// 加载第一方压缩器插件（共享引擎的压缩器槽位）。
fn load_comp_plugin(
    engine: &audio::Engine,
) -> Option<(u32, tuneux_pinx::LoadedPlugin, Vec<tuneux_pinx::Capability>)> {
    load_first_party_plugin(engine, "compressor", "tuneux-comp", |e| {
        e.alloc_compressor_slot().map(|(s, _)| s)
    })
}

/// 一份已加载的皮肤（名称 + 调色板）。
pub struct LoadedSkin {
    /// 皮肤名（皮肤文本 `name=` 键；缺省用文件名去前缀）。
    pub name: String,
    /// 解析出的调色板。
    pub palette: Palette,
}

/// 加载全部第一方可视化面板插件（能力 `meter_read`）：扫描 plugins/ 下
///「可视化-*」前缀的全部 .wasm，逐份验签（仅 Trusted 入清单）、init。
/// 与皮肤同口径：文件缺失 / 验签非 Trusted / 解析失败 → 跳过（记核查日志）。
/// 可视化插件不占效果器槽位（init 传 0）；tick 为可选导出。
fn load_visual_plugins(engine: &audio::Engine) -> Vec<tuneux_pinx::LoadedPlugin> {
    let allowed = [tuneux_pinx::Capability::MeterRead];
    let dir = config::plugins_dir();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let stem = p.file_stem()?.to_str()?.to_owned();
                    (p.extension()?.to_str()? == "wasm" && stem.starts_with("可视化-"))
                        .then_some(stem)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut out = Vec::new();
    for file_stem in names {
        let Some((mut plugin, granted)) =
            load_first_party_core(engine, &file_stem, "tuneux-vis", Some(&allowed))
        else {
            continue;
        };
        if plugin.call_init(0).is_err() {
            continue;
        }
        // 日志键带文件名（签名 id 仍是 "tuneux-vis"，见 record_plugin_load 说明）。
        record_plugin_load(&format!("tuneux-vis:{file_stem}"), plugin.tristate, granted);
        out.push(plugin);
    }
    out
}

/// 加载全部第一方皮肤插件（能力 `theme`）：扫描 plugins/ 下「皮肤-*」
/// 前缀的全部 .wasm，逐份验签（仅 Trusted 入清单）、init 后经
/// theme_register 取皮肤文本解析为调色板（插件只供色）。
///
/// 能力面固定收紧为 `[theme]`——manifest 多声明的能力一律不授予（纵深
/// 防御，与效果器插件的「清单声明即授予」口径不同）。任一文件缺失 /
/// 验签非 Trusted / 解析失败 → 跳过该份（记核查日志），不阻断启动。
/// 清单按皮肤名排序；无皮肤文件时返回空清单（终端原生配色）。
fn load_skin_palettes(engine: &audio::Engine) -> Vec<LoadedSkin> {
    let allowed = [tuneux_pinx::Capability::Theme];
    let dir = config::plugins_dir();
    // 收集「皮肤-*.wasm」文件名（排序保证清单顺序稳定）。
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let stem = p.file_stem()?.to_str()?.to_owned();
                    (p.extension()?.to_str()? == "wasm" && stem.starts_with("皮肤-"))
                        .then_some(stem)
                })
                .collect()
        })
        .unwrap_or_default();
    names.sort();

    let mut out: Vec<LoadedSkin> = Vec::new();
    for file_stem in names {
        let Some((mut plugin, granted)) =
            load_first_party_core(engine, &file_stem, "tuneux-skin", Some(&allowed))
        else {
            continue;
        };
        if plugin.call_init(0).is_err() {
            continue;
        }
        // 同可视化插件：日志键带文件名，签名 id 保持 "tuneux-skin"。
        record_plugin_load(
            &format!("tuneux-skin:{file_stem}"),
            plugin.tristate,
            granted,
        );
        let Some(text) = plugin.theme().and_then(|b| std::str::from_utf8(b).ok()) else {
            continue;
        };
        let Some(palette) = Palette::from_skin(text) else {
            continue;
        };
        // 皮肤名：文本 name= 键优先，文件名去「皮肤-」前缀兜底。
        let fallback = file_stem.trim_start_matches("皮肤-").to_string();
        let name = crate::tui::theme::skin_name(text).unwrap_or(fallback);
        out.push(LoadedSkin { name, palette });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

impl App {
    /// 新建应用状态：启动引擎、打开浏览器（上次目录）、恢复播放列表。
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
        let engine_init_error_at = engine_init_error.as_ref().map(|_| Instant::now());
        // 启动即下发介质风格，避免配置与声音不一致（介质纯声音修饰、无画面）。
        // 一并下发 ReplayGain 开关（默认关）。
        if let Some(e) = &engine {
            e.set_medium(medium);
            e.set_replay_gain_enabled(config.replay_gain);
        }
        // 加载第一方均衡器插件（WASM 控制面，共享引擎的均衡器槽位）。
        // 失败仅静默——均衡器面板会显示"插件未加载"。
        let (eq_slot, eq_plugin) = engine
            .as_ref()
            .and_then(load_eq_plugin)
            .map(|(s, p, caps)| {
                record_plugin_load("tuneux-eq", p.tristate, caps);
                (Some(s), Some(p))
            })
            .unwrap_or((None, None));
        let (comp_slot, comp_plugin) = engine
            .as_ref()
            .and_then(load_comp_plugin)
            .map(|(s, p, caps)| {
                record_plugin_load("tuneux-comp", p.tristate, caps);
                (Some(s), Some(p))
            })
            .unwrap_or((None, None));
        // 加载全部第一方皮肤插件（能力 theme）：plugins/ 下「皮肤-*」前缀扫描。
        // 加载全部第一方可视化面板插件（能力 meter_read）：「可视化-*」前缀扫描。
        let visual_plugins = engine.as_ref().map(load_visual_plugins).unwrap_or_default();
        let skins = engine.as_ref().map(load_skin_palettes).unwrap_or_default();
        // 皮肤选择：0 = 内置默认（DOS 风），1 = 终端原生，2.. = 皮肤清单。
        let skin_sel = match config.skin.as_str() {
            "" => 0,
            "终端原生" => 1,
            name => skins
                .iter()
                .position(|s| s.name == name)
                .map(|i| i + 2)
                .unwrap_or(0),
        };
        let skin = match skin_sel {
            0 => None,
            1 => Some(Palette::terminal()),
            i => skins.get(i - 2).map(|s| s.palette),
        };
        // 恢复播放列表持久化状态（独立文件）。
        let playlist_state = config::load_playlist_state();

        let mut playlist = Playlist::new();
        playlist.set_shuffle(config.shuffle);
        playlist.set_view(config.playlist_view);

        // 恢复上次播放列表并预提取元数据（多列展示立即正确）。
        // 失效路径（文件已删除/移动）自动跳过。
        let mut metadata_cache = HashMap::new();
        for item in &playlist_state.items {
            if !item.path.exists() {
                continue;
            }
            playlist.add(item.clone());
            if !metadata_cache.contains_key(&item.path) {
                let mut md = TrackMetadata::from_file(&item.path);
                // 剥离封面原始字节（与热路径 media.rs 同口径）：from_file 会
                // 填入内嵌封面或同目录约定图片——不剥离则同专辑每首各持一份
                // 相同封面副本（2000 首 × 1MB cover.jpg ≈ 2 GB 重复字节）。
                md.cover = None;
                metadata_cache.insert(item.path.clone(), md);
            }
        }

        // 自动选中上次播放的歌曲（高亮落在它上，用户按 Enter 才播放）。
        // CUE 优先按 path+分轨号定位，找不到再退回仅 path。
        if let Some(current_path) = &playlist_state.current {
            let cue_want = playlist_state.current_cue;
            let exact = playlist.items().iter().position(|it| {
                &it.path == current_path && it.cue.as_ref().map(|c| c.index) == cue_want
            });
            let fallback = playlist
                .items()
                .iter()
                .position(|it| &it.path == current_path);
            if let Some(index) = exact.or(fallback) {
                playlist.set_selected(index);
            }
        }

        // ReplayGain 缓存：从持久化状态载入上次会话的测量结果。
        let replay_gain_cache = playlist_state.replay_gain.clone();

        // 目录异步载入通道。
        let (dir_load_tx, dir_load_rx) = crossbeam_channel::unbounded();
        // 浏览器搜索收集与目录加入的后台通道（同机制，见对应字段说明）。
        let (search_load_tx, search_load_rx) = crossbeam_channel::unbounded();
        let (add_load_tx, add_load_rx) = crossbeam_channel::unbounded();

        Self {
            skin,
            skins,
            skin_sel,
            skin_picker: false,
            skin_picker_sel: 0,
            skin_prev: None,
            visual_plugins,
            visual_text: String::new(),
            engine,
            browser: FsBrowser::open(&initial),
            dir_load_tx,
            dir_load_rx,
            dir_load_gen: 0,
            search_load_tx,
            search_load_rx,
            search_load_gen: 0,
            add_load_tx,
            add_load_rx,
            playlist,
            playlist_state,
            replay_gain_cache,
            // 浏览器可见时启动焦点落在浏览器（与 toggle_browser_panel 打开时
            // 置焦点的口径一致）；隐藏/封面时焦点在播放列表。
            focus: if config.left_panel == LeftPanel::Browser {
                playlist::Panel::Browser
            } else {
                playlist::Panel::Playlist
            },
            left_panel: config.left_panel,
            spectrum_mode: config.spectrum_mode,
            playback_medium: medium,
            replay_gain: config.replay_gain,
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
            consecutive_failures: 0,
            current_path: None,
            current_metadata: None,
            metadata_cache,
            search_mode: false,
            search_query: String::new(),
            search_target: SearchTarget::Playlist,
            command_mode: false,
            command_query: String::new(),
            menu_active: false,
            menu_top: 0,
            menu_dropdown: false,
            menu_item: 0,
            last_error: engine_init_error,
            last_error_at: engine_init_error_at,
            frame_tick: 0,
            spectrum_peaks: std::cell::RefCell::new(audio::spectrum::SpectrumPeakHold::default()),
            about_visible: false,
            eq_visible: false,
            eq_band_sel: 0,
            eq_plugin,
            eq_slot: eq_slot.unwrap_or(0),
            comp_visible: false,
            comp_param_sel: 0,
            comp_plugin,
            comp_slot: comp_slot.unwrap_or(0),
            pending_clear: false,
            pending_clear_at: None,
            pending_cue_offset: None,
        }
    }

    /// 把当前播放列表与当前曲目写回持久化状态（退出/定时调用）。
    pub fn save_playlist_to_state(&mut self) {
        self.playlist_state.items = self.playlist.items().to_vec();
        self.playlist_state.current = self.current_path.clone();
        // 当前曲的 CUE 分轨号：恢复时定位上次播放的分轨。
        self.playlist_state.current_cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.as_ref())
            .map(|c| c.index);
        // 增益缓存同步回持久化状态（随播放状态文件落盘）。修剪规则：
        // 保留当前列表曲目；不在列表但文件仍在磁盘的也保留（测量代价高，
        // 重新加入无需重测）；文件已不存在的清掉，防无界增长。
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

    /// 执行一个自定义动作，返回 true 表示按键已被消费。
    ///
    /// 未知动作名返回 false，调用方据此回退到硬编码默认键。
    pub(crate) fn execute_action(&mut self, action: &str, config: &mut Config) -> bool {
        match action {
            // toggle_play：与硬编码空格键分支等价
            "toggle_play" => {
                self.toggle_play();
                true
            }
            // next / prev：复用既有切曲函数（与硬编码 n / p 同路径）
            "next" => {
                self.advance_to_next_track(config);
                true
            }
            "prev" => {
                self.advance_to_prev_track(config);
                true
            }
            // volume_up / volume_down：与硬编码 + / - 分支等价
            "volume_up" => {
                self.volume_up();
                true
            }
            "volume_down" => {
                self.volume_down();
                true
            }
            // 未知动作名：忽略，回退默认键
            _ => false,
        }
    }

    /// 处理一个按键事件；返回 false 表示请求退出。
    ///
    /// 基础层键位与 tuneux 一致；面板开合（b/c/v/l）、搜索（/）、
    /// 关于（?）、清空二次确认（x x）承 tuneux。扩展层为菜单栏与命令模式。
    pub fn handle_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        // Ctrl+C 退出（最优先，唯一被响应的带修饰字符键）。
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }

        // —— 关于弹窗（最优先：任意键关闭并吃掉，避免误触发其它操作）——
        if self.about_visible {
            self.about_visible = false;
            return true;
        }

        // —— 皮肤选择器（↑/↓ 即时预览、Enter 确认并持久化、Esc 取消恢复）——
        if self.skin_picker {
            let count = self.skins.len() + 2; // +2 = 内置默认 / 终端原生
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.skin_picker_sel = self.skin_picker_sel.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.skin_picker_sel + 1 < count {
                        self.skin_picker_sel += 1;
                    }
                }
                KeyCode::Enter => {
                    self.skin_sel = self.skin_picker_sel;
                    config.skin = match self.skin_sel {
                        0 => String::new(),
                        1 => "终端原生".to_string(),
                        i => self.skins[i - 2].name.clone(),
                    };
                    config::save(config);
                    self.skin_prev = None;
                    self.skin_picker = false;
                }
                KeyCode::Esc => {
                    self.skin = self.skin_prev.take();
                    self.skin_picker = false;
                }
                _ => {}
            }
            // 即时预览：弹窗仍开时，高亮项即为生效调色板。
            if self.skin_picker {
                self.skin = match self.skin_picker_sel {
                    0 => None,
                    1 => Some(Palette::terminal()),
                    i => self.skins.get(i - 2).map(|s| s.palette),
                };
            }
            return true;
        }

        // —— 均衡器面板（↑/↓ 调增益、←/→ 切段、e 旁路、Esc 关闭）——
        if self.eq_visible {
            return self.handle_eq_key(key);
        }

        // —— 压缩器面板（↑/↓ 调参数、←/→ 切参数、e 旁路、Esc 关闭）——
        if self.comp_visible {
            return self.handle_comp_key(key);
        }

        // 清空确认：5 秒过期（与提示同寿命），或按非 x 键取消。
        if self.pending_clear {
            let expired = self
                .pending_clear_at
                .is_some_and(|t| t.elapsed() >= Duration::from_secs(5));
            if expired || !matches!(key.code, KeyCode::Char('x')) {
                self.pending_clear = false;
                self.pending_clear_at = None;
            }
        }

        // —— 搜索模式（吃掉所有按键，按目标面板分派）——
        if self.search_mode {
            return self.handle_search_key(key, config);
        }

        // —— 命令模式（吃掉所有按键，输入命令）——
        if self.command_mode {
            return self.handle_command_key(key, config);
        }

        // —— 菜单栏激活态：方向键/Enter/Esc 导航，吃掉所有按键 ——
        if self.menu_active {
            return self.handle_menu_key(key, config);
        }

        // —— 封面浏览模式：方向键/Enter/n/p 导航，其余键（含 c 切换）走全局 ——
        if self.left_panel == LeftPanel::CoverBrowser && self.handle_cover_browser_key(key, config)
        {
            return true;
        }

        // —— 菜单唤起：F10 打开第一栏（F10 不在 keymap 可配置键内，恒保留）——
        if key.code == KeyCode::F(10) {
            self.menu_active = true;
            self.menu_top = 0;
            self.menu_dropdown = false;
            return true;
        }

        // —— 自定义键映射层（配置驱动，优先于下方硬编码默认键与数字菜单）——
        // KeyEvent → 键描述 → 反查 keymap：命中且为已知动作则执行并返回；
        // 未命中或动作未知则回退到下方默认键逻辑。
        // 置于数字菜单唤起之前：用户把数字键映射为动作时，配置必须生效
        //（修复：此前数字 1-8 被菜单拦截先于本层，数字键映射永不生效且无警告）。
        if let Some(action) = keycode_to_action(&key, &config.keymap) {
            // 先复制出动作名，断开对 config 的不可变借用，便于下方可变借用
            let action = action.to_string();
            if self.execute_action(&action, config) {
                return true;
            }
            // 未知动作名：忽略该映射，继续走默认键
        }

        // —— 菜单唤起：数字 1-8 直接打开对应栏下拉 ——
        // （macOS 的 F 键被系统占用需按 Fn，数字键三平台直接可用）
        // 与其他字符键同规则：带 CTRL/ALT 修饰不响应（Alt+数字在部分终端
        // 是 ESC 序列；Ctrl+数字不应触发界面动作）。
        if let KeyCode::Char(c) = key.code {
            if c.is_ascii_digit()
                && c != '0'
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                let idx = (c as u8 - b'1') as usize;
                if idx < menus().len() {
                    self.menu_active = true;
                    self.menu_top = idx;
                    self.menu_dropdown = true;
                    self.menu_item = 0;
                    return true;
                }
            }
        }

        // 其余字符键要求无 CTRL/ALT 修饰：避免 Ctrl+Q / Alt+N 等误触
        //（显式配置的映射已在上方反查层放行）。
        if matches!(key.code, KeyCode::Char(_))
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return true;
        }

        // —— 全局键（不受焦点影响）——
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return false,
            KeyCode::Tab => {
                // Tab 在已开启的可导航面板间切换：浏览器仅在左侧面板为
                // Browser 时可聚焦；歌词/频谱/封面为展示面板不可聚焦。
                self.focus = match self.focus {
                    playlist::Panel::Browser => playlist::Panel::Playlist,
                    playlist::Panel::Playlist => {
                        if self.left_panel == LeftPanel::Browser {
                            playlist::Panel::Browser
                        } else {
                            playlist::Panel::Playlist
                        }
                    }
                };
                return true;
            }
            KeyCode::Char('r') => {
                config.repeat = config.repeat.next();
                self.refresh_preload(config);
                return true;
            }
            KeyCode::Char('s') => {
                config.shuffle = !config.shuffle;
                self.playlist.set_shuffle(config.shuffle);
                self.refresh_preload(config);
                return true;
            }
            KeyCode::Char('n') => {
                self.advance_to_next_track(config);
                return true;
            }
            KeyCode::Char('p') => {
                self.advance_to_prev_track(config);
                return true;
            }
            KeyCode::Char(' ') => {
                self.toggle_play();
                return true;
            }
            // ←/→：快退/快进 5 秒（全局）
            KeyCode::Left => {
                self.seek_by(-5.0);
                return true;
            }
            KeyCode::Right => {
                self.seek_by(5.0);
                return true;
            }
            // +/-：音量增减（全局）
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.volume_up();
                return true;
            }
            // 歌词偏移：`[` 提前、`]` 延后（每次 0.5 秒）。
            KeyCode::Char('[') => {
                config.lyrics_offset -= 0.5;
                self.flash_message(&format!("歌词偏移 {:.1}s", config.lyrics_offset));
                return true;
            }
            KeyCode::Char(']') => {
                config.lyrics_offset += 0.5;
                self.flash_message(&format!("歌词偏移 {:.1}s", config.lyrics_offset));
                return true;
            }
            KeyCode::Char('-') => {
                self.volume_down();
                return true;
            }
            KeyCode::Char('a') => {
                self.add_current_browser_to_playlist(config);
                return true;
            }
            KeyCode::Char('b') => {
                self.toggle_browser_panel(config);
                return true;
            }
            KeyCode::Char('v') => {
                self.toggle_spectrum_panel(config);
                return true;
            }
            KeyCode::Char('l') => {
                self.toggle_lyrics_panel(config);
                return true;
            }
            KeyCode::Char('m') => {
                // m：循环播放介质风格（无 → 4 磁带 → 4 黑胶 → 无），只改声音，
                // 用 corex 的 ALL 单一真源，新增介质无需改此处。
                let all = audio::PlaybackMedium::ALL;
                let idx = all
                    .iter()
                    .position(|&m| m == self.playback_medium)
                    .unwrap_or(0);
                self.apply_medium(all[(idx + 1) % all.len()], config);
                return true;
            }
            KeyCode::Char('x') => {
                // x：清空播放列表（两次确认，防误触）。
                if self.pending_clear {
                    self.clear_playlist();
                    self.pending_clear = false;
                    self.pending_clear_at = None;
                    self.flash_message("播放列表已清空");
                } else if !self.playlist.is_empty() {
                    self.pending_clear = true;
                    self.pending_clear_at = Some(Instant::now());
                    self.flash_message("再按一次 x 确认清空播放列表");
                }
                return true;
            }
            KeyCode::Char('d') => {
                self.remove_selected_track();
                return true;
            }
            KeyCode::Char('g') => {
                self.playlist.toggle_view();
                config.playlist_view = self.playlist.view();
                return true;
            }
            KeyCode::Char('c') => {
                self.toggle_cover_panel(config);
                return true;
            }
            KeyCode::Char('?') => {
                self.about_visible = !self.about_visible;
                return true;
            }
            // —— fx 扩展层：命令模式与功能键（只加法，不占基础层）——
            KeyCode::Char(':') => {
                self.command_mode = true;
                self.command_query.clear();
                return true;
            }
            // F5-F8：面板开合兜底（对应 b/c/l/v）。F10 菜单见菜单分支。
            KeyCode::F(5) => {
                self.toggle_browser_panel(config);
                return true;
            }
            KeyCode::F(6) => {
                self.toggle_cover_panel(config);
                return true;
            }
            KeyCode::F(7) => {
                self.toggle_lyrics_panel(config);
                return true;
            }
            KeyCode::F(8) => {
                self.toggle_spectrum_panel(config);
                return true;
            }
            KeyCode::F(9) => {
                self.eq_visible = !self.eq_visible;
                return true;
            }
            _ => {}
        }

        // —— 焦点相关键 ——
        match self.focus {
            playlist::Panel::Browser => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.browser.move_up(),
                KeyCode::Down | KeyCode::Char('j') => self.browser.move_down(),
                KeyCode::Home => self.browser.move_to_top(),
                KeyCode::End => self.browser.move_to_bottom(),
                KeyCode::Char('/') => {
                    self.search_mode = true;
                    self.search_target = SearchTarget::Browser;
                    self.search_query.clear();
                    // 进入搜索态后异步收集目录树（大目录按 `/` 不卡 UI），
                    // 收集期间维持一级列表，结果到达后关键字立即生效。
                    self.browser.begin_search();
                    self.search_async();
                }
                KeyCode::Enter => self.handle_browser_enter(config),
                KeyCode::Backspace => {
                    // 异步返回上级目录。
                    if let Some(parent) = self.browser.cwd().parent() {
                        let parent = parent.to_path_buf();
                        self.navigate_async(&parent);
                    }
                }
                _ => {}
            },
            playlist::Panel::Playlist => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.playlist.move_selection_up(),
                KeyCode::Down | KeyCode::Char('j') => self.playlist.move_selection_down(),
                KeyCode::Home => self.playlist.move_selection_to_top(),
                KeyCode::End => self.playlist.move_selection_to_bottom(),
                KeyCode::Char('/') => {
                    self.search_mode = true;
                    self.search_target = SearchTarget::Playlist;
                    self.search_query.clear();
                    self.jump_selected_to_filter_first();
                }
                // 返回浏览器——仅浏览器可见时（与 Tab / 基础版同款守卫）：
                // 左面板隐藏/封面时不响应，避免焦点落进不可见面板。
                KeyCode::Backspace if self.left_panel == LeftPanel::Browser => {
                    self.focus = playlist::Panel::Browser;
                }
                KeyCode::Enter => match self.playlist.selected().cloned() {
                    // 选中专辑组头：折叠/展开该专辑（ByAlbum 视图）。
                    Some(playlist::Selection::Album(album)) => {
                        self.playlist.toggle_album(&album);
                    }
                    Some(playlist::Selection::Track(sel)) => {
                        self.playlist.jump_to(sel);
                        self.play_and_update_current(sel, config);
                    }
                    None => {
                        // 无选中但列表非空：播第一首（与基础版同口径——
                        // 无选中按 Enter 的用户意图就是开始播，不留死键）。
                        if !self.playlist.is_empty() {
                            self.playlist.jump_to(0);
                            self.play_and_update_current(0, config);
                        }
                    }
                },
                _ => {}
            },
        }
        true
    }

    /// 搜索模式按键分派（吃字符、上下导航、Esc 退出、Enter 确认）。
    fn handle_search_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.search_mode = false;
                self.search_query.clear();
                // 退出浏览器搜索时恢复一级条目。
                if self.search_target == SearchTarget::Browser {
                    self.browser.end_search();
                }
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.apply_search_query();
            }
            // 搜索态导航只用 ↑/↓：j/k 让位给输入（否则含 j/k 的歌名——
            // 周杰伦、JJ、K 开头英文歌——永远敲不出来）。
            KeyCode::Up => self.search_nav(-1),
            KeyCode::Down => self.search_nav(1),
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.apply_search_query();
            }
            KeyCode::Enter => match self.search_target {
                SearchTarget::Playlist => {
                    if let Some(sel) = self.playlist.selected_track() {
                        if self.filter_playlist().contains(&sel) {
                            self.playlist.jump_to(sel);
                            self.play_and_update_current(sel, config);
                        }
                    }
                }
                SearchTarget::Browser => {
                    self.handle_browser_enter(config);
                }
            },
            _ => {}
        }
        true
    }

    /// 短暂提示（走 last_error 通道，5 秒后自动清除）。
    pub(crate) fn flash_message(&mut self, msg: &str) {
        self.last_error = Some(msg.to_string());
        self.last_error_at = Some(Instant::now());
    }

    /// b / F5：Hidden ↔ Browser（互斥：开浏览器会关掉封面）。
    fn toggle_browser_panel(&mut self, config: &mut Config) {
        if self.left_panel == LeftPanel::Browser {
            self.left_panel = LeftPanel::Hidden;
            self.focus = playlist::Panel::Playlist;
        } else {
            self.left_panel = LeftPanel::Browser;
            self.focus = playlist::Panel::Browser;
        }
        config.left_panel = self.left_panel;
    }

    /// c / F6：Hidden → Cover → CoverBrowser → Hidden（互斥：开封面会关掉浏览器）。
    /// 封面/封面浏览不可聚焦（焦点始终在播放列表）。
    fn toggle_cover_panel(&mut self, config: &mut Config) {
        self.left_panel = match self.left_panel {
            LeftPanel::Cover => LeftPanel::CoverBrowser,
            LeftPanel::CoverBrowser => LeftPanel::Hidden,
            // Hidden 或 Browser → Cover
            _ => LeftPanel::Cover,
        };
        if self.left_panel != LeftPanel::Hidden {
            self.focus = playlist::Panel::Playlist;
        }
        config.left_panel = self.left_panel;
    }

    /// 封面浏览模式按键：Up/Down/j/k 移动、Enter 播放、PageUp/PageDown 翻页；
    /// 其余键（含 c 切换、n/p 切曲、Left/Right seek、l 歌词）返回 false 走全局，避免冲突。
    fn handle_cover_browser_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        let albums = self.cover_browser_albums();
        let count = albums.len();
        if count == 0 {
            // 空列表无可导航：不吞键，让 c 仍能关闭封面面板。
            return false;
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cover_browser_sel = self.cover_browser_sel.saturating_sub(1);
                self.cover_browser_scroll_into_view(count);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cover_browser_sel = (self.cover_browser_sel + 1).min(count - 1);
                self.cover_browser_scroll_into_view(count);
            }
            KeyCode::Enter => {
                if let Some((_, path)) = albums.get(self.cover_browser_sel) {
                    let path = path.clone();
                    if let Some(idx) = self.playlist.items().iter().position(|it| it.path == path) {
                        self.playlist.jump_to(idx);
                        self.play_and_update_current(idx, config);
                    }
                }
            }
            KeyCode::PageDown => {
                let step = self.cover_browser_visible.max(1);
                self.cover_browser_scroll = (self.cover_browser_scroll + step).min(count - 1);
                self.cover_browser_sel = self.cover_browser_scroll;
            }
            KeyCode::PageUp => {
                let step = self.cover_browser_visible.max(1);
                self.cover_browser_scroll = self.cover_browser_scroll.saturating_sub(step);
                self.cover_browser_sel = self.cover_browser_scroll;
            }
            _ => return false,
        }
        true
    }

    /// 让选中项滚入可视窗：选中在 scroll 之前则回退 scroll，超出窗口末尾则前推 scroll。
    fn cover_browser_scroll_into_view(&mut self, count: usize) {
        let visible = self.cover_browser_visible.max(1);
        if self.cover_browser_sel < self.cover_browser_scroll {
            self.cover_browser_scroll = self.cover_browser_sel;
        }
        if self.cover_browser_sel >= self.cover_browser_scroll + visible {
            self.cover_browser_scroll = self.cover_browser_sel + 1 - visible;
        }
        self.cover_browser_scroll = self.cover_browser_scroll.min(count - 1);
    }
    /// l / F7：切换歌词（显示/隐藏），可与频谱共存。
    fn toggle_lyrics_panel(&mut self, config: &mut Config) {
        self.lyrics_mode = self.lyrics_mode.next();
        config.lyrics_mode = self.lyrics_mode;
    }

    /// v / F8：循环频谱模式（关 → 半屏 → 全屏 → 示波器 → 插件面板 → 关）。
    fn toggle_spectrum_panel(&mut self, config: &mut Config) {
        self.spectrum_mode = self.spectrum_mode.next();
        config.spectrum_mode = self.spectrum_mode;
    }

    /// 命令模式按键分派（吃字符、Backspace 删、Esc 取消、Enter 执行）。
    fn handle_command_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.command_mode = false;
                self.command_query.clear();
            }
            KeyCode::Backspace => {
                self.command_query.pop();
            }
            KeyCode::Char(c) => {
                self.command_query.push(c);
            }
            KeyCode::Enter => {
                let cmd = std::mem::take(&mut self.command_query);
                self.command_mode = false;
                self.execute_command(&cmd, config);
            }
            _ => {}
        }
        true
    }

    /// 执行一条命令（首版内置少量命令，框架供后续插件注册扩展）。
    ///
    /// 支持：`quit` 退出；`volume <0-100>` 设音量；
    /// `repeat <off|list|single>` 循环模式；`save-m3u`/`load-m3u [path]` 保存/载入
    /// m3u 播放列表；`help` 打开关于/帮助。
    /// 未知命令走 last_error 提示。
    fn execute_command(&mut self, raw: &str, config: &mut Config) {
        let line = raw.trim();
        if line.is_empty() {
            return;
        }
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("").to_lowercase();
        match cmd.as_str() {
            "quit" | "q" | "exit" => {
                // 退出交由主循环：这里用标志位不合适，直接提示用 q 键。
                self.flash_message("退出请按 q 或 Ctrl+C");
            }
            "volume" | "vol" => {
                // is_finite 校验："nan"/"inf" 能 parse 成功但 clamp 对 NaN 失效，
                // 会把音量归零并每帧落盘。
                if let Some(v) = parts
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .filter(|v| v.is_finite())
                {
                    let v = v.clamp(0.0, 100.0) / 100.0;
                    if let Some(engine) = &self.engine {
                        engine.send(audio::AudioCmd::SetVolume(v));
                    }
                    self.flash_message(&format!("音量已设为 {}%", (v * 100.0).round() as u32));
                } else {
                    self.flash_message("用法：volume <0-100>");
                }
            }
            "repeat" => match parts.next().map(|s| s.to_lowercase()).as_deref() {
                Some("off") => {
                    config.repeat = crate::config::RepeatMode::Off;
                    self.refresh_preload(config);
                    self.flash_message("循环：关闭");
                }
                Some("list") | Some("all") => {
                    config.repeat = crate::config::RepeatMode::List;
                    self.refresh_preload(config);
                    self.flash_message("循环：列表");
                }
                Some("single") | Some("one") => {
                    config.repeat = crate::config::RepeatMode::Single;
                    self.refresh_preload(config);
                    self.flash_message("循环：单曲");
                }
                _ => self.flash_message("用法：repeat <off|list|single>"),
            },
            "save-m3u" | "m3u-save" => {
                let path = self.browser.cwd().join("playlist.m3u");
                match self.save_m3u(&path) {
                    Ok(n) => self.flash_message(&format!("已保存 {n} 首到 {}", path.display())),
                    Err(e) => self.flash_message(&format!("保存失败：{e}")),
                }
            }
            "load-m3u" | "m3u-load" => {
                // 可选路径参数；缺省用浏览器当前目录下的 playlist.m3u。
                let path = match parts.next() {
                    Some(p) => PathBuf::from(p),
                    None => self.browser.cwd().join("playlist.m3u"),
                };
                match self.load_m3u(&path, config) {
                    Ok(n) => self.flash_message(&format!("已载入 {n} 首")),
                    Err(e) => self.flash_message(&format!("载入失败：{e}")),
                }
            }
            "bookmark" | "bm" => self.add_bookmark(),
            "bookmarks" | "bm-list" => self.list_bookmarks(),
            "bookmark-jump" | "bm-jump" => {
                if let Some(n) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
                    self.jump_bookmark(n, config);
                } else {
                    self.flash_message("用法：bookmark-jump <序号>");
                }
            }
            "bookmark-del" | "bm-del" => {
                if let Some(n) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
                    self.remove_bookmark(n);
                } else {
                    self.flash_message("用法：bookmark-del <序号>");
                }
            }
            "help" | "about" => {
                self.about_visible = true;
            }
            _ => self.flash_message(&format!("未知命令：{cmd}（输入 help 查看）")),
        }
    }

    /// 落地介质选择（菜单 SetMedium 与 m 键循环共用）：
    /// 设置字段、引擎下发、配置持久化（介质只改声音，不占界面）。
    fn apply_medium(&mut self, medium: audio::PlaybackMedium, config: &mut Config) {
        self.playback_medium = medium;
        if let Some(engine) = &self.engine {
            engine.set_medium(medium);
        }
        config.playback_medium = medium.as_str().to_string();
        // 界面不再显示介质，切换后提示当前档位（声音变化本身不易一眼看出）。
        self.flash_message(&format!("介质：{}", menu::medium_menu_label(medium)));
    }

    /// 菜单栏激活态按键分派：←/→ 切菜单、↑/↓ 选项、Enter 执行、Esc 收起/退出。
    /// 返回 false 表示请求退出（选中"文件→退出"）。
    fn handle_menu_key(&mut self, key: KeyEvent, config: &mut Config) -> bool {
        let ms = menus();
        match key.code {
            // F10 再按一次关闭菜单（与 Esc 等价），避免"只能 Esc 退出"。
            KeyCode::Esc | KeyCode::F(10) => {
                if self.menu_dropdown {
                    self.menu_dropdown = false; // 收起下拉，仍停在菜单栏
                } else {
                    self.menu_active = false; // 退出菜单栏
                }
            }
            // 数字 1-N 直接跳到对应栏并展开下拉（与菜单栏数字一一对应）；
            // 与主分派同规则：带 CTRL/ALT 修饰不响应。
            KeyCode::Char(c)
                if c.is_ascii_digit()
                    && c != '0'
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let idx = (c as u8 - b'1') as usize;
                if idx < ms.len() {
                    self.menu_top = idx;
                    self.menu_dropdown = true;
                    self.menu_item = 0;
                }
            }
            KeyCode::Left => {
                self.menu_top = (self.menu_top + ms.len() - 1) % ms.len();
                if self.menu_dropdown {
                    self.menu_item = 0;
                }
            }
            KeyCode::Right => {
                self.menu_top = (self.menu_top + 1) % ms.len();
                if self.menu_dropdown {
                    self.menu_item = 0;
                }
            }
            KeyCode::Down => {
                if !self.menu_dropdown {
                    self.menu_dropdown = true;
                    self.menu_item = 0;
                } else {
                    let n = ms[self.menu_top].items.len();
                    if n > 0 {
                        self.menu_item = (self.menu_item + 1) % n;
                    }
                }
            }
            KeyCode::Up => {
                if self.menu_dropdown {
                    let n = ms[self.menu_top].items.len();
                    if n > 0 {
                        self.menu_item = (self.menu_item + n - 1) % n;
                    }
                }
            }
            KeyCode::Enter => {
                // F10 唤起菜单栏但未展开下拉时，Enter 不应执行任何动作
                //（menu_item 初始为 0 且 F10/←→ 不复位——旧缺陷：F10 → Enter
                // 直接盲执行第 0 项，而文件菜单第 0 项就是「退出」）。
                if !self.menu_dropdown {
                    self.menu_dropdown = true;
                    self.menu_item = 0;
                    return true;
                }
                let item = ms[self.menu_top].items.get(self.menu_item).cloned();
                self.menu_active = false;
                self.menu_dropdown = false;
                match item {
                    Some(mi) if mi.enabled => {
                        if matches!(mi.action, MenuAction::Quit) {
                            return false; // 文件→退出：请求退出
                        }
                        self.execute_menu_action(mi.action, config);
                    }
                    Some(_) => self.flash_message("该功能暂未提供（随插件/后续版本开放）"),
                    None => {}
                }
            }
            _ => {}
        }
        true
    }

    /// 执行菜单动作（"退出"在 handle_menu_key 内直接返回退出，不在此）。
    fn execute_menu_action(&mut self, action: MenuAction, config: &mut Config) {
        match action {
            MenuAction::TogglePlay => self.toggle_play(),
            MenuAction::PrevTrack => self.advance_to_prev_track(config),
            MenuAction::NextTrack => self.advance_to_next_track(config),
            MenuAction::CycleRepeat => {
                config.repeat = config.repeat.next();
                self.refresh_preload(config);
            }
            MenuAction::ToggleShuffle => {
                config.shuffle = !config.shuffle;
                self.playlist.set_shuffle(config.shuffle);
                self.refresh_preload(config);
            }
            MenuAction::VolumeUp => self.volume_up(),
            MenuAction::VolumeDown => self.volume_down(),
            MenuAction::ToggleBrowser => self.toggle_browser_panel(config),
            MenuAction::ToggleCover => self.toggle_cover_panel(config),
            MenuAction::ToggleLyrics => self.toggle_lyrics_panel(config),
            MenuAction::ToggleSpectrum => self.toggle_spectrum_panel(config),
            MenuAction::TogglePlaylistView => {
                self.playlist.toggle_view();
                config.playlist_view = self.playlist.view();
            }
            MenuAction::About => {
                self.about_visible = true;
            }
            // 输出设备：corex 已自动跟随系统默认输出（无需手动），这里给出说明。
            MenuAction::OutputDevice => self.flash_message(
                "输出设备：自动跟随系统默认输出——插拔耳机/连断蓝牙约 2 秒内自动切换续播；手动指定设备暂未提供",
            ),
            // ReplayGain：响度归一开关（默认关），切换后立即下发引擎。
            MenuAction::ReplayGain => {
                config.replay_gain = !config.replay_gain;
                self.replay_gain = config.replay_gain;
                if let Some(e) = &self.engine {
                    e.set_replay_gain_enabled(config.replay_gain);
                }
                let state = if config.replay_gain { "已开启" } else { "已关闭" };
                self.flash_message(&format!("ReplayGain 响度归一：{state}"));
            }
            // 皮肤配色：打开选择器（↑/↓ 即时预览、Enter 确认并持久化、Esc 取消恢复）。
            MenuAction::SkinSelect => {
                self.skin_prev = self.skin;
                self.skin_picker_sel = self.skin_sel;
                self.skin_picker = true;
            }
            // 均衡器：切换面板（Esc 关闭）。插件加载失败时面板内提示。
            MenuAction::Equalizer => self.eq_visible = !self.eq_visible,
            // 压缩器：切换面板（Esc 关闭）。
            MenuAction::Compressor => self.comp_visible = !self.comp_visible,
            // 插件菜单的清单项：同样打开对应面板（√ 已表示加载态）。
            MenuAction::PluginEq => self.eq_visible = !self.eq_visible,
            MenuAction::PluginComp => self.comp_visible = !self.comp_visible,
            // 可视化面板：直接切到频谱 v 循环的「插件」态；无可视化插件时提示。
            MenuAction::PluginVisual => {
                if self.visual_plugins.is_empty() {
                    self.flash_message("无可视化插件（plugins/ 下放「可视化-*」插件并重启）");
                } else {
                    self.spectrum_mode = crate::config::SpectrumMode::Plugin;
                    config.spectrum_mode = self.spectrum_mode;
                }
            }
            // 介质：菜单显式选择（与 m 键循环同一落地函数）。
            MenuAction::SetMedium(m) => self.apply_medium(m, config),
            // 未实现功能的兜底（enabled=false 已在上方拦截，此为保险）。
            _ => self.flash_message("该功能暂未提供（随插件/后续版本开放）"),
        }
    }

    /// 均衡器面板按键：↑/↓ 调当前段 ±1dB，←/→ 切段，e 旁路，r 恢复默认，
    /// u 卸载/加载（插入↔拔出），Esc 关闭；其它键忽略（不关闭面板，避免误触）。
    /// 未加载时只响应 Esc（关闭）与 u（加载），其余写参数键无效（避免写已释放槽位）。
    fn handle_eq_key(&mut self, key: KeyEvent) -> bool {
        if self.eq_plugin.is_none() && !matches!(key.code, KeyCode::Esc | KeyCode::Char('u')) {
            return true;
        }
        match key.code {
            KeyCode::Esc => self.eq_visible = false,
            KeyCode::Up => self.adjust_eq_band(1.0),
            KeyCode::Down => self.adjust_eq_band(-1.0),
            KeyCode::Left => self.eq_band_sel = self.eq_band_sel.saturating_sub(1),
            KeyCode::Right => self.eq_band_sel = (self.eq_band_sel + 1).min(audio::EQ_BANDS - 1),
            KeyCode::Char('e') => self.toggle_eq_enabled(),
            KeyCode::Char('r') => self.reset_eq(),
            KeyCode::Char('u') => self.toggle_eq_loaded(),
            _ => {}
        }
        true
    }

    /// 均衡器恢复默认：10 段增益归零（直通）。
    fn reset_eq(&mut self) {
        if let Some(e) = &self.engine {
            for i in 0..audio::EQ_BANDS {
                e.set_eq_band(self.eq_slot, i as u32, 0.0);
            }
        }
    }

    /// 调整当前选中段增益（±1dB 步进，钳制 ±12），写入 corex 槽位。
    fn adjust_eq_band(&mut self, delta: f32) {
        if let Some(e) = &self.engine {
            let current = e.eq_slots()[self.eq_slot as usize].band(self.eq_band_sel);
            let new = (current + delta).clamp(audio::EQ_GAIN_MIN_DB, audio::EQ_GAIN_MAX_DB);
            e.set_eq_band(self.eq_slot, self.eq_band_sel as u32, new);
        }
    }

    /// 切换均衡器旁路（enabled 开关）。
    fn toggle_eq_enabled(&mut self) {
        if let Some(e) = &self.engine {
            let on = !e.eq_slots()[self.eq_slot as usize].enabled();
            e.set_eq_enabled(self.eq_slot, on);
        }
    }

    /// 卸载/加载均衡器插件（u 键，插入↔拔出）：已加载则释放槽位并丢弃实例
    /// （释放会把参数重置为默认）；未加载则重新分配槽位并 init(slot)。
    /// 面板保持打开，就地提示当前状态。
    fn toggle_eq_loaded(&mut self) {
        if self.eq_plugin.is_some() {
            if let Some(e) = &self.engine {
                e.free_eq_slot(self.eq_slot);
            }
            self.eq_plugin = None;
            return;
        }
        let Some((slot, plugin, caps)) = self.engine.as_ref().and_then(load_eq_plugin) else {
            return;
        };
        record_plugin_load("tuneux-eq", plugin.tristate, caps);
        self.eq_slot = slot;
        self.eq_plugin = Some(plugin);
    }

    /// 压缩器面板按键：↑/↓ 调当前参数、←/→ 切参数、e 旁路、r 恢复默认，
    /// u 卸载/加载（插入↔拔出）、Esc 关闭。
    /// 未加载时只响应 Esc（关闭）与 u（加载），其余写参数键无效（避免写已释放槽位）。
    fn handle_comp_key(&mut self, key: KeyEvent) -> bool {
        if self.comp_plugin.is_none() && !matches!(key.code, KeyCode::Esc | KeyCode::Char('u')) {
            return true;
        }
        match key.code {
            KeyCode::Esc => self.comp_visible = false,
            KeyCode::Up => self.adjust_comp_param(true),
            KeyCode::Down => self.adjust_comp_param(false),
            KeyCode::Left => self.comp_param_sel = self.comp_param_sel.saturating_sub(1),
            KeyCode::Right => self.comp_param_sel = (self.comp_param_sel + 1).min(4),
            KeyCode::Char('e') => self.toggle_comp_enabled(),
            KeyCode::Char('r') => self.reset_comp(),
            KeyCode::Char('u') => self.toggle_comp_loaded(),
            _ => {}
        }
        true
    }

    /// 压缩器恢复默认：阈值 -20dB、压缩比 4:1、启动 10ms、释放 100ms、补偿 0。
    fn reset_comp(&mut self) {
        if let Some(e) = &self.engine {
            let p = e.compressor_slots()[self.comp_slot as usize].clone();
            p.set_threshold(-20.0);
            p.set_ratio(4.0);
            p.set_attack_ms(10.0);
            p.set_release_ms(100.0);
            p.set_makeup(0.0);
        }
    }

    /// 调整当前选中压缩器参数（true=+，false=-；各参数不同步进）。
    fn adjust_comp_param(&mut self, up: bool) {
        let Some(e) = &self.engine else {
            return;
        };
        let p = e.compressor_slots()[self.comp_slot as usize].clone();
        let step = match self.comp_param_sel {
            0 => 1.0,  // 阈值 dB
            1 => 0.5,  // 压缩比
            2 => 5.0,  // 启动 ms
            3 => 10.0, // 释放 ms
            _ => 1.0,  // 补偿 dB
        };
        let sign = if up { 1.0 } else { -1.0 };
        // setter 返回"是否接受"：只有非有限值（NaN/Inf）会被拒返回 false，
        // 有限值一律接受返回 true（越界只是钳到边界，仍算接受）。此处步进的
        // 基准值取自既有参数（恒有限），加减一个有限步长后仍有限，
        // 故返回值必然为 true，无需检查。
        match self.comp_param_sel {
            0 => p.set_threshold(p.threshold() + sign * step),
            1 => p.set_ratio(p.ratio() + sign * step),
            2 => p.set_attack_ms(p.attack_ms() + sign * step),
            3 => p.set_release_ms(p.release_ms() + sign * step),
            _ => p.set_makeup(p.makeup() + sign * step),
        };
    }

    /// 切换压缩器旁路。
    fn toggle_comp_enabled(&mut self) {
        if let Some(e) = &self.engine {
            let on = !e.compressor_slots()[self.comp_slot as usize].enabled();
            e.compressor_slots()[self.comp_slot as usize].set_enabled(on);
        }
    }

    /// 卸载/加载压缩器插件（u 键，插入↔拔出）：同均衡器，释放槽位即重置默认。
    fn toggle_comp_loaded(&mut self) {
        if self.comp_plugin.is_some() {
            if let Some(e) = &self.engine {
                e.free_compressor_slot(self.comp_slot);
            }
            self.comp_plugin = None;
            return;
        }
        let Some((slot, plugin, caps)) = self.engine.as_ref().and_then(load_comp_plugin) else {
            return;
        };
        record_plugin_load("tuneux-comp", plugin.tristate, caps);
        self.comp_slot = slot;
        self.comp_plugin = Some(plugin);
    }

    /// "a" 键：把浏览器当前选中条目（文件或整个目录）加入播放列表。
    ///
    /// 列表原本为空时，加入后自动播放第一首。
    fn add_current_browser_to_playlist(&mut self, config: &mut Config) {
        let Some(entry) = self.browser.current() else {
            return;
        };
        match entry {
            fs_browser::Entry::File { path, .. } => {
                let was_empty = self.playlist.is_empty();
                let path = path.clone();
                // .cue 文件：按 FILE 引用展开分轨（cue+bin / cue+flac 镜像场景）。
                if fs_browser::is_cue_file(&path) {
                    match cue::cue_items_from_cue_file(&path) {
                        Some((items, skipped, _)) => {
                            if skipped > 0 {
                                self.flash_message(&format!(
                                    "已跳过 {skipped} 条数据轨（不可播放）"
                                ));
                            }
                            if config.dedup_on_add {
                                self.playlist.add_many_dedup(items);
                            } else {
                                self.playlist.add_many(items);
                            }
                        }
                        None => {
                            self.flash_message("cue 解析失败或 FILE 引用的音频文件不存在");
                            return;
                        }
                    }
                    return;
                }
                // 整轨 + 同名 .cue → 展开为多首 CUE 曲目；否则按普通文件加入。
                let (expanded, skipped) = self.cue_items_for(&path);
                if skipped > 0 {
                    self.flash_message(&format!("已跳过 {skipped} 条数据轨（不可播放）"));
                }
                if !expanded.is_empty() {
                    // CUE 分轨按 (path, cue.index) 判定唯一性。
                    if config.dedup_on_add {
                        self.playlist.add_many_dedup(expanded);
                    } else {
                        self.playlist.add_many(expanded);
                    }
                } else {
                    let md = self.get_or_extract_metadata(&path);
                    let item = playlist::PlaylistItem {
                        path,
                        album: md.album,
                        track_number: md.track_number,
                        cue: None,
                    };
                    if config.dedup_on_add {
                        self.playlist.add_dedup(item);
                    } else {
                        self.playlist.add(item);
                    }
                }
                if was_empty && !self.playlist.is_empty() {
                    // 加入前列表为空：播放/选中"专辑-曲序"排序后的第一首
                    // （目录分支为异步加入，首播在 apply_dir_add 合入时触发）。
                    let first = self.playlist.display_order().first().copied().unwrap_or(0);
                    self.playlist.set_selected(first);
                    self.play_and_update_current(first, config);
                }
            }
            fs_browser::Entry::Dir { path, .. } => {
                // 异步递归加入：目录遍历、CUE 展开、逐文件元数据探测全在
                // 后台线程（重活不卡 UI），结果由主循环合入。
                let path = path.clone();
                self.add_dir_async(path);
            }
        }
    }

    /// 异步递归收集目录并构建播放列表条目（`a` 键加目录用）：
    /// 后台线程完成目录遍历、CUE 展开与逐文件元数据探测（重活全在后台，
    /// 元数据缓存优先——与同步路径同口径），结果由主循环轮询后经
    /// [`App::apply_dir_add`] 批量合入。大目录加入不再卡 UI。
    fn add_dir_async(&mut self, dir: PathBuf) {
        // 快照当前元数据缓存给后台线程（命中免探测）；只读不写，无竞争。
        let cache = self.metadata_cache.clone();
        let tx = self.add_load_tx.clone();
        self.flash_message(&format!("正在扫描目录：{}", dir.display()));
        // 优雅降级：线程启动失败（极罕见）时提示而非 panic。
        if std::thread::Builder::new()
            .name("fx-dir-add".to_string())
            .spawn(move || {
                let mut paths = Vec::new();
                FsBrowser::collect_music_recursive(&dir, &mut paths);
                let (mut items, mds, skipped) = cue::build_items_for_paths(&paths, &cache);
                // 按"专辑-曲序"预排序：让新增批次的插入序 = 显示序，
                // 添加目录后自动播放/选中的第一首就是专辑-曲序的第一首。
                Playlist::sort_items(&mut items);
                let _ = tx.send((dir, items, mds, skipped));
            })
            .is_err()
        {
            self.flash_message("目录加入线程启动失败");
        }
    }

    /// 合入一次后台目录加入的结果（主循环轮询用）：
    /// 新探测的元数据补入缓存、条目批量加入（按配置去重）、
    /// 加入前列表为空时自动播放第一首、完成提示。
    pub(crate) fn apply_dir_add(
        &mut self,
        dir: PathBuf,
        items: Vec<playlist::PlaylistItem>,
        mds: Vec<(PathBuf, TrackMetadata)>,
        skipped_data_tracks: usize,
        config: &Config,
    ) {
        if skipped_data_tracks > 0 {
            self.flash_message(&format!(
                "已跳过 {skipped_data_tracks} 条数据轨（不可播放）"
            ));
        }
        let was_empty = self.playlist.is_empty();
        let before = self.playlist.len();
        let empty_dir = items.is_empty();
        for (path, md) in mds {
            // 只补缺失项：不覆盖已有缓存（含当前曲目等热条目）。
            self.metadata_cache.entry(path).or_insert(md);
        }
        if config.dedup_on_add {
            self.playlist.add_many_dedup(items);
        } else {
            self.playlist.add_many(items);
        }
        let added = self.playlist.len() - before;
        if empty_dir || self.playlist.is_empty() {
            self.flash_message(&format!("目录中无音乐文件：{}", dir.display()));
            return;
        }
        if added == 0 {
            self.flash_message(&format!("未加入新曲目（均已存在）：{}", dir.display()));
            return;
        }
        if was_empty {
            // 加入前列表为空：播放/选中"专辑-曲序"排序后的第一首。
            let first = self.playlist.display_order().first().copied().unwrap_or(0);
            self.playlist.set_selected(first);
            self.play_and_update_current(first, config);
        }
        self.flash_message(&format!("已加入 {added} 首：{}", dir.display()));
    }

    /// 异步收集当前目录树（浏览器搜索 `/` 用）：后台线程递归收集，
    /// 结果由主循环轮询后经浏览器的 apply_search_collected 提交；
    /// 收集期间浏览器维持一级列表，已输入的关键字在结果到达后生效。
    fn search_async(&mut self) {
        let cwd = self.browser.cwd().to_path_buf();
        let gen = self.search_load_gen;
        self.search_load_gen = self.search_load_gen.wrapping_add(1);
        let tx = self.search_load_tx.clone();
        // 优雅降级：线程启动失败（极罕见）时提示而非 panic。
        if std::thread::Builder::new()
            .name("fx-search-load".to_string())
            .spawn(move || {
                let (entries, truncated) = crate::fs_browser::collect_recursive_entries(&cwd);
                let _ = tx.send((gen, entries, truncated));
            })
            .is_err()
        {
            self.flash_message("搜索收集线程启动失败");
        }
    }

    /// 把一批文件路径加入播放列表（整轨 + 同名 .cue 展开分轨，其余按普通文件）。
    fn add_files_to_playlist(&mut self, paths: &[std::path::PathBuf], config: &mut Config) {
        let mut items: Vec<playlist::PlaylistItem> = Vec::new();
        let mut skipped_total = 0usize;
        for p in paths {
            let (expanded, skipped) = self.cue_items_for(p);
            skipped_total += skipped;
            if !expanded.is_empty() {
                items.extend(expanded);
            } else {
                let md = self.get_or_extract_metadata(p);
                items.push(playlist::PlaylistItem {
                    path: p.clone(),
                    album: md.album,
                    track_number: md.track_number,
                    cue: None,
                });
            }
        }
        if skipped_total > 0 {
            self.flash_message(&format!("已跳过 {skipped_total} 条数据轨（不可播放）"));
        }
        if config.dedup_on_add {
            self.playlist.add_many_dedup(items);
        } else {
            self.playlist.add_many(items);
        }
    }

    /// 收集封面浏览的专辑列表：去重的 (专辑名, 代表曲目路径)。
    ///
    /// 代表曲目 = 该专辑在播放列表中的第一首（用于取封面 + Enter 播放）。
    pub(crate) fn cover_browser_albums(&self) -> Vec<(String, std::path::PathBuf)> {
        // 去重键 = 专辑名 + 父目录：同名不同专辑（如多张 "Greatest Hits"）
        // 不误合并；同目录同名仍归并。用 HashSet 使去重 O(n)——原为 O(n²)
        // 线性扫描，大列表下每帧/每键各调用一次会卡顿。
        let mut seen: std::collections::HashSet<(String, Option<std::path::PathBuf>)> =
            std::collections::HashSet::new();
        let mut albums: Vec<(String, std::path::PathBuf)> = Vec::new();
        for item in self.playlist.items() {
            let album = item.album.clone().unwrap_or_else(|| "未知专辑".to_string());
            let dir = item.path.parent().map(|p| p.to_path_buf());
            if seen.insert((album.clone(), dir)) {
                albums.push((album, item.path.clone()));
            }
        }
        albums
    }
    /// 保存当前播放列表为 m3u（每个唯一整轨文件路径一行）。返回写入的曲目数。
    fn save_m3u(&self, path: &std::path::Path) -> Result<usize, String> {
        let mut paths: Vec<std::path::PathBuf> = Vec::new();
        for item in self.playlist.items() {
            if !paths.contains(&item.path) {
                paths.push(item.path.clone());
            }
        }
        let content = tuneux_mediax::m3u::serialize(&paths);
        std::fs::write(path, content).map_err(|e| e.to_string())?;
        Ok(paths.len())
    }

    /// 从 m3u 加载路径加入播放列表。返回实际加入的条目数
    ///（去重开启时可能小于路径数——提示按实际加入数显示）。
    fn load_m3u(&mut self, path: &std::path::Path, config: &mut Config) -> Result<usize, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let paths = tuneux_mediax::m3u::parse(&content);
        if paths.is_empty() {
            return Err("m3u 中无有效路径".to_string());
        }
        // 相对路径按 m3u 文件所在目录解析（外部播放器导出的 m3u 主流写法）。
        let base = path.parent().unwrap_or(std::path::Path::new(""));
        let resolved: Vec<std::path::PathBuf> = paths
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    base.join(p)
                }
            })
            .collect();
        let before = self.playlist.len();
        self.add_files_to_playlist(&resolved, config);
        let added = self.playlist.len() - before;
        Ok(added)
    }
    /// 给当前曲目加书签（记录路径 + 位置 + 标签）。同曲重复加则更新位置。
    /// 在播 CUE 分轨时记分轨书签（cue 起点 + 分轨内偏移）：跳转才能定位到
    /// 正确分轨并从书签处起播，而不是被 CUE 起点覆盖、永远落在分轨头。
    fn add_bookmark(&mut self) {
        let Some(path) = self.current_path.clone() else {
            self.flash_message("无当前曲目");
            return;
        };
        let cue_start_ms = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.as_ref().map(|c| c.start_ms));
        let pos = self.engine.as_ref().map(|e| e.position()).unwrap_or(0.0);
        // CUE 分轨书签存分轨内相对偏移（书签契约：position_secs 相对分轨开头）；
        // 引擎 position() 返回整轨绝对位置，须减去分轨起点，否则跳转会双重
        // 计入起点而落到错误位置。
        let pos = match cue_start_ms {
            Some(cs) => (pos - cs as f64 / 1000.0).max(0.0),
            None => pos,
        };
        let label = self
            .current_metadata
            .as_ref()
            .and_then(|m| m.title.clone())
            .or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| path.display().to_string());
        let is_new = self
            .playlist_state
            .bookmarks
            .add(&path, cue_start_ms, pos, &label);
        if is_new {
            self.flash_message(&format!("已加书签：{label} @ {pos:.0}s"));
        } else {
            self.flash_message(&format!("已更新书签：{label} @ {pos:.0}s"));
        }
    }

    /// 列出所有书签（序号 + 标签 + 位置）。
    fn list_bookmarks(&mut self) {
        let bms = &self.playlist_state.bookmarks.items;
        if bms.is_empty() {
            self.flash_message("暂无书签");
            return;
        }
        let mut msg = String::new();
        for (i, b) in bms.iter().enumerate() {
            msg.push_str(&format!("{i}:{}@{:.0}s ", b.label, b.position_secs));
        }
        self.flash_message(&msg);
    }

    /// 跳到第 n 个书签：写回位置并播放该曲（不在列表则先加入）。
    fn jump_bookmark(&mut self, n: usize, config: &mut Config) {
        let Some(b) = self.playlist_state.bookmarks.items.get(n).cloned() else {
            self.flash_message("无此书签");
            return;
        };
        // 整轨书签：写回断点位置（绕过 save_position 的 0.5s 阈值），供续播。
        // 分轨书签不写断点表——断点表以文件路径为键，分轨偏移会污染该整轨
        // 作为普通曲目播放时的续播点；分轨内定位由下方 pending_cue_offset 承担。
        if b.cue_start_ms.is_none() {
            self.playlist_state
                .positions
                .insert(b.path.clone(), b.position_secs);
        }
        // 分轨书签：只匹配同一 cue 起点的展开分轨项（整轨书签保持任意同 path 项）。
        let idx = if let Some(i) = self.playlist.items().iter().position(|it| {
            it.path == b.path
                && match b.cue_start_ms {
                    Some(cs) => it.cue.as_ref().is_some_and(|c| c.start_ms == cs),
                    None => true,
                }
        }) {
            i
        } else {
            self.add_files_to_playlist(std::slice::from_ref(&b.path), config);
            // 兼容路径也要按 cue 起点匹配（CUE 文件展开为多条分轨，仅按 path
            // 搜会命中第一条分轨而非书签所在分轨）。
            match self.playlist.items().iter().position(|it| {
                it.path == b.path
                    && match b.cue_start_ms {
                        Some(cs) => it.cue.as_ref().is_some_and(|c| c.start_ms == cs),
                        None => true,
                    }
            }) {
                Some(i) => i,
                None => {
                    self.flash_message("书签文件不存在");
                    return;
                }
            }
        };
        // 压入 history，使 p 键能回到跳转前的曲目（与打开文件/浏览器 Enter 一致）。
        self.playlist.jump_to(idx);
        // 分轨书签：把分轨内偏移暂存，play_and_update_current 的 CUE 分支消费
        //（从 cue 起点 + 偏移处起播，否则会被 cue 起点覆盖成从头播）。
        if b.cue_start_ms.is_some() {
            self.pending_cue_offset = Some(b.position_secs);
        }
        self.play_and_update_current(idx, config);
        self.flash_message(&format!("跳到书签：{}", b.label));
    }

    /// 删除第 n 个书签。
    fn remove_bookmark(&mut self, n: usize) {
        if self.playlist_state.bookmarks.items.get(n).is_none() {
            self.flash_message("无此书签");
            return;
        }
        let b = self.playlist_state.bookmarks.items.remove(n);
        self.flash_message(&format!("已删书签：{}", b.label));
    }

    /// 异步导航到目录：后台线程读目录，结果由主循环轮询后 apply_loaded。
    fn navigate_async(&mut self, target: &std::path::Path) {
        match self.browser.resolve_target(target) {
            Ok(resolved) => {
                let gen = self.dir_load_gen;
                self.dir_load_gen = self.dir_load_gen.wrapping_add(1);
                let tx = self.dir_load_tx.clone();
                // 优雅降级：线程启动失败（极罕见）时提示而非 panic。
                if std::thread::Builder::new()
                    .name("fx-dir-load".to_string())
                    .spawn(move || {
                        let result = crate::fs_browser::compute_entries(&resolved);
                        let _ = tx.send((gen, resolved, result));
                    })
                    .is_err()
                {
                    self.flash_message("目录载入线程启动失败");
                }
            }
            Err(e) => self.flash_message(&e),
        }
    }

    /// 浏览器 Enter：进入目录，或对选中文件加入列表并播放。
    ///
    /// 搜索模式下定位到目标后自动退出搜索——用户搜索的目的就是快速
    /// 跳到某个文件/目录，找到后理应立即回到正常浏览（与 tuneux 一致；
    /// 目录与文件分支都要退，否则旧关键字残留、后续按键仍被搜索框吃掉）。
    fn handle_browser_enter(&mut self, config: &mut Config) {
        let was_searching = self.search_mode && self.search_target == SearchTarget::Browser;
        if let Some(dir) = self.browser.selected_dir() {
            // 异步进入目录；退出搜索态（否则按键被搜索框吞掉、旧关键字残留）。
            self.navigate_async(&dir);
            if was_searching {
                self.exit_browser_search();
            }
        } else if let Some(fs_browser::Entry::File { path, .. }) = self.browser.current() {
            let path = path.clone();
            // .cue 文件：按 FILE 引用展开分轨并播放第一轨（镜像场景入口）。
            if fs_browser::is_cue_file(&path) {
                match cue::cue_items_from_cue_file(&path) {
                    Some((items, skipped, audio_path)) => {
                        if skipped > 0 {
                            self.flash_message(&format!("已跳过 {skipped} 条数据轨（不可播放）"));
                        }
                        // 记下第一轨起点：去重全命中时据此定位「第一曲」
                        //（CUE 同 path，不能只按 path 定位，否则命中任意分轨）。
                        let first_start_ms = items
                            .first()
                            .and_then(|it| it.cue.as_ref().map(|c| c.start_ms));
                        let start_idx = self.playlist.items().len();
                        if config.dedup_on_add {
                            self.playlist.add_many_dedup(items);
                        } else {
                            self.playlist.add_many(items);
                        }
                        // 播放首个加入的分轨；全部去重命中时按「path + 第一轨起点」定位。
                        let play_idx = if start_idx < self.playlist.items().len() {
                            start_idx
                        } else {
                            self.playlist
                                .items()
                                .iter()
                                .position(|it| {
                                    it.path == audio_path
                                        && first_start_ms.is_some_and(|ms| {
                                            it.cue.as_ref().map(|c| c.start_ms) == Some(ms)
                                        })
                                })
                                .unwrap_or(0)
                        };
                        self.playlist.jump_to(play_idx);
                        self.play_and_update_current(play_idx, config);
                    }
                    None => {
                        self.flash_message("cue 解析失败或 FILE 引用的音频文件不存在");
                    }
                }
                if was_searching {
                    self.exit_browser_search();
                }
                return;
            }
            // 整轨 + 同名 .cue → 展开为多首 CUE 曲目，加入并播放本整轨第一曲。
            let (expanded, skipped) = self.cue_items_for(&path);
            if skipped > 0 {
                self.flash_message(&format!("已跳过 {skipped} 条数据轨（不可播放）"));
            }
            if !expanded.is_empty() {
                // 记下第一轨起点：去重全命中时据此定位「第一曲」（同 path 多分轨）。
                let first_start_ms = expanded
                    .first()
                    .and_then(|it| it.cue.as_ref().map(|c| c.start_ms));
                let start_idx = self.playlist.items().len();
                if config.dedup_on_add {
                    self.playlist.add_many_dedup(expanded);
                } else {
                    self.playlist.add_many(expanded);
                }
                // 播放首个加入的分轨；若全部去重命中，则按「path + 第一轨起点」定位。
                let play_idx = if start_idx < self.playlist.items().len() {
                    start_idx
                } else {
                    self.playlist
                        .items()
                        .iter()
                        .position(|it| {
                            it.path == path
                                && first_start_ms.is_some_and(|ms| {
                                    it.cue.as_ref().map(|c| c.start_ms) == Some(ms)
                                })
                        })
                        .unwrap_or(0)
                };
                self.playlist.jump_to(play_idx);
                self.play_and_update_current(play_idx, config);
                if was_searching {
                    self.exit_browser_search();
                }
                return;
            }
            let md = self.get_or_extract_metadata(&path);
            let item = playlist::PlaylistItem {
                path: path.clone(),
                album: md.album.clone(),
                track_number: md.track_number,
                cue: None,
            };
            // 去重加入：已存在则不重复添加。
            let added = if config.dedup_on_add {
                self.playlist.add_dedup(item)
            } else {
                self.playlist.add(item);
                true
            };
            if !added {
                // 去重命中：文件已在列表，定位并立即播放（而非静默无反应）。
                if let Some(idx) = self.playlist.items().iter().position(|it| it.path == path) {
                    self.playlist.jump_to(idx);
                    self.play_and_update_current(idx, config);
                }
                if was_searching {
                    self.exit_browser_search();
                }
                return;
            }
            let new_idx = self.playlist.items().len() - 1;
            // 手动点选播放：压入 history，使 p 键能回到点选前的曲目。
            self.playlist.jump_to(new_idx);
            self.play_and_update_current(new_idx, config);
            if was_searching {
                self.exit_browser_search();
            }
        }
    }

    /// 退出浏览器搜索态（搜索中 Enter 定位到目标后调用）：
    /// 清关键字并恢复一级条目，后续按键不再被搜索框吃掉。
    fn exit_browser_search(&mut self) {
        self.search_mode = false;
        self.search_query.clear();
        self.browser.end_search();
    }

    /// 删除选中的曲目（选中专辑组头时忽略）。
    fn remove_selected_track(&mut self) {
        let Some(playlist::Selection::Track(index)) = self.playlist.selected().cloned() else {
            return;
        };
        let was_current = self.playlist.current_index() == Some(index);
        self.playlist.remove(index);
        if was_current {
            if let Some(engine) = &self.engine {
                engine.send(audio::AudioCmd::Stop);
            }
            self.reset_current_track_state();
        }
        if self.playlist.is_empty() {
            self.reset_current_track_state();
        }
    }

    /// 清空播放列表（含停止播放、清空曲目元数据与歌词/封面）。
    fn clear_playlist(&mut self) {
        if let Some(engine) = &self.engine {
            engine.send(audio::AudioCmd::Stop);
        }
        self.playlist.clear();
        self.reset_current_track_state();
    }

    /// 清空「当前曲目」相关状态（元数据 / 路径 / 频谱 / 歌词 / 封面缓存），
    /// 供删除当前曲、清空列表等场景复用。
    fn reset_current_track_state(&mut self) {
        self.current_metadata = None;
        self.current_path = None;
        self.spectrum_peaks.borrow_mut().reset();
        self.current_lyrics = None;
        self.cover_cache = None;
        self.cover_thumb = None;
        self.cover_failed_path = None;
        self.cover_thumb_cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{keycode_to_action, OFFICIAL_PUBKEY};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// 自定义键映射：命中返回动作名、未命中 None、不可自定义键 None
    ///（回退默认键）。配置写法差异（大小写 / 修饰符顺序）经规范化对齐。
    #[test]
    fn keymap_custom_binding_hit_and_miss() {
        let mut km = std::collections::HashMap::new();
        km.insert("toggle_play".to_string(), "ctrl+p".to_string());
        let hit = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(keycode_to_action(&hit, &km), Some("toggle_play"));
        let miss = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(keycode_to_action(&miss, &km), None);
        // F 键不可自定义（不在 key_to_desc 白名单）→ None
        let f5 = KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE);
        assert_eq!(keycode_to_action(&f5, &km), None);
    }

    /// 分轨书签存取往返：存储时从引擎绝对位置减去分轨起点（相对偏移），
    /// 跳转时再由 cue 起点 + 偏移起播——往返必须落回原位（防双重计入起点）。
    #[test]
    fn cue_bookmark_position_roundtrip() {
        // 分轨 2：整轨起点 60s；播放到整轨 75s 处打书签。
        let cue_start_ms = 60_000u64;
        let engine_pos = 75.0; // 引擎返回整轨绝对位置
                               // 存储换算（add_bookmark 口径）：相对偏移 15s。
        let stored = (engine_pos - cue_start_ms as f64 / 1000.0).max(0.0);
        assert_eq!(stored, 15.0);
        // 书签层往返（契约：分轨书签 position_secs 相对分轨开头）。
        let mut list = tuneux_mediax::bookmark::BookmarkList::default();
        let path = std::path::Path::new("/music/album.cue");
        assert!(list.add(path, Some(cue_start_ms), stored, "分轨2"));
        let b = &list.items[0];
        // 消费换算（jump_bookmark → 播放分支口径）：cue 起点 + 偏移起播。
        let jump_secs = b.cue_start_ms.unwrap() as f64 / 1000.0 + b.position_secs;
        assert_eq!(jump_secs, 75.0, "往返必须落回整轨 75s（双重计入会得 135s）");
    }

    /// 校验某个第一方插件：官方公钥验签 committed 的 .sig 必须匹配 committed 的 .wasm。
    /// 防止「wasm 改动后忘记重新签名」导致插件在用户处被静默降级/加载失败。
    fn assert_officially_signed(name: &str, id: &str) {
        let base = format!("{}/../../plugins/{name}", env!("CARGO_MANIFEST_DIR"));
        let wasm = std::fs::read(format!("{base}.wasm")).expect("读取 wasm 失败");
        let sig_bytes = std::fs::read(format!("{base}.sig")).expect("读取 .sig 失败");
        let sig: [u8; 64] = sig_bytes.try_into().expect(".sig 应为 64 字节");

        let mut message = Vec::with_capacity(id.len() + wasm.len());
        message.extend_from_slice(id.as_bytes());
        message.extend_from_slice(&wasm);
        assert!(
            tuneux_pinx::verify_signature(&OFFICIAL_PUBKEY, &message, &sig),
            "{name} 的 .sig 与 .wasm 不匹配（wasm 改动后需重新签名）"
        );
    }

    #[test]
    fn first_party_plugins_are_officially_signed() {
        assert_officially_signed("equalizer", "tuneux-eq");
        assert_officially_signed("compressor", "tuneux-comp");
        assert_officially_signed("皮肤-Norton蓝", "tuneux-skin");
        assert_officially_signed("皮肤-午夜蓝", "tuneux-skin");
    }

    #[test]
    fn skin_plugin_registers_parseable_theme() {
        // 用随包的「皮肤-Norton蓝.wasm」验证：init 经 theme_register 注册的皮肤
        // 文本可解析为调色板（无签名仅影响三态、不影响主题捕获，故不依赖官方私钥）。
        let base = format!("{}/../../plugins/皮肤-Norton蓝", env!("CARGO_MANIFEST_DIR"));
        let wasm = std::fs::read(format!("{base}.wasm")).expect("读取皮肤 wasm 失败");
        let eq_slots: [std::sync::Arc<tuneux_corex::EqParams>; tuneux_corex::EQ_SLOTS] =
            std::array::from_fn(|_| std::sync::Arc::new(tuneux_corex::EqParams::new()));
        let comp_slots: [std::sync::Arc<tuneux_corex::CompressorParams>; tuneux_corex::COMP_SLOTS] =
            std::array::from_fn(|_| std::sync::Arc::new(tuneux_corex::CompressorParams::new()));
        let host = tuneux_pinx::WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let mut plugin = host
            .load(
                &wasm,
                "tuneux-skin",
                None,
                &tuneux_pinx::TrustList::default(),
                &[tuneux_pinx::Capability::Theme],
                &[tuneux_pinx::Capability::Theme],
            )
            .expect("皮肤插件应加载成功");
        plugin.call_init(0).expect("init 应执行成功");
        let text =
            std::str::from_utf8(plugin.theme().expect("应注册皮肤")).expect("皮肤文本应为 UTF-8");
        let pal = crate::tui::theme::Palette::from_skin(text).expect("皮肤应可解析");
        // 断言值跟随当前随包皮肤（Norton 蓝）：换皮肤时同步改这里。
        assert_eq!(pal.bg, Some(ratatui::style::Color::Rgb(0x00, 0x00, 0xaa)));
        assert_eq!(
            pal.border,
            Some(ratatui::style::Color::Rgb(0x55, 0xff, 0xff))
        );
        assert_eq!(pal.border_type, ratatui::widgets::BorderType::Double);
    }
}
