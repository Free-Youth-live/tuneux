//! 音频线程主循环：持 cpal Stream + HeapCons，分发 AudioCmd 命令。
//!
//! 从 engine_thread.rs 拆分。音频线程独占 cpal::Stream（macOS 非 Send）与
//! HeapCons；命令分发 match 原样保留；设备热切换轮询逻辑内联在本模块。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::{Receiver, Sender};
use ringbuf::HeapCons;

use crate::audio::engine::{AudioCmd, DecoderCmd, SharedState};
use crate::audio::engine_thread::stream_builder::{
    build_stream, rebuild_stream, switch_stream_for_playback,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn audio_loop(
    device: cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    consumer: HeapCons<f32>,
    device_sample_rate: u32,
    device_channels: usize,
    cmd_rx: Receiver<AudioCmd>,
    dec_cmd_tx: Sender<DecoderCmd>,
    state: Arc<SharedState>,
    finished_tx: Sender<()>,
    failed_tx: Sender<()>,
    close: Arc<AtomicBool>,
    init_tx: SyncSender<Result<(), String>>,
) {
    // —— 创建 cpal 输出流（初始用设备默认采样率）——
    // stream 用 Option 包装：原生采样率直通时换曲会重建流（drop 旧的、
    // 建新的不同采样率流），需要可重新赋值。
    let state_cb = Arc::clone(&state);
    let initial_stream: Option<Stream> = build_stream(
        &device,
        &config,
        sample_format,
        consumer,
        state_cb,
        device_sample_rate,
        device_channels,
    );

    if initial_stream.is_none() {
        // 建流失败：init_tx 已把原因通知主线程（UI 状态栏），不再 eprintln。
        let _ = init_tx.send(Err("创建输出流失败".into()));
        close.store(true, Ordering::Relaxed);
        return;
    }
    let mut stream: Option<Stream> = initial_stream;
    // 当前流使用的采样率（重建后会更新）。用于判断换曲时是否需要重建。
    let mut current_stream_sr = device_sample_rate;

    // 初始化成功，通知 spawn_threads 可以返回了
    let _ = init_tx.send(Ok(()));
    // 记录初始流采样率，供解码线程判断是否需要重采样
    state.set_stream_sample_rate(device_sample_rate);

    // 初始暂停，等待 Play
    if let Some(s) = stream.as_ref() {
        let _ = s.pause();
    }

    // —— 设备热切换状态 ——
    // 记录当前正在播放的文件路径，用于设备变化时重建流（参考 switch_stream_for_playback）。
    // - Play / PlayResume 时设置（流/解码器就绪，能"接着播"）；
    // - Stop 时清空（已停止，没东西可播）；
    // - 解码线程 EOF 后通过 take_playback_finished 检测到时清空（播完了）。
    let mut current_playback_path: Option<std::path::PathBuf> = None;
    // 设备轮询节流：每 ~2 秒问一次系统默认输出设备。cpal 0.15 没有设备热插拔事件，
    // 必须主动 poll；2 秒是常见的"用户可感知延迟 vs 唤醒开销"折衷——插拔耳机到听见
    // 切到新设备最迟 ~2s。
    let mut last_device_check = Instant::now();
    let device_check_interval = Duration::from_secs(2);
    // 记录上次成功的默认输出设备名（name() 返回 Option<String>，None 表示"无可用设备"）。
    // 用设备名而非 Device 指针——cpal::Device 不实现 Hash/PartialEq，无法直接比较。
    // 用 String 作为"当前绑定的默认设备"标识符，配合 Option<String> 表示"上一次不可用"。
    let mut last_default_device_name: Option<String> = device.name().ok();

    // —— 主命令循环 ——
    while !close.load(Ordering::Relaxed) {
        // 空闲时轮询"解码完成"信号：解码线程遇到 EOF/致命错误时置位，
        // 音频线程自动暂停流，避免播完继续吐静音让用户被动等。
        if state.take_playback_finished() {
            state.set_playing(false);
            if let Some(s) = stream.as_ref() {
                let _ = s.pause();
            }
            // EOF / 致命解码错误：曲终已停，不再有"正在播放"的曲目，
            // 清空路径以免设备热切换误触发重建。
            current_playback_path = None;
        }

        // —— 设备热切换检测：节流轮询默认输出设备，变化则重建流 ——
        // cpal 0.15 没有设备热插拔事件，只能 poll。每轮 recv_timeout 100ms + 2s 节流
        // → 系统调用开销可忽略。
        if last_device_check.elapsed() >= device_check_interval {
            last_device_check = Instant::now();
            let new_device_name = cpal::default_host()
                .default_output_device()
                .and_then(|d| d.name().ok());
            // 名称变了才视为"设备切换"；都不可用（Some→None 或 None→None 同名）则跳过。
            if new_device_name != last_default_device_name {
                // 设备变化：走下方重建逻辑即可，状态栏无需逐次刷屏。
                last_default_device_name = new_device_name;

                // 有播放路径即重建（含暂停态——暂停时拔插耳机，恢复后不能绑旧设备；
                // 重建保持暂停状态，恢复播放时用新流）。EOF/停止时路径已清空，自然跳过。
                if let Some(path) = current_playback_path.clone() {
                    // 拿真实的新设备引用（前面只比对了 name，这里再取一次 device）
                    if let Some(new_device) = cpal::default_host().default_output_device() {
                        // 通知 UI：正在切设备，避免旧设备最后一帧被错误呈现。
                        // 同时让回调丢弃旧环形缓冲里的"待播"样本（设备变了，旧样本
                        // 与新设备的 sample rate / 通道可能对不上）。
                        state.bump_flush();

                        // 等效 switch_stream_for_playback 的重建：
                        // 1) 先让解码线程停推（Stop），再短暂等，避免旧 consumer 被 take 时
                        //    解码线程阻塞在 push_all（与 switch_stream_for_playback 同样理由）。
                        // 2) drop 旧 Stream（释放旧 consumer）。
                        // 3) 用新设备重建流——优先尝试新设备默认采样率（兼容性最好），
                        //    失败回退到旧 device_sample_rate，最后回退到 None。
                        // 4) 发 Load(path) 让解码线程重打开当前文件——NewProducer 已清掉
                        //    current，没有 Load 用户听到的是静音。
                        let _ = dec_cmd_tx.send(DecoderCmd::Stop);
                        std::thread::sleep(Duration::from_millis(30));
                        stream.take();

                        // 拿到新设备的默认采样率（如果可读）；拿不到则用 spawn 时捕获的
                        // device_sample_rate 兜底。新设备默认采样率是最稳的"目标 sr"——
                        // cpal 在新设备上必支持自家默认值，build_output_stream 不会因采样率不支持失败。
                        let new_default_sr = new_device
                            .default_output_config()
                            .map(|c| c.sample_rate().0)
                            .unwrap_or(device_sample_rate);
                        // 重建目标采样率 = 新设备的默认采样率（无论与当前流是否
                        // 一致，取值都等于 new_default_sr，无需分支）。
                        let target_sr = new_default_sr;
                        let rebuilt = rebuild_stream(
                            &new_device,
                            target_sr,
                            device_channels as u16,
                            sample_format,
                            &state,
                        );
                        match rebuilt {
                            Some((new_stream, new_prod)) => {
                                let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(new_prod));
                                stream = Some(new_stream);
                                current_stream_sr = target_sr;
                                state.set_stream_sample_rate(target_sr);
                                // 流默认暂停，下面 Load 完后 set_playing(true) 会启动它。
                                if let Some(s) = stream.as_ref() {
                                    let _ = s.pause();
                                }
                                // 重新加载当前文件：NewProducer 已清掉 current / resampler，
                                // 这里用同路径 Load（不传 secs——从 0 重新开始；设备切换时
                                // 用户对"接着上次"的预期弱于"听见声音"，重新开始是更稳的取舍）。
                                let _ = dec_cmd_tx.send(DecoderCmd::Load(path));
                                // 进度基准清零：重建后从文件头重播，回调 add_frames 从 0 起，
                                // 否则进度显示错位超前。
                                state.reset_position(0.0);
                                // 新流按设备默认采样率：文件原生率大概率不同，保守标为非直通
                                //（避免 UI"直通"标识失真）。
                                state.set_bitstream(false);
                                // 若之前在播放：必须显式 play()——cpal 流 pause 后回调不运行，
                                // 等"自然消费"会永久静音。
                                if state.is_playing() {
                                    if let Some(s) = stream.as_ref() {
                                        let _ = s.play();
                                    }
                                }
                            }

                            None => {
                                // 新设备建流失败：保留 device_sample_rate 回退（最后一次机会）
                                match rebuild_stream(
                                    &new_device,
                                    device_sample_rate,
                                    device_channels as u16,
                                    sample_format,
                                    &state,
                                ) {
                                    Some((new_stream, new_prod)) => {
                                        let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(new_prod));
                                        stream = Some(new_stream);
                                        current_stream_sr = device_sample_rate;
                                        state.set_stream_sample_rate(device_sample_rate);
                                        if let Some(s) = stream.as_ref() {
                                            let _ = s.pause();
                                        }
                                        let _ = dec_cmd_tx.send(DecoderCmd::Load(path));
                                        state.reset_position(0.0);
                                        state.set_bitstream(false);
                                        if state.is_playing() {
                                            if let Some(s) = stream.as_ref() {
                                                let _ = s.play();
                                            }
                                        }
                                    }
                                    None => {
                                        // stream 已 drop，保持 None——下一拍 is_playing=true
                                        // 但 stream=None 不影响线程健康，只是听不见声音。
                                        // 彻底失败必须让用户知道（听不见声音），走 UI 错误通道。
                                        state.set_last_error(
                                            "音频设备不可用：新设备建流失败，已保持静音"
                                                .to_string(),
                                        );
                                        current_stream_sr = device_sample_rate;
                                        state.set_stream_sample_rate(device_sample_rate);
                                    }
                                }
                            }
                        }
                    }
                }
                // 设备不可用（None）时不重建，只更新 last 状态，等设备回来下次检测再处理。
            }
        }

        let cmd = match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => c,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };

        match cmd {
            AudioCmd::Play(path) => {
                // 原生采样率直通 + 流重建（含失败回退）。
                // 公共函数内部会同步 stream_sample_rate / bitstream 标志。
                switch_stream_for_playback(
                    &path,
                    &device,
                    device_sample_rate,
                    device_channels,
                    sample_format,
                    &mut stream,
                    &mut current_stream_sr,
                    &dec_cmd_tx,
                    &state,
                );
                state.reset_position(0.0);
                state.bump_flush();
                let _ = state.take_playback_finished();
                let _ = dec_cmd_tx.send(DecoderCmd::Load(path.clone()));
                state.set_playing(true);
                if let Some(s) = stream.as_ref() {
                    let _ = s.play();
                }
                // 记录当前播放路径，供设备热切换时重建流使用。
                // 必须在 play() 之后——只有流真的启动了才视为"在播"。
                current_playback_path = Some(path);
            }
            AudioCmd::PlayResume { path, secs } => {
                // 接着上次听：与 Play 相同的直通 + 流重建（含回退）。
                switch_stream_for_playback(
                    &path,
                    &device,
                    device_sample_rate,
                    device_channels,
                    sample_format,
                    &mut stream,
                    &mut current_stream_sr,
                    &dec_cmd_tx,
                    &state,
                );
                // base=secs：回调从 0 计数时 position() 返回 ~secs；解码器加载
                // 完立刻 seek 到 secs，避免 Play+Seek 竞态。
                state.reset_position(secs);
                state.bump_flush();
                let _ = state.take_playback_finished();
                let _ = dec_cmd_tx.send(DecoderCmd::LoadAndSeek {
                    path: path.clone(),
                    secs,
                });
                state.set_playing(true);
                if let Some(s) = stream.as_ref() {
                    let _ = s.play();
                }
                // 记录当前播放路径（PlayResume 同上，路径决定重建用哪首歌的流）
                current_playback_path = Some(path);
            }
            AudioCmd::Pause => {
                state.set_playing(false);
                if let Some(s) = stream.as_ref() {
                    let _ = s.pause();
                }
            }
            AudioCmd::Resume => {
                state.set_playing(true);
                if let Some(s) = stream.as_ref() {
                    let _ = s.play();
                }
            }
            AudioCmd::Stop => {
                state.set_playing(false);
                if let Some(s) = stream.as_ref() {
                    let _ = s.pause();
                }
                state.bump_flush();
                let _ = dec_cmd_tx.send(DecoderCmd::Stop);
                state.reset_position(0.0);
                // 状态归零：时长与直通标志一并重置，避免"停止后元数据面板
                // 仍显示上一曲时长 / 技术参数行残留直通亮色"
                state.set_duration(0.0);
                state.set_bitstream(false);
                // 停止后清空播放路径——设备热切换时无路径可重建（正确行为）
                current_playback_path = None;
            }
            AudioCmd::Seek(secs) => {
                state.reset_position(secs);
                state.bump_flush();
                let _ = dec_cmd_tx.send(DecoderCmd::Seek(secs));
            }
            AudioCmd::SetVolume(vol) => {
                state.set_volume(vol);
            }
            AudioCmd::PreloadNext(path) => {
                // Gapless 预载：转发给解码线程提前打开下一曲后端（不重建流）。
                let _ = dec_cmd_tx.send(DecoderCmd::Preload(path));
            }
        }
    }

    // 通知解码线程退出
    let _ = dec_cmd_tx.send(DecoderCmd::Exit);
    // drop Stream：停止播放、释放设备（Option 自动 drop 内部 Stream）
    drop(stream);
    // finished_tx / failed_tx 不再需要，drop 让接收端能感知结束
    drop(finished_tx);
    drop(failed_tx);
}
