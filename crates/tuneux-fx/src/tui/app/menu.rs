//! # 菜单栏模型（文件 / 编辑 / 播放 / 音效 / 视图 / 工具 / 设置 / 帮助）
//!
//! 定义顶级菜单与各菜单项的数据结构、动作枚举，以及静态菜单表。
//! 菜单状态（哪栏激活、下拉是否展开、选中项、子菜单展开）在 [`super::App`]；
//! 动作执行与勾选态计算在 [`super::App`]（需访问运行时状态）；
//! 下拉渲染在 `render`。本模块只负责"菜单长什么样"。
//!
//! 2026-10-14 菜单结构调整：八栏骨架（数字 1-8 唤起），介质归入「音效」
//! 子菜单，新增「编辑」栏，「播放」含播放顺序六档单选与书架子菜单，
//! 插件顶层栏撤销（可视化切换并入「视图 › 频谱」，管理归「工具 › 插件管理」）。

use tuneux_corex::PlaybackMedium;
use tuneux_mediax::{RepeatMode, ShuffleMode};

/// 播放顺序六档（「播放 › 播放顺序」子菜单的互斥单选项）。
///
/// 菜单是写入快捷方式：点选即写入定值 (循环, 随机) 组合；
/// r / s 键的二维循环切换行为不变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayOrder {
    /// 顺序（播完即停）。
    Sequential,
    /// 单曲循环。
    RepeatOne,
    /// 列表循环。
    RepeatAll,
    /// 随机曲目（派对式不间断：一轮播完重洗再播）。
    ShuffleTracks,
    /// 随机专辑（整张连播）。
    ShuffleAlbums,
    /// 真随机（允许重复）。
    TrueRandom,
}

impl PlayOrder {
    /// 全部六档（菜单顺序）。
    pub const ALL: [PlayOrder; 6] = [
        PlayOrder::Sequential,
        PlayOrder::RepeatOne,
        PlayOrder::RepeatAll,
        PlayOrder::ShuffleTracks,
        PlayOrder::ShuffleAlbums,
        PlayOrder::TrueRandom,
    ];

    /// 菜单写入表：点菜单 = 写入定值 (repeat, shuffle) 组合。
    /// 随机三档的 repeat 定值取 List（随机播完一轮重洗再播，不停）。
    pub const fn write_combo(self) -> (RepeatMode, ShuffleMode) {
        match self {
            PlayOrder::Sequential => (RepeatMode::Off, ShuffleMode::Off),
            PlayOrder::RepeatOne => (RepeatMode::Single, ShuffleMode::Off),
            PlayOrder::RepeatAll => (RepeatMode::List, ShuffleMode::Off),
            PlayOrder::ShuffleTracks => (RepeatMode::List, ShuffleMode::Tracks),
            PlayOrder::ShuffleAlbums => (RepeatMode::List, ShuffleMode::Albums),
            PlayOrder::TrueRandom => (RepeatMode::List, ShuffleMode::TrueRandom),
        }
    }

    /// 菜单项的 i18n key。
    pub const fn label_key(self) -> &'static str {
        match self {
            PlayOrder::Sequential => "order.sequential",
            PlayOrder::RepeatOne => "order.repeat_one",
            PlayOrder::RepeatAll => "order.repeat_all",
            PlayOrder::ShuffleTracks => "order.shuffle_tracks",
            PlayOrder::ShuffleAlbums => "order.shuffle_albums",
            PlayOrder::TrueRandom => "order.true_random",
        }
    }

    /// 勾选显示：优先级 shuffle > repeat。混合态（repeat 非写入表定值且
    /// shuffle 开，如 single+tracks）无菜单项可精确对应——勾随机侧，
    /// 完整组合由状态栏呈现。
    pub fn checked_for(repeat: RepeatMode, shuffle: ShuffleMode) -> PlayOrder {
        match shuffle {
            ShuffleMode::Tracks => PlayOrder::ShuffleTracks,
            ShuffleMode::Albums => PlayOrder::ShuffleAlbums,
            ShuffleMode::TrueRandom => PlayOrder::TrueRandom,
            ShuffleMode::Off => match repeat {
                RepeatMode::Off => PlayOrder::Sequential,
                RepeatMode::Single => PlayOrder::RepeatOne,
                RepeatMode::List => PlayOrder::RepeatAll,
            },
        }
    }
}

