//! # Opus 解码后端（Ogg Opus 容器）
//!
//! 组合 [`super::ogg_opus`]（Ogg 解封装，输出原始 Opus 包）与
//! `opus-decoder` crate（包 → 交错 f32 PCM），实现 [DecoderBackend]。
//!
//! ## 关键设计
//!
//! - **采样率**：`opus-decoder` 只支持 8/12/16/24/48 kHz 五种输出率。
//!   OpusHead 的 input sample rate 若属于这五者则直接采用，否则回退到
//!   Opus 原生 48 kHz（并如实上报 48 kHz，保证 `params.sample_rate`
//!   与实际 PCM 采样率一致，下游 rubato 重采样兜底）。
//! - **预跳过（pre-skip）**：OpusHead 的 pre-skip 以 48 kHz 计，按输出率
//!   等比缩放后，在解码出的样本前端丢弃。
//! - **seek**：Ogg 无内建索引，首版用"从头重扫"——回卷文件、重读头包、
//!   重置解码器、逐包解码累计时长直到越过目标，把跨目标的帧裁剪后
//!   缓存为下一帧输出。
//! - **时长**：首版不解析（`duration = None`），避免为读最后一页而
//!   反向扫描文件；不影响播放，仅 TUI 时长显示暂缺。

use std::path::Path;

use opus_decoder::{OpusDecoder, OpusError};

use super::decoder::{AudioParams, DecodeError, DecoderBackend};
use super::ogg_opus::{looks_like_ogg_opus, OggOpusReader, OggSeekPoint};

/// OpusHead 头包魔数。
const OPUS_HEAD_MAGIC: &[u8; 8] = b"OpusHead";
/// OpusTags 头包魔数（注释 / 元数据包，解码时跳过）。
const OPUS_TAGS_MAGIC: &[u8; 8] = b"OpusTags";
/// OpusHead 头包最小长度：魔数 8 + 版本 1 + 声道 1 + 预跳过 2
/// + 输入采样率 4 + 增益 2 + 映射族 1 = 19 字节。
const OPUS_HEAD_MIN_LEN: usize = 19;

/// 解析 OpusHead 头包，返回（输入采样率, 声道数, 预跳过样本数, Q7.8 dB 输出增益）。
///
/// 按 RFC 7845 §5.1 的字段顺序解析；只接受映射族 0（标准 1~2 声道），
/// 其它映射族（环绕声等）首版不支持。
fn parse_opus_head(packet: &[u8]) -> Result<(u32, u8, u16, i16), DecodeError> {
    if packet.len() < OPUS_HEAD_MIN_LEN || &packet[..8] != OPUS_HEAD_MAGIC {
        return Err(DecodeError::Unsupported("非 OpusHead 头包".to_string()));
    }

    let version = packet[8];
    if version != 1 {
        return Err(DecodeError::Unsupported(format!(
            "Opus 版本 {version} 不受支持"
        )));
    }

    let channels = packet[9];
    let pre_skip = u16::from_le_bytes([packet[10], packet[11]]);
    let input_sample_rate = u32::from_le_bytes([packet[12], packet[13], packet[14], packet[15]]);
    let output_gain = i16::from_le_bytes([packet[16], packet[17]]);
    let mapping_family = packet[18];
    if mapping_family != 0 {
        return Err(DecodeError::Unsupported(format!(
            "Opus 映射族 {mapping_family} 不受支持（仅支持 0）"
        )));
    }

    Ok((input_sample_rate, channels, pre_skip, output_gain))
}

/// 把 OpusHead 的输入采样率映射为 opus-decoder 支持的五种输出率之一。
///
/// 不在 {8,12,16,24,48} kHz 内的（如 44.1 / 22.05 kHz）回退到 48 kHz 原生率，
/// 保证解码输出与 `params.sample_rate` 一致。
fn choose_output_rate(input_rate: u32) -> u32 {
    match input_rate {
        8_000 | 12_000 | 16_000 | 24_000 | 48_000 => input_rate,
        _ => 48_000,
    }
}

