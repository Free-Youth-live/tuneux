//! # FFmpeg 进程外解码后端
//!
//! ## 设计动机
//!
//! FFmpeg（libavcodec/libavformat）采用 LGPL/GPL 许可，直接链接会将
//! 传染条款带入 tuneux 整体二进制。为规避此风险，本后端以**子进程**方式
//! 调用 `ffmpeg` CLI，宿主进程仅通过管道读写 PCM 数据，不构成"衍生作品"
//! 意义上的链接——这是业界通行的合规隔离手段。
//!
//! 约束：FFmpeg 后端仅在桌面平台可选启用（移动端 / WASM 无 CLI）。
//!
//! ## 二进制查找链（ffmpeg 与 ffprobe 同规格镜像）
//!
//! 1. exe 同目录 `ffmpeg/bin/ffmpeg`（发布包捆绑，便携语义优先）；
//! 2. exe 同目录 `ffmpeg/bin/ffmpeg.exe`（Windows 变体）；
//! 3. **bundle 感知回退**：exe 位于 `<App>.app/Contents/MacOS/` 时，
//!    向上一级试 bundle 内的 `../Resources/ffmpeg/bin/`（macOS 打包
//!    把 ffmpeg/ 收进 `Contents/Resources/` 时命中）；
//! 4. 系统 PATH（用户自装）。
//!
//! 每个候选先跑 `-version` 试执行，再过**许可闸门**：配置串含
//! `--enable-gpl` / `--enable-nonfree` 即跳过该候选（我们只随附 LGPL
//! 构建，违例二进制拒绝使用）。首个可用的候选胜出；解析结果（含负
//! 结果）OnceLock 缓存至进程结束——中途安装 ffmpeg 需重启播放器重探。
//!
//! ## IPC 协议
//!
//! ```text
//! spawn: ffmpeg -hide_banner -loglevel error \
//!            [-ss <secs>]       \   // seek 时从目标秒开始
//!            -i <input_path>    \
//!            -vn -sn            \   // 丢弃视频/字幕流（视频容器只取音轨）
//!            [-ar 48000]        \   // 仅采样率探测失败时钉 48k（回退旧口径）
//!            -f wav             \   // WAV 容器：头部自描述采样率/声道
//!            -acodec pcm_f32le  \   // 32-bit float 小端（保住 24-bit 源位深）
//!            -ac 2              \   // 声道归一立体声
//!            pipe:1                 // stdout 输出
//! ```
//!
//! - **跟随音源采样率**：正常路径不加 `-ar`，输出率 = 音源原生率；
//!   宿主解析 WAV 头取实际率/声道（**不依赖 RIFF size 字段**——管道流
//!   该字段是 0 / 0xFFFFFFFF 占位，本实现只按块头顺序扫描到 `data`）。
//! - **stdout**：f32le PCM 流（交错 L,R,L,R,...），直接按 4 字节小端
//!   转 f32，无需旧版 s16le 的 ÷32768 归一化。
//! - **stderr**：ffmpeg 错误/警告文本，由独立线程异步读取并缓存，
//!   在 `DecodeError` 中透传（避免管道满阻塞子进程）。
//! - **seek**：kill 当前子进程，带 `-ss` 重新 spawn。简单可靠（无需
//!   维护 ffmpeg 内部状态），代价是 ~100ms 重启延迟。
//!
//! ## 一次探测多字段（ffprobe）
//!
//! `probe_sample_rate` 钩子与 `open` 共用同一发 ffprobe（flat
//! key=value 输出），顺带取时长 / 码率 / 编码名：时长回填
//! `AudioParams.duration`（收口长尾格式时长为空的缺口），编码名用于
//! 技术参数显示。**ffprobe 缺席或失败单独容错**：回退旧口径 48k 出口
//! （命令行钉 `-ar 48000`，行为同改造前），不影响解码主路。探测值与
//! WAV 头实测值均过**合理性钳制**（采样率 ≤ 768 kHz、声道 ≤ 8）——
//! 恶意容器声明的荒谬参数只 48k 降级或拒流，不参与缓冲分配。
//!
//! ## 降级策略
//!
//! `open` 时解析 ffmpeg 二进制；找不到（或全部候选被许可闸门拒绝）
//! 返回 `DecodeError::Unsupported`，工厂层据此回退或提示。不 panic。

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// 需要 FFmpeg 才能解码的文件扩展名集合（大小写不敏感）。
///
/// - 音频长尾 11 种：symphonia 不支持或支持不完整，是 FFmpeg 后端的主要目标；
/// - 视频容器 11 种：mkv/avi/ts/m2ts/wmv/mpg/mpeg/vob/mp4/mov/rmvb——
///   取音轨播放（命令行的 `-vn -sn` 本就丢弃画面，纯音频行为）。
///
/// 注意：**不含** symphonia 已支持的格式（mp3/flac/wav/ogg/m4a/aac/alac/opus/wv）
/// ——工厂对 symphonia 格式走进程内后端，ffmpeg 仅作长尾补位。
/// 曾含的 `ofr`（OptimFROG）已删：FFmpeg 源码无该格式实现，列入永远
/// 解码失败，属过度承诺。
const FFMPEG_EXTENSIONS: &[&str] = &[
    // —— 音频长尾 ——
    "ape", // Monkey's Audio
    "wma", // Windows Media Audio
    "flv", // Flash Video（音频轨）
    "tak", // TAK 无损
    "mpc", // Musepack
    "shn", // Shorten
    "ac3", // Dolby Digital
    "dts", // DTS
    "tta", // True Audio
    "dsf", // DSD Stream File（1-bit 音频，ffmpeg 解为 PCM）
    "dff", // DSDIFF（DSD 的另一种容器）
    // —— 视频容器（取音轨，不出画面） ——
    "mkv", "avi", "ts", "m2ts", "wmv", "mpg", "mpeg", "vob", "mp4", "mov", "rmvb",
];

/// ffmpeg / ffprobe 解析结果缓存（含负结果），进程内只解析一次。
static FFMPEG_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
static FFPROBE_BIN: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 流式 WAV 头最大扫描字节数（正常 < 200 字节；超限视为畸形输出）。
const WAV_HEADER_SCAN_LIMIT: usize = 64 * 1024;

