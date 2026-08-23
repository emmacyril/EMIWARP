use super::*;
use crate::config::EmiConfig;
use crate::identity::{Capability, Entitlements};
use crate::provider::{ProviderKind, ProviderProfile, WireSchema};

fn cfg_with(kind: ProviderKind, api_key: Option<&str>) -> EmiConfig {
    EmiConfig {
        provider: ProviderProfile {
            kind,
            base_url: kind.default_base_url().to_owned(),
            api_key: api_key.map(str::to_owned),
            model: kind.default_model().to_owned(),
            harness_command: None,
        },
        ..EmiConfig::default()
    }
}

#[test]
fn vendor_hosts_are_denied_and_others_allowed() {
    for url in [
        "https://app.warp.dev/graphql/v2",
        "https://oz.warp.dev/api/run",
        "wss://rtc.app.warp.dev/graphql/v2",
        "https://releases.warp.dev/channel_versions.json",
        "https://dataplane.rudderstack.com/v1/batch",
    ] {
        assert_eq!(
            egress::classify(url),
            egress::EgressDecision::DenyVendorHost,
            "expected {url} to be denied"
        );
    }

    for url in [
        "http://127.0.0.1:11434/v1/chat/completions",
        "https://api.openai.com/v1/chat/completions",
        "https://api.anthropic.com/v1/messages",
        "http://[::1]:8000/v1/chat/completions",
    ] {
        assert_eq!(
            egress::classify(url),
            egress::EgressDecision::Allow,
            "expected {url} to be allowed"
        );
    }
}

#[test]
fn lookalike_hosts_are_not_treated_as_vendor() {
    // Suffix matching must be label-aware: `notwarp.dev` is a different domain.
    assert_eq!(
        egress::classify("https://notwarp.dev/x"),
        egress::EgressDecision::Allow
    );
    // ...but a real subdomain is.
    assert_eq!(
        egress::classify("https://cdn.app.warp.dev/x"),
        egress::EgressDecision::DenyVendorHost
    );
}

#[test]
fn no_warp_host_is_reachable_under_any_configuration() {
    // EmiWarp exposes no setting that re-enables Warp-operated infrastructure.
    // This test is the executable form of the project's central constraint:
    // every vendor host stays blocked regardless of how the build is configured.
    for url in [
        "https://app.warp.dev/graphql/v2",
        "https://oz.warp.dev/api/run",
        "https://dataplane.rudderstack.com/v1/batch",
    ] {
        assert!(!crate::egress_allowed(url), "{url} must never be reachable");
    }
    assert!(crate::egress_allowed("http://127.0.0.1:11434/v1/chat/completions"));
}

#[test]
fn local_capabilities_track_provider_configuration() {
    let unconfigured = Entitlements::resolve(&cfg_with(ProviderKind::OpenAI, None));
    assert!(unconfigured.allows(Capability::Workspace));
    assert!(unconfigured.allows(Capability::AiPanel));
    assert!(
        !unconfigured.allows(Capability::LocalAgent),
        "hosted provider without a key cannot run an agent"
    );

    let configured = Entitlements::resolve(&cfg_with(ProviderKind::OpenAI, Some("sk-test")));
    assert!(configured.allows(Capability::LocalAgent));

    // Ollama needs no key.
    let ollama = Entitlements::resolve(&cfg_with(ProviderKind::Ollama, None));
    assert!(ollama.allows(Capability::LocalAgent));
}

#[test]
fn provider_env_overlay_repoints_each_cli() {
    let anthropic = cfg_with(ProviderKind::Anthropic, Some("sk-ant-x")).provider;
    let env = anthropic.harness_env();
    assert_eq!(env["ANTHROPIC_API_KEY"], "sk-ant-x");
    assert_eq!(env["ANTHROPIC_BASE_URL"], "https://api.anthropic.com");
    assert_eq!(anthropic.command(), "claude");

    // Ollama drives the OpenAI-compatible CLI with a placeholder key, because
    // the CLI requires the variable to be set even when the server ignores it.
    let ollama = ProviderProfile::ollama_default();
    let env = ollama.harness_env();
    assert_eq!(env["OPENAI_BASE_URL"], "http://127.0.0.1:11434/v1");
    assert_eq!(env["OPENAI_API_KEY"], "sk-noauth");
    assert_eq!(ollama.command(), "codex");
}

#[test]
fn chat_endpoints_match_each_wire_schema() {
    let ollama = ProviderProfile::ollama_default();
    assert_eq!(
        ollama.chat_endpoint(),
        "http://127.0.0.1:11434/v1/chat/completions"
    );

    let anthropic = cfg_with(ProviderKind::Anthropic, Some("k")).provider;
    assert_eq!(anthropic.chat_endpoint(), "https://api.anthropic.com/v1/messages");
    assert_eq!(anthropic.kind.schema(), WireSchema::AnthropicMessages);
}

#[test]
fn provider_slugs_round_trip() {
    for kind in [
        ProviderKind::Ollama,
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::Gemini,
        ProviderKind::OpenAiCompatible,
    ] {
        assert_eq!(ProviderKind::parse(kind.slug()), Some(kind));
    }
    assert_eq!(ProviderKind::parse("claude"), Some(ProviderKind::Anthropic));
    assert_eq!(ProviderKind::parse("nope"), None);
}

#[test]
fn config_parser_handles_comments_quotes_and_exports() {
    use crate::config::parse_line_for_test as parse;
    assert_eq!(parse("# comment").unwrap(), None);
    assert_eq!(parse("").unwrap(), None);
    assert_eq!(
        parse("EMIWARP_MODEL_NAME=llama3.1:8b").unwrap(),
        Some(("EMIWARP_MODEL_NAME".into(), "llama3.1:8b".into()))
    );
    assert_eq!(
        parse("export EMIWARP_API_KEY=\"sk-with space\"").unwrap(),
        Some(("EMIWARP_API_KEY".into(), "sk-with space".into()))
    );
    assert_eq!(
        parse("EMIWARP_BASE_URL=http://x:1 # trailing").unwrap(),
        Some(("EMIWARP_BASE_URL".into(), "http://x:1".into()))
    );
    assert!(parse("not a kv pair").is_err());
    assert!(parse("BAD-KEY=1").is_err());
}
