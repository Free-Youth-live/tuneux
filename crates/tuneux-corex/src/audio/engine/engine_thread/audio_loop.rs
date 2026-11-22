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

use super::super::{AudioCmd, DecoderCmd, SharedState};
use super::stream_builder::{build_stream, fmt_code, rebuild_stream, switch_stream_for_playback};
use super::CurrentTrack;

/// 消费解码线程的「当前曲目」通知，同步音频线程的播放路径/区间。
///
/// 无缝切曲只发生在解码线程：若仅靠本线程在 Play / Stop 上的赋值，
/// current_playback_path/range 会停留在上一曲，切曲后设备热切换会
/// reload 旧曲。解码线程在每次曲目变化（Load /
/// LoadAndSeek / LoadRange / 无缝切换 / Stop）时经该通道推送权威值，
/// 这里逐条消费覆盖本地值——通知按解码线程的发送顺序到达，末条
/// 即当前曲目（手动切曲/停止后，解码线程随后的 Load / Stop 通知
/// 自然覆盖此前残留的无缝切换通知）。
fn apply_track_refs(
    rx: &Receiver<Option<CurrentTrack>>,
    path: &mut Option<std::path::PathBuf>,
    range: &mut Option<(f64, Option<f64>)>,
) {
    while let Ok(notify) = rx.try_recv() {
        match notify {
            Some((p, r)) => {
                *path = Some(p);
                *range = r;
            }
            None => {
                *path = None;
                *range = None;
            }
        }
    }
}

/// 新设备默认流参数解析：拿到默认配置用新值，拿不到回退旧值。
///
/// 播放中热切换与停止/曲终热切换两个分支共用：两处都必须按新设备
/// 默认值同步流参数（采样率/通道数/格式），下一次建流才落在新设备
/// 布局上。
fn resolve_stream_defaults(
    new_cfg: Option<cpal::SupportedStreamConfig>,
    fallback_sr: u32,
    fallback_channels: u16,
    fallback_format: SampleFormat,
) -> (u32, u16, SampleFormat) {
    match new_cfg {
        Some(c) => (c.sample_rate().0, c.channels(), c.sample_format()),
        None => (fallback_sr, fallback_channels, fallback_format),
    }
}

/// 启动输出流，并把结果如实同步到播放状态、错误通道。
///
/// 状态同步原则：**状态标记必须在动作成功之后**——`play()` 成功才置
/// `[播]`；失败回落 `[停]`、把具体原因写入 `last_error`（UI 经
/// `take_last_error` 显示），再发 failed 事件交给上层决定是否跳曲。
///
/// 注意：`Some(Err(_))` 不得丢弃 cpal 原始错误文本——「失败无详情」会让
/// 用户只能看到反复跳曲，无法判断是设备被独占还是设备消失。
fn start_stream(
    state: &SharedState,
    stream: Option<&cpal::Stream>,
    failed_tx: &Sender<()>,
) -> bool {
    let play_result = stream.map(|s| s.play().map_err(|e| e.to_string()));
    start_stream_result(state, play_result, failed_tx)
}

