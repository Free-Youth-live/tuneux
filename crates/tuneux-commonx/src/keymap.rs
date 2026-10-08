//! 键位描述解析与校验（三产品通用）。
//!
//! 自定义键位映射（`keymap`）以「动作名 → 键描述」形式写入配置文件；
//! 本模块负责把键描述解析为规范形式，并在加载期剔除非法 / 保留键映射。
//!
//! # 语法
//!
//! 键描述 = `[shift+][ctrl+][alt+]<键名>`。修饰符大小写不敏感、顺序不敏感
//! （输出统一为 shift → ctrl → alt），段间可含空白。`<键名>` 为单个字符
//! 或具名键（space / tab / enter / esc / backspace / up / down / left /
//! right / home / end 及各自别名）。

use std::collections::HashMap;

/// 解析并规范化一条自定义键描述（`keymap` 的值），返回规范形式；非法返回 `None`。
///
/// # 语法
///
/// 键描述 = `[shift+][ctrl+][alt+]<键名>`。修饰符大小写不敏感、顺序不敏感
/// （输出统一为 shift → ctrl → alt），段间可含空白。`<键名>` 为：
///
/// - **单个字符**：直接写字符本身，如 `"n"`、`"+"`、`"-"`、`"="`。
///   `" "`（单空格）等价于 `"space"`。
/// - **具名键**（小写、别名见下）：`"space"`、`"tab"`、`"enter"`（别名
///   `"return"`）、`"esc"`（别名 `"escape"`）、`"backspace"`（别名 `"delete"`）、
///   `"up"`、`"down"`、`"left"`、`"right"`、`"home"`、`"end"`。
///
/// 修饰符 **shift 仅对字母键有意义**（如 `"shift+n"`）：符号键的 Shift 是
/// 打出符号本身所需，键描述里不写 shift（`"+"` 即代表加号键）。
///
/// # 返回 `None`（该映射被忽略，回退内置默认键）的情形
///
/// 空串、纯修饰符（如 `"shift+"`）、多个键名（如 `"ab"`）、未知键名
/// （如 `"f1"`、`"foo"`）。
pub fn parse_key_desc(desc: &str) -> Option<String> {
    // 单空格直接代表空格键
    if desc == " " {
        return Some("space".to_string());
    }

    // 单字符键（含 '+'、'-' 等会被 split('+') 拆散的特殊字符）直接规范化；
    // 单字符不可能携带修饰符。
    let trimmed = desc.trim();
    if trimmed.chars().count() == 1 {
        return canonical_key_name(trimmed, false);
    }

    let mut shift = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut base: Option<&str> = None;

    // 按 '+' 拆分修饰符与键名；'+' 键本身已在上面的单字符分支处理。
    for part in trimmed.split('+') {
        let p = part.trim();
        if p.is_empty() {
            return None; // 空段（如 "shift++n"）
        }
        match p.to_ascii_lowercase().as_str() {
            "shift" => shift = true,
            "ctrl" | "control" => ctrl = true,
            "alt" | "option" | "meta" => alt = true,
            _ => {
                // 非修饰符段即键名；键名只能出现一次
                if base.is_some() {
                    return None;
                }
                base = Some(p);
            }
        }
    }

    let base = base?; // 纯修饰符（无键名）
    let canonical = canonical_key_name(base, shift)?;

    // 按固定顺序拼出规范形式
    let mut out = String::new();
    if shift {
        out.push_str("shift+");
    }
    if ctrl {
        out.push_str("ctrl+");
    }
    if alt {
        out.push_str("alt+");
    }
    out.push_str(&canonical);
    Some(out)
}

/// 把键名部分规范化为小写具名键或单字符（字母 + shift 时统一小写）。
///
/// 输出格式与按键事件侧的规范键描述保持同一格式，二者对齐后才能
/// 命中映射。
pub fn canonical_key_name(name: &str, shift: bool) -> Option<String> {
    // 具名键：大小写不敏感
    let named = match name.to_ascii_lowercase().as_str() {
        "space" => Some("space"),
        "tab" => Some("tab"),
        "enter" | "return" => Some("enter"),
        "esc" | "escape" => Some("esc"),
        "backspace" | "delete" => Some("backspace"),
        "up" => Some("up"),
        "down" => Some("down"),
        "left" => Some("left"),
        "right" => Some("right"),
        "home" => Some("home"),
        "end" => Some("end"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_string());
    }

    // 单字符键：字母 + shift 时统一小写（与 key_to_desc 一致），其余原样
    let mut chars = name.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None; // 多字符且非具名键 → 未知
    }
    let c = if shift && c.is_ascii_alphabetic() {
        c.to_ascii_lowercase()
    } else {
        c
    };
    Some(c.to_string())
}

