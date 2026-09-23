//! 生成第一方插件 .wasm（均衡器 / 压缩器 / 皮肤，开发工具，非运行时代码）。
//! 用法：cargo run --example gen_eq_plugin -p tuneux-pinx -- <equalizer|compressor|skin>
//!
//! 只写指定目标：不带参数时报用法退出、**零写入**。不提供「一次重生成
//! 全部」——.wasm 字节一旦变化，对应 .sig 必须持官方私钥重签，整目录
//! 重生成会把重签义务强加给所有插件，容易造成「改一个、忘签其余」的失配。

fn main() {
    let Some(name) = std::env::args().nth(1) else {
        eprintln!(
            "用法：cargo run --example gen_eq_plugin -p tuneux-pinx -- <equalizer|compressor|皮肤-名称>"
        );
        eprintln!("（只生成指定插件的 .wasm；不提供全量重生成，避免波及其他插件的签名）");
        std::process::exit(2);
    };
    match name.as_str() {
        "equalizer" | "compressor" => gen(&name),
        // 皮肤插件：assets/<名>.wat → plugins/<名>.wasm；名须以「皮肤-」
        // 开头（皮肤命名规则），源文件必须已存在。
        s if s.starts_with("皮肤-") => gen_skin(&name),
        // 可视化面板插件：同皮肤规则（前缀「可视化-」，源文件须已存在）。
        s if s.starts_with("可视化-") => gen_skin(&name),
        other => {
            eprintln!(
                "未知插件：{other}（可用：equalizer | compressor | 皮肤-<名称> | 可视化-<名称>）"
            );
            std::process::exit(2);
        }
    }
}

/// 生成皮肤插件：assets/皮肤-<名>.wat → plugins/皮肤-<名>.wasm。
fn gen_skin(name: &str) {
    let wat_path = format!(
        "{}/../tuneux-fx/assets/{name}.wat",
        env!("CARGO_MANIFEST_DIR")
    );
    if !std::path::Path::new(&wat_path).exists() {
        eprintln!("皮肤源文件不存在：{wat_path}（先写 .wat 再生成）");
        std::process::exit(2);
    }
    let out_path = format!("{}/../../plugins/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    let wat = std::fs::read_to_string(&wat_path).expect("读取 WAT 源失败");
    let wasm = wat::parse_str(&wat).expect("WAT 编译失败");
    std::fs::write(&out_path, &wasm).expect("写 wasm 失败");
    println!("生成 {name}.wasm {} 字节", wasm.len());
    println!("提示：.wasm 字节有变时须持官方私钥重签对应 .sig");
}

fn gen(name: &str) {
    let wat_path = format!(
        "{}/../tuneux-fx/assets/{name}.wat",
        env!("CARGO_MANIFEST_DIR")
    );
    let out_path = format!("{}/../../plugins/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    let wat = std::fs::read_to_string(&wat_path).expect("读取 WAT 源失败");
    let wasm = wat::parse_str(&wat).expect("WAT 编译失败");
    std::fs::write(&out_path, &wasm).expect("写 wasm 失败");
    println!("生成 {name}.wasm {} 字节", wasm.len());
    println!("提示：.wasm 字节有变时须持官方私钥重签对应 .sig");
}
