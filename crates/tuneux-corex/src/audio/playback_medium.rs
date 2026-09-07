//! # 播放介质风格（音效）
//!
//! 模拟「同一首歌用不同播放介质播放」的听感：磁带 / 黑胶。
//! 本模块只负责声音（DSP），视觉由各发行版自行渲染。
//!
//! 运行在音频回调线程，必须零堆分配、零锁、零 host call——
//! 效果器状态（延迟线、随机数种子等）随回调闭包常驻，切换介质
//! 只通过一个原子量传递，不在回调里做任何分配。
//!
//! 结构：
//! - [`PlaybackMedium`]：介质枚举（None / Tape / Vinyl）；
//! - [`PlaybackMediumEffect`]：效果器接口（原地处理一段交错 PCM）；
//! - [`MediumEffects`]：有状态效果器的聚合结构，按当前介质分发；
//!   新增介质 = 加一个字段 + 加一行 match（编译期由穷尽 match 强制）。
//!
//! 介质切换：效果器已做增益归一（磁带 tanh 补偿、黑胶高频搁架温和），
//! 电平接近连续；切换瞬间再由 [`MediumEffects::apply`] 用数回调的
//! 短交叉淡化（`wet` 由 0.25→1 渐入）兜底，避免滤波器状态突变产生咔哒。

use std::fmt;
use std::str::FromStr;

/// 未知的介质风格名称（字符串解析失败时返回）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnknownMedium;

impl fmt::Display for UnknownMedium {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("未知的介质风格名称")
    }
}

impl std::error::Error for UnknownMedium {}

/// 播放介质风格。
///
/// 磁带 / 黑胶各 4 档，按「老化 / 磨损程度从新到老」排列；
/// 每档对应一组声音参数（见 TapeParams / VinylParams）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PlaybackMedium {
    /// 不启用任何介质风格（原始数字输出）。
    #[default]
    None,
    /// 磁带 · 透明高保真（全新）：底噪极低、无抖晃、无掉粉。
    TapeClear,
    /// 磁带 · 白色清新（轻度使用）：轻微底噪 / 抖晃 / 高频略暗。
    TapeWhite,
    /// 磁带 · 深棕经典（中度老化）：明显底噪 + 磁饱和 + 抖晃 + 掉粉。
    TapeClassic,
    /// 磁带 · 红色老磁带（重度老化）：高频狠闷、抖晃重、掉粉频繁 + 周期带盘扰动。
    TapeAged,
    /// 黑胶 · 蓝色低噪声（全新压制）：噼啪极少、底噪极低。
    VinylClean,
    /// 黑胶 · 红色高动态（轻度使用）：噼啪清脆、偏亮。
    VinylDynamic,
    /// 黑胶 · 黑色标准（中度磨损）：标准噼啪 + 隆隆。
    VinylStandard,
    /// 黑胶 · 彩胶老黑胶（重度磨损）：划痕噼啪密集、槽纹磨损高频闷 + 周期偏心 wow。
    VinylAged,
}

impl PlaybackMedium {
    /// 全部介质变体（界面循环切换 / 候选列出的单一真源）。
    /// 顺序：无 → 磁带（新→老）→ 黑胶（新→老）。
    pub const ALL: [PlaybackMedium; 9] = [
        PlaybackMedium::None,
        PlaybackMedium::TapeClear,
        PlaybackMedium::TapeWhite,
        PlaybackMedium::TapeClassic,
        PlaybackMedium::TapeAged,
        PlaybackMedium::VinylClean,
        PlaybackMedium::VinylDynamic,
        PlaybackMedium::VinylStandard,
        PlaybackMedium::VinylAged,
    ];

    /// 稳定短名（供上层配置与日志复用）。与 [`FromStr`] 互逆。
    pub const fn as_str(self) -> &'static str {
        match self {
            PlaybackMedium::None => "none",
            PlaybackMedium::TapeClear => "tape_clear",
            PlaybackMedium::TapeWhite => "tape_white",
            PlaybackMedium::TapeClassic => "tape_classic",
            PlaybackMedium::TapeAged => "tape_aged",
            PlaybackMedium::VinylClean => "vinyl_clean",
            PlaybackMedium::VinylDynamic => "vinyl_dynamic",
            PlaybackMedium::VinylStandard => "vinyl_standard",
            PlaybackMedium::VinylAged => "vinyl_aged",
        }
    }

    /// 判别值（原子量存储用，u32 与原子类型对齐）。
    pub(crate) const fn discriminant(self) -> u32 {
        match self {
            PlaybackMedium::None => 0,
            PlaybackMedium::TapeClear => 1,
            PlaybackMedium::TapeWhite => 2,
            PlaybackMedium::TapeClassic => 3,
            PlaybackMedium::TapeAged => 4,
            PlaybackMedium::VinylClean => 5,
            PlaybackMedium::VinylDynamic => 6,
            PlaybackMedium::VinylStandard => 7,
            PlaybackMedium::VinylAged => 8,
        }
    }

    /// 从判别值还原；未知值回退 [`PlaybackMedium::None`]（不 panic）。
    pub(crate) const fn from_discriminant(value: u32) -> Self {
        match value {
            0 => PlaybackMedium::None,
            1 => PlaybackMedium::TapeClear,
            2 => PlaybackMedium::TapeWhite,
            3 => PlaybackMedium::TapeClassic,
            4 => PlaybackMedium::TapeAged,
            5 => PlaybackMedium::VinylClean,
            6 => PlaybackMedium::VinylDynamic,
            7 => PlaybackMedium::VinylStandard,
            8 => PlaybackMedium::VinylAged,
            _ => PlaybackMedium::None,
        }
    }
}

impl FromStr for PlaybackMedium {
    type Err = UnknownMedium;

