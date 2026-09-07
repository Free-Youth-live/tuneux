//! # 均衡器效果器（corex 音频线程 DSP）
//!
//! 10 段 peaking 均衡器：插件（WASM 控制面）经宿主函数写每段增益（dB），
//! 本模块在音频线程读取原子参数、执行双二阶（biquad）滤波。
//!
//! 红线遵守：音频线程零锁（参数经原子读取）、零堆分配（系数重算用栈上
//! 局部 + 固定数组）、零 host call。参数未变化时复用缓存系数，不重算。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 均衡器段数（固定 10 段，octave 排列）。
pub const EQ_BANDS: usize = 10;
/// 均衡器槽位数（v2 动态多槽位：最多 8 个均衡器叠加）。
pub const EQ_SLOTS: usize = 8;

/// 10 段中心频率（Hz，octave：31.25 → 16k）。
pub const EQ_FREQS: [f32; EQ_BANDS] = [
    31.25, 62.5, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

/// 每段 Q 值（octave EQ 常用约 1.0，带宽约 1 octave）。
const EQ_Q: f32 = 1.0;

/// 每段增益下限（dB）。
pub const EQ_GAIN_MIN_DB: f32 = -12.0;
/// 每段增益上限（dB）。
pub const EQ_GAIN_MAX_DB: f32 = 12.0;

/// 均衡器参数存储：插件控制面写入、音频线程读取。
///
/// 用原子数组承载 10 段增益（f32 以位模式存 AtomicU32）+ 使能位，
/// 满足音频线程「零锁、零分配」读取。
#[derive(Debug)]
pub struct EqParams {
    bands: [AtomicU32; EQ_BANDS],
    enabled: AtomicBool,
}

impl EqParams {
    /// 新建（全部 0 dB、启用）。
    pub fn new() -> Self {
        Self {
            bands: std::array::from_fn(|_| AtomicU32::new(0.0f32.to_bits())),
            enabled: AtomicBool::new(true),
        }
    }

    /// 写某段增益（dB），越界返回 false。增益钳制到 [-12, +12] dB。
    pub fn set_band(&self, band: usize, gain_db: f32) -> bool {
        if band >= EQ_BANDS {
            return false;
        }
        let clamped = gain_db.clamp(EQ_GAIN_MIN_DB, EQ_GAIN_MAX_DB);
        self.bands[band].store(clamped.to_bits(), Ordering::Relaxed);
        true
    }

    /// 读某段增益（dB）；越界返回 0.0（与 set_band 的越界防护对称，杜绝 panic）。
    pub fn band(&self, band: usize) -> f32 {
        if band >= EQ_BANDS {
            return 0.0;
        }
        f32::from_bits(self.bands[band].load(Ordering::Relaxed))
    }

    /// 设置槽位启用（false = 宿主跳过 DSP，直通）。
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// 槽位是否启用。
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// 重置到默认（10 段 0 dB、启用）——卸载/释放槽位时调用。
    pub fn reset(&self) {
        for b in &self.bands {
            b.store(0.0f32.to_bits(), Ordering::Relaxed);
        }
        self.enabled.store(true, Ordering::Relaxed);
    }
}

impl Default for EqParams {
    fn default() -> Self {
        Self::new()
    }
}

/// 双二阶 peaking 系数（RBJ Audio EQ Cookbook）。`a0` 已归一。
#[derive(Debug, Clone, Copy)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// 计算某段在某采样率下的 peaking 系数。
fn peaking_coeffs(freq_hz: f32, q: f32, gain_db: f32, sample_rate: u32) -> BiquadCoeffs {
    let a = 10f32.powf(gain_db / 40.0);
    let w0 = std::f32::consts::TAU * freq_hz / sample_rate.max(1) as f32;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q.max(1e-6));

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha / a;

    // a0 归一（a0 恒 > 0：a ≥ 10^(-12/40) > 0，alpha ≥ 0）。
    BiquadCoeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

/// 有状态均衡器：10 段 biquad，L/R 独立，音频线程常驻。
///
/// 系数按「增益 + 采样率」缓存：参数未变时不重算（每回调只做若干次原子读
/// 与比较，不触碰堆）。
#[derive(Debug)]
pub(crate) struct EqEffect {
    /// 10 段系数（b0, b1, b2, a1, a2）。
    coeffs: [BiquadCoeffs; EQ_BANDS],
    /// 10 段 L 声道状态（x1, x2, y1, y2）。
    state_l: [[f32; 4]; EQ_BANDS],
    /// 10 段 R 声道状态。
    state_r: [[f32; 4]; EQ_BANDS],
    /// 缓存的增益（判断参数是否变化）。
    cached_gains: [f32; EQ_BANDS],
    /// 缓存的采样率。
    cached_sr: u32,
    /// 缓存的使能位。
    cached_enabled: bool,
}

impl Default for EqEffect {
    fn default() -> Self {
        Self {
            coeffs: [BiquadCoeffs {
                b0: 1.0,
                b1: 0.0,
                b2: 0.0,
                a1: 0.0,
                a2: 0.0,
            }; EQ_BANDS],
            state_l: [[0.0; 4]; EQ_BANDS],
            state_r: [[0.0; 4]; EQ_BANDS],
            cached_gains: [0.0; EQ_BANDS],
            cached_sr: 0,
            cached_enabled: true,
        }
    }
}

impl EqEffect {
    /// 处理一段交错样本（L, R, L, R, ...）。`channels` 为 1 或 2。
    ///
    /// 音频线程调用：零锁（原子读）、零堆分配。
    pub(crate) fn process(
        &mut self,
        data: &mut [f32],
        channels: usize,
        sample_rate: u32,
        params: &EqParams,
    ) {
        if channels != 1 && channels != 2 {
            return;
        }
        let enabled = params.enabled();
        if !enabled {
            return;
        }

        // 读 10 段增益，检测变化（含采样率/使能变化）。
        let mut gains = [0.0f32; EQ_BANDS];
        for (i, g) in gains.iter_mut().enumerate() {
            *g = params.band(i);
        }
        let changed = sample_rate != self.cached_sr
            || enabled != self.cached_enabled
            || gains != self.cached_gains;
        if changed {
            for (i, c) in self.coeffs.iter_mut().enumerate() {
                *c = peaking_coeffs(EQ_FREQS[i], EQ_Q, gains[i], sample_rate);
            }
            self.cached_gains = gains;
            self.cached_sr = sample_rate;
            self.cached_enabled = enabled;
        }

        // 逐样本过 10 段 biquad（L/R 独立状态）。
        if channels == 2 {
            for frame in data.as_chunks_mut::<2>().0 {
                let x_l = frame[0];
                let x_r = frame[1];
                frame[0] = Self::run_bands(&mut self.state_l, &self.coeffs, x_l);
                frame[1] = Self::run_bands(&mut self.state_r, &self.coeffs, x_r);
            }
        } else {
            for x in data.iter_mut() {
                *x = Self::run_bands(&mut self.state_l, &self.coeffs, *x);
            }
        }
    }

    /// 单声道样本依次通过 10 段 biquad。
    #[inline]
    fn run_bands(
        state: &mut [[f32; 4]; EQ_BANDS],
        coeffs: &[BiquadCoeffs; EQ_BANDS],
        x0: f32,
    ) -> f32 {
        let mut x = x0;
        for band in 0..EQ_BANDS {
            let c = &coeffs[band];
            let s = &mut state[band];
            // s = [x1, x2, y1, y2]
            let y = c.b0 * x + c.b1 * s[0] + c.b2 * s[1] - c.a1 * s[2] - c.a2 * s[3];
            s[1] = s[0];
            s[0] = x;
            s[3] = s[2];
            s[2] = y;
            x = y;
        }
        x
    }

    /// 重置全部状态（换曲/seek 时调用，避免滤波器状态残留）。
    pub(crate) fn reset(&mut self) {
        self.state_l = [[0.0; 4]; EQ_BANDS];
        self.state_r = [[0.0; 4]; EQ_BANDS];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_write_read_roundtrip_and_clamp() {
        let p = EqParams::new();
        assert!(p.set_band(0, 6.0));
        assert_eq!(p.band(0), 6.0);
        // 越界返回 false。
        assert!(!p.set_band(EQ_BANDS, 1.0));
        // 钳制到 [-12, +12]。
        p.set_band(1, 99.0);
        assert_eq!(p.band(1), EQ_GAIN_MAX_DB);
        p.set_band(2, -99.0);
        assert_eq!(p.band(2), EQ_GAIN_MIN_DB);
    }

    #[test]
    fn band_read_out_of_bounds_returns_zero() {
        let p = EqParams::new();
        p.set_band(0, 6.0);
        assert_eq!(p.band(EQ_BANDS), 0.0, "越界读应返回 0.0 而非 panic");
        assert_eq!(p.band(EQ_BANDS + 100), 0.0);
    }

    #[test]
    fn eq_passthrough_when_all_zero() {
        let mut fx = EqEffect::default();
        let params = EqParams::new(); // 全 0 dB
        let mut data = vec![0.25f32, -0.5, 0.75, -0.125];
        let orig = data.clone();
        fx.process(&mut data, 2, 48_000, &params);
        // 0 dB 增益 = 单位滤波（b0=1, 其余 0），输出应等于输入（允许浮点误差）。
        for (a, b) in data.iter().zip(orig.iter()) {
            assert!((a - b).abs() < 1e-5, "0 dB 应直通：{a} vs {b}");
        }
    }

    #[test]
    fn eq_processes_finite_values() {
        let mut fx = EqEffect::default();
        let params = EqParams::new();
        params.set_band(0, 6.0);
        params.set_band(9, -6.0);
        let mut data = vec![0.1f32; 256];
        fx.process(&mut data, 1, 44_100, &params);
        assert!(data.iter().all(|v| v.is_finite()), "输出必须有限");
        // 增益不为 0 时应改变信号（不再全等）。
        assert!(
            data.iter().any(|&v| (v - 0.1).abs() > 1e-6),
            "EQ 应改变信号"
        );
    }

    #[test]
    fn disabled_bypasses() {
        let mut fx = EqEffect::default();
        let params = EqParams::new();
        params.set_band(0, 12.0);
        params.set_enabled(false);
        let mut data = vec![0.5f32, -0.25];
        let orig = data.clone();
        fx.process(&mut data, 2, 48_000, &params);
        assert_eq!(data, orig, "禁用时应完全直通");
    }

    #[test]
    fn reset_clears_state() {
        let mut fx = EqEffect::default();
        let params = EqParams::new();
        params.set_band(0, 6.0);
        fx.process(&mut [0.5f32; 32], 1, 48_000, &params);
        fx.reset();
        let params2 = EqParams::new(); // 全 0
        let mut data = vec![0.5f32; 4];
        fx.process(&mut data, 1, 48_000, &params2);
        assert!(
            data.iter().all(|v| (v - 0.5).abs() < 1e-5),
            "重置后 0 dB 应直通"
        );
    }
}
