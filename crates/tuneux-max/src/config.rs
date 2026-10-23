//! # max 配置 / 播放状态持久化 + 内置中文语言表
//!
//! tuneux-max.toml 与 tuneux-max-playlist.toml 的加载 / 净化 / 原子保存
//! （路径规则经 commonx portable_path：exe 同目录优先，不可写回退系统配置目录）。
//! 口径与双 TUI 一致：加载失败回退默认值绝不阻塞启动；启动期诊断走
//! eprintln（此刻语言表尚未加载，属开发者通道，不进 i18n）。
//!
//! zh 内置语言表只收录新 UI 实际引用的词条（词条集与 app_v2 同步维护，
//! 反向完整性由 tools/check-i18n-keys.sh 守护）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use tuneux_commonx::config_status::backup_corrupt_file;
/// 加载状态与损坏备份口径已下沉 commonx（三端共用）；再导出保持
/// `crate::config::LoadStatus` 引用不变。
pub use tuneux_commonx::config_status::LoadStatus;

/// 配置损坏停写锁（tuneux-max.toml 维度）：置位后本会话一切保存入口跳过。
static CONFIG_SAVE_LOCKED: AtomicBool = AtomicBool::new(false);
/// 播放状态损坏停写锁（tuneux-max-playlist.toml 维度）。
static STATE_SAVE_LOCKED: AtomicBool = AtomicBool::new(false);

/// 播放列表列开关（列头右键配置；默认全开）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PlaylistCols {
    /// 序号列（#）。
    pub num: bool,
    /// 标题列。
    pub title: bool,
    /// 艺术家列。
    pub artist: bool,
    /// 专辑列（分组视图下组头已有专辑名，平铺视图此列有价值）。
    pub album: bool,
    /// 时长列。
    pub duration: bool,
}

impl Default for PlaylistCols {
    fn default() -> Self {
        Self {
            num: true,
            title: true,
            artist: true,
            album: false,
            duration: true,
        }
    }
}

/// max 配置（新 UI 最小集；功能落地时再按需增补字段）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MaxConfig {
    /// 界面语言（语言表文件名，不含 .txt；zh = 内置中文）。
    pub lang: String,
    /// 音量 0.0–1.0。
    pub volume: f32,
    /// 上次浏览目录（浏览器启动起点）。
    pub last_dir: Option<PathBuf>,
    /// 界面主题名（内置主题 id）。
    #[serde(default = "default_theme")]
    pub theme: String,
    /// 界面字体名（字体目录里的显示名；内置 Sarasa Mono SC 为默认，
    /// fonts/ 目录自投字体按文件名入列）。
    #[serde(default = "default_font")]
    pub font: String,
    /// 循环模式（媒体层枚举，serde 表示冻结为 off/single/list）。
    pub repeat: tuneux_mediax::RepeatMode,
    /// 播放列表视图（flat / by_album，serde 表示由 mediax 冻结）。
    #[serde(default = "default_view")]
    pub view: tuneux_mediax::PlaylistView,
    /// 模块加装覆盖（皮肤 id → 模块 id → 开关）：皮肤默认集之上的
    /// 用户自定义，右键/菜单切换写入；换肤时该皮肤的覆盖自动生效。
    #[serde(default)]
    pub mod_over: std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
    /// 播放列表列开关（列头右键配置）。
    #[serde(default)]
    pub pl_cols: PlaylistCols,
    /// 停靠布局树（叶 = 模块，内节点 = 可拖分割）；None = 全关空 dock。
    #[serde(default = "default_dock")]
    pub dock: Option<crate::dock::DockNode>,
    /// 布局预设（名字 → dock 树快照；命令/菜单存取）。
    #[serde(default)]
    pub dock_presets: std::collections::BTreeMap<String, crate::dock::DockNode>,
    /// 书签（mediax BookmarkList serde 直存；命令 bm/bm-jump/bm-del 操作）。
    #[serde(default)]
    pub bookmarks: tuneux_mediax::bookmark::BookmarkList,
}

