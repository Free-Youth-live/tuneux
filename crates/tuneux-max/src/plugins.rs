//! # 插件宿主（tuneux-pinx）
//!
//! 复用与 fx 同协议的 WASM 插件：均衡器 / 压缩器（audio_dsp 能力，
//! 皮肤（theme 能力，只供色）/ 可视化（meter_read 能力，逐帧取频谱产文本）。
//! 装载口径与 fx 一致：manifest 声明、官方公钥验签 Trusted、能力求交授予。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tuneux_corex::{CompressorParams, Engine, EqParams, COMP_SLOTS, EQ_SLOTS};
use tuneux_pinx::{
    manifest_plugin_id, parse_manifest, Capability, LoadPolicy, LoadedPlugin, TrustList, WasmHost,
    COMP_ID, EQ_ID, OFFICIAL_PUBKEY, VIS_ID,
};

use crate::theme::Skin;

/// 插件目录：exe 同目录 plugins/ 优先；只读安装时回退用户配置目录；
/// 调试构建再回退仓库根 plugins/（免去手工拷贝）。
pub fn plugins_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("plugins");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("tuneux-max");
    path.push("plugins");
    if path.is_dir() {
        return path;
    }
    #[cfg(debug_assertions)]
    {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        if repo.is_dir() {
            return repo;
        }
    }
    path
}

/// 已加载的均衡器 / 压缩器插件（槽位号 + 实例 + 授予能力）。
pub struct LoadedEffect {
    /// 插件文件主名（equalizer / compressor）。
    pub stem: String,
    /// 引擎槽位号。
    pub slot: u32,
    /// 实际授予的能力集（插件管理页展示）。
    pub granted: Vec<Capability>,
}

/// 已加载的皮肤插件（只保留可渲染的配色，不保留 WASM 实例）。
pub struct LoadedSkin {
    /// 插件文件主名。
    pub stem: String,
    /// 皮肤显示名。
    pub name: String,
    /// 解析后的配色。
    pub skin: Skin,
}

/// 已加载的可视化插件（逐帧 tick）。
pub struct LoadedVisual {
    /// 插件文件主名（显示名）。
    pub stem: String,
    /// 已装载实例。
    pub plugin: LoadedPlugin,
}

/// 一次装载得到的全部插件。
#[derive(Default)]
pub struct PluginSet {
    /// 均衡器插件。
    pub eq: Option<LoadedEffect>,
    /// 压缩器插件。
    pub comp: Option<LoadedEffect>,
    /// 皮肤清单（按共享主题顺序排序，与 fx 同名同序）。
    pub skins: Vec<LoadedSkin>,
    /// 可视化清单（按文件名排序）。
    pub visuals: Vec<LoadedVisual>,
}

/// 读取插件三件套：.wasm / .manifest / .sig（64 字节）。
fn read_plugin(dir: &Path, stem: &str) -> Option<(Vec<u8>, String, [u8; 64])> {
    let wasm = std::fs::read(dir.join(format!("{stem}.wasm"))).ok()?;
    let manifest = std::fs::read_to_string(dir.join(format!("{stem}.manifest"))).ok()?;
    let sig: [u8; 64] = std::fs::read(dir.join(format!("{stem}.sig")))
        .ok()?
        .try_into()
        .ok()?;
    Some((wasm, manifest, sig))
}

/// 宿主槽位池：有引擎时复用引擎槽位，无引擎时用独立空槽位。
fn eq_slots(engine: Option<&Engine>) -> [Arc<EqParams>; EQ_SLOTS] {
    match engine {
        Some(engine) => engine.eq_slots(),
        None => std::array::from_fn(|_| Arc::new(EqParams::new())),
    }
}

fn comp_slots(engine: Option<&Engine>) -> [Arc<CompressorParams>; COMP_SLOTS] {
    match engine {
        Some(engine) => engine.compressor_slots(),
        None => std::array::from_fn(|_| Arc::new(CompressorParams::new())),
    }
}