/// 校验并清理 keymap：剔除非法动作名（未知动作 / 空白键描述），
/// 拒绝保留键映射（退出键不可被映射走）。
///
/// # 参数（事实与策略注入）
///
/// - `actions`：合法动作名白名单（产品各自的 keymap 动作集）；
/// - `reserved`：保留键（规范化后的键描述，如 `q` / `ctrl+c`）——
///   退出路径是安全底线，被映射走后用户配置失误将无法退出程序。
pub fn validate_keymap(keymap: &mut HashMap<String, String>, actions: &[&str], reserved: &[&str]) {
    keymap.retain(|action, desc| {
        let action_ok = actions.contains(&action.as_str());
        // 键描述必须可解析：加载期即拒绝，而非运行期静默不生效。
        let parsed = parse_key_desc(desc);
        if !action_ok || parsed.is_none() {
            eprintln!(
                "[配置] 忽略非法快捷键映射：动作 {action}（描述 {desc}）——合法动作：{}",
                actions.join(" / ")
            );
            return false;
        }
        // 保留键拒绝（q / Ctrl+C 退出底线，不可重映射；大小写不敏感——
        // 规范化对 ctrl+字母保留原大小写，"Ctrl+C" 规范为 "ctrl+C"）。
        if parsed
            .as_deref()
            .is_some_and(|d| reserved.iter().any(|r| r.eq_ignore_ascii_case(d)))
        {
            eprintln!(
                "[配置] 忽略保留键映射：动作 {action}（描述 {desc}）——q / Ctrl+C 为退出键，不可重映射"
            );
            return false;
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_desc_cases() {
        // 合法：单字符 / 具名键 / 修饰符
        assert_eq!(parse_key_desc("n"), Some("n".to_string()));
        assert_eq!(parse_key_desc("space"), Some("space".to_string()));
        assert_eq!(parse_key_desc(" "), Some("space".to_string()));
        assert_eq!(parse_key_desc("shift+n"), Some("shift+n".to_string()));
        assert_eq!(parse_key_desc("Shift+N"), Some("shift+n".to_string()));
        assert_eq!(parse_key_desc("ctrl+c"), Some("ctrl+c".to_string()));
        assert_eq!(
            parse_key_desc("alt + ctrl + q"),
            Some("ctrl+alt+q".to_string())
        );
        assert_eq!(parse_key_desc("+"), Some("+".to_string()));
        assert_eq!(parse_key_desc("-"), Some("-".to_string()));
        assert_eq!(parse_key_desc("Enter"), Some("enter".to_string()));
        assert_eq!(parse_key_desc("return"), Some("enter".to_string()));
        assert_eq!(parse_key_desc("escape"), Some("esc".to_string()));
        // 非法：空串 / 纯修饰符 / 多字符非具名键
        assert_eq!(parse_key_desc(""), None);
        assert_eq!(parse_key_desc("shift+"), None);
        assert_eq!(parse_key_desc("ab"), None);
        assert_eq!(parse_key_desc("f1"), None);
        assert_eq!(parse_key_desc("foo"), None);
    }

    #[test]
    fn validate_keymap_rejects_reserved_and_unparsable() {
        let actions = ["toggle_play", "next", "prev", "volume_up", "volume_down"];
        let reserved = ["q", "ctrl+c"];
        let mut map = HashMap::from([
            ("toggle_play".to_string(), "q".to_string()), // 保留：q
            ("next".to_string(), "Ctrl+C".to_string()),   // 保留：ctrl+c（大小写变体）
            ("prev".to_string(), "shift+q".to_string()),  // 合法：shift+q 非保留键
            ("volume_up".to_string(), "f5".to_string()),  // 非法：F 键不可自定义
            ("volume_down".to_string(), "bogus key".to_string()), // 非法：无法解析
            ("unknown_action".to_string(), "n".to_string()), // 非法：未知动作
        ]);
        validate_keymap(&mut map, &actions, &reserved);
        assert!(!map.contains_key("toggle_play"), "q 映射应被拒绝");
        assert!(
            !map.contains_key("next"),
            "ctrl+c 映射应被拒绝（大小写不敏感）"
        );
        assert!(!map.contains_key("volume_up"), "F 键描述应被解析层拒绝");
        assert!(!map.contains_key("volume_down"), "无法解析的描述应被拒绝");
        assert!(!map.contains_key("unknown_action"), "未知动作应被拒绝");
        assert_eq!(
            map.get("prev").map(|s| s.as_str()),
            Some("shift+q"),
            "合法映射应保留"
        );
    }
}
