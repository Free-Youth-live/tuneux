//! # UI 模型（index-pure UI 状态与动作集）
//!
//! UiAction 为 P0 冻结的语义全集（fx 将来做 GUI 化移植时共用口径）；
//! 当前唯一使用方是 tuneux-max。模型不引用 mediax/corex 类型
//! （index-pure 约束）：播放列表行用索引、路径用不透明 ID。
//! 产品薄壳负责「索引 ↔ 数据」的映射与引擎命令的执行。
//!
//! # 与 P0 冻结分母（60）的账目（tools/p0-decision.py 为准）
//!
//! 60 = 51 个「语义必须一致」+ 9 个非动作（4 个禁用占位 + 5 个渲染
//! 差异标记）。本枚举承载全部 51 个语义动作：42 个 1:1 变体 + 命令
//! 模式 9 个 id 经 CommandStart / CommandExecute(String) / CommandCancel
//! 三变体统一承载（指令字符串由产品侧解析）。9 个非动作条目不设
//! 变体（禁用项无行为、渲染标记无触发）。Quit 不在脚本清单内，但
//! P0 §四键位表要求「q 退出」，属登记过的增量（见 P0 §八）。

// —— 焦点 / 模式 / 面板 ——//

/// 键盘焦点目标（Tab 循环切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTarget {
    /// 文件浏览器。
    Browser,
    /// 播放列表。
    Playlist,
}

/// 当前 UI 模式（互斥）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    /// 正常浏览 / 导航。
    Normal,
    /// 搜索输入中。
    Search,
    /// 命令模式输入中。
    Command,
    /// 菜单栏导航中。
    Menu,
    /// 等待第二次 x 确认清空。
    ConfirmClear,
}

// —— UiAction（P0 冻结；账目见文件头）——//

/// UI 动作（max 与 fx 语义必须一致的 51 个；命令模式 9 个 id 经
/// CommandExecute(String) 等三变体承载，渲染差异 9 个不设变体）。
/// 产品侧 match 分发到各自的引擎 / 数据操作；逐变体语义见名下文档，
/// 与 P0 冻结清单（tools/p0-decision.py）及键位表（P0 §四）对应。
#[derive(Debug, Clone, PartialEq)]
pub enum UiAction {
    // 播放控制
    /// 播放 / 暂停切换（默认键 Space；冻结 id play_toggle）。
    PlayToggle,
    /// 上一曲（默认键 p；冻结 id prev_track）。
    PrevTrack,
    /// 下一曲（默认键 n；冻结 id next_track）。
    NextTrack,
    /// 相对后退；载荷为秒数（默认键 ← 传 5.0；冻结 id seek_back）。
    SeekBack(f64),
    /// 相对前进；载荷为秒数（默认键 → 传 5.0；冻结 id seek_forward）。
    SeekForward(f64),
    /// 音量步增（默认键 +，步长 5%；冻结 id volume_up）。
    VolumeUp,
    /// 音量步减（默认键 -，步长 5%；冻结 id volume_down）。
    VolumeDown,
    /// 循环模式三态循环：关 → 列表 → 单曲 → 关（默认键 r；冻结 id cycle_repeat）。
    CycleRepeat,
    /// 随机播放开关（默认键 s；冻结 id toggle_shuffle）。
    ToggleShuffle,

    // 介质
    /// 介质循环切换（默认键 m；冻结 id medium_cycle）。
    MediumCycle,
    /// 上一介质（默认键 [；冻结 id medium_prev）。
    MediumPrev,
    /// 下一介质（默认键 ]；冻结 id medium_next）。
    MediumNext,
    /// 指定介质（菜单 9 项直选；冻结 id medium_set）。
    /// 载荷为 PlaybackMedium::ALL 的索引（0..=8，index-pure 不引 corex 类型，
    /// 产品侧映射；越界索引由产品侧忽略）。
    MediumSet(u8),

