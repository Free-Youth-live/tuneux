//! # 菜单栏：数据驱动静态表 + 渲染 + 动作执行
//!
//! 2026-10-14 菜单结构调整：顶层八栏（文件/编辑/播放/音效/视图/工具/设置/帮助），
//! 数字键 1–8 直开。菜单结构是静态表（[`menus`]），渲染器把表展开为 egui 菜单，
//! 勾选态由运行时快照（[`MenuSnap`]）计算；点击只收集动作、闭包外统一执行
//! （egui 所有权约束：表是静态数据，执行需 `&mut` 应用状态）。
//! 介质/模块/预设/皮肤/语言/字体/输出设备为运行时动态子菜单（[`DynMenu`]）。
//! 快捷键一览弹窗的内容也从本表生成，不另维护第二份键位清单。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui;
use egui::text::{LayoutJob, TextFormat};
use egui::{Pos2, Ui};

use tuneux_commonx::ui::UiAction;
use tuneux_commonx::I18n;
use tuneux_corex::PlaybackMedium;
use tuneux_mediax::{PlaylistItem, RepeatMode, ShuffleMode};

use super::{fmt_time, medium_key, mono, MaxAppV2, PlRow, FONT};
use crate::{dock, theme};

/// 菜单动作（点击收集、统一执行）。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum MenuAction {
    // —— 文件 ——
    OpenFiles,
    AddBrowserDir,
    AddBrowserSelection,
    SavePlaylist,
    LoadPlaylist,
    Quit,
    // —— 编辑 ——
    RemoveSelected,
    ClearPlaylist,
    Search,
    Properties,
    // —— 播放 ——
    PlayPause,
    PrevTrack,
    NextTrack,
    SeekBack,
    SeekForward,
    /// 播放顺序六档单选：写入（循环, 随机）定值组合（2026-10-14 定稿映射表）。
    SetPlayOrder(RepeatMode, ShuffleMode),
    VolumeUp,
    VolumeDown,
    ToggleMute,
    BookmarkAdd,
    BookmarkList,
    BookmarkDelete,
    // —— 音效 ——
    SetMedium(PlaybackMedium),
    ToggleEq,
    ToggleFilter,
    ToggleCompressor,
    ToggleReplayGain,
    /// 输出设备仅展示（输出跟随系统），点击给提示。
    OutputDeviceFollow,
    // —— 视图 ——
    ToggleBrowser,
    ToggleSpectrum,
    ToggleLyrics,
    ToggleCover,
    ToggleGroup,
    ToggleModule(dock::ModuleId),
    ToggleMod(&'static str),
    PresetSave,
    PresetLoad(String),
    // —— 设置 ——
    SetTheme(String),
    SetLang(String),
    SetFont(usize),
    OpenConfigDir,
    // —— 工具 ——
    PluginManager,
    TagEdit,
    Convert,
    CoverManage,
    CommandBar,
    // —— 帮助 ——
    ShowShortcuts,
    ShowManual,
    About,
    // —— 右键就近投影（动态目标由上下文注入，不入静态表）——
    /// 曲目行：播放（item = 底层索引）。
    PlayItem(usize),
    /// 曲目行：移除（item = 底层索引）。
    RemoveItem(usize),
    /// 曲目行：属性（播放列表行索引）。
    PropertiesRow(usize),
    /// 在文件管理器中显示。
    RevealPath(PathBuf),
    /// 浏览器目录行：进入。
    BrowserEnter(usize),
    /// 浏览器目录行：加入此目录。
    BrowserAddDir(PathBuf),
    /// 浏览器文件行：仅加入 / 加入并播放。
    BrowserAddFile(PathBuf, bool),
    /// 组头：折叠/展开切换。
    ToggleGroupCollapse(String),
    ExpandAllGroups,
    /// dock 叶：关闭模块。
    DockLeafClose(dock::ModuleId),
    /// dock 叶：在 target 处拆分插入 new（bool = 水平/垂直）。
    DockLeafSplit(dock::ModuleId, dock::ModuleId, bool),
    /// dock 叶：原位更换。
    DockLeafReplace(dock::ModuleId, dock::ModuleId),
}

/// 菜单表节点。
pub(super) enum MenuEntry {
    /// 普通项（enabled=false 为灰显占位，不隐藏）。
    Item {
        /// i18n key。
        label: &'static str,
        /// 快捷键标注（右段显示；空串 = 无）。
        shortcut: &'static str,
        action: MenuAction,
        enabled: bool,
    },
    /// 分隔线。
    Sep,
    /// 静态子菜单。
    Sub {
        label: &'static str,
        entries: &'static [MenuEntry],
    },
    /// 运行时动态子菜单。
    Dyn(DynMenu),
}

/// 动态子菜单种类（内容由运行时快照展开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DynMenu {
    Medium,
    Modules,
    Presets,
    OutputDevices,
    Skins,
    Langs,
    Fonts,
}

/// 一个顶层菜单（标题 i18n key + 条目）。
pub(super) struct TopMenu {
    pub title: &'static str,
    pub entries: &'static [MenuEntry],
}

impl MenuEntry {
    const fn item(label: &'static str, shortcut: &'static str, action: MenuAction) -> Self {
        Self::Item {
            label,
            shortcut,
            action,
            enabled: true,
        }
    }
    const fn disabled(label: &'static str, action: MenuAction) -> Self {
        Self::Item {
            label,
            shortcut: "",
            action,
            enabled: false,
        }
    }
}