/// 将 OpusHead 的 Q7.8 dB 增益换算为 PCM 样本的线性振幅倍率。
fn output_gain_factor(output_gain_db: i16) -> f32 {
    10f32.powf(f32::from(output_gain_db) / (20.0 * 256.0))
}

/// 判断该包是否为头包（OpusHead / OpusTags），解码时应跳过。
fn is_header_packet(packet: &[u8]) -> bool {
    packet.len() >= 8 && (&packet[..8] == OPUS_HEAD_MAGIC || &packet[..8] == OPUS_TAGS_MAGIC)
}

/// 轻量探测 Opus 输出采样率：读 OpusHead 输入采样率，映射解码输出率。
///
/// 供直通判断（[`super::decoder::probe_sample_rate`]）：Opus 输出率由
/// OpusHead 决定（8/12/16/24/48k 原生；44.1k 等按 48k 编码），并非恒 48k——
/// 读头而非硬编码，避免把 24k 语音文件误判成 48k 直通（会变调）。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    // 第一个逻辑包必为 OpusHead（RFC 7845 §4.2），读它即可，不建解码器。
    let head_packet = OggOpusReader::open(path).ok()?.next_packet().ok()??;
    let (input_rate, _, _, _) = parse_opus_head(&head_packet).ok()?;
    Some(choose_output_rate(input_rate))
}

/// 判断路径是否应交给 Opus 后端：扩展名为 `.opus`，或 Ogg 首包魔数为 OpusHead。
pub(crate) fn is_opus_path(path: &Path) -> bool {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("opus"))
        .unwrap_or(false)
    {
        return true;
    }
    looks_like_ogg_opus(path)
}

/// Opus 解码后端（内部实现，经 [`super::decoder::open_backend`] 分发）。
///
/// 一个实例对应一个已打开的 `.opus` 文件；换曲时丢弃旧实例、新建。
pub(crate) struct OpusBackend {
    /// Ogg 解封装读取器：输出原始 Opus 包。
    reader: OggOpusReader,
    /// 纯 Rust Opus 解码器（包 → 交错 PCM）。
    decoder: OpusDecoder,
    /// 技术参数（采样率 / 声道 / 位深 / 编码名）。
    params: AudioParams,
    /// 声道数（缓存，避免反复从 params 取）。
    channels: usize,
    /// 起始预跳过样本数（按输出采样率缩放后的每声道样本）。
    pre_skip_out: u64,
    /// 剩余待跳过的预跳过样本（每声道）。
    samples_to_skip: u64,
    /// seek 后缓存的"跨目标帧"样本，供下一次 `decode_next` 优先返回。
    pending_samples: Vec<f32>,
    /// 页级 seek 索引（懒构建）：首次 seek 时由 scan_seek_index 生成并缓存。
    seek_index: Option<Vec<OggSeekPoint>>,
    /// OpusHead 中的 Q7.8 dB 输出增益对应的线性倍率。
    output_gain: f32,
}

impl OpusBackend {
    /// 打开 `.opus` 文件：解析头包、建立解码器、准备预跳过。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let mut reader = OggOpusReader::open(path)?;

        // 第一个逻辑包必为 OpusHead（RFC 7845 §4.2：ID 头在 BOS 页）。
        let head_packet = reader
            .next_packet()?
            .ok_or_else(|| DecodeError::Unsupported("空的 Ogg 流".to_string()))?;
        let (input_rate, channels, pre_skip, output_gain_db) = parse_opus_head(&head_packet)?;

        if channels == 0 || channels > 2 {
            return Err(DecodeError::Unsupported(format!(
                "Opus 声道数 {channels} 超出 1~2 范围"
            )));
        }