/// 采样率合理性上限（Hz）：768 kHz 覆盖 768k PCM 与 DSD128 转换率
/// （705.6 kHz）。恶意构造的容器可声明荒谬采样率（u32 上限），若照单
/// 全收会触发巨额缓冲分配（4 GHz × 8 B/10 ≈ 3.4 GB）——本地 DoS 面，
/// 两处入口（ffprobe 探测值 / WAV 头实测值）都必须过未钩。
const MAX_SANE_SAMPLE_RATE: u32 = 768_000;
/// 声道数合理性上限：8 = 7.1 布局上限（输出侧另有 -ac 2 归一，
/// 此处仅防畸形头把 u16 上限当声道数参与缓冲尺寸计算）。
const MAX_SANE_CHANNELS: u16 = 8;

/// 采样率是否在合理范围（1 Hz ~ 768 kHz）。
fn sane_rate(rate: u32) -> bool {
    (1..=MAX_SANE_SAMPLE_RATE).contains(&rate)
}

/// 声道数是否在合理范围（1 ~ 8）。
fn sane_channels(ch: u16) -> bool {
    (1..=MAX_SANE_CHANNELS).contains(&ch)
}

/// 判断路径是否应交给 FFmpeg 后端处理（按扩展名，大小写不敏感）。
pub(crate) fn is_ffmpeg_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            let lower = ext.to_ascii_lowercase();
            FFMPEG_EXTENSIONS.contains(&lower.as_str())
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 二进制查找链 + 许可闸门
// ---------------------------------------------------------------------------

/// exe 目录是否形如 `<App>.app/Contents/MacOS`（macOS bundle 内可执行目录）。
fn is_macos_bundle_exe_dir(dir: &Path) -> bool {
    if dir.file_name().and_then(OsStr::to_str) != Some("MacOS") {
        return false;
    }
    let Some(contents) = dir.parent() else {
        return false;
    };
    if contents.file_name().and_then(OsStr::to_str) != Some("Contents") {
        return false;
    }
    contents
        .parent()
        .and_then(|app| app.extension())
        .and_then(OsStr::to_str)
        .map(|e| e.eq_ignore_ascii_case("app"))
        .unwrap_or(false)
}

/// 工具查找候选链（ffmpeg / ffprobe 同规格镜像；顺序即优先级）。
///
/// 纯函数：给定 exe 目录即可推导全部候选，便于单测查找链与 bundle 回退。
fn tool_candidates(exe_dir: &Path, tool: &str) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(6);
    // ①② exe 同目录 ffmpeg/bin/（含 .exe 变体）
    let bundled_bin = exe_dir.join("ffmpeg").join("bin");
    out.push(bundled_bin.join(tool));
    out.push(bundled_bin.join(format!("{tool}.exe")));
    // ③ bundle 感知回退：向上一步进 Resources/，试 bundle 内的 ffmpeg/bin/
    if is_macos_bundle_exe_dir(exe_dir) {
        let inside = exe_dir.join("../Resources/ffmpeg/bin");
        out.push(inside.join(tool));
        out.push(inside.join(format!("{tool}.exe")));
    }
    // ④ 系统 PATH
    out.push(PathBuf::from(tool));
    out
}

/// 试跑 `-version` 并取标准输出；执行失败或非零退出返回 None。
fn read_version_output(bin: &Path) -> Option<String> {
    let out = Command::new(bin)
        .arg("-version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 许可闸门：配置串含 `--enable-gpl` / `--enable-nonfree` 即不可用。
///
/// 只看 `configuration:` 行（避免 `--disable-gpl` 之类反向开关误伤）；
/// 该行缺席时退回全文本匹配（无配置串可查，含违例串仍应拒绝）。
fn has_forbidden_flags(version_output: &str) -> bool {
    let haystack = version_output
        .lines()
        .find(|l| l.starts_with("configuration:"))
        .unwrap_or(version_output);
    haystack.contains("--enable-gpl") || haystack.contains("--enable-nonfree")
}

/// 按候选顺序解析首个可用工具：存在 → 可执行 → 过许可闸门。
///
/// 绝对路径候选先查文件存在（快路径，不 fork）；末位 PATH 裸名直接
/// 尝试执行。任一候选被闸门拒绝则继续试下一个，全部落空返回 None。
fn resolve_tool_in(candidates: &[PathBuf]) -> Option<PathBuf> {
    for cand in candidates {
        if cand.is_absolute() && !cand.is_file() {
            continue;
        }
        let Some(version) = read_version_output(cand) else {
            continue;
        };
        if has_forbidden_flags(&version) {
            continue; // 许可闸门：违例构建拒绝使用
        }
        return Some(cand.clone());
    }
    None
}

/// 解析（并缓存）工具路径；找不到或全部候选未过闸门返回 None。
fn resolve_tool(tool: &str, cache: &OnceLock<Option<PathBuf>>) -> Option<PathBuf> {
    cache
        .get_or_init(|| {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf))?;
            resolve_tool_in(&tool_candidates(&exe_dir, tool))
        })
        .clone()
}

fn ffmpeg_bin() -> Option<PathBuf> {
    resolve_tool("ffmpeg", &FFMPEG_BIN)
}

fn ffprobe_bin() -> Option<PathBuf> {
    resolve_tool("ffprobe", &FFPROBE_BIN)
}

// ---------------------------------------------------------------------------
// ffprobe 一次探测多字段
// ---------------------------------------------------------------------------

/// 一发 ffprobe 顺带取回的字段（各字段独立可选，缺席不致命）。
#[derive(Debug, Default, Clone, PartialEq)]
struct ProbeInfo {
    /// 音源原生采样率（PCM 出口口径；DSD 容器 demuxer 已换算）。
    sample_rate: Option<u32>,
    /// 总时长（秒）。
    duration: Option<f64>,
    /// 码率（bps）。AudioParams 现无对应字段，暂留供属性显示扩展用。
    bit_rate: Option<u64>,
    /// 编码名（如 "ape"、"dts"），技术参数显示用。
    codec_name: Option<String>,
}

/// 解析 ffprobe flat key=value 输出（`default=noprint_wrappers=1`）。
///
/// 选 flat 而非 JSON：零依赖前提下 key=value 逐行解析最稳（JSON 需
/// 处理转义与嵌套）；本函数只取四个简单值字段，"N/A" 一律视为缺席。
fn parse_probe_output(text: &str) -> ProbeInfo {
    let mut info = ProbeInfo::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue; // 段标记 / 空行
        };
        let value = value.trim();
        if value.is_empty() || value == "N/A" {
            continue;
        }
        match key.trim() {
            "sample_rate" => info.sample_rate = value.parse().ok(),
            "duration" => info.duration = value.parse().ok(),
            "bit_rate" => info.bit_rate = value.parse().ok(),
            "codec_name" => info.codec_name = Some(value.to_string()),
            _ => {}
        }
    }
    info
}

