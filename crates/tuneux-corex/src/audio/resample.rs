//! # 重采样模块
//!
//! 当音频文件的采样率与输出设备实际使用的采样率不一致时，
//! 直接灌入会导致**变调变速**（如 44.1kHz 文件在 48kHz 设备上播放会变快变尖）。
//! 本模块用 rubato 做高质量重采样，把文件采样率转换为设备采样率。
//!
//! ## 何时重采样
//!
//! cpal 打开输出流时会指定一个采样率。理想情况下我们直接用文件本身的采样率
//! 打开设备（让硬件做 SRC），但部分设备不支持任意采样率，强制用文件采样率
//! 可能导致打开设备失败。因此策略为：
//!
//! 1. 优先用 cpal 设备的默认采样率打开流（兼容性最好）；
//! 2. 若文件采样率 ≠ 设备采样率，则在软件层用 rubato 重采样到设备采样率；
//! 3. 若二者相同，则直通，无重采样开销。
//!
//! rubato 的 `FftFixedIn` 是固定输入块大小的重采样器，适合我们的流式解码
//! 模型（每次喂入解码产出的若干帧）。它会按需累积/输出，可能输入 N 帧不
//! 产生正好 N 帧输出——这是正常的，调用方需循环。
//!
//! ## sub_chunks 为什么不是 1
//!
//! rubato 内部用 FFT 重采样，`fft_size_in = ceil(chunk/sub_chunks / min_chunk) * min_chunk`，
//! 其中 `min_chunk = from_sr / gcd(from_sr, to_sr)`。
//! 当 `sub_chunks=1` 且 chunk=1024、44.1k→48k：
//!   `fft_size_in = ceil(1024/147) * 147 = 7 * 147 = 1029`
//! `fft_size_in > chunk_frames` 意味着 `process(1024)` 喂入后 `floor(1024/1029) = 0`，
//! 第一次永远 0 输出（音频线程会卡顿）。修复：`sub_chunks=4` 让
//! `fft_size_in = ceil(256/147) * 147 = 2 * 147 = 294`，远小于 1024，
//! 首包即可输出 3 × 320 = 960 帧。
//!
//! ## 为什么需要内部缓冲
//!
//! FftFixedIn::process 严格校验：每通道输入必须**正好 chunk_frames** 帧。
//! 短输入会触发 `Insufficient buffer size` 错误。
//! 解码器给我们的输入是变长的（MP3 一包 576 帧，FLAC 一包 16384 帧），
//! 必须内部缓冲到 `chunk_frames` 再调 process。
//! **绝不补零**——若用 padding 把短包补成 chunk_frames，rubato 会把补的零
//! 当真实音频处理，产生滤波器衰减尾（30s MP3 经 44.1k→48k 会多出十几秒），
//! 端到端测试收不到 EOF。正确做法是累积缓冲，padding 只在 flush() 末尾出现一次。
//!
//! 注意：rubato 只处理单通道。多通道时对每个通道分别重采样，本模块封装了
//! 这个拆分/合并逻辑。

use rubato::{FftFixedIn, Resampler};

/// 重采样错误。rubato 的错误类型较啰嗦，这里简化为字符串信息。
#[derive(Debug)]
pub enum ResampleError {
    /// 重采样器构造或处理失败（参数非法、内部错误等）。
    Rubato(String),
}

impl std::fmt::Display for ResampleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResampleError::Rubato(msg) => write!(f, "重采样错误：{msg}"),
        }
    }
}

impl std::error::Error for ResampleError {}

/// FftFixedIn 的子块数。
///
/// 固定为 4 是因为 chunk_frames=1024 是引擎侧约定，要保证对所有常见采样率
/// 组合都满足 `fft_size_in <= chunk_frames`（44.1k→48k 是最坏情况）。
/// 见模块文档「sub_chunks 为什么不是 1」。
const SUB_CHUNKS: usize = 4;

