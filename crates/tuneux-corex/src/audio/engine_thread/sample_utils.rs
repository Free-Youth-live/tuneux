//! 采样工具：通道适配、环形缓冲写入、频谱累加（纯函数，无线程上下文）。
//!
//! 从 engine_thread.rs 拆分（文件过大）；这些函数均为纯输入→输出，
//! 不持有任何线程状态，可独立单测。
//!
//! - adapt_channels：文件通道数 → 设备通道数转换（上混复制通道衰减 -3dB）
//! - push_all：向 ringbuf 推入整帧数据（帧对齐保护）
//! - accumulate_spectrum：音频回调内累加 FFT 输入缓冲与窗口峰值

use crate::audio::spectrum;
use ringbuf::{traits::*, HeapProd};
use std::time::Duration;

/// 把交错样本写入 L/R 频谱环形缓冲，并累计 L/R 峰值。
///
/// 处理单声道/立体声/多声道：
/// - 每帧（`channels` 个样本）写入 `l_buf`/`r_buf` 的同一位置；
/// - 单声道（1 通道）时右声道复制左声道，保证左右频谱/电平一致；
/// - 写入完成后推进 `buf_idx`（环形偏移）。
///
/// 这是频谱可视化的数据源：窗口结束时把 buffer 重排后做 FFT。
/// 帧指针按"每帧结束推进"：单声道每样本一帧，立体声每对 L+R 一帧。
pub(super) fn accumulate_spectrum(
    data: &[f32],
    channels: usize,
    buf_idx: &mut usize,
    l_buf: &mut [f32; spectrum::FFT_SIZE_FOR_BUF],
    r_buf: &mut [f32; spectrum::FFT_SIZE_FOR_BUF],
    window_peak_l: &mut f32,
    window_peak_r: &mut f32,
) {
    let ch = channels.max(1);
    let mut local_frame: usize = 0;
    for (i, &s) in data.iter().enumerate() {
        let abs_s = s.abs();
        let ch_idx = i % ch;
        let write_pos = (*buf_idx + local_frame) % spectrum::FFT_SIZE_FOR_BUF;
        match ch_idx {
            0 => {
                if abs_s > *window_peak_l {
                    *window_peak_l = abs_s;
                }
                l_buf[write_pos] = s;
                // 单声道：右声道复制左声道
                if ch == 1 {
                    r_buf[write_pos] = s;
                    if abs_s > *window_peak_r {
                        *window_peak_r = abs_s;
                    }
                }
            }
            1 => {
                if abs_s > *window_peak_r {
                    *window_peak_r = abs_s;
                }
                r_buf[write_pos] = s;
            }
            _ => {} // >2 声道：额外声道暂忽略（音乐多为单/立体声）
        }
        // 每写完一帧（ch 个样本）的最后一样本，帧指针前进。
        // 单声道每样本即一帧；立体声每对 L+R 即一帧。
        if ch_idx == ch - 1 {
            local_frame += 1;
        }
    }
    if local_frame > 0 {
        *buf_idx = (*buf_idx + local_frame) % spectrum::FFT_SIZE_FOR_BUF;
    }
}

