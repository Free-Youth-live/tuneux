//! # WavPack 自研解码后端（`.wv` 无损流）
//!
//! 自研 WavPack 解码后端：`.wv` 解码由第三方 `wavicle` crate
//! 换为本模块手写实现——wavicle 降为 dev-dependencies，仅作测试对拍
//! 参照物（生产依赖面再减一员）。
//!
//! ## 支持面（与换下的 wavicle 完全对齐，零退化）
//!
//! - 无损 mono / stereo，16/24/32 位整数与 bit-exact 32 位浮点（IEEE 位型
//!   还原，NaN / ±0 / 次正规数全保留）；多块文件顺序拼接；
//! - 去相关全 term（-3..-1 / 1..=8 / 17 / 18）+ 权重自适应、joint stereo
//!   中/侧还原、false stereo 展开、INT32_INFO / FLOAT_INFO 与 wvx 附加流；
//! - 块 CRC 与幅度检查（损坏即 Decode 错误，与 wavicle 同为硬错误口径）；
//! - 范围拒绝（与 wavicle 同口径映射为"不支持"）：DSD、hybrid/lossy、
//!   超过 2 声道、8 位整数、版本超出 0x402..=0x410、ID3 前缀。
//!
//! ## 结构与内存
//!
//! 沿用原后端的「打开即整段解码」形态（decode_next 按块切片、seek 为内存
//! 游标 O(1)），行为对外无变化；块（block）是 WavPack 的自封装压缩单元，
//! 每块自带全部解码参数（去相关 / 熵编码状态），块间零状态延续。
//!
//! 格式细节按 wavicle 0.1.0 源码逐位实现：位流字节内 LSB 优先；
//! `read_code` 截断二进制内先消费位为高位；块 CRC 在 joint 还原之后、
//! float/int32/shift fixup 之前计算。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// 每次 `decode_next` 输出的帧数（与原后端同口径）。
const CHUNK_FRAMES: usize = 4096;

/// 头部固定长度（字节）。
const HEADER_LEN: usize = 32;
/// 块总尺寸上限（1 MB）。
const MAX_BLOCK_SIZE: usize = 1 << 20;
/// 单块帧数上限。
const MAX_BLOCK_SAMPLES: u32 = 131072;
/// 去相关 pass 数上限。
const MAX_NTERMS: usize = 16;

/// 标准采样率表（flags 的 SRATE 索引 0..=14；15 = 非标准，真值在子块）。
const SAMPLE_RATES: [u32; 15] = [
    6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000,
    192000,
];

/// wp_exp2s 的 8 位定点尾数表（log2 → 线性，见函数注释）。
#[rustfmt::skip]
const EXP2_TABLE: [u8; 256] = [
    0x00, 0x01, 0x01, 0x02, 0x03, 0x03, 0x04, 0x05, 0x06, 0x06, 0x07, 0x08, 0x08, 0x09, 0x0a, 0x0b,
    0x0b, 0x0c, 0x0d, 0x0e, 0x0e, 0x0f, 0x10, 0x10, 0x11, 0x12, 0x13, 0x13, 0x14, 0x15, 0x16, 0x16,
    0x17, 0x18, 0x19, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1d, 0x1e, 0x1f, 0x20, 0x20, 0x21, 0x22, 0x23,
    0x24, 0x24, 0x25, 0x26, 0x27, 0x28, 0x28, 0x29, 0x2a, 0x2b, 0x2c, 0x2c, 0x2d, 0x2e, 0x2f, 0x30,
    0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3a, 0x3b, 0x3c, 0x3d,
    0x3e, 0x3f, 0x40, 0x41, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x48, 0x49, 0x4a, 0x4b,
    0x4c, 0x4d, 0x4e, 0x4f, 0x50, 0x51, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a,
    0x5b, 0x5c, 0x5d, 0x5e, 0x5e, 0x5f, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79,
    0x7a, 0x7b, 0x7c, 0x7d, 0x7e, 0x7f, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x87, 0x88, 0x89, 0x8a,
    0x8b, 0x8c, 0x8d, 0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
    0x9c, 0x9d, 0x9f, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad,
    0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xbc, 0xbd, 0xbe, 0xbf, 0xc0,
    0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc8, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf, 0xd0, 0xd2, 0xd3, 0xd4,
    0xd6, 0xd7, 0xd8, 0xd9, 0xdb, 0xdc, 0xdd, 0xde, 0xe0, 0xe1, 0xe2, 0xe4, 0xe5, 0xe6, 0xe8, 0xe9,
    0xea, 0xec, 0xed, 0xee, 0xf0, 0xf1, 0xf2, 0xf4, 0xf5, 0xf6, 0xf8, 0xf9, 0xfa, 0xfc, 0xfd, 0xff,
];

/// log2（定点 8.8）→ 线性值。WavPack 的 median / 历史值统一经此展开。
/// ENTROPY_VARS 的 log 是 u16 零扩展（非负）；DECORR_SAMPLES 的 log 是
/// i16 符号扩展（可负）——两侧不对称是格式原样，必须照做。
fn wp_exp2s(log: i32) -> i32 {
    if log < 0 {
        return -wp_exp2s(-log);
    }
    let value = u32::from(EXP2_TABLE[(log & 0xFF) as usize]) | 0x100;
    let e = log >> 8;
    if e <= 9 {
        (value >> (9 - e)) as i32
    } else {
        (value.wrapping_shl(((e - 9) & 0x1F) as u32)) as i32
    }
}

// =============================================================================
// 块头与子块解析
// =============================================================================

/// 块头解析结果（字段含义见各字段注释）。
#[derive(Debug, Clone, Copy)]
struct BlockHeader {
    /// 磁盘块长（= ck_size + 8）。
    block_len: usize,
    block_samples: u32,
    flags: u32,
    crc: u32,
}

impl BlockHeader {
    fn bytes_per_sample(&self) -> u32 {
        (self.flags & 3) + 1
    }
    fn mono_stored(&self) -> bool {
        self.flags & (1 << 2) != 0
    }
    fn joint_stereo(&self) -> bool {
        self.flags & (1 << 4) != 0
    }
    fn float_data(&self) -> bool {
        self.flags & (1 << 7) != 0
    }
    fn int32_data(&self) -> bool {
        self.flags & (1 << 8) != 0
    }
    fn output_shift(&self) -> u32 {
        (self.flags >> 13) & 0x1F
    }
    fn magnitude(&self) -> u32 {
        (self.flags >> 18) & 0x1F
    }
    fn srate_index(&self) -> u32 {
        (self.flags >> 23) & 0xF
    }
    fn false_stereo(&self) -> bool {
        self.flags & (1 << 30) != 0
    }
    /// 解码按单声道处理（MONO 或 FALSE_STEREO）。
    fn mono_data(&self) -> bool {
        self.mono_stored() || self.false_stereo()
    }
    /// 输出声道数。
    fn channels(&self) -> u16 {
        if self.mono_data() && self.false_stereo() {
            2
        } else if self.mono_stored() {
            1
        } else {
            2
        }
    }
}

/// flags 中 hybrid 族的位（3/6/9/10/29），任一置位即超范围。
const ANY_HYBRID: u32 = (1 << 3) | (1 << 6) | (1 << 9) | (1 << 10) | (1 << 29);

/// 解析 32 字节块头；结构与范围问题直接分类返回。
fn parse_block_header(buf: &[u8]) -> Result<BlockHeader, DecodeError> {
    if buf.len() < HEADER_LEN {
        return Err(DecodeError::Decode(
            "WavPack 块头不足 32 字节（截断）".into(),
        ));
    }
    if &buf[0..4] != b"wvpk" {
        return Err(DecodeError::Decode(
            "不是 WavPack 流（坏魔数或带 ID3 前缀）".into(),
        ));
    }
    let ck_size = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
    let block_len = ck_size + 8;
    if block_len > MAX_BLOCK_SIZE || ck_size < 24 {
        return Err(DecodeError::Decode("WavPack 块尺寸非法".into()));
    }
    let version = u16::from_le_bytes([buf[8], buf[9]]);
    if !(0x402..=0x410).contains(&version) {
        return Err(DecodeError::Unsupported(format!(
            "WavPack 版本 {version:#x} 超出支持范围（0x402..=0x410）"
        )));
    }
    let block_samples = u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
    if block_samples > MAX_BLOCK_SAMPLES {
        return Err(DecodeError::Decode("WavPack 单块帧数超限".into()));
    }
    let flags = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    if flags & (1 << 31) != 0 {
        return Err(DecodeError::Unsupported("WavPack DSD 流不支持".into()));
    }
    if flags & ANY_HYBRID != 0 {
        return Err(DecodeError::Unsupported(
            "WavPack hybrid/lossy 模式不支持".into(),
        ));
    }
    let crc = u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]);
    let _ = version; // 版本仅作范围校验，不参与后续解码分支
    Ok(BlockHeader {
        block_len,
        block_samples,
        flags,
        crc,
    })
}

/// 元数据子块（id 保留 0x20 可选位；data 已去奇填充）。
struct SubBlock<'a> {
    id: u8,
    data: &'a [u8],
}

