//! 列表与浏览器操作：清空、删除、加入播放列表、浏览器 Enter。
//!
//! 依赖 media / cue / playback，被 keys（handle_key）引用。

use std::path::PathBuf;

use crate::config::Config;
use crate::fs_browser;
use crate::playlist;
use tuneux_corex as audio;
use tuneux_mediax::metadata;

use super::search::SearchTarget;
use super::App;

impl App {
    /// 清空播放列表（含停止播放、清空曲目元数据与歌词）。
    pub fn clear_playlist(&mut self) {
        if let Some(engine) = &self.engine {
            engine.send(audio::AudioCmd::Stop);
        }
        self.playlist.clear();
        self.reset_current_track_state();
        // 清空后残留的二次确认状态一并复位（否则下次 x 仍处于"待确认"）。
        self.pending_clear = false;
        self.pending_clear_at = None;
    }

    /// 删除选中的曲目（若选中的是曲目）。
    ///
    /// 删除当前播放项时停止播放；列表清空时清空曲目元数据与歌词。
    pub fn remove_selected_track(&mut self) {
        let Some(playlist::Selection::Track(index)) = self.playlist.selected().cloned() else {
            return;
        };
        let was_current = self.playlist.current_index() == Some(index);
        self.playlist.remove(index);

        if was_current {
            if let Some(engine) = &self.engine {
                engine.send(audio::AudioCmd::Stop);
            }
            self.reset_current_track_state();
        }
        if self.playlist.is_empty() {
            self.reset_current_track_state();
        }
    }

    /// 清空「当前曲目」相关状态（元数据 / 路径 / 频谱 / 歌词 / 封面缓存），
    /// 供删除当前曲、清空列表等场景复用（与 fx 同名方法同源）。
    fn reset_current_track_state(&mut self) {
        self.current_metadata = None;
        self.current_path = None;
        self.spectrum_peaks.borrow_mut().reset();
        self.current_lyrics = None;
        self.cover_cache = None;
        self.cover_thumb = None;
        self.cover_failed_path = None;
        self.cover_thumb_cache.clear();
    }

    /// "a" 键：把浏览器当前选中条目加入播放列表。
    ///
    /// 文件分支同步加入（单个文件，开销小）；目录分支异步递归加入
    /// （[`App::add_dir_async`]），结果由主循环合入——空列表的自动首播
    /// 也在合入时触发（[`App::apply_dir_add`]）。
    pub fn add_current_browser_to_playlist(&mut self, config: &mut Config) {
        let Some(entry) = self.browser.current() else {
            return;
        };
        match entry {
            fs_browser::Entry::File { path, .. } => {
                let was_empty = self.playlist.is_empty();
                let path = path.clone();
                // 整轨 + 同名 .cue → 展开为多首 CUE 曲目；否则按普通文件加入
                let expanded = self.cue_items_for(&path);
                if !expanded.is_empty() {
                    // 按配置决定是否去重（CUE 分轨按 (path, cue.index) 判定唯一性）
                    if config.dedup_on_add {
                        self.playlist.add_many_dedup(expanded);
                    } else {
                        self.playlist.add_many(expanded);
                    }
                } else {
                    let md = self.get_or_extract_metadata(&path);
                    let item = playlist::PlaylistItem {
                        path,
                        album: md.album,
                        track_number: md.track_number,
                        cue: None,
                    };
                    if config.dedup_on_add {
                        self.playlist.add_dedup(item);
                    } else {
                        self.playlist.add(item);
                    }
                }
                if was_empty && !self.playlist.is_empty() {
                    // 播放/选中"专辑-曲序"排序后的第一首，而非插入序第 0 个。
                    // （预排序后二者通常相同；跨批次添加时仍以全局显示序为准。）
                    let first = self.playlist.display_order().first().copied().unwrap_or(0);
                    self.playlist.set_selected(first);
                    self.play_and_update_current(first, config);
                }
            }
            fs_browser::Entry::Dir { path, .. } => {
                // 异步递归加入：目录遍历、CUE 展开、逐文件元数据探测全在
                // 后台线程（重活不卡 UI），结果由主循环合入。
                let path = path.clone();
                self.add_dir_async(path);
            }
        }
    }

