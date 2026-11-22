//! # 滤波器效果器（corex 音频线程 DSP）
//!
//! 二阶低通滤波器（biquad LPF，RBJ Audio EQ Cookbook 公式）：
//! 截止频率 + 谐振 Q 两参数。音频线程读取原子参数、执行双二阶
//! 差分方程；系数仅在参数变化时重算（零锁零分配红线遵守）。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 滤波器槽位数（与 EQ/压缩器同款多槽位机制）。
pub const FILTER_SLOTS: usize = 4;

/// 截止频率下限（Hz）。
pub const FILTER_CUTOFF_MIN_HZ: f32 = 20.0;
/// 截止频率上限（Hz）。
pub const FILTER_CUTOFF_MAX_HZ: f32 = 20000.0;
/// 截止频率默认值（Hz，全频段通过）。
pub const FILTER_CUTOFF_DEFAULT_HZ: f32 = 20000.0;
/// 谐振 Q 下限。
pub const FILTER_Q_MIN: f32 = 0.1;
/// 谐振 Q 上限。
pub const FILTER_Q_MAX: f32 = 20.0;
/// 谐振 Q 默认值（约 0.707 = Butterworth 最平坦）。
pub const FILTER_Q_DEFAULT: f32 = 0.707;

// =============================================================================
// 参数存储（控制面写 / 音频线程读，零锁原子）
// =============================================================================

/// f32 → u32 位模式（原子存储用）。
fn f32_to_bits(v: f32) -> u32 {
    v.to_bits()
}

/// u32 位模式 → f32。
fn bits_to_f32(v: u32) -> f32 {
    f32::from_bits(v)
}

/// 滤波器参数：截止频率 + 谐振 Q + 使能。
#[derive(Debug)]
pub struct FilterParams {
    cutoff_bits: AtomicU32,
    q_bits: AtomicU32,
    enabled: AtomicBool,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self {
            cutoff_bits: AtomicU32::new(f32_to_bits(FILTER_CUTOFF_DEFAULT_HZ)),
            q_bits: AtomicU32::new(f32_to_bits(FILTER_Q_DEFAULT)),
            enabled: AtomicBool::new(false),
        }
    }
}

impl FilterParams {
    /// 截止频率（Hz）。
    pub fn cutoff_hz(&self) -> f32 {
        bits_to_f32(self.cutoff_bits.load(Ordering::Relaxed))
    }

    /// 设置截止频率（Hz，自动钳制到有效范围）。
    ///
    /// 返回「已接受（true）/ 被拒绝（false）」，与压缩器/均衡器 setter
    /// 同口径；返回值不再表示「值是否变化」。
    /// 非有限值拒绝且保持原值（既有口径）。
    pub fn set_cutoff_hz(&self, hz: f32) -> bool {
        if !hz.is_finite() {
            return false;
        }
        let clamped = hz.clamp(FILTER_CUTOFF_MIN_HZ, FILTER_CUTOFF_MAX_HZ);
        self.cutoff_bits
            .store(f32_to_bits(clamped), Ordering::Relaxed);
        true
    }

    /// 谐振 Q 值。
    pub fn resonance_q(&self) -> f32 {
        bits_to_f32(self.q_bits.load(Ordering::Relaxed))
    }

    /// 设置谐振 Q（自动钳制）。
    ///
    /// 返回「已接受（true）/ 被拒绝（false）」，与压缩器/均衡器 setter
    /// 同口径；返回值不再表示「值是否变化」。
    /// 非有限值拒绝且保持原值（既有口径）。
    pub fn set_resonance_q(&self, q: f32) -> bool {
        if !q.is_finite() {
            return false;
        }
        let clamped = q.clamp(FILTER_Q_MIN, FILTER_Q_MAX);
        self.q_bits.store(f32_to_bits(clamped), Ordering::Relaxed);
        true
    }

    /// 使能。
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// 设置使能。
    pub fn set_enabled(&self, v: bool) {
        self.enabled.store(v, Ordering::Relaxed);
    }

    /// 重置到默认（截止 20 kHz / Q 0.707 / 关）。
    pub fn reset(&self) {
        self.set_cutoff_hz(FILTER_CUTOFF_DEFAULT_HZ);
        self.set_resonance_q(FILTER_Q_DEFAULT);
        self.set_enabled(false);
    }
}

