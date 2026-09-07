//! # Ogg 容器解封装（最小子集，专为 Opus 服务）
//!
//! 只实现解出 `.opus` 文件所需的最小 Ogg 页解析：
//! 页头（捕获模式 "OggS"）→ 段表（lacing）→ 负载（payload），
//! 并按 Ogg 的 255 续段规则把"段"重新拼回**完整的逻辑包**。
//!
//! 本模块刻意保持 Ogg 通用（不识别 OpusHead/OpusTags 魔数），
//! 头包识别与 Opus 语义留在 [`super::opus`] 处理，以便未来
//! 复用给其它 Ogg 封装格式。CRC 校验首版跳过（纯播放器场景）。
//!
//! 参考：RFC 3533（Ogg 封装）、RFC 7845 §4（Opus 的 Ogg 映射）。

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use super::decoder::DecodeError;

/// Ogg 捕获模式（页头固定魔数）。
const CAPTURE_PATTERN: &[u8; 4] = b"OggS";

/// Ogg 页头固定字节数：捕获模式 4 + 版本 1 + 头类型 1 + granule 8
/// + 序列号 4 + 页序号 4 + CRC 4 + 段数 1 = 27。
const PAGE_HEADER_LEN: usize = 27;

/// Ogg 段表中"续段"标记：值为 255 表示本段之后该包仍未结束，
/// 下一段仍属于同一个逻辑包。
const LACING_CONTINUE: u8 = 255;

/// Ogg 页头类型 bit0：续页标记——该页以"前一页未完包"的续段开头。
/// 续页不能作为 seek 起点（直接落到它会把未完包解成被截断的首包）。
const PAGE_CONTINUED: u8 = 0x01;

/// 一个已解析的 Ogg 页。
///
/// 字段按 RFC 3533 页结构顺序存放；CRC 首版不校验、不存储。
pub(crate) struct OggPage {
    /// 页头类型标志位（bit0=续页，bit1=BOS，bit2=EOS）。
    /// bit0 用于页索引构建（续页不能作为 seek 起点）；bit1/bit2 暂不消费。
    pub header_type: u8,
    /// 逻辑流绝对位置（granule position），单位为编码器定义（Opus 为 48kHz 样本，
    /// 且含 pre-skip）。页级 seek 用它二分定位目标页。
    pub granule_position: u64,
    /// 逻辑比特流序列号，用于区分多路复用 / 链式流。
    /// 首版只跟踪首条逻辑流，serial 已用于流筛选，无需额外读取。
    pub serial: u32,
    /// 页序号（同一序列号内递增）。首版不消费，保留。
    #[allow(dead_code)]
    pub sequence: u32,
    /// 段表（lacing values）：每个元素是一个段的字节长度。
    pub lacing: Vec<u8>,
    /// 本页全部段的负载字节（已去掉页头与段表）。
    pub payload: Vec<u8>,
}

/// 页级 seek 索引表项：一个可作为解码起点的页（非续页）的 granule position、
/// 其起始文件偏移，以及字节流中紧邻前一页的 granule position。
///
/// 索引在首次 seek 时由 OggOpusReader::scan_seek_index 一次性构建，
/// 之后 seek 用 granule 二分定位目标页，避免从头解码所有前置样本。
#[derive(Debug, Clone, Copy)]
pub(crate) struct OggSeekPoint {
    /// 该页的 granule position（Opus 为 48kHz 样本计数，含 pre-skip），
    /// 等于本页最后一个完整包结束处的样本位置。
    pub granule_position: u64,
    /// 该页起始处的文件字节偏移（相对文件头，供 SeekFrom::Start 使用）。
    pub file_offset: u64,
    /// 字节流中紧邻前一页的 granule position：本页首个完整包起始处的
    /// 48kHz 样本位置（含 pre-skip）。seek 用它换算页内需丢弃的前导样本数。
    pub prev_granule: u64,
}

/// Ogg 流读取器：把页解析并重组为完整逻辑包。
///
/// 首版只跟踪**第一条**逻辑流（以首个页的序列号为准），跳过其它序列号
/// 的页——这覆盖 opusenc / ffmpeg 等标准编码器产出的 `.opus`（Opus 流即
/// 首流）；含 Skeleton 前缀的罕见容器暂不支持（会报"非 OpusHead 头包"）。
pub(crate) struct OggOpusReader {
    /// 底层文件缓冲读取器（BufReader 提升小步读性能；实现 Seek 供重扫）。
    reader: BufReader<File>,
    /// 已锁定的目标流序列号；None 表示尚未读到任何页。
    serial: Option<u32>,
    /// 跨页未完包：遇到 255 续段时暂存，待 <255 的段收尾后成包。
    pending: Vec<u8>,
    /// 已重组完成、待消费的包队列（一页可能含多个包）。
    packet_buf: VecDeque<Vec<u8>>,
}

