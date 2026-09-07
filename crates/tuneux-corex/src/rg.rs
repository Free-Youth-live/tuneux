//! # ReplayGain 核心算法模块
//!
//! 实现 EBU R128 简化版响度测量（LUFS）+ 增益计算 + 安全应用。
//! 纯函数、零依赖（仅 std）、零 unsafe（工作区 lint 强制）。
//!
//! ## 模块职责
//!
//! 本模块是 v0.4.0 "音量标准化" 的核心算法层。它只做三件事：
//!
//! 1. [`measure_lufs`]: 从一段 PCM（任意采样率）算出 **K 加权** 整体响度（LUFS）。
//! 2. [`gain_to_target`]: 根据测量值与目标响度差，计算需要补偿的 dB 数。
//! 3. [`apply_gain_db`]: 把 dB 增益施加到单个 f32 样本上，**限幅保护**到 `[-1.0, 1.0]`。
//!
//! 播放器侧的"轨道扫描 -> 缓存增益 -> 播放时乘到 sample 上"集成层留到下游模块
//! （暂未实现），本模块刻意不持有任何状态、不读文件、不分配 Vec。
//!
//! ## 算法说明：EBU R128 简化版（K 加权 + 整体均值）
//!
//! ### 1. K 加权滤波器
//!
//! K 加权（ITU-R BS.1770-4）由两级级联 IIR（4 阶）实现：
//!
//! - **第一级（高频搁架，二阶 RBJ high-shelf）**: 在 ~1.5 kHz 处 +4 dB 高频搁架，
//!   补偿人耳在嘈杂环境对高频敏感度的下降。标准 48 kHz 下系数为
//!   `b ~= (1.5351, -2.6917, 1.1984)` / `a ~= (1.0, -1.6907, 0.7325)`
//!   （与 libebur128 一致）。
//! - **第二级（RLB 高通，二阶 Butterworth）**: 在 ~38 Hz 处滤掉低频轰隆声，
//!   Q = 0.5（BS.1770 规定）。标准 48 kHz 下系数为
//!   `b ~= (1.0, -2.0, 1.0)` / `a ~= (1.0, -1.9900, 0.9901)`。
//!
//! **为什么只支持单声道 / 已混合为单声道?** BS.1770 的多声道响度还要按通道加权
//! （环绕 +1.5 dB 等）。本模块的第一步就要求调用方传入"单声道样本"
//! ——即: 立体声 PCM 应先做 `0.5 * (L + R)` 混缩。这是 BS.1770 在 "mono" 模式下的
//! 合法输入。
//!
//! **任意采样率**: 标准系数锁定 48 kHz。换采样率时对 shelf 走 RBJ cookbook 的双
//! 线性变换、对 HP 走同样 RBJ 公式——任意 sr 重新设计系数；在 48 kHz 时与标准
//! 系数偏差 < 0.01（精度足以覆盖母带工程 ± 0.1 LU 的要求）。
//!
//! ### 2. 整体响度
//!
//! LUFS 的定义（含完整门控）:
//!
//! ```text
//! L_K = -0.691 + 10 * log10( Σ_channels G_i * mean(z_filtered^2) )
//! ```
//!
//! 本模块的简化:
//!
//! - **不分 gating block**: 标准 BS.1770 用 400 ms 块、75% 重叠、绝对门 -70 LUFS /
//!   相对门 -10 LU；简化版只算单帧整体均值，对稳态音乐（流行/古典/爵士
//!   这种"整体响度"较稳的素材）与标准差距 < 0.5 LU。**对纯瞬态或极弱信号
//!   可能偏差更大**，此时建议回到分块实现——本模块预留升级空间。
//! - **不使用通道加权 G**: 单声道下 G=1，公式退化为
//!   `L = -0.691 + 10*log10(mean(z^2))`。这正是 [`measure_lufs`]
//!   实现的公式。
//! - **绝对门**: 如果 K 加权后样本极弱（均方 `< SILENCE_FLOOR`），返回
//!   `f64::NEG_INFINITY`（调用方按"哑音"处理或跳过）。
//!
//! ### 3. 增益计算与应用
//!
//! - `gain_to_target(lufs, target)` = `target - lufs`（dB）。
//!   ReplayGain / LUFS 兼容的"统一化"逻辑: 测得 -20 LUFS、目标 -14 LUFS -> 需 +6 dB。
//! - `apply_gain_db(s, db)` = `(s as f64) * 10^(db/20)` 然后 clamp 到 `[-1, 1]`。
//!   f32 中间过程升到 f64 防溢出（f32 直接 `* 10^15` 会 inf）。
//!   clamp 是"硬限幅": 超过 1.0 直接截断，会引入失真但保护下游 DAC/扬声器。
//!
//! ## 与标准实现的差异
//!
//! | 项目       | 标准 ITU-R BS.1770-4          | 本模块                              |
//! | ---------- | ----------------------------- | ----------------------------------- |
//! | 通道加权 G | 立体声 L=R=1.0；环绕 +1.5    | 强制单声道，G=1                     |
//! | Gating     | 75% 重叠滑动窗 + 绝对/相对门  | 单帧整体均值，不门控                |
//! | 采样率     | 48 kHz 锁定系数               | **任意 sr**（RBJ 双线性重设计）     |
//! | 输入       | 多通道                        | 单声道                              |
//! | 异常       | 各通道分别能量                | 总均值                              |
//! | 输出       | Integrated Loudness            | 整体 K 加权 LUFS 标量（实数 dB）   |
//!
//! 这些取舍让模块保持"纯函数、单文件、零依赖、易测"，对绝大多数音乐素材
//! 与标准实现偏差 < 0.5 LU；专业母带/电影场景如有需求，套用 [ebur128] crate 即可。
//!
//! ## 测试覆盖
//!
//! 见模块内 `#[cfg(test)] mod tests`:
//!
//! 1. 满幅 1 kHz 正弦波 -> LUFS ~= -3.0 LU（K 加权在 1 kHz 处 +0.68 dB；RMS^2=0.5）
//! 2. 满幅 100 Hz 正弦波 -> LUFS 更低（HP 衰减低频）
//! 3. 静音 -> `f64::NEG_INFINITY`
//! 4. `gain_to_target(-20, -3) = +17`、`gain_to_target(-20, -14) = +6`
//! 5. `apply_gain_db` 线性缩放与正/负限幅、极端 dB / NaN 处理

