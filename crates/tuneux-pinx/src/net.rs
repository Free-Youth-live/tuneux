//! # 网络代发钩子
//!
//! 插件永不直连网络：所有请求经宿主代发——插件提交声明式 [`Intent`]，
//! 宿主校验域名白名单后调用发行版注入的 [`NetworkTransport`] 完成传输。
//! 本 crate 永不自行联网，也不引入任何网络相关依赖。
//!
//! 当前网络能力默认关闭（见 [`crate::caps::network_available`]）：
//! [`OfflineTransport`] 对一切请求返回结构化 [`NetError::OfflineDenied`]，
//! 「断网不塌」——插件拿到的是可优雅降级的明确答复，而不是故障。
//! 将来接通真实网络只需由发行版注入真实传输层，接口保持不变。

/// 请求意图：插件 × 域名 × 用途，是显式授权的最小单位。
///
/// 用户授权的对象是「这个插件为了这个用途访问这个域名」，
/// 而不是笼统的「允许联网」——授权范围因此可以被精确描述与撤销。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// 发起请求的插件标识。
    pub plugin_id: String,
    /// 目标域名（精确匹配白名单，不支持通配）。
    pub domain: String,
    /// 用途说明（展示给用户，作为授权依据）。
    pub purpose: String,
    /// 请求方式。
    pub method: Method,
    /// 响应体积上限（字节）。
    pub max_bytes: u64,
    /// 超时（毫秒）。
    pub timeout_ms: u32,
}

/// 请求方式（当前只允许无请求体的读取，写操作随真实传输层再放开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// 读取（GET）。
    Get,
}

/// 域名白名单：精确匹配，禁通配。
///
/// 通配会把授权范围扩大到不可预知的子域，因此构造时直接拒绝
/// 含通配符的条目。防同源逃逸（解析后 IP 归属校验）随真实传输层
/// 实装一并引入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainWhitelist {
    domains: Vec<String>,
}

/// 白名单构造错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhitelistError {
    /// 条目含通配符（`*`），整体拒绝构造。
    Wildcard,
}

impl DomainWhitelist {
    /// 构造白名单；任一条目含通配符即整体拒绝。
    pub fn new(domains: Vec<String>) -> Result<Self, WhitelistError> {
        if domains.iter().any(|d| d.contains('*')) {
            return Err(WhitelistError::Wildcard);
        }
        Ok(Self { domains })
    }

    /// 域名是否被允许（大小写不敏感的精确相等）。
    pub fn allows(&self, domain: &str) -> bool {
        self.domains.iter().any(|d| d.eq_ignore_ascii_case(domain))
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.domains.len()
    }

    /// 是否为空（空白名单 = 一律拒绝）。
    pub fn is_empty(&self) -> bool {
        self.domains.is_empty()
    }
}

/// 网络请求的结果错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// 网络能力未开启或未授权：结构化降级，不是故障。
    OfflineDenied,
    /// 域名不在白名单。
    DomainNotAllowed,
    /// 超出体积 / 频率限制。
    LimitExceeded,
    /// 传输失败（网络层错误）。
    TransportFailed,
}

/// 传输层接口：由发行版注入（桌面 HTTP 客户端 / 移动端系统网络栈）。
///
/// 本 crate 只定义接口与策略，不提供真实实现——保证「宿主运行时
/// 永不自行联网」在依赖层面可核查（不引入网络相关 crate）。
pub trait NetworkTransport {
    /// 执行一次代发请求，返回响应体。
    fn request(&self, intent: &Intent) -> Result<Vec<u8>, NetError>;
}

/// 默认传输层：对一切请求返回 [`NetError::OfflineDenied`]。
///
/// 网络能力关闭期间的唯一实现；插件据此走离线降级路径。
#[derive(Debug, Clone, Copy, Default)]
pub struct OfflineTransport;

impl NetworkTransport for OfflineTransport {
    fn request(&self, _intent: &Intent) -> Result<Vec<u8>, NetError> {
        Err(NetError::OfflineDenied)
    }
}

/// 校验意图并通过传输层执行请求：先域名白名单，后传输。
///
/// 白名单未通过返回 [`NetError::DomainNotAllowed`]；传输层其余错误
/// 原样透传。体积 / 频率限制随真实传输层实装加入。
pub fn dispatch<T: NetworkTransport + ?Sized>(
    transport: &T,
    whitelist: &DomainWhitelist,
    intent: &Intent,
) -> Result<Vec<u8>, NetError> {
    if !whitelist.allows(&intent.domain) {
        return Err(NetError::DomainNotAllowed);
    }
    transport.request(intent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(domain: &str) -> Intent {
        Intent {
            plugin_id: "demo".to_string(),
            domain: domain.to_string(),
            purpose: "获取封面".to_string(),
            method: Method::Get,
            max_bytes: 1024,
            timeout_ms: 3000,
        }
    }

    #[test]
    fn offline_transport_denies_everything() {
        let t = OfflineTransport;
        assert_eq!(
            t.request(&intent("example.com")),
            Err(NetError::OfflineDenied)
        );
    }

    #[test]
    fn whitelist_rejects_wildcard_entries() {
        assert_eq!(
            DomainWhitelist::new(vec!["*.example.com".to_string()]),
            Err(WhitelistError::Wildcard)
        );
    }

    #[test]
    fn whitelist_matches_exactly_and_case_insensitive() {
        let list = DomainWhitelist::new(vec!["Cover.Example.com".to_string()]).unwrap();
        assert!(list.allows("cover.example.com"));
        assert!(!list.allows("api.example.com"));
        assert!(!list.allows("example.com"));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn dispatch_checks_whitelist_before_transport() {
        let list = DomainWhitelist::new(vec!["ok.com".to_string()]).unwrap();
        // 白名单外：直接拒绝，不触达传输层。
        assert_eq!(
            dispatch(&OfflineTransport, &list, &intent("bad.com")),
            Err(NetError::DomainNotAllowed)
        );
        // 白名单内：进入传输层，离线传输层返回离线拒绝。
        assert_eq!(
            dispatch(&OfflineTransport, &list, &intent("ok.com")),
            Err(NetError::OfflineDenied)
        );
    }

    #[test]
    fn empty_whitelist_denies_all() {
        let list = DomainWhitelist::new(Vec::new()).unwrap();
        assert!(list.is_empty());
        assert_eq!(
            dispatch(&OfflineTransport, &list, &intent("any.com")),
            Err(NetError::DomainNotAllowed)
        );
    }
}