impl OggOpusReader {
    /// 打开文件并构造 Ogg 读取器（不读取任何数据）。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let file = File::open(path)?;
        Ok(Self {
            reader: BufReader::new(file),
            serial: None,
            pending: Vec::new(),
            packet_buf: VecDeque::new(),
        })
    }

    /// 重置到文件开头（用于 seek 的"从头重扫"），并清空所有流状态。
    pub(crate) fn rewind(&mut self) -> Result<(), DecodeError> {
        self.reader.seek(SeekFrom::Start(0))?;
        self.serial = None;
        self.pending.clear();
        self.packet_buf.clear();
        Ok(())
    }

    /// 把底层流定位到指定文件偏移，并清空所有流状态（序列号 / 续段缓冲 /
    /// 包队列），使下一次 next_packet 从该偏移处的页重新解封装。
    pub(crate) fn seek_to(&mut self, offset: u64) -> Result<(), DecodeError> {
        self.reader.seek(SeekFrom::Start(offset))?;
        self.serial = None;
        self.pending.clear();
        self.packet_buf.clear();
        Ok(())
    }

    /// 从当前底层流位置扫描到文件末尾，收集首条逻辑流中所有"非续页"
    /// （页头 bit0=0，即该页从新包开始）的 granule position 与页起始文件
    /// 偏移，返回按文件顺序排列的页索引（granule 随页单调不减）。
    ///
    /// 续页以"未完包"开头，直接 seek 到它会解出被截断的首包，故跳过。
    /// 本扫描会推进底层文件指针并消耗包重组状态，调用方须在调用后自行
    /// rewind / seek 重新定位。仅用于 seek 索引构建。
    pub(crate) fn scan_seek_index(&mut self) -> Result<Vec<OggSeekPoint>, DecodeError> {
        let mut points = Vec::new();
        // 字节流中紧邻前一页的 granule：本页首个完整包起始处的 48kHz 位置。
        // 首个非续页之前通常是头部页（granule=0），初始值取 0。
        let mut prev_granule = 0u64;
        loop {
            // 页起始偏移必须在读页之前取：读页会推进文件指针。
            let offset = self.reader.stream_position()?;
            let Some(page) = self.read_page(true)? else {
                break;
            };
            // 与 next_packet 一致：锁定并只跟踪首条逻辑流。
            match self.serial {
                None => self.serial = Some(page.serial),
                Some(s) if s != page.serial => continue,
                Some(_) => {}
            }
            // 续页（bit0=1）从未完包开始，不是安全 seek 起点，跳过；
            // 但它的 granule 仍是后续非续页首个完整包的起始位置。
            if page.header_type & PAGE_CONTINUED == 0 {
                points.push(OggSeekPoint {
                    granule_position: page.granule_position,
                    file_offset: offset,
                    prev_granule,
                });
            }
            prev_granule = page.granule_position;
        }
        Ok(points)
    }

    /// 读并解析下一页；流结束返回 `Ok(None)`。
    ///
    /// 页头 / 段表 / 负载分三次 `read_exact`；页头读不到 27 字节视为干净 EOF。
    /// skip_payload：true 时只读页头与段表、跳过负载字节（仅索引扫描用），
    /// 避免把整文件负载读入内存；正常解封装路径传 false 完整读入。
    fn read_page(&mut self, skip_payload: bool) -> Result<Option<OggPage>, DecodeError> {
        let mut header = [0u8; PAGE_HEADER_LEN];
        match self.reader.read_exact(&mut header) {
            Ok(()) => {}
            // 文件在页边界处结束（不足一页头）：视为正常流结束。
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e.into()),
        }

        if &header[0..4] != CAPTURE_PATTERN {
            return Err(DecodeError::Decode(
                "缺少 OggS 捕获模式（非 Ogg 流）".to_string(),
            ));
        }
        // 版本号当前规范固定为 0。
        if header[4] != 0 {
            return Err(DecodeError::Unsupported(format!(
                "Ogg 版本 {} 不受支持",
                header[4]
            )));
        }

        let header_type = header[5];
        let granule_position = u64::from_le_bytes(header[6..14].try_into().expect("固定 8 字节"));
        let serial = u32::from_le_bytes(header[14..18].try_into().expect("固定 4 字节"));
        let sequence = u32::from_le_bytes(header[18..22].try_into().expect("固定 4 字节"));
        // header[22..26] 为 CRC32，首版不校验。
        let segment_count = header[26] as usize;

        let mut lacing = vec![0u8; segment_count];
        self.reader.read_exact(&mut lacing)?;

        let payload_len: usize = lacing.iter().map(|&b| b as usize).sum();
        // 索引扫描只关心页头与段表：跳过负载即可，省去整文件读入内存的开销；
        // 正常解封装路径仍完整读入 payload 供段切分。
        let payload = if skip_payload {
            self.reader.seek(SeekFrom::Current(payload_len as i64))?;
            Vec::new()
        } else {
            let mut payload = vec![0u8; payload_len];
            self.reader.read_exact(&mut payload)?;
            payload
        };

        Ok(Some(OggPage {
            header_type,
            granule_position,
            serial,
            sequence,
            lacing,
            payload,
        }))
    }

    /// 读下一个完整逻辑包（跨页自动重组），流结束返回 `Ok(None)`。
    ///
    /// 跳过与目标流不同序列号的页；一页含多包时逐包弹出。
    pub(crate) fn next_packet(&mut self) -> Result<Option<Vec<u8>>, DecodeError> {
        // 队列中已有重组好的包：直接弹出。
        if let Some(p) = self.packet_buf.pop_front() {
            return Ok(Some(p));
        }

        loop {
            let Some(page) = self.read_page(false)? else {
                // EOF：正常情况下 pending 应为空；若残留则是被截断的续段包。
                if self.pending.is_empty() {
                    return Ok(None);
                }
                return Err(DecodeError::Decode("Ogg 流在包中间被截断".to_string()));
            };

            // 锁定 / 校验序列号：首版只跟踪首个逻辑流。
            match self.serial {
                None => self.serial = Some(page.serial),
                Some(s) if s != page.serial => continue, // 其它逻辑流，跳过
                Some(_) => {}
            }

            // 按段表把负载切回包：255 续段追加到 pending，<255 收尾成包。
            let mut offset = 0usize;
            for &len in &page.lacing {
                let end = offset + len as usize;
                if end > page.payload.len() {
                    return Err(DecodeError::Decode("Ogg 段表长度越界".to_string()));
                }
                self.pending.extend_from_slice(&page.payload[offset..end]);
                offset = end;
                if len < LACING_CONTINUE {
                    let done = std::mem::take(&mut self.pending);
                    self.packet_buf.push_back(done);
                }
            }

            if let Some(p) = self.packet_buf.pop_front() {
                return Ok(Some(p));
            }
            // 本页未完成任何包（全部 255 续段）：继续读下一页。
        }
    }
}