/// 迭代元数据区的子块（尺寸单位是 16 位字；奇尺寸去尾字节；
/// LARGE 用 3 字节尺寸）。区域耗尽即停，越界即解码错误。
struct SubBlocks<'a> {
    meta: &'a [u8],
    pos: usize,
}

impl<'a> SubBlocks<'a> {
    fn new(meta: &'a [u8]) -> Self {
        Self { meta, pos: 0 }
    }
}

impl<'a> Iterator for SubBlocks<'a> {
    type Item = Result<SubBlock<'a>, DecodeError>;
    fn next(&mut self) -> Option<Self::Item> {
        let meta = self.meta;
        if self.pos + 2 > meta.len() {
            return None;
        }
        let id_byte = meta[self.pos];
        let large = id_byte & 0x80 != 0;
        let odd = id_byte & 0x40 != 0;
        let id = id_byte & 0x3F;
        let (words, hdr) = if large {
            if self.pos + 4 > meta.len() {
                return Some(Err(DecodeError::Decode("WavPack 子块头越界".into())));
            }
            (
                (u32::from(meta[self.pos + 1])
                    | (u32::from(meta[self.pos + 2]) << 8)
                    | (u32::from(meta[self.pos + 3]) << 16)) as usize,
                4,
            )
        } else {
            (usize::from(meta[self.pos + 1]), 2)
        };
        let payload = words * 2;
        if odd && payload == 0 {
            return Some(Err(DecodeError::Decode("WavPack 奇尺寸子块零长度".into())));
        }
        let start = self.pos + hdr;
        if start + payload > meta.len() {
            return Some(Err(DecodeError::Decode("WavPack 子块载荷越界".into())));
        }
        let real = if odd { payload - 1 } else { payload };
        self.pos = start + payload;
        Some(Ok(SubBlock {
            id,
            data: &meta[start..start + real],
        }))
    }
}

// =============================================================================
// 位流读取（字节内 LSB 优先；越界 sticky 喂 0，事后由块校验兜底）
// =============================================================================

/// WavPack 位流读器：`sr` 存 `bc` 个未消费位，消费从 bit 0（最先到的位）进行。
struct BsReader<'a> {
    buf: &'a [u8],
    next: usize,
    sr: u32,
    bc: u32,
    error: bool,
}

impl<'a> BsReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            next: 0,
            sr: 0,
            bc: 0,
            error: false,
        }
    }

    fn load_byte(&mut self) -> u32 {
        if self.next < self.buf.len() {
            let b = self.buf[self.next];
            self.next += 1;
            u32::from(b)
        } else {
            self.error = true;
            0
        }
    }

    /// 读 1 位。
    fn getbit(&mut self) -> u32 {
        if self.bc > 0 {
            self.bc -= 1;
        } else {
            self.sr = self.load_byte();
            self.bc = 7;
        }
        let bit = self.sr & 1;
        self.sr >>= 1;
        bit
    }

    /// 读 n 位（先到的位 = 值的 bit 0）。
    fn getbits(&mut self, n: u32) -> u32 {
        let mut local = u64::from(self.sr);
        while self.bc < n {
            local |= u64::from(self.load_byte()) << self.bc;
            self.bc += 8;
        }
        let value = (local & ((1u64 << n) - 1)) as u32;
        self.bc -= n;
        self.sr = (local >> n) as u32;
        value
    }

    /// 截断二进制码（truncated binary）：一个 code 内先消费的位是高位。
    fn read_code(&mut self, maxcode: u32) -> u32 {
        if maxcode < 2 {
            return if maxcode == 1 { self.getbit() } else { 0 };
        }
        let bitcount = 32 - maxcode.leading_zeros();
        let extras = (1u32 << bitcount) - maxcode - 1;
        // 预取 bitcount 位（同 getbits 的补字节方式，但先不消费）。
        let mut local = u64::from(self.sr);
        while self.bc < bitcount {
            local |= u64::from(self.load_byte()) << self.bc;
            self.bc += 8;
        }
        let mut code = (local as u32) & ((1u32 << (bitcount - 1)) - 1);
        let used;
        if code >= extras {
            code = (code << 1) - extras + (((local >> (bitcount - 1)) as u32) & 1);
            used = bitcount;
        } else {
            used = bitcount - 1;
        }
        self.bc -= used;
        self.sr = (local >> used) as u32;
        code
    }

    /// 一元计数（n 个 1 后跟 0 表示 n；16 = 逃逸接 EGC；17 = 流结束 None）。
    fn read_ones_count(&mut self, limit: u32) -> Option<u32> {
        let mut n = 0u32;
        while n < limit + 1 && self.getbit() != 0 {
            n += 1;
        }
        if n == limit + 1 {
            return None;
        }
        if n == limit {
            let d = self.read_egc_count()?;
            n = d + limit;
        }
        Some(n)
    }

    /// EGC 计数（Elias-gamma 风格：cbits 位长，值含最高隐含 1）。
    fn read_egc_count(&mut self) -> Option<u32> {
        let mut cbits = 0u32;
        while cbits < 33 && self.getbit() != 0 {
            cbits += 1;
        }
        if cbits == 33 {
            return None;
        }
        if cbits < 2 {
            return Some(cbits);
        }
        let mut value = 0u32;
        let mut mask = 1u32;
        for _ in 0..cbits - 1 {
            if self.getbit() != 0 {
                value |= mask;
            }
            mask <<= 1;
        }
        Some(value | mask)
    }
}

// =============================================================================
// 熵解码（words coder：一元前缀 → 截断二进制低位 → 符号位）
// =============================================================================

/// 每声道 3 个自适应 median（存 ×16 定点）。
#[derive(Default, Clone, Copy)]
struct Medians {
    median: [u32; 3],
}

impl Medians {
    fn get(&self, i: usize) -> u32 {
        (self.median[i] >> 4) + 1
    }
    fn inc0(&mut self) {
        self.median[0] = self.median[0].wrapping_add(self.median[0].wrapping_add(128) / 128 * 5);
    }
    fn dec0(&mut self) {
        self.median[0] = self.median[0].wrapping_sub(self.median[0].wrapping_add(126) / 128 * 2);
    }
    fn inc1(&mut self) {
        self.median[1] = self.median[1].wrapping_add(self.median[1].wrapping_add(64) / 64 * 5);
    }
    fn dec1(&mut self) {
        self.median[1] = self.median[1].wrapping_sub(self.median[1].wrapping_add(62) / 64 * 2);
    }
    fn inc2(&mut self) {
        self.median[2] = self.median[2].wrapping_add(self.median[2].wrapping_add(32) / 32 * 5);
    }
    fn dec2(&mut self) {
        self.median[2] = self.median[2].wrapping_sub(self.median[2].wrapping_add(30) / 32 * 2);
    }
}

/// 熵解码器状态（holding 系列与零行程贯穿整个块）。
struct WordsDecoder {
    c: [Medians; 2],
    holding_one: u32,
    holding_zero: bool,
    zeros_acc: u32,
}

impl WordsDecoder {
    fn new(medians: [Medians; 2]) -> Self {
        Self {
            c: medians,
            holding_one: 0,
            holding_zero: false,
            zeros_acc: 0,
        }
    }

    /// 解码 nframes 帧残差写入 buffer（mono 每帧 1 个，stereo 交错 2 个）。
    /// 返回是否完整产出（流提前结束 / 位流越界 → false）。
    fn get_words_lossless(
        &mut self,
        r: &mut BsReader,
        nframes: u32,
        mono: bool,
        buffer: &mut Vec<i32>,
    ) -> bool {
        buffer.clear();
        let nsamples = if mono { nframes } else { 2 * nframes } as usize;
        buffer.reserve(nsamples);
        let mut csamples = 0usize;
        while csamples < nsamples {
            let mut ch = if mono { 0 } else { csamples & 1 };

            // A. 上一偶数前缀遗留的「无前缀 0 区间」样本。
            if self.holding_zero {
                self.holding_zero = false;
                let rice_k = self.c[ch].get(0);
                let low = r.read_code(rice_k - 1);
                self.c[ch].dec0();
                let sign = r.getbit();
                buffer.push(if sign == 1 { !(low as i32) } else { low as i32 });
                csamples += 1;
                if csamples == nsamples {
                    break;
                }
                if !mono {
                    ch = csamples & 1;
                }
            }

            // B. 零行程（判原始 median[0] < 2，且 holding_one == 0）。
            if self.c[0].median[0] < 2 && self.holding_one == 0 && self.c[1].median[0] < 2 {
                if self.zeros_acc > 0 {
                    self.zeros_acc -= 1;
                    if self.zeros_acc > 0 {
                        buffer.push(0);
                        csamples += 1;
                        continue;
                    }
                    // 减到 0 → 落回正常解码（行程后首个非零样本）。
                } else {
                    let Some(n) = r.read_egc_count() else {
                        break; // 流结束标志
                    };
                    self.zeros_acc = n;
                    if n > 0 {
                        self.c[0] = Medians::default();
                        self.c[1] = Medians::default();
                        buffer.push(0);
                        csamples += 1;
                        continue;
                    }
                }
            }

            // C. 正常样本：一元前缀。
            let Some(rr) = r.read_ones_count(16) else {
                break; // 流结束标志
            };
            let carry = self.holding_one;
            self.holding_one = rr & 1;
            self.holding_zero = (rr & 1) == 0;
            let ones_count = (rr >> 1) + carry;

            let cm = &mut self.c[ch];
            let mut low: u32;
            let high: u32;
            if ones_count == 0 {
                low = 0;
                high = cm.get(0) - 1;
                cm.dec0();
            } else {
                low = cm.get(0);
                cm.inc0();
                if ones_count == 1 {
                    high = low + cm.get(1) - 1;
                    cm.dec1();
                } else {
                    low += cm.get(1);
                    cm.inc1();
                    if ones_count == 2 {
                        high = low + cm.get(2) - 1;
                        cm.dec2();
                    } else {
                        low += (ones_count - 2) * cm.get(2);
                        high = low + cm.get(2) - 1;
                        cm.inc2();
                    }
                }
            }
            low = low.wrapping_add(r.read_code((high - low) & 0x7FFF_FFFF));
            let sign = r.getbit();
            buffer.push(if sign == 1 { !(low as i32) } else { low as i32 });
            csamples += 1;
        }
        csamples == nsamples && !r.error
    }
}

