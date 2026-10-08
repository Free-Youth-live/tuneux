//! # 网络代发钩子
//!
//! 插件永不直连网络：所有请求经宿主代发——插件提交声明式 [`Intent`]，
//! 宿主完成**意图仲裁**（[`RequestContext`] 宿主侧三条：身份带外绑定、
//! 限额钳制取小、用途以签名 manifest 为准）与**白名单预检**（URL 解析后
//! 取主机名比对，防 userinfo / 端口 / 尾点 / IDN 同形 / IP 字面量绕过），
//! 再调用发行版注入的 [`NetworkTransport`] 完成传输。
//!
//! 职责分界（安全检查不拆两半）：本模块只做预检与仲裁；固定 IP 连接
//!（防 DNS rebinding）、重定向逐跳复检、TLS 校验、体积/超时执行、
//! 图像解码上限等传输层安全清单，统一落在注入的真实传输实现内。
//!
//! 本 crate 永不自行联网，也不引入任何网络相关依赖。
//! 当前网络能力默认关闭（见 [`crate::caps::network_available`]）：
//! [`OfflineTransport`] 对一切请求返回结构化 [`NetError::OfflineDenied`]，
//! 「断网不塌」——插件拿到的是可优雅降级的明确答复，而不是故障。
//! 将来接通真实网络只需由发行版注入真实传输层，仲裁与预检语义不变。

/// 请求意图：插件 × 目标 URL × 用途，是显式授权的最小单位。
///
/// 用户授权的对象是「这个插件为了这个用途访问这个域名」，
/// 而不是笼统的「允许联网」——授权范围因此可以被精确描述与撤销。
///
/// **信任口径**：`plugin_id` / `purpose` / `max_bytes` / `timeout_ms`
/// 均为插件自报值，宿主一律不信任——[`dispatch`] 会用
/// [`RequestContext`] 的带外值覆盖身份与用途、用宿主上限钳制限额
///（取小），传输层收到的是仲裁后的有效意图。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// 发起请求的插件标识（插件自报，仅作日志展示；仲裁时被宿主带外值覆盖）。
    pub plugin_id: String,
    /// 请求的完整 URL（预检解析主机名后与白名单比对；仅允许无请求体的读取）。
    pub url: String,
    /// 用途说明（插件自报，仅作日志展示；仲裁时被签名 manifest 的值覆盖）。
    pub purpose: String,
    /// 请求方式。
    pub method: Method,
    /// 期望的响应体积上限（字节；宿主用自己的策略上限钳制取小）。
    pub max_bytes: u64,
    /// 期望的超时（毫秒；宿主用自己的策略上限钳制取小）。
    pub timeout_ms: u32,
}

/// 宿主侧请求上下文：仲裁依据全部来自宿主带外通道，绝不读插件自报值。
///
/// 三条口径：
/// 1. `plugin_id` 由宿主在插件加载时带外绑定（闭包捕获装载 id），
///    杜绝「第三方插件冒充第一方身份」；
/// 2. `max_bytes` / `timeout_ms` 是被限制方自行申报的限制——宿主用
///    自己的策略上限钳制取小，申报值只在更严格时生效；
/// 3. `purpose` 以**签名的 manifest** 为准（v2 签名已覆盖 manifest），
///    不取每次请求的实时字段，防「用途说明与实际行为脱钩」。
#[derive(Debug, Clone, Copy)]
pub struct RequestContext<'a> {
    /// 宿主带外绑定的插件标识（加载时确定，插件不可改写）。
    pub plugin_id: &'a str,
    /// 签名 manifest 中声明的用途（授权展示与日志的唯一可信来源）。
    pub purpose: &'a str,
    /// 宿主的响应体积策略上限（字节；与插件申报值取小）。
    pub max_bytes_cap: u64,
    /// 宿主的超时策略上限（毫秒；与插件申报值取小）。
    pub timeout_cap_ms: u32,
    /// 是否显式豁免明文 HTTP（内网 NAS 场景；默认 false = 仅 HTTPS）。
    pub allow_plain_http: bool,
}

/// 请求方式（当前只允许无请求体的读取，写操作随真实传输层再放开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// 读取（GET）。
    Get,
}