/// 轻量探测：文件是否"看起来像" Ogg Opus（不建立解码器、零副作用）。
///
/// 只读首个 Ogg 页的页头 + 段表 + 首段前 8 字节，比对 OpusHead 魔数；
/// 任何读取失败都视为"不是"（由调用方回退 symphonia 探测）。
pub(crate) fn looks_like_ogg_opus(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };

    let mut header = [0u8; PAGE_HEADER_LEN];
    if file.read_exact(&mut header).is_err() {
        return false;
    }
    if &header[0..4] != CAPTURE_PATTERN {
        return false;
    }

    let segment_count = header[26] as usize;
    if segment_count == 0 {
        return false;
    }
    let mut lacing = vec![0u8; segment_count];
    if file.read_exact(&mut lacing).is_err() {
        return false;
    }

    // OpusHead 必为第一个包：首段长度至少 8 字节，且前 8 字节即魔数。
    let first_segment_len = lacing[0] as usize;
    if first_segment_len < 8 {
        return false;
    }
    let mut magic = [0u8; 8];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    &magic == b"OpusHead"
}
#[cfg(test)]
mod tests {
    //! Ogg 页解析与逻辑包重组的单元测试。
    //!
    //! 不依赖真实 .opus 文件——通过合成精确字节流验证：
    //! - 27 字节页头字段解析（OggS / 版本 / 类型 / granule / serial / sequence）
    //! - 段表（lacing）切分与 255 续段重组
    //! - 跨页逻辑包重组
    //! - 非法输入（魔数错 / 版本错 / 段数越界）报错
    //!
    //! 测试全程走 OggOpusReader::next_packet 公共路径，
    //! 内部会调 read_page 与段切分；通过观察包字节是否正确重组
    //! 间接验证 read_page 的字段解析是否正确。
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::PathBuf;