/// 菜单项被选中后执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// 子菜单父项占位（不直接执行；Enter / → 展开子菜单）。
    Submenu,
    // —— 文件 ——
    Quit,
    /// 把浏览器当前目录整体加入播放列表（异步扫描）。
    AddBrowserDir,
    /// 加入浏览器选中项（同 a 键）。
    AddBrowserSelection,
    /// 保存播放列表为 m3u（浏览器当前目录 playlist.m3u）。
    SavePlaylist,
    /// 从浏览器当前目录 playlist.m3u 载入。
    LoadPlaylist,
    // —— 编辑 ——
    RemoveSelected,
    /// 清空播放列表（与 x 键同款二次确认）。
    ClearPlaylist,
    Search,
    Properties,
    // —— 播放 ——
    TogglePlay,
    PrevTrack,
    NextTrack,
    SeekBack,
    SeekForward,
    /// 播放顺序六档单选（写入定值组合，见 [`PlayOrder::write_combo`]）。
    SetPlayOrder(PlayOrder),
    VolumeUp,
    VolumeDown,
    BookmarkAdd,
    /// 书签列表弹窗（Enter 跳转、d 删除）。
    BookmarkList,
    /// 删除书签入口：同样打开书签列表弹窗（弹窗内 d 删除）。
    BookmarkDelete,
    // —— 音效 ——
    /// 介质（播放介质风格，显式选而非盲循环，与 m 键同口径）。
    SetMedium(PlaybackMedium),
    Equalizer,
    Filter,
    Compressor,
    ReplayGain,
    /// 输出设备：自动跟随系统默认（信息项，选中给出说明）。
    OutputDevice,
    // —— 视图（开关类，渲染时按运行时状态打勾）——
    ToggleBrowser,
    ToggleLyrics,
    ToggleCover,
    TogglePlaylistView,
    /// 内置频谱（v 键循环同族；从插件面板档切回内置档）。
    SpectrumBuiltin,
    /// 可视化插件面板（原插件顶层栏的可视化切换，并入此处单选）。
    SpectrumPlugin,
    // —— 工具 ——
    PluginManager,
    TagEdit,
    Convert,
    CoverManage,
    /// 命令输入条（同 : 键）。
    CommandBar,
    // —— 设置 ——
    SkinSelect,
    LangSelect,
    /// 配置目录（信息项，选中显示路径）。
    ConfigDir,
    // —— 帮助 ——
    /// 快捷键一览（内容从本静态表生成，不另维护键位清单）。
    Shortcuts,
    /// 使用手册（信息项，显示手册路径）。
    Manual,
    About,
}

/// 单个菜单项。
#[derive(Debug, Clone)]
pub struct MenuItem {
    /// 菜单项文字（i18n key）。
    pub label: &'static str,
    /// 快捷键提示（右对齐显示，无则空串）。
    pub shortcut: &'static str,
    /// 触发的动作（子菜单父项为 [`MenuAction::Submenu`]）。
    pub action: MenuAction,
    /// 是否可用（不可用项灰显不隐藏）。
    pub enabled: bool,
    /// 子菜单（最多一层；None = 叶项）。
    pub children: Option<Vec<MenuItem>>,
}

/// 一个顶级菜单（标题 + 菜单项列表）。
#[derive(Debug, Clone)]
pub struct Menu {
    /// 菜单栏上的标题（i18n key）。菜单由数字 1-8 + 方向键导航。
    pub title: &'static str,
    /// 菜单项。
    pub items: Vec<MenuItem>,
}

/// 介质菜单项的 i18n key（渲染 / 提示时经语言表翻译；与 corex 注释同源措辞）。
/// `pub(super)`：供 App 在 m 键 / 菜单切换后 flash 提示当前档位。
pub(super) fn medium_menu_label(m: PlaybackMedium) -> &'static str {
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

