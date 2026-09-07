//! # 能力系统（能力总线）
//!
//! 插件向宿主申请的能力枚举与授予仲裁。本模块只做「定义与仲裁」，
//! 不做「实现」——能力实现分布在不同层：
//!
//! - [`Capability::AudioPlay`] / [`Capability::MetadataRead`]：
//!   由本 crate 以薄适配方式调用 `tuneux-corex` / `tuneux-mediax`（不重写）；
//! - [`Capability::UiWrite`]：由发行版注入回调实现（终端 / 图形界面各不相同）；
//! - [`Capability::Network`]：由发行版注入传输层实现（见 [`crate::net`]）。
//!
//! 三条仲裁规则：
//!
//! 1. **求交授予**：实际授予 = 插件申请 ∩ 宿主允许，未申请的不授予；
//! 2. **互斥**：网络能力与内容读取能力互斥——堵住「读取的元数据经网络
//!    批量外带」这条不可见通道。该规则必要但不充分：可见的界面外带
//!    通道（把内容刷到状态栏）由发行版以「通知来源前缀」等方式兜底，
//!    与本规则、侧载插件默认无网络并列构成三重防线。
//! 3. **网络可用性**：网络能力还要求编译期开启 network 特性；特性关闭时
//!    即使申请 / 允许命中也不授予（显式报错而非静默降级）。

/// 插件可向宿主申请的能力。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// 音频播放控制（播放 / 暂停 / 跳转等；薄适配 corex 引擎）。
    AudioPlay,
    /// 读取曲目元数据（标题 / 专辑 / 封面等；薄适配 mediax）。
    MetadataRead,
    /// 向界面写入有限文本（状态栏 / 通知；发行版注入回调实现）。
    UiWrite,
    /// 网络请求（宿主代发；默认拒绝，须显式授权且编译期开启 network 特性）。
    Network,
    /// 音频效果器参数控制（均衡器等 DSP 槽位写入；薄适配 corex，音频线程执行）。
    AudioDsp,
    /// 向界面注册一份调色板（皮肤）：插件只供色、宿主负责渲染（v1 只动颜色）。
    Theme,
}

impl Capability {
    /// 稳定短名（记录日志 / 界面展示用）。变更会造成历史记录不可比，勿改。
    pub const fn name(self) -> &'static str {
        match self {
            Capability::AudioPlay => "audio_play",
            Capability::MetadataRead => "metadata_read",
            Capability::UiWrite => "ui_write",
            Capability::Network => "network",
            Capability::AudioDsp => "audio_dsp",
            Capability::Theme => "theme",
        }
    }

    /// 从能力短名解析（稳定名，见 [`Capability::name`]）。未知返回 None。
    pub fn from_name(name: &str) -> Option<Capability> {
        match name {
            "audio_play" => Some(Capability::AudioPlay),
            "metadata_read" => Some(Capability::MetadataRead),
            "ui_write" => Some(Capability::UiWrite),
            "network" => Some(Capability::Network),
            "audio_dsp" => Some(Capability::AudioDsp),
            "theme" => Some(Capability::Theme),
            _ => None,
        }
    }
}

/// 解析 MV3 式能力清单（manifest 文本）：每行一个能力短名，`#` 注释与空行忽略。
///
/// 未知短名报错（显式失败，不静默跳过——插件清单写错应被发现，而不是悄悄
/// 少授能力）。返回去重后的能力列表（保持声明顺序）。
pub fn parse_manifest(text: &str) -> Result<Vec<Capability>, String> {
    let mut out: Vec<Capability> = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match Capability::from_name(line) {
            Some(cap) => {
                if !out.contains(&cap) {
                    out.push(cap);
                }
            }
            None => return Err(format!("未知能力短名（第 {} 行）：{line}", idx + 1)),
        }
    }
    Ok(out)
}

/// 能力授予失败的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    /// 网络能力与内容读取能力互斥，二者不能同时授予。
    NetworkExclusive,
    /// 申请了网络能力，但当前构建未开启 network 特性，网络一律不可授予。
    NetworkUnavailable,
}

/// 网络能力在当前构建下是否可用。
///
/// 由编译期 `network` 特性决定：特性关闭时（默认）网络能力一律不可
/// 授予——「离线优先、联网可选」在编译期即可核查（不引入网络依赖）。
pub const fn network_available() -> bool {
    cfg!(feature = "network")
}

