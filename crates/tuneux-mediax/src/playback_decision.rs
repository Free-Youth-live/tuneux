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

/// 断点续播判定：距结尾 5 秒内视为已听完 → 从头播（None）；否则续播（Some）。
///
/// 纯函数：`saved_secs` 为上次保存的进度、`duration` 为曲目总时长。
pub fn resume_secs(saved_secs: Option<f64>, duration: Option<f64>) -> Option<f64> {
    let already_finished = matches!(
        (saved_secs, duration),
        (Some(s), Some(d)) if s >= d - 5.0
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
