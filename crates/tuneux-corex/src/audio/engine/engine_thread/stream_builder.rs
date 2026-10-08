//! 音频流构建：cpal 流创建、重建、直通（含实时回调）。
//!
//! 从 engine_thread.rs 拆分。本模块是**音频侧**辅助：build_stream 的回调闭包
//! 运行在 cpal 回调线程，有零堆分配、无锁的硬实时约束——回调管线封装在
//! [`CallbackCtx`] 中，三种样本格式（F32 / I16 / U16）共用同一条 f32 内部
//! 管线，仅在出口做一次格式转换（转换缓冲在闭包创建时预分配）。
//!
//! - sample_rate_supported：设备是否支持某采样率
//! - rebuild_stream：按设备重建输出流（热切换/回退用；成功后同步
//!   SharedState 的通道数与格式代码）
//! - switch_stream_for_playback：切到新文件时重建流（采样率直通；通道数与
//!   格式读 SharedState 当前值，保持同流一致）
//! - build_stream：创建 cpal 输出流（核心回调）

use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use crossbeam_channel::Sender;
use ringbuf::{traits::*, HeapCons, HeapProd, HeapRb};

use super::super::compressor::CompressorEffect;
use super::super::equalizer::EqEffect;
use super::super::filter::FilterEffect;
use super::super::playback_medium::MediumEffects;
use super::super::spectrum;
use super::super::{DecoderCmd, SharedState};
use super::sample_utils::accumulate_spectrum;

pub(super) const RING_CAPACITY: usize = 48000 * 2 * 2;

/// 非 F32 格式转换缓冲上限（样本数）。cpal 默认缓冲通常 ≤ 2048 帧，
/// 8 通道也仅 16384 样本；65536 留足余量，超限即本回调静音（绝不
/// 在回调内分配）。
const CONVERT_SCRATCH_CAP: usize = 65536;

/// 样本格式 → 代码（SharedState 存储用；0 同时表示「未设置」，
/// 读取方回退 F32 语义与之一致）。
pub(super) fn fmt_code(f: SampleFormat) -> u8 {
    match f {
        SampleFormat::F32 => 0,
        SampleFormat::I16 => 1,
        SampleFormat::U16 => 2,
        _ => 0,
    }
}

/// 代码 → 样本格式；未知代码回退 F32。
pub(super) fn fmt_from_code(code: u8) -> SampleFormat {
    match code {
        1 => SampleFormat::I16,
        2 => SampleFormat::U16,
        _ => SampleFormat::F32,
    }
}

/// f32（已限幅 [-1,1]）→ i16。
fn f32_to_i16(v: f32) -> i16 {
    (v * 32767.0).round() as i16
}

/// f32（已限幅 [-1,1]）→ u16（中点 32768 = 静音）。
fn f32_to_u16(v: f32) -> u16 {
    ((v + 1.0) * 0.5 * 65535.0).round() as u16
}

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

/// 重建 ringbuf + Stream（用于采样率切换 / 设备热切换）。
///
/// 返回 (新 Stream, 新 Producer)。新 Producer 应通过 DecoderCmd::NewProducer
/// 发给解码线程；新 Stream 由调用方持有。
///
/// 成功后把目标通道数与格式代码写入 SharedState（解码线程的通道适配、
/// 重采样器构建与回调的 DSP 通道数自此同源）；失败不写（状态保持旧流值）。
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
    state.set_stream_channels(channels as u32);
    state.set_stream_format_code(fmt_code(sample_format));
    Some((stream, producer))
}

