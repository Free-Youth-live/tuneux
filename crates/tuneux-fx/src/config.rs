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

// 本模块与基础版 config 同源，本产品线已分化出皮肤/首启继承/歌词偏移/书签等。
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::playlist::PlaylistItem;

use tuneux_commonx::{sanitize_f32, sanitize_f64, validate_keymap};

/// 配置操作结果别名。
///
/// 错误聚合为 trait 对象，避免为每种底层错误（IO、TOML 解析）单独定义类型，
/// 简化代码。配置模块的错误对用户均不致命，调用方据此回退默认值。
pub type ConfigResult<T> = Result<T, Box<dyn std::error::Error>>;

/// 循环播放模式的 i18n key（状态条显示）。
/// 呈现归发行版：枚举本体在数据层（tuneux-mediax），文案走 i18n 表。
pub fn repeat_key(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "repeat.off",
        RepeatMode::Single => "repeat.single",
        RepeatMode::List => "repeat.list",
    }
}

/// 中文内置表（i18n 的「锚」与最终回退）：key → 中文文案。
/// 注意：新增 UI 文案时在此追加 key，并同步 `locales/en.txt`（官方英文）。
pub(crate) fn zh_table() -> tuneux_commonx::LangTable {
    tuneux_commonx::LangTable::parse(
        "\
# —— 面板标题 ——\n\
panel.current_track = \"当前曲目\"\n\
panel.status = \"状态\"\n\
panel.browser = \"文件浏览器\"\n\
panel.playlist = \"播放列表\"\n\
panel.lyrics = \"歌词\"\n\
panel.cover = \"专辑封面\"\n\
panel.cover_browser = \"封面浏览\"\n\
panel.spectrum = \"频 谱\"\n\
panel.oscilloscope = \"示波器\"\n\
panel.gauge = \"指针表\"\n\
panel.level = \"电平\"\n\
panel.bands = \"频段\"\n\
panel.about = \"关于\"\n\
panel.skin = \"皮肤配色\"\n\
panel.eq = \"均衡器\"\n\
panel.compressor = \"压缩器\"\n\
panel.plugin = \"插 件\"\n\
# —— 状态栏 ——\n\
status.playing = \"[播]\"\n\
status.stopped = \"[停]\"\n\
status.shuffle = \" 随机\"\n\
status.volume = \"音量\"\n\
# —— 元数据展示 ——\n\
metadata.unknown_title = \"未知曲目\"\n\
metadata.unknown_artist = \"未知艺人\"\n\
metadata.unknown_album = \"未知专辑\"\n\
metadata.artist_label = \"歌手\"\n\
metadata.album_label = \"专辑\"\n\
metadata.track_label = \"曲目\"\n\
# —— 循环模式 ——\n\
repeat.off = \"顺序\"\n\
repeat.single = \"单曲\"\n\
repeat.list = \"循环\"\n\
# —— 功能键栏 ——\n\
fkey.menu = \"菜单\"\n\
fkey.panels = \"面板\"\n\
fkey.eq = \"均衡器\"\n\
fkey.help = \"帮助\"\n\
# —— 菜单 ——\n\
menu.file = \"文件\"\n\
menu.play = \"播放\"\n\
menu.medium = \"介质\"\n\
menu.view = \"视图\"\n\
menu.tools = \"工具\"\n\
menu.settings = \"设置\"\n\
menu.plugins = \"插件\"\n\
menu.help = \"帮助\"\n\
menu.quit = \"退出\"\n\
menu.toggle_play = \"播放 / 暂停\"\n\
menu.prev = \"上一曲\"\n\
menu.next = \"下一曲\"\n\
menu.repeat = \"循环模式\"\n\
menu.shuffle = \"随机播放\"\n\
menu.vol_up = \"音量增大\"\n\
menu.vol_down = \"音量减小\"\n\
menu.browser = \"文件浏览器\"\n\
menu.cover = \"专辑封面\"\n\
menu.lyrics = \"歌词\"\n\
menu.spectrum = \"频谱\"\n\
menu.group = \"播放列表分组\"\n\
menu.eq = \"均衡器\"\n\
menu.compressor = \"压缩器\"\n\
menu.dsp = \"DSP\"\n\
menu.tag_edit = \"标签编辑\"\n\
menu.convert = \"格式转换\"\n\
menu.cover_mgmt = \"封面管理\"\n\
menu.output_device = \"输出设备（自动跟随）\"\n\
menu.replaygain = \"ReplayGain 响度归一\"\n\
msg.cover_fail = \"封面解码失败（已跳过）\"\n\
msg.unknown_format = \"未知格式\"\n\
msg.consecutive_fail = \"连续 10 首无法播放，已停止自动切换\"\n\
msg.quit_hint = \"退出请按 q 或 Ctrl+C\"\n\
msg.usage_vol = \"用法：volume <0-100>\"\n\
msg.repeat_off = \"循环：关闭\"\n\
msg.repeat_list = \"循环：列表\"\n\
msg.repeat_single = \"循环：单曲\"\n\
msg.usage_bm_jump = \"用法：bookmark-jump <序号>\"\n\
msg.m3u_empty = \"m3u 中无有效路径\"\n\
msg.no_current = \"无当前曲目\"\n\
msg.bm_empty = \"暂无书签\"\n\
msg.bm_not_found = \"无此书签\"\n\
msg.bm_no_file = \"书签文件不存在\"\n\
msg.plugin_eq_missing = \"均衡器插件未加载 · 按 u 加载\"\n\
msg.plugin_comp_missing = \"压缩器插件未加载 · 按 u 加载\"\n\
msg.eq_enabled = \"已启用 · e 旁路\"\n\
msg.eq_bypassed = \"已旁路 · e 恢复\"\n\
msg.eq_hint = \"↑↓ ±1dB · ←→ 切段 · e 旁路 · r 恢复默认 · u 卸载 · Esc 关闭\"\n\
msg.eq_hint_load = \"u 加载插件 · Esc 关闭\"\n\
msg.comp_hint = \"↑↓ 调 · ←→ 切 · e 旁路 · r 恢复默认 · u 卸载 · Esc 关闭\"\n\
msg.comp_threshold = \"阈值\"\n\
msg.comp_ratio = \"压缩比\"\n\
msg.comp_attack = \"启动\"\n\
msg.comp_release = \"释放\"\n\
panel.filter = \"滤波器\"\n\
filter.hint = \"上下调·左右切·e旁路·r重置·Esc关闭\"\n\
msg.comp_makeup = \"补偿\"\n\
filter.on = \"开\"\n\
menu.filter = \"滤波器\"\n\
filter.cutoff = \"截止\"\n\
filter.resonance = \"谐振\"\n\
filter.off = \"关\"\n\
msg.no_visual = \"无可视化插件画面（plugins/ 下放「可视化-*」插件并经签名后重启）\"\n\
msg.output_device_info = \"输出设备：自动跟随系统默认输出——插拔耳机/连断蓝牙约 2 秒内自动切换续播；手动指定设备暂未提供\"\n\
msg.feature_na = \"该功能暂未提供（随插件/后续版本开放）\"\n\
msg.rg_on = \"已开启\"\n\
msg.rg_off = \"已关闭\"\n\
msg.lang_switched = \"已切换为{}\"\n\
msg.engine_fail = \"音频引擎初始化失败，播放功能不可用：{}\"\n\
msg.medium = \"介质\"\n\
msg.bm_jump = \"跳到书签：{}\"\n\
msg.bm_deleted = \"已删书签：{}\"\n\
msg.no_visual_pick = \"无可视化插件（plugins/ 下放「可视化-*」插件并重启）\"\n\
empty.playlist = \"（空）按 b 打开浏览器，a 加入列表\"\n\
group.unknown_album = \"未知专辑\"\n\
group.track_count = \"{}首\"\n\
spectrum.low = \"低\"\n\
spectrum.mid = \"中\"\n\
spectrum.high = \"高\"\n\
medium.none = \"关闭（原始输出）\"\n\
medium.tape_clear = \"磁带·透明（高保真）\"\n\
medium.tape_white = \"磁带·白色（清新）\"\n\
medium.tape_classic = \"磁带·深棕（经典）\"\n\
medium.tape_aged = \"磁带·红色（老化）\"\n\
medium.vinyl_clean = \"黑胶·蓝色（低噪声）\"\n\
medium.vinyl_dynamic = \"黑胶·红色（高动态）\"\n\
medium.vinyl_standard = \"黑胶·黑色（标准）\"\n\
medium.vinyl_aged = \"黑胶·彩胶（老化）\"\n\
medium.unknown = \"未知介质\"\n\
# —— 关于弹窗 ——\n\
about.brand = \"tuneux-fx · FreeYouth\"\n\
about.tagline = \"插件化命令行音乐播放器\"\n\
about.plugins_cat = \"插件\"\n\
about.formats = \"支持 MP3 · FLAC · WAV · OGG · OPUS · WV · M4A · AAC · ALAC\"\n\
about.keys1 = \"空格 播放/暂停 · n/p 下一曲/上一曲 · ←→ ±5秒 · +/- 音量\"\n\
about.keys2 = \"b 浏览器 · c 封面 · l 歌词 · v 频谱 · g 分组 · a 加入\"\n\
about.keys3 = \"F10 菜单 · : 命令 · m 介质 · ? 关于 · q 退出\"\n\
about.deps = \"基于以下开源项目构建：\"\n\
about.dep_audio = \"音频  cpal · symphonia · opus-decoder · rubato · rustfft · ringbuf\"\n\
about.dep_ui = \"界面  ratatui · crossterm · image · unicode-width\"\n\
about.dep_common = \"通用  serde · toml · dirs · encoding_rs · crossbeam-channel\"\n\
about.dep_plugin = \"插件  wasmi · ed25519-dalek\"\n\
about.dep_platform = \"平台  zbus（Linux）· rdev（Windows）\"\n\
about.license = \"本项目采用木兰宽松许可证 v2（MulanPSL-2.0）\"\n\
about.copyright = \"© 不羁的青春（FreeYouth）\"\n\
about.close = \"按任意键关闭\"\n\
search.esc_exit_search = \"（无匹配）Esc 退出搜索\"\n\
menu.lang_select = \"语言…\"\n\
menu.skin_select = \"皮肤配色…\"\n\
menu.visual = \"可视化面板\"\n\
menu.about = \"关于 / 快捷键速查\"\n\
fkey.bar = \"1-8菜单  F5-F8面板  F9均衡器  F10菜单  ?帮助\"\n\
empty.cover = \"（无封面）\n按 c 隐藏\"\n\
empty.dir = \"（空目录）\"\n\
empty.lyrics = \"（无歌词）\n放置同名 .lrc 或在标签内嵌歌词（USLT/LYRICS）可显示\"\n\
empty.playlist_short = \"（播放列表为空）\n按 c 隐藏\"\n\
msg.bm_unknown_album = \"未知专辑\"\n\
msg.clear_confirm = \"再按一次 x 确认清空播放列表\"\n\
msg.cleared = \"播放列表已清空\"\n\
msg.cue_fail = \"cue 解析失败或 FILE 引用的音频文件不存在\"\n\
msg.empty = \"（未播放）\"\n\
msg.scan_dir = \"正在扫描目录\"\n\
msg.skipped_data = \"已跳过 {} 条数据轨（不可播放）\"\n\
msg.thread_fail = \"线程启动失败\"\n\
search.esc_exit = \"Esc 退出\"\n\
search.truncated = \"目录过大，仅搜索前 2 万条\"\n\
cmd.execute = \"回车执行 · Esc 取消\"\n\
msg.bm_added = \"书签已添加：{} @ {}s\"\n\
msg.bm_updated = \"书签已更新：{} @ {}s\"\n\
msg.error = \"错误\"\n\
msg.load_fail = \"加载失败\"\n\
msg.loaded = \"已加载 {} 首\"\n\
msg.lyrics_offset = \"歌词偏移\"\n\
msg.rg_state = \"ReplayGain\"\n\
msg.save_fail = \"保存失败\"\n\
msg.saved_m3u = \"已保存 {} 首到 {}\"\n\
msg.unknown_cmd = \"未知命令：{}（输入 help）\"\n\
msg.usage_bm_del = \"用法：bookmark-del <索引>\"\n\
msg.usage_repeat = \"用法：repeat <off|list|single>\"\n\
msg.vol_set = \"音量已设为 {}%\"\n\
skin.default_dos = \"默认（DOS）\"\n\
skin.hint = \"↑↓ 预览 · Enter 确认 · Esc 取消\"\n\
skin.terminal_native = \"终端原生\"\n\
",
    )
}

