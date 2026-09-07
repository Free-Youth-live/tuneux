//! # 验签三态（信任标签）
//!
//! 插件身份分三档：签名是「标签」而不是「门槛」——未签名插件仍可安装，
//! 但走降级通道（明确标注 + 首次确认 + 记录文件哈希，永不静默）。
//! 最严格的收紧上限是「必须有一个有效签名（任何作者自签均可）」，
//! 永不要求「必须官方签名」——避免把插件生态锁死成单一来源。
//!
//! 职责边界：本模块只产出「事实」（三态与加载口径）；徽章文案、
//! 确认弹窗、通知前缀等呈现方式归发行版。
//!
//! Ed25519 验签（verify_signature / classify）已在本模块实装；签名是唯一
//! 信任来源（无签名一律 Unsigned，无按插件 id 白名单免签的后门）。

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// 插件信任三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tristate {
    /// 已认证：命中宿主内置信任清单（官方插件），可静默加载。
    Trusted,
    /// 已签名·作者未知：第三方自签。首次使用时询问是否信任该作者，
    /// 信任后在本地记录作者公钥；同一插件换用新密钥时明确提示。
    SignedUnknown,
    /// 未签名或验签未通过：明确标注，首次显式确认后才可加载，
    /// 并记录文件哈希，此后永不静默加载。
    Unsigned,
}

impl Tristate {
    /// 该三态对应的加载口径（宿主与发行版共同遵守的最小行为）。
    pub const fn load_policy(self) -> LoadPolicy {
        match self {
            Tristate::Trusted => LoadPolicy::Silent,
            Tristate::SignedUnknown => LoadPolicy::AskAuthor,
            Tristate::Unsigned => LoadPolicy::ConfirmAndRecord,
        }
    }

    /// 记录用短名（trusted / signed_unknown / unsigned）。变更会造成
    /// 历史记录不可比，勿改。
    pub const fn name(self) -> &'static str {
        match self {
            Tristate::Trusted => "trusted",
            Tristate::SignedUnknown => "signed_unknown",
            Tristate::Unsigned => "unsigned",
        }
    }
}

/// 各三态对应的加载口径（枚举含义见各项说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadPolicy {
    /// 静默加载（界面可展示官方徽章）。
    Silent,
    /// 首次询问作者信任；本地记住公钥，换钥明确提示。
    AskAuthor,
    /// 首次显式确认 + 记录文件哈希，永不静默。
    ConfirmAndRecord,
}

/// 宿主内置信任清单：官方签名公钥（信任根）。
///
/// 锚点必须随宿主一起分发，不能由加载方在运行时自行提供——
/// 否则任何人都能递上自己的公钥完成「自证」，验签形同虚设。
/// 信任唯一来源是「命中官方公钥 + 验签通过」；插件 id 是随二进制公开的
/// 字符串，不作为信任依据（无按 id 白名单免签放行的路径）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustList {
    official_pubkeys: Vec<[u8; 32]>,
}

impl TrustList {
    /// 用官方公钥（Ed25519 公钥，32 字节）构建信任清单。
    pub fn from_parts(official_pubkeys: Vec<[u8; 32]>) -> Self {
        Self { official_pubkeys }
    }

    /// 是否命中官方公钥。
    pub fn contains_pubkey(&self, key: &[u8; 32]) -> bool {
        self.official_pubkeys.iter().any(|k| k == key)
    }

    /// 清单是否为空（空清单 = 不存在任何官方插件）。
    pub fn is_empty(&self) -> bool {
        self.official_pubkeys.is_empty()
    }
}

/// Ed25519 验签：公钥（32 字节）验证签名（64 字节）对消息的签名。
///
/// 任何失败（公钥非法 / 签名长度错 / 验证不过）返回 false，不 panic。
pub fn verify_signature(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    vk.verify(message, &Signature::from_bytes(signature))
        .is_ok()
}