// =============================================================================
// 去相关引擎
// =============================================================================

/// 权重还原（元数据的 i8 → 内部权重）。
fn restore_weight(w: i8) -> i32 {
    let mut r = (w as i32) * 8;
    if r > 0 {
        r += (r + 64) >> 7;
    }
    r
}

/// 权重应用（全部 wrapping；sam 超 16 位走防溢出「胖」路径）。
fn apply_weight(w: i32, sam: i32) -> i32 {
    if sam != i32::from(sam as i16) {
        // 拆 32×32 乘法防溢出：低 16 位与高 16 位分开乘后相加。
        (((sam & 0xFFFF).wrapping_mul(w) >> 9)
            .wrapping_add(((sam & !0xFFFF) >> 9).wrapping_mul(w))
            .wrapping_add(1))
            >> 1
    } else {
        (w.wrapping_mul(sam).wrapping_add(512)) >> 10
    }
}

/// 权重更新（正 term；无钳制）。source = 预测依据值，result = 输入残差。
fn update_weight(w: &mut i32, delta: i32, source: i32, result: i32) {
    if source != 0 && result != 0 {
        let s = (source ^ result) >> 31;
        // 同号 → w += delta；异号 → w -= delta。
        *w = (delta ^ s).wrapping_add(w.wrapping_sub(s));
    }
}

/// 权重更新（负 term 专用，钳到 ±1024）。
fn update_weight_clip(w: &mut i32, delta: i32, source: i32, result: i32) {
    if source != 0 && result != 0 {
        let s = (source ^ result) >> 31;
        let mut w0 = (*w ^ s).wrapping_add(delta - s);
        w0 = w0.clamp(-1024, 1024);
        *w = (w0 ^ s) - s;
    }
}

/// 单个去相关 pass 的状态。
#[derive(Default, Clone)]
struct DecorrPass {
    term: i32,
    delta: i32,
    weight_a: i32,
    weight_b: i32,
    samples_a: [i32; 8],
    samples_b: [i32; 8],
}

/// mono 单 pass（term 17/18 用双槽历史；1..=8 用 8 槽环形，结尾旋转归一）。
fn decorr_mono_pass(p: &mut DecorrPass, buf: &mut [i32]) {
    let mut w = p.weight_a;
    match p.term {
        17 | 18 => {
            let mut s0 = p.samples_a[0];
            let mut s1 = p.samples_a[1];
            for s in buf.iter_mut() {
                let sam = if p.term == 17 {
                    2i32.wrapping_mul(s0).wrapping_sub(s1)
                } else {
                    3i32.wrapping_mul(s0).wrapping_sub(s1) >> 1
                };
                let out = apply_weight(w, sam).wrapping_add(*s);
                update_weight(&mut w, p.delta, sam, *s);
                s1 = s0;
                s0 = out;
                *s = out;
            }
            p.samples_a[0] = s0;
            p.samples_a[1] = s1;
        }
        1..=8 => {
            let term = p.term as usize;
            let mut m = 0usize;
            let mut k = term & 7;
            let mut ring = p.samples_a;
            for s in buf.iter_mut() {
                let sam = ring[m];
                let out = apply_weight(w, sam).wrapping_add(*s);
                update_weight(&mut w, p.delta, sam, *s);
                ring[k] = out;
                *s = out;
                m = (m + 1) & 7;
                k = (k + 1) & 7;
            }
            // mono 独有收尾：环形缓冲左旋归一（不影响本块已产出样本）。
            if m != 0 {
                let mut rotated = [0i32; 8];
                for (i, v) in rotated.iter_mut().enumerate() {
                    *v = ring[(m + i) & 7];
                }
                ring = rotated;
            }
            p.samples_a = ring;
        }
        _ => {}
    }
    p.weight_a = w;
}

/// stereo 单 pass（交错缓冲，逐帧先 L 后 R；A=左 B=右）。
fn decorr_stereo_pass(p: &mut DecorrPass, buf: &mut [i32]) {
    let (mut wa, mut wb) = (p.weight_a, p.weight_b);
    match p.term {
        17 | 18 => {
            let (mut a0, mut a1) = (p.samples_a[0], p.samples_a[1]);
            let (mut b0, mut b1) = (p.samples_b[0], p.samples_b[1]);
            for f in buf.as_chunks_mut::<2>().0 {
                let sam_a = if p.term == 17 {
                    2i32.wrapping_mul(a0).wrapping_sub(a1)
                } else {
                    a0.wrapping_add(a0.wrapping_sub(a1) >> 1)
                };
                let out_a = apply_weight(wa, sam_a).wrapping_add(f[0]);
                update_weight(&mut wa, p.delta, sam_a, f[0]);
                a1 = a0;
                a0 = out_a;
                f[0] = out_a;

                let sam_b = if p.term == 17 {
                    2i32.wrapping_mul(b0).wrapping_sub(b1)
                } else {
                    b0.wrapping_add(b0.wrapping_sub(b1) >> 1)
                };
                let out_b = apply_weight(wb, sam_b).wrapping_add(f[1]);
                update_weight(&mut wb, p.delta, sam_b, f[1]);
                b1 = b0;
                b0 = out_b;
                f[1] = out_b;
            }
            p.samples_a[0] = a0;
            p.samples_a[1] = a1;
            p.samples_b[0] = b0;
            p.samples_b[1] = b1;
        }
        1..=8 => {
            let term = p.term as usize;
            let mut m = 0usize;
            let mut k = term & 7;
            for f in buf.as_chunks_mut::<2>().0 {
                let sam_a = p.samples_a[m];
                let out_a = apply_weight(wa, sam_a).wrapping_add(f[0]);
                update_weight(&mut wa, p.delta, sam_a, f[0]);
                p.samples_a[k] = out_a;
                f[0] = out_a;

                let sam_b = p.samples_b[m];
                let out_b = apply_weight(wb, sam_b).wrapping_add(f[1]);
                update_weight(&mut wb, p.delta, sam_b, f[1]);
                p.samples_b[k] = out_b;
                f[1] = out_b;

                m = (m + 1) & 7;
                k = (k + 1) & 7;
            }
        }
        -1 => {
            // 用 B 的历史预测 A，再用新 A 预测 B（samples_b[0] 本 term 不消费）。
            let mut hist_a = p.samples_a[0];
            for f in buf.as_chunks_mut::<2>().0 {
                let sam_a = f[0].wrapping_add(apply_weight(wa, hist_a));
                update_weight_clip(&mut wa, p.delta, hist_a, f[0]);
                f[0] = sam_a;
                let out_b = f[1].wrapping_add(apply_weight(wb, sam_a));
                update_weight_clip(&mut wb, p.delta, sam_a, f[1]);
                hist_a = out_b;
                f[1] = out_b;
            }
            p.samples_a[0] = hist_a;
        }
        -2 => {
            // 镜像 -1：用 A 的历史预测 B，再用新 B 预测 A（samples_a[0] 不消费）。
            let mut hist_b = p.samples_b[0];
            for f in buf.as_chunks_mut::<2>().0 {
                let sam_b = f[1].wrapping_add(apply_weight(wb, hist_b));
                update_weight_clip(&mut wb, p.delta, hist_b, f[1]);
                f[1] = sam_b;
                let out_a = f[0].wrapping_add(apply_weight(wa, sam_b));
                update_weight_clip(&mut wa, p.delta, sam_b, f[0]);
                hist_b = out_a;
                f[0] = out_a;
            }
            p.samples_b[0] = hist_b;
        }
        -3 => {
            let (mut hist_a, mut hist_b) = (p.samples_a[0], p.samples_b[0]);
            for f in buf.as_chunks_mut::<2>().0 {
                let sam_a = f[0].wrapping_add(apply_weight(wa, hist_a));
                update_weight_clip(&mut wa, p.delta, hist_a, f[0]);
                let sam_b = f[1].wrapping_add(apply_weight(wb, hist_b));
                update_weight_clip(&mut wb, p.delta, hist_b, f[1]);
                f[0] = sam_a;
                hist_b = sam_a;
                f[1] = sam_b;
                hist_a = sam_b;
            }
            p.samples_a[0] = hist_a;
            p.samples_b[0] = hist_b;
        }
        _ => {}
    }
    p.weight_a = wa;
    p.weight_b = wb;
}