/// 用 ffprobe 探测音源：原生采样率 + 时长 + 码率 + 编码名。
/// ffprobe 缺席 / 执行失败 / 文件不识 均返回 None（调用方自行回退）。
fn probe_media(ffprobe: &Path, path: &Path) -> Option<ProbeInfo> {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name,sample_rate",
            "-show_entries",
            "format=duration,bit_rate",
            "-of",
            "default=noprint_wrappers=1",
        ])
        // "--" 终结选项：文件名以 - 开头时防止被 ffprobe 当作选项解析
        //（已用实验区真实二进制验证支持；解码侧 -i 的参数位不受影响）。
        .arg("--")
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(parse_probe_output(&String::from_utf8_lossy(&out.stdout)))
}

/// 轻量采样率探测钩子（后端注册表用）：ffprobe 取音源原生率。
///
/// ffprobe 缺席 / 失败返回 None，引擎按既有口径回退
/// （`open` 侧同步回退 48k 出口，行为同改造前）。
pub(crate) fn probe_sample_rate(path: &Path) -> Option<u32> {
    let ffprobe = ffprobe_bin()?;
    probe_media(&ffprobe, path).and_then(|i| i.sample_rate)
}

// ---------------------------------------------------------------------------
// 流式 WAV 头解析
// ---------------------------------------------------------------------------

/// 流式 WAV 头解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WavHeader {
    /// 实际采样率（Hz）。
    sample_rate: u32,
    /// 实际声道数（命令行 `-ac 2` 归一后应为 2，以头部为准）。
    channels: u16,
    /// `data` 块负载起点（字节偏移）；其后全部为 PCM。
    data_offset: usize,
}

/// 解析 WAV 头：按块顺序扫描到 `data` 为止。
///
/// - **不得依赖 RIFF / data 的 size 字段**：管道流中它们是 0 或
///   0xFFFFFFFF 占位，本函数只认块头（id + size）顺序；
/// - 缓冲不足（头未收齐）返回 None，调用方继续读；
/// - `fmt ` 必须先到且给出有效率/声道，否则视为畸形。
fn parse_wav_header(buf: &[u8]) -> Option<WavHeader> {
    if buf.len() < 12 || &buf[..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u32)> = None;
    loop {
        if buf.len() < pos + 8 {
            return None; // 块头未收齐
        }
        let id = &buf[pos..pos + 4];
        let size = u32::from_le_bytes([buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]]);
        if id == b"fmt " {
            if size < 16 || buf.len() < pos + 8 + 16 {
                return None;
            }
            let channels = u16::from_le_bytes([buf[pos + 10], buf[pos + 11]]);
            let rate =
                u32::from_le_bytes([buf[pos + 12], buf[pos + 13], buf[pos + 14], buf[pos + 15]]);
            fmt = Some((channels, rate));
        } else if id == b"data" {
            // data 的 size 是占位值，找到即停，其后全是 PCM 负载。
            let (channels, sample_rate) = fmt?;
            if channels == 0 || sample_rate == 0 {
                return None;
            }
            return Some(WavHeader {
                sample_rate,
                channels,
                data_offset: pos + 8,
            });
        }
        // 其他块（fact/LIST 等）按真实 size 跳过；占位上限的未知块无法跳过。
        if size == u32::MAX {
            return None;
        }
        let step = 8usize
            .checked_add(size as usize)?
            .checked_add(size as usize & 1)?;
        pos = pos.checked_add(step)?;
    }
}

// ---------------------------------------------------------------------------
// 解码后端
// ---------------------------------------------------------------------------

/// 构造 ffmpeg 解码命令行参数（纯函数，便于断言）。
///
/// `pin_48k` 为 true 时钉 `-ar 48000`（探测失败的回退口径）；
/// 否则不加 `-ar`，输出跟随音源原生率。
fn decode_args(input: &Path, seek_secs: Option<f64>, pin_48k: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error"]
        .iter()
        .map(OsString::from)
        .collect();
    if let Some(secs) = seek_secs {
        args.push("-ss".into());
        args.push(format!("{secs:.3}").into());
    }
    args.push("-i".into());
    args.push(input.as_os_str().to_os_string());
    args.extend(["-vn", "-sn"].iter().map(OsString::from));
    if pin_48k {
        args.extend(["-ar", "48000"].iter().map(OsString::from));
    }
    args.extend(
        ["-f", "wav", "-acodec", "pcm_f32le", "-ac", "2", "pipe:1"]
            .iter()
            .map(OsString::from),
    );
    args
}

/// FFmpeg 进程外解码后端。
///
/// 一个实例对应一个已打开文件；换曲时丢弃旧实例、新建。
/// 所有 FFmpeg 交互都发生在子进程边界，本模块不链接任何 C 库。
pub(crate) struct FfmpegBackend {
    /// ffmpeg 子进程（stdout = WAV/PCM 管道；stderr 由异步线程读取）。
    child: Option<Child>,
    /// 解析后的 ffmpeg 路径（spawn 复用，不经 PATH 再查）。
    ffmpeg_bin: PathBuf,
    /// 技术参数（采样率为探测值或回退 48k；立体声；f32 出口 32 bit）。
    params: AudioParams,
    /// f32le 读取缓冲（100ms，按实际率/声道在头解析后校准）。
    pcm_buf: Vec<u8>,
    /// 头解析阶段的累计缓冲；头解析完成后转为遗留 PCM 字节暂存。
    pending: Vec<u8>,
    /// pending 中已消费位置。
    pending_pos: usize,
    /// WAV 头是否已解析（seek respawn 后复位重解析）。
    header_done: bool,
    /// 探测失败回退：命令行钉 `-ar 48000`（行为同改造前）。
    pin_48k: bool,
    /// 输入文件路径（seek respawn 时复用）。
    input_path: Box<Path>,
    /// stderr 异步读取缓存（错误透传用）。
    stderr_cache: Option<Arc<Mutex<String>>>,
}

