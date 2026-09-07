//! # WavPack 解码后端（`.wv` 无损流）
//!
//! 基于纯 Rust 的 `wavicle` crate 实现 [DecoderBackend]，使 tuneux-corex
//! 在 symphonia 与 Opus 之外新增第三个后端，验证"可插拔"抽象接缝。
//!
//! ## 关键设计
//!
//! - **整体解码**：`wavicle` 0.1.0 只暴露 `wavicle::decode_stream`
//!   （一次性解出整个流的交错 `Vec<i32>`），其内部"逐块解码"函数
//!   `decode_block` 是私有实现、未公开。因此本后端在 `open` 时把整段
//!   PCM 一次解出、保留为 i32 缓冲，`decode_next` 再按固定帧数切片输出，
//!   对调用方（解码线程）仍表现为"流式分块"。代价是打开即占整段 PCM 内存
//!   （见下文"内存局限"）。
//! - **样本格式**：整数样本以"本地位宽"右对齐存储（16/24/32 位），除以
//!   `2^(位深-1)` 归一化到 f32；浮点流（32-bit float）的每个 i32 即 f32
//!   的 IEEE-754 位型，用 `f32::from_bits` 逐位还原（保留 NaN/±0/次正规数，
//!   位精确）。
//! - **seek**：因整段样本已在内存，seek 是 O(1) 的游标定位，无需从头重扫。
//!   这与 OpusBackend 的"从头重扫"风格不同——是 wavicle 整体解码带来的偏差。
//! - **范围限制**：wavicle 只支持 1~2 声道、16/24/32 位整数与 32 位浮点；
//!   多声道 / DSD / hybrid / 8-bit 流会被 wavicle 以 `OutOfScope` 或
//!   `NotYetImplemented` 拒绝，本模块统一映射为"不支持"。
//!
//! ## 内存局限
//!
//! WavPack 是无损格式，解压后 PCM 体积 ≈ 位深/8 × 采样率 × 声道 × 时长。
//! 整体解码意味着一个 5 分钟 44.1kHz / 16-bit 立体声文件约占用
//! `44100 × 300 × 2 × 4 ≈ 106 MB`（i32 缓冲），长文件 / 高采样率更高。
//! 这是 wavicle 公开 API 的限制；待其开放逐块解码接口后可降为真·流式。

use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// 每次 `decode_next` 输出的帧数（每声道样本数）。
///
/// 4096 帧在 44.1kHz 下约 93ms，与 symphonia 每包、Opus 每帧的量级相当，
/// 既保证 ringbuf 响应，又不至于切得太碎。整体解码后按此固定帧数切片。
const CHUNK_FRAMES: usize = 4096;

/// 判断路径是否应交给 WavPack 后端：扩展名为 `.wv`（大小写不敏感）。
pub(crate) fn is_wavpack_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wv"))
        .unwrap_or(false)
}

/// 轻量探测 `.wv` 文件的采样率（只读首块头，不建解码器、不解码）。
///
/// 供 `super::decoder::probe_sample_rate` 在"原生采样率直通"判断中复用：
/// 标准采样率直接从首块头的 rate index 读出；非标准采样率（index 0xF）
/// 再读首块的 `ID_SAMPLE_RATE` 子块（3 字节 LE）。任何读取失败都返回
/// None（调用方回退到设备默认采样率 + 软件重采样），零副作用。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;

    // 首块头 32 字节：含魔数 / 版本 / flags（其中 rate index 决定标准采样率）。
    let mut header = [0u8; wavicle::block::HEADER_LEN];
    file.read_exact(&mut header).ok()?;
    let h = wavicle::BlockHeader::parse(&header).ok()?;

    if let Some(rate) = h.flags.sample_rate() {
        return Some(rate);
    }

    // 非标准采样率：读首块剩余元数据，找 ID_SAMPLE_RATE 子块（3 字节 LE）。
    // 用 saturating_sub 防御性处理（parse 已保证 block_len >= 32，此处仅兜底）。
    let meta_len = h.block_len().saturating_sub(wavicle::block::HEADER_LEN);
    let mut metadata = vec![0u8; meta_len];
    file.read_exact(&mut metadata).ok()?;
    let parsed = wavicle::Block {
        header: h,
        metadata: &metadata,
    };
    for sub in parsed.sub_blocks().flatten() {
        if sub.id == wavicle::format::meta::SAMPLE_RATE && sub.data.len() >= 3 {
            return Some(
                u32::from(sub.data[0]) | u32::from(sub.data[1]) << 8 | u32::from(sub.data[2]) << 16,
            );
        }
    }
    None
}

