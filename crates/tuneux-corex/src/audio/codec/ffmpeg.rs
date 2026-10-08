//! # FFmpeg 进程外解码后端
//!
//! ## 设计动机
//!
//! FFmpeg（libavcodec/libavformat）采用 LGPL/GPL 许可，直接链接会将
//! 传染条款带入 tuneux 整体二进制。为规避此风险，本后端以**子进程**方式
//! 调用系统已安装的 `ffmpeg` CLI，宿主进程仅通过管道读写 PCM 数据，
//! 不构成"衍生作品"意义上的链接——这是业界通行的合规隔离手段。
//!
//! 约束：FFmpeg 后端仅在桌面平台可选启用（移动端 / WASM 无 CLI）。
//!
//! ## IPC 协议
//!
//! ```text
//! spawn: ffmpeg -hide_banner -loglevel error \
//!            [-ss <secs>]      \   // seek 时从目标秒开始
//!            -i <input_path>   \
//!            -vn -sn           \   // 丢弃视频/字幕流
//!            -f s16le          \   // 输出格式：有符号 16-bit 小端 PCM
//!            -ac 2             \   // 强制立体声（下游统一双声道）
//!            -ar 48000         \   // 固定 48 kHz（与 Opus 原生率对齐）
//!            pipe:1                // stdout 输出 PCM
//! ```
//!
//! - **stdout**：原始 s16le PCM 流（交错 L,R,L,R,...），宿主侧读取并
//!   转换为 f32（÷32768.0）。
//! - **stderr**：ffmpeg 错误/警告文本，由独立线程异步读取并缓存，
//!   在 `DecodeError` 中透传（避免管道满阻塞子进程）。
//! - **seek**：方案 A——kill 当前子进程，带 `-ss` 重新 spawn。
//!   简单可靠（无需维护 ffmpeg 内部状态），代价是 ~100ms 重启延迟。
//!
//! ## 降级策略
//!
//! `open` 时先探测 `ffmpeg` 二进制是否存在；不存在返回
//! `DecodeError::Unsupported`，工厂层据此回退或提示安装。不 panic。

use std::io::Read;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};

use super::decoder::{AudioParams, DecodeError, DecoderBackend};

/// 需要 FFmpeg 才能解码的音频文件扩展名集合。
///
/// 这些格式 symphonia 不支持或支持不完整，是 FFmpeg 后端的主要目标。
/// 匹配规则：大小写不敏感。
///
/// 注意：**不含** symphonia 已支持的格式（mp3/flac/wav/ogg/m4a/aac/alac/opus/wv）
/// ——工厂对 symphonia 格式走进程内后端，ffmpeg 仅作长尾补位。
const FFMPEG_EXTENSIONS: &[&str] = &[
    "ape", // Monkey's Audio
    "wma", // Windows Media Audio
    "flv", // Flash Video（音频轨）
    "tak", // TAK 无损
    "ofr", // OptimFROG
    "mpc", // Musepack
    "shn", // Shorten
    "ac3", // Dolby Digital
    "dts", // DTS
    "tta", // True Audio
    "dsf", // DSD Stream File（SACD 1-bit 音频，ffmpeg 自动 DSD→PCM）
    "dff", // DSDIFF（DSD 的另一种容器，SACD 1-bit 音频）
];

