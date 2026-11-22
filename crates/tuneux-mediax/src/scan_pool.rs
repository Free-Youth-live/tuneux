//! # 并行扫描池（三端共享）
//!
//! 目录遍历 + 标签读取并行化：1 walker 线程（递归 + 分类 + 同名整轨
//! 去重）+ N 标签工人线程（并行读标签 / 展开 CUE），结果流式回传。
//! 多核 SSD 下标签阶段近线性加速；HDD 受随机 IO 限制收益有限。
//!
//! 三端共用（tuneux / fx / max）：submit() 投递路径 → 主循环 try_recv()
//! 逐帧排空 → pending() > 0 表示扫描中。

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::metadata::TrackMetadata;
use crate::playlist_item::PlaylistItem;

// =============================================================================
// 公共类型
// =============================================================================

/// 扫描池回传给 UI 的事件。
#[derive(Debug)]
pub enum ScanEvent {
    /// 一个文件展开出的可加入条目（CUE 可包含多轨）。
    Items(Vec<PlaylistItem>),
    /// CUE 无法展开，必须保留诊断，不能让同名整轨被跳过后无声消失。
    CueError {
        /// 无法解析或找不到音频的 CUE 路径。
        path: PathBuf,
        /// 供 UI 显示的错误说明。
        message: String,
    },
}

/// 并行扫描池（walker + N 工人）。
pub struct ScanPool {
    /// 扫描请求发送端（UI → walker）。
    req_tx: Sender<PathBuf>,
    /// 事件接收端（工人 → UI；每项一批）。
    res_rx: Receiver<ScanEvent>,
    /// 在途扫描条目数（>0 = 扫描中）。
    pending: Arc<AtomicUsize>,
}

impl Default for ScanPool {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanPool {
    /// 创建扫描池（workers = min(CPU 核数, 8)，至少 1 个工人）。
    pub fn new() -> Self {
        let n_workers = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(4)
            .clamp(1, 8);
        Self::with_workers(n_workers)
    }

