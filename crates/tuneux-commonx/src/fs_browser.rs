//! 文件浏览器的数据 / 导航内核。
//!
//! 提供目录浏览机制：列出目录内容、过滤音乐文件、递归收集、管理导航
//! 状态（当前目录、选中项、滚动偏移、搜索）。与任何 UI 呈现解耦——
//! 渲染、unicode 列宽截断、异步通道与代次计数留在产品层；max 的 egui
//! 文件浏览器直接复用本模块，另写渲染。
//!
//! # 事实与策略注入
//!
//! 「什么算可播」是内核的事实（`KNOWN_AUDIO_EXTS`），但本模块不依赖内核——
//! 由产品经 [`FsBrowserConfig::audio_exts`] 传入扩展名清单（含 `.cue` 与否
//! 由产品决定）。机制留通用、清单是参数。
//!
//! ## 目录条目排序规则
//!
//! 1. **子目录在前，文件在后**——目录是"可进入"的容器，置顶更符合
//!    文件管理器的习惯；
//! 2. **同类内按文件名不区分大小写升序**——避免大小写差异导致排序跳跃。
//!
//! ## 文件名编码：UTF-8 严格策略（零乱码）
//!
//! 文件名**非有效 UTF-8** 的条目一律跳过（`OsStr::to_str()` 严格判定，
//! 不用 `to_string_lossy()` 容忍替换成占位符），从源头杜绝乱码。

use std::path::{Path, PathBuf};

/// 目录浏览的构造配置（事实注入，零内部依赖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FsBrowserConfig {
    /// 可播音频扩展名（小写、不含点）。来自内核 `KNOWN_AUDIO_EXTS`。
    /// `.cue` 索引文件是通用音乐概念（非产品策略），本模块一律视为
    /// 可进入条目（见 [`is_cue_file`]），无需在此清单中单独声明。
    pub audio_exts: &'static [&'static str],
}

/// 判断文件路径是否为受支持的音频文件（按扩展名，大小写不敏感）。
///
/// 判断依据仅看扩展名（不读文件头）：浏览器要在用户每次进入目录时
/// 即时列出，读文件头会有可感延迟；真实格式由解码时再校验。
fn is_supported(path: &Path, audio_exts: &[&str]) -> bool {
    match path.extension() {
        Some(ext) => {
            let ext_lower = ext.to_string_lossy().to_lowercase();
            audio_exts.contains(&ext_lower.as_str())
        }
        None => false,
    }
}

/// 路径是否 `.cue` 分轨索引文件（大小写不敏感）。
pub fn is_cue_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
}

/// 一条目录条目（目录或音乐文件）。
///
/// 用枚举而非统一结构体（带 is_dir 标志），是因为目录和文件在
/// UI 中的行为完全不同（目录可进入、文件可播放），枚举让这些
/// 差异在类型层面体现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// 子目录。`name` 为显示名（不含路径），`path` 为完整路径。
    Dir {
        /// 显示名。
        name: String,
        /// 完整路径。
        path: PathBuf,
    },
    /// 音乐文件。同样保存显示名与完整路径。
    File {
        /// 显示名。
        name: String,
        /// 完整路径。
        path: PathBuf,
    },
}

impl Entry {
    /// 条目的显示名（用于列表渲染）。
    pub fn name(&self) -> &str {
        match self {
            Entry::Dir { name, .. } => name,
            Entry::File { name, .. } => name,
        }
    }

    /// 条目的完整路径。
    pub fn path(&self) -> &Path {
        match self {
            Entry::Dir { path, .. } => path,
            Entry::File { path, .. } => path,
        }
    }

    /// 是否为目录。
    pub fn is_dir(&self) -> bool {
        matches!(self, Entry::Dir { .. })
    }
}

