//! 音频设备探针：列出各输出设备的默认配置与支持的采样率区间。
//!
//! 用法：cargo run --example device_probe -p tuneux-corex
//!
//! 诊断目的：原生采样率直通依赖 cpal 的 supported_output_configs() 区间判定；
//! macOS 上 cpal 切硬件速率要求 AvailableNominalSampleRates 中存在目标速率的
//! **点区间**（min == max）。本探针把默认配置与采样率区间都打出来，用于判断
//! 某设备（如外接 USB DAC）能否接受 192kHz / 384kHz 原生输出。

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());

    let devices: Vec<_> = match host.output_devices() {
        Ok(it) => it.collect(),
        Err(e) => {
            eprintln!("枚举输出设备失败：{e}");
            return;
        }
    };
    println!(
        "共 {} 个输出设备
",
        devices.len()
    );

    for (i, device) in devices.iter().enumerate() {
        let name = device.name().unwrap_or_else(|_| "<未知>".to_string());
        let mark = if default_name.as_ref() == Some(&name) {
            "（默认）"
        } else {
            ""
        };
        println!("[{i}] {name}{mark}");

        match device.default_output_config() {
            Ok(cfg) => println!(
                "    默认: {} Hz, {} 声道, {:?}",
                cfg.sample_rate().0,
                cfg.channels(),
                cfg.sample_format()
            ),
            Err(_) => println!("    默认: 不可用"),
        }

        match device.supported_output_configs() {
            Ok(configs) => {
                let mut min = u32::MAX;
                let mut max = 0u32;
                for c in configs {
                    min = min.min(c.min_sample_rate().0);
                    max = max.max(c.max_sample_rate().0);
                }
                if min <= max {
                    println!("    采样率区间: {min} Hz ..= {max} Hz");
                } else {
                    println!("    采样率区间: 无");
                }
            }
            Err(_) => println!("    采样率区间: 查询失败"),
        }
    }
}
