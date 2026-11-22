//! # 第一方插件家族签名 id
//!
//! 四个家族 id 是**跨产品签名契约**：.sig 文件按这些 id 逐字节签发，
//! fx / max 双端装载与官方公钥验签必须同源——改一个字符即验签失败
//! （且是静默跳过装载的失败形态）。集中为常量防字面量漂移；
//! 测试断言里保留字面量作契约钉（换成常量会变同义反复、失去守卫）。
//!
//! 装载/扫描逻辑归各产品（fx `load_skin_palettes` / max
//! `load_skin_plugins`——两份生产验证过的实现；统一收口从它们提
//! 公因子，不预建第三份）。

/// 均衡器插件家族的签名 id。
pub const EQ_ID: &str = "tuneux-eq";

/// 压缩器插件家族的签名 id。
pub const COMP_ID: &str = "tuneux-comp";

/// 皮肤插件签名 id 的家族前缀；每份皮肤还需追加自身名称。
pub const SKIN_ID: &str = "tuneux-skin";

/// 由皮肤文件名生成逐皮肤签名 id，例如「皮肤-深蓝」→「tuneux-skin:深蓝」。
pub fn skin_id_for_stem(stem: &str) -> String {
    format!("{SKIN_ID}:{}", stem.trim_start_matches("皮肤-"))
}

/// 可视化面板插件家族的签名 id（一个 id 可带多个可视化实例，
/// 「可视化-*」前缀族）。
pub const VIS_ID: &str = "tuneux-vis";
