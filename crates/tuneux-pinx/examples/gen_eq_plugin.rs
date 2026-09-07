//! 生成第一方插件 .wasm（均衡器 + 压缩器，开发工具，非运行时代码）。
//! 用法：cargo run --example gen_eq_plugin -p tuneux-pinx

fn main() {
    gen("equalizer");
    gen("compressor");
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
}
