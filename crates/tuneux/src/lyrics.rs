//! # 歌词模块
//!
//! 加载、解析 LRC 歌词，并按播放进度定位当前应高亮的行。
//!
//! ## 数据来源
//!
//! 1. 外挂 `.lrc`（同目录同名，如 `稻香.mp3` → `稻香.lrc`）——优先；
//! 2. 内嵌歌词（音频文件标签里的 Lyrics，如 ID3v2 USLT / Vorbis LYRICS），
//!    通过 metadata 模块提取、`Lyrics::from_embedded` 解析——无 .lrc 时兜底。
//!
//! ## 编码
//!
//! 歌词文件编码混乱（老文件常为 GBK，新文件为 UTF-8）。用 `encoding_rs`
//! 自动检测：UTF-8/UTF-16 BOM → UTF-8 严格 → GB18030（兼容 GBK，全映射不失败）。

use std::cmp::Ordering;
use std::path::Path;

use encoding_rs::{GB18030, UTF_16BE, UTF_16LE, UTF_8};

/// 一行歌词。
#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    /// 时间戳（秒）。
    pub timestamp: f64,
    /// 歌词文本。
    pub text: String,
}

/// 整首歌词（按时间戳升序）。
#[derive(Debug, Clone, Default)]
pub struct Lyrics {
    /// 按时间戳升序排列的歌词行。
    pub lines: Vec<LyricLine>,
}

impl Lyrics {
    /// 从 LRC 文本解析。
    ///
    /// 忽略 `[ti:]`、`[ar:]` 等元数据行；只保留带 `[mm:ss.xx]` 时间戳的行。
    /// 同一行多个时间戳（副歌重复）会拆成多行。
    /// `[offset:±毫秒]` 标签会整体平移所有时间戳（正值 = 歌词延后）。
    pub fn parse(text: &str) -> Self {
        // offset 标签可能出现在任意位置，先收集原始行，最后统一平移。
        let mut offset_secs: f64 = 0.0;
        let mut raw: Vec<(f64, String)> = Vec::new();
        for line in text.lines() {
            if let Some(off_ms) = parse_offset(line) {
                offset_secs = off_ms / 1000.0;
                continue;
            }
            for (timestamp, text) in parse_line(line) {
                if !text.is_empty() {
                    raw.push((timestamp, text));
                }
            }
        }
        let mut lines: Vec<LyricLine> = raw
            .into_iter()
            .map(|(ts, text)| LyricLine {
                timestamp: ts + offset_secs,
                text,
            })
            .collect();
        // 按时间戳升序（部分歌词文件可能乱序）
        lines.sort_by(|a, b| {
            a.timestamp
                .partial_cmp(&b.timestamp)
                .unwrap_or(Ordering::Equal)
        });
        Self { lines }
    }

    /// 从文件加载（自动检测编码）。失败返回 None。
    pub fn load_from_file(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        let text = decode_bytes(&bytes);
        Some(Self::parse(&text))
    }

    /// 从内嵌歌词文本解析（来自文件标签，如 ID3v2 USLT / Vorbis LYRICS）。
    ///
    /// 复用 [`Self::parse`] 的 LRC 解析。symphonia 提取的 USLT/LYRICS 常见是
    /// 带时间戳的 LRC 文本（网易云音乐等打标签工具的内嵌格式），可直接解析。
    ///
    /// 返回 None 的情况（调用方据此视为“无歌词”而不是显示一片空白）：
    /// - 文本为空；
    /// - 文本没有任何带时间戳的行（纯文本歌词无法定位高亮行——
    ///   纯文本歌词展示属后续工作）。
    pub fn from_embedded(text: &str) -> Option<Self> {
        let lyrics = Self::parse(text);
        if lyrics.is_empty() {
            None
        } else {
            Some(lyrics)
        }
    }

