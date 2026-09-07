//! # 文件浏览器模块
//!
//! 提供 TUI 内置的文件浏览能力：列出目录内容、过滤音乐文件、
//! 管理导航状态（当前目录、选中项、滚动偏移）。
//!
//! 这是用户在 tuneux 中查找音乐的**主要入口**（项目无命令行参数、
//! 无子命令）。用户通过它浏览磁盘、定位音乐文件或目录，再播放或
//! 加入播放列表。
//!
//! ## 目录条目排序规则
//!
//! 为了让浏览体验直观，条目按以下优先级排序：
//! 1. **子目录在前，文件在后**——目录是"可进入"的容器，置顶更符合
//!    文件管理器的习惯（如 Windows 资源管理器、macOS Finder）；
//! 2. **同类内按文件名不区分大小写升序**——避免大小写差异导致排序
//!    跳跃（如 `Bach.mp3` 排在 `adele.mp3` 后面会很奇怪）。
//!
//! 隐藏文件（以 `.` 开头，Unix 习惯）默认不显示，避免干扰。
//!
//! ## 音乐格式识别
//!
//! 仅显示扩展名匹配的文件（见 [`SUPPORTED_EXTS`]）。非音乐文件
//! （如 `.txt`、`.jpg`）不列出，保持列表干净。目录一律显示
//! （用户可能把音乐放在任意子目录里）。
//!
//! ## 文件名编码：UTF-8 严格策略（零乱码）
//!
//! tuneux 全项目以 UTF-8 为文件名的唯一标准。文件名**非有效 UTF-8**
//! 的条目一律跳过，既不在浏览器显示，也不加入播放列表。这样用户看到
//! 的所有文件名都是正确显示的字符，**永远不会出现 `U+FFFD` 占位符或 `???`**。
//! 代价：极少数非 UTF-8 文件名（如旧 NAS 上的 GBK、非 Unicode locale 下的
//! 文件）需用户先用系统工具重命名为 UTF-8 才能被识别。
//!
//! 实现上用 `OsStr::to_str()`（返回 `Option`，严格判定）而非
//! `to_string_lossy()`（容忍替换成占位符），从源头杜绝乱码。

// 本模块与插件版（tuneux-fx）fs_browser 同源；异步化导航后 navigate_to/enter_selected/go_up
// 仅由 FsBrowser::open 与单元测试使用，临时豁免 dead_code 警告。
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// tuneux 认识的音乐文件扩展名（小写，不含点），清单由内核统一维护
/// （`tuneux_corex::KNOWN_AUDIO_EXTS`：原生 + ffmpeg 长尾）。浏览器只负责
/// 显示"认识"的格式；能否播放由解码时判定，不能播放的会自动跳过并提示。
pub use tuneux_corex::KNOWN_AUDIO_EXTS as SUPPORTED_EXTS;

/// 判断文件路径是否为 tuneux 支持的音乐文件。
///
/// 判断依据仅看扩展名（不读文件头），原因：
/// - 速度快——浏览器要在用户每次进入目录时即时列出，读文件头会有可感延迟；
/// - symphonia 解码时会再次校验真实格式，扩展名误判不会导致崩溃，
///   顶多播放时报错，影响可控。
///
/// 路径无扩展名或扩展名不在支持列表中，均返回 false。
pub fn is_supported(path: &Path) -> bool {
    match path.extension() {
        // extension() 返回 OsStr，转成小写 Unicode 再比对
        Some(ext) => {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            SUPPORTED_EXTS.contains(&ext_lower.as_str())
        }
        None => false,
    }
}

/// 一条目录条目（目录或音乐文件）。
///
/// 用枚举而非统一结构体（带 is_dir 标志），是因为目录和文件在
/// TUI 中的行为完全不同（目录可进入、文件可播放），枚举让这些
/// 差异在类型层面体现，调用方 match 时编译器保证处理全部分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// 子目录。`name` 为显示名（不含路径），`path` 为完整路径。
    Dir { name: String, path: PathBuf },
    /// 音乐文件。同样保存显示名与完整路径。
    File { name: String, path: PathBuf },
}

impl Entry {
    /// 条目的显示名（用于 TUI 列表渲染）。
    pub fn name(&self) -> &str {
        match self {
            Entry::Dir { name, .. } => name,
            Entry::File { name, .. } => name,
        }
    }

    /// 条目的完整路径。
    ///
    /// 公共 API 预留：调用方目前都通过 `Entry::Dir { path, .. }` /
    /// `Entry::File { path, .. }` 模式匹配直接取 `path` 字段，本方法暂未被调用，
    /// 故用 `dead_code` 压制警告。保留是为了让外部模块未来可像 `name()`
    /// 一样以方法调用方式取路径，不必重新引入 `match`。
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        match self {
            Entry::Dir { path, .. } => path,
            Entry::File { path, .. } => path,
        }
    }

    /// 是否为目录。便于在 TUI 渲染时加目录图标 `▸`。
    pub fn is_dir(&self) -> bool {
        matches!(self, Entry::Dir { .. })
    }
}