    /// 从配置字符串解析介质风格（大小写不敏感）。
    /// 兼容旧短名：`tape` → 深棕经典、`vinyl` → 黑色标准。
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for medium in PlaybackMedium::ALL {
            if medium.as_str().eq_ignore_ascii_case(s) {
                return Ok(medium);
            }
        }
        // 旧短名向后兼容。
        match s.to_ascii_lowercase().as_str() {
            "tape" => Ok(PlaybackMedium::TapeClassic),
            "vinyl" => Ok(PlaybackMedium::VinylStandard),
            _ => Err(UnknownMedium),
        }
    }
}

/// 介质音效处理接口：原地处理一段交错 PCM。
///
/// `channels` 为通道数（调用方保证 ≥1）；v1 按立体声处理前两通道、其余直通。
/// `sample_rate` 为流采样率（Hz），抖晃频率 / 噪声带宽 / 搁架系数均按其定标。
/// `wet` ∈ [0, 1]：0 = 直通、1 = 全效果，用于介质切换时的短交叉淡化。
/// 实现必须零堆分配。
pub(crate) trait PlaybackMediumEffect {
    /// 处理一段样本（就地修改）。
    fn process(&mut self, data: &mut [f32], channels: usize, sample_rate: u32, wet: f32);
    /// 重置内部状态（换曲 / seek 时调用，丢弃上一曲残留）。默认空实现。
    fn reset(&mut self) {}
}

// —— 共用 DSP 辅助（零依赖、零堆分配）——

/// 32 位 LCG 伪随机（零依赖）：返回 [-1.0, 1.0] 均匀分布噪声。
fn lcg_noise(state: &mut u32) -> f32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*state as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// 32 位 LCG 伪随机：推进并返回原始 u32 值。
///
/// 用于极低频事件（如磁带随机「哒」声）的整数阈值判断——浮点概率在
/// f32 精度下会把极小的触发率量化到「几乎永不触发」，整数比较才精确。
fn lcg_u32(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state
}

/// 非有限值兜底为 0：NaN/Inf 一旦进入 IIR 或延迟线状态会自我维持、
/// 污染整首曲（reset 只在换曲触发），须在写入状态前拦截。
fn finite(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else {
        0.0
    }
}

/// 从环形延迟线线性插值读；`pos` 可为负或越界，自动回绕。
///
/// 用 `rem_euclid` 保证取模结果在 [0, len)，再对 usize 取模兜底
/// 浮点边界（如 pos 恰为 len 的倍数时的舍入误差），杜绝越界；
/// 空缓冲直接返回 0（避免 `% 0`）。
fn delay_read(buf: &[f32], pos: f32) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    let len = buf.len() as f32;
    let p = pos.rem_euclid(len);
    let i0 = (p as usize) % buf.len();
    let i1 = (i0 + 1) % buf.len();
    let frac = p.fract();
    buf[i0] * (1.0 - frac) + buf[i1] * frac
}

// —— 磁带（底噪 + 磁饱和 + 抖晃）——

/// 磁带每声道延迟线长度（样本）。抖晃最大读取偏移 ≈ BASE + WOW + FLUTTER + AGED，
/// 96 足够容纳并留余量；固定大小保证音频回调零堆分配。
const TAPE_DELAY_LEN: usize = 96;
/// 抖晃基础延迟（样本）。
const TAPE_DELAY_BASE: f32 = 20.0;
/// 抖晃 wow 频率（Hz），慢速音高漂移。
const TAPE_WOW_RATE: f32 = 0.8;
/// 抖晃 flutter 与 wow 频率之比（flutter 通常 6-10Hz）。
const TAPE_FLUTTER_RATIO: f32 = 10.0;
/// 随机「哒」声包络时间常数（秒）。
const TAPE_DROP_TAU_S: f32 = 0.003;
/// 掉粉恢复时间常数（秒）。
const TAPE_DROPOUT_TAU_S: f32 = 0.02;
/// 方位角误差低通截止频率（Hz）。
const TAPE_AZ_LP_HZ: f32 = 3_000.0;
/// 老磁带带盘扰动频率（Hz）：带盘每转一圈约 0.5 秒。
const TAPE_AGED_WOBBLE_HZ: f32 = 2.0;
/// 老磁带带盘扰动深度（样本，叠加到抖晃 offset 上）。
const TAPE_AGED_WOBBLE_DEPTH: f32 = 8.0;

/// 磁带声音参数组：每档介质对应一组，控制老化 / 受损程度。
#[derive(Debug, Clone, Copy)]
pub(crate) struct TapeParams {
    /// 底噪幅度（线性）。
    pub hiss: f32,
    /// 磁饱和驱动（tanh 软削波，>1 更饱和）。
    pub drive: f32,
    /// 高频滚降截止频率（Hz），越低越「闷」。
    pub lp_hz: f32,
    /// 抖晃 wow 深度（样本），越大音高越飘。
    pub wow_depth: f32,
    /// 抖晃 flutter 深度（样本）。
    pub flutter_depth: f32,
    /// 方位角梳状强度（越大高频越「发虚」）。
    pub az_comb: f32,
    /// 调制噪声系数（随信号起伏的颗粒沙沙）。
    pub mod_noise: f32,
    /// 随机「哒」声触发率（次/秒）。
    pub drop_rate: f32,
    /// 随机「哒」声幅度。
    pub drop_amp: f32,
    /// 掉粉触发率（次/秒）。
    pub dropout_rate: f32,
    /// 掉粉深度（音量骤降到 1 - DEPTH 倍）。
    pub dropout_depth: f32,
    /// 是否老磁带（叠加周期带盘扰动）。
    pub aged: bool,
}

