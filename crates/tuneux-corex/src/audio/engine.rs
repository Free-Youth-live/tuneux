//! # 音频引擎模块
//!
//! 整合解码、重采样、cpal 输出，提供统一的播放控制接口。
//! 采用**三线程模型**：
//!
//! ```text
//! 主线程（TUI）
//!   │  audio_cmd 通道
//!   ▼
//! 音频线程 ── 持有 cpal Stream（本线程独占，不跨线程）
//!   │  ▲                       │
//!   │  │ consumer               │ decoder_cmd 通道
//!   │  │ (消费 ringbuf)          ▼
//!   │  └── ringbuf ◀── producer ── 解码线程（常驻）
//!   │
//!   └ finished 通道 → 主线程（EOF 通知，用于自动下一曲）
//! ```
//!
//! ## 设计要点
//!
//! - **cpal Stream 必须在单一线程内创建并保活**：macOS 上 Stream 非 Send。
//!   音频线程独占 Stream，主线程通过 audio_cmd 通道控制。
//! - **解码线程常驻**：不按需 spawn/stop。producer 一直在解码线程
//!   （ringbuf 的 Prod 不可 Clone，必须固定在一个线程）。换曲时音频线程
//!   通过 decoder_cmd 通道通知解码线程加载新文件。
//! - **ringbuf 是无锁 SPSC**：解码线程（唯一 producer）→ 音频回调（唯一 consumer），
//!   不会阻塞实时回调。
//! - **状态共享用原子变量**：进度、音量、播放状态跨线程读取，避免 Mutex
//!   （回调持锁有死锁/延迟风险）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::{Receiver, Sender};

/// 音量原子存储比例：u32 千分比（0-1000），避免 f32 原子操作缺失。
const VOLUME_SCALE: u32 = 1000;

/// 主线程 → 音频线程 的控制命令。
///
/// #[non_exhaustive]：契约化保护——未来新增命令不破坏外部匹配。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AudioCmd {
    /// 加载并播放指定文件。
    Play(PathBuf),
    /// 加载并播放指定文件，加载完成后自动 seek 到 `secs`。
    /// 用于"接着上次听"——避免 Play+Seek 的竞态（Seek 在 Load 完成前到达会丢失）。
    PlayResume {
        /// 待播放的文件路径。
        path: PathBuf,
        /// 加载完成后 seek 的目标秒数。
        secs: f64,
    },
    /// 暂停。
    Pause,
    /// 恢复播放。
    Resume,
    /// 停止并清空。
    Stop,
    /// Seek 到指定秒数。
    Seek(f64),
    /// 设置音量（0.0-1.0）。
    SetVolume(f32),
    /// 预载下一曲（Gapless 无缝播放）：解码线程提前打开下一曲后端并缓存；
    /// 当前曲 EOF 时若预载就绪且采样率一致，直接无缝切换（不 draining/不重发 Play）。
    /// None = 取消预载（手动切曲/seek/停止时下发）。
    PreloadNext(Option<PathBuf>),
}

/// 音频线程 → 解码线程 的命令。
///
/// 注意：`NewProducer` 含 `HeapProd<f32>`（不可 Clone/Debug），
/// 故整个枚举不派生 Clone/Debug。
/// pub(crate)：内部实现命令（ringbuf 类型不进公共契约）。
pub(crate) enum DecoderCmd {
    /// 加载新文件并开始解码。
    Load(PathBuf),
    /// 加载新文件，加载完立刻 seek 到 `secs`（用于 PlayResume 路径）。
    LoadAndSeek { path: PathBuf, secs: f64 },
    /// Seek 到秒数。
    Seek(f64),
    /// 停止当前解码。
    Stop,
    /// 预载下一曲后端（Gapless）：open 并缓存，不启动解码。
    Preload(Option<PathBuf>),
    /// 更换 ringbuf 的 producer。
    /// 用于原生采样率直通：换曲时若采样率变化，音频线程会销毁旧 Stream、
    /// 重建 ringbuf（旧的 consumer 随旧 Stream drop），把新 producer 通过
    /// 此命令传给解码线程。解码线程 drop 旧 producer、用新的继续推数据。
    NewProducer(ringbuf::HeapProd<f32>),
    /// 退出线程。
    Exit,
}