    /// 异步递归收集目录并构建播放列表条目（`a` 键加目录用）：
    /// 后台线程完成目录遍历、CUE 展开与逐文件元数据探测（重活全在后台，
    /// 元数据缓存优先——与同步路径同口径），结果由主循环轮询后经
    /// [`App::apply_dir_add`] 批量合入。大目录加入不再卡 UI。
    pub(crate) fn add_dir_async(&mut self, dir: PathBuf) {
        // 快照当前元数据缓存给后台线程（命中免探测）；只读不写，无竞争。
        let cache = self.metadata_cache.clone();
        let tx = self.add_load_tx.clone();
        self.last_error = Some(format!("正在扫描目录：{}", dir.display()));
        self.last_error_at = Some(std::time::Instant::now());
        // 优雅降级：线程启动失败（极罕见）时提示而非 panic。
        if std::thread::Builder::new()
            .name("tuneux-dir-add".to_string())
            .spawn(move || {
                let mut paths = Vec::new();
                fs_browser::FsBrowser::collect_music_recursive(&dir, &mut paths);
                let (mut items, mds) = super::cue::build_items_for_paths(&paths, &cache);
                // 按"专辑-曲序"预排序：让新增批次的插入序 = 显示序，
                // 添加目录后自动播放/选中的第一首就是专辑-曲序的第一首。
                playlist::Playlist::sort_items(&mut items);
                let _ = tx.send((dir, items, mds));
            })
            .is_err()
        {
            self.last_error = Some("目录加入线程启动失败".to_string());
            self.last_error_at = Some(std::time::Instant::now());
        }
    }

    /// 合入一次后台目录加入的结果（主循环轮询与测试共用）：
    /// 新探测的元数据补入缓存、条目批量加入（按配置去重）、
    /// 加入前列表为空时自动播放第一首、完成提示。
    pub(crate) fn apply_dir_add(
        &mut self,
        dir: PathBuf,
        items: Vec<playlist::PlaylistItem>,
        mds: Vec<(PathBuf, metadata::TrackMetadata)>,
        config: &Config,
    ) {
        let was_empty = self.playlist.is_empty();
        let before = self.playlist.len();
        let empty_dir = items.is_empty();
        for (path, md) in mds {
            // 只补缺失项：不覆盖已有缓存（含当前曲目等热条目）。
            self.metadata_cache.entry(path).or_insert(md);
        }
        if config.dedup_on_add {
            self.playlist.add_many_dedup(items);
        } else {
            self.playlist.add_many(items);
        }
        let added = self.playlist.len() - before;
        if empty_dir || self.playlist.is_empty() {
            self.last_error = Some(format!("目录中无音乐文件：{}", dir.display()));
            self.last_error_at = Some(std::time::Instant::now());
            return;
        }
        if added == 0 {
            self.last_error = Some(format!("未加入新曲目（均已存在）：{}", dir.display()));
            self.last_error_at = Some(std::time::Instant::now());
            return;
        }
        if was_empty {
            // 加入前列表为空：播放/选中"专辑-曲序"排序后的第一首。
            let first = self.playlist.display_order().first().copied().unwrap_or(0);
            self.playlist.set_selected(first);
            self.play_and_update_current(first, config);
        }
        self.last_error = Some(format!("已加入 {added} 首：{}", dir.display()));
        self.last_error_at = Some(std::time::Instant::now());
    }

    /// 异步收集当前目录树（浏览器搜索 `/` 用）：后台线程递归收集，
    /// 结果由主循环轮询后经 [`fs_browser::FsBrowser::apply_search_collected`]
    /// 提交；收集期间浏览器维持一级列表，已输入的关键字在结果到达后生效。
    pub(crate) fn search_async(&mut self) {
        let cwd = self.browser.cwd().to_path_buf();
        let gen = self.search_load_gen;
        self.search_load_gen = self.search_load_gen.wrapping_add(1);
        let tx = self.search_load_tx.clone();
        // 优雅降级：线程启动失败（极罕见）时提示而非 panic。
        if std::thread::Builder::new()
            .name("tuneux-search-load".to_string())
            .spawn(move || {
                let (entries, truncated) = crate::fs_browser::collect_recursive_entries(&cwd);
                let _ = tx.send((gen, entries, truncated));
            })
            .is_err()
        {
            self.last_error = Some("搜索收集线程启动失败".to_string());
            self.last_error_at = Some(std::time::Instant::now());
        }
    }