/// 按介质取磁带参数组（新 → 老，老化程度递增、差异明显）。
pub(crate) fn tape_params(medium: PlaybackMedium) -> TapeParams {
    match medium {
        PlaybackMedium::TapeClear => TapeParams {
            hiss: 0.001,
            drive: 1.2,
            lp_hz: 10_000.0,
            wow_depth: 5.0,
            flutter_depth: 1.0,
            az_comb: 0.0,
            mod_noise: 0.005,
            drop_rate: 0.0,
            drop_amp: 0.2,
            dropout_rate: 0.0,
            dropout_depth: 0.7,
            aged: false,
        },
        PlaybackMedium::TapeWhite => TapeParams {
            hiss: 0.002,
            drive: 1.5,
            lp_hz: 8_000.0,
            wow_depth: 20.0,
            flutter_depth: 2.0,
            az_comb: 0.3,
            mod_noise: 0.01,
            drop_rate: 0.002,
            drop_amp: 0.2,
            dropout_rate: 0.003,
            dropout_depth: 0.7,
            aged: false,
        },
        PlaybackMedium::TapeClassic => TapeParams {
            hiss: 0.004,
            drive: 2.0,
            lp_hz: 6_000.0,
            wow_depth: 40.0,
            flutter_depth: 3.0,
            az_comb: 0.6,
            mod_noise: 0.02,
            drop_rate: 0.004,
            drop_amp: 0.2,
            dropout_rate: 0.008,
            dropout_depth: 0.7,
            aged: false,
        },
        PlaybackMedium::TapeAged => TapeParams {
            hiss: 0.01,
            drive: 2.8,
            lp_hz: 5_000.0,
            wow_depth: 60.0,
            flutter_depth: 5.0,
            az_comb: 0.8,
            mod_noise: 0.04,
            drop_rate: 0.01,
            drop_amp: 0.25,
            dropout_rate: 0.02,
            dropout_depth: 0.8,
            aged: true,
        },
        _ => tape_params(PlaybackMedium::TapeClassic),
    }
}

/// 磁带介质效果：高频滚降 + 方位角发虚 + 底噪/调制噪声 + 磁饱和 + 抖晃。
///
/// 处理顺序（模拟真实磁带链路）：原始样本 → 高频滚降（一阶低通 ~6kHz，
/// 「闷」感）→ 方位角误差（高频梳状滤波，「发虚」）→ 底噪 + 调制噪声
/// （随信号起伏的颗粒沙沙）→ 磁饱和（软削波，增益按 tanh(drive) 归一）
/// → 抖晃（延迟线变速，音高缓慢起伏）→ 随机「哒」声 + 掉粉音量骤降。
/// L/R 各持一条延迟线、低通状态与独立噪声种子，互不串扰。
#[derive(Debug)]
pub(crate) struct TapeEffect {
    /// 当前声音参数组（由 MediumEffects::apply 按介质设置）。
    params: TapeParams,
    /// 老磁带带盘扰动 LFO 相位。
    aged_phase: f32,
    /// 左声道噪声种子（LCG，底噪用）。
    rng_l: u32,
    /// 右声道噪声种子（与左独立，避免两声道底噪结构相关）。
    rng_r: u32,
    /// 抖晃 LFO 相位。
    lfo_phase: f32,
    /// 随机「哒」声种子（独立于底噪，保证触发时机随机）。
    rng_drop: u32,
    /// 随机「哒」声包络（指数衰减，双极性）。
    drop_env: f32,
    /// 掉粉触发种子（独立）。
    rng_dropout: u32,
    /// 掉粉深度包络（0 = 正常，越大音量越轻）。
    dropout: f32,
    /// 高频滚降低通状态（左声道）。
    lp_l: f32,
    /// 高频滚降低通状态（右声道）。
    lp_r: f32,
    /// 方位角低通状态（左声道，提取高频用）。
    az_lp_l: f32,
    /// 方位角低通状态（右声道）。
    az_lp_r: f32,
    /// 方位角前一高频样本（左声道，梳状滤波用）。
    az_prev_l: f32,
    /// 方位角前一高频样本（右声道）。
    az_prev_r: f32,
    /// 左声道延迟线。
    delay_l: [f32; TAPE_DELAY_LEN],
    /// 右声道延迟线。
    delay_r: [f32; TAPE_DELAY_LEN],
    /// 延迟线写索引。
    delay_idx: usize,
    /// 测试观察用：`process` 被调用的次数（仅测试构建存在并计数）。
    #[cfg(test)]
    process_calls: u32,
}

impl Default for TapeEffect {
    fn default() -> Self {
        Self {
            params: tape_params(PlaybackMedium::TapeClassic),
            aged_phase: 0.0,
            rng_l: 0x9E37_79B9,
            rng_r: 0x5DEE_CE66,
            lfo_phase: 0.0,
            rng_drop: 0xA5A5_5A5A,
            drop_env: 0.0,
            rng_dropout: 0x5A5A_A5A5,
            dropout: 0.0,
            lp_l: 0.0,
            lp_r: 0.0,
            az_lp_l: 0.0,
            az_lp_r: 0.0,
            az_prev_l: 0.0,
            az_prev_r: 0.0,
            delay_l: [0.0; TAPE_DELAY_LEN],
            delay_r: [0.0; TAPE_DELAY_LEN],
            delay_idx: 0,
            #[cfg(test)]
            process_calls: 0,
        }
    }
}

