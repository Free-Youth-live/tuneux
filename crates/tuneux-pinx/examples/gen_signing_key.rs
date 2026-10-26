//! 生成/读取官方签名私钥（仓库外本地文件）并给第一方插件签 .sig（开发工具，非运行时）。
//!
//! 安全闸（防误操作导致全部插件降级 Unsigned）：
//! - 无论复用还是新生成种子，都从种子派生公钥并与 verify::OFFICIAL_PUBKEY
//!   逐字节比对，不符即报错退出——换种子而不换内嵌公钥 = 全部第一方插件
//!   验签失败 → fx/max 静默不加载任何插件；
//! - 种子文件缺失/损坏时**不会**静默生成新种子，除非显式设
//!   TUNEUX_ALLOW_NEW_SEED=1（首次初始化用）。
//!
//! 用法：cargo run --example gen_signing_key -p tuneux-pinx
//!
//! 产物三处：
//! - 私钥种子（32 字节）写入 seed_path() 指向的仓库外文件（绝不提交进仓库）；
//! - 官方公钥（32 字节）打印到 stdout，人工内嵌进二进制信任根；
//! - equalizer.sig / compressor.sig / skin.sig（各 64 字节）写入 plugins/，随包分发。
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

    // 公钥自校验：种子派生公钥必须与 verify::OFFICIAL_PUBKEY 逐字节一致。
    // 种子对/公钥错 = 签出的全部 .sig 与产品内嵌信任根不匹配 = 插件全降级。
    if pubkey != tuneux_pinx::OFFICIAL_PUBKEY {
        eprintln!(
            "错误：种子派生公钥与 verify::OFFICIAL_PUBKEY 不匹配！\n\n\
  派生公钥: {}\n  内嵌公钥: {}\n\n\
这通常是种子文件与内嵌公钥来自不同次初始化。请核对种子文件；\n\
若确要换信任根，需同步更新 OFFICIAL_PUBKEY 并重新构建全部产品。\n\
（拒绝继续：此刻签出的每一个 .sig 都会验签失败）",
            hex(&pubkey),
            hex(&tuneux_pinx::OFFICIAL_PUBKEY)
        );
        std::process::exit(1);
    }
    println!("公钥自校验通过（派生 == 内嵌信任根）");

    println!("官方公钥（内嵌进二进制信任根，可公开）:");
    println!("{}", hex(&pubkey));
    // 私钥种子本身不打印（避免终端回滚 / CI 日志记录），只提示落盘路径。
    println!(
        "官方私钥种子已保存到 {}（仓库外，切勿提交）",
        path.display()
    );

    sign_plugin(&signing, "equalizer", "tuneux-eq");
    sign_plugin(&signing, "compressor", "tuneux-comp");
    // 皮肤与可视化插件（plugins/ 下「皮肤-*」「可视化-*」前缀的 .wasm）：逐份签名。
    let plugins_dir = format!("{}/../../plugins", env!("CARGO_MANIFEST_DIR"));
    let mut skin_names: Vec<String> = std::fs::read_dir(&plugins_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let name = p.file_stem()?.to_str()?.to_owned();
                    (p.extension()?.to_str()? == "wasm"
                        && (name.starts_with("皮肤-") || name.starts_with("可视化-")))
                    .then_some(name)
                })
                .collect()
        })
        .unwrap_or_default();
    skin_names.sort();
    for name in skin_names {
        // 插件 id 按前缀分工：皮肤 tuneux-skin、可视化 tuneux-vis（v2
        // 签名消息 = id ‖ manifest ‖ wasm，两侧必须一致，否则验签不匹配）。
        let id = if name.starts_with("可视化-") {
            "tuneux-vis"
        } else {
            "tuneux-skin"
        };
        sign_plugin(&signing, &name, id);
    }
}

/// 复用已有种子；缺失/损坏时仅在显式许可（TUNEUX_ALLOW_NEW_SEED=1）下
/// 生成新种子并落盘，否则报错退出——静默换种子会让内嵌公钥失配，
/// 全部第一方插件降级 Unsigned（fx/max 静默不加载）。
fn load_or_generate_seed(path: &std::path::Path) -> [u8; 32] {
    if let Ok(bytes) = std::fs::read(path) {
        if bytes.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&bytes);
            println!("复用已有私钥种子（{}）", path.display());
            return s;
        }
    }
    if std::env::var("TUNEUX_ALLOW_NEW_SEED").ok().as_deref() != Some("1") {
        eprintln!(
            "错误：私钥种子缺失或损坏（{}）。\n\n\
若这是首次初始化：设 TUNEUX_ALLOW_NEW_SEED=1 后重跑以生成新种子，\n\
并把打印出的公钥内嵌进 verify::OFFICIAL_PUBKEY（随后重新构建全部产品）。\n\n\
若这不是首次：种子丢失意味着旧签名无法复现——需从备份恢复种子，\n\
或接受重签并同步更换内嵌公钥（全部用户需升级才识别新签名）。\n\
（拒绝静默生成：新种子与本工具内嵌公钥不匹配 = 全部插件降级 Unsigned）",
            path.display()
        );
        std::process::exit(1);
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
    let manifest_path = format!(
        "{}/../../plugins/{name}.manifest",
        env!("CARGO_MANIFEST_DIR")
    );
    let wasm = std::fs::read(&wasm_path).expect("读 wasm 失败");
    // v2 签名格式：id ‖ manifest ‖ wasm（能力清单纳入签名面）。
    // manifest 读不到即报错退出：用空字节继续会产出必然验签失败的 .sig
    // 且打印"成功"——静默坏签比失败更糟。
    let manifest = std::fs::read(&manifest_path)
        .unwrap_or_else(|e| panic!("读 {name}.manifest 失败（{e}）：签名面不完整，拒绝继续"));
    let mut message = Vec::with_capacity(id.len() + manifest.len() + wasm.len());
    message.extend_from_slice(id.as_bytes());
    message.extend_from_slice(&manifest);
    message.extend_from_slice(&wasm);
    let sig = signing.sign(&message);
    std::fs::write(&sig_path, sig.to_bytes()).expect("写 .sig 失败");
    println!(
        "签名 {name}.wasm + {name}.manifest -> {name}.sig（{} 字节，v2 格式）",
        sig.to_bytes().len()
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
