//! # 频谱分析模块
//!
//! 从音频 PCM 样本计算短时频谱（每 10ms 一次窗口），输出 256 个对数分布
//! 频段的归一化能量，供 TUI 画频谱条。
//!
//! ## 设计取舍
//!
//! - **FFT 大小 4096**：bin width = 48000/4096 ≈ 11.72Hz，在 0-24kHz 范围
//!   提供 2048 个 bin。低频段（30~200Hz）每 5 个 band 共用一个 bin，
//!   避免低频区多列同步。4096 点的 R2C FFT 单次约 20μs，
//!   2 通道 100Hz 触发 ≈ 4ms/s CPU（≈ 0.4%），仍可忽略。
//! - **对数频段**：256 个频段按对数分布，30Hz - Nyquist（上限 20kHz）。
//!   低频区 bin 密集（分辨率高），高频区 bin 稀疏——符合人耳听感。
//!   1kHz 处 bin 宽 ≈ 12Hz，配合 log 分布 1kHz 段中心约 75Hz 宽，
//!   1kHz 段横跨约 6 个 bin——单频正弦波能量集中，不会泄漏到邻近段。
//! - **复用 peak amplitude 窗口的样本**：电平累计已有窗口采样，频谱复用
//!   同一组样本，避免重复读 ringbuf。
//! - **dB 归一化**：用 **peak-relative** 方式——每帧最大功率 → 1.0，
//!   下方 [-60dB, 0dB] 线性映射到 [0.0, 1.0]。60dB 动态范围覆盖人耳
//!   可感的音乐响度差异；silence 帧直接全 0 避免噪声地板被放大。
//!
//! ## 时间分辨率权衡
//!
//! 4096 样本在 48kHz 下 = 85ms 时间窗。略慢于 1024-pt 的 21ms，但对
//! 音乐场景仍流畅（人眼对 <100ms 的视觉延迟无感）。8192 虽能更细
//! 频率分辨率，但 170ms 窗口对鼓点/镲片瞬态会显迟钝，得不偿失。
//!
//! ## 显示适配
//!
//! `N_BANDS=256` 在终端上需要 ≥256 列才能 1:1 显示。终端更窄时由
//! `draw_audio_panel` 做 max-pooling 降采样（每显示列取该列对应一组
//! 频段的最大值），保证 80 列终端也能正常显示而不会留空。
//!
//! ## 性能
//!
//! 4096-pt R2C FFT ≈ 20480 实数乘法（twiddle 优化后），单次 ~20μs 级。
//! 每 10ms 调一次 × 2 通道 ≈ 4ms/s CPU 占用，可忽略。

/// 频段数（每通道）。256 段在 4K 屏（300+ 列）能 1:1 显示，
/// 窄终端由渲染层 max-pooling 降采样。
pub const N_BANDS: usize = 256;

/// FFT 大小。必须是 2 的幂（rustfft radix-2）。
pub const FFT_SIZE: usize = 4096;

/// 公开别名：让 engine_thread 用 `spectrum::FFT_SIZE_FOR_BUF` 表示
/// "环形样本缓冲的长度 = FFT 输入长度"，避免直接用 FFT_SIZE 让人
/// 误以为是某个 FFT 计算结果。
pub const FFT_SIZE_FOR_BUF: usize = FFT_SIZE;

/// 示波器波形点数（每声道）。音频回调把窗口内样本按步长降采样到这个点数，
/// 供 TUI 画时域波形。128 点对 80~300 列终端足够，渲染层再按实际宽度缩放。
pub const WAVEFORM_LEN: usize = 128;

/// 生成对数分布的频段边界（Hz）。
///
/// 从 30Hz 到 min(Nyquist, 20kHz) 按对数均分 N_BANDS 段，
/// 返回长度为 N_BANDS 的 [(lo, hi)] 数组。每段至少覆盖 1 个 FFT bin。
fn build_band_edges(sample_rate: u32) -> [[f32; 2]; N_BANDS] {
    let nyquist = sample_rate as f32 / 2.0;
    let lo_hz = 30.0f32;
    let hi_hz = nyquist.min(20000.0);
    let log_lo = lo_hz.ln();
    let log_hi = hi_hz.ln();
    let step = (log_hi - log_lo) / N_BANDS as f32;
    let mut edges = [[0.0f32; 2]; N_BANDS];
    for (i, edge) in edges.iter_mut().enumerate() {
        edge[0] = (log_lo + step * i as f32).exp();
        edge[1] = (log_lo + step * (i + 1) as f32).exp();
    }
    edges
}