impl PlaybackMediumEffect for TapeEffect {
    fn process(&mut self, data: &mut [f32], channels: usize, sample_rate: u32, wet: f32) {
        let ch = channels.max(1);
        // 抖晃 LFO 步进（每帧一次）。
        let lfo_step = std::f32::consts::TAU * TAPE_WOW_RATE / sample_rate.max(1) as f32;
        // 磁饱和增益补偿（小信号增益 ≈ 1），每次调用算一次。
        let p = self.params;
        let drive_comp = p.drive.tanh();
        // 高频滚降系数（一阶低通 ~6kHz），模拟磁带高频损失。
        let lp_alpha = 1.0 - (-std::f32::consts::TAU * p.lp_hz / sample_rate.max(1) as f32).exp();
        // 随机「哒」声 / 掉粉：触发率与衰减均按采样率定标。
        let sr = sample_rate.max(1) as f32;
        // 整数阈值：u32 全量程 × 每样本触发率，精确到整数，避免 f32 精度损失。
        let drop_threshold = (p.drop_rate as f64 / sr as f64 * (u32::MAX as f64 + 1.0)) as u32;
        let drop_decay = (-1.0 / (TAPE_DROP_TAU_S * sr)).exp();
        let dropout_threshold =
            (p.dropout_rate as f64 / sr as f64 * (u32::MAX as f64 + 1.0)) as u32;
        let dropout_decay = (-1.0 / (TAPE_DROPOUT_TAU_S * sr)).exp();
        // 方位角误差：低通系数（提取高频成分）。
        let az_alpha = 1.0 - (-std::f32::consts::TAU * TAPE_AZ_LP_HZ / sr).exp();
        // 老磁带带盘扰动：每 0.5 秒一圈的周期波动。
        let aged_step = std::f32::consts::TAU * TAPE_AGED_WOBBLE_HZ / sr;

        for frame in data.chunks_mut(ch) {
            // 极低频随机「哒」声：双极性冲击后指数衰减（L/R 共享一次触发）。
            if lcg_u32(&mut self.rng_drop) < drop_threshold {
                let polarity = if lcg_u32(&mut self.rng_drop) & 1 == 0 {
                    1.0
                } else {
                    -1.0
                };
                self.drop_env = p.drop_amp * polarity;
            }
            self.drop_env *= drop_decay;
            // 掉粉（drop-out）：磁粉脱落导致音量瞬间骤降，几十毫秒恢复。
            if lcg_u32(&mut self.rng_dropout) < dropout_threshold {
                self.dropout = p.dropout_depth;
            }
            self.dropout *= dropout_decay;

            self.lfo_phase += lfo_step;
            // 回绕到 [0, TAU)，避免相位持续累积耗尽 f32 精度后抖晃冻结。
            if self.lfo_phase >= std::f32::consts::TAU {
                self.lfo_phase -= std::f32::consts::TAU;
            }
            // wow（低频大深度）+ flutter（高频小深度）复合抖晃。
            let wow = p.wow_depth * self.lfo_phase.sin();
            let flutter = p.flutter_depth * (self.lfo_phase * TAPE_FLUTTER_RATIO).sin();
            let mut aged_wobble = 0.0;
            if p.aged {
                self.aged_phase += aged_step;
                if self.aged_phase >= std::f32::consts::TAU {
                    self.aged_phase -= std::f32::consts::TAU;
                }
                aged_wobble = TAPE_AGED_WOBBLE_DEPTH * self.aged_phase.sin();
            }
            let offset = TAPE_DELAY_BASE + wow + flutter + aged_wobble;

            // 左声道：高频滚降 → 方位角 → 底噪/调制噪声 → 磁饱和 → 抖晃 → 掉粉。
            let dry = finite(frame.first().copied().unwrap_or(0.0));
            self.lp_l += lp_alpha * (dry - self.lp_l);
            let dull = self.lp_l;
            // 方位角误差：提取高频 → 梳状滤波（相位错乱、发虚）。
            self.az_lp_l += az_alpha * (dull - self.az_lp_l);
            let high = dull - self.az_lp_l;
            let comb = high - p.az_comb * self.az_prev_l;
            self.az_prev_l = high;
            let azimuthed = self.az_lp_l + comb;
            // 底噪 + 调制噪声（随信号幅度起伏的颗粒沙沙）。
            let noise_gain = p.hiss + p.mod_noise * azimuthed.abs();
            let x = azimuthed + lcg_noise(&mut self.rng_l) * noise_gain;
            let saturated = (x * p.drive).tanh() / drive_comp;
            self.delay_l[self.delay_idx] = saturated;
            let wet_out = (delay_read(&self.delay_l, self.delay_idx as f32 - offset)
                + self.drop_env)
                * (1.0 - self.dropout);
            if let Some(s) = frame.first_mut() {
                *s = dry + wet * (wet_out - dry);
            }

            // 右声道：高频滚降 → 方位角 → 底噪/调制噪声 → 磁饱和 → 抖晃 → 掉粉。
            let dry_r = finite(frame.get(1).copied().unwrap_or(0.0));
            self.lp_r += lp_alpha * (dry_r - self.lp_r);
            let dull_r = self.lp_r;
            self.az_lp_r += az_alpha * (dull_r - self.az_lp_r);
            let high_r = dull_r - self.az_lp_r;
            let comb_r = high_r - p.az_comb * self.az_prev_r;
            self.az_prev_r = high_r;
            let azimuthed_r = self.az_lp_r + comb_r;
            let noise_gain_r = p.hiss + p.mod_noise * azimuthed_r.abs();
            let x_r = azimuthed_r + lcg_noise(&mut self.rng_r) * noise_gain_r;
            let saturated_r = (x_r * p.drive).tanh() / drive_comp;
            self.delay_r[self.delay_idx] = saturated_r;
            let wet_out_r = (delay_read(&self.delay_r, self.delay_idx as f32 - offset)
                + self.drop_env)
                * (1.0 - self.dropout);
            if let Some(s) = frame.get_mut(1) {
                *s = dry_r + wet * (wet_out_r - dry_r);
            }

            self.delay_idx = (self.delay_idx + 1) % TAPE_DELAY_LEN;
        }

        #[cfg(test)]
        {
            self.process_calls += 1;
        }
    }

    fn reset(&mut self) {
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.delay_idx = 0;
        self.lfo_phase = 0.0;
        self.aged_phase = 0.0;
        self.lp_l = 0.0;
        self.lp_r = 0.0;
        self.drop_env = 0.0;
        self.dropout = 0.0;
        self.az_lp_l = 0.0;
        self.az_lp_r = 0.0;
        self.az_prev_l = 0.0;
        self.az_prev_r = 0.0;
        // rng_l / rng_r / rng_drop / rng_dropout 刻意保留：避免每曲噪声序列完全相同。
    }
}