// =============================================================================
// 子块载荷解析
// =============================================================================

/// 元数据 id（本实现需要的）。
const ID_DECORR_TERMS: u8 = 0x02;
const ID_DECORR_WEIGHTS: u8 = 0x03;
const ID_DECORR_SAMPLES: u8 = 0x04;
const ID_ENTROPY_VARS: u8 = 0x05;
const ID_HYBRID_PROFILE: u8 = 0x06;
const ID_SHAPING_WEIGHTS: u8 = 0x07;
const ID_FLOAT_INFO: u8 = 0x08;
const ID_INT32_INFO: u8 = 0x09;
const ID_WV_BITSTREAM: u8 = 0x0A;
const ID_WVC_BITSTREAM: u8 = 0x0B;
const ID_WVX_BITSTREAM: u8 = 0x0C;
const ID_CHANNEL_INFO: u8 = 0x0D;
const ID_DSD_BLOCK: u8 = 0x0E;
const ID_SAMPLE_RATE: u8 = 0x27;
const ID_WVX_NEW_BITSTREAM: u8 = 0x2C;

/// 已知必识 id（id ≤ 0x1F 且不在此表 → 错误；≥ 0x20 不识别 → 跳过）。
fn is_known_required_id(id: u8) -> bool {
    matches!(
        id,
        0x00 | 0x01
            | ID_DECORR_TERMS
            | ID_DECORR_WEIGHTS
            | ID_DECORR_SAMPLES
            | ID_ENTROPY_VARS
            | ID_HYBRID_PROFILE
            | ID_SHAPING_WEIGHTS
            | ID_FLOAT_INFO
            | ID_INT32_INFO
            | ID_WV_BITSTREAM
            | ID_WVC_BITSTREAM
            | ID_WVX_BITSTREAM
            | ID_CHANNEL_INFO
            | ID_DSD_BLOCK
    )
}

/// 一块的解码要素（由子块收集）。
struct BlockParts<'a> {
    passes: Vec<DecorrPass>,
    medians: [Medians; 2],
    wv_stream: &'a [u8],
    int32_info: Option<[u8; 4]>,
    float_info: Option<[u8; 4]>,
    /// (crc_wvx, 位流, 是否新版格式)。
    wvx: Option<(u32, &'a [u8], bool)>,
}

/// 收集并解析一块的全部子块（含作用域闸门与顺序约束）。
fn collect_parts<'a>(
    header: &BlockHeader,
    meta: &'a [u8],
) -> Result<Option<BlockParts<'a>>, DecodeError> {
    let mut terms_raw: Option<&'a [u8]> = None;
    let mut weights_raw: Option<&'a [u8]> = None;
    let mut samples_raw: Option<&'a [u8]> = None;
    let mut entropy_raw: Option<&'a [u8]> = None;
    let mut wv_stream: Option<&'a [u8]> = None;
    let mut int32_info = None;
    let mut float_info = None;
    let mut wvx = None;

    for sub in SubBlocks::new(meta) {
        let sub = sub?;
        match sub.id {
            ID_HYBRID_PROFILE | ID_SHAPING_WEIGHTS | ID_WVC_BITSTREAM => {
                return Err(DecodeError::Unsupported("WavPack hybrid 模式不支持".into()));
            }
            ID_DSD_BLOCK => {
                return Err(DecodeError::Unsupported("WavPack DSD 流不支持".into()));
            }
            ID_CHANNEL_INFO => {
                let Some(&nch) = sub.data.first() else {
                    return Err(DecodeError::Decode("WavPack CHANNEL_INFO 空载荷".into()));
                };
                if nch > 2 {
                    return Err(DecodeError::Unsupported(
                        "WavPack 超过 2 声道的流不支持".into(),
                    ));
                }
            }
            ID_DECORR_TERMS => terms_raw = Some(sub.data),
            ID_DECORR_WEIGHTS => {
                if terms_raw.is_none() {
                    return Err(DecodeError::Decode(
                        "WavPack WEIGHTS 出现在 TERMS 之前".into(),
                    ));
                }
                weights_raw = Some(sub.data);
            }
            ID_DECORR_SAMPLES => {
                if terms_raw.is_none() {
                    return Err(DecodeError::Decode(
                        "WavPack SAMPLES 出现在 TERMS 之前".into(),
                    ));
                }
                samples_raw = Some(sub.data);
            }
            ID_ENTROPY_VARS => entropy_raw = Some(sub.data),
            ID_WV_BITSTREAM => wv_stream = Some(sub.data),
            ID_INT32_INFO => {
                if sub.data.len() != 4 {
                    return Err(DecodeError::Decode("WavPack INT32_INFO 长度非法".into()));
                }
                int32_info = Some([sub.data[0], sub.data[1], sub.data[2], sub.data[3]]);
            }
            ID_FLOAT_INFO => {
                if sub.data.len() != 4 {
                    return Err(DecodeError::Decode("WavPack FLOAT_INFO 长度非法".into()));
                }
                float_info = Some([sub.data[0], sub.data[1], sub.data[2], sub.data[3]]);
            }
            ID_WVX_BITSTREAM | ID_WVX_NEW_BITSTREAM => {
                if sub.data.len() <= 4 || sub.data.len() % 2 != 0 {
                    return Err(DecodeError::Decode("WavPack WVX 载荷长度非法".into()));
                }
                let crc = u32::from_le_bytes([sub.data[0], sub.data[1], sub.data[2], sub.data[3]]);
                wvx = Some((crc, &sub.data[4..], sub.id == ID_WVX_NEW_BITSTREAM));
            }
            ID_SAMPLE_RATE => {} // 采样率在 decode_stream / probe 层扫描，此处不收集
            id if id < 0x20 && !is_known_required_id(id) => {
                return Err(DecodeError::Decode(format!(
                    "WavPack 未知必识子块 id {id:#x}"
                )));
            }
            _ => {} // 可选子块：跳过
        }
    }

    // 纯元数据块（block_samples == 0）：跳过要素收集。
    if header.block_samples == 0 {
        return Ok(None);
    }

    let terms_raw = terms_raw.ok_or_else(|| DecodeError::Decode("WavPack 缺 TERMS".into()))?;
    let entropy_raw =
        entropy_raw.ok_or_else(|| DecodeError::Decode("WavPack 缺 ENTROPY_VARS".into()))?;
    let wv_stream = wv_stream.ok_or_else(|| DecodeError::Decode("WavPack 缺位流".into()))?;

    // TERMS：最多 16 字节；passes[0] 拿载荷最后一字节。
    if terms_raw.len() > MAX_NTERMS {
        return Err(DecodeError::Decode("WavPack TERMS 超过 16 条".into()));
    }
    let mono = header.mono_data();
    let mut passes = Vec::with_capacity(terms_raw.len());
    for &b in terms_raw.iter().rev() {
        let term = (b & 0x1F) as i32 - 5;
        let delta = (b >> 5) as i32;
        let legal = matches!(term, -3..=-1 | 1..=8 | 17 | 18);
        if !legal || (mono && term < 0) {
            return Err(DecodeError::Decode(format!(
                "WavPack 非法去相关 term {term}"
            )));
        }
        passes.push(DecorrPass {
            term,
            delta,
            ..Default::default()
        });
    }

    // WEIGHTS：从最后一个 pass 向前，每 pass 先 A 后 B。
    if let Some(data) = weights_raw {
        let termcnt = if mono { data.len() } else { data.len() / 2 };
        if termcnt > passes.len() {
            return Err(DecodeError::Decode("WavPack WEIGHTS 数量超 pass 数".into()));
        }
        let mut idx = 0;
        for p in passes.iter_mut().rev() {
            if idx >= data.len() {
                break;
            }
            p.weight_a = restore_weight(data[idx] as i8);
            idx += 1;
            if !mono {
                if idx >= data.len() {
                    break;
                }
                p.weight_b = restore_weight(data[idx] as i8);
                idx += 1;
            }
        }
    }

    // SAMPLES：i16 符号扩展经 wp_exp2s；从最后一个 pass 向前按布局读取。
    if let Some(data) = samples_raw {
        let mut pos = 0usize;
        let take = |pos: &mut usize| -> i32 {
            if *pos + 2 > data.len() {
                return 0;
            }
            let v = i16::from_le_bytes([data[*pos], data[*pos + 1]]) as i32;
            *pos += 2;
            wp_exp2s(v)
        };
        'outer: for p in passes.iter_mut().rev() {
            match p.term {
                17 | 18 => {
                    if pos + 4 > data.len() {
                        break 'outer;
                    }
                    p.samples_a[0] = take(&mut pos);
                    p.samples_a[1] = take(&mut pos);
                    if !mono {
                        if pos + 4 > data.len() {
                            break 'outer;
                        }
                        p.samples_b[0] = take(&mut pos);
                        p.samples_b[1] = take(&mut pos);
                    }
                }
                t if t < 0 => {
                    if pos + 4 > data.len() {
                        break 'outer;
                    }
                    p.samples_a[0] = take(&mut pos);
                    p.samples_b[0] = take(&mut pos);
                }
                t => {
                    for m in 0..(t as usize) {
                        if pos + 2 > data.len() {
                            break 'outer;
                        }
                        p.samples_a[m] = take(&mut pos);
                        if !mono {
                            if pos + 2 > data.len() {
                                break 'outer;
                            }
                            p.samples_b[m] = take(&mut pos);
                        }
                    }
                }
            }
        }
        if pos != data.len() {
            return Err(DecodeError::Decode("WavPack SAMPLES 载荷长度不符".into()));
        }
    }

    // ENTROPY_VARS：恰好 6/12 字节；u16 零扩展经 wp_exp2s。
    let want = if mono { 6 } else { 12 };
    if entropy_raw.len() != want {
        return Err(DecodeError::Decode("WavPack ENTROPY_VARS 长度非法".into()));
    }
    let mut medians = [Medians::default(); 2];
    for (i, chunk) in entropy_raw.as_chunks::<2>().0.iter().enumerate() {
        let log = u16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        medians[i / 3].median[i % 3] = wp_exp2s(log) as u32;
    }

    Ok(Some(BlockParts {
        passes,
        medians,
        wv_stream,
        int32_info,
        float_info,
        wvx,
    }))
}

