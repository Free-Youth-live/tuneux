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
    pub fn set_cutoff_hz(&self, hz: f32) -> bool {
        let clamped = hz.clamp(FILTER_CUTOFF_MIN_HZ, FILTER_CUTOFF_MAX_HZ);
        let changed = clamped != self.cutoff_hz();
        self.cutoff_bits
            .store(f32_to_bits(clamped), Ordering::Relaxed);
        changed
    }

    /// 谐振 Q 值。
    pub fn resonance_q(&self) -> f32 {
        bits_to_f32(self.q_bits.load(Ordering::Relaxed))
    }

    /// 设置谐振 Q（自动钳制）。
    pub fn set_resonance_q(&self, q: f32) -> bool {
        let clamped = q.clamp(FILTER_Q_MIN, FILTER_Q_MAX);
        let changed = clamped != self.resonance_q();
        self.q_bits.store(f32_to_bits(clamped), Ordering::Relaxed);
        changed
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
        for frame in data.chunks_mut(channels.max(1)) {
            if frame.len() >= 2 {
                let (l, r) =
                    self.stereo
                        .process_frame(params, sample_rate as f32, frame[0], frame[1]);
                frame[0] = l;
                frame[1] = r;
            } else if frame.len() == 1 {
                let (l, _) =
                    self.stereo
                        .process_frame(params, sample_rate as f32, frame[0], frame[0]);
                frame[0] = l;
            }
        }
    }

    /// 重置内部状态（换曲 / seek 时清残留）。
    pub fn reset(&mut self) {
        self.stereo = BiquadStereo::default();
    }
}