    /// 异步导航到目录：后台线程读目录，结果由主循环轮询后 apply_loaded。
    /// 大目录/网络盘不再卡 UI；快速连续导航只应用最新一次（代次计数丢陈旧结果）。
    pub(crate) fn navigate_async(&mut self, target: &std::path::Path) {
        match self.browser.resolve_target(target) {
            Ok(resolved) => {
                let gen = self.dir_load_gen;
                self.dir_load_gen = self.dir_load_gen.wrapping_add(1);
                let tx = self.dir_load_tx.clone();
                // 优雅降级：线程启动失败（极罕见）时提示而非 panic。
                if std::thread::Builder::new()
                    .name("tuneux-dir-load".to_string())
                    .spawn(move || {
                        let result = crate::fs_browser::compute_entries(&resolved);
                        let _ = tx.send((gen, resolved, result));
                    })
                    .is_err()
                {
                    self.last_error = Some("目录载入线程启动失败".to_string());
                    self.last_error_at = Some(std::time::Instant::now());
                }
            }
            Err(e) => {
                self.last_error = Some(e);
                self.last_error_at = Some(std::time::Instant::now());
            }
        }
    }

    /// 浏览器 Enter：进入目录或播放文件（普通模式与搜索模式共用）。
    ///
    /// 搜索模式下定位到目标后自动退出搜索——用户搜索的目的就是快速
    /// 跳到某个文件/目录，找到后理应立即回到正常浏览。
    pub(crate) fn handle_browser_enter(&mut self, config: &mut Config) {
        let was_searching = self.search_mode && self.search_target == SearchTarget::Browser;
        if let Some(dir) = self.browser.selected_dir() {
            // 异步进入目录（后台线程读，不卡 UI）。
            self.navigate_async(&dir);
        } else if let Some(fs_browser::Entry::File { path, .. }) = self.browser.current() {
            let path = path.clone();
            // 整轨 + 同名 .cue → 展开为多首 CUE 曲目，加入并播放本整轨第一曲。
            let expanded = self.cue_items_for(&path);
            if !expanded.is_empty() {
                let start_idx = self.playlist.items().len();
                if config.dedup_on_add {
                    self.playlist.add_many_dedup(expanded);
                } else {
                    self.playlist.add_many(expanded);
                }
                // 播放首个加入的分轨；若全部去重命中，则按 path 定位第一曲。
                let play_idx = if start_idx < self.playlist.items().len() {
                    start_idx
                } else {
                    self.playlist
                        .items()
                        .iter()
                        .position(|it| it.path == path)
                        .unwrap_or(0)
                };
                self.playlist.jump_to(play_idx);
                self.play_and_update_current(play_idx, config);
            } else {
                let md = self.get_or_extract_metadata(&path);
                let item = playlist::PlaylistItem {
                    path: path.clone(),
                    album: md.album.clone(),
                    track_number: md.track_number,
                    cue: None,
                };
                // 按配置决定是否去重；去重命中时定位并立即播放（与 fx 同口径）。
                let added = if config.dedup_on_add {
                    self.playlist.add_dedup(item)
                } else {
                    self.playlist.add(item);
                    true
                };
                if !added {
                    // 去重命中：文件已在列表，定位并立即播放（而非静默无反应）。
                    if let Some(idx) = self.playlist.items().iter().position(|it| it.path == path) {
                        self.playlist.jump_to(idx);
                        self.play_and_update_current(idx, config);
                    }
                } else {
                    let new_idx = self.playlist.len() - 1;
                    // 手动点选播放：压入 history，使 p 键能回到点选前的曲目。
                    self.playlist.jump_to(new_idx);
                    self.play_and_update_current(new_idx, config);
                }
            }
        }
        // 搜索模式下定位到目标后退出搜索
        if was_searching {
            self.search_mode = false;
            self.search_query.clear();
            self.browser.end_search();
        }
    }
}