/// 构建 i18n 查询器：请求语言表（`locales/<lang>.txt`，exe 同目录）+ zh 内置兜底。
/// `lang == "zh"` 或文件缺失时 primary 为空表，走 zh 兜底；绝不 panic。
pub(crate) fn build_i18n(lang: &str) -> tuneux_commonx::I18n {
    // 加载机制已下沉 commonx（唯一实现）；zh 表留在本 crate（完整性校验守护）。
    tuneux_commonx::build_i18n(lang, zh_table())
}

/// 扫描可用语言：内置 zh + exe 同目录 locales/*.txt。
/// 返回 (lang_id, display_name) 列表，按字母排序（zh 始终第一）。
/// display_name 取 .txt 首行 `# display: xxx` 注释；无注释时用文件名。
pub(crate) fn scan_langs() -> Vec<(String, String)> {
    // 语言扫描机制已下沉 commonx（唯一实现，三产品同源）。
    tuneux_commonx::scan_langs()
}

// 循环播放模式（三态：Off / Single / List）下沉在数据层共享，
// serde 表示（"off"/"single"/"list"）已冻结；此处重导出保持
// `crate::config::RepeatMode` 既有引用路径不变。
pub use tuneux_mediax::RepeatMode;

// 播放列表显示模式（Flat/ByAlbum）下沉在数据层（tuneux-mediax），
// serde 表示（"flat"/"by_album"）已冻结；此处重导出保持
// `crate::config::PlaylistView` 既有引用路径不变。
pub use tuneux_mediax::PlaylistView;