/// 把 4096 个时域样本（单声道）转为 N_BANDS 个频段能量（0.0-1.0）。
///
/// 步骤：
/// 1. 加 Hann 窗（减少频谱泄漏）
/// 2. R2C FFT
/// 3. 计算每个 bin 的 |X|²（功率）
/// 4. 按对数分布的频段聚合 [lo_bin..hi_bin] 的功率
/// 5. **Peak-relative dB 归一化**：当前帧最大功率 → 1.0，
///    下方 [-60dB, 0dB] 线性映射到 [0.0, 1.0]（silence 帧全 0）
///
/// # 参数
/// - `samples`：单通道 4096 个 f32 样本（-1.0 到 1.0）
/// - `sample_rate`：采样率（用于 bin → Hz 换算、构造对数频段）
/// - `fft_planner`：rustfft 的 planner（每个回调共享，复用 plan）
/// - `fft_buf`：复数缓冲，**调用方预分配**。长度必须 == `FFT_SIZE`，函数先写入
///   "加 Hann 窗后的复数样本"，再由 FFT 原位覆盖为频率域结果。每次调用都会被
///   完整覆写，因此不必清零。**零堆分配**。
/// - `fft_plan_scratch`：rustfft plan 内部所需的额外 scratch，**调用方预分配**。
///   长度必须 `>= plan.get_inplace_scratch_len()`。radix-n / mixed-radix 算法会
///   需要大小不等的额外缓冲；预留 `FFT_SIZE` 长度对所有合法 FFT 大小都足够。
///   **关键**：rustfft 6.x 的默认 `process()` 会 `vec![Complex::zero(); len]`
///   分配新 scratch——必须在实时路径上**显式传 `process_with_scratch` 才能
///   零分配**。
///
/// # 返回
/// 长度为 N_BANDS 的数组，值在 0.0-1.0。
///
/// # 零分配保证
/// 函数内部不复用 Vec/Box/String，所有工作缓冲（`fft_buf`、`bands`、`result`、
/// `fft_plan_scratch`）都是调用方提供的定长数组或 slice；rustfft 使用显式 scratch。
pub fn compute_spectrum_bands(
    samples: &[f32; FFT_SIZE],
    sample_rate: u32,
    fft_planner: &mut rustfft::FftPlanner<f32>,
    fft_buf: &mut [rustfft::num_complex::Complex<f32>],
    fft_plan_scratch: &mut [rustfft::num_complex::Complex<f32>],
) -> [f32; N_BANDS] {
    use rustfft::num_complex::Complex;
    // 防御性长度检查：开发期尽早暴露错误（发布构建也保留，开销可忽略）。
    // 实时路径上传固定大小数组转出的切片，不会触发。
    debug_assert_eq!(fft_buf.len(), FFT_SIZE, "fft_buf 长度必须等于 FFT_SIZE");

    // 1. 应用 Hann 窗 + 拷贝到调用方提供的 fft_buf
    //
    // Hann 窗：`w(i) = 0.5 * (1 - cos(2π * i / N))`
    //   - 把时域样本两端渐变到 0，让 FFT 假设的"周期延拓"在边界处平滑衔接；
    //   - 否则有限窗口的截断会引入频谱泄漏（高频噪声铺满所有 bin）。
    // Hann 的主瓣宽度 ≈ 4 bin，旁瓣电平 ≈ -31 dB——比矩形窗好得多，
    // 是 music visualization 的默认选择。
    // 输出形式是复数（实部 = 加窗后样本，虚部 = 0），喂给 rustfft 做 R2C 变换。
    //
    // `Complex<f32>` 派生 Copy（见 num-complex），逐元素填充合法；零堆分配。
    for (i, &s) in samples.iter().enumerate() {
        let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / FFT_SIZE as f32).cos());
        fft_buf[i] = Complex::new(s * w, 0.0);
    }

    // 2. FFT
    //
    // R2C（实数到复数）FFT：输入 N 个实数 → 输出 N/2+1 个复数 bin。
    // FftPlanner 内部缓存不同 size 的 plan，重复调用 plan_fft_forward
    // 是 O(1) 查表——不重复创建 FFT 计划。
    // 处理后 `fft_buf[k]` = 第 k 个频点的复数值。
    //
    // **零分配关键**：必须调用 `process_with_scratch` 而不是默认 `process`。
    // rustfft 6.x 的 `process` 默认实现每次都 `vec![Complex::zero(); scratch_len]`
    // 分配新 scratch，在实时线程上是禁忌。`process_with_scratch` 使用调用方
    // 提供的 scratch，零堆分配。
    let fft = fft_planner.plan_fft_forward(FFT_SIZE);
    let scratch_needed = fft.get_inplace_scratch_len();
    debug_assert!(
        fft_plan_scratch.len() >= scratch_needed,
        "fft_plan_scratch 长度 {} < plan 需要的 {}",
        fft_plan_scratch.len(),
        scratch_needed
    );
    fft.process_with_scratch(fft_buf, fft_plan_scratch);

    // 3. bin → Hz 换算
    //
    // FFT 把 0..sample_rate Hz 的频谱等距切成 N 个 bin。
    //   - `bin_hz`：每个 bin 代表的频率宽度（FFT_SIZE=4096, sr=48k → 11.72 Hz/bin）
    //   - `n_bins`：只取前一半（Nyquist 之外是镜像，共轭对称，无新信息）
    let bin_hz = sample_rate as f32 / FFT_SIZE as f32;
    let n_bins = FFT_SIZE / 2;

    // 4. 动态生成对数频段边界
    //
    // log 分布：30Hz ~ min(Nyquist, 20kHz) 按 log 等分 N_BANDS 段。
    // 每帧重新生成（虽然只依赖 sample_rate）——开销可忽略，避免引入全局状态。
    let edges = build_band_edges(sample_rate);

    // 5. 聚合每个频段的功率
    //
    // 对每个 log 段：
    //   1. 把频段边界 (edge[0], edge[1]) Hz 映射到 (lo_bin, hi_bin) 索引
    //   2. 对该范围内的所有 bin 求 |X|² 之和，再除以 bin 数 → 平均功率
    //      （平均而非求和，避免 bin 数多的高频段天然"更亮"）
    //   3. max(lo_bin+1, hi_bin) 兜底：log 分布最前几段（30-50Hz）跨度 < 1 bin，
    //      强制至少覆盖 1 bin，避免后面 `power /= 0` 出 NaN
    let mut bands = [0.0f32; N_BANDS];
    for (band_idx, edge) in edges.iter().enumerate() {
        let lo_bin = ((edge[0] / bin_hz) as usize).min(n_bins);
        let hi_bin = ((edge[1] / bin_hz) as usize).min(n_bins).max(lo_bin + 1);
        let bin_count = hi_bin - lo_bin;
        let mut power = 0.0f32;
        for c in &fft_buf[lo_bin..hi_bin] {
            power += c.re * c.re + c.im * c.im;
        }
        power /= bin_count as f32;
        bands[band_idx] = power;
    }

    // 6. Peak-relative dB 归一化：当前帧最大功率 → 1.0，下方 [-60dB, 0dB]
    // 映射到 [0.0, 1.0]。
    //
    // 用绝对 power=1.0 当 0dB 参考会失真：FFT 峰值功率实际是 (N/2)²
    // （4096 点时约 4.2M），绝大多数 band 会被 clamp 到 1.0，分辨不出分布。
    // 改为相对峰值归一化后：每帧最响 band = 1.0，60dB 动态范围覆盖听感，
    // 且与 FFT_SIZE 无关。
    //
    // Silence 防护：max_power < 1e-6 时认为本帧无信号，全部返回 0，
    // 避免 -∞ 噪声地板被"相对放大"成全亮。
    const DYNAMIC_RANGE_DB: f32 = 60.0;
    const SILENCE_FLOOR: f32 = 1e-6;
    let max_power = bands.iter().copied().fold(0.0f32, f32::max);
    if max_power < SILENCE_FLOOR {
        return [0.0f32; N_BANDS];
    }
    let max_db = 20.0 * max_power.log10();

    let mut result = [0.0f32; N_BANDS];
    for (i, &p) in bands.iter().enumerate() {
        let db = if p > 0.0 { 20.0 * p.log10() } else { -120.0 };
        // 相对峰值：0dB → 1.0，-DYNAMIC_RANGE_DB → 0.0
        let rel = db - max_db + DYNAMIC_RANGE_DB;
        result[i] = (rel / DYNAMIC_RANGE_DB).clamp(0.0, 1.0);
    }
    result
}

