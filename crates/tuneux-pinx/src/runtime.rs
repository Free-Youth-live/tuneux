//! # WASM 插件运行时（控制面沙箱）
//!
//! 引入 wasmi 解释器加载 / 实例化插件模块、注入宿主函数、执行插件控制面
//! 逻辑（写效果器参数、日志）。插件永不直连底层能力——fs / net 不注册，
//! 插件只面对本模块注入的少量宿主函数（能力默认拒绝）。
//!
//! 音频线程零插件：插件只经宿主函数写参数（控制面），实际 DSP 由宿主
//! （corex）在音频线程执行。均衡器参数经 EqParams（Arc 共享）薄适配落到
//! corex 音频线程槽位，本模块不触碰音频样本。
//!
//! # host ABI v1（已冻结）
//!
//! 命名空间 "host"，经第一方均衡器 / 压缩器插件端到端跑通后冻结：
//!
//! - `eq_set(slot: i32, band: i32, gain_db: f32) -> i32`
//!   写均衡器某段增益（dB，钳制 ±12）。slot 为本插件被分配的槽位号（v2 动态
//!   多槽位，非固定 0），band 为 [0, 10) 段号；返回 0 成功 / 1 槽位错 / 2 段号错。
//! - `compressor_set(slot: i32, param: i32, value: f32) -> i32`
//!   写压缩器某参数。param：0=阈值 1=压缩比 2=启动 3=释放 4=补偿增益；
//!   slot 同上为分配槽；返回 0 成功 / 1 槽位错 / 2 参数号错。
//! - `log(ptr: i32, len: i32) -> i32`
//!   读插件线性内存一段 UTF-8 追加到宿主日志。len ≤ 64KiB（超限拒绝）；
//!   返回 0 成功 / 3 内存或编码错。
//! - `theme_register(ptr: i32, len: i32) -> i32`
//!   插件把一份皮肤描述（调色板文本）写进线性内存后注册；宿主读原始字节
//!   交发行版解析（插件只供色、不碰渲染）。len ≤ 8KiB（超限拒绝）；
//!   返回 0 成功 / 3 内存错。须授予 `theme` 能力才注册（v1 增补）。
//!
//! 演进走版本化（插件头声明 interface_version / minimum_runtime_version），
//! 不破坏已冻结面。fs / net 永不注册（能力默认拒绝）。

use std::sync::Arc;

