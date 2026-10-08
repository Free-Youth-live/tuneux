//! # 解码子层（codec）：纯字节解析 + 文件/进程 IO
//!
//! 与实时引擎（engine/）完全解耦：不引线程、不引 cpal、不引 DSP。
//! 各后端（symphonia / WAV / FLAC / WavPack / Opus / CD-DA / FFmpeg）
//! 经注册表分发，互相只引用 decoder 里定义的共享类型。
//!
//! ## 窄腰约定
//!
//! 本目录对外（engine 侧）只经下方 pub(crate) use 暴露五个符号；
//! 其余类型与函数一律模块私有，不得直接跨层引用。方向断言（codec
//! 不依赖 engine）由 guard 与单测双重守护。

pub(crate) mod cdda;
pub(crate) mod decoder;
pub(crate) mod ffmpeg;
pub(crate) mod flac;
pub(crate) mod ogg_opus;
pub(crate) mod opus;
pub(crate) mod probe;
pub(crate) mod wav;
pub(crate) mod wavpack;

// —— 窄腰：engine 侧只准经此五符号使用 codec ——
// allow(unused)：这是接口声明，个别符号可能暂未被消费但属契约面。
#[allow(unused_imports)]
pub(crate) use decoder::{
    open_backend, probe_sample_rate, AudioParams, DecodeError, DecoderBackend,
};

#[cfg(test)]
mod direction_tests {
    /// 方向断言：codec 源码（剥注释后）不得引用 engine 路径。
    /// 与 guard.sh 第 6.5 道同一口径；单测版让 cargo test 即触发。
    #[test]
    fn codec_never_references_engine() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/audio/codec");
        let Ok(entries) = std::fs::read_dir(dir) else {
            panic!("无法读取 codec 目录：{dir}");
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                panic!("无法读取源文件：{}", path.display());
            };
            let stripped = strip_comments(&source);
            for (lineno, line) in stripped.lines().enumerate() {
                let l = line.trim();
                // 运行时拼接搜索模式，避免源码字面量自匹配（假阳性）。
                let pat1 = ["crate::audio::", "engine"].concat();
                let pat2 = ["super::super::", "engine"].concat();
                let pat3 = ["use super::", "engine"].concat();
                if l.contains(&pat1) || l.contains(&pat2) || l.contains(&pat3) {
                    panic!(
                        "codec 反向依赖 engine：{}:{} → {}",
                        path.display(),
                        lineno + 1,
                        l
                    );
                }
            }
        }
    }

    /// 剥除行注释与块注释（防注释里的说明文字造成假阳性）。
    fn strip_comments(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut in_block = false;
        for line in src.lines() {
            let mut cleaned = String::new();
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                if in_block {
                    if c == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        in_block = false;
                    }
                    continue;
                }
                if c == '/' && chars.peek() == Some(&'/') {
                    break;
                }
                if c == '/' && chars.peek() == Some(&'*') {
                    chars.next();
                    in_block = true;
                    continue;
                }
                cleaned.push(c);
            }
            out.push_str(&cleaned);
            out.push('\n');
        }
        out
    }
}
