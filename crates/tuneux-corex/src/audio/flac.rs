//! # FLAC 自研解码后端
//!
//! 自研 FLAC 解码后端：接管 `.flac` 的解码路径。
//! 纯 std 手写、零新增依赖、流式逐帧解码（不整段入内存）。
//!
//! 支持面（覆盖参考编码器全部产出形态）：
//! - 元数据：STREAMINFO（必含）+ SEEKTABLE（可选，供快速 seek）；
//!   其余块（VORBIS_COMMENT / PICTURE 等）跳过——标签读取仍走
//!   symphonia（`probe_metadata` 不动，元数据零退化）；
//! - 子帧四型：CONSTANT / VERBATIM / FIXED（0-4 阶）/ LPC（1-32 阶，
//!   64 位中间累加，含负移位）；
//! - 残差：Rice / Rice2 分区编码 + 逃逸（原始位宽）形态，分区阶任意；
//! - 声道：1-8 独立 + 左/侧、右/侧、中/侧三种立体声去相关
//!   （侧声道位深 +1）；位深 4-32 位任意；
//! - 完整性：帧头 CRC-8 与整帧 CRC-16 双校验（损坏即报 Decode 错误）；
//!   尾部截断宽容（播到断点）。
//!
//! seek 策略：SEEKTABLE 在 → 定位不大于目标的最近 seek 点后解码丢弃至
//! 精确帧；不在 → 从首帧解码丢弃（v1 保守口径，大文件 seek 偏慢，
//! 注释在案）。严格 `fLaC` 魔数在偏移 0——带 ID3 前缀的文件返回
//! [`DecodeError::Unsupported`]，由 `open_backend` 回退 symphonia。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// 单次解码输出的最大帧数（与其他后端同口径）。
const CHUNK_FRAMES: usize = 4096;

// =============================================================================
// CRC 表（编译期生成）
// =============================================================================

/// CRC-8 查找表（多项式 0x07，初值 0，MSB 在前；FLAC 帧头校验）。
const fn make_crc8_table() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u8;
        let mut b = 0;
        while b < 8 {
            v = if v & 0x80 != 0 {
                (v << 1) ^ 0x07
            } else {
                v << 1
            };
            b += 1;
        }
        t[i] = v;
        i += 1;
    }
    t
}

/// CRC-16 查找表（多项式 0x8005，初值 0，MSB 在前；FLAC 整帧校验）。
const fn make_crc16_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            v = if v & 0x8000 != 0 {
                (v << 1) ^ 0x8005
            } else {
                v << 1
            };
            b += 1;
        }
        t[i] = v;
        i += 1;
    }
    t
}

const CRC8_TABLE: [u8; 256] = make_crc8_table();
const CRC16_TABLE: [u16; 256] = make_crc16_table();

fn crc8_update(crc: u8, byte: u8) -> u8 {
    CRC8_TABLE[(crc ^ byte) as usize]
}

fn crc16_update(crc: u16, byte: u8) -> u16 {
    CRC16_TABLE[(((crc >> 8) ^ byte as u16) & 0xFF) as usize] ^ (crc << 8)
}

// =============================================================================
// 位读器（MSB 在前，直接基于 File；按字节消费记账维护帧级 CRC-8/16）
// =============================================================================

/// 流式位读器：每次从文件按需取字节填入 64 位缓冲，MSB 优先消费。
/// 预读可能越出当前消费点，因此 CRC 不随取入更新，而随**消费**更新——
/// 取入的字节进待消费队列，位数用尽才计入帧级 CRC（`fcrc8` / `fcrc16`，
/// 每帧由 start_frame 清零，与 FLAC「CRC 只覆盖本帧」的语义对齐）。
struct BitReader {
    file: File,
    /// 位缓冲（有效位在低位侧，共 `bits` 位）。
    buf: u64,
    bits: u32,
    /// 已取入的总字节位置（文件偏移）。
    pos: u64,
    /// 已取入但未完全消费的字节（值 + 残余未消费位数）。
    pending: std::collections::VecDeque<(u8, u8)>,
    /// 本帧已消费字节的 CRC-8 / CRC-16（start_frame 清零）。
    fcrc8: u8,
    fcrc16: u16,
    /// 是否已到文件尾。
    eof: bool,
}

impl BitReader {
    fn new(file: File, pos: u64) -> Self {
        Self {
            file,
            buf: 0,
            bits: 0,
            pos,
            pending: std::collections::VecDeque::new(),
            fcrc8: 0,
            fcrc16: 0,
            eof: false,
        }
    }

    /// 重新定位（seek 后缓冲与待消费队列作废、帧 CRC 重置）。
    fn reset_to(&mut self, pos: u64) -> Result<(), DecodeError> {
        self.file.seek(SeekFrom::Start(pos))?;
        self.buf = 0;
        self.bits = 0;
        self.pos = pos;
        self.pending.clear();
        self.fcrc8 = 0;
        self.fcrc16 = 0;
        self.eof = false;
        Ok(())
    }

    /// 帧起点：帧级 CRC 清零（FLAC 的 CRC-8/16 均只覆盖本帧）。
    fn start_frame(&mut self) {
        self.fcrc8 = 0;
        self.fcrc16 = 0;
    }

    /// 从文件补一字节入缓冲；返回是否成功（文件尾返回 false）。
    fn fill_byte(&mut self) -> Result<bool, DecodeError> {
        let mut b = [0u8; 1];
        match self.file.read(&mut b) {
            Ok(0) => {
                self.eof = true;
                Ok(false)
            }
            Ok(_) => {
                self.buf = (self.buf << 8) | u64::from(b[0]);
                self.bits += 8;
                self.pos += 1;
                self.pending.push_back((b[0], 8));
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => self.fill_byte(),
            Err(e) => Err(DecodeError::Io(e.to_string())),
        }
    }

    /// 消费 n 位：逐字节记账，用尽的字节计入帧级 CRC。
    fn consume(&mut self, n: u32) {
        let mut left = n;
        while left > 0 {
            let Some((.., bits_left)) = self.pending.front_mut() else {
                break; // 不变量：pending 位数恒等于 buf 位数，不应到此
            };
            let take = (*bits_left as u32).min(left);
            *bits_left -= take as u8;
            left -= take;
            if *bits_left == 0 {
                let (b, _) = self.pending.pop_front().expect("front 已确认存在");
                self.fcrc8 = crc8_update(self.fcrc8, b);
                self.fcrc16 = crc16_update(self.fcrc16, b);
            }
        }
        self.bits -= n;
        self.buf &= if self.bits == 64 {
            u64::MAX
        } else if self.bits == 0 {
            0
        } else {
            (1u64 << self.bits) - 1
        };
    }

    /// 读 n 位（0 ≤ n ≤ 32）。不足且文件尾 → 截断类 Decode 错误。
    fn read_bits(&mut self, n: u32) -> Result<u32, DecodeError> {
        Ok(self.read_bits64(n)? as u32)
    }

    /// 读 n 位（0 ≤ n ≤ 64），64 位返回（总样本数 / seek 点用）。
    fn read_bits64(&mut self, n: u32) -> Result<u64, DecodeError> {
        debug_assert!(n <= 64);
        if n == 0 {
            return Ok(0);
        }
        while self.bits < n {
            if !self.fill_byte()? {
                return Err(DecodeError::Decode("帧中途到达文件尾（截断）".into()));
            }
        }
        let v = if n == 64 {
            self.buf
        } else {
            (self.buf >> (self.bits - n)) & ((1u64 << n) - 1)
        };
        self.consume(n);
        Ok(v)
    }

    /// 读带符号 n 位（补码）。
    fn read_signed(&mut self, n: u32) -> Result<i64, DecodeError> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read_bits64(n)?;
        let shift = 64 - n;
        Ok(((v << shift) as i64) >> shift)
    }

