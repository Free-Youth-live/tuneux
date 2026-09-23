//! # CUE 分轨索引解析
//!
//! 解析标准 `.cue` 文本（红皮书 CUE 语法子集），提取曲目分轨信息，
//! 供播放器把"整轨音频文件 + .cue"展开为多首可独立播放的曲目。
//! 纯 Rust、零 unsafe；`#![deny(missing_docs)]` 已覆盖。
//!
//! 扩展（光盘媒体需求）：
//! - 新增 [`CueSheet`]（专辑级 TITLE / PERFORMER / FILE 引用）与
//!   [`parse_cue_sheet`]；原 [`parse_cue`] 签名不变（向后兼容）；
//! - [`CueTrack`] 新增：INDEX 00 间隙起点、曲目终点、轨迹类型
//!   （数据轨标记不可播）、起始扇区换算；
//! - [`decode_cue_bytes`] 编码链自发行版下沉（UTF-8 → BOM → GB18030，
//!   中文圈 .cue 的硬需求）。

use std::fmt;

/// 一条 CUE 曲目。
#[derive(Debug, Clone, PartialEq)]
pub struct CueTrack {
    /// 曲目号（1-based，按 CUE 文件中的 `TRACK n AUDIO` 序号）。
    pub index: u32,
    /// 曲目标题（TRACK 内的 `TITLE "..."`；缺失时回退为 "Track {index}"）。
    pub title: String,
    /// 表演者（TRACK 内的 `PERFORMER "..."`，可选）。
    pub performer: Option<String>,
    /// 起始时间（秒，来自 `INDEX 01 MM:SS:FF`，FF 为帧，75 帧/秒）。
    pub start_secs: f64,
    /// 间隙起点（秒，来自 `INDEX 00`；无 INDEX 00 时为 None）。
    /// 间隙 = 上一曲结束到本曲 INDEX 01 之间的区间（CD 标准 2 秒 pre-gap，
    /// 也可能藏有隐藏音轨）。
    pub pregap_start_secs: Option<f64>,
    /// 终点时间（秒）= 下一曲的 INDEX 01 起点；末曲为 None（播到文件末尾）。
    pub end_secs: Option<f64>,
    /// 是否音频轨（`TRACK n AUDIO`）；`MODE1/MODE2` 等数据轨为 false
    ///（数据轨不可播：选中时调用方应明确报"不支持"，不出声不崩溃）。
    pub is_audio: bool,
}

impl CueTrack {
    /// 起始扇区号（CD-DA 规格：75 扇区/秒，向下取整）。
    pub fn start_sector(&self) -> u64 {
        // 整数运算避免浮点截断：f64 × 75 再 as u64 在 5.34% 的
        // MM:SS:FF 组合上少一扇区（如 00:00:55 → 54.999.. → 54）。
        // 改用秒的整数部分 × 75 + 小数部分（帧）直接取整。
        let whole = self.start_secs.trunc() as u64;
        let frac = self.start_secs - self.start_secs.trunc();
        // frac 是 0..1 的帧数/75，乘 75 后加 0.5 四舍五入到最近整数
        //（浮点乘法此处安全：帧数是精确的 n/75，≤ 74）。
        let frames = (frac * 75.0 + 0.5) as u64;
        whole * 75 + frames
    }

    /// 曲目时长（秒；末曲无终点时为 None，由调用方用文件时长补）。
    pub fn duration_secs(&self) -> Option<f64> {
        self.end_secs.map(|e| (e - self.start_secs).max(0.0))
    }
}

/// 解析后的 CUE Sheet（专辑级信息 + 文件引用 + 曲目表）。
#[derive(Debug, Clone, PartialEq)]
pub struct CueSheet {
    /// 专辑标题（第一个 TRACK 之前的 `TITLE "..."`）。
    pub album_title: Option<String>,
    /// 专辑表演者（第一个 TRACK 之前的 `PERFORMER "..."`）。
    pub album_performer: Option<String>,
    /// FILE 字段引用的音频文件名（未解析为路径——相对 .cue 所在目录，
    /// 由调用方拼接；多 FILE 段暂不支持，解析即报错）。
    pub audio_file: Option<String>,
    /// 曲目列表（按文件中出现的顺序）。
    pub tracks: Vec<CueTrack>,
}