        let sample_rate = choose_output_rate(input_rate);
        let channels_usize = channels as usize;
        let decoder = OpusDecoder::new(sample_rate, channels_usize)
            .map_err(|e| DecodeError::Unsupported(format!("Opus 解码器初始化失败：{e}")))?;

        // pre-skip 以 48kHz 计，按输出采样率等比缩放（四舍五入）。
        let pre_skip_out = (u64::from(pre_skip) * u64::from(sample_rate) + 24_000) / 48_000;

        let params = AudioParams::new(
            Some(sample_rate),
            Some(channels as u16),
            Some(16),
            "Opus".to_string(),
            0,    // Ogg 单流无"音轨 ID"概念，固定 0（调用方不按轨过滤）。
            None, // 时长首版不解析，见模块文档。
        );

        Ok(Self {
            reader,
            decoder,
            params,
            channels: channels_usize,
            pre_skip_out,
            samples_to_skip: pre_skip_out,
            pending_samples: Vec::new(),
            seek_index: None,
            output_gain: output_gain_factor(output_gain_db),
        })
    }

    /// 解码单个原始 Opus 包为交错 f32 样本（不做预跳过）。
    fn decode_packet(&mut self, packet: &[u8]) -> Result<Vec<f32>, DecodeError> {
        // 预分配最大帧缓冲；decode_float 只写实际样本数，再截断。
        let max = self.decoder.max_frame_size_per_channel();
        let mut pcm = vec![0.0f32; max * self.channels];
        let samples_per_channel = self
            .decoder
            .decode_float(packet, &mut pcm, false)
            .map_err(map_opus_error)?;
        let total = samples_per_channel * self.channels;
        pcm.truncate(total);
        if self.output_gain != 1.0 {
            for sample in &mut pcm {
                *sample *= self.output_gain;
            }
        }
        Ok(pcm)
    }

    /// 对样本前端应用预跳过：丢弃 `samples_to_skip` 个每声道样本并扣减计数。
    fn apply_pre_skip(&mut self, samples: &mut Vec<f32>) {
        if self.samples_to_skip == 0 {
            return;
        }
        let skip = (self.samples_to_skip as usize).min(samples.len() / self.channels);
        let drain = skip * self.channels;
        samples.drain(..drain);
        self.samples_to_skip -= skip as u64;
    }

    /// 从头重扫式 seek（回退路径）：回卷文件、重读头包、重置解码器与预跳过。
    ///
    /// 用于 granule 页级定位失败（目标超出索引末尾 / 索引为空）时兜底；
    /// 目标超出流末尾时钳到末尾返回 Ok（与 wav/flac/wavpack/cdda 契约一致，
    /// 末尾秒数按流末 granule / 48_000 换算，与页级路径同口径）。
    fn seek_scan_from_start(&mut self, secs: f64) -> Result<f64, DecodeError> {
        self.reader.rewind()?;
        let head = self
            .reader
            .next_packet()?
            .ok_or_else(|| DecodeError::Decode("重扫时流为空".to_string()))?;
        parse_opus_head(&head)?;
        self.decoder.reset();
        self.samples_to_skip = self.pre_skip_out;
        self.pending_samples.clear();

        let rate = f64::from(self.params.sample_rate.unwrap_or(48_000));
        let target = (secs * rate).round() as u64; // 目标输出样本（每声道）
        let mut played = 0u64;

        loop {
            let Some(packet) = self.reader.next_packet()? else {
                // 目标超出末尾：与 wav/flac/wavpack/cdda 统一契约——钳到
                // 末尾返回 Ok（原返回 Err 会让调用方吞掉后直接 EOF 静默
                // 停播）。秒数换算与页级路径同口径：granule 恒为 48 kHz
                // 样本计数（RFC 7845），除以 48_000，不按输出采样率缩放。
                let end_secs = self.reader.last_granule() as f64 / 48_000.0;
                return Ok(end_secs);
            };
            if is_header_packet(&packet) {
                continue;
            }
            let mut samples = self.decode_packet(&packet)?;
            self.apply_pre_skip(&mut samples);

            let spc = (samples.len() / self.channels) as u64;
            if played + spc >= target {
                let consume = (target.saturating_sub(played) as usize) * self.channels;
                let drain = consume.min(samples.len());
                samples.drain(..drain);
                self.pending_samples = samples;
                break;
            }
            played += spc;
        }

        Ok(secs)
    }
}