// =============================================================================
// 样本重建（单块）
// =============================================================================

/// 解码一块，产出交错 i32 样本（含全部 fixup 与校验）。
fn decode_block(header: &BlockHeader, meta: &[u8]) -> Result<Vec<i32>, DecodeError> {
    let Some(parts) = collect_parts(header, meta)? else {
        return Ok(Vec::new()); // 纯元数据块
    };
    let mono = header.mono_data();

    // 1. 熵解码残差。
    let mut buffer: Vec<i32> = Vec::new();
    let mut words = WordsDecoder::new(parts.medians);
    let mut reader = BsReader::new(parts.wv_stream);
    if !words.get_words_lossless(&mut reader, header.block_samples, mono, &mut buffer) {
        return Err(DecodeError::Decode("WavPack 位流耗尽（截断或损坏）".into()));
    }

    // 2. 逆去相关（pass 数组正序 = 编码逆序）。
    let mut passes = parts.passes;
    for p in passes.iter_mut() {
        if mono {
            decorr_mono_pass(p, &mut buffer);
        } else {
            decorr_stereo_pass(p, &mut buffer);
        }
    }

    // 3. joint stereo 还原 + 块 CRC + 幅度检查（同一循环）。
    let mute_limit = (1i64 << header.magnitude()) + 2;
    let mut crc: u32 = 0xFFFF_FFFF;
    if mono {
        for &s in &buffer {
            if (s as i64).abs() > mute_limit {
                return Err(DecodeError::Decode("WavPack 样本超声明幅度".into()));
            }
            crc = crc.wrapping_mul(3).wrapping_add(s as u32);
        }
    } else {
        for f in buffer.as_chunks_mut::<2>().0 {
            if header.joint_stereo() {
                f[1] = f[1].wrapping_sub(f[0] >> 1);
                f[0] = f[0].wrapping_add(f[1]);
            }
            if (f[0] as i64).abs() > mute_limit || (f[1] as i64).abs() > mute_limit {
                return Err(DecodeError::Decode("WavPack 样本超声明幅度".into()));
            }
            crc = crc
                .wrapping_mul(9)
                .wrapping_add((f[0] as u32).wrapping_mul(3))
                .wrapping_add(f[1] as u32);
        }
    }
    if crc != header.crc {
        return Err(DecodeError::Decode("WavPack 块 CRC 校验失败".into()));
    }

    // 4. fixup：float（不再走整数 shift）或 int32；然后统一 shift。
    if header.float_data() {
        let info = parts
            .float_info
            .ok_or_else(|| DecodeError::Decode("WavPack 缺 FLOAT_INFO".into()))?;
        return float_fixup(&mut buffer, info, parts.wvx, mono, header.false_stereo());
    }
    let mut shift = header.output_shift();
    if header.int32_data() {
        let info = parts
            .int32_info
            .ok_or_else(|| DecodeError::Decode("WavPack 缺 INT32_INFO".into()))?;
        shift = int32_fixup(&mut buffer, info, parts.wvx, shift)?;
    }
    shift &= 0x1F;
    if shift != 0 {
        for v in buffer.iter_mut() {
            *v = ((*v as u32) << shift) as i32;
        }
    }

    // 5. false stereo 展开（float 路径在其内部已处理）。
    if header.false_stereo() {
        let mut expanded = Vec::with_capacity(buffer.len() * 2);
        for &s in &buffer {
            expanded.push(s);
            expanded.push(s);
        }
        buffer = expanded;
    }
    Ok(buffer)
}

/// INT32 fixup（含可选 wvx 低位恢复）；返回调整后的 shift。
fn int32_fixup(
    buffer: &mut [i32],
    info: [u8; 4],
    wvx: Option<(u32, &[u8], bool)>,
    mut shift: u32,
) -> Result<u32, DecodeError> {
    let sent_bits = u32::from(info[0] & 0x1F);
    let zeros = u32::from(info[1] & 0x1F);
    let ones = u32::from(info[2] & 0x1F);
    let dups = u32::from(info[3] & 0x1F);

    let expand = |v: i32| -> i32 {
        if zeros != 0 {
            v << zeros
        } else if ones != 0 {
            v.wrapping_add(1).wrapping_shl(ones).wrapping_sub(1)
        } else if dups != 0 {
            let lo = v & 1;
            v.wrapping_add(lo).wrapping_shl(dups).wrapping_sub(lo)
        } else {
            v
        }
    };

    match wvx {
        Some((crc_wvx, stream, is_new)) => {
            let mut r = BsReader::new(stream);
            let max_width = if is_new { r.getbits(5) } else { 0 };
            let mut crc_x: u32 = 0xFFFF_FFFF;
            for v in buffer.iter_mut() {
                if sent_bits != 0 {
                    if max_width != 0 {
                        let pv = if *v < 0 { !(*v as u32) } else { *v as u32 };
                        let vbits = 32 - pv.leading_zeros();
                        let width = vbits + sent_bits;
                        let n = if width <= max_width {
                            sent_bits
                        } else {
                            sent_bits.saturating_sub(width - max_width)
                        };
                        if n > 0 {
                            let data = r.getbits(n) & ((1u32 << n) - 1);
                            *v = (((*v) << n) | (data as i32)) << (sent_bits - n);
                        } else {
                            *v <<= sent_bits;
                        }
                    } else {
                        let data = r.getbits(sent_bits) & ((1u32 << sent_bits) - 1);
                        *v = ((*v) << sent_bits) | (data as i32);
                    }
                }
                *v = expand(*v);
                crc_x = crc_x
                    .wrapping_mul(9)
                    .wrapping_add(((*v as u32) & 0xFFFF).wrapping_mul(3))
                    .wrapping_add(((*v as u32) >> 16) & 0xFFFF);
            }
            if r.error {
                return Err(DecodeError::Decode("WavPack WVX 位流耗尽".into()));
            }
            if crc_x != crc_wvx {
                return Err(DecodeError::Decode("WavPack WVX CRC 校验失败".into()));
            }
            Ok(shift)
        }
        None => {
            if sent_bits == 0 && (zeros + ones + dups) != 0 {
                for v in buffer.iter_mut() {
                    *v = expand(*v);
                }
            } else {
                shift += zeros + sent_bits + ones + dups;
            }
            Ok(shift)
        }
    }
}