impl CueSheet {
    /// 按曲目号查曲目（1-based）。
    pub fn track(&self, number: u32) -> Option<&CueTrack> {
        self.tracks.iter().find(|t| t.index == number)
    }

    /// 曲目数。
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// 可播音频轨迭代（滤掉数据轨）。
    pub fn audio_tracks(&self) -> impl Iterator<Item = &CueTrack> {
        self.tracks.iter().filter(|t| t.is_audio)
    }
}

/// CUE 解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueParseError {
    /// 语法错误（行号 + 说明）。
    Syntax {
        /// 出错行号（1-based）。
        line: u32,
        /// 错误说明。
        message: String,
    },
}

impl fmt::Display for CueParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { line, message } => write!(f, "CUE 语法错误（第 {line} 行）：{message}"),
        }
    }
}

impl std::error::Error for CueParseError {}

/// 将 `.cue` 文件字节解码为 UTF-8 文本。
///
/// 解码顺序：UTF-8（最快路径）→ BOM 探测（UTF-16LE/BE）→ GB18030
/// （中文 Windows 生成的 `.cue` 常见编码，GBK 为其子集）。
/// 该链路原在各产品层各自实现，后下沉 corex 复用。
#[must_use]
pub fn decode_cue_bytes(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    if let Some((enc, bom_len)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode(&bytes[bom_len..]).0.into_owned();
    }
    encoding_rs::GB18030.decode(bytes).0.into_owned()
}

