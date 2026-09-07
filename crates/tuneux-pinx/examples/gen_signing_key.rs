//! 生成/读取官方签名私钥（仓库外本地文件）并给第一方插件签 .sig（开发工具，非运行时）。
//! 用法：cargo run --example gen_signing_key -p tuneux-pinx
//!
//! 产物三处：
//! - 私钥种子（32 字节）写入 seed_path() 指向的仓库外文件（绝不提交进仓库）；
//! - 官方公钥（32 字节）打印到 stdout，人工内嵌进二进制信任根；
//! - equalizer.sig / compressor.sig（各 64 字节）写入 plugins/，随包分发。
//!
//! 种子路径解析（不在代码里写死任何本机绝对路径）：
//! 1. 环境变量 TUNEUX_OFFICIAL_SIGNING_SEED 优先；
//! 2. 缺省回退到家目录隐藏文件 ~/.tuneux-official-signing-seed。

use std::path::PathBuf;

use ed25519_dalek::{Signer, SigningKey};

/// 官方私钥种子落盘路径：优先环境变量，缺省回退家目录隐藏文件。
fn seed_path() -> PathBuf {
    if let Ok(p) = std::env::var("TUNEUX_OFFICIAL_SIGNING_SEED") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".tuneux-official-signing-seed")
}

fn main() {
    let path = seed_path();
    let seed = load_or_generate_seed(&path);
    let signing = SigningKey::from_bytes(&seed);
    let pubkey = signing.verifying_key().to_bytes();

    println!("官方公钥（内嵌进二进制信任根，可公开）:");
    println!("{}", hex(&pubkey));
    // 私钥种子本身不打印（避免终端回滚 / CI 日志记录），只提示落盘路径。
    println!(
        "官方私钥种子已保存到 {}（仓库外，切勿提交）",
        path.display()
    );

    sign_plugin(&signing, "equalizer", "tuneux-eq");
    sign_plugin(&signing, "compressor", "tuneux-comp");
}

/// 复用已有种子；不存在则用系统安全随机源生成并落盘（幂等）。
fn load_or_generate_seed(path: &std::path::Path) -> [u8; 32] {
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&bytes);
            println!("复用已有私钥种子（{}）", path.display());
            return s;
        }
    }
    let mut s = [0u8; 32];
    getrandom::getrandom(&mut s).expect("系统安全随机源不可用");
    std::fs::write(path, s).expect("写私钥种子失败");
    println!("已生成新私钥种子并保存（{}）", path.display());
    s
}

/// 给单个插件签名：sig = sign(种子, 插件 id ‖ wasm 字节)（防掉包，见 verify::classify）。
fn sign_plugin(signing: &SigningKey, name: &str, id: &str) {
    let wasm_path = format!("{}/../../plugins/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    let sig_path = format!("{}/../../plugins/{name}.sig", env!("CARGO_MANIFEST_DIR"));
    let wasm = std::fs::read(&wasm_path).expect("读 wasm 失败");
    let mut message = Vec::with_capacity(id.len() + wasm.len());
    message.extend_from_slice(id.as_bytes());
    message.extend_from_slice(&wasm);
    let sig = signing.sign(&message);
    std::fs::write(&sig_path, sig.to_bytes()).expect("写 .sig 失败");
    println!(
        "签名 {name}.wasm -> {name}.sig（{} 字节）",
        sig.to_bytes().len()
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