/// 播放前的「原生采样率直通 + 流重建（含失败回退）」。
///
/// `AudioCmd::Play` / `PlayResume` / `PlayRange` 共用同一套直通与重建逻辑。
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
/// 通道数与样本格式读 SharedState 当前值（同设备换曲不变；设备热切换
/// 路径由 rebuild_stream 写入新值），不再用 spawn 捕获值。
///
/// 注意 bitstream 按最终流采样率重算：若目标 44.1kHz 重建失败回退到
/// 48kHz，实际已降级，标志必须为 false。
#[allow(clippy::too_many_arguments)]
pub(super) fn switch_stream_for_playback(
    path: &std::path::Path,
    device: &cpal::Device,
    device_sample_rate: u32,
    stream: &mut Option<Stream>,
    current_stream_sr: &mut u32,
    dec_cmd_tx: &Sender<DecoderCmd>,
    state: &Arc<SharedState>,
) {
    // 同流通道数 / 格式：读 SharedState（spawn 与重建时写入），未设置回退保守值
    let channels = state.stream_channels().max(1) as u16;
    let sample_format = fmt_from_code(state.stream_format_code());

    // —— 直通 ——
    let (target_sr, want_bitstream) = match crate::audio::codec::decoder::probe_sample_rate(path) {
        Some(sr) if sample_rate_supported(device, sr) => (sr, true),
        // 设备不支持或读不到采样率：保持当前流，重采样降级
        _ => (*current_stream_sr, false),
    };

    // —— 采样率变化或流丢失时重建 ——
    // stream 为 None（上次重建彻底失败）时即使采样率相同也必须重建，
    // 否则后续同采样率曲目将永久静音（只能靠设备名变化或重启恢复）。
    if target_sr != *current_stream_sr || stream.is_none() {
        // 先让解码线程停止解码（Stop），再短暂等待，确保它不在 push_all
        // 里阻塞——否则旧 consumer 被 take 后，push_all 会卡在满 ringbuf
        // 上（虽有重试上限兜底，但等待会让换曲延迟可感）。Stop 让
        // decoding=false，decoder_loop 下一轮就不再 push，能立即响应
        // 随后的 NewProducer。
        let _ = dec_cmd_tx.send(DecoderCmd::Stop);
        std::thread::sleep(Duration::from_millis(30));

        // drop 旧 Stream（释放旧 consumer，旧 ringbuf 随之回收）
        stream.take();
        match rebuild_stream(device, target_sr, channels, sample_format, state) {
            Some((new_stream, new_prod)) => {
                // 新 producer 发给解码线程（替换旧的）
                let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(new_prod));
                *stream = Some(new_stream);
                *current_stream_sr = target_sr;
                state.set_stream_sample_rate(target_sr);
            }
            None => {
                // 重建失败：回退用初始采样率重建（降级）。极端情况，静默（raw 屏幕）。
                *stream =
                    rebuild_stream(device, device_sample_rate, channels, sample_format, state).map(
                        |(s, p)| {
                            let _ = dec_cmd_tx.send(DecoderCmd::NewProducer(p));
                            s
                        },
                    );
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

/// 实时回调管线状态（FnMut 闭包捕获的全部可变状态）。
///
/// 回调在 cpal 音频线程运行：零堆分配、无锁。所有缓冲（FFT scratch、
/// 效果器池、格式转换缓冲）在创建时预分配，回调内只索引写入。
struct CallbackCtx {
    consumer: HeapCons<f32>,
    state: Arc<SharedState>,
    /// 目标流通道数（闭包创建时捕获；流重建即新闭包，天然跟随新设备）。
    stream_channels: usize,
    /// 目标流采样率（同上）。
    stream_sample_rate: u32,
    last_flush: u64,
    window_frames: u64,
    window_peak_l: f32,
    window_peak_r: f32,
    l_buf: [f32; spectrum::FFT_SIZE_FOR_BUF],
    r_buf: [f32; spectrum::FFT_SIZE_FOR_BUF],
    buf_idx: usize,
    fft_planner: rustfft::FftPlanner<f32>,
    // 四个复数缓冲堆分配（Box）：合计约 256KB，若随结构体栈上构造
    // 会把音频线程（默认 2MB 栈）的 build_stream 帧顶爆（实锤过栈溢出）。
    // 流构建期一次性分配，回调内零分配的红线不变。
    fft_buf_l: Box<[rustfft::num_complex::Complex<f32>]>,
    fft_buf_r: Box<[rustfft::num_complex::Complex<f32>]>,
    fft_plan_scratch_l: Box<[rustfft::num_complex::Complex<f32>]>,
    fft_plan_scratch_r: Box<[rustfft::num_complex::Complex<f32>]>,
    medium_effects: MediumEffects,
    eq_effects: [EqEffect; super::super::equalizer::EQ_SLOTS],
    comp_effects: [CompressorEffect; super::super::compressor::COMP_SLOTS],
    filter_effects: [FilterEffect; super::super::filter::FILTER_SLOTS],
    /// 非 F32 格式的出口转换缓冲（创建时预分配，回调内零分配）。
    scratch: Vec<f32>,
    /// 当前平滑增益：一阶斜坡向目标音量靠拢。flush（换曲/seek）后从 0
    /// 起爬升，消除硬切的咔哒；音量滑动时消除 zipper noise。
    gain_smooth: f32,
    /// 一阶斜坡系数（按采样率计算：1 - exp(-1 / (sr × τ))，τ ≈ 5ms）。
    /// 创建时一次性算好，回调内只乘。
    gain_coef: f32,
}

impl CallbackCtx {
    fn new(
        consumer: HeapCons<f32>,
        state: Arc<SharedState>,
        stream_sample_rate: u32,
        stream_channels: usize,
    ) -> Self {
        use rustfft::num_complex::Complex;
        Self {
            consumer,
            state,
            stream_channels,
            stream_sample_rate,
            last_flush: 0,
            window_frames: 0,
            window_peak_l: 0.0,
            window_peak_r: 0.0,
            l_buf: [0.0; spectrum::FFT_SIZE_FOR_BUF],
            r_buf: [0.0; spectrum::FFT_SIZE_FOR_BUF],
            buf_idx: 0,
            fft_planner: {
                // 预热：plan 惰性构建会在首次 cpal 回调内分配（违反零分配
                // 纪律）；此处构建期一次预热，回调内只命中缓存。
                let mut p = rustfft::FftPlanner::<f32>::new();
                let _ = p.plan_fft_forward(spectrum::FFT_SIZE);
                p
            },
            fft_buf_l: vec![Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF].into_boxed_slice(),
            fft_buf_r: vec![Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF].into_boxed_slice(),
            fft_plan_scratch_l: vec![Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF]
                .into_boxed_slice(),
            fft_plan_scratch_r: vec![Complex::new(0.0, 0.0); spectrum::FFT_SIZE_FOR_BUF]
                .into_boxed_slice(),
            medium_effects: MediumEffects::default(),
            eq_effects: std::array::from_fn(|_| EqEffect::default()),
            filter_effects: std::array::from_fn(|_| FilterEffect::default()),
            comp_effects: std::array::from_fn(|_| CompressorEffect::default()),
            scratch: vec![0.0; CONVERT_SCRATCH_CAP],
            // 增益斜坡：从 0 起步（首回调淡入）；τ=5ms 在 48kHz 下约
            // 240 样本收敛到 95%，用户无感且彻底消除 zipper / 咔哒。
            gain_smooth: 0.0,
            gain_coef: {
                let sr = stream_sample_rate.max(1) as f32;
                let tau = 0.005; // 5ms 时间常数
                1.0 - (-1.0 / (sr * tau)).exp()
            },
        }
    }

    /// f32 管线主体：取样本 → ReplayGain → 介质 DSP → EQ → 压缩器 →
    /// 音量限幅 → 静音补尾 → 进度累加 → 电平/频谱/波形累计。
    fn process_f32(&mut self, data: &mut [f32]) {
        let stream_channels = self.stream_channels;
        let stream_sample_rate = self.stream_sample_rate;

        // —— 冲刷检测：世代变化说明换曲/seek，丢弃旧缓冲 ——
        let cur_epoch = self.state.flush_epoch();
        if cur_epoch != self.last_flush {
            self.consumer.clear();
            self.last_flush = cur_epoch;
            // 切曲后窗口重置，避免上一曲的峰值/频谱污染新曲
            self.window_frames = 0;
            self.window_peak_l = 0.0;
            self.window_peak_r = 0.0;
            // 清空环形缓冲（静音/全零频谱），避免旧数据影响首窗
            self.l_buf.fill(0.0);
            self.r_buf.fill(0.0);
            self.buf_idx = 0;
            // 增益从零起爬升（flush 淡入）：旧缓冲被 clear 后新数据
            // 立刻进 DAC，若增益不归零会产生换曲/seek 的咔哒。
            self.gain_smooth = 0.0;
            // 重置介质效果器状态，避免上一曲延迟线 / 相位残留带进新曲。
            self.medium_effects.reset();
            for e in &mut self.eq_effects {
                e.reset();
            }
            for c in &mut self.comp_effects {
                c.reset();
            }
            for f in &mut self.filter_effects {
                f.reset();
            }
        }

        // 从 ringbuf 取样本（非阻塞）
        let n = self.consumer.pop_slice(data);
        // 先应用 ReplayGain 标准化增益（介质 DSP 之前，让介质听感与监听音量无关）。
        // replay_gain() 在开关关闭时恒返回 1.0（引擎内部判断）。
        let rg = self.state.replay_gain();
        for s in data[..n].iter_mut() {
            *s *= rg;
        }
        // 播放介质风格 DSP：ReplayGain 之后、音量之前、频谱累计之前。
        // 介质底噪 / 噼啪是加性信号、磁饱和是电平相关非线性，必须放在音量之前，
        // 否则静音时噪声仍输出、且调音量会改变介质音色。
        self.medium_effects.apply(
            self.state.medium(),
            &mut data[..n],
            stream_channels.max(1),
            stream_sample_rate,
        );
        // 均衡器 DSP：介质之后、音量之前（EQ 是音色修饰，与监听音量解耦）。
        // 按槽位升序遍历，空槽（未分配）跳过。
        for (i, e) in self.eq_effects.iter_mut().enumerate() {
            if self.state.eq_slot_used(i) {
                e.process(
                    &mut data[..n],
                    stream_channels.max(1),
                    stream_sample_rate,
                    self.state.eq_slot(i),
                );
            }
        }
        // 压缩器 DSP：均衡器之后、音量之前。
        for (i, c) in self.comp_effects.iter_mut().enumerate() {
            if self.state.comp_slot_used(i) {
                c.process(
                    &mut data[..n],
                    stream_channels.max(1),
                    stream_sample_rate,
                    self.state.comp_slot(i),
                );
            }
        }
        // 滤波器 DSP：压缩器之后、音量之前。
        for (i, f) in self.filter_effects.iter_mut().enumerate() {
            if self.state.filter_slot_used(i) {
                f.process(
                    &mut data[..n],
                    stream_channels.max(1),
                    stream_sample_rate,
                    self.state.filter_slot(i),
                );
            }
        }

        // 再应用音量（介质 DSP 之后），并做输出限幅：ReplayGain 正增益 +
        // EQ/压缩器补偿叠加可能使 |样本|>1，到 DAC 即硬削波（爆音）。
        // clamp 到 [-1, 1] 是最后一道防线（rg.rs 的 apply_gain_db 限幅
        // 未接入播放路径，此处回调内就地兜底）。
        let vol = self.state.volume();
        let coef = self.gain_coef;
        let mut gs = self.gain_smooth;
        for s in data[..n].iter_mut() {
            // 一阶斜坡向目标靠拢：消除音量阶跃的 zipper 与 flush 后的咔哒。
            gs += (vol - gs) * coef;
            *s = (*s * gs).clamp(-1.0, 1.0);
        }
        self.gain_smooth = gs;
        // 不足部分静音
        for s in data[n..].iter_mut() {
            *s = 0.0;
        }
        // 累加进度（帧数 = 样本数 / 通道数）
        let frames = (n / stream_channels.max(1)) as u64;
        self.state.add_frames(frames);

        // —— 累计 L/R peak + 频谱环形缓冲 ——
        // 同一遍循环：算 peak 同时把样本存到环形 buffer（详见
        // accumulate_spectrum）。buf_idx 由函数内部推进。
        accumulate_spectrum(
            &data[..n],
            stream_channels,
            &mut self.buf_idx,
            &mut self.l_buf,
            &mut self.r_buf,
            &mut self.window_peak_l,
            &mut self.window_peak_r,
        );

        self.window_frames += frames;
        if self.window_frames >= LEVEL_WINDOW_FRAMES {
            // 先写 level
            self.state
                .set_level_lr(self.window_peak_l.min(1.0), self.window_peak_r.min(1.0));
            // 跑 FFT：环形 buffer 需重排为"oldest..newest"顺序。
            // l_sorted/r_sorted：栈上定长数组，每次窗口结束填一次后
            // 立即被 compute_spectrum_bands 消费（仅作为不可变借用），
            // 不产生堆分配。
            let mut l_sorted = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
            let mut r_sorted = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
            for i in 0..spectrum::FFT_SIZE_FOR_BUF {
                l_sorted[i] = self.l_buf[(self.buf_idx + i) % spectrum::FFT_SIZE_FOR_BUF];
                r_sorted[i] = self.r_buf[(self.buf_idx + i) % spectrum::FFT_SIZE_FOR_BUF];
            }
            // 调 compute_spectrum_bands：传外层状态里的复数 scratch buffer
            //（&mut 转 &mut slice）——函数内部不再 Vec::with_capacity/collect，
            // 零堆分配。L/R 两个 scratch 各自独立：两次调用顺序进行（不嵌套）。
            //
            // 注意：必须同时传 fft_plan_scratch_*——rustfft 6.x 的 `process`
            // 默认 vec! 分配；改用 `process_with_scratch` 后 plan 内部 scratch
            // 由我们提供，否则仍会在回调里堆分配。
            let bands_l = spectrum::compute_spectrum_bands(
                &l_sorted,
                stream_sample_rate,
                &mut self.fft_planner,
                &mut self.fft_buf_l,
                &mut self.fft_plan_scratch_l,
            );
            let bands_r = spectrum::compute_spectrum_bands(
                &r_sorted,
                stream_sample_rate,
                &mut self.fft_planner,
                &mut self.fft_buf_r,
                &mut self.fft_plan_scratch_r,
            );
            self.state.set_spectrum_lr(&bands_l, &bands_r);

            // 波形：从 FFT 环形缓冲（≈85ms 时窗）按步长降采样到 WAVEFORM_LEN，
            // 覆盖写入。示波器显示最近一小段时域波形（每 ~10ms 刷新一次）。
            let step = spectrum::FFT_SIZE_FOR_BUF / spectrum::WAVEFORM_LEN;
            let mut wf_l = [0.0f32; spectrum::WAVEFORM_LEN];
            let mut wf_r = [0.0f32; spectrum::WAVEFORM_LEN];
            for i in 0..spectrum::WAVEFORM_LEN {
                wf_l[i] = l_sorted[i * step];
                wf_r[i] = r_sorted[i * step];
            }
            self.state.set_waveform_lr(&wf_l, &wf_r);

            self.window_frames = 0;
            self.window_peak_l = 0.0;
            self.window_peak_r = 0.0;
        }
    }

    /// 非 F32 设备格式出口：f32 管线跑在预分配 scratch 上，再逐样本转换。
    ///
    /// 缓冲超预设上限（极罕见）时本回调静音——绝不在回调内分配。
    fn process_into<T: cpal::Sample>(&mut self, data: &mut [T], conv: fn(f32) -> T) {
        if data.len() > self.scratch.len() {
            for s in data.iter_mut() {
                *s = T::EQUILIBRIUM;
            }
            return;
        }
        // scratch 临时移出（mem::take 是 move，零分配），避开与
        // process_f32 对 self 的双重可变借用；用完归还原 Vec。
        let mut tmp = std::mem::take(&mut self.scratch);
        {
            let buf = &mut tmp[..data.len()];
            self.process_f32(buf);
        }
        let n = data.len();
        for (d, v) in data.iter_mut().zip(tmp[..n].iter()) {
            *d = conv(*v);
        }
        self.scratch = tmp;
    }
}

/// 累计窗口帧数（~10ms @ 48kHz ≈ 480 样本/通道；用帧数而非 Instant
/// 避免回调内 syscall，节奏不规则时也稳定）。
const LEVEL_WINDOW_FRAMES: u64 = 480;

/// 建 cpal 输出流并接线回调。
///
/// `stream_sample_rate` / `stream_channels` 是**目标流**的参数，不是"设备默认值"：
/// 首次由 `spawn_threads` 传设备默认值，重建时由 `rebuild_stream` 按其调用方
/// 的目标值传入。回调内频谱 FFT、均衡器、压缩器与电平帧数换算全按这两个值计算。
///
/// 样本格式支持 F32 / I16 / U16（覆盖桌面设备默认格式绝大多数）：内部管线
/// 恒为 f32，非 F32 格式在回调出口转换。其它格式返回 None，由上层错误通道提示。
pub(super) fn build_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    consumer: HeapCons<f32>,
    state: Arc<SharedState>,
    stream_sample_rate: u32,
    stream_channels: usize,
) -> Option<Stream> {
    let ctx = CallbackCtx::new(consumer, state, stream_sample_rate, stream_channels);
    // 流运行期错误：静默。设备断开会由音频线程的 2s 轮询检测并走
    // 重建/报错通道（last_error），这里打印只会弄脏 raw-mode 屏幕。
    let stream = match sample_format {
        SampleFormat::F32 => {
            let mut ctx = ctx;
            device.build_output_stream::<f32, _, _>(
                config,
                move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| ctx.process_f32(data),
                |_| {},
                None,
            )
        }
        SampleFormat::I16 => {
            let mut ctx = ctx;
            device.build_output_stream::<i16, _, _>(
                config,
                move |data: &mut [i16], _info: &cpal::OutputCallbackInfo| {
                    ctx.process_into(data, f32_to_i16);
                },
                |_| {},
                None,
            )
        }
        SampleFormat::U16 => {
            let mut ctx = ctx;
            device.build_output_stream::<u16, _, _>(
                config,
                move |data: &mut [u16], _info: &cpal::OutputCallbackInfo| {
                    ctx.process_into(data, f32_to_u16);
                },
                |_| {},
                None,
            )
        }
        _ => return None,
    }
    .ok()?;

    Some(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 格式代码往返：code → format → code 恒等；未知代码回退 F32。
    #[test]
    fn format_code_roundtrip() {
        assert_eq!(
            fmt_from_code(fmt_code(SampleFormat::F32)),
            SampleFormat::F32
        );
        assert_eq!(
            fmt_from_code(fmt_code(SampleFormat::I16)),
            SampleFormat::I16
        );
        assert_eq!(
            fmt_from_code(fmt_code(SampleFormat::U16)),
            SampleFormat::U16
        );
        assert_eq!(fmt_from_code(200), SampleFormat::F32);
    }

    /// 出口转换精度：满幅 / 半幅 / 静音三点的 i16 / u16 映射正确，
    /// 且转换单调（保序）。
    #[test]
    fn sample_conversion_endpoints() {
        assert_eq!(f32_to_i16(1.0), 32767);
        assert_eq!(f32_to_i16(-1.0), -32767);
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_u16(1.0), 65535);
        assert_eq!(f32_to_u16(-1.0), 0);
        assert_eq!(f32_to_u16(0.0), 32768);
        // 单调性：采样网格上保序
        let mut prev_i = i16::MIN;
        let mut prev_u = 0u16;
        for k in 0..=256 {
            let v = -1.0 + 2.0 * (k as f32) / 256.0;
            let i = f32_to_i16(v);
            let u = f32_to_u16(v);
            assert!(i >= prev_i, "i16 单调性破坏于 {v}");
            assert!(u >= prev_u, "u16 单调性破坏于 {v}");
            prev_i = i;
            prev_u = u;
        }
    }

    /// 回调管线：ring 推已知样本 → process_f32 应用音量与静音补尾。
    /// B1 增益斜坡：gain 从 0 起步（flush 淡入语义），需先推预热样本
    /// 让斜坡收敛到目标音量（τ=5ms @48kHz，约 1500 样本达 99.8%），
    /// 再推验证样本检查收敛区的精确乘法。
    #[test]
    fn callback_pipeline_applies_volume_and_silence_tail() {
        let ring = HeapRb::<f32>::new(4096);
        let (mut prod, cons) = ring.split();
        let state = Arc::new(SharedState::default());
        state.set_volume(0.5);
        // 预热：推 2048 个样本让增益收敛
        for _ in 0..512 {
            let _ = prod.push_slice(&[1.0, -1.0, 0.5, 1.0]);
        }
        // 验证样本
        let _ = prod.push_slice(&[1.0, -1.0, 0.5]);
        let mut ctx = CallbackCtx::new(cons, state, 48000, 2);
        let mut out = [9.0f32; 8];
        // 消费预热样本（256 次 × 8 = 2048）
        for _ in 0..256 {
            ctx.process_f32(&mut out);
        }
        // 此调用取到验证样本：3 个 × 收敛后音量 0.5 + 5 个静音
        ctx.process_f32(&mut out);
        assert!((out[0] - 0.5).abs() < 0.01, "out[0]={}", out[0]);
        assert!((out[1] + 0.5).abs() < 0.01, "out[1]={}", out[1]);
        assert!((out[2] - 0.25).abs() < 0.01, "out[2]={}", out[2]);
        assert!(out[3..].iter().all(|&s| s == 0.0));
    }

    /// 非 F32 出口：i16 转换路径与 f32 管线数值一致。
    /// B1 增益斜坡：两条管线吃相同输入、有相同平滑，验证「出口一致」
    /// 而非绝对值（指数斜坡不精确收敛到 1.0，绝对断言必炸）。
    #[test]
    fn callback_pipeline_converted_output_matches_f32() {
        let state = Arc::new(SharedState::default());
        state.set_volume(1.0);

        let ring_a = HeapRb::<f32>::new(4096);
        let (mut prod_a, cons_a) = ring_a.split();
        let mut ctx_a = CallbackCtx::new(cons_a, state.clone(), 48000, 2);
        let mut f32_out = [0.0f32; 4];

        let ring_b = HeapRb::<f32>::new(4096);
        let (mut prod_b, cons_b) = ring_b.split();
        let mut ctx_b = CallbackCtx::new(cons_b, state, 48000, 2);
        let mut i16_out = [0i16; 4];

        for _ in 0..600 {
            let _ = prod_a.push_slice(&[0.5, -0.25, 0.5, 0.5]);
            let _ = prod_b.push_slice(&[0.5, -0.25, 0.5, 0.5]);
        }
        for _ in 0..600 {
            ctx_a.process_f32(&mut f32_out);
            ctx_b.process_into(&mut i16_out, f32_to_i16);
            for i in 0..4 {
                assert_eq!(i16_out[i], f32_to_i16(f32_out[i]), "样本 {i} 不一致");
            }
        }
    }
}
