//! # WAV（RIFF）自研解码后端
//!
//! 自研 WAV 解码后端：接管 `.wav` 的解码路径。
//! 纯 std 手写、零新增依赖、流式读文件（File + 游标，不整段入内存）。
//!
//! 支持面：PCM 整型 8/16/24/32 位（8 位为无符号、其余有符号小端）+
//! IEEE 浮点 32/64 位 + WAVE_FORMAT_EXTENSIBLE（样本 MSB 对齐，
//! 按容器位深归一化）。fmt 之外的 chunk（LIST / fact 等）一律跳过，
//! chunk 尺寸按 RIFF 规则向偶数对齐。不支持的编码形态（ADPCM、
//! µ-law 等）返回 [`DecodeError::Unsupported`]，由 `open_backend`
//! 回退 symphonia——接管不以牺牲存量可播文件为代价。
//!
//! 标签取舍：WAV 的 LIST-INFO 标签探测仍走 symphonia（`probe_metadata`
//! 不动，元数据零退化）；本模块只管解码与采样率直通探测。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// 单次解码输出的最大帧数（与 WavPack 后端同口径）。
const CHUNK_FRAMES: usize = 4096;

/// 样本编码形态（由 fmt chunk 的格式标签 + 位深决定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SampleFormat {
    /// PCM 无符号 8 位（0-255，128 为零点）。
    PcmU8,
    /// PCM 有符号整型：16/24/32 位小端（24 位读 3 字节符号扩展）。
    PcmInt { bytes: u32 },
    /// IEEE 浮点：32/64 位（值域已是 -1.0~1.0 标称，原样透传）。
    IeeeFloat { bytes: u32 },
}

/// 解析出的 WAV 头部事实（fmt + data 两 chunk 的最小集合）。
struct WavHeader {
    format: SampleFormat,
    channels: u16,
    sample_rate: u32,
    /// 报告用位深（UI 技术参数行显示）。
    bits_per_sample: u32,
    /// 每帧字节数（所有通道一个采样点的总字节，含尾部填充）。
    block_align: u16,
    /// 每样本字节数（由编码形态决定）。
    bytes_per_sample: u32,
    /// data chunk 数据区起点（文件偏移）。
    data_start: u64,
    /// data 数据区长度（字节）。
    data_len: u64,
}

/// 按扩展名或 RIFF/WAVE 魔数判定 WAV 文件（魔数兜底改名文件）。
pub(crate) fn is_wav_path(path: &Path) -> bool {
    let by_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    if by_ext {
        return true;
    }
    // 魔数兜底：「RIFF????WAVE」（12 字节）。
    let mut buf = [0u8; 12];
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    &buf[0..4] == b"RIFF" && &buf[8..12] == b"WAVE"
}

/// 轻量采样率探测（原生采样率直通用）：只解析到 fmt chunk。
/// 失败返回 None，调用方按「不直通」降级处理，不影响播放。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    let mut file = File::open(path).ok()?;
    parse_header(&mut file).ok().map(|h| h.sample_rate)
}

