//! 播放控制：播放/暂停、音量、seek、切曲全流程、引擎错误轮询。
//!
//! 与 tuneux 同一引擎命令模型（[`audio::AudioCmd`]）。已接入 ReplayGain
//! （查缓存应用整曲增益；引擎测量结果由主循环回读缓存）与 CUE 分轨。

use crate::config::Config;
use crate::playlist;
use tuneux_corex as audio;

use super::App;

/// 切曲生效守卫帧数：Play/PlayResume/Seek 经通道异步生效，期间 `position()`
/// 仍是旧值。守卫期内主循环不做 CUE 终点判定（防点选更早分轨被误判连跳）。
/// 播放中帧率约 30fps，15 帧 ≈ 0.5s，足够解码线程完成重开/seek。
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

    /// 拉取 engine 的最新错误，并清理过期的错误显示（主循环每帧调用）。
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

    /// 切曲前清理：取走引擎滞留的测量增益（旧曲结果，不能滞留被新曲错领），
    /// 并排空残留的切曲 / 结束 / 失败事件（防旧事件触发额外自动切曲）。
    /// 返回旧曲滞留的测量增益（dB）；调用方负责归档到旧曲路径。
    fn drain_residual_events(&self) -> Option<f64> {
        let engine = self.engine.as_ref()?;
        let gain = engine.take_measured_gain_db();
        while engine.poll_track_switched().is_some() {}
        while engine.poll_finished().is_some() {}
        while engine.poll_failed().is_some() {}
        gain
    }

    /// 切到列表指定项，触发播放 + 更新元数据（断点续播）。
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
        // 只设置 current（不 push history）：history 由 next/prev/jump_to 维护。
        if !self.playlist.set_current(index) {
            return;
        }
        self.playlist.set_selected(index);

        let Some(item) = self.playlist.items().get(index) else {
            return;
        };
        // 先克隆出需要的字段，断开对 items() 的不可变借用，随后才能可变操作。
        let album_opt = item.album.clone();
        let path = item.path.clone();
        // CUE 引用提前克隆：避免与下方可变借用冲突。
        let cue_override = item.cue.clone();
        // 书签跳转 CUE 分轨时的分轨内偏移（秒，用后即清）；无则 0（分轨头起播）。
        let cue_offset = self.pending_cue_offset.take().unwrap_or(0.0);
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
        // 元数据缓存不带封面字节（省内存）；命中缓存切回时封面缺失，
        // 为当前曲目补提取一次，保证封面面板正常显示。已确认无封面的
        // 文件记负标记，不再重复整文件标签探测。
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
        // 切曲：清空频谱峰值白帽，避免上一曲高峰压在新曲前奏上
        self.spectrum_peaks.borrow_mut().reset();
        if same_file {
            // 同一整轨文件的分轨切换：path 未变，封面/歌词复用，
            // 避免每个分轨边界多一次封面解码 + 磁盘读。
        } else {
            self.cover_cache = None;
            // 加载歌词（统一入口：.lrc 优先，内嵌兜底）。
            self.current_lyrics = self.load_lyrics_for(&path);
        }

        // 切曲前清理：取走旧曲滞留的测量增益并排空残留事件，稍后归档到旧曲路径。
        let stale_gain = self.drain_residual_events();
        if let Some(engine) = &self.engine {
            if let Some(cue) = cue_override.as_ref() {
                // CUE 分轨：从 CUE 起点起播（整轨内片段，不参与断点续播）；
                // 书签跳转时叠加分轨内偏移（cue_offset），普通切分轨为 0。
                let start_secs = cue.start_ms as f64 / 1000.0 + cue_offset;
                if same_file && engine.is_playing() {
                    // 同一整轨文件正在播：仅文件内 Seek——不重开文件（免卡顿）、
                    // 不重置响度分析器（测量跨分轨连续，不被切分清零）。
                    engine.send(audio::AudioCmd::Seek(start_secs));
                } else {
                    // 跨文件，或引擎不在播放态（暂停/播完自动暂停）：Seek 只
                    // 移动位置，既不出声也不驱动解码，必须重开文件才能起播。
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
            // 预载下一曲（顺序播放的无缝衔接准备）。
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
        // 旧曲滞留的测量增益归档（下次播放旧曲时直接复用，免重测）。
        if let (Some(db), Some(old)) = (stale_gain, prev_path) {
            self.replay_gain_cache.insert(old, db);
        }
        // 切曲生效守卫：Play/Seek 异步生效，若干帧内 position 仍是旧值，
        // 守卫期内主循环不做 CUE 终点判定。
        self.switch_guard = SWITCH_GUARD_FRAMES;
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

    /// 把 [`playlist::NavOutcome`] 翻译成实际的播放操作。
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
                    // 清残留事件并取走滞留的测量增益（稍后归档到本曲）。
                    let stale_gain = self.drain_residual_events();
                    if let Some(engine) = &self.engine {
                        if let Some(c) = cue {
                            if engine.is_playing() {
                                // 播放中的 CUE 单曲循环：回片段起点走文件内 Seek
                                //——不重开文件（免周期性卡顿），响度分析器连续
                                //（测量不被每个循环清零重测）。
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

    /// Gapless 无缝切曲后的前端状态同步。
    ///
    /// 解码线程已无缝切换到预载曲目（`Engine::poll_track_switched` 事件触发）。
    /// 这里**只更新 UI 状态**（当前索引/元数据/进度基准），绝不重发 Play，
    /// 以免打断无缝衔接。随后按新 current 补发预载，维持无缝链。
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
            // CUE 分轨：标题/表演者优先取分轨信息，与列表及状态栏口径一致
            //（系统媒体面板也显示分轨名而非整轨专辑名）。
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
                // 命中缓存切回时封面缺失则补提取；已确认无封面的文件有
                // 负标记，不再重复整文件标签探测。
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
        // 续发预载再下一曲（顺序播放预测；单曲/随机返回 None 自动取消）。
        if let Some(engine) = &self.engine {
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
    }

    /// 播放/暂停切换：正在播放则暂停（暂停前保存进度），否则恢复。
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
                // CUE 分轨的"结尾"是分轨 end_ms（非整轨时长），重播从分轨起点起。
                let end_secs = match cue.as_ref().and_then(|c| c.end_ms) {
                    Some(ms) => ms as f64 / 1000.0,
                    None => engine.duration(),
                };
                let at_end = end_secs > 0.0 && engine.position() >= end_secs - 0.1;
                if at_end {
                    // 重播前先排空残留事件、取走滞留增益（稍后归档到本曲）。
                    let stale_gain = self.drain_residual_events();
                    if self.current_path.is_some() {
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

    /// 重新下发预载（切换循环/随机模式后调用，保证预载目标与新策略一致）。
    ///
    /// 顺序模式预载显示序下一曲；单曲循环/随机时 `Playlist::peek_next` 返回
    /// `None`，等效于取消残留预载。
    pub(crate) fn refresh_preload(&mut self, config: &Config) {
        if let Some(engine) = &self.engine {
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
    }

    /// 相对当前位置 seek（秒）；钳制在 [0, 时长] 内，CUE 分轨再钳到本曲区间。
    pub(crate) fn seek_by(&mut self, delta: f64) {
        let (cur_pos, dur) = match &self.engine {
            Some(e) => (e.position(), e.duration()),
            None => return,
        };
        let mut new_pos = cur_pos + delta;
        if new_pos < 0.0 {
            new_pos = 0.0;
        }
        if dur > 0.0 && new_pos > dur {
            new_pos = dur;
        }
        // CUE 分轨：钳制在本曲区间 [start, end] 内。
        let new_pos = self.clamp_cue_seek(new_pos);
        if let Some(engine) = &self.engine {
            engine.send(audio::AudioCmd::Seek(new_pos));
        }
    }

    /// CUE 分轨曲目：把 seek 目标钳制在本曲区间 [start, end] 内，
    /// 防止快进/快退越界到整轨的其他曲目。
    /// 普通曲目（cue 为 None）原样返回；end_ms 为 None（末曲）只钳下限。
    pub(crate) fn clamp_cue_seek(&self, pos: f64) -> f64 {
        let cue = self
            .playlist
            .current_index()
            .and_then(|i| self.playlist.items().get(i))
            .and_then(|it| it.cue.as_ref());
        playlist::clamp_seek_to_cue(pos, cue)
    }

    /// 音量 +5%（钳制到 1.0 满音量）。
    pub(crate) fn volume_up(&mut self) {
        if let Some(engine) = &self.engine {
            let v = (engine.volume() + 0.05).min(1.0);
            engine.send(audio::AudioCmd::SetVolume(v));
        }
    }

    /// 音量 -5%（钳制到 0.0 静音）。
    pub(crate) fn volume_down(&mut self) {
        if let Some(engine) = &self.engine {
            let v = (engine.volume() - 0.05).max(0.0);
            engine.send(audio::AudioCmd::SetVolume(v));
        }
    }
}
