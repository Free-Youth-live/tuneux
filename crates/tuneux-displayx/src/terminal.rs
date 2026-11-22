//! 终端色彩能力探测：显式覆盖优先，其后读取常见终端信号。

use std::sync::OnceLock;

/// 终端真彩能力判定：返回是否按 24 位色输出。
///
/// `TUNEUX_COLOR_DEPTH` 显式覆盖优先；其后依次识别 Windows Terminal、
/// 常见终端程序、ConEmu、TERM 与 COLORTERM。无信号时 Windows 默认按真彩，
/// 其它平台默认使用 16 色。
#[allow(clippy::too_many_arguments)]
pub fn truecolor_from_signals(
    depth_override: Option<&str>,
    wt_session: bool,
    term_program: Option<&str>,
    conemu_ansi: Option<&str>,
    term: Option<&str>,
    colorterm: Option<&str>,
    is_windows: bool,
) -> bool {
    if let Some(value) = depth_override {
        let value = value.trim().to_ascii_lowercase();
        if matches!(value.as_str(), "truecolor" | "24bit" | "1" | "on") {
            return true;
        }
        if matches!(value.as_str(), "256" | "16" | "0" | "off") {
            return false;
        }
    }
    if wt_session {
        return true;
    }
    if let Some(program) = term_program {
        if matches!(
            program.to_ascii_lowercase().as_str(),
            "iterm.app"
                | "wezterm"
                | "vscode"
                | "vscode-insiders"
                | "hyper"
                | "windowsterminal"
                | "tabby"
                | "konsole"
        ) {
            return true;
        }
    }
    if let Some(value) = conemu_ansi {
        if value.eq_ignore_ascii_case("on") {
            return true;
        }
    }
    if let Some(value) = term {
        let value = value.to_ascii_lowercase();
        if value.contains("truecolor") || value.contains("direct") {
            return true;
        }
    }
    if let Some(value) = colorterm {
        let value = value.to_ascii_lowercase();
        if value.contains("truecolor") || value.contains("24bit") {
            return true;
        }
    }
    is_windows
}

/// 读取当前进程终端能力；一次会话内环境不变，结果只计算一次。
pub fn env_truecolor() -> bool {
    static CAPABILITY: OnceLock<bool> = OnceLock::new();
    *CAPABILITY.get_or_init(|| {
        let var = |key: &str| std::env::var(key).ok();
        truecolor_from_signals(
            var("TUNEUX_COLOR_DEPTH").as_deref(),
            var("WT_SESSION").is_some(),
            var("TERM_PROGRAM").as_deref(),
            var("ConEmuANSI").as_deref(),
            var("TERM").as_deref(),
            var("COLORTERM").as_deref(),
            cfg!(windows),
        )
    })
}