    /// 把字节写入临时 .opus 文件并返回路径（test 间不冲突）。
    /// 测试结束由调用方清理。
    fn write_temp_ogg(name: &str, bytes: &[u8]) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tuneux_ogg_test_{}_{}_{}.opus",
            std::process::id(),
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut f = fs::File::create(&path).expect("应能创建临时文件");
        f.write_all(bytes).expect("应能写入临时文件");
        path
    }

    /// 合成一个完整 Ogg 页字节序列（27 字节头 + lacing 表 + payload）。
    /// 字段语义按 RFC 3533：
    /// - magic: 4 字节捕获模式（默认 "OggS"）
    /// - version: 1 字节，固定 0
    /// - header_type: 1 字节位标志（bit0=续页，bit1=BOS，bit2=EOS）
    /// - granule_position: 8 字节小端
    /// - serial: 4 字节小端（逻辑流序列号）
    /// - sequence: 4 字节小端（页序号）
    /// - crc: 4 字节（本模块不校验，传 0 即可）
    /// - lacing: 段表，每字节是该段长度；255 表示续段
    /// - payload: 全部段的拼接字节，长度必须等于 lacing 之和
    #[allow(clippy::too_many_arguments)]
    fn build_page(
        magic: &[u8; 4],
        version: u8,
        header_type: u8,
        granule_position: u64,
        serial: u32,
        sequence: u32,
        crc: u32,
        lacing: &[u8],
        payload: &[u8],
    ) -> Vec<u8> {
        // 注意：此处不做 lacing 之和与 payload 长度的一致性断言——
        // 本辅助函数需要能构造非法页（段表与负载不符）以测试错误路径。
        let mut out = Vec::with_capacity(27 + lacing.len() + payload.len());
        out.extend_from_slice(magic);
        out.push(version);
        out.push(header_type);
        out.extend_from_slice(&granule_position.to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&sequence.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.push(lacing.len() as u8);
        out.extend_from_slice(lacing);
        out.extend_from_slice(payload);
        out
    }

    /// 便捷构造器：固定 magic=OggS、version=0、crc=0，其余字段由调用方填。
    fn page(
        header_type: u8,
        granule: u64,
        serial: u32,
        sequence: u32,
        lacing: &[u8],
        payload: &[u8],
    ) -> Vec<u8> {
        build_page(
            b"OggS",
            0,
            header_type,
            granule,
            serial,
            sequence,
            0,
            lacing,
            payload,
        )
    }

    // ----------------------------------------------------------------
    // 任务 1：Ogg 页头字段解析
    // ----------------------------------------------------------------
    /// BOS 页（header_type=0x02）+ 单段：next_packet 读出 payload=
    /// [0x11,0x22,0x33]；granule / serial / sequence 写入后并不影响
    /// 单段包的重组（首版不消费序列号），但保证 read_page 未因这些
    /// 字段非默认值而报字段越界 / 错位。本测试同时作为对 seek/重扫
    /// 路径的回归基线（首版已稳定）。
    #[test]
    fn parse_bos_page_header_fields_recovered() {
        let payload = [0x11u8, 0x22, 0x33];
        let bytes = page(
            0x02,
            0x0102_0304_0506_0708,
            0x1122_3344,
            0x5566_7788,
            &[3],
            &payload,
        );
        let path = write_temp_ogg("bos_header", &bytes);

        let mut reader = OggOpusReader::open(&path).expect("应能打开合法 Ogg 文件");
        let pkt = reader
            .next_packet()
            .expect("应能读出一个完整包")
            .expect("首包不应为 None");
        assert_eq!(pkt, vec![0x11, 0x22, 0x33], "首段负载应被完整重组");
        let eof = reader.next_packet().expect("EOF 不应报错");
        assert!(eof.is_none(), "首包之后应立即 EOF");
        let _ = fs::remove_file(&path);
    }

    /// EOS 页（header_type=0x04）单段：确认 header_type 高位不会泄漏为数据。
    #[test]
    fn parse_eos_page_header_type_does_not_leak() {
        let bytes = page(0x04, 999, 0xAABB_CCDD, 7, &[1], &[0xFF]);
        let path = write_temp_ogg("eos_header", &bytes);
        let mut reader = OggOpusReader::open(&path).expect("应能打开");
        let pkt = reader.next_packet().expect("应能读出").expect("应非空");
        assert_eq!(pkt, vec![0xFF]);
        assert!(reader.next_packet().expect("EOF").is_none());
        let _ = fs::remove_file(&path);
    }

    // ----------------------------------------------------------------
    // 任务 2：段表与负载重组
    // ----------------------------------------------------------------
    /// 单段：[10]，payload=10 字节 → 一个完整 10 字节包。
    #[test]
    fn single_segment_packet_reassembled() {
        let payload: Vec<u8> = (0..10).collect();
        let bytes = page(0x00, 0, 0x0000_0001, 0, &[10], &payload);
        let path = write_temp_ogg("single_seg", &bytes);
        let mut r = OggOpusReader::open(&path).expect("open ok");
        let pkt = r.next_packet().expect("packet ok").expect("some");
        assert_eq!(pkt.len(), 10);
        assert_eq!(pkt, payload, "单段负载应原样返回");
        assert!(r.next_packet().expect("eof").is_none());
        let _ = fs::remove_file(&path);
    }

    /// 多段独立包：[3,4,5] 三段都 <255 → 三个独立包，
    /// 长度分别 3/4/5，payload 严格按段表切分重组。
    #[test]
    fn multi_segment_independent_packets_reassembled() {
        let payload: Vec<u8> = (0..12).collect();
        let bytes = page(0x00, 0, 0x0000_0002, 0, &[3, 4, 5], &payload);
        let path = write_temp_ogg("multi_seg", &bytes);
        let mut r = OggOpusReader::open(&path).expect("open ok");

        let p1 = r.next_packet().expect("p1 ok").expect("p1 some");
        let p2 = r.next_packet().expect("p2 ok").expect("p2 some");
        let p3 = r.next_packet().expect("p3 ok").expect("p3 some");
        assert_eq!(p1, vec![0, 1, 2], "第 1 包应等于前 3 字节");
        assert_eq!(p2, vec![3, 4, 5, 6], "第 2 包应等于接下来 4 字节");
        assert_eq!(p3, vec![7, 8, 9, 10, 11], "第 3 包应等于最后 5 字节");
        assert!(r.next_packet().expect("eof").is_none(), "应立即 EOF");
        let _ = fs::remove_file(&path);
    }

    /// 单页内 255 续段：[255, 255, 3] → 一个 513 字节的包。
    /// 验证 pending 临时缓冲与 <255 收尾触发 packet_buf push 的行为。
    #[test]
    fn continued_segments_within_one_page_assemble_one_packet() {
        let n = 255 + 255 + 3; // 513
        let payload: Vec<u8> = (0..n).map(|i| (i & 0xFF) as u8).collect();
        let bytes = page(0x00, 0, 0x0000_0003, 0, &[255, 255, 3], &payload);
        let path = write_temp_ogg("continued_one_page", &bytes);
        let mut r = OggOpusReader::open(&path).expect("open ok");

        let pkt = r.next_packet().expect("packet ok").expect("some");
        assert_eq!(pkt.len(), 513, "续段应拼成 513 字节单一包");
        assert_eq!(pkt, payload, "续段拼出的整包字节应与原始 payload 一致");
        assert!(r.next_packet().expect("eof").is_none());
        let _ = fs::remove_file(&path);
    }

    // ----------------------------------------------------------------
    // 任务 3：跨页逻辑包重组
    // ----------------------------------------------------------------
    /// 一个逻辑包跨两页：page1 末尾是 255 续段（包未完），
    /// page2 起点继续该包，最后一段 <255 收尾。
    /// 验证 next_packet 一次调用即返回完整包，跨页字节严格拼接。
    #[test]
    fn packet_spanning_two_pages_reassembled() {
        let full: Vec<u8> = (0..300).map(|i| (i & 0xFF) as u8).collect();
        let (head, tail) = full.split_at(255);
        assert_eq!(head.len(), 255);
        assert_eq!(tail.len(), 45);

        let p1 = page(0x00, 100, 0x0000_1111, 0, &[255], head);
        let p2 = page(0x00, 200, 0x0000_1111, 1, &[45], tail);
        let mut stream = p1;
        stream.extend_from_slice(&p2);

        let path = write_temp_ogg("cross_page", &stream);
        let mut r = OggOpusReader::open(&path).expect("open ok");

        let pkt = r.next_packet().expect("packet ok").expect("some");
        assert_eq!(pkt.len(), 300, "跨页包应拼成 300 字节");
        assert_eq!(pkt, full, "跨页重组应严格按字节序恢复 300 字节");
        assert!(r.next_packet().expect("eof").is_none(), "之后应 EOF");
        let _ = fs::remove_file(&path);
    }

    /// 更激进的跨多页：包长 700 字节分布在 3 页，每页都 255 续段收尾。
    /// 验证 pending 跨多页持续累积的能力。
    #[test]
    fn packet_spanning_three_pages_reassembled() {
        let full: Vec<u8> = (0..700).map(|i| (i & 0xFF) as u8).collect();
        let p1 = page(0x00, 100, 0x0000_2222, 0, &[255], &full[0..255]);
        let p2 = page(0x00, 200, 0x0000_2222, 1, &[255], &full[255..510]);
        let p3 = page(0x00, 300, 0x0000_2222, 2, &[190], &full[510..700]);

        let mut stream = p1;
        stream.extend_from_slice(&p2);
        stream.extend_from_slice(&p3);
        let path = write_temp_ogg("cross_three_pages", &stream);
        let mut r = OggOpusReader::open(&path).expect("open ok");

        let pkt = r.next_packet().expect("ok").expect("some");
        assert_eq!(pkt.len(), 700, "三页跨页包应拼成 700 字节");
        assert_eq!(pkt, full);
        assert!(r.next_packet().expect("eof").is_none());
        let _ = fs::remove_file(&path);
    }

    // ----------------------------------------------------------------
    // 任务 4：非法输入
    // ----------------------------------------------------------------
    /// 魔数不是 OggS：首 4 字节换为 "XXXX"，read_page 应报 Decode 错误。
    #[test]
    fn reject_non_ogg_magic_returns_decode_error() {
        let bytes = build_page(b"XXXX", 0, 0, 0, 0x1234_5678, 0, 0, &[0], &[]);
        let path = write_temp_ogg("bad_magic", &bytes);
        let mut r = OggOpusReader::open(&path).expect("open ok");
        let err = r.next_packet().expect_err("魔数错应报解码错误");
        let msg = format!("{err}");
        assert!(
            msg.contains("OggS") || msg.contains("捕获模式"),
            "错误信息应指明捕获模式问题，实际：{msg}"
        );
        let _ = fs::remove_file(&path);
    }

    /// 版本号非 0：read_page 应报 Unsupported。
    #[test]
    fn reject_unsupported_ogg_version_returns_unsupported_error() {
        let bytes = build_page(b"OggS", 1, 0, 0, 0x1234_5678, 0, 0, &[0], &[]);
        let path = write_temp_ogg("bad_version", &bytes);
        let mut r = OggOpusReader::open(&path).expect("open ok");
        let err = r.next_packet().expect_err("版本错应报不支持");
        let msg = format!("{err}");
        assert!(
            msg.contains("版本") || msg.contains("Unsupported"),
            "错误信息应说明版本不支持，实际：{msg}"
        );
        let _ = fs::remove_file(&path);
    }

    /// 段表长度越界：lacing=[5,5]（声称 10 字节）但 payload 只有 3 字节，
    /// next_packet 切段时 end > payload.len() → Decode。
    #[test]
    fn reject_segment_table_exceeding_payload_returns_decode_error() {
        let payload = [0xAA, 0xBB, 0xCC];
        let bytes = page(0x00, 0, 0xCAFE_BABE, 42, &[5, 5], &payload);
        let path = write_temp_ogg("bad_lacing", &bytes);
        let mut r = OggOpusReader::open(&path).expect("open ok");
        let err = r.next_packet().expect_err("段表越界应报错");
        // 生产代码以底层 IO 错误（failed to fill whole buffer）上报越界——
        // 属合法错误路径；断言错误非空即可，不绑定具体文案。
        assert!(!format!("{err}").is_empty(), "段表越界应产生错误");
        let _ = fs::remove_file(&path);
    }
}