    /// 创建指定工人数的扫描池；0 按 1 处理，确保任务可完成。
    pub fn with_workers(n_workers: usize) -> Self {
        let (req_tx, req_rx) = unbounded::<PathBuf>();
        let (work_tx, work_rx) = unbounded::<PathBuf>();
        let (res_tx, res_rx) = unbounded::<ScanEvent>();
        let pending = Arc::new(AtomicUsize::new(0));

        // —— walker：遍历目录树，按目录过滤同名整轨镜像后喂工人 ——
        //
        // pending 计数规则（防假完成 + 防永久泄漏）：
        // · 根请求在 submit 时 +1（终态 pop 后归还）——覆盖排队期，防假完成；
        // · 子目录 push 进 stack 时 +1（L-S3：否则「根只含子目录」
        //   的树在目录列出完毕归还后 pending 瞬时归零 → 收尾判定提前触发）；
        // · 文件 send 给工人时 +1（工人在处理完成后归还）；
        // · 归还统一走 PendingGuard（Drop RAII，L-S4：工人 panic /
        //   walker 崩溃 / send 失败均不会泄漏计数）。
        {
            let work_tx = work_tx.clone();
            let pend = pending.clone();
            let _ = std::thread::Builder::new()
                .name("scan-walker".into())
                .spawn(move || {
                    while let Ok(root) = req_rx.recv() {
                        // L-S6：符号链接环防护——按 canonicalize 规范化路径
                        // 建 visited 集合；同一物理目录只遍历一次。
                        // 用 metadata().is_dir()（跟随 symlink）判定目录，
                        // 但入 visited 用 canonicalize 规范化后的路径。
                        let mut visited: std::collections::HashSet<PathBuf> =
                            std::collections::HashSet::new();
                        let mut stack = vec![root];
                        // 根请求在 submit 时已登记，pop 后由守卫归还。
                        while let Some(dir) = stack.pop() {
                            // RAII 守卫：无论这个目录的处理如何退出，
                            // 计数都必然归还（L-S4）。
                            let _guard = PendingGuard(&pend);
                            if dir.is_file() {
                                pend.fetch_add(1, Ordering::Relaxed);
                                let _ = work_tx.send(dir);
                                continue;
                            }
                            // L-S6：符号链接环——canonicalize 后查 visited
                            let canon = std::fs::canonicalize(&dir).unwrap_or(dir.clone());
                            if !visited.insert(canon) {
                                continue; // 已遍历过的物理目录（环或重复链接）
                            }
                            let Ok(rd) = std::fs::read_dir(&dir) else {
                                continue; // _guard Drop 时归还计数
                            };
                            let mut audios: Vec<PathBuf> = Vec::new();
                            let mut cue_stems: Vec<String> = Vec::new();
                            for e in rd.flatten() {
                                let fp = e.path();
                                if fp.is_dir() {
                                    // L-S3：子目录 push 时即计数——不然
                                    // 目录列出完归还后、下次 pop 前
                                    // pending 瞬时归零（假完成）。
                                    pend.fetch_add(1, Ordering::Relaxed);
                                    stack.push(fp);
                                    continue;
                                }
                                let Some(ext) = fp.extension().and_then(|x| x.to_str()) else {
                                    continue;
                                };
                                let el = ext.to_ascii_lowercase();
                                if el == "cue" {
                                    if let Some(st) = fp.file_stem().and_then(|x| x.to_str()) {
                                        cue_stems.push(st.to_string());
                                    }
                                    pend.fetch_add(1, Ordering::Relaxed);
                                    let _ = work_tx.send(fp);
                                } else if is_audio_ext(&el) {
                                    audios.push(fp);
                                }
                            }
                            // 有同名 .cue 的整轨镜像跳过（防与分轨重复）
                            for fp in audios {
                                let shadowed =
                                    fp.file_stem().and_then(|x| x.to_str()).is_some_and(|st| {
                                        cue_stems.iter().any(|c| c.eq_ignore_ascii_case(st))
                                    });
                                if !shadowed {
                                    pend.fetch_add(1, Ordering::Relaxed);
                                    let _ = work_tx.send(fp);
                                }
                            }
                            // _guard Drop：归还本目录的计数
                        }
                        // 根目录的计数在此归还（stack 空了 = 根的 _guard 已 Drop）
                    }
                });
        }

        // —— 标签工人：并行处理单个文件（标签读取 / CUE 展开） ——
        for wid in 0..n_workers.max(1) {
            let work_rx = work_rx.clone();
            let res_tx = res_tx.clone();
            let pend = pending.clone();
            let _ = std::thread::Builder::new()
                .name(format!("scan-tag-{wid}"))
                .spawn(move || {
                    while let Ok(fp) = work_rx.recv() {
                        // RAII 守卫：无论处理如何退出（含 panic），计数归还。
                        let _guard = PendingGuard(&pend);
                        // panic 隔离：单个坏文件崩溃不得杀死工人线程（否则 pending
                        // 永不归还，全部工人死亡后界面永久「扫描中」）。
                        let fp_error = fp.clone();
                        let outcome =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                let is_cue = fp
                                    .extension()
                                    .and_then(|x| x.to_str())
                                    .is_some_and(|x| x.eq_ignore_ascii_case("cue"));
                                if is_cue {
                                    match crate::cue::cue_items_from_cue_file(&fp) {
                                        Some((items, _, _)) => ScanEvent::Items(items),
                                        None => ScanEvent::CueError {
                                            path: fp.clone(),
                                            message: "CUE 解析失败、音频文件缺失或没有可播音轨"
                                                .into(),
                                        },
                                    }
                                } else {
                                    let md = TrackMetadata::from_file(&fp);
                                    ScanEvent::Items(vec![PlaylistItem {
                                        path: fp,
                                        album: md.album,
                                        track_number: md.track_number,
                                        cue: None,
                                    }])
                                }
                            }));
                        let event = outcome.unwrap_or(ScanEvent::CueError {
                            path: fp_error,
                            message: "内部错误：处理文件时异常退出（已跳过该文件）".into(),
                        });
                        let _ = res_tx.send(event);
                        // _guard Drop：归还计数
                    }
                });
        }

        Self {
            req_tx,
            res_rx,
            pending,
        }
    }

    /// 投递扫描请求（目录或文件路径）。
    pub fn submit(&self, path: PathBuf) {
        self.pending.fetch_add(1, Ordering::Relaxed);
        if self.req_tx.send(path).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// 非阻塞取一个事件（无事件返回 None）。
    pub fn try_recv(&self) -> Option<ScanEvent> {
        self.res_rx.try_recv().ok()
    }

    /// 在途扫描条目数（>0 = 扫描中）。
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }

    /// 是否可宣告本轮完成：工人完成且 UI 已排空全部结果。
    pub fn is_idle(&self) -> bool {
        self.pending() == 0 && self.res_rx.is_empty()
    }
}