/// 共享的播放状态。主线程读取用于 TUI，音频/解码线程更新。
///
/// 全部用原子变量，回调中读取/累加无锁。
/// 手动实现 Default（而非 derive），因为 spectrum_lr 是 N_BANDS 元素
/// 数组（当前 256），超过 Rust 标准库 Default 数组实现的 32 上限。
pub(super) struct SharedState {
    /// 进度基准秒数（Play/Seek 时设置，f64 位模式存 u64）。
    /// 实际进度 = position_base + frames_played / sample_rate。
    position_base: AtomicU64,
    /// 已播放帧数（音频回调累加）。
    frames_played: AtomicU64,
    /// 当前曲目总时长（秒）。未知为 0。
    duration_secs: AtomicU64,
    /// 音量（千分比 0-1000）。
    volume: AtomicU32,
    /// ReplayGain 线性增益（千分比，1000 = 1.0x；不启用时 1000）。
    /// 应用在音频回调（与音量相乘）。
    replay_gain: AtomicU32,
    /// ReplayGain 测量结果（dB × 100，Q 定点；未测量时为 u32::MAX 哨兵）。
    /// 解码线程在曲目分析完成后写入，App 在播放结束时读取缓存。
    measured_gain_db: AtomicU32,
    /// 是否正在播放。
    is_playing: AtomicBool,
    /// 换曲/seek 的"冲刷世代号"。每次换曲或 seek 时递增，
    /// 音频回调检测到变化时清空 consumer 缓冲（丢弃旧曲残留样本）。
    /// 用 AtomicU64 + 回调局部记忆实现"通知回调做某事"，无需回调持锁。
    flush_epoch: AtomicU64,
    /// 解码完成信号（decoder → audio）。解码线程遇到 EOF 或致命错误时
    /// 置 true，音频线程主循环每轮检查到后自动暂停流，避免播完继续吐静音。
    /// 用 AtomicBool 边沿触发：消费后立刻清零，防止重复触发。
    playback_finished: AtomicBool,
    /// 实时左右声道电平（peak amplitude 0.0-1.0，f32 位模式存 u32）。
    /// 音频回调每 ~10ms 累计窗口内最大绝对值后写入。
    /// 主线程读出来画 VU 表。AtomicU32 避免 f32 原子操作缺失。
    level_lr: [AtomicU32; 2],
    /// 实时左右声道频谱（每通道 N_BANDS 个频段，dB 归一化 0.0-1.0）。
    /// 音频回调每 ~10ms 对窗口内样本做 FFT，按对数分布的频段取聚合
    /// 能量后写入。主线程读出来画频谱条。
    /// 维度：[channel][band]，channel 0=L, 1=R。
    spectrum_lr: [[AtomicU32; super::spectrum::N_BANDS]; 2],
    /// 最近一次解码/打开错误信息。解码线程写、主线程读后清空。
    /// 必须用 Mutex 因为内容是 String（堆分配）。错误路径非热点，
    /// 不在音频回调里访问，不影响实时性。
    /// Mutex 不是 poison-proof 的，但跨线程仅 2 个持有者且都短锁，OK。
    last_error: Mutex<Option<String>>,
    /// 当前流的实际采样率（Hz）。换曲重建流时会更新，解码线程据此判断
    /// 是否需要软件重采样，而非用 spawn 时捕获的初始设备采样率。
    stream_sample_rate: AtomicU32,
    /// 当前是否为"原生采样率直通"（bit-perfect @ 文件采样率）。
    /// true=直通（Stream 用文件采样率，无软件重采样）；
    /// false=降级（设备不支持文件采样率，走 rubato 软件重采样）。
    /// 音频线程换曲时设置，主线程读取用于 UI：直通时技术参数行亮色，降级暗色。
    bitstream: AtomicBool,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            position_base: AtomicU64::new(0),
            frames_played: AtomicU64::new(0),
            duration_secs: AtomicU64::new(0),
            volume: AtomicU32::new(0),
            replay_gain: AtomicU32::new(VOLUME_SCALE),
            measured_gain_db: AtomicU32::new(u32::MAX),
            is_playing: AtomicBool::new(false),
            flush_epoch: AtomicU64::new(0),
            playback_finished: AtomicBool::new(false),
            level_lr: [AtomicU32::new(0), AtomicU32::new(0)],
            spectrum_lr: [
                core::array::from_fn(|_| AtomicU32::new(0)),
                core::array::from_fn(|_| AtomicU32::new(0)),
            ],
            last_error: Mutex::new(None),
            stream_sample_rate: AtomicU32::new(0),
            bitstream: AtomicBool::new(false),
        }
    }
}