    // 面板开合
    /// 文件浏览器面板开合（默认键 b / F5；冻结 id toggle_browser）。
    ToggleBrowser,
    /// 专辑封面面板开合（默认键 c / F6；冻结 id toggle_cover）。
    ToggleCover,
    /// 歌词面板开合（默认键 l / F7；冻结 id toggle_lyrics）。
    ToggleLyrics,
    /// 频谱面板开合（默认键 v / F8，max 中 v 循环频谱/示波器；
    /// 冻结 id toggle_spectrum）。
    ToggleSpectrum,
    /// 均衡器面板开合（默认键 F9；冻结 id toggle_eq）。
    ToggleEq,
    /// 播放列表分组视图开关（默认键 g；冻结 id toggle_group）。
    ToggleGroupView,

    // 播放列表管理
    /// 加入浏览器当前选中条目（文件单曲入表，目录递归入表；
    /// 默认键 a；冻结 id add_current）。
    AddCurrent,
    /// 把指定浏览器目录加入播放列表；载荷为浏览器条目索引
    /// （index-pure；冻结 id add_dir，fx 侧由浏览器 Enter 触发）。
    AddDir(usize),
    /// 删除播放列表选中条目（默认键 d；冻结 id delete_item）。
    DeleteSelected,
    /// 清空播放列表；需二次确认（默认键 x x——模型层置 ConfirmClear
    /// 模式，再按一次才生效；冻结 id clear_confirm）。
    ClearPlaylist,

    // 导航（焦点面板内移动选中）
    /// 上移一行（默认键 ↑ / k；冻结 id nav_up）。
    NavUp,
    /// 下移一行（默认键 ↓ / j；冻结 id nav_down）。
    NavDown,
    /// 确认：进入目录 / 播放选中 / 折叠分组头（默认键 Enter；
    /// 冻结 id nav_enter）。
    NavEnter,
    /// 返回 / 取消：关弹窗、退搜索与命令模式（默认键 Esc；
    /// 冻结 id nav_back）。
    NavBack,
    /// 跳到列表头（默认键 Home；冻结 id nav_home）。
    NavHome,
    /// 跳到列表尾（默认键 End；冻结 id nav_end）。
    NavEnd,
    /// 焦点循环：浏览器 ↔ 播放列表（默认键 Tab；冻结 id cycle_focus）。
    CycleFocus,

    // 设置
    /// 皮肤选择（菜单入口；冻结 id skin_select）。
    SkinSelect,
    /// 语言选择（菜单入口；冻结 id lang_select）。
    LangSelect,
    /// 输出设备选择（菜单入口；冻结 id output_device）。
    OutputDevice,
    /// ReplayGain 响度补偿开关（菜单入口；冻结 id replaygain）。
    ReplayGainToggle,

    // 功能面板（菜单入口）
    /// 压缩器面板开合（冻结 id compressor）。
    ToggleCompressor,
    /// EQ 插件遥控面板（冻结 id plugin_eq；面板内部自示插件加载状态）。
    PluginEq,
    /// 压缩器插件遥控面板（冻结 id plugin_comp）。
    PluginComp,
    /// 可视化插件面板（冻结 id plugin_visual）。
    PluginVisual,

    // 信息
    /// 关于 / 快捷键速查弹窗（默认键 ?；冻结 id about）。
    About,

    // 搜索
    /// 打开搜索（默认键 /；冻结 id search_start）。
    SearchStart,
    /// 搜索结果导航（冻结 id search_navigate）：在过滤后的结果集内
    /// 上/下移动选中（true = 向下）；空集由产品侧忽略。
    SearchNavigate(bool),
    /// 退出搜索（默认键 Esc；冻结 id search_exit）。
    SearchExit,