/// 把 opus-decoder 的错误映射为后端无关的 [DecodeError]。
fn map_opus_error(e: OpusError) -> DecodeError {
    match e {
        OpusError::InvalidPacket => DecodeError::Decode("Opus 包畸形".to_string()),
        OpusError::InternalError => DecodeError::Decode("Opus 解码器内部错误".to_string()),
        OpusError::BufferTooSmall => DecodeError::Decode("Opus 输出缓冲不足".to_string()),
        OpusError::InvalidArgument(what) => {
            DecodeError::Unsupported(format!("Opus 参数非法：{what}"))
        }
    }
}

impl DecoderBackend for OpusBackend {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        // seek 后缓存的跨目标帧优先返回；返回前仍应用剩余的预跳过
        //（seek(0) 恢复 pre-skip 后，首帧的暖机段在此处丢弃）。
        if !self.pending_samples.is_empty() {
            let mut pending = std::mem::take(&mut self.pending_samples);
            self.apply_pre_skip(&mut pending);
            if !pending.is_empty() {
                return Ok(Some(pending));
            }
            // 整块被预跳过吞掉：落入下方循环继续读包（与空帧跳过同口径）。
        }

        loop {
            let Some(packet) = self.reader.next_packet()? else {
                return Ok(None);
            };
            // 防御：跳过意外出现在流中的头包（如链式流的后续 ID 头）。
            if is_header_packet(&packet) {
                continue;
            }
            let mut samples = self.decode_packet(&packet)?;
            self.apply_pre_skip(&mut samples);
            if samples.is_empty() {
                continue;
            }
            return Ok(Some(samples));
        }
    }

    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek("seek 秒数必须为非负有限值".to_string()));
        }

        // 目标 granule（48 kHz 样本计数，Opus 语义含 pre-skip）。
        let target_granule = (secs * 48_000.0).round() as u64;

        // 懒构建页级 seek 索引（granule 定位基础）。
        let index = match &self.seek_index {
            Some(idx) => idx.clone(),
            None => {
                // 必须从流首扫描：scan_seek_index 从当前文件指针开始，
                // 从中段扫描会导致 prev_granule 初值错误（硬编码 0 只有
                // 从流首扫描才成立）→ 向后 seek 的 prev_granule 恒为 0
                // → skip = target − 0，实际落点比请求值多出「已播时长」。
                // 扫完后 seek_to 会重置流状态，无需恢复位置。
                self.reader.rewind()?;
                let idx = self.reader.scan_seek_index()?;
                self.seek_index = Some(idx.clone());
                idx
            }
        };

        // 找第一个 granule_position >= 目标的页（线性查找：索引 granule
        // 单调不减，页数级别可接受；二分是可选优化）。
        // 用该页的 prev_granule（页内首包起点）换算需丢弃的前导样本。
        let found = index
            .iter()
            .find(|pt| pt.granule_position >= target_granule)
            .copied();

        match found {
            Some(point) => {
                // —— granule 页级定位 ——
                self.reader.seek_to(point.file_offset)?;
                self.decoder.reset();
                self.pending_samples.clear();
                // granule 时钟含 pre-skip：页内 skip 换算自带 pre-skip 偏移，
                // 常规目标不重复跳过；但目标 0 即回到流首（命中头页），
                // skip_out=0 时若不恢复 pre-skip，暖机段会当有效样本输出，
                // 与冷启动 open 行为不一致——此时恢复 pre_skip_out。
                self.samples_to_skip = if target_granule == 0 {
                    self.pre_skip_out
                } else {
                    0
                };

                let rate = f64::from(self.params.sample_rate.unwrap_or(48_000));
                // 页内需丢弃的前导样本（48 kHz 计数 → 输出样本）。
                let skip_48k = target_granule.saturating_sub(point.prev_granule);
                let skip_out = ((skip_48k as f64) * rate / 48_000.0).round() as u64;

                let mut skipped = 0u64;
                loop {
                    let Some(packet) = self.reader.next_packet()? else {
                        // 流已到末尾：统一后端契约——越界钳到末尾返回 Ok
                        //（与 wav/flac/wavpack/cdda 同口径；原返回 Err
                        // 会让调用方吞掉后直接 EOF 播放静默终止）。
                        // 秒数换算：granule 恒为 48 kHz 样本计数（RFC 7845），
                        // 与解码输出率无关——不得按 rate 缩放（旧式曾对
                        // 8/12/16/24 kHz 输出率的文件把落点缩小 3~6 倍）。
                        let end_secs = point.granule_position as f64 / 48_000.0;
                        return Ok(end_secs);
                    };
                    if is_header_packet(&packet) {
                        continue;
                    }
                    let mut samples = self.decode_packet(&packet)?;
                    let spc = (samples.len() / self.channels) as u64;
                    if skipped + spc >= skip_out {
                        // 本帧跨越目标：丢弃目标之前的部分，余量缓存为下一帧输出。
                        let consume = (skip_out.saturating_sub(skipped) as usize) * self.channels;
                        let drain = consume.min(samples.len());
                        samples.drain(..drain);
                        self.pending_samples = samples;
                        break;
                    }
                    skipped += spc;
                }
                Ok(secs)
            }
            None => {
                // 目标超出索引末尾（末页 granule 之后）或索引为空：
                // 回退到从头重扫（保守正确）。
                self.seek_scan_from_start(secs)
            }
        }
    }
}
#[cfg(test)]
mod tests {
    //! OpusHead 头包解析的单元测试。
    //!
    //! 调用同模块私有的 [parse_opus_head]：合成 19 字节包，
    //! 验证魔数 / 版本 / 声道 / 预跳过 / 输入采样率 / 映射族字段
    //! 的解析正确性，以及非法输入（魔数错 / 版本错 / 长度过短 /
    //! 映射族 > 0）的报错路径。
    use super::*;