/// URL 预检拒绝原因（全部返回结构化错误，绝不 panic）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlError {
    /// 结构非法：缺 scheme 分隔、空主机、多尾点等。
    Malformed,
    /// scheme 不是 https（且未显式豁免明文 http）。
    UnsupportedScheme,
    /// authority 含 userinfo（`allowed.com@evil.com`）——整体拒绝，
    /// 不做「取 @ 后段」的宽容解析（宽容解析正是绕过面）。
    UserinfoPresent,
    /// 主机名含非 ASCII 字符——IDN 同形攻击面，预检层一律拒绝
    /// （punycode 转换与展示属产品层策略，不进信任判定）。
    NonAsciiHost,
    /// 主机是 IP 字面量（IPv4 / IPv6）——白名单只收域名；
    /// IP 归属校验（私网/回环/保留段拒绝）与固定 IP 连接属传输层职责。
    IpLiteralHost,
}

/// 解析并规范化后的目标主机（白名单比对的唯一输入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// 规范化主机名：ASCII 小写、至多一个尾点已去除。
    pub name: String,
    /// 显式端口（白名单比对不含端口；传输层按需使用）。
    pub port: Option<u16>,
}

/// 解析 URL 的 scheme + authority 并做预检（手写解析，零依赖）。
///
/// 拒绝策略宁严勿宽：userinfo、非 ASCII 主机、IP 字面量、多尾点、
/// 空主机一律结构化拒绝；尾点最多容忍一个（DNS 根表示法），
/// 规范化为无尾点小写形式再参与比对。
///
/// `allow_plain_http` 为 false 时仅接受 `https:`。
pub fn parse_url(url: &str, allow_plain_http: bool) -> Result<Host, UrlError> {
    // 1. scheme
    let (scheme, rest) = url.split_once("://").ok_or(UrlError::Malformed)?;
    let scheme_ok = match scheme {
        "https" => true,
        "http" => allow_plain_http,
        _ => false,
    };
    if !scheme_ok {
        return Err(UrlError::UnsupportedScheme);
    }
    // 2. authority = 到首个 '/' '?' '#' 为止
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    if authority.is_empty() {
        return Err(UrlError::Malformed);
    }
    // 3. userinfo：含 '@' 整体拒绝（不做宽容切分）
    if authority.contains('@') {
        return Err(UrlError::UserinfoPresent);
    }
    // 4. IPv6 字面量：'[' 开头一律按 IP 字面量拒绝
    if authority.starts_with('[') {
        return Err(UrlError::IpLiteralHost);
    }
    // 5. 端口拆分（authority 已排除 IPv6 字面量，最后一个 ':' 之后是端口）
    let (host_part, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p.parse().map_err(|_| UrlError::Malformed)?;
            (h, Some(port))
        }
        None => (authority, None),
    };
    if host_part.is_empty() {
        return Err(UrlError::Malformed);
    }
    // 6. 非 ASCII 主机（IDN 同形）拒绝
    if !host_part.is_ascii() {
        return Err(UrlError::NonAsciiHost);
    }
    // 7. 尾点：至多容忍一个（根表示法）；两个及以上 = 结构非法
    let mut name = host_part.to_ascii_lowercase();
    if let Some(stripped) = name.strip_suffix('.') {
        if stripped.ends_with('.') {
            return Err(UrlError::Malformed);
        }
        name = stripped.to_string();
    }
    if name.is_empty() {
        return Err(UrlError::Malformed);
    }
    // 8. IPv4 字面量拒绝（全数字与点的组合）
    if name.chars().all(|c| c.is_ascii_digit() || c == '.') && name.contains('.') {
        return Err(UrlError::IpLiteralHost);
    }
    Ok(Host { name, port })
}

/// 域名白名单：规范化精确匹配，禁通配。
///
/// 通配会把授权范围扩大到不可预知的子域，构造时直接拒绝含通配符的
/// 条目；条目本身也做与请求侧同一套规范化（小写、至多一个尾点），
/// 防「白名单条目写法差异」造成的比对漏判。
/// 防同源逃逸（解析后 IP 归属校验、固定 IP 连接）属传输层职责，
/// 随真实传输实现引入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainWhitelist {
    domains: Vec<String>,
}

/// 白名单构造错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhitelistError {
    /// 条目含通配符（`*`），整体拒绝构造。
    Wildcard,
    /// 条目结构非法（空串、含 `@`、非 ASCII、多尾点、IP 字面量）。
    MalformedEntry,
}