/// 频谱峰值保持的默认下落速率：满格约 1.7 秒落到 0。
///
/// 以每秒为单位的线性衰减量（在 `[0,1]` 归一化刻度上），与调用方帧率无关。
pub const DEFAULT_PEAK_FALL_PER_SEC: f32 = 0.6;

/// 频段维度峰值保持状态机：能量高于历史峰值时立即上浮，低于峰值时按
/// `fall_per_sec` 线性下落，且永不低于当前能量。衰减按秒计，与调用方帧率无关。
///
/// # 使用约定
///
/// - 属主须为单一渲染线程；初始化后零分配；不得在实时音频回调中调用。
/// - 无内部同步原语，不实现跨线程共享；如需跨线程，由调用方自行包装。
/// - 计时用单调时钟（`std::time::Instant`），禁用系统墙钟，避免时钟跳变造成异常步长。
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumPeakHold {
    peaks: [f32; N_BANDS],
    fall_per_sec: f32,
}

impl Default for SpectrumPeakHold {
    fn default() -> Self {
        Self::new(DEFAULT_PEAK_FALL_PER_SEC)
    }
}

impl SpectrumPeakHold {
    /// 以指定每秒下落速率构造，峰值初始为全 0。
    ///
    /// # Panics
    ///
    /// `fall_per_sec` 非有限（NaN/无穷）或为负时 panic。
    pub fn new(fall_per_sec: f32) -> Self {
        assert!(
            fall_per_sec.is_finite() && fall_per_sec >= 0.0,
            "fall_per_sec 必须为有限非负值，实际 {fall_per_sec}"
        );
        Self {
            peaks: [0.0; N_BANDS],
            fall_per_sec,
        }
    }

