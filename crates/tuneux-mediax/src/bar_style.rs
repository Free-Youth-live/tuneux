//! # 频谱柱字符风格（bar_style）
//!
//! 跨产品共用的频谱 / 电平表 / 示波器字符风格枚举。字符集宿主内置、
//! 逐字符过宽度校验（杜绝全角字符混入 1 列布局的「宽度炸弹」）：
//! fx 经皮肤键 `bar_style` 选择；基础版经配置文件 `bar_style` 键选择。
//! 非法值一律回落默认（Matrix）。

use serde::{Deserialize, Serialize};

/// 频谱柱字符风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BarStyle {
    /// 细字符雨（默认；1 列宽）。
    #[default]
    Matrix,
    /// 块字符 █▓▒░（1 列宽）。
    Blocks,
    /// ASCII 复古 #*=（1 列宽）。
    Ascii,
    /// 汉字柱（2 列宽，横向分辨率减半）。
    Hanzi,
}

impl BarStyle {
    /// 单柱显示宽度（终端列数）。
    pub fn col_width(self) -> usize {
        match self {
            BarStyle::Hanzi => 2,
            _ => 1,
        }
    }

    /// 柱体字符池（均为该风格列宽的整倍数宽度）。
    pub fn pool(self) -> &'static [&'static str] {
        match self {
            // Matrix 风细字符池（全部 1 列宽）：每帧每格随机换，营造"数据雨"。
            #[allow(clippy::unicode_not_nfc)]
            BarStyle::Matrix => &[
                "1", "l", "i", "I", "|", "'", ":", ".", "·", "•", "◦", ";", ",", "`", "´", "j",
                "J", "~", "/", "\\", "⁄", "-", "–", "—", "=", "│", "┆", "┊", "¦", "∣", "!", "?",
                "+", "×", "∗", "˖", "˗",
            ],
            BarStyle::Blocks => &["█", "▓", "▒", "░", "▄", "■", "●"],
            BarStyle::Ascii => &["#", "*", "=", "+", "x", "%", "@"],
            // 汉字池：音声波频 + 李白《将进酒》摘句（「人生得意须尽欢，莫使
            // 金樽空对月」「天生我材必有用，千金散尽还复来」）。均经
            // unicode-width 校验为 2 列宽（单测守护）。
            BarStyle::Hanzi => &[
                "音", "声", "波", "频", "人", "生", "得", "意", "须", "尽", "欢", "莫", "使", "金",
                "樽", "空", "对", "月", "天", "生", "我", "材", "必", "有", "用", "千", "金", "散",
                "还", "复", "来",
            ],
        }
    }

    /// 从配置 / 皮肤文本名解析；未知名返回 None（调用方回落默认）。
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "matrix" => Some(Self::Matrix),
            "blocks" => Some(Self::Blocks),
            "ascii" => Some(Self::Ascii),
            "hanzi" => Some(Self::Hanzi),
            _ => None,
        }
    }
}

// =============================================================================
// 单元测试
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// 各风格字符池宽度守护：逐字符宽度 == 风格列宽（1 列 / 2 列）。
    /// 这是对位渲染的前提，防字符集误加全角 / 零宽字符。
    #[test]
    fn style_pools_match_col_width() {
        for style in [
            BarStyle::Matrix,
            BarStyle::Blocks,
            BarStyle::Ascii,
            BarStyle::Hanzi,
        ] {
            for ch in style.pool() {
                assert_eq!(
                    UnicodeWidthStr::width(*ch),
                    style.col_width(),
                    "{style:?} 池字符 {ch:?} 宽度与列宽不符"
                );
            }
        }
    }

    /// from_name：四个合法名 + 非法名回落 None。
    #[test]
    fn from_name_roundtrip() {
        assert_eq!(BarStyle::from_name("matrix"), Some(BarStyle::Matrix));
        assert_eq!(BarStyle::from_name("blocks"), Some(BarStyle::Blocks));
        assert_eq!(BarStyle::from_name("ascii"), Some(BarStyle::Ascii));
        assert_eq!(BarStyle::from_name("hanzi"), Some(BarStyle::Hanzi));
        assert_eq!(BarStyle::from_name("bogus"), None);
    }
}
