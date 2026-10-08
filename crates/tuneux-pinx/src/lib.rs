//! # tuneux-pinx —— 插件宿主运行时
//!
//! 定位：插件系统与各发行版（tuneux-fx / 将来的 tuneux-max）之间的
//! **能力总线**——只负责「定义与仲裁」，不负责「实现」：
//!
//! - 音频 / 数据能力：以薄适配调用 `tuneux-corex` / `tuneux-mediax`，不重写；
//! - 界面能力：由发行版注入回调，本 crate 只定义接口；
//! - 网络能力：由发行版注入传输层，本 crate **永不自行联网**。
//!
//! 四层边界：音频进 corex、领域数据进 mediax、插件运行时进 pinx、
//! 其余发行版自写；基础版 tuneux 永不依赖本 crate。
//!
//! 当前包含的策略层（均可独立测试）：
//! - [`caps`]：能力枚举、求交授予、互斥规则（网络 ⟂ 内容读取）；
//! - [`verify`]：插件信任三态（已认证 / 已签名·作者未知 / 未签名）与加载口径；
//! - [`net`]：网络代发钩子——意图仲裁（身份带外绑定 / 限额钳制 / 用途取
//!   签名 manifest）、URL 预检与域名白名单（解析后主机比对）、传输层接口与离线拒绝；
//! - [`journal`]：插件加载与授权的追加式记录（核查日志，非防篡改）；
//! - [`runtime`]：WASM 控制面运行时——wasmi 装载 / 实例化、宿主函数注入
//!   （`eq_set` / `compressor_set` / `theme_register` / `meter_read` 等）、
//!   燃料 / 内存页硬配额、能力默认拒绝（ABI 清单见 runtime 模块文档）。
//!
//! 音频线程零插件：插件只经宿主函数写参数（控制面），实际 DSP 由宿主
//! （corex）在音频线程执行。宿主接口以真实插件（均衡器）跑通后冻结。

// 测试代码允许 unwrap：断言失败即测试失败，语义与生产路径不同
//（生产路径零 unwrap 由 workspace lints 强制）。
#![cfg_attr(test, allow(clippy::unwrap_used))]
// 公开项必须有文档注释（与 corex / mediax / commonx 同口径；信任边界 crate 尤其需要）。
#![deny(missing_docs)]
pub mod caps;
pub mod ids;
pub mod journal;
pub mod net;
pub mod runtime;
pub mod verify;

pub use caps::{parse_manifest, CapError, Capability};
pub use ids::{COMP_ID, EQ_ID, SKIN_ID};
pub use runtime::{HostError, HostState, LoadedPlugin, WasmHost};
pub use verify::{classify, verify_signature, LoadPolicy, Tristate, TrustList};
