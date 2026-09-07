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

    /// 设置阈值（dB），钳制 ±60..0。
    pub fn set_threshold(&self, db: f32) {
        let v = db.clamp(COMP_THRESHOLD_MIN_DB, COMP_THRESHOLD_MAX_DB);
        self.threshold_db.store(v.to_bits(), Ordering::Relaxed);
    }
    /// 读取阈值（dB）。
    pub fn threshold(&self) -> f32 {
        f32::from_bits(self.threshold_db.load(Ordering::Relaxed))
    }
    /// 设置压缩比（1 = 不压缩），钳制 1..20。
    pub fn set_ratio(&self, r: f32) {
        let v = r.clamp(COMP_RATIO_MIN, COMP_RATIO_MAX);
        self.ratio.store(v.to_bits(), Ordering::Relaxed);
    }
    /// 读取压缩比。
    pub fn ratio(&self) -> f32 {
        f32::from_bits(self.ratio.load(Ordering::Relaxed))
    }
    /// 设置启动时间（ms），钳制 0.1..1000。
    pub fn set_attack_ms(&self, ms: f32) {
        let v = ms.clamp(0.1, 1000.0);
        self.attack_ms.store(v.to_bits(), Ordering::Relaxed);
    }
    /// 读取启动时间（ms）。
    pub fn attack_ms(&self) -> f32 {
        f32::from_bits(self.attack_ms.load(Ordering::Relaxed))
    }
    /// 设置释放时间（ms），钳制 1..5000。
    pub fn set_release_ms(&self, ms: f32) {
        let v = ms.clamp(1.0, 5000.0);
        self.release_ms.store(v.to_bits(), Ordering::Relaxed);
    }
    /// 读取释放时间（ms）。
    pub fn release_ms(&self) -> f32 {
        f32::from_bits(self.release_ms.load(Ordering::Relaxed))
    }
    /// 设置补偿增益（dB），钳制 ±20。
    pub fn set_makeup(&self, db: f32) {
        let v = db.clamp(COMP_MAKEUP_MIN_DB, COMP_MAKEUP_MAX_DB);
        self.makeup_db.store(v.to_bits(), Ordering::Relaxed);
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
    x * 10f32.powf((gain_db + makeup) / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_clamp() {
        let p = CompressorParams::new();
        p.set_threshold(999.0);
        assert_eq!(p.threshold(), COMP_THRESHOLD_MAX_DB);
        p.set_ratio(0.1);
        assert_eq!(p.ratio(), COMP_RATIO_MIN);
        p.set_makeup(-999.0);
        assert_eq!(p.makeup(), COMP_MAKEUP_MIN_DB);
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