impl MenuItem {
    const fn new(label: &'static str, shortcut: &'static str, action: MenuAction) -> Self {
        Self {
            label,
            shortcut,
            action,
            enabled: true,
            children: None,
        }
    }
    const fn disabled(label: &'static str, shortcut: &'static str, action: MenuAction) -> Self {
        Self {
            label,
            shortcut,
            action,
            enabled: false,
            children: None,
        }
    }
    /// 子菜单父项（Enter / → 展开，自身不执行动作）。
    fn submenu(label: &'static str, children: Vec<MenuItem>) -> Self {
        Self {
            label,
            shortcut: "",
            action: MenuAction::Submenu,
            enabled: true,
            children: Some(children),
        }
    }
}

/// 静态菜单表：八栏（文件 / 编辑 / 播放 / 音效 / 视图 / 工具 / 设置 / 帮助）。
///
/// 未实现的功能（工具域）以灰显项占位，不隐藏——让用户知道"将来会有"，
/// 选中时给出提示。输出设备 / 配置目录 / 使用手册为信息项（选中给出说明）。
/// ReplayGain 为开关（选中切换，勾选态反映开/关）；播放顺序与介质为单选
/// （勾选态见 [`PlayOrder::checked_for`]）。
pub fn menus() -> &'static [Menu] {
    // 静态化：只构建一次、每帧复用同一引用，避免反复重建 Vec<Menu>。
    static MENUS: std::sync::LazyLock<Vec<Menu>> = std::sync::LazyLock::new(|| {
        vec![
            Menu {
                title: "menu.file",
                items: vec![
                    // 打开文件/目录不提供路径手敲入口：文件浏览器（b 键）+ / 搜索
                    // + 加入项完整覆盖。
                    MenuItem::new("menu.add_dir", "", MenuAction::AddBrowserDir),
                    MenuItem::new("menu.add_selection", "a", MenuAction::AddBrowserSelection),
                    MenuItem::new("menu.save_playlist", "", MenuAction::SavePlaylist),
                    MenuItem::new("menu.load_playlist", "", MenuAction::LoadPlaylist),
                    MenuItem::new("menu.quit", "q", MenuAction::Quit),
                ],
            },
            Menu {
                title: "menu.edit",
                items: vec![
                    MenuItem::new("menu.remove_selected", "d", MenuAction::RemoveSelected),
                    MenuItem::new("menu.clear_playlist", "x", MenuAction::ClearPlaylist),
                    MenuItem::new("menu.search", "/", MenuAction::Search),
                    MenuItem::new("menu.properties", "i", MenuAction::Properties),
                ],
            },
            Menu {
                title: "menu.play",
                items: vec![
                    MenuItem::new("menu.toggle_play", "Space", MenuAction::TogglePlay),
                    MenuItem::new("menu.prev", "p", MenuAction::PrevTrack),
                    MenuItem::new("menu.next", "n", MenuAction::NextTrack),
                    MenuItem::new("menu.seek_back", "←", MenuAction::SeekBack),
                    MenuItem::new("menu.seek_forward", "→", MenuAction::SeekForward),
                    MenuItem::submenu(
                        "menu.play_order",
                        PlayOrder::ALL
                            .iter()
                            .map(|&o| MenuItem::new(o.label_key(), "", MenuAction::SetPlayOrder(o)))
                            .collect(),
                    ),
                    MenuItem::new("menu.vol_up", "+", MenuAction::VolumeUp),
                    MenuItem::new("menu.vol_down", "-", MenuAction::VolumeDown),
                    MenuItem::submenu(
                        "menu.bookmark",
                        vec![
                            MenuItem::new("menu.bm_add", "", MenuAction::BookmarkAdd),
                            MenuItem::new("menu.bm_list", "", MenuAction::BookmarkList),
                            MenuItem::new("menu.bm_del", "", MenuAction::BookmarkDelete),
                        ],
                    ),
                ],
            },
            Menu {
                title: "menu.audio",
                items: vec![
                    // 介质：9 变体随 corex ALL 自动同步（新增介质无需改此处）。
                    MenuItem::submenu(
                        "menu.medium",
                        PlaybackMedium::ALL
                            .iter()
                            .map(|&m| {
                                MenuItem::new(medium_menu_label(m), "", MenuAction::SetMedium(m))
                            })
                            .collect(),
                    ),
                    MenuItem::new("menu.eq", "e", MenuAction::Equalizer),
                    MenuItem::new("menu.filter", "f", MenuAction::Filter),
                    MenuItem::new("menu.compressor", "", MenuAction::Compressor),
                    MenuItem::new("menu.replaygain", "", MenuAction::ReplayGain),
                    MenuItem::new("menu.output_device", "", MenuAction::OutputDevice),
                ],
            },
            Menu {
                title: "menu.view",
                items: vec![
                    MenuItem::new("menu.browser", "b", MenuAction::ToggleBrowser),
                    MenuItem::submenu(
                        "menu.spectrum",
                        vec![
                            MenuItem::new(
                                "menu.spectrum_builtin",
                                "v",
                                MenuAction::SpectrumBuiltin,
                            ),
                            MenuItem::new("menu.visual", "", MenuAction::SpectrumPlugin),
                        ],
                    ),
                    MenuItem::new("menu.lyrics", "l", MenuAction::ToggleLyrics),
                    MenuItem::new("menu.cover", "c", MenuAction::ToggleCover),
                    MenuItem::new("menu.group", "g", MenuAction::TogglePlaylistView),
                ],
            },
            Menu {
                title: "menu.tools",
                items: vec![
                    MenuItem::new("menu.plugin_mgr", "", MenuAction::PluginManager),
                    MenuItem::disabled("menu.tag_edit", "", MenuAction::TagEdit),
                    MenuItem::disabled("menu.convert", "", MenuAction::Convert),
                    MenuItem::disabled("menu.cover_mgmt", "", MenuAction::CoverManage),
                    MenuItem::new("menu.command_bar", ":", MenuAction::CommandBar),
                ],
            },
            Menu {
                title: "menu.settings",
                items: vec![
                    MenuItem::new("menu.skin_select", "", MenuAction::SkinSelect),
                    MenuItem::new("menu.lang_select", "", MenuAction::LangSelect),
                    MenuItem::new("menu.config_dir", "", MenuAction::ConfigDir),
                ],
            },
            Menu {
                title: "menu.help",
                items: vec![
                    MenuItem::new("menu.shortcuts", "", MenuAction::Shortcuts),
                    MenuItem::new("menu.manual", "", MenuAction::Manual),
                    MenuItem::new("menu.about", "?", MenuAction::About),
                ],
            },
        ]
    });
    MENUS.as_slice()
}