use crate::caps::{arbitrate, Capability};
use crate::verify::{classify, Tristate, TrustList};
use tuneux_corex::{CompressorParams, EqParams, COMP_SLOTS, EQ_SLOTS};
use wasmi::{Caller, Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

/// 宿主函数返回值：成功。
const OK: i32 = 0;
/// 宿主函数返回值：未知槽位。
const ERR_BAD_SLOT: i32 = 1;
/// 宿主函数返回值：参数号越界（段号/压缩器参数号）。
const ERR_BAD_BAND: i32 = 2;
/// 宿主函数返回值：读内存失败。
const ERR_MEM: i32 = 3;
/// 单次 log 最大字节数：防插件传巨大 len 让宿主分配超大缓冲（OOM DoS）。
const MAX_LOG_LEN: u32 = 64 * 1024;
/// 单次 theme_register 最大字节数：皮肤文本很小，8 KiB 足够且防 OOM DoS。
const MAX_THEME_LEN: u32 = 8 * 1024;

/// 宿主状态：存进 wasmi Store，宿主函数经 Caller 访问。
#[derive(Debug)]
pub struct HostState {
    /// 均衡器槽位池（薄适配 corex，Arc 共享；eq_set(slot, …) 按槽位索引写）。
    pub eq_slots: [Arc<EqParams>; EQ_SLOTS],
    /// 压缩器槽位池（同均衡器）。
    pub comp_slots: [Arc<CompressorParams>; COMP_SLOTS],
    /// 插件 log 输出（供测试与宿主读取，字符串按追加顺序）。
    pub logs: Vec<String>,
    /// 本插件被分配的槽位号（call_init 时写入）：宿主函数据此做槽位隔离，
    /// 插件只能写自己分配的槽（防跨槽覆盖）；init 之前为 None。
    assigned_slot: Option<u32>,
    /// 资源配额（内存页 / 表 / 栈）。由 StoreLimits 承载，实例化前接进 Store。
    limits: StoreLimits,
    /// 插件注册的皮肤字节（theme_register 写入；原始，由发行版解析）。
    theme: Option<Vec<u8>>,
}

impl HostState {
    fn new(
        eq_slots: [Arc<EqParams>; EQ_SLOTS],
        comp_slots: [Arc<CompressorParams>; COMP_SLOTS],
        max_mem_pages: u32,
    ) -> Self {
        Self {
            eq_slots,
            comp_slots,
            logs: Vec::new(),
            assigned_slot: None,
            limits: StoreLimitsBuilder::new()
                // memory_size 单位是字节（页 × 64 KiB）。
                .memory_size(max_mem_pages as usize * 64 * 1024)
                .build(),
            theme: None,
        }
    }

    /// 插件经 theme_register 注册的皮肤字节（原始，由发行版解析）。
    pub fn theme(&self) -> Option<&[u8]> {
        self.theme.as_deref()
    }
}

/// 插件装载 / 执行错误（把 wasmi 错误收敛为宿主自有错误，不泄露 wasmi 类型）。
#[derive(Debug)]
pub enum HostError {
    /// 模块编译失败（wasm 字节非法）。
    Compile(String),
    /// 实例化 / 链接失败（含插件 import 了未授予的宿主函数 → 默认拒绝）。
    Instantiate(String),
    /// 导出函数缺失或签名不匹配。
    Export(String),
    /// 执行 trap / 燃料耗尽。
    Trap(String),
    /// 能力仲裁失败（网络互斥 / 网络不可用等）。
    Cap(String),
}

/// WASM 宿主：持有 wasmi 引擎（解释器），可反复装载插件。
pub struct WasmHost {
    engine: Engine,
    /// 单次调用燃料上限（防死循环；timeout 在纯解释器下按燃料折算）。
    fuel_per_call: u64,
    /// 内存页上限。
    max_mem_pages: u32,
    /// 均衡器槽位池（来自 corex 引擎，eq_set 按 slot 索引写；插件可申请任意槽）。
    eq_slots: [Arc<EqParams>; EQ_SLOTS],
    /// 压缩器槽位池。
    comp_slots: [Arc<CompressorParams>; COMP_SLOTS],
}

impl WasmHost {
    /// 新建宿主。fuel_per_call 是每次插件调用的燃料预算，
    /// max_mem_pages 是插件线性内存页上限（64 KiB / 页），
    /// eq_slots / comp_slots 是效果器槽位池（来自 corex 引擎）。
    pub fn new(
        fuel_per_call: u64,
        max_mem_pages: u32,
        eq_slots: [Arc<EqParams>; EQ_SLOTS],
        comp_slots: [Arc<CompressorParams>; COMP_SLOTS],
    ) -> Self {
        // consume_fuel(true) 必须在建 Engine 时开（Config 只能设置一次）。
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        Self {
            engine,
            fuel_per_call,
            max_mem_pages,
            eq_slots,
            comp_slots,
        }
    }

    /// 编译并装载插件：能力求交 → 按授予面注入宿主函数 → 实例化。
    ///
    /// `requested` 为插件声明申请的能力（清单），`allowed` 为宿主侧允许集；
    /// 实际授予 = 求交（见 [`arbitrate`]）。未授予的能力对应宿主函数不注册，
    /// 插件 import 即实例化失败（能力默认拒绝）。
    pub fn load(
        &self,
        wasm_bytes: &[u8],
        plugin_id: &str,
        signature: Option<(&[u8; 32], &[u8; 64])>,
        trust: &TrustList,
        requested: &[Capability],
        allowed: &[Capability],
    ) -> Result<LoadedPlugin, HostError> {
        // 验签 → 信任三态（身份标签；不拒绝加载，三态随插件返回供发行版呈现）。
        let tristate = classify(plugin_id, wasm_bytes, signature, trust);
        let granted =
            arbitrate(requested, allowed).map_err(|e| HostError::Cap(format!("{e:?}")))?;
        let module =
            Module::new(&self.engine, wasm_bytes).map_err(|e| HostError::Compile(e.to_string()))?;

        let mut store = Store::new(
            &self.engine,
            HostState::new(
                self.eq_slots.clone(),
                self.comp_slots.clone(),
                self.max_mem_pages,
            ),
        );
        store.limiter(|s| &mut s.limits);

        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        Self::define_host_functions(&mut linker, &granted)
            .map_err(|e| HostError::Instantiate(e.to_string()))?;

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(|e| HostError::Instantiate(e.to_string()))?;

        Ok(LoadedPlugin {
            store,
            instance,
            fuel_per_call: self.fuel_per_call,
            tristate,
        })
    }

    /// 注册基础函数：log 调试日志。始终注册、不参与授予面（非敏感能力）。
    fn define_log(linker: &mut Linker<HostState>) -> Result<(), wasmi::Error> {
        // log(ptr, len) -> i32：读插件线性内存一段 UTF-8，追加到宿主日志。
        linker.func_wrap(
            "host",
            "log",
            |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                if len > MAX_LOG_LEN {
                    return ERR_MEM;
                }
                let mut buf = vec![0u8; len as usize];
                let read = caller
                    .get_export("memory")
                    .and_then(|e| e.into_memory())
                    .map(|mem| mem.read(&caller, ptr as usize, &mut buf));
                match read {
                    Some(Ok(())) => match String::from_utf8(buf) {
                        Ok(text) => {
                            caller.data_mut().logs.push(text);
                            OK
                        }
                        Err(_) => ERR_MEM,
                    },
                    _ => ERR_MEM,
                }
            },
        )?;
        Ok(())
    }

    /// 注册宿主函数：只注册「授予面」内的能力函数 + 基础 log。
    ///
    /// - [`Capability::AudioDsp`] 授予 → 注册 eq_set / compressor_set
    ///   （均衡器 / 压缩器参数写入）；
    /// - [`Capability::Theme`] 授予 → 注册 theme_register（皮肤调色板）；
    /// - log 始终注册（基础能力）；fs / net 永不注册；
    /// - 插件 import 未注册函数即实例化失败（默认拒绝）。
    fn define_host_functions(
        linker: &mut Linker<HostState>,
        granted: &[Capability],
    ) -> Result<(), wasmi::Error> {
        if granted.contains(&Capability::AudioDsp) {
            // eq_set(slot, band, gain_db) -> i32：写均衡器某段增益（薄适配 corex 槽位）。
            linker.func_wrap(
                "host",
                "eq_set",
                |caller: Caller<'_, HostState>, slot: u32, band: u32, gain: f32| -> i32 {
                    // 槽位隔离：只允许写本插件被分配的槽。
                    if caller.data().assigned_slot != Some(slot) {
                        return ERR_BAD_SLOT;
                    }
                    let Some(params) = caller.data().eq_slots.get(slot as usize) else {
                        return ERR_BAD_SLOT;
                    };
                    if params.set_band(band as usize, gain) {
                        OK
                    } else {
                        ERR_BAD_BAND
                    }
                },
            )?;

            // compressor_set(slot, param, value) -> i32：写压缩器某参数。
            // param：0=阈值 1=压缩比 2=启动 3=释放 4=补偿增益。
            linker.func_wrap(
                "host",
                "compressor_set",
                |caller: Caller<'_, HostState>, slot: u32, param: u32, value: f32| -> i32 {
                    // 槽位隔离：只允许写本插件被分配的槽。
                    if caller.data().assigned_slot != Some(slot) {
                        return ERR_BAD_SLOT;
                    }
                    let Some(c) = caller.data().comp_slots.get(slot as usize) else {
                        return ERR_BAD_SLOT;
                    };
                    match param {
                        0 => c.set_threshold(value),
                        1 => c.set_ratio(value),
                        2 => c.set_attack_ms(value),
                        3 => c.set_release_ms(value),
                        4 => c.set_makeup(value),
                        _ => return ERR_BAD_BAND,
                    }
                    OK
                },
            )?;
        }
        if granted.contains(&Capability::Theme) {
            // theme_register(ptr, len) -> i32：插件把皮肤描述写进线性内存后注册，
            // 宿主读原始字节交发行版解析（插件只供色、不碰渲染）。
            linker.func_wrap(
                "host",
                "theme_register",
                |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                    if len > MAX_THEME_LEN {
                        return ERR_MEM;
                    }
                    let mut buf = vec![0u8; len as usize];
                    let read = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .map(|mem| mem.read(&caller, ptr as usize, &mut buf));
                    match read {
                        Some(Ok(())) => {
                            caller.data_mut().theme = Some(buf);
                            OK
                        }
                        _ => ERR_MEM,
                    }
                },
            )?;
        }
        Self::define_log(linker)?;
        Ok(())
    }
}

