//! 配置便携路径与可写性探测。
//!
//! 发行版配置文件（`tuneux.toml` / `tuneux-fx.toml` 等）的存储位置遵循
//! 「exe 同目录优先、系统配置目录回退」的便携策略；本模块提供该机制的
//! 通用实现，产品只需传入文件名与系统配置目录下的子目录名。

use std::path::{Path, PathBuf};

/// 计算「exe 同目录优先、系统配置目录回退」的某文件路径。
///
/// 处理逻辑：
/// 1. 取 exe 所在目录，若该目录下已存在该文件（说明此前用过便携模式），
///    或该目录可写（尝试创建临时探测文件验证），则使用 `exe_dir/文件`。
/// 2. 否则回退到系统配置目录 `config_dir/<app_dir_name>/文件`，
///    并自动创建中间目录。
///
/// # 参数
///
/// - `filename`：文件名（如 `"tuneux.toml"`）；
/// - `app_dir_name`：系统配置目录下的子目录名（如 `"tuneux"` / `"tuneux-fx"`，
///   不同产品隔离，避免互覆盖）。
pub fn portable_path(filename: &str, app_dir_name: &str) -> PathBuf {
    // 尝试路径 1：exe 同目录
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(filename);
            // 文件已存在 → 沿用便携模式（即使目录现变为只读，也尊重既有文件）
            // 文件不存在但目录可写 → 新建便携文件
            if candidate.exists() || is_writable(dir) {
                return candidate;
            }
        }
    }

    // 回退路径 2：系统配置目录
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(app_dir_name);
    // 回退目录不存在时自动创建（首次运行），失败忽略——save 时再处理错误
    let _ = std::fs::create_dir_all(&path);
    path.push(filename);
    path
}

/// 探测目录是否可写：尝试创建并删除一个临时探测文件。
///
/// 返回 true 表示可写。任何 IO 错误（权限不足、只读文件系统、路径不存在）
/// 均视为不可写，返回 false。
pub fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".tuneux_write_probe");
    let writable = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&probe)
        .is_ok();
    // 探测成功后清理临时文件；失败也无妨，下次启动会覆盖
    if writable {
        let _ = std::fs::remove_file(&probe);
    }
    writable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_writable_on_temp_dir() {
        let dir = std::env::temp_dir();
        assert!(is_writable(&dir), "临时目录应可写");
    }

    #[test]
    fn is_writable_on_nonexistent_dir() {
        let dir = std::env::temp_dir().join("tuneux_commonx_no_such_dir_12345");
        assert!(!is_writable(&dir), "不存在的目录应判定为不可写");
    }

    #[test]
    fn portable_path_appends_app_dir() {
        // 仅验证：函数不 panic，且返回路径的最后一个组件是文件名。
        let p = portable_path("demo.toml", "demo-app");
        assert_eq!(p.file_name().and_then(|s| s.to_str()), Some("demo.toml"));
    }
}