/// 从文件头解析 RIFF 结构，定位 fmt 与 data 两 chunk。
fn parse_header(file: &mut File) -> Result<WavHeader, DecodeError> {
    file.seek(SeekFrom::Start(0))?;
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return Err(DecodeError::Unsupported("不是 RIFF/WAVE 文件".into()));
    }

    let mut fmt: Option<(u16, u16, u32, u16, u16, Vec<u8>)> = None; // (tag, ch, sr, align, bits, 原始)
    let mut data: Option<(u64, u64)> = None; // (起点, 长度)
    loop {
        let mut head = [0u8; 8];
        match file.read_exact(&mut head) {
            Ok(()) => {}
            // 正常走到文件尾（data 已定位）：跳出；data 未定位：视为缺 data。
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(DecodeError::Io(e.to_string())),
        }
        let id = &head[0..4];
        let size = u32::from_le_bytes(head[4..8].try_into().expect("4 字节定长")) as u64;
        if id == b"fmt " {
            if size < 16 {
                return Err(DecodeError::Decode("fmt chunk 过短".into()));
            }
            // fmt chunk 合法长度上限：标准 16 字节，WAVE_FORMAT_EXTENSIBLE
            // 40 字节。声明数百 MB / 4 GiB 的 fmt chunk 属于伪造或损坏文件，
            // 先验证再分配（防「先分配后校验」的无界分配 DoS）。
            if size > 64 {
                return Err(DecodeError::Unsupported(format!(
                    "fmt chunk 异常大（{size} 字节，合法上限 64）"
                )));
            }
            let mut buf = vec![0u8; size as usize];
            file.read_exact(&mut buf)?;
            let tag = u16::from_le_bytes([buf[0], buf[1]]);
            let channels = u16::from_le_bytes([buf[2], buf[3]]);
            let sample_rate = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
            let block_align = u16::from_le_bytes([buf[12], buf[13]]);
            let bits = u16::from_le_bytes([buf[14], buf[15]]);
            fmt = Some((tag, channels, sample_rate, block_align, bits, buf));
        } else if id == b"data" {
            let start = file.stream_position()?;
            data = Some((start, size));
            // 跳过数据区（含奇数字节的偶数对齐填充）。
            let skip = size + (size & 1);
            if file.seek(SeekFrom::Current(skip as i64)).is_err() {
                break;
            }
        } else {
            // 未知 chunk 跳过（尺寸 + 奇偶对齐填充）。
            let skip = size + (size & 1);
            if file.seek(SeekFrom::Current(skip as i64)).is_err() {
                break;
            }
        }
        if fmt.is_some() && data.is_some() {
            break;
        }
    }

    let (tag, channels, sample_rate, block_align, bits, fmt_buf) =
        fmt.ok_or_else(|| DecodeError::Unsupported("缺 fmt chunk".into()))?;
    let (data_start, data_len) =
        data.ok_or_else(|| DecodeError::Unsupported("缺 data chunk".into()))?;

    // WAVE_FORMAT_EXTENSIBLE：真实格式在 SubFormat GUID 前 2 字节。
    let effective_tag = if tag == 0xFFFE && fmt_buf.len() >= 40 {
        u16::from_le_bytes([fmt_buf[24], fmt_buf[25]])
    } else {
        tag
    };
    let format = match (effective_tag, bits) {
        (1, 8) => SampleFormat::PcmU8,
        (1, 16) => SampleFormat::PcmInt { bytes: 2 },
        (1, 24) => SampleFormat::PcmInt { bytes: 3 },
        (1, 32) => SampleFormat::PcmInt { bytes: 4 },
        (3, 32) => SampleFormat::IeeeFloat { bytes: 4 },
        (3, 64) => SampleFormat::IeeeFloat { bytes: 8 },
        _ => {
            return Err(DecodeError::Unsupported(format!(
                "不支持的 WAV 编码形态（格式标签 {effective_tag} / 位深 {bits}）"
            )))
        }
    };
    if channels == 0 {
        return Err(DecodeError::Decode("通道数为 0".into()));
    }
    if sample_rate == 0 {
        return Err(DecodeError::Decode("采样率为 0".into()));
    }
    let bytes_per_sample = match format {
        SampleFormat::PcmU8 => 1,
        SampleFormat::PcmInt { bytes } | SampleFormat::IeeeFloat { bytes } => bytes,
    };
    // 帧内至少装得下全部通道的样本；多出的尾部字节（填充）读取时跳过。
    if (block_align as u32) < channels as u32 * bytes_per_sample {
        return Err(DecodeError::Decode(
            "block_align 与通道数/位深不符（头部损坏）".into(),
        ));
    }
    Ok(WavHeader {
        format,
        channels,
        sample_rate,
        bits_per_sample: bits as u32,
        block_align,
        bytes_per_sample,
        data_start,
        data_len,
    })
}