/// 已装载插件：持有其 Store（含宿主状态）与实例句柄。
pub struct LoadedPlugin {
    store: Store<HostState>,
    instance: wasmi::Instance,
    /// 每次插件调用的燃料预算（装载时从宿主拷贝）。
    fuel_per_call: u64,
    /// 验签得到的信任三态（身份标签，供发行版呈现徽章 / 标注 / 询问）。
    pub tristate: Tristate,
}

impl LoadedPlugin {
    /// 调用插件导出的 init 函数（签名 (i32) -> i32）：传入插件被分配的槽位号，
    /// 返回插件返回值。每次调用前重置燃料预算。
    pub fn call_init(&mut self, slot: u32) -> Result<i32, HostError> {
        // 先绑定槽位再执行 init：init 期间的宿主函数调用据此校验，
        // 插件只能写自己分配的槽（防跨槽覆盖）。
        self.store.data_mut().assigned_slot = Some(slot);
        let func = self
            .instance
            .get_typed_func::<(i32,), i32>(&self.store, "init")
            .map_err(|e| HostError::Export(e.to_string()))?;
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(|e| HostError::Trap(e.to_string()))?;
        func.call(&mut self.store, (slot as i32,))
            .map_err(|e| HostError::Trap(e.to_string()))
    }

    /// 只读访问宿主状态（均衡器槽位 / 日志）。
    pub fn state(&self) -> &HostState {
        self.store.data()
    }