/// 授予仲裁：求交 + 互斥检查 + 网络可用性检查。
///
/// - `requested`：插件声明申请的能力（来自插件清单）；
/// - `allowed`：宿主侧允许该插件获得的能力（信任级别与用户授权共同决定）；
/// - 返回实际授予集（保持申请顺序、已去重）；违反互斥或网络不可用时报错。
///
/// 报错而非静默剔除：允许集出现网络能力而特性未开启，说明上游配置
/// 已经出错，应当显式失败而不是悄悄降级。
pub fn arbitrate(
    requested: &[Capability],
    allowed: &[Capability],
) -> Result<Vec<Capability>, CapError> {
    let granted = intersect(requested, allowed);
    // 互斥：网络 ⟂ 内容读取。
    if granted.contains(&Capability::Network) && granted.contains(&Capability::MetadataRead) {
        return Err(CapError::NetworkExclusive);
    }
    // 网络可用性：特性未开启时，允许集里不该出现网络能力。
    if granted.contains(&Capability::Network) && !network_available() {
        return Err(CapError::NetworkUnavailable);
    }
    Ok(granted)
}

/// 求交：保留 `requested` 的顺序并去重（私有辅助）。
fn intersect(requested: &[Capability], allowed: &[Capability]) -> Vec<Capability> {
    let mut out: Vec<Capability> = Vec::new();
    for &cap in requested {
        if allowed.contains(&cap) && !out.contains(&cap) {
            out.push(cap);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersect_keeps_request_order_and_dedups() {
        let requested = [
            Capability::UiWrite,
            Capability::AudioPlay,
            Capability::UiWrite,
        ];
        let allowed = [
            Capability::AudioPlay,
            Capability::UiWrite,
            Capability::Network,
        ];
        assert_eq!(
            intersect(&requested, &allowed),
            vec![Capability::UiWrite, Capability::AudioPlay]
        );
    }

    #[test]
    fn arbitrate_grants_intersection_only() {
        let requested = [Capability::AudioPlay, Capability::UiWrite];
        let allowed = [Capability::AudioPlay];
        assert_eq!(
            arbitrate(&requested, &allowed),
            Ok(vec![Capability::AudioPlay])
        );
    }

    #[test]
    fn arbitrate_rejects_network_with_metadata_read() {
        let requested = [Capability::MetadataRead, Capability::Network];
        assert_eq!(
            arbitrate(&requested, &requested),
            Err(CapError::NetworkExclusive)
        );
    }

    #[test]
    fn arbitrate_allows_network_without_metadata_read_when_available() {
        // 网络能力可用时，网络可与界面写入共存（互斥只针对内容读取）。
        if network_available() {
            let requested = [Capability::UiWrite, Capability::Network];
            assert_eq!(
                arbitrate(&requested, &requested),
                Ok(vec![Capability::UiWrite, Capability::Network])
            );
        }
    }

    #[cfg(not(feature = "network"))]
    #[test]
    fn arbitrate_rejects_network_when_feature_off() {
        let requested = [Capability::Network];
        assert_eq!(
            arbitrate(&requested, &requested),
            Err(CapError::NetworkUnavailable)
        );
    }

    #[test]
    fn capability_names_are_stable() {
        assert_eq!(Capability::AudioPlay.name(), "audio_play");
        assert_eq!(Capability::MetadataRead.name(), "metadata_read");
        assert_eq!(Capability::UiWrite.name(), "ui_write");
        assert_eq!(Capability::Network.name(), "network");
        assert_eq!(Capability::AudioDsp.name(), "audio_dsp");
        assert_eq!(Capability::Theme.name(), "theme");
    }

    #[test]
    fn parse_manifest_parses_and_dedups() {
        let text = "# 能力清单\naudio_dsp\n\naudio_play\naudio_dsp\n";
        assert_eq!(
            parse_manifest(text),
            Ok(vec![Capability::AudioDsp, Capability::AudioPlay])
        );
    }

    #[test]
    fn parse_manifest_rejects_unknown() {
        assert!(parse_manifest("audio_dsp\nbogus\n").is_err());
    }
}