/// `start_stream` 的可单测内核（三分支）：
///
/// - `Some(Ok(()))`：`play()` 成功 → 置 `[播]`，不动错误通道；
/// - `Some(Err(文本))`：`play()` 失败 → 回落 `[停]`，错误文本带 cpal 原始
///   信息，发 failed 事件；
/// - `None`：没有可用输出流（建流失败/设备不可用）→ 回落 `[停]`，写
///   不可用文案，发 failed 事件。
///
/// 返回是否真正开始播放（便于调用方/测试断言，不改变副作用顺序）。
fn start_stream_result(
    state: &SharedState,
    play_result: Option<Result<(), String>>,
    failed_tx: &Sender<()>,
) -> bool {
    match play_result {
        Some(Ok(())) => {
            state.set_playing(true);
            true
        }
        Some(Err(e)) => {
            state.set_playing(false);
            state.set_last_error(format!("音频输出流启动失败：{e}"));
            let _ = failed_tx.send(());
            false
        }
        None => {
            state.set_playing(false);
            state.set_last_error("音频输出流不可用：未能建立输出流".to_string());
            let _ = failed_tx.send(());
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn audio_loop(
    mut device: cpal::Device,
    config: StreamConfig,
    sample_format: SampleFormat,
    consumer: HeapCons<f32>,
    device_sample_rate: u32,
    device_channels: usize,
    cmd_rx: Receiver<AudioCmd>,
    dec_cmd_tx: Sender<DecoderCmd>,
    cur_track_rx: Receiver<Option<CurrentTrack>>,
    state: Arc<SharedState>,
    finished_tx: Sender<()>,
    failed_tx: Sender<()>,
    close: Arc<AtomicBool>,
    init_tx: SyncSender<Result<(), String>>,
) {
    // —— 创建 cpal 输出流（初始用设备默认采样率）——
    // stream 用 Option 包装：原生采样率直通时换曲会重建流（drop 旧的、
    // 建新的不同采样率流），需要可重新赋值。
    // 初始流的通道数 / 格式写入 SharedState：解码线程的通道适配与
    // 换曲重建自此读当前值，不再各自拿 spawn 捕获值。
    state.set_stream_channels(device_channels as u32);
    state.set_stream_format_code(fmt_code(sample_format));
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
    // CUE 区间随路径一起保留；普通整文件为 None。
    let mut current_playback_range: Option<(f64, Option<f64>)> = None;
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
        // 无缝切曲同步：解码线程的权威曲目通知（详见
        // apply_track_refs）。必须在设备热切换检测之前消费，reload 才
        // 总是指向最新曲目。
        apply_track_refs(
            &cur_track_rx,
            &mut current_playback_path,
            &mut current_playback_range,
        );
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
            current_playback_range = None;
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
                        device = new_device.clone();
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

                        // 采样率跟随新设备默认值（cpal 在新设备上必支持自家
                        // 默认，重建成功率最高）；拿不到配置时回退 spawn 捕获值。
                        //
                        // 通道数与样本格式同样跟随新设备（0.6.0 起）：
                        // 新设备默认布局/格式与旧设备不同时，流按新值建，
                        // SharedState 同步写入（rebuild_stream 成功路径），解码线程
                        // 的通道适配 / ring 交错 / 重采样器构建经随后的 Load 按
                        // 新通道数重建——三处同源，不会变速变调。I16/U16 默认
                        // 格式的设备自此可用（回调出口转换，内部管线恒 f32）。
                        let (target_sr, new_channels, new_format) = resolve_stream_defaults(
                            new_device.default_output_config().ok(),
                            device_sample_rate,
                            device_channels as u16,
                            sample_format,
                        );
                        let rebuilt = rebuild_stream(
                            &new_device,
                            target_sr,
                            new_channels,
                            new_format,
                            &state,
                        );
                        // 设备切换前的播放态与暂停进度：进度必须用**旧**流采样率
                        // 换算——frames_played 由回调按当时流的采样率累加，
                        // rebuild_stream / NewProducer 都不清零它（只有
                        // reset_position 会），所以要在下面覆盖 current_stream_sr
                        // **之前**取值；否则等于把旧率下的帧数除以新率，得到偏小的
                        // 秒数（44.1k→48k 播 3 分钟约少 14.6 秒，Resume 时向后退）。
                        let resume_secs = state.position(current_stream_sr.max(1));
                        let reload = |path: std::path::PathBuf| match current_playback_range {
                            Some((start, end)) => {
                                let _ = dec_cmd_tx.send(DecoderCmd::LoadRange {
                                    path,
                                    start_secs: start,
                                    end_secs: end,
                                });
                                let resume = resume_secs.max(start);
                                if resume > start {
                                    let _ = dec_cmd_tx.send(DecoderCmd::Seek(resume));
                                }
                            }
                            None => {
                                let _ = dec_cmd_tx.send(DecoderCmd::LoadAndSeek {
                                    path,
                                    secs: resume_secs,
                                });
                            }
                        };

                        match rebuilt {
                            Some((new_stream, new_prod)) => {
                                let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(new_prod));
                                stream = Some(new_stream);
                                current_stream_sr = target_sr;
                                state.set_stream_sample_rate(target_sr);
                                // 流默认暂停；Load 完成后由 start_stream 统一启动并置播放态。
                                if let Some(s) = stream.as_ref() {
                                    let _ = s.pause();
                                }
                                // 播放中与暂停态都保留当前位置；CUE 先恢复原区间，
                                // 再 seek 到区间内当前位置，避免退化成整文件播放。
                                // 解码线程按实际 seek 落点更新位置；失败时不提前跳表。
                                reload(path);
                                // 新流按设备默认采样率：文件原生率大概率不同，保守标为非直通
                                //（避免 UI"直通"标识失真）。
                                state.set_bitstream(false);
                                // 若之前在播放：必须显式启动——cpal 流 pause 后回调不运行，
                                // 等"自然消费"会永久静音。启动失败走统一口径（回落
                                // [停] + 上报原因），不得在此吞掉 play() 错误。
                                if state.is_playing() {
                                    start_stream(&state, stream.as_ref(), &failed_tx);
                                }
                            }

                            None => {
                                // 新设备建流失败：保留 device_sample_rate 回退（最后一次机会）。
                                // 通道数 / 格式与首选分支同口径：跟随新设备。
                                match rebuild_stream(
                                    &new_device,
                                    device_sample_rate,
                                    new_channels,
                                    new_format,
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
                                        // 回退建流同样恢复原位置与 CUE 区间。
                                        reload(path);
                                        state.set_bitstream(false);
                                        // 启动失败同样回落 [停] + 上报，杜绝假播放。
                                        if state.is_playing() {
                                            start_stream(&state, stream.as_ref(), &failed_tx);
                                        }
                                    }
                                    None => {
                                        // 新设备彻底建流失败：不能保留「[播] 但无声」的假状态，
                                        // 回落 [停] 并上报设备级原因。这里不发 failed 事件——
                                        // 设备级故障发 failed 会触发自动跳曲遍历整个列表，
                                        // 反而放大问题；停止 + 错误提示已足够，恢复由用户操作。
                                        state.set_playing(false);
                                        state.set_last_error(
                                            "音频设备不可用：新设备建流失败，播放已停止"
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
                if current_playback_path.is_none() {
                    // 停止/曲终后的设备变化也更新句柄；旧流失效后，下一次 Play
                    // 会在新设备上重建，而不是继续向启动时的旧设备输出。
                    // 与播放中热切换同口径：按新设备默认值同步流参数到
                    // SharedState（旧实现只换句柄，下一次
                    // Play 仍按旧设备布局/采样率建流）。
                    if let Some(new_device) = cpal::default_host().default_output_device() {
                        let (sr, ch, fmt) = resolve_stream_defaults(
                            new_device.default_output_config().ok(),
                            device_sample_rate,
                            device_channels as u16,
                            sample_format,
                        );
                        current_stream_sr = sr;
                        state.set_stream_sample_rate(sr);
                        state.set_stream_channels(ch as u32);
                        state.set_stream_format_code(fmt_code(fmt));
                        // 流已 drop，无「直通」可谈，标志复位避免 UI 残留旧值。
                        state.set_bitstream(false);
                        device = new_device;
                        stream.take();
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
                    &mut stream,
                    &mut current_stream_sr,
                    &dec_cmd_tx,
                    &state,
                );
                state.reset_position(0.0);
                state.bump_flush();
                let _ = state.take_playback_finished();
                let _ = dec_cmd_tx.send(DecoderCmd::Load(path.clone()));
                let started = start_stream(&state, stream.as_ref(), &failed_tx);
                // 只有流真正启动成功才记录播放路径：热切换重建以该路径为准，
                // 启动失败时不应留下「可重建的在播曲目」，否则设备变化会为一个
                // 从未开始的曲目重建流（注释与真实状态机保持一致）。
                if started {
                    current_playback_path = Some(path);
                    current_playback_range = None;
                }
            }
            AudioCmd::PlayResume { path, secs } => {
                // 接着上次听：与 Play 相同的直通 + 流重建（含回退）。
                switch_stream_for_playback(
                    &path,
                    &device,
                    device_sample_rate,
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
                let started = start_stream(&state, stream.as_ref(), &failed_tx);
                // 记录当前播放路径（同 Play：只有流真正启动成功才记录，
                // 路径决定热切换重建用哪首歌的流）。
                if started {
                    current_playback_path = Some(path);
                    current_playback_range = None;
                }
            }
            AudioCmd::PlayRange {
                path,
                start_secs,
                end_secs,
            } => {
                // 区间播放：与 PlayResume 相同的直通 + 流重建（含回退）。
                switch_stream_for_playback(
                    &path,
                    &device,
                    device_sample_rate,
                    &mut stream,
                    &mut current_stream_sr,
                    &dec_cmd_tx,
                    &state,
                );
                // 进度按文件时间轴（base = start_secs）：UI 层按既有
                // track_progress 口径换算区间相对进度；时长由解码线程
                // 按区间长度覆盖上报。
                state.reset_position(start_secs);
                state.bump_flush();
                let _ = state.take_playback_finished();
                let _ = dec_cmd_tx.send(DecoderCmd::LoadRange {
                    path: path.clone(),
                    start_secs,
                    end_secs,
                });
                let started = start_stream(&state, stream.as_ref(), &failed_tx);
                // 同 Play：只有流真正启动成功才记录路径与区间。
                if started {
                    current_playback_path = Some(path);
                    current_playback_range = Some((start_secs, end_secs));
                }
            }
            AudioCmd::Pause => {
                state.set_playing(false);
                if let Some(s) = stream.as_ref() {
                    let _ = s.pause();
                }
            }
            AudioCmd::Resume => {
                start_stream(&state, stream.as_ref(), &failed_tx);
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
                state.set_src_params(None);
                // 停止即离开末尾态：不清理的话「空态」与
                // 「停在末尾」混淆，主线程会在下一次成功 Load 前一直按末尾态
                // 判定（Resume 无意义、空格重播判定错位）。
                state.clear_at_eof();
                // 停止后清空播放路径——设备热切换时无路径可重建（正确行为）
                current_playback_path = None;
                current_playback_range = None;
            }
            AudioCmd::Seek(secs) => {
                // 解码线程确认实际落点后才重置时钟并冲刷旧缓冲；失败时
                // 当前位置与正在播放的内容都保持不变。
                let _ = dec_cmd_tx.send(DecoderCmd::Seek(secs));
            }
            AudioCmd::SetVolume(vol) => {
                state.set_volume(vol);
            }
            AudioCmd::PreloadNext(target) => {
                // Gapless 预载：转发给解码线程提前打开下一曲后端（不重建流）；
                // 区间预载由解码线程先 seek 到 start_secs 再缓存。
                let _ = dec_cmd_tx.send(DecoderCmd::Preload(target));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 「当前曲目」通知按 FIFO 覆盖：区间曲目 → 整文件曲目 → 清除。
    /// 切曲/停止后音频线程的播放路径/区间必须跟随解码线程权威值。
    #[test]
    fn apply_track_refs_follows_fifo_notifications() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut path: Option<std::path::PathBuf> = Some("a.wav".into());
        let mut range: Option<(f64, Option<f64>)> = None;
        // 空通道：本地值不动。
        apply_track_refs(&rx, &mut path, &mut range);
        assert_eq!(path.as_deref(), Some(std::path::Path::new("a.wav")));
        // 无缝切到区间曲目：路径与区间一起更新。
        let _ = tx.send(Some((
            std::path::PathBuf::from("cue.flac"),
            Some((1.0, Some(2.0))),
        )));
        apply_track_refs(&rx, &mut path, &mut range);
        assert_eq!(path.as_deref(), Some(std::path::Path::new("cue.flac")));
        assert_eq!(range, Some((1.0, Some(2.0))));
        // 切到整文件曲目：路径更新、区间清空。
        let _ = tx.send(Some((std::path::PathBuf::from("b.wav"), None)));
        apply_track_refs(&rx, &mut path, &mut range);
        assert_eq!(path.as_deref(), Some(std::path::Path::new("b.wav")));
        assert_eq!(range, None);
        // 停止（None）：路径/区间归空。
        let _ = tx.send(None);
        apply_track_refs(&rx, &mut path, &mut range);
        assert!(path.is_none(), "清除后路径应为 None");
        assert!(range.is_none(), "清除后区间应为 None");
    }

    /// `start_stream` 三分支契约：
    /// 成功才置播放态；play() 失败回落 [停] 且错误带原始文本；无输出流
    /// 回落 [停] 且错误为不可用文案。失败分支都必须发出 failed 事件。
    #[test]
    fn start_stream_result_three_branches() {
        // 成功：置播放态、无错误、无失败事件。
        let state = SharedState::default();
        state.set_playing(false);
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(start_stream_result(&state, Some(Ok(())), &tx));
        assert!(state.is_playing(), "play() 成功才置 [播]");
        assert!(state.take_last_error().is_none(), "成功不应写错误");
        assert!(rx.try_recv().is_err(), "成功不应发 failed 事件");

        // play() 失败：必须回落 [停]、写 cpal 原始错误文本、发 failed 事件。
        let state = SharedState::default();
        state.set_playing(true);
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(!start_stream_result(
            &state,
            Some(Err("Device or resource busy".to_string())),
            &tx
        ));
        assert!(!state.is_playing(), "play() 失败必须回落 [停]");
        let err = state.take_last_error().expect("失败应写入错误原因");
        assert!(
            err.contains("音频输出流启动失败"),
            "应说明是输出流启动失败：{err}"
        );
        assert!(
            err.contains("Device or resource busy"),
            "应保留 cpal 原始文本：{err}"
        );
        assert!(rx.try_recv().is_ok(), "play() 失败应发 failed 事件");

        // 无输出流：必须回落 [停]、写不可用文案、发 failed 事件。
        let state = SharedState::default();
        state.set_playing(true);
        let (tx, rx) = crossbeam_channel::unbounded();
        assert!(!start_stream_result(&state, None, &tx));
        assert!(!state.is_playing(), "无输出流必须回落 [停]");
        let err = state.take_last_error().expect("失败应写入错误原因");
        assert!(
            err.contains("音频输出流不可用"),
            "应说明输出流不可用：{err}"
        );
        assert!(rx.try_recv().is_ok(), "无输出流应发 failed 事件");
    }

    /// 新设备默认流参数解析：拿不到默认配置时回退旧值（两分支共用本函数）。
    /// Some 分支需真实 cpal 设备（SupportedStreamConfig 字段
    /// 私有不可构造），由真实设备路径覆盖。
    #[test]
    fn resolve_stream_defaults_falls_back_without_config() {
        assert_eq!(
            resolve_stream_defaults(None, 48_000, 2, cpal::SampleFormat::F32),
            (48_000, 2, cpal::SampleFormat::F32)
        );
    }
}