/// 静态菜单表：八栏，序即数字键 1–8 的映射序。
///
/// 播放顺序六档写入表（2026-10-14 定稿）：随机三档的循环定值取「列表」
/// （一轮播完重洗再播的不间断预期）；r/s 键的循环切换行为不变。
pub(super) fn menus() -> &'static [TopMenu] {
    static MENUS: &[TopMenu] = &[
        TopMenu {
            title: "menu.file",
            entries: &[
                // 快捷键标注渲染时按平台替换为 ⌘O / Ctrl+O
                MenuEntry::item("menu.open_files", "Ctrl+O", MenuAction::OpenFiles),
                MenuEntry::item("menu.add_dir", "", MenuAction::AddBrowserDir),
                MenuEntry::item("menu.add_selection", "a", MenuAction::AddBrowserSelection),
                MenuEntry::Sep,
                MenuEntry::item("menu.save_playlist", "", MenuAction::SavePlaylist),
                MenuEntry::item("menu.load_playlist", "", MenuAction::LoadPlaylist),
                MenuEntry::Sep,
                MenuEntry::item("menu.quit", "q", MenuAction::Quit),
            ],
        },
        TopMenu {
            title: "menu.edit",
            entries: &[
                MenuEntry::item("menu.remove_selected", "d", MenuAction::RemoveSelected),
                MenuEntry::item("menu.clear_playlist", "x", MenuAction::ClearPlaylist),
                MenuEntry::Sep,
                MenuEntry::item("menu.search", "/", MenuAction::Search),
                MenuEntry::item("menu.properties", "i", MenuAction::Properties),
            ],
        },
        TopMenu {
            title: "menu.play",
            entries: &[
                MenuEntry::item("menu.play_pause", "Space", MenuAction::PlayPause),
                MenuEntry::item("menu.prev", "p", MenuAction::PrevTrack),
                MenuEntry::item("menu.next", "n", MenuAction::NextTrack),
                MenuEntry::item("menu.seek_back", "←", MenuAction::SeekBack),
                MenuEntry::item("menu.seek_forward", "→", MenuAction::SeekForward),
                MenuEntry::Sep,
                MenuEntry::Sub {
                    label: "menu.play_order",
                    entries: &[
                        MenuEntry::item(
                            "order.sequential",
                            "",
                            MenuAction::SetPlayOrder(RepeatMode::Off, ShuffleMode::Off),
                        ),
                        MenuEntry::item(
                            "order.repeat_one",
                            "",
                            MenuAction::SetPlayOrder(RepeatMode::Single, ShuffleMode::Off),
                        ),
                        MenuEntry::item(
                            "order.repeat_all",
                            "",
                            MenuAction::SetPlayOrder(RepeatMode::List, ShuffleMode::Off),
                        ),
                        MenuEntry::item(
                            "order.shuffle_tracks",
                            "",
                            MenuAction::SetPlayOrder(RepeatMode::List, ShuffleMode::Tracks),
                        ),
                        MenuEntry::item(
                            "order.shuffle_albums",
                            "",
                            MenuAction::SetPlayOrder(RepeatMode::List, ShuffleMode::Albums),
                        ),
                        MenuEntry::item(
                            "order.true_random",
                            "",
                            MenuAction::SetPlayOrder(RepeatMode::List, ShuffleMode::TrueRandom),
                        ),
                    ],
                },
                MenuEntry::Sep,
                MenuEntry::item("menu.vol_up", "+", MenuAction::VolumeUp),
                MenuEntry::item("menu.vol_down", "-", MenuAction::VolumeDown),
                MenuEntry::item("menu.mute", "", MenuAction::ToggleMute),
                MenuEntry::Sep,
                MenuEntry::Sub {
                    label: "menu.bookmarks",
                    entries: &[
                        MenuEntry::item("menu.add_bookmark", "", MenuAction::BookmarkAdd),
                        MenuEntry::item("menu.bookmark_list", "", MenuAction::BookmarkList),
                        MenuEntry::item("menu.bookmark_delete", "", MenuAction::BookmarkDelete),
                    ],
                },
            ],
        },
        TopMenu {
            title: "menu.audio",
            entries: &[
                MenuEntry::Dyn(DynMenu::Medium),
                MenuEntry::Sep,
                MenuEntry::item("menu.eq", "e", MenuAction::ToggleEq),
                MenuEntry::item("menu.filter", "", MenuAction::ToggleFilter),
                MenuEntry::item("menu.compressor", "", MenuAction::ToggleCompressor),
                MenuEntry::Sep,
                MenuEntry::item("menu.replaygain", "", MenuAction::ToggleReplayGain),
                MenuEntry::Sep,
                MenuEntry::Dyn(DynMenu::OutputDevices),
            ],
        },
        TopMenu {
            title: "menu.view",
            entries: &[
                MenuEntry::item("menu.panel_browser", "b", MenuAction::ToggleBrowser),
                MenuEntry::item("menu.panel_spectrum", "v", MenuAction::ToggleSpectrum),
                MenuEntry::item("menu.lyrics", "l", MenuAction::ToggleLyrics),
                MenuEntry::item("menu.panel_cover", "c", MenuAction::ToggleCover),
                MenuEntry::item("menu.group", "g", MenuAction::ToggleGroup),
                MenuEntry::Sep,
                MenuEntry::Dyn(DynMenu::Modules),
                MenuEntry::Dyn(DynMenu::Presets),
            ],
        },
        TopMenu {
            title: "menu.tools",
            entries: &[
                MenuEntry::item("menu.plugin_manager", "", MenuAction::PluginManager),
                MenuEntry::Sep,
                MenuEntry::disabled("menu.tag_edit", MenuAction::TagEdit),
                MenuEntry::disabled("menu.convert", MenuAction::Convert),
                MenuEntry::disabled("menu.cover_mgmt", MenuAction::CoverManage),
                MenuEntry::Sep,
                MenuEntry::item("menu.command_bar", ":", MenuAction::CommandBar),
            ],
        },
        TopMenu {
            title: "menu.settings",
            entries: &[
                MenuEntry::Dyn(DynMenu::Skins),
                MenuEntry::Dyn(DynMenu::Langs),
                MenuEntry::Dyn(DynMenu::Fonts),
                MenuEntry::Sep,
                MenuEntry::item("menu.open_config_dir", "", MenuAction::OpenConfigDir),
            ],
        },
        TopMenu {
            title: "menu.help",
            entries: &[
                MenuEntry::item("menu.shortcuts", "", MenuAction::ShowShortcuts),
                MenuEntry::item("menu.manual", "", MenuAction::ShowManual),
                MenuEntry::Sep,
                MenuEntry::item("menu.about", "?", MenuAction::About),
            ],
        },
    ];
    MENUS
}

/// 播放顺序勾选判定：shuffle 优先于 repeat——混合态（repeat 非写入表定值
/// 且 shuffle 非关）勾随机侧；shuffle 关时按 repeat 勾前三档。
fn play_order_checked(
    repeat: RepeatMode,
    shuffle: ShuffleMode,
    item: (RepeatMode, ShuffleMode),
) -> bool {
    let (r, s) = item;
    if s.is_on() {
        shuffle == s
    } else {
        shuffle == ShuffleMode::Off && repeat == r
    }
}

/// 快捷键标注（打开文件按平台显示 ⌘O / Ctrl+O；其余取表内字面值）。
pub(super) fn shortcut_text(action: &MenuAction, raw: &str) -> String {
    if matches!(action, MenuAction::OpenFiles) {
        if cfg!(target_os = "macos") {
            "⌘O".to_string()
        } else {
            "Ctrl+O".to_string()
        }
    } else {
        raw.to_string()
    }
}