    /// 合成 19 字节 OpusHead 包。
    /// 字段顺序按 RFC 7845 §5.1：魔数 8 + version 1 + channels 1
    /// + pre_skip 2 + input_sample_rate 4 + output_gain 2 + mapping 1。
    fn build_opus_head(
        version: u8,
        channels: u8,
        pre_skip: u16,
        input_sample_rate: u32,
        output_gain: i16,
        mapping_family: u8,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(19);
        buf.extend_from_slice(b"OpusHead");
        buf.push(version);
        buf.push(channels);
        buf.extend_from_slice(&pre_skip.to_le_bytes());
        buf.extend_from_slice(&input_sample_rate.to_le_bytes());
        buf.extend_from_slice(&output_gain.to_le_bytes());
        buf.push(mapping_family);
        // 字段顺序自检：合成数据自身的一致性是第一道防线。
        debug_assert_eq!(buf.len(), 19, "OpusHead 应正好 19 字节");
        buf
    }

    /// 合法 OpusHead（默认字段）：应解析为输入采样率 48000、
    /// 声道 2、预跳过 312、输出增益 0。
    #[test]
    fn parse_valid_opus_head_default_fields() {
        let pkt = build_opus_head(1, 2, 312, 48_000, 0, 0);
        let (rate, ch, pre, gain) = parse_opus_head(&pkt).expect("合法 OpusHead 应解析成功");
        assert_eq!(rate, 48_000, "输入采样率应为 48000");
        assert_eq!(ch, 2, "声道数应为 2");
        assert_eq!(pre, 312, "预跳过应为 312");
        assert_eq!(gain, 0, "输出增益应为 0");
    }

