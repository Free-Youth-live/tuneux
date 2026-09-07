//! # 菜单栏模型（参照 foobar2000 组织：文件 / 播放 / 视图 / 工具 / 设置 / 插件 / 帮助 + 介质）
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
    OpenFile,
    OpenDir,
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
    Compressor,
    Dsp,
    TagEdit,
    Convert,
    CoverManage,
    // —— 设置 ——
    OutputDevice,
    ReplayGain,
    Theme,
    // —— 插件（插件域，清单；√ = 已加载）——
    PluginEq,
    PluginComp,
    // —— 帮助 ——
    Help,
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

/// 介质菜单项的中文显示名（含一句音色特征；与 corex 注释同源措辞）。
/// `pub(super)`：供 App 在 m 键 / 菜单切换后 flash 提示当前档位。
pub(super) fn medium_menu_label(m: PlaybackMedium) -> &'static str {
    match m {
        PlaybackMedium::None => "关闭（原始输出）",
        PlaybackMedium::TapeClear => "磁带·透明（高保真）",
        PlaybackMedium::TapeWhite => "磁带·白色（清新）",
        PlaybackMedium::TapeClassic => "磁带·深棕（经典）",
        PlaybackMedium::TapeAged => "磁带·红色（老化）",
        PlaybackMedium::VinylClean => "黑胶·蓝色（低噪声）",
        PlaybackMedium::VinylDynamic => "黑胶·红色（高动态）",
        PlaybackMedium::VinylStandard => "黑胶·黑色（标准）",
        PlaybackMedium::VinylAged => "黑胶·彩胶（老化）",
        _ => "未知介质",
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
                title: "文件",
                items: vec![
                    MenuItem::new("打开文件…（.cue 自动分轨）", "F3", MenuAction::OpenFile),
                    MenuItem::new("打开目录…", "F4", MenuAction::OpenDir),
                    MenuItem::new("退出", "q", MenuAction::Quit),
                ],
            },
            Menu {
                title: "播放",
                items: vec![
                    MenuItem::new("播放 / 暂停", "空格", MenuAction::TogglePlay),
                    MenuItem::new("上一曲", "p", MenuAction::PrevTrack),
                    MenuItem::new("下一曲", "n", MenuAction::NextTrack),
                    MenuItem::new("循环模式", "r", MenuAction::CycleRepeat),
                    MenuItem::new("随机播放", "s", MenuAction::ToggleShuffle),
                    MenuItem::new("音量增大", "+", MenuAction::VolumeUp),
                    MenuItem::new("音量减小", "-", MenuAction::VolumeDown),
                ],
            },
            // 介质：9 变体随 corex ALL 自动同步（新增介质无需改此处）。
            Menu {
                title: "介质",
                items: PlaybackMedium::ALL
                    .iter()
                    .map(|&m| MenuItem::new(medium_menu_label(m), "", MenuAction::SetMedium(m)))
                    .collect(),
            },
            Menu {
                title: "视图",
                items: vec![
                    MenuItem::new("文件浏览器", "b", MenuAction::ToggleBrowser),
                    MenuItem::new("专辑封面", "c", MenuAction::ToggleCover),
                    MenuItem::new("歌词", "l", MenuAction::ToggleLyrics),
                    MenuItem::new("频谱", "v", MenuAction::ToggleSpectrum),
                    MenuItem::new("播放列表分组", "g", MenuAction::TogglePlaylistView),
                ],
            },
            Menu {
                title: "工具",
                items: vec![
                    MenuItem::new("均衡器", "F9", MenuAction::Equalizer),
                    MenuItem::new("压缩器", "", MenuAction::Compressor),
                    MenuItem::disabled("DSP", "", MenuAction::Dsp),
                    MenuItem::disabled("标签编辑", "", MenuAction::TagEdit),
                    MenuItem::disabled("格式转换", "", MenuAction::Convert),
                    MenuItem::disabled("封面管理", "", MenuAction::CoverManage),
                ],
            },
            Menu {
                title: "设置",
                items: vec![
                    MenuItem::new("输出设备（自动跟随）", "", MenuAction::OutputDevice),
                    MenuItem::new("ReplayGain 响度归一", "", MenuAction::ReplayGain),
                    MenuItem::new("配色切换", "F2", MenuAction::Theme),
                ],
            },
            Menu {
                title: "插件",
                items: vec![
                    MenuItem::new("均衡器", "", MenuAction::PluginEq),
                    MenuItem::new("压缩器", "", MenuAction::PluginComp),
                ],
            },
            Menu {
                title: "帮助",
                items: vec![
                    MenuItem::new("快捷键速查 / 关于", "?", MenuAction::Help),
                    MenuItem::new("关于", "", MenuAction::About),
                ],
            },
        ]
    });
    MENUS.as_slice()
}
