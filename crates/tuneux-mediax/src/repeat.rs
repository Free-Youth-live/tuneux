//! 循环播放模式（播放领域数据，供各发行版共享）。
//!
//! 序列化表示（toml `repeat = "off" | "single" | "list"`）已冻结、向后
//! 兼容；中文显示文案（顺序 / 单曲 / 循环）是呈现，归各发行版自带，
//! 不进数据层。

use serde::{Deserialize, Serialize};

/// 循环播放模式。
///
/// 三态循环：关闭 → 单曲 → 列表，`r` 键经 [`RepeatMode::next`] 循环切换。
/// 用枚举而非魔法数字，配合 serde 以可读字符串存入 TOML，配置文件对人友好。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum RepeatMode {
    /// 不循环：播放到列表末尾即停止。
    #[default]
    Off,
    /// 单曲循环：当前曲目无限重复。
    Single,
    /// 列表循环：整列表循环播放。
    List,
}

impl RepeatMode {
    /// 循环切换到下一个模式：Off → Single → List → Off。
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::Single,
            RepeatMode::Single => RepeatMode::List,
            RepeatMode::List => RepeatMode::Off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 序列化表示冻结：三态必须是 "off" / "single" / "list" 小写——
    /// 旧配置文件按此解析，任何改动都是破坏性变更。
    #[test]
    fn serde_representation_is_frozen() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Wrap {
            repeat: RepeatMode,
        }
        fn roundtrip(mode: RepeatMode) -> String {
            let text = toml::to_string(&Wrap { repeat: mode }).unwrap();
            // 形如 `repeat = "off"\n`
            assert!(toml::from_str::<Wrap>(&text).unwrap().repeat == mode);
            text.trim().to_string()
        }
        assert_eq!(roundtrip(RepeatMode::Off), "repeat = \"off\"");
        assert_eq!(roundtrip(RepeatMode::Single), "repeat = \"single\"");
        assert_eq!(roundtrip(RepeatMode::List), "repeat = \"list\"");
        // 旧配置真实写法（无引号，toml 字符串两种形式等价）也能解析。
        assert_eq!(
            toml::from_str::<Wrap>("repeat = 'single'").unwrap().repeat,
            RepeatMode::Single
        );
        // 未知值拒绝（serde 默认），不静默回退。
        assert!(toml::from_str::<Wrap>("repeat = 'loop'").is_err());
    }

    /// next() 三态循环闭合。
    #[test]
    fn next_cycles_through_all_modes() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Single);
        assert_eq!(RepeatMode::Single.next(), RepeatMode::List);
        assert_eq!(RepeatMode::List.next(), RepeatMode::Off);
    }
}