/// 重采样器：把文件采样率的 PCM（f32）转为设备采样率。
///
/// 工作流程：
/// 1. 构造时指定 输入/输出采样率、通道数、每次处理的输入块大小（chunk_frames）；
/// 2. 调用 `process` 喂入交错 f32 样本（任意长度），得到重采样后的交错 f32 样本；
///    不足 chunk_frames 的部分会内部缓冲；
/// 3. 文件结束时调用 `flush` 取出残留在缓冲和重采样器内的样本（包括滤波器延迟）。
///
/// chunk_frames 选择：太小则每次调用开销高、可能无输出；太大则延迟高。
/// 取 1024（约 23ms @44.1kHz）是延迟与效率的常见折中。
pub struct Resample {
    /// rubato 重采样器。FftFixedIn = 固定输入帧数，FFT 加速。
    resampler: FftFixedIn<f32>,
    /// 通道数。
    channels: usize,
    /// 每次期望输入的帧数（chunk 大小）。
    chunk_frames: usize,
    /// 内部输入缓冲（交错）。累积不足 chunk_frames 的输入样本。
    /// 长度 ≤ chunk_frames × channels（永远不会留超过一个完整 chunk）。
    input_buffer: Vec<f32>,
}

impl Resample {
    /// 创建重采样器。
    ///
    /// 参数：
    /// - `from_sr`：源（文件）采样率；
    /// - `to_sr`：目标（设备）采样率；
    /// - `channels`：通道数；
    /// - `chunk_frames`：每次处理的输入帧数。
    ///
    /// 注：本函数固定使用 `sub_chunks=4`（见模块文档「sub_chunks 为什么不是 1」）。
    pub fn new(
        from_sr: u32,
        to_sr: u32,
        channels: usize,
        chunk_frames: usize,
    ) -> Result<Self, ResampleError> {
        let resampler = FftFixedIn::<f32>::new(
            from_sr as usize,
            to_sr as usize,
            chunk_frames,
            SUB_CHUNKS,
            channels,
        )
        .map_err(|e| ResampleError::Rubato(e.to_string()))?;

        Ok(Self {
            resampler,
            channels,
            chunk_frames,
            input_buffer: Vec::new(),
        })
    }

    /// 处理一批交错 f32 输入样本，返回重采样后的交错 f32 样本。
    ///
    /// **输入长度无关**：本函数对任意长度输入都安全：
    /// - 任意长度：附加到内部 `input_buffer`，然后按 `chunk_frames` 切片
    ///   循环调 `process`（每片都是满块，符合 FftFixedIn 契约）
    /// - 不足 `chunk_frames` 的部分留在缓冲里，等下次调用凑够再处理
    /// - 输入为空：直接返回空 Vec
    ///
    /// **为什么不补零**：对短输入补零，rubato 会把补的零当真实音频处理，
    /// 产生滤波器衰减尾（30s MP3 经 44.1k→48k 会多出十几秒）。改为内部
    /// 缓冲后，padding 只在 `flush()` 末尾出现一次。
    pub fn process(&mut self, interleaved: &[f32]) -> Result<Vec<f32>, ResampleError> {
        if interleaved.is_empty() {
            return Ok(Vec::new());
        }
        let total_new_frames = interleaved.len() / self.channels;
        if total_new_frames == 0 {
            return Ok(Vec::new());
        }

        // 1. 把新输入追加到内部缓冲
        self.input_buffer.extend_from_slice(interleaved);
        let buffered_frames = self.input_buffer.len() / self.channels;

        // 2. 计算能处理的完整 chunk 数
        let full_chunk_count = buffered_frames / self.chunk_frames;
        if full_chunk_count == 0 {
            // 缓冲还不够一个 chunk，留着下次
            return Ok(Vec::new());
        }
        let process_frames = full_chunk_count * self.chunk_frames;

        // 3. 循环每片，拆通道后调 resampler.process
        let mut output = Vec::new();
        for chunk_idx in 0..full_chunk_count {
            let start_frame = chunk_idx * self.chunk_frames;
            let end_frame = start_frame + self.chunk_frames;
            let start_sample = start_frame * self.channels;
            let end_sample = end_frame * self.channels;
            let chunk_interleaved = &self.input_buffer[start_sample..end_sample];

            // 拆通道：rubato 期望每通道一个独立 Vec
            let per_channel: Vec<Vec<f32>> = (0..self.channels)
                .map(|ch| {
                    (0..self.chunk_frames)
                        .map(|f| chunk_interleaved[f * self.channels + ch])
                        .collect()
                })
                .collect();
            let chunk_refs: Vec<&[f32]> = per_channel.iter().map(|v| v.as_slice()).collect();

            let outputs = self
                .resampler
                .process(&chunk_refs, None)
                .map_err(|e| ResampleError::Rubato(e.to_string()))?;

            // 交错合并
            interleave_channels(&outputs, self.channels, &mut output);
        }

        // 4. 保留 leftover（< chunk_frames），丢弃已处理的
        let leftover_frames = buffered_frames - process_frames;
        let leftover_samples = leftover_frames * self.channels;
        if leftover_samples > 0 {
            // 把 leftover 移到缓冲开头
            let total_samples = self.input_buffer.len();
            self.input_buffer
                .copy_within(total_samples - leftover_samples.., 0);
            self.input_buffer.truncate(leftover_samples);
        } else {
            self.input_buffer.clear();
        }

        Ok(output)
    }