/// WavPack 解码后端（内部实现，经 [`super::decoder::open_backend`] 分发）。
///
/// 一个实例对应一个已打开的 `.wv` 文件；换曲时丢弃旧实例、新建。
/// 打开时即整段解码（wavicle 无逐块公开 API），`decode_next` 按块切片输出。
pub(crate) struct WavPackBackend {
    /// 技术参数（采样率 / 声道 / 位深 / 编码名 / 时长）。
    params: AudioParams,
    /// 整段解码得到的交错样本（i32）。
    ///
    /// 整数流为"本地位宽右对齐"的符号值（16/24/32 位）；浮点流为 f32 的
    /// IEEE-754 位型。两者由 `is_float` 区分后转 f32。
    samples: Vec<i32>,
    /// 是否 32-bit 浮点流（决定 i32 → f32 的转换方式）。
    is_float: bool,
    /// 声道数（缓存，避免反复从 params 取）。
    channels: usize,
    /// 整数样本归一化的除数（`2^(位深-1)`）；浮点流不使用。
    int_divisor: f64,
    /// 下一个 `decode_next` 输出的起始帧号（每声道计）。
    cursor_frames: usize,
}

impl WavPackBackend {
    /// 打开 `.wv` 文件：整段解码，建立技术参数。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        // wavicle 只接受字节流：整文件读入后一次性解码（读入的字节在解码后即可释放）。
        let bytes = std::fs::read(path)?;
        let decoded = wavicle::decode_stream(&bytes).map_err(map_wavicle_error)?;

        let channels = decoded.channels as usize;
        // wavicle 只产出 1~2 声道；0 视为异常，防御性拒绝。
        if channels == 0 {
            return Err(DecodeError::Unsupported("WavPack 声道数为 0".to_string()));
        }
        let sample_rate = decoded.sample_rate;

        // 总帧数 = 交错样本数 / 声道数；据此精确计算时长（比 Opus 的 None 更完整）。
        let total_frames = decoded.samples.len() / channels;
        let duration = if sample_rate == 0 {
            None
        } else {
            Some(total_frames as f64 / f64::from(sample_rate))
        };

        // 整数归一化除数：2^(位深-1)。位深 < 2 时兜底 1.0（正常不会发生，仅防除零）。
        let bits = decoded.bits_per_sample;
        let int_divisor = if bits >= 2 {
            2.0f64.powi((bits - 1) as i32)
        } else {
            1.0
        };

        let params = AudioParams::new(
            Some(sample_rate),
            Some(channels as u16),
            Some(decoded.bits_per_sample),
            "WavPack".to_string(),
            0,        // .wv 无"音轨 ID"概念，固定 0（调用方不按轨过滤）。
            duration, // 整段解码后可由样本数精确计算。
        );

        Ok(Self {
            params,
            samples: decoded.samples,
            is_float: decoded.is_float,
            channels,
            int_divisor,
            cursor_frames: 0,
        })
    }

    /// 把单个解码样本 i32 转成交错输出 f32。
    ///
    /// 浮点流：i32 即 f32 的 IEEE 位型，逐位还原；整数流：除以 2^(位深-1)
    /// 归一化到 [-1, 1)。整数经 f64 中转再截到 f32，避免直接 `as f32`
    /// 在 24/32 位大值上引入不必要的舍入。
    #[inline]
    fn to_f32(&self, s: i32) -> f32 {
        if self.is_float {
            f32::from_bits(s as u32)
        } else {
            (s as f64 / self.int_divisor) as f32
        }
    }
}

/// 把 wavicle 的错误映射为后端无关的 [DecodeError]。
///
/// 分类规则：
/// - `OutOfScope`（有效 WavPack 但超范围：多声道 / DSD / hybrid）→ 不支持
/// - `NotYetImplemented`（如 8-bit 整数）→ 不支持
/// - 其余（CRC 不匹配 / 截断 / 坏魔数 / 坏子块等畸形数据）→ 解码错误
fn map_wavicle_error(e: wavicle::Error) -> DecodeError {
    use wavicle::Error;
    match e {
        Error::OutOfScope(scope) => {
            DecodeError::Unsupported(format!("WavPack 超出支持范围：{scope}"))
        }
        Error::NotYetImplemented(what) => DecodeError::Unsupported(format!("WavPack {what}")),
        other => DecodeError::Decode(format!("WavPack 解码失败：{other}")),
    }
}

impl DecoderBackend for WavPackBackend {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        let total_frames = self.samples.len() / self.channels;
        if self.cursor_frames >= total_frames {
            return Ok(None); // EOF
        }

        // 切出本块：最多 CHUNK_FRAMES 帧，且不超过剩余帧数。
        let frames = CHUNK_FRAMES.min(total_frames - self.cursor_frames);
        let start = self.cursor_frames * self.channels;
        let end = start + frames * self.channels;

        let mut out = Vec::with_capacity(frames * self.channels);
        for &s in &self.samples[start..end] {
            out.push(self.to_f32(s));
        }
        self.cursor_frames += frames;
        Ok(Some(out))
    }

    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek("seek 秒数必须为非负有限值".to_string()));
        }
        // 整段样本已在内存：seek 即按采样率换算目标帧并移动游标（O(1)）。
        let rate = f64::from(self.params.sample_rate.unwrap_or(44_100));
        let target = (secs * rate).round() as usize;
        let total_frames = self.samples.len() / self.channels;
        // 越过末尾时钳制到末尾：随后 decode_next 返回 None（自然结束，不报错）。
        self.cursor_frames = target.min(total_frames);
        Ok(secs)
    }
}