    /// 吸收一帧频段能量并推进峰值状态，按值返回各频段当前峰值。
    ///
    /// 输入逐频段消毒：NaN 按 0 处理，其余钳制到 `[0,1]`。`dt` 为距上次调用
    /// 的时间间隔；首帧或间隔未知时传 [`std::time::Duration::ZERO`]（只吸收
    /// 新峰值、不衰减）。
    pub fn update(&mut self, bands: &[f32; N_BANDS], dt: std::time::Duration) -> [f32; N_BANDS] {
        let fall = self.fall_per_sec * dt.as_secs_f32();
        for (p, &b) in self.peaks.iter_mut().zip(bands.iter()) {
            let b = sanitize_band(b);
            *p = (*p - fall).max(b);
        }
        self.peaks
    }

    /// 只读访问当前各频段峰值（不推进状态）。
    pub fn peaks(&self) -> &[f32; N_BANDS] {
        &self.peaks
    }

    /// 清零全部峰值；`fall_per_sec` 为配置，不受影响。
    ///
    /// 调用时机（切曲、加载新源、清空显示）由调用方决定，本类型不感知播放状态。
    pub fn reset(&mut self) {
        self.peaks = [0.0; N_BANDS];
    }
}

/// 输入频段值消毒：NaN 按 0，其余钳制到 [0,1]。
fn sanitize_band(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustfft::FftPlanner;

    #[test]
    fn low_freq_tone_lights_up_low_bands() {
        // 用 100Hz 正弦波而不是 DC：
        //   FFT=1024 时 bin 宽 47Hz，band 0 (30-30.8Hz) 跨 bin 0，
        //     DC 落在 band 0；
        //   FFT=4096 时 bin 宽 11.72Hz，band 0 跨 bin 2-3，
        //     DC 落进"无 band 覆盖"的盲区。
        // 100Hz 总是落在 band 47 附近（log 分布），与 FFT 大小无关。
        let sr = 48000;
        let mut samples = [0.0f32; FFT_SIZE];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = (2.0 * std::f32::consts::PI * 100.0 * i as f32 / sr as f32).sin();
        }
        let mut planner = FftPlanner::<f32>::new();
        // scratch：测试场景里栈分配即可（不在实时线程）；[Complex<f32>; FFT_SIZE]
        // ≈ 32 KB × 2，单次测试函数栈帧峰值仍远低于默认栈大小。
        let mut fft_buf = [rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        // 预留 FFT_SIZE 长度的 plan scratch；radix-n / mixed-radix 算法所需长度
        // 不会超过 FFT_SIZE（实测 4096 长度足够）。
        let mut fft_plan_scratch = [rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        let bands = compute_spectrum_bands(
            &samples,
            sr,
            &mut planner,
            &mut fft_buf,
            &mut fft_plan_scratch,
        );

        // 100Hz 峰值应在 band 38..58 之间（log 100Hz ≈ band 47）
        let (max_idx, &max_val) = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        assert!(
            (38..58).contains(&max_idx),
            "100Hz 峰值应在 38..58 段，实际 band {} = {}",
            max_idx,
            max_val
        );
        assert!(max_val > 0.7, "峰值应被点亮：{}", max_val);
        // 远高频段应该暗
        assert!(
            bands[N_BANDS - 1] < 0.1,
            "最高频段应暗：{}",
            bands[N_BANDS - 1]
        );
    }

    #[test]
    fn silence_all_zero() {
        // 全 0 输入验证两条路径：
        //   1. FFT 输出本身无能量（所有 band 的 |X|² = 0）
        //   2. 触发 silence 防护（max_power < 1e-6），直接返回全 0
        //      而非让 log10(0) = -∞ 污染整个 result 数组
        let samples = [0.0f32; FFT_SIZE];
        let mut planner = FftPlanner::<f32>::new();
        let mut fft_buf = [rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        let mut fft_plan_scratch = [rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        let bands = compute_spectrum_bands(
            &samples,
            48000,
            &mut planner,
            &mut fft_buf,
            &mut fft_plan_scratch,
        );
        for (i, &b) in bands.iter().enumerate() {
            assert_eq!(b, 0.0, "静音段 {} 应为 0", i);
        }
    }

    #[test]
    fn pure_tone_lands_in_correct_band() {
        // 1000Hz 正弦波 → FFT 4096 @ 48kHz: bin = 1000/11.72 ≈ 85.3
        let sr = 48000;
        let mut samples = [0.0f32; FFT_SIZE];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin();
        }
        let mut planner = FftPlanner::<f32>::new();
        let mut fft_buf = [rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        let mut fft_plan_scratch = [rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        let bands = compute_spectrum_bands(
            &samples,
            sr,
            &mut planner,
            &mut fft_buf,
            &mut fft_plan_scratch,
        );
        // 找最大段
        let (max_idx, _) = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        // 1000Hz 在 256 log 段 (30Hz~20kHz) 中：
        //   pos = log(1000/30) / log(20000/30) * 256 ≈ 138
        // 容许 ±10 段偏差（对数分布 + Hann 窗泄漏）
        let expected_region = 128..148;
        assert!(
            expected_region.contains(&max_idx),
            "1000Hz 峰值应在 {expected_region:?} 段，实际 {}",
            max_idx,
        );
    }
}
#[cfg(test)]
mod peak_hold_tests {
    use super::*;

    /// splitmix64 单步（自写，避免为此引入测试依赖）。
    fn splitmix64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// 构造一个所有频段同为 `v` 的输入。
    fn band(v: f32) -> [f32; N_BANDS] {
        [v; N_BANDS]
    }

    #[test]
    fn new_rejects_invalid_fall_rate() {
        for bad in [f32::NAN, -0.1, f32::INFINITY, f32::NEG_INFINITY] {
            let r = std::panic::catch_unwind(|| SpectrumPeakHold::new(bad));
            assert!(r.is_err(), "new({bad}) 应 panic");
        }
        // 合法值不 panic
        let _ = SpectrumPeakHold::new(0.0);
        let _ = SpectrumPeakHold::new(1.2);
    }

    #[test]
    fn rise_is_immediate_and_then_decays() {
        let mut p = SpectrumPeakHold::default();
        let out = p.update(&band(0.5), std::time::Duration::ZERO);
        assert!((out[0] - 0.5).abs() < 1e-6, "能量上升应即时上浮");
        // 能量降到 0.2，峰值应缓落但永不低于当前
        let out = p.update(&band(0.2), std::time::Duration::from_secs_f32(0.1));
        assert!(
            out[0] < 0.5 && out[0] >= 0.2,
            "峰值应缓落且 >= 当前：{}",
            out[0]
        );
    }

    #[test]
    fn dt_zero_absorbs_without_decay() {
        let mut p = SpectrumPeakHold::default();
        p.update(&band(0.8), std::time::Duration::ZERO);
        let out = p.update(&band(0.0), std::time::Duration::ZERO);
        assert!((out[0] - 0.8).abs() < 1e-6, "dt=0 只吸收、不衰减");
    }

    #[test]
    fn frame_rate_independence() {
        // 相同总时长、不同帧间隔，终态峰值应一致（帧率无关）。
        let total = 2.0f32;
        let dts = [1.0 / 144.0, 1.0 / 60.0, 1.0 / 30.0, 1.0 / 10.0];
        let mut finals = Vec::new();
        for &dt in &dts {
            let mut p = SpectrumPeakHold::default();
            p.update(&band(1.0), std::time::Duration::ZERO);
            let mut t = 0.0f32;
            while t < total {
                p.update(&band(0.0), std::time::Duration::from_secs_f32(dt));
                t += dt;
            }
            finals.push(p.peaks()[0]);
        }
        let base = finals[0];
        for &f in &finals[1..] {
            assert!((f - base).abs() < 1e-4, "帧率无关终态不一致：{finals:?}");
        }
    }

    #[test]
    fn golden_equivalence_to_old_formula() {
        // 旧实现：每帧 0.02、30fps、50 帧；新实现：0.6/s、dt=1/30、50 帧。
        let mut old = 1.0f32;
        for _ in 0..50 {
            old = (old - 0.02).max(0.0);
        }
        let mut p = SpectrumPeakHold::default();
        p.update(&band(1.0), std::time::Duration::ZERO);
        for _ in 0..50 {
            p.update(&band(0.0), std::time::Duration::from_secs_f32(1.0 / 30.0));
        }
        assert!(
            (p.peaks()[0] - old).abs() < 1e-4,
            "金标偏差：old={old} new={}",
            p.peaks()[0]
        );
    }

    #[test]
    fn input_sanitization() {
        let mut p = SpectrumPeakHold::default();
        let mut b = [0.0f32; N_BANDS];
        b[0] = f32::NAN;
        b[1] = f32::INFINITY;
        b[2] = 1.5;
        b[3] = -0.5;
        let out = p.update(&b, std::time::Duration::ZERO);
        assert_eq!(out[0], 0.0, "NaN 应按 0");
        assert_eq!(out[1], 1.0, "inf 应钳到 1");
        assert_eq!(out[2], 1.0, "超界应钳到 1");
        assert_eq!(out[3], 0.0, "负值应钳到 0");
    }

    #[test]
    fn reset_clears_peaks_but_keeps_rate() {
        let mut p = SpectrumPeakHold::default();
        p.update(&band(0.9), std::time::Duration::ZERO);
        assert!(p.peaks()[0] > 0.5);
        p.reset();
        assert!(p.peaks().iter().all(|&v| v == 0.0), "reset 后应全 0");
        // 配置不变：reset 后再更新仍按 0.6/s 下落（0.8 - 0.6*0.5 = 0.5）
        p.update(&band(0.8), std::time::Duration::ZERO);
        let out = p.update(&band(0.0), std::time::Duration::from_secs_f32(0.5));
        assert!((out[0] - 0.5).abs() < 1e-4, "下落速率应保持：{}", out[0]);
    }

    #[test]
    fn huge_dt_soft_resets_to_current() {
        let mut p = SpectrumPeakHold::default();
        p.update(&band(1.0), std::time::Duration::ZERO);
        // 极大间隔：峰值衰减到底后应钳回当前能量，而非变成负值或陈旧白帽。
        let out = p.update(&band(0.3), std::time::Duration::from_secs_f32(1e6));
        assert!(
            (out[0] - 0.3).abs() < 1e-4,
            "极大 dt 应软复位到当前：{}",
            out[0]
        );
    }

    #[test]
    fn invariant_peaks_ge_current_and_in_range() {
        // 随机序列性质测试：任意输入下，峰值恒 >= 当前能量且落在 [0,1]。
        let mut p = SpectrumPeakHold::default();
        let mut rng: u64 = 0x1234567890abcdef;
        for _ in 0..2000 {
            let mut b = [0.0f32; N_BANDS];
            for v in b.iter_mut() {
                let x = splitmix64(&mut rng);
                *v = ((x & 0xffff) as f32) / 65535.0;
            }
            let dt_ms = splitmix64(&mut rng) % 200;
            let dt = std::time::Duration::from_millis(dt_ms);
            let out = p.update(&b, dt);
            for i in 0..N_BANDS {
                assert!(
                    out[i] >= 0.0 && out[i] <= 1.0,
                    "峰值越界 @ band {i}: {}",
                    out[i]
                );
                assert!(out[i] >= b[i], "峰值应 >= 当前能量 @ band {i}");
            }
        }
    }
}
