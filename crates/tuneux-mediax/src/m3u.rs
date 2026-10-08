//! # m3u 播放列表解析与序列化
//!
//! 简单 m3u 文本格式：`#EXTM3U` 头 + 每行一个文件路径；`#` 开头的行是
//! 注释/指令（`#EXTINF` 等），解析时忽略。

use std::path::PathBuf;

/// 解析 m3u 文本为路径列表。
///
/// - 跳过空行与 `#` 开头的注释/指令行；
/// - 每行去首尾空白后作为路径（可为相对或绝对路径）。
pub fn parse(content: &str) -> Vec<PathBuf> {
    // 剥除 UTF-8 BOM：带 BOM 的 m3u 首行 `\u{feff}#EXTM3U` 不以 `#` 开头，
    // 会被误判为路径加入列表。
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(PathBuf::from)
        .collect()
}

/// 把路径列表序列化为 m3u 文本（`#EXTM3U` 头 + 每行一个路径）。
pub fn serialize(paths: &[PathBuf]) -> String {
    let mut out = String::from("#EXTM3U\n");
    for p in paths {
        out.push_str(&p.to_string_lossy());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skips_comments_and_blanks() {
        let content = "#EXTM3U\n#EXTINF:210,稻香\n/music/稻香.mp3\n\n/music/晴天.flac\n";
        let paths = parse(content);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/music/稻香.mp3"),
                PathBuf::from("/music/晴天.flac")
            ]
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        let paths = parse("  /a.mp3  \n\t/b.flac\t\n");
        assert_eq!(
            paths,
            vec![PathBuf::from("/a.mp3"), PathBuf::from("/b.flac")]
        );
    }

    #[test]
    fn serialize_roundtrip() {
        let paths = vec![PathBuf::from("/a.mp3"), PathBuf::from("/b.flac")];
        let text = serialize(&paths);
        assert!(text.starts_with("#EXTM3U\n"));
        assert_eq!(parse(&text), paths);
    }

    #[test]
    fn parse_empty_is_empty() {
        assert!(parse("").is_empty());
        assert!(parse("#EXTM3U\n#EXTINF:1,x\n").is_empty());
    }

    #[test]
    fn parse_strips_bom() {
        let content = "\u{feff}#EXTM3U\n/a.mp3\n";
        let paths = parse(content);
        assert_eq!(paths, vec![PathBuf::from("/a.mp3")]);
    }

    /// 模糊化烟雾测试：任意字节的 m3u 文本经 parse 不得 panic。
    ///（m3u 是外部可投喂的不可信文本；真 fuzz 用 cargo-fuzz + 消毒器，
    /// 此处用确定性伪随机做「永不 panic」轻量守护。）
    #[test]
    fn parse_never_panics_on_garbage() {
        let mut state: u64 = 0x5eed_2026_0927_00b1;
        let mut rng = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            state >> 32
        };
        for len in 0..256usize {
            for variant in 0..3 {
                let bytes: Vec<u8> = (0..len).map(|_| (rng() & 0xff) as u8).collect();
                let mut text = String::from_utf8_lossy(&bytes).into_owned();
                // 变体：裸垃圾 / 带 #EXTM3U 头 / 带 #EXTINF 行，后两者走进更深分支。
                match variant {
                    1 => text = format!("#EXTM3U\n{text}"),
                    2 => text = format!("#EXTM3U\n#EXTINF:{} , x\n{text}", rng() % 1000),
                    _ => {}
                }
                let _ = parse(&text);
                let _ = serialize(&parse(&text));
            }
        }
    }
}