/// 播放列表显示模式的默认值（fx：平铺大排行）。
fn default_playlist_view() -> PlaylistView {
    PlaylistView::Flat
}

/// 可视化面板的显示模式（`v` 键切换：关 → 频谱半屏 → 频谱全屏 → 示波器 →
/// 插件面板 → 指针表 → 关）。
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
    /// 插件可视化面板（「可视化-*」插件输出的字符画）占满整个主区。
    Plugin,
    /// 指针表（灰阶 2.5D 字符网格的仿真 VU 指针表，L/R 双表）。
    Gauge,
}

impl SpectrumMode {
    /// 循环到下一模式：Hidden → Half → Full → Oscilloscope → Plugin → Gauge → Hidden。
    pub fn next(self) -> Self {
        match self {
            SpectrumMode::Hidden => SpectrumMode::Half,
            SpectrumMode::Half => SpectrumMode::Full,
            SpectrumMode::Full => SpectrumMode::Oscilloscope,
            SpectrumMode::Oscilloscope => SpectrumMode::Plugin,
            SpectrumMode::Plugin => SpectrumMode::Gauge,
            SpectrumMode::Gauge => SpectrumMode::Hidden,
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

    /// 皮肤名（plugins/ 下「皮肤-*」清单内按名匹配；空 = 内置默认 DOS 风配色）。
    #[serde(default)]
    pub skin: String,

    /// 是否随机播放。
    #[serde(default)]
    pub shuffle: bool,

    /// 播放列表显示模式（平铺 / 按专辑分组）。
    #[serde(default = "default_playlist_view")]
    pub playlist_view: PlaylistView,

    /// 频谱显示模式（关 / 半屏 / 全屏 / 示波器 / 插件面板），退出时保留。
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
    /// 已接入按键处理（优先于内置默认键）；合法动作名见 KEYMAP_ACTIONS
    ///（toggle_play / next / prev / volume_up / volume_down）。
    #[serde(default)]
    pub keymap: HashMap<String, String>,
    /// 系统媒体键开关（默认开启）。
    ///
    /// 关闭场景：Windows 低层键盘钩子是系统级全局行为、Linux MPRIS 服务名
    /// 可能与其他播放器冲突——用户可设 `media_keys_enabled = false` 关闭。
    #[serde(default = "default_media_keys_enabled")]
    pub media_keys_enabled: bool,

    /// 界面语言：`"zh"` 默认（内置中文表）；其它语言读 `locales/<lang>.txt`。
    /// 自由字符串（非封闭枚举），空串回落 `"zh"`；fx 经菜单「设置 → 语言」
    /// 切换就地重建语言表、立即生效（基础版启动时读取）。
    #[serde(default = "default_lang")]
    pub lang: String,
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

/// 界面语言默认值：中文。
fn default_lang() -> String {
    "zh".to_string()
}

/// 手动实现 Default：音量默认满音量 1.0（而非 f32 派生的 0.0 静音），
/// 浏览器比例默认 0.4，其余字段用类型的自然默认。
impl Default for Config {
    fn default() -> Self {
        Self {
            volume: default_volume(),
            replay_gain: false,
            repeat: RepeatMode::default(),
            skin: String::new(),
            shuffle: false,
            playlist_view: default_playlist_view(),
            spectrum_mode: SpectrumMode::Hidden,
            lyrics_mode: LyricsMode::Hidden,
            lyrics_offset: 0.0,
            // -fx 默认浏览器常显；tuneux 默认隐藏，按 b 唤出。
            left_panel: LeftPanel::Browser,
            playback_medium: "none".to_string(),
            browser_ratio: default_browser_ratio(),
            last_dir: None,
            dedup_on_add: default_dedup_on_add(),
            keymap: HashMap::new(),
            media_keys_enabled: default_media_keys_enabled(),
            lang: default_lang(),
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

/// 播放状态（保存到独立文件 `tuneux-fx-playlist.toml`）。
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
    #[serde(default)]
    pub items: Vec<PlaylistItem>,
    /// 每首曲目的播放进度（路径 → 秒）。
    #[serde(default)]
    pub positions: BTreeMap<PathBuf, f64>,
    /// ReplayGain 测量结果缓存（路径 → 增益 dB）。
    ///
    /// 与断点续播进度一同持久化到 `tuneux-fx-playlist.toml`：同一首歌曲
    /// 首次播放时流式分析测得整曲响度，之后每次播放直接套用缓存
    /// 的增益，免去重复分析（跨会话有效）。`#[serde(default)]`
    /// 保证旧版 `tuneux-fx-playlist.toml`（无此字段）仍可正常加载。
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

/// 配置文件 `tuneux-fx.toml` 的存储路径。
pub fn config_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-fx.toml", "tuneux-fx")
}

/// 播放状态文件 `tuneux-fx-playlist.toml` 的存储路径（与基础版分离，避免互覆盖）。
pub fn playlist_state_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-fx-playlist.toml", "tuneux-fx")
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
    tuneux_commonx::portable_path("tuneux-fx-plugins.log", "tuneux-fx")
}

/// keymap 合法动作名白名单（与 tui/app/mod.rs 键处理支持的动作一致）。
const KEYMAP_ACTIONS: &[&str] = &["toggle_play", "next", "prev", "volume_up", "volume_down"];

/// 校验并清理 keymap：剔除非法动作名（未知动作 / 空白键描述），
/// 避免用户误配导致快捷键静默失效或与退出键（q/Ctrl+C）冲突。
///
/// 非法映射打警告并剔除；保留键冲突由文档警示。
/// 保留键（规范化后的键描述）：不可作为自定义映射的目标。
/// 退出路径是安全底线——q / Ctrl+C 被映射走后，用户配置失误将无法退出
/// 程序（手册「q / Ctrl+C 不可重映射」的承诺由本清单兑现）。
const RESERVED_KEYS: &[&str] = &["q", "ctrl+c"];

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

/// 加载配置：始终返回有效 Config——加载失败时打印警告并回退默认值，
/// 保证程序在任何情况下都能启动。首次启动时尝试继承 tuneux 的配置。
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
    validate_keymap(&mut cfg.keymap, KEYMAP_ACTIONS, RESERVED_KEYS);
    // 浮点字段净化：手写成 nan/inf 时 clamp 失效（NaN 比较全 false），
    // 会导致 0 宽度浏览器不可见 / 音量归零等，回退默认值。
    cfg.browser_ratio = sanitize_f32(cfg.browser_ratio, default_browser_ratio());
    cfg.lyrics_offset = sanitize_f64(cfg.lyrics_offset, 0.0);
    cfg.volume = sanitize_f32(cfg.volume, default_volume());
    // 语言字段净化：空串/纯空白回落 zh。
    if cfg.lang.trim().is_empty() {
        cfg.lang = default_lang();
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
    atomic_write(path, &text)?;
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

/// 保存播放状态到独立文件 `tuneux-fx-playlist.toml`。
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
    atomic_write(path, &text)?;
    Ok(())
}

// =============================================================================
// 单元测试
// =============================================================================
/// 原子写文件：先写同目录临时文件再 rename 替换。
///
/// 直接 `fs::write` 是「原地截断再写」，写入过程中断电/强杀会产生半截文件，
/// 下次启动解析失败 → 回退默认值 → 播放列表/断点/键位静默丢失。
/// rename(2) 在同一文件系统上是原子操作，要么旧文件完整、要么新文件完整。
fn atomic_write(path: &Path, content: &str) -> ConfigResult<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuneux_commonx::parse_key_desc;

    /// 循环模式切换顺序正确：Off → Single → List → Off。
    #[test]
    fn repeat_mode_cycle() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Single);
        assert_eq!(RepeatMode::Single.next(), RepeatMode::List);
        assert_eq!(RepeatMode::List.next(), RepeatMode::Off);
    }

    /// zh_table key 卫生：每个 key 都是干净的 `[a-z][a-z0-9_.]*` 形态。
    ///
    /// 守护内嵌字符串的续行符写法（`\n\` 多写一个反斜杠会把字面 `\n`
    /// 粘到下一个 key 前面——源码级 grep 看不到，只有运行时才暴露，
    /// 污染后的 key 查不到、中文界面会直接显示原始 key）。
    #[test]
    fn zh_table_keys_are_clean() {
        let zh = zh_table();
        assert!(
            zh.len() >= 170,
            "zh_table 应至少 170 条（当前 {}）",
            zh.len()
        );
        for k in zh.keys() {
            let shaped = k.starts_with(|c: char| c.is_ascii_lowercase())
                && k.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
            assert!(shaped, "key 形态异常: {k:?}");
        }
    }

    /// 旧配置兼容：repeat 字段沿用冻结表示（"off"/"single"/"list"）解析。
    /// 枚举本体下沉数据层（tuneux-mediax）后表示不变，旧 toml 无需迁移。
    #[test]
    fn old_toml_repeat_field_still_parses() {
        let cfg: Config = toml::from_str("repeat = \"single\"\n").unwrap();
        assert_eq!(cfg.repeat, RepeatMode::Single);
        // 旧文件常见无引号单引号写法同样解析。
        let cfg: Config = toml::from_str("repeat = 'list'\n").unwrap();
        assert_eq!(cfg.repeat, RepeatMode::List);
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
            skin: "午夜蓝".to_string(),
            shuffle: true,
            playlist_view: PlaylistView::Flat,
            spectrum_mode: SpectrumMode::Half,
            lyrics_mode: LyricsMode::Visible,
            lyrics_offset: 0.5,
            left_panel: LeftPanel::Browser,
            playback_medium: "tape".to_string(),
            browser_ratio: 0.55,
            last_dir: Some(PathBuf::from("/tmp/music")),
            dedup_on_add: false,
            keymap: HashMap::from([
                ("toggle_play".to_string(), "space".to_string()),
                ("volume_up".to_string(), "+".to_string()),
            ]),
            media_keys_enabled: true,
            lang: default_lang(),
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

    /// 保留键校验：q / Ctrl+C（含大小写变体）不可作为映射目标——退出底线；
    /// 无法解析的描述同样在加载期拒绝（与字段文档承诺一致）。
    #[test]
    fn validate_keymap_rejects_reserved_and_unparsable() {
        let mut map = HashMap::from([
            ("toggle_play".to_string(), "q".to_string()), // 保留：q
            ("next".to_string(), "Ctrl+C".to_string()),   // 保留：ctrl+c（大小写变体）
            ("prev".to_string(), "shift+q".to_string()),  // 合法：shift+q 非保留键
            ("volume_up".to_string(), "f5".to_string()),  // 非法：F 键不可自定义
            ("volume_down".to_string(), "bogus key".to_string()), // 非法：无法解析
        ]);
        validate_keymap(&mut map, KEYMAP_ACTIONS, RESERVED_KEYS);
        assert!(!map.contains_key("toggle_play"), "q 映射应被拒绝");
        assert!(
            !map.contains_key("next"),
            "ctrl+c 映射应被拒绝（大小写不敏感）"
        );
        assert!(!map.contains_key("volume_up"), "F 键描述应被解析层拒绝");
        assert!(!map.contains_key("volume_down"), "无法解析的描述应被拒绝");
        assert_eq!(
            map.get("prev").map(|s| s.as_str()),
            Some("shift+q"),
            "合法映射应保留"
        );
    }
}