/// `channels` 用于**帧对齐**：交错样本按 `index % channels` 解释，若 ringbuf
/// 里累积的样本数不是 channels 的整数倍，回调会把 L/R 声道相位搞错（静默
/// 错位直到下次 flush）。因此本函数只推入整帧（剩余不足一帧的尾部丢弃），
/// 从源头保证 ringbuf 永远帧对齐。
///
/// **重要**：有重试上限（MAX_RETRIES），这是防死锁的关键——换曲时旧
/// consumer 被 drop 后 ringbuf 永远满，若无上限则无限 sleep，解码线程永远
/// 回不到循环顶部去处理 NewProducer 命令，整个管线死锁。上限放弃剩余数据
/// 后解码线程回到循环顶部，能处理换 producer 命令恢复正常。
pub(super) fn push_all(producer: &mut HeapProd<f32>, samples: &[f32], channels: usize) {
    let mut pushed = 0;
    let mut retries = 0u32;
    const MAX_RETRIES: u32 = 100; // 100 × 2ms = 最长等 200ms（流重建/瞬态窗口足够；过长会卡顿）
    let ch = channels.max(1);
    while pushed < samples.len() {
        // 只推整帧：剩余不足一帧的尾部丢弃（正常路径上样本总是整帧，
        // 这是对异常输入的防御——半帧会破坏回调侧声道解释）
        let remaining = samples.len() - pushed;
        let to_push = remaining - remaining % ch;
        if to_push == 0 {
            break; // 只剩半帧：丢弃
        }
        let n = producer.push_slice(&samples[pushed..pushed + to_push]);
        pushed += n;
        if n == 0 {
            retries += 1;
            if retries > MAX_RETRIES {
                // 放弃剩余数据（流重建窗口期/暂停超时 consumer 不可用）
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        } else {
            // 推进了，重置重试计数
            retries = 0;
        }
    }
}

/// 通道数适配：文件通道 ≠ 设备通道时的简单转换。
///
/// - from < to：上混，复制通道填充。
///   **复制的通道衰减 -3dB**（×0.7071）保持响度一致：mono → stereo 时
///   `[L] → [L, L×0.7071]`，原始左声道不动，复制的右声道减半功率，
///   避免单声道文件在立体声设备上整体响度翻倍（指出的问题）。
/// - from > to：下混，取前 to 个通道（简化，不做加权）；
/// - 相等：直接返回。
pub(super) fn adapt_channels(samples: &[f32], from_ch: usize, to_ch: usize) -> Vec<f32> {
    if from_ch == to_ch || from_ch == 0 || to_ch == 0 {
        return samples.to_vec();
    }
    // 复制通道的衰减系数：-3dB ≈ 0.7071（功率减半，响度与原始声道一致）
    const DUP_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let frames = samples.len() / from_ch;
    let mut out = Vec::with_capacity(frames * to_ch);
    if from_ch < to_ch {
        for f in 0..frames {
            let base = f * from_ch;
            for c in 0..to_ch {
                // 原始通道原样输出；超出源通道数的复制通道衰减 -3dB
                let v = if c < from_ch {
                    samples[base + c]
                } else {
                    samples[base] * DUP_GAIN
                };
                out.push(v);
            }
        }
    } else {
        for f in 0..frames {
            let base = f * from_ch;
            for c in 0..to_ch {
                out.push(samples[base + c]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::spectrum;
    use ringbuf::HeapRb;

    /// 单声道：每样本一帧，buf_idx 必须逐帧推进。
    /// 通道上混：mono → stereo 时复制的通道衰减 -3dB（×0.7071），
    /// 原始通道不动，避免单声道响度翻倍。
    #[test]
    fn adapt_channels_mono_to_stereo_attenuates_dup() {
        let mono = [0.8_f32, -0.4, 0.5, -0.2];
        let stereo = adapt_channels(&mono, 1, 2);
        // 4 帧 → 8 样本
        assert_eq!(stereo.len(), 8, "mono 4 帧应上混为 8 样本");
        // 原始通道（L）原样；复制通道（R）衰减
        assert_eq!(stereo[0], 0.8, "L 原样");
        assert!(
            (stereo[1] - 0.8 * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6,
            "R 衰减 -3dB"
        );
        assert_eq!(stereo[2], -0.4, "L 原样");
        assert!(
            (stereo[3] - (-0.4 * std::f32::consts::FRAC_1_SQRT_2)).abs() < 1e-6,
            "R 衰减 -3dB"
        );
    }

    /// 通道下混：stereo → mono 取第一通道，不衰减。
    #[test]
    fn adapt_channels_stereo_to_mono_takes_first() {
        let stereo = [0.8_f32, 0.2, -0.4, 0.1];
        let mono = adapt_channels(&stereo, 2, 1);
        assert_eq!(mono.len(), 2, "stereo 2 帧应下混为 2 样本");
        assert_eq!(mono[0], 0.8);
        assert_eq!(mono[1], -0.4);
    }

    /// 通道数相等：原样返回（不复制、不衰减）。
    #[test]
    fn adapt_channels_equal_returns_as_is() {
        let stereo = [0.8_f32, 0.2, -0.4, 0.1];
        let out = adapt_channels(&stereo, 2, 2);
        assert_eq!(out, stereo);
    }

    #[test]
    fn mono_advances_buf_idx_and_copies_r() {
        let mut l_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut r_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut buf_idx = 0usize;
        let mut pl = 0.0f32;
        let mut pr = 0.0f32;

        let mono = [0.1, 0.2, 0.3, 0.4];
        accumulate_spectrum(
            &mono,
            1,
            &mut buf_idx,
            &mut l_buf,
            &mut r_buf,
            &mut pl,
            &mut pr,
        );

        // 单声道每样本一帧，应推进 4
        assert_eq!(buf_idx, 4, "单声道 buf_idx 应推进样本数");
        // L 写入 4 个不同位置（而非覆盖同一格）
        assert_eq!(l_buf[0], 0.1);
        assert_eq!(l_buf[1], 0.2);
        assert_eq!(l_buf[2], 0.3);
        assert_eq!(l_buf[3], 0.4);
        // R 复制 L
        assert_eq!(r_buf[0], 0.1);
        assert_eq!(r_buf[3], 0.4);
        // peak L/R 均为最大值 0.4
        assert!((pl - 0.4).abs() < 1e-6);
        assert!((pr - 0.4).abs() < 1e-6);
    }

    /// 立体声：每对 L+R 一帧，L/R 分别写入，peak 分别累计。
    #[test]
    fn stereo_writes_lr_and_advances_per_pair() {
        let mut l_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut r_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut buf_idx = 0usize;
        let mut pl = 0.0f32;
        let mut pr = 0.0f32;

        // 立体声 2 帧：L0 R0 L1 R1
        let stereo = [0.1, -0.2, 0.3, -0.4];
        accumulate_spectrum(
            &stereo,
            2,
            &mut buf_idx,
            &mut l_buf,
            &mut r_buf,
            &mut pl,
            &mut pr,
        );

        // 2 帧推进 2
        assert_eq!(buf_idx, 2);
        assert_eq!(l_buf[0], 0.1);
        assert_eq!(l_buf[1], 0.3);
        assert_eq!(r_buf[0], -0.2);
        assert_eq!(r_buf[1], -0.4);
        // peak L = 0.3，peak R = 0.4
        assert!((pl - 0.3).abs() < 1e-6);
        assert!((pr - 0.4).abs() < 1e-6);
    }

    /// 环形回绕：buf_idx 越过缓冲末尾后应取模回绕。
    #[test]
    fn mono_wraps_around_buffer_end() {
        let mut l_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut r_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut pl = 0.0f32;
        let mut pr = 0.0f32;

        // 从缓冲末尾前 2 格开始写 4 个单声道样本，应回绕到开头
        let mut buf_idx = spectrum::FFT_SIZE_FOR_BUF - 2;
        let mono = [1.0, 2.0, 3.0, 4.0];
        accumulate_spectrum(
            &mono,
            1,
            &mut buf_idx,
            &mut l_buf,
            &mut r_buf,
            &mut pl,
            &mut pr,
        );

        assert_eq!(buf_idx, 2, "回绕后 buf_idx 应为 4 - 2");
        assert_eq!(l_buf[spectrum::FFT_SIZE_FOR_BUF - 2], 1.0);
        assert_eq!(l_buf[spectrum::FFT_SIZE_FOR_BUF - 1], 2.0);
        assert_eq!(l_buf[0], 3.0);
        assert_eq!(l_buf[1], 4.0);
    }

    // -----------------------------------------------------------------
    //  关联：push_all 的帧对齐 + 重试上限（独立单元测试）
    // -----------------------------------------------------------------

    /// push_all 必须只推**整帧**样本（剩余不足一帧的尾部丢弃）。
    /// 验证：输入 5 个 stereo 样本（2 帧 + 1 个孤立 L），应只推 4 个（2 帧）。
    /// 修" 帧错位"前的代码会用 to_push=5 全推 —— 导致回调把 [L,R,L,R,L]
    /// 当作 2.5 帧解释，左/右相位错乱（静默错位到下次 flush）。
    #[test]
    fn push_all_aligns_to_frame_boundary() {
        let ring = HeapRb::<f32>::new(64);
        let (mut producer, _consumer) = ring.split();
        // 5 个 stereo 样本：2 帧 + 1 个孤立样本（半帧尾部）
        let input = [0.1f32, 0.2, 0.3, 0.4, 0.5];
        push_all(&mut producer, &input, 2);
        assert_eq!(
            producer.occupied_len(),
            4,
            "5 个 stereo 样本应只推 2 整帧（4 样本），半帧尾部丢弃"
        );
    }

    #[test]
    #[ignore]
    fn push_all_gives_up_after_max_retries_when_full() {
        // 容量极小的 ringbuf
        let ring = HeapRb::<f32>::new(8); // 8 样本 = 2 stereo 帧
        let (mut producer, consumer) = ring.split();
        // 用 producer 自身装满（无 consumer 消费）
        let fill = [0.0f32; 8];
        let pushed = producer.push_slice(&fill);
        assert_eq!(pushed, 8);
        assert_eq!(producer.occupied_len(), 8);
        // drop consumer —— 模拟"旧 consumer 被 take 后 release"
        drop(consumer);

        // 现在 push 64 样本（32 帧）：会一直 retry → MAX_RETRIES=100 → 放弃
        let input = vec![1.0f32; 64];
        let start = std::time::Instant::now();
        push_all(&mut producer, &input, 2);
        let elapsed = start.elapsed();
        // 验证：调用在 < 1.5s 返回 —— 重试上限生效（500 × 2ms ≈ 1s）
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "push_all 重试超时返回（实测 {elapsed:?}），避免流重建期死锁"
        );
    }
}