impl Default for MaxConfig {
    fn default() -> Self {
        Self {
            lang: "zh".to_string(),
            volume: 0.8,
            last_dir: None,
            theme: default_theme(),
            font: default_font(),
            repeat: tuneux_mediax::RepeatMode::Off,
            view: default_view(),
            mod_over: std::collections::BTreeMap::new(),
            pl_cols: PlaylistCols::default(),
            dock: default_dock(),
            dock_presets: std::collections::BTreeMap::new(),
            bookmarks: tuneux_mediax::bookmark::BookmarkList::default(),
        }
    }
}

/// 默认 dock：浏览器 | （列表 / 封面 上下）——与重写前默认观感对齐。
fn default_dock() -> Option<crate::dock::DockNode> {
    // 默认布局 = fx TUI 同构（2026-10-09 定为默认口径）：
    // 左浏览器（约 1/3）+ 右播放列表（约 2/3，主区）；歌词模块
    // 常驻右下（fx 的 l 面板对应物）。其余模块（频谱/EQ/封面等）
    // 经右键菜单或「视图→模块」按需加装。
    use crate::dock::{DockNode, ModuleId};
    Some(DockNode::Split {
        side_by_side: true,
        ratio: 0.35,
        a: Box::new(DockNode::leaf(ModuleId::Browser)),
        b: Box::new(DockNode::Split {
            side_by_side: false,
            ratio: 0.72,
            a: Box::new(DockNode::leaf(ModuleId::Playlist)),
            b: Box::new(DockNode::leaf(ModuleId::Lyrics)),
        }),
    })
}

fn default_theme() -> String {
    "dos".to_string()
}

fn default_font() -> String {
    crate::theme::EMBEDDED_FONT_NAME.to_string()
}

fn default_view() -> tuneux_mediax::PlaylistView {
    // 2026-10-09 起默认按专辑分组（产品口径：加入即分组、按曲序排）。
    tuneux_mediax::PlaylistView::ByAlbum
}

/// 配置文件路径（exe 同目录优先，回退系统配置目录）。
pub fn config_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-max.toml", "tuneux-max")
}

/// 加载配置：缺失 / 解析失败回退默认，绝不 panic、绝不阻塞启动。
/// 音量做 NaN/越界净化（损坏配置不得产生无声或爆音启动）。
///
/// 文件损坏（存在但解析失败）时：先备份原文件为 `.corrupt.bak`，再锁死
/// 本会话的保存入口——默认值只在内存兜底运行，绝不落盘覆写用户原文件。
pub fn load() -> MaxConfig {
    let path = config_path();
    let mut cfg = if path.exists() {
        match std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<MaxConfig>(&t).ok())
        {
            Some(cfg) => cfg,
            None => {
                let bak = backup_corrupt_file(&path);
                CONFIG_SAVE_LOCKED.store(true, Ordering::SeqCst);
                eprintln!(
                    "[配置] 解析失败，本次会话使用默认值且不会自动保存（{}）",
                    path.display()
                );
                match bak {
                    Some(b) => eprintln!("[配置] 原文件已备份到 {}，请修复配置后重启", b.display()),
                    None => eprintln!("[配置] 原文件备份失败，请立即手工复制保存原文件"),
                }
                MaxConfig::default()
            }
        }
    } else {
        MaxConfig::default()
    };
    cfg.volume = tuneux_commonx::sanitize_f32(cfg.volume, 0.8).clamp(0.0, 1.0);
    cfg
}

/// 保存配置（原子写：临时文件 + rename）。失败静默（配置丢失不致命）。
/// 损坏停写锁置位时（加载发现原文件损坏）整体跳过，见 [`load`]。
pub fn save(cfg: &MaxConfig) {
    atomic_write_if_unlocked(&config_path(), cfg, &CONFIG_SAVE_LOCKED);
}

/// 带停写锁检查的原子写（锁与路径均可注入，便于单元测试）。
fn atomic_write_if_unlocked<T: Serialize>(path: &Path, value: &T, locked: &AtomicBool) {
    if locked.load(Ordering::SeqCst) {
        return;
    }
    atomic_write(path, value);
}

/// 插件目录：exe 同目录 plugins/（便携口径）优先，安装态回退配置目录。
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
    path.push("tuneux-max");
    path.push("plugins");
    path
}

/// 插件装载核查日志路径（pinx journal 口径，与 fx 分文件）。
pub fn plugin_log_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-max-plugins.log", "tuneux-max")
}