    /// 取出重采样器内残留的样本（文件结束时调用）。
    ///
    /// 两步：
    /// 1. 把内部缓冲里残留的不足 chunk_frames 的部分补零到 chunk_frames，
    ///    再调一次 process()（补的零会被 rubato 当真实音频处理，但只是
    ///    文件最末尾的一小段，肉耳几乎听不到）；
    /// 2. 调 process_partial(None) 抽出 rubato 内部的滤波器延迟样本。
    ///
    /// 调用本函数后应丢弃该 `Resample`（或重新构造）——状态已不再适合处理
    /// 下一首歌。
    pub fn flush(&mut self) -> Result<Vec<f32>, ResampleError> {
        let mut output = Vec::new();

        // 1. 处理缓冲里的 leftover（补零到 chunk_frames）
        if !self.input_buffer.is_empty() {
            let buffered_frames = self.input_buffer.len() / self.channels;
            if buffered_frames > 0 && buffered_frames < self.chunk_frames {
                // 补零
                let needed_samples = self.chunk_frames * self.channels;
                self.input_buffer.resize(needed_samples, 0.0);

                // 拆通道
                let per_channel: Vec<Vec<f32>> = (0..self.channels)
                    .map(|ch| {
                        (0..self.chunk_frames)
                            .map(|f| self.input_buffer[f * self.channels + ch])
                            .collect()
                    })
                    .collect();
                let chunk_refs: Vec<&[f32]> = per_channel.iter().map(|v| v.as_slice()).collect();

                let outputs = self
                    .resampler
                    .process(&chunk_refs, None)
                    .map_err(|e| ResampleError::Rubato(e.to_string()))?;

                interleave_channels(&outputs, self.channels, &mut output);
            }
            self.input_buffer.clear();
        }

        // 2. 抽出 rubato 内部的滤波器延迟
        let drained = self
            .resampler
            .process_partial::<&[f32]>(None, None)
            .map_err(|e| ResampleError::Rubato(e.to_string()))?;
        interleave_channels(&drained, self.channels, &mut output);

        Ok(output)
    }
}