/// 把一个样本的字节（小端）转换为归一化 f32。
#[inline]
fn convert_sample(format: SampleFormat, b: &[u8]) -> f32 {
    match format {
        SampleFormat::PcmU8 => (b[0] as i32 - 128) as f32 / 128.0,
        SampleFormat::PcmInt { bytes: 2 } => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
        SampleFormat::PcmInt { bytes: 3 } => {
            // 24 位：3 字节装入 i32 低三位，第 4 字节按符号位填充（符号扩展）。
            let sign = if b[2] & 0x80 != 0 { 0xFF } else { 0x00 };
            let v = i32::from_le_bytes([b[0], b[1], b[2], sign]);
            v as f32 / 8_388_608.0
        }
        SampleFormat::PcmInt { bytes: 4 } => {
            i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0
        }
        SampleFormat::IeeeFloat { bytes: 4 } => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        SampleFormat::IeeeFloat { bytes: 8 } => {
            f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32
        }
        _ => 0.0, // 不可达：构造期已限定形态
    }
}

/// WAV 解码后端：流式读取 data chunk，按块产出交错 f32 样本。
pub(crate) struct WavBackend {
    params: AudioParams,
    file: File,
    format: SampleFormat,
    channels: u16,
    block_align: u16,
    bytes_per_sample: u32,
    data_start: u64,
    /// 数据区终点（文件偏移，不含）。
    data_end: u64,
    /// 当前读位置（文件偏移，恒按 block_align 对齐）。
    cursor: u64,
}

impl WavBackend {
    /// 打开并解析 WAV 文件；不支持的编码形态返回
    /// [`DecodeError::Unsupported`]（供上层回退 symphonia）。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let mut file = File::open(path).map_err(DecodeError::from)?;
        let h = parse_header(&mut file)?;
        let total_frames = h.data_len / h.block_align as u64;
        let duration = Some(total_frames as f64 / f64::from(h.sample_rate));
        let params = AudioParams::new(
            Some(h.sample_rate),
            Some(h.channels),
            Some(h.bits_per_sample),
            "PCM".into(),
            0,
            duration,
        );
        // 读游标配到数据区起点。
        file.seek(SeekFrom::Start(h.data_start))?;
        Ok(Self {
            params,
            file,
            format: h.format,
            channels: h.channels,
            block_align: h.block_align,
            bytes_per_sample: h.bytes_per_sample,
            data_start: h.data_start,
            data_end: h.data_start + h.data_len,
            cursor: h.data_start,
        })
    }
}

impl DecoderBackend for WavBackend {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    /// 按块（≤4096 帧）读取并转换；读游标即文件游标，天然续读。
    /// 截断文件宽容处理：读到多少算多少，下次调用返回 EOF。
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        let align = self.block_align as u64;
        let remaining = self.data_end.saturating_sub(self.cursor);
        if remaining < align {
            return Ok(None); // 不足一帧即 EOF
        }
        let frames = ((remaining / align) as usize).min(CHUNK_FRAMES);
        let want = frames * align as usize;
        let mut buf = vec![0u8; want];
        // 宽容截断：循环读满或读尽为止，按实际字节数换算帧数。
        let mut got = 0usize;
        while got < want {
            match self.file.read(&mut buf[got..]) {
                Ok(0) => break, // 文件尾
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(DecodeError::Io(e.to_string())),
            }
        }
        let real_frames = got / align as usize;
        if real_frames == 0 {
            self.cursor = self.data_end;
            return Ok(None);
        }
        self.cursor += (real_frames as u64) * align;

        let ch = self.channels as usize;
        let bps = self.bytes_per_sample as usize;
        let mut out = Vec::with_capacity(real_frames * ch);
        for frame in buf[..real_frames * align as usize].chunks_exact(align as usize) {
            for c in 0..ch {
                let off = c * bps;
                out.push(convert_sample(self.format, &frame[off..off + bps]));
            }
        }
        Ok(Some(out))
    }

    /// 按秒定位：向下取整到帧边界（落点不晚于请求值），越界钳到末尾
    ///（随后 decode_next 自然 EOF）。下游缓冲清理由音频回调侧负责。
    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek(format!("非法 seek 秒数：{secs}")));
        }
        let align = self.block_align as u64;
        let sr = self.params.sample_rate.unwrap_or(0);
        if sr == 0 {
            return Err(DecodeError::Seek("采样率未知，无法按秒定位".into()));
        }
        let max_frames = (self.data_end - self.data_start) / align;
        let frame = ((secs * f64::from(sr)) as u64).min(max_frames);
        self.cursor = self.data_start + frame * align;
        self.file.seek(SeekFrom::Start(self.cursor))?;
        Ok(secs)
    }
}