/// 文件浏览器的导航与显示状态。
///
/// 这个结构体持有"浏览器当前看到什么"的全部信息：
/// - 当前所在目录（`cwd`）；
/// - 该目录下排序后的条目列表（`entries`）；
/// - 用户选中了第几项（`selected`）；
/// - 列表滚动到第几行（`scroll`）——当条目多于可视区域时，
///   `scroll` 决定从哪一行开始绘制，实现长列表的滚动浏览。
///
/// `selected` 与 `scroll` 都用 usize，由 TUI 渲染层根据终端
/// 高度换算成实际坐标。本模块只维护语义状态，不关心屏幕尺寸。
#[derive(Debug, Clone)]
pub struct FsBrowser {
    /// 当前所在目录（current working directory）。
    cwd: PathBuf,

    /// 当前目录下排序后的条目列表（已过滤隐藏文件和非音乐文件）。
    entries: Vec<Entry>,

    /// 选中条目的索引（0 起）。若 `entries` 为空，此值无意义但保持 0。
    selected: usize,

    /// 列表首行对应的条目索引（滚动偏移）。
    /// 渲染时从 `entries[scroll]` 开始向下画 `可视行数` 条。
    scroll: usize,

    /// 搜索关键字（空串 = 不过滤）。由 TUI 搜索框（`/` 键）写入。
    /// 搜索模式下 `entries` 被替换为递归过滤结果，退出时恢复一级条目。
    filter: String,

    /// 搜索模式标志：begin_search 置 true，end_search 置 false。
    searching: bool,

    /// 递归收集的整个目录树条目（搜索模式缓存，退出时清空）。
    search_all: Vec<Entry>,

    /// 搜索是否因超过条目数上限而被截断。
    /// 由 apply_search_collected 提交收集结果时设置（begin_search 先置
    /// false），渲染层据此提示"目录过大，仅搜索了部分内容"。
    search_truncated: bool,

    /// 目录树异步收集是否仍在进行（begin_search 置真，
    /// apply_search_collected / end_search 置假）：期间 set_filter 只记
    /// 关键字不过滤——缓存尚空，此刻过滤会把列表清成空白。
    search_collecting: bool,

    /// 进入搜索前的一级条目（退出搜索时恢复）。
    saved_entries: Vec<Entry>,

    /// 进入搜索前的选中项与滚动偏移（退出搜索时恢复）。
    saved_selected: usize,
    saved_scroll: usize,

    /// 最近一次目录读取的错误信息（若有）。
    /// 用 Option 而非 Result 字段，是因为浏览器是长期存活的状态，
    /// 错误属于"上一次操作的结果"而非浏览器本身的失败。TUI 渲染时
    /// 若有值则显示给用户（如"无权限访问"）。
    last_error: Option<String>,
}

impl FsBrowser {
    /// 创建浏览器并打开指定初始目录。
    ///
    /// 若初始目录无法读取（不存在、无权限），会回退到用户主目录，
    /// 再不行回退到当前工作目录 `"."`，保证浏览器总能展示**某个**目录，
    /// 不会因启动目录异常而卡在空状态。
    pub fn open(initial_dir: &Path) -> Self {
        let mut browser = Self {
            cwd: PathBuf::new(),
            entries: Vec::new(),
            selected: 0,
            scroll: 0,
            filter: String::new(),
            searching: false,
            search_all: Vec::new(),
            search_truncated: false,
            search_collecting: false,
            saved_entries: Vec::new(),
            saved_selected: 0,
            saved_scroll: 0,
            last_error: None,
        };
        // 尝试进入初始目录；失败则逐级回退
        if !browser.navigate_to(initial_dir) {
            // 回退 1：用户主目录
            let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
            if !browser.navigate_to(&home) {
                // 回退 2：当前工作目录（几乎一定可读）
                browser.navigate_to(Path::new("."));
            }
        }
        browser
    }

    /// 当前所在目录。
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// 当前目录的条目列表（已排序、已过滤）。
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// 选中条目的索引。
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// 滚动偏移（首行对应的条目索引）。
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// 最近一次搜索是否因条目数超限被截断。
    pub fn search_truncated(&self) -> bool {
        self.search_truncated
    }

    /// 最近一次错误信息（若有）。TUI 据此显示提示。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// 当前选中的条目（若列表非空）。
    ///
    /// 返回 `Option<&Entry>` 而非 `&Entry`，因为空目录时无选中项，
    /// 调用方（Enter 播放 / a 加入列表）需要优雅处理 None。
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// 进入搜索模式：保存当前一级条目状态，标记「目录树收集中」。
    ///
    /// 收集已异步化（App 侧后台线程调 [`collect_recursive_entries`]，
    /// 结果经 [`FsBrowser::apply_search_collected`] 提交）：收集期间
    /// entries 维持一级列表、[`FsBrowser::set_filter`] 只记关键字不过滤，结果到达后
    /// 按当前关键字过滤立即生效。必须成对调用 [`FsBrowser::end_search`]
    /// 恢复一级条目。
    pub fn begin_search(&mut self) {
        self.saved_entries = self.entries.clone();
        self.saved_selected = self.selected;
        self.saved_scroll = self.scroll;
        self.search_all = Vec::new();
        self.search_truncated = false;
        self.searching = true;
        self.search_collecting = true;
        self.filter.clear();
        // 不替换 entries：等待异步收集结果（apply_search_collected）。
    }

