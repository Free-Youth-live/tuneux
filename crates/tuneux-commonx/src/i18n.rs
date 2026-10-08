//! 国际化（i18n）：语言表加载与回退链。
//!
//! 机制归通用层（三产品共用，含基础版 tuneux）；**文案归产品**——zh 内置表由
//! 各产品作为「锚」构造注入，其它语言是 `locales/<lang>.txt` 文件（官方英文
//! `en.txt` 随包分发，用户可自建其它语言）。
//!
//! 核心约定：
//!
//! - **默认中文**：zh 内置表编译进二进制，是最终回退；
//! - **其它语言是文件**：`key = "文本"` 每行一条，`#` 注释与空行忽略（复用
//!   皮肤插件的 key=value 格式）；
//! - **回退链**：请求语言 → zh 内置表 → key 本身；文件缺失 / 解析失败回退 zh，
//!   绝不 panic；
//! - **语言 = 文件，不是 pinx 插件**：基础版 tuneux 永不碰 pinx，文件方案
//!   三产品通用。

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;

/// 语言表：key → 文本。
#[derive(Debug, Clone, Default)]
pub struct LangTable {
    map: HashMap<String, String>,
}

impl LangTable {
    /// 从「`key = "文本"`」文本解析：`#` 注释与空行忽略，非法行跳过，不 panic。
    pub fn parse(text: &str) -> Self {
        let mut map = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            map.insert(key.to_string(), unquote(value.trim()));
        }
        Self { map }
    }

    /// 从文件加载：不存在 / 读取失败返回 `None`（调用方回退），绝不 panic。
    pub fn load(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        Some(Self::parse(&text))
    }

    /// 按 key 查文本（直接命中本表；无回退）。
    pub fn get(&self, key: &str) -> Option<&str> {
        self.map.get(key).map(|s| s.as_str())
    }

    /// 表内条目数（测试 / pre-push key 完整性校验用）。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 全部 key（测试 / key 卫生断言用：无序，仅供完整性与合法性检查）。
    ///
    /// 背景守护：内嵌 zh 表写在 Rust 字符串字面量里，行尾续行符写错
    ///（`\n\` 多一个反斜杠）会把字面 `\n` 粘到下一个 key 前面，
    /// 源码级 grep 看不到、只有运行时才暴露；本访问器供产品层测试
    /// 断言「每个 key 都匹配 `[a-z][a-z0-9_.]*`」。
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(|s| s.as_str())
    }

    /// 表是否为空。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// 去掉包裹引号（`"..."` / `'...'`）；无引号原样返回。
fn unquote(s: &str) -> String {
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[s.len() - 1] == b'"') || (b[0] == b'\'' && b[s.len() - 1] == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// i18n 查询器：请求语言表 → zh 内置兜底表 → key 本身。
#[derive(Debug, Clone, Default)]
pub struct I18n {
    /// 请求语言表（如 en.txt；缺失时为空表）。
    primary: LangTable,
    /// zh 内置兜底表（编译进二进制，最终回退）。
    fallback: LangTable,
}

impl I18n {
    /// 构造：`primary` 为请求语言表，`fallback` 为 zh 内置表。
    pub fn new(primary: LangTable, fallback: LangTable) -> Self {
        Self { primary, fallback }
    }

    /// 查询：primary → fallback → key 本身（永不 panic）。
    ///
    /// 命中表时零分配（借自表内 `String`）；仅「均未命中回退 key」这一调试
    /// 路径发生一次分配——正常发布态 zh 表应覆盖全部 key，此路径几乎不触发。
    pub fn t(&self, key: &str) -> Cow<'_, str> {
        if let Some(v) = self.primary.get(key) {
            return Cow::Borrowed(v);
        }
        if let Some(v) = self.fallback.get(key) {
            return Cow::Borrowed(v);
        }
        Cow::Owned(key.to_string())
    }
}

/// 构建 i18n 查询器：请求语言表（`locales/<lang>.txt`，exe 同目录）+ zh 兜底表。
///
/// 机制归通用层（本函数），**文案归产品**——`zh_fallback` 由各产品作为「锚」
/// 构造注入（`zh_table()` 留在产品 crate，由 key 完整性校验脚本守护）。`lang == "zh"` 或文件缺失时 primary 为空表，走 zh 兜底；绝不 panic。
pub fn build_i18n(lang: &str, zh_fallback: LangTable) -> I18n {
    let primary = if lang == "zh" {
        LangTable::default()
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|e| {
                e.parent()
                    .map(|d| d.join("locales").join(format!("{lang}.txt")))
            })
            .filter(|p| p.exists())
            .and_then(|p| LangTable::load(&p))
            .unwrap_or_default()
    };
    I18n::new(primary, zh_fallback)
}

/// 扫描可用语言：内置 zh + exe 同目录 locales/*.txt。
///
/// 返回 (lang_id, display_name) 列表，按字母排序（zh 始终第一）。
/// display_name 取 .txt 首行 `# display: xxx` 注释；无注释时用文件名。
pub fn scan_langs() -> Vec<(String, String)> {
    let mut out = vec![("zh".to_string(), "中文".to_string())];
    let dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("locales").to_path_buf()));
    let Some(dir) = dir else { return out };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut langs: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("txt") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "zh" {
            continue; // zh 是内置，不重复列出
        }
        // 尝试读首行 # display: xxx 注释取显示名
        let display = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|l| l.starts_with("# display:"))
                    .map(|l| l.trim_start_matches("# display:").trim().to_string())
            })
            .unwrap_or_else(|| stem.to_string());
        langs.push((stem.to_string(), display));
    }
    langs.sort();
    out.extend(langs);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ignores_comments_blanks_and_bad_lines() {
        let text = "# 注释\n\nplay = \"播放\"\npause = 暂停\ninvalid line without equals\n";
        let table = LangTable::parse(text);
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn parse_strips_quotes() {
        let text = "a = \"带空格的文本\"\nb = '单引号'\nc = 裸文本\n";
        let table = LangTable::parse(text);
        assert_eq!(table.get("a"), Some("带空格的文本"));
        assert_eq!(table.get("b"), Some("单引号"));
        assert_eq!(table.get("c"), Some("裸文本"));
    }

    #[test]
    fn load_missing_returns_none() {
        let p = std::env::temp_dir().join("tuneux_commonx_no_lang_file_12345.txt");
        let _ = std::fs::remove_file(&p);
        assert!(LangTable::load(&p).is_none());
    }

    #[test]
    fn t_falls_back_primary_then_zh_then_key() {
        let primary = LangTable::parse("play = Play\n");
        let zh = LangTable::parse("play = \"播放\"\npause = \"暂停\"\n");
        let i18n = I18n::new(primary, zh);

        // primary 命中。
        assert_eq!(i18n.t("play"), "Play");
        // primary 缺失 → zh 兜底。
        assert_eq!(i18n.t("pause"), "暂停");
        // 均缺失 → key 本身。
        assert_eq!(i18n.t("missing_key"), "missing_key");
    }

    #[test]
    fn t_never_panics_on_garbage_table() {
        // 空表 + 任意 key 也应回退到 key 本身。
        let i18n = I18n::default();
        assert_eq!(i18n.t("任意 key"), "任意 key");
    }
}
