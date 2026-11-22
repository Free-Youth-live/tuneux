//! 电平数据探针（手动运行）：真实播放 0.9 幅度正弦波，检查 level_lr 是否达到
//! 变色阈值。用于核对「电平表不变色」这类问题到底出在数据侧还是渲染侧。
//!
//! 运行：cargo test -p tuneux-corex --test level_probe -- --ignored --nocapture

use std::time::{Duration, Instant};

/// 生成 2 秒 1kHz/44.1k/16bit/立体声正弦 WAV（幅度 0.9）。
fn write_probe_wav(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    let sr: u32 = 44_100;
    let secs = 2u32;
    let frames = sr * secs;
    let data_bytes = frames * 4; // 2 声道 × 2 字节
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // fmt 块大小
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&2u16.to_le_bytes())?; // 声道数
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&(sr * 4).to_le_bytes())?; // 字节率
    f.write_all(&4u16.to_le_bytes())?; // 块对齐
    f.write_all(&16u16.to_le_bytes())?; // 位深
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for i in 0..frames {
        let v = (0.9 * 32767.0 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin())
            as i16;
        f.write_all(&v.to_le_bytes())?;
        f.write_all(&v.to_le_bytes())?;
    }
    Ok(())
}

#[test]
#[ignore = "需要真实音频设备：手动运行电平数据探针（会短暂发声）"]
fn real_playback_level_reaches_threshold() {
    let wav = std::env::temp_dir().join("tuneux-level-probe.wav");
    write_probe_wav(&wav).expect("写测试音频");
    // 用 50% 音量：电平表数值应仍按源电平统计（与音量无关），
    // 否则音量调小后峰值等比例缩水、黄/红档永远到不了。
    let engine = tuneux_corex::Engine::new(0.5).expect("打开音频引擎");
    engine.send(tuneux_corex::AudioCmd::Play(wav));
    let start = Instant::now();
    let (mut max_l, mut max_r) = (0.0f32, 0.0f32);
    while start.elapsed() < Duration::from_secs(2) {
        let (l, r) = engine.level_lr();
        max_l = max_l.max(l);
        max_r = max_r.max(r);
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("max level L={max_l:.3} R={max_r:.3}");
    assert!(
        max_l > 0.6 || max_r > 0.6,
        "电平数据应超过 60%（黄档）：L={max_l:.3} R={max_r:.3}"
    );
}