    // 命令模式
    /// 进入命令模式（默认键 :；冻结 id cmd_mode）。
    CommandStart,
    /// 执行命令；载荷为原始命令行（含参数，产品侧自行解析；
    /// 冻结 id cmd_execute，并统一承载指令 cmd_repeat / cmd_volume /
    /// cmd_bookmark / cmd_bm_del / cmd_m3u_save / cmd_help 六个 id——
    /// 指令名与参数就在载荷字符串里）。
    CommandExecute(String),
    /// 取消命令输入（默认键 Esc；冻结 id cmd_cancel）。
    CommandCancel,

    // 退出
    /// 退出程序（默认键 q；P0 §八增量登记项，不在脚本 60 清单内）。
    Quit,
}

// —— UiModel（index-pure UI 状态）——//

/// UI 状态模型（不含领域数据；索引由产品侧映射）。
#[derive(Debug, Clone)]
pub struct UiModel {
    /// 当前焦点。
    pub focus: FocusTarget,
    /// 当前模式。
    pub mode: UiMode,
    /// 播放列表选中行索引。
    pub playlist_selected: Option<usize>,
    /// 播放列表滚动偏移（可见区首行）。
    pub playlist_scroll: usize,
    /// 浏览器选中行索引。
    pub browser_selected: Option<usize>,
    /// 浏览器滚动偏移。
    pub browser_scroll: usize,
    /// 搜索文本。
    pub search_query: String,
    /// 命令模式输入。
    pub command_input: String,
    /// 菜单当前打开的索引。
    pub menu_open: Option<usize>,
    /// 菜单当前高亮项索引。
    pub menu_highlight: Option<usize>,
    /// 关于弹窗是否可见。
    pub about_visible: bool,
    /// 转瞬消息（错误/提示，自动过期）。
    pub flash: Option<String>,
}

impl Default for UiModel {
    fn default() -> Self {
        Self {
            focus: FocusTarget::Browser,
            mode: UiMode::Normal,
            playlist_selected: None,
            playlist_scroll: 0,
            browser_selected: Some(0),
            browser_scroll: 0,
            search_query: String::new(),
            command_input: String::new(),
            menu_open: None,
            menu_highlight: None,
            about_visible: false,
            flash: None,
        }
    }
}

impl UiModel {
    /// 执行一个 UI 动作，更新模型状态（焦点 / 模式 / 选中 / 弹窗）。
    ///
    /// 引擎命令与数据变更由产品侧 execute 处理——模型层不返回任何
    /// 操作，仅完成自身状态变迁。
    pub fn apply(&mut self, action: &UiAction) {
        use UiAction::*;
        match action {
            // 面板开合类动作（ToggleBrowser/Cover/Lyrics/Spectrum/Eq）由
            // 产品侧处理（max = dock 树装卸），模型层不再持有面板状态

            // 焦点循环
            CycleFocus => {
                self.focus = match self.focus {
                    FocusTarget::Browser => FocusTarget::Playlist,
                    FocusTarget::Playlist => FocusTarget::Browser,
                };
            }

            // 模式切换
            SearchStart => {
                self.mode = UiMode::Search;
                self.search_query.clear();
            }
            SearchExit => {
                self.mode = UiMode::Normal;
                self.search_query.clear();
            }
            CommandStart => {
                self.mode = UiMode::Command;
                self.command_input.clear();
            }
            CommandCancel => {
                self.mode = UiMode::Normal;
                self.command_input.clear();
            }
            CommandExecute(_) => {
                self.mode = UiMode::Normal;
                self.command_input.clear();
            }

            // 导航
            NavBack => {
                if self.mode != UiMode::Normal {
                    self.mode = UiMode::Normal;
                    self.search_query.clear();
                    self.command_input.clear();
                } else if self.about_visible {
                    self.about_visible = false;
                }
            }
            About => self.about_visible = !self.about_visible,
            ClearPlaylist => self.mode = UiMode::ConfirmClear,

            _ => {} // 其余动作由产品侧处理
        }
    }
}