/// 播放状态文件路径（列表体积大且频繁变化，与配置分离存储）。
pub fn state_path() -> PathBuf {
    tuneux_commonx::portable_path("tuneux-max-playlist.toml", "tuneux-max")
}

/// 加载播放状态并报告加载状态（供 UI 弹出损坏提示）。
///
/// 缺失 / 解析失败回退空态，绝不 panic。损坏时与 [`load`] 同口径：
/// 备份原文件为 `.corrupt.bak` 并锁死本会话播放状态写回——绝不用内存
/// 空列表覆写磁盘上的用户列表。
pub fn load_state_with_status() -> (tuneux_mediax::PlaylistState, LoadStatus) {
    let path = state_path();
    if !path.exists() {
        return (tuneux_mediax::PlaylistState::default(), LoadStatus::Missing);
    }
    let parsed = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| toml::from_str(&t).ok());
    match parsed {
        Some(st) => (st, LoadStatus::Ok),
        None => {
            let backup = backup_corrupt_file(&path);
            STATE_SAVE_LOCKED.store(true, Ordering::SeqCst);
            (
                tuneux_mediax::PlaylistState::default(),
                LoadStatus::Corrupt { backup },
            )
        }
    }
}

/// 保存播放状态（原子写）。损坏停写锁置位时整体跳过，
/// 见 [`load_state_with_status`]。
pub fn save_state(st: &tuneux_mediax::PlaylistState) {
    atomic_write_if_unlocked(&state_path(), st, &STATE_SAVE_LOCKED);
}

/// 原子写：先写 .tmp 再 rename（半截文件不得替换旧配置）。
fn atomic_write<T: Serialize>(path: &std::path::Path, value: &T) {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let Ok(text) = toml::to_string_pretty(value) else {
        return;
    };
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, text.as_bytes()).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, path);
}

