//! 播放控制：播放/暂停、音量、seek（含 CUE 钳制）、切曲全流程、引擎轮询。
//!
//! 依赖 media（歌词/元数据），被 actions / keys 引用。

use crate::config::Config;
use crate::playlist;
use tuneux_corex as audio;

use super::App;

impl App {
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
    pub fn play_and_update_current(&mut self, index: usize, config: &mut Config) {
        // 切曲前：存旧位置
        if let Some(old_path) = &self.current_path {
            if let Some(engine) = &self.engine {
                let pos = engine.position();
                self.playlist_state.save_position(old_path, pos);
            }
        }

        // 只设置 current（不 push history）：history 由 next/prev/jump_to 维护，
        // 此处重复 push 会破坏 shuffle 模式下 prev 的正确性。
        if !self.playlist.set_current(index) {
            return;
        }
        self.playlist.set_selected(index);
        // 展开当前曲目所在专辑，保证 ByAlbum 视图下选中曲目可见。
        if let Some(album) = self
            .playlist
            .items()
            .get(index)
            .and_then(|it| it.album.clone())
        {
            self.playlist.expand_album(&album);
        }

        let Some(item) = self.playlist.items().get(index) else {
            return;
        };
        let path = item.path.clone();
        // CUE 引用提前克隆：避免与下方可变借用（get_or_extract_metadata）冲突
        let cue_override = item.cue.clone();
        self.current_metadata = Some(self.get_or_extract_metadata(&path));
        self.current_path = Some(path.clone());
        self.cover_cache = None;
        // 加载歌词（统一入口：.lrc 优先，内嵌兜底，见 load_lyrics_for）
        self.current_lyrics = self.load_lyrics_for(&path);

        if let Some(engine) = &self.engine {
            // CUE 分轨曲目：直接从 CUE 起点播放（整轨内片段，不参与断点续播）
            if let Some(cue) = cue_override.as_ref() {
                engine.send(audio::AudioCmd::PlayResume {
                    path: path.clone(),
                    secs: cue.start_ms as f64 / 1000.0,
                });
                return;
            }
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
            // ReplayGain：查缓存应用整曲增益（dB → 线性；首次播放无缓存 = 1.0）
            let rg_gain = self
                .replay_gain_cache
                .get(&path)
                .copied()
                .map(|db| 10f64.powf(db / 20.0) as f32)
                .unwrap_or(1.0);
            engine.set_replay_gain(rg_gain);
            // Gapless：预载下一曲（顺序播放预测；shuffle/单曲循环返回 None 不预载，
            // 由 EOF→draining→finished 流程兜底）
            let next_path = self.playlist.peek_next(config.repeat);
            engine.send(audio::AudioCmd::PreloadNext(next_path));
        }
    }

    /// Gapless 无缝切曲后的前端状态同步。
    ///
    /// 解码线程已无缝切换到预载曲目（Engine::poll_track_switched 事件触发），
    /// 这里**只更新 UI 状态**（当前曲目索引/元数据/进度基准），不重发 Play。
    pub fn advance_ui_on_gapless(&mut self, config: &mut Config) {
        let outcome = self.playlist.next(config.repeat);
        if let playlist::NavOutcome::Switch(idx) = outcome {
            if !self.playlist.set_current(idx) {
                return;
            }
            self.playlist.set_selected(idx);
            if let Some(item) = self.playlist.items().get(idx) {
                let path = item.path.clone();
                self.current_metadata = Some(self.get_or_extract_metadata(&path));
                // 加载歌词（统一入口，与 play_and_update_current 一致）
                self.current_lyrics = self.load_lyrics_for(&path);
                self.current_path = Some(path);
            }
            // 无缝切曲后，继续预载再下一曲
            if let Some(engine) = &self.engine {
                let next_path = self.playlist.peek_next(config.repeat);
                engine.send(audio::AudioCmd::PreloadNext(next_path));
            }
        }
    }

    /// 切到下一首（按 repeat + shuffle 策略）。
    pub fn advance_to_next_track(&mut self, config: &mut Config) {
        let outcome = self.playlist.next(config.repeat);
        self.handle_nav_outcome(outcome, config);
    }

    /// 切到上一首（按 repeat + shuffle 策略）。
    pub fn advance_to_prev_track(&mut self, config: &mut Config) {
        let outcome = self.playlist.prev(config.repeat);
        self.handle_nav_outcome(outcome, config);
    }

    /// 把 NavOutcome 翻译成实际的播放/暂停操作
    pub fn handle_nav_outcome(&mut self, outcome: playlist::NavOutcome, config: &mut Config) {
        match outcome {
            playlist::NavOutcome::Switch(idx) => {
                self.play_and_update_current(idx, config);
            }
            playlist::NavOutcome::Repeat => {
                if let Some(curr) = self.playlist.current_index() {
                    if let Some(item) = self.playlist.items().get(curr) {
                        let path = item.path.clone();
                        if let Some(engine) = &self.engine {
                            if let Some(cue) = &item.cue {
                                engine.send(audio::AudioCmd::PlayResume {
                                    path,
                                    secs: cue.start_ms as f64 / 1000.0,
                                });
                            } else {
                                engine.send(audio::AudioCmd::Play(path));
                            }
                        }
                    }
                }
            }
            playlist::NavOutcome::End => {}
        }
    }

    /// 浏览器 Enter：进入目录或播放文件（普通模式与搜索模式共用）。
    ///
    /// 搜索模式下定位到目标后自动退出搜索——用户搜索的目的就是快速
    /// 跳到某个文件/目录，找到后理应立即回到正常浏览。
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

    /// 若整轨文件旁存在同名 `.cue`，解析并展开为 CUE 曲目条目。
    ///
    /// 无 `.cue` / 解析失败时返回空（调用方回退为普通条目）。
    /// 展开后的条目：path 同整轨文件、track_number = CUE 曲目号、
    /// cue = 起点/终点毫秒 + 标题 + 表演者，播放时从对应 INDEX 位置起播、
    /// 到达终点自动切下一曲。
    /// 播放/暂停切换：正在播放则暂停（暂停前保存进度供断点续播），否则恢复。
    ///
    /// 从硬编码空格键分支提取，供默认键与自定义键复用同一逻辑。
    pub(crate) fn toggle_play(&mut self) {
        if let Some(engine) = &self.engine {
            if engine.is_playing() {
                if let Some(path) = &self.current_path {
                    let pos = engine.position();
                    self.playlist_state.save_position(path, pos);
                }
                engine.send(audio::AudioCmd::Pause);
            } else {
                engine.send(audio::AudioCmd::Resume);
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
