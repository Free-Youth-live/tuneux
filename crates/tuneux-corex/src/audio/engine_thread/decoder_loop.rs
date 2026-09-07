//! 解码线程：常驻解码循环 + 文件加载与状态机。
//!
//! 从 engine_thread.rs 拆分。解码线程独占 HeapProd（不可 Clone，必须固定线程），
//! 持 current/resampler/analyzer/preloaded 跨迭代状态；SharedState 以 Arc 共享。
//!
//! - try_load：Load / LoadAndSeek 共用（建解码器 + 重采样器）
//! - decoder_loop：主循环（解码 → 通道适配 → 重采样 → push_all → ReplayGain 分析）

use std::sync::atomic::{AtomicBool, Ordering};

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use ringbuf::{traits::*, HeapProd};

use crate::audio::decoder::{open_backend, DecoderBackend};
use crate::audio::engine::{DecoderCmd, SharedState};
use crate::audio::engine_thread::sample_utils::{adapt_channels, push_all};
use crate::audio::resample::Resample;

/// ReplayGain 目标响度（LUFS）：-14 为流媒体兼容标准（Spotify 等）。
const RG_TARGET_LUFS: f64 = -14.0;

/// ReplayGain 分析器用的有效采样率：优先取流的真实采样率（原生采样率直通
/// 会在重建流时改变它），未设置（0）时回退 spawn 捕获值。与 try_load 的
/// 重采样判定同口径，保证 K 加权滤波器/块长与推入样本的实际速率一致。
fn analyzer_rate(state: &Arc<SharedState>, device_sample_rate: u32) -> u32 {
    let s = state.stream_sample_rate();
    if s > 0 {
        s
    } else {
        device_sample_rate
    }
}