/// 内置中文语言表（新 UI 词条全集）。
pub fn zh_table() -> tuneux_commonx::LangTable {
    tuneux_commonx::LangTable::parse(
        "\
menu.file = \"文件\"\n\
menu.play = \"播放\"\n\
menu.medium = \"介质\"\n\
menu.view = \"视图\"\n\
menu.tools = \"工具\"\n\
menu.settings = \"设置\"\n\
menu.plugins = \"插件\"\n\
menu.help = \"帮助\"\n\
menu.quit = \"退出\"\n\
menu.open_files = \"打开文件…\"\n\
menu.add_dir = \"加入浏览器目录\"\n\
menu.play_pause = \"播放 / 暂停\"\n\
menu.prev = \"上一曲\"\n\
menu.next = \"下一曲\"\n\
menu.repeat = \"循环模式\"\n\
menu.shuffle = \"随机播放\"\n\
menu.vol_up = \"音量增大\"\n\
menu.vol_down = \"音量减小\"\n\
menu.panel_browser = \"浏览器面板\"\n\
menu.panel_cover = \"封面面板\"\n\
menu.panel_spectrum = \"频谱面板\"\n\
menu.lyrics = \"歌词\"\n\
menu.group = \"播放列表分组\"\n\
menu.eq = \"均衡器\"\n\
menu.compressor = \"压缩器\"\n\
menu.dsp = \"DSP 链\"\n\
menu.tag_edit = \"标签编辑\"\n\
menu.convert = \"格式转换\"\n\
menu.cover_mgmt = \"封面管理\"\n\
menu.output_device = \"输出设备\"\n\
menu.replaygain = \"ReplayGain\"\n\
menu.skin_select = \"皮肤选择\"\n\
menu.lang_select = \"语言选择\"\n\
menu.font = \"字体\"\n\
menu.about = \"关于\"\n\
menu.ctx_play = \"播放\"\n\
menu.ctx_remove = \"从播放列表移除\"\n\
menu.ctx_open = \"打开\"\n\
menu.ctx_add_dir = \"加入此目录\"\n\
menu.ctx_add_play = \"加入并播放\"\n\
menu.ctx_add_only = \"仅加入\"\n\
menu.ctx_reveal = \"在文件管理器中显示\"\n\
medium.none = \"关（原声）\"\n\
medium.tape_clear = \"磁带·清（Hi-Fi）\"\n\
medium.tape_white = \"磁带·白（清新）\"\n\
medium.tape_classic = \"磁带·经典\"\n\
medium.tape_aged = \"磁带·陈年\"\n\
medium.vinyl_clean = \"黑胶·净\"\n\
medium.vinyl_dynamic = \"黑胶·动态\"\n\
medium.vinyl_standard = \"黑胶·标准\"\n\
medium.vinyl_aged = \"黑胶·陈年\"\n\
medium.unknown = \"未知介质\"\n\
panel.current_track = \"当前曲目\"\n\
panel.playlist = \"播放列表\"\n\
panel.cover = \"专辑封面\"\n\
panel.lyrics = \"歌词\"\n\
panel.spectrum = \"频谱\"\n\
panel.level = \"电平\"\n\
panel.status = \"状态\"\n\
status.playing = \"[播]\"\n\
status.stopped = \"[停]\"\n\
metadata.artist_label = \"歌手\"\n\
metadata.album_label = \"专辑\"\n\
metadata.track_label = \"曲\"\n\
metadata.unknown_title = \"（未知标题）\"\n\
metadata.unknown_artist = \"（未知艺术家）\"\n\
metadata.unknown_album = \"（未知专辑）\"\n\
spectrum.low = \"低\"\n\
spectrum.mid = \"中\"\n\
spectrum.high = \"高\"\n\
repeat.list = \"列表\"\n\
repeat.single = \"单曲\"\n\
empty.dir = \"（空目录）\"\n\
empty.playlist = \"（空）双击浏览器中的文件加入并播放\"\n\
empty.lyrics = \"（无歌词——放置同名 .lrc 文件）\"\n\
cover.none = \"无封面\"\n\
msg.empty = \"（未播放——双击浏览器中的文件开始）\"\n\
msg.consecutive_fail = \"连续 10 首无法播放，已停止自动切换\"\n\
msg.added = \"已加入 {} 首\"\n\
msg.scanning = \"正在扫描目录并读取标签…\"\n\
msg.clear_confirm = \"再按一次 x 确认清空播放列表\"\n\
msg.not_yet = \"该功能开发中\"\n\
msg.dev_follow = \"输出设备跟随系统设置\"\n\
panel.oscilloscope = \"示波器\"\n\
search.esc_exit = \"Esc 退出\"\n\
search.esc_exit_search = \"无匹配（Esc 退出搜索）\"\n\
list.default = \"默认\"\n\
group.unknown_album = \"未知专辑\"\n\
group.track_count = \"{}首\"\n\
menu.ctx_collapse = \"折叠分组\"\n\
menu.ctx_expand = \"展开分组\"\n\
menu.ctx_expand_all = \"展开全部分组\"\n\
menu.clear_playlist = \"清空播放列表\"\n\
col.title = \"标题\"\n\
col.artist = \"艺术家\"\n\
col.dur = \"时长\"\n\
menu.modules = \"模块\"\n\
mod.transport = \"传输工具条\"\n\
mod.col_header = \"播放列表列头\"\n\
mod.status_tech = \"状态栏技术行\"\n\
mod.spec_axis = \"频谱坐标轴\"\n\
mod.group_rule = \"分组头下划线\"\n\
panel.bands = \"三桶频段\"\n\
dock.empty = \"无模块——视图 → 模块 加装\"\n\
menu.dock_close = \"关闭模块\"\n\
menu.dock_split_h = \"水平拆分加装\"\n\
menu.dock_split_v = \"垂直拆分加装\"\n\
menu.dock_change = \"更换为…\"\n\
menu.dock_back = \"返回\"\n\
cmd.execute = \"回车执行 · Esc 取消\"\n\
msg.unknown_cmd = \"未知命令：{}（help 查看）\"\n\
msg.usage_vol = \"用法：vol 0-100\"\n\
msg.usage_repeat = \"用法：repeat off|list|single\"\n\
msg.usage_bm_del = \"用法：bm-del 序号\"\n\
msg.usage_bm_jump = \"用法：bm-jump 序号\"\n\
msg.vol_set = \"音量：{}%\"\n\
msg.repeat_off = \"循环：关闭\"\n\
msg.repeat_list = \"循环：列表\"\n\
msg.repeat_single = \"循环：单曲\"\n\
msg.quit_hint = \"用 q 键退出\"\n\
msg.saved_m3u = \"已保存 {} 首到\"\n\
msg.m3u_empty = \"播放列表为空，无需保存\"\n\
msg.save_fail = \"保存失败\"\n\
msg.loaded = \"已加载 {} 首\"\n\
msg.load_fail = \"读取失败\"\n\
msg.state_corrupt = \"播放列表文件损坏：已备份为 {}，本次会话不会自动保存\"\n\
msg.state_corrupt_nobackup = \"播放列表文件损坏且自动备份失败：请立即手工复制保存原文件；本次会话不会自动保存\"\n\
msg.bm_added = \"已加书签\"\n\
msg.bm_updated = \"书签已更新\"\n\
msg.bm_deleted = \"已删书签：{}\"\n\
msg.bm_empty = \"暂无书签\"\n\
msg.bm_jump = \"跳到书签：{}\"\n\
msg.bm_no_file = \"书签文件不存在\"\n\
msg.bm_not_found = \"无此书签\"\n\
msg.preset_saved = \"布局已保存：{}\"\n\
msg.preset_loaded = \"布局已载入：{}\"\n\
msg.preset_deleted = \"布局已删除：{}\"\n\
msg.preset_not_found = \"没有该布局预设\"\n\
msg.preset_empty = \"暂无布局预设（preset-save 保存当前）\"\n\
menu.presets = \"布局预设\"\n\
msg.plugin_eq_missing = \"均衡器插件未加载 · 按 u 重新加载\"\n\
msg.plugin_comp_missing = \"压缩器插件未加载 · 按 u 重新加载\"\n\
msg.comp_threshold = \"阈值\"\n\
msg.comp_ratio = \"压缩比\"\n\
msg.comp_attack = \"启动\"\n\
msg.comp_release = \"释放\"\n\
panel.dspchain = \"DSP 链\"\n\
panel.gauge = \"指针表\"\n\
dsp.eq = \"均衡器\"\n\
dsp.comp = \"压缩器\"\n\
dsp.filter = \"滤波器\"\n\
dsp.bypassed = \"旁路\"\n\
dsp.active = \"激活\"\n\
panel.visualizer = \"可视化\"\n\
msg.comp_makeup = \"补偿\"\n\
menu.preset_save = \"保存当前布局\"\n\
mod.group_covers = \"分组封面\"\n\
panel.waveform = \"波形\"\n\
panel.filter = \"滤波器\"\n\
filter.cutoff = \"截止\"\n\
filter.resonance = \"谐振\"\n\
filter.off = \"关\"\n\
msg.no_visual = \"无可视化插件画面\"\n\
btn.ok = \"确定\"\n\
btn.volume = \"音量\"\n\
btn.shuffle = \"随机\"\n\
fkey.menu = \"菜单\"\n\
fkey.eq = \"均衡器\"\n\
fkey.help = \"帮助\"\n\
menu.add_bookmark = \"添加书签（当前位置）\"\n\
col.num = \"序号\"\n\
col.album = \"专辑\"\n\
browser.up = \"上级目录\"\n\
browser.home = \"主目录\"\n\
panel.about = \"关于\"\n\
about.brand = \"tuneux-max · FreeYouth\"\n\
popup.properties = \"属性\"\n\
prop.codec = \"编码\"\n\
prop.channels = \"声道\"\n\
prop.duration = \"时长\"\n\
prop.path = \"路径\"\n\
about.tagline = \"插件化图形音乐播放器\"\n\
about.plugins_cat = \"插件\"\n\
about.formats = \"支持 MP3 · FLAC · WAV · OGG · OPUS · WV · M4A · AAC · ALAC\"\n\
about.keys1 = \"空格 播放/暂停 · n/p 下一曲/上一曲 · ←→ ±5秒 · +/- 音量\"\n\
about.keys2 = \"b 浏览器 · c 封面 · l 歌词 · v 频谱/示波器 · g 分组 · a 加入 · / 搜索\"\n\
about.keys3 = \"1-8 菜单 · e 均衡器 · m 介质 · x两次 清空 · ? 关于 · q 退出\"\n\
about.deps = \"基于以下开源项目构建：\"\n\
panel.eq = \"均衡器\"\n\
panel.compressor = \"压缩器\"\n\
panel.skin = \"皮肤\"\n\
eq.preset = \"预设\"\n\
eq.on = \"EQ 开关\"\n\
eq.reset = \"重置\"\n\
about.license = \"本项目采用木兰宽松许可证 v2（MulanPSL-2.0）\"\n\
about.offline = \"纯离线 · 不收集任何数据\"\n\
menu.edit = \"编辑\"\n\
menu.audio = \"音效\"\n\
menu.add_selection = \"加入浏览器选中项\"\n\
menu.save_playlist = \"保存播放列表…\"\n\
menu.load_playlist = \"载入播放列表…\"\n\
menu.remove_selected = \"移除选中\"\n\
menu.search = \"搜索\"\n\
menu.properties = \"属性\"\n\
menu.seek_back = \"快退\"\n\
menu.seek_forward = \"快进\"\n\
menu.play_order = \"播放顺序\"\n\
order.sequential = \"顺序\"\n\
order.repeat_one = \"单曲循环\"\n\
order.repeat_all = \"列表循环\"\n\
order.shuffle_tracks = \"随机曲目\"\n\
order.shuffle_albums = \"随机专辑\"\n\
order.true_random = \"真随机\"\n\
menu.mute = \"静音\"\n\
menu.bookmarks = \"书签\"\n\
menu.bookmark_list = \"列表…\"\n\
menu.bookmark_delete = \"删除…\"\n\
menu.filter = \"滤波器\"\n\
menu.plugin_manager = \"插件管理…\"\n\
menu.command_bar = \"命令输入条\"\n\
menu.open_config_dir = \"打开配置目录\"\n\
menu.shortcuts = \"快捷键一览…\"\n\
menu.manual = \"使用手册\"\n\
menu.reload_plugins = \"重载插件\"\n\
plugins.dir = \"插件目录\"\n\
plugins.log = \"装载日志\"\n\
plugins.loaded = \"已装载\"\n\
plugins.not_loaded = \"未装载\"\n\
plugins.none = \"（无插件文件——把 .wasm 放入插件目录后重载）\"\n\
plugins.pending_empty = \"待确认：（空）\"\n\
msg.dir_open_fail = \"无法打开\"\n\
msg.manual_missing = \"使用手册随发行包附带（安装目录内 使用手册-tuneux-max.txt）\"\n\
bm.hint = \"Enter 跳转 · ✕ 删除 · Esc 关闭\"\n\
",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// zh_table 关键词条抽查（渲染层直接引用的锚点）。
    #[test]
    fn zh_table_spot_checks() {
        let t = zh_table();
        assert_eq!(t.get("menu.file"), Some("文件"));
        assert_eq!(t.get("panel.current_track"), Some("当前曲目"));
        assert_eq!(t.get("status.playing"), Some("[播]"));
        assert_eq!(t.get("medium.tape_classic"), Some("磁带·经典"));
        assert_eq!(t.get("list.default"), Some("默认"));
    }

    /// zh_table key 卫生：每个 key 都是干净的 `[a-z][a-z0-9_.]*` 形态。
    ///
    /// 守护内嵌字符串的续行符写法（`\n\` 多写一个反斜杠会把字面 `\n`
    /// 粘到下一个 key 前面——源码级 grep 看不到，只有运行时才暴露）。
    #[test]
    fn zh_table_keys_are_clean() {
        for k in zh_table().keys() {
            let shaped = k.starts_with(|c: char| c.is_ascii_lowercase())
                && k.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
            assert!(shaped, "key 形态异常: {k:?}");
        }
    }

    /// 配置 toml 往返：字段不丢、默认值兜底。
    #[test]
    fn config_toml_roundtrip() {
        let cfg = MaxConfig {
            lang: "en".to_string(),
            volume: 0.55,
            last_dir: Some(PathBuf::from("/music")),
            theme: "term".to_string(),
            font: "更纱黑体等宽".to_string(),
            repeat: tuneux_mediax::RepeatMode::List,
            view: tuneux_mediax::PlaylistView::ByAlbum,
            mod_over: std::collections::BTreeMap::new(),
            pl_cols: PlaylistCols::default(),
            dock: default_dock(),
            dock_presets: std::collections::BTreeMap::new(),
            bookmarks: tuneux_mediax::bookmark::BookmarkList::default(),
        };
        let text = toml::to_string_pretty(&cfg).expect("serialize");
        let back: MaxConfig = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.lang, "en");
        assert!((back.volume - 0.55).abs() < 1e-6);
        assert_eq!(back.last_dir, Some(PathBuf::from("/music")));
        assert_eq!(back.theme, "term");
        assert_eq!(back.font, "更纱黑体等宽");
        assert_eq!(back.repeat, tuneux_mediax::RepeatMode::List);
        assert_eq!(back.view, tuneux_mediax::PlaylistView::ByAlbum);
    }

    /// 旧配置兼容：含已废弃 modern 字段的配置文件正常加载（serde 忽略
    /// 未知字段，不报错不迁移——现代模式已删，该值无意义）。
    #[test]
    fn legacy_config_with_modern_field_still_loads() {
        let cfg: MaxConfig = toml::from_str("modern = true\n").unwrap();
        assert!((cfg.volume - 0.8).abs() < 1e-6, "其余字段默认值不受影响");
    }

    /// 旧配置文件带已删除字段（layout 等）仍可加载（serde 忽略未知字段）。
    #[test]
    fn legacy_config_with_removed_fields_loads() {
        let text = "lang = \"zh\"\nvolume = 0.7\nlayout = \"compact\"\nshow_browser = true\n";
        let cfg: MaxConfig = toml::from_str(text).expect("旧配置应可加载");
        assert!((cfg.volume - 0.7).abs() < 1e-6);
        assert_eq!(cfg.theme, "dos", "缺省主题应为 dos");
    }

    /// 空文件 / 损坏输入回退默认（Default 派生路径）。
    #[test]
    fn empty_config_falls_back_to_default() {
        let cfg: MaxConfig = toml::from_str("").expect("空串应得默认");
        assert_eq!(cfg.lang, "zh");
        assert_eq!(cfg.theme, "dos");
    }

    // —— 损坏配置保护（2026-10-13，与双 TUI 同源修复）——

    /// 损坏文件备份：生成 .corrupt.bak 且内容与原文件一致；
    /// 已有备份时不得覆盖（保留最早现场）。
    #[test]
    fn corrupt_file_backed_up_once() {
        let tmp = std::env::temp_dir().join("tuneux_max_test_corrupt_backup.toml");
        let bak = PathBuf::from(format!("{}.corrupt.bak", tmp.display()));
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&bak);
        std::fs::write(&tmp, "not = [valid").expect("写入损坏配置");
        let first = backup_corrupt_file(&tmp).expect("首次备份应成功");
        assert_eq!(first, bak);
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "not = [valid");
        // 第二次损坏内容不同，备份不得覆盖
        std::fs::write(&tmp, "other = [broken").expect("改写损坏配置");
        let second = backup_corrupt_file(&tmp).expect("已有备份仍返回路径");
        assert_eq!(
            std::fs::read_to_string(&second).unwrap(),
            "not = [valid",
            "已有备份不得被覆盖"
        );
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&bak);
    }

    /// 停写锁：置位后配置/播放状态保存整体跳过（不落盘、不建文件）。
    #[test]
    fn save_skipped_when_locked() {
        let tmp = std::env::temp_dir().join("tuneux_max_test_save_locked.toml");
        let _ = std::fs::remove_file(&tmp);
        let locked = AtomicBool::new(true);
        atomic_write_if_unlocked(&tmp, &MaxConfig::default(), &locked);
        assert!(!tmp.exists(), "锁定态不得创建/改写配置文件");
        // 播放状态维度同口径
        let ptmp = std::env::temp_dir().join("tuneux_max_test_save_locked_playlist.toml");
        let _ = std::fs::remove_file(&ptmp);
        atomic_write_if_unlocked(&ptmp, &tuneux_mediax::PlaylistState::default(), &locked);
        assert!(!ptmp.exists(), "锁定态不得创建/改写播放状态文件");
        // 未置位时正常落盘
        locked.store(false, Ordering::SeqCst);
        atomic_write_if_unlocked(&tmp, &MaxConfig::default(), &locked);
        assert!(tmp.exists(), "未锁定应正常落盘");
        let _ = std::fs::remove_file(&tmp);
    }
}