/// FLOAT fixup：把解码值还原为 IEEE-754 位型（全程整数运算）。
/// 输出覆盖 buffer 为位型 i32；false stereo 展开也在此处理。
fn float_fixup(
    buffer: &mut [i32],
    info: [u8; 4],
    wvx: Option<(u32, &[u8], bool)>,
    mono: bool,
    false_stereo: bool,
) -> Result<Vec<i32>, DecodeError> {
    let flags = info[0];
    let shift = info[1] & 0x1F;
    let max_exp = info[2];
    const SHIFT_ONES: u8 = 0x01;
    const SHIFT_SAME: u8 = 0x02;
    const SHIFT_SENT: u8 = 0x04;
    const ZEROS_SENT: u8 = 0x08;
    const NEG_ZEROS: u8 = 0x10;

    let mut out: Vec<i32> = Vec::with_capacity(buffer.len());
    match wvx {
        Some((crc_wvx, stream, is_new)) => {
            let mut r = BsReader::new(stream);
            let (min_shifted_zeros, max_shifted_ones) = if is_new {
                (r.getbits(5), r.getbits(5))
            } else {
                (0, 0)
            };
            let mut crc: u32 = 0xFFFF_FFFF;
            for &value in buffer.iter() {
                let mut mantissa: u32 = 0;
                let mut exponent: u32 = 0;
                let mut sign: u32 = 0;
                let mut exp = u32::from(max_exp);
                if value == 0 {
                    if flags & ZEROS_SENT != 0 {
                        if r.getbit() != 0 {
                            mantissa = r.getbits(23) & 0x7F_FFFF;
                            if exp >= 25 {
                                exponent = r.getbits(8) & 0xFF;
                            }
                            sign = r.getbit();
                        } else if flags & NEG_ZEROS != 0 {
                            sign = r.getbit();
                        }
                    }
                } else {
                    let mut v = (value as u32) << shift;
                    if (v as i32) < 0 {
                        v = (-(v as i32)) as u32;
                        sign = 1;
                    }
                    if v == 0x0100_0000 {
                        // Inf / NaN 哨兵。
                        if r.getbit() != 0 {
                            mantissa = r.getbits(23) & 0x7F_FFFF;
                        }
                        exponent = 255;
                    } else {
                        // 左归一化到 bit23（sc 记录移出的低位数）。
                        let mut sc = 0u32;
                        if exp != 0 {
                            loop {
                                if v & 0x80_0000 != 0 {
                                    break;
                                }
                                exp -= 1;
                                if exp == 0 {
                                    break;
                                }
                                sc += 1;
                                v <<= 1;
                            }
                        }
                        sc &= 0x1F;
                        if sc != 0 {
                            if (flags & SHIFT_ONES != 0)
                                || (flags & SHIFT_SAME != 0 && r.getbit() != 0)
                            {
                                v |= (1u32 << sc) - 1;
                            } else if flags & SHIFT_SENT != 0 {
                                let mut num_zeros = 0u32;
                                if max_shifted_ones != 0 && sc > max_shifted_ones {
                                    num_zeros = sc - max_shifted_ones;
                                }
                                if min_shifted_zeros > num_zeros {
                                    num_zeros = min_shifted_zeros.min(sc);
                                }
                                if sc > num_zeros {
                                    let n = sc - num_zeros;
                                    v |= (r.getbits(n) << num_zeros) & ((1u32 << sc) - 1);
                                }
                            }
                        }
                        mantissa = v & 0x7F_FFFF;
                        exponent = exp;
                    }
                }
                crc = crc
                    .wrapping_mul(27)
                    .wrapping_add(mantissa.wrapping_mul(9))
                    .wrapping_add(exponent.wrapping_mul(3))
                    .wrapping_add(sign);
                out.push(((sign << 31) | (exponent << 23) | mantissa) as i32);
            }
            if r.error {
                return Err(DecodeError::Decode("WavPack WVX 位流耗尽".into()));
            }
            if crc != crc_wvx {
                return Err(DecodeError::Decode("WavPack WVX CRC 校验失败".into()));
            }
        }
        None => {
            // 无 wvx：仅在编码器判定无残余可发时无损。
            for &value in buffer.iter() {
                if value == 0 {
                    out.push(0);
                    continue;
                }
                let mut exp = u32::from(max_exp);
                let mut sign = 0u32;
                let mut v = (value as u32) << shift;
                if (v as i32) < 0 {
                    v = (-(v as i32)) as u32;
                    sign = 1;
                }
                if v >= 0x0100_0000 {
                    while v & 0x0F00_0000 != 0 {
                        v >>= 1;
                        exp += 1;
                    }
                } else if exp != 0 {
                    let mut sc = 0u32;
                    loop {
                        if v & 0x80_0000 != 0 {
                            break;
                        }
                        exp -= 1;
                        if exp == 0 {
                            break;
                        }
                        sc += 1;
                        v <<= 1;
                    }
                    sc &= 0x1F;
                    if sc != 0 && (flags & SHIFT_ONES != 0) {
                        v |= (1u32 << sc) - 1;
                    }
                }
                out.push(((sign << 31) | (exp << 23) | (v & 0x7F_FFFF)) as i32);
            }
        }
    }
    let _ = mono;
    if false_stereo {
        let mut expanded = Vec::with_capacity(out.len() * 2);
        for &s in &out {
            expanded.push(s);
            expanded.push(s);
        }
        out = expanded;
    }
    Ok(out)
}

// =============================================================================
// 多块流
// =============================================================================

/// 整流解码结果（字段语义与原后端相同）。
struct DecodedStream {
    samples: Vec<i32>,
    channels: u32,
    sample_rate: u32,
    bits_per_sample: u32,
    is_float: bool,
}

/// 顺序迭代全部块并拼接样本（首块决定输出格式，后续块格式必须一致）。
fn decode_stream(bytes: &[u8]) -> Result<DecodedStream, DecodeError> {
    let mut pos = 0usize;
    let mut samples: Vec<i32> = Vec::new();
    let mut format: Option<(u16, u32, u32, bool)> = None; // (ch, rate, bits, float)

    while pos < bytes.len() {
        if pos + HEADER_LEN > bytes.len() {
            // 尾部不足一块：可能是 APEv2/ID3v1 标签（合法——WavPack 原生
            // 标签格式正是 APEv2，foobar2000/dBpoweramp 默认写入 .wv 尾部）。
            // 已解码到至少一块则视为流结束，否则报截断错误。
            if format.is_some() && !samples.is_empty() {
                break;
            }
            return Err(DecodeError::Decode("WavPack 流尾部不足一块（截断）".into()));
        }
        // 遇到非 wvpk 魔数（尾部标签的开头）：同样视为流结束。
        if &bytes[pos..pos + 4] != b"wvpk" {
            if format.is_some() && !samples.is_empty() {
                break;
            }
            return Err(DecodeError::Decode("WavPack 块魔数不匹配".into()));
        }
        let header = parse_block_header(&bytes[pos..pos + HEADER_LEN])?;
        let end = pos + header.block_len;
        if end > bytes.len() {
            return Err(DecodeError::Decode("WavPack 块体截断".into()));
        }
        if header.bytes_per_sample() == 1 {
            return Err(DecodeError::Unsupported("WavPack 8 位整数流不支持".into()));
        }
        let meta = &bytes[pos + HEADER_LEN..end];
        let block_samples = decode_block(&header, meta)?;

        if header.block_samples > 0 {
            let channels = u32::from(header.channels());
            let bits = header.bytes_per_sample() * 8;
            let is_float = header.float_data();
            let rate = if header.srate_index() == 0xF {
                // 非标准率：取本块 SAMPLE_RATE 子块（无则非法）。
                let mut found = None;
                for sub in SubBlocks::new(meta).flatten() {
                    if sub.id == ID_SAMPLE_RATE && sub.data.len() >= 3 {
                        found = Some(
                            u32::from(sub.data[0])
                                | u32::from(sub.data[1]) << 8
                                | u32::from(sub.data[2]) << 16,
                        );
                    }
                }
                found.ok_or_else(|| {
                    DecodeError::Decode("WavPack 非标准采样率缺 SAMPLE_RATE 子块".into())
                })?
            } else {
                SAMPLE_RATES[header.srate_index() as usize]
            };
            match format {
                None => {
                    format = Some((channels as u16, rate, bits, is_float));
                }
                Some((ch, sr, b, f)) => {
                    if ch != channels as u16 || sr != rate || b != bits || f != is_float {
                        return Err(DecodeError::Decode("WavPack 多块格式中途变化".into()));
                    }
                }
            }
            samples.extend_from_slice(&block_samples);
        }
        pos = end;
    }

    let (ch, rate, bits, is_float) =
        format.ok_or_else(|| DecodeError::Decode("WavPack 空流（无音频块）".into()))?;
    Ok(DecodedStream {
        samples,
        channels: ch as u32,
        sample_rate: rate,
        bits_per_sample: bits,
        is_float,
    })
}

// =============================================================================
// 后端
// =============================================================================

/// 判断路径是否应交给 WavPack 后端：扩展名为 `.wv`（大小写不敏感）。
pub(crate) fn is_wavpack_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wv"))
}

/// 读取首块头与首块元数据（只读 32 字节头 + 首块体，不整文件读入、不解码）。
fn read_first_block(path: &Path) -> Option<(BlockHeader, Vec<u8>)> {
    let mut file = File::open(path).ok()?;
    let mut head = [0u8; HEADER_LEN];
    file.read_exact(&mut head).ok()?;
    let header = parse_block_header(&head).ok()?;
    let meta_len = header.block_len.saturating_sub(HEADER_LEN);
    // block_len 来自文件声明（u32），上限设 1 MiB——正常 FLAC/WavPack 块
    // 远小于此；伪造巨大 block_len 属 DoS 向量（先分配后读失败仍占内存）。
    if meta_len > 1 << 20 {
        return None; // 异常大块属伪造/损坏：按"探测失败"处理
    }
    let mut meta = vec![0u8; meta_len];
    file.read_exact(&mut meta).ok()?;
    Some((header, meta))
}

/// 轻量探测 `.wv` 文件的采样率（只读首块，不整文件读入）。
///
/// 标准采样率直接从首块头的 rate index 读出；非标准采样率（index 0xF）
/// 再读首块的 `SAMPLE_RATE` 子块（3 字节 LE）。任何读取失败都返回 None
///（调用方回退到设备默认采样率 + 软件重采样），零副作用。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    let (header, meta) = read_first_block(path)?;
    if header.srate_index() != 0xF {
        return Some(SAMPLE_RATES[header.srate_index() as usize]);
    }
    for sub in SubBlocks::new(&meta).flatten() {
        if sub.id == ID_SAMPLE_RATE && sub.data.len() >= 3 {
            return Some(
                u32::from(sub.data[0]) | u32::from(sub.data[1]) << 8 | u32::from(sub.data[2]) << 16,
            );
        }
    }
    None
}

