//! 列表与浏览器操作：清空、删除、加入播放列表、浏览器 Enter。
//!
//! 依赖 media / cue / playback，被 keys（handle_key）引用。

use crate::config::Config;
use crate::fs_browser;
use crate::playlist;
use tuneux_corex as audio;

use super::search::SearchTarget;
use super::App;

impl App {
    /// 清空播放列表（含停止播放、清空曲目元数据与歌词）。
    pub fn clear_playlist(&mut self) {
        if let Some(engine) = &self.engine {
            engine.send(audio::AudioCmd::Stop);
        }
        self.playlist.clear();
        self.current_metadata = None;
        self.current_path = None;
        self.current_lyrics = None;
        self.cover_cache = None;
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
            self.current_metadata = None;
            self.current_path = None;
            self.current_lyrics = None;
            self.cover_cache = None;
        }
        if self.playlist.is_empty() {
            self.current_metadata = None;
            self.current_path = None;
            self.current_lyrics = None;
            self.cover_cache = None;
        }
    }

    /// "a" 键：把浏览器当前选中条目加入播放列表。
    pub fn add_current_browser_to_playlist(&mut self, config: &mut Config) {
        let Some(entry) = self.browser.current() else {
            return;
        };
        let was_empty = self.playlist.is_empty();
        match entry {
            fs_browser::Entry::File { path, .. } => {
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
            }
            fs_browser::Entry::Dir { path, .. } => {
                let path = path.clone();
                let mut paths = Vec::new();
                fs_browser::FsBrowser::collect_music_recursive(&path, &mut paths);
                let mut items: Vec<playlist::PlaylistItem> = Vec::new();
                for p in paths {
                    let expanded = self.cue_items_for(&p);
                    if !expanded.is_empty() {
                        items.extend(expanded);
                    } else {
                        let md = self.get_or_extract_metadata(&p);
                        items.push(playlist::PlaylistItem {
                            path: p,
                            album: md.album,
                            track_number: md.track_number,
                            cue: None,
                        });
                    }
                }
                // 按"专辑-曲序"预排序后加入：让新增批次的插入序 = 显示序，
                // 这样添加目录后自动播放/选中的第一首就是专辑-曲序的第一首。
                playlist::Playlist::sort_items(&mut items);
                // 按配置决定是否去重
                if config.dedup_on_add {
                    self.playlist.add_many_dedup(items);
                } else {
                    self.playlist.add_many(items);
                }
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

    /// CUE 分轨曲目：把 seek 目标钳制在本曲区间 [start, end] 内，
    /// 防止快进/快退越界到整轨的其他曲目。
    /// 普通曲目（cue 为 None）原样返回；end_ms 为 None（末曲）只钳下限。
    /// 浏览器 Enter：进入目录或播放文件（普通模式与搜索模式共用）。
    ///
    /// 搜索模式下定位到目标后自动退出搜索——用户搜索的目的就是快速
    /// 跳到某个文件/目录，找到后理应立即回到正常浏览。
    pub(crate) fn handle_browser_enter(&mut self, config: &mut Config) {
        let was_searching = self.search_mode && self.search_target == SearchTarget::Browser;
        if self.browser.enter_selected() {
            // 已进入目录（navigate_to 内部会清空 filter）
        } else if let Some(fs_browser::Entry::File { path, .. }) = self.browser.current() {
            let path = path.clone();
            let md = self.get_or_extract_metadata(&path);
            let item = playlist::PlaylistItem {
                path,
                album: md.album.clone(),
                track_number: md.track_number,
                cue: None,
            };
            // 按配置决定是否去重；去重时若已存在则不跳转
            let added = if config.dedup_on_add {
                self.playlist.add_dedup(item)
            } else {
                self.playlist.add(item);
                true
            };
            if added {
                let new_idx = self.playlist.items().len() - 1;
                // 手动点选播放：压入 history，使 p 键能回到点选前的曲目。
                self.playlist.jump_to(new_idx);
                self.play_and_update_current(new_idx, config);
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
