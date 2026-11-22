//! # 配置与播放状态持久化
//!
//! 配置 `tuneux-max.toml` / 播放状态 `tuneux-max-playlist.toml` 与双 TUI
//! 同口径：可执行文件同目录优先，不可写时回退系统配置目录；原子写。
//! 本文件只放偏好与播放状态的读写，不承载 UI。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tuneux_commonx::config_status::{backup_corrupt_file, LoadStatus};
use tuneux_commonx::keymap::parse_key_desc;
use tuneux_commonx::{validate_keymap, LangTable};

static CONFIG_SAVE_LOCKED: AtomicBool = AtomicBool::new(false);
static PLAYLIST_SAVE_LOCKED: AtomicBool = AtomicBool::new(false);
use tuneux_mediax::{PlaylistState, PlaylistView, RepeatMode};

/// 用户偏好配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 音量 0.0–1.0。
    pub volume: f32,
    /// 循环模式。
    pub repeat: RepeatMode,
    /// 随机模式（沿用 mediax 口径：off / tracks / true_random / albums）。
    pub shuffle_mode: String,
    /// ReplayGain 响度归一开关（默认关闭）。
    pub replay_gain: bool,
    /// 播放介质风格（默认 none）。
    pub playback_medium: String,
    /// 界面语言（zh 内置；其它语言读 exe 同目录 locales/）。
    pub lang: String,
    /// 界面主题：仅支持 `terminal`（终端原生）。
    pub theme: String,
    /// 当前选中皮肤名（默认终端原生；持久化，启动 / 重载后按名恢复）。
    pub skin: String,
    /// 播放列表显示模式（默认按专辑分组；g 键切换，持久化）。
    pub playlist_view: PlaylistView,
    /// 上次浏览目录。
    pub last_dir: Option<PathBuf>,
    /// 浏览器/播放列表横向比例（0.15–0.6）。
    pub browser_ratio: f32,
    /// 是否启用系统媒体键。
    pub media_keys_enabled: bool,
    /// 自定义键位映射：动作名 → 键描述（如 `toggle_play = "space"`）。
    pub keymap: HashMap<String, String>,
}

/// keymap 合法动作白名单（与 fx 对齐的可用子集）。
pub const KEYMAP_ACTIONS: &[&str] = &[
    "toggle_play",
    "next",
    "prev",
    "volume_up",
    "volume_down",
    "cycle_repeat",
    "toggle_shuffle",
    "toggle_browser",
    "toggle_cover",
    "toggle_lyrics",
    "cycle_spectrum",
    "cycle_medium",
    "toggle_group_view",
];

/// 保留键：退出路径不可被自定义映射覆盖。
pub const RESERVED_KEYS: &[&str] = &["q", "ctrl+q"];

impl Default for Config {
    fn default() -> Self {
        Self {
            volume: 1.0,
            repeat: RepeatMode::Off,
            shuffle_mode: "off".to_string(),
            replay_gain: false,
            playback_medium: "none".to_string(),
            lang: "zh".to_string(),
            theme: "terminal".to_string(),
            skin: "终端原生".to_string(),
            playlist_view: PlaylistView::ByAlbum,
            last_dir: None,
            browser_ratio: 0.3,
            media_keys_enabled: true,
            keymap: HashMap::new(),
        }
    }
}

/// 配置文件路径。
pub fn config_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-max.toml", "tuneux-max")
}

/// 播放状态文件路径。
pub fn state_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-max-playlist.toml", "tuneux-max")
}

/// 加载配置并报告损坏状态，供启动界面提示。
pub fn load_with_status() -> (Config, LoadStatus) {
    load_config_from(&config_path(), &CONFIG_SAVE_LOCKED)
}

