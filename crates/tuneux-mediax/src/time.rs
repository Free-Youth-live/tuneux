//! 时间分解工具（纯数据变换，供各发行版共享）。
//!
//! 只做「整秒 → (时, 分, 秒)」分解；取整策略（截断 / 四舍五入）与
//! 显示格式（m:ss / h:mm:ss）是呈现口径，归各发行版。

/// 把非负整秒分解为（时, 分, 秒）。
///
/// 输入应为调用方已取整的秒数（f64 → u64 的取整策略由调用方决定）；
/// 例：`hms_parts(3675)` = `(1, 1, 15)`，`hms_parts(59)` = `(0, 0, 59)`。
pub fn hms_parts(total_secs: u64) -> (u64, u64, u64) {
    (total_secs / 3600, (total_secs % 3600) / 60, total_secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decomposes_boundaries() {
        assert_eq!(hms_parts(0), (0, 0, 0));
        assert_eq!(hms_parts(59), (0, 0, 59));
        assert_eq!(hms_parts(60), (0, 1, 0));
        assert_eq!(hms_parts(3599), (0, 59, 59));
        assert_eq!(hms_parts(3600), (1, 0, 0));
        assert_eq!(hms_parts(3675), (1, 1, 15));
        assert_eq!(hms_parts(86_399), (23, 59, 59));
        // 超过一天的时长（长播放列表累计场景）：小时位无上限增长。
        assert_eq!(hms_parts(90_000), (25, 0, 0));
    }
}