/// 装载一个第一方插件核心：清单校验 -> 官方验签 -> 实例化。
fn load_core(
    engine: Option<&Engine>,
    stem: &str,
    id: &str,
    allowed: Option<&[Capability]>,
) -> Option<(LoadedPlugin, Vec<Capability>)> {
    let dir = plugins_dir();
    let (wasm, manifest, sig) = read_plugin(&dir, stem)?;
    if manifest_plugin_id(&manifest)
        .ok()?
        .is_some_and(|manifest_id| manifest_id != id)
    {
        return None;
    }
    let requested = parse_manifest(&manifest).ok()?;
    let allowed = allowed.unwrap_or(&requested);
    let granted: Vec<Capability> = requested
        .iter()
        .filter(|capability| allowed.contains(capability))
        .copied()
        .collect();
    let host = WasmHost::new(100_000, 4, eq_slots(engine), comp_slots(engine));
    let trust = TrustList::from_parts(vec![OFFICIAL_PUBKEY]);
    let plugin = host
        .load_enforced(
            &wasm,
            id,
            manifest.as_bytes(),
            Some((&OFFICIAL_PUBKEY, &sig)),
            &trust,
            &requested,
            allowed,
            LoadPolicy::Silent,
        )
        .ok()?;
    Some((plugin, granted))
}

/// 装载均衡器插件并分配引擎槽位。
pub fn load_eq(engine: &Engine) -> Option<LoadedEffect> {
    let (mut plugin, granted) = load_core(Some(engine), "equalizer", EQ_ID, None)?;
    let slot = engine.alloc_eq_slot().map(|(slot, _)| slot)?;
    if plugin.call_init(slot).is_err() {
        engine.free_eq_slot(slot);
        return None;
    }
    Some(LoadedEffect {
        stem: "equalizer".to_string(),
        slot,
        granted,
    })
}

/// 装载压缩器插件并分配引擎槽位。
pub fn load_comp(engine: &Engine) -> Option<LoadedEffect> {
    let (mut plugin, granted) = load_core(Some(engine), "compressor", COMP_ID, None)?;
    let slot = engine.alloc_compressor_slot().map(|(slot, _)| slot)?;
    if plugin.call_init(slot).is_err() {
        engine.free_compressor_slot(slot);
        return None;
    }
    Some(LoadedEffect {
        stem: "compressor".to_string(),
        slot,
        granted,
    })
}

/// 扫描并装载全部「皮肤-*」插件（能力面固定收紧为 theme，与 fx 同口径）。
///
/// 逐份验签（仅 Trusted 入清单）、init 后经 theme_register 取皮肤文本解析；
/// 任一文件缺失 / 验签失败 / 解析失败 → 跳过该份，不阻断启动。皮肤名取
/// 文本 `name=` 键（解析时已处理），文件名去前缀兜底。清单按双端共享的
/// 主题顺序排序（同名同序）；引擎不可用时仍尝试装载（皮肤只供色）。
pub fn load_skins(engine: Option<&Engine>) -> Vec<LoadedSkin> {
    let allowed = [Capability::Theme];
    let dir = plugins_dir();
    // 收集「皮肤-*.wasm」文件名（排序保证装载顺序稳定）。
    let mut stems: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let stem = path.file_stem()?.to_str()?.to_string();
                    (path.extension()?.to_str()? == "wasm" && stem.starts_with("皮肤-"))
                        .then_some(stem)
                })
                .collect()
        })
        .unwrap_or_default();
    stems.sort();
    let mut skins = Vec::new();
    for stem in stems {
        let id = tuneux_pinx::ids::skin_id_for_stem(&stem);
        let Some((mut plugin, _granted)) = load_core(engine, &stem, &id, Some(&allowed)) else {
            continue;
        };
        if plugin.call_init(0).is_err() {
            continue;
        }
        let Some(text) = plugin
            .theme()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        else {
            continue;
        };
        let fallback = stem.trim_start_matches("皮肤-").to_string();
        let Some(skin) = Skin::parse(text, &fallback) else {
            continue;
        };
        skins.push(LoadedSkin {
            stem,
            name: skin.name.clone(),
            skin,
        });
    }
    sort_skins(&mut skins);
    skins
}