fn load_config_from(path: &Path, locked: &AtomicBool) -> (Config, LoadStatus) {
    let (mut cfg, status) = match load_from(path) {
        Some(cfg) => (cfg, LoadStatus::Ok),
        None if path.exists() => {
            let backup = backup_corrupt_file(path);
            locked.store(true, Ordering::SeqCst);
            (Config::default(), LoadStatus::Corrupt { backup })
        }
        None => (Config::default(), LoadStatus::Missing),
    };
    validate_keymap(&mut cfg.keymap, KEYMAP_ACTIONS, RESERVED_KEYS);
    // 语法合法但运行时按键事件产生不了的描述（数字 / 符号 / shift+具名键）
    // 一并剔除，避免映射保存后静默失效。
    cfg.keymap
        .retain(|_, desc| runtime_key_desc_supported(desc));
    cfg.volume = tuneux_commonx::sanitize_f32(cfg.volume, 1.0);
    cfg.browser_ratio = tuneux_commonx::sanitize_f32(cfg.browser_ratio, 0.3);
    if cfg.lang.trim().is_empty() {
        cfg.lang = "zh".to_string();
    }
    (cfg, status)
}

/// 从指定路径加载配置。
fn load_from(path: &Path) -> Option<Config> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut cfg: Config = toml::from_str(&text).ok()?;
    // 键描述规范化为与按键事件同口径（如 "P" → "p"、"Shift+N" → "shift+n"）。
    for desc in cfg.keymap.values_mut() {
        if let Some(canonical) = parse_key_desc(desc) {
            *desc = canonical;
        }
    }
    Some(cfg)
}

/// 运行时（egui 按键事件）能产生的键描述：具名键、单字母、+ = -。
/// shift 仅对单字母有意义（与 app 侧 key_desc 同口径）。
fn runtime_key_desc_supported(desc: &str) -> bool {
    let base_supported = |base: &str| {
        let mut chars = base.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c.is_ascii_alphabetic() || matches!(c, '+' | '-' | '='),
            _ => matches!(
                base,
                "space"
                    | "tab"
                    | "enter"
                    | "esc"
                    | "backspace"
                    | "up"
                    | "down"
                    | "left"
                    | "right"
                    | "home"
                    | "end"
            ),
        }
    };
    if desc.chars().count() == 1 {
        return base_supported(desc);
    }
    let mut parts: Vec<&str> = desc.split('+').collect();
    let Some(base) = parts.pop() else {
        return false;
    };
    if !base_supported(base) {
        return false;
    }
    let mut shift = false;
    for modifier in parts {
        match modifier {
            "shift" => shift = true,
            "ctrl" | "alt" => {}
            _ => return false,
        }
    }
    if shift && !(base.len() == 1 && base.chars().all(|c| c.is_ascii_alphabetic())) {
        return false;
    }
    true
}

/// 保存配置（失败静默，不阻塞退出）。
pub fn save(cfg: &Config) {
    let _ = save_to_if_unlocked(&config_path(), cfg, &CONFIG_SAVE_LOCKED);
}

fn save_to_if_unlocked(
    path: &Path,
    cfg: &Config,
    locked: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    if locked.load(Ordering::SeqCst) {
        return Ok(());
    }
    save_to(path, cfg)
}

/// 保存配置到指定路径。
fn save_to(path: &Path, cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let text = toml::to_string_pretty(cfg)?;
    atomic_write(path, &text)
}

/// 加载播放状态并报告损坏状态，供启动界面提示。
pub fn load_state_with_status() -> (PlaylistState, LoadStatus) {
    load_state_from(&state_path(), &PLAYLIST_SAVE_LOCKED)
}

fn load_state_from(path: &Path, locked: &AtomicBool) -> (PlaylistState, LoadStatus) {
    let parsed = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok());
    match parsed {
        Some(state) => (state, LoadStatus::Ok),
        None if path.exists() => {
            let backup = backup_corrupt_file(path);
            locked.store(true, Ordering::SeqCst);
            (PlaylistState::default(), LoadStatus::Corrupt { backup })
        }
        None => (PlaylistState::default(), LoadStatus::Missing),
    }
}