    /// 提交异步收集的目录树结果（主循环在代次匹配后调用）。
    ///
    /// 到达后按**当前关键字**重新过滤一遍，让收集期间已输入的内容立即生效；
    /// 若用户已退出搜索（Esc / 导航），直接丢弃返回。
    pub fn apply_search_collected(&mut self, entries: Vec<Entry>, truncated: bool) {
        if !self.searching {
            return;
        }
        self.search_all = entries;
        self.search_truncated = truncated;
        self.search_collecting = false;
        self.entries = filter_entries_by_name(&self.search_all, &self.filter);
        self.selected = 0;
        self.scroll = 0;
    }

    /// 更新搜索关键字：从递归收集的缓存中过滤，跳到首个匹配项。
    ///
    /// 前置条件：已调用 [`FsBrowser::begin_search`]。每次关键字变化
    /// （增/删字符）都调用，保证选中项始终落在过滤结果内。
    /// 收集仍在进行（search_collecting）时只记录关键字、不动列表——
    /// 缓存还是空的，此刻过滤会把列表清成空白；结果到达时
    /// [`FsBrowser::apply_search_collected`] 会按当前关键字补一次过滤立即生效。
    pub fn set_filter(&mut self, query: &str) {
        self.filter = query.to_string();
        if self.search_collecting {
            return;
        }
        self.entries = filter_entries_by_name(&self.search_all, &self.filter);
        self.selected = 0;
        self.scroll = 0;
    }

    /// 退出搜索模式：恢复进入搜索前的一级条目、选中项与滚动偏移。
    ///
    /// 若不在搜索模式（searching == false）则直接返回，避免重复调用
    /// 导致 entries 被意外清空。
    pub fn end_search(&mut self) {
        if !self.searching {
            return;
        }
        self.entries = std::mem::take(&mut self.saved_entries);
        self.selected = self.saved_selected;
        self.scroll = self.saved_scroll;
        self.filter.clear();
        self.search_all.clear();
        self.search_collecting = false;
        self.searching = false;
    }