    /// 合法 OpusHead（自定义字段）：单声道 + 较小预跳过 + 16 kHz。
    /// 验证 LE 字节序与字段偏移解析正确。
    #[test]
    fn parse_valid_opus_head_custom_fields() {
        // pre_skip=120 (0x0078)，rate=16000 (0x00003E80)，output_gain=-128。
        let pkt = build_opus_head(1, 1, 120, 16_000, -128, 0);
        let (rate, ch, pre, gain) = parse_opus_head(&pkt).expect("应解析");
        assert_eq!(rate, 16_000);
        assert_eq!(ch, 1);
        assert_eq!(pre, 120);
        assert_eq!(gain, -128);
    }

    #[test]
    fn output_gain_uses_decibel_amplitude_scale() {
        assert_eq!(output_gain_factor(0), 1.0);
        let minus_six_db = output_gain_factor(-512);
        assert!((minus_six_db - 10f32.powf(-2.0 / 20.0)).abs() < 1e-6);
        assert!(output_gain_factor(-128) < 1.0);
        assert!(output_gain_factor(128) > 1.0);
    }

    /// 合法 OpusHead：边界值——最小采样率 8 kHz、零预跳过、双声道。
    #[test]
    fn parse_valid_opus_head_edge_values() {
        let pkt = build_opus_head(1, 2, 0, 8_000, 0, 0);
        let (rate, ch, pre, _) = parse_opus_head(&pkt).expect("应解析");
        assert_eq!(rate, 8_000);
        assert_eq!(ch, 2);
        assert_eq!(pre, 0);
    }

    /// 合法 OpusHead：24 kHz 采样率（opus-decoder 支持的五种之一）。
    #[test]
    fn parse_valid_opus_head_24khz() {
        let pkt = build_opus_head(1, 1, 0, 24_000, 0, 0);
        let (rate, _, _, _) = parse_opus_head(&pkt).expect("应解析");
        assert_eq!(rate, 24_000);
    }

    /// 非法：长度过短（仅 8 字节魔数）→ Unsupported。
    #[test]
    fn reject_too_short_packet_returns_unsupported() {
        let pkt: Vec<u8> = b"OpusHead".to_vec();
        assert_eq!(pkt.len(), 8);
        let err = parse_opus_head(&pkt).expect_err("过短应报错");
        let msg = format!("{err}");
        assert!(
            msg.contains("OpusHead") || msg.contains("头包"),
            "错误信息应指明非 OpusHead 头包，实际：{msg}"
        );
    }

    /// 非法：魔数错（破坏最后一字节，从 'd' 改成 0xFF）。
    #[test]
    fn reject_bad_magic_returns_unsupported() {
        let mut pkt = build_opus_head(1, 2, 0, 48_000, 0, 0);
        pkt[7] = 0xFF;
        let err = parse_opus_head(&pkt).expect_err("魔数错应报错");
        let msg = format!("{err}");
        assert!(
            msg.contains("OpusHead"),
            "错误信息应指明非 OpusHead，实际：{msg}"
        );
    }

    /// 非法：版本号不是 1（如 2）→ Unsupported。
    #[test]
    fn reject_unsupported_opus_version_returns_unsupported() {
        let pkt = build_opus_head(2, 2, 0, 48_000, 0, 0);
        let err = parse_opus_head(&pkt).expect_err("版本 2 应报错");
        let msg = format!("{err}");
        assert!(msg.contains("版本"), "错误信息应说明版本问题，实际：{msg}");
    }

    /// 非法：映射族 = 1（环绕声）→ Unsupported。
    #[test]
    fn reject_nonzero_mapping_family_returns_unsupported() {
        let pkt = build_opus_head(1, 2, 0, 48_000, 0, 1);
        let err = parse_opus_head(&pkt).expect_err("映射族 1 应报错");
        let msg = format!("{err}");
        assert!(
            msg.contains("映射族"),
            "错误信息应说明映射族不受支持，实际：{msg}"
        );
    }