impl SharedState {
    /// 设置进度基准（Play/Seek 时调用），同时清零帧计数。
    pub(super) fn reset_position(&self, base_secs: f64) {
        self.position_base
            .store(base_secs.to_bits(), Ordering::Relaxed);
        self.frames_played.store(0, Ordering::Relaxed);
    }
    /// 累加已播放帧数（音频回调调用）。
    pub(super) fn add_frames(&self, frames: u64) {
        self.frames_played.fetch_add(frames, Ordering::Relaxed);
    }
    /// 当前进度秒数 = base + frames / sample_rate。
    pub(super) fn position(&self, sample_rate: u32) -> f64 {
        let base = f64::from_bits(self.position_base.load(Ordering::Relaxed));
        let frames = self.frames_played.load(Ordering::Relaxed) as f64;
        base + frames / sample_rate.max(1) as f64
    }
    pub(super) fn set_duration(&self, secs: f64) {
        self.duration_secs.store(secs.to_bits(), Ordering::Relaxed);
    }
    pub(super) fn duration(&self) -> f64 {
        f64::from_bits(self.duration_secs.load(Ordering::Relaxed))
    }
    pub(super) fn set_volume(&self, vol: f32) {
        let v = (vol.clamp(0.0, 1.0) * VOLUME_SCALE as f32) as u32;
        self.volume.store(v, Ordering::Relaxed);
    }
    pub(super) fn volume(&self) -> f32 {
        self.volume.load(Ordering::Relaxed) as f32 / VOLUME_SCALE as f32
    }

    /// 设置 ReplayGain 线性增益（不启用传 1.0）。
    pub(super) fn set_replay_gain(&self, gain: f32) {
        let g = gain.clamp(0.0, 16.0); // 保守上限：+24dB
        let v = (g * VOLUME_SCALE as f32) as u32;
        self.replay_gain.store(v, Ordering::Relaxed);
    }
    /// 当前 ReplayGain 线性增益（默认 1.0）。
    pub(super) fn replay_gain(&self) -> f32 {
        self.replay_gain.load(Ordering::Relaxed) as f32 / VOLUME_SCALE as f32
    }

