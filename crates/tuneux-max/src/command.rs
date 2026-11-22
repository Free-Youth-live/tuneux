//! # 命令模式解析
//!
//! 与 fx 同口径：命令由 `:` 唤起、Enter 执行；解析与执行分离，解析可单测。

use std::path::PathBuf;

use tuneux_mediax::{RepeatMode, ShuffleMode};

/// 一条已解析命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 退出程序。
    Quit,
    /// 设置音量（0.0–1.0）。
    Volume(f32),
    /// 设置循环模式。
    Repeat(RepeatMode),
    /// 设置随机模式。
    Shuffle(ShuffleMode),
    /// 清空播放列表。
    Clear,
    /// 打开关于。
    About,
    /// 显示命令帮助。
    Help,
    /// 保存播放列表为 m3u。
    SaveM3u(PathBuf),
    /// 载入 m3u 替换播放列表。
    LoadM3u(PathBuf),
}

/// 命令帮助文本。
pub const HELP: &str = "quit | volume <0-100> | repeat off|single|list | shuffle off|tracks|albums|true_random | save-m3u <路径> | load-m3u <路径> | clear | about";

/// 多余参数一律拒绝：静默忽略会让「volume 5 6」这类笔误难以察觉。
fn ensure_no_more(parts: &mut std::str::SplitWhitespace<'_>, head: &str) -> Result<(), String> {
    if parts.next().is_some() {
        return Err(format!("命令 {head} 含多余参数"));
    }
    Ok(())
}

/// 解析一行命令。
pub fn parse(input: &str) -> Result<Command, String> {
    let trimmed = input.trim();
    let mut parts = trimmed.split_whitespace();
    let Some(head) = parts.next() else {
        return Err("空命令".to_string());
    };
    match head {
        "quit" | "q" => ensure_no_more(&mut parts, head).map(|_| Command::Quit),
        "volume" | "vol" => {
            let value = parts.next().ok_or("缺少音量参数（0-100）")?;
            let percent: f32 = value.parse().map_err(|_| format!("非法音量：{value}"))?;
            if !(0.0..=100.0).contains(&percent) {
                return Err(format!("音量超出范围：{value}"));
            }
            ensure_no_more(&mut parts, head).map(|_| Command::Volume(percent / 100.0))
        }
        "repeat" => {
            let mode = parts.next().ok_or("缺少循环模式参数")?;
            let cmd = match mode {
                "off" => Ok(Command::Repeat(RepeatMode::Off)),
                "single" => Ok(Command::Repeat(RepeatMode::Single)),
                "list" => Ok(Command::Repeat(RepeatMode::List)),
                other => Err(format!("未知循环模式：{other}")),
            };
            ensure_no_more(&mut parts, head).and(cmd)
        }
        "shuffle" => {
            let mode = parts.next().ok_or("缺少随机模式参数")?;
            let mode = match mode {
                "off" => ShuffleMode::Off,
                "tracks" => ShuffleMode::Tracks,
                "albums" => ShuffleMode::Albums,
                "true_random" => ShuffleMode::TrueRandom,
                other => return Err(format!("未知随机模式：{other}")),
            };
            ensure_no_more(&mut parts, head).map(|_| Command::Shuffle(mode))
        }
        "clear" => ensure_no_more(&mut parts, head).map(|_| Command::Clear),
        "about" => ensure_no_more(&mut parts, head).map(|_| Command::About),
        "help" | "?" => ensure_no_more(&mut parts, head).map(|_| Command::Help),
        "save-m3u" | "load-m3u" => {
            let rest = parts.collect::<Vec<_>>().join(" ");
            if rest.is_empty() {
                return Err(format!("{head} 需要文件路径"));
            }
            if head == "save-m3u" {
                Ok(Command::SaveM3u(PathBuf::from(rest)))
            } else {
                Ok(Command::LoadM3u(PathBuf::from(rest)))
            }
        }
        other => Err(format!("未知命令：{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_core_commands() {
        assert_eq!(parse("quit").unwrap(), Command::Quit);
        assert_eq!(parse("volume 42").unwrap(), Command::Volume(0.42));
        assert_eq!(
            parse("repeat single").unwrap(),
            Command::Repeat(RepeatMode::Single)
        );
        assert_eq!(
            parse("shuffle true_random").unwrap(),
            Command::Shuffle(ShuffleMode::TrueRandom)
        );
        assert_eq!(parse("clear").unwrap(), Command::Clear);
        assert_eq!(parse("about").unwrap(), Command::About);
    }

    #[test]
    fn parses_m3u_paths_with_spaces() {
        assert_eq!(
            parse("save-m3u /tmp/my list.m3u").unwrap(),
            Command::SaveM3u(PathBuf::from("/tmp/my list.m3u"))
        );
        assert_eq!(
            parse("load-m3u /tmp/a.m3u").unwrap(),
            Command::LoadM3u(PathBuf::from("/tmp/a.m3u"))
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("").is_err());
        assert!(parse("unknown").is_err());
        assert!(parse("volume abc").is_err());
        assert!(parse("volume 101").is_err());
        assert!(parse("repeat bogus").is_err());
        assert!(parse("save-m3u").is_err());
        assert!(parse("volume 5 6").is_err(), "多余参数应拒绝");
        assert!(parse("clear x").is_err());
    }
}
