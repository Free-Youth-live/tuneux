//! 编译可视化插件 WAT 为 .wasm。
fn main() {
    // 正确的 manifest 格式：每行一个能力短名（与 equalizer/skin 同格式）
    let manifest = "# tuneux 第一方可视化插件 · 能力清单\n# 每行一个能力短名；# 注释与空行忽略。\nmeter_read\n";

    let wat1 = include_str!("../../tuneux-fx/assets/可视化-能量条.wat");
    let wasm1 = wat::parse_str(wat1).expect("能量条 WAT 编译失败");
    std::fs::write("plugins/可视化-能量条.wasm", &wasm1).expect("写可视化插件 能量条.wasm 失败");
    std::fs::write("plugins/可视化-能量条.manifest", manifest)
        .expect("写可视化插件 能量条.manifest 失败");
    println!("OK 可视化-能量条 ({}B)", wasm1.len());

    let wat2 = include_str!("../../tuneux-fx/assets/可视化-频谱瀑布.wat");
    let wasm2 = wat::parse_str(wat2).expect("瀑布 WAT 编译失败");
    std::fs::write("plugins/可视化-频谱瀑布.wasm", &wasm2)
        .expect("写可视化插件 频谱瀑布.wasm 失败");
    std::fs::write("plugins/可视化-频谱瀑布.manifest", manifest)
        .expect("写可视化插件 频谱瀑布.manifest 失败");
    println!("OK 可视化-频谱瀑布 ({}B)", wasm2.len());
    println!("manifest 已修正为一能力一行格式");
}
