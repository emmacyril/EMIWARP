//! EmiWarp — local-first configuration layer for a Warp client fork.
//!
//! This crate is the *only* place EmiWarp logic lives. Upstream never touches
//! `crates/emiwarp/`, so it carries zero merge-conflict risk across syncs. Every
//! integration with upstream code is a one-line call into this crate, applied by
//! `scripts/sync_and_build.sh` as a patch with an anchored-injection fallback.
//!
//! ## Scope
//!
//! EmiWarp removes the client's dependency on Warp-operated infrastructure and
//! points inference at a provider the user controls. Because nothing in this
//! build reaches Warp's servers, there is no account, tier, or subscription
//! concept to model — see [`identity`] and [`egress`].
//!
//! ## License
//!
//! Derived from warpdotdev/warp, AGPL-3.0-only (the `warpui`/`warpui_core`
//! crates are MIT). Distributing an EmiWarp binary obliges you to publish the
//! complete corresponding source under AGPL-3.0.

pub mod config;
pub mod discovery;
pub mod egress;
pub mod identity;
pub mod provider;

use std::sync::OnceLock;

pub use config::{Diagnostic, EmiConfig};
pub use discovery::{AuthMode, DiscoveredHarness, Inventory, LocalServer, LoginProbe};
pub use egress::{EgressDecision, classify as classify_egress};
pub use identity::{Capability, Entitlements, LocalPrincipal};
pub use provider::{ProviderKind, ProviderProfile, WireSchema};

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// Process-wide resolved EmiWarp state.
#[derive(Debug)]
pub struct Runtime {
    pub config: EmiConfig,
    pub entitlements: Entitlements,
    pub diagnostics: Vec<Diagnostic>,
}

/// Resolves EmiWarp state once per process.
///
/// Safe to call from any call site at any time, including before upstream's own
/// initialization — it touches only the environment and the filesystem.
pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        let (config, diagnostics) = EmiConfig::load();
        let entitlements = Entitlements::resolve(&config);
        Runtime {
            config,
            entitlements,
            diagnostics,
        }
    })
}

// ---------------------------------------------------------------------------
// Call-site shims.
//
// Each of these is what a patched upstream line calls. Keeping the bodies here
// means an upstream refactor can move the call site without changing behaviour,
// and a patch that fails to apply is a one-line fix rather than a re-derivation.
// ---------------------------------------------------------------------------

/// Backs the patched `AuthState::is_logged_in()`.
pub fn is_logged_in() -> bool {
    runtime().entitlements.is_logged_in()
}

/// Backs the patched onboarding branch in `app/src/root_view.rs`.
/// `true` boots straight to the workspace.
pub fn skip_onboarding() -> bool {
    runtime().config.skip_onboarding
}

/// Backs the patched egress check in `crates/http_client`.
///
/// There is no configuration that re-enables Warp-operated hosts. EmiWarp does
/// not use them, so a request to one is always a bug — either an upstream call
/// site the patch series has not yet caught, or a regression.
pub fn egress_allowed(url: &str) -> bool {
    classify_egress(url).is_allowed()
}

/// Capability gate for patched feature-flag call sites.
pub fn allows(cap: Capability) -> bool {
    runtime().entitlements.allows(cap)
}

/// Product name for UI chrome.
pub fn brand_name() -> &'static str {
    &runtime().config.brand_name
}

/// Active provider profile.
pub fn provider() -> &'static ProviderProfile {
    &runtime().config.provider
}

/// Scans for installed agent CLIs and running local model servers.
///
/// Not cached: the user may start Ollama or sign into a CLI while EmiWarp is
/// running, and a stale "not installed" is worse than re-probing.
pub fn discover() -> Inventory {
    discovery::discover()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;

/// Environment overlay applied to a spawned agent-CLI harness.
///
/// Injected at `app/src/ai/agent_sdk/driver.rs`, immediately before the resolved
/// environment is frozen. This is the whole provider-substitution mechanism:
/// every supported agent CLI reads its endpoint and credentials from the
/// environment, so repointing them needs no change to the CLI itself.
pub fn harness_env_overlay() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    provider()
        .harness_env()
        .into_iter()
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)))
        .collect()
}