impl DomainWhitelist {
    /// 构造白名单；任一条目含通配符或结构非法即整体拒绝。
    pub fn new(domains: Vec<String>) -> Result<Self, WhitelistError> {
        let mut normalized = Vec::with_capacity(domains.len());
        for d in &domains {
            if d.contains('*') {
                return Err(WhitelistError::Wildcard);
            }
            normalized.push(normalize_entry(d).ok_or(WhitelistError::MalformedEntry)?);
        }
        Ok(Self {
            domains: normalized,
        })
    }

    /// 解析后的主机是否在白名单内（规范化精确相等；不含端口）。
    pub fn allows(&self, host: &Host) -> bool {
        self.domains.iter().any(|d| d == &host.name)
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

/// 白名单条目规范化：小写、至多去一个尾点；非法形态返回 None。
fn normalize_entry(entry: &str) -> Option<String> {
    let e = entry.trim();
    if e.is_empty() || !e.is_ascii() || e.contains('@') {
        return None;
    }
    let mut name = e.to_ascii_lowercase();
    if let Some(stripped) = name.strip_suffix('.') {
        if stripped.ends_with('.') {
            return None;
        }
        name = stripped.to_string();
    }
    if name.is_empty() {
        return None;
    }
    // IP 字面量不入白名单（与请求侧同口径）
    if name.starts_with('[')
        || (name.chars().all(|c| c.is_ascii_digit() || c == '.') && name.contains('.'))
    {
        return None;
    }
    Some(name)
}

/// 网络请求的结果错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// 网络能力未开启或未授权：结构化降级，不是故障。
    OfflineDenied,
    /// 域名不在白名单。
    DomainNotAllowed,
    /// URL 预检拒绝（结构非法 / scheme 不允许 / userinfo / 非 ASCII / IP 字面量）。
    UrlRejected,
    /// 超出体积 / 频率限制。
    LimitExceeded,
    /// 传输失败（网络层错误）。
    TransportFailed,
}

/// 传输层接口：由发行版注入（桌面 HTTP 客户端 / 移动端系统网络栈）。
///
/// 本 crate 只定义接口与策略，不提供真实实现——保证「宿主运行时
/// 永不自行联网」在依赖层面可核查（不引入网络相关 crate）。
/// 实现收到的是 [`dispatch`] 仲裁后的有效意图（身份/用途为宿主带外值、
/// 限额已钳制取小）。
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

/// 仲裁意图并通过传输层执行请求：预检 → 白名单 → 钳制 → 传输。
///
/// 顺序不可换：先 URL 预检与白名单（不触达传输层），再构造仲裁后的
/// 有效意图（身份/用途覆盖 + 限额取小）交给传输层。任何一步失败都
/// 返回结构化错误，绝不 panic。体积 / 频率限制的**执行**随真实传输层
/// 实装（本层只负责把钳制后的上限写进有效意图）。
pub fn dispatch<T: NetworkTransport + ?Sized>(
    transport: &T,
    whitelist: &DomainWhitelist,
    ctx: &RequestContext<'_>,
    intent: &Intent,
) -> Result<Vec<u8>, NetError> {
    // 1. URL 预检（scheme / userinfo / ASCII / IP 字面量 / 尾点规范化）
    let host = parse_url(&intent.url, ctx.allow_plain_http).map_err(|_| NetError::UrlRejected)?;
    // 2. 白名单比对（解析后的规范化主机名，禁字符串相等口径）
    if !whitelist.allows(&host) {
        return Err(NetError::DomainNotAllowed);
    }
    // 3. 宿主侧三条：身份带外覆盖、用途取 manifest、限额钳制取小
    let mut effective = intent.clone();
    effective.plugin_id = ctx.plugin_id.to_string();
    effective.purpose = ctx.purpose.to_string();
    effective.max_bytes = effective.max_bytes.min(ctx.max_bytes_cap);
    effective.timeout_ms = effective.timeout_ms.min(ctx.timeout_cap_ms);
    // 4. 交给传输层（收到的永远是仲裁后的意图）
    transport.request(&effective)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(url: &str) -> Intent {
        Intent {
            plugin_id: "self-claimed-id".to_string(),
            url: url.to_string(),
            purpose: "self-claimed-purpose".to_string(),
            method: Method::Get,
            max_bytes: 1024,
            timeout_ms: 3000,
        }
    }

    fn ctx() -> RequestContext<'static> {
        RequestContext {
            plugin_id: "host-bound-id",
            purpose: "manifest-purpose",
            max_bytes_cap: 4 * 1024 * 1024,
            timeout_cap_ms: 10_000,
            allow_plain_http: false,
        }
    }