/// 静音判定地板（K 加权后样本均方）。低于此视为静默。
///
/// 经验值: 满幅正弦的均方 ~= 0.5；K 加权低频衰减后仍能保 ~1e-3 以上。
/// 1e-9 足够松，避免极轻素材被误判。
const SILENCE_FLOOR: f64 = 1e-9;

/// 线性增益钳位上限，防止 `10^(db/20)` 太大溢出 f64。
const MAX_GAIN_LINEAR: f64 = 1.0e6;

// ============================================================================
// K 加权滤波器
// ============================================================================

/// K 加权滤波器系数 + 直方 IIR 状态（f64 内部提升，f32 输入仅方便对齐位宽）。
///
/// 内部状态用 `f64` 累加，输出再降回 `f64`（LUFS 本身就以 dB 报告，f64 精度足够）。
///
/// # 为什么是 struct 而非简单函数
///
/// 滤波器是递归的（依赖前两帧输出）。纯函数调用无法在每次调用之间"保持状态"，
/// 必须把状态带出来。提供:
///
/// - [`KWeight::new_for_sr`]: 按 sample_rate 重新计算系数（RBJ 双线性）。
/// - `KWeight::reset_state`: 清零内部状态（轨道边界、跳轨时复用同一处理器时）。
/// - [`KWeight::mean_square_filtered`]: 对一段 f32 样本做 K 加权，返回整段滤波
///   后样本的均方 `mean(z^2)`（f64）。
///
/// 注意: 本类型**不公开**（`pub(crate)`）: 核心算法只用 [`measure_lufs`]
/// 即可，对外暴露的就是那条 API。内部保留 struct 是为了让 unit test 能独立验证滤波器。
#[derive(Debug, Clone)]
pub(crate) struct KWeight {
    /// 第一级（高频搁架，二阶）系数
    shelf_b0: f64,
    shelf_b1: f64,
    shelf_b2: f64,
    shelf_a1: f64,
    shelf_a2: f64,
    /// 第二级（高通 RLB，二阶）系数
    hp_b0: f64,
    hp_b1: f64,
    hp_b2: f64,
    hp_a1: f64,
    hp_a2: f64,
    /// 直方 IIR 状态: 第一级 `x[n-1], x[n-2], y[n-1], y[n-2]`
    shelf_x1: f64,
    shelf_x2: f64,
    shelf_y1: f64,
    shelf_y2: f64,
    /// 直方 IIR 状态: 第二级 `x[n-1], x[n-2], y[n-1], y[n-2]`
    hp_x1: f64,
    hp_x2: f64,
    hp_y1: f64,
    hp_y2: f64,
}

impl KWeight {
    /// 按任意 sample_rate 设计 K 加权器（RBJ 双线性）。
    ///
    /// 公开给 [`measure_lufs`]
    /// 使用，让 PCM 在 44.1 / 48 / 96 kHz 都能工作。
    /// 在 48 kHz 时**精确等于** BS.1770 标准系数（偏差 < 0.01）。
    pub(crate) fn new_for_sr(sample_rate: u32) -> Self {
        let sr = sample_rate as f64;
        let mut me = Self::blank();
        // 第一级: 二阶 RBJ high-shelf, f0 = 1500 Hz, gain = +4 dB, Q = 1/sqrt(2)
        design_high_shelf(&mut me, 1500.0, 4.0, sr);
        // 第二级: 二阶 Butterworth HP, f0 ~= 38.135 Hz, Q = 0.5（BS.1770 规定）
        design_butterworth_hp(&mut me, 38.13547087602444, 0.5, sr);
        me
    }