/// 皮肤清单顺序：与双 TUI 的共享主题顺序**同名同序**（内置藏青非皮肤，
/// 不在清单）。依赖红线规定 max 永不引入 TUI 渲染栈（ratatui），故不直接
/// 引用共享渲染层的顺序常量，此处镜像同一份名单；共享顺序变更时须同步
/// 本表（装载单测以独立名单守护序与名）。
const SKIN_ORDER: &[&str] = &[
    "终端原生",
    "磷光绿",
    "经典青",
    "深蓝",
    "黑白",
    "灰白",
    "暗彩",
];

/// 按共享主题顺序排序（与 fx 同名同序）；未收录的名字排末尾、按名互排。
fn sort_skins(skins: &mut [LoadedSkin]) {
    let rank = |name: &str| {
        SKIN_ORDER
            .iter()
            .position(|skin_name| *skin_name == name)
            .unwrap_or(usize::MAX)
    };
    skins.sort_by(|a, b| {
        rank(&a.name)
            .cmp(&rank(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// 扫描并装载全部可视化插件（能力面固定收紧为 meter_read）。
pub fn load_visuals(engine: Option<&Engine>) -> Vec<LoadedVisual> {
    let allowed = [Capability::MeterRead];
    let dir = plugins_dir();
    let mut stems: Vec<String> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let stem = path.file_stem()?.to_str()?.to_string();
                    (path.extension()?.to_str()? == "wasm" && stem.starts_with("可视化-"))
                        .then_some(stem)
                })
                .collect()
        })
        .unwrap_or_default();
    stems.sort();
    let mut visuals = Vec::new();
    for stem in stems {
        let Some((mut plugin, _granted)) = load_core(engine, &stem, VIS_ID, Some(&allowed)) else {
            continue;
        };
        if plugin.call_init(0).is_err() {
            continue;
        }
        visuals.push(LoadedVisual { stem, plugin });
    }
    visuals
}

/// 装载全部插件（引擎不可用时仍尝试皮肤 / 可视化；效果器需引擎槽位故跳过）。
pub fn load_all(engine: Option<&Engine>) -> PluginSet {
    let (eq, comp) = match engine {
        Some(engine) => (load_eq(engine), load_comp(engine)),
        None => (None, None),
    };
    PluginSet {
        eq,
        comp,
        skins: load_skins(engine),
        visuals: load_visuals(engine),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_stem_prefixes_match_docs() {
        for stem in ["皮肤-磷光绿", "皮肤-黑白"] {
            assert!(stem.starts_with("皮肤-"));
        }
        for stem in ["可视化-能量条", "可视化-频谱瀑布"] {
            assert!(stem.starts_with("可视化-"));
        }
        assert!(!("compressor".starts_with("皮肤-")));
    }

    #[test]
    fn loading_without_engine_does_not_panic() {
        let _ = load_skins(None);
        let _ = load_visuals(None);
    }

    #[test]
    fn all_bundled_skins_load_in_shared_theme_order() {
        let skins = load_skins(None);
        // 独立名单守护（与共享主题顺序同名同序；内置藏青非皮肤不在清单）：
        // 镜像表 SKIN_ORDER 若漂移，此断言即失败。
        let expected = [
            "终端原生",
            "磷光绿",
            "经典青",
            "深蓝",
            "黑白",
            "灰白",
            "暗彩",
        ];
        let names: Vec<&str> = skins.iter().map(|skin| skin.name.as_str()).collect();
        assert_eq!(names, expected, "皮肤清单应与共享主题顺序同名同序");
        assert_eq!(skins[0].name, "终端原生", "默认皮肤居首");
        assert_eq!(skins[0].stem, "皮肤-终端原生");
        for skin in &skins {
            assert!(skin.stem.starts_with("皮肤-"));
        }
    }

    #[test]
    fn sort_skins_puts_unknown_names_last() {
        let mut skins = vec![
            LoadedSkin {
                stem: "皮肤-自定义".to_string(),
                name: "自定义".to_string(),
                skin: Skin::parse("name=自定义", "自定义").expect("应解析成功"),
            },
            LoadedSkin {
                stem: "皮肤-黑白".to_string(),
                name: "黑白".to_string(),
                skin: Skin::parse("name=黑白", "黑白").expect("应解析成功"),
            },
        ];
        sort_skins(&mut skins);
        assert_eq!(skins[0].name, "黑白", "收录名优先于未收录名");
        assert_eq!(skins[1].name, "自定义");
    }
}
