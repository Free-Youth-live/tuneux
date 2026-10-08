//! CUE 分轨展开（fx 接线）。
//!
//! 解析与展开已下沉 `tuneux-mediax`；本模块只保留依赖 App 元数据缓存的入口。

use tuneux_mediax::cue::cue_items_from;

use super::App;

/// 展开结果：分轨条目 + 跳过的数据轨数（供 UI 提示）。
pub(crate) type CueExpansion = tuneux_mediax::cue::CueExpansion;

// 供 mod.rs 使用（点中 .cue 文件展开；批量后台构建已由共享扫描池接管）。
pub(crate) use tuneux_mediax::cue::cue_items_from_cue_file;

impl App {
    /// 若整轨文件旁存在同名 `.cue`，解析并展开为 CUE 曲目条目。
    ///
    /// 无 `.cue` / 解析失败时返回空（调用方回退为普通条目）。
    /// 元数据走缓存（单文件路径：浏览器 Enter / a 键文件分支等同步场景）。
    /// 批量后台构建已由共享扫描池接管（tuneux_mediax::scan_pool）。
    pub(crate) fn cue_items_for(&mut self, path: &std::path::Path) -> CueExpansion {
        let md = self.get_or_extract_metadata(path);
        cue_items_from(path, &md)
    }
}
