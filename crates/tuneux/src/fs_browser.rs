//! 文件浏览器（基础版接线）。
//!
//! 数据 / 导航内核（`Entry` / `FsBrowser` / 递归收集 / 符号链接环防护 /
//! Windows 盘符与隐藏目录过滤）已下沉 `tuneux-commonx`；本模块只做产品
//! 策略注入——「什么算可播」以内核事实 `KNOWN_AUDIO_EXTS` 为参数传入。

use std::path::Path;

pub use tuneux_commonx::fs_browser::{Entry, FsBrowser, FsBrowserConfig};

/// tuneux 认识的音乐文件扩展名（小写，不含点），清单由内核统一维护
/// （`tuneux_corex::KNOWN_AUDIO_EXTS`：原生 + ffmpeg 长尾）。浏览器只负责
/// 显示"认识"的格式；能否播放由解码时判定。
pub use tuneux_corex::KNOWN_AUDIO_EXTS as SUPPORTED_EXTS;

/// 目录浏览构造配置：音频扩展名清单注入（`.cue` 由 commonx 统一视为
/// 可进入条目，无需在此声明）。
pub fn browser_config() -> FsBrowserConfig {
    FsBrowserConfig {
        audio_exts: SUPPORTED_EXTS,
    }
}

/// 路径是否 `.cue` 分轨索引文件（大小写不敏感）。
pub fn is_cue_file(path: &Path) -> bool {
    tuneux_commonx::fs_browser::is_cue_file(path)
}

/// 递归收集 cwd 下所有目录与音乐文件（含子目录），用于浏览器搜索。
/// 结果条目 name 使用相对 cwd 的路径；带符号链接环防护与条目数上限。
pub fn collect_recursive_entries(cwd: &Path) -> (Vec<Entry>, bool) {
    tuneux_commonx::fs_browser::collect_recursive_entries(cwd, browser_config())
}

/// 读取并过滤一个已规范化目录的条目：读目录、UTF-8/隐藏过滤、目录/文件
/// 分类、排序；Windows 盘符根额外列出其他盘符。
pub fn compute_entries(resolved: &Path) -> Result<Vec<Entry>, String> {
    tuneux_commonx::fs_browser::compute_entries(resolved, browser_config())
}
