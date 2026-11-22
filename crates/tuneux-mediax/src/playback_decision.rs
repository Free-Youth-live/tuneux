//! 播放动作决议（FCIS：功能核心-命令外壳）
//!
//! 把「该做什么」的决策提为纯数据 [`PlaybackDecision`] 与纯函数——只读输入、
//! 返回动作，**不碰引擎、不碰 UI**。副作用（`engine.send`、界面更新）由产品层
//! 执行。这样决策逻辑可单测（无需引擎夹具），且 tuneux / tuneux-fx 双端共用
//! 同一份决策，杜绝「改一漏一」。
//!
//! 边界（只收播放动作决议，不收渲染决策）：约七个变体封顶——一旦想塞进
//! 渲染/布局决策，说明放错了层。

use crate::playlist_item::CueRef;

/// 播放动作决议（纯数据）。
#[derive(Debug, Clone, PartialEq)]
pub enum PlaybackDecision {
    /// 从头播整文件。
    Play,
    /// 从指定秒断点续播。
    ResumeFrom {
        /// 续播起点（秒）。
        secs: f64,
    },
    /// 按 CUE 分轨区间播放（引擎侧终点判定）。
    PlayRange {
        /// 分轨起点（秒）。
        start_secs: f64,
        /// 分轨终点（秒）；None = 播到文件末尾。
        end_secs: Option<f64>,
    },
    /// 文件内 seek（播放中的 CUE 单曲循环回片段起点，不重开文件）。
    Seek {
        /// 目标秒。
        secs: f64,
    },
    /// 暂停。
    Pause,
    /// 恢复。
    Resume,
    /// 无操作（如 `NavOutcome::End`）。
    Noop,
}

/// 连续播放失败上限：达到后停止自动跳曲（列表全损坏 / 设备持续故障时
/// 不再逐首尝试刷屏）。计数**饱和**在上限，只有用户手动发起点播才清零恢复。
pub const CONSECUTIVE_FAILURE_LIMIT: u32 = 10;

/// 播放失败后推进连续失败计数：返回 `(新计数, 是否继续自动跳下一首)`。
///
/// 三端（tuneux / tuneux-fx / tuneux-max）共用同一份决议，杜绝
/// 「修了一端、另外两端未同步」：
///
/// - 未达上限：计数 +1，继续自动跳曲；
/// - 达到上限：计数**保持在上限**（饱和），停止自动跳曲——清零只在
///   用户用户手动发起点播时发生，保证持续故障不会造成「每 10 次再循环一轮」。
///
/// 纯函数（无引擎 / 无 UI），便于单测 9 / 10 / 11 / 20 次等边界。
pub fn advance_failure_count(current: u32) -> (u32, bool) {
    let next = current.saturating_add(1).min(CONSECUTIVE_FAILURE_LIMIT);
    (next, next < CONSECUTIVE_FAILURE_LIMIT)
}

/// 断点续播判定：距结尾 5 秒内视为已听完 → 从头播（None）；否则续播（Some）。
///
/// 纯函数：`saved_secs` 为上次保存的进度、`duration` 为曲目总时长。
pub fn resume_secs(saved_secs: Option<f64>, duration: Option<f64>) -> Option<f64> {
    // 时长为 0（解码层对无时长曲目显式置 0）视为未知时长：不做「已听完」判定，
    // 否则已存断点永远被误判为听完、曲目每次从头重播。
    let already_finished = matches!(
        (saved_secs, duration),
        (Some(s), Some(d)) if d > 0.0 && s >= d - 5.0
    );
    saved_secs.filter(|&s| s > 0.5 && !already_finished)
}

/// 播放/暂停切换决议：正在播放 → `Pause`；已到结尾 → 重播（`Play`/`PlayRange`）；
/// 否则 `Resume`。
pub fn toggle_decision(is_playing: bool, at_end: bool, cue: Option<&CueRef>) -> PlaybackDecision {
    if is_playing {
        return PlaybackDecision::Pause;
    }
    if at_end {
        match cue {
            Some(c) => PlaybackDecision::PlayRange {
                start_secs: c.start_ms as f64 / 1000.0,
                end_secs: c.end_ms.map(|e| e as f64 / 1000.0),
            },
            None => PlaybackDecision::Play,
        }
    } else {
        PlaybackDecision::Resume
    }
}