    /// 读一元编码（连续 0 的个数，遇 1 停）。
    fn read_unary(&mut self) -> Result<u32, DecodeError> {
        let mut q = 0u32;
        loop {
            let bit = self.read_bits(1)?;
            if bit == 1 {
                return Ok(q);
            }
            q += 1;
            // 防御：畸流下一元长度不可能超过一块样本数 × 64。
            if q > 1_000_000 {
                return Err(DecodeError::Decode("一元编码超长（流损坏）".into()));
            }
        }
    }

    /// 字节对齐（丢弃不足一字节的残余位；残余位属本帧字节，计入 CRC）。
    fn align_byte(&mut self) {
        let drop = self.bits % 8;
        self.consume(drop);
    }
}

// =============================================================================
// 元数据解析
// =============================================================================

/// STREAMINFO 的关键字段（其余最小/最大帧尺寸等不消费）。
struct StreamInfo {
    sample_rate: u32,
    channels: u8,
    bits_per_sample: u8,
    total_samples: u64,
}

/// SEEKTABLE 的一个 seek 点（样本序号 → 首帧起的相对字节偏移）。
#[derive(Debug, Clone, Copy)]
struct SeekPoint {
    sample: u64,
    offset: u64,
}

/// 元数据解析结果：STREAMINFO + 首帧文件偏移 + 可选 SEEKTABLE。
struct FlacMeta {
    info: StreamInfo,
    first_frame_offset: u64,
    seek_table: Option<Vec<SeekPoint>>,
}

/// 解析 fLaC 头与元数据块链；返回首帧位置与 STREAMINFO。
fn parse_metadata(file: &mut File) -> Result<FlacMeta, DecodeError> {
    file.seek(SeekFrom::Start(0))?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic)?;
    if &magic != b"fLaC" {
        return Err(DecodeError::Unsupported(
            "不是 FLAC 流（或带 ID3 前缀）".into(),
        ));
    }
    let mut info: Option<StreamInfo> = None;
    let mut seek_table = None;
    loop {
        let mut head = [0u8; 4];
        file.read_exact(&mut head).map_err(|e| {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                DecodeError::Decode("元数据块链截断".into())
            } else {
                DecodeError::Io(e.to_string())
            }
        })?;
        let is_last = head[0] & 0x80 != 0;
        let block_type = head[0] & 0x7F;
        let len = u32::from_be_bytes([0, head[1], head[2], head[3]]) as u64;
        match block_type {
            0 => {
                // STREAMINFO：34 字节定长，必须首块。
                if len != 34 {
                    return Err(DecodeError::Decode("STREAMINFO 长度非法".into()));
                }
                let mut b = [0u8; 34];
                file.read_exact(&mut b)?;
                let sample_rate =
                    (u32::from(b[10]) << 12) | (u32::from(b[11]) << 4) | (u32::from(b[12]) >> 4);
                let channels = ((b[12] >> 1) & 0x07) + 1;
                let bits_per_sample = (((b[12] & 0x01) << 4) | (b[13] >> 4)) + 1;
                let total_samples = (u64::from(b[13] & 0x0F) << 32)
                    | (u64::from(b[14]) << 24)
                    | (u64::from(b[15]) << 16)
                    | (u64::from(b[16]) << 8)
                    | u64::from(b[17]);
                if sample_rate == 0 || sample_rate > 655_350 {
                    return Err(DecodeError::Decode("STREAMINFO 采样率非法".into()));
                }
                info = Some(StreamInfo {
                    sample_rate,
                    channels,
                    bits_per_sample,
                    total_samples,
                });
            }
            3 => {
                // SEEKTABLE：每条 18 字节（样本序号 / 相对偏移 / 帧内样本数）。
                let mut points = Vec::new();
                let mut left = len;
                while left >= 18 {
                    let mut b = [0u8; 18];
                    file.read_exact(&mut b)?;
                    let sample = u64::from_be_bytes(b[0..8].try_into().expect("8 字节"));
                    let offset = u64::from_be_bytes(b[8..16].try_into().expect("8 字节"));
                    // 占位点（sample = u64::MAX）忽略。
                    if sample != u64::MAX {
                        points.push(SeekPoint { sample, offset });
                    }
                    left -= 18;
                }
                if left != 0 {
                    file.seek(SeekFrom::Current(left as i64))?;
                }
                if !points.is_empty() {
                    seek_table = Some(points);
                }
            }
            _ => {
                file.seek(SeekFrom::Current(len as i64))?;
            }
        }
        if is_last {
            break;
        }
    }
    let info = info.ok_or_else(|| DecodeError::Decode("缺 STREAMINFO 块".into()))?;
    let first_frame_offset = file.stream_position()?;
    Ok(FlacMeta {
        info,
        first_frame_offset,
        seek_table,
    })
}

// =============================================================================
// 帧解码
// =============================================================================

/// 声道分配（帧头 4 位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelAssignment {
    /// 独立声道（1-8）。
    Independent(u8),
    /// 左 + 侧（right = left - side）。
    LeftSide,
    /// 右 + 侧（left = right + side）。
    RightSide,
    /// 中 + 侧（mid/side 重建）。
    MidSide,
}

/// 帧头解析结果。
struct FrameHeader {
    block_size: u32,
    assignment: ChannelAssignment,
    bits_per_sample: u8,
}