// —— 黑胶（底噪隆隆 + 嘶声 + 唱针噼啪 + 高频搁架）——

/// 唱针噼啪包络时间常数（秒），按采样率定标。
const VINYL_CLICK_TAU_S: f32 = 0.001;
/// 高频搁架截止频率（Hz），模拟唱针高频响应。
const VINYL_SHELF_HZ: f32 = 10_000.0;
/// 老黑胶偏心 wow 频率（Hz）：33⅓ 转/分 ≈ 1.8 秒/圈。
const VINYL_AGED_WOW_HZ: f32 = 0.556;
/// 老黑胶偏心 wow 深度（样本，音高摆动幅度）。
const VINYL_AGED_WOW_DEPTH: f32 = 8.0;
/// 老黑胶划痕咔哒幅度（每圈一次，比随机噼啪更脆更响）。
const VINYL_AGED_SCRATCH_AMP: f32 = 0.15;
/// 黑胶延迟线长度（样本，偏心 wow 用）。
const VINYL_DELAY_LEN: usize = 96;
/// 黑胶延迟线基础延迟（样本）。
const VINYL_DELAY_BASE: f32 = 4.0;

/// 黑胶声音参数组：每档介质对应一组，控制磨损 / 受损程度。
#[derive(Debug, Clone, Copy)]
pub(crate) struct VinylParams {
    /// 唱针噼啪触发率（次/秒）。
    pub click_rate: f32,
    /// 唱针噼啪幅度。
    pub click_amp: f32,
    /// 低频隆隆（噪声源幅度）。
    pub rumble: f32,
    /// 高频嘶声幅度。
    pub hiss: f32,
    /// 高频搁架增益（<1 = 高频衰减）。
    pub shelf_gain: f32,
    /// 是否老黑胶（偏心 wow + 每圈划痕咔哒）。
    pub aged: bool,
}

/// 按介质取黑胶参数组（新 → 旧，磨损程度递增、差异明显）。
pub(crate) fn vinyl_params(medium: PlaybackMedium) -> VinylParams {
    match medium {
        PlaybackMedium::VinylClean => VinylParams {
            click_rate: 2.0,
            click_amp: 0.02,
            rumble: 0.02,
            hiss: 0.0008,
            shelf_gain: 0.71,
            aged: false,
        },
        PlaybackMedium::VinylDynamic => VinylParams {
            click_rate: 6.0,
            click_amp: 0.05,
            rumble: 0.03,
            hiss: 0.0015,
            shelf_gain: 0.71,
            aged: false,
        },
        PlaybackMedium::VinylStandard => VinylParams {
            click_rate: 8.0,
            click_amp: 0.06,
            rumble: 0.05,
            hiss: 0.002,
            shelf_gain: 0.5,
            aged: false,
        },
        PlaybackMedium::VinylAged => VinylParams {
            click_rate: 15.0,
            click_amp: 0.12,
            rumble: 0.08,
            hiss: 0.005,
            shelf_gain: 0.35,
            aged: true,
        },
        _ => vinyl_params(PlaybackMedium::VinylStandard),
    }
}

/// 黑胶介质效果：底噪（低频隆隆 + 高频嘶声）+ 唱针噼啪 + 温和高频搁架。
///
/// 高频搁架只在高频段温和衰减（模拟唱针频响），低频保持平直——
/// 不施加完整的 RIAA 去加重（那要求源文件已做前置加重，否则全曲变闷）。
/// 隆隆为单声道（转盘同源），搁架 L/R 各自独立（保持立体声）。
#[derive(Debug)]
pub(crate) struct VinylEffect {
    /// 当前声音参数组（由 MediumEffects::apply 按介质设置）。
    params: VinylParams,
    /// 老黑胶偏心 wow LFO 相位。
    aged_phase: f32,
    /// 老黑胶划痕咔哒包络。
    scratch_env: f32,
    /// 偏心 wow 左声道延迟线。
    delay_l: [f32; VINYL_DELAY_LEN],
    /// 偏心 wow 右声道延迟线。
    delay_r: [f32; VINYL_DELAY_LEN],
    /// 延迟线写索引。
    delay_idx: usize,
    /// LCG 随机数种子（噪声 + 噼啪用）。
    rng_state: u32,
    /// 唱针噼啪包络（指数衰减，双极性）。
    click_env: f32,
    /// 隆隆低通状态（一阶 IIR，L/R 共享）。
    rumble_state: f32,
    /// 高频搁架低通状态（左声道）。
    shelf_state_l: f32,
    /// 高频搁架低通状态（右声道）。
    shelf_state_r: f32,
    /// 测试观察用：`process` 被调用的次数（仅测试构建存在并计数）。
    #[cfg(test)]
    process_calls: u32,
}

impl Default for VinylEffect {
    fn default() -> Self {
        Self {
            params: vinyl_params(PlaybackMedium::VinylStandard),
            aged_phase: 0.0,
            scratch_env: 0.0,
            delay_l: [0.0; VINYL_DELAY_LEN],
            delay_r: [0.0; VINYL_DELAY_LEN],
            delay_idx: 0,
            rng_state: 0xC0FF_EE11,
            click_env: 0.0,
            rumble_state: 0.0,
            shelf_state_l: 0.0,
            shelf_state_r: 0.0,
            #[cfg(test)]
            process_calls: 0,
        }
    }
}

