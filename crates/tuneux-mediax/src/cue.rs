//! # CUE 分轨索引解析与展开（音频领域数据层）
//!
//! 解析标准 `.cue` 文本（红皮书 CUE 语法子集），并把「整轨音频文件 + .cue」
//! 或「cue 文件（按 FILE 引用镜像）」展开为可独立播放的 [`PlaylistItem`]
//! 分轨条目。纯 Rust、零 unsafe；`#![deny(missing_docs)]` 已覆盖。
//!
//! 归属（2026-09-20 边界修订确定）：cue 是**音频领域数据**（文本 → 数据
//! 变换），自 corex 迁出、与双端展开层合一，收口到 mediax 一个完整模块——
//! 内核 corex 只接受 f64 秒的区间语义，不解析 `CueTrack` / `CueSheet` 文本。

use std::fmt;
use std::path::{Path, PathBuf};

use crate::metadata::TrackMetadata;
use crate::playlist_item::{CueRef, PlaylistItem};

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
    pub pregap_start_secs: Option<f64>,
    /// 终点时间（秒）= 下一曲的 INDEX 01 起点；末曲为 None（播到文件末尾）。
    pub end_secs: Option<f64>,
    /// 是否音频轨（`TRACK n AUDIO`）；`MODE1/MODE2` 等数据轨为 false。
    pub is_audio: bool,
}