    /// 根据播放位置（秒）返回当前应高亮的行索引。
    ///
    /// 返回最后一个 `timestamp <= position` 的行；位置早于第一行时返回 0。
    pub fn current_line(&self, position: f64) -> usize {
        match self.lines.binary_search_by(|l| {
            l.timestamp
                .partial_cmp(&position)
                .unwrap_or(Ordering::Equal)
        }) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// 是否有歌词。
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// 解析 `[offset:±毫秒]` 标签。不是 offset 标签返回 None。
fn parse_offset(line: &str) -> Option<f64> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix("[offset:")?.strip_suffix(']')?;
    inner.trim().parse::<f64>().ok()
}

/// 解析一行 LRC，返回该行的所有 (时间戳, 文本) 对。
fn parse_line(line: &str) -> Vec<(f64, String)> {
    let mut timestamps = Vec::new();
    let mut rest = line;
    // 逐个提取 [xxx] 标签，能解析成时间戳的收集起来
    while let Some(start) = rest.find('[') {
        let Some(rel_end) = rest[start + 1..].find(']') else {
            break;
        };
        let end = start + 1 + rel_end;
        let tag = &rest[start + 1..end];
        if let Some(ts) = parse_timestamp(tag) {
            timestamps.push(ts);
        }
        rest = &rest[end + 1..];
    }
    let text = rest.trim().to_string();
    timestamps
        .into_iter()
        .map(|ts| (ts, text.clone()))
        .collect()
}

/// 解析 `mm:ss.xx` 或 `mm:ss` 为秒数。非法返回 None。
fn parse_timestamp(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let minutes: f64 = parts.next()?.trim().parse().ok()?;
    let seconds_part = parts.next()?;
    let mut sec_parts = seconds_part.split('.');
    let seconds: f64 = sec_parts.next()?.parse().ok()?;
    // 小数部分：按位数换算（2 位 = 厘秒，3 位 = 毫秒）
    let fraction = sec_parts
        .next()
        .map(|f| {
            let digits: f64 = f.parse().unwrap_or(0.0);
            digits / 10f64.powi(f.len() as i32)
        })
        .unwrap_or(0.0);
    Some(minutes * 60.0 + seconds + fraction)
}

/// 解码歌词文件字节为 UTF-8 字符串（BOM → UTF-8 严格 → GB18030）。
fn decode_bytes(bytes: &[u8]) -> String {
    // UTF-8 BOM
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return UTF_8.decode(&bytes[3..]).0.into_owned();
    }
    // UTF-16 LE BOM
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return UTF_16LE.decode(&bytes[2..]).0.into_owned();
    }
    // UTF-16 BE BOM
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return UTF_16BE.decode(&bytes[2..]).0.into_owned();
    }
    // 无 BOM：尝试 UTF-8 严格解码，失败则按 GB18030（兼容 GBK）解码
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => GB18030.decode(bytes).0.into_owned(),
    }
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_lrc() {
        let text = "[ti:测试]\n[00:01.00]第一句\n[00:05.50]第二句\n[00:10]第三句\n";
        let lyrics = Lyrics::parse(text);
        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].timestamp, 1.0);
        assert_eq!(lyrics.lines[0].text, "第一句");
        assert_eq!(lyrics.lines[1].timestamp, 5.5);
        assert_eq!(lyrics.lines[2].timestamp, 10.0);
        assert_eq!(lyrics.lines[2].text, "第三句");
    }

    #[test]
    fn parse_multiple_timestamps() {
        // 同一行两个时间戳（副歌重复）
        let text = "[00:01.00][00:30.00]重复的副歌\n";
        let lyrics = Lyrics::parse(text);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].timestamp, 1.0);
        assert_eq!(lyrics.lines[1].timestamp, 30.0);
        assert_eq!(lyrics.lines[0].text, "重复的副歌");
    }

    #[test]
    fn parse_skips_metadata() {
        let text = "[ti:标题]\n[ar:歌手]\n[00:02.00]实际歌词\n";
        let lyrics = Lyrics::parse(text);
        assert_eq!(lyrics.lines.len(), 1);
        assert_eq!(lyrics.lines[0].text, "实际歌词");
        assert_eq!(lyrics.lines[0].timestamp, 2.0);
    }

    #[test]
    fn parse_offset_shifts_timestamps() {
        // 正 offset（毫秒）：时间戳整体延后
        let text = "[offset:500]\n[00:02.00]第一句\n[00:05.00]第二句\n";
        let lyrics = Lyrics::parse(text);
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].timestamp, 2.5);
        assert_eq!(lyrics.lines[1].timestamp, 5.5);

        // 负 offset：时间戳整体提前
        let l2 = Lyrics::parse("[offset:-300]\n[00:02.00]X\n");
        assert_eq!(l2.lines[0].timestamp, 1.7);
    }

    #[test]
    fn current_line_binary_search() {
        let text = "[00:00.00]A\n[00:10.00]B\n[00:20.00]C\n";
        let lyrics = Lyrics::parse(text);
        assert_eq!(lyrics.current_line(0.0), 0);
        assert_eq!(lyrics.current_line(5.0), 0);
        assert_eq!(lyrics.current_line(10.0), 1);
        assert_eq!(lyrics.current_line(19.9), 1);
        assert_eq!(lyrics.current_line(20.0), 2);
        assert_eq!(lyrics.current_line(999.0), 2);
    }

    #[test]
    fn from_embedded_lrc_text() {
        // 内嵌 LRC 文本（带时间戳）：正常解析
        let text = "[00:01.00]内嵌第一句\n[00:05.00]内嵌第二句\n";
        let lyrics = Lyrics::from_embedded(text).expect("带时间戳的内嵌歌词应解析成功");
        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "内嵌第一句");
        assert_eq!(lyrics.lines[1].timestamp, 5.0);
    }

    #[test]
    fn from_embedded_plain_or_empty_is_none() {
        // 纯文本歌词（无时间戳）：无法定位高亮行，返回 None
        assert!(Lyrics::from_embedded("第一行\n第二行\n").is_none());
        // 空文本
        assert!(Lyrics::from_embedded("").is_none());
        // 只有元数据标签的文本
        assert!(Lyrics::from_embedded("[ti:标题]\n").is_none());
    }

    #[test]
    fn decode_utf8_and_gbk() {
        // UTF-8 无 BOM
        let utf8 = "你好".as_bytes();
        assert_eq!(decode_bytes(utf8), "你好");
        // UTF-8 BOM
        let utf8_bom = [0xEF, 0xBB, 0xBF, 0xE4, 0xBD, 0xA0, 0xE5, 0xA5, 0xBD];
        assert_eq!(decode_bytes(&utf8_bom), "你好");
        // GBK（"你好" = C4 E3 BA C3）
        let gbk = [0xC4, 0xE3, 0xBA, 0xC3];
        assert_eq!(decode_bytes(&gbk), "你好");
    }
}
