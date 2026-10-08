//! 浮点净化助手。
//!
//! 配置文件里的浮点字段可能被手写成 `nan` / `inf`；这类值进入
//! `f32::clamp` 会原样返回（NaN 与任何比较都为 false），最终落到
//! 音频增益 / 界面比例等位置会造成静音或 0 宽面板。统一在加载期
//! 以「非有限值回退默认」净化，跨产品同口径。

/// 净化 `f32`：`NaN` / `±Inf` 回退默认值，否则原样返回。
///
/// 典型用法：`cfg.browser_ratio = sanitize_f32(cfg.browser_ratio, 0.4);`
pub fn sanitize_f32(value: f32, default: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        default
    }
}

/// 净化 `f64`：`NaN` / `±Inf` 回退默认值，否则原样返回。
pub fn sanitize_f64(value: f64, default: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_values_pass_through() {
        assert_eq!(sanitize_f32(0.4, 0.5), 0.4);
        assert_eq!(sanitize_f64(-8.5, 0.0), -8.5);
        assert_eq!(sanitize_f32(0.0, 1.0), 0.0);
    }

    #[test]
    fn non_finite_values_fall_back() {
        assert_eq!(sanitize_f32(f32::NAN, 0.4), 0.4);
        assert_eq!(sanitize_f32(f32::INFINITY, 0.4), 0.4);
        assert_eq!(sanitize_f32(f32::NEG_INFINITY, 0.4), 0.4);
        assert_eq!(sanitize_f64(f64::NAN, 1.5), 1.5);
        assert_eq!(sanitize_f64(f64::INFINITY, 1.5), 1.5);
    }
}