    /// 插件经 theme_register 注册的皮肤字节（由发行版解析为调色板）。
    pub fn theme(&self) -> Option<&[u8]> {
        self.store.data().theme()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use tuneux_corex::EQ_BANDS;

    const EQ_PLUGIN_WAT: &str = r#"
(module
  (import "host" "eq_set" (func $eq_set (param i32 i32 f32) (result i32)))
  (import "host" "log" (func $log (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "eq loaded")
  (func (export "init") (param $slot i32) (result i32)
    (call $eq_set (local.get $slot) (i32.const 0) (f32.const 3.0)) drop
    (call $eq_set (local.get $slot) (i32.const 1) (f32.const 2.0)) drop
    (call $eq_set (local.get $slot) (i32.const 2) (f32.const 1.0)) drop
    (call $eq_set (local.get $slot) (i32.const 3) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 4) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 5) (f32.const 0.0)) drop
    (call $eq_set (local.get $slot) (i32.const 6) (f32.const 1.0)) drop
    (call $eq_set (local.get $slot) (i32.const 7) (f32.const 2.0)) drop
    (call $eq_set (local.get $slot) (i32.const 8) (f32.const 3.0)) drop
    (call $eq_set (local.get $slot) (i32.const 9) (f32.const 4.0)) drop
    (call $log (i32.const 0) (i32.const 9)) drop
    (i32.const 0))
)
"#;

    #[test]
    fn loads_and_runs_eq_plugin() {
        let wasm = wat::parse_str(EQ_PLUGIN_WAT).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots.clone(), comp_slots);
        // 官方插件走真验签：私钥签名 → 官方公钥内嵌信任清单 → Trusted。
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-eq".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        let mut plugin = host
            .load(
                &wasm,
                "tuneux-eq",
                Some((&pubkey, &sig)),
                &trust,
                &[Capability::AudioDsp],
                &[Capability::AudioDsp],
            )
            .expect("插件应加载成功");
        assert_eq!(
            plugin.tristate,
            Tristate::Trusted,
            "官方签名命中内嵌公钥应为 Trusted"
        );

        let ret = plugin.call_init(0).expect("init 应执行成功");
        assert_eq!(ret, 0, "插件返回值应为 0");

        let expect: [f32; EQ_BANDS] = [3.0, 2.0, 1.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0];
        for (i, e) in expect.iter().enumerate() {
            assert_eq!(eq_slots[0].band(i), *e, "第 {i} 段增益应写入槽位 0");
        }
        assert_eq!(
            plugin.state().logs,
            vec!["eq loaded".to_string()],
            "log 应追加到宿主日志"
        );
    }