    /// 写入 ReplayGain 测量结果（dB × 100；无效传 None 清哨兵）。
    pub(super) fn set_measured_gain_db(&self, gain_db: f64) {
        let v = if gain_db.is_finite() {
            (gain_db * 100.0).round() as i64 as u32
        } else {
            u32::MAX
        };
        self.measured_gain_db.store(v, Ordering::Relaxed);
    }
    /// 读取并清除 ReplayGain 测量结果（dB）；未测量返回 None。
    pub(super) fn take_measured_gain_db(&self) -> Option<f64> {
        let v = self.measured_gain_db.swap(u32::MAX, Ordering::Relaxed);
        if v == u32::MAX {
            None
        } else {
            Some(v as i64 as f64 / 100.0)
        }
    }
    pub(super) fn set_playing(&self, playing: bool) {
        self.is_playing.store(playing, Ordering::Relaxed);
    }
    pub(super) fn is_playing(&self) -> bool {
        self.is_playing.load(Ordering::Relaxed)
    }
    /// 递增冲刷世代（换曲/seek 时调用），通知回调清空缓冲。
    pub(super) fn bump_flush(&self) -> u64 {
        self.flush_epoch.fetch_add(1, Ordering::Relaxed) + 1
    }
    /// 读取当前冲刷世代（回调对比检测变化）。
    pub(super) fn flush_epoch(&self) -> u64 {
        self.flush_epoch.load(Ordering::Relaxed)
    }
    /// 通知音频线程"解码已结束"（EOF 或致命错误）。由解码线程调用。
    pub(super) fn signal_playback_finished(&self) {
        self.playback_finished.store(true, Ordering::Relaxed);
    }
    /// 音频线程主循环轮询：若解码完成标志已置位则清零并返回 true。
    /// 返回 true 表示"刚收到一次结束事件，应暂停流"。
    pub(super) fn take_playback_finished(&self) -> bool {
        self.playback_finished.swap(false, Ordering::Relaxed)
    }
    /// 写入左右声道电平（peak amplitude，0.0-1.0）。音频回调调用。
    pub(super) fn set_level_lr(&self, l: f32, r: f32) {
        self.level_lr[0].store(l.to_bits(), Ordering::Relaxed);
        self.level_lr[1].store(r.to_bits(), Ordering::Relaxed);
    }
    /// 读取左右声道电平。主线程调用。
    pub(super) fn level_lr(&self) -> (f32, f32) {
        (
            f32::from_bits(self.level_lr[0].load(Ordering::Relaxed)),
            f32::from_bits(self.level_lr[1].load(Ordering::Relaxed)),
        )
    }
    /// 写入左右声道频谱（N_BANDS 个频段，dB 归一化 0.0-1.0）。
    /// 频谱在解码线程/音频线程里每 10ms 算一次后调用。
    pub(super) fn set_spectrum_lr(&self, l: &[f32], r: &[f32]) {
        for (i, &v) in l.iter().enumerate().take(super::spectrum::N_BANDS) {
            self.spectrum_lr[0][i].store(v.to_bits(), Ordering::Relaxed);
        }
        for (i, &v) in r.iter().enumerate().take(super::spectrum::N_BANDS) {
            self.spectrum_lr[1][i].store(v.to_bits(), Ordering::Relaxed);
        }
    }
    /// 读取左右声道频谱。主线程调用。
    pub(super) fn spectrum_lr(&self) -> [[f32; super::spectrum::N_BANDS]; 2] {
        // std::array::from_fn 按索引闭包构造二维数组，避免手写下标循环
        // （needless_range_loop 警告），逻辑与逐下标赋值完全等价。
        std::array::from_fn(|ch| {
            std::array::from_fn(|i| f32::from_bits(self.spectrum_lr[ch][i].load(Ordering::Relaxed)))
        })
    }
    /// 写入最近一次错误（解码/打开失败）。覆盖式。
    /// 解码线程在 eprintln! 之后调用，UI 能向用户显示"刚才跳过了什么"。
    pub(super) fn set_last_error(&self, msg: String) {
        if let Ok(mut g) = self.last_error.lock() {
            *g = Some(msg);
        }
    }
    /// 读取并清空最近一次错误。主线程轮询。
    /// 边沿触发：每条错误只显示一次（不会因轮询而重复显示）。
    pub(super) fn take_last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|mut g| g.take())
    }
    /// 设置当前流的实际采样率。音频线程换曲/重建流时调用。
    pub(super) fn set_stream_sample_rate(&self, sr: u32) {
        self.stream_sample_rate.store(sr, Ordering::Relaxed);
    }
    /// 读取当前流的采样率。解码线程据此判断是否需要重采样。
    pub(super) fn stream_sample_rate(&self) -> u32 {
        self.stream_sample_rate.load(Ordering::Relaxed)
    }
    /// 设置当前是否为原生采样率直通。音频线程换曲时调用。
    pub(super) fn set_bitstream(&self, on: bool) {
        self.bitstream.store(on, Ordering::Relaxed);
    }
    /// 读取直通状态。主线程 UI 据此决定技术参数行颜色。
    pub(super) fn bitstream(&self) -> bool {
        self.bitstream.load(Ordering::Relaxed)
    }
}

/// 音频引擎句柄。主线程持有，用于下发命令和读取状态。
///
/// 内部线程在 Engine 被 drop 时通过 close 信号自动退出。
/// #[non_exhaustive]：契约化保护（字段全私有，外部经 Engine::new 构造）。
#[non_exhaustive]
pub struct Engine {
    /// audio_cmd 发送端（主 → 音频线程）。
    cmd_tx: Sender<AudioCmd>,
    /// 共享状态。
    state: Arc<SharedState>,
    /// 设备采样率（计算进度用）。
    sample_rate: u32,
    /// EOF 事件接收端（音频/解码线程 → 主线程）。
    finished_rx: Receiver<()>,
    /// Gapless 无缝切曲事件接收端（解码线程通知"已无缝切换到预载曲目"）。
    track_switched_rx: Receiver<()>,
    /// 关闭信号。
    close: Arc<AtomicBool>,
}