/// 渲染期运行时快照（渲染器是纯函数，闭包内不借应用状态）。
pub(super) struct MenuSnap {
    repeat: RepeatMode,
    shuffle: ShuffleMode,
    rg_on: bool,
    muted: bool,
    cur_medium: PlaybackMedium,
    /// dock 模块开关（与 dock::ModuleId::ALL 同序对齐）。
    dock_has: Vec<(dock::ModuleId, bool)>,
    /// 皮肤模块加装项（key, 显示名, 开关）。
    mods: Vec<(&'static str, String, bool)>,
    presets: Vec<String>,
    /// 皮肤清单（id, 显示名）：内置 + 皮肤插件。
    themes: Vec<(String, String)>,
    cur_theme: String,
    langs: Vec<(String, String)>,
    cur_lang: String,
    fonts: Vec<String>,
    font_idx: usize,
    devices: Vec<String>,
}

impl MenuSnap {
    fn dock_on(&self, m: dock::ModuleId) -> bool {
        self.dock_has
            .iter()
            .find(|(id, _)| *id == m)
            .map(|(_, on)| *on)
            .unwrap_or(false)
    }
}

/// 勾选态（None = 不显示勾选位）。
fn checked_of(snap: &MenuSnap, a: &MenuAction) -> Option<bool> {
    match a {
        MenuAction::SetPlayOrder(r, s) => {
            Some(play_order_checked(snap.repeat, snap.shuffle, (*r, *s)))
        }
        MenuAction::ToggleReplayGain => Some(snap.rg_on),
        MenuAction::ToggleMute => Some(snap.muted),
        MenuAction::ToggleBrowser => Some(snap.dock_on(dock::ModuleId::Browser)),
        MenuAction::ToggleSpectrum => Some(snap.dock_on(dock::ModuleId::Spectrum)),
        MenuAction::ToggleLyrics => Some(snap.dock_on(dock::ModuleId::Lyrics)),
        MenuAction::ToggleCover => Some(snap.dock_on(dock::ModuleId::Cover)),
        MenuAction::ToggleModule(m) => Some(snap.dock_on(*m)),
        MenuAction::ToggleMod(key) => snap
            .mods
            .iter()
            .find(|(k, _, _)| k == key)
            .map(|(_, _, on)| *on),
        MenuAction::SetMedium(m) => Some(*m == snap.cur_medium),
        MenuAction::SetTheme(id) => Some(*id == snap.cur_theme),
        MenuAction::SetLang(id) => Some(*id == snap.cur_lang),
        MenuAction::SetFont(i) => Some(*i == snap.font_idx),
        _ => None,
    }
}

/// 动态子菜单的标题 i18n key。
fn dyn_label(d: DynMenu) -> &'static str {
    match d {
        DynMenu::Medium => "menu.medium",
        DynMenu::Modules => "menu.modules",
        DynMenu::Presets => "menu.presets",
        DynMenu::OutputDevices => "menu.output_device",
        DynMenu::Skins => "menu.skin_select",
        DynMenu::Langs => "menu.lang_select",
        DynMenu::Fonts => "menu.font",
    }
}

/// 菜单项文字（勾选前缀 + 标签 + 快捷键标注）。
fn item_text(
    i18n: &I18n,
    snap: &MenuSnap,
    label: &str,
    shortcut: &str,
    action: &MenuAction,
) -> String {
    let prefix = match checked_of(snap, action) {
        Some(true) => "√ ",
        Some(false) => "  ",
        None => "",
    };
    let sc = shortcut_text(action, shortcut);
    if sc.is_empty() {
        format!("{prefix}{}", i18n.t(label))
    } else {
        format!("{prefix}{}  {sc}", i18n.t(label))
    }
}

/// 普通菜单项：点击收集动作并关菜单。
fn push_item(ui: &mut Ui, text: String, action: MenuAction, acts: &mut Vec<MenuAction>) {
    if ui.button(text).clicked() {
        acts.push(action);
        ui.close();
    }
}

/// 递归渲染菜单表（静态项 / 分隔线 / 静态子菜单 / 动态子菜单）。
fn render_entries(
    ui: &mut Ui,
    i18n: &I18n,
    snap: &MenuSnap,
    entries: &[MenuEntry],
    acts: &mut Vec<MenuAction>,
) {
    for e in entries {
        match e {
            MenuEntry::Sep => {
                ui.separator();
            }
            MenuEntry::Item {
                label,
                shortcut,
                action,
                enabled,
            } => {
                let text = item_text(i18n, snap, label, shortcut, action);
                if *enabled {
                    push_item(ui, text, action.clone(), acts);
                } else {
                    // 占位项灰显不隐藏
                    ui.add_enabled(false, egui::Button::new(text));
                }
            }
            MenuEntry::Sub { label, entries } => {
                ui.menu_button(i18n.t(label).into_owned(), |ui| {
                    render_entries(ui, i18n, snap, entries, acts);
                });
            }
            MenuEntry::Dyn(d) => {
                // 字体子菜单：目录仅内嵌一款时隐藏（无谓的单项菜单）；
                // 用户向 fonts/ 投放自定义字体后目录 >1 款，菜单自动现身。
                if !matches!(d, DynMenu::Fonts) || snap.fonts.len() > 1 {
                    render_dyn(ui, i18n, snap, *d, acts);
                }
            }
        }
    }
}

