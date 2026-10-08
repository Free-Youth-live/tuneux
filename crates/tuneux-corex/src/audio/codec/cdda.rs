//! # CD-DA 镜像（.bin）自研解码后端
//!
//! 光盘媒体扩展（镜像进程内路线）：把光盘镜像 `.bin`
//! 当作「可 seek 的原始 PCM 文件」直接解码——结构即 wav.rs 去 RIFF 头版：
//! 固定 CD 规格（44.1kHz / 16bit / 立体声交错、小端），流式按扇区读取，
//! 不整段入内存。
//!
//! 支持面：
//! - 扇区尺寸探测：2352（纯音频）/ 2448（DAO-96 含 96 字节子通道，
//!   读时剥离尾部）——按 `文件大小 % 扇区尺寸 == 0` 判定，两者皆可时取 2352；
//! - 构造泛型 `R: Read + Seek`：[`CddaBackend::open`] 走文件，
//!   [`CddaBackend::from_reader`] 走内存源（如 `Cursor<Vec<u8>>`）——
//!   「PCM 已在内存」场景由此零成本接入，无需 PCM 命令通道；
//! - seek 按扇区对齐（PCM 天然帧精确）。
//!
//! 入口约定：`.bin` 不进 `KNOWN_AUDIO_EXTS`（浏览器不列出无关 .bin）；
//! 消费方经 cue 引导（FILE 引用）或直接以路径 / 区间命令打开。
//! 物理光驱不在本模块范围（进程外工具路线，另案）。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// CD-DA 规格常量。
pub const SECTOR_AUDIO: usize = 2352; // 每扇区音频字节数
const SAMPLES_PER_SECTOR: usize = 588; // 每扇区采样帧数（2352 / 4）
const SAMPLE_RATE: u32 = 44100;
const CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: u32 = 16;
/// DAO-96 形态：每扇区尾部 96 字节子通道。
const SUBCHANNEL_SIZE: usize = 96;
const SECTOR_RAW: usize = SECTOR_AUDIO + SUBCHANNEL_SIZE; // 2448
/// 单次解码输出的最大帧数（与其他后端同口径）。
const CHUNK_FRAMES: usize = 4096;

/// 按扩展名 + 尺寸探测判定 `.bin` 镜像（扩展名不符直接否；
/// 尺寸须被 2352 或 2448 整除，否则放行给后续后端）。
pub(crate) fn is_bin_path(path: &Path) -> bool {
    let by_ext = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("bin"));
    if !by_ext {
        return false;
    }
    detect_sector_size(path).is_some()
}

/// CD-DA 同步模式：每个扇区开头的 12 字节固定模式（00 FF×10 00）。
/// 用于区分 2352 与 2448 形态——纯音频扇区第一个字节是 00，
/// 而 DAO-96 镜像紧跟子通道数据。
const CD_SYNC: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];

/// 探测扇区尺寸：文件大小整除判定 + 首扇区同步模式二次确认。
///
/// 单纯用整除有互素陷阱：2448/2352 = 51/49 互素，扇区数为 49 的倍数时
/// 两种尺寸都整除——旧实现先判 2352 即误判（DAO-96 镜像按 2352 步进读
/// 会把 96 字节子通道当 48 帧音频输出 = 整轨爆音）。
/// 修复：整除可判定时，再看第二扇区起始位置是否有 CD 同步模式——
/// 2352 步进读到同步模式说明真的是 2352；读不到说明实际步进是 2448。
fn detect_sector_size(path: &Path) -> Option<usize> {
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 {
        return None;
    }
    let by_2352 = len % SECTOR_AUDIO as u64 == 0;
    let by_2448 = len % SECTOR_RAW as u64 == 0;
    match (by_2352, by_2448) {
        (true, false) => Some(SECTOR_AUDIO),
        (false, true) => Some(SECTOR_RAW),
        (true, true) => {
            // 两者皆整除（扇区数是 49 的倍数）：读第二扇区起始位置确认。
            // 2352 形态第二扇区从 offset 2352 开始，应看到 CD 同步模式；
            // 2448 形态第二扇区从 offset 2448 开始，offset 2352 处是
            // 第一扇区的子通道数据（非同步模式）。
            let mut buf = [0u8; CD_SYNC.len()];
            let Ok(mut f) = std::fs::File::open(path) else {
                return Some(SECTOR_AUDIO); // 保守：打不开按 2352
            };
            let Ok(_) = f
                .seek(std::io::SeekFrom::Start(SECTOR_AUDIO as u64))
                .and_then(|_| f.read_exact(&mut buf))
            else {
                return Some(SECTOR_AUDIO);
            };
            if buf == CD_SYNC {
                Some(SECTOR_AUDIO)
            } else {
                Some(SECTOR_RAW)
            }
        }
        (false, false) => None,
    }
}

