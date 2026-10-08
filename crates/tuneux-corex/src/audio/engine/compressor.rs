//! # 压缩器效果器（corex 音频线程 DSP）
//!
//! 峰值检测 + 一阶平滑包络跟随的压缩器：插件（WASM 控制面）经宿主函数写
//! 参数（阈值/压缩比/启动/释放/补偿增益），本模块在音频线程读取原子参数、
//! 执行增益衰减。
//!
//! 红线同均衡器：音频线程零锁（原子读）、零堆分配（系数缓存）、零 host call。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// 阈值范围（dB）。
pub const COMP_THRESHOLD_MIN_DB: f32 = -60.0;
pub const COMP_THRESHOLD_MAX_DB: f32 = 0.0;
/// 压缩比范围（1 = 不压缩）。
pub const COMP_RATIO_MIN: f32 = 1.0;
pub const COMP_RATIO_MAX: f32 = 20.0;
/// 补偿增益范围（dB）。
pub const COMP_MAKEUP_MIN_DB: f32 = -20.0;
pub const COMP_MAKEUP_MAX_DB: f32 = 20.0;
/// 压缩器槽位数（v2 动态多槽位：最多 8 个压缩器叠加）。
pub const COMP_SLOTS: usize = 8;

/// 压缩器参数存储：插件控制面写入、音频线程读取（原子，零锁）。
#[derive(Debug)]
pub struct CompressorParams {
    threshold_db: AtomicU32,
    ratio: AtomicU32,
    attack_ms: AtomicU32,
    release_ms: AtomicU32,
    makeup_db: AtomicU32,
    enabled: AtomicBool,
}

impl CompressorParams {
    /// 新建（默认：阈值 -20dB、压缩比 4:1、启动 10ms、释放 100ms、补偿 0、启用）。
    pub fn new() -> Self {
        Self {
            threshold_db: AtomicU32::new((-20.0f32).to_bits()),
            ratio: AtomicU32::new(4.0f32.to_bits()),
            attack_ms: AtomicU32::new(10.0f32.to_bits()),
            release_ms: AtomicU32::new(100.0f32.to_bits()),
            makeup_db: AtomicU32::new(0.0f32.to_bits()),
            enabled: AtomicBool::new(true),
        }
    }