/// 动态子菜单渲染（内容来自运行时快照）。
fn render_dyn(ui: &mut Ui, i18n: &I18n, snap: &MenuSnap, d: DynMenu, acts: &mut Vec<MenuAction>) {
    let title = i18n.t(dyn_label(d)).into_owned();
    ui.menu_button(title, |ui| match d {
        DynMenu::Medium => {
            for &m in PlaybackMedium::ALL.iter() {
                let prefix = if m == snap.cur_medium { "√ " } else { "  " };
                push_item(
                    ui,
                    format!("{prefix}{}", i18n.t(medium_key(m))),
                    MenuAction::SetMedium(m),
                    acts,
                );
            }
        }
        DynMenu::Modules => {
            for (key, label, on) in &snap.mods {
                let prefix = if *on { "√ " } else { "  " };
                push_item(
                    ui,
                    format!("{prefix}{label}"),
                    MenuAction::ToggleMod(key),
                    acts,
                );
            }
            ui.separator();
            for m in dock::ModuleId::ALL {
                let prefix = if snap.dock_on(m) { "√ " } else { "  " };
                let label = i18n.t(m.label_key()).into_owned();
                push_item(
                    ui,
                    format!("{prefix}{label}"),
                    MenuAction::ToggleModule(m),
                    acts,
                );
            }
        }
        DynMenu::Presets => {
            push_item(
                ui,
                i18n.t("menu.preset_save").into_owned(),
                MenuAction::PresetSave,
                acts,
            );
            if snap.presets.is_empty() {
                ui.weak(i18n.t("msg.preset_empty").into_owned());
            }
            for nm in &snap.presets {
                push_item(ui, nm.clone(), MenuAction::PresetLoad(nm.clone()), acts);
            }
        }
        DynMenu::OutputDevices => {
            // 输出跟随系统：设备名是信息项，点击给提示（沿用旧菜单口径）
            for dev in &snap.devices {
                push_item(ui, dev.clone(), MenuAction::OutputDeviceFollow, acts);
            }
        }
        DynMenu::Skins => {
            for (id, name) in &snap.themes {
                let prefix = if *id == snap.cur_theme { "√ " } else { "  " };
                push_item(
                    ui,
                    format!("{prefix}{name}"),
                    MenuAction::SetTheme(id.clone()),
                    acts,
                );
            }
        }
        DynMenu::Langs => {
            for (id, name) in &snap.langs {
                let prefix = if *id == snap.cur_lang { "√ " } else { "  " };
                push_item(
                    ui,
                    format!("{prefix}{name}"),
                    MenuAction::SetLang(id.clone()),
                    acts,
                );
            }
        }
        DynMenu::Fonts => {
            for (i, name) in snap.fonts.iter().enumerate() {
                let prefix = if i == snap.font_idx { "√ " } else { "  " };
                push_item(ui, format!("{prefix}{name}"), MenuAction::SetFont(i), acts);
            }
        }
    });
}