    /// 非法：映射族 = 255（任意非 0 值都该被拒，验证判断是 != 0 而非只拒绝 1）。
    #[test]
    fn reject_mapping_family_255_returns_unsupported() {
        let pkt = build_opus_head(1, 2, 0, 48_000, 0, 255);
        assert!(parse_opus_head(&pkt).is_err(), "映射族 255 应同样被拒");
    }

    // ----------------------------------------------------------------
    // seek 契约与预跳过（合成仅含头包的流，不依赖真实 Opus 音频包）
    // ----------------------------------------------------------------

    /// 合成一个 Ogg 页（27 字节头 + 段表 + 负载），serial 固定 0x0000_4242。
    fn ogg_page(
        header_type: u8,
        granule: u64,
        sequence: u32,
        lacing: &[u8],
        payload: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(27 + lacing.len() + payload.len());
        out.extend_from_slice(b"OggS");
        out.push(0);
        out.push(header_type);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&0x0000_4242u32.to_le_bytes());
        out.extend_from_slice(&sequence.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.push(lacing.len() as u8);
        out.extend_from_slice(lacing);
        out.extend_from_slice(payload);
        out
    }

    /// 写临时 .opus 流：BOS 页 OpusHead + 标签页 OpusTags（无音频包）。
    fn write_header_only_opus() -> std::path::PathBuf {
        let head = build_opus_head(1, 2, 312, 48_000, 0, 0);
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&[0, 0, 0, 0]);
        let mut stream = ogg_page(0x02, 0, 0, &[head.len() as u8], &head);
        stream.extend_from_slice(&ogg_page(0x00, 0, 1, &[tags.len() as u8], &tags));
        let mut path = std::env::temp_dir();
        path.push(format!(
            "tuneux_opus_test_{}_{}.opus",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, stream).expect("应能写入临时文件");
        path
    }

    /// 越界 seek 走回退路径（目标超出索引末尾）：修复前整文件重解码后
    /// 返回 Err，与 wav/flac/wavpack/cdda「越界钳到末尾 Ok」契约矛盾；
    /// 修复后钳到末尾返回 Ok，末尾秒数按 granule/48000 换算（此处流末
    /// granule=0 → 0.0），随后 decode_next 干净 EOF。
    #[test]
    fn seek_beyond_end_fallback_path_clamps_to_end() {
        let path = write_header_only_opus();
        let mut b = OpusBackend::open(&path).expect("应能打开");
        let r = b.seek(999.0).expect("越界 seek 应钳到末尾返回 Ok");
        assert_eq!(r, 0.0, "流末 granule=0，换算末尾秒数应为 0.0");
        assert!(
            b.decode_next().expect("读").is_none(),
            "钳到末尾后应立即 EOF"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// seek(0) 恢复 pre-skip：页级路径一律置 samples_to_skip=0 时，
    /// seek(0) 首帧会把暖机段当有效样本输出，与冷启动 open 不一致；
    /// 修复后目标 0 恢复 pre_skip_out，且 decode_next 的缓存帧路径
    /// 会应用剩余预跳过。
    #[test]
    fn seek_to_zero_restores_pre_skip_counter() {
        let path = write_header_only_opus();
        let mut b = OpusBackend::open(&path).expect("应能打开");
        let r = b.seek(0.0).expect("seek(0) 应成功");
        assert_eq!(r, 0.0);
        assert!(
            b.samples_to_skip == b.pre_skip_out && b.pre_skip_out == 312,
            "seek(0) 应恢复 pre-skip（预跳过 312 @48kHz），实际：skip={} pre={}",
            b.samples_to_skip,
            b.pre_skip_out
        );
        assert!(b.decode_next().expect("读").is_none(), "头包流解码完应 EOF");
        let _ = std::fs::remove_file(&path);
    }
}