impl PlaybackMediumEffect for VinylEffect {
    fn process(&mut self, data: &mut [f32], channels: usize, sample_rate: u32, wet: f32) {
        let ch = channels.max(1);
        let p = self.params;
        let sr = sample_rate.max(1) as f32;
        // 一阶 IIR 系数按采样率定标：隆隆 ~50Hz、高频搁架 ~10kHz。
        let rumble_alpha = 1.0 - (-std::f32::consts::TAU * 50.0 / sr).exp();
        let shelf_alpha = 1.0 - (-std::f32::consts::TAU * VINYL_SHELF_HZ / sr).exp();
        // 噼啪触发率按采样率定标：整数阈值（u32 全量程 × 每样本概率）。
        let click_threshold = (p.click_rate as f64 / sr as f64 * (u32::MAX as f64 + 1.0)) as u32;
        let click_decay = (-1.0 / (VINYL_CLICK_TAU_S * sr)).exp();
        // 老黑胶偏心 wow：每 1.8 秒一圈的音高摆动 LFO。
        let aged_step = std::f32::consts::TAU * VINYL_AGED_WOW_HZ / sr;

        for frame in data.chunks_mut(ch) {
            // 唱针噼啪：双极性随机冲击，随后指数衰减（L/R 共享一个包络）。
            if lcg_u32(&mut self.rng_state) < click_threshold {
                let polarity = if lcg_u32(&mut self.rng_state) & 1 == 0 {
                    1.0
                } else {
                    -1.0
                };
                self.click_env = p.click_amp * polarity;
            }
            self.click_env *= click_decay;

            // 老黑胶：偏心 wow（音高摆动）+ 每圈一次的划痕咔哒。
            let mut wow_offset = 0.0;
            if p.aged {
                self.aged_phase += aged_step;
                if self.aged_phase >= std::f32::consts::TAU {
                    self.aged_phase -= std::f32::consts::TAU;
                    // 相位回绕 = 转完一圈，触发一次划痕咔哒。
                    let polarity = if lcg_u32(&mut self.rng_state) & 1 == 0 {
                        1.0
                    } else {
                        -1.0
                    };
                    self.scratch_env = VINYL_AGED_SCRATCH_AMP * polarity;
                }
                wow_offset = VINYL_AGED_WOW_DEPTH * self.aged_phase.sin();
            }
            self.scratch_env *= click_decay;
            let offset = VINYL_DELAY_BASE + wow_offset;

            // 隆隆（单声道，转盘同源）。
            let rumble_in = lcg_noise(&mut self.rng_state) * p.rumble;
            self.rumble_state += rumble_alpha * (rumble_in - self.rumble_state);

            // 左声道：偏心 wow 延迟线 → 高频搁架 → 表面噪声。
            if let Some(s) = frame.first_mut() {
                let dry = finite(*s);
                self.delay_l[self.delay_idx] = dry;
                let warped = delay_read(&self.delay_l, self.delay_idx as f32 - offset);
                self.shelf_state_l += shelf_alpha * (warped - self.shelf_state_l);
                let shelved = p.shelf_gain * warped + (1.0 - p.shelf_gain) * self.shelf_state_l;
                let hiss = lcg_noise(&mut self.rng_state) * p.hiss;
                let wet_out =
                    shelved + self.rumble_state + hiss + self.click_env + self.scratch_env;
                *s = dry + wet * (wet_out - dry);
            }

            // 右声道：偏心 wow 延迟线 → 高频搁架 → 表面噪声。
            if let Some(s) = frame.get_mut(1) {
                let dry = finite(*s);
                self.delay_r[self.delay_idx] = dry;
                let warped = delay_read(&self.delay_r, self.delay_idx as f32 - offset);
                self.shelf_state_r += shelf_alpha * (warped - self.shelf_state_r);
                let shelved = p.shelf_gain * warped + (1.0 - p.shelf_gain) * self.shelf_state_r;
                let hiss = lcg_noise(&mut self.rng_state) * p.hiss;
                let wet_out =
                    shelved + self.rumble_state + hiss + self.click_env + self.scratch_env;
                *s = dry + wet * (wet_out - dry);
            }

            self.delay_idx = (self.delay_idx + 1) % VINYL_DELAY_LEN;
        }

        #[cfg(test)]
        {
            self.process_calls += 1;
        }
    }

    fn reset(&mut self) {
        self.click_env = 0.0;
        self.rumble_state = 0.0;
        self.shelf_state_l = 0.0;
        self.shelf_state_r = 0.0;
        self.aged_phase = 0.0;
        self.scratch_env = 0.0;
        self.delay_l.fill(0.0);
        self.delay_r.fill(0.0);
        self.delay_idx = 0;
        // rng_state 刻意保留：避免每曲噪声序列完全相同。
    }
}

/// 介质切换交叉淡化的步数（每步一个回调，约 10ms；wet 由 1/(N+1) 渐入到 1）。
const MEDIUM_RAMP_STEPS: usize = 3;

/// 有状态效果器的聚合：磁带 / 黑胶各自常驻、互不排斥。
///
/// 音频回调闭包常驻持有一个本结构实例，切换介质不改本结构、
/// 只改 [`PlaybackMedium`] 原子量。新增介质 = 加一个字段 + 加一行 match。
#[derive(Debug, Default)]
pub(crate) struct MediumEffects {
    tape: TapeEffect,
    vinyl: VinylEffect,
    /// 上次 `apply` 见到的介质（检测切换用）。
    prev_medium: PlaybackMedium,
    /// 交叉淡化剩余步数（0 = 已完成 / 未在淡化）。
    ramp_remaining: usize,
}

