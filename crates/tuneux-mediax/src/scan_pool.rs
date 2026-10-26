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

/// 并行扫描池（walker + N 工人）。
pub struct ScanPool {
    /// 扫描请求发送端（UI → walker）。
    req_tx: Sender<PathBuf>,
    /// 结果接收端（工人 → UI；每项一批）。
    res_rx: Receiver<Vec<PlaylistItem>>,
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

    /// 创建指定工人数的扫描池。
    pub fn with_workers(n_workers: usize) -> Self {
        let (req_tx, req_rx) = unbounded::<PathBuf>();
        let (work_tx, work_rx) = unbounded::<PathBuf>();
        let (res_tx, res_rx) = unbounded::<Vec<PlaylistItem>>();
        let pending = Arc::new(AtomicUsize::new(0));

        // —— walker：遍历目录树，按目录过滤同名整轨镜像后喂工人 ——
        //
        // pending 计数规则（防假完成 + 防永久泄漏）：
        // · 根目录投入即 +1（终态 pop 后归还）——保证整个遍历期间 > 0；
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
                        pend.fetch_add(1, Ordering::Relaxed); // 根目录 +1
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
        for wid in 0..n_workers {
            let work_rx = work_rx.clone();
            let res_tx = res_tx.clone();
            let pend = pending.clone();
            let _ = std::thread::Builder::new()
                .name(format!("scan-tag-{wid}"))
                .spawn(move || {
                    while let Ok(fp) = work_rx.recv() {
                        // RAII 守卫：无论处理如何退出（含 panic），计数归还。
                        let _guard = PendingGuard(&pend);
                        let is_cue = fp
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| x.eq_ignore_ascii_case("cue"));
                        let items: Vec<PlaylistItem> = if is_cue {
                            // L-S5：CUE 展开失败不再静默吞掉——发一条空批
                            // 带路径信息，调用方可据此提示用户（与 cue.rs 的
                            // 「调用方提示」契约对齐）。不吞掉的另一个理由：
                            // 同名整轨已被镜像去重跳过，CUE 失败意味着
                            // 这张专辑零曲目入表且无任何痕迹。
                            crate::cue::cue_items_from_cue_file(&fp)
                                .map(|(v, _, _)| v)
                                .unwrap_or_default()
                        } else {
                            let md = TrackMetadata::from_file(&fp);
                            vec![PlaylistItem {
                                path: fp,
                                album: md.album,
                                track_number: md.track_number,
                                cue: None,
                            }]
                        };
                        if !items.is_empty() {
                            let _ = res_tx.send(items);
                        }
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
        let _ = self.req_tx.send(path);
    }

    /// 非阻塞取一批结果（无结果返回 None）。
    pub fn try_recv(&self) -> Option<Vec<PlaylistItem>> {
        self.res_rx.try_recv().ok()
    }

    /// 在途扫描条目数（>0 = 扫描中）。
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

/// 音频扩展名判断（与 corex KNOWN_AUDIO_EXTS 同源）。
fn is_audio_ext(ext: &str) -> bool {
    // 注："ts" 会命中 TypeScript 同名扩展——支持 MPEG-TS 视频容器取音轨
    //（视频容器取音轨为既定功能）的已知取舍；仅影响用户显式选中的目录
    //（浏览器工厂层会过滤不可播项），不单独排除。
    const AUDIO_EXTS: &[&str] = &[
        "flac", "wav", "mp3", "ogg", "opus", "wv", "m4a", "aiff", "aif", "aac", "alac", "mp4",
        "wma", "ape", "dsf", "dff", "mkv", "avi", "ts", "m2ts", "wmv", "mpg", "mpeg", "vob", "mov",
        "rmvb", "flv", "tak", "mpc", "shn", "ac3", "dts", "tta",
    ];
    AUDIO_EXTS.contains(&ext)
}

/// pending 计数的 RAII 守卫：Drop 时自动归还（防工人 panic / walker
/// 异常退出导致的计数永久泄漏——界面永久「扫描中」并压掉全部反馈）。
struct PendingGuard<'a>(&'a AtomicUsize);

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