impl FfmpegBackend {
    /// 打开文件并启动 ffmpeg 子进程。
    ///
    /// 先解析 ffmpeg 二进制（查找链 + 许可闸门，结果缓存）；不可用
    /// 返回 `Unsupported`（优雅降级）。ffprobe 探测失败仅回退 48k 出口，
    /// 不阻断打开。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        let ffmpeg = ffmpeg_bin().ok_or_else(|| {
            DecodeError::Unsupported(
                "ffmpeg 未找到，或二进制为不合规构建（含 --enable-gpl / --enable-nonfree）"
                    .to_string(),
            )
        })?;
        Self::open_impl(path, ffmpeg, ffprobe_bin())
    }

    /// 打开实现：二进制由调用方给定（测试可注入真实工具路径）。
    fn open_impl(
        path: &Path,
        ffmpeg: PathBuf,
        ffprobe: Option<PathBuf>,
    ) -> Result<Self, DecodeError> {
        // 一次探测多字段；ffprobe 缺席/失败/探测值荒谬（超出合理性钳制）
        // 均单独容错 → 回退 48k 出口。
        let probe = ffprobe.and_then(|p| probe_media(&p, path));
        let (sample_rate, pin_48k) = match probe.as_ref().and_then(|i| i.sample_rate) {
            Some(rate) if sane_rate(rate) => (Some(rate), false),
            _ => (Some(48_000), true),
        };
        let codec_name = probe
            .as_ref()
            .and_then(|i| i.codec_name.as_deref())
            // 探测命中：ffprobe 原样编码名（如 dsd_lsbf_planar，产品实测
            // 期望口径；不再加 FFmpeg/ 前缀与大写化，来源标注归属性弹窗）。
            .map(str::to_string)
            .unwrap_or_else(|| {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_uppercase())
                    .unwrap_or_else(|| "UNKNOWN".into());
                // 探测缺席的降级路径保留 FFmpeg/ 前缀：标记参数来自外部
                // 进程而非文件探测，与探测命中的裸名区分。
                format!("FFmpeg/{ext}")
            });
        let duration = probe.as_ref().and_then(|i| i.duration);

        let params = AudioParams::new(
            sample_rate,
            Some(2),  // 声道归一立体声（-ac 2）
            Some(32), // f32le 出口
            codec_name,
            0,
            duration,
        );

        let rate = sample_rate.unwrap_or(48_000) as usize;
        let backend = Self {
            child: None,
            ffmpeg_bin: ffmpeg,
            params,
            pcm_buf: vec![0u8; (rate * 2 * 4 / 10).max(4096)], // 100ms 立体声 f32le
            pending: Vec::new(),
            pending_pos: 0,
            header_done: false,
            pin_48k,
            input_path: path.to_path_buf().into_boxed_path(),
            stderr_cache: None,
        };
        backend.start()
    }

    /// 构造完成后启动首个子进程（open_impl 的收尾步）。
    fn start(mut self) -> Result<Self, DecodeError> {
        self.spawn_process(None)?;
        Ok(self)
    }

    /// 启动（或重启）ffmpeg 子进程。
    ///
    /// `seek_secs` 为 Some 时加 `-ss` 参数（从目标秒开始输出）。
    /// stdout 管道归本实例读取；stderr 由异步线程读入缓存。
    fn spawn_process(&mut self, seek_secs: Option<f64>) -> Result<(), DecodeError> {
        let mut cmd = Command::new(&self.ffmpeg_bin);
        cmd.args(decode_args(&self.input_path, seek_secs, self.pin_48k))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| DecodeError::Io(format!("spawn ffmpeg 失败：{e}")))?;

        // stderr 异步读取（避免管道满阻塞子进程；错误文本缓存供透传）
        if let Some(stderr) = child.stderr.take() {
            let cache = Arc::new(Mutex::new(String::new()));
            let cache2 = Arc::clone(&cache);
            std::thread::spawn(move || {
                let mut reader = stderr;
                let mut buf = String::new();
                let _ = reader.read_to_string(&mut buf);
                if let Ok(mut guard) = cache2.lock() {
                    *guard = buf;
                }
            });
            self.stderr_cache = Some(cache);
        }

        self.child = Some(child);
        Ok(())
    }

    /// 终止子进程并等待退出（seek respawn / Drop 时用）。
    fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// 读取并解析 WAV 头（管道流；size 字段为占位值，不依赖）。
    /// Ok(Some) 解析完成；Ok(None) 头部阶段即 EOF。
    fn read_stream_header(
        stdout: &mut impl std::io::Read,
        buf: &mut Vec<u8>,
    ) -> Result<Option<WavHeader>, DecodeError> {
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(h) = parse_wav_header(buf) {
                return Ok(Some(h));
            }
            if buf.len() > WAV_HEADER_SCAN_LIMIT {
                return Err(DecodeError::Decode(
                    "FFmpeg 输出头解析失败（非 WAV 或畸形流）".to_string(),
                ));
            }
            match stdout.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(DecodeError::Io(format!("读取 FFmpeg 输出失败：{e}"))),
            }
        }
    }
}

impl Drop for FfmpegBackend {
    fn drop(&mut self) {
        self.kill_child();
    }
}

impl DecoderBackend for FfmpegBackend {
    fn params(&self) -> &AudioParams {
        &self.params
    }

    /// 解码下一批样本：首调先解析 WAV 头，其后从 stdout 读 f32le 块。
    ///
    /// - 读到 0 字节（EOF）：检查子进程退出码，正常则 `Ok(None)`；
    /// - 子进程异常退出：读取 stderr 缓存并返回 `DecodeError::Decode`。
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        let Some(child) = self.child.as_mut() else {
            return Err(DecodeError::Decode("FFmpeg 子进程未启动".to_string()));
        };