    /// 设置阈值（dB），钳制 ±60..0。非有限值拒绝并返回 false。
    ///
    /// `f32::clamp` 对 NaN **原样返回**（两次比较均为假），仅靠钳制挡不住，
    /// 故在入口按与 `EqParams::set_band` 相同的口径拒绝。
    /// 逐参数后果不同：阈值取 NaN 时 `*env > NaN` 恒假 → 压缩器**静默失效**
    ///（永不压缩，不产生 NaN）；压缩比 / 补偿增益取 NaN 才会经 `gain_db` /
    /// `powf` 把 NaN 传到输出，而末级 `clamp(-1,1)` 同样不拦 NaN → 直达 DAC。
    pub fn set_threshold(&self, db: f32) -> bool {
        if !db.is_finite() {
            return false;
        }
        let v = db.clamp(COMP_THRESHOLD_MIN_DB, COMP_THRESHOLD_MAX_DB);
        self.threshold_db.store(v.to_bits(), Ordering::Relaxed);
        true
    }
    /// 读取阈值（dB）。
    pub fn threshold(&self) -> f32 {
        f32::from_bits(self.threshold_db.load(Ordering::Relaxed))
    }
    /// 设置压缩比（1 = 不压缩），钳制 1..20。非有限值拒绝并返回 false。
    pub fn set_ratio(&self, r: f32) -> bool {
        if !r.is_finite() {
            return false;
        }
        let v = r.clamp(COMP_RATIO_MIN, COMP_RATIO_MAX);
        self.ratio.store(v.to_bits(), Ordering::Relaxed);
        true
    }
    /// 读取压缩比。
    pub fn ratio(&self) -> f32 {
        f32::from_bits(self.ratio.load(Ordering::Relaxed))
    }
    /// 设置启动时间（ms），钳制 0.1..1000。非有限值拒绝并返回 false
    ///（NaN 会让 `attack_coeff` 变 NaN，包络永久失活）。
    pub fn set_attack_ms(&self, ms: f32) -> bool {
        if !ms.is_finite() {
            return false;
        }
        let v = ms.clamp(0.1, 1000.0);
        self.attack_ms.store(v.to_bits(), Ordering::Relaxed);
        true
    }
    /// 读取启动时间（ms）。
    pub fn attack_ms(&self) -> f32 {
        f32::from_bits(self.attack_ms.load(Ordering::Relaxed))
    }
    /// 设置释放时间（ms），钳制 1..5000。非有限值拒绝并返回 false。
    pub fn set_release_ms(&self, ms: f32) -> bool {
        if !ms.is_finite() {
            return false;
        }
        let v = ms.clamp(1.0, 5000.0);
        self.release_ms.store(v.to_bits(), Ordering::Relaxed);
        true
    }
    /// 读取释放时间（ms）。
    pub fn release_ms(&self) -> f32 {
        f32::from_bits(self.release_ms.load(Ordering::Relaxed))
    }
    /// 设置补偿增益（dB），钳制 ±20。非有限值拒绝并返回 false。
    pub fn set_makeup(&self, db: f32) -> bool {
        if !db.is_finite() {
            return false;
        }
        let v = db.clamp(COMP_MAKEUP_MIN_DB, COMP_MAKEUP_MAX_DB);
        self.makeup_db.store(v.to_bits(), Ordering::Relaxed);
        true
    }
    /// 读取补偿增益（dB）。
    pub fn makeup(&self) -> f32 {
        f32::from_bits(self.makeup_db.load(Ordering::Relaxed))
    }
    /// 设置使能（false = 直通）。
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }
    /// 是否启用。
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// 重置到默认（阈值 -20dB、压缩比 4:1、启动 10ms、释放 100ms、补偿 0、启用）。
    pub fn reset(&self) {
        self.threshold_db
            .store((-20.0f32).to_bits(), Ordering::Relaxed);
        self.ratio.store(4.0f32.to_bits(), Ordering::Relaxed);
        self.attack_ms.store(10.0f32.to_bits(), Ordering::Relaxed);
        self.release_ms.store(100.0f32.to_bits(), Ordering::Relaxed);
        self.makeup_db.store(0.0f32.to_bits(), Ordering::Relaxed);
        self.enabled.store(true, Ordering::Relaxed);
    }
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self::new()
    }
}

/// 有状态压缩器：峰值检测 + 一阶平滑包络 + 增益衰减，L/R 独立。
#[derive(Debug)]
pub(crate) struct CompressorEffect {
    /// L 声道包络（dB）。
    env_l: f32,
    /// R 声道包络（dB）。
    env_r: f32,
    /// 缓存的启动/释放系数（参数或采样率变化时重算）。
    attack_coeff: f32,
    release_coeff: f32,
    cached_sr: u32,
    cached_attack: f32,
    cached_release: f32,
}

impl Default for CompressorEffect {
    fn default() -> Self {
        Self {
            env_l: -120.0,
            env_r: -120.0,
            attack_coeff: 1.0,
            release_coeff: 1.0,
            cached_sr: 0,
            cached_attack: 0.0,
            cached_release: 0.0,
        }
    }
}

