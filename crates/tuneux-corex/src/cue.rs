//! # CUE 分轨索引解析
//!
//! 解析标准 `.cue` 文本（红皮书 CUE 语法子集），提取曲目分轨信息，
//! 供播放器把"整轨音频文件 + .cue"展开为多首可独立播放的曲目。
//! 纯 Rust、零依赖、零 unsafe；`#![deny(missing_docs)]` 已覆盖。

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

/// 解析 CUE 文本，返回按文件顺序排列的曲目列表。
///
/// 支持的语法子集：`TRACK`、TRACK 内 `TITLE`/`PERFORMER`/`INDEX 01`；
/// `REM`/`FILE`/`CATALOG`/`FLAGS` 等其余行忽略。
pub fn parse_cue(content: &str) -> Result<Vec<CueTrack>, CueParseError> {
    // 去除 UTF-8 BOM（部分工具生成的 .cue 带 BOM，会使首行关键字失配）
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut tracks: Vec<CueTrack> = Vec::new();
    let mut current: Option<CueTrack> = None;
    let mut file_count = 0u32;

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
            current = Some(CueTrack {
                index: idx,
                title: format!("Track {idx}"),
                performer: None,
                start_secs: 0.0,
            });
        } else if let Some(track) = current.as_mut() {
            if upper.starts_with("TITLE ") {
                let raw = &line["TITLE ".len()..];
                track.title = parse_quoted(raw).unwrap_or_else(|| raw.trim().to_string());
            } else if upper.starts_with("PERFORMER ") {
                let raw = &line["PERFORMER ".len()..];
                track.performer = Some(parse_quoted(raw).unwrap_or_else(|| raw.trim().to_string()));
            } else if upper.starts_with("INDEX ") {
                let mut parts = line["INDEX ".len()..].split_whitespace();
                let num: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                if num == 1 {
                    let time_str = parts.next();
                    let secs =
                        time_str
                            .and_then(parse_cue_time)
                            .ok_or_else(|| CueParseError::Syntax {
                                line: line_no,
                                message: "INDEX 01 时间格式应为 MM:SS:FF".to_string(),
                            })?;
                    track.start_secs = secs;
                }
            }
            // FLAGS / POSTGAP / PREGAP / PRE-EMPHASIS 等：忽略
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
    Ok(tracks)
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
}