/// 轻量采样率探测（原生采样率直通用）：CD 规格恒 44.1kHz。
/// 非 `.bin` 或尺寸不合法返回 None（不直通降级）。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    if is_bin_path(path) {
        Some(SAMPLE_RATE)
    } else {
        None
    }
}

/// CD-DA 镜像解码后端：流式按扇区读取，i16LE 交错 → f32 输出。
pub(crate) struct CddaBackend<R: Read + Seek> {
    params: AudioParams,
    reader: R,
    /// 磁盘扇区步进（2352 或 2448；2448 时只读前 2352 音频字节）。
    sector_step: u64,
    total_sectors: u64,
    /// 已消费的帧游标（帧级记账：块边界非扇区对齐时账实一致——
    /// 扇区级取整会在文件尾部多记消费、少算剩余，丢最多一扇区的帧）。
    /// seek 保证落在扇区边界（扇区对齐语义不变）；2352 形态块内推进
    /// 可为任意帧数。
    cursor: u64,
}

impl<R: Read + Seek> CddaBackend<R> {
    /// 总帧数（= 扇区数 × 588）。
    fn total_frames(&self) -> u64 {
        self.total_sectors * SAMPLES_PER_SECTOR as u64
    }
}

impl CddaBackend<File> {
    /// 从 `.bin` 文件打开（2352/2448 探测；尺寸非法返回 Unsupported）。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let Some(sector_step) = detect_sector_size(path) else {
            return Err(DecodeError::Unsupported(
                "不是合法的 .bin 镜像（尺寸不被 2352/2448 整除）".into(),
            ));
        };
        let file = File::open(path).map_err(DecodeError::from)?;
        let len = file.metadata()?.len();
        Self::from_parts(file, sector_step, len / sector_step as u64)
    }
}

impl<R: Read + Seek + Send> CddaBackend<R> {
    /// 从任意可 seek 源构造（内存镜像用 `Cursor<Vec<u8>>`；
    /// 扇区尺寸须显式给出——内存源无文件大小可探）。
    /// 「PCM 已在内存」场景的入口（当前仅测试消费）。
    #[allow(dead_code)]
    pub(crate) fn from_reader(
        reader: R,
        sector_step: usize,
        total_sectors: u64,
    ) -> Result<Self, DecodeError> {
        Self::from_parts(reader, sector_step, total_sectors)
    }

    fn from_parts(reader: R, sector_step: usize, total_sectors: u64) -> Result<Self, DecodeError> {
        if !matches!(sector_step, SECTOR_AUDIO | SECTOR_RAW) {
            return Err(DecodeError::Unsupported(format!(
                "非法扇区尺寸 {sector_step}（仅支持 2352/2448）"
            )));
        }
        if total_sectors == 0 {
            return Err(DecodeError::Decode(".bin 镜像为空（0 扇区）".into()));
        }
        let duration = Some(total_sectors as f64 / 75.0); // 75 扇区/秒
        let params = AudioParams::new(
            Some(SAMPLE_RATE),
            Some(CHANNELS),
            Some(BITS_PER_SAMPLE),
            "CDDA".into(),
            0,
            duration,
        );
        let mut b = Self {
            params,
            reader,
            sector_step: sector_step as u64,
            total_sectors,
            cursor: 0,
        };
        b.reader.seek(SeekFrom::Start(0))?;
        Ok(b)
    }
}