/// 轻量探测 `.wv` 的技术参数（采样率/声道/位深/时长），不解码音频样本。
///
/// 时长通过逐块读 32 字节头并跳过块体、累加 `block_samples` 求得，不触发
/// 任何熵解码——修复 probe_metadata 对 .wv 整文件解码的旧缺陷。
/// 标签留空（APEv2 标签读取属后续工作，与 Opus 口径一致）。
pub(crate) fn probe_params(path: &Path) -> Option<AudioParams> {
    let (first, meta) = read_first_block(path)?;
    let sample_rate = if first.srate_index() != 0xF {
        SAMPLE_RATES[first.srate_index() as usize]
    } else {
        let mut found = None;
        for sub in SubBlocks::new(&meta).flatten() {
            if sub.id == ID_SAMPLE_RATE && sub.data.len() >= 3 {
                found = Some(
                    u32::from(sub.data[0])
                        | u32::from(sub.data[1]) << 8
                        | u32::from(sub.data[2]) << 16,
                );
                break;
            }
        }
        found?
    };
    let channels = first.channels();
    let bits = first.bytes_per_sample() * 8;

    // 逐块头累加帧数（block_samples 为每声道帧数），跳块体不解码。
    //
    // 迭代上限：正常 `.wv` 每块约 0.5 s 音频（1 小时素材约 7 千块），65536 块
    // 对应数十小时，正常文件远不会触顶。但块长由文件自己声明，`parse_block_header`
    // 只拒 `ck_size < 24`（故最小步长 32 字节）——伪造一个"每块仅头部"的文件
    // 会让循环跑 文件大小/32 次（4 GB ≈ 1.3 亿次 seek+read，分钟级挂起）。
    // 探测路径（导入 / 扫描媒体库）不该被单个文件拖住。
    const MAX_BLOCK_SCAN: u64 = 65536;
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let mut total_frames = u64::from(first.block_samples);
    let mut pos = first.block_len as u64;
    let mut scanned = 0u64;
    let mut truncated = false;
    while pos < file_len {
        if scanned >= MAX_BLOCK_SCAN {
            truncated = true;
            break;
        }
        scanned += 1;
        file.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; HEADER_LEN];
        if file.read_exact(&mut h).is_err() {
            break;
        }
        let bh = match parse_block_header(&h) {
            Ok(b) => b,
            Err(_) => break,
        };
        total_frames += u64::from(bh.block_samples);
        pos += bh.block_len as u64;
    }
    // 扫描被截断时 total_frames 只是下界，据此算出的时长会偏短（进度条与
    // 时长显示都会失真）——宁可报"时长未知"（上层按未知时长口径处理），
    // 也不给出一个偏小的假时长。
    let duration = if sample_rate > 0 && !truncated {
        Some(total_frames as f64 / f64::from(sample_rate))
    } else {
        None
    };

    Some(AudioParams::new(
        Some(sample_rate),
        Some(channels),
        Some(bits),
        "WavPack".into(),
        0,
        duration,
    ))
}

/// WavPack 解码后端（自研实现，经 [`super::decoder::open_backend`] 分发）。
///
/// 一个实例对应一个已打开的 `.wv` 文件；换曲时丢弃旧实例、新建。
/// 打开时即整段解码，`decode_next` 按块切片输出，seek 为内存游标 O(1)。
pub(crate) struct WavPackBackend {
    /// 技术参数（采样率 / 声道 / 位深 / 编码名 / 时长）。
    params: AudioParams,
    /// 整段解码得到的交错样本（i32）。
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
        let bytes = std::fs::read(path)?;
        let decoded = decode_stream(&bytes)?;