    #[test]
    fn theme_register_captures_skin_bytes() {
        let wat = r#"
(module
  (import "host" "theme_register" (func $theme_register (param i32 i32) (result i32)))
  (import "host" "log" (func $log (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "bg=#0d1321")
  (func (export "init") (param $slot i32) (result i32)
    (call $theme_register (i32.const 0) (i32.const 10)) drop
    (call $log (i32.const 0) (i32.const 10)) drop
    (i32.const 0))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let mut plugin = host
            .load(
                &wasm,
                "skin",
                None,
                &TrustList::default(),
                &[Capability::Theme],
                &[Capability::Theme],
            )
            .expect("皮肤插件应加载成功");
        let ret = plugin.call_init(0).expect("init 应执行成功");
        assert_eq!(ret, 0, "插件返回值应为 0");
        assert_eq!(
            plugin.theme(),
            Some(&b"bg=#0d1321"[..]),
            "theme_register 应捕获皮肤字节"
        );
    }

    #[test]
    fn rejects_plugin_importing_ungranted_fn() {
        let wat = r#"
(module
  (import "host" "fs_read" (func $fs (param i32 i32 i32) (result i32)))
  (func (export "init") (result i32) (i32.const 0)))
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        assert!(
            matches!(
                host.load(
                    &wasm,
                    "x",
                    None,
                    &TrustList::default(),
                    &[Capability::AudioDsp],
                    &[Capability::AudioDsp],
                ),
                Err(HostError::Instantiate(_))
            ),
            "未授予面应实例化失败"
        );
    }

    #[test]
    fn rejects_ungranted_cap_function() {
        // 插件 import 了 eq_set，但未申请 AudioDsp → 求交为空 → eq_set 不注册，
        // 实例化失败（能力默认拒绝：授予面决定函数注册，而非插件自说自话）。
        let wat = r#"
(module
  (import "host" "eq_set" (func $eq (param i32 i32 f32) (result i32)))
  (func (export "init") (result i32) (i32.const 0)))
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        assert!(
            matches!(
                host.load(
                    &wasm,
                    "x",
                    None,
                    &TrustList::default(),
                    &[],
                    &[Capability::AudioDsp],
                ),
                Err(HostError::Instantiate(_))
            ),
            "未授予能力对应的函数应实例化失败"
        );
    }

    #[test]
    fn eq_set_rejects_unassigned_slot() {
        // 插件 init 试图写非分配槽位（分配 0、写 1）→ 宿主拒绝，实现槽位隔离。
        let wat = r#"
(module
  (import "host" "eq_set" (func $eq_set (param i32 i32 f32) (result i32)))
  (memory (export "memory") 1)
  (func (export "init") (param $slot i32) (result i32)
    (call $eq_set (i32.const 1) (i32.const 0) (f32.const 6.0)) drop
    (i32.const 0))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots.clone(), comp_slots);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-eq".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        let mut plugin = host
            .load(
                &wasm,
                "tuneux-eq",
                Some((&pubkey, &sig)),
                &trust,
                &[Capability::AudioDsp],
                &[Capability::AudioDsp],
            )
            .expect("插件应加载成功");
        let _ = plugin.call_init(0).expect("init 应执行成功");
        // 分配槽 0；插件试图写槽 1 → 被拒绝，槽 1 保持默认 0.0。
        assert_eq!(eq_slots[1].band(0), 0.0, "未分配槽不应被写入");
        assert_eq!(eq_slots[0].band(0), 0.0, "分配槽也未被插件写入");
    }
}