/// 快捷键一览的生成数据：(菜单标题 key, [(快捷键, 菜单项 label key)])。
///
/// 内容从静态菜单表生成（含子菜单项），只收有快捷键标注的项——
/// 键位定义只有菜单表这一处真相源，帮助弹窗不手写第二份清单。
pub fn shortcut_rows() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    let mut out: Vec<(&'static str, Vec<(&'static str, &'static str)>)> = Vec::new();
    for m in menus() {
        let mut rows = Vec::new();
        collect_shortcuts(&m.items, &mut rows);
        if !rows.is_empty() {
            out.push((m.title, rows));
        }
    }
    out
}

/// 递归收集一层菜单的快捷键标注项（子菜单仅一层，见 [`MenuItem::children`]）。
fn collect_shortcuts(items: &[MenuItem], out: &mut Vec<(&'static str, &'static str)>) {
    for it in items {
        if !it.shortcut.is_empty() {
            out.push((it.shortcut, it.label));
        }
        if let Some(children) = &it.children {
            collect_shortcuts(children, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 收集菜单表中的全部动作（含子菜单项）。
    fn collect_actions(items: &[MenuItem], out: &mut Vec<MenuAction>) {
        for it in items {
            out.push(it.action);
            if let Some(children) = &it.children {
                collect_actions(children, out);
            }
        }
    }

    fn all_table_actions() -> Vec<MenuAction> {
        let mut out = Vec::new();
        for m in menus() {
            collect_actions(&m.items, &mut out);
        }
        out
    }

    /// 数字键序与八栏骨架一一对应：1=文件 … 8=帮助。
    /// 菜单唤起按下标映射（`Char('1'..='8')` → menus()[idx]），顺序钉死防错位。
    #[test]
    fn skeleton_matches_digit_order() {
        let titles: Vec<&str> = menus().iter().map(|m| m.title).collect();
        assert_eq!(
            titles,
            [
                "menu.file",
                "menu.edit",
                "menu.play",
                "menu.audio",
                "menu.view",
                "menu.tools",
                "menu.settings",
                "menu.help",
            ],
            "八栏骨架顺序即数字键 1-8 的映射"
        );
    }

    /// 播放顺序写入表：六档各对应定值 (repeat, shuffle) 组合；
    /// 随机三档的 repeat 恒为 List（一轮播完重洗再播的不间断预期）。
    #[test]
    fn play_order_write_table() {
        use RepeatMode as R;
        use ShuffleMode as S;
        let expect = [
            (PlayOrder::Sequential, (R::Off, S::Off)),
            (PlayOrder::RepeatOne, (R::Single, S::Off)),
            (PlayOrder::RepeatAll, (R::List, S::Off)),
            (PlayOrder::ShuffleTracks, (R::List, S::Tracks)),
            (PlayOrder::ShuffleAlbums, (R::List, S::Albums)),
            (PlayOrder::TrueRandom, (R::List, S::TrueRandom)),
        ];
        for (ord, combo) in expect {
            assert_eq!(ord.write_combo(), combo, "{ord:?} 的写入组合");
        }
    }

    /// 勾选显示优先级 shuffle > repeat：混合态（如 single+tracks）勾随机侧；
    /// shuffle 关时按 repeat 三态勾顺序/单曲/列表。
    #[test]
    fn play_order_check_prefers_shuffle() {
        use RepeatMode as R;
        use ShuffleMode as S;
        // 精确命中写入表定值
        for ord in PlayOrder::ALL {
            let (r, s) = ord.write_combo();
            assert_eq!(PlayOrder::checked_for(r, s), ord, "{ord:?} 定值应勾回自身");
        }
        // 混合态：repeat 非定值 + shuffle 开 → 勾随机侧
        assert_eq!(
            PlayOrder::checked_for(R::Single, S::Tracks),
            PlayOrder::ShuffleTracks
        );
        assert_eq!(
            PlayOrder::checked_for(R::Off, S::TrueRandom),
            PlayOrder::TrueRandom
        );
        assert_eq!(
            PlayOrder::checked_for(R::Single, S::Albums),
            PlayOrder::ShuffleAlbums
        );
        // shuffle 关：按 repeat 勾
        assert_eq!(
            PlayOrder::checked_for(R::Off, S::Off),
            PlayOrder::Sequential
        );
        assert_eq!(
            PlayOrder::checked_for(R::Single, S::Off),
            PlayOrder::RepeatOne
        );
        assert_eq!(
            PlayOrder::checked_for(R::List, S::Off),
            PlayOrder::RepeatAll
        );
    }

    /// 子菜单父项良构：动作恒为 Submenu 占位、子项非空、不再嵌套（单层）。
    #[test]
    fn submenu_parents_are_well_formed() {
        for m in menus() {
            for it in &m.items {
                if let Some(children) = &it.children {
                    assert_eq!(it.action, MenuAction::Submenu, "{} 父项动作", it.label);
                    assert!(!children.is_empty(), "{} 子菜单为空", it.label);
                    assert!(
                        children.iter().all(|c| c.children.is_none()),
                        "{} 子菜单不可再嵌套",
                        it.label
                    );
                    assert!(it.enabled, "{} 父项不可灰显", it.label);
                }
            }
        }
    }

    /// 菜单表覆盖全部动作变体：每个动作至少在表中出现一次
    ///（动作 → 执行分支的完备性由 execute_menu_action 的无通配 match
    /// 在编译期强制；本测试钉死反向——执行器有的动作菜单里都能点到）。
    #[test]
    fn table_covers_all_actions() {
        let actions = all_table_actions();
        let has = |a: MenuAction| actions.contains(&a);
        for a in [
            MenuAction::Quit,
            MenuAction::AddBrowserDir,
            MenuAction::AddBrowserSelection,
            MenuAction::SavePlaylist,
            MenuAction::LoadPlaylist,
            MenuAction::RemoveSelected,
            MenuAction::ClearPlaylist,
            MenuAction::Search,
            MenuAction::Properties,
            MenuAction::TogglePlay,
            MenuAction::PrevTrack,
            MenuAction::NextTrack,
            MenuAction::SeekBack,
            MenuAction::SeekForward,
            MenuAction::VolumeUp,
            MenuAction::VolumeDown,
            MenuAction::BookmarkAdd,
            MenuAction::BookmarkList,
            MenuAction::BookmarkDelete,
            MenuAction::Equalizer,
            MenuAction::Filter,
            MenuAction::Compressor,
            MenuAction::ReplayGain,
            MenuAction::OutputDevice,
            MenuAction::ToggleBrowser,
            MenuAction::ToggleLyrics,
            MenuAction::ToggleCover,
            MenuAction::TogglePlaylistView,
            MenuAction::SpectrumBuiltin,
            MenuAction::SpectrumPlugin,
            MenuAction::PluginManager,
            MenuAction::TagEdit,
            MenuAction::Convert,
            MenuAction::CoverManage,
            MenuAction::CommandBar,
            MenuAction::SkinSelect,
            MenuAction::LangSelect,
            MenuAction::ConfigDir,
            MenuAction::Shortcuts,
            MenuAction::Manual,
            MenuAction::About,
        ] {
            assert!(has(a), "菜单表缺少动作 {a:?}");
        }
        // 播放顺序六档全员在表
        for ord in PlayOrder::ALL {
            assert!(has(MenuAction::SetPlayOrder(ord)), "缺少播放顺序 {ord:?}");
        }
        // 介质项与 corex ALL 全量同步（顺序一致）
        let media: Vec<PlaybackMedium> = actions
            .iter()
            .filter_map(|a| match a {
                MenuAction::SetMedium(m) => Some(*m),
                _ => None,
            })
            .collect();
        assert_eq!(media.as_slice(), PlaybackMedium::ALL.as_slice());
    }

    /// 占位灰显项恰好三个（标签编辑/格式转换/封面管理），其余项可用。
    #[test]
    fn disabled_placeholders_are_exact() {
        let disabled: Vec<MenuAction> = menus()
            .iter()
            .flat_map(|m| m.items.iter())
            .filter(|it| !it.enabled)
            .map(|it| it.action)
            .collect();
        assert_eq!(
            disabled,
            [
                MenuAction::TagEdit,
                MenuAction::Convert,
                MenuAction::CoverManage
            ]
        );
    }

    /// 快捷键一览内容由菜单表生成（含子菜单项的标注）。
    #[test]
    fn shortcut_rows_generated_from_table() {
        let rows = shortcut_rows();
        // 平坦项：帮助栏的关于（?）
        assert!(
            rows.iter()
                .any(|(t, rs)| *t == "menu.help" && rs.contains(&("?", "menu.about"))),
            "帮助栏应含 ? → 关于"
        );
        // 子菜单项：视图栏内置频谱（v）
        assert!(
            rows.iter()
                .any(|(t, rs)| *t == "menu.view" && rs.contains(&("v", "menu.spectrum_builtin"))),
            "视图栏应含 v → 内置频谱（子菜单项）"
        );
        // 无快捷键标注的项（如播放顺序六档）不收录
        assert!(
            !rows
                .iter()
                .any(|(_, rs)| rs.iter().any(|(_, l)| *l == "order.sequential")),
            "无标注项不应出现在一览"
        );
    }
}