impl CueTrack {
    /// 起始扇区号（CD-DA 规格：75 扇区/秒，向下取整）。
    pub fn start_sector(&self) -> u64 {
        let whole = self.start_secs.trunc() as u64;
        let frac = self.start_secs - self.start_secs.trunc();
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
    /// FILE 字段引用的音频文件名（未解析为路径；相对 .cue 所在目录）。
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
        if upper.starts_with("FILE ") {
            file_count += 1;
            if file_count > 1 {
                return Err(CueParseError::Syntax {
                    line: line_no,
                    message: "多 FILE 段 CUE 暂不支持（当前仅支持单整轨 + 多 INDEX 分轨）"
                        .to_string(),
                });
            }
            let raw = &line["FILE ".len()..];
            audio_file = Some(parse_file_name(raw));
            continue;
        }

        if upper.starts_with("TRACK ") {
            if let Some(t) = current.take() {
                tracks.push(t);
            }
            let idx = line["TRACK ".len()..]
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .ok_or_else(|| CueParseError::Syntax {
                    line: line_no,
                    message: "TRACK 后缺少合法的十进制曲目号（如 TRACK 01 AUDIO）".to_string(),
                })?;
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
                        _ => {}
                    }
                }
            }
            None => {
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

// =============================================================================
// 展开层：CueSheet → PlaylistItem（纯函数，读磁盘不碰 App 状态）
// =============================================================================

/// 展开结果：分轨条目 + 跳过的数据轨数（供 UI 提示）。
pub type CueExpansion = (Vec<PlaylistItem>, usize);

/// 把解析好的 CueSheet 展开为分轨条目（纯函数）。数据轨跳过并计数。
fn expand_sheet(sheet: &CueSheet, audio_path: &Path, md: &TrackMetadata) -> CueExpansion {
    let track_duration_ms = md.duration.map(|d| (d * 1000.0) as u64);
    let mut skipped = 0usize;
    let items = sheet
        .tracks
        .iter()
        .filter(|t| {
            if t.is_audio {
                true
            } else {
                skipped += 1;
                false
            }
        })
        .map(|t| {
            let end_ms = t
                .end_secs
                .map(|e| (e * 1000.0) as u64)
                .or(track_duration_ms);
            PlaylistItem {
                path: audio_path.to_path_buf(),
                album: md.album.clone(),
                track_number: Some(t.index),
                cue: Some(CueRef {
                    index: t.index,
                    title: t.title.clone(),
                    performer: t.performer.clone(),
                    start_ms: (t.start_secs * 1000.0) as u64,
                    end_ms,
                }),
            }
        })
        .collect();
    (items, skipped)
}

/// 读 + 解析 .cue 文件（编码链 UTF-8 → BOM → GB18030）。
fn read_cue_sheet(cue_path: &Path) -> Option<CueSheet> {
    let bytes = std::fs::read(cue_path).ok()?;
    let content = decode_cue_bytes(&bytes);
    parse_cue_sheet(&content).ok()
}

/// FILE 字段解析为实际音频路径：cue 所在目录 + FILE 名；文件不存在返回 None。
fn resolve_audio_path(cue_path: &Path, sheet: &CueSheet) -> Option<PathBuf> {
    let name = sheet.audio_file.as_deref()?;
    let dir = cue_path.parent().unwrap_or_else(|| Path::new("."));
    let p = dir.join(name);
    p.is_file().then_some(p)
}

/// 老入口：整轨音频文件 + 同名 `.cue`。返回空 = 调用方回退普通条目。
///
/// FILE 字段优先：cue 是索引权威——FILE 指向别的文件（如镜像 .bin）时
/// 以 FILE 为准并重新探测其元数据；FILE 缺失 / 指向本路径 / 文件不存在
/// 时回落传入的整轨路径（沿用调用方已探测的元数据，缓存语义不破）。
pub fn cue_items_from(path: &Path, md: &TrackMetadata) -> CueExpansion {
    let cue_path = path.with_extension("cue");
    if !cue_path.is_file() {
        return (Vec::new(), 0);
    }
    let Some(sheet) = read_cue_sheet(&cue_path) else {
        return (Vec::new(), 0);
    };
    match resolve_audio_path(&cue_path, &sheet) {
        Some(p) if p.as_path() != path => {
            let md2 = TrackMetadata::from_file(&p);
            expand_sheet(&sheet, &p, &md2)
        }
        _ => expand_sheet(&sheet, path, md),
    }
}

/// 新入口：`.cue` 文件本身被点中 → 按 FILE 引用展开为分轨条目。
///
/// 返回（条目, 跳过数据轨数, 实际音频路径）；cue 解析失败 / FILE 缺失 /
/// 音频文件不存在 / 全是数据轨 → None（调用方提示用户）。
pub fn cue_items_from_cue_file(cue_path: &Path) -> Option<(Vec<PlaylistItem>, usize, PathBuf)> {
    let sheet = read_cue_sheet(cue_path)?;
    let audio_path = resolve_audio_path(cue_path, &sheet)?;
    let md = TrackMetadata::from_file(&audio_path);
    let (items, skipped) = expand_sheet(&sheet, &audio_path, &md);
    if items.is_empty() {
        return None;
    }
    Some((items, skipped, audio_path))
}

/// 把一批已收集的音乐文件路径构建为播放列表条目：整轨 + 同名 `.cue`
/// 展开为分轨，其余按普通条目；元数据**缓存优先**（未命中才探测文件）。
/// 纯函数（只读磁盘、无 App 状态），可在后台线程运行。
/// 第三元返回值 = 本批跳过的数据轨总数（供 UI 一次性提示）。
#[allow(clippy::type_complexity)]
pub fn build_items_for_paths(
    paths: &[PathBuf],
    cache: &std::collections::HashMap<PathBuf, TrackMetadata>,
) -> (Vec<PlaylistItem>, Vec<(PathBuf, TrackMetadata)>, usize) {
    let mut items = Vec::new();
    let mut probed = Vec::new();
    let mut skipped_total = 0usize;
    for path in paths {
        let (md, from_probe) = match cache.get(path) {
            Some(cached) => (cached.clone(), false),
            None => (TrackMetadata::from_file(path), true),
        };
        let (expanded, skipped) = cue_items_from(path, &md);
        skipped_total += skipped;
        if !expanded.is_empty() {
            items.extend(expanded);
        } else {
            items.push(PlaylistItem {
                path: path.clone(),
                album: md.album.clone(),
                track_number: md.track_number,
                cue: None,
            });
        }
        if from_probe {
            let mut cached = md;
            cached.cover = None;
            probed.push((path.clone(), cached));
        }
    }
    (items, probed, skipped_total)
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as IoWrite;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

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
        assert_eq!(tracks[0].title, "第一乐章");
        assert_eq!(tracks[1].start_secs, 330.0);
        assert!((tracks[2].start_secs - (12.0 * 60.0 + 45.0 + 37.0 / 75.0)).abs() < 1e-9);
    }

    #[test]
    fn handles_unquoted_and_missing_title() {
        let cue = "TRACK 01 AUDIO\n  INDEX 01 00:00:00\nTRACK 02 AUDIO\n  TITLE PlainTitle\n  INDEX 01 01:00:00\n";
        let tracks = parse_cue(cue).unwrap();
        assert_eq!(tracks[0].title, "Track 1");
        assert_eq!(tracks[1].title, "PlainTitle");
    }

    #[test]
    fn errors_on_no_tracks() {
        assert!(parse_cue("REM nothing here\n").is_err());
        assert!(parse_cue("").is_err());
    }

    #[test]
    fn errors_on_bad_time() {
        let cue = "TRACK 01 AUDIO\n  INDEX 01 99:99:99\n";
        assert!(parse_cue(cue).is_err());
    }

    #[test]
    fn track_number_missing_errors() {
        let cue = "TRACK AUDIO\n  INDEX 01 00:00:00\n";
        assert!(parse_cue(cue).is_err());
    }

    #[test]
    fn multi_file_rejected() {
        let cue = "FILE \"a.flac\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\nFILE \"b.flac\" WAVE\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n";
        assert!(parse_cue(cue).is_err());
    }

    #[test]
    fn missing_index01_errors() {
        let cue = "TRACK 01 AUDIO\n  INDEX 01 00:00:00\nTRACK 02 AUDIO\n  INDEX 00 01:00:00\n";
        assert!(parse_cue(cue).is_err());
    }

    #[test]
    fn bom_is_ignored() {
        let cue =
            "\u{feff}TRACK 01 AUDIO\n  INDEX 01 00:00:00\nTRACK 02 AUDIO\n  INDEX 01 01:00:00\n";
        let tracks = parse_cue(cue).unwrap();
        assert_eq!(tracks.len(), 2);
    }

    #[test]
    fn frame_precision() {
        assert!((parse_cue_time("00:00:37").unwrap() - 37.0 / 75.0).abs() < 1e-9);
        assert_eq!(parse_cue_time("01:30:00"), Some(90.0));
        assert_eq!(parse_cue_time("00:00:75"), None);
        assert_eq!(parse_cue_time("bad"), None);
    }

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
        assert_eq!(sheet.audio_file.as_deref(), Some("album.bin"));
        assert_eq!(sheet.track_count(), 3);
        assert_eq!(sheet.tracks[0].end_secs, Some(180.0));
        assert_eq!(sheet.tracks[2].end_secs, None);
        assert!(sheet.track(2).is_some());
        assert!(sheet.track(9).is_none());
    }

    #[test]
    fn pregap_and_sector() {
        let cue = "FILE \"a.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 00 01:28:00\n    INDEX 01 01:30:00\n";
        let sheet = parse_cue_sheet(cue).unwrap();
        let t2 = sheet.track(2).unwrap();
        assert!((t2.pregap_start_secs.unwrap() - 88.0).abs() < 1e-9);
        assert_eq!(t2.start_sector(), 90 * 75);
    }

    #[test]
    fn data_track_marked_unplayable() {
        let cue = "FILE \"a.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 03:00:00\n";
        let sheet = parse_cue_sheet(cue).unwrap();
        assert!(!sheet.tracks[0].is_audio);
        assert!(sheet.tracks[1].is_audio);
        assert_eq!(sheet.audio_tracks().count(), 1);
    }

    #[test]
    fn decode_bytes_encoding_chain() {
        assert_eq!(decode_cue_bytes("标题".as_bytes()), "标题");
        let mut utf16 = vec![0xFFu8, 0xFE];
        utf16.extend("标".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode_cue_bytes(&utf16), "标");
        assert_eq!(decode_cue_bytes(&[0xB1, 0xEA]), "标");
    }

    #[test]
    fn file_unquoted_fallback() {
        let cue = "FILE album.wav WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n";
        let sheet = parse_cue_sheet(cue).unwrap();
        assert_eq!(sheet.audio_file.as_deref(), Some("album.wav"));
    }

    /// 模糊化烟雾测试：任意字节序列经 decode → parse 不得 panic。
    ///（真 fuzz 用 cargo-fuzz + 消毒器；此处用确定性伪随机做「永不 panic」的
    /// 回归防线——cue 解析面对的是不可信文件，任何 panic 都是缺陷。）
    #[test]
    fn decode_and_parse_never_panic_on_garbage() {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        for len in 0..=512usize {
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                bytes.push((state >> 32) as u8);
            }
            let text = decode_cue_bytes(&bytes);
            // 结果无论成功与否都不能 panic（Err / None 均可接受）。
            let _ = parse_cue_sheet(&text);
            let _ = parse_cue(&text);
        }
    }

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn make_fixture(cue_content: &str) -> (PathBuf, PathBuf, PathBuf) {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("tuneux-cue-test-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let bin = dir.join("CDImage.bin");
        let mut f = std::fs::File::create(&bin).expect("建 bin");
        f.write_all(&vec![0u8; 2 * 2352]).expect("写 bin");
        drop(f);
        let cue = dir.join("album.cue");
        let mut f = std::fs::File::create(&cue).expect("建 cue");
        f.write_all(cue_content.as_bytes()).expect("写 cue");
        (dir, bin, cue)
    }

    #[test]
    fn file_field_points_to_bin() {
        let (dir, bin, _cue) = make_fixture(
            "FILE \"CDImage.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        );
        let flac = dir.join("album.flac");
        std::fs::write(&flac, b"not really audio").unwrap();
        let md = TrackMetadata::from_file(&flac);
        let (items, skipped) = cue_items_from(&flac, &md);
        assert_eq!(skipped, 0);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, bin, "条目路径应以 cue 的 FILE 为准");
        let cue0 = items[0].cue.as_ref().unwrap();
        assert_eq!(cue0.start_ms, 0);
        assert_eq!(cue0.end_ms, Some(60_000));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_tracks_skipped_and_counted() {
        let (dir, _bin, _cue) = make_fixture(
            "FILE \"CDImage.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 01:00:00\n",
        );
        let flac = dir.join("album.flac");
        std::fs::write(&flac, b"x").unwrap();
        let md = TrackMetadata::from_file(&flac);
        let (items, skipped) = cue_items_from(&flac, &md);
        assert_eq!(skipped, 1);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cue.as_ref().unwrap().index, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cue_file_entry_paths() {
        let (dir, bin, cue) =
            make_fixture("FILE \"CDImage.bin\" BINARY\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n");
        let Some((items, skipped, audio_path)) = cue_items_from_cue_file(&cue) else {
            panic!("应展开成功");
        };
        assert_eq!(skipped, 0);
        assert_eq!(items.len(), 1);
        assert_eq!(audio_path, bin);
        let no_file_cue = dir.join("nofile.cue");
        std::fs::write(&no_file_cue, "TRACK 01 AUDIO\n  INDEX 01 00:00:00\n").unwrap();
        assert!(cue_items_from_cue_file(&no_file_cue).is_none());
        let missing_cue = dir.join("missing.cue");
        std::fs::write(
            &missing_cue,
            "FILE \"nope.wav\" WAVE\n  TRACK 01 AUDIO\n  INDEX 01 00:00:00\n",
        )
        .unwrap();
        assert!(cue_items_from_cue_file(&missing_cue).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fallback_same_file_keeps_legacy_behavior() {
        let (dir, _bin, _cue) = make_fixture(
            "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n    TITLE \"老路径\"\n    INDEX 01 00:00:00\n",
        );
        let flac = dir.join("album.flac");
        std::fs::write(&flac, b"x").unwrap();
        let md = TrackMetadata::from_file(&flac);
        let (items, _) = cue_items_from(&flac, &md);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, flac);
        assert_eq!(items[0].cue.as_ref().unwrap().title, "老路径");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_cue_returns_empty() {
        let seq = TEMP_SEQ.fetch_add(1, AtomicOrdering::SeqCst);
        let lonely = std::env::temp_dir().join(format!(
            "tuneux-cue-lonely-{}-{seq}.flac",
            std::process::id()
        ));
        std::fs::write(&lonely, b"x").unwrap();
        let md = TrackMetadata::from_file(&lonely);
        let (items, skipped) = cue_items_from(&lonely, &md);
        assert!(items.is_empty() && skipped == 0);
        let _ = std::fs::remove_file(&lonely);
    }
}