// =============================================================================
// 单元测试
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    /// 合成 WAV 文件字节：RIFF + fmt + data。
    fn make_wav(tag: u16, channels: u16, sample_rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let bps = (bits / 8) as u32;
        let block_align = (channels as u32 * bps) as u16;
        let byte_rate = sample_rate * block_align as u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&tag.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    /// 合成 WAVE_FORMAT_EXTENSIBLE 头（40 字节 fmt）。
    fn make_wav_extensible(
        sub_tag: u16,
        channels: u16,
        sample_rate: u32,
        container_bits: u16,
        valid_bits: u16,
        data: &[u8],
    ) -> Vec<u8> {
        let bps = (container_bits / 8) as u32;
        let block_align = (channels as u32 * bps) as u16;
        let byte_rate = sample_rate * block_align as u32;
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&0xFFFEu16.to_le_bytes());
        fmt.extend_from_slice(&channels.to_le_bytes());
        fmt.extend_from_slice(&sample_rate.to_le_bytes());
        fmt.extend_from_slice(&byte_rate.to_le_bytes());
        fmt.extend_from_slice(&block_align.to_le_bytes());
        fmt.extend_from_slice(&container_bits.to_le_bytes());
        fmt.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        fmt.extend_from_slice(&valid_bits.to_le_bytes());
        fmt.extend_from_slice(&0u32.to_le_bytes()); // channel mask
        fmt.extend_from_slice(&sub_tag.to_le_bytes()); // SubFormat GUID 前 2 字节
        fmt.extend_from_slice(&[0u8; 14]); // GUID 其余字节（定值不校验）
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(4u32 + 8 + 40 + 8 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&fmt);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    /// 写临时文件，返路径（测试间不冲突）。
    fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("tuneux-wav-test-{}-{seq}.wav", std::process::id()));
        let mut f = File::create(&path).expect("应能创建临时文件");
        f.write_all(bytes).expect("应能写入临时文件");
        path
    }

    /// 收集后端全部输出样本。
    fn decode_all(b: &mut WavBackend) -> Vec<f32> {
        let mut out = Vec::new();
        while let Ok(Some(chunk)) = b.decode_next() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// 16 位立体声：参数 / 样本值 / EOF 全链路。
    #[test]
    fn pcm16_stereo_roundtrip() {
        let samples: Vec<i16> = vec![0, 1000, -1000, 32767, -32768, 12345];
        let mut data = Vec::new();
        for s in &samples {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let path = write_temp(&make_wav(1, 2, 44100, 16, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        assert_eq!(b.params().sample_rate, Some(44100));
        assert_eq!(b.params().channels, Some(2));
        assert_eq!(b.params().bits_per_sample, Some(16));
        assert_eq!(b.params().codec_name, "PCM");
        // 6 样本 = 3 帧 → duration = 3/44100。
        let d = b.params().duration.expect("应有时长");
        assert!((d - 3.0 / 44100.0).abs() < 1e-9, "duration 实际 {d}");
        let out = decode_all(&mut b);
        assert_eq!(out.len(), samples.len());
        for (o, s) in out.iter().zip(&samples) {
            assert_eq!(*o, *s as f32 / 32768.0, "样本值不符");
        }
        let _ = std::fs::remove_file(&path);
    }

    /// 8 位无符号：128 为零点。
    #[test]
    fn pcm8_unsigned_zero_center() {
        let data = vec![0u8, 128, 255];
        let path = write_temp(&make_wav(1, 1, 8000, 8, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out, vec![-1.0, 0.0, 127.0 / 128.0]);
        let _ = std::fs::remove_file(&path);
    }

    /// 24 位符号扩展：最小负值与最大正值。
    #[test]
    fn pcm24_sign_extension() {
        // 0x800000（最小）与 0x7FFFFF（最大）。
        let data = vec![0x00u8, 0x00, 0x80, 0xFF, 0xFF, 0x7F];
        let path = write_temp(&make_wav(1, 1, 48000, 24, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 8_388_607.0 / 8_388_608.0);
        let _ = std::fs::remove_file(&path);
    }

    /// 32 位整型与 32/64 位浮点。
    #[test]
    fn pcm32_and_floats() {
        let mut data = Vec::new();
        data.extend_from_slice(&i32::MIN.to_le_bytes());
        data.extend_from_slice(&i32::MAX.to_le_bytes());
        let path = write_temp(&make_wav(1, 1, 44100, 32, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out[0], -1.0);
        assert_eq!(out[1], 2_147_483_647.0 / 2_147_483_648.0);
        let _ = std::fs::remove_file(&path);

        let mut data = Vec::new();
        data.extend_from_slice(&0.5f32.to_le_bytes());
        data.extend_from_slice(&(-0.25f32).to_le_bytes());
        let path = write_temp(&make_wav(3, 1, 44100, 32, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out, vec![0.5, -0.25]);
        let _ = std::fs::remove_file(&path);

        let mut data = Vec::new();
        data.extend_from_slice(&0.75f64.to_le_bytes());
        let path = write_temp(&make_wav(3, 1, 44100, 64, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out, vec![0.75]);
        let _ = std::fs::remove_file(&path);
    }

    /// EXTENSIBLE 24-in-32（MSB 对齐）：按容器 32 位读、数值左对齐。
    #[test]
    fn extensible_24_in_32_msb_aligned() {
        // 24 位值 0x400000（= 0.5）左对齐存入 32 位容器。
        let mut data = Vec::new();
        data.extend_from_slice(&0x4000_0000i32.to_le_bytes());
        let path = write_temp(&make_wav_extensible(1, 1, 44100, 32, 24, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out, vec![0.5]);
        let _ = std::fs::remove_file(&path);
    }

    /// 畸形与边界：非 RIFF / 通道 0 / 缺 data / 不支持编码。
    #[test]
    fn malformed_headers_rejected() {
        let path = write_temp(b"NOPE________");
        assert!(matches!(
            WavBackend::open(&path),
            Err(DecodeError::Unsupported(_))
        ));
        let _ = std::fs::remove_file(&path);

        // 通道数 0。
        let path = write_temp(&make_wav(1, 0, 44100, 16, &[0, 0]));
        assert!(WavBackend::open(&path).is_err());
        let _ = std::fs::remove_file(&path);

        // ADPCM（tag=2）：Unsupported（供 open_backend 回退）。
        let path = write_temp(&make_wav(2, 1, 44100, 4, &[0u8; 16]));
        assert!(matches!(
            WavBackend::open(&path),
            Err(DecodeError::Unsupported(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    /// 截断 data：读到断点为止，随后 EOF，不报错。
    #[test]
    fn truncated_data_is_tolerant() {
        let mut bytes = make_wav(1, 1, 8000, 16, &[0x34, 0x12, 0x78, 0x56]);
        bytes.truncate(bytes.len() - 1); // 砍掉最后 1 字节 → 末帧残缺
        let path = write_temp(&bytes);
        let mut b = WavBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        // 只剩 1 个完整帧（2 字节）。
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], 0x1234 as f32 / 32768.0);
        let _ = std::fs::remove_file(&path);
    }

    /// seek：落点帧对齐、负值/NaN 拒绝、越界钳到 EOF。
    #[test]
    fn seek_lands_on_frame_boundary() {
        // 10 帧单声道 10Hz：帧 i 的样本值 = i。
        let mut data = Vec::new();
        for i in 0..10i16 {
            data.extend_from_slice(&(i * 1000).to_le_bytes());
        }
        let path = write_temp(&make_wav(1, 1, 10, 16, &data));
        let mut b = WavBackend::open(&path).expect("应能打开");
        // seek 0.55s → 帧 5（向下取整）。
        b.seek(0.55).expect("seek 应成功");
        let out = decode_all(&mut b);
        assert_eq!(out[0], 5000.0 / 32768.0);
        assert_eq!(out.len(), 5);
        // 非法输入。
        assert!(matches!(b.seek(-1.0), Err(DecodeError::Seek(_))));
        assert!(matches!(b.seek(f64::NAN), Err(DecodeError::Seek(_))));
        // 越界 → EOF。
        b.seek(999.0).expect("越界 seek 应钳到末尾");
        assert!(b.decode_next().expect("读").is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// 直通探测与路径判定。
    #[test]
    fn probe_and_path_detection() {
        let path = write_temp(&make_wav(1, 2, 48000, 16, &[0, 0]));
        assert_eq!(probe_sample_rate(&path), Some(48000));
        assert!(is_wav_path(&path));
        // 魔数兜底：无 .wav 扩展名的同名文件。
        let bytes = make_wav(1, 1, 44100, 16, &[0, 0]);
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let noext =
            std::env::temp_dir().join(format!("tuneux-wav-test-{}-{seq}.bin", std::process::id()));
        let mut f = File::create(&noext).expect("建");
        f.write_all(&bytes).expect("写");
        drop(f);
        assert!(is_wav_path(&noext));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&noext);
    }

    /// 与 symphonia 对拍：同一文件两后端全量解码，逐样本位级一致。
    ///（对拍是「自研输出可信任」的核心证据。）
    #[test]
    fn bit_exact_against_symphonia() {
        // 混合波形的 16 位立体声（覆盖 0 / 正 / 负 / 极值）。
        let raw: Vec<i16> = (0..5000)
            .map(|i| ((i * 7919) % 65536 - 32768) as i16)
            .collect();
        let mut data = Vec::new();
        for s in &raw {
            data.extend_from_slice(&s.to_le_bytes());
        }
        let path = write_temp(&make_wav(1, 2, 44100, 16, &data));

        let mut mine = WavBackend::open(&path).expect("自研应打开");
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

    /// 24 位对拍：符号扩展修复后与 symphonia 位级一致（防回归）。
    #[test]
    fn bit_exact_against_symphonia_24bit() {
        // 覆盖 0 / 正负小值 / 极值。
        let raw: Vec<i32> = vec![0, 1, -1, 0x7FFFFF, -8388608, 0x123456, -0x123456];
        let mut data = Vec::new();
        for s in &raw {
            data.extend_from_slice(&s.to_le_bytes()[0..3]);
        }
        let path = write_temp(&make_wav(1, 1, 48000, 24, &data));
        let mut mine = WavBackend::open(&path).expect("自研应打开");
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

    /// 真实语料对拍：测试音频/test_tone.wav（352KB 真实 RIFF PCM），
    /// 自研与 symphonia 全量逐样本位级一致 + probe 采样率正确。
    #[test]
    #[ignore = "需要 测试音频/test_tone.wav 测试夹具；运行：cargo test -- --ignored"]
    fn bit_exact_real_world_tone() {
        // 相对路径兼容两种运行 cwd（workspace 根 / crate 根）。
        let candidates = ["测试音频/test_tone.wav", "../../测试音频/test_tone.wav"];
        let path = candidates
            .iter()
            .map(std::path::Path::new)
            .find(|p| p.exists())
            .expect("缺少测试夹具 test_tone.wav");
        assert!(is_wav_path(path), "夹具应被识别为 WAV");
        assert_eq!(probe_sample_rate(path), Some(44100), "夹具采样率 44100");
        let mut mine = WavBackend::open(path).expect("自研应打开");
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
        for (i, (a, b)) in mine_out.iter().zip(theirs_out.iter()).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "第 {i} 个样本不一致：{a} vs {b}");
        }
    }
}