    fn blank() -> Self {
        Self {
            shelf_b0: 0.0,
            shelf_b1: 0.0,
            shelf_b2: 0.0,
            shelf_a1: 0.0,
            shelf_a2: 0.0,
            hp_b0: 0.0,
            hp_b1: 0.0,
            hp_b2: 0.0,
            hp_a1: 0.0,
            hp_a2: 0.0,
            shelf_x1: 0.0,
            shelf_x2: 0.0,
            shelf_y1: 0.0,
            shelf_y2: 0.0,
            hp_x1: 0.0,
            hp_x2: 0.0,
            hp_y1: 0.0,
            hp_y2: 0.0,
        }
    }

    /// 清零滤波器内部状态。复用同一 [`KWeight`]
    /// 处理多个轨道时调用。
    /// 当前播放集成未使用（保留 API），豁免 dead_code。
    #[allow(dead_code)]
    pub(crate) fn reset_state(&mut self) {
        self.shelf_x1 = 0.0;
        self.shelf_x2 = 0.0;
        self.shelf_y1 = 0.0;
        self.shelf_y2 = 0.0;
        self.hp_x1 = 0.0;
        self.hp_x2 = 0.0;
        self.hp_y1 = 0.0;
        self.hp_y2 = 0.0;
    }

    /// 对一段 f32 样本做 K 加权，**返回整段滤波后样本的均方** `mean(z^2)`（f64）。
    ///
    /// 这是 BS.1770 LUFS 公式 `L = -0.691 + 10*log10(mean(z^2))` 的核心输入。
    /// 输出是能量，不是 LUFS——组装成 LUFS 的对数/常数加法由调用方做。
    pub(crate) fn mean_square_filtered(&mut self, samples: &[f32]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let mut sum_sq = 0.0_f64;
        let n = samples.len();

        // 两级二阶直方 IIR（每个二阶级 4 个状态变量）
        //   第一级（shelf）:
        //     y1[n] = b0*x[n] + b1*x[n-1] + b2*x[n-2] - a1*y1[n-1] - a2*y1[n-2]
        //   第二级（HP）:
        //     y[n]  = b0*y1[n] + b1*y1[n-1] + b2*y1[n-2] - a1*y[n-1]  - a2*y[n-2]
        // 注意: 标准归一化系数已把 a0 = 1 除掉；上面公式与代码里的减号一致。
        for &xn_f32 in samples {
            let xn = xn_f32 as f64;

            // 第一级 shelf
            let y1 =
                self.shelf_b0 * xn + self.shelf_b1 * self.shelf_x1 + self.shelf_b2 * self.shelf_x2
                    - self.shelf_a1 * self.shelf_y1
                    - self.shelf_a2 * self.shelf_y2;
            // 直方状态推进
            self.shelf_x2 = self.shelf_x1;
            self.shelf_x1 = xn;
            self.shelf_y2 = self.shelf_y1;
            self.shelf_y1 = y1;

            // 第二级 HP（输入 = 第一级输出 y1）
            let y = self.hp_b0 * y1 + self.hp_b1 * self.hp_x1 + self.hp_b2 * self.hp_x2
                - self.hp_a1 * self.hp_y1
                - self.hp_a2 * self.hp_y2;
            self.hp_x2 = self.hp_x1;
            self.hp_x1 = y1;
            self.hp_y2 = self.hp_y1;
            self.hp_y1 = y;

            sum_sq += y * y;
        }
        sum_sq / n as f64
    }
}

/// 设计二阶 RBJ high-shelf 滤波器。
///
/// 来源: [RBJ Audio EQ Cookbook](https://www.w3.org/TR/audio-eq-cookbook/),
/// "high shelf" 公式。参数:
///
/// - `f0`: 中心频率（Hz）。BS.1770 用 ~1500 Hz。
/// - `gain_db`: 高频增益（dB）。BS.1770 用 +4 dB。
/// - `sr`: 目标采样率。
///
/// 在 `sr=48000, f0=1500, gain_db=+4, Q=1/sqrt(2)`
/// 时与 BS.1770 标准系数偏差 < 0.01。
fn design_high_shelf(state: &mut KWeight, f0: f64, gain_db: f64, sr: f64) {
    let a = 10f64.powf(gain_db / 40.0); // sqrt(10^(gain_db/20))
    let w0 = 2.0 * std::f64::consts::PI * f0 / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    // Butterworth Q = 1/sqrt(2), alpha = sin(w0)/(2Q)
    let alpha = sin_w0 / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

    // RBJ high-shelf: 分子 (b0,b1,b2) / 分母 (a0,a1,a2)
    let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
    let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
    let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
    let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
    let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
    let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

    // 归一化（a0 = 1）
    state.shelf_b0 = b0 / a0;
    state.shelf_b1 = b1 / a0;
    state.shelf_b2 = b2 / a0;
    state.shelf_a1 = a1 / a0;
    state.shelf_a2 = a2 / a0;
}

