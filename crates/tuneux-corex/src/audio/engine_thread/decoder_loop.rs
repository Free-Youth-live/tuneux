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

/// Gapless 预载槽位元素：解码后端 + 可选区间（start_secs, end_secs；
/// 整文件预载为 None）。
type PreloadedSlot = (Box<dyn DecoderBackend>, Option<(f64, Option<f64>)>);

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
            // 文件通道数不在此处读取：通道适配与重采样通道数一律以设备通道数为准
            //（解码循环里 adapt_channels 恒输出 device_channels 路交错，单声道已上混）。
            // 若文件采样率 ≠ 设备采样率，必须建重采样器；建失败
            // 不能降级（直接播会变调变速），按"无法播放"处理。
            let need_resample = file_sample_rate != effective_device_sr;
            let (new_resampler, init_ok) = if need_resample {
                // 重采样器通道数必须与送入数据的实际通道数一致：解码循环里
                // adapt_channels 恒输出 device_channels 路交错（单声道已上混），
                // 若按文件通道数建重采样器，滤波窗口会跨越 L/R 交替样本造成串扰。
                match Resample::new(file_sample_rate, effective_device_sr, device_channels, 1024) {
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
    let mut preloaded: Option<PreloadedSlot> = None;
    // 当前文件的采样率（Load 时记录，用于无缝切换的采样率一致性判断）。
    let mut current_rate: Option<u32> = None;
    // ReplayGain 流式响度分析器：Load 时创建，feed 解码样本，曲目结束取增益。
    let mut analyzer: Option<crate::rg::LoudnessAnalyzer> = None;
    // PlayRange 区间状态（LoadRange 设置；Load / LoadAndSeek / Stop 清空）：
    // 终点按「重采样前、文件采样率」在源文件时间轴累计——cue 的 INDEX 时间
    // 是源率口径，若在重采样后按设备率累计会偏差约 8.8%（44.1k→48k 时）。
    let mut range_end: Option<f64> = None;
    // 区间起点（秒；= seek 后的 start_secs，不从 0 起算）。
    let mut range_base: f64 = 0.0;
    // 已解码的文件帧数（重采样前、文件通道计）。
    let mut range_decoded_frames: u64 = 0;
    // 已推入 ringbuf 的设备帧数（重采样后）：finished 路径的 flush 尾巴
    // 按区间设备帧长截断，防 position 越过区间终点。
    let mut range_pushed_frames: u64 = 0;

    while !close.load(Ordering::Relaxed) {
        // —— 1. 处理命令（非阻塞）——
        match dec_cmd_rx.try_recv() {
            Ok(DecoderCmd::Preload(target)) => {
                // Gapless 预载：提前打开下一曲后端缓存（不启动解码）。
                // 手动切曲会先发 Load 清空预载；此处仅 open 后端。
                preloaded = match target {
                    // 预载失败静默：真正切到该曲时 try_load 会走"无法播放"通道报错。
                    Some(t) => {
                        let range = t.start_secs.map(|s| (s, t.end_secs));
                        match open_backend(&t.path) {
                            Ok(mut b) => {
                                // 区间预载：先把后端 seek 到起点——否则无缝切换
                                // 会从文件头播（同文件相邻区间 / cue 相邻曲目）；
                                // seek 失败同样弃预载（静默从文件头播是放错曲，
                                // 不如同切曲时走正常报错路径）。
                                let seeked = match t.start_secs {
                                    Some(s) => b.seek(s).is_ok(),
                                    None => true,
                                };
                                if seeked {
                                    Some((b, range))
                                } else {
                                    None
                                }
                            }
                            Err(_) => None,
                        }
                    }
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
                // 新曲目：取消任何排空等待；手动切曲作废预载与区间状态
                draining = false;
                preloaded = None;
                range_end = None;
                // 解码恢复：清除末尾态（加载失败路径已在 try_load 内置位）。
                if decoding {
                    state.clear_at_eof();
                }
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
                    range_end = None;
                    state.clear_at_eof();
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
            Ok(DecoderCmd::LoadRange {
                path,
                start_secs,
                end_secs,
            }) => {
                // 区间播放（PlayRange）：与 LoadAndSeek 同流程打开并 seek，
                // 另置区间状态与按区间长度的时长上报。
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
                    // seek 到区间起点；失败按「无法播放」上报（区间起点定位
                    // 不到却从头播是放错曲，与续播 seek 的宽容口径不同）。
                    let seek_ok = current
                        .as_mut()
                        .is_some_and(|dec| dec.seek(start_secs).is_ok());
                    if !seek_ok {
                        state.set_last_error(format!(
                            "无法定位区间起点 {start_secs:.1}s：{}",
                            path.display()
                        ));
                        state.signal_playback_finished();
                        let _ = failed_tx.send(());
                        decoding = false;
                        draining = false;
                        range_end = None;
                    } else {
                        state.clear_at_eof();
                        // 区间状态：文件轴起点 / 终点 / 计数清零；时长按区间长度
                        //（end_secs 缺省 = 播到文件末尾，时长 = 文件时长 - 起点）。
                        range_base = start_secs;
                        range_decoded_frames = 0;
                        range_pushed_frames = 0;
                        range_end = end_secs;
                        let file_dur = current.as_ref().and_then(|b| b.params().duration);
                        let len = match (end_secs, file_dur) {
                            (Some(e), _) => (e - start_secs).max(0.0),
                            (None, Some(d)) => (d - start_secs).max(0.0),
                            (None, None) => 0.0,
                        };
                        state.set_duration(len);
                    }
                } else {
                    decoding = false;
                    draining = false;
                }
            }
            Ok(DecoderCmd::Seek(secs)) => {
                if current.is_some() {
                    // seek 是否成功决定了后续"状态是否要跟随新位置"——失败时解码器
                    // 仍在旧位置，任何按新位置重置的基准都会错位（见下）。
                    let seek_ok = if let Some(dec) = current.as_mut() {
                        dec.seek(secs).is_ok()
                        // seek 后旧缓冲由回调侧 flush 处理（音频线程已 bump_flush）
                    } else {
                        false
                    };
                    // 重采样器状态重置：内部缓冲残留 pre-seek 样本（≤1023 帧）
                    // 与 FFT 滤波器历史会把 seek 点前约 20-30ms 旧位置音频混入
                    // 新位置输出（仅文件率 ≠ 设备率时存在重采样器）。
                    // 同参数重建，失败不可达，防御性忽略。无论 seek 成败都清：
                    // 清了只是丢一小段缓冲，残留反而会在下一曲混入旧样本。
                    if let Some(rs) = resampler.as_mut() {
                        let _ = rs.reset();
                    }
                    // ReplayGain 分析器作废：seek 跳过的中段不参与响度统计，
                    // 保留旧 analyzer 会让「曲首段 + seek 后段」的加权平均值被
                    // 缓存并跨曲次回灌。**作废之后本曲不再重建**（解码循环里没有
                    // 惰性重建，feed 处是 `if let Some(a)`），即 seek 过的这次播放
                    // 不再产出测量值，曲终 `analyzer.take()` 为 None 也就不写缓存
                    // ——这是刻意的保守取舍：宁可没有测量值，也不写入残缺值。
                    // seek 失败则位置未变，旧 analyzer 仍然有效，不作废。
                    if seek_ok {
                        analyzer = None;
                    }
                    // 区间内 seek：终点累计基准跟随新位置（否则倒退 seek 会让
                    // 区间提前结束 / 前进 seek 越界）。**仅在 seek 成功时**重置：
                    // 失败时解码器仍在旧位置，提前重置基准会让区间终点按错误的
                    // 起点累计，表现为 cue 分轨的切歌点漂移。
                    if seek_ok && range_end.is_some() {
                        range_base = secs;
                        range_decoded_frames = 0;
                        range_pushed_frames = 0;
                    }
                    // EOF 后排空状态下 seek：解码器 seek 到新位置后能继续
                    // 产出样本，恢复解码并取消排空等待——修"播完后进度条
                    // 拖不动、Resume 只出静音"的问题。
                    decoding = true;
                    draining = false;
                    // 解码恢复：EOF 后 seek 回来应能继续出声（清除末尾态）。
                    state.clear_at_eof();
                }
            }
            Ok(DecoderCmd::Stop) => {
                decoding = false;
                draining = false;
                current = None;
                range_end = None;
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
                // 先取一批并做 PlayRange 终点截断（终点在重采样前、按文件
                // 采样率累计）；截后为空或文件 EOF 都汇入 hit_end 统一处理。
                let mut hit_end = false;
                match dec.decode_next() {
                    Ok(Some(samples)) => {
                        // 通道适配：文件通道 ≠ 设备通道时转换。
                        // 通道数从 decoder 参数实时读取，无需跨循环保持状态。
                        let file_channels =
                            dec.params().channels.unwrap_or(device_channels as u16) as usize;
                        let file_rate = dec.params().sample_rate.unwrap_or(0);
                        let mut samples = samples;
                        // PlayRange 终点判定：按源文件时间轴累计（重采样前），
                        // 截断在区间帧界——终点落批内时只喂终点前部分。
                        if let (Some(end), true) = (range_end, file_rate > 0) {
                            let total = ((end - range_base).max(0.0) * f64::from(file_rate)) as u64;
                            let remaining = total.saturating_sub(range_decoded_frames);
                            let batch_frames = samples.len() / file_channels.max(1);
                            if remaining == 0 {
                                samples.clear();
                                hit_end = true;
                            } else if (batch_frames as u64) > remaining {
                                samples.truncate(remaining as usize * file_channels);
                                hit_end = true;
                            }
                            range_decoded_frames += (samples.len() / file_channels.max(1)) as u64;
                        }
                        let adapted = adapt_channels(&samples, file_channels, device_channels);
                        // 重采样（若需要）
                        let final_samples = if let Some(rs) = resampler.as_mut() {
                            rs.process(&adapted).unwrap_or_default()
                        } else {
                            adapted
                        };
                        range_pushed_frames +=
                            (final_samples.len() / device_channels.max(1)) as u64;
                        // 推入 ringbuf（满了阻塞等待）
                        push_all(&mut producer, &final_samples, device_channels);
                        // ReplayGain：喂入流式分析器（重采样后的设备率样本）
                        if let Some(a) = analyzer.as_mut() {
                            a.feed(&final_samples);
                        }
                    }
                    Ok(None) => {
                        hit_end = true;
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
                if hit_end {
                    // —— 流尽（文件 EOF 或区间终点）：先把重采样器内部残留推完
                    //（flush 抽出不足一个 chunk 的缓冲 + 滤波器延迟，否则这
                    // 20-30ms 也丢），再走无缝切换 / 排空流程。
                    // 不能在这里立即 signal_playback_finished / finished_tx
                    // ——见 `draining` 字段注释（结尾 2 秒被吞）。——
                    // Gapless 无缝切换：预载就绪且采样率与声道数与当前文件一致时，
                    // 直接把 current 换成预载后端继续解码（ringbuf 不断流，
                    // 音频回调无空拍）；通知前端"曲目已无缝切换"（只更新 UI，
                    // 不重发 Play）。否则回退 draining 流程。
                    let can_switch = preloaded.is_some()
                        && preloaded.as_ref().and_then(|(b, _)| b.params().sample_rate)
                            == current_rate
                        && preloaded.as_ref().and_then(|(b, _)| b.params().channels)
                            == current.as_ref().and_then(|c| c.params().channels);
                    if let Some(rs) = resampler.as_mut() {
                        // flush 失败：20-30ms 尾部静默丢失，不打断结束流程。
                        // （运行期 eprintln 会弄脏 raw-mode 屏幕，故静默。）
                        if let Ok(mut tail) = rs.flush() {
                            // PlayRange finished（非无缝切换）：尾巴按区间设备帧长
                            // 截断，防 position 越过区间终点（gapless 场景尾巴
                            // 交给切换吃掉，不截）。
                            if let (Some(end), false) = (range_end, can_switch) {
                                let rate = analyzer_rate(&state, device_sample_rate).max(1);
                                let range_frames =
                                    ((end - range_base).max(0.0) * f64::from(rate)) as u64;
                                let remaining = range_frames.saturating_sub(range_pushed_frames);
                                let keep = remaining as usize * device_channels;
                                if tail.len() > keep {
                                    tail.truncate(keep);
                                }
                            }
                            if !tail.is_empty() {
                                range_pushed_frames += (tail.len() / device_channels.max(1)) as u64;
                                push_all(&mut producer, &tail, device_channels);
                            }
                        }
                    }
                    if can_switch {
                        if let Some((next, next_range)) = preloaded.take() {
                            // ReplayGain：无缝切换不触发 draining/finished，
                            // 这里显式保存旧曲分析结果并重置新曲分析器
                            //（App 在 track_switched 时读取缓存）。
                            if let Some(a) = analyzer.take() {
                                let g = a.gain_db(RG_TARGET_LUFS);
                                state.set_measured_gain_db(g);
                            }
                            // 同步更新时长：区间预载按区间长度，整文件预载按文件
                            // 时长——不补这里 UI 会一直显示上一曲时长（状态栏失真）。
                            match next_range {
                                Some((start, end)) => {
                                    let file_dur = next.params().duration;
                                    let len = match (end, file_dur) {
                                        (Some(e), _) => (e - start).max(0.0),
                                        (None, Some(d)) => (d - start).max(0.0),
                                        (None, None) => 0.0,
                                    };
                                    state.set_duration(len);
                                }
                                None => {
                                    if let Some(dur) = next.params().duration {
                                        state.set_duration(dur);
                                    } else {
                                        state.set_duration(0.0);
                                    }
                                }
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
                            // 区间状态续接：新区间的起点 / 终点 / 计数清零。
                            match next_range {
                                Some((start, end)) => {
                                    range_base = start;
                                    range_end = end;
                                    range_decoded_frames = 0;
                                    range_pushed_frames = 0;
                                }
                                None => {
                                    range_end = None;
                                }
                            }
                            // 同采样率同声道：resampler 配置不变，可直接复用；
                            // 重置解码状态继续推数据。
                            decoding = true;
                            draining = false;
                            state.clear_at_eof();
                            // 不清空 ringbuf：保留旧曲尾巴（约 2 秒）自然播完，
                            // 实现真正无缝（重采样器滤波尾巴已在上方 flush 推入）。
                            // 进度基准设为负的尾巴时长——旧曲尾巴播完前
                            // position ≤ 0（UI 侧钳制显示为 0），播完后新曲从 0 起。
                            let tail_frames = producer.occupied_len() / device_channels.max(1);
                            // 尾巴样本按流的真实采样率换算：原生直通会重建流、改变采样率，
                            // 不能用 spawn 捕获的 device_sample_rate，否则时长会偏约 8%。
                            let tail_secs = tail_frames as f64
                                / analyzer_rate(&state, device_sample_rate).max(1) as f64;
                            // 进度基准：区间切换按文件时间轴（新区间起点 - 尾巴），
                            // 整文件切换从负尾巴起（新文件从 0 起）。
                            let base = match next_range {
                                Some((start, _)) => start - tail_secs,
                                None => -tail_secs,
                            };
                            state.reset_position(base);
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

// =============================================================================
// PlayRange 集成测试（真驱动 decoder_loop：真实 WAV 后端 + 真 ringbuf）
// =============================================================================

#[cfg(test)]
mod range_tests {
    use super::*;
    use crate::audio::engine::{PreloadTarget, SharedState};
    use ringbuf::HeapRb;
    use std::io::Write as IoWrite;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    /// 造一个 3 秒 44.1kHz 立体声 WAV：帧 i 的两声道样本同为
    /// i16 值 (i % 30000)（便于按帧号逐点对齐区间起止）。
    fn make_ramp_wav(frames: usize) -> std::path::PathBuf {
        let mut data = Vec::with_capacity(frames * 4);
        for i in 0..frames {
            let v = (i % 30000) as i16;
            data.extend_from_slice(&v.to_le_bytes());
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&2u16.to_le_bytes()); // 立体声
        v.extend_from_slice(&44100u32.to_le_bytes());
        v.extend_from_slice(&176400u32.to_le_bytes()); // byte rate
        v.extend_from_slice(&4u16.to_le_bytes()); // block align
        v.extend_from_slice(&16u16.to_le_bytes()); // 16 位
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "tuneux-range-test-{}-{seq}.wav",
            std::process::id()
        ));
        let mut f = std::fs::File::create(&path).expect("建临时文件");
        f.write_all(&v).expect("写临时文件");
        path
    }

    /// 真驱动 decoder_loop：发命令、全程消费，直到 finished 上报。
    /// 返回（全部消费到的样本, 是否发生无缝切换, 结束时的 duration）。
    fn drive(cmds: Vec<DecoderCmd>) -> (Vec<f32>, bool, f64, bool) {
        let state = Arc::new(SharedState::default());
        state.set_playing(true);
        let state_in = state.clone();
        let (prod, mut cons) = HeapRb::<f32>::new(1 << 20).split();
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (fin_tx, fin_rx) = crossbeam_channel::unbounded();
        let (fail_tx, _fail_rx) = crossbeam_channel::unbounded::<()>();
        let (sw_tx, sw_rx) = crossbeam_channel::unbounded();
        let close = Arc::new(AtomicBool::new(false));
        let close_in = close.clone();
        let handle = std::thread::spawn(move || {
            decoder_loop(
                prod, cmd_rx, state_in, fin_tx, fail_tx, sw_tx, close_in, 44100, 2,
            );
        });
        for c in cmds {
            cmd_tx.send(c).expect("发命令");
        }
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            while let Some(s) = cons.try_pop() {
                out.push(s);
            }
            if fin_rx.try_recv().is_ok() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("finished 上报超时（已收 {} 样本）", out.len());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        close.store(true, Ordering::Relaxed);
        let _ = cmd_tx.send(DecoderCmd::Exit);
        let _ = handle.join();
        let switched = sw_rx.try_recv().is_ok();
        (out, switched, state.duration(), state.at_eof())
    }

    /// 帧号 → WAV 内 i16 样本的归一化值（与 make_ramp_wav 同口径）。
    fn frame_value(i: u64) -> f32 {
        (i % 30000) as i16 as f32 / 32768.0
    }

    /// finished 场景：区间 [1.0, 2.0) 播完自然结束——输出恰为 1 秒整
    ///（起点/终点帧逐点对齐，不多不少，position 不越界）。
    #[test]
    fn play_range_finished_exact_bounds() {
        let path = make_ramp_wav(132300); // 3 秒
        let (out, switched, dur, at_eof) = drive(vec![DecoderCmd::LoadRange {
            path: path.clone(),
            start_secs: 1.0,
            end_secs: Some(2.0),
        }]);
        assert!(!switched, "无预载不应无缝切换");
        assert!(
            at_eof,
            "finished 后应处于末尾态（无时长曲目重播判定依赖它）"
        );
        assert_eq!(dur, 1.0, "时长应按区间长度上报");
        // 恰 44100 帧 × 2 声道（文件率 = 设备率，无重采样尾巴）。
        assert_eq!(out.len(), 88200, "输出样本数应恰为区间长度");
        assert_eq!(out[0], frame_value(44100), "区间起点帧错位");
        assert_eq!(
            out[out.len() - 2],
            frame_value(44100 + 44100 - 1),
            "区间终点帧错位"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// gapless 场景：区间 [1.0, 2.0) + 预载相邻区间 [2.0, 3.0) ——
    /// 无缝切换发生、两段输出合计恰为 2 秒整、切换点样本逐点对齐
    ///（「区间终点落在持续声音中」夹具：下一曲开头零丢失）。
    #[test]
    fn play_range_gapless_adjacent_ranges() {
        let path = make_ramp_wav(132300);
        let (out, switched, _dur, at_eof) = drive(vec![
            DecoderCmd::LoadRange {
                path: path.clone(),
                start_secs: 1.0,
                end_secs: Some(2.0),
            },
            DecoderCmd::Preload(Some(PreloadTarget::range(path.clone(), 2.0, Some(3.0)))),
        ]);
        assert!(switched, "相邻区间应发生无缝切换");
        assert!(at_eof, "末段播完后应处于末尾态");
        assert_eq!(out.len(), 176400, "两段区间合计应恰为 2 秒");
        assert_eq!(out[0], frame_value(44100), "第一段起点帧错位");
        assert_eq!(
            out[88200],
            frame_value(88200),
            "切换后第二段起点帧错位（下一曲开头丢失）"
        );
        assert_eq!(
            out[out.len() - 2],
            frame_value(88200 + 44100 - 1),
            "第二段终点帧错位"
        );
        let _ = std::fs::remove_file(&path);
    }
}
