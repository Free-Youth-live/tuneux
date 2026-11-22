//! # 音频引擎线程实现
//!
//! 实现音频线程（持 cpal Stream）和常驻解码线程。
//!
//! ## 数据流
//!
//! ```text
//! [解码线程]  symphonia 解码 → 重采样 → producer.push → ringbuf
//!                                                        │
//!                                            ringbuf（无锁 SPSC）
//!                                                        │
//! [音频线程]  cpal 回调：consumer.pop → 应用音量 → 喂设备
//!                          └ 累加 frames_played（进度）
//! ```
//!
//! ## 命令流
//!
//! - 主线程 → audio_cmd → 音频线程
//! - 音频线程 → decoder_cmd → 解码线程
//! - 解码线程 → 主线程：finished（EOF）/ failed（打开解码失败）/
//!   track_switched（无缝切换）
//! - 解码线程 → 音频线程：当前曲目通知（Load / LoadAndSeek / LoadRange /
//!   无缝切换 / Stop）——音频线程据此同步设备热切换的 reload 目标
//!
//! ## 子模块（文件过大拆分）
//!
//! - [`audio_loop`]：音频线程主循环 + AudioCmd 命令分发
//! - [`decoder_loop`]：解码线程主循环 + 文件加载 + 状态机测试
//! - [`stream_builder`]：cpal 流构建/重建/直通（含实时回调）
//! - [`sample_utils`]：纯函数（通道适配、缓冲写入、频谱累加）

mod audio_loop;
mod decoder_loop;
mod sample_utils;
mod stream_builder;

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::StreamConfig;
use crossbeam_channel::{Receiver, Sender};
use ringbuf::{traits::*, HeapRb};

use super::{AudioCmd, DecoderCmd, SharedState};

use self::audio_loop::audio_loop;
use self::decoder_loop::decoder_loop;

use self::stream_builder::RING_CAPACITY;

/// 「当前曲目」引用：路径 + 可选区间（None = 整文件）。
///
/// 解码线程 → 音频线程 的通知载荷；`Option` 外层为 None 表示「无当前曲目」
/// （Stop 清除）。无缝切曲只在解码线程内发生，音频线程据此类通知同步
/// 设备热切换的 reload 目标，避免切曲后 reload 旧曲。
type CurrentTrack = (std::path::PathBuf, Option<(f64, Option<f64>)>);

/// 启动音频线程与解码线程。返回设备采样率（用于进度计算）。
///
/// 在调用线程内完成 cpal 设备初始化（可能失败需传播错误），
/// 然后 spawn 两个后台线程。
pub(super) fn spawn_threads(
    cmd_rx: Receiver<AudioCmd>,
    state: Arc<SharedState>,
    finished_tx: Sender<()>,
    failed_tx: Sender<()>,
    track_switched_tx: Sender<()>,
    close: Arc<AtomicBool>,
) -> Result<u32, Box<dyn std::error::Error>> {
    // —— 初始化 cpal 输出设备 ——
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("找不到音频输出设备")?;

    let supported = device.default_output_config()?;
    let device_sample_rate = supported.sample_rate().0;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let device_channels = config.channels as usize;

    // —— 创建 ringbuf ——
    let ring = HeapRb::<f32>::new(RING_CAPACITY);
    let (producer, consumer) = ring.split();

    // —— decoder 命令通道（音频线程 → 解码线程）——
    let (dec_cmd_tx, dec_cmd_rx) = crossbeam_channel::unbounded::<DecoderCmd>();

    // —— 解码线程 → 音频线程 的「当前曲目」通知通道 ——
    // 无缝切曲只发生在解码线程内：Load/Stop 等命令均由音频线程转发，
    // 音频线程无从得知切换后的新曲目。解码线程在每次曲目变化（Load /
    // LoadAndSeek / LoadRange / 无缝切换 / Stop）时推送权威路径/区间，
    // 音频线程主循环逐条消费，设备热切换的 reload 恒指向当前曲目。
    let (cur_track_tx, cur_track_rx) = crossbeam_channel::unbounded::<Option<CurrentTrack>>();

    // —— 初始化同步通道：音频线程把"流创建结果"同步回 spawn_threads ——
    // 必须同步等：cpal 流可能因设备独占等原因失败，Engine::new 必须知道
    // 才能向用户报错。音频线程建流后通过该通道发送 Ok/Err，然后继续主循环。
    let (init_tx, init_rx) = sync_channel::<Result<(), String>>(1);

    // —— spawn 解码线程（常驻，持有 producer）——
    {
        let state = Arc::clone(&state);
        let finished_tx = finished_tx.clone();
        let failed_tx = failed_tx.clone();
        let track_switched_tx = track_switched_tx.clone();
        let cur_track_tx = cur_track_tx.clone();
        let close = Arc::clone(&close);
        std::thread::Builder::new()
            .name("tuneux-decoder".into())
            .spawn(move || {
                decoder_loop(
                    producer,
                    dec_cmd_rx,
                    state,
                    finished_tx,
                    failed_tx,
                    track_switched_tx,
                    cur_track_tx,
                    close,
                    device_sample_rate,
                    device_channels,
                );
            })?;
    }

    // 超时/断连/音频 spawn 失败路径需要置 close，但原 close 会被 move 进
    // 下方音频线程闭包。
    let close_err = Arc::clone(&close);
    // 音频 spawn 失败时补发 Exit 用：dec_cmd_tx 将被 move 进音频闭包。
    let dec_cmd_tx_err = dec_cmd_tx.clone();

    // —— spawn 音频线程（持 consumer 与 cpal Stream）——
    // 栈余量 4MB：build_stream 帧含回调管线的栈上定长缓冲（FFT 环形缓冲等），
    // 默认 2MB 栈在管线结构体化后实测溢出（RUST 默认 spawned 线程栈偏小）；
    // 内存按页懒提交，余量不占常驻。
    let audio_spawn = std::thread::Builder::new()
        .name("tuneux-audio".into())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            audio_loop(
                device,
                config,
                sample_format,
                consumer,
                device_sample_rate,
                device_channels,
                cmd_rx,
                dec_cmd_tx,
                cur_track_rx,
                state,
                finished_tx,
                failed_tx,
                close,
                init_tx,
            );
        });
    if let Err(e) = audio_spawn {
        // 音频线程 spawn 失败：解码线程已先行启动且仍在等待命令——置 close
        // 并发 Exit 让它退出（与下方 init 超时/断连路径同口径），否则解码
        // 线程空转永不回收。dec_cmd_tx 已 move 进音频
        // 闭包（失败即随闭包 drop），这里用预先 clone 的发送端补发 Exit。
        close_err.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = dec_cmd_tx_err.send(DecoderCmd::Exit);
        return Err(e.into());
    }

    // 等待音频线程初始化结果。带超时避免极端情况下死等（cpal 本身初始化一般 <100ms）。
    match init_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => Ok(device_sample_rate),
        Ok(Err(e)) => Err(e.into()),
        Err(RecvTimeoutError::Timeout) => {
            // 置 close 让已 spawn 的两个线程退出，防泄漏（音频线程持 cpal
            // 设备，泄漏会阻止后续 Engine::new 抢占设备）。
            close_err.store(true, std::sync::atomic::Ordering::Relaxed);
            Err("音频线程初始化超时".into())
        }
        Err(RecvTimeoutError::Disconnected) => {
            close_err.store(true, std::sync::atomic::Ordering::Relaxed);
            Err("音频线程异常退出".into())
        }
    }
}
