//! Network egress policy.
//!
//! Upstream's client reaches several Warp-operated hosts. EmiWarp classifies
//! every outbound host and blocks the vendor ones by default, so that a call
//! site we failed to patch fails closed rather than silently phoning home (or
//! silently spending an account's request budget).
//!
//! This is defence in depth for the sync workflow: when upstream adds a new
//! endpoint, the patch series may not know about it, but the host check does.

/// Hosts operated by Warp. Suffix-matched against the request authority.
const VENDOR_HOST_SUFFIXES: &[&str] = &[
    "warp.dev",
    "app.warp.dev",
    "oz.warp.dev",
    "rtc.app.warp.dev",
    "sessions.app.warp.dev",
    "releases.warp.dev",
    "dataplane.rudderstack.com",
];

/// Classification of an outbound request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressDecision {
    /// Not a Warp-operated host — the user's own provider, an MCP server, etc.
    Allow,
    /// A Warp-operated host. Blocked in local-only mode.
    DenyVendorHost,
}

impl EgressDecision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Classifies a URL. Unparseable input is allowed through unchanged so that we
/// never break a request shape we did not anticipate — the host allowlist is a
/// backstop, not the primary mechanism.
pub fn classify(url: &str) -> EgressDecision {
    match host_of(url) {
        Some(host) if is_vendor_host(&host) => EgressDecision::DenyVendorHost,
        _ => EgressDecision::Allow,
    }
}

/// `true` when `host` is (or is a subdomain of) a Warp-operated host.
pub fn is_vendor_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    VENDOR_HOST_SUFFIXES.iter().any(|suffix| {
        host == *suffix || host.ends_with(&format!(".{suffix}"))
    })
}

/// Extracts the host from a URL without pulling in a URL parser.
fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|s| !s.is_empty())?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // Strip a port, taking care not to mangle a bracketed IPv6 literal.
    let host = if let Some(end) = authority.find(']') {
        &authority[..=end]
    } else {
        authority.split_once(':').map_or(authority, |(h, _)| h)
    };
    Some(host.to_owned())
}
