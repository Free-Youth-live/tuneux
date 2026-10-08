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
        {
            let work_tx = work_tx.clone();
            let pend = pending.clone();
            let _ = std::thread::Builder::new()
                .name("scan-walker".into())
                .spawn(move || {
                    while let Ok(root) = req_rx.recv() {
                        let mut stack = vec![root];
                        while let Some(dir) = stack.pop() {
                            if dir.is_file() {
                                pend.fetch_add(1, Ordering::Relaxed);
                                let _ = work_tx.send(dir);
                                continue;
                            }
                            // 目录在途标记：列出期间 pending > 0（防假完成）
                            pend.fetch_add(1, Ordering::Relaxed);
                            let Ok(rd) = std::fs::read_dir(&dir) else {
                                pend.fetch_sub(1, Ordering::Relaxed);
                                continue;
                            };
                            let mut audios: Vec<PathBuf> = Vec::new();
                            let mut cue_stems: Vec<String> = Vec::new();
                            for e in rd.flatten() {
                                let fp = e.path();
                                if fp.is_dir() {
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
                            // 目录列出完毕：在途标记归还
                            pend.fetch_sub(1, Ordering::Relaxed);
                        }
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
                        let is_cue = fp
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| x.eq_ignore_ascii_case("cue"));
                        let items: Vec<PlaylistItem> = if is_cue {
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
                        pend.fetch_sub(1, Ordering::Relaxed);
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
    const AUDIO_EXTS: &[&str] = &[
        "flac", "wav", "mp3", "ogg", "opus", "wv", "m4a", "aiff", "aif", "aac", "alac", "mp4",
        "wma", "ape", "dsf", "dff",
    ];
    AUDIO_EXTS.contains(&ext)
}