impl MediumEffects {
    /// 按当前介质分发到对应效果器；`None` 直通跳过。
    ///
    /// 对 `medium` 做穷尽 match：新增介质变体时编译器会在此报错，
    /// 强制补上对应分发，避免新介质静默无效。
    pub(crate) fn apply(
        &mut self,
        medium: PlaybackMedium,
        data: &mut [f32],
        channels: usize,
        sample_rate: u32,
    ) {
        // 介质切换时启动短交叉淡化：`wet` 由 1/(N+1) 渐入到 1，
        // 兜底切换瞬间的电平 / 滤波器状态突变（增益归一后残留跳变极小）。
        if medium != self.prev_medium {
            self.prev_medium = medium;
            self.ramp_remaining = MEDIUM_RAMP_STEPS;
        }
        let wet = if self.ramp_remaining == 0 {
            1.0
        } else {
            let progress = (MEDIUM_RAMP_STEPS - self.ramp_remaining + 1) as f32;
            self.ramp_remaining -= 1;
            (progress / (MEDIUM_RAMP_STEPS + 1) as f32).min(1.0)
        };
        match medium {
            PlaybackMedium::None => {}
            PlaybackMedium::TapeClear
            | PlaybackMedium::TapeWhite
            | PlaybackMedium::TapeClassic
            | PlaybackMedium::TapeAged => {
                self.tape.params = tape_params(medium);
                self.tape.process(data, channels, sample_rate, wet);
            }
            PlaybackMedium::VinylClean
            | PlaybackMedium::VinylDynamic
            | PlaybackMedium::VinylStandard
            | PlaybackMedium::VinylAged => {
                self.vinyl.params = vinyl_params(medium);
                self.vinyl.process(data, channels, sample_rate, wet);
            }
        }
    }