/// 由「是否命中官方公钥 / 是否验签通过」判定信任三态。
///
/// - 签名消息 = 插件 id ‖ wasm 字节（003 §3.3，防掉包）；
/// - 官方公钥命中且验签通过 → [`Tristate::Trusted`]；
/// - 验签通过但作者非官方 → [`Tristate::SignedUnknown`]；
/// - 无签名 / 验签失败 → [`Tristate::Unsigned`]。
pub fn classify(
    plugin_id: &str,
    wasm_bytes: &[u8],
    signature: Option<(&[u8; 32], &[u8; 64])>,
    trust: &TrustList,
) -> Tristate {
    // 拼接签名消息（load 路径，非音频线程，允许分配）。
    let mut message = Vec::with_capacity(plugin_id.len() + wasm_bytes.len());
    message.extend_from_slice(plugin_id.as_bytes());
    message.extend_from_slice(wasm_bytes);

    match signature {
        Some((pubkey, sig)) if verify_signature(pubkey, &message, sig) => {
            if trust.contains_pubkey(pubkey) {
                Tristate::Trusted
            } else {
                Tristate::SignedUnknown
            }
        }
        // 无签名 / 验签失败：一律 Unsigned（签名是唯一信任来源，无 id 白名单后门）。
        Some(_) | None => Tristate::Unsigned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tristate_maps_to_load_policies() {
        assert_eq!(Tristate::Trusted.load_policy(), LoadPolicy::Silent);
        assert_eq!(Tristate::SignedUnknown.load_policy(), LoadPolicy::AskAuthor);
        assert_eq!(
            Tristate::Unsigned.load_policy(),
            LoadPolicy::ConfirmAndRecord
        );
    }

    #[test]
    fn tristate_names_are_stable() {
        assert_eq!(Tristate::Trusted.name(), "trusted");
        assert_eq!(Tristate::SignedUnknown.name(), "signed_unknown");
        assert_eq!(Tristate::Unsigned.name(), "unsigned");
    }

    #[test]
    fn trust_list_lookups() {
        let key = [7u8; 32];
        let list = TrustList::from_parts(vec![key]);
        assert!(list.contains_pubkey(&key));
        assert!(!list.contains_pubkey(&[8u8; 32]));
        assert!(!list.is_empty());
    }

    #[test]
    fn empty_trust_list_is_empty() {
        assert!(TrustList::default().is_empty());
    }

    #[test]
    fn verify_signature_roundtrip() {
        use ed25519_dalek::{Signer, SigningKey};
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let vk = signing.verifying_key();
        let msg = b"plugin id + wasm bytes";
        let sig = signing.sign(msg);
        assert!(verify_signature(&vk.to_bytes(), msg, &sig.to_bytes()));
        // 篡改消息 → 失败。
        let mut bad = msg.to_vec();
        bad[0] ^= 1;
        assert!(!verify_signature(&vk.to_bytes(), &bad, &sig.to_bytes()));
        // 错误公钥 → 失败。
        assert!(!verify_signature(&[0u8; 32], msg, &sig.to_bytes()));
    }

    #[test]
    fn classify_maps_signature_to_tristate() {
        use ed25519_dalek::{Signer, SigningKey};
        let signing = SigningKey::from_bytes(&[9u8; 32]);
        let vk = signing.verifying_key();
        let id = "official-eq";
        let wasm = b"wasm bytes";
        let mut message = id.as_bytes().to_vec();
        message.extend_from_slice(wasm);
        let sig = signing.sign(&message);

        // 官方公钥命中 → Trusted。
        let trust = TrustList::from_parts(vec![vk.to_bytes()]);
        assert_eq!(
            classify(id, wasm, Some((&vk.to_bytes(), &sig.to_bytes())), &trust),
            Tristate::Trusted
        );
        // 验签通过但作者非官方 → SignedUnknown。
        assert_eq!(
            classify(
                id,
                wasm,
                Some((&vk.to_bytes(), &sig.to_bytes())),
                &TrustList::default()
            ),
            Tristate::SignedUnknown
        );
        // 无签名 → 一律 Unsigned（id 不再参与判定，签名是唯一信任来源）。
        assert_eq!(classify(id, wasm, None, &trust), Tristate::Unsigned);
        // 篡改 wasm → 验签失败 → Unsigned。
        let mut bad = wasm.to_vec();
        bad[0] ^= 1;
        assert_eq!(
            classify(id, &bad, Some((&vk.to_bytes(), &sig.to_bytes())), &trust),
            Tristate::Unsigned
        );
    }
}
