//! Local identity and capability resolution.
//!
//! EmiWarp has no account system, no tier, and no subscription concept. Upstream
//! Warp needs those because its agent loop runs on Warp-operated servers that
//! bill per request. EmiWarp does not use those servers at all — inference runs
//! against an endpoint the user configures and pays for directly — so the entire
//! notion of an entitlement has nothing left to describe.
//!
//! There is deliberately no `is_premium`, `has_active_subscription`, or plan
//! enum anywhere in this crate. Those would be vestigial: a value nothing reads,
//! describing a relationship that does not exist. The capabilities below are all
//! local, and each is gated only on whether it can physically work.

use crate::config::EmiConfig;

/// Who the client is when there is no account server to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPrincipal {
    pub display_name: String,
    pub email: Option<String>,
    /// Stable per-install id. Local only — never transmitted anywhere.
    pub install_id: String,
}

impl LocalPrincipal {
    pub fn resolve(cfg: &EmiConfig) -> Self {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "local".to_owned());
        Self {
            display_name: cfg.get("EMIWARP_USER_NAME").unwrap_or(&user).to_owned(),
            email: cfg.get("EMIWARP_USER_EMAIL").map(str::to_owned),
            install_id: install_id(),
        }
    }
}

/// Local capabilities the UI checks before rendering or dispatching.
///
/// Every variant is satisfiable on the user's own machine with the user's own
/// credentials. Nothing here corresponds to a purchasable Warp feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Render the terminal workspace.
    Workspace,
    /// Show the AI panel and model picker.
    AiPanel,
    /// Run an agent through a locally-spawned CLI harness.
    LocalAgent,
}

/// The resolved local capability set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entitlements {
    principal: LocalPrincipal,
    ai_configured: bool,
}

impl Entitlements {
    pub fn resolve(cfg: &EmiConfig) -> Self {
        let p = &cfg.provider;
        Self {
            principal: LocalPrincipal::resolve(cfg),
            ai_configured: !p.kind.requires_api_key() || p.api_key.is_some(),
        }
    }

    pub fn principal(&self) -> &LocalPrincipal {
        &self.principal
    }

    pub fn allows(&self, cap: Capability) -> bool {
        match cap {
            // Unconditional: these are local rendering concerns.
            Capability::Workspace | Capability::AiPanel => true,
            // Gated only on whether a provider is actually reachable — a missing
            // API key is a configuration problem, not a permission one.
            Capability::LocalAgent => self.ai_configured,
        }
    }

    /// What upstream's render gate should see.
    ///
    /// Upstream asks `is_logged_in()` to decide whether to show the workspace or
    /// the login splash. EmiWarp always has a usable local identity, so the
    /// workspace always renders.
    pub fn is_logged_in(&self) -> bool {
        true
    }
}

fn install_id() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
        .hash(&mut h);
    format!("emi-{:016x}", h.finish())
}
