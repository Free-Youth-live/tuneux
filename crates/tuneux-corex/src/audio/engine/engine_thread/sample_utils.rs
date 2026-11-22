//! 采样工具：通道适配、环形缓冲写入、频谱累加（纯函数，无线程上下文）。
//!
//! 从 engine_thread.rs 拆分（文件过大）；这些函数均为纯输入→输出，
//! 不持有任何线程状态，可独立单测。
//!
//! - adapt_channels：文件通道数 → 设备通道数转换（单声道上混等功率居中，
//!   多声道源的复制通道衰减 -3dB）
//! - push_all：向 ringbuf 推入整帧数据（帧对齐保护）
//! - accumulate_peak：音频回调内在**音量之前**累计窗口峰值（电平表数据源）
//! - accumulate_spectrum：音频回调内累加 FFT 输入缓冲（频谱数据源）

use crate::audio::engine::spectrum;
use ringbuf::{traits::*, HeapProd};
use std::time::Duration;

/// 累计窗口内 L/R 峰值（窗口结束时读出作为电平表数值）。
///
/// **调用点必须在「音量」之前**：电平表的黄/红阈值（60% / 85%）按
/// 源/DSP 电平判读，不能随音量档位漂移——否则音量调小后峰值等比例
/// 缩水，永远到不了黄/红档，观感上就是"电平表不变色"。
///
/// 单声道（1 通道）时右声道与左声道同峰值（与频谱缓冲的复制口径一致）。
pub(super) fn accumulate_peak(data: &[f32], channels: usize, peak_l: &mut f32, peak_r: &mut f32) {
    let ch = channels.max(1);
    for (i, &s) in data.iter().enumerate() {
        let abs_s = s.abs();
        match i % ch {
            0 => {
                if abs_s > *peak_l {
                    *peak_l = abs_s;
                }
                // 单声道：右声道峰值与左同步（等功率上混后两侧同幅）。
                if ch == 1 {
                    *peak_r = (*peak_r).max(abs_s);
                }
            }
            1 => {
                *peak_r = (*peak_r).max(abs_s);
            }
            _ => {} // >2 声道：额外声道暂忽略（音乐多为单/立体声）
        }
    }
}