        let channels = decoded.channels as usize;
        if channels == 0 {
            return Err(DecodeError::Unsupported("WavPack 声道数为 0".to_string()));
        }
        let sample_rate = decoded.sample_rate;
        let total_frames = decoded.samples.len() / channels;
        let duration = if sample_rate == 0 {
            None
        } else {
            Some(total_frames as f64 / f64::from(sample_rate))
        };
        let bits = decoded.bits_per_sample;
        let int_divisor = if bits >= 2 {
            2.0f64.powi((bits - 1) as i32)
        } else {
            1.0
        };
        let params = AudioParams::new(
            Some(sample_rate),
            Some(channels as u16),
            Some(bits),
            "WavPack".to_string(),
            0,
            duration,
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

    /// 浮点流：i32 即 f32 的 IEEE 位型，逐位还原；整数流：除以 2^(位深-1)
    /// 归一化（经 f64 中转避免 24/32 位大值的直接舍入）。
    #[inline]
    fn to_f32(&self, s: i32) -> f32 {
        if self.is_float {
            f32::from_bits(s as u32)
        } else {
            (s as f64 / self.int_divisor) as f32
        }
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
        // 截断（向下取整）：与其他后端（cdda/wav/flac）的 as u64 口径一致。
        let target = (secs * rate) as usize;
        let total_frames = self.samples.len() / self.channels;
        self.cursor_frames = target.min(total_frames);
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

    /// xorshift64* 伪随机（测试数据生成，避免引入 rand 依赖）。
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545F4914F6CDD1D)
        }
    }

    /// 生成测试样本（interleaved i32，限定在 bits 位深内）。
    /// 模式混合：平滑波形 / 随机 / 静音段（零行程）/ 极值跳变。
    fn gen_samples(frames: usize, channels: usize, bits: u32, seed: u64) -> Vec<i32> {
        let mut rng = Rng(seed | 1);
        let max = (1i64 << (bits - 1)) - 1;
        let min = -(1i64 << (bits - 1));
        let mut out = Vec::with_capacity(frames * channels);
        for f in 0..frames {
            for c in 0..channels {
                let phase = (f % 977) as f64 / 977.0 * std::f64::consts::TAU;
                let seg = (f / 61) % 4;
                let v: i64 = match seg {
                    0 => ((phase * (1 + c) as f64).sin() * max as f64 * 0.8) as i64,
                    1 => (rng.next() as i64) % (max + 1),
                    2 => 0,
                    _ => {
                        if rng.next() & 1 == 1 {
                            max
                        } else {
                            min
                        }
                    }
                };
                out.push(v.clamp(min, max) as i32);
            }
        }
        out
    }

    fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("tuneux-wv-test-{}-{seq}.wv", std::process::id()));
        let mut f = std::fs::File::create(&path).expect("应能创建临时文件");
        f.write_all(bytes).expect("应能写入临时文件");
        path
    }

    // -----------------------------------------------------------------
    // 位级组件自检
    // -----------------------------------------------------------------

    /// read_code 位序自检（规格样例：字节 0b1110_0100 → 连续 read_code(3) 得 0,2,1,3）。
    #[test]
    fn read_code_bit_order_selfcheck() {
        let mut r = BsReader::new(&[0b1110_0100]);
        assert_eq!(r.read_code(3), 0);
        assert_eq!(r.read_code(3), 2);
        assert_eq!(r.read_code(3), 1);
        assert_eq!(r.read_code(3), 3);
    }

    /// getbit 为字节内 LSB 优先。
    #[test]
    fn getbit_lsb_first() {
        let mut r = BsReader::new(&[0b0000_0101]);
        assert_eq!(r.getbit(), 1);
        assert_eq!(r.getbit(), 0);
        assert_eq!(r.getbit(), 1);
        assert_eq!(r.getbit(), 0);
        assert_eq!(r.getbits(4), 0);
    }

    /// wp_exp2s 锚点（0 / ±0x100 与中间值）。
    #[test]
    fn exp2s_anchors() {
        assert_eq!(wp_exp2s(0), 0);
        assert_eq!(wp_exp2s(0x100), 1);
        assert_eq!(wp_exp2s(-0x100), -1);
        assert_eq!(wp_exp2s(0x200), 2);
        // e=1 的截尾路径：2^1.5 ≈ 2.83 → 尾数 362 >> 8 = 1。
        assert_eq!(wp_exp2s(0x180), 1);
        assert_eq!(wp_exp2s(-0x180), -1);
    }

    /// restore_weight 端点与符号处理。
    #[test]
    fn restore_weight_values() {
        assert_eq!(restore_weight(0), 0);
        assert_eq!(restore_weight(1), 8);
        assert_eq!(restore_weight(-1), -8);
        assert_eq!(restore_weight(127), 1024);
        assert_eq!(restore_weight(-128), -1024);
    }

    // -----------------------------------------------------------------
    // 往返对拍（wavicle dev-dep 编码 → 自研解码 vs wavicle 解码 vs 原样本）
    // -----------------------------------------------------------------

    /// 整数往返：给定声道/位深/帧数，编码后双解码器位级一致且等于原样本。
    fn roundtrip_int(frames: usize, channels: u32, bits: u32, seed: u64) {
        let raw = gen_samples(frames, channels as usize, bits, seed);
        let bytes = wavicle::encode_int(
            wavicle::EncodeParams {
                channels,
                sample_rate: 44100,
                bits_per_sample: bits,
            },
            &raw,
        )
        .expect("参考编码器应产出");
        let mine = decode_stream(&bytes).expect("自研应解码");
        let theirs = wavicle::decode_stream(&bytes).expect("wavicle 应解码");
        assert_eq!(mine.channels, theirs.channels, "声道数不一致");
        assert_eq!(mine.sample_rate, theirs.sample_rate, "采样率不一致");
        assert_eq!(mine.bits_per_sample, theirs.bits_per_sample, "位深不一致");
        assert_eq!(mine.is_float, theirs.is_float, "float 标记不一致");
        assert_eq!(mine.samples, theirs.samples, "样本位级不一致");
        assert_eq!(mine.samples, raw, "与原样本不一致（非无损）");
    }

    #[test]
    fn roundtrip_int16_stereo() {
        roundtrip_int(3000, 2, 16, 11);
    }

    #[test]
    fn roundtrip_int16_mono() {
        roundtrip_int(3000, 1, 16, 22);
    }

    #[test]
    fn roundtrip_int24_stereo() {
        roundtrip_int(2000, 2, 24, 33);
    }

    #[test]
    fn roundtrip_int32_stereo() {
        roundtrip_int(2000, 2, 32, 44);
    }

    /// 多块文件（10 万帧 > 单块 32768 帧上限）：拼接逻辑对拍。
    #[test]
    fn roundtrip_multiblock() {
        roundtrip_int(100_000, 2, 16, 55);
    }

    /// 32 位浮点往返：双解码器位级一致且逐位等于原样本。
    #[test]
    fn roundtrip_float32_stereo() {
        let mut rng = Rng(66 | 1);
        let frames = 2000usize;
        let mut raw = Vec::with_capacity(frames * 2);
        for f in 0..frames {
            for c in 0..2 {
                let phase = (f % 311) as f64 / 311.0 * std::f64::consts::TAU;
                let seg = (f / 53) % 3;
                let v: f32 = match seg {
                    0 => ((phase * (1 + c) as f64).sin() * 0.9) as f32,
                    1 => (rng.next() as f32 / u64::MAX as f32) * 2.0 - 1.0,
                    _ => 0.0,
                };
                raw.push(v);
            }
        }
        let bytes = wavicle::encode_float(2, 44100, &raw).expect("参考编码器应产出");
        let mine = decode_stream(&bytes).expect("自研应解码");
        let theirs = wavicle::decode_stream(&bytes).expect("wavicle 应解码");
        assert!(mine.is_float, "应为浮点流");
        assert_eq!(mine.samples, theirs.samples, "样本位级不一致");
        let mine_f: Vec<f32> = mine
            .samples
            .iter()
            .map(|&s| f32::from_bits(s as u32))
            .collect();
        assert_eq!(mine_f.len(), raw.len());
        for (i, (a, b)) in mine_f.iter().zip(raw.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "第 {i} 个浮点样本不一致：{a} vs {b}"
            );
        }
    }

    /// 8 位整数流：与 wavicle 同口径拒绝（不支持）。
    #[test]
    fn int8_is_unsupported() {
        let raw = gen_samples(64, 1, 8, 77);
        let bytes = wavicle::encode_int(
            wavicle::EncodeParams {
                channels: 1,
                sample_rate: 8000,
                bits_per_sample: 8,
            },
            &raw,
        )
        .expect("参考编码器应产出");
        assert!(matches!(
            decode_stream(&bytes),
            Err(DecodeError::Unsupported(_))
        ));
    }

    // -----------------------------------------------------------------
    // 后端端到端
    // -----------------------------------------------------------------

    /// WavPackBackend：open → 参数 → 全量输出 → seek → EOF。
    #[test]
    fn backend_end_to_end() {
        let raw = gen_samples(1000, 2, 16, 88);
        let bytes = wavicle::encode_int(
            wavicle::EncodeParams {
                channels: 2,
                sample_rate: 44100,
                bits_per_sample: 16,
            },
            &raw,
        )
        .expect("编码");
        let path = write_temp(&bytes);
        let mut b = WavPackBackend::open(&path).expect("应能打开");
        assert_eq!(b.params().sample_rate, Some(44100));
        assert_eq!(b.params().channels, Some(2));
        assert_eq!(b.params().bits_per_sample, Some(16));
        assert_eq!(b.params().codec_name, "WavPack");
        let d = b.params().duration.expect("应有时长");
        assert!((d - 1000.0 / 44100.0).abs() < 1e-9, "duration 实际 {d}");
        let mut out = Vec::new();
        while let Ok(Some(chunk)) = b.decode_next() {
            out.extend_from_slice(&chunk);
        }
        assert_eq!(out.len(), raw.len());
        for (i, (o, s)) in out.iter().zip(raw.iter()).enumerate() {
            assert_eq!(*o, (*s as f64 / 32768.0) as f32, "第 {i} 个样本不符");
        }
        // seek 到 0.01s（441 帧）后首样本 = raw[441*2]。
        b.seek(0.01).expect("seek 应成功");
        let mut tail = Vec::new();
        while let Ok(Some(chunk)) = b.decode_next() {
            tail.extend_from_slice(&chunk);
        }
        assert_eq!(tail[0], (raw[441 * 2] as f64 / 32768.0) as f32);
        assert_eq!(tail.len(), raw.len() - 441 * 2);
        // 越界 → EOF；非法输入拒绝。
        b.seek(999.0).expect("越界 seek 应钳到末尾");
        assert!(b.decode_next().expect("读").is_none());
        assert!(matches!(b.seek(-1.0), Err(DecodeError::Seek(_))));
        assert!(matches!(b.seek(f64::NAN), Err(DecodeError::Seek(_))));
        let _ = std::fs::remove_file(&path);
    }

    /// 直通探测：标准率索引与 SAMPLE_RATE 子块（非标准率）两路径。
    #[test]
    fn probe_sample_rate_paths() {
        let raw = gen_samples(16, 1, 16, 99);
        let bytes = wavicle::encode_int(
            wavicle::EncodeParams {
                channels: 1,
                sample_rate: 44100,
                bits_per_sample: 16,
            },
            &raw,
        )
        .expect("编码");
        let path = write_temp(&bytes);
        assert_eq!(probe_sample_rate(&path), Some(44100));
        assert!(is_wavpack_path(&path));
        let _ = std::fs::remove_file(&path);

        // 非标准采样率（46000 不在索引表）→ SAMPLE_RATE 子块路径。
        let bytes = wavicle::encode_int(
            wavicle::EncodeParams {
                channels: 1,
                sample_rate: 46000,
                bits_per_sample: 16,
            },
            &raw,
        )
        .expect("编码");
        let path = write_temp(&bytes);
        assert_eq!(probe_sample_rate(&path), Some(46000));
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------
    // 边界与错误分类
    // -----------------------------------------------------------------

    /// 坏魔数 → 解码错误；hybrid / 版本越界 → 不支持。
    #[test]
    fn error_classification() {
        assert!(matches!(
            decode_stream(b"NOPE________"),
            Err(DecodeError::Decode(_))
        ));

        let raw = gen_samples(16, 1, 16, 111);
        let good = wavicle::encode_int(
            wavicle::EncodeParams {
                channels: 1,
                sample_rate: 8000,
                bits_per_sample: 16,
            },
            &raw,
        )
        .expect("编码");

        // hybrid 标志位（flags 第 3 位）→ Unsupported。
        let mut hybrid = good.clone();
        hybrid[24] |= 1 << 3;
        assert!(matches!(
            decode_stream(&hybrid),
            Err(DecodeError::Unsupported(_))
        ));

        // 版本越界（0x500）→ Unsupported。
        let mut badver = good.clone();
        badver[8] = 0x00;
        badver[9] = 0x05;
        assert!(matches!(
            decode_stream(&badver),
            Err(DecodeError::Unsupported(_))
        ));

        // 截断 → Decode。
        let truncated = &good[..good.len() / 2];
        assert!(matches!(
            decode_stream(truncated),
            Err(DecodeError::Decode(_))
        ));
    }

    /// 位流区翻转一字节 → 块 CRC 校验失败（Decode）。
    #[test]
    fn crc_corruption_detected() {
        let raw = gen_samples(500, 1, 16, 222);
        let mut bytes = wavicle::encode_int(
            wavicle::EncodeParams {
                channels: 1,
                sample_rate: 8000,
                bits_per_sample: 16,
            },
            &raw,
        )
        .expect("编码");
        // 翻转位流区一字节（块内偏移 ~60 处，确保落在数据区）。
        let idx = 60;
        bytes[idx] ^= 0xFF;
        assert!(matches!(decode_stream(&bytes), Err(DecodeError::Decode(_))));
    }

    /// 真实语料对拍：测试音频/测试样例.wv，自研与 wavicle 位级一致。
    #[test]
    #[ignore = "需要 测试音频 夹具；运行：cargo test -- --ignored"]
    fn bit_exact_real_world_wavpack() {
        let candidates = ["测试音频/测试样例.wv", "../../测试音频/测试样例.wv"];
        let path = candidates
            .iter()
            .map(std::path::Path::new)
            .find(|p| p.exists())
            .expect("缺少测试夹具 测试样例.wv");
        let bytes = std::fs::read(path).expect("读夹具");
        let mine = decode_stream(&bytes).expect("自研应解码");
        let theirs = wavicle::decode_stream(&bytes).expect("wavicle 应解码");
        assert_eq!(mine.channels, theirs.channels);
        assert_eq!(mine.sample_rate, theirs.sample_rate);
        assert_eq!(mine.bits_per_sample, theirs.bits_per_sample);
        assert_eq!(mine.is_float, theirs.is_float);
        assert!(!mine.samples.is_empty(), "夹具应有样本");
        assert_eq!(mine.samples, theirs.samples, "样本位级不一致");
    }
}
