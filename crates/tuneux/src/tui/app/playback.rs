//! 播放控制：播放/暂停、音量、seek（含 CUE 钳制）、切曲全流程、引擎轮询。
//!
//! 依赖 media（歌词/元数据），被 actions / keys 引用。

use crate::config::Config;
use crate::playlist;
use tuneux_corex as audio;

use super::App;

/// 切曲生效守卫帧数：Play/PlayResume/Seek 经通道异步生效，期间 `position()`
/// 仍是旧值。守卫期内主循环不做 CUE 终点判定（防点选更早分轨被误判连跳）。
pub(crate) const SWITCH_GUARD_FRAMES: u32 = 15;

impl App {
    /// 当前条目的 CUE 引用（克隆；普通曲目为 None）。
    fn current_cue(&self) -> Option<playlist::CueRef> {
        self.playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.clone())
    }

    /// 当前条目是否为 CUE 分轨（断点保存等场景的守卫）。
    pub(crate) fn current_item_is_cue(&self) -> bool {
        self.playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .is_some_and(|it| it.cue.is_some())
    }

    /// 拉取 engine 的最新错误，并清理过期的错误显示。
    pub fn refresh_last_error(&mut self) {
        if let Some(engine) = &self.engine {
            if let Some(err) = engine.take_last_error() {
                self.last_error = Some(err);
                self.last_error_at = Some(std::time::Instant::now());
            }
        }
        if let Some(at) = self.last_error_at {
            if at.elapsed() > std::time::Duration::from_secs(5) {
                self.last_error = None;
                self.last_error_at = None;
            }
        }
    }

    /// 切到列表指定项，触发播放 + 更新元数据。
    pub fn play_and_update_current(&mut self, index: usize, config: &Config) {
        // 切曲前：存旧位置（供下次接着听）。CUE 分轨不写断点——断点表以文件
        // 路径为键，整轨位置会覆盖该文件作为普通曲目播放时的续播点。
        let old_is_cue = self.current_item_is_cue();
        if !old_is_cue {
            if let Some(old_path) = &self.current_path {
                if let Some(engine) = &self.engine {
                    let pos = engine.position();
                    self.playlist_state.save_position(old_path, pos);
                }
            }
        }
        // 只设置 current（不 push history）：history 由 next/prev/jump_to 维护，
        // 此处重复 push 会破坏 shuffle 模式下 prev 的正确性。
        if !self.playlist.set_current(index) {
            return;
        }
        self.playlist.set_selected(index);
        let Some(item) = self.playlist.items().get(index) else {
            return;
        };
        let album_opt = item.album.clone();
        let path = item.path.clone();
        // CUE 引用提前克隆：避免与下方可变借用（get_or_extract_metadata）冲突。
        let cue_override = item.cue.clone();
        // 展开当前曲目所在专辑，保证 ByAlbum 视图下选中曲目可见。
        if let Some(album) = album_opt {
            self.playlist.expand_album(&album);
        }
        // 记住切换前的路径：同一整轨文件的 CUE 分轨切换走文件内 Seek（不重开）。
        let prev_path = self.current_path.clone();
        let same_file = prev_path.as_ref() == Some(&path);
        self.current_metadata = Some(self.get_or_extract_metadata(&path));
        // CUE 分轨：标题/表演者优先取 .cue 内的曲目信息——与列表口径一致，
        // 状态栏与系统媒体面板显示分轨名而非整轨专辑名。
        if let Some(cue) = cue_override.as_ref() {
            if let Some(md) = self.current_metadata.as_mut() {
                md.title = Some(cue.title.clone());
                if let Some(performer) = cue.performer.clone() {
                    md.artist = Some(performer);
                }
            }
        }
        // 元数据缓存不带封面字节；命中缓存切回时封面缺失，为当前曲目补提取一次，
        // 已确认无封面的文件记负标记，不再重复整文件标签探测。
        if !same_file {
            if let Some(md) = self.current_metadata.as_mut() {
                if md.cover.is_none() && !self.coverless_files.contains(&path) {
                    md.cover = tuneux_mediax::metadata::TrackMetadata::from_file(&path).cover;
                    if md.cover.is_none() {
                        self.coverless_files.insert(path.clone());
                    }
                }
            }
        }
        self.current_path = Some(path.clone());
        // 新曲目：清空频谱峰值白帽，避免上一曲高峰压在新曲前奏上。
        self.spectrum_peaks.borrow_mut().reset();
        if same_file {
            // 同一整轨文件的分轨切换：封面/歌词复用，避免每个分轨边界多一次解码+读盘。
        } else {
            self.cover_cache = None;
            self.current_lyrics = self.load_lyrics_for(&path);
        }

        // 排空残留事件前先取走滞留的测量增益（旧曲的测量结果，不能滞留被新曲错领）。
        let mut stale_gain = None;
        if let Some(engine) = &self.engine {
            stale_gain = engine.take_measured_gain_db();
            while engine.poll_track_switched().is_some() {}
            while engine.poll_finished().is_some() {}
            while engine.poll_failed().is_some() {}

            if let Some(cue) = cue_override.as_ref() {
                // CUE 分轨：从 CUE 起点起播（整轨内片段，不参与断点续播）。
                let start_secs = cue.start_ms as f64 / 1000.0;
                if same_file && engine.is_playing() {
                    // 同一整轨文件正在播：仅文件内 Seek——不重开文件（免卡顿）、
                    // 不重置响度分析器（测量跨分轨连续，不被切分清零）。
                    engine.send(audio::AudioCmd::Seek(start_secs));
                } else {
                    engine.send(audio::AudioCmd::PlayResume {
                        path: path.clone(),
                        secs: start_secs,
                    });
                }
            } else {
                // 断点续播：距结尾 5 秒内视为已听完，从头播；否则接着上次。
                let saved_secs = self.playlist_state.get_position(&path);
                let is_already_finished = match (
                    saved_secs,
                    self.current_metadata.as_ref().and_then(|m| m.duration),
                ) {
                    (Some(s), Some(dur)) => s >= dur - 5.0,
                    _ => false,
                };
                let resume_secs = saved_secs.filter(|&s| s > 0.5 && !is_already_finished);
                match resume_secs {
                    Some(secs) => engine.send(audio::AudioCmd::PlayResume {
                        path: path.clone(),
                        secs,
                    }),
                    None => engine.send(audio::AudioCmd::Play(path.clone())),
                }
            }
            // ReplayGain：查缓存应用整曲增益（dB → 线性；首次播放无缓存 = 1.0）。
            // 所有切曲路径统一应用（含 CUE），避免沿用上一曲的残留增益。
            let rg_gain = self
                .replay_gain_cache
                .get(&path)
                .copied()
                .map(|db| 10f64.powf(db / 20.0) as f32)
                .unwrap_or(1.0);
            engine.set_replay_gain(rg_gain);
            // Gapless：预载下一曲（顺序播放预测；shuffle/单曲循环返回 None 不预载）。
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
        // 旧曲滞留的测量增益归档（下次播放旧曲时直接复用，免重测）。
        if let (Some(db), Some(old)) = (stale_gain, prev_path) {
            self.replay_gain_cache.insert(old, db);
        }
        // 切曲生效守卫：Play/Seek 异步生效，守卫期内主循环不做 CUE 终点判定。
        self.switch_guard = SWITCH_GUARD_FRAMES;
    }

    /// Gapless 无缝切曲后的前端状态同步。
    ///
    /// 解码线程已无缝切换到预载曲目（Engine::poll_track_switched 事件触发），
    /// 这里**只更新 UI 状态**（当前曲目索引/元数据/进度基准），不重发 Play。
    pub fn advance_ui_on_gapless(&mut self, config: &Config) {
        let outcome = self.playlist.next(config.repeat);
        let playlist::NavOutcome::Switch(idx) = outcome else {
            return;
        };
        if !self.playlist.set_current(idx) {
            return;
        }
        self.playlist.set_selected(idx);
        if let Some(item) = self.playlist.items().get(idx) {
            let album_opt = item.album.clone();
            let path = item.path.clone();
            let cue = item.cue.clone();
            let same_file = self.current_path.as_ref() == Some(&path);
            self.current_metadata = Some(self.get_or_extract_metadata(&path));
            // CUE 分轨：标题/表演者优先取分轨信息，与列表及状态栏口径一致。
            if let Some(cue) = cue.as_ref() {
                if let Some(md) = self.current_metadata.as_mut() {
                    md.title = Some(cue.title.clone());
                    if let Some(performer) = cue.performer.clone() {
                        md.artist = Some(performer);
                    }
                }
            }
            self.current_path = Some(path.clone());
            if same_file {
                // 同一整轨文件：封面/歌词复用，不重复解码与读盘。
            } else {
                self.cover_cache = None;
                // 命中缓存切回时封面缺失则补提取；已确认无封面的文件有负标记。
                if let Some(md) = self.current_metadata.as_mut() {
                    if md.cover.is_none() && !self.coverless_files.contains(&path) {
                        md.cover = tuneux_mediax::metadata::TrackMetadata::from_file(&path).cover;
                        if md.cover.is_none() {
                            self.coverless_files.insert(path.clone());
                        }
                    }
                }
                self.current_lyrics = self.load_lyrics_for(&path);
            }
            // 展开所在专辑，保证 ByAlbum 视图下当前曲可见。
            if let Some(album) = album_opt {
                self.playlist.expand_album(&album);
            }
        }
        // ReplayGain：无缝切到新曲，应用新曲增益（避免沿用上一曲残留增益）。
        if let Some(path) = &self.current_path {
            let rg_gain = self
                .replay_gain_cache
                .get(path)
                .copied()
                .map(|db| 10f64.powf(db / 20.0) as f32)
                .unwrap_or(1.0);
            if let Some(engine) = &self.engine {
                engine.set_replay_gain(rg_gain);
            }
        }
        // 无缝切曲后，继续预载再下一曲。
        if let Some(engine) = &self.engine {
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
    }

    /// 切到下一首（按 repeat + shuffle 策略）。
    pub fn advance_to_next_track(&mut self, config: &Config) {
        let outcome = self.playlist.next(config.repeat);
        self.handle_nav_outcome(outcome, config);
    }

    /// 切到上一首（按 repeat + shuffle 策略）。
    pub fn advance_to_prev_track(&mut self, config: &Config) {
        let outcome = self.playlist.prev(config.repeat);
        self.handle_nav_outcome(outcome, config);
    }

    /// 把 NavOutcome 翻译成实际的播放/暂停操作
    pub fn handle_nav_outcome(&mut self, outcome: playlist::NavOutcome, config: &Config) {
        match outcome {
            playlist::NavOutcome::Switch(idx) => {
                self.play_and_update_current(idx, config);
            }
            playlist::NavOutcome::Repeat => {
                // 单曲循环：重新发 Play 让流再起（CUE 分轨从片段起点起播）。
                let path_cue = self.playlist.current_index().and_then(|curr| {
                    self.playlist
                        .items()
                        .get(curr)
                        .map(|it| (it.path.clone(), it.cue.clone()))
                });
                if let Some((path, cue)) = path_cue {
                    let mut stale_gain = None;
                    if let Some(engine) = &self.engine {
                        // 清残留事件并取走滞留的测量增益（归档到本曲），
                        // 避免旧切曲事件触发额外自动切曲。
                        stale_gain = engine.take_measured_gain_db();
                        while engine.poll_track_switched().is_some() {}
                        while engine.poll_finished().is_some() {}
                        while engine.poll_failed().is_some() {}
                        if let Some(c) = cue {
                            if engine.is_playing() {
                                // 播放中的 CUE 单曲循环：回片段起点走文件内 Seek——
                                // 不重开文件（免周期性卡顿），响度分析器连续。
                                engine.send(audio::AudioCmd::Seek(c.start_ms as f64 / 1000.0));
                            } else {
                                engine.send(audio::AudioCmd::PlayResume {
                                    path: path.clone(),
                                    secs: c.start_ms as f64 / 1000.0,
                                });
                            }
                        } else {
                            engine.send(audio::AudioCmd::Play(path.clone()));
                        }
                        // 单曲循环无"下一曲"：显式取消残留预载。
                        engine.send(audio::AudioCmd::PreloadNext(None));
                        // 同曲重复应用同一增益（幂等），切曲路径统一。
                        let rg_gain = self
                            .replay_gain_cache
                            .get(&path)
                            .copied()
                            .map(|db| 10f64.powf(db / 20.0) as f32)
                            .unwrap_or(1.0);
                        engine.set_replay_gain(rg_gain);
                    }
                    if let Some(db) = stale_gain {
                        self.replay_gain_cache.insert(path, db);
                    }
                    self.switch_guard = SWITCH_GUARD_FRAMES;
                }
            }
            playlist::NavOutcome::End => {}
        }
    }

    /// CUE 分轨曲目：把 seek 目标钳制在本曲区间 [start, end] 内，
    /// 防止快进/快退越界到整轨的其他曲目。
    /// 普通曲目（cue 为 None）原样返回；end_ms 为 None（末曲）只钳下限。
    pub(crate) fn clamp_cue_seek(&self, pos: f64) -> f64 {
        let Some(cue) = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.as_ref())
        else {
            return pos;
        };
        let start = cue.start_ms as f64 / 1000.0;
        let p = pos.max(start);
        match cue.end_ms {
            Some(e) => p.min(e as f64 / 1000.0),
            None => p,
        }
    }

    /// 刷新预载目标：循环/随机模式变化后调用，重发 PreloadNext。
    /// 顺序模式预载下一曲；单曲循环/随机返回 None，清掉旧预载（否则
    /// 旧预载曲会在切曲时抢先播放，声音与界面错位）。
    pub(crate) fn refresh_preload(&mut self, config: &Config) {
        if let Some(engine) = &self.engine {
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
    }

    /// 播放/暂停切换：正在播放则暂停（暂停前保存进度供断点续播），否则恢复。
    ///
    /// 从硬编码空格键分支提取，供默认键与自定义键复用同一逻辑。
    pub(crate) fn toggle_play(&mut self) {
        // 无曲目时忽略：避免向空闲引擎发 Resume 造成"playing=true 却无流"的脏状态。
        if self.current_path.is_none() {
            return;
        }
        // 提前取好当前条目信息（引擎借用期间不能再调 self 方法）。
        let is_cue = self.current_item_is_cue();
        let cue = self.current_cue();
        if let Some(engine) = &self.engine {
            if engine.is_playing() {
                // CUE 分轨不写断点：整轨位置会覆盖该文件普通播放的续播点。
                if !is_cue {
                    if let Some(path) = &self.current_path {
                        let pos = engine.position();
                        self.playlist_state.save_position(path, pos);
                    }
                }
                engine.send(audio::AudioCmd::Pause);
            } else {
                // 已播到结尾（EOF/单曲播完）：从头重播当前曲，而非无效 Resume。
                let end_secs = match cue.as_ref().and_then(|c| c.end_ms) {
                    Some(ms) => ms as f64 / 1000.0,
                    None => engine.duration(),
                };
                let at_end = end_secs > 0.0 && engine.position() >= end_secs - 0.1;
                if at_end {
                    let mut stale_gain = None;
                    if self.current_path.is_some() {
                        // 重播前先排空残留的结束/切曲事件，并取走滞留的测量增益。
                        stale_gain = engine.take_measured_gain_db();
                        while engine.poll_track_switched().is_some() {}
                        while engine.poll_finished().is_some() {}
                        while engine.poll_failed().is_some() {}
                        if let Some(path) = &self.current_path {
                            match cue.as_ref() {
                                Some(c) => engine.send(audio::AudioCmd::PlayResume {
                                    path: path.clone(),
                                    secs: c.start_ms as f64 / 1000.0,
                                }),
                                None => engine.send(audio::AudioCmd::Play(path.clone())),
                            }
                        }
                    }
                    if let (Some(db), Some(path)) = (stale_gain, self.current_path.clone()) {
                        self.replay_gain_cache.insert(path, db);
                    }
                    // 重播是切曲：守卫防滞后 position 误判 CUE 终点。
                    self.switch_guard = SWITCH_GUARD_FRAMES;
                } else {
                    engine.send(audio::AudioCmd::Resume);
                }
            }
        }
    }

    /// 音量 +5%（钳制到 1.0 满音量）。供硬编码 `+`/`=` 与自定义键复用。
    pub(crate) fn volume_up(&mut self) {
        if let Some(engine) = &self.engine {
            let v = (engine.volume() + 0.05).min(1.0);
            engine.send(audio::AudioCmd::SetVolume(v));
        }
    }

    /// 音量 -5%（钳制到 0.0 静音）。供硬编码 `-` 与自定义键复用。
    pub(crate) fn volume_down(&mut self) {
        if let Some(engine) = &self.engine {
            let v = (engine.volume() - 0.05).max(0.0);
            engine.send(audio::AudioCmd::SetVolume(v));
        }
    }
}