    /// 导航到指定目录，重新读取并排序条目。
    ///
    /// 成功返回 true 并重置选中项与滚动偏移到顶部（进入新目录后
    /// 理应从第一项开始看）。失败（目录不存在、无权限、不是目录）
    /// 返回 false 并记录错误信息，**不改变现有状态**——这样用户
    /// 在误输入路径或权限不足时，浏览器保持在原目录，不会"丢失"。
    pub fn navigate_to(&mut self, target: &Path) -> bool {
        // 规范化路径：把相对路径解析为绝对路径，避免显示成 ".." 这类
        // 难以理解的形式。canonicalize 要求路径存在，正好顺便校验。
        let resolved = match target.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                self.last_error = Some(format!("无法打开目录“{}”：{e}", target.display()));
                return false;
            }
        };

        // 确认目标是目录而非文件（canonicalize 对文件也成功）
        if !resolved.is_dir() {
            self.last_error = Some(format!("“{}”不是目录", resolved.display()));
            return false;
        }

        // 读取目录条目（读目录 + 过滤 + 排序 + Windows 盘符）。
        let entries = match compute_entries(&resolved) {
            Ok(e) => e,
            Err(e) => {
                self.last_error = Some(e);
                return false;
            }
        };
        self.apply_loaded(resolved, entries);
        true
    }

    /// 用已载入的目录条目提交导航状态（cwd + 条目 + 复位选中/滚动/搜索）。
    pub(crate) fn apply_loaded(&mut self, cwd: PathBuf, entries: Vec<Entry>) {
        self.cwd = cwd;
        self.entries = entries;
        self.selected = 0;
        self.scroll = 0;
        self.filter = String::new();
        self.search_all.clear();
        self.searching = false;
        self.last_error = None;
    }

    /// 返回上一级目录。
    ///
    /// 到达文件系统根目录（无 parent）时返回 false，浏览器保持不动。
    /// 这是 Unix `/` 和 Windows `C:\` 等根目录的自然边界。
    pub fn go_up(&mut self) -> bool {
        match self.cwd.parent() {
            Some(parent) => {
                let parent = parent.to_path_buf();
                self.navigate_to(&parent)
            }
            // 已在根目录，无上级可返回
            None => false,
        }
    }

    /// 进入当前选中的条目（仅当选中项为目录时有效）。
    ///
    /// 对音乐文件调用此方法返回 false（文件应交给播放器，而非"进入"）。
    /// 这种"类型驱动的行为分离"让 TUI 的 Enter 键逻辑很清晰：
    /// 若 enter_selected() 成功则已进入新目录；否则说明选中的是文件，
    /// 调用方转而触发播放。
    pub fn enter_selected(&mut self) -> bool {
        match self.current() {
            Some(Entry::Dir { path, .. }) => {
                let path = path.clone();
                self.navigate_to(&path)
            }
            // 文件或空列表：不进入
            _ => false,
        }
    }

    /// 校验目标目录并返回规范化路径（不读目录）。供异步导航使用。
    pub(crate) fn resolve_target(&self, target: &Path) -> Result<PathBuf, String> {
        let resolved = target
            .canonicalize()
            .map_err(|e| format!("无法打开目录“{}”：{e}", target.display()))?;
        if !resolved.is_dir() {
            return Err(format!("“{}”不是目录", resolved.display()));
        }
        Ok(resolved)
    }

    /// 选中项为目录时返回其路径（供异步进入），否则 None。
    pub(crate) fn selected_dir(&self) -> Option<PathBuf> {
        match self.current() {
            Some(Entry::Dir { path, .. }) => Some(path.clone()),
            _ => None,
        }
    }

    /// 选中项上移一格。
    ///
    /// 已在顶部时保持不动（不循环到列表末尾，避免操作不可预测）。
    /// `entries` 本身就是当前显示列表（搜索时为过滤结果），
    /// 因此这里直接按 entries 索引移动即可。
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// 选中项下移一格。
    ///
    /// 已在末尾时保持不动。
    pub fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    /// 移到列表第一项。
    pub fn move_to_top(&mut self) {
        self.selected = 0;
    }

    /// 移到列表最后一项。
    pub fn move_to_bottom(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.entries.len() - 1;
        }
    }

    /// 根据可视区域高度调整滚动偏移，确保选中项始终可见。
    ///
    /// TUI 渲染层在每次绘制前调用此方法，传入可视行数（区域高度），
    /// 本方法据此调整 `scroll`：
    /// - 若选中项在 `scroll` 之前（上方不可见），上移 `scroll`；
    /// - 若选中项超出 `scroll + visible`（下方不可见），下移 `scroll`。
    ///
    /// 这种"选中项驱动滚动"的逻辑让 ↑↓ 移动时视图自动跟随，
    /// 符合所有列表 UI 的通用习惯。
    pub fn ensure_visible(&mut self, visible: usize) {
        if self.entries.is_empty() || visible == 0 {
            return;
        }
        // 选中项在可视区上方 → 把 scroll 提到选中项
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        // 选中项在可视区下方 → 把 scroll 推到"选中项 - visible + 1"
        // 让选中项恰好出现在可视区最后一行
        if self.selected >= self.scroll + visible {
            // 饱和减法，避免 underflow
            self.scroll = self.selected.saturating_sub(visible) + 1;
        }
    }

    /// 收集指定目录（递归）下所有支持的音乐文件路径。
    ///
    /// 用于"把整个目录加入播放列表"（按 `a` 键）。
    pub fn collect_music_recursive(dir: &Path, out: &mut Vec<PathBuf>) {
        // 顶层调用：初始化已访问集合，并把起始目录的真实路径放入。
        // 用 canonicalize 而非原始路径，是因为软链会让"逻辑路径"和
        // "真实路径"不同——判重必须基于真实路径才有意义。
        let mut visited = std::collections::HashSet::new();
        if let Ok(real) = dir.canonicalize() {
            visited.insert(real);
        }
        Self::collect_inner(dir, out, &mut visited);
    }

    /// `collect_music_recursive` 的内部递归实现，携带已访问集合。
    /// 抽成独立函数是为了让公开接口签名简洁（无需暴露 visited 参数）。
    fn collect_inner(
        dir: &Path,
        out: &mut Vec<PathBuf>,
        visited: &mut std::collections::HashSet<PathBuf>,
    ) {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return, // 无权限等：跳过此目录
        };
        for entry in rd.flatten() {
            let path = entry.path();

            // 跳过非 UTF-8 文件名与隐藏文件/目录（以 . 开头），
            // 与浏览器展示策略一致。
            let file_name = entry.file_name();
            let name = match file_name.to_str() {
                Some(s) => s,
                None => continue,
            };
            if name.starts_with('.') {
                continue;
            }

            if path.is_dir() {
                #[cfg(windows)]
                if dir_is_hidden_system(&path) {
                    continue;
                }
                // 符号链接环防护：解析真实路径，已访问则跳过。
                // canonicalize 失败（如目标不存在）的坏链接也直接跳过。
                let real = match path.canonicalize() {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if visited.insert(real.clone()) {
                    // insert 返回 true 表示是新路径，继续递归
                    Self::collect_inner(&path, out, visited);
                }
                // 若已在集合中（insert 返回 false），说明遇到环，跳过
            } else if is_supported(&path) {
                out.push(path);
            }
        }
    }
}

/// 递归搜索的条目数上限。超过后停止收集并置截断标志，
/// 避免在超大目录（家目录、根目录等）下按 `/` 时 UI 卡死。
const SEARCH_MAX_ENTRIES: usize = 20_000;

/// 递归搜索的进度状态：已收集条目数 + 是否被截断。
///
/// 把计数与截断标志合并成一个结构体，用单个 `&mut` 参数传入递归
/// 函数，避免参数过多（clippy too_many_arguments）。
struct SearchProgress {
    /// 已收集的条目数（目录 + 音乐文件）。
    count: usize,
    /// 是否因达到 [`SEARCH_MAX_ENTRIES`] 上限被截断。
    truncated: bool,
}