/// ffmpeg 可用性缓存：首次 `ffmpeg -version` 探测后复用，
/// 避免每次打开长尾格式都 fork 一次子进程探测。
/// 注意：负结果（不可用）同样缓存至进程结束——中途安装 ffmpeg 需重启播放器后重探。
static FFMPEG_AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// 探测（并缓存）ffmpeg 命令是否可用。
fn ffmpeg_available() -> bool {
    *FFMPEG_AVAILABLE.get_or_init(|| {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    })
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

/// FFmpeg 进程外解码后端（完整实现）。
///
/// 一个实例对应一个已打开音频文件；换曲时丢弃旧实例、新建。
/// 所有 FFmpeg 交互都发生在子进程边界，本模块不链接任何 C 库。
pub(crate) struct FfmpegBackend {
    /// ffmpeg 子进程（stdout = PCM 管道；stderr 由异步线程读取）。
    child: Option<Child>,
    /// 技术参数（固定：48 kHz / 立体声 / 16 bit，与命令行参数一致）。
    params: AudioParams,
    /// s16le 读取缓冲（100ms @48kHz/stereo = 19200 字节）。
    pcm_buf: Vec<u8>,
    /// 输入文件路径（seek respawn 时复用）。
    input_path: Box<Path>,
    /// stderr 异步读取缓存（错误透传用）。
    stderr_cache: Option<Arc<Mutex<String>>>,
}

impl FfmpegBackend {
    /// 打开音频文件并启动 ffmpeg 子进程。
    ///
    /// 先探测 `ffmpeg` 二进制可用性；不可用返回 `Unsupported`（优雅降级）。
    /// 可用则 spawn 子进程开始输出 PCM。
    pub(crate) fn open(path: &Path) -> Result<Self, DecodeError> {
        // —— 降级检查：ffmpeg 二进制是否可用（结果缓存，只探测一次） ——
        if !ffmpeg_available() {
            return Err(DecodeError::Unsupported(
                "ffmpeg 未安装或不可用；请安装 FFmpeg 后重试".to_string(),
            ));
        }

        let ext_display = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_uppercase())
            .unwrap_or_else(|| "UNKNOWN".into());

        let params = AudioParams::new(
            Some(48_000), // FFmpeg 输出固定 48 kHz（与 -ar 一致）
            Some(2),      // 强制立体声（与 -ac 一致）
            Some(16),     // s16le 输出 → 16 bit
            format!("FFmpeg/{ext_display}"),
            0,
            None, // 时长待 ffprobe 提取（后续增强）
        );

        let mut backend = Self {
            child: None,
            params,
            pcm_buf: vec![0u8; 19_200], // 100ms @48kHz/stereo/s16le
            input_path: path.to_path_buf().into_boxed_path(),
            stderr_cache: None,
        };
        backend.spawn_process(None)?;
        Ok(backend)
    }

    /// 启动（或重启）ffmpeg 子进程。
    ///
    /// `seek_secs` 为 Some 时加 `-ss` 参数（从目标秒开始输出）。
    /// stdout 管道归本实例读取；stderr 由异步线程读入缓存。
    fn spawn_process(&mut self, seek_secs: Option<f64>) -> Result<(), DecodeError> {
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.arg("-hide_banner").arg("-loglevel").arg("error");
        if let Some(secs) = seek_secs {
            cmd.arg("-ss").arg(format!("{secs:.3}"));
        }
        cmd.arg("-i")
            .arg(self.input_path.as_ref())
            .arg("-vn")
            .arg("-sn")
            .arg("-f")
            .arg("s16le")
            .arg("-ac")
            .arg("2")
            .arg("-ar")
            .arg("48000")
            .arg("pipe:1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

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

    /// 解码下一批样本：从 stdout 读 s16le 块 → 转交错 f32。
    ///
    /// - 读到 0 字节（EOF）：检查子进程退出码，正常则 `Ok(None)`；
    /// - 子进程异常退出：读取 stderr 缓存并返回 `DecodeError::Decode`。
    fn decode_next(&mut self) -> Result<Option<Vec<f32>>, DecodeError> {
        let Some(child) = self.child.as_mut() else {
            return Err(DecodeError::Decode("FFmpeg 子进程未启动".to_string()));
        };
        let stdout = child
            .stdout
            .as_mut()
            .ok_or_else(|| DecodeError::Decode("FFmpeg stdout 管道不可用".to_string()))?;

        // 读满一块（或读到 EOF 的尾部）
        let mut filled = 0usize;
        while filled < self.pcm_buf.len() {
            match stdout.read(&mut self.pcm_buf[filled..]) {
                Ok(0) => break, // EOF（正常结束或子进程退出）
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    return Err(DecodeError::Io(format!("读取 FFmpeg 输出失败：{e}")));
                }
            }
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

        // s16le → f32（交错样本，÷32768.0 归一化）
        let sample_count = filled / 2;
        let mut out = Vec::with_capacity(sample_count);
        for i in 0..sample_count {
            let bytes = [self.pcm_buf[i * 2], self.pcm_buf[i * 2 + 1]];
            let s = i16::from_le_bytes(bytes);
            out.push(s as f32 / 32768.0);
        }
        Ok(Some(out))
    }

    /// Seek 到指定秒数（方案 A：kill + 带 -ss 重新 spawn）。
    fn seek(&mut self, secs: f64) -> Result<f64, DecodeError> {
        if !secs.is_finite() || secs < 0.0 {
            return Err(DecodeError::Seek("seek 秒数必须为非负有限值".to_string()));
        }
        self.kill_child();
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

    /// 探测系统是否安装了可用的 ffmpeg 命令。
    ///
    /// 返回 true 才继续集成测试；本机未装 ffmpeg 时整个模块跳过
    /// （CI 上由 workflow 安装 ffmpeg 后执行 `cargo test -- --ignored`）。
    fn probe_ffmpeg_for_test() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    }

    /// 用系统 ffmpeg 生成一段 AC3 测试音频（1 秒 440Hz 正弦波）。
    ///
    /// AC3 属于 FFMPEG_EXTENSIONS 长尾格式，symphonia 不支持，
    /// 只能走进程外 ffmpeg 后端——正好用它验证整条链路。
    fn generate_ac3_sample(path: &std::path::Path) -> bool {
        std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:a",
                "ac3",
            ])
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|st| st.success())
            .unwrap_or(false)
    }

    /// FFmpeg 进程外后端完整链路：探测 → 生成样本 → 打开 → 解码出非零 PCM。
    ///
    /// 运行方式：`cargo test -p tuneux-corex -- --ignored`（需系统安装 ffmpeg）。
    #[test]
    #[ignore = "需要系统安装 ffmpeg；运行：cargo test -p tuneux-corex -- --ignored"]
    fn ffmpeg_backend_decodes_generated_ac3() {
        if !probe_ffmpeg_for_test() {
            eprintln!("跳过：系统未安装 ffmpeg");
            return;
        }
        let tmp = std::env::temp_dir().join("tuneux_test_generated.ac3");
        assert!(generate_ac3_sample(&tmp), "用 ffmpeg 生成 AC3 样本失败");

        let mut backend = FfmpegBackend::open(&tmp).expect("应能打开生成的 AC3");
        let params = backend.params();
        assert_eq!(params.sample_rate, Some(48000), "AC3 解码采样率应为 48kHz");

        // 解码若干批，确认有非零 PCM（正弦波必然非零）。
        let mut got_nonzero = false;
        for _ in 0..10 {
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
        let _ = std::fs::remove_file(&tmp);
    }

    /// FFmpeg 后端 seek：kill + respawn 后应能继续解出非零 PCM。
    ///
    /// 运行方式：`cargo test -p tuneux-corex -- --ignored`（需系统安装 ffmpeg）。
    #[test]
    #[ignore = "需要系统安装 ffmpeg；运行：cargo test -p tuneux-corex -- --ignored"]
    fn ffmpeg_backend_seek_then_decode() {
        if !probe_ffmpeg_for_test() {
            eprintln!("跳过：系统未安装 ffmpeg");
            return;
        }
        let tmp = std::env::temp_dir().join("tuneux_test_seek.ac3");
        assert!(generate_ac3_sample(&tmp), "用 ffmpeg 生成 AC3 样本失败");

        let mut backend = FfmpegBackend::open(&tmp).expect("应能打开生成的 AC3");
        // 先解一批，确认就绪
        let _ = backend.decode_next().expect("首批解码应成功");
        // seek 到 0.5 秒（样本共 1 秒），respawn 后应返回实际 seek 位置
        let actual = backend.seek(0.5).expect("seek 应成功");
        assert!(actual > 0.0, "seek 应返回非零位置");
        // respawn 后继续解码，应有非零 PCM
        let mut got_nonzero = false;
        for _ in 0..10 {
            match backend.decode_next() {
                Ok(Some(samples)) => {
                    if samples.iter().any(|&s| s.abs() > 1e-6) {
                        got_nonzero = true;
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => panic!("seek 后解码失败：{e:?}"),
            }
        }
        assert!(got_nonzero, "seek 后应能解出非零音频样本");
        let _ = std::fs::remove_file(&tmp);
    }
}
