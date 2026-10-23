//! # 配置加载状态与损坏保护（三发行版共用）
//!
//! 「文件损坏 ≠ 用户同意丢弃原文件」的统一口径：损坏（文件存在但读取/
//! 解析失败）时先备份原文件，再由调用方锁死本会话写回，防止内存中的
//! 默认值被定时/退出保存覆写回磁盘。本模块集中提供状态枚举、备份落盘
//! 与 UI 提示文案构造，三端（tuneux / tuneux-fx / tuneux-max）共用，
//! 避免各自实现产生口径漂移。

use std::path::{Path, PathBuf};

/// 加载状态：区分「文件缺失」与「文件损坏」。
///
/// 损坏（文件存在但读取/解析失败）不等于用户同意丢弃原文件：必须先备份、
/// 再在本会话停写，防止内存中的默认值被定时/退出保存覆写回磁盘
/// （2026-10-13 修复：损坏的配置/播放列表曾被 5 秒定时保存静默抹掉）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    /// 加载成功。
    Ok,
    /// 文件不存在（首次启动），按默认值运行，保存不受限。
    Missing,
    /// 文件存在但读取/解析失败：`backup` 为备份落盘位置；本会话禁止写回。
    Corrupt {
        /// 备份落盘位置；`None` = 备份失败（用户提示必须如实区分）。
        backup: Option<PathBuf>,
    },
}

/// 备份损坏文件为 `<原路径>.corrupt.bak`；已有备份则不覆盖（保留最早
/// 现场）。返回备份路径；拷贝失败返回 None（备份失败不阻塞启动）。
pub fn backup_corrupt_file(path: &Path) -> Option<PathBuf> {
    let bak = PathBuf::from(format!("{}.corrupt.bak", path.display()));
    if bak.exists() {
        return Some(bak);
    }
    std::fs::copy(path, &bak).ok().map(|_| bak)
}

/// 损坏状态 → UI 提示文案（三端共用口径，i18n 文案由调用方传入）。
///
/// - `Corrupt { backup: Some }`：`backed_up_msg` 的 `{}` 占位渲染真实备份路径；
/// - `Corrupt { backup: None }`：`no_backup_msg`（如实告知备份失败）；
/// - `Ok` / `Missing`：`None`（不产生提示）。
pub fn corrupt_flash_msg(
    status: &LoadStatus,
    backed_up_msg: &str,
    no_backup_msg: &str,
) -> Option<String> {
    match status {
        LoadStatus::Corrupt { backup: Some(bak) } => {
            Some(backed_up_msg.replace("{}", &bak.display().to_string()))
        }
        LoadStatus::Corrupt { backup: None } => Some(no_backup_msg.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 损坏 + 备份成功：文案渲染真实备份路径（含 .corrupt.bak 后缀）。
    #[test]
    fn corrupt_msg_with_backup_renders_path() {
        let st = LoadStatus::Corrupt {
            backup: Some(PathBuf::from("/tmp/x.toml.corrupt.bak")),
        };
        let msg = corrupt_flash_msg(&st, "已备份为 {}", "备份失败").unwrap();
        assert_eq!(msg, "已备份为 /tmp/x.toml.corrupt.bak");
    }

    /// 损坏 + 备份失败：走 no_backup 文案，不得声称已备份。
    #[test]
    fn corrupt_msg_without_backup_uses_fallback() {
        let st = LoadStatus::Corrupt { backup: None };
        let msg = corrupt_flash_msg(&st, "已备份为 {}", "备份失败，请手工保存").unwrap();
        assert_eq!(msg, "备份失败，请手工保存");
    }

    /// 正常路径（Ok / Missing）不产生提示。
    #[test]
    fn ok_and_missing_produce_no_flash() {
        assert!(corrupt_flash_msg(&LoadStatus::Ok, "a {}", "b").is_none());
        assert!(corrupt_flash_msg(&LoadStatus::Missing, "a {}", "b").is_none());
    }

    /// backup_corrupt_file：备份生成且内容一致；已有备份不覆盖（保留最早现场）。
    #[test]
    fn backup_corrupt_file_creates_and_keeps_first() {
        let tmp = std::env::temp_dir().join("commonx_test_corrupt_backup.toml");
        let bak = PathBuf::from(format!("{}.corrupt.bak", tmp.display()));
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&bak);
        std::fs::write(&tmp, "broken = [").unwrap();
        assert_eq!(backup_corrupt_file(&tmp).as_deref(), Some(bak.as_path()));
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "broken = [");
        // 第二次损坏内容不同，备份不得被覆盖
        std::fs::write(&tmp, "other = [").unwrap();
        backup_corrupt_file(&tmp);
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "broken = [");
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&bak);
    }
}
