//! # 界面文案（中文锚表 + 可选语言文件）
//!
//! 中文锚表在 `config::zh_table`（守卫脚本按函数区间提取键集合）；
//! 其它语言读 exe 同目录 `locales/<lang>.txt`，缺失时回退中文。

use tuneux_commonx::{build_i18n, I18n};

/// 按配置语言构建界面文案。
pub fn build(lang: &str) -> I18n {
    build_i18n(lang, crate::config::zh_table())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zh_table_has_core_keys() {
        let table = crate::config::zh_table();
        for key in ["app.title", "menu.file", "panel.browser", "common.close"] {
            assert!(table.get(key).is_some(), "缺中文键：{key}");
        }
    }

    #[test]
    fn i18n_falls_back_to_zh() {
        let i18n = build("zz");
        assert_eq!(i18n.t("app.title"), "tuneux-max");
    }
}