    fn whitelist() -> DomainWhitelist {
        DomainWhitelist::new(vec![
            "allowed.com".to_string(),
            "Cover.Example.com.".to_string(),
        ])
        .unwrap()
    }

    /// 捕获型传输层：记录收到的有效意图，供仲裁断言。
    struct CaptureTransport(std::cell::RefCell<Option<Intent>>);

    impl NetworkTransport for CaptureTransport {
        fn request(&self, intent: &Intent) -> Result<Vec<u8>, NetError> {
            *self.0.borrow_mut() = Some(intent.clone());
            Ok(Vec::new())
        }
    }

    #[test]
    fn offline_transport_denies_everything() {
        let t = OfflineTransport;
        assert_eq!(
            t.request(&intent("https://example.com/")),
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
    fn whitelist_rejects_malformed_entries() {
        assert_eq!(
            DomainWhitelist::new(vec!["".to_string()]),
            Err(WhitelistError::MalformedEntry)
        );
        assert_eq!(
            DomainWhitelist::new(vec!["a@b.com".to_string()]),
            Err(WhitelistError::MalformedEntry)
        );
        assert_eq!(
            DomainWhitelist::new(vec!["127.0.0.1".to_string()]),
            Err(WhitelistError::MalformedEntry)
        );
        assert_eq!(
            DomainWhitelist::new(vec!["exámple.com".to_string()]),
            Err(WhitelistError::MalformedEntry)
        );
    }

    #[test]
    fn whitelist_normalizes_entries_and_matches_case_insensitive() {
        let list = whitelist();
        // 条目 "Cover.Example.com." 规范化为 cover.example.com
        assert!(list.allows(&Host {
            name: "cover.example.com".to_string(),
            port: None
        }));
        assert!(!list.allows(&Host {
            name: "api.example.com".to_string(),
            port: None
        }));
        assert!(!list.allows(&Host {
            name: "example.com".to_string(),
            port: None
        }));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn empty_whitelist_denies_all() {
        let list = DomainWhitelist::new(Vec::new()).unwrap();
        assert!(list.is_empty());
        assert_eq!(
            dispatch(
                &OfflineTransport,
                &list,
                &ctx(),
                &intent("https://any.com/")
            ),
            Err(NetError::DomainNotAllowed)
        );
    }

    // —— P3 负向用例：每种绕过一个测试 ——

    #[test]
    fn rejects_userinfo_spoofing() {
        // allowed.com@evil.com：真实主机是 evil.com——整体拒绝，不做宽容切分
        assert_eq!(
            parse_url("https://allowed.com@evil.com/", false),
            Err(UrlError::UserinfoPresent)
        );
        assert_eq!(
            dispatch(
                &OfflineTransport,
                &whitelist(),
                &ctx(),
                &intent("https://allowed.com@evil.com/")
            ),
            Err(NetError::UrlRejected)
        );
    }

    #[test]
    fn rejects_path_confusion_and_subdomain_suffix() {
        // 路径里藏白名单域名不改变主机判定
        let h = parse_url("https://evil.com/https://allowed.com", false).unwrap();
        assert_eq!(h.name, "evil.com");
        assert_eq!(
            dispatch(
                &OfflineTransport,
                &whitelist(),
                &ctx(),
                &intent("https://evil.com/https://allowed.com")
            ),
            Err(NetError::DomainNotAllowed)
        );
        // 后缀拼接：allowed.com.evil.com 的主机是 evil.com 的子域
        assert_eq!(
            dispatch(
                &OfflineTransport,
                &whitelist(),
                &ctx(),
                &intent("https://allowed.com.evil.com/")
            ),
            Err(NetError::DomainNotAllowed)
        );
    }

    #[test]
    fn normalizes_case_port_and_single_trailing_dot() {
        // 大小写 + 显式端口 + 单尾点：规范化后命中白名单
        let h = parse_url("https://ALLOWED.com.:8443/x", false).unwrap();
        assert_eq!(h.name, "allowed.com");
        assert_eq!(h.port, Some(8443));
        assert!(whitelist().allows(&h));
        // 多尾点是绕过面：拒绝
        assert_eq!(
            parse_url("https://allowed.com../", false),
            Err(UrlError::Malformed)
        );
    }

    #[test]
    fn rejects_idn_homoglyph_hosts() {
        // 非 ASCII 主机（同形攻击面）预检层一律拒绝
        assert_eq!(
            parse_url("https://аllowed.com/", false),
            Err(UrlError::NonAsciiHost)
        );
    }

    #[test]
    fn rejects_ip_literal_hosts() {
        assert_eq!(
            parse_url("https://127.0.0.1/", false),
            Err(UrlError::IpLiteralHost)
        );
        assert_eq!(
            parse_url("https://[::1]/", false),
            Err(UrlError::IpLiteralHost)
        );
    }

    #[test]
    fn rejects_plain_http_unless_explicitly_exempted() {
        assert_eq!(
            parse_url("http://allowed.com/", false),
            Err(UrlError::UnsupportedScheme)
        );
        // 内网 NAS 显式豁免口径：allow_plain_http = true 时放行
        let h = parse_url("http://allowed.com/", true).unwrap();
        assert_eq!(h.name, "allowed.com");
        // 其它 scheme 一律拒绝
        assert_eq!(
            parse_url("ftp://allowed.com/", true),
            Err(UrlError::UnsupportedScheme)
        );
        assert_eq!(parse_url("allowed.com/x", false), Err(UrlError::Malformed));
    }

    // —— P2 宿主侧三条：仲裁语义测试 ——

    #[test]
    fn dispatch_overrides_identity_and_purpose_with_host_values() {
        let cap = CaptureTransport(std::cell::RefCell::new(None));
        dispatch(
            &cap,
            &whitelist(),
            &ctx(),
            &intent("https://allowed.com/cover.jpg"),
        )
        .unwrap();
        let got = cap.0.borrow().clone().unwrap();
        // 身份与用途：宿主带外值覆盖插件自报值
        assert_eq!(got.plugin_id, "host-bound-id");
        assert_eq!(got.purpose, "manifest-purpose");
    }

    #[test]
    fn dispatch_clamps_limits_to_smaller_of_two() {
        let cap = CaptureTransport(std::cell::RefCell::new(None));
        // 插件自报值远大于宿主上限：钳制到上限
        let mut big = intent("https://allowed.com/");
        big.max_bytes = u64::MAX;
        big.timeout_ms = u32::MAX;
        dispatch(&cap, &whitelist(), &ctx(), &big).unwrap();
        let got = cap.0.borrow().clone().unwrap();
        assert_eq!(got.max_bytes, 4 * 1024 * 1024);
        assert_eq!(got.timeout_ms, 10_000);
        // 插件自报值比宿主上限更严格：取小 = 保留自报值
        let mut small = intent("https://allowed.com/");
        small.max_bytes = 1024;
        small.timeout_ms = 500;
        dispatch(&cap, &whitelist(), &ctx(), &small).unwrap();
        let got = cap.0.borrow().clone().unwrap();
        assert_eq!(got.max_bytes, 1024);
        assert_eq!(got.timeout_ms, 500);
    }

    #[test]
    fn dispatch_checks_whitelist_before_transport() {
        let list = whitelist();
        // 白名单外：直接拒绝，不触达传输层
        assert_eq!(
            dispatch(
                &OfflineTransport,
                &list,
                &ctx(),
                &intent("https://bad.com/")
            ),
            Err(NetError::DomainNotAllowed)
        );
        // 白名单内：进入传输层，离线传输层返回离线拒绝
        assert_eq!(
            dispatch(
                &OfflineTransport,
                &list,
                &ctx(),
                &intent("https://allowed.com/")
            ),
            Err(NetError::OfflineDenied)
        );
    }

    #[test]
    fn parse_url_never_panics_on_arbitrary_bytes() {
        // 模糊化烟雾测试：任意字节 URL 预检不得 panic（结果 Ok/Err 均可）。
        //（真 fuzz 用 cargo-fuzz + 消毒器；此处用确定性伪随机做「永不 panic」
        // 的轻量守护，进 cargo test 常规跑。）
        let mut seed: u64 = 0x5eed_2026_0927_0003;
        let mut rng = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for len in 0..64usize {
            for _ in 0..64 {
                let bytes: Vec<u8> = (0..len).map(|_| (rng() & 0xff) as u8).collect();
                let url = String::from_utf8_lossy(&bytes);
                let _ = parse_url(&url, rng() & 1 == 0);
                let _ = DomainWhitelist::new(vec![url.to_string()]);
            }
        }
    }
}
