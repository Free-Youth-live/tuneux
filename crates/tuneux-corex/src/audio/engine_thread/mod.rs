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

use super::engine::{AudioCmd, DecoderCmd, SharedState};

use self::audio_loop::audio_loop;
use self::decoder_loop::decoder_loop;

use self::stream_builder::RING_CAPACITY;

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
                    close,
                    device_sample_rate,
                    device_channels,
                );
            })?;
    }

    // —— spawn 音频线程（持 consumer 与 cpal Stream）——
    std::thread::Builder::new()
        .name("tuneux-audio".into())
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
                state,
                finished_tx,
                failed_tx,
                close,
                init_tx,
            );
        })?;

    // 等待音频线程初始化结果。带超时避免极端情况下死等（cpal 本身初始化一般 <100ms）。
    match init_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(Ok(())) => Ok(device_sample_rate),
        Ok(Err(e)) => Err(e.into()),
        Err(RecvTimeoutError::Timeout) => Err("音频线程初始化超时".into()),
        Err(RecvTimeoutError::Disconnected) => Err("音频线程异常退出".into()),
    }
}