/// 采样率编码表（帧头 4 位；0 = 用 STREAMINFO）。
fn parse_frame_header(r: &mut BitReader, info: &StreamInfo) -> Result<FrameHeader, DecodeError> {
    let sync = r.read_bits(14)?;
    if sync != 0b11_1111_1111_1110 {
        return Err(DecodeError::Decode(
            "帧同步码丢失（流损坏或偏移错位）".into(),
        ));
    }
    let _reserved = r.read_bits(1)?;
    let _blocking_strategy = r.read_bits(1)?;
    let bs_code = r.read_bits(4)?;
    let sr_code = r.read_bits(4)?;
    let ch_code = r.read_bits(4)?;
    let bps_code = r.read_bits(3)?;
    let _reserved2 = r.read_bits(1)?;

    // 帧号 / 样本号（UTF-8 风格变长，最多 7 字节）：本实现不消费其值，
    // 但必须正确跳过对应字节数。
    let first = r.read_bits(8)? as u8;
    let extra = if first & 0x80 == 0 {
        0
    } else if first & 0xE0 == 0xC0 {
        1
    } else if first & 0xF0 == 0xE0 {
        2
    } else if first & 0xF8 == 0xF0 {
        3
    } else if first & 0xFC == 0xF8 {
        4
    } else if first & 0xFE == 0xFC {
        5
    } else if first == 0xFE {
        6
    } else {
        return Err(DecodeError::Decode("帧号编码非法".into()));
    };
    for _ in 0..extra {
        r.read_bits(8)?;
    }

    // 块尺寸。
    let block_size = match bs_code {
        0b0001 => 192,
        0b0010..=0b0101 => 576 << (bs_code - 2),
        0b0110 => r.read_bits(8)? + 1,
        0b0111 => r.read_bits(16)? + 1,
        0b1000..=0b1111 => 256 << (bs_code - 8),
        _ => return Err(DecodeError::Decode("块尺寸编码非法".into())),
    };
    // 采样率（本实现只消费 STREAMINFO 值；显式编码须正确跳过附加字节）。
    match sr_code {
        0b0000 => {} // STREAMINFO
        0b1100 => {
            r.read_bits(8)?;
        }
        0b1101 | 0b1110 => {
            r.read_bits(16)?;
        }
        0b1111 => return Err(DecodeError::Decode("采样率编码非法".into())),
        _ => {} // 其余为定值表，不消费附加字节
    }
    // 位深。
    let bits_per_sample = match bps_code {
        0b000 => info.bits_per_sample,
        0b001 => 8,
        0b010 => 12,
        0b100 => 16,
        0b101 => 20,
        0b110 => 24,
        0b111 => 32,
        _ => return Err(DecodeError::Decode("位深编码非法".into())),
    };
    let assignment = match ch_code {
        0..=7 => {
            let n = ch_code as u8 + 1;
            if n != info.channels {
                return Err(DecodeError::Decode("帧声道数与 STREAMINFO 不符".into()));
            }
            ChannelAssignment::Independent(n)
        }
        8 => ChannelAssignment::LeftSide,
        9 => ChannelAssignment::RightSide,
        10 => ChannelAssignment::MidSide,
        _ => return Err(DecodeError::Decode("声道分配编码非法".into())),
    };
    // 帧头 CRC-8（覆盖同步码起到此的全部字节）。
    let computed8 = r.fcrc8;
    let expected8 = r.read_bits(8)? as u8;
    if computed8 != expected8 {
        return Err(DecodeError::Decode(format!(
            "帧头 CRC-8 校验失败（算得 {computed8:#04x}，流记 {expected8:#04x}）"
        )));
    }
    Ok(FrameHeader {
        block_size,
        assignment,
        bits_per_sample,
    })
}

/// 残差分区解码（Rice / Rice2 + 逃逸形态），把残差值写入 out。
fn decode_residual(
    r: &mut BitReader,
    block_size: u32,
    predictor_order: u32,
    out: &mut Vec<i64>,
) -> Result<(), DecodeError> {
    let method = r.read_bits(2)?;
    if method > 1 {
        return Err(DecodeError::Decode("残差编码方式非法".into()));
    }
    let param_bits = if method == 0 { 4 } else { 5 };
    let escape = if method == 0 { 0xF } else { 0x1F };
    let porder = r.read_bits(4)?;
    let partitions = 1u32 << porder;
    if porder > 0 && !block_size.is_multiple_of(partitions) {
        return Err(DecodeError::Decode("分区阶与块尺寸不符".into()));
    }
    out.clear();
    out.reserve(block_size as usize);
    for p in 0..partitions {
        let mut n = block_size >> porder;
        if p == 0 {
            // 首分区先扣掉预测阶数的暖机样本数。
            if n < predictor_order {
                return Err(DecodeError::Decode("首分区小于预测阶数".into()));
            }
            n -= predictor_order;
        }
        let param = r.read_bits(param_bits)?;
        if param == escape {
            // 逃逸：5 位原始位宽，逐样本按补码读。
            let raw_bits = r.read_bits(5)?;
            for _ in 0..n {
                out.push(if raw_bits == 0 {
                    0
                } else {
                    r.read_signed(raw_bits)?
                });
            }
        } else {
            for _ in 0..n {
                let q = r.read_unary()?;
                let rem = if param > 0 { r.read_bits64(param)? } else { 0 };
                let folded = ((q as u64) << param) | rem;
                // 之字形展开：偶 = 非负，奇 = 负。
                out.push(if folded & 1 == 0 {
                    (folded >> 1) as i64
                } else {
                    -((folded >> 1) as i64) - 1
                });
            }
        }
    }
    Ok(())
}

/// 解码一个子帧，输出该声道的样本（写入 out，长度 = block_size）。
fn decode_subframe(
    r: &mut BitReader,
    block_size: u32,
    bps: u8,
    out: &mut Vec<i64>,
) -> Result<(), DecodeError> {
    let _padding = r.read_bits(1)?;
    let type_code = r.read_bits(6)?;
    let wasted = if r.read_bits(1)? == 1 {
        r.read_unary()? + 1
    } else {
        0
    };
    let eff_bps = bps
        .checked_sub(wasted as u8)
        .ok_or_else(|| DecodeError::Decode("无效位超过位深".into()))?;
    if eff_bps == 0 {
        return Err(DecodeError::Decode("有效位深为 0".into()));
    }

    match type_code {
        0 => {
            // CONSTANT：整块同值。
            let v = r.read_signed(eff_bps as u32)?;
            out.clear();
            out.resize(block_size as usize, v);
        }
        1 => {
            // VERBATIM：原始样本直存。
            out.clear();
            out.reserve(block_size as usize);
            for _ in 0..block_size {
                out.push(r.read_signed(eff_bps as u32)?);
            }
        }
        t if (0b001000..=0b001100).contains(&t) => {
            // FIXED：0-4 阶定系数预测 + Rice 残差。
            let order = (t & 0x07) as usize;
            if block_size < order as u32 {
                return Err(DecodeError::Decode("块尺寸小于预测阶数".into()));
            }
            out.clear();
            out.reserve(block_size as usize);
            for _ in 0..order {
                out.push(r.read_signed(eff_bps as u32)?);
            }
            let mut residual = Vec::new();
            decode_residual(r, block_size, order as u32, &mut residual)?;
            for (i, &res) in residual.iter().enumerate() {
                let n = order + i;
                let pred = match order {
                    0 => 0,
                    1 => out[n - 1],
                    2 => 2 * out[n - 1] - out[n - 2],
                    3 => 3 * out[n - 1] - 3 * out[n - 2] + out[n - 3],
                    _ => 4 * out[n - 1] - 6 * out[n - 2] + 4 * out[n - 3] - out[n - 4],
                };
                out.push(pred + res);
            }
        }
        t if t & 0x20 != 0 => {
            // LPC：1-32 阶线性预测 + Rice 残差（64 位中间累加）。
            let order = ((t & 0x1F) + 1) as usize;
            if block_size < order as u32 {
                return Err(DecodeError::Decode("块尺寸小于 LPC 阶数".into()));
            }
            out.clear();
            out.reserve(block_size as usize);
            for _ in 0..order {
                out.push(r.read_signed(eff_bps as u32)?);
            }
            let precision = r.read_bits(4)? + 1;
            if precision == 16 {
                return Err(DecodeError::Decode("LPC 精度编码非法".into()));
            }
            let shift = r.read_signed(5)?;
            let mut coef = Vec::with_capacity(order);
            for _ in 0..order {
                coef.push(r.read_signed(precision)?);
            }
            let mut residual = Vec::new();
            decode_residual(r, block_size, order as u32, &mut residual)?;
            for (i, &res) in residual.iter().enumerate() {
                let n = order + i;
                let mut sum: i64 = 0;
                for (j, &c) in coef.iter().enumerate() {
                    sum += c * out[n - 1 - j];
                }
                let pred = if shift >= 0 {
                    sum >> shift
                } else {
                    sum << (-shift) as u32
                };
                out.push(pred + res);
            }
        }
        _ => return Err(DecodeError::Decode("子帧类型编码非法".into())),
    }
    // FLAC 规范：子帧头声明 wasted bits 时，样本最低 k 位在编码时被截掉，
    // 解码后必须把每个样本左移 k 位还原（与 symphonia 的 samples_shl 等价）。
    // 不还原则 16bit 母带封装进 24bit 容器（k=8）整体衰减 2^8 倍（−48 dB），
    // 且帧 CRC 校验的是原始帧字节而非样本值——静默错音无法被现有校验发现。
    if wasted > 0 {
        for s in out.iter_mut() {
            *s <<= wasted;
        }
    }
    Ok(())
}