/// 音频扩展名判断——直接引用 corex 事实源（tuneux_corex::KNOWN_AUDIO_EXTS）。
fn is_audio_ext(ext: &str) -> bool {
    // 同步纪律：本函数不持有任何清单副本，与 corex 零分叉；格式增删只改
    // corex 一处。2026-11-15 审查发现此处旧副本多出 aiff/aif 与 corex
    // 分叉——扫描池收入的条目在文件浏览器（按 corex 清单过滤）不可见，
    // 本修复删除副本、改为引用事实源（mediax 依赖 corex，方向不变）。
    // 注："ts" 会命中 TypeScript 同名扩展——支持 MPEG-TS 视频容器取音轨
    //（视频容器取音轨为既定功能）的已知取舍；仅影响用户显式选中的目录
    //（浏览器工厂层会过滤不可播项），不单独排除。
    tuneux_corex::KNOWN_AUDIO_EXTS.contains(&ext)
}

/// pending 计数的 RAII 守卫：Drop 时自动归还（防工人 panic / walker
/// 异常退出导致的计数永久泄漏——界面永久「扫描中」并压掉全部反馈）。
struct PendingGuard<'a>(&'a AtomicUsize);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn next_event(pool: &ScanPool) -> ScanEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = pool.try_recv() {
                return event;
            }
            assert!(Instant::now() < deadline, "扫描结果超时");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn zero_workers_still_processes_and_releases_count() {
        let path = std::env::temp_dir().join(format!(
            "tuneux_scan_zero_workers_{}.mp3",
            std::process::id()
        ));
        std::fs::write(&path, b"test").unwrap();
        let pool = ScanPool::with_workers(0);
        pool.submit(path.clone());
        assert_eq!(pool.pending(), 1, "提交即登记，不能出现假完成");
        match next_event(&pool) {
            ScanEvent::Items(items) => assert_eq!(items[0].path, path),
            _ => panic!("普通文件应产生条目"),
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while pool.pending() != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(pool.pending(), 0);
        assert!(pool.is_idle());
        let _ = std::fs::remove_file(path);
    }

    /// 扩展名清单与 corex 事实源同源（引用而非副本）：修复前本模块副本
    /// 多出 aiff/aif 与 corex 分叉，扫描池收入的条目在文件浏览器不可见。
    #[test]
    fn audio_exts_reference_corex_source() {
        assert!(is_audio_ext("flac"));
        assert!(is_audio_ext("mp3"));
        assert!(is_audio_ext("wv"));
        assert!(!is_audio_ext("aiff"), "aiff 不在 corex 清单，不得收入");
        assert!(!is_audio_ext("aif"), "aif 不在 corex 清单，不得收入");
        assert!(!is_audio_ext("txt"));
        assert!(!is_audio_ext("cue"), "cue 走独立分支，不算音频扩展名");
    }

    #[test]
    fn invalid_cue_reports_error_instead_of_empty_batch() {
        let path = std::env::temp_dir().join(format!(
            "tuneux_scan_invalid_cue_{}.cue",
            std::process::id()
        ));
        std::fs::write(&path, b"not a cue").unwrap();
        let pool = ScanPool::with_workers(1);
        pool.submit(path.clone());
        match next_event(&pool) {
            ScanEvent::CueError {
                path: error_path, ..
            } => assert_eq!(error_path, path),
            _ => panic!("损坏的 CUE 应报告错误"),
        }
        let _ = std::fs::remove_file(path);
    }
}