/// 把 rubato 输出的“每通道独立 Vec”交错合并为一个交错 f32 缓冲。
///
/// rubato 返回 `Vec<Vec<f32>>`（每通道一列），而音频引擎需要交错样本
/// `[L0, R0, L1, R1, ...]`。本函数按帧遍历、逐通道取值，写入 `output`。
/// 用迭代器而非下标循环（避免 needless_range_loop 警告）。
fn interleave_channels(outputs: &[Vec<f32>], channels: usize, output: &mut Vec<f32>) {
    let out_frames = outputs.first().map_or(0, |v| v.len());
    output.reserve(out_frames * channels);
    output.extend((0..out_frames).flat_map(|f| outputs.iter().take(channels).map(move |ch| ch[f])));
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// 用单频正弦波构造测试输入。
    /// sr：采样率，frames：帧数，freq：频率（Hz），channels：通道数
    fn sine(sr: u32, frames: usize, freq: f32, channels: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * channels);
        for f in 0..frames {
            let s = (2.0 * std::f32::consts::PI * freq * f as f32 / sr as f32).sin();
            for _ in 0..channels {
                out.push(s);
            }
        }
        out
    }

    /// 输入恰好 chunk_frames：满块直接处理，无 padding
    #[test]
    fn exact_chunk_input() {
        let mut rs = Resample::new(48000, 48000, 2, 1024).unwrap();
        let input = sine(48000, 1024, 440.0, 2);
        let out = rs.process(&input).unwrap();
        // 同采样率，sub=4：fft_in=fft_out=256，1024/256=4 → 4×256=1024 输出
        assert_eq!(out.len(), 1024 * 2, "1:1 比率，输出 1024 帧 × 2 通道");
    }

    /// 输入 < chunk_frames：缓冲累积，不产生输出
    #[test]
    fn short_input_buffers_until_full_chunk() {
        let mut rs = Resample::new(48000, 48000, 2, 1024).unwrap();
        // 100 帧 < fft_in=256：无输出（全在缓冲里）
        let out1 = rs.process(&sine(48000, 100, 440.0, 2)).unwrap();
        assert_eq!(out1.len(), 0, "100 帧 < chunk_frames=1024，缓冲中无输出");
        // 再喂 924 帧，总 1024 帧，触发一个完整 chunk
        let out2 = rs.process(&sine(48000, 924, 440.0, 2)).unwrap();
        // sub=4, fft_out=256, 1024/256=4 → 4×256=1024 输出
        assert_eq!(out2.len(), 1024 * 2, "凑满 1024 帧后输出 1024 帧");
    }

    /// **核心回归测试**：输入 > chunk_frames 整数倍（192kHz FLAC 常见）
    /// 修复前：未定义行为，输出混乱 / 越界读
    /// 修复后：按 chunk 切片循环 process，每次都是满块
    #[test]
    fn large_input_processes_in_chunks() {
        let mut rs = Resample::new(192000, 48000, 2, 1024).unwrap();
        // 192kHz → 48kHz，比例 1:4。16384 帧 = 16 chunk
        let input = sine(192000, 16384, 440.0, 2);
        let out = rs.process(&input).unwrap();
        // sub=4, 192k 下 fft_in=256, fft_out=64, 1024/256=4 → 4×64=256 输出/片
        // 16 片 × 256 = 4096 帧（精确：16384/4=4096）
        let expected_frames = 16384 / 4;
        let actual_frames = out.len() / 2;
        assert!(
            (actual_frames as i64 - expected_frames as i64).abs() <= 2,
            "192k→48k（1:4）大输入，输出 {actual_frames} 帧应≈{expected_frames}"
        );
    }

    /// 极端测试：1 个样本输入不 panic（rubato 内部累积中，无输出）
    #[test]
    fn tiny_input_does_not_panic() {
        let mut rs = Resample::new(48000, 48000, 1, 1024).unwrap();
        let out = rs.process(&[0.5]).unwrap();
        // 1 帧 < fft_in=256，无输出
        assert_eq!(out.len(), 0, "1 帧输入不产生输出（待累积）");
    }

    /// 空输入不 panic
    #[test]
    fn empty_input_returns_empty() {
        let mut rs = Resample::new(48000, 48000, 2, 1024).unwrap();
        assert!(rs.process(&[]).unwrap().is_empty());
    }

    /// 输入帧数恰好是 chunk_frames 整数倍
    #[test]
    fn exact_multiple_of_chunk_does_not_overflow() {
        let mut rs = Resample::new(48000, 48000, 2, 1024).unwrap();
        // 4096 = 4 × 1024 整倍数
        let input = sine(48000, 4096, 440.0, 2);
        let out = rs.process(&input).unwrap();
        // 4 chunk × 1024 输出 = 4096 帧
        let actual = out.len() / 2;
        assert!(
            (actual as i64 - 4096).abs() <= 1,
            "整倍数输入（修复前 bug 点），输出 {actual} 帧应≈4096"
        );
    }

    /// MP3 packet 尺寸：1152 样本/包（576 帧@stereo），upsample 44100→48000
    ///
    /// sub_chunks=4 时 44.1k→48k 的 fft_in=294, fft_out=320。
    /// 576 帧 MP3 包 < chunk_frames=1024，被缓冲，无输出。
    /// 再喂 448 帧凑满 1024，触发处理。
    #[test]
    fn mp3_packet_size_44100_to_48000() {
        let mut rs = Resample::new(44100, 48000, 2, 1024).unwrap();
        // 一包 MP3：stereo, 1152 样本 (576 帧/通道)
        let packet1 = sine(44100, 576, 440.0, 2);
        let out1 = rs.process(&packet1).unwrap();
        assert_eq!(
            out1.len(),
            0,
            "576 帧 MP3 包 < chunk_frames=1024，缓冲中无输出"
        );
        // 再喂 448 帧凑满 1024
        let packet2 = sine(44100, 448, 440.0, 2);
        let out2 = rs.process(&packet2).unwrap();
        // sub=4, fft_in=294, fft_out=320, 1024/294=3 → 3×320=960 帧
        let out_frames = out2.len() / 2;
        assert_eq!(out_frames, 960, "凑满 1024 帧后输出 3×fft_out=960 帧");
    }

    /// 连续多个满块 44.1k→48k：累计输出逼近采样率比
    #[test]
    fn mp3_continuous_full_chunks() {
        let mut rs = Resample::new(44100, 48000, 2, 1024).unwrap();
        // 100 个满块 1024 帧
        let mut total_out = 0;
        for _ in 0..100 {
            let input = sine(44100, 1024, 440.0, 2);
            let out = rs.process(&input).unwrap();
            total_out += out.len() / 2;
        }
        // 线性期望：100 × 1024 × 48000/44100
        let expected = 100.0 * 1024.0 * 48000.0 / 44100.0;
        let actual = total_out;
        assert!(
            (actual as f64 - expected).abs() / expected < 0.05,
            "100 个满块 44.1k→48k，输出 {actual} 帧应≈{expected:.0}（±5%）"
        );
    }

    /// MP3 实际场景：短包累积到满块再处理
    ///
    /// 模拟 30s MP3（44.1kHz, 576 帧/包 = 约 2288 包）。喂入后看
    /// 总输出是否接近线性期望（30s × 48kHz = 1,440,000 帧）。
    #[test]
    fn mp3_packets_accumulate_to_full_chunks() {
        let mut rs = Resample::new(44100, 48000, 2, 1024).unwrap();
        // 模拟 5s MP3 ≈ 383 包
        let num_packets = 383;
        let mut total_out = 0;
        for _ in 0..num_packets {
            let input = sine(44100, 576, 440.0, 2);
            let out = rs.process(&input).unwrap();
            total_out += out.len() / 2;
        }
        // 5s × 44100 = 220,500 帧输入
        // 期望输出：220,500 × 48000/44100 = 240,000 帧
        let expected = num_packets as f64 * 576.0 * 48000.0 / 44100.0;
        let actual = total_out;
        let ratio = actual as f64 / expected;
        assert!(
            (ratio - 1.0).abs() < 0.02,
            "{num_packets} 个 MP3 包（5s），输出 {actual} 帧应≈{expected:.0}（±2%），实际 {ratio:.3}"
        );
    }

    /// flush() 抽出缓冲 leftover + 滤波器延迟
    #[test]
    fn flush_drains_buffer_and_filter() {
        let mut rs = Resample::new(44100, 48000, 2, 1024).unwrap();
        // 喂一个 576 帧的 MP3 包（全部进缓冲，无输出）
        let packet = sine(44100, 576, 440.0, 2);
        let _ = rs.process(&packet).unwrap();
        // flush 应该处理缓冲 leftover + 滤波器延迟
        let flushed = rs.flush().unwrap();
        // 576 帧缓冲补零到 1024 处理：3×320=960 帧（数据部分）
        // 加上滤波器延迟尾部：~160 帧
        // 总共 ≈ 1000+ 帧
        let out_frames = flushed.len() / 2;
        assert!(
            out_frames >= 320,
            "flush 应抽出 ≥320 帧（缓冲+滤波器延迟），实际 {out_frames}"
        );
    }

    /// 192kHz FLAC 包（16384 帧）→ 48kHz：多 chunk 路径稳定
    #[test]
    fn flac_192k_packet() {
        let mut rs = Resample::new(192000, 48000, 2, 1024).unwrap();
        let input = sine(192000, 16384, 440.0, 2);
        let out = rs.process(&input).unwrap();
        // 16384/4=4096 帧输出（精确）
        let actual = out.len() / 2;
        assert!(
            (actual as i64 - 4096).abs() <= 2,
            "192k→48k（1:4）FLAC 整包，输出 {actual} 帧应≈4096"
        );
    }
}