// =============================================================================
// Biquad DSP（音频线程内联使用，栈上状态零分配）
// =============================================================================

/// 单通道 biquad 状态 + 系数缓存。
#[derive(Clone, Copy, Debug, Default)]
pub struct BiquadChannel {
    // 差分方程状态
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    // 归一化系数（RBJ LPF）
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    // 系数缓存标记（参数变化时置 false 触发重算）
    coef_cutoff: f32,
    coef_q: f32,
    coef_sr: f32,
}

impl BiquadChannel {
    /// 更新系数（仅在参数变化时调用；RBJ Audio EQ Cookbook LPF 公式）。
    fn update_coefficients(&mut self, cutoff: f32, q: f32, sample_rate: f32) {
        if cutoff == self.coef_cutoff && q == self.coef_q && sample_rate == self.coef_sr {
            return; // 系数未变，复用缓存
        }
        // Nyquist bypass：截止频率接近或超过 Nyquist（0.95×sr/2）时滤波
        // 退化为直通。不做 bypass 的话，低采样率下（22.05k/24k/32k 与
        // 默认 20kHz 截止的组合）butterworth 极点会跑到单位圆外（|a2/a0|>1）
        // → 数样本后 Inf → NaN，且 `f32::clamp` 对 NaN 原样放行 → F32
        // 设备 NaN 直入 DAC，整首报废。与均衡器的同名保护同口径。
        if cutoff >= 0.95 * (sample_rate.max(1.0) / 2.0) || !cutoff.is_finite() {
            self.b0 = 1.0;
            self.b1 = 0.0;
            self.b2 = 0.0;
            self.a1 = 0.0;
            self.a2 = 0.0;
            self.coef_cutoff = cutoff;
            self.coef_q = q;
            self.coef_sr = sample_rate;
            return;
        }
        let omega = 2.0 * std::f32::consts::PI * cutoff / sample_rate;
        let alpha = omega.sin() / (2.0 * q);
        let cos_w = omega.cos();

        let b0 = (1.0 - cos_w) / 2.0;
        let b1 = 1.0 - cos_w;
        let b2 = (1.0 - cos_w) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w;
        let a2 = 1.0 - alpha;

        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
        self.coef_cutoff = cutoff;
        self.coef_q = q;
        self.coef_sr = sample_rate;
    }

    /// 处理单个样本（直接 II 型差分方程）。
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        // 非有限出口守卫：一旦出现 NaN/Inf（参数极端组合、上游 NaN 输入）
        // 清空内部状态并直通当前输入——不清状态的话 y1/y2 的 NaN 会自我
        // 维持到下一次 reset，整首报废。与均衡器的同名守卫同口径。
        if !y.is_finite() {
            self.x1 = 0.0;
            self.x2 = 0.0;
            self.y1 = 0.0;
            self.y2 = 0.0;
            return x;
        }
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// 双通道 biquad（L/R 独立状态）。
#[derive(Clone, Copy, Debug, Default)]
pub struct BiquadStereo {
    left: BiquadChannel,
    right: BiquadChannel,
}

impl BiquadStereo {
    /// 处理一帧（交错 L/R 更新）。
    #[inline]
    pub fn process_frame(
        &mut self,
        params: &FilterParams,
        sample_rate: f32,
        l: f32,
        r: f32,
    ) -> (f32, f32) {
        let cutoff = params.cutoff_hz();
        let q = params.resonance_q();
        self.left.update_coefficients(cutoff, q, sample_rate);
        self.right.update_coefficients(cutoff, q, sample_rate);
        (self.left.process(l), self.right.process(r))
    }
}

/// 滤波器效果器（与 EqEffect / CompressorEffect 同接口——供音频回调调用）。
#[derive(Clone, Copy, Debug, Default)]
pub struct FilterEffect {
    stereo: BiquadStereo,
}

impl FilterEffect {
    /// 处理一批交错样本（与 EqEffect::process 同签名）。
    ///
    /// 与 EQ/压缩器统一口径：通道数非 1/2 时**整体直通**。
    /// 旧实现只滤每帧前 2 声道、其余声道直通，与其余两个 DSP
    /// 的「非 1/2 声道整体直通」口径分裂，且文档未声明。
    pub fn process(
        &mut self,
        data: &mut [f32],
        channels: usize,
        sample_rate: u32,
        params: &FilterParams,
    ) {
        if !params.enabled() {
            return;
        }
        if channels != 1 && channels != 2 {
            return;
        }
        if channels == 2 {
            for frame in data.as_chunks_mut::<2>().0 {
                let (l, r) =
                    self.stereo
                        .process_frame(params, sample_rate as f32, frame[0], frame[1]);
                frame[0] = l;
                frame[1] = r;
            }
        } else {
            for x in data.iter_mut() {
                let (l, _) = self
                    .stereo
                    .process_frame(params, sample_rate as f32, *x, *x);
                *x = l;
            }
        }
    }