/// 把交错样本写入 L/R 频谱环形缓冲。
///
/// 处理单声道/立体声/多声道：
/// - 每帧（`channels` 个样本）写入 `l_buf`/`r_buf` 的同一位置；
/// - 单声道（1 通道）时右声道复制左声道，保证左右频谱一致；
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
) {
    let ch = channels.max(1);
    let mut local_frame: usize = 0;
    for (i, &s) in data.iter().enumerate() {
        let ch_idx = i % ch;
        let write_pos = (*buf_idx + local_frame) % spectrum::FFT_SIZE_FOR_BUF;
        match ch_idx {
            0 => {
                l_buf[write_pos] = s;
                // 单声道：右声道复制左声道（保证 L/R 频谱一致）
                if ch == 1 {
                    r_buf[write_pos] = s;
                }
            }
            1 => {
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
/// - from < to：上混。
///   **单声道源（from=1）：等功率居中**——两声道同乘 1/√2（×0.7071），
///   能量和不变（响度与单声道一致）且声像居中。旧式 `[L, L×0.7071]`
///   （左声道原样、复制声道衰减）声像偏左且总能量 1.5×，“不翻倍”与
///   “居中”两个目标各只完成一半，已废弃。
///   多声道源（from>1）：原始通道原样输出，超出源通道数的复制通道
///   衰减 -3dB；
/// - from > to：下混，取前 to 个通道（简化，不做加权）；
/// - 相等：直接返回。
pub(super) fn adapt_channels(samples: &[f32], from_ch: usize, to_ch: usize) -> Vec<f32> {
    if from_ch == to_ch || from_ch == 0 || to_ch == 0 {
        return samples.to_vec();
    }
    // 上混增益：-3dB ≈ 0.7071（功率减半）——单声道等功率居中时两声道
    // 同乘此系数（能量和不变）；多声道源的复制通道也用它衰减。
    const UP_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let frames = samples.len() / from_ch;
    let mut out = Vec::with_capacity(frames * to_ch);
    if from_ch < to_ch {
        for f in 0..frames {
            let base = f * from_ch;
            if from_ch == 1 {
                // 单声道 → 多声道：等功率居中——全部输出通道同乘 1/√2，
                // 响度与源一致且声像居中（不得只衰减复制通道）。
                for _ in 0..to_ch {
                    out.push(samples[base] * UP_GAIN);
                }
                continue;
            }
            for c in 0..to_ch {
                // 多声道源上混：原始通道原样输出；超出源通道数的复制通道
                // 衰减 -3dB。
                let v = if c < from_ch {
                    samples[base + c]
                } else {
                    samples[base] * UP_GAIN
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
    use crate::audio::engine::spectrum;
    use ringbuf::HeapRb;

    /// 单声道：每样本一帧，buf_idx 必须逐帧推进。
    /// 通道上混：mono → stereo 等功率居中——两声道同乘 1/√2（×0.7071），
    /// 能量和不变（响度与单声道一致）且声像居中。
    ///
    /// 回归：旧式 `[L, L×0.7071]`（左声道原样、复制声道衰减）声像偏左
    /// 且总能量 1.5×——本断言两声道同幅，偏置形态必红。
    #[test]
    fn adapt_channels_mono_to_stereo_equal_power() {
        let mono = [0.8_f32, -0.4, 0.5, -0.2];
        let stereo = adapt_channels(&mono, 1, 2);
        // 4 帧 → 8 样本
        assert_eq!(stereo.len(), 8, "mono 4 帧应上混为 8 样本");
        for (i, &m) in mono.iter().enumerate() {
            assert!(
                (stereo[i * 2] - m * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6,
                "L 应 ×0.7071（帧 {i}）"
            );
            assert!(
                (stereo[i * 2 + 1] - m * std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6,
                "R 应与 L 同幅（帧 {i}）"
            );
        }
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

    /// 组合回归（重采样通道对齐）：mono 44.1k 经 adapt 等功率上混后再
    /// 重采样到 48k——重采样器按设备通道数（2）构造时，L/R 各自独立
    /// 滤波、同幅关系保持（等功率上混后 R == L，比例 ≈ 1.0）；若按文件
    /// 通道数（1）错配构造，滤波窗跨越交错样本会造成声道串扰、
    /// 同幅关系被破坏。
    #[test]
    fn mono_upsampled_then_resampled_keeps_channel_ratio() {
        use crate::audio::engine::resample::Resample;
        // 440Hz 正弦，44.1kHz，2048 样本（mono）。
        let n = 2048;
        let mono: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44100.0).sin())
            .collect();
        // adapt 等功率上混为立体声（L == R == mono × 0.7071）。
        let stereo = adapt_channels(&mono, 1, 2);
        assert_eq!(stereo.len(), n * 2);
        // 按设备通道数构造重采样器（与解码循环送入数据的实际通道数一致）。
        let mut rs = Resample::new(44100, 48000, 2, 1024).expect("建重采样器");
        let out = rs.process(&stereo).expect("重采样");
        assert!(
            out.len() > n / 2,
            "输出样本数应 ≈ 输入×48/44.1（实际 {}）",
            out.len()
        );
        // 线性滤波器对两通道分别作用：R 应恒 ≈ L（同幅，比例 ≈ 1.0）。
        //（跳过首尾滤波器瞬态，取中段逐帧断言比例）
        let frames = out.len() / 2;
        let mut checked = 0;
        for f in frames / 4..frames * 3 / 4 {
            let l = out[f * 2];
            let r = out[f * 2 + 1];
            if l.abs() > 1e-3 {
                let ratio = r / l;
                assert!(
                    (ratio - 1.0).abs() < 0.01,
                    "帧 {f} 通道比例 {ratio} 偏离 1.0（串扰迹象）"
                );
                checked += 1;
            }
        }
        assert!(checked > 200, "应有足够样本参与断言（实际 {checked}）");
    }

    #[test]
    fn mono_advances_buf_idx_and_copies_r() {
        let mut l_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut r_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut buf_idx = 0usize;

        let mono = [0.1, 0.2, 0.3, 0.4];
        accumulate_spectrum(&mono, 1, &mut buf_idx, &mut l_buf, &mut r_buf);

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
    }

    /// 峰值累计：单声道左右同峰值；立体声分别取最大绝对值。
    #[test]
    fn accumulate_peak_takes_max_per_channel() {
        let mut pl = 0.0f32;
        let mut pr = 0.0f32;
        accumulate_peak(&[0.1, -0.7, 0.3], 1, &mut pl, &mut pr);
        assert!((pl - 0.7).abs() < 1e-6, "单声道 L 峰值 {pl}");
        assert!((pr - 0.7).abs() < 1e-6, "单声道 R 复制峰值 {pr}");

        let mut pl = 0.0f32;
        let mut pr = 0.0f32;
        // 立体声 2 帧：L0 R0 L1 R1
        accumulate_peak(&[0.1, -0.2, 0.9, -0.4], 2, &mut pl, &mut pr);
        assert!((pl - 0.9).abs() < 1e-6, "立体声 L 峰值 {pl}");
        assert!((pr - 0.4).abs() < 1e-6, "立体声 R 峰值 {pr}");
    }

    /// 立体声：每对 L+R 一帧，L/R 分别写入，peak 分别累计。
    #[test]
    fn stereo_writes_lr_and_advances_per_pair() {
        let mut l_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut r_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut buf_idx = 0usize;

        // 立体声 2 帧：L0 R0 L1 R1
        let stereo = [0.1, -0.2, 0.3, -0.4];
        accumulate_spectrum(&stereo, 2, &mut buf_idx, &mut l_buf, &mut r_buf);

        // 2 帧推进 2
        assert_eq!(buf_idx, 2);
        assert_eq!(l_buf[0], 0.1);
        assert_eq!(l_buf[1], 0.3);
        assert_eq!(r_buf[0], -0.2);
        assert_eq!(r_buf[1], -0.4);
    }

    /// 环形回绕：buf_idx 越过缓冲末尾后应取模回绕。
    #[test]
    fn mono_wraps_around_buffer_end() {
        let mut l_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];
        let mut r_buf = [0.0f32; spectrum::FFT_SIZE_FOR_BUF];

        // 从缓冲末尾前 2 格开始写 4 个单声道样本，应回绕到开头
        let mut buf_idx = spectrum::FFT_SIZE_FOR_BUF - 2;
        let mono = [1.0, 2.0, 3.0, 4.0];
        accumulate_spectrum(&mono, 1, &mut buf_idx, &mut l_buf, &mut r_buf);

        assert_eq!(buf_idx, 2, "回绕后 buf_idx 应为 4 - 2");
        assert_eq!(l_buf[spectrum::FFT_SIZE_FOR_BUF - 2], 1.0);
        assert_eq!(l_buf[spectrum::FFT_SIZE_FOR_BUF - 1], 2.0);
        assert_eq!(l_buf[0], 3.0);
        assert_eq!(l_buf[1], 4.0);
    }

    // -----------------------------------------------------------------
    // push_all 的帧对齐 + 重试上限（独立单元测试）
    // -----------------------------------------------------------------

    /// push_all 必须只推**整帧**样本（剩余不足一帧的尾部丢弃）。
    /// 验证：输入 5 个 stereo 样本（2 帧 + 1 个孤立 L），应只推 4 个（2 帧）。
    /// 旧实现会用 to_push=5 全推 —— 导致回调把 [L,R,L,R,L]
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
    #[ignore = "时序敏感（断言 < 1.5s 内返回），负载高的机器上会偶发失败；\
                运行：cargo test -p tuneux-corex -- --ignored"]
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
        // 验证：调用在 < 1.5s 返回 —— 重试上限生效（100 × 2ms = 200ms）
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "push_all 重试超时返回（实测 {elapsed:?}），避免流重建期死锁"
        );
    }
}