    /// 重置全部效果器状态（换曲 / seek 时调用）。
    pub(crate) fn reset(&mut self) {
        self.tape.reset();
        self.vinyl.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_round_trips_all_variants() {
        for medium in PlaybackMedium::ALL {
            assert_eq!(medium.as_str().parse::<PlaybackMedium>(), Ok(medium));
        }
    }

    #[test]
    fn from_str_is_case_insensitive() {
        assert_eq!(
            "TAPE".parse::<PlaybackMedium>(),
            Ok(PlaybackMedium::TapeClassic)
        );
        assert_eq!(
            "Vinyl".parse::<PlaybackMedium>(),
            Ok(PlaybackMedium::VinylStandard)
        );
        assert_eq!("None".parse::<PlaybackMedium>(), Ok(PlaybackMedium::None));
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert_eq!("mp3".parse::<PlaybackMedium>(), Err(UnknownMedium));
        assert_eq!("".parse::<PlaybackMedium>(), Err(UnknownMedium));
        assert_eq!("vinyl record".parse::<PlaybackMedium>(), Err(UnknownMedium));
    }

    #[test]
    fn discriminant_round_trips_and_falls_back() {
        for medium in PlaybackMedium::ALL {
            assert_eq!(
                PlaybackMedium::from_discriminant(medium.discriminant()),
                medium
            );
        }
        // 未知判别值回退 None（不 panic）。
        assert_eq!(PlaybackMedium::from_discriminant(9), PlaybackMedium::None);
    }

    #[test]
    fn default_is_none() {
        assert_eq!(PlaybackMedium::default(), PlaybackMedium::None);
    }

    #[test]
    fn apply_none_is_passthrough() {
        let mut effects = MediumEffects::default();
        let mut data = [0.5f32, -0.5, 0.25, -0.25];
        let snapshot = data;
        effects.apply(PlaybackMedium::None, &mut data, 2, 48000);
        assert_eq!(data, snapshot);
        assert_eq!(effects.tape.process_calls, 0);
        assert_eq!(effects.vinyl.process_calls, 0);
    }

    #[test]
    fn apply_dispatches_to_correct_effect() {
        let mut effects = MediumEffects::default();
        let mut data = [0.0f32; 4];
        // Tape 命中磁带效果器。
        effects.apply(PlaybackMedium::TapeClassic, &mut data, 2, 48000);
        assert_eq!(effects.tape.process_calls, 1);
        assert_eq!(effects.vinyl.process_calls, 0);
        // Vinyl 命中黑胶效果器。
        effects.apply(PlaybackMedium::VinylStandard, &mut data, 2, 48000);
        assert_eq!(effects.tape.process_calls, 1);
        assert_eq!(effects.vinyl.process_calls, 1);
    }

    #[test]
    fn reset_is_harmless_on_stubs() {
        let mut effects = MediumEffects::default();
        effects.reset();
    }

    #[test]
    fn tape_effect_is_finite_and_changes() {
        let mut tape = TapeEffect::default();
        // 长度超过延迟线（32），确保延迟线填满后抖晃输出非零。
        let mut data = [0.5f32; 2048];
        tape.process(&mut data, 2, 48000, 1.0);
        assert!(
            data.iter().all(|s| s.is_finite()),
            "磁带 DSP 输出应为有限值"
        );
        assert!(
            data.iter().any(|&s| s.abs() > 0.001),
            "延迟线填满后磁带 DSP 应输出非零"
        );
    }

    #[test]
    fn tape_effect_reset_clears_delay() {
        let mut tape = TapeEffect::default();
        let mut data = [0.5f32; 64];
        tape.process(&mut data, 2, 48000, 1.0);
        tape.reset();
        assert!(tape.delay_l.iter().all(|&s| s == 0.0));
        assert!(tape.delay_r.iter().all(|&s| s == 0.0));
        assert_eq!(tape.delay_idx, 0);
        assert_eq!(tape.lfo_phase, 0.0);
    }

    #[test]
    fn tape_effect_no_panic_various_conditions() {
        let mut tape = TapeEffect::default();
        // 模拟音频回调：多次调用，不同长度 / 声道 / 采样率。
        for i in 0..20_000 {
            let len = (i % 480) + 1;
            let mut data = vec![0.3f32; len * 2];
            let sr = if i % 2 == 0 { 44_100 } else { 48_000 };
            let ch = if i % 3 == 0 { 1 } else { 2 };
            tape.process(&mut data, ch, sr, 1.0);
            assert!(data.iter().all(|s| s.is_finite()));
        }
    }

    #[test]
    fn vinyl_effect_is_finite_and_changes() {
        let mut vinyl = VinylEffect::default();
        let mut data = [0.5f32; 2048];
        let snapshot = data;
        vinyl.process(&mut data, 2, 48000, 1.0);
        assert!(
            data.iter().all(|s| s.is_finite()),
            "黑胶 DSP 输出应为有限值"
        );
        assert_ne!(data, snapshot, "黑胶 DSP 应改变样本（底噪/噼啪/搁架）");
    }

    // —— 回归：延迟线越界与搁架跨声道两个必现缺陷的防护线 ——

    /// 长跑 ≥5 秒、多采样率、多声道：曾在 1.75s 处索引越界 panic。
    #[test]
    fn tape_and_vinyl_long_run_no_panic() {
        let mut tape = TapeEffect::default();
        let mut vinyl = VinylEffect::default();
        for (sr, ch) in [(44_100, 1usize), (48_000, 2), (96_000, 8)] {
            let frames = sr as usize * 5;
            let mut data = vec![0.4f32; frames * ch];
            tape.process(&mut data, ch, sr, 1.0);
            assert!(data.iter().all(|s| s.is_finite()));
            data.fill(0.4);
            vinyl.process(&mut data, ch, sr, 1.0);
            assert!(data.iter().all(|s| s.is_finite()));
        }
    }

    /// 奇数长度切片（帧不对齐）：process 按 chunks_mut 容错，不应 panic。
    #[test]
    fn odd_length_slice_does_not_panic() {
        let mut tape = TapeEffect::default();
        let mut vinyl = VinylEffect::default();
        // 3 声道但长度不是 3 的倍数 → 末帧不完整。
        let mut a = [0.2f32; 7];
        tape.process(&mut a, 3, 48000, 1.0);
        assert!(a.iter().all(|s| s.is_finite()));
        let mut b = [0.2f32; 7];
        vinyl.process(&mut b, 3, 48000, 1.0);
        assert!(b.iter().all(|s| s.is_finite()));
    }

    /// 立体声分离：左声道正弦 / 右声道静音，右声道不得拿到左声道内容。
    /// （曾因搁架状态跨声道共享，右声道混入 80% 左声道内容，分离度 -2.2dB。）
    #[test]
    fn tape_stereo_separation() {
        let mut tape = TapeEffect::default();
        let sr = 48_000u32;
        let frames = sr as usize; // 1 秒
        let mut data = vec![0.0f32; frames * 2];
        for (i, pair) in data.chunks_mut(2).enumerate() {
            pair[0] = (i as f32 * std::f32::consts::TAU * 1000.0 / sr as f32).sin() * 0.5;
            pair[1] = 0.0;
        }
        tape.process(&mut data, 2, sr, 1.0);
        let rms = |idx: usize| {
            let sum: f32 = data.chunks(2).map(|p| p[idx] * p[idx]).sum();
            (sum / frames as f32).sqrt()
        };
        let (l, r) = (rms(0), rms(1));
        assert!(r < l * 0.1, "右声道不应混入左声道信号：L={l} R={r}");
    }

    /// 立体声分离（黑胶）：搁架状态已按 L/R 拆分，右声道只含共享噪声。
    #[test]
    fn vinyl_stereo_separation() {
        let mut vinyl = VinylEffect::default();
        let sr = 48_000u32;
        let frames = sr as usize; // 1 秒
        let mut data = vec![0.0f32; frames * 2];
        for (i, pair) in data.chunks_mut(2).enumerate() {
            pair[0] = (i as f32 * std::f32::consts::TAU * 1000.0 / sr as f32).sin() * 0.5;
            pair[1] = 0.0;
        }
        vinyl.process(&mut data, 2, sr, 1.0);
        let rms = |idx: usize| {
            let sum: f32 = data.chunks(2).map(|p| p[idx] * p[idx]).sum();
            (sum / frames as f32).sqrt()
        };
        let (l, r) = (rms(0), rms(1));
        // 右声道 = 共享隆隆 + 共享噼啪 + 自身嘶声，远小于左声道信号 + 噪声。
        assert!(r < l * 0.3, "右声道不应拿到左声道信号：L={l} R={r}");
    }

    /// NaN/Inf 输入被拦截，状态不被污染，后续干净样本输出有限。
    #[test]
    fn nan_input_is_sanitized() {
        let mut tape = TapeEffect::default();
        let mut vinyl = VinylEffect::default();
        let mut dirty = [f32::NAN, f32::INFINITY, 0.5, 0.5];
        tape.process(&mut dirty, 2, 48000, 1.0);
        assert!(dirty.iter().all(|s| s.is_finite()));
        // 污染样本之后接干净样本，仍应有限（状态未被 NaN 自我维持）。
        let mut clean = [0.5f32; 2048];
        tape.process(&mut clean, 2, 48000, 1.0);
        assert!(clean.iter().all(|s| s.is_finite()));
        let mut dirty_v = [f32::NAN, f32::INFINITY, 0.5, 0.5];
        vinyl.process(&mut dirty_v, 2, 48000, 1.0);
        assert!(dirty_v.iter().all(|s| s.is_finite()));
        let mut clean_v = [0.5f32; 2048];
        vinyl.process(&mut clean_v, 2, 48000, 1.0);
        assert!(clean_v.iter().all(|s| s.is_finite()));
    }

    /// 切介质触发交叉淡化：切换后首个回调 wet < 1，随后渐入到 1。
    #[test]
    fn medium_switch_ramps_wet() {
        let mut effects = MediumEffects::default();
        let mut data = [0.5f32; 4];
        let snapshot = data;
        // None → Tape：首个回调 wet = 1/4 = 0.25，输出介于干湿之间（≠干、≠全湿）。
        effects.apply(PlaybackMedium::TapeClassic, &mut data, 2, 48000);
        assert_ne!(data, snapshot, "wet=0.25 时应已有部分效果");
        // 连续多回调后 wet 到达 1（此时不再等于 snapshot，且第二次逼近全湿）。
        for _ in 0..(MEDIUM_RAMP_STEPS + 2) {
            data = snapshot;
            effects.apply(PlaybackMedium::TapeClassic, &mut data, 2, 48000);
        }
        // 淡化结束后 ramp_remaining 归零。
        assert_eq!(effects.ramp_remaining, 0);
    }
}