        let mut filled = 0usize;
        let mut read_err: Option<DecodeError> = None;
        {
            let stdout = child
                .stdout
                .as_mut()
                .ok_or_else(|| DecodeError::Decode("FFmpeg stdout 管道不可用".to_string()))?;

            // 阶段一：首调解析 WAV 头，遗留字节转入 pending 当 PCM。
            if !self.header_done {
                match Self::read_stream_header(stdout, &mut self.pending) {
                    Ok(Some(header)) => {
                        // 合理性钳制：头部实测值荒谬（畸形/恶意流）时拒绝，
                        // 不让它参与缓冲尺寸计算（open 侧探测值同口径钳过）。
                        if !sane_rate(header.sample_rate) || !sane_channels(header.channels) {
                            read_err = Some(DecodeError::Decode(format!(
                                "FFmpeg 输出头参数异常（{} Hz / {} ch 超出合理范围）",
                                header.sample_rate, header.channels
                            )));
                        } else {
                            // 以头部实际率/声道校准缓冲（正常与探测一致；
                            // 不一致时按头部适配，参数仍以探测口径上报）。
                            let buf_len =
                                (header.sample_rate as usize * usize::from(header.channels) * 4
                                    / 10)
                                    .max(4096);
                            if self.pcm_buf.len() != buf_len {
                                self.pcm_buf = vec![0u8; buf_len];
                            }
                            self.pending_pos = header.data_offset;
                            self.header_done = true;
                        }
                    }
                    Ok(None) => {} // 头部阶段即 EOF：走下方 EOF 出口检查
                    Err(e) => read_err = Some(e),
                }
            }

            // 阶段二：读满一块（或读到 EOF 的尾部）；先消费 pending 遗留。
            if read_err.is_none() && self.header_done {
                while filled < self.pcm_buf.len() {
                    if self.pending_pos < self.pending.len() {
                        let n = (self.pcm_buf.len() - filled)
                            .min(self.pending.len() - self.pending_pos);
                        self.pcm_buf[filled..filled + n]
                            .copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
                        self.pending_pos += n;
                        filled += n;
                        if self.pending_pos >= self.pending.len() {
                            self.pending.clear();
                            self.pending_pos = 0;
                        }
                        continue;
                    }
                    match stdout.read(&mut self.pcm_buf[filled..]) {
                        Ok(0) => break, // EOF（正常结束或子进程退出）
                        Ok(n) => filled += n,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(e) => {
                            read_err = Some(DecodeError::Io(format!("读取 FFmpeg 输出失败：{e}")));
                            break;
                        }
                    }
                }
            }
        }

        if let Some(e) = read_err {
            return Err(e);
        }

        if filled == 0 {
            // EOF：若子进程异常退出（非 0），透传 stderr 错误
            if let Some(status) = child
                .try_wait()
                .map_err(|e| DecodeError::Io(format!("等待 ffmpeg 子进程失败：{e}")))?
            {
                if !status.success() {
                    let stderr = self
                        .stderr_cache
                        .as_ref()
                        .and_then(|c| c.lock().ok().map(|g| g.clone()))
                        .unwrap_or_default();
                    let detail = if stderr.trim().is_empty() {
                        format!("退出码 {:?}", status.code())
                    } else {
                        stderr.trim().to_string()
                    };
                    return Err(DecodeError::Decode(format!("FFmpeg 解码失败：{detail}")));
                }
            }
            return Ok(None);
        }

        // f32le → f32（4 字节小端直转，无旧版 ÷32768 归一化）
        let sample_count = filled / 4;
        let mut out = Vec::with_capacity(sample_count);
        let (chunks, _remainder) = self.pcm_buf[..sample_count * 4].as_chunks::<4>();
        for chunk in chunks {
            out.push(f32::from_le_bytes(*chunk));
        }
        Ok(Some(out))
    }

    /// Seek 到指定秒数（kill + 带 -ss 重新 spawn；WAV 头随之重解析）。
    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek("seek 秒数必须为非负有限值".to_string()));
        }
        self.kill_child();
        self.pending.clear();
        self.pending_pos = 0;
        self.header_done = false;
        self.spawn_process(Some(secs))?;
        Ok(secs)
    }
}