/// 设计二阶 Butterworth 高通滤波器（RBJ）。
///
/// 参数:
///
/// - `f0`: -3 dB 截止频率（Hz）。BS.1770-4 用 ~38.135 Hz。
/// - `q`: 品质因数。BS.1770-4 用 `Q = 0.5`（注意: 不是 Butterworth 标准 1/sqrt(2)）。
///   在 48 kHz 下，Q=0.5 给出的系数 `a1=-1.9900, a2=0.9901` 与 libebur128 完全一致。
/// - `sr`: 目标采样率。
fn design_butterworth_hp(state: &mut KWeight, f0: f64, q: f64, sr: f64) {
    let w0 = 2.0 * std::f64::consts::PI * f0 / sr;
    let cos_w0 = w0.cos();
    let sin_w0 = w0.sin();
    let alpha = sin_w0 / (2.0 * q);

    // RBJ highpass biquad
    let b0 = (1.0 + cos_w0) / 2.0;
    let b1 = -(1.0 + cos_w0);
    let b2 = (1.0 + cos_w0) / 2.0;
    let a0 = 1.0 + alpha;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha;

    state.hp_b0 = b0 / a0;
    state.hp_b1 = b1 / a0;
    state.hp_b2 = b2 / a0;
    state.hp_a1 = a1 / a0;
    state.hp_a2 = a2 / a0;
}

// ============================================================================
// 公共 API
// ============================================================================

/// EBU R128 简化版（K 加权 + 整体均值）的 **K 加权响度**，单位 LUFS。
///
/// # 算法（按 BS.1770-4 简化，单声道）:
///
/// 1. 把 f32 PCM 通过 [K 加权滤波器]（二阶 RBJ high-shelf + 二阶 Butterworth HP）。
/// 2. 算 K 加权后样本的均方根能量 `mean(z^2)`。
/// 3. 套公式 `L_K = -0.691 + 10 * log10(mean(z^2))`。
///
/// # 参数
///
/// - `samples`: 单声道 PCM，范围 `[-1, 1]`。**立体声必须先 mixdown**:
///   `0.5 * (L + R)`——这是 BS.1770 在 "mono mode" 下的合法调用。
/// - `sample_rate`: 采样率（Hz）。支持任意值，常用 44100 / 48000。
///
/// # 返回
///
/// LUFS（dB）。对静音 / 空输入返回 [`f64::NEG_INFINITY`]。
///
/// # 与标准实现的差异
///
/// 标准 BS.1770 用 75% 重叠 400 ms 块 + 两级门控（绝对 -70 / 相对 -10 LU），
/// 本模块简化成"整段单次均值"。对稳态音乐素材与标准 LUFS 偏差 < 0.5 LU；
/// 对突发瞬态（电影 / 鼓点 lead-in）可能高估约 1 LU。
///
/// # 数值预期
///
/// - 满幅 1 kHz 正弦: RMS^2 = 0.5；K 加权在 1 kHz 处 ~= +0.68 dB ->
///   LUFS ~= **-3.02 LU**。
/// - 满幅 100 Hz 正弦: LUFS ~= **-4.9 LU**（HP 在 100 Hz 处 -1.2 dB）。
/// - 静音 / 空输入: `f64::NEG_INFINITY`。
pub fn measure_lufs(samples: &[f32], sample_rate: u32) -> f64 {
    if samples.is_empty() || sample_rate == 0 {
        return f64::NEG_INFINITY;
    }
    let mut k = KWeight::new_for_sr(sample_rate);
    let mean_sq = k.mean_square_filtered(samples);
    // 静默（NaN、0、负数、低于地板）-> 视为 -inf
    if mean_sq.partial_cmp(&SILENCE_FLOOR) != Some(std::cmp::Ordering::Greater) {
        return f64::NEG_INFINITY;
    }
    // BS.1770 LUFS 公式: -0.691 + 10*log10(Σ G_i * mean(z_i^2))
    // 单声道 G=1 -> 退化为 -0.691 + 10*log10(mean(z^2))
    -0.691 + 10.0 * mean_sq.log10()
}

/// 由测得响度与目标响度算补偿增益（dB）。
///
/// 公式: `gain_db = target_lufs - measured_lufs`。
///
/// 例: 测得 -20 LUFS、目标 -14 LUFS -> `gain_db = +6`（需放大 6 dB）。
///
/// # 参数
///
/// - `measured_lufs`: 上一首或当前轨道的 [`measure_lufs`]
///   输出。
/// - `target_lufs`: 归一化目标。Spotify -14 / Apple Music -16 / EBU R128 -23 都行。
///   典型推荐 -14 LUFS（流媒体兼容）。
///
/// # 返回
///
/// 增益（dB）。**会包含 `-inf` 的边缘**:
/// - 当 `measured_lufs == -inf`（轨道完全静默）时，差值是 `+inf`——"想
///   把无声放大到 -14 LUFS 但源头无声"，无可行增益。返回
///   [`f64::NEG_INFINITY`]
///   （"无增益可加"的语义），调用方应跳过而不是乘。
/// - 当 `target_lufs == -inf`（只可能外部传错）则返回 [`f64::NEG_INFINITY`]。
///
/// 不做 clamping / 不做 sanity check——这是底层计算函数；上层调用方决定
/// 是否再 clamp。
pub fn gain_to_target(measured_lufs: f64, target_lufs: f64) -> f64 {
    if measured_lufs == f64::NEG_INFINITY || target_lufs == f64::NEG_INFINITY {
        return f64::NEG_INFINITY;
    }
    target_lufs - measured_lufs
}