/// 保存播放状态（失败静默）。
pub fn save_state(state: &PlaylistState) {
    let path = state_path();
    let _ = save_state_to_if_unlocked(&path, state, &PLAYLIST_SAVE_LOCKED);
}

fn save_state_to_if_unlocked(
    path: &Path,
    state: &PlaylistState,
    locked: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    if locked.load(Ordering::SeqCst) {
        return Ok(());
    }
    let text = toml::to_string_pretty(state)?;
    atomic_write(path, &text)
}

/// 中文锚表。
///
/// 守卫脚本按「`fn zh_table` 到首个 `}` 区间内的 `key = "..."` 行」提取
/// 中文键集合，因此键行必须保持在该函数体内（语言表移动时同步迁移）。
pub fn zh_table() -> LangTable {
    LangTable::parse(
        r#"
app.title = "tuneux-max"
panel.browser = "浏览器"
panel.playlist = "播放列表"
panel.now_playing = "当前曲目"
empty.browser = "（空目录）"
empty.playlist = "（播放列表为空）"
menu.file = "文件"
menu.quit = "退出"
menu.playback = "播放"
menu.play_pause = "播放 / 暂停"
menu.stop = "停止"
menu.prev = "上一曲"
menu.next = "下一曲"
menu.view = "视图"
menu.theme = "主题"
menu.theme_terminal = "终端原生"
menu.settings = "设置"
menu.lang = "语言"
menu.lang_zh = "中文"
menu.lang_en = "English"
menu.help = "帮助"
menu.about = "关于"
menu.shortcuts = "快捷键"
play.playing = "播放中"
play.paused = "已暂停"
play.stopped = "未播放"
action.add = "加入列表"
action.clear = "清空列表"
action.remove = "移除选中"
ctx.add_play = "加入并播放"
ctx.open = "进入"
ctx.add_folder = "加入文件夹（递归）"
ctx.play = "播放"
ctx.remove = "移除"
msg.engine_unavailable = "音频引擎不可用（请检查系统音频设备）"
msg.engine_error = "音频错误"
msg.added = "已加入播放列表"
msg.empty_selection = "没有选中项"
msg.unknown_format = "未知格式"
msg.cue_invalid = "CUE 解析失败或音频文件缺失"
msg.cue_skipped = "已跳过数据轨"
msg.already_added = "已在播放列表中"
msg.consecutive_fail = "连续 10 首无法播放，已停止自动切换"
msg.m3u_skipped = "已跳过 {} 条非 UTF-8 路径"
msg.config_corrupt = "配置文件损坏：已备份为 {}，本次会话不会自动保存"
msg.config_corrupt_nobackup = "配置文件损坏且自动备份失败：请立即手工复制保存原文件；本次会话不会自动保存"
msg.state_corrupt = "播放列表文件损坏：已备份为 {}，本次会话不会自动保存"
msg.state_corrupt_nobackup = "播放列表文件损坏且自动备份失败：请立即手工复制保存原文件；本次会话不会自动保存"
msg.rg_state = "ReplayGain"
msg.rg_on = "已开启"
msg.rg_off = "已关闭"
msg.medium = "介质"
menu.replaygain = "ReplayGain 响度归一"
menu.medium = "介质"
medium.none = "关闭（原始输出）"
medium.tape_clear = "磁带·透明（高保真）"
medium.tape_white = "磁带·白色（清新）"
medium.tape_classic = "磁带·深棕（经典）"
medium.tape_aged = "磁带·红色（老化）"
medium.vinyl_clean = "黑胶·蓝色（低噪声）"
medium.vinyl_dynamic = "黑胶·红色（高动态）"
medium.vinyl_standard = "黑胶·黑色（标准）"
medium.vinyl_aged = "黑胶·彩胶（老化）"
medium.unknown = "未知介质"
browser.up = "返回上一级"
about.version = "版本"
about.license = "开源许可：木兰宽松许可证，第 2 版（MulanPSL-2.0）"
about.engine = "音频引擎：tuneux-corex"
about.desc = "tuneux GUI 版：终端原生配色 + 原生窗口交互"
common.close = "关闭"
common.ok = "好"
fbar.hint = "空格 播放/暂停 · ↑↓ 选择 · Enter 打开/播放 · Del 移除 · b 浏览器 · c 封面 · l 歌词 · v 频谱 · / 搜索 · : 命令 · Ctrl+Q 退出"
menu.media_keys = "系统媒体键"
menu.command = "命令模式…"
search.hint = "搜索（递归）"
search.searching = "搜索中…"
search.none = "（无匹配）"
search.truncated = "结果过多，仅显示前一部分"
playlist.filter = "过滤播放列表"
command.hint = "输入命令，Enter 执行，Esc 取消"
command.help_title = "命令"
msg.media_keys_on = "已启用系统媒体键"
msg.media_keys_off = "已关闭系统媒体键"
msg.m3u_saved = "播放列表已保存"
msg.m3u_loaded = "播放列表已载入"
msg.save_failed = "保存失败"
msg.open_failed = "打开失败"
menu.effects = "音效"
menu.eq = "均衡器…"
menu.compressor = "压缩器…"
menu.plugins = "插件管理…"
menu.reload_plugins = "重载插件"
menu.skin = "皮肤"
menu.skin_terminal = "终端原生"
menu.visual = "可视化"
menu.visual_builtin = "内置频谱"
panel.eq = "均衡器"
panel.compressor = "压缩器"
panel.plugins = "插件"
plugin.none = "（未加载）"
plugin.eq = "均衡器"
plugin.compressor = "压缩器"
plugin.skins = "皮肤"
plugin.visuals = "可视化"
plugin.granted = "能力"
plugin.count = "数量"
msg.plugins_reloaded = "插件已重载"
eq.band = "频段"
eq.gain = "增益"
eq.bypass = "旁路"
eq.reset = "复位"
comp.threshold = "阈值"
comp.ratio = "压缩比"
comp.attack = "启动"
comp.release = "释放"
comp.makeup = "补偿"
comp.enabled = "启用"
group.unknown_album = "未知专辑"
group.track_count = "{}首"
panel.spectrum = "频谱"
panel.bands = "频段"
panel.level = "电平"
spectrum.high = "高"
spectrum.mid = "中"
spectrum.low = "低"
panel.cover = "封面"
panel.lyrics = "歌词"
empty.cover = "（无封面）"
empty.lyrics = "（无歌词）"
menu.view_browser = "浏览器"
menu.view_cover = "封面"
menu.view_lyrics = "歌词"
menu.view_spectrum = "频谱"
view.spectrum_hidden = "关"
view.spectrum_half = "半屏"
view.spectrum_full = "全屏"
        "#,
    )
}
/// 原子写：先写同目录临时文件再 rename，避免半截文件。
fn atomic_write(path: &Path, content: &str) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_terminal_native() {
        let cfg = Config::default();
        assert_eq!(cfg.theme, "terminal");
        assert_eq!(cfg.lang, "zh");
        assert!((cfg.volume - 1.0).abs() < 1e-6);
    }

    #[test]
    fn config_roundtrip() {
        let tmp = std::env::temp_dir().join("tuneux_max_config_roundtrip.toml");
        let _ = std::fs::remove_file(&tmp);
        let cfg = Config {
            volume: 0.42,
            repeat: RepeatMode::List,
            shuffle_mode: "tracks".to_string(),
            lang: "en".to_string(),
            browser_ratio: 0.45,
            ..Config::default()
        };
        save_to(&tmp, &cfg).expect("保存应成功");
        let loaded = load_from(&tmp).expect("加载应成功");
        assert!((loaded.volume - 0.42).abs() < 1e-6);
        assert_eq!(loaded.lang, "en");
        assert!((loaded.browser_ratio - 0.45).abs() < 1e-6);
        assert_eq!(loaded.repeat, RepeatMode::List);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn invalid_keymap_entries_are_filtered_on_load() {
        let tmp = std::env::temp_dir().join("tuneux_max_keymap_filter.toml");
        std::fs::write(
            &tmp,
            "[keymap]\nbogus_action = \"x\"\ntoggle_play = \"foo\"\nnext = \"ctrl+q\"\nprev = \"P\"\n",
        )
        .expect("写测试配置");
        let cfg = load_from(&tmp).expect("加载应成功");
        let mut keymap = cfg.keymap;
        validate_keymap(&mut keymap, KEYMAP_ACTIONS, RESERVED_KEYS);
        assert!(!keymap.contains_key("bogus_action"), "未知动作应剔除");
        assert!(!keymap.contains_key("toggle_play"), "非法键描述应剔除");
        assert!(!keymap.contains_key("next"), "保留键应拒绝映射");
        assert_eq!(
            keymap.get("prev").map(String::as_str),
            Some("p"),
            "合法映射应保留"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn keymap_runtime_unsupported_descs_are_rejected() {
        assert!(runtime_key_desc_supported("space"));
        assert!(runtime_key_desc_supported("p"));
        assert!(runtime_key_desc_supported("shift+p"));
        assert!(runtime_key_desc_supported("ctrl+alt+p"));
        assert!(runtime_key_desc_supported("+"));
        assert!(!runtime_key_desc_supported("1"), "数字键运行时产生不了");
        assert!(!runtime_key_desc_supported("?"), "符号键运行时产生不了");
        assert!(
            !runtime_key_desc_supported("shift+space"),
            "shift 仅对字母有意义"
        );
        assert!(!runtime_key_desc_supported("f1"));
    }

    #[test]
    fn missing_config_falls_back_to_default() {
        let missing = std::env::temp_dir().join("tuneux_max_missing_12345.toml");
        let _ = std::fs::remove_file(&missing);
        assert!(load_from(&missing).is_none());
    }

    #[test]
    fn corrupt_files_are_backed_up_and_never_overwritten() {
        let dir = std::env::temp_dir().join("tuneux_max_corrupt_guard");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("tuneux-max.toml");
        let state_path = dir.join("tuneux-max-playlist.toml");
        std::fs::write(&config_path, "broken = [").unwrap();
        std::fs::write(&state_path, "broken = [").unwrap();

        let config_lock = AtomicBool::new(false);
        let (_, config_status) = load_config_from(&config_path, &config_lock);
        assert!(matches!(config_status, LoadStatus::Corrupt { .. }));
        assert!(config_lock.load(Ordering::SeqCst));
        save_to_if_unlocked(&config_path, &Config::default(), &config_lock)
            .expect("停写应静默成功");
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "broken = [");

        let state_lock = AtomicBool::new(false);
        let (_, state_status) = load_state_from(&state_path, &state_lock);
        assert!(matches!(state_status, LoadStatus::Corrupt { .. }));
        assert!(state_lock.load(Ordering::SeqCst));
        save_state_to_if_unlocked(&state_path, &PlaylistState::default(), &state_lock)
            .expect("停写应静默成功");
        assert_eq!(std::fs::read_to_string(&state_path).unwrap(), "broken = [");
        assert!(dir.join("tuneux-max.toml.corrupt.bak").exists());
        assert!(dir.join("tuneux-max-playlist.toml.corrupt.bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_english_table_covers_every_chinese_key() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales/en.txt");
        let english = LangTable::load(&path).expect("英文语言表应可加载");
        for key in zh_table().keys() {
            assert!(english.get(key).is_some(), "en.txt 缺少 {key}");
        }
    }
}