/// 读取并过滤一个已规范化目录的条目：读目录、UTF-8/隐藏过滤、目录/文件分类、
/// 排序（目录在前、文件在后）；Windows 盘符根额外列出其他盘符。
/// 供同步导航（navigate_to）与后台异步载入共用。
pub(crate) fn compute_entries(resolved: &Path) -> Result<Vec<Entry>, String> {
    let read = std::fs::read_dir(resolved)
        .map_err(|e| format!("无法读取目录“{}”：{e}", resolved.display()))?;
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let name = match entry.file_name().to_str() {
            Some(s) => s.to_owned(),
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            #[cfg(windows)]
            if dir_is_hidden_system(&path) {
                continue;
            }
            dirs.push(Entry::Dir { name, path });
        } else if is_supported(&path) {
            files.push(Entry::File { name, path });
        }
    }
    #[cfg(windows)]
    {
        let mut comps = resolved.components();
        let is_drive_root = matches!(comps.next(), Some(std::path::Component::Prefix(_)))
            && matches!(comps.next(), Some(std::path::Component::RootDir))
            && comps.next().is_none();
        if is_drive_root {
            let current_drive = resolved
                .components()
                .find_map(|c| match c {
                    std::path::Component::Prefix(p) => match p.kind() {
                        std::path::Prefix::Disk(d) => Some(d as char),
                        std::path::Prefix::VerbatimDisk(d) => Some(d as char),
                        _ => None,
                    },
                    _ => None,
                })
                .unwrap_or('C');
            for letter in b'A'..=b'Z' {
                let letter = letter as char;
                if letter == current_drive {
                    continue;
                }
                let root = format!("{letter}:\\");
                let path = PathBuf::from(&root);
                if path.exists() {
                    dirs.push(Entry::Dir { name: root, path });
                }
            }
        }
    }
    sort_by_name(&mut dirs);
    sort_by_name(&mut files);
    dirs.extend(files);
    Ok(dirs)
}

/// 对条目列表按显示名不区分大小写升序排序。
///
/// 抽成独立函数便于目录和文件分别调用。用 `to_lowercase` 比较
/// 而非 `to_ascii_lowercase`，是为了让非 ASCII 文件名（如中文、
/// 带变音符号）也能按语言习惯排序——`to_ascii_lowercase` 只处理
/// ASCII，遇到"É"等字符大小写判定会失真。
fn sort_by_name(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let na = a.name().to_lowercase();
        let nb = b.name().to_lowercase();
        na.cmp(&nb)
    });
}

/// 按关键字过滤条目（名称包含关键字，不区分大小写）。
///
/// 关键字为空时返回全部。用于浏览器搜索：从递归缓存的完整目录树中
/// 筛出匹配项。
fn filter_entries_by_name(entries: &[Entry], query: &str) -> Vec<Entry> {
    if query.is_empty() {
        return entries.to_vec();
    }
    let q = query.to_lowercase();
    entries
        .iter()
        .filter(|e| e.name().to_lowercase().contains(&q))
        .cloned()
        .collect()
}

/// 递归收集 cwd 下所有目录与音乐文件（含子目录），用于浏览器搜索。
///
/// 条目 name 使用**相对 cwd 的路径**（如 "专辑/01 - 稻香.mp3"），让用户在
/// 搜索结果里一眼看出文件位于哪个子目录。带符号链接环防护与条目数上限
/// （超过 [`SEARCH_MAX_ENTRIES`] 截断）。自由函数（只读磁盘、无 UI 状态），
/// 供 App 的后台线程异步调用——大目录按 `/` 不再卡 UI。
pub(crate) fn collect_recursive_entries(cwd: &Path) -> (Vec<Entry>, bool) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    let mut visited = std::collections::HashSet::new();
    if let Ok(real) = cwd.canonicalize() {
        visited.insert(real);
    }
    let mut progress = SearchProgress {
        count: 0,
        truncated: false,
    };
    walk_recursive(
        cwd,
        cwd,
        &mut dirs,
        &mut files,
        &mut visited,
        SEARCH_MAX_ENTRIES,
        &mut progress,
    );
    // 目录在前、文件在后，同类按名排序
    sort_by_name(&mut dirs);
    sort_by_name(&mut files);
    dirs.extend(files);
    (dirs, progress.truncated)
}

/// 递归遍历目录树，收集目录与音乐文件（带符号链接环防护）。
///
/// 相对路径始终以 `base`（收集起点）为基准；每收集一个条目 `progress.count`
/// 加一，达到 `max_entries` 时置 `progress.truncated` 并停止，防止超大
/// 目录遍历拖垮调用方。
fn walk_recursive(
    base: &Path,
    dir: &Path,
    dirs: &mut Vec<Entry>,
    files: &mut Vec<Entry>,
    visited: &mut std::collections::HashSet<PathBuf>,
    max_entries: usize,
    progress: &mut SearchProgress,
) {
    if progress.truncated {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return, // 无权限等：跳过此目录
    };
    for entry in rd.flatten() {
        if progress.truncated {
            return;
        }
        let path = entry.path();

        // UTF-8 严格策略（见模块文档）：跳过非 UTF-8 文件名。
        let Some(name) = entry.file_name().to_str().map(|s| s.to_owned()) else {
            continue;
        };
        // 跳过隐藏文件/目录
        if name.starts_with('.') {
            continue;
        }

        // 相对 base 的显示名（各段文件名均已是有效 UTF-8）。
        let rel_name = path
            .strip_prefix(base)
            .ok()
            .and_then(|r| r.to_str().map(|s| s.to_owned()))
            .unwrap_or_else(|| name.clone());

        if path.is_dir() {
            #[cfg(windows)]
            if dir_is_hidden_system(&path) {
                continue;
            }
            // 符号链接环防护：解析真实路径，已访问则跳过
            let real = match path.canonicalize() {
                Ok(r) => r,
                Err(_) => continue,
            };
            if visited.insert(real) {
                walk_recursive(base, &path, dirs, files, visited, max_entries, progress);
                dirs.push(Entry::Dir {
                    name: rel_name,
                    path,
                });
                progress.count += 1;
                if progress.count >= max_entries {
                    progress.truncated = true;
                    return;
                }
            }
        } else if is_supported(&path) {
            files.push(Entry::File {
                name: rel_name,
                path,
            });
            progress.count += 1;
            if progress.count >= max_entries {
                progress.truncated = true;
                return;
            }
        }
    }
}

