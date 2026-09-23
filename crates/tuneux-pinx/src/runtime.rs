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
//!   多槽位，非固定 0），band 为 [0, 10) 段号；返回 0 成功 / 1 槽位错 /
//!   2 段号错或取值非有限（NaN/Inf 被拒，槽位保持原值）。
//! - `compressor_set(slot: i32, param: i32, value: f32) -> i32`
//!   写压缩器某参数。param：0=阈值 1=压缩比 2=启动 3=释放 4=补偿增益；
//!   slot 同上为分配槽；返回 0 成功 / 1 槽位错 /
//!   2 参数号错或取值非有限（NaN/Inf 被拒，参数保持原值）。
//! - `log(ptr: i32, len: i32) -> i32`
//!   读插件线性内存一段 UTF-8 追加到宿主日志。len ≤ 64KiB（超限拒绝）；
//!   累计总量 ≤ 1MiB（超限同样拒绝）；返回 0 成功 / 3 内存、编码或配额错。
//! - `theme_register(ptr: i32, len: i32) -> i32`
//!   插件把一份皮肤描述（调色板文本）写进线性内存后注册；宿主读原始字节
//!   交发行版解析（插件只供色、不碰渲染）。len ≤ 8KiB（超限拒绝）；
//!   返回 0 成功 / 3 内存错。须授予 `theme` 能力才注册（v1 增补）。
//!
//! v2 增补（已冻结面不动，新增函数向后兼容）：
//! - `meter_len() -> i32`：返回当前频谱段数（0 = 无数据）。须 `meter_read` 能力。
//! - `meter_read(ptr: i32, len: i32) -> i32`：把 min(len, 段数) 个 f32（LE
//!   字节）写入插件线性内存；返回实际写入段数；3 = 内存错。须 `meter_read`
//!   能力。数据由发行版周期推入（`set_meter`），插件在 `tick` 导出里读取
//!   （`call_tick`：tick 为可选导出，未导出跳过）。
//!
//! 可视化画面约定（v2，配合 tick / meter_read 使用）：插件在 tick 里把
//! 字符画写进线性内存约定区——`[0,4)` = u32 LE 画面字节长度，
//! `[1024..1024+len)` = UTF-8 文本（行以 \n 分隔）；宿主经 `read_visual`
//! 读回并贴上插件面板。插件只产文本、不碰渲染。
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
/// 宿主日志累计字节上限：单次限长之外再设总量防线——插件循环调用 log
/// 可持续膨胀宿主内存；超限后 log 返回 3（与单次超限同口径，ABI 不变）。
const MAX_LOG_TOTAL_BYTES: usize = 1024 * 1024;
/// 单个插件表的元素上限：wasmi 默认不限表元素，巨型初始表可在实例化期
/// 绕过内存页配额让宿主分配数 GB（DoS）；4096 元素对控制面插件绰绰有余。
const MAX_TABLE_ELEMENTS: usize = 4096;
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
    /// 日志累计字节数（总量配额计数，见 [`MAX_LOG_TOTAL_BYTES`]）。
    log_bytes: usize,
    /// 本插件被分配的槽位号（call_init 时写入）：宿主函数据此做槽位隔离，
    /// 插件只能写自己分配的槽（防跨槽覆盖）；init 之前为 None。
    assigned_slot: Option<u32>,
    /// 资源配额（内存字节 / 表元素 / 内存与表**个数**上限）。实例化前接进 Store。
    limits: StoreLimits,
    /// 插件注册的皮肤字节（theme_register 写入；原始，由发行版解析）。
    theme: Option<Vec<u8>>,
    /// 宿主注入的实时频谱快照（归一化 0.0-1.0，N_BANDS 段；空 = 无数据）。
    /// 数据由发行版周期推入（set_meter），插件经 meter_read 读取；
    /// 只读通道，插件不可写。
    meter: Vec<f32>,
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
            log_bytes: 0,
            assigned_slot: None,
            limits: StoreLimitsBuilder::new()
                // memory_size 单位是字节（页 × 64 KiB）。
                .memory_size(max_mem_pages as usize * 64 * 1024)
                // 表元素配额：堵住「巨型初始表绕过内存页配额」的 DoS 路径。
                .table_elements(MAX_TABLE_ELEMENTS)
                // 内存 / 表**个数**上限 = 1：wasmi 的 memory_size / table_elements
                // 是**逐件**上限（wasmi_core limiter 文档："applied to each linear
                // memory individually"），不限个数时会被多实例放大——wasmi 2.0.0
                // 的 DEFAULT_MEMORY_LIMIT / DEFAULT_TABLE_LIMIT 均为 10 000
                //（wasmi/src/limiter.rs），即 `(memory 4) × 10 000` 可把 4 页
                // 配额放大到 4 万页（≈2.5 GiB），表元素同理。
                // 个数限 1 后逐件上限即等价于 store 级上限。
                // 单内存 / 单表与本沙箱 ABI 约定一致：插件只需一块线性内存
                //（约定区见模块头），常规工具链（C/Rust）也只产出一个内存与
                // 一个函数表。
                .memories(1)
                .tables(1)
                .build(),
            theme: None,
            meter: Vec::new(),
        }
    }

    /// 插件经 theme_register 注册的皮肤字节（原始，由发行版解析）。
    pub fn theme(&self) -> Option<&[u8]> {
        self.theme.as_deref()
    }

    /// 发行版推入最新频谱快照（每帧 / 每周期调用；插件经 meter_read 读取）。
    pub fn set_meter(&mut self, bands: &[f32]) {
        self.meter.clear();
        self.meter.extend_from_slice(bands);
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

        // 内存 / 表配额在 Store 创建时接进 limiter（见 HostState::new）：
        // memory_size / table_elements 是**逐件**上限，另以 memories(1) /
        // tables(1) 限制个数，使逐件上限等价于 store 级上限——否则多内存
        // 模块可把配额放大内存个数倍。
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

        // 实例化前先给足燃料：含 start 段的插件在 start 执行时就需要燃料，
        // wasmi 初始 remaining=0，否则含 start 段的合法插件会在入口即耗尽陷阱
        //（被误报为 Instantiate，排障困难）。call_init/call_tick 前各自重设。
        store
            .set_fuel(self.fuel_per_call)
            .map_err(|e| HostError::Trap(e.to_string()))?;

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
                            let data = caller.data_mut();
                            // 总量配额：累计超限拒绝（防循环调用膨胀宿主内存）。
                            if data.log_bytes.saturating_add(text.len()) > MAX_LOG_TOTAL_BYTES {
                                return ERR_MEM;
                            }
                            data.log_bytes += text.len();
                            data.logs.push(text);
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
                    // setter 对非有限值（NaN/Inf）返回 false：仅靠 corex 侧的
                    // clamp 挡不住 NaN（`f32::clamp` 对 NaN 原样返回），会一路
                    // 传到音频线程的输出。返回值口径与 eq_set 一致（2 = 非法）。
                    let accepted = match param {
                        0 => c.set_threshold(value),
                        1 => c.set_ratio(value),
                        2 => c.set_attack_ms(value),
                        3 => c.set_release_ms(value),
                        4 => c.set_makeup(value),
                        _ => return ERR_BAD_BAND,
                    };
                    if accepted {
                        OK
                    } else {
                        ERR_BAD_BAND
                    }
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
        if granted.contains(&Capability::MeterRead) {
            // meter_len() -> i32：返回当前频谱段数（0 = 无数据）。
            linker.func_wrap(
                "host",
                "meter_len",
                |caller: Caller<'_, HostState>| -> i32 { caller.data().meter.len() as i32 },
            )?;
            // meter_read(ptr, len) -> i32：把 min(len, 段数) 个 f32（LE 字节）
            // 写入插件线性内存；返回实际写入段数；3 = 内存错。
            linker.func_wrap(
                "host",
                "meter_read",
                |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
                    let meter = &caller.data().meter;
                    let n = (len as usize).min(meter.len());
                    if n == 0 {
                        return OK;
                    }
                    let mut buf = Vec::with_capacity(n * 4);
                    for v in &meter[..n] {
                        buf.extend_from_slice(&v.to_le_bytes());
                    }
                    let write = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .map(|mem| mem.write(&mut caller, ptr as usize, &buf));
                    match write {
                        Some(Ok(())) => n as i32,
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

    /// 发行版推入最新频谱快照（每帧 / 每周期调用；插件经 meter_read 读取）。
    pub fn set_meter(&mut self, bands: &[f32]) {
        self.store.data_mut().set_meter(bands);
    }

    /// 读取插件在 tick 里写入的可视化画面（内存约定区：[0,4) = u32 LE 长度，
    /// [1024..1024+len) = UTF-8 文本）。无画面 / 长度非法 / 读取失败返回 None。
    pub fn read_visual(&mut self) -> Option<String> {
        let mem = self
            .instance
            .get_export(&self.store, "memory")
            .and_then(|e| e.into_memory())?;
        let mut len_buf = [0u8; 4];
        mem.read(&self.store, 0, &mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len == 0 || len > 64 * 1024 {
            return None;
        }
        let mut buf = vec![0u8; len];
        mem.read(&self.store, 1024, &mut buf).ok()?;
        String::from_utf8(buf).ok()
    }

    /// 调用插件的 `tick` 导出（周期调用机制：可视化插件在每帧 tick 里
    /// meter_read 取最新频谱）。插件未导出 tick 时跳过（返回 Ok(0)）；
    /// 导出但执行 trap 返回 Err。每次调用前重置燃料预算。
    pub fn call_tick(&mut self) -> Result<i32, HostError> {
        let Ok(func) = self.instance.get_typed_func::<(), i32>(&self.store, "tick") else {
            // tick 是可选导出：未导出的插件不周期调用。
            return Ok(0);
        };
        self.store
            .set_fuel(self.fuel_per_call)
            .map_err(|e| HostError::Trap(e.to_string()))?;
        func.call(&mut self.store, ())
            .map_err(|e| HostError::Trap(e.to_string()))
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

    /// 多内存模块：两个自定义 `(memory 4)` 在 4 页单件上限下会被放行
    ///（StoreLimits 逐内存生效），必须由 `memories(1)` 的个数上限拦下
    ///（回归旧缺陷：多内存可把配额放大内存个数倍）。
    #[test]
    fn multi_memory_module_total_quota() {
        // 模块**自定义**两个 4 页内存（非导入）：StoreLimits 逐内存生效，
        // 单件 4 页配额会放行；memories(1) 的个数上限使第二个内存被拒。
        let wat = r#"
(module
  (memory 4)
  (memory 4)
  (func (export "init") (param $slot i32) (result i32) (i32.const 0))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let err = match host.load(&wasm, "multi-mem", None, &TrustList::default(), &[], &[]) {
            Err(e) => e,
            Ok(_) => panic!("跨内存合计 8 页 > 上限 4 页，应被拒"),
        };
        assert!(
            matches!(err, HostError::Instantiate(_)),
            "应报实例化错误（配额拒绝），实际 {err:?}"
        );
    }

    /// 反向守护：单件配额内不得误伤——单个 4 页内存（= 上限）必须放行。
    #[test]
    fn single_memory_within_quota_is_allowed() {
        let wat = r#"
(module
  (memory (export "memory") 4)
  (func (export "init") (param $slot i32) (result i32) (i32.const 0))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        host.load(&wasm, "one-mem", None, &TrustList::default(), &[], &[])
            .expect("单个 4 页内存正好等于上限，应放行");
    }

    /// 表元素配额：巨型初始表在实例化期即被拒（堵住绕过内存页配额的 DoS）。
    #[test]
    fn giant_table_rejected_by_quota() {
        let wat = r#"
(module
  (table 1000000 funcref)
  (func (export "init") (param $slot i32) (result i32) (i32.const 0))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let err = match host.load(&wasm, "giant-table", None, &TrustList::default(), &[], &[]) {
            Err(e) => e,
            Ok(_) => panic!("巨型表应被配额拒绝"),
        };
        assert!(
            matches!(err, HostError::Instantiate(_)),
            "应为实例化失败，实际 {err:?}"
        );
    }

    /// 配额内的正常表不受影响（防误伤合法插件）。
    #[test]
    fn small_table_within_quota_loads() {
        let wat = r#"
(module
  (table 8 funcref)
  (func (export "init") (param $slot i32) (result i32) (i32.const 0))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let mut plugin = host
            .load(&wasm, "small-table", None, &TrustList::default(), &[], &[])
            .expect("配额内的表应正常装载");
        assert_eq!(plugin.call_init(0).expect("init"), 0);
    }

    /// log 总量配额：单次 64KiB 合规，但累计超 1MiB 后拒绝（返回 3），
    /// 宿主日志总量恰停在配额上限——防插件循环调用膨胀宿主内存。
    #[test]
    fn log_total_quota_caps_host_memory() {
        let wat = r#"
(module
  (import "host" "log" (func $log (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "init") (param $slot i32) (result i32)
    (local $i i32) (local $n i32) (local $ok i32)
    ;; 填满一页 'a'（合法 UTF-8）
    (local.set $i (i32.const 0))
    (block $b (loop $l
      (i32.store8 (local.get $i) (i32.const 97))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 65536)))
    ))
    ;; 20 次 log(0, 65536)：前 16 次成功（16 × 64KiB = 1MiB 配额），其后拒绝
    (local.set $n (i32.const 0))
    (block $b2 (loop $l2
      (local.set $ok (i32.add (local.get $ok)
        (i32.eqz (call $log (i32.const 0) (i32.const 65536)))))
      (local.set $n (i32.add (local.get $n) (i32.const 1)))
      (br_if $l2 (i32.lt_u (local.get $n) (i32.const 20)))
    ))
    (local.get $ok))
)
"#;
        let wasm = wat::parse_str(wat).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        // 燃料放大：填页循环（65536 次 store）超出默认单次预算。
        let host = WasmHost::new(10_000_000, 4, eq_slots, comp_slots);
        let mut plugin = host
            .load(&wasm, "log-quota", None, &TrustList::default(), &[], &[])
            .expect("插件应加载成功");
        let ok = plugin.call_init(0).expect("init 应执行成功");
        assert_eq!(ok, 16, "配额内应成功 16 次（16 × 64KiB = 1MiB）");
        let total: usize = plugin.state().logs.iter().map(|s| s.len()).sum();
        assert_eq!(total, 1024 * 1024, "日志总量应恰为配额上限");
        assert_eq!(plugin.state().logs.len(), 16, "日志条数应为 16");
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

    /// meter_read 可视化插件：tick 里读频谱并校验数值。
    const METER_PLUGIN_WAT: &str = r#"
(module
  (import "host" "meter_len" (func $meter_len (result i32)))
  (import "host" "meter_read" (func $meter_read (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (func (export "init") (param $slot i32) (result i32) (i32.const 0))
  ;; tick：读 4 段频谱到内存 [16,32)；无数据直接返回 0；校验第 0 段 == 0.25。
  (func (export "tick") (result i32)
    (local $n i32)
    (local.set $n (call $meter_read (i32.const 16) (i32.const 4)))
    (if (i32.eqz (local.get $n)) (then (return (i32.const 0))))
    (if (f32.ne (f32.load (i32.const 16)) (f32.const 0.25))
      (then (return (i32.const -1))))
    (local.get $n))
)
"#;

    /// meter_read 端到端：宿主注入频谱快照 → 插件 tick 里读出并校验
    ///（数值一致 + 返回实际段数）。
    #[test]
    fn meter_read_roundtrip_via_tick() {
        let wasm = wat::parse_str(METER_PLUGIN_WAT).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-meter".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        let mut plugin = host
            .load(
                &wasm,
                "tuneux-meter",
                Some((&pubkey, &sig)),
                &trust,
                &[Capability::MeterRead],
                &[Capability::MeterRead],
            )
            .expect("插件应加载成功");
        plugin.call_init(0).expect("init 应执行成功");

        // 宿主注入频谱快照；插件 tick 读出并校验。
        plugin.set_meter(&[0.25, 0.5, 0.75, 1.0]);
        let n = plugin.call_tick().expect("tick 应执行成功");
        assert_eq!(n, 4, "应读入 4 段（实际 {n}）");

        // 未注入数据时：meter_len 为 0，meter_read 写 0 段。
        let host2 = WasmHost::new(
            100_000,
            4,
            std::array::from_fn(|_| Arc::new(EqParams::new())),
            std::array::from_fn(|_| Arc::new(CompressorParams::new())),
        );
        let mut plugin2 = host2
            .load(
                &wasm,
                "tuneux-meter",
                Some((&pubkey, &sig)),
                &trust,
                &[Capability::MeterRead],
                &[Capability::MeterRead],
            )
            .expect("应加载");
        plugin2.call_init(0).expect("init");
        let n0 = plugin2.call_tick().expect("tick");
        assert_eq!(n0, 0, "未注入数据时应读入 0 段");
    }

    /// 未授予 meter_read 能力的插件：host 函数不注册，实例化即失败
    ///（导入找不到 → 装载报错，能力默认拒绝）。
    #[test]
    fn meter_read_requires_capability() {
        let wasm = wat::parse_str(METER_PLUGIN_WAT).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-meter".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        // 申请 meter_read 但授予为空：导入函数未注册，装载必须失败。
        let result = host.load(
            &wasm,
            "tuneux-meter",
            Some((&pubkey, &sig)),
            &trust,
            &[Capability::MeterRead],
            &[],
        );
        assert!(result.is_err(), "无能力授予时导入未注册，装载应失败");
    }

    /// tick 为可选导出：无 tick 的插件 call_tick 返回 Ok(0)（跳过不报错）。
    #[test]
    fn call_tick_optional_export() {
        let wasm = wat::parse_str(EQ_PLUGIN_WAT).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
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
            .expect("应加载");
        assert_eq!(plugin.call_tick().expect("无 tick 导出应跳过"), 0);
    }

    /// 可视化画面约定：tick 里写 [0,4)=长度 + [1024..)=文本，宿主
    /// read_visual 读回。
    #[test]
    fn read_visual_roundtrip() {
        const VISUAL_WAT: &str = r#"
(module
  (memory (export "memory") 1)
  (data (i32.const 1024) "你好，可视化")
  (func (export "init") (param i32) (result i32) (i32.const 0))
  (func (export "tick") (result i32)
    (i32.store (i32.const 0) (i32.const 18))
    (i32.const 0))
)
"#;
        let wasm = wat::parse_str(VISUAL_WAT).expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-vis".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        let mut plugin = host
            .load(&wasm, "tuneux-vis", Some((&pubkey, &sig)), &trust, &[], &[])
            .expect("应加载（零能力需求）");
        plugin.call_init(0).expect("init");
        // 未 tick 前：约定区长度为 0 → None。
        assert_eq!(plugin.read_visual(), None);
        plugin.call_tick().expect("tick");
        assert_eq!(
            plugin.read_visual().as_deref(),
            Some("你好，可视化"),
            "tick 后应读到插件写入的画面"
        );
    }

    /// 第一方可视化插件「能量条」端到端：注入低桶满能量频谱 → tick →
    /// 读回字符画，验证低行满块、中高行空块。
    #[test]
    fn visual_energy_bars_end_to_end() {
        // 读 assets 里的 WAT 源现场编译（不依赖 plugins/ 产物与签名）。
        let wat_path = format!(
            "{}/../tuneux-fx/assets/可视化-能量条.wat",
            env!("CARGO_MANIFEST_DIR")
        );
        let wasm =
            wat::parse_str(std::fs::read_to_string(&wat_path).expect("读取可视化 WAT 源失败"))
                .expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-vis".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        let mut plugin = host
            .load(
                &wasm,
                "tuneux-vis",
                Some((&pubkey, &sig)),
                &trust,
                &[Capability::MeterRead],
                &[Capability::MeterRead],
            )
            .expect("可视化插件应加载");
        plugin.call_init(0).expect("init");
        // 注入：前 85 段满能量（低桶 [0,85)），其余 0。
        let mut bands = vec![0.0f32; 256];
        for v in &mut bands[..85] {
            *v = 1.0;
        }
        plugin.set_meter(&bands);
        plugin.call_tick().expect("tick");
        let frame = plugin.read_visual().expect("应有画面");
        let mut lines = frame.lines();
        let low = lines.next().expect("低行");
        assert!(low.starts_with("低 "), "低行前缀：{low}");
        assert_eq!(low.matches('█').count(), 10, "低桶满能量应满格：{low}");
        let mid = lines.next().expect("中行");
        assert_eq!(mid.matches('█').count(), 0, "中桶零能量应空格：{mid}");
        let high = lines.next().expect("高行");
        assert_eq!(high.matches('█').count(), 0, "高桶零能量应空格：{high}");
    }

    /// 全链路真实性验证：1kHz 纯音 → corex 真实 FFT（compute_spectrum_bands）
    /// → 频谱快照 → 插件 tick → 字符画——中桶应显著亮、低/高桶应近空。
    /// 证明插件面板显示的是真实音频数据，不是演示动画。
    #[test]
    fn visual_energy_bars_reflect_real_fft() {
        // 1kHz 纯音（48kHz 采样，FFT_SIZE 样本）——中频。
        const N: usize = tuneux_corex::spectrum::FFT_SIZE;
        let mut samples = [0.0f32; N];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48000.0).sin();
        }
        let mut planner = rustfft::FftPlanner::new();
        let mut fft_buf = vec![rustfft::num_complex::Complex::new(0.0f32, 0.0); N];
        let mut scratch = vec![rustfft::num_complex::Complex::new(0.0f32, 0.0); N];
        let bands = tuneux_corex::spectrum::compute_spectrum_bands(
            &samples,
            48000,
            &mut planner,
            &mut fft_buf,
            &mut scratch,
        );
        // 先 sanity：1kHz 纯音的频谱峰值应在中桶区（256 段对数分布，
        // 48k 采样 Nyquist 24kHz，1kHz ≈ 第 10-11 段——在低桶！）。
        // 不断言桶归属，先记录峰值位置，用插件画面验证一致性。
        // 读 assets 里的 WAT 源现场编译（不依赖 plugins/ 产物与签名）。
        let wat_path = format!(
            "{}/../tuneux-fx/assets/可视化-能量条.wat",
            env!("CARGO_MANIFEST_DIR")
        );
        let wasm =
            wat::parse_str(std::fs::read_to_string(&wat_path).expect("读取可视化 WAT 源失败"))
                .expect("WAT 应编译成功");
        let eq_slots: [Arc<EqParams>; EQ_SLOTS] =
            std::array::from_fn(|_| Arc::new(EqParams::new()));
        let comp_slots: [Arc<CompressorParams>; COMP_SLOTS] =
            std::array::from_fn(|_| Arc::new(CompressorParams::new()));
        let host = WasmHost::new(100_000, 4, eq_slots, comp_slots);
        let signing = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let pubkey = signing.verifying_key().to_bytes();
        let mut message = b"tuneux-vis".to_vec();
        message.extend_from_slice(&wasm);
        let sig = signing.sign(&message).to_bytes();
        let trust = TrustList::from_parts(vec![pubkey]);
        let mut plugin = host
            .load(
                &wasm,
                "tuneux-vis",
                Some((&pubkey, &sig)),
                &trust,
                &[Capability::MeterRead],
                &[Capability::MeterRead],
            )
            .expect("可视化插件应加载");
        plugin.call_init(0).expect("init");
        plugin.set_meter(&bands);
        plugin.call_tick().expect("tick");
        let frame = plugin.read_visual().expect("应有画面");
        // 真实 FFT 的 256 段按线性频率分布；1kHz 落在低桶（约第 10 段），
        // 因此插件画面的「低」行应有块、且块数与真实频谱能量一致（非随机）。
        let mut lines = frame.lines();
        let low = lines.next().expect("低行");
        let mid = lines.next().expect("中行");
        let high = lines.next().expect("高行");
        // 峰值段位置与块数的关系：真实链路 = 画面数值与 FFT 输出一致。
        //（实测：corex 频段为对数分布，1kHz 落第 138 段 ∈ 中桶 [86,171)。
        // 三桶实为 低≈20-200Hz / 中≈200Hz-2kHz / 高≈2k-20kHz。）
        let peak_idx = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .expect("频谱非空");
        assert!(
            (86..171).contains(&peak_idx),
            "1kHz 峰值应在中桶（实际第 {peak_idx} 段）"
        );
        assert_eq!(low.matches('█').count(), 0, "低桶应空：{low}");
        assert!(mid.matches('█').count() > 0, "中桶应有块：{mid}");
        assert_eq!(high.matches('█').count(), 0, "高桶应空：{high}");
    }
}