/// 解析 CUE 文本为完整 [`CueSheet`]（专辑级信息 + 曲目表）。
///
/// 支持的语法子集：`FILE`、`TRACK`、`TRACK` 内 `TITLE`/`PERFORMER`/
/// `INDEX 00`/`INDEX 01`；`REM`/`CATALOG`/`FLAGS` 等其余行忽略。
/// 多 FILE 段 CUE 明确拒绝（当前展开逻辑不支持，v2 再议）。
pub fn parse_cue_sheet(content: &str) -> Result<CueSheet, CueParseError> {
    // 去除 UTF-8 BOM（部分工具生成的 .cue 带 BOM，会使首行关键字失配）
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut tracks: Vec<CueTrack> = Vec::new();
    let mut current: Option<CueTrack> = None;
    let mut file_count = 0u32;
    let mut audio_file: Option<String> = None;
    let mut album_title: Option<String> = None;
    let mut album_performer: Option<String> = None;

    for (i, raw_line) in content.lines().enumerate() {
        let line_no = (i + 1) as u32;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();

        if upper.starts_with("REM ")
            || upper.starts_with("CATALOG ")
            || upper.starts_with("CDTEXTFILE ")
        {
            continue;
        }
        // 多 FILE 段 CUE（一张 .cue 引用多个音频文件）：当前展开逻辑不支持，
        // 明确拒绝而非错误展开。
        if upper.starts_with("FILE ") {
            file_count += 1;
            if file_count > 1 {
                return Err(CueParseError::Syntax {
                    line: line_no,
                    message: "多 FILE 段 CUE 暂不支持（当前仅支持单整轨 + 多 INDEX 分轨）"
                        .to_string(),
                });
            }
            // FILE "name" TYPE：文件名取引号段（无引号取首个空白分隔段）。
            let raw = &line["FILE ".len()..];
            audio_file = Some(parse_file_name(raw));
            continue;
        }

        if upper.starts_with("TRACK ") {
            if let Some(t) = current.take() {
                tracks.push(t);
            }
            // TRACK 号必须为十进制数字（如 "01"）；缺失/非法直接报错，
            // 避免伪序号污染排序与显示。
            let idx = line["TRACK ".len()..]
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| CueParseError::Syntax {
                    line: line_no,
                    message: "TRACK 后缺少合法的十进制曲目号（如 TRACK 01 AUDIO）".to_string(),
                })?;
            // 轨迹类型：AUDIO 可播，MODE1/MODE2 等数据轨标记不可播。
            let mode = line["TRACK ".len()..]
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_uppercase();
            current = Some(CueTrack {
                index: idx,
                title: format!("Track {idx}"),
                performer: None,
                start_secs: 0.0,
                pregap_start_secs: None,
                end_secs: None,
                is_audio: mode == "AUDIO",
            });
            continue;
        }

        match current.as_mut() {
            Some(track) => {
                if upper.starts_with("TITLE ") {
                    let raw = &line["TITLE ".len()..];
                    track.title = parse_quoted(raw).unwrap_or_else(|| raw.trim().to_string());
                } else if upper.starts_with("PERFORMER ") {
                    let raw = &line["PERFORMER ".len()..];
                    track.performer =
                        Some(parse_quoted(raw).unwrap_or_else(|| raw.trim().to_string()));
                } else if upper.starts_with("INDEX ") {
                    let mut parts = line["INDEX ".len()..].split_whitespace();
                    let num: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                    let time_str = parts.next();
                    match num {
                        0 => {
                            // INDEX 00 = 间隙起点（可选；格式非法同样报错，防静默错位）
                            if let Some(t) = time_str {
                                track.pregap_start_secs =
                                    Some(parse_cue_time(t).ok_or_else(|| {
                                        CueParseError::Syntax {
                                            line: line_no,
                                            message: "INDEX 00 时间格式应为 MM:SS:FF".to_string(),
                                        }
                                    })?);
                            }
                        }
                        1 => {
                            let secs = time_str.and_then(parse_cue_time).ok_or_else(|| {
                                CueParseError::Syntax {
                                    line: line_no,
                                    message: "INDEX 01 时间格式应为 MM:SS:FF".to_string(),
                                }
                            })?;
                            track.start_secs = secs;
                        }
                        _ => {} // INDEX 02+（少见的多索引）：忽略
                    }
                }
                // FLAGS / POSTGAP / PREGAP / PRE-EMPHASIS 等：忽略
            }
            None => {
                // 第一个 TRACK 之前：专辑级 TITLE / PERFORMER。
                if upper.starts_with("TITLE ") {
                    let raw = &line["TITLE ".len()..];
                    album_title = Some(parse_quoted(raw).unwrap_or_else(|| raw.trim().to_string()));
                } else if upper.starts_with("PERFORMER ") {
                    let raw = &line["PERFORMER ".len()..];
                    album_performer =
                        Some(parse_quoted(raw).unwrap_or_else(|| raw.trim().to_string()));
                }
            }
        }
    }

    // 收尾：检查最后一首；并校验每首都有合法的 INDEX 01 起点
    if let Some(t) = current.take() {
        tracks.push(t);
    }
    if tracks.is_empty() {
        return Err(CueParseError::Syntax {
            line: 1,
            message: "未找到任何 TRACK 条目".to_string(),
        });
    }
    for (pos, t) in tracks.iter().enumerate() {
        // INDEX 01 未出现（如仅 INDEX 00 pregap）时 start_secs 保持 0，
        // 非首曲会全部从文件头起播——明确报错；
        // 第一首从 0 起是合法情形，跳过。
        if pos > 0 && t.start_secs == 0.0 {
            return Err(CueParseError::Syntax {
                line: 1,
                message: format!(
                    "曲目 {} 缺少 INDEX 01 起点（仅 INDEX 00 pregap 不被支持）",
                    t.index
                ),
            });
        }
    }
    // 终点推算：每曲终点 = 下一曲 INDEX 01 起点；末曲 None（播到文件末尾）。
    for i in 0..tracks.len().saturating_sub(1) {
        let next_start = tracks[i + 1].start_secs;
        tracks[i].end_secs = Some(next_start);
    }
    Ok(CueSheet {
        album_title,
        album_performer,
        audio_file,
        tracks,
    })
}

/// 解析 CUE 文本，返回按文件顺序排列的曲目列表（向后兼容签名；
/// 需要专辑级信息时用 [`parse_cue_sheet`]）。
pub fn parse_cue(content: &str) -> Result<Vec<CueTrack>, CueParseError> {
    parse_cue_sheet(content).map(|s| s.tracks)
}