/// 收集表内快捷键项（快捷键一览弹窗用；递归静态子菜单，动态子菜单无快捷键）。
fn collect_shortcuts(entries: &[MenuEntry], out: &mut Vec<(String, &'static str)>) {
    for e in entries {
        match e {
            MenuEntry::Item {
                label,
                shortcut,
                action,
                ..
            } => {
                let sc = shortcut_text(action, shortcut);
                if !sc.is_empty() {
                    out.push((sc, label));
                }
            }
            MenuEntry::Sub { entries, .. } => collect_shortcuts(entries, out),
            _ => {}
        }
    }
}

/// 插件管理页行（文件 + 装载状态 + 核查日志信任级）。
pub(super) struct PluginRow {
    pub file: String,
    /// Some = 运行时确知（均衡器/压缩器）；None = 看核查日志。
    pub loaded: Option<bool>,
    /// 核查日志最后一条的信任级（无记录 = "-"）。
    pub trust: String,
}

impl MaxAppV2 {
    /// 渲染快照（菜单闭包内只读这些数据）。
    fn menu_snap(&self) -> MenuSnap {
        let dock_has = dock::ModuleId::ALL
            .iter()
            .map(|&m| (m, self.config.dock.as_ref().is_some_and(|d| d.contains(m))))
            .collect();
        let mod_entries = self.mod_entries();
        let mods = mod_entries
            .into_iter()
            .map(|(k, label)| (k, label, self.mod_on(k)))
            .collect();
        let mut themes: Vec<(String, String)> = theme::builtin_list()
            .into_iter()
            .map(|(id, name)| (id.to_string(), name.to_string()))
            .collect();
        themes.extend(
            self.skin_plugins
                .iter()
                .map(|(n, _)| (n.clone(), n.clone())),
        );
        MenuSnap {
            repeat: self.config.repeat,
            shuffle: self.playlist.shuffle_mode(),
            rg_on: self.replay_gain,
            muted: self.volume_before_mute.is_some(),
            cur_medium: self
                .engine
                .as_ref()
                .map(|e| e.medium())
                .unwrap_or(PlaybackMedium::None),
            dock_has,
            mods,
            presets: self.config.dock_presets.keys().cloned().collect(),
            themes,
            cur_theme: self.config.theme.clone(),
            langs: tuneux_commonx::scan_langs(),
            cur_lang: self.config.lang.clone(),
            fonts: self.fonts.iter().map(|f| f.name.clone()).collect(),
            font_idx: self.font_idx,
            devices: self.output_devices.clone(),
        }
    }

    /// 菜单栏（1 行：数字弱化 + 标题；数字键 1–8 直开）。
    pub(super) fn render_menu_bar(&mut self, ctx: &egui::Context) {
        let snap = self.menu_snap();
        let pal = self.palette.clone();
        let i18n = &self.i18n;

        let mut acts: Vec<MenuAction> = Vec::new();
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
                egui::MenuBar::new().ui(ui, |ui| {
                    bar_id = Some(ui.id());
                    for (idx, top) in menus().iter().enumerate() {
                        let title = i18n.t(top.title).into_owned();
                        let mut job = LayoutJob::default();
                        job.append(
                            &format!("{} ", idx + 1),
                            0.0,
                            TextFormat::simple(mono(FONT), pal.fg_weak),
                        );
                        job.append(
                            &format!("{title}  "),
                            0.0,
                            TextFormat::simple(mono(FONT), pal.fg),
                        );
                        let label_text = format!("{} {title}  ", idx + 1);
                        let r = ui.menu_button(job, |ui| {
                            render_entries(ui, i18n, &snap, top.entries, &mut acts);
                        });
                        rects.push((label_text, r.response.rect.left_bottom()));
                    }
                });
            });

        // Cmd/Ctrl+O 打开文件（三平台：COMMAND 自动映射 mac=Cmd / 其余=Ctrl）
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O)) {
            self.pending_open_files = true;
        }
        if self.pending_open_files {
            self.pending_open_files = false;
            self.open_files_dialog(ctx);
        }
        // 数字键 1-8：程序化打开对应菜单（BarState 直设，等效点击；
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

        for a in acts {
            self.exec_menu_action(a, ctx);
        }
    }

    /// 菜单动作统一执行（全枚举匹配：新增动作必须在此落地，编译期强制）。
    pub(super) fn exec_menu_action(&mut self, a: MenuAction, ctx: &egui::Context) {
        match a {
            MenuAction::OpenFiles => self.open_files_dialog(ctx),
            MenuAction::AddBrowserDir => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.enqueue_scan(dir);
                }
                // 原生对话框关闭后取回窗口焦点（macOS 有时不自动归还）
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            MenuAction::AddBrowserSelection => self.execute(UiAction::AddCurrent),
            MenuAction::SavePlaylist => self.save_m3u(),
            MenuAction::LoadPlaylist => self.load_m3u(None),
            MenuAction::Quit => self.execute(UiAction::Quit),
            MenuAction::RemoveSelected => self.execute(UiAction::DeleteSelected),
            MenuAction::ClearPlaylist => self.execute(UiAction::ClearPlaylist),
            MenuAction::Search => self.execute(UiAction::SearchStart),
            MenuAction::Properties => self.open_properties_for_selection(),
            MenuAction::PlayPause => self.execute(UiAction::PlayToggle),
            MenuAction::PrevTrack => self.execute(UiAction::PrevTrack),
            MenuAction::NextTrack => self.execute(UiAction::NextTrack),
            MenuAction::SeekBack => self.execute(UiAction::SeekBack(5.0)),
            MenuAction::SeekForward => self.execute(UiAction::SeekForward(5.0)),
            MenuAction::SetPlayOrder(r, s) => {
                self.config.repeat = r;
                self.playlist.set_shuffle_mode(s);
                // 预载目标随新策略刷新（与 r/s 键同口径）
                self.refresh_preload();
            }
            MenuAction::VolumeUp => self.execute(UiAction::VolumeUp),
            MenuAction::VolumeDown => self.execute(UiAction::VolumeDown),
            MenuAction::ToggleMute => self.toggle_mute(),
            MenuAction::BookmarkAdd => self.add_bookmark_now(),
            MenuAction::BookmarkList | MenuAction::BookmarkDelete => {
                self.bm_sel = 0;
                self.bookmarks_visible = true;
            }
            MenuAction::SetMedium(m) => {
                if let Some(e) = &self.engine {
                    e.set_medium(m);
                }
            }
            MenuAction::ToggleEq => self.execute(UiAction::ToggleEq),
            MenuAction::ToggleFilter => self.toggle_dock_module(dock::ModuleId::Filter),
            MenuAction::ToggleCompressor => self.toggle_dock_module(dock::ModuleId::Comp),
            MenuAction::ToggleReplayGain => {
                self.replay_gain = !self.replay_gain;
                if let Some(e) = &self.engine {
                    e.set_replay_gain_enabled(self.replay_gain);
                }
            }
            MenuAction::OutputDeviceFollow => {
                let m = self.i18n.t("msg.dev_follow").into_owned();
                self.flash_msg(m);
            }
            MenuAction::ToggleBrowser => self.execute(UiAction::ToggleBrowser),
            MenuAction::ToggleSpectrum => self.execute(UiAction::ToggleSpectrum),
            MenuAction::ToggleLyrics => self.execute(UiAction::ToggleLyrics),
            MenuAction::ToggleCover => self.execute(UiAction::ToggleCover),
            MenuAction::ToggleGroup => self.execute(UiAction::ToggleGroupView),
            MenuAction::ToggleModule(m) => self.toggle_dock_module(m),
            MenuAction::ToggleMod(key) => self.toggle_mod(key),
            MenuAction::PresetSave => self.preset_save(None),
            MenuAction::PresetLoad(name) => self.preset_load(&name),
            MenuAction::SetTheme(id) => {
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
            MenuAction::SetLang(id) => {
                self.config.lang = id.clone();
                self.i18n = tuneux_commonx::build_i18n(&id, crate::config::zh_table());
            }
            MenuAction::SetFont(i) => {
                self.font_idx = i;
                self.fonts_dirty = true;
                if let Some(f) = self.fonts.get(i) {
                    self.config.font = f.name.clone();
                }
            }
            MenuAction::OpenConfigDir => self.open_config_dir(),
            MenuAction::PluginManager => {
                self.plugins_rows = self.plugin_rows();
                self.plugins_visible = true;
            }
            // 灰显占位项不会触发；分支存在只为全枚举完备
            MenuAction::TagEdit | MenuAction::Convert | MenuAction::CoverManage => {
                let m = self.i18n.t("msg.not_yet").into_owned();
                self.flash_msg(m);
            }
            MenuAction::CommandBar => self.execute(UiAction::CommandStart),
            MenuAction::ShowShortcuts => self.shortcuts_visible = !self.shortcuts_visible,
            MenuAction::ShowManual => self.open_manual(),
            MenuAction::About => self.execute(UiAction::About),
            // —— 右键就近投影（动态目标）——
            MenuAction::PlayItem(i) => {
                if i < self.playlist.items().len() {
                    self.play_item(i);
                }
            }
            MenuAction::RemoveItem(i) => self.remove_playlist_item(i),
            MenuAction::PropertiesRow(r) => {
                self.properties_row = Some(r);
                self.properties_visible = true;
            }
            MenuAction::RevealPath(p) => self.reveal_in_file_manager(&p),
            MenuAction::BrowserEnter(i) => {
                if i < self.browser.entries().len() {
                    self.browser.select(i);
                    self.browser.enter_selected();
                }
            }
            MenuAction::BrowserAddDir(p) => self.enqueue_scan(p),
            MenuAction::BrowserAddFile(p, play) => {
                if play {
                    if let Some(idx) = self.add_file_sync(&p) {
                        self.play_item(idx);
                    }
                } else {
                    self.playlist.add(PlaylistItem {
                        path: p,
                        album: None,
                        track_number: None,
                        cue: None,
                    });
                }
            }
            MenuAction::ToggleGroupCollapse(album) => self.toggle_collapse(&album),
            MenuAction::ExpandAllGroups => self.collapsed.clear(),
            MenuAction::DockLeafClose(m) => {
                if let Some(tree) = self.config.dock.take() {
                    self.config.dock = tree.prune(m);
                }
            }
            MenuAction::DockLeafSplit(t, new, sbs) => {
                if let Some(tree) = self.config.dock.as_mut() {
                    tree.split_insert(t, new, sbs);
                }
            }
            MenuAction::DockLeafReplace(t, new) => {
                if let Some(tree) = self.config.dock.as_mut() {
                    tree.replace_leaf(t, new);
                }
            }
        }
    }

    /// 打开文件对话框（菜单与 Cmd/Ctrl+O 共用）。
    fn open_files_dialog(&mut self, ctx: &egui::Context) {
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

    /// 静音切换（与现代顶栏静音按钮同一状态通道）。
    fn toggle_mute(&mut self) {
        match self.volume_before_mute.take() {
            Some(v) => {
                if let Some(e) = &self.engine {
                    e.send(tuneux_corex::AudioCmd::SetVolume(v));
                    self.config.volume = v;
                }
            }
            None => {
                let vol = self
                    .engine
                    .as_ref()
                    .map(|e| e.volume())
                    .unwrap_or(self.config.volume);
                if vol > 0.0 {
                    self.volume_before_mute = Some(vol);
                    if let Some(e) = &self.engine {
                        e.send(tuneux_corex::AudioCmd::SetVolume(0.0));
                    }
                }
            }
        }
    }

    /// 平台打开路径（macOS open / Windows explorer / 其他 xdg-open）。
    /// 返回是否成功发起；失败由调用方退化为路径提示。
    fn open_path_with_os(path: &Path) -> bool {
        let cmd = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };
        Self::spawn_detached(std::process::Command::new(cmd).arg(path))
    }

    /// 后台发起子进程（静默标准流；仅判断是否成功发起）。
    fn spawn_detached(cmd: &mut std::process::Command) -> bool {
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    }

    /// 在文件管理器中显示并选中（macOS reveal / Windows 选中 /
    /// 其他打开所在目录）；发起失败退化为路径提示。
    fn reveal_in_file_manager(&mut self, path: &Path) {
        if !Self::reveal_path_with_os(path) {
            let m = format!("{}：{}", self.i18n.t("msg.dir_open_fail"), path.display());
            self.flash_msg(m);
        }
    }

    /// 平台 reveal 调用（返回是否成功发起）。
    fn reveal_path_with_os(path: &Path) -> bool {
        #[cfg(target_os = "macos")]
        {
            Self::spawn_detached(std::process::Command::new("open").arg("-R").arg(path))
        }
        #[cfg(target_os = "windows")]
        {
            Self::spawn_detached(
                std::process::Command::new("explorer").arg(format!("/select,{}", path.display())),
            )
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            match path.parent() {
                Some(dir) => Self::open_path_with_os(dir),
                None => false,
            }
        }
    }

    /// 移除指定条目（右键行删除；选中行钳位与 d 键同口径）。
    fn remove_playlist_item(&mut self, item: usize) {
        if item >= self.playlist.items().len() {
            return;
        }
        self.playlist.remove(item);
        let after = self.build_rows();
        if let Some(ri) = self.ui.playlist_selected {
            self.ui.playlist_selected = if after.is_empty() {
                None
            } else {
                Some(ri.min(after.len() - 1))
            };
        }
    }

    /// 打开配置目录（失败退化为路径提示）。
    fn open_config_dir(&mut self) {
        let cfg = crate::config::config_path();
        let dir = cfg.parent().map(|p| p.to_path_buf()).unwrap_or(cfg);
        if !Self::open_path_with_os(&dir) {
            let m = format!("{}：{}", self.i18n.t("msg.dir_open_fail"), dir.display());
            self.flash_msg(m);
        }
    }

    /// 使用手册：exe 同目录 / macOS 包内 Resources 查找，找到则打开，
    /// 找不到给路径提示。
    fn open_manual(&mut self) {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe() {
            if let Some(d) = exe.parent() {
                candidates.push(d.join("使用手册-tuneux-max.txt"));
                candidates.push(d.join("../Resources/使用手册-tuneux-max.txt"));
            }
        }
        match candidates.iter().find(|p| p.exists()) {
            Some(p) if Self::open_path_with_os(p) => {}
            Some(p) => self.flash_msg(p.display().to_string()),
            None => {
                let m = self.i18n.t("msg.manual_missing").into_owned();
                self.flash_msg(m);
            }
        }
    }

    /// 保存播放列表为 m3u（浏览器当前目录 playlist.m3u；与 :save-m3u 同语义）。
    pub(super) fn save_m3u(&mut self) {
        if self.playlist.items().is_empty() {
            let m = self.i18n.t("msg.m3u_empty").into_owned();
            self.flash_msg(m);
            return;
        }
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

    /// 载入 m3u（None = 浏览器当前目录 playlist.m3u；与 :load-m3u 同语义）。
    pub(super) fn load_m3u(&mut self, path: Option<PathBuf>) {
        let path = path.unwrap_or_else(|| self.browser.cwd().join("playlist.m3u"));
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

    /// 跳转书签（1 基序号；与 :bm-jump 同语义，书签弹窗 Enter/点击共用）。
    pub(super) fn bookmark_jump(&mut self, nm: usize) {
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
                let seek =
                    b.position_secs + b.cue_start_ms.map(|m| m as f64 / 1000.0).unwrap_or(0.0);
                if let Some(e) = &self.engine {
                    e.send(tuneux_corex::AudioCmd::Seek(seek));
                }
                let m = self.i18n.t("msg.bm_jump").replace("{}", &nm.to_string());
                self.flash_msg(m);
            }
        }
    }

    /// 打开属性弹窗：播放列表选中行优先，无选中回落当前播放曲。
    pub(super) fn open_properties_for_selection(&mut self) {
        let rows = self.build_rows();
        let row = self
            .ui
            .playlist_selected
            .filter(|&r| matches!(rows.get(r), Some(PlRow::Track { .. })))
            .or_else(|| {
                let cur = self.playlist.current_index()?;
                rows.iter()
                    .position(|r| matches!(r, PlRow::Track { item, .. } if *item == cur))
            });
        if let Some(r) = row {
            self.properties_row = Some(r);
            self.properties_visible = true;
        }
    }

    /// 插件管理页行快照：plugins/ 目录 .wasm 清单 + 运行时装载状态 +
    /// 核查日志最后一条信任级（打开/重载时刷新，避免每帧读盘）。
    fn plugin_rows(&self) -> Vec<PluginRow> {
        let dir = crate::config::plugins_dir();
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        let p = e.path();
                        if p.extension().and_then(|x| x.to_str()) == Some("wasm") {
                            p.file_name().and_then(|n| n.to_str()).map(|s| s.to_owned())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        // 核查日志：同一插件取最后一条记录
        let mut trust: HashMap<String, String> = HashMap::new();
        if let Ok(content) = std::fs::read_to_string(crate::config::plugin_log_path()) {
            for line in content.lines() {
                let mut id = None;
                let mut tr = None;
                for kv in line.split(' ') {
                    if let Some(v) = kv.strip_prefix("plugin=") {
                        id = Some(v.to_string());
                    }
                    if let Some(v) = kv.strip_prefix("trust=") {
                        tr = Some(v.to_string());
                    }
                }
                if let (Some(i), Some(t)) = (id, tr) {
                    trust.insert(i, t);
                }
            }
        }
        files
            .iter()
            .map(|f| {
                let stem = f.strip_suffix(".wasm").unwrap_or(f.as_str());
                let (loaded, jid) = match stem {
                    "equalizer" => (
                        Some(self.eq_plugin.is_some()),
                        tuneux_pinx::EQ_ID.to_string(),
                    ),
                    "compressor" => (
                        Some(self.comp_plugin.is_some()),
                        tuneux_pinx::COMP_ID.to_string(),
                    ),
                    s if s.starts_with("皮肤-") => (None, format!("tuneux-skin:{s}")),
                    s if s.starts_with("可视化-") => (None, format!("tuneux-visual:{s}")),
                    s => (None, s.to_string()),
                };
                PluginRow {
                    file: f.clone(),
                    loaded,
                    trust: trust.get(&jid).cloned().unwrap_or_else(|| "-".to_string()),
                }
            })
            .collect()
    }

    /// 书签弹窗（最小实现：列出 / 点击或 Enter 跳转 / ✕ 删除；
    /// ↑↓·Enter·Esc 键在输入轮询的模态分支处理）。
    pub(super) fn render_bookmarks_window(&mut self, ctx: &egui::Context) {
        if !self.bookmarks_visible {
            return;
        }
        let title = self.i18n.t("menu.bookmarks").into_owned();
        let hint = self.i18n.t("bm.hint").into_owned();
        let empty = self.i18n.t("msg.bm_empty").into_owned();
        let mut open = self.bookmarks_visible;
        let mut jump: Option<usize> = None;
        let mut del: Option<usize> = None;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.weak(hint);
                ui.separator();
                if self.config.bookmarks.items.is_empty() {
                    ui.weak(&empty);
                }
                egui::ScrollArea::vertical()
                    .max_height(280.0)
                    .show(ui, |ui| {
                        for (i, b) in self.config.bookmarks.items.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let text = format!(
                                    "{}. {} [{}]",
                                    i + 1,
                                    b.label,
                                    fmt_time(b.position_secs)
                                );
                                if ui.selectable_label(self.bm_sel == i, text).clicked() {
                                    jump = Some(i + 1);
                                }
                                if ui.small_button("✕").clicked() {
                                    del = Some(i);
                                }
                            });
                        }
                    });
            });
        self.bookmarks_visible = open;
        if let Some(n) = jump {
            self.bookmark_jump(n);
            self.bookmarks_visible = false;
        }
        if let Some(i) = del {
            if i < self.config.bookmarks.items.len() {
                self.config.bookmarks.items.remove(i);
                let m = self
                    .i18n
                    .t("msg.bm_deleted")
                    .replace("{}", &(i + 1).to_string());
                self.flash_msg(m);
                self.bm_sel = self
                    .bm_sel
                    .min(self.config.bookmarks.items.len().saturating_sub(1));
            }
        }
    }

    /// 插件管理页（只读 + 重载；待确认区随信任链接通的后续阶段再开放操作）。
    pub(super) fn render_plugins_window(&mut self, ctx: &egui::Context) {
        if !self.plugins_visible {
            return;
        }
        let pal = self.palette.clone();
        let title = self.i18n.t("menu.plugin_manager").into_owned();
        let (t_dir, t_log) = (
            self.i18n.t("plugins.dir").into_owned(),
            self.i18n.t("plugins.log").into_owned(),
        );
        let (t_loaded, t_not_loaded) = (
            self.i18n.t("plugins.loaded").into_owned(),
            self.i18n.t("plugins.not_loaded").into_owned(),
        );
        let (t_none, t_pending) = (
            self.i18n.t("plugins.none").into_owned(),
            self.i18n.t("plugins.pending_empty").into_owned(),
        );
        let t_reload = self.i18n.t("menu.reload_plugins").into_owned();
        // 运行时装载概览（关于弹窗自检同口径）
        let mark = |b: bool| if b { "√" } else { "—" };
        let summary = format!(
            "{} {} · {} {} · {} {} · {} {}",
            self.i18n.t("panel.eq"),
            mark(self.eq_plugin.is_some()),
            self.i18n.t("panel.compressor"),
            mark(self.comp_plugin.is_some()),
            self.i18n.t("panel.skin"),
            self.skin_plugins.len(),
            self.i18n.t("panel.visualizer"),
            self.visual_plugins.len()
        );
        let dir = crate::config::plugins_dir();
        let log = crate::config::plugin_log_path();
        let mut open = self.plugins_visible;
        let mut reload = false;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(&summary).monospace());
                ui.separator();
                if self.plugins_rows.is_empty() {
                    ui.weak(&t_none);
                }
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for row in &self.plugins_rows {
                            ui.horizontal(|ui| {
                                ui.label(&row.file);
                                let status = match row.loaded {
                                    Some(true) => t_loaded.as_str(),
                                    Some(false) => t_not_loaded.as_str(),
                                    None => "-",
                                };
                                ui.label(egui::RichText::new(status).color(pal.fg_weak));
                                ui.label(
                                    egui::RichText::new(&row.trust).color(pal.fg_weak).small(),
                                );
                            });
                        }
                    });
                ui.separator();
                ui.weak(format!("{t_dir}：{}", dir.display()));
                ui.weak(format!("{t_log}：{}", log.display()));
                ui.weak(&t_pending);
                ui.add_space(4.0);
                if ui.button(format!("{t_reload} (u)")).clicked() {
                    reload = true;
                }
            });
        self.plugins_visible = open;
        if reload {
            self.reload_plugins();
            self.plugins_rows = self.plugin_rows();
        }
    }

    /// 快捷键一览弹窗（内容生成自菜单静态表，不另维护键位清单）。
    pub(super) fn render_shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.shortcuts_visible {
            return;
        }
        let title = self.i18n.t("menu.shortcuts").into_owned();
        // 数字键行 + 各菜单的快捷键项，全部从静态表展开
        let digits = menus()
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{} {}", i + 1, self.i18n.t(m.title)))
            .collect::<Vec<_>>()
            .join(" · ");
        let sections: Vec<(String, Vec<(String, String)>)> = menus()
            .iter()
            .map(|m| {
                let mut rows = Vec::new();
                collect_shortcuts(m.entries, &mut rows);
                let rows = rows
                    .into_iter()
                    .map(|(sc, key)| (sc, self.i18n.t(key).into_owned()))
                    .collect();
                (self.i18n.t(m.title).into_owned(), rows)
            })
            .collect();
        let mut open = self.shortcuts_visible;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new(digits).monospace());
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .show(ui, |ui| {
                        for (title, rows) in &sections {
                            if rows.is_empty() {
                                continue;
                            }
                            ui.label(egui::RichText::new(title).strong());
                            for (sc, label) in rows {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(sc).monospace());
                                    ui.label(label);
                                });
                            }
                            ui.add_space(4.0);
                        }
                    });
            });
        self.shortcuts_visible = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 递归收集表内全部动作（测试用）。
    fn collect_actions(entries: &[MenuEntry], out: &mut Vec<MenuAction>) {
        for e in entries {
            match e {
                MenuEntry::Item { action, .. } => out.push(action.clone()),
                MenuEntry::Sub { entries, .. } => collect_actions(entries, out),
                _ => {}
            }
        }
    }

    /// 顶层八栏，序即数字键 1–8 的一一对应。
    #[test]
    fn eight_top_menus_in_digit_order() {
        let titles: Vec<&str> = menus().iter().map(|m| m.title).collect();
        assert_eq!(
            titles,
            vec![
                "menu.file",
                "menu.edit",
                "menu.play",
                "menu.audio",
                "menu.view",
                "menu.tools",
                "menu.settings",
                "menu.help",
            ],
            "数字键 d 打开 menus()[d-1]，顺序必须与骨架一致"
        );
    }

    /// 播放顺序六档：写入组合与 2026-10-14 定稿映射表逐项一致，
    /// 随机三档的循环定值必须是「列表」。
    #[test]
    fn play_order_write_table() {
        let play = &menus()[2];
        let sub = play.entries.iter().find_map(|e| match e {
            MenuEntry::Sub { label, entries } if *label == "menu.play_order" => Some(*entries),
            _ => None,
        });
        let entries = sub.expect("播放菜单必须有播放顺序子菜单");
        let got: Vec<(&str, RepeatMode, ShuffleMode)> = entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item {
                    label,
                    action: MenuAction::SetPlayOrder(r, s),
                    ..
                } => Some((*label, *r, *s)),
                _ => None,
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("order.sequential", RepeatMode::Off, ShuffleMode::Off),
                ("order.repeat_one", RepeatMode::Single, ShuffleMode::Off),
                ("order.repeat_all", RepeatMode::List, ShuffleMode::Off),
                (
                    "order.shuffle_tracks",
                    RepeatMode::List,
                    ShuffleMode::Tracks
                ),
                (
                    "order.shuffle_albums",
                    RepeatMode::List,
                    ShuffleMode::Albums
                ),
                (
                    "order.true_random",
                    RepeatMode::List,
                    ShuffleMode::TrueRandom
                ),
            ]
        );
    }

    /// 勾选优先级 shuffle > repeat：12 种状态组合下六档恰有一项被勾；
    /// 混合态（repeat 非定值且 shuffle 非关）勾随机侧。
    #[test]
    fn play_order_check_priority() {
        let repeats = [RepeatMode::Off, RepeatMode::Single, RepeatMode::List];
        let shuffles = [
            ShuffleMode::Off,
            ShuffleMode::Tracks,
            ShuffleMode::Albums,
            ShuffleMode::TrueRandom,
        ];
        let items = [
            (RepeatMode::Off, ShuffleMode::Off),
            (RepeatMode::Single, ShuffleMode::Off),
            (RepeatMode::List, ShuffleMode::Off),
            (RepeatMode::List, ShuffleMode::Tracks),
            (RepeatMode::List, ShuffleMode::Albums),
            (RepeatMode::List, ShuffleMode::TrueRandom),
        ];
        for r in repeats {
            for s in shuffles {
                let checked: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| play_order_checked(r, s, **it))
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(checked.len(), 1, "状态 ({r:?}, {s:?}) 应恰勾一项");
                if s.is_on() {
                    // 混合态勾 shuffle 侧（索引 3-5）
                    assert!(checked[0] >= 3, "({r:?}, {s:?}) 应勾随机侧");
                }
            }
        }
        // 混合态示例：单曲循环 + 曲目随机 → 勾「随机曲目」而非「单曲循环」
        assert!(play_order_checked(
            RepeatMode::Single,
            ShuffleMode::Tracks,
            (RepeatMode::List, ShuffleMode::Tracks)
        ));
        assert!(!play_order_checked(
            RepeatMode::Single,
            ShuffleMode::Tracks,
            (RepeatMode::Single, ShuffleMode::Off)
        ));
    }

    /// 表内每个动作都能在执行器落地——执行器为全枚举匹配（编译期强制），
    /// 此处核对表内动作无遗漏关键项、占位项保持灰显。
    #[test]
    fn table_actions_complete() {
        let mut acts = Vec::new();
        for m in menus() {
            collect_actions(m.entries, &mut acts);
        }
        // 关键动作各出现一次
        for want in [
            MenuAction::Quit,
            MenuAction::SavePlaylist,
            MenuAction::LoadPlaylist,
            MenuAction::RemoveSelected,
            MenuAction::ClearPlaylist,
            MenuAction::Search,
            MenuAction::Properties,
            MenuAction::ToggleMute,
            MenuAction::ToggleEq,
            MenuAction::ToggleFilter,
            MenuAction::ToggleCompressor,
            MenuAction::ToggleReplayGain,
            MenuAction::PluginManager,
            MenuAction::OpenConfigDir,
            MenuAction::CommandBar,
            MenuAction::ShowShortcuts,
            MenuAction::ShowManual,
            MenuAction::About,
            MenuAction::BookmarkAdd,
            MenuAction::BookmarkList,
            MenuAction::BookmarkDelete,
        ] {
            assert_eq!(
                acts.iter().filter(|a| **a == want).count(),
                1,
                "{want:?} 应在菜单表中恰出现一次"
            );
        }
        // 占位三项：存在且灰显
        let tools = &menus()[5];
        let disabled: Vec<&MenuAction> = tools
            .entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item {
                    enabled: false,
                    action,
                    ..
                } => Some(action),
                _ => None,
            })
            .collect();
        assert_eq!(
            disabled,
            vec![
                &MenuAction::TagEdit,
                &MenuAction::Convert,
                &MenuAction::CoverManage
            ]
        );
    }

    /// 快捷键一览内容全部来自静态表：表内非空快捷键项逐项在列。
    #[test]
    fn shortcut_rows_generated_from_table() {
        let mut rows = Vec::new();
        for m in menus() {
            collect_shortcuts(m.entries, &mut rows);
        }
        let keys: Vec<&str> = rows.iter().map(|(sc, _)| sc.as_str()).collect();
        for want in [
            "Space", "p", "n", "←", "→", "+", "-", "a", "d", "x", "/", "i", "e", "b", "v", "l",
            "c", "g", ":", "?",
        ] {
            assert!(keys.contains(&want), "快捷键 {want} 应在生成清单中");
        }
        // 打开文件按平台标注
        assert!(keys.contains(&"⌘O") || keys.contains(&"Ctrl+O"));
    }
}