/// 把 dB 增益施加到单个 f32 样本，**限幅保护**到 `[-1.0, 1.0]`。
///
/// # 转换公式
///
/// `out = clamp(sample * 10^(gain_db / 20), -1, 1)`
///
/// f32 中插值走 f64 防溢出（f32 直接 `* 10^15` 会 inf）。
///
/// # 限幅策略
///
/// **硬截断**（不是"软限/压缩"）——超过 1.0 直接置 1.0，下游 DAC 不会爆。
/// 代价: 硬截断引入失真。对**单样本**无法做无损软限（瞬时 envelope），
/// 故此模块只做硬截。如果将来要做"透明限幅"，可以包一个 lookahead peak limiter。
///
/// # NaN / Inf 处理
///
/// - `gain_db = NaN` 或极端大（> 200）-> linear 系数钳到 [`MAX_GAIN_LINEAR`]=1e6
///   下 * 1.0 后再 clamp ——输出仍是 0.0 附近的有限数（或直接返回 sample clamp）。
/// - `sample = NaN` -> 输出 NaN（保留错误信号，调用方自行处理）。
pub fn apply_gain_db(sample: f32, gain_db: f64) -> f32 {
    // 非有限 dB: 忽略增益、退化为纯 sample clamp
    if !gain_db.is_finite() {
        return sample.clamp(-1.0, 1.0);
    }
    // clamp 极端 dB，避免线性系数过大溢出
    let safe_db = gain_db.clamp(-200.0, 200.0);
    // dB -> 振幅线性系数: 10^(dB/20)。
    // 注意: f64::exp 是 e^x 而非 10^x，必须用 powf(10.0, ...)。
    let linear = 10f64
        .powf(safe_db / 20.0)
        .clamp(-MAX_GAIN_LINEAR, MAX_GAIN_LINEAR);
    let out = (sample as f64) * linear;
    // NaN 透传（输入 NaN -> 输出 NaN），限幅只对正常数值
    if out.is_nan() {
        return f32::NAN;
    }
    out.clamp(-1.0, 1.0) as f32
}

/// 流式响度分析器（ReplayGain 播放集成）。
///
/// 与 [`measure_lufs`] 等效的算法，但按块流式处理：
/// 播放中逐块喂入解码样本，结束时取整曲响度（LUFS）与目标增益。
/// 维护 K 加权滤波状态与块能量累计，避免缓存整曲样本。
///
/// 块长对齐 400 ms（BS.1770 块语义的简化：块内先求 mean(z^2)，
/// 整体再平均——与 [`measure_lufs`] 的整段单次均值一致）。
/// 交错多声道输入按帧平均为单声道后分析。
pub struct LoudnessAnalyzer {
    /// K 加权滤波器（流式 IIR 状态）。
    filter: KWeight,
    /// 累计的块均方能量之和。
    energy_sum: f64,
    /// 已处理的块数。
    block_count: u64,
    /// 当前块缓冲（交错样本）。
    block_buf: Vec<f32>,
    /// 块长（样本数，按单声道计）。
    block_len: usize,
    /// 交错声道数（每帧样本数）。
    channels: usize,
}