/// 解析 `MM:SS:FF` 时间（FF 为帧，75 帧/秒）为秒数。
fn parse_cue_time(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let mm: u32 = parts.next()?.parse().ok()?;
    let ss: u32 = parts.next()?.parse().ok()?;
    let ff: u32 = parts.next()?.parse().ok()?;
    if ss >= 60 || ff >= 75 {
        return None;
    }
    Some(f64::from(mm) * 60.0 + f64::from(ss) + f64::from(ff) / 75.0)
}

/// 解析引号包裹的字符串 `"..."`；非引号形式返回 None。
fn parse_quoted(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        Some(s[1..s.len() - 1].to_string())
    } else {
        None
    }
}

/// 解析 FILE 行的文件名：优先取第一个引号段（`"name" TYPE`），
/// 无引号则取首个空白分隔段（`name TYPE`）。
fn parse_file_name(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            return rest[..end].to_string();
        }
    }
    s.split_whitespace().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 基本解析：三首曲目、标题/表演者/起始时间齐全。
    #[test]
    fn parses_basic_tracks() {
        let cue = r#"
REM GENRE "Classical"
FILE "album.flac" WAVE
  TRACK 01 AUDIO
    TITLE "第一乐章"
    PERFORMER "乐团A"
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    TITLE "第二乐章"
    PERFORMER "乐团A"
    INDEX 01 05:30:00
  TRACK 03 AUDIO
    TITLE "第三乐章"
    PERFORMER "乐团A"
    INDEX 01 12:45:37
"#;
        let tracks = parse_cue(cue).unwrap();
        assert_eq!(tracks.len(), 3);
        assert_eq!(tracks[0].index, 1);
        assert_eq!(tracks[0].title, "第一乐章");
        assert_eq!(tracks[0].performer.as_deref(), Some("乐团A"));
        assert_eq!(tracks[0].start_secs, 0.0);
        assert_eq!(tracks[1].start_secs, 330.0);
        // 12*60 + 45 + 37/75
        assert!((tracks[2].start_secs - (12.0 * 60.0 + 45.0 + 37.0 / 75.0)).abs() < 1e-9);
    }

    /// 无引号标题与缺失 TITLE 的回退。
    #[test]
    fn handles_unquoted_and_missing_title() {
        let cue = "TRACK 01 AUDIO\n  INDEX 01 00:00:00\nTRACK 02 AUDIO\n  TITLE PlainTitle\n  INDEX 01 01:00:00\n";
        let tracks = parse_cue(cue).unwrap();
        assert_eq!(tracks[0].title, "Track 1");
        assert_eq!(tracks[1].title, "PlainTitle");
    }

    /// 空文件 / 无 TRACK 报错。
    #[test]
    fn errors_on_no_tracks() {
        assert!(parse_cue("REM nothing here\n").is_err());
        assert!(parse_cue("").is_err());
    }

    /// 非法时间格式报错。
    #[test]
    fn errors_on_bad_time() {
        let cue = "TRACK 01 AUDIO\n  INDEX 01 99:99:99\n";
        assert!(parse_cue(cue).is_err());
    }

    /// TRACK 号缺失（如 "TRACK AUDIO"）应报错，而非伪序号兜底。
    #[test]
    fn track_number_missing_errors() {
        let cue = "TRACK AUDIO\n  INDEX 01 00:00:00\n";
        assert!(parse_cue(cue).is_err());
    }

    /// 多 FILE 段 CUE（一张 .cue 引用多个音频文件）应明确拒绝。
    #[test]
    fn multi_file_rejected() {
        let cue = "FILE \"a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\nFILE \"b.flac\" WAVE\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n";
        assert!(parse_cue(cue).is_err());
    }

    /// 非首曲缺少 INDEX 01（如仅 INDEX 00 pregap）应报错，避免全部从文件头起播。
    #[test]
    fn missing_index01_errors() {
        let cue = "TRACK 01 AUDIO\n  INDEX 01 00:00:00\nTRACK 02 AUDIO\n  INDEX 00 01:00:00\n";
        assert!(parse_cue(cue).is_err());
    }

    /// UTF-8 BOM 前缀不应影响解析。
    #[test]
    fn bom_is_ignored() {
        let cue =
            "\u{feff}TRACK 01 AUDIO\n  INDEX 01 00:00:00\nTRACK 02 AUDIO\n  INDEX 01 01:00:00\n";
        let tracks = parse_cue(cue).unwrap();
        assert_eq!(tracks.len(), 2);
    }

    /// 75 帧/秒换算精度与帧号边界（合法范围 0-74）。
    #[test]
    fn frame_precision() {
        assert!((parse_cue_time("00:00:37").unwrap() - 37.0 / 75.0).abs() < 1e-9);
        assert_eq!(parse_cue_time("01:30:00"), Some(90.0));
        // 帧号 75 非法（0-74），应拒绝
        assert_eq!(parse_cue_time("00:00:75"), None);
        assert_eq!(parse_cue_time("bad"), None);
    }

    // -----------------------------------------------------------------
    // 2026-09-16 扩展测试
    // -----------------------------------------------------------------

    /// CueSheet：专辑级 TITLE/PERFORMER + FILE 引用 + 终点推算。
    #[test]
    fn sheet_album_fields_and_end_secs() {
        let cue = r#"TITLE "测试专辑"
PERFORMER "测试乐团"
FILE "album.bin" BINARY
  TRACK 01 AUDIO
    INDEX 01 00:00:00
  TRACK 02 AUDIO
    INDEX 01 03:00:00
  TRACK 03 AUDIO
    INDEX 01 07:30:00
"#;
        let sheet = parse_cue_sheet(cue).unwrap();
        assert_eq!(sheet.album_title.as_deref(), Some("测试专辑"));
        assert_eq!(sheet.album_performer.as_deref(), Some("测试乐团"));
        assert_eq!(sheet.audio_file.as_deref(), Some("album.bin"));
        assert_eq!(sheet.track_count(), 3);
        assert_eq!(sheet.tracks[0].end_secs, Some(180.0));
        assert_eq!(sheet.tracks[1].end_secs, Some(450.0));
        assert_eq!(sheet.tracks[2].end_secs, None); // 末曲播到文件末尾
        assert_eq!(sheet.tracks[0].duration_secs(), Some(180.0));
        assert!(sheet.track(2).is_some());
        assert!(sheet.track(9).is_none());
    }

    /// INDEX 00 间隙起点 + 起始扇区换算。
    #[test]
    fn pregap_and_sector() {
        let cue = "FILE \"a.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 01:28:00\n    INDEX 01 01:30:00\n";
        let sheet = parse_cue_sheet(cue).unwrap();
        let t2 = sheet.track(2).unwrap();
        assert!((t2.pregap_start_secs.unwrap() - 88.0).abs() < 1e-9);
        assert!((t2.start_secs - 90.0).abs() < 1e-9);
        assert_eq!(t2.start_sector(), 90 * 75);
        // 无 INDEX 00 的曲目为 None。
        assert!(sheet.track(1).unwrap().pregap_start_secs.is_none());
    }

    /// 数据轨（MODE1）标记不可播；audio_tracks 过滤。
    #[test]
    fn data_track_marked_unplayable() {
        let cue = "FILE \"a.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n";
        let sheet = parse_cue_sheet(cue).unwrap();
        assert!(!sheet.tracks[0].is_audio);
        assert!(sheet.tracks[1].is_audio);
        assert_eq!(sheet.audio_tracks().count(), 1);
    }

    /// decode_cue_bytes：UTF-8 / UTF-16LE(BOM) / GB18030 三链。
    #[test]
    fn decode_bytes_encoding_chain() {
        // UTF-8。
        assert_eq!(decode_cue_bytes("标题".as_bytes()), "标题");
        // UTF-16LE 带 BOM。
        let mut utf16 = vec![0xFFu8, 0xFE];
        utf16.extend("标".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode_cue_bytes(&utf16), "标");
        // GB18030（「标」= B1 EA）。
        assert_eq!(decode_cue_bytes(&[0xB1, 0xEA]), "标");
    }

    /// FILE 无引号文件名回退。
    #[test]
    fn file_unquoted_fallback() {
        let cue = "FILE album.wav WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let sheet = parse_cue_sheet(cue).unwrap();
        assert_eq!(sheet.audio_file.as_deref(), Some("album.wav"));
    }
}