/// 解码一整帧，输出交错前各声道样本（channels × block_size）。
/// 返回帧内样本数（帧数）。CRC-16 校验失败 → Decode 错误。
fn decode_frame(
    r: &mut BitReader,
    info: &StreamInfo,
    out: &mut Vec<Vec<i64>>,
) -> Result<u32, DecodeError> {
    r.start_frame();
    let header = parse_frame_header(r, info)?;
    let channels = match header.assignment {
        ChannelAssignment::Independent(n) => n as usize,
        _ => 2,
    };
    out.clear();
    out.resize(channels, Vec::new());
    for (ch, slot) in out.iter_mut().enumerate() {
        // 侧声道位深 +1（左/侧、右/侧的侧道；中/侧的侧道）。
        let sub_bps = match (header.assignment, ch) {
            (ChannelAssignment::LeftSide, 1)
            | (ChannelAssignment::RightSide, 0)
            | (ChannelAssignment::MidSide, 1) => header.bits_per_sample + 1,
            _ => header.bits_per_sample,
        };
        let mut sub = Vec::new();
        decode_subframe(r, header.block_size, sub_bps, &mut sub)?;
        *slot = sub;
    }
    // 帧尾按字节对齐后读 CRC-16（覆盖本帧全部字节，不含本字段自身）。
    r.align_byte();
    let computed = r.fcrc16;
    let expected = r.read_bits(16)? as u16;
    if computed != expected {
        return Err(DecodeError::Decode(format!(
            "帧 CRC-16 校验失败（算得 {computed:#06x}，流记 {expected:#06x}）"
        )));
    }

    // 声道去相关。
    match header.assignment {
        ChannelAssignment::Independent(_) => {}
        ChannelAssignment::LeftSide => {
            // left = l（原样，写回保持借用安全的一致结构）、side = l - s。
            let (l, s) = (out[0].clone(), out[1].clone());
            for i in 0..l.len() {
                out[1][i] = l[i] - s[i];
            }
        }
        ChannelAssignment::RightSide => {
            // left = right + side（out[0] 写回 left，与枚举文档一致）。
            let (s, rr) = (out[0].clone(), out[1].clone());
            for i in 0..s.len() {
                out[0][i] = rr[i] + s[i];
            }
        }
        ChannelAssignment::MidSide => {
            let (m, s) = (out[0].clone(), out[1].clone());
            for i in 0..m.len() {
                let mid = (m[i] << 1) | (s[i] & 1);
                out[0][i] = (mid + s[i]) >> 1;
                out[1][i] = (mid - s[i]) >> 1;
            }
        }
    }
    Ok(header.block_size)
}

// =============================================================================
// 后端
// =============================================================================

/// 按扩展名或 fLaC 魔数判定 FLAC 文件。
pub(crate) fn is_flac_path(path: &Path) -> bool {
    let by_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("flac"));
    if by_ext {
        return true;
    }
    let mut buf = [0u8; 4];
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    f.read_exact(&mut buf).is_ok() && &buf == b"fLaC"
}

/// 轻量采样率探测（原生采样率直通用）：只读 STREAMINFO。
/// 失败返回 None，调用方按「不直通」降级处理。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;
    parse_metadata(&mut file).ok().map(|m| m.info.sample_rate)
}

/// FLAC 解码后端：流式逐帧解码，帧样本先入行缓冲再按块产出。
pub(crate) struct FlacBackend {
    params: AudioParams,
    reader: BitReader,
    info: StreamInfo,
    first_frame_offset: u64,
    seek_table: Option<Vec<SeekPoint>>,
    /// 已解码未产出的交错样本缓冲（f32 归一化后）。
    pending: std::collections::VecDeque<f32>,
    /// 音频数据结束的文件偏移（seek 点范围校验用）。0 = 未知（不校验）。
    data_end_offset: u64,
    /// 归一化除数（2^(bps-1)）。
    scale: f32,
    /// 是否已到流尾（最后一帧已消费）。
    stream_done: bool,
}

