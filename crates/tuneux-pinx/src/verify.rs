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

use ed25519_dalek::{Signature, VerifyingKey};

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

/// Ed25519 验签（严格模式）：公钥（32 字节）验证签名（64 字节）对消息的签名。
///
/// 任何失败（公钥非法 / 签名长度错 / 验证不过 / 弱公钥）返回 false，不 panic。
///
/// 严格模式（`verify_strict` + `is_weak` 双重防护）：ed25519-dalek 的普通
/// `verify` 不拒绝小阶（弱）公钥——攻击者可自选弱公钥（如单位元编码
/// `01 00..00`），对任意消息存在与私钥无关的合法签名（取任意 S，令
/// R = S·B）。这使「已签名·作者未知」三态可被无密钥者骗取。严格模式
/// 同时拒绝非规范签名与非规范公钥编码。
pub fn verify_signature(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    // 拒绝小阶（弱）公钥：is_weak 检查点是否在小阶子群。
    if vk.is_weak() {
        return false;
    }
    vk.verify_strict(message, &Signature::from_bytes(signature))
        .is_ok()
}

/// 由「是否命中官方公钥 / 是否验签通过」判定信任三态。
///
/// - 签名消息 = 插件 id ‖ wasm 字节（绑定插件身份，防掉包）；
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

    /// 弱（小阶）公钥应被拒绝：非严格 verify 对任意消息返回 true——
    /// 严格模式 + is_weak 双重防护后必须 false（回归旧缺陷：
    /// SignedUnknown 三态可被无密钥者骗取）。
    ///
    /// 注意向量必须是**真正的小阶点**：矩阵写 `[1u8; 32]` 是 32 个 0x01，
    /// 可正常解压且 `is_weak() == false`，那样的测试会因为"签名与公钥不匹配"
    /// 而通过，对旧的非严格实现同样通过，等于没有防护。小阶公钥是
    /// 单位元的压缩编码 `01 00 .. 00`，此处逐字节构造并断言 `is_weak()`。
    #[test]
    fn verify_signature_rejects_weak_public_key() {
        use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
        // 小阶公钥：单位元（identity）的压缩编码 01 00 .. 00。
        let mut weak_key = [0u8; 32];
        weak_key[0] = 1;
        let vk = VerifyingKey::from_bytes(&weak_key).expect("单位元编码应可解压");
        assert!(
            vk.is_weak(),
            "测试向量必须是小阶公钥，否则本测试不覆盖目标缺陷"
        );

        // 与私钥无关的伪造签名：R = [1]B（生成元的压缩编码），S = 1。
        // 对任意消息都成立，这正是非严格 verify 的漏洞。
        // R = [1]B 的压缩编码（0x58 后跟 31 个 0x66），S = 1（小端 01 00..00）。
        let mut forged = [0u8; 64];
        for b in forged[..32].iter_mut() {
            *b = 0x66;
        }
        forged[0] = 0x58;
        forged[32] = 1;
        let forged = Signature::from_bytes(&forged);
        // 对照断言：非严格 verify 会接受它——把修复退回旧实现时这里会失败，
        // 从而保证本测试真正锚定"非严格 vs 严格"的差别。
        assert!(
            vk.verify(b"anything", &forged).is_ok(),
            "对照：非严格 verify 本应接受小阶公钥下的伪造签名"
        );
        // 修复后的入口必须拒绝。
        assert!(
            !verify_signature(&weak_key, b"anything", &forged.to_bytes()),
            "弱公钥必须被拒（is_weak + verify_strict）"
        );
        assert!(
            !verify_signature(&weak_key, b"totally-different-payload", &forged.to_bytes()),
            "换任意消息同样必须被拒"
        );

        // 正常公钥不受影响。
        let signing = SigningKey::from_bytes(&[7u8; 32]);
        let sig = signing.sign(b"anything");
        let vk = signing.verifying_key().to_bytes();
        assert!(verify_signature(&vk, b"anything", &sig.to_bytes()));
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