// 确保 FfmpegBackend 满足 Send 约束（DecoderBackend: Send）。
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<FfmpegBackend>();
};

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------- 测试工具 ----------------

    /// 建唯一临时目录（避免并行测试互相踩踏）。
    fn unique_tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tuneux_ffharness_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("临时目录可建");
        dir
    }

    /// 写一个打印假 `-version` 输出的可执行脚本（Unix）。
    #[cfg(unix)]
    fn write_fake_tool(path: &Path, version_text: &str) {
        use std::os::unix::fs::PermissionsExt;
        let script = format!("#!/bin/sh\ncat <<'EOF'\n{version_text}\nEOF\n");
        std::fs::write(path, script).expect("脚本可写");
        let mut perm = std::fs::metadata(path).expect("可读元数据").permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(path, perm).expect("可加可执行位");
    }

    const LGPL_VERSION: &str = "ffmpeg version 9.0.2\nconfiguration: --disable-network --disable-autodetect\nlibavutil 59.";
    const GPL_VERSION: &str =
        "ffmpeg version 9.0.2\nconfiguration: --enable-gpl --enable-libx264\nlibavutil 59.";
    const NONFREE_VERSION: &str =
        "ffmpeg version 9.0.2\nconfiguration: --enable-nonfree --enable-libfdk-aac\nlibavutil 59.";

    // ---------------- 查找链 ----------------

    /// 查找链优先级：exe 同目录 ffmpeg/bin（含 .exe 变体）最前，PATH 裸名兜底。
    #[test]
    fn candidates_order_bundled_first_path_last() {
        let exe_dir = Path::new("/tmp/fake_player");
        let c = tool_candidates(exe_dir, "ffmpeg");
        assert_eq!(c[0], exe_dir.join("ffmpeg/bin/ffmpeg"));
        assert_eq!(c[1], exe_dir.join("ffmpeg/bin/ffmpeg.exe"));
        assert_eq!(
            c.last(),
            Some(&PathBuf::from("ffmpeg")),
            "末位应为 PATH 裸名"
        );
        // 非 bundle 目录不触发旁置回退
        assert_eq!(c.len(), 3);
    }

    /// bundle 感知回退：exe 位于 <App>.app/Contents/MacOS 时，
    /// 候选链插入「向上一步进 Resources/ 的 ffmpeg/bin/」。
    #[test]
    fn candidates_bundle_aware_fallback() {
        let exe_dir = Path::new("/tmp/Tune.app/Contents/MacOS");
        let c = tool_candidates(exe_dir, "ffprobe");
        assert!(
            c.contains(&exe_dir.join("../Resources/ffmpeg/bin/ffprobe")),
            "bundle 内 exe 应有 Resources 回退候选：{c:?}"
        );
        // Resources 位在捆绑位之后、PATH 之前
        let pos_bundled = c
            .iter()
            .position(|p| *p == exe_dir.join("ffmpeg/bin/ffprobe"))
            .expect("捆绑位在链内");
        let pos_inside = c
            .iter()
            .position(|p| *p == exe_dir.join("../Resources/ffmpeg/bin/ffprobe"))
            .expect("Resources 位在链内");
        assert!(pos_bundled < pos_inside, "捆绑位优先于 Resources 位");
        assert_eq!(c.last(), Some(&PathBuf::from("ffprobe")));
        // 近 miss 不触发：MacOS 目录但外层不是 .app
        let not_bundle = Path::new("/tmp/plain/Contents/MacOS");
        assert_eq!(tool_candidates(not_bundle, "ffmpeg").len(), 3);
    }

    /// bundle 回退端到端：构造 Contents/{MacOS,Resources} 目录结构 +
    /// Resources 内伪二进制，resolve 应穿过 `../Resources` 命中。
    #[cfg(unix)]
    #[test]
    fn resolve_hits_bundle_resources_fallback() {
        let root = unique_tmp("bundle");
        let exe_dir = root.join("Tune.app/Contents/MacOS");
        std::fs::create_dir_all(&exe_dir).expect("bundle 目录可建");
        let inside = root.join("Tune.app/Contents/Resources/ffmpeg/bin");
        std::fs::create_dir_all(&inside).expect("Resources 目录可建");
        write_fake_tool(&inside.join("ffmpeg"), LGPL_VERSION);

        let resolved = resolve_tool_in(&tool_candidates(&exe_dir, "ffmpeg"));
        let hit = resolved.expect("应解析到 Resources 内 ffmpeg");
        assert!(
            hit.to_string_lossy().contains("../Resources/ffmpeg/bin"),
            "应命中 bundle 内 Resources 回退候选，实际：{hit:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 查找链优先级端到端：捆绑位存在时优先于 Resources 位。
    #[cfg(unix)]
    #[test]
    fn resolve_prefers_bundled_over_resources() {
        let root = unique_tmp("prio");
        let exe_dir = root.join("Tune.app/Contents/MacOS");
        std::fs::create_dir_all(&exe_dir).expect("bundle 目录可建");
        let bundled = exe_dir.join("ffmpeg/bin");
        std::fs::create_dir_all(&bundled).expect("捆绑目录可建");
        write_fake_tool(&bundled.join("ffmpeg"), LGPL_VERSION);
        let inside = root.join("Tune.app/Contents/Resources/ffmpeg/bin");
        std::fs::create_dir_all(&inside).expect("Resources 目录可建");
        write_fake_tool(&inside.join("ffmpeg"), LGPL_VERSION);

        let hit = resolve_tool_in(&tool_candidates(&exe_dir, "ffmpeg")).expect("应解析成功");
        assert_eq!(hit, bundled.join("ffmpeg"), "捆绑位应胜出");
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---------------- 许可闸门 ----------------

    /// 闸门判定：GPL / nonfree 配置串拒绝；LGPL 配置串放行；
    /// 反向开关（--disable-gpl）不误伤。
    #[test]
    fn sane_bounds_reject_extremes() {
        // 常规率全过：44.1k / 48k / 192k / 352.8k（DSD64）/ 705.6k（DSD128）/ 768k。
        for r in [44_100, 48_000, 192_000, 352_800, 705_600, 768_000] {
            assert!(sane_rate(r), "{r} Hz 应在合理范围");
        }
        // 荒谬值全拒：0 / 超上限 / u32 上限（恶意容器声明面）。
        for r in [0, 768_001, 4_000_000, u32::MAX] {
            assert!(!sane_rate(r), "{r} Hz 应被拒绝");
        }
        // 声道：1~8 过，0 / 9 / u16 上限拒。
        for c in [1u16, 2, 6, 8] {
            assert!(sane_channels(c));
        }
        for c in [0u16, 9, u16::MAX] {
            assert!(!sane_channels(c));
        }
    }

    #[test]
    fn gate_flags_forbidden_configs() {
        assert!(has_forbidden_flags(GPL_VERSION), "--enable-gpl 应拒绝");
        assert!(
            has_forbidden_flags(NONFREE_VERSION),
            "--enable-nonfree 应拒绝"
        );
        assert!(!has_forbidden_flags(LGPL_VERSION), "LGPL 构建应放行");
        assert!(
            !has_forbidden_flags("configuration: --disable-gpl --disable-nonfree"),
            "反向开关不应误伤"
        );
        // 无 configuration 行时退回全文本匹配：含违例串仍拒绝
        assert!(has_forbidden_flags("some wrapper --enable-gpl"));
        assert!(!has_forbidden_flags("ffmpeg version 9.0.2\nlibavutil 59."));
    }

    /// 闸门端到端：首个候选为 GPL 构建时跳过，命中后续合规候选。
    #[cfg(unix)]
    #[test]
    fn gate_skips_gpl_candidate() {
        let root = unique_tmp("gate");
        let gpl = root.join("ffmpeg_gpl");
        let lgpl = root.join("ffmpeg_lgpl");
        write_fake_tool(&gpl, GPL_VERSION);
        write_fake_tool(&lgpl, LGPL_VERSION);

        let hit = resolve_tool_in(&[gpl.clone(), lgpl.clone()]);
        assert_eq!(hit, Some(lgpl), "GPL 候选应被闸门跳过");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 闸门端到端：全部候选违例（或不可执行）时解析失败 → 上层 Unsupported。
    #[cfg(unix)]
    #[test]
    fn gate_rejects_all_forbidden() {
        let root = unique_tmp("gate_all");
        let gpl = root.join("ffmpeg");
        let nonfree = root.join("ffprobe");
        write_fake_tool(&gpl, GPL_VERSION);
        write_fake_tool(&nonfree, NONFREE_VERSION);

        assert_eq!(resolve_tool_in(&[gpl, nonfree]), None, "全违例应无可用");
        // 不存在的绝对路径候选被跳过，不 panic
        assert_eq!(
            resolve_tool_in(&[root.join("missing_ffmpeg")]),
            None,
            "不存在候选应落空"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---------------- ffprobe 输出解析 ----------------

    /// 一发探测多字段：采样率 / 时长 / 码率 / 编码名齐备。
    #[test]
    fn parse_probe_output_full() {
        let text = "codec_name=dts\nsample_rate=96000\nduration=12.345000\nbit_rate=1536000\n";
        let info = parse_probe_output(text);
        assert_eq!(info.sample_rate, Some(96000));
        assert_eq!(info.duration, Some(12.345));
        assert_eq!(info.bit_rate, Some(1_536_000));
        assert_eq!(info.codec_name.as_deref(), Some("dts"));
    }

    /// 容错：N/A 与缺字段视为缺席；垃圾行不 panic。
    #[test]
    fn parse_probe_output_tolerates_na_and_garbage() {
        let info = parse_probe_output("codec_name=N/A\nsample_rate=N/A\nduration=3.5\nhello\n=\n");
        assert_eq!(info.sample_rate, None);
        assert_eq!(info.codec_name, None);
        assert_eq!(info.duration, Some(3.5));
        assert_eq!(parse_probe_output(""), ProbeInfo::default());
    }

    // ---------------- 解码命令行 ----------------

    /// 跟随音源：正常路径无 -ar；回退路径钉 -ar 48000；其余参数恒定。
    #[test]
    fn decode_args_follow_source_rate() {
        let input = Path::new("/tmp/a.dsf");
        let follow = decode_args(input, None, false);
        let follow_str: Vec<String> = follow
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!follow_str.iter().any(|a| a == "-ar"), "跟随路径不得有 -ar");
        for want in [
            "-vn",
            "-sn",
            "-f",
            "wav",
            "-acodec",
            "pcm_f32le",
            "-ac",
            "2",
            "pipe:1",
        ] {
            assert!(follow_str.iter().any(|a| a == want), "缺参数 {want}");
        }

        let pinned = decode_args(input, None, true);
        let pinned_str: Vec<String> = pinned
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let pos = pinned_str
            .iter()
            .position(|a| a == "-ar")
            .expect("回退路径应有 -ar");
        assert_eq!(pinned_str[pos + 1], "48000", "回退应钉 48k");

        // seek 参数形如 -ss 1.500
        let seek = decode_args(input, Some(1.5), false);
        let seek_str: Vec<String> = seek
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let pos = seek_str.iter().position(|a| a == "-ss").expect("应有 -ss");
        assert_eq!(seek_str[pos + 1], "1.500");
    }

    // ---------------- WAV 头解析 ----------------

    /// 构造 WAV 头字节（riff_size / data_size 由调用方给，可填占位值）。
    fn wav_bytes(rate: u32, channels: u16, riff_size: u32, data_size: u32) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&riff_size.to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes()); // byte_rate（解析不依赖）
        v.extend_from_slice(&0u16.to_le_bytes()); // block_align
        v.extend_from_slice(&32u16.to_le_bytes()); // bits
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_size.to_le_bytes());
        v
    }

    /// 常规头：解析出率/声道与 data 偏移。
    #[test]
    fn wav_header_normal() {
        let buf = wav_bytes(96_000, 2, 1_000_000, 500_000);
        let h = parse_wav_header(&buf).expect("常规头应可解析");
        assert_eq!(h.sample_rate, 96_000);
        assert_eq!(h.channels, 2);
        assert_eq!(h.data_offset, 44);
    }

    /// 流式占位：RIFF size = 0、data size = 0xFFFFFFFF 时必须照常解析
    ///（管道输出即此形态，宿主不得依赖 size 字段）。
    #[test]
    fn wav_header_streaming_placeholders() {
        for (riff, data) in [(0u32, u32::MAX), (u32::MAX, u32::MAX), (0, 0)] {
            let mut buf = wav_bytes(352_800, 2, riff, data);
            buf.extend_from_slice(&[1, 2, 3, 4]); // 头后 PCM 字节不影响解析
            let h = parse_wav_header(&buf)
                .unwrap_or_else(|| panic!("占位 size (riff={riff:#x}, data={data:#x}) 应可解析"));
            assert_eq!(h.sample_rate, 352_800);
            assert_eq!(h.data_offset, 44);
        }
    }

    /// 含中间块（fact/LIST）：按真实块长跳过后命中 data。
    #[test]
    fn wav_header_skips_intermediate_chunks() {
        let buf = wav_bytes(48_000, 2, 0, 0);
        // 在 fmt 与 data 之间插入一个 4 字节 fact 块（fmt 块在偏移 36 结束）
        let mut with_fact = Vec::new();
        with_fact.extend_from_slice(&buf[..36]); // RIFF+fmt
        with_fact.extend_from_slice(b"fact");
        with_fact.extend_from_slice(&4u32.to_le_bytes());
        with_fact.extend_from_slice(&[0, 0, 0, 0]);
        with_fact.extend_from_slice(&buf[36..]); // data
        let h = parse_wav_header(&with_fact).expect("应跳过 fact 块");
        assert_eq!(h.data_offset, 56);
    }

    /// 容错：截断 / 垃圾 / 无 fmt 直出 data 均返回 None（不 panic）。
    #[test]
    fn wav_header_incomplete_or_garbage() {
        assert_eq!(parse_wav_header(b"RIFF"), None, "截断");
        assert_eq!(parse_wav_header(&[0u8; 64]), None, "垃圾");
        let mut no_fmt = Vec::new();
        no_fmt.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        no_fmt.extend_from_slice(b"data");
        no_fmt.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(parse_wav_header(&no_fmt), None, "无 fmt 直出 data 为畸形");
    }

    // ---------------- 扩展名清单 ----------------

    /// 视频容器在清单内（取音轨）；ofr 已删；大小写不敏感。
    #[test]
    fn extension_list_video_in_ofr_out() {
        for ext in [
            "mkv", "avi", "ts", "m2ts", "wmv", "mpg", "mpeg", "vob", "mp4", "mov", "rmvb",
        ] {
            let name = format!("a.{ext}");
            assert!(
                is_ffmpeg_path(Path::new(&name)),
                "视频容器 {ext} 应在清单内"
            );
        }
        assert!(!is_ffmpeg_path(Path::new("a.ofr")), "ofr 应已删除");
        assert!(is_ffmpeg_path(Path::new("A.MKV")), "大小写不敏感");
        for ext in [
            "ape", "wma", "tak", "mpc", "shn", "ac3", "dts", "tta", "dsf", "dff", "flv",
        ] {
            let name = format!("a.{ext}");
            assert!(is_ffmpeg_path(Path::new(&name)), "音频长尾 {ext} 应保留");
        }
        // symphonia 格式不在清单（进程内后端优先）
        assert!(!is_ffmpeg_path(Path::new("a.mp3")));
        assert!(!is_ffmpeg_path(Path::new("a.flac")));
    }

    // ---------------- 真实二进制冒烟（ignored） ----------------

    /// 生成最小 DSF 样本（DSD64 立体声，payload 为交替位图样）。
    /// DSF 头结构简单可手写，dsf 在扩展名清单内，正合长尾链路验证。
    fn write_test_dsf(path: &Path) {
        const CHANNELS: u32 = 2;
        const BLOCK: u32 = 4096;
        const BLOCKS: u64 = 4;
        let data_len = (BLOCK as u64) * (CHANNELS as u64) * BLOCKS;
        let file_size = 28 + 52 + 12 + data_len;
        let mut v = Vec::new();
        v.extend_from_slice(b"DSD ");
        v.extend_from_slice(&28u64.to_le_bytes());
        v.extend_from_slice(&file_size.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes()); // 无元数据
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&52u64.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes()); // format_version
        v.extend_from_slice(&0u32.to_le_bytes()); // format_id
        v.extend_from_slice(&2u32.to_le_bytes()); // channel_type = stereo
        v.extend_from_slice(&CHANNELS.to_le_bytes());
        v.extend_from_slice(&2_822_400u32.to_le_bytes()); // DSD64
        v.extend_from_slice(&1u32.to_le_bytes()); // bits per sample
        v.extend_from_slice(&(BLOCK as u64 * 8 * BLOCKS).to_le_bytes()); // sample_count
        v.extend_from_slice(&BLOCK.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes()); // reserved
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(12 + data_len).to_le_bytes());
        // 声道交错块：伪随机位图样（保证低通滤波后仍有带内能量，
        // 0xAA/0x55 之类周期图样的音调在 DSD→PCM 低通通带外会被滤成静音）
        let mut state: u64 = 0x2026_1015_d5f0_0001;
        for _ in 0..data_len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            v.push((state & 0xff) as u8);
        }
        std::fs::write(path, &v).expect("DSF 样本可写");
    }

    /// 真实工具目录：TUNEUX_FFMPEG_DIR 环境变量指向含 ffmpeg/ffprobe 的目录。
    fn real_tools() -> Option<(PathBuf, PathBuf)> {
        let dir = std::env::var("TUNEUX_FFMPEG_DIR").ok()?;
        let ffmpeg = Path::new(&dir).join("ffmpeg");
        let ffprobe = Path::new(&dir).join("ffprobe");
        if ffmpeg.is_file() && ffprobe.is_file() {
            Some((ffmpeg, ffprobe))
        } else {
            None
        }
    }

    /// 真实二进制全链路：probe 跟随原生率 → WAV 头解析 → 解出非零 f32 → seek。
    ///
    /// 运行：`TUNEUX_FFMPEG_DIR=<构建目录> cargo test -- --ignored`
    #[test]
    #[ignore = "需真实 ffmpeg/ffprobe；TUNEUX_FFMPEG_DIR=<dir> cargo test -- --ignored"]
    fn real_binary_full_chain_dsf() {
        let Some((ffmpeg, ffprobe)) = real_tools() else {
            eprintln!("跳过：TUNEUX_FFMPEG_DIR 未指向真实工具目录");
            return;
        };
        let tmp = unique_tmp("smoke");
        let dsf = tmp.join("smoke.dsf");
        write_test_dsf(&dsf);

        // 探测：DSD64 容器率 2822400，demuxer 已换算 PCM 率 352800
        let info = probe_media(&ffprobe, &dsf).expect("ffprobe 探测应成功");
        assert_eq!(info.sample_rate, Some(352_800), "应得 DSD→PCM 出口率");
        assert!(info.duration.is_some(), "应顺带取到时长");
        assert!(info.codec_name.is_some(), "应顺带取到编码名");

        // 打开 + 解码
        let mut backend =
            FfmpegBackend::open_impl(&dsf, ffmpeg, Some(ffprobe)).expect("应能打开 DSF");
        let params = backend.params();
        assert_eq!(params.sample_rate, Some(352_800), "参数应跟随音源率");
        assert_eq!(params.channels, Some(2));
        assert_eq!(params.bits_per_sample, Some(32), "f32le 出口应为 32 bit");

        let mut got_nonzero = false;
        for _ in 0..20 {
            match backend.decode_next() {
                Ok(Some(samples)) => {
                    if samples.iter().any(|&s| s.abs() > 1e-6) {
                        got_nonzero = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => panic!("解码失败：{e:?}"),
            }
        }
        assert!(got_nonzero, "应解出非零音频样本");

        // seek 后应继续解出非零 PCM（kill + -ss 重启 + WAV 头重解析）
        backend.seek(0.01).expect("seek 应成功");
        let mut got_after_seek = false;
        for _ in 0..20 {
            match backend.decode_next() {
                Ok(Some(samples)) => {
                    if samples.iter().any(|&s| s.abs() > 1e-6) {
                        got_after_seek = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => panic!("seek 后解码失败：{e:?}"),
            }
        }
        assert!(got_after_seek, "seek 后应能解出非零音频样本");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 探测失败回退链：ffprobe 缺席时 open 仍成功、参数钉 48k、命令行钉 -ar。
    ///
    /// 运行：`TUNEUX_FFMPEG_DIR=<dir> cargo test -- --ignored`
    #[test]
    #[ignore = "需真实 ffmpeg；TUNEUX_FFMPEG_DIR=<dir> cargo test -- --ignored"]
    fn real_binary_probe_missing_falls_back_48k() {
        let Some((ffmpeg, _)) = real_tools() else {
            eprintln!("跳过：TUNEUX_FFMPEG_DIR 未指向真实工具目录");
            return;
        };
        let tmp = unique_tmp("smoke48");
        let dsf = tmp.join("smoke.dsf");
        write_test_dsf(&dsf);

        let mut backend =
            FfmpegBackend::open_impl(&dsf, ffmpeg, None).expect("ffprobe 缺席也应打开");
        let params = backend.params();
        assert_eq!(params.sample_rate, Some(48_000), "探测失败应回退 48k");
        assert!(backend.pin_48k, "回退路径命令行应钉 -ar 48000");
        // 回退路径同样能解出 PCM（出口被 ffmpeg 钉在 48k）
        match backend.decode_next() {
            Ok(Some(samples)) => assert!(!samples.is_empty(), "回退路径应解出样本"),
            other => panic!("回退路径首解码应成功，实际：{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