/// 文件浏览器的导航与显示状态。
///
/// 持有"浏览器当前看到什么"的全部信息：当前目录（`cwd`）、排序后的
/// 条目列表（`entries`）、选中项（`selected`）、滚动偏移（`scroll`）、
/// 搜索状态。
///
/// `selected` 与 `scroll` 是**导航状态**（"用户在目录树的哪个位置"），
/// 与 Playlist 的 UI 呈现状态不同质——任何文件浏览器（TUI 或 egui GUI）
/// 都有「当前选中项」概念，因此随本模块下沉；产品渲染据此换算坐标。
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

    /// 搜索关键字（空串 = 不过滤）。由产品搜索框写入。
    /// 搜索模式下 `entries` 被替换为递归过滤结果，退出时恢复一级条目。
    filter: String,

    /// 搜索模式标志：begin_search 置 true，end_search 置 false。
    searching: bool,

    /// 递归收集的整个目录树条目（搜索模式缓存，退出时清空）。
    search_all: Vec<Entry>,

    /// 搜索是否因超过条目数上限而被截断。
    search_truncated: bool,

    /// 目录树异步收集是否仍在进行：期间 set_filter 只记关键字不过滤。
    search_collecting: bool,

    /// 进入搜索前的一级条目（退出搜索时恢复）。
    saved_entries: Vec<Entry>,

    /// 进入搜索前的选中项与滚动偏移（退出搜索时恢复）。
    saved_selected: usize,
    saved_scroll: usize,

    /// 最近一次目录读取的错误信息（若有）。
    last_error: Option<String>,

    /// 目录浏览配置（音频扩展名清单）。
    config: FsBrowserConfig,
}