impl FlacBackend {
    /// 打开并解析 FLAC 文件；结构性问题返回错误。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let mut file = File::open(path).map_err(DecodeError::from)?;
        let meta = parse_metadata(&mut file)?;
        let duration = if meta.info.total_samples > 0 {
            Some(meta.info.total_samples as f64 / f64::from(meta.info.sample_rate))
        } else {
            None
        };
        let params = AudioParams::new(
            Some(meta.info.sample_rate),
            Some(u16::from(meta.info.channels)),
            Some(u32::from(meta.info.bits_per_sample)),
            "FLAC".into(),
            0,
            duration,
        );
        let scale = (1u64 << (meta.info.bits_per_sample - 1)) as f32;
        let data_end = file.seek(std::io::SeekFrom::End(0)).unwrap_or(0);
        // seek(End) 后文件指针在 EOF，必须归位到首帧偏移——BitReader 的
        // fill_byte 直接读文件，指针在 EOF 时首次读取返回 0 字节（全静音）。
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(meta.first_frame_offset))?;
        let reader = BitReader::new(file, meta.first_frame_offset);
        Ok(Self {
            params,
            reader,
            info: StreamInfo {
                sample_rate: meta.info.sample_rate,
                channels: meta.info.channels,
                bits_per_sample: meta.info.bits_per_sample,
                total_samples: meta.info.total_samples,
            },
            first_frame_offset: meta.first_frame_offset,
            seek_table: meta.seek_table,
            pending: std::collections::VecDeque::new(),
            data_end_offset: data_end,
            scale,
            stream_done: false,
        })
    }

    /// 解码一帧并入行缓冲；返回本帧样本数（0 = 流尾）。
    fn pump_frame(&mut self) -> Result<u32, DecodeError> {
        if self.stream_done {
            return Ok(0);
        }
        let mut channels: Vec<Vec<i64>> = Vec::new();
        match decode_frame(&mut self.reader, &self.info, &mut channels) {
            Ok(n) => {
                let ch = self.info.channels as usize;
                for i in 0..n as usize {
                    for c in 0..ch {
                        let v = channels
                            .get(c)
                            .and_then(|col| col.get(i))
                            .copied()
                            .unwrap_or(0);
                        self.pending.push_back(v as f32 / self.scale);
                    }
                }
                Ok(n)
            }
            Err(DecodeError::Decode(msg)) if msg.contains("截断") => {
                // 尾部截断宽容：已产出的部分有效，标记流尾。
                self.stream_done = true;
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

impl DecoderBackend for FlacBackend {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    /// 按块（≤4096 帧）产出交错 f32；内部按帧泵入，帧界与块界无关。
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        let ch = self.info.channels as usize;
        let want = CHUNK_FRAMES * ch;
        while self.pending.len() < want && !self.stream_done {
            if self.pump_frame()? == 0 {
                break;
            }
        }
        if self.pending.is_empty() {
            return Ok(None);
        }
        let take = self.pending.len().min(want);
        // 按整帧截取（通道对齐），残余留缓冲。
        let take = take - (take % ch);
        if take == 0 {
            return Ok(None);
        }
        let out: Vec<f32> = self.pending.drain(..take).collect();
        Ok(Some(out))
    }

    /// SEEKTABLE 定位 + 解码丢弃至精确帧；无表则从头解码丢弃（v1 口径）。
    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek(format!("非法 seek 秒数：{secs}")));
        }
        let sr = self.params.sample_rate.unwrap_or(0);
        if sr == 0 {
            return Err(DecodeError::Seek("采样率未知，无法按秒定位".into()));
        }
        let target = (secs * f64::from(sr)) as u64;
        // 选 seek 起点：SEEKTABLE 中不大于 target 的最后一点；
        // 无表或无匹配点 → 从头（偏移 = 首帧、样本 = 0）。
        let (start_sample, start_offset) = self
            .seek_table
            .as_ref()
            .and_then(|t| {
                t.iter()
                    .filter(|p| p.sample <= target)
                    .max_by_key(|p| p.sample)
                    .map(|p| (p.sample, p.offset))
            })
            .unwrap_or((0, 0));
        // 偏移范围校验：SEEKTABLE 声明的偏移 + 首帧基址不得越过数据末尾。
        // 越界意味着表与音频段不匹配（写入标签后偏移未更新等）——按保守
        // 口径回退到从头线性解码（旧缺陷：越界 seek 静默跳到文件末尾并
        // 返回 Ok，用户以为 seek 成功但实际跳到结尾停止播放）。
        let absolute_offset = self.first_frame_offset + start_offset;
        if start_offset > 0 && self.data_end_offset > 0 && absolute_offset >= self.data_end_offset {
            // 表不可信：退回从头线性解码（保守口径）。
            self.reader.reset_to(self.first_frame_offset)?;
            self.pending.clear();
            self.stream_done = false;
            let mut cursor = 0u64;
            while cursor < target {
                let mut ch: Vec<Vec<i64>> = Vec::new();
                match decode_frame(&mut self.reader, &self.info, &mut ch) {
                    Ok(n) => {
                        let n = n as u64;
                        if cursor + n > target {
                            let skip = (target - cursor) as usize;
                            let c_count = self.info.channels as usize;
                            for i in skip..n as usize {
                                for c in 0..c_count {
                                    let v =
                                        ch.get(c).and_then(|col| col.get(i)).copied().unwrap_or(0);
                                    self.pending.push_back(v as f32 / self.scale);
                                }
                            }
                            cursor = target;
                        } else {
                            cursor += n;
                        }
                    }
                    Err(_) => break,
                }
            }
            return Ok(secs);
        }
        self.reader.reset_to(absolute_offset)?;
        self.pending.clear();
        self.stream_done = false;

        // 解码丢弃：从 start_sample 起逐帧推进到 target。
        let mut cursor = start_sample;
        while cursor < target {
            let mut channels: Vec<Vec<i64>> = Vec::new();
            match decode_frame(&mut self.reader, &self.info, &mut channels) {
                Ok(n) => {
                    let n = n as u64;
                    if cursor + n > target {
                        // 目标落在本帧内：只丢弃前段，余下入行缓冲。
                        let skip = (target - cursor) as usize;
                        let ch = self.info.channels as usize;
                        for i in skip..n as usize {
                            for c in 0..ch {
                                let v = channels
                                    .get(c)
                                    .and_then(|col| col.get(i))
                                    .copied()
                                    .unwrap_or(0);
                                self.pending.push_back(v as f32 / self.scale);
                            }
                        }
                        cursor = target;
                    } else {
                        cursor += n;
                    }
                }
                Err(DecodeError::Decode(msg)) if msg.contains("截断") => {
                    self.stream_done = true;
                    break;
                }
                Err(e) => return Err(e),
            }
            if self.stream_done {
                break;
            }
        }
        Ok(secs)
    }
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    // -----------------------------------------------------------------
    // 测试侧最小 FLAC 编码器（合成合法流：喂自己的解码器，也喂 symphonia
    // 做对拍——编码器本身同步经受交叉验证）。
    // -----------------------------------------------------------------

    /// MSB 在前的位写出器（测试够用即可，不追求效率）。
    #[derive(Default)]
    struct BitWriter {
        out: Vec<u8>,
        cur: u32,
        nbits: u32,
    }

    impl BitWriter {
        fn write_bits(&mut self, v: u64, n: u32) {
            for i in (0..n).rev() {
                let bit = ((v >> i) & 1) as u32;
                self.cur = (self.cur << 1) | bit;
                self.nbits += 1;
                if self.nbits == 8 {
                    self.out.push(self.cur as u8);
                    self.cur = 0;
                    self.nbits = 0;
                }
            }
        }
        fn align(&mut self) {
            if self.nbits > 0 {
                self.cur <<= 8 - self.nbits;
                self.out.push(self.cur as u8);
                self.cur = 0;
                self.nbits = 0;
            }
        }
    }

    fn write_signed(w: &mut BitWriter, v: i64, n: u32) {
        let mask = if n == 64 { u64::MAX } else { (1u64 << n) - 1 };
        w.write_bits((v as u64) & mask, n);
    }

    fn crc8_of(bytes: &[u8]) -> u8 {
        bytes.iter().fold(0u8, |c, &b| crc8_update(c, b))
    }
    fn crc16_of(bytes: &[u8]) -> u16 {
        bytes.iter().fold(0u16, |c, &b| crc16_update(c, b))
    }

    /// 子帧编码形态（覆盖 CONSTANT / VERBATIM / FIXED；LPC 由真实语料对拍覆盖）。
    enum Sub {
        Constant(i64),
        Verbatim(Vec<i64>),
        Fixed {
            samples: Vec<i64>,
            order: usize,
            k: u32,
            rice2: bool,
        },
    }

    fn fixed_residuals(samples: &[i64], order: usize) -> Vec<i64> {
        (order..samples.len())
            .map(|n| {
                let pred = match order {
                    0 => 0,
                    1 => samples[n - 1],
                    2 => 2 * samples[n - 1] - samples[n - 2],
                    3 => 3 * samples[n - 1] - 3 * samples[n - 2] + samples[n - 3],
                    _ => {
                        4 * samples[n - 1] - 6 * samples[n - 2] + 4 * samples[n - 3]
                            - samples[n - 4]
                    }
                };
                samples[n] - pred
            })
            .collect()
    }

    fn encode_subframe(w: &mut BitWriter, sub: &Sub, bps: u8) {
        match sub {
            Sub::Constant(v) => {
                w.write_bits(0, 1);
                w.write_bits(0b000000, 6);
                w.write_bits(0, 1);
                write_signed(w, *v, bps as u32);
            }
            Sub::Verbatim(samples) => {
                w.write_bits(0, 1);
                w.write_bits(0b000001, 6);
                w.write_bits(0, 1);
                for &s in samples {
                    write_signed(w, s, bps as u32);
                }
            }
            Sub::Fixed {
                samples,
                order,
                k,
                rice2,
            } => {
                w.write_bits(0, 1);
                w.write_bits(0b001000 | (*order as u64), 6);
                w.write_bits(0, 1);
                for &s in &samples[..*order] {
                    write_signed(w, s, bps as u32);
                }
                let residuals = fixed_residuals(samples, *order);
                w.write_bits(u64::from(*rice2), 2);
                w.write_bits(0, 4); // 分区阶 0（单分区）
                w.write_bits(u64::from(*k), if *rice2 { 5 } else { 4 });
                for r in residuals {
                    let folded = ((r << 1) ^ (r >> 63)) as u64; // 之字形
                    let q = folded >> k;
                    for _ in 0..q {
                        w.write_bits(0, 1);
                    }
                    w.write_bits(1, 1);
                    if *k > 0 {
                        w.write_bits(folded & ((1u64 << k) - 1), *k);
                    }
                }
            }
        }
    }

    /// 编码一整帧（块 ≤256，块尺寸走 8 位扩展码；帧号单字节）。
    fn encode_frame(
        frame_no: u32,
        block_size: u32,
        ch_code: u8,
        bps: u8,
        subs: Vec<Sub>,
    ) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.write_bits(0x3FFE, 14); // 同步码
        w.write_bits(0, 1); // reserved
        w.write_bits(0, 1); // blocking strategy（固定块）
        w.write_bits(0b0110, 4); // 块尺寸：8 位扩展
        w.write_bits(0, 4); // 采样率：用 STREAMINFO
        w.write_bits(u64::from(ch_code), 4);
        let bps_code = match bps {
            8 => 0b001,
            16 => 0b100,
            24 => 0b110,
            other => panic!("测试未覆盖位深 {other}"),
        };
        w.write_bits(bps_code, 3);
        w.write_bits(0, 1); // reserved
        w.write_bits(u64::from(frame_no), 8); // 帧号（小值单字节形态）
        w.write_bits(u64::from(block_size - 1), 8);
        w.align();
        let c8 = crc8_of(&w.out);
        w.write_bits(u64::from(c8), 8);
        for (i, sub) in subs.iter().enumerate() {
            // 侧声道位深 +1（左/侧、右/侧的侧道与中/侧的侧道）。
            let sub_bps = match (ch_code, i) {
                (8, 1) | (9, 0) | (10, 1) => bps + 1,
                _ => bps,
            };
            encode_subframe(&mut w, sub, sub_bps);
        }
        w.align();
        let c16 = crc16_of(&w.out);
        w.write_bits(u64::from(c16), 16);
        w.out
    }

    fn streaminfo(sr: u32, ch: u8, bps: u8, total: u64, block: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&block.to_be_bytes());
        b.extend_from_slice(&block.to_be_bytes());
        b.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // 最小/最大帧尺寸（不填）
        let packed: u64 = (u64::from(sr) << 44)
            | ((u64::from(ch) - 1) << 41)
            | ((u64::from(bps) - 1) << 36)
            | (total & 0xF_FFFF_FFFF);
        b.extend_from_slice(&packed.to_be_bytes());
        b.extend_from_slice(&[0u8; 16]); // MD5（不填）
        b
    }

    fn meta_block(t: u8, last: bool, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![
            (if last { 0x80 } else { 0 }) | t,
            ((payload.len() >> 16) & 0xFF) as u8,
            ((payload.len() >> 8) & 0xFF) as u8,
            (payload.len() & 0xFF) as u8,
        ];
        v.extend_from_slice(payload);
        v
    }

    /// 组装完整 FLAC 流（STREAMINFO + 可选 SEEKTABLE + 帧序列）。
    fn make_flac(
        sr: u32,
        ch: u8,
        bps: u8,
        total: u64,
        block: u16,
        frames: &[Vec<u8>],
        seek_points: Option<Vec<(u64, u64)>>,
    ) -> Vec<u8> {
        let mut v = b"fLaC".to_vec();
        v.extend(meta_block(
            0,
            seek_points.is_none(),
            &streaminfo(sr, ch, bps, total, block),
        ));
        if let Some(points) = seek_points {
            let mut payload = Vec::new();
            for (s, o) in points {
                payload.extend_from_slice(&s.to_be_bytes());
                payload.extend_from_slice(&o.to_be_bytes());
                payload.extend_from_slice(&block.to_be_bytes());
            }
            v.extend(meta_block(3, true, &payload));
        }
        for f in frames {
            v.extend_from_slice(f);
        }
        v
    }

    fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "tuneux-flac-test-{}-{seq}.flac",
            std::process::id()
        ));
        let mut f = File::create(&path).expect("应能创建临时文件");
        f.write_all(bytes).expect("应能写入临时文件");
        path
    }

    fn decode_all(b: &mut FlacBackend) -> Vec<f32> {
        let mut out = Vec::new();
        while let Ok(Some(chunk)) = b.decode_next() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// wasted bits（dropped bits）必须左移还原。
    ///
    /// FLAC 规定：子帧头置 wasted 标志时，样本最低 k 位在编码时被截掉，
    /// 解码器解出子帧后要把每个样本左移 k 位。旧实现只把有效位深从 bps 降到
    /// bps-k、**没有左移**，于是 16bit 母带封装进 24bit 容器（k=8）之类的合法
    /// 文件整体衰减 2^k 倍（−48 dB）；帧 CRC-16 覆盖的是原始帧字节而非样本值，
    /// 现有校验发现不了——常规测试又因真实语料标注 `#[ignore]` 而覆盖不到。
    ///
    /// 手工构造 CONSTANT 子帧（bps=16、k=8、值 0x7F）：
    ///   padding(1)=0 | type(6)=000000 | wasted 标志(1)=1
    ///   | 一元计数(7 个 0 后一个 1 → wasted=8) | 有效位深样本(8)=0b01111111
    /// 期望 = 0x7F << 8。左移位于 match 之后，四条子帧分支共用同一段代码，
    /// 故 CONSTANT 一条即可守护。
    #[test]
    fn subframe_restores_wasted_bits() {
        let path = write_temp(&[0x01, 0x01, 0x7F]);
        let file = File::open(&path).expect("应能打开临时文件");
        let mut r = BitReader::new(file, 0);
        let mut out: Vec<i64> = Vec::new();
        decode_subframe(&mut r, 1, 16, &mut out).expect("子帧应解码成功");
        assert_eq!(out, vec![0x7Fi64 << 8], "wasted=8 必须左移 8 位还原");
        let _ = std::fs::remove_file(&path);
    }

    /// 对照：wasted=0 时不得左移（防止"一律左移"式的过度修复）。
    #[test]
    fn subframe_without_wasted_bits_is_unchanged() {
        // padding(1)=0 | type(6)=000000 | wasted 标志(1)=0 | 样本(16)=0x00FF
        let path = write_temp(&[0x00, 0x00, 0xFF]);
        let file = File::open(&path).expect("应能打开临时文件");
        let mut r = BitReader::new(file, 0);
        let mut out: Vec<i64> = Vec::new();
        decode_subframe(&mut r, 1, 16, &mut out).expect("子帧应解码成功");
        assert_eq!(out, vec![0x00FFi64], "无 wasted 标志时样本原样输出");
        let _ = std::fs::remove_file(&path);
    }

    fn expect_samples(out: &[f32], raw: &[i64], bps: u8) {
        let scale = (1u64 << (bps - 1)) as f32;
        assert_eq!(out.len(), raw.len(), "样本数不符");
        for (i, (o, r)) in out.iter().zip(raw.iter()).enumerate() {
            assert_eq!(*o, *r as f32 / scale, "第 {i} 个样本不符");
        }
    }

    /// VERBATIM 立体声 16 位：参数 / 样本值 / EOF。
    #[test]
    fn verbatim_stereo_roundtrip() {
        let l: Vec<i64> = vec![0, 1000, -1000, 32767, -32768, 12345, -23456, 777];
        let r: Vec<i64> = vec![-5, 6, -7, 8, -9, 10, -11, 12];
        let frame = encode_frame(
            0,
            8,
            1,
            16,
            vec![Sub::Verbatim(l.clone()), Sub::Verbatim(r.clone())],
        );
        let total = (l.len() * 2) as u64; // 两声道共 8 帧
        let bytes = make_flac(44100, 2, 16, 8, 8, &[frame], None);
        let _ = total;
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        assert_eq!(b.params().sample_rate, Some(44100));
        assert_eq!(b.params().channels, Some(2));
        assert_eq!(b.params().bits_per_sample, Some(16));
        assert_eq!(b.params().codec_name, "FLAC");
        let d = b.params().duration.expect("应有时长");
        assert!((d - 8.0 / 44100.0).abs() < 1e-9, "duration 实际 {d}");
        let out = decode_all(&mut b);
        let interleaved: Vec<i64> = l.iter().zip(r.iter()).flat_map(|(a, b)| [*a, *b]).collect();
        expect_samples(&out, &interleaved, 16);
        let _ = std::fs::remove_file(&path);
    }

    /// CONSTANT 单声道 + EOF。
    #[test]
    fn constant_mono() {
        let frame = encode_frame(0, 16, 0, 16, vec![Sub::Constant(-1000)]);
        let bytes = make_flac(8000, 1, 16, 16, 16, &[frame], None);
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        expect_samples(&out, &[-1000; 16], 16);
        let _ = std::fs::remove_file(&path);
    }

    /// FIXED 各阶 + Rice / Rice2（含 0 阶无暖机路径）。
    #[test]
    fn fixed_orders_with_rice() {
        // 平滑序列：各阶残差都很小。
        let s: Vec<i64> = (0..32).map(|i| i * i / 4 + 100).collect();
        for (order, k, rice2) in [
            (0usize, 4u32, false),
            (1, 2, false),
            (2, 2, false),
            (4, 3, true),
        ] {
            let frame = encode_frame(
                0,
                32,
                0,
                16,
                vec![Sub::Fixed {
                    samples: s.clone(),
                    order,
                    k,
                    rice2,
                }],
            );
            let bytes = make_flac(44100, 1, 16, 32, 32, &[frame], None);
            let path = write_temp(&bytes);
            let mut b = FlacBackend::open(&path).expect("应能打开");
            let out = decode_all(&mut b);
            expect_samples(&out, &s, 16);
            let _ = std::fs::remove_file(&path);
        }
    }

    /// 24 位 VERBATIM：归一化除数 2^23。
    #[test]
    fn verbatim_24bit() {
        let s: Vec<i64> = vec![0, 0x7FFFFF, -8388608, 0x123456];
        let frame = encode_frame(0, 4, 0, 24, vec![Sub::Verbatim(s.clone())]);
        let bytes = make_flac(48000, 1, 24, 4, 4, &[frame], None);
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        assert_eq!(b.params().bits_per_sample, Some(24));
        let out = decode_all(&mut b);
        expect_samples(&out, &s, 24);
        let _ = std::fs::remove_file(&path);
    }

    /// 左/侧立体声去相关：right = left - side。
    #[test]
    fn left_side_decorrelation() {
        let l: Vec<i64> = vec![3000, -2000, 100, 5000, -8000, 12345, -32768, 32767];
        let r: Vec<i64> = vec![1000, 2500, -400, 3000, -7000, 11111, -30000, 30000];
        let side: Vec<i64> = l.iter().zip(r.iter()).map(|(a, b)| a - b).collect();
        let frame = encode_frame(
            0,
            8,
            8,
            16,
            vec![Sub::Verbatim(l.clone()), Sub::Verbatim(side)],
        );
        let bytes = make_flac(44100, 2, 16, 8, 8, &[frame], None);
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        let interleaved: Vec<i64> = l.iter().zip(r.iter()).flat_map(|(a, b)| [*a, *b]).collect();
        expect_samples(&out, &interleaved, 16);
        let _ = std::fs::remove_file(&path);
    }

    /// 中/侧立体声去相关（偶数奇偶样本，往返精确）。
    #[test]
    fn mid_side_decorrelation() {
        let l: Vec<i64> = vec![4000, -2000, 600, 8000, -9000, 12000, -30000, 32000];
        let r: Vec<i64> = vec![2000, 4000, -800, 4000, -5000, 10000, -28000, 30000];
        let mid: Vec<i64> = l.iter().zip(r.iter()).map(|(a, b)| (a + b) >> 1).collect();
        let side: Vec<i64> = l.iter().zip(r.iter()).map(|(a, b)| a - b).collect();
        let frame = encode_frame(0, 8, 10, 16, vec![Sub::Verbatim(mid), Sub::Verbatim(side)]);
        let bytes = make_flac(44100, 2, 16, 8, 8, &[frame], None);
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        let interleaved: Vec<i64> = l.iter().zip(r.iter()).flat_map(|(a, b)| [*a, *b]).collect();
        expect_samples(&out, &interleaved, 16);
        let _ = std::fs::remove_file(&path);
    }

    /// 与 symphonia 对拍：合成 FIXED 流全量位级一致（交叉验证编码器与解码器）。
    #[test]
    fn bit_exact_against_symphonia() {
        let s: Vec<i64> = (0..64).map(|i| ((i * 37) % 199) - 99).collect();
        let frame = encode_frame(
            0,
            64,
            1,
            16,
            vec![
                Sub::Fixed {
                    samples: s.clone(),
                    order: 2,
                    k: 4,
                    rice2: false,
                },
                Sub::Verbatim(s.clone()),
            ],
        );
        let bytes = make_flac(44100, 2, 16, 64, 64, &[frame], None);
        let path = write_temp(&bytes);
        let mut mine = FlacBackend::open(&path).expect("自研应打开");
        let mut theirs =
            super::super::decoder::SymphoniaBackend::open(&path).expect("symphonia 应打开");
        let mine_out = decode_all(&mut mine);
        let mut theirs_out = Vec::new();
        while let Ok(Some(chunk)) = theirs.decode_next() {
            theirs_out.extend_from_slice(&chunk);
        }
        assert_eq!(mine_out.len(), theirs_out.len(), "样本总数不一致");
        for (i, (a, b)) in mine_out.iter().zip(theirs_out.iter()).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "第 {i} 个样本不一致：{a} vs {b}");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// SEEKTABLE seek：定位到 seek 点后精确到样本。
    #[test]
    fn seek_with_seektable() {
        // 4 帧 × 16 样本单声道：帧 i 内容恒为 i*100。
        let frames: Vec<Vec<u8>> = (0..4)
            .map(|i| encode_frame(i, 16, 0, 16, vec![Sub::Constant(i as i64 * 100)]))
            .collect();
        // 帧偏移 = 头部（fLaC + STREAMINFO 块 + SEEKTABLE 块）之后的累计长度。
        let header_len = 4 + 4 + 34 + 4 + 2 * 18;
        let off2 = (frames[0].len() + frames[1].len()) as u64;
        let bytes = make_flac(64, 1, 16, 64, 16, &frames, Some(vec![(0, 0), (32, off2)]));
        let _ = header_len;
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        // seek 0.5s（采样率 64 → 目标样本 32）→ 帧 2 起点；其后帧 3 继续播。
        b.seek(0.5).expect("seek 应成功");
        let out = decode_all(&mut b);
        let expected: Vec<i64> = [200; 16].into_iter().chain([300; 16]).collect();
        expect_samples(&out, &expected, 16);
        // 非法输入。
        assert!(matches!(b.seek(-1.0), Err(DecodeError::Seek(_))));
        assert!(matches!(b.seek(f64::NAN), Err(DecodeError::Seek(_))));
        let _ = std::fs::remove_file(&path);
    }

    /// 无 SEEKTABLE seek：从头解码丢弃，落帧内精确位置。
    #[test]
    fn seek_without_seektable_decodes_forward() {
        // 2 帧：帧 0 恒 0，帧 1 为 0..16 递増（落帧内样本 8 = 800）。
        let ramp: Vec<i64> = (0..16).map(|i| i * 100).collect();
        let frames = vec![
            encode_frame(0, 16, 0, 16, vec![Sub::Constant(0)]),
            encode_frame(1, 16, 0, 16, vec![Sub::Verbatim(ramp.clone())]),
        ];
        let bytes = make_flac(32, 1, 16, 32, 16, &frames, None);
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        // seek 0.75s（采样率 32 → 目标样本 24 = 帧 1 的第 8 个）。
        b.seek(0.75).expect("seek 应成功");
        let out = decode_all(&mut b);
        expect_samples(&out, &ramp[8..], 16);
        let _ = std::fs::remove_file(&path);
    }

    /// CRC-16 损坏：帧内翻一字节 → Decode 错误。
    #[test]
    fn crc16_corruption_detected() {
        let frame = encode_frame(0, 16, 0, 16, vec![Sub::Constant(7)]);
        let mut bytes = make_flac(8000, 1, 16, 16, 16, std::slice::from_ref(&frame), None);
        // 翻转帧数据区一字节（帧在 4 + 38 头之后）。
        let idx = 4 + 38 + 5;
        bytes[idx] ^= 0xFF;
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        let mut err = None;
        loop {
            match b.decode_next() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let msg = format!("{err:?}");
        assert!(
            err.is_some() && msg.contains("CRC"),
            "应报 CRC 错误，实际 {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 帧头 CRC-8 损坏：同步区翻一字节 → Decode 错误。
    #[test]
    fn crc8_corruption_detected() {
        let frame = encode_frame(0, 16, 0, 16, vec![Sub::Constant(7)]);
        let mut bytes = make_flac(8000, 1, 16, 16, 16, &[frame], None);
        let idx = 4 + 38 + 2; // 帧头区
        bytes[idx] ^= 0x55;
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        let mut err = None;
        loop {
            match b.decode_next() {
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        let msg = format!("{err:?}");
        assert!(
            err.is_some() && msg.contains("CRC"),
            "应报 CRC 错误，实际 {msg}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 尾部截断：已有产出有效，随后 EOF，不报错。
    #[test]
    fn truncated_tail_is_tolerant() {
        let frames = vec![
            encode_frame(0, 16, 0, 16, vec![Sub::Constant(11)]),
            encode_frame(1, 16, 0, 16, vec![Sub::Constant(22)]),
        ];
        let mut bytes = make_flac(8000, 1, 16, 32, 16, &frames, None);
        bytes.truncate(bytes.len() - 4); // 砍第二帧尾部
        let path = write_temp(&bytes);
        let mut b = FlacBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        expect_samples(&out, &[11; 16], 16); // 只剩第一帧
        let _ = std::fs::remove_file(&path);
    }

    /// 非 FLAC → Unsupported（供 open_backend 回退）。
    #[test]
    fn not_flac_is_unsupported() {
        // 用非 .flac 扩展名隔离扩展名判定（魔数判定走 "NOPE" ≠ fLaC）。
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("tuneux-flac-test-{}-{seq}.bin", std::process::id()));
        let mut f = File::create(&path).expect("建");
        f.write_all(b"NOPE________").expect("写");
        drop(f);
        assert!(matches!(
            FlacBackend::open(&path),
            Err(DecodeError::Unsupported(_))
        ));
        assert_eq!(probe_sample_rate(&path), None);
        assert!(!is_flac_path(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// 直通探测与路径判定（含大写扩展名与魔数兜底）。
    #[test]
    fn probe_and_path_detection() {
        let frame = encode_frame(0, 8, 0, 16, vec![Sub::Constant(1)]);
        let bytes = make_flac(48000, 1, 16, 8, 8, &[frame], None);
        let path = write_temp(&bytes);
        assert_eq!(probe_sample_rate(&path), Some(48000));
        assert!(is_flac_path(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// 真实语料对拍：测试音频/山丘/01 山丘_EM.flac（真实编码器产出，
    /// 覆盖 LPC 路径），自研与 symphonia 全量逐样本位级一致。
    #[test]
    #[ignore = "需要 测试音频 夹具；运行：cargo test -- --ignored"]
    fn bit_exact_real_world_flac() {
        let candidates = [
            "测试音频/山丘/01 山丘_EM.flac",
            "../../测试音频/山丘/01 山丘_EM.flac",
        ];
        let path = candidates
            .iter()
            .map(std::path::Path::new)
            .find(|p| p.exists())
            .expect("缺少测试夹具 山丘 flac");
        assert!(is_flac_path(path), "夹具应被识别为 FLAC");
        assert!(probe_sample_rate(path).is_some(), "夹具应能读出采样率");
        let mut mine = FlacBackend::open(path).expect("自研应打开");
        let mut theirs =
            super::super::decoder::SymphoniaBackend::open(path).expect("symphonia 应打开");
        let mine_out = decode_all(&mut mine);
        let mut theirs_out = Vec::new();
        while let Ok(Some(chunk)) = theirs.decode_next() {
            theirs_out.extend_from_slice(&chunk);
        }
        assert!(
            !mine_out.is_empty() && mine_out.len() == theirs_out.len(),
            "样本总数不一致：自研 {} vs symphonia {}",
            mine_out.len(),
            theirs_out.len()
        );
        let mut diff = 0usize;
        for (a, b) in mine_out.iter().zip(theirs_out.iter()) {
            if a.to_bits() != b.to_bits() {
                diff += 1;
            }
        }
        assert_eq!(diff, 0, "不一致样本数 {diff} / {}", mine_out.len());
    }

    /// 真实语料对拍②：李宗盛专辑轨（另一编码器产出的 LPC 文件）。
    #[test]
    #[ignore = "需要 测试音频 夹具；运行：cargo test -- --ignored"]
    fn bit_exact_real_world_flac_album() {
        let candidates = [
            "测试音频/生命中的精灵/生命中的精灵 - 李宗盛.flac",
            "../../测试音频/生命中的精灵/生命中的精灵 - 李宗盛.flac",
        ];
        let path = candidates
            .iter()
            .map(std::path::Path::new)
            .find(|p| p.exists())
            .expect("缺少测试夹具 李宗盛 flac");
        let mut mine = FlacBackend::open(path).expect("自研应打开");
        let mut theirs =
            super::super::decoder::SymphoniaBackend::open(path).expect("symphonia 应打开");
        let mine_out = decode_all(&mut mine);
        let mut theirs_out = Vec::new();
        while let Ok(Some(chunk)) = theirs.decode_next() {
            theirs_out.extend_from_slice(&chunk);
        }
        assert!(
            !mine_out.is_empty() && mine_out.len() == theirs_out.len(),
            "样本总数不一致：自研 {} vs symphonia {}",
            mine_out.len(),
            theirs_out.len()
        );
        let mut diff = 0usize;
        for (a, b) in mine_out.iter().zip(theirs_out.iter()) {
            if a.to_bits() != b.to_bits() {
                diff += 1;
            }
        }
        assert_eq!(diff, 0, "不一致样本数 {diff} / {}", mine_out.len());
    }
}
