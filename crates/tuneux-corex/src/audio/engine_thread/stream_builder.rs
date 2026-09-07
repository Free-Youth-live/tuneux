//! 音频流构建：cpal 流创建、重建、直通（含实时回调）。
//!
//! 从 engine_thread.rs 拆分。本模块是**音频侧**辅助：build_stream 的回调闭包
//! 运行在 cpal 回调线程，有零堆分配、无锁的硬实时约束——拆分时未改动回调
//! 内部实现（FFT scratch 预分配、音量×ReplayGain、flush 世代检测等原样保留）。
//!
//! - sample_rate_supported：设备是否支持某采样率
//! - rebuild_stream：按设备重建输出流（热切换/回退用）
//! - switch_stream_for_playback：切到新文件时重建流（采样率直通）
//! - build_stream：创建 cpal 输出流（核心回调）

use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use ringbuf::{traits::*, HeapCons, HeapProd, HeapRb};

use crate::audio::engine::{DecoderCmd, SharedState};
use crate::audio::engine_thread::sample_utils::accumulate_spectrum;
use crate::audio::spectrum;

pub(super) const RING_CAPACITY: usize = 48000 * 2 * 2;

/// 查询设备是否支持指定采样率。
///
/// cpal 的 `supported_output_configs()` 返回若干 `SupportedStreamConfigRange`，
/// 每个覆盖一段采样率区间 [min, max]。只要目标采样率落在任一区间内即视为支持。
/// 用于原生采样率直通策略：换曲时判断能否用文件原生采样率打开流。
pub(super) fn sample_rate_supported(device: &cpal::Device, rate: u32) -> bool {
    match device.supported_output_configs() {
        Ok(mut configs) => {
            configs.any(|c| rate >= c.min_sample_rate().0 && rate <= c.max_sample_rate().0)
        }
        Err(_) => false,
    }
}

/// 重建 ringbuf + Stream（用于采样率切换）。
///
/// 返回 (新 Stream, 新 Producer)。新 Producer 应通过 DecoderCmd::NewProducer
/// 发给解码线程；新 Stream 由调用方持有。
///
/// 流程：建新 ringbuf → split → 用目标采样率构造 StreamConfig → build_stream。
/// 旧的 ringbuf/Stream/consumer 由调用方在调用前 drop（consumer 随旧 Stream 释放）。
pub(super) fn rebuild_stream(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u16,
    sample_format: SampleFormat,
    state: &Arc<SharedState>,
) -> Option<(Stream, HeapProd<f32>)> {
    // 建新 ringbuf（与初始容量一致）
    let ring = HeapRb::<f32>::new(RING_CAPACITY);
    let (producer, consumer) = ring.split();

    // 构造目标采样率的 StreamConfig
    let config = StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let stream = build_stream(
        device,
        &config,
        sample_format,
        consumer,
        Arc::clone(state),
        sample_rate,
        channels as usize,
    )?;
    Some((stream, producer))
}

/// 播放前的「原生采样率直通 + 流重建（含失败回退）」。
///
/// `AudioCmd::Play` 与 `AudioCmd::PlayResume` 共用同一套直通与重建逻辑。
///
/// 处理流程：
/// 1. 轻量 probe 读文件采样率；设备支持 → 目标 = 文件原生采样率（直通），
///    否则保持当前流（rubato 软件重采样降级）；
/// 2. 目标 ≠ 当前流采样率时重建流：先让解码线程 Stop 并等 30ms，确保它
///    不在 push_all 里阻塞（否则旧 consumer 被 take 后会卡在满 ringbuf 上），
///    然后 drop 旧 Stream → 按目标采样率重建 → 新 producer 发给解码线程；
///    重建失败则回退初始设备采样率（降级），仍失败则 stream 保持 None
///    （后续 play 前有判空，静默但不会崩溃）；
/// 3. 同步 `stream_sample_rate`（解码线程据此判断是否重采样）与
///    `bitstream` 标志（UI 据此亮/暗技术参数行）。
///
/// 注意 bitstream 按最终流采样率重算：若目标 44.1kHz 重建失败回退到
/// 48kHz，实际已降级，标志必须为 false。
#[allow(clippy::too_many_arguments)]
pub(super) fn switch_stream_for_playback(
    path: &std::path::Path,
    device: &cpal::Device,
    device_sample_rate: u32,
    device_channels: usize,
    sample_format: SampleFormat,
    stream: &mut Option<Stream>,
    current_stream_sr: &mut u32,
    dec_cmd_tx: &Sender<DecoderCmd>,
    state: &Arc<SharedState>,
) {
    // —— 直通 ——
    let (target_sr, want_bitstream) = match crate::audio::decoder::probe_sample_rate(path) {
        Some(sr) if sample_rate_supported(device, sr) => (sr, true),
        // 设备不支持或读不到采样率：保持当前流，重采样降级
        _ => (*current_stream_sr, false),
    };

    // —— 采样率变化时重建流 ——
    if target_sr != *current_stream_sr {
        // 先让解码线程停止解码（Stop），再短暂等待，确保它不在 push_all
        // 里阻塞——否则旧 consumer 被 take 后，push_all 会卡在满 ringbuf
        // 上（虽有重试上限兜底，但等待会让换曲延迟可感）。Stop 让
        // decoding=false，decoder_loop 下一轮就不再 push，能立即响应
        // 随后的 NewProducer。
        let _ = dec_cmd_tx.send(DecoderCmd::Stop);
        std::thread::sleep(Duration::from_millis(30));

        // drop 旧 Stream（释放旧 consumer，旧 ringbuf 随之回收）
        stream.take();
        match rebuild_stream(
            device,
            target_sr,
            device_channels as u16,
            sample_format,
            state,
        ) {
            Some((new_stream, new_prod)) => {
                // 新 producer 发给解码线程（替换旧的）
                let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(new_prod));
                *stream = Some(new_stream);
                *current_stream_sr = target_sr;
                state.set_stream_sample_rate(target_sr);
            }
            None => {
                // 重建失败：回退用初始采样率重建（降级）。极端情况。
                eprintln!("[音频] 流重建失败（{target_sr} Hz），回退降级");
                *stream = rebuild_stream(
                    device,
                    device_sample_rate,
                    device_channels as u16,
                    sample_format,
                    state,
                )
                .map(|(s, p)| {
                    let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(p));
                    s
                });
                *current_stream_sr = device_sample_rate;
                state.set_stream_sample_rate(device_sample_rate);
            }
        }
        // 新流默认暂停，调用方的 set_playing(true) + play() 会启动它
        if let Some(s) = stream.as_ref() {
            let _ = s.pause();
        }
    }

    // —— bitstream 标志按最终流采样率重算 ——
    // 回退场景下 current_stream_sr 已变为初始值 ≠ 目标，实际是降级播放，
    // 标志必须为 false，避免 UI 误亮技术参数行。
    let final_bitstream = want_bitstream && *current_stream_sr == target_sr;
    state.set_bitstream(final_bitstream);
}