impl CompressorEffect {
    /// 处理一段交错样本（L, R, L, R, ...）。`channels` 为 1 或 2。
    ///
    /// 音频线程调用：零锁（原子读）、零堆分配。
    pub(crate) fn process(
        &mut self,
        data: &mut [f32],
        channels: usize,
        sample_rate: u32,
        params: &CompressorParams,
    ) {
        if channels != 1 && channels != 2 {
            return;
        }
        if !params.enabled() {
            return;
        }
        let attack = params.attack_ms();
        let release = params.release_ms();
        if sample_rate != self.cached_sr
            || attack != self.cached_attack
            || release != self.cached_release
        {
            let sr = sample_rate.max(1) as f32;
            self.attack_coeff = 1.0 - (-1.0 / (attack / 1000.0 * sr)).exp();
            self.release_coeff = 1.0 - (-1.0 / (release / 1000.0 * sr)).exp();
            self.cached_sr = sample_rate;
            self.cached_attack = attack;
            self.cached_release = release;
        }
        let threshold = params.threshold();
        let ratio = params.ratio();
        let makeup = params.makeup();

        // 上游 push_all 只推整帧，立体声缓冲恒为偶数长度；断言把这一不变式
        // 显式化，余下的孤立样本（正常路径不存在）按静默忽略处理。
        debug_assert!(
            data.len().is_multiple_of(2),
            "立体声缓冲必须帧对齐（上游 push_all 保证）"
        );
        if channels == 2 {
            let (ac, rc) = (self.attack_coeff, self.release_coeff);
            for frame in data.as_chunks_mut::<2>().0 {
                frame[0] =
                    process_sample(frame[0], &mut self.env_l, threshold, ratio, makeup, ac, rc);
                frame[1] =
                    process_sample(frame[1], &mut self.env_r, threshold, ratio, makeup, ac, rc);
            }
        } else {
            let (ac, rc) = (self.attack_coeff, self.release_coeff);
            for x in data.iter_mut() {
                *x = process_sample(*x, &mut self.env_l, threshold, ratio, makeup, ac, rc);
            }
        }
    }

    pub(crate) fn reset(&mut self) {
        self.env_l = -120.0;
        self.env_r = -120.0;
    }
}