    /// 重置内部状态（换曲 / seek 时清残留）。
    pub fn reset(&mut self) {
        self.stereo = BiquadStereo::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// setter 返回「已接受/被拒绝」而非「值是否变化」：
    /// 写入同值也必须返回 true（与压缩器/均衡器 setter 同口径）。
    #[test]
    fn setters_return_acceptance_not_change_detection() {
        let p = FilterParams::default();
        // 同值写入：旧「值是否变化」语义返回 false，新「已接受」语义返回 true。
        assert!(p.set_cutoff_hz(p.cutoff_hz()), "同值写入应为已接受");
        assert!(p.set_resonance_q(p.resonance_q()), "同值写入应为已接受");
        // 越界值钳制后仍为「已接受」。
        assert!(p.set_cutoff_hz(1_000_000.0));
        assert_eq!(p.cutoff_hz(), FILTER_CUTOFF_MAX_HZ);
        // 非有限值仍拒绝（既有口径保持）。
        assert!(!p.set_cutoff_hz(f32::NAN));
        assert!(!p.set_resonance_q(f32::INFINITY));
    }

    /// 非 1/2 声道整体直通：旧实现只滤每帧前 2 声道、
    /// 其余声道直通，与 EQ/压缩器「非 1/2 声道整体直通」口径分裂。
    #[test]
    fn multi_channel_beyond_stereo_passes_through_whole_buffer() {
        let mut fx = FilterEffect::default();
        let p = FilterParams::default();
        p.set_enabled(true);
        p.set_cutoff_hz(500.0);
        p.set_resonance_q(5.0);
        for channels in [0, 3, 4, 6] {
            let mut data = vec![0.5f32; channels * 3];
            let orig = data.clone();
            fx.process(&mut data, channels, 48_000, &p);
            assert_eq!(data, orig, "{channels} 声道应整体直通");
        }
    }

    /// 单声道/立体声仍正常滤波：启用 + 低截止时高频交替信号被改变且输出有限。
    #[test]
    fn mono_and_stereo_processed_when_enabled() {
        let mut fx = FilterEffect::default();
        let p = FilterParams::default();
        p.set_enabled(true);
        p.set_cutoff_hz(500.0);
        p.set_resonance_q(1.0);
        // 交替样本（高频成分）：500Hz LPF 必须改变它。
        let mut data: Vec<f32> = (0..32)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        fx.process(&mut data, 1, 48_000, &p);
        assert!(data.iter().all(|v| v.is_finite()), "单声道输出必须有限");
        assert!(
            data.iter()
                .any(|&v| (v - 0.5).abs() > 1e-3 || (v + 0.5).abs() > 1e-3),
            "单声道低通滤波应改变高频交替信号"
        );
        let mut data: Vec<f32> = (0..32)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        fx.process(&mut data, 2, 48_000, &p);
        assert!(data.iter().all(|v| v.is_finite()), "立体声输出必须有限");
        assert!(
            data.iter()
                .any(|&v| (v - 0.5).abs() > 1e-3 || (v + 0.5).abs() > 1e-3),
            "立体声低通滤波应改变高频交替信号"
        );
    }

    #[test]
    fn non_finite_params_are_rejected_without_overwrite() {
        let p = FilterParams::default();
        p.set_cutoff_hz(1_000.0);
        p.set_resonance_q(1.2);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!p.set_cutoff_hz(bad));
            assert!(!p.set_resonance_q(bad));
            assert_eq!(p.cutoff_hz(), 1_000.0);
            assert_eq!(p.resonance_q(), 1.2);
        }
    }
}
