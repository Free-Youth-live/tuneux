//! # 第一方皮肤插件加载（fx / max 共用）
//!
//! 公共部分收口：扫描「皮肤-*」.wasm → 读三件套 → 官方验签 →
//! init → 取皮肤文本。产品侧只做各自的调色板解析。
//!
//! 验签严格 Trusted（LoadPolicy::Silent + load_enforced），
//! 能力固定窄集 [`Capability::Theme`]（纵深防御——清单被篡改多声明能力也拿不到）。

use std::path::Path;

use crate::caps::Capability;
use crate::runtime::WasmHost;
use crate::verify::{TrustList, OFFICIAL_PUBKEY};

use tuneux_corex::{CompressorParams, EqParams};

/// 皮肤插件的能力窄集：只授 Theme（清单多声明也不给）。
const SKIN_ALLOWED: [Capability; 1] = [Capability::Theme];

/// 皮肤插件的签名 id（族共用——一个 id 可带多个皮肤实例）。
const SKIN_ID: &str = "tuneux-skin";

/// 加载一份第一方皮肤插件：读三件套 → 验签 → init → 取文本。
///
/// 返回 (皮肤名, 皮肤文本)；皮肤名取文本 name= 行，无则用文件名去前缀。
/// 任一步失败返回 None（调用方静默跳过该皮肤）。
pub fn load_skin(dir: &Path, stem: &str) -> Option<(String, String)> {
    let wasm = std::fs::read(dir.join(format!("{stem}.wasm"))).ok()?;
    let manifest = std::fs::read_to_string(dir.join(format!("{stem}.manifest"))).ok()?;
    let sig: [u8; 64] = std::fs::read(dir.join(format!("{stem}.sig")))
        .ok()?
        .try_into()
        .ok()?;

    let requested = crate::caps::parse_manifest(&manifest).ok()?;
    let host = WasmHost::new(
        100_000,
        4,
        std::array::from_fn(|_| std::sync::Arc::new(EqParams::default())),
        std::array::from_fn(|_| std::sync::Arc::new(CompressorParams::default())),
    );
    let trust = TrustList::from_parts(vec![OFFICIAL_PUBKEY]);
    let mut plugin = host
        .load_enforced(
            &wasm,
            SKIN_ID,
            manifest.as_bytes(),
            Some((&OFFICIAL_PUBKEY, &sig)),
            &trust,
            &requested,
            &SKIN_ALLOWED,
            crate::verify::LoadPolicy::Silent,
        )
        .ok()?;
    plugin.call_init(0).ok()?;
    let text = std::str::from_utf8(plugin.theme()?).ok()?.to_string();

    // 皮肤名：文本内 name= 行 > 文件名去「皮肤-」前缀
    let name = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("name="))
        .map(|x| x.trim().to_string())
        .unwrap_or_else(|| stem.trim_start_matches("皮肤-").to_string());
    Some((name, text))
}

/// 扫描目录中全部第一方皮肤（「皮肤-」前缀 .wasm），排序后逐份加载。
///
/// 返回 (名称, 文本) 列表；加载失败的条目静默跳过。
pub fn scan_skins(dir: &Path) -> Vec<(String, String)> {
    let mut stems: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let stem = p.file_stem()?.to_str()?.to_owned();
                    (p.extension()?.to_str()? == "wasm" && stem.starts_with("皮肤-"))
                        .then_some(stem)
                })
                .collect()
        })
        .unwrap_or_default();
    stems.sort();
    stems
        .iter()
        .filter_map(|stem| load_skin(dir, stem))
        .collect()
}
