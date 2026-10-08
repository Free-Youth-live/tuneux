//! # 菜单栏模型（文件 / 播放 / 视图 / 工具 / 设置 / 插件 / 帮助 + 介质）
//!
//! 定义顶级菜单与各菜单项的数据结构、动作枚举，以及静态菜单表。
//! 菜单状态（哪栏激活、下拉是否展开、选中项）在 [`super::App`]；
//! 动作执行与勾选态计算在 [`super::App`]（需访问运行时状态）；
//! 下拉渲染在 `render`。本模块只负责"菜单长什么样"。

use tuneux_corex::PlaybackMedium;

/// 菜单项被选中后执行的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    // —— 文件 ——
    Quit,
    // —— 播放 ——
    TogglePlay,
    PrevTrack,
    NextTrack,
    CycleRepeat,
    ToggleShuffle,
    VolumeUp,
    VolumeDown,
    // —— 介质（播放介质风格，与 m 键同口径，显式选而非盲循环）——
    SetMedium(PlaybackMedium),
    // —— 视图（开关类，渲染时按运行时状态打勾）——
    ToggleBrowser,
    ToggleCover,
    ToggleLyrics,
    ToggleSpectrum,
    TogglePlaylistView,
    // —— 工具（插件工作区，未装灰显）——
    Equalizer,
    Filter,
    Compressor,
    Dsp,
    TagEdit,
    Convert,
    CoverManage,
    // —— 设置 ——
    OutputDevice,
    ReplayGain,
    SkinSelect,
    LangSelect,
    // —— 插件（插件域，清单；√ = 已加载）——
    PluginEq,
    PluginComp,
    PluginVisual,
    // —— 帮助 ——
    About,
}

/// 单个菜单项。
#[derive(Debug, Clone)]
pub struct MenuItem {
    /// 菜单项文字（中文）。
    pub label: &'static str,
    /// 快捷键提示（右对齐显示，无则空串）。
    pub shortcut: &'static str,
    /// 触发的动作。
    pub action: MenuAction,
    /// 是否可用（不可用项灰显不隐藏）。
    pub enabled: bool,
}

/// 一个顶级菜单（标题 + 菜单项列表）。
#[derive(Debug, Clone)]
pub struct Menu {
    /// 菜单栏上的标题（如 "文件"）。菜单由数字 1-N（或 F10）+ 方向键导航，不占 Alt+字母。
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
        }
    }
    const fn disabled(label: &'static str, shortcut: &'static str, action: MenuAction) -> Self {
        Self {
            label,
            shortcut,
            action,
            enabled: false,
        }
    }
}

/// 静态菜单表：八栏（文件 / 播放 / 介质 / 视图 / 工具 / 设置 / 插件 / 帮助）。
///
/// 未实现的功能（工具/插件域）以灰显项占位，不隐藏——
/// 让用户知道"将来会有"，选中时给出提示。输出设备已为自动行为（信息项）；
/// ReplayGain 现为开关（选中切换，勾选态反映开/关）。
pub fn menus() -> &'static [Menu] {
    // 静态化：只构建一次、每帧复用同一引用，避免反复重建 Vec<Menu>。
    static MENUS: std::sync::LazyLock<Vec<Menu>> = std::sync::LazyLock::new(|| {
        vec![
            Menu {
                title: "menu.file",
                items: vec![
                    // 打开文件/目录已移除：文件浏览器（b 键）+ / 搜索 + a 加入
                    // 完整覆盖，命令模式不再提供路径手敲入口。
                    MenuItem::new("menu.quit", "q", MenuAction::Quit),
                ],
            },
            Menu {
                title: "menu.play",
                items: vec![
                    MenuItem::new("menu.toggle_play", "Space", MenuAction::TogglePlay),
                    MenuItem::new("menu.prev", "p", MenuAction::PrevTrack),
                    MenuItem::new("menu.next", "n", MenuAction::NextTrack),
                    MenuItem::new("menu.repeat", "r", MenuAction::CycleRepeat),
                    MenuItem::new("menu.shuffle", "s", MenuAction::ToggleShuffle),
                    MenuItem::new("menu.vol_up", "+", MenuAction::VolumeUp),
                    MenuItem::new("menu.vol_down", "-", MenuAction::VolumeDown),
                ],
            },
            // 介质：9 变体随 corex ALL 自动同步（新增介质无需改此处）。
            Menu {
                title: "menu.medium",
                items: PlaybackMedium::ALL
                    .iter()
                    .map(|&m| MenuItem::new(medium_menu_label(m), "", MenuAction::SetMedium(m)))
                    .collect(),
            },
            Menu {
                title: "menu.view",
                items: vec![
                    MenuItem::new("menu.browser", "b", MenuAction::ToggleBrowser),
                    MenuItem::new("menu.cover", "c", MenuAction::ToggleCover),
                    MenuItem::new("menu.lyrics", "l", MenuAction::ToggleLyrics),
                    MenuItem::new("menu.spectrum", "v", MenuAction::ToggleSpectrum),
                    MenuItem::new("menu.group", "g", MenuAction::TogglePlaylistView),
                ],
            },
            Menu {
                title: "menu.tools",
                items: vec![
                    MenuItem::new("menu.eq", "F9", MenuAction::Equalizer),
                    MenuItem::new("menu.filter", "f", MenuAction::Filter),
                    MenuItem::new("menu.compressor", "", MenuAction::Compressor),
                    MenuItem::disabled("menu.dsp", "", MenuAction::Dsp),
                    MenuItem::disabled("menu.tag_edit", "", MenuAction::TagEdit),
                    MenuItem::disabled("menu.convert", "", MenuAction::Convert),
                    MenuItem::disabled("menu.cover_mgmt", "", MenuAction::CoverManage),
                ],
            },
            Menu {
                title: "menu.settings",
                items: vec![
                    MenuItem::new("menu.output_device", "", MenuAction::OutputDevice),
                    MenuItem::new("menu.replaygain", "", MenuAction::ReplayGain),
                    MenuItem::new("menu.skin_select", "", MenuAction::SkinSelect),
                    MenuItem::new("menu.lang_select", "", MenuAction::LangSelect),
                ],
            },
            Menu {
                title: "menu.plugins",
                items: vec![
                    MenuItem::new("menu.eq", "", MenuAction::PluginEq),
                    MenuItem::new("menu.compressor", "", MenuAction::PluginComp),
                    MenuItem::new("menu.visual", "v", MenuAction::PluginVisual),
                ],
            },
            Menu {
                title: "menu.help",
                items: vec![
                    // 原两项（快捷键速查/关于、关于）打开的是同一个弹窗，合并为一项。
                    MenuItem::new("menu.about", "?", MenuAction::About),
                ],
            },
        ]
    });
    MENUS.as_slice()
}