pub(super) fn try_load(
    path: &std::path::Path,
    current: &mut Option<Box<dyn DecoderBackend>>,
    resampler: &mut Option<Resample>,
    state: &Arc<SharedState>,
    failed_tx: &Sender<()>,
    device_sample_rate: u32,
    device_channels: usize,
) -> bool {
    match open_backend(path) {
        Ok(dec) => {
            // 取当前流的实际采样率（SharedState.stream_sample_rate）。
            // 此值由音频线程在流初始化/重建时写入——解码线程不能用
            // spawn 时捕获的 device_sample_rate 判断是否需要重采样，
            // 因为原生采样率直通功能会在换曲时重建流、改变采样率。
            // 初始值为 0（尚未设置）时用 spawn 捕获值兜底。
            let stream_sr = state.stream_sample_rate();
            let effective_device_sr = if stream_sr > 0 {
                stream_sr
            } else {
                device_sample_rate
            };
            let file_sample_rate = dec.params().sample_rate.unwrap_or(effective_device_sr);
            let file_channels = dec.params().channels.unwrap_or(device_channels as u16) as usize;
            // 若文件采样率 ≠ 设备采样率，必须建重采样器；建失败
            // 不能降级（直接播会变调变速），按"无法播放"处理。
            let need_resample = file_sample_rate != effective_device_sr;
            let (new_resampler, init_ok) = if need_resample {
                match Resample::new(
                    file_sample_rate,
                    effective_device_sr,
                    file_channels.min(device_channels),
                    1024,
                ) {
                    Ok(r) => (Some(r), true),
                    Err(_) => (None, false),
                }
            } else {
                (None, true)
            };
            if init_ok {
                *resampler = new_resampler;
                // 推时长到 SharedState，让 UI 在播放前就能显示时长。
                if let Some(dur) = dec.params().duration {
                    state.set_duration(dur);
                } else {
                    // 无时长元数据（如 Opus / FFmpeg 后端）：清零，避免残留上一曲时长。
                    state.set_duration(0.0);
                }
                *current = Some(dec);
                true
            } else {
                // 初始化失败：清空状态并通知主线程，与打开失败
                // 走同一条"无法播放"路径（finished 事件）
                *current = None;
                // 重采样器一并清空，维持"失败即清空"的状态不变量
                // （否则残留的上一曲 resampler 会在下次 Load 前被
                // decode 分支读到——虽然 current=None 使其不可达，
                // 但保持显式清理更不易出错）。
                *resampler = None;
                // 错误信息用实际生效的设备采样率（直通后可能与初始值不同），
                // 与上方 effective_device_sr 的取值口径一致。
                state.set_last_error(format!(
                    "重采样器初始化失败（{file_sample_rate}→{effective_device_sr} Hz）"
                ));
                state.signal_playback_finished();
                let _ = failed_tx.send(());
                false
            }
        }
        Err(e) => {
            *current = None;
            *resampler = None; // 同上：失败即清空
                               // 把错误暴露给 UI：让用户知道"刚才那首被跳过了"，
                               // 而不是默默下一首。path 用 file_name() 截短避免整路径刷屏。
            let fname = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            state.set_last_error(format!("打开失败 [{fname}]：{e}"));
            state.signal_playback_finished(); // 通知音频线程暂停
            let _ = failed_tx.send(()); // 通知主线程（无法播放）
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn decoder_loop(
    mut producer: HeapProd<f32>,
    dec_cmd_rx: Receiver<DecoderCmd>,
    state: Arc<SharedState>,
    finished_tx: Sender<()>,
    failed_tx: Sender<()>,
    track_switched_tx: Sender<()>,
    close: Arc<AtomicBool>,
    device_sample_rate: u32,
    device_channels: usize,
) {
    // 当前打开的解码器与重采样器。文件采样率/通道数仅在 Load 时使用，
    // 不需要跨命令保持（重采样器内部已封装这些信息）。
    let mut current: Option<Box<dyn DecoderBackend>> = None;
    let mut resampler: Option<Resample> = None;
    // 是否应继续解码当前文件
    let mut decoding: bool = false;
    // 是否处于"EOF 后排空等待"状态：解码已结束，但 ringbuf 里还有
    // 未播出的样本（容量约 2 秒、背压让它常态接近满），必须等回调
    // 消费完（occupied_len == 0）才上报"播放结束"。
    // 不能 EOF 就立即 signal_playback_finished / finished_tx——否则音频
    // 线程会 pause 流、回调停止消费，ringbuf 里那 ~2 秒尾巴永远播不出来，
    // 换曲时又被 consumer.clear() 直接丢弃（每首歌结尾被吞 2 秒）。
    let mut draining: bool = false;
    // Gapless 预载槽：EOF 时若就绪且采样率一致，无缝切换到该后端继续解码。
    // 手动 Play/Seek/Stop 会清空预载（避免陈旧预载误切换）。
    let mut preloaded: Option<Box<dyn DecoderBackend>> = None;
    // 当前文件的采样率（Load 时记录，用于无缝切换的采样率一致性判断）。
    let mut current_rate: Option<u32> = None;
    // ReplayGain 流式响度分析器：Load 时创建，feed 解码样本，曲目结束取增益。
    let mut analyzer: Option<crate::rg::LoudnessAnalyzer> = None;

    while !close.load(Ordering::Relaxed) {
        // —— 1. 处理命令（非阻塞）——
        match dec_cmd_rx.try_recv() {
            Ok(DecoderCmd::Preload(path)) => {
                // Gapless 预载：提前打开下一曲后端缓存（不启动解码）。
                // 手动切曲会先发 Load 清空预载；此处仅 open 后端。
                preloaded = match path {
                    // 预载失败静默：真正切到该曲时 try_load 会走"无法播放"通道报错。
                    Some(p) => open_backend(&p).ok(),
                    None => None,
                };
            }
            Ok(DecoderCmd::Load(path)) => {
                // 注意：不在此清空 ringbuf（producer 端无 clear 方法）。
                // 换曲时音频线程会 bump_flush，回调侧检测到世代变化后
                // 用 consumer.clear() 丢弃旧曲残留样本。
                // 直接赋值：try_load 返回 bool，if-else 冗余（clippy）
                decoding = try_load(
                    &path,
                    &mut current,
                    &mut resampler,
                    &state,
                    &failed_tx,
                    device_sample_rate,
                    device_channels,
                );
                // 新曲目：取消任何排空等待；手动切曲作废预载
                draining = false;
                preloaded = None;
                current_rate = current.as_ref().and_then(|b| b.params().sample_rate);
                // ReplayGain：新曲目重置流式分析器（按流采样率分析重采样后样本）。
                // 开关关闭时不建分析器，省去每样本累计开销。
                analyzer = if state.replay_gain_enabled() {
                    Some(crate::rg::LoudnessAnalyzer::new(
                        analyzer_rate(&state, device_sample_rate),
                        device_channels,
                    ))
                } else {
                    None
                };
            }
            Ok(DecoderCmd::LoadAndSeek { path, secs }) => {
                // 跟 Load 一样打开文件，加载完立刻 seek 到目标位置
                if try_load(
                    &path,
                    &mut current,
                    &mut resampler,
                    &state,
                    &failed_tx,
                    device_sample_rate,
                    device_channels,
                ) {
                    decoding = true;
                    draining = false;
                    preloaded = None;
                    current_rate = current.as_ref().and_then(|b| b.params().sample_rate);
                    analyzer = if state.replay_gain_enabled() {
                        Some(crate::rg::LoudnessAnalyzer::new(
                            analyzer_rate(&state, device_sample_rate),
                            device_channels,
                        ))
                    } else {
                        None
                    };
                    // 立刻 seek——此时文件已加载、current 已就绪，seek 不会丢失
                    if let Some(dec) = current.as_mut() {
                        // 续播 seek 失败则从头播（低频；无需打扰 UI）。
                        let _ = dec.seek(secs);
                    }
                } else {
                    decoding = false;
                    draining = false;
                }
            }
            Ok(DecoderCmd::Seek(secs)) => {
                if current.is_some() {
                    if let Some(dec) = current.as_mut() {
                        let _ = dec.seek(secs);
                        // seek 后旧缓冲由回调侧 flush 处理（音频线程已 bump_flush）
                    }
                    // EOF 后排空状态下 seek：解码器 seek 到新位置后能继续
                    // 产出样本，恢复解码并取消排空等待——修"播完后进度条
                    // 拖不动、Resume 只出静音"的问题。
                    decoding = true;
                    draining = false;
                }
            }
            Ok(DecoderCmd::Stop) => {
                decoding = false;
                draining = false;
                current = None;
                // 旧缓冲由回调侧 flush 处理
            }
            Ok(DecoderCmd::NewProducer(new_prod)) => {
                // 原生采样率直通：音频线程重建了 ringbuf，换上新 producer。
                // 旧 producer 赋值时自动 drop（及其内部 Arc 引用）。
                producer = new_prod;
                // 重采样器作废：新流的采样率已和（新）目标对齐，是否重采样
                // 由随后的 Load 命令根据"文件采样率 vs 新 device_sample_rate"
                // 重新决定。这里清掉避免沿用上一曲的错误配置。
                resampler = None;
                // current 也作废：上一曲的 decoder 指向旧流，新的 Load 会重建
                current = None;
                decoding = false;
                draining = false;
            }
            Ok(DecoderCmd::Exit) => break,
            Err(_) => {} // 无命令，继续
        }

        // —— 2. 解码一批数据 ——
        // 暂停时（is_playing == false）不解码：回调停跑 → ringbuf 满 →
        // push_all 会一直重试到上限后丢包（暂停 1 秒就丢一个 packet，
        // 长暂停还会一路丢到 EOF 误触发切歌）。挂起解码线程，只处理命令，
        // Resume 后从原位置继续，零丢失。
        if decoding && state.is_playing() {
            if let Some(dec) = current.as_mut() {
                match dec.decode_next() {
                    Ok(Some(samples)) => {
                        // 通道适配：文件通道 ≠ 设备通道时转换。
                        // 通道数从 decoder 参数实时读取，无需跨循环保持状态。
                        let file_channels =
                            dec.params().channels.unwrap_or(device_channels as u16) as usize;
                        let adapted = adapt_channels(&samples, file_channels, device_channels);
                        // 重采样（若需要）
                        let final_samples = if let Some(rs) = resampler.as_mut() {
                            rs.process(&adapted).unwrap_or_default()
                        } else {
                            adapted
                        };
                        // 推入 ringbuf（满了阻塞等待）
                        push_all(&mut producer, &final_samples, device_channels);
                        // ReplayGain：喂入流式分析器（重采样后的设备率样本）
                        if let Some(a) = analyzer.as_mut() {
                            a.feed(&final_samples);
                        }
                    }
                    Ok(None) => {
                        // EOF：先把重采样器内部残留推完（flush 抽出不足一个
                        // chunk 的缓冲 + 滤波器延迟，否则这 20-30ms 也丢），
                        // 然后进入"排空等待"，等 ringbuf 播完再上报结束。
                        // 不能在这里立即 signal_playback_finished / finished_tx
                        // ——见 `draining` 字段注释（结尾 2 秒被吞）。
                        if let Some(rs) = resampler.as_mut() {
                            // flush 失败：20-30ms 尾部静默丢失，不打断结束流程。
                            // （运行期 eprintln 会弄脏 raw-mode 屏幕，故静默。）
                            if let Ok(tail) = rs.flush() {
                                if !tail.is_empty() {
                                    push_all(&mut producer, &tail, device_channels);
                                }
                            }
                        }
                        // Gapless 无缝切换：预载就绪且采样率与声道数与当前文件一致时，
                        // 直接把 current 换成预载后端继续解码（ringbuf 不断流，
                        // 音频回调无空拍）；通知前端"曲目已无缝切换"（只更新 UI，
                        // 不重发 Play）。否则回退 draining 流程。
                        let can_switch = preloaded.is_some()
                            && preloaded.as_ref().and_then(|b| b.params().sample_rate)
                                == current_rate
                            && preloaded.as_ref().and_then(|b| b.params().channels)
                                == current.as_ref().and_then(|c| c.params().channels);
                        if can_switch {
                            if let Some(next) = preloaded.take() {
                                // ReplayGain：无缝切换不触发 draining/finished，
                                // 这里显式保存旧曲分析结果并重置新曲分析器
                                //（App 在 track_switched 时读取缓存）。
                                if let Some(a) = analyzer.take() {
                                    let g = a.gain_db(RG_TARGET_LUFS);
                                    state.set_measured_gain_db(g);
                                }
                                // 同步更新时长：gapless 绕过 try_load，不补这里 UI 会一直显示
                                // 上一曲总时长（状态栏失真，且 toggle_play 的 at_end 判定错位）。
                                if let Some(dur) = next.params().duration {
                                    state.set_duration(dur);
                                } else {
                                    state.set_duration(0.0);
                                }
                                current = Some(next);
                                analyzer = if state.replay_gain_enabled() {
                                    Some(crate::rg::LoudnessAnalyzer::new(
                                        analyzer_rate(&state, device_sample_rate),
                                        device_channels,
                                    ))
                                } else {
                                    None
                                };
                                // 同采样率同声道：resampler 配置不变，可直接复用；
                                // 重置解码状态继续推数据。
                                decoding = true;
                                draining = false;
                                // 不 flush：保留 ringbuf 里旧曲尾巴（约 2 秒）自然播完，
                                // 实现真正无缝。进度基准设为负的尾巴时长——旧曲尾巴
                                // 播完前 position ≤ 0（UI 侧钳制显示为 0），播完后新曲从 0 起。
                                let tail_frames = producer.occupied_len() / device_channels.max(1);
                                // 尾巴样本按流的真实采样率换算：原生直通会重建流、改变采样率，
                                // 不能用 spawn 捕获的 device_sample_rate，否则时长会偏约 8%。
                                let tail_secs = tail_frames as f64
                                    / analyzer_rate(&state, device_sample_rate).max(1) as f64;
                                state.reset_position(-tail_secs);
                                let _ = track_switched_tx.send(());
                            } else {
                                decoding = false;
                                draining = true;
                            }
                        } else {
                            decoding = false;
                            draining = true;
                        }
                    }
                    Err(e) => {
                        decoding = false;
                        draining = false;
                        // 把错误暴露给 UI——用户能看到"刚才那首解码挂了"，
                        // 而不是默默切下一首
                        state.set_last_error(format!("解码错误：{e}"));
                        state.signal_playback_finished(); // 通知音频线程暂停
                        let _ = failed_tx.send(());
                    }
                }
            }
        } else if draining {
            // —— 排空等待：EOF 后 ringbuf 里是最后一曲的尾巴，回调正在
            //    消费。等它归零（真正播完）再上报"播放结束"，避免提前
            //    pause + 换曲 clear 把 ~2 秒结尾吞掉。——
            // 注：暂停期间（is_playing == false）回调停跑、occupied_len 冻结
            // 在 >0，finished 不会触发——这是设计行为：用户暂停时保留尾巴、
            // 不自动切歌；Resume 后尾巴播完仍会正常上报结束。用户也可用
            // Next（Play→Load）/ Seek / Stop 任一退出排空态。
            if producer.occupied_len() == 0 {
                draining = false;
                state.signal_playback_finished(); // 通知音频线程暂停
                let _ = finished_tx.send(());
                // ReplayGain：曲目分析完成，把整曲增益（目标 -14 LUFS）存共享状态，
                // App 在播放结束时读取并缓存（下次播放该曲生效）。
                if let Some(a) = analyzer.take() {
                    let g = a.gain_db(RG_TARGET_LUFS);
                    state.set_measured_gain_db(g);
                }
            } else {
                // 还有残留，睡一小会再查（避免忙等）
                std::thread::sleep(Duration::from_millis(2));
            }
        } else {
            // 空闲时短暂休眠，避免忙等
            std::thread::sleep(Duration::from_millis(10));
        }

        // —— 3. ringbuf 接近满时，稍微让一让（背压）——
        // capacity 返回 NonZeroUsize，用 .get() 取 usize；occupied_len 是已占用长度
        if producer.occupied_len() + 1024 >= producer.capacity().get() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::audio::engine::SharedState;

    enum FakeOutcome {
        /// 正常产出一批交错样本（push 进 ringbuf）。
        Samples(Vec<f32>),
        /// EOF：decode_next 返回 Ok(None)。
        Eof,
        /// 解码错误（致命路径，区别于 EOF：立即上报）。
        Err,
    }

    /// 单 tick 后解码线程应做的动作（测试观察点）。
    #[derive(Debug, PartialEq, Eq)]
    enum TickAction {
        /// 解码分支被执行了一次（说明 is_playing 门控放行）。
        Decoded,
        /// 命中 draining 分支但 ringbuf 还有数据 → 等待（不上报）。
        DrainingWait,
        /// draining 完成：occupied == 0 → signal_playback_finished + 通知主线程。
        DrainFinished,
        /// 完全空闲：解码/draining 都不进行（暂停门控生效）。
        Idle,
    }

    /// decoder_loop 状态机的纯函数对照（测试模块私有）。
    ///
    /// 字段含义严格对齐生产代码 `decoder_loop` 中的同名变量：
    /// - `decoding`：是否应继续解码当前文件
    /// - `draining`：是否处于 EOF 后排空等待
    /// - `occupied`：ringbuf 已占用样本数（draining 完成条件）
    /// - `decoded_calls`：累计"调过 decode_next"的次数（验证 is_playing 门控）
    /// - `signal_finished_calls`：累计 signal_playback_finished 调用次数
    struct DecoderSim {
        decoding: bool,
        draining: bool,
        occupied: usize,
        decoded_calls: u32,
        signal_finished_calls: u32,
    }

    impl DecoderSim {
        fn new() -> Self {
            Self {
                decoding: false,
                draining: false,
                occupied: 0,
                decoded_calls: 0,
                signal_finished_calls: 0,
            }
        }

        /// 单 tick 判定：严格对照 decoder_loop 的规则。
        ///
        /// 对照规则（与 `decoder_loop` 同步）：
        /// 1. `if decoding && state.is_playing()` → 调 decode_next：
        ///    - Ok(Some(s))：累加 occupied（生产代码会 adapt/resample/push）
        ///    - Ok(None)：decoding=false, draining=true（不立即上报）
        ///    - Err：decoding=false, draining=false, signal_playback_finished
        /// 2. `else if draining` → occupied==0 时上报结束并退出 draining
        /// 3. else → Idle（生产代码 sleep；测试不 sleep）
        ///
        /// 注：本对照**不**真调 push_all / flush，只关注状态转移。
        /// `push_all` 的帧对齐 / 重试上限有独立单测覆盖。
        fn step(&mut self, state: &SharedState, outcome: FakeOutcome) -> TickAction {
            if self.decoding && state.is_playing() {
                self.decoded_calls += 1;
                match outcome {
                    FakeOutcome::Samples(samples) => {
                        // 生产代码这里会 adapt_channels / resample / push_all；
                        // 测试只关心 occupied 累计（用样本数表示）。
                        self.occupied += samples.len();
                        TickAction::Decoded
                    }
                    FakeOutcome::Eof => {
                        // 关键：进入 draining，**不**立即 signal_playback_finished。
                        // 生产代码此处还会 resampler.flush() + push_all(tail)。
                        self.decoding = false;
                        self.draining = true;
                        TickAction::DrainingWait
                    }
                    FakeOutcome::Err => {
                        // 致命错误：与 EOF 不同，立即上报（UI 应感知到错误）
                        self.decoding = false;
                        self.draining = false;
                        self.occupied = 0;
                        state.signal_playback_finished();
                        self.signal_finished_calls += 1;
                        TickAction::DrainFinished
                    }
                }
            } else if self.draining {
                if self.occupied == 0 {
                    self.draining = false;
                    state.signal_playback_finished();
                    self.signal_finished_calls += 1;
                    TickAction::DrainFinished
                } else {
                    TickAction::DrainingWait
                }
            } else {
                TickAction::Idle
            }
        }

        /// 模拟音频回调消费样本（draining 测试用：每次 pop 一些 occupied）。
        fn consume(&mut self, n: usize) {
            self.occupied = self.occupied.saturating_sub(n);
        }
    }

    // -----------------------------------------------------------------
    // 暂停时挂起解码
    // -----------------------------------------------------------------

    /// Pause 后下一 tick 不应进入解码分支 —— 即 is_playing=false 门控生效。
    /// 验证：decoded_calls == 0；occupied 保持初始值（0）。
    #[test]
    fn pause_gates_decoding() {
        let state = SharedState::default();
        let mut sim = DecoderSim::new();

        // 模拟 Load 成功 + Play：decoding=true，is_playing=true
        sim.decoding = true;
        state.set_playing(true);

        // 主线程发 Pause → is_playing=false
        state.set_playing(false);

        // 跑一个 tick：解码器有样本，但 is_playing=false → 应走 Idle
        let action = sim.step(&state, FakeOutcome::Samples(vec![0.1, 0.2, 0.3, 0.4]));
        assert_eq!(action, TickAction::Idle, "暂停时应 Idle，不调 decode_next");
        assert_eq!(sim.decoded_calls, 0, "暂停期间 decode_next 调用次数应为 0");
        assert_eq!(sim.occupied, 0, "暂停期间不应推数据进 ringbuf");
    }

    /// 致命解码错误：与 EOF 不同，立即上报（不等排空）——UI 应感知到错误。
    #[test]
    fn fatal_error_signals_immediately() {
        let state = SharedState::default();
        let mut sim = DecoderSim::new();
        sim.decoding = true;
        state.set_playing(true);

        let a = sim.step(&state, FakeOutcome::Err);
        assert_eq!(
            a,
            TickAction::DrainFinished,
            "致命错误路径返回 DrainFinished"
        );
        assert!(state.take_playback_finished(), "致命错误立即上报播放结束");
        assert_eq!(sim.signal_finished_calls, 1);
        assert!(!sim.draining, "致命错误不进入 draining");
        assert_eq!(sim.occupied, 0, "致命错误清空缓冲");
    }

    /// Pause → Resume 后解码恢复：从暂停点继续推数据，无丢帧（状态机层面）。
    #[test]
    fn resume_continues_decoding() {
        let state = SharedState::default();
        let mut sim = DecoderSim::new();

        sim.decoding = true;
        state.set_playing(true);

        // Pause：is_playing = false
        state.set_playing(false);

        // 暂停期：3 个 tick 都不应解码（is_playing 门控生效）
        for _ in 0..3 {
            let a = sim.step(&state, FakeOutcome::Samples(vec![0.0; 1024]));
            assert_eq!(a, TickAction::Idle);
        }
        assert_eq!(sim.decoded_calls, 0);

        // Resume
        state.set_playing(true);

        // 恢复后 1 tick：应正常解码并累加 occupied
        let a = sim.step(&state, FakeOutcome::Samples(vec![0.0; 512]));
        assert_eq!(a, TickAction::Decoded);
        assert_eq!(sim.decoded_calls, 1);
        assert_eq!(sim.occupied, 512);
    }

    // -----------------------------------------------------------------
    // EOF 后排空（不立即上报）
    // -----------------------------------------------------------------

    /// 正常 EOF 路径：解码器返回 None 后 decoding→false, draining→true；
    /// **关键**：signal_playback_finished **未**触发（否则 2 秒尾巴被吞）。
    #[test]
    fn eof_activates_draining_without_signal() {
        let state = SharedState::default();
        let mut sim = DecoderSim::new();

        sim.decoding = true;
        sim.occupied = 4096; // 假设 ringbuf 里还有 4k 样本
        state.set_playing(true);

        // 这一 tick 假解码器返回 EOF
        let action = sim.step(&state, FakeOutcome::Eof);
        assert_eq!(action, TickAction::DrainingWait);
        assert!(!sim.decoding, "EOF 后 decoding 应为 false");
        assert!(sim.draining, "EOF 后 draining 应为 true");
        assert_eq!(sim.occupied, 4096);
        // 关键不变量：EOF 后**未**上报
        assert!(
            !state.take_playback_finished(),
            "关键：EOF 后**不能**立即 signal_playback_finished，否则尾巴被吞"
        );
        assert_eq!(sim.signal_finished_calls, 0);
    }

    /// occupied 归零时才上报完成，draining 退出。
    /// 路径：先 EOF 进入 draining → 回调分批消费 → 最后一 tick occupied==0 → 上报。
    #[test]
    fn drain_completes_when_buffer_empty() {
        let state = SharedState::default();
        let mut sim = DecoderSim::new();

        sim.decoding = true;
        sim.occupied = 2000;
        state.set_playing(true);

        // EOF tick
        let a = sim.step(&state, FakeOutcome::Eof);
        assert_eq!(a, TickAction::DrainingWait);
        assert!(!state.take_playback_finished(), "occupied>0 时不应上报");

        // 回调分批消费（模拟音频回调 pop）
        sim.consume(800);
        let a = sim.step(&state, FakeOutcome::Samples(vec![]));
        assert_eq!(
            a,
            TickAction::DrainingWait,
            "occupied=1200>0 仍 DrainingWait"
        );
        assert!(!state.take_playback_finished(), "occupied>0 仍不应上报");

        sim.consume(1200);
        assert_eq!(sim.occupied, 0);
        let a = sim.step(&state, FakeOutcome::Samples(vec![]));
        assert_eq!(a, TickAction::DrainFinished, "occupied=0 时上报");
        assert!(!sim.draining, "上报后 draining 退出");
        assert!(
            state.take_playback_finished(),
            "occupied 归零后应 signal_playback_finished"
        );
        assert_eq!(sim.signal_finished_calls, 1);
    }

    /// 关键 UX：EOF 进入 draining 后**用户暂停**——occupied 冻结，draining
    /// 不上报（设计行为：保留尾巴等待 Resume，避免用户被动切歌）。Resume
    /// 后回调继续消费、归零再上报。联合路径。
    #[test]
    fn drain_paused_holds_until_resume() {
        let state = SharedState::default();
        let mut sim = DecoderSim::new();

        sim.decoding = true;
        sim.occupied = 1000;
        state.set_playing(true);

        // EOF → draining
        let _ = sim.step(&state, FakeOutcome::Eof);
        assert!(sim.draining);
        assert!(!state.take_playback_finished());

        // 用户 Pause：is_playing=false
        state.set_playing(false);

        // 暂停期间回调停跑，occupied 冻结在 >0：draining 分支仍执行
        // （生产代码 draining 不门控 is_playing），但 occupied>0 → DrainingWait，
        // finished 不触发（设计行为：暂停保留尾巴，不自动切歌）。
        for _ in 0..5 {
            let a = sim.step(&state, FakeOutcome::Samples(vec![0.0; 10]));
            assert_eq!(
                a,
                TickAction::DrainingWait,
                "暂停时 occupied 冻结 → draining 分支返回 DrainingWait（非 Idle）"
            );
        }
        assert_eq!(sim.occupied, 1000, "暂停期间 occupied 不变");
        assert!(sim.draining, "draining 状态保留");
        assert!(
            !state.take_playback_finished(),
            "联合：暂停期间不应上报（保留尾巴）"
        );
        assert_eq!(sim.signal_finished_calls, 0);

        // 用户 Resume：回调重新开始消费
        state.set_playing(true);
        sim.consume(1000); // 一波消费完
        assert_eq!(sim.occupied, 0);

        // Resume 后下一 tick draining 分支判定 occupied==0 → 上报
        let a = sim.step(&state, FakeOutcome::Samples(vec![]));
        assert_eq!(a, TickAction::DrainFinished);
        assert!(!sim.draining);
        assert!(state.take_playback_finished());
        assert_eq!(sim.signal_finished_calls, 1);
    }
}