pub(super) fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    mut consumer: HeapCons<f32>,
    state: Arc<SharedState>,
    device_sample_rate: u32,
    device_channels: usize,
) -> Option<Stream> {
    if sample_format != SampleFormat::F32 {
        eprintln!("[音频] 当前设备样本格式 {sample_format:?} 非 f32，暂不支持");
        return None;
    }

    // 回调闭包维护上次见到的 flush 世代，用于检测换曲/seek 信号。
    // FnMut 允许闭包持有可变状态。
    let mut last_flush = 0u64;
    // 累计窗口内的 L/R 峰值：每累计 ~10ms 样本就写一次 SharedState。
    // 10ms @ 48kHz ≈ 480 样本/通道；用 `n` 而非 `Instant` 避免 syscall。
    // 单调计数在回调节奏不规则时（如设备欠载）也稳定。
    const LEVEL_WINDOW_FRAMES: u64 = 480; // ~10ms @ 48kHz
    let mut window_frames: u64 = 0;
    let mut window_peak_l: f32 = 0.0;
    let mut window_peak_r: f32 = 0.0;
    // 频谱用的环形样本缓冲（每通道 FFT_SIZE 个，循环覆盖）。
    // 频谱在窗口结束时跑——直接复用上面累计的"近 FFT_SIZE 帧"。
    // 用 FFT_SIZE_FOR_BUF 而不是硬编码 4096，避免将来再调 FFT 大小
    // 时这条注释又变成 stale。
    let mut l_buf: [f32; spectrum::FFT_SIZE_FOR_BUF] = [0.0; spectrum::FFT_SIZE_FOR_BUF];
    let mut r_buf: [f32; spectrum::FFT_SIZE_FOR_BUF] = [0.0; spectrum::FFT_SIZE_FOR_BUF];
    let mut buf_idx: usize = 0;
    // rustfft 计划器（planner 内部缓存 FFT 计划，复用零开销；每次新建
    // 计划会有几 ms 开销，所以放外层 FnMut 捕获里）
    let mut fft_planner = rustfft::FftPlanner::<f32>::new();
    // FFT 输入 scratch buffer：compute_spectrum_bands 需要一个长度 = FFT_SIZE
    // 的复数缓冲用于"加 Hann 窗后样本 → R2C FFT 输入"。在实时回调里**绝对不能**
    // 堆分配（堆分配可能触发 GC/锁/缺页中断，破坏实时约束导致卡顿/丢帧）。
    // 因此在 FnMut 闭包状态里预分配 L/R 两份定长数组（约 64 KB），回调内只
    // 索引写入——零堆分配。
    // `Complex<f32>` 在 num-complex 中 derive Copy（确认），所以可以用 `[expr; N]`。
    use rustfft::num_complex::Complex;
    let mut fft_buf_l: [Complex<f32>; spectrum::FFT_SIZE_FOR_BUF] =
        [Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF];
    let mut fft_buf_r: [Complex<f32>; spectrum::FFT_SIZE_FOR_BUF] =
        [Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF];
    // rustfft plan 内部 scratch：radix-n / mixed-radix 算法可能需要非零长度的
    // 额外临时缓冲；rustfft 6.x 的 `process` 默认实现每次 `vec![..]` 分配，
    // 在实时线程上**必须**改用 `process_with_scratch` 并提供这个外层 scratch，
    // 否则仍然堆分配。预留 FFT_SIZE 长度对所有合法 FFT 大小都足够。
    let mut fft_plan_scratch_l: [Complex<f32>; spectrum::FFT_SIZE_FOR_BUF] =
        [Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF];
    let mut fft_plan_scratch_r: [Complex<f32>; spectrum::FFT_SIZE_FOR_BUF] =
        [Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF];
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                // —— 冲刷检测：世代变化说明换曲/seek，丢弃旧缓冲 ——
                let cur_epoch = state.flush_epoch();
                if cur_epoch != last_flush {
                    consumer.clear();
                    last_flush = cur_epoch;
                    // 切曲后窗口重置，避免上一曲的峰值/频谱污染新曲
                    window_frames = 0;
                    window_peak_l = 0.0;
                    window_peak_r = 0.0;
                    // 清空环形缓冲（静音/全零频谱），避免旧数据影响首窗
                    l_buf.fill(0.0);
                    r_buf.fill(0.0);
                    buf_idx = 0;
                }

                // 从 ringbuf 取样本（非阻塞）
                let n = consumer.pop_slice(data);
                // 应用音量 + ReplayGain 标准化增益（叠加）
                let vol = state.volume();
                let rg = state.replay_gain();
                for s in data[..n].iter_mut() {
                    *s *= vol * rg;
                }
                // 不足部分静音
                for s in data[n..].iter_mut() {
                    *s = 0.0;
                }
                // 累加进度（帧数 = 样本数 / 通道数）
                let frames = (n / device_channels.max(1)) as u64;
                state.add_frames(frames);

                // —— 累计 L/R peak + 频谱环形缓冲 ——
                // 同一遍循环：算 peak 同时把样本存到环形 buffer（详见
                // accumulate_spectrum）。buf_idx 由函数内部推进。
                accumulate_spectrum(
                    &data[..n],
                    device_channels,
                    &mut buf_idx,
                    &mut l_buf,
                    &mut r_buf,
                    &mut window_peak_l,
                    &mut window_peak_r,
                );

                window_frames += frames;
                if window_frames >= LEVEL_WINDOW_FRAMES {
                    // 先写 level
                    state.set_level_lr(window_peak_l.min(1.0), window_peak_r.min(1.0));
                    // 跑 FFT：环形 buffer 需重排为"oldest..newest"顺序
                    // 简单做法：从 buf_idx 开始依次取 FFT_SIZE 个样本
                    //
                    // l_sorted/r_sorted：栈上定长数组，每次窗口结束填一次后
                    // 立即被 compute_spectrum_bands 消费（仅作为不可变借用），
                    // 不产生堆分配。
                    let mut l_sorted = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
                    let mut r_sorted = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
                    for i in 0..spectrum::FFT_SIZE_FOR_BUF {
                        l_sorted[i] = l_buf[(buf_idx + i) % spectrum::FFT_SIZE_FOR_BUF];
                        r_sorted[i] = r_buf[(buf_idx + i) % spectrum::FFT_SIZE_FOR_BUF];
                    }
                    // 调 compute_spectrum_bands：传外层 FnMut 状态里的复数
                    // scratch buffer（&mut 转 &mut slice）——函数内部不再
                    // Vec::with_capacity/collect，零堆分配。L/R 两个 scratch
                    // 各自独立：两次调用顺序进行（不嵌套），但独立所有让
                    // 闭包借用规则最简单。
                    //
                    // 注意：必须同时传 `fft_plan_scratch_*`——rustfft 6.x 的
                    // `process` 默认 vec! 分配；改用 `process_with_scratch`
                    // 后 plan 内部 scratch 由我们提供，否则仍会在回调里堆分配。
                    // L/R 频段结果：闭包内局部栈数组（零堆分配），
                    // 直接由 compute_spectrum_bands 赋值（无预初始化，
                    // 满足 clippy unused-assignments）
                    let bands_l = spectrum::compute_spectrum_bands(
                        &l_sorted,
                        device_sample_rate,
                        &mut fft_planner,
                        &mut fft_buf_l,
                        &mut fft_plan_scratch_l,
                    );
                    let bands_r = spectrum::compute_spectrum_bands(
                        &r_sorted,
                        device_sample_rate,
                        &mut fft_planner,
                        &mut fft_buf_r,
                        &mut fft_plan_scratch_r,
                    );
                    state.set_spectrum_lr(&bands_l, &bands_r);

                    window_frames = 0;
                    window_peak_l = 0.0;
                    window_peak_r = 0.0;
                }
            },
            |err| eprintln!("[音频] 流错误：{err:?}"),
            None,
        )
        .ok()?;

    Some(stream)
}