impl LoudnessAnalyzer {
    /// 创建分析器。sample_rate 决定 K 加权系数与 400 ms 块长；channels 为交错每帧声道数。
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        let ch = channels.max(1);
        let block_len = ((f64::from(sample_rate) * 0.4) as usize).max(1);
        Self {
            filter: KWeight::new_for_sr(sample_rate.max(1)),
            energy_sum: 0.0,
            block_count: 0,
            block_buf: Vec::with_capacity(block_len * ch),
            block_len,
            channels: ch,
        }
    }

    /// 喂入一段交错样本（每帧 channels 个；不足整帧的尾部保留到下一块）。
    pub fn feed(&mut self, samples: &[f32]) {
        self.block_buf.extend_from_slice(samples);
        let frame = self.block_len * self.channels;
        while self.block_buf.len() >= frame {
            let block = self.block_buf.drain(..frame).collect::<Vec<_>>();
            // 每帧平均为单声道（避免交错顺序打乱 K 加权滤波）
            let mut mono = Vec::with_capacity(self.block_len);
            for f in 0..self.block_len {
                let mut acc = 0.0f64;
                for c in 0..self.channels {
                    acc += f64::from(block[f * self.channels + c]);
                }
                mono.push((acc / self.channels as f64) as f32);
            }
            let mean_sq = self.filter.mean_square_filtered(&mono);
            self.energy_sum += mean_sq;
            self.block_count += 1;
        }
    }

    /// 取整曲响度（LUFS）。未处理任何块或整段静默返回 [`f64::NEG_INFINITY`]。
    pub fn lufs(&self) -> f64 {
        if self.block_count == 0 {
            return f64::NEG_INFINITY;
        }
        let mean = self.energy_sum / self.block_count as f64;
        if mean.partial_cmp(&SILENCE_FLOOR) != Some(std::cmp::Ordering::Greater) {
            return f64::NEG_INFINITY;
        }
        -0.691 + 10.0 * mean.log10()
    }

    /// 目标响度增益（dB）。整曲静默或未分析返回 [`f64::NEG_INFINITY`]（调用方跳过）。
    pub fn gain_db(&self, target_lufs: f64) -> f64 {
        gain_to_target(self.lufs(), target_lufs)
    }
}
// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    // 测试用 f64 精度正弦（避免 f32 PI 与 f64 混算）
    use std::f64::consts::PI;

    /// 生成 `seconds` 秒、采样率 `sr` 的满幅正弦（amp = 1.0）。
    fn full_sine(seconds: f64, freq: f64, sr: u32) -> Vec<f32> {
        let n = (seconds * sr as f64) as usize;
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f64 / sr as f64).sin() as f32)
            .collect()
    }

    /// 用确定算法手算一个 K 加权频率响应，便于 test 校核滤波器是否合理。
    fn cascade_freq_resp(sr: f64, f_hz: f64) -> f64 {
        // shelf @ 1500, +4dB, Q=1/sqrt(2)
        let a = 10f64.powf(4.0 / 40.0);
        let w0s = 2.0 * std::f64::consts::PI * 1500.0 / sr;
        let cs = w0s.cos();
        let sn = w0s.sin();
        let al_s = sn / 2.0 * (a + 1.0 / a).sqrt();
        let t_a = 2.0 * a.sqrt() * al_s;
        let a0_s = (a + 1.0) - (a - 1.0) * cs + t_a;
        let sb0 = a * ((a + 1.0) + (a - 1.0) * cs + t_a) / a0_s;
        let sb1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cs) / a0_s;
        let sb2 = a * ((a + 1.0) + (a - 1.0) * cs - t_a) / a0_s;
        let sa1 = 2.0 * ((a - 1.0) - (a + 1.0) * cs) / a0_s;
        let sa2 = ((a + 1.0) - (a - 1.0) * cs - t_a) / a0_s;

        // HP @ 38.135, Q=0.5
        let w0h = 2.0 * std::f64::consts::PI * 38.13547087602444 / sr;
        let ch = w0h.cos();
        let sh = w0h.sin();
        let al_h = sh / (2.0 * 0.5);
        let a0_h = 1.0 + al_h;
        let hb0 = ((1.0 + ch) / 2.0) / a0_h;
        let hb1 = -(1.0 + ch) / a0_h;
        let hb2 = ((1.0 + ch) / 2.0) / a0_h;
        let ha1 = -2.0 * ch / a0_h;
        let ha2 = (1.0 - al_h) / a0_h;

        // 在 f_hz 处的级联幅值
        let w = 2.0 * std::f64::consts::PI * f_hz / sr;
        let c1 = w.cos();
        let c2 = (2.0 * w).cos();
        let s1 = w.sin();
        let s2 = (2.0 * w).sin();

        let mag = |b0: f64, b1: f64, b2: f64, a1: f64, a2: f64| -> f64 {
            let nr = b0 + b1 * c1 + b2 * c2;
            let ni = -(b1 * s1 + b2 * s2);
            let dr = 1.0 + a1 * c1 + a2 * c2;
            let di = -(a1 * s1 + a2 * s2);
            ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
        };
        mag(sb0, sb1, sb2, sa1, sa2) * mag(hb0, hb1, hb2, ha1, ha2)
    }

    /// 流式分析器与一次性 measure_lufs 结果一致（分块喂入 vs 整段）。
    #[test]
    fn loudness_analyzer_matches_measure_lufs() {
        let sr = 48_000u32;
        let n = sr as usize;
        let samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 1000.0 * i as f64 / sr as f64).sin() as f32)
            .collect();
        let expected = measure_lufs(&samples, sr);
        assert!(expected.is_finite(), "正弦应产生有限 LUFS");
        let mut analyzer = LoudnessAnalyzer::new(sr, 1);
        for chunk in samples.chunks(4096) {
            analyzer.feed(chunk);
        }
        let got = analyzer.lufs();
        assert!(
            (got - expected).abs() < 0.1,
            "流式应接近一次性：got={got}, expected={expected}"
        );
    }

    /// 流式分析器：交错立体声输入（每帧 2 声道平均为单声道）。
    #[test]
    fn loudness_analyzer_stereo_interleaved() {
        let sr = 44_100u32;
        let n = sr as usize * 2;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / sr as f64;
                (2.0 * PI * 440.0 * t).sin() as f32 * 0.5
            })
            .collect();
        let mut interleaved = Vec::with_capacity(samples.len() * 2);
        for s in &samples {
            interleaved.push(*s);
            interleaved.push(*s);
        }
        let mut analyzer = LoudnessAnalyzer::new(sr, 2);
        for chunk in interleaved.chunks(2048) {
            analyzer.feed(chunk);
        }
        let lufs = analyzer.lufs();
        assert!(
            lufs.is_finite() && lufs < -3.0,
            "0.5 振幅正弦应低于满幅：{lufs}"
        );
    }
    #[test]
    fn full_sine_1khz_is_close_to_minus3_lufs() {
        // 满幅 1 kHz 正弦，RMS^2 = 0.5；K 加权在 1 kHz 处 ~= +0.68 dB
        // 理论 LUFS = -0.691 + 0.68 + 10*log10(0.5) ~= -0.691 + 0.68 - 3.010 ~= -3.02 LU
        // 给 +-0.5 LU 容差（filter ringing / 窗长边界）
        let sr = 48_000;
        let pcm = full_sine(1.0, 1000.0, sr);
        let lufs = measure_lufs(&pcm, sr);
        assert!(
            (lufs - (-3.02)).abs() < 0.5,
            "满幅 1kHz LUFS 应 ~= -3.02 LU，实际 {lufs}"
        );
    }

    #[test]
    fn low_freq_100hz_is_lower_than_1khz() {
        // 100 Hz 处 K 加权（HP @ 38 Hz）应给出比 1 kHz 更低的 LUFS
        let sr = 48_000;
        let pcm_1k = full_sine(1.0, 1000.0, sr);
        let pcm_100 = full_sine(1.0, 100.0, sr);
        let lufs_1k = measure_lufs(&pcm_1k, sr);
        let lufs_100 = measure_lufs(&pcm_100, sr);
        assert!(
            lufs_100 < lufs_1k,
            "100Hz LUFS 应低于 1kHz（HP 衰减低频），1k={lufs_1k} 100={lufs_100}"
        );
    }

    #[test]
    fn silence_returns_neg_infinity() {
        let lufs = measure_lufs(&vec![0.0f32; 48000], 48_000);
        assert!(lufs == f64::NEG_INFINITY, "静音应返回 -inf，实际 {lufs}");
    }

    #[test]
    fn empty_input_returns_neg_infinity() {
        let lufs = measure_lufs(&[], 48_000);
        assert!(lufs == f64::NEG_INFINITY);
    }

    #[test]
    fn zero_sample_rate_returns_neg_infinity() {
        // sr = 0 是异常输入，应按 -inf 处理（与"静音"统一语义）
        let lufs = measure_lufs(&full_sine(0.1, 1000.0, 48_000), 0);
        assert!(lufs == f64::NEG_INFINITY);
    }

    #[test]
    fn gain_to_target_basic() {
        // 典型: -20 LUFS -> 目标 -14 LUFS -> +6 dB
        assert!((gain_to_target(-20.0, -14.0) - 6.0).abs() < 1e-9);
        // 反向: -10 LUFS -> 目标 -23 LUFS -> -13 dB（衰减）
        assert!((gain_to_target(-10.0, -23.0) - (-13.0)).abs() < 1e-9);
        // 任务硬性要求: -20 -> -3 -> +17 dB
        assert!((gain_to_target(-20.0, -3.0) - 17.0).abs() < 1e-9);
    }

    #[test]
    fn gain_to_target_handles_neg_infinity() {
        // measured = -inf -> 无增益可加（音源无声），返回 -inf
        assert_eq!(gain_to_target(f64::NEG_INFINITY, -14.0), f64::NEG_INFINITY);
        // target = -inf -> 异常输入，返回 -inf
        assert_eq!(gain_to_target(-20.0, f64::NEG_INFINITY), f64::NEG_INFINITY);
    }

    #[test]
    fn apply_gain_db_zero_db_is_identity() {
        assert_eq!(apply_gain_db(0.5, 0.0), 0.5);
        assert_eq!(apply_gain_db(-0.5, 0.0), -0.5);
        assert_eq!(apply_gain_db(0.0, 0.0), 0.0);
    }

    #[test]
    fn apply_gain_db_pos_6db_doubles_rms() {
        // +6 dB ~= x1.9953（与精确 x2 偏差 < 0.5%）
        let s = 0.25_f32;
        let out = apply_gain_db(s, 6.0);
        assert!(
            (out - 0.4988).abs() < 0.005,
            "+6 dB 应 ~= x1.995，0.25 -> 0.4988，实际 {out}"
        );
    }

    #[test]
    fn apply_gain_db_neg_6db_halves() {
        let s = 0.5_f32;
        let out = apply_gain_db(s, -6.0);
        assert!(
            (out - 0.2506).abs() < 0.005,
            "-6 dB 应 ~= x0.5012，0.5 -> 0.2506，实际 {out}"
        );
    }

    #[test]
    fn apply_gain_db_clips_to_one() {
        // +20 dB 放大 0.5 -> 5.0，必须硬截到 1.0
        let out = apply_gain_db(0.5, 20.0);
        assert_eq!(out, 1.0, "+20dB x 0.5 必须 clamp 到 1.0，实际 {out}");
        // 负向同理
        let out_neg = apply_gain_db(-0.5, 20.0);
        assert_eq!(
            out_neg, -1.0,
            "+20dB x -0.5 必须 clamp 到 -1.0，实际 {out_neg}"
        );
        // 接近满幅 + 较小增益也应 clamp
        let out_close = apply_gain_db(0.9, 6.0);
        assert!(
            (0.9..=1.0).contains(&out_close),
            "+6dB x 0.9 = 1.7957 应 clamp 到 1.0，实际 {out_close}"
        );
    }

    #[test]
    fn apply_gain_db_handles_extreme_db() {
        // +200 dB 仍应是有限值（限幅钳住），不是 NaN
        let out = apply_gain_db(0.001, 200.0);
        assert!(out.is_finite(), "极端 dB 仍应有限，实际 {out}");
        assert!(out <= 1.0, "极端 dB 后限幅到 <=1.0，实际 {out}");
        // NaN dB -> 返回值等于输入被 clamp
        let out_nan = apply_gain_db(0.7, f64::NAN);
        assert_eq!(
            out_nan, 0.7,
            "NaN dB 应被忽略（->sample 截幅），实际 {out_nan}"
        );
        // +-inf dB 同上
        assert_eq!(apply_gain_db(0.3, f64::INFINITY), 0.3);
        assert_eq!(apply_gain_db(0.3, f64::NEG_INFINITY), 0.3);
        // 输入 NaN -> 输出 NaN（错误信号透传）
        let out_sample_nan = apply_gain_db(f32::NAN, 0.0);
        assert!(out_sample_nan.is_nan());
    }

    #[test]
    fn k_weight_zero_state_passes_silence() {
        // 静默通过 K 加权，平均能量近 0
        let mut k = KWeight::new_for_sr(48_000);
        let pcm = vec![0.0f32; 48_000];
        let ms = k.mean_square_filtered(&pcm);
        assert!(ms.abs() < 1e-20, "静默的 K 加权均方应 ~0，实际 {ms}");
    }

    #[test]
    fn k_weight_1khz_close_to_unity() {
        // 1 kHz 满幅正弦，K 加权后均方 ~= 0.5 x 10^(+0.68/10) ~= 0.585
        let mut k = KWeight::new_for_sr(48_000);
        let pcm = full_sine(1.0, 1000.0, 48_000);
        let ms = k.mean_square_filtered(&pcm);
        let expected = 0.5 * cascade_freq_resp(48_000.0, 1000.0).powi(2);
        assert!(
            (ms - expected).abs() < 0.02,
            "1 kHz K 加权均方应 ~= {expected:.4}（含 0.68 dB 抬升），实际 {ms}"
        );
    }

    #[test]
    fn k_weight_hp_attenuates_below_38hz() {
        // 10 Hz 处 HP 应重衰减：38Hz Q=0.5 HP @ 10Hz ~ -23.8 dB，
        // 即满幅正弦原 ms=0.5，衰减后 ~ 0.002。
        let mut k = KWeight::new_for_sr(48_000);
        let pcm_10hz = full_sine(1.0, 10.0, 48_000);
        let ms_10 = k.mean_square_filtered(&pcm_10hz);
        assert!(
            ms_10 < 0.01,
            "10 Hz 应被 HP 重衰减，实测 ms={ms_10}（应 << 0.5）"
        );
    }

    #[test]
    fn k_weight_supports_44100() {
        // 同一份算法在 44.1 kHz 下也应能跑、不 panic；与 48 kHz 同频响偏差在合理范围
        let mut k48 = KWeight::new_for_sr(48_000);
        let mut k44 = KWeight::new_for_sr(44_100);
        let pcm48 = full_sine(1.0, 1000.0, 48_000);
        // 44.1 kHz 下生成 1 秒 1 kHz 正弦
        let pcm44 = full_sine(1.0, 1000.0, 44_100);
        let ms48 = k48.mean_square_filtered(&pcm48);
        let ms44 = k44.mean_square_filtered(&pcm44);
        // 两个采样率下 1 kHz K 加权均方应大致接近（频率响应在 1 kHz 处都接近 +0.6~0.7 dB）
        let ratio = ms44 / ms48;
        assert!(
            ratio > 0.9 && ratio < 1.1,
            "44.1k 与 48k 同一正弦的 K 加权均方应接近，比值={ratio}（应在 0.9~1.1）"
        );
    }

    #[test]
    fn reset_state_makes_filter_deterministic() {
        // 复用同一 KWeight 处理两段不同样本: 先跑 A -> reset -> 跑 B，得到的
        // mean_sq 必须与"新 KWeight 跑 B"一致（防止跨轨道状态泄漏）。
        let mut k = KWeight::new_for_sr(48_000);
        let a = full_sine(0.5, 1000.0, 48_000);
        let b = full_sine(0.5, 200.0, 48_000);
        let _ = k.mean_square_filtered(&a);
        k.reset_state();
        let ms_after_reset = k.mean_square_filtered(&b);

        let mut k2 = KWeight::new_for_sr(48_000);
        let ms_fresh = k2.mean_square_filtered(&b);
        assert!(
            (ms_after_reset - ms_fresh).abs() < 1e-9,
            "reset_state 后处理结果应与新实例一致: reset={ms_after_reset} fresh={ms_fresh}"
        );
    }
}