/// Windows：判断目录是否带「隐藏 / 系统」文件属性——浏览器与递归扫描应跳过
/// （$RECYCLE.BIN、System Volume Information 等系统目录读入慢且对音乐浏览无意义）。
/// 仅用于目录分支；隐藏的音乐文件不受影响（与主流播放器一致）。
#[cfg(windows)]
fn dir_is_hidden_system(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    std::fs::metadata(path)
        .map(|md| md.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0)
        .unwrap_or(false)
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `is_supported` 能正确识别各扩展名（含大写）。
    #[test]
    fn detects_supported_extensions() {
        assert!(is_supported(Path::new("song.mp3")));
        assert!(is_supported(Path::new("song.FLAC"))); // 大写也算
        assert!(is_supported(Path::new("track.m4a")));
        assert!(!is_supported(Path::new("readme.txt")));
        assert!(!is_supported(Path::new("noext")));
        assert!(!is_supported(Path::new("cover.jpg")));
    }

    /// 在临时目录构造一个混合内容目录，验证排序与过滤：
    /// 目录在前、文件在后，同类按名称排序，非音乐文件与隐藏文件不显示。
    #[test]
    fn lists_and_sorts_entries() {
        let tmp = std::env::temp_dir().join("tuneux_browser_test");
        // 清理可能残留的旧测试目录
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // 构造测试结构：
        //   tmp/
        //     zAlbum/            （目录，名字靠后，但因是目录应排在前）
        //     aSong.mp3          （音乐文件）
        //     .hidden.mp3        （隐藏文件，应被过滤）
        //     readme.txt         （非音乐，应被过滤）
        //     bsong.flac         （音乐文件，小写 b 在 a 后）
        fs::create_dir_all(tmp.join("zAlbum")).unwrap();
        fs::File::create(tmp.join("aSong.mp3")).unwrap();
        fs::File::create(tmp.join(".hidden.mp3")).unwrap();
        fs::File::create(tmp.join("readme.txt")).unwrap();
        fs::File::create(tmp.join("bsong.flac")).unwrap();

        let browser = FsBrowser::open(&tmp);
        let names: Vec<&str> = browser.entries().iter().map(|e| e.name()).collect();

        // 期望顺序：目录 zAlbum 在前，文件按名排序 aSong、bsong
        assert_eq!(names, vec!["zAlbum", "aSong.mp3", "bsong.flac"]);
        assert_eq!(browser.selected(), 0);

        // 清理
        let _ = fs::remove_dir_all(&tmp);
    }

    /// 上下移动选中项的边界：到顶/到底不越界。
    #[test]
    fn move_selection_bounds() {
        let tmp = std::env::temp_dir().join("tuneux_browser_move_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("a.mp3")).unwrap();
        fs::File::create(tmp.join("b.mp3")).unwrap();
        fs::File::create(tmp.join("c.mp3")).unwrap();

        let mut br = FsBrowser::open(&tmp);
        assert_eq!(br.selected(), 0);

        // 顶部再上移：不动
        br.move_up();
        assert_eq!(br.selected(), 0);

        // 下移两次到末尾
        br.move_down();
        br.move_down();
        assert_eq!(br.selected(), 2);

        // 末尾再下移：不动
        br.move_down();
        assert_eq!(br.selected(), 2);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 递归搜索：能匹配子目录下的文件，条目名含相对路径。
    #[test]
    fn search_matches_files_in_subdirectories() {
        let tmp = std::env::temp_dir().join("tuneux_browser_filter_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("周杰伦")).unwrap();
        fs::File::create(tmp.join("周杰伦/稻香.mp3")).unwrap(); // 子目录文件
        fs::File::create(tmp.join("青花瓷.mp3")).unwrap();
        fs::File::create(tmp.join("晴天.flac")).unwrap();

        let mut br = FsBrowser::open(&tmp);
        // 初始一级条目：1 目录 + 2 文件 = 3
        assert_eq!(br.entries().len(), 3);

        // 进入搜索：收集已异步化，这里同步模拟主循环的提交路径
        //（后台线程调 collect_recursive_entries，结果经 apply_search_collected）。
        br.begin_search();
        assert_eq!(br.entries().len(), 3, "收集结果到达前维持一级列表");
        let collected = collect_recursive_entries(br.cwd());
        br.apply_search_collected(collected.0, collected.1);
        assert_eq!(br.entries().len(), 4, "递归收集应含子目录文件");

        // 搜索"稻"：只匹配子目录里的 稻香.mp3
        br.set_filter("稻");
        assert_eq!(br.entries().len(), 1, "应匹配子目录里的稻香.mp3");
        assert_eq!(br.entries()[0].name(), "周杰伦/稻香.mp3", "应显示相对路径");
        assert_eq!(br.selected(), 0, "应跳到首个匹配项");

        // 退出搜索：恢复一级条目
        br.end_search();
        assert_eq!(br.entries().len(), 3, "退出后应恢复一级条目");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 收集未到达期间键入关键字：列表必须维持一级条目（不被空缓存清空），
    /// 收集结果到达后按已输入的关键字过滤立即生效。
    /// 回归旧缺陷：set_filter 无条件从空的 search_all 过滤，第一次键入
    /// 就把列表清成空白，大目录收集期间浏览器长时间空白。
    #[test]
    fn set_filter_during_collection_keeps_one_level_list() {
        let tmp = std::env::temp_dir().join("tuneux_browser_collecting_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("周杰伦")).unwrap();
        fs::File::create(tmp.join("周杰伦/稻香.mp3")).unwrap();
        fs::File::create(tmp.join("青花瓷.mp3")).unwrap();
        fs::File::create(tmp.join("晴天.flac")).unwrap();

        let mut br = FsBrowser::open(&tmp);
        assert_eq!(br.entries().len(), 3);

        // 进入搜索后、结果到达前键入关键字：一级列表必须保留
        br.begin_search();
        br.set_filter("稻");
        assert_eq!(br.entries().len(), 3, "收集期间键入不应清空一级列表");

        // 结果到达：按已输入的关键字补过滤，立即只剩匹配项
        let collected = collect_recursive_entries(br.cwd());
        br.apply_search_collected(collected.0, collected.1);
        assert_eq!(br.entries().len(), 1, "结果到达后应按当前关键字过滤");
        assert_eq!(br.entries()[0].name(), "周杰伦/稻香.mp3");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 递归搜索后 ↑↓ 在过滤结果内移动。
    #[test]
    fn move_selection_within_filter() {
        let tmp = std::env::temp_dir().join("tuneux_browser_filter_move_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("子目录")).unwrap();
        fs::File::create(tmp.join("a歌.mp3")).unwrap();
        fs::File::create(tmp.join("other.mp3")).unwrap();
        fs::File::create(tmp.join("子目录/b歌.mp3")).unwrap();
        fs::File::create(tmp.join("子目录/c歌.flac")).unwrap();

        let mut br = FsBrowser::open(&tmp);
        br.begin_search();
        let collected = collect_recursive_entries(br.cwd());
        br.apply_search_collected(collected.0, collected.1);
        br.set_filter("歌");
        // 匹配：a歌.mp3、子目录/b歌.mp3、子目录/c歌.flac（按名排序）
        assert_eq!(br.entries().len(), 3, "应匹配 3 个含'歌'的文件（含子目录）");
        assert_eq!(br.selected(), 0);

        // 下移两次应停在最后一个匹配项
        br.move_down();
        br.move_down();
        assert_eq!(br.selected(), 2);
        // 再下移不动（过滤结果边界）
        br.move_down();
        assert_eq!(br.selected(), 2);

        // 上移回到首个匹配项
        br.move_to_top();
        assert_eq!(br.selected(), 0);

        br.end_search();

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 递归搜索截断：超过上限后停止收集并置截断标志。
    #[test]
    fn walk_recursive_truncates_at_limit() {
        let tmp = std::env::temp_dir().join("tuneux_search_truncate_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // 造 5 个音乐文件
        for i in 0..5 {
            fs::File::create(tmp.join(format!("{i}.mp3"))).unwrap();
        }

        let _br = FsBrowser::open(&tmp);
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(tmp.canonicalize().unwrap());
        let mut progress = SearchProgress {
            count: 0,
            truncated: false,
        };

        // 上限 3：应只收集 3 个文件并置截断标志
        walk_recursive(
            &tmp,
            &tmp,
            &mut dirs,
            &mut files,
            &mut visited,
            3,
            &mut progress,
        );

        assert!(progress.truncated, "超过上限应置截断标志");
        assert_eq!(files.len(), 3, "应只收集 3 个文件");
        assert_eq!(progress.count, 3);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// `ensure_visible` 在选中项超出可视区时正确下推滚动偏移。
    #[test]
    fn ensure_visible_scrolls() {
        let tmp = std::env::temp_dir().join("tuneux_browser_scroll_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // 造 10 个文件
        for i in 0..10 {
            fs::File::create(tmp.join(format!("{i}.mp3"))).unwrap();
        }

        let mut br = FsBrowser::open(&tmp);
        // 选中第 8 项，可视区高度 3：应下推 scroll 让第 8 项可见
        br.selected = 8;
        br.ensure_visible(3);
        // scroll 应为 8 - 3 + 1 = 6
        assert_eq!(br.scroll(), 6);

        // 选中第 1 项：应上拉 scroll 到 1
        br.selected = 1;
        br.ensure_visible(3);
        assert_eq!(br.scroll(), 1);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 进入子目录与返回上级配对工作。
    #[test]
    fn enter_and_go_up() {
        let tmp = std::env::temp_dir().join("tuneux_browser_nav_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::File::create(tmp.join("sub").join("inner.mp3")).unwrap();

        let mut br = FsBrowser::open(&tmp);
        // 当前目录下应有一个 sub 目录
        assert_eq!(br.entries().len(), 1);
        assert!(br.current().unwrap().is_dir());

        // 进入 sub
        assert!(br.enter_selected());
        // sub 内有 inner.mp3
        assert_eq!(br.entries().len(), 1);
        assert_eq!(br.entries()[0].name(), "inner.mp3");

        // 返回上级应回到 tmp
        assert!(br.go_up());
        assert_eq!(br.entries().len(), 1);
        assert_eq!(br.entries()[0].name(), "sub");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// navigate_to 对不存在的路径返回 false，且不破坏当前状态。
    #[test]
    fn navigate_invalid_keeps_state() {
        let tmp = std::env::temp_dir().join("tuneux_browser_invalid_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("keep.mp3")).unwrap();

        let mut br = FsBrowser::open(&tmp);
        let cwd_before = br.cwd().to_path_buf();

        // 尝试导航到不存在的路径
        let ok = br.navigate_to(&tmp.join("does_not_exist"));
        assert!(!ok, "导航到不存在路径应失败");
        // 当前目录与条目应保持不变
        assert_eq!(br.cwd(), cwd_before);
        assert_eq!(br.entries().len(), 1);
        // 应记录错误信息
        assert!(br.last_error().is_some());

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 递归收集音乐文件应包含所有层级，并跳过非音乐文件。
    #[test]
    fn collect_recursive() {
        let tmp = std::env::temp_dir().join("tuneux_browser_recur_test");
        let _ = fs::remove_dir_all(&tmp);
        // 结构：
        //   tmp/a.mp3
        //   tmp/sub1/b.flac
        //   tmp/sub1/sub2/c.wav
        //   tmp/sub1/notmusic.txt
        fs::create_dir_all(tmp.join("sub1/sub2")).unwrap();
        fs::File::create(tmp.join("a.mp3")).unwrap();
        fs::File::create(tmp.join("sub1/b.flac")).unwrap();
        fs::File::create(tmp.join("sub1/sub2/c.wav")).unwrap();
        fs::File::create(tmp.join("sub1/notmusic.txt")).unwrap();

        let mut collected = Vec::new();
        FsBrowser::collect_music_recursive(&tmp, &mut collected);
        // 应收集到 3 个音乐文件（不含 txt）
        assert_eq!(collected.len(), 3);

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 非 UTF-8 文件名严格跳过：保证浏览器永不显示乱码。
    /// 仅在 Unix 测试（Unix 文件名为任意字节，可构造非法 UTF-8）。
    #[cfg(unix)]
    #[test]
    fn non_utf8_filenames_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let tmp = std::env::temp_dir().join("tuneux_browser_nonutf8_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // 正常 UTF-8 文件名
        fs::File::create(tmp.join("正常.mp3")).unwrap();
        // 非法 UTF-8 字节序列（0xFF 0xFE 在 UTF-8 中非法）
        let bad_name: OsString = OsString::from_vec(vec![0xFF, 0xFE, b'.', b'm', b'p', b'3']);
        let _ = std::fs::File::create(tmp.join(&bad_name));

        let browser = FsBrowser::open(&tmp);
        let names: Vec<&str> = browser.entries().iter().map(|e| e.name()).collect();
        // 只应有"正常.mp3"一个条目，非法 UTF-8 文件名被跳过
        assert_eq!(names, vec!["正常.mp3"], "非 UTF-8 文件名应被跳过");

        // collect 也应跳过非 UTF-8 文件名
        let mut collected = Vec::new();
        FsBrowser::collect_music_recursive(&tmp, &mut collected);
        assert_eq!(collected.len(), 1, "递归收集同样跳过非 UTF-8 名");

        let _ = fs::remove_dir_all(&tmp);
    }

    /// 符号链接环防护：自指/互指软链不应导致无限递归。
    /// 仅在 Unix 测试（Windows 创建符号链接需管理员权限，CI 不便）。
    #[cfg(unix)]
    #[test]
    fn collect_recursive_handles_symlink_loop() {
        use std::os::unix::fs::symlink;
        let tmp = std::env::temp_dir().join("tuneux_browser_symlink_test");
        let _ = fs::remove_dir_all(&tmp);
        // 结构：
        //   tmp/song.mp3
        //   tmp/sub/         （真实子目录）
        //   tmp/sub/inner.flac
        //   tmp/sub/loop     → tmp（指向祖先，构成环）
        //   tmp/selfref      → tmp（指向自身上级，构成另一个环）
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::File::create(tmp.join("song.mp3")).unwrap();
        fs::File::create(tmp.join("sub/inner.flac")).unwrap();
        // sub/loop → tmp（回到祖先）
        symlink(&tmp, tmp.join("sub/loop")).unwrap();
        // tmp/selfref → tmp（指向自身）
        symlink(&tmp, tmp.join("selfref")).unwrap();

        // 若无环防护，此调用会栈溢出（测试进程崩溃而非正常断言失败）
        let mut collected = Vec::new();
        FsBrowser::collect_music_recursive(&tmp, &mut collected);

        // 应只收集到 2 个真实音乐文件，不会因环而重复或卡死
        // （song.mp3 出现一次，inner.flac 一次）
        let mp3_count = collected
            .iter()
            .filter(|p| p.extension().unwrap_or_default() == "mp3")
            .count();
        let flac_count = collected
            .iter()
            .filter(|p| p.extension().unwrap_or_default() == "flac")
            .count();
        assert_eq!(mp3_count, 1, "song.mp3 只应被收集一次");
        assert_eq!(flac_count, 1, "inner.flac 只应被收集一次");

        let _ = fs::remove_dir_all(&tmp);
    }
}