impl FsBrowser {
    /// 创建浏览器并打开指定初始目录。
    ///
    /// 若初始目录无法读取（不存在、无权限），会回退到用户主目录，
    /// 再不行回退到当前工作目录 `"."`，保证浏览器总能展示**某个**目录。
    pub fn open(initial_dir: &Path, config: FsBrowserConfig) -> Self {
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
            config,
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

    /// 最近一次错误信息（若有）。
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// 当前选中的条目（若列表非空）。
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// 进入搜索模式：保存当前一级条目状态，标记「目录树收集中」。
    ///
    /// 收集已异步化（产品侧后台线程调 [`collect_recursive_entries`]，
    /// 结果经 [`FsBrowser::apply_search_collected`] 提交）：收集期间
    /// entries 维持一级列表、[`FsBrowser::set_filter`] 只记关键字不过滤。
    /// 必须成对调用 [`FsBrowser::end_search`] 恢复一级条目。
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
    /// 收集仍在进行（search_collecting）时只记录关键字、不动列表——
    /// 缓存还是空的，此刻过滤会把列表清成空白；结果到达时
    /// [`FsBrowser::apply_search_collected`] 会按当前关键字补一次过滤。
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
    /// 成功返回 true 并重置选中项与滚动偏移到顶部。失败（目录不存在、
    /// 无权限、不是目录）返回 false 并记录错误信息，**不改变现有状态**。
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
        let entries = match compute_entries(&resolved, self.config) {
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
    pub fn apply_loaded(&mut self, cwd: PathBuf, entries: Vec<Entry>) {
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
    pub fn resolve_target(&self, target: &Path) -> Result<PathBuf, String> {
        let resolved = target
            .canonicalize()
            .map_err(|e| format!("无法打开目录“{}”：{e}", target.display()))?;
        if !resolved.is_dir() {
            return Err(format!("“{}”不是目录", resolved.display()));
        }
        Ok(resolved)
    }

    /// 选中项为目录时返回其路径（供异步进入），否则 None。
    pub fn selected_dir(&self) -> Option<PathBuf> {
        match self.current() {
            Some(Entry::Dir { path, .. }) => Some(path.clone()),
            _ => None,
        }
    }

    /// 选中项上移一格（已在顶部时保持不动）。
    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// 选中项下移一格（已在末尾时保持不动）。
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

    /// 直接选中指定行（鼠标点选入口，GUI 发行版用）。
    ///
    /// 越界钳制到有效范围；列表为空时无操作（selected 保持 0，
    /// 与字段不变式一致）。
    pub fn select(&mut self, index: usize) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = index.min(self.entries.len() - 1);
    }

    /// 根据可视区域高度调整滚动偏移，确保选中项始终可见。
    ///
    /// 产品渲染层在每次绘制前调用此方法，传入可视行数（区域高度）。
    pub fn ensure_visible(&mut self, visible: usize) {
        if self.entries.is_empty() || visible == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + visible {
            self.scroll = self.selected.saturating_sub(visible) + 1;
        }
    }

    /// 收集指定目录（递归）下所有受支持的音频文件路径。
    ///
    /// 用于"把整个目录加入播放列表"。带符号链接环防护。
    pub fn collect_music_recursive(dir: &Path, out: &mut Vec<PathBuf>, config: FsBrowserConfig) {
        let mut visited = std::collections::HashSet::new();
        if let Ok(real) = dir.canonicalize() {
            visited.insert(real);
        }
        Self::collect_inner(dir, out, &mut visited, config);
    }

    /// [`collect_music_recursive`] 的内部递归实现，携带已访问集合。
    fn collect_inner(
        dir: &Path,
        out: &mut Vec<PathBuf>,
        visited: &mut std::collections::HashSet<PathBuf>,
        config: FsBrowserConfig,
    ) {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return, // 无权限等：跳过此目录
        };
        for entry in rd.flatten() {
            let path = entry.path();

            // UTF-8 严格策略（见模块文档）：跳过非 UTF-8 文件名。
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
                let real = match path.canonicalize() {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if visited.insert(real.clone()) {
                    Self::collect_inner(&path, out, visited, config);
                }
            } else if is_supported(&path, config.audio_exts) || is_cue_file(&path) {
                out.push(path);
            }
        }
    }
}

/// 递归搜索的条目数上限。超过后停止收集并置截断标志，
/// 避免在超大目录（家目录、根目录等）下按 `/` 时 UI 卡死。
const SEARCH_MAX_ENTRIES: usize = 20_000;

/// 递归搜索的进度状态：已收集条目数 + 是否被截断。
struct SearchProgress {
    count: usize,
    truncated: bool,
}

/// 读取并过滤一个已规范化目录的条目：读目录、UTF-8/隐藏过滤、目录/文件分类、
/// 排序（目录在前、文件在后）；Windows 盘符根额外列出其他盘符。
/// 供同步导航（navigate_to）与后台异步载入共用。
pub fn compute_entries(resolved: &Path, config: FsBrowserConfig) -> Result<Vec<Entry>, String> {
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
        } else if is_supported(&path, config.audio_exts) || is_cue_file(&path) {
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
fn sort_by_name(entries: &mut [Entry]) {
    entries.sort_by(|a, b| {
        let na = a.name().to_lowercase();
        let nb = b.name().to_lowercase();
        na.cmp(&nb)
    });
}

/// 按关键字过滤条目（名称包含关键字，不区分大小写）。
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
/// 条目 name 使用**相对 cwd 的路径**（如 "专辑/01 - 稻香.mp3"）。
/// 带符号链接环防护与条目数上限（超过上限截断，见私有常量
/// SEARCH_MAX_ENTRIES；截断时返回值第二项为 true）。
/// 自由函数（只读磁盘、无 UI 状态），供产品的后台线程异步调用。
pub fn collect_recursive_entries(cwd: &Path, config: FsBrowserConfig) -> (Vec<Entry>, bool) {
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
        config,
    );
    sort_by_name(&mut dirs);
    sort_by_name(&mut files);
    dirs.extend(files);
    (dirs, progress.truncated)
}

/// 递归遍历目录树，收集目录与音乐文件（带符号链接环防护）。
#[allow(clippy::too_many_arguments)]
fn walk_recursive(
    base: &Path,
    dir: &Path,
    dirs: &mut Vec<Entry>,
    files: &mut Vec<Entry>,
    visited: &mut std::collections::HashSet<PathBuf>,
    max_entries: usize,
    progress: &mut SearchProgress,
    config: FsBrowserConfig,
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
                walk_recursive(
                    base,
                    &path,
                    dirs,
                    files,
                    visited,
                    max_entries,
                    progress,
                    config,
                );
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
        } else if is_supported(&path, config.audio_exts) || is_cue_file(&path) {
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

    fn cfg() -> FsBrowserConfig {
        FsBrowserConfig {
            audio_exts: &["mp3", "flac", "m4a", "wav", "cue"],
        }
    }

    #[test]
    fn detects_supported_extensions() {
        assert!(is_supported(Path::new("song.mp3"), cfg().audio_exts));
        assert!(is_supported(Path::new("song.FLAC"), cfg().audio_exts));
        assert!(is_supported(Path::new("track.m4a"), cfg().audio_exts));
        assert!(!is_supported(Path::new("readme.txt"), cfg().audio_exts));
        assert!(!is_supported(Path::new("noext"), cfg().audio_exts));
        assert!(!is_supported(Path::new("cover.jpg"), cfg().audio_exts));
        assert!(is_cue_file(Path::new("album.CUE")));
        assert!(!is_cue_file(Path::new("album.mp3")));
    }

    #[test]
    fn lists_and_sorts_entries() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(tmp.join("zAlbum")).unwrap();
        fs::File::create(tmp.join("aSong.mp3")).unwrap();
        fs::File::create(tmp.join(".hidden.mp3")).unwrap();
        fs::File::create(tmp.join("readme.txt")).unwrap();
        fs::File::create(tmp.join("bsong.flac")).unwrap();

        let browser = FsBrowser::open(&tmp, cfg());
        let names: Vec<&str> = browser.entries().iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["zAlbum", "aSong.mp3", "bsong.flac"]);
        assert_eq!(browser.selected(), 0);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn select_clamps_and_ignores_empty() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_select_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("a.mp3")).unwrap();
        fs::File::create(tmp.join("b.mp3")).unwrap();

        let mut browser = FsBrowser::open(&tmp, cfg());
        browser.select(1);
        assert_eq!(browser.selected(), 1);
        // 越界钳制到末行
        browser.select(99);
        assert_eq!(browser.selected(), 1);
        let _ = fs::remove_dir_all(&tmp);

        // 空目录：无操作不 panic（open 对无效路径会回退 home/cwd，
        // 故用真实空目录验证空列表分支）
        let empty_dir = std::env::temp_dir().join("tuneux_commonx_browser_select_empty");
        let _ = fs::remove_dir_all(&empty_dir);
        fs::create_dir_all(&empty_dir).unwrap();
        let mut empty = FsBrowser::open(&empty_dir, cfg());
        assert!(empty.entries().is_empty());
        empty.select(5);
        assert_eq!(empty.selected(), 0);
        let _ = fs::remove_dir_all(&empty_dir);
    }

    #[test]
    fn move_selection_bounds() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_move_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("a.mp3")).unwrap();
        fs::File::create(tmp.join("b.mp3")).unwrap();
        fs::File::create(tmp.join("c.mp3")).unwrap();

        let mut br = FsBrowser::open(&tmp, cfg());
        assert_eq!(br.selected(), 0);
        br.move_up();
        assert_eq!(br.selected(), 0);
        br.move_down();
        br.move_down();
        assert_eq!(br.selected(), 2);
        br.move_down();
        assert_eq!(br.selected(), 2);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn search_matches_files_in_subdirectories() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_filter_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("周杰伦")).unwrap();
        fs::File::create(tmp.join("周杰伦/稻香.mp3")).unwrap();
        fs::File::create(tmp.join("青花瓷.mp3")).unwrap();
        fs::File::create(tmp.join("晴天.flac")).unwrap();

        let mut br = FsBrowser::open(&tmp, cfg());
        assert_eq!(br.entries().len(), 3);
        br.begin_search();
        assert_eq!(br.entries().len(), 3, "收集结果到达前维持一级列表");
        let collected = collect_recursive_entries(br.cwd(), cfg());
        br.apply_search_collected(collected.0, collected.1);
        assert_eq!(br.entries().len(), 4, "递归收集应含子目录文件");
        br.set_filter("稻");
        assert_eq!(br.entries().len(), 1, "应匹配子目录里的稻香.mp3");
        assert_eq!(br.entries()[0].name(), "周杰伦/稻香.mp3");
        br.end_search();
        assert_eq!(br.entries().len(), 3, "退出后应恢复一级条目");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn set_filter_during_collection_keeps_one_level_list() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_collecting_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("周杰伦")).unwrap();
        fs::File::create(tmp.join("周杰伦/稻香.mp3")).unwrap();
        fs::File::create(tmp.join("青花瓷.mp3")).unwrap();
        fs::File::create(tmp.join("晴天.flac")).unwrap();

        let mut br = FsBrowser::open(&tmp, cfg());
        assert_eq!(br.entries().len(), 3);
        br.begin_search();
        br.set_filter("稻");
        assert_eq!(br.entries().len(), 3, "收集期间键入不应清空一级列表");
        let collected = collect_recursive_entries(br.cwd(), cfg());
        br.apply_search_collected(collected.0, collected.1);
        assert_eq!(br.entries().len(), 1);
        assert_eq!(br.entries()[0].name(), "周杰伦/稻香.mp3");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn walk_recursive_truncates_at_limit() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_search_truncate_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for i in 0..5 {
            fs::File::create(tmp.join(format!("{i}.mp3"))).unwrap();
        }
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(tmp.canonicalize().unwrap());
        let mut progress = SearchProgress {
            count: 0,
            truncated: false,
        };
        walk_recursive(
            &tmp,
            &tmp,
            &mut dirs,
            &mut files,
            &mut visited,
            3,
            &mut progress,
            cfg(),
        );
        assert!(progress.truncated, "超过上限应置截断标志");
        assert_eq!(files.len(), 3);
        assert_eq!(progress.count, 3);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_visible_scrolls() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_scroll_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for i in 0..10 {
            fs::File::create(tmp.join(format!("{i}.mp3"))).unwrap();
        }
        let mut br = FsBrowser::open(&tmp, cfg());
        br.selected = 8;
        br.ensure_visible(3);
        assert_eq!(br.scroll(), 6);
        br.selected = 1;
        br.ensure_visible(3);
        assert_eq!(br.scroll(), 1);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn collect_recursive() {
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_recur_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub1/sub2")).unwrap();
        fs::File::create(tmp.join("a.mp3")).unwrap();
        fs::File::create(tmp.join("sub1/b.flac")).unwrap();
        fs::File::create(tmp.join("sub1/sub2/c.wav")).unwrap();
        fs::File::create(tmp.join("sub1/notmusic.txt")).unwrap();

        let mut collected = Vec::new();
        FsBrowser::collect_music_recursive(&tmp, &mut collected, cfg());
        assert_eq!(collected.len(), 3);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_filenames_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_nonutf8_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::File::create(tmp.join("正常.mp3")).unwrap();
        let bad_name: OsString = OsString::from_vec(vec![0xFF, 0xFE, b'.', b'm', b'p', b'3']);
        let _ = std::fs::File::create(tmp.join(&bad_name));

        let browser = FsBrowser::open(&tmp, cfg());
        let names: Vec<&str> = browser.entries().iter().map(|e| e.name()).collect();
        assert_eq!(names, vec!["正常.mp3"], "非 UTF-8 文件名应被跳过");

        let mut collected = Vec::new();
        FsBrowser::collect_music_recursive(&tmp, &mut collected, cfg());
        assert_eq!(collected.len(), 1);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn collect_recursive_handles_symlink_loop() {
        use std::os::unix::fs::symlink;
        let tmp = std::env::temp_dir().join("tuneux_commonx_browser_symlink_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("sub")).unwrap();
        fs::File::create(tmp.join("song.mp3")).unwrap();
        fs::File::create(tmp.join("sub/inner.flac")).unwrap();
        symlink(&tmp, tmp.join("sub/loop")).unwrap();
        symlink(&tmp, tmp.join("selfref")).unwrap();

        let mut collected = Vec::new();
        FsBrowser::collect_music_recursive(&tmp, &mut collected, cfg());

        let mp3_count = collected
            .iter()
            .filter(|p| p.extension().unwrap_or_default() == "mp3")
            .count();
        let flac_count = collected
            .iter()
            .filter(|p| p.extension().unwrap_or_default() == "flac")
            .count();
        assert_eq!(mp3_count, 1);
        assert_eq!(flac_count, 1);
        let _ = fs::remove_dir_all(&tmp);
    }
}