/// 单曲循环（`NavOutcome::Repeat`）决议：播放中 CUE 回片段起点 `Seek`；
/// 非播放 CUE 用 `PlayRange` 带回终点；普通曲目从头 `Play`。
pub fn single_repeat_decision(is_playing: bool, cue: Option<&CueRef>) -> PlaybackDecision {
    match cue {
        Some(c) if is_playing => PlaybackDecision::Seek {
            secs: c.start_ms as f64 / 1000.0,
        },
        Some(c) => PlaybackDecision::PlayRange {
            start_secs: c.start_ms as f64 / 1000.0,
            end_secs: c.end_ms.map(|e| e as f64 / 1000.0),
        },
        None => PlaybackDecision::Play,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_ms: u64, end_ms: Option<u64>) -> CueRef {
        CueRef {
            index: 1,
            title: "Track 1".to_string(),
            performer: None,
            start_ms,
            end_ms,
        }
    }

    /// 连续失败计数：1..9 次继续自动跳曲；第 10 次停止；上限处饱和，
    /// 第 11、20 次失败不再跳曲也不再增长（防「每 10 次再循环」假修复）。
    #[test]
    fn advance_failure_count_stops_at_limit() {
        let mut count = 0u32;
        for expected in 1..=9 {
            let (next, keep_skipping) = advance_failure_count(count);
            assert_eq!(next, expected, "前 9 次失败计数逐次 +1");
            assert!(keep_skipping, "第 {expected} 次失败仍应自动跳曲");
            count = next;
        }
        let (next, keep_skipping) = advance_failure_count(count);
        assert_eq!(next, CONSECUTIVE_FAILURE_LIMIT);
        assert!(!keep_skipping, "第 10 次失败应停止自动跳曲");
        // 上限处继续失败：第 11、20 次都不再跳曲、计数饱和。
        for _ in 0..10 {
            let (c, keep) = advance_failure_count(next);
            assert_eq!(c, CONSECUTIVE_FAILURE_LIMIT, "上限处计数应饱和");
            assert!(!keep, "上限之后不得恢复自动跳曲");
        }
        assert_eq!(advance_failure_count(0), (1, true), "手动清零后可重新计数");
    }

    #[test]
    fn resume_secs_thresholds() {
        // 无保存进度 → None（从头播）。
        assert_eq!(resume_secs(None, Some(300.0)), None);
        // 距离结尾 ≥5 秒且 >0.5 秒 → 续播。
        assert_eq!(resume_secs(Some(100.0), Some(300.0)), Some(100.0));
        // 距结尾 5 秒内 → 视为已听完 → 从头播。
        assert_eq!(resume_secs(Some(297.0), Some(300.0)), None);
        // ≤0.5 秒 → 噪声，从头播。
        assert_eq!(resume_secs(Some(0.3), Some(300.0)), None);
        // 无时长信息 → 有保存进度即续播（>0.5）。
        assert_eq!(resume_secs(Some(100.0), None), Some(100.0));
    }

    #[test]
    fn toggle_decision_branches() {
        // 正在播放 → 暂停。
        assert_eq!(toggle_decision(true, false, None), PlaybackDecision::Pause);
        // 已到结尾 + 普通曲目 → 从头播。
        assert_eq!(toggle_decision(false, true, None), PlaybackDecision::Play);
        // 已到结尾 + CUE → 按区间播。
        assert_eq!(
            toggle_decision(false, true, Some(&cue(10_000, Some(20_000)))),
            PlaybackDecision::PlayRange {
                start_secs: 10.0,
                end_secs: Some(20.0)
            }
        );
        // 未到结尾 → 恢复。
        assert_eq!(
            toggle_decision(false, false, None),
            PlaybackDecision::Resume
        );
    }

    #[test]
    fn single_repeat_decision_branches() {
        // 播放中的 CUE 单曲循环 → 回片段起点 Seek（不重开文件）。
        assert_eq!(
            single_repeat_decision(true, Some(&cue(5_000, Some(15_000)))),
            PlaybackDecision::Seek { secs: 5.0 }
        );
        // 非播放 CUE → PlayRange 带回终点。
        assert_eq!(
            single_repeat_decision(false, Some(&cue(5_000, Some(15_000)))),
            PlaybackDecision::PlayRange {
                start_secs: 5.0,
                end_secs: Some(15.0)
            }
        );
        // 普通曲目 → 从头播。
        assert_eq!(single_repeat_decision(true, None), PlaybackDecision::Play);
    }
}