impl<R: Read + Seek + Send> DecoderBackend for CddaBackend<R> {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    /// 按块（≤4096 帧）读取；2448 形态逐扇区剥离尾部 96 字节子通道。
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        if self.cursor >= self.total_frames() {
            return Ok(None);
        }
        let frames_left = (self.total_frames() - self.cursor) as usize;
        let frames = frames_left.min(CHUNK_FRAMES);
        let mut out = Vec::with_capacity(frames * CHANNELS as usize);

        if self.sector_step == SECTOR_AUDIO as u64 {
            // 纯音频形态：整块连读（每帧 4 字节 = 2 声道 × 16 位）。
            let want = frames * 4;
            let mut buf = vec![0u8; want];
            let mut got = 0usize;
            while got < want {
                match self.reader.read(&mut buf[got..]) {
                    Ok(0) => break,
                    Ok(n) => got += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(DecodeError::Io(e.to_string())),
                }
            }
            let real_frames = got / 4;
            if real_frames == 0 {
                self.cursor = self.total_frames();
                return Ok(None);
            }
            self.cursor += real_frames as u64;
            for b in buf[..real_frames * 4].as_chunks::<2>().0 {
                out.push(i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0);
            }
        } else {
            // DAO-96 形态：逐扇区读 2352，**读后立即**跳过尾部 96 字节子通道。
            // 块粒度取整扇区（CHUNK_FRAMES/588 = 6 扇区/块，末块可不足）：
            // 块内永不截断半扇区，且任何退出路径都不会漏跳子通道——
            // 修复旧实现「块边界落在扇区中间时先 break 后跳子通道」导致的
            // 后续块整体错位 96 字节与丢帧（回归：2448 多块对拍测试）。
            let sectors_left =
                ((self.total_frames() - self.cursor) / SAMPLES_PER_SECTOR as u64) as usize;
            let sectors = sectors_left.min(CHUNK_FRAMES / SAMPLES_PER_SECTOR);
            let mut buf = vec![0u8; SECTOR_AUDIO];
            let mut sectors_read = 0u64;
            while (sectors_read as usize) < sectors {
                let mut got = 0usize;
                while got < SECTOR_AUDIO {
                    match self.reader.read(&mut buf[got..]) {
                        Ok(0) => break,
                        Ok(n) => got += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(DecodeError::Io(e.to_string())),
                    }
                }
                if got < SECTOR_AUDIO {
                    break; // 截断的末扇区：丢弃（不足一扇区不产样本）
                }
                sectors_read += 1;
                // 子通道紧随本扇区音频之后跳过——后续无论从哪里退出，
                // 游标都停在扇区边界上。
                self.reader
                    .seek(SeekFrom::Current(SUBCHANNEL_SIZE as i64))?;
                for b in buf.as_chunks::<2>().0 {
                    out.push(i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0);
                }
            }
            self.cursor += sectors_read * SAMPLES_PER_SECTOR as u64;
            if out.is_empty() {
                self.cursor = self.total_frames();
                return Ok(None);
            }
        }
        Ok(Some(out))
    }

    /// 按秒定位：秒 → 扇区（75/秒，向下取整）→ 字节偏移，越界钳到末尾。
    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek(format!("非法 seek 秒数：{secs}")));
        }
        let sector = ((secs * 75.0) as u64).min(self.total_sectors);
        self.cursor = sector * SAMPLES_PER_SECTOR as u64;
        self.reader
            .seek(SeekFrom::Start(sector * self.sector_step))?;
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

    /// 写临时文件（指定扩展名）。
    fn write_temp(bytes: &[u8], ext: &str) -> std::path::PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "tuneux-cdda-test-{}-{seq}.{ext}",
            std::process::id()
        ));
        let mut f = File::create(&path).expect("应能创建临时文件");
        f.write_all(bytes).expect("应能写入临时文件");
        path
    }

    /// 生成已知 PCM：扇区 s 内帧 f 的样本值 = s*100+f（i16，两声道同值）。
    fn gen_pcm(sectors: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(sectors * SECTOR_AUDIO);
        for s in 0..sectors {
            for f in 0..SAMPLES_PER_SECTOR {
                let v = (s * 100 + f / 10) as i16;
                data.extend_from_slice(&v.to_le_bytes());
                data.extend_from_slice(&v.to_le_bytes());
            }
        }
        data
    }

    /// 同一 PCM 的 RIFF 包装（对拍用：WavBackend 读它）。
    fn wrap_riff(pcm: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36u32 + pcm.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&44100u32.to_le_bytes());
        v.extend_from_slice(&176400u32.to_le_bytes());
        v.extend_from_slice(&4u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        v.extend_from_slice(pcm);
        v
    }

    fn decode_all<B: DecoderBackend>(b: &mut B) -> Vec<f32> {
        let mut out = Vec::new();
        while let Ok(Some(chunk)) = b.decode_next() {
            out.extend_from_slice(&chunk);
        }
        out
    }

    /// 基本链路：参数 / 样本值 / EOF（2352 形态）。
    #[test]
    fn basic_2352_roundtrip() {
        let pcm = gen_pcm(4);
        let path = write_temp(&pcm, "bin");
        let mut b = CddaBackend::open(&path).expect("应能打开");
        assert_eq!(b.params().sample_rate, Some(44100));
        assert_eq!(b.params().channels, Some(2));
        assert_eq!(b.params().bits_per_sample, Some(16));
        assert_eq!(b.params().codec_name, "CDDA");
        let d = b.params().duration.expect("应有时长");
        assert!((d - 4.0 / 75.0).abs() < 1e-9, "duration 实际 {d}");
        let out = decode_all(&mut b);
        assert_eq!(out.len(), 4 * SAMPLES_PER_SECTOR * 2);
        // 扇区 1 帧 20 的左声道样本 = 100 + 2 = 102。
        let idx = (SAMPLES_PER_SECTOR + 20) * 2;
        assert_eq!(out[idx], 102.0 / 32768.0);
        assert_eq!(probe_sample_rate(&path), Some(44100));
        assert!(is_bin_path(&path));
        let _ = std::fs::remove_file(&path);
    }

    /// 2448（DAO-96）形态：探测正确、子通道被剥离、样本连续对齐。
    #[test]
    fn raw_2448_subchannel_stripped() {
        let pcm = gen_pcm(3);
        // 每 2352 音频字节后插 96 字节伪子通道（0xAA 填充）。
        let mut raw = Vec::with_capacity(3 * SECTOR_RAW);
        for s in 0..3 {
            raw.extend_from_slice(&pcm[s * SECTOR_AUDIO..(s + 1) * SECTOR_AUDIO]);
            raw.extend_from_slice(&[0xAAu8; SUBCHANNEL_SIZE]);
        }
        let path = write_temp(&raw, "bin");
        let mut b = CddaBackend::open(&path).expect("应能打开");
        let out = decode_all(&mut b);
        assert_eq!(out.len(), 3 * SAMPLES_PER_SECTOR * 2);
        // 末扇区末帧左声道 = 200 + 58 = 258。
        let last_l = out[out.len() - 2];
        assert_eq!(last_l, 258.0 / 32768.0);
        let _ = std::fs::remove_file(&path);
    }

    /// 2448 多块回归：同一 PCM 分别包成 2352 与 2448（含子通道）两种形态，
    /// 全量解码位级一致——块边界（每 6 扇区一块）上的错位或丢帧都会被
    /// 逐样本对拍撞出（回归旧缺陷：先 break 后跳子通道，后续块整体错位
    /// 96 字节且每块丢数十帧；旧测试仅 3 扇区单块，测不到边界）。
    #[test]
    fn raw_2448_multichunk_bit_exact_vs_2352() {
        let sectors = 15; // 三块：6 + 6 + 3
        let pcm = gen_pcm(sectors);
        // 2448 形态：每 2352 音频字节后插 96 字节伪子通道。
        let mut raw = Vec::with_capacity(sectors * SECTOR_RAW);
        for s in 0..sectors {
            raw.extend_from_slice(&pcm[s * SECTOR_AUDIO..(s + 1) * SECTOR_AUDIO]);
            raw.extend_from_slice(&[0xAAu8; SUBCHANNEL_SIZE]);
        }
        let bin_2352 = write_temp(&pcm, "bin");
        let bin_2448 = write_temp(&raw, "bin");
        let a = decode_all(&mut CddaBackend::open(&bin_2352).expect("2352 应打开"));
        let b = decode_all(&mut CddaBackend::open(&bin_2448).expect("2448 应打开"));
        assert_eq!(a.len(), sectors * SAMPLES_PER_SECTOR * 2, "2352 样本总数");
        assert_eq!(a.len(), b.len(), "两种形态样本总数应一致");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(
                x.to_bits(),
                y.to_bits(),
                "第 {i} 个样本不一致（块边界错位/丢帧）"
            );
        }
        let _ = std::fs::remove_file(&bin_2352);
        let _ = std::fs::remove_file(&bin_2448);
    }

    /// 与 WavBackend 对拍：同一 PCM 裸流（.bin）与 RIFF 包装（.wav）
    /// 双后端全量解码位级一致。
    #[test]
    fn bit_exact_against_wav_backend() {
        let pcm = gen_pcm(6);
        let bin_path = write_temp(&pcm, "bin");
        let wav_path = write_temp(&wrap_riff(&pcm), "wav");
        let mut cdda = CddaBackend::open(&bin_path).expect("cdda 应打开");
        let mut wav = super::super::wav::WavBackend::open(&wav_path).expect("wav 应打开");
        let a = decode_all(&mut cdda);
        let b = decode_all(&mut wav);
        assert_eq!(a.len(), b.len(), "样本总数不一致");
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "第 {i} 个样本不一致：{x} vs {y}");
        }
        let _ = std::fs::remove_file(&bin_path);
        let _ = std::fs::remove_file(&wav_path);
    }

    /// from_reader：内存镜像（Cursor）同数据同输出。
    #[test]
    fn from_reader_cursor_source() {
        let pcm = gen_pcm(2);
        let cursor = std::io::Cursor::new(pcm.clone());
        let mut b = CddaBackend::from_reader(cursor, SECTOR_AUDIO, 2).expect("应能构造");
        let out = decode_all(&mut b);
        assert_eq!(out.len(), 2 * SAMPLES_PER_SECTOR * 2);
        // 非法扇区尺寸拒绝。
        assert!(CddaBackend::from_reader(std::io::Cursor::new(pcm), 2400, 2).is_err());
    }

    /// seek：扇区对齐落点、越界钳 EOF、非法输入拒绝。
    #[test]
    fn seek_sector_aligned() {
        let pcm = gen_pcm(75); // 1 秒
        let path = write_temp(&pcm, "bin");
        let mut b = CddaBackend::open(&path).expect("应能打开");
        // seek 0.5s → 扇区 37（向下取整）；首样本 = 扇区 37 帧 0 = 3700。
        b.seek(0.5).expect("seek 应成功");
        let out = decode_all(&mut b);
        assert_eq!(out[0], 3700.0 / 32768.0);
        assert!(matches!(b.seek(-1.0), Err(DecodeError::Seek(_))));
        assert!(matches!(b.seek(f64::NAN), Err(DecodeError::Seek(_))));
        b.seek(999.0).expect("越界 seek 应钳到末尾");
        assert!(b.decode_next().expect("读").is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// 尺寸非法的 .bin：is_bin_path 否、open 报 Unsupported（放行给后续后端）。
    #[test]
    fn garbage_bin_rejected() {
        let path = write_temp(&[0u8; 1000], "bin"); // 1000 不被 2352/2448 整除
        assert!(!is_bin_path(&path));
        assert!(matches!(
            CddaBackend::open(&path),
            Err(DecodeError::Unsupported(_))
        ));
        assert_eq!(probe_sample_rate(&path), None);
        let _ = std::fs::remove_file(&path);
    }
}