impl Engine {
    /// 启动音频引擎（创建音频线程与解码线程），立即返回。
    pub fn new(initial_volume: f32) -> Result<Self, Box<dyn std::error::Error>> {
        let state = Arc::new(SharedState::default());
        state.set_volume(initial_volume);
        state.set_playing(false);

        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let (finished_tx, finished_rx) = crossbeam_channel::unbounded();
        let (track_switched_tx, track_switched_rx) = crossbeam_channel::unbounded();
        let close = Arc::new(AtomicBool::new(false));

        // 启动音频线程（内部再 spawn 解码线程）
        let sample_rate = super::engine_thread::spawn_threads(
            cmd_rx,
            Arc::clone(&state),
            finished_tx,
            track_switched_tx,
            Arc::clone(&close),
        )?;

        Ok(Self {
            cmd_tx,
            state,
            sample_rate,
            finished_rx,
            track_switched_rx,
            close,
        })
    }

    /// 下发命令（非阻塞）。
    pub fn send(&self, cmd: AudioCmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// 当前播放进度（秒）。
    pub fn position(&self) -> f64 {
        // 优先用当前流的真实采样率：原生采样率直通会重建流（如 44.1kHz），
        // 此时 frames 必须除以流采样率才是正确秒数；用构造时的初始值
        // （如 48000）会把进度算慢约 8%。0 表示尚未播放过，回退初始值。
        let sr = self.state.stream_sample_rate();
        let sr = if sr > 0 { sr } else { self.sample_rate };
        self.state.position(sr)
    }
    /// 总时长（秒），未知返回 0.0。
    pub fn duration(&self) -> f64 {
        self.state.duration()
    }
    /// 音量（0.0-1.0）。
    pub fn volume(&self) -> f32 {
        self.state.volume()
    }
    /// 是否正在播放。
    pub fn is_playing(&self) -> bool {
        self.state.is_playing()
    }
    /// 左右声道实时电平（peak amplitude，0.0-1.0）。
    /// 供 TUI 绘制 VU 表使用。未播放时为 (0.0, 0.0)。
    pub fn level_lr(&self) -> (f32, f32) {
        self.state.level_lr()
    }
    /// 左右声道实时频谱（dB 归一化 0.0-1.0，每通道 N_BANDS 段）。
    /// 供 TUI 绘制频谱条使用。未播放时全 0。
    pub fn spectrum_lr(&self) -> [[f32; super::spectrum::N_BANDS]; 2] {
        self.state.spectrum_lr()
    }
    /// 当前是否为原生采样率直通（bit-perfect @ 文件采样率）。
    /// UI 据此决定技术参数行的颜色：直通亮色，降级暗色。
    pub fn bitstream(&self) -> bool {
        self.state.bitstream()
    }

    /// 设置 ReplayGain 线性增益（不启用传 1.0）。
    pub fn set_replay_gain(&self, gain: f32) {
        self.state.set_replay_gain(gain);
    }

    /// 读取并清除 ReplayGain 测量结果（dB）；未测量返回 None。
    pub fn take_measured_gain_db(&self) -> Option<f64> {
        self.state.take_measured_gain_db()
    }
    /// 测试用：手动注入频谱（绕过真实音频回调 + FFT）。
    /// TUI snapshot 测试用，生产代码不应调用。
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_spectrum_lr_for_test(&self, l: &[f32], r: &[f32]) {
        self.state.set_spectrum_lr(l, r);
    }
    /// 取最近一次解码/打开错误（如"打开失败：xxx"），并清空。
    /// 错误显示后由 TUI 调用一次就清，避免重复显示。
    pub fn take_last_error(&self) -> Option<String> {
        self.state.take_last_error()
    }
    /// 测试用：手动注入电平（绕过真实音频回调）。
    /// TUI snapshot 测试用，生产代码不应调用。
    #[cfg(any(test, feature = "test-helpers"))]
    pub fn set_level_lr_for_test(&self, l: f32, r: f32) {
        self.state.set_level_lr(l, r);
    }

    /// 尝试取出 EOF 事件（非阻塞）。
    /// 返回 Some(()) 表示当前曲目已播完，主线程据此切下一曲。
    pub fn poll_finished(&self) -> Option<()> {
        self.finished_rx.try_recv().ok()
    }

    /// 尝试取出 Gapless 无缝切曲事件（非阻塞）。
    /// 返回 Some(()) 表示解码线程已无缝切换到预载的下一曲
    /// （前端应只更新 UI/当前曲目索引，**不要**重发 Play）。
    pub fn poll_track_switched(&self) -> Option<()> {
        self.track_switched_rx.try_recv().ok()
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.close.store(true, Ordering::Relaxed);
        let _ = self.cmd_tx.send(AudioCmd::Stop);
    }
}