/// 单样本压缩：包络跟随 → 增益衰减 → 应用补偿。
#[inline]
fn process_sample(
    x: f32,
    env: &mut f32,
    threshold: f32,
    ratio: f32,
    makeup: f32,
    attack_coeff: f32,
    release_coeff: f32,
) -> f32 {
    let level_db = 20.0 * (x.abs() + 1e-9).log10();
    let coeff = if level_db > *env {
        attack_coeff
    } else {
        release_coeff
    };
    *env += (level_db - *env) * coeff;
    // 压缩增益：包络超阈值时按压缩比衰减。
    let gain_db = if *env > threshold {
        (threshold - *env) * (1.0 - 1.0 / ratio)
    } else {
        0.0
    };
    let out = x * 10f32.powf((gain_db + makeup) / 20.0);
    // 出口非有限值防护（与均衡器同口径）：参数入口已拒绝非有限值，但参数若被
    // 绕过 setter 直接污染（或包络跑到 Inf），`powf` 会把结果放大成 Inf/NaN，
    // 而末级 `clamp(-1,1)` 不拦 NaN → 直达 DAC。此处回退为输入样本（直通）。
    // 注意兜的是**本模块新产生的**非有限值：若输入 x 自身已非有限（上游解码 /
    // 重采样 / ReplayGain 的输出无有限性检查），这里原样返回，不构成保证。
    if out.is_finite() {
        out
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_clamp() {
        let p = CompressorParams::new();
        assert!(p.set_threshold(999.0));
        assert_eq!(p.threshold(), COMP_THRESHOLD_MAX_DB);
        assert!(p.set_ratio(0.1));
        assert_eq!(p.ratio(), COMP_RATIO_MIN);
        assert!(p.set_makeup(-999.0));
        assert_eq!(p.makeup(), COMP_MAKEUP_MIN_DB);
    }

    /// 非有限值必须被拒且**不改动**原值。
    ///
    /// 回归旧缺陷：五个 setter 只做 `clamp`，而 `f32::NAN.clamp(lo, hi)`
    /// 返回 NaN（不是钳到边界）——插件经 `compressor_set` 传入 NaN 即可让
    /// 增益变 NaN，而末级 `clamp(-1,1)` 同样不拦 NaN → 直达 DAC。
    /// 均衡器侧的 `set_band` 当时已有该防护，压缩器缺失（防护不对称）。
    #[test]
    fn setters_reject_non_finite() {
        let p = CompressorParams::new();
        let (t0, r0, a0, rel0, makeup0) = (
            p.threshold(),
            p.ratio(),
            p.attack_ms(),
            p.release_ms(),
            p.makeup(),
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(!p.set_threshold(bad), "阈值应拒绝 {bad}");
            assert!(!p.set_ratio(bad), "压缩比应拒绝 {bad}");
            assert!(!p.set_attack_ms(bad), "启动应拒绝 {bad}");
            assert!(!p.set_release_ms(bad), "释放应拒绝 {bad}");
            assert!(!p.set_makeup(bad), "补偿应拒绝 {bad}");
        }
        // 原值保持不变（拒绝 = 无副作用，而非写入哨兵/默认值）。
        assert_eq!(p.threshold(), t0);
        assert_eq!(p.ratio(), r0);
        assert_eq!(p.attack_ms(), a0);
        assert_eq!(p.release_ms(), rel0);
        assert_eq!(p.makeup(), makeup0);
        // 各参数读回均为有限值（NaN 一旦写入会自我维持）。
        for v in [
            p.threshold(),
            p.ratio(),
            p.attack_ms(),
            p.release_ms(),
            p.makeup(),
        ] {
            assert!(v.is_finite(), "参数读回必须有限，实际 {v}");
        }
    }

    /// 出口防护：参数被绕过 setter 直接污染时，输出仍须有限。
    ///
    /// 注意不能用"把包络写成 NaN"来构造：`NaN > threshold` 为假 → `gain_db`
    /// 取 0.0 → 输出本来就是有限的，那样的测试对出口防护不敏感（恒通过）。
    /// 这里改为注入 `makeup = +Inf`：`10^((gain + Inf)/20) = Inf`，出口防护
    /// 必须把它回退为输入样本。
    #[test]
    fn process_output_stays_finite_with_poisoned_params() {
        let mut fx = CompressorEffect::default();
        let p = CompressorParams::new();
        p.makeup_db
            .store(f32::INFINITY.to_bits(), Ordering::Relaxed);
        let mut data = vec![0.5f32; 8];
        fx.process(&mut data, 2, 48_000, &p);
        assert!(
            data.iter().all(|v| v.is_finite()),
            "参数被污染为 Inf 时输出必须仍是有限值，实际 {data:?}"
        );
    }

    #[test]
    fn compressor_reduces_above_threshold() {
        let mut fx = CompressorEffect::default();
        let p = CompressorParams::new();
        p.set_threshold(-30.0);
        p.set_ratio(20.0);
        p.set_makeup(0.0);
        // 大幅超阈值信号：应被衰减。attack 约 10ms，包络需时间上升，
        // 检查包络稳定后的尾部样本被压缩。
        let mut data = vec![0.8f32; 20_000];
        fx.process(&mut data, 1, 48_000, &p);
        assert!(data.iter().all(|v| v.is_finite()));
        assert!(
            data[10_000..].iter().all(|&v| v < 0.8),
            "包络稳定后超阈值信号应被压缩"
        );
    }

    #[test]
    fn disabled_bypasses() {
        let mut fx = CompressorEffect::default();
        let p = CompressorParams::new();
        p.set_enabled(false);
        let mut data = vec![0.5f32, -0.25];
        let orig = data.clone();
        fx.process(&mut data, 2, 48_000, &p);
        assert_eq!(data, orig, "禁用应直通");
    }

    #[test]
    fn reset_clears_envelope() {
        let mut fx = CompressorEffect::default();
        let p = CompressorParams::new();
        p.set_threshold(-60.0);
        fx.process(&mut vec![0.9f32; 128], 1, 48_000, &p);
        fx.reset();
        let p2 = CompressorParams::new();
        p2.set_threshold(0.0);
        p2.set_ratio(1.0);
        p2.set_makeup(0.0);
        let mut data = vec![0.5f32; 4];
        fx.process(&mut data, 1, 48_000, &p2);
        assert!(
            data.iter().all(|v| (v - 0.5).abs() < 1e-5),
            "重置后 ratio=1 应直通"
        );
    }
}
