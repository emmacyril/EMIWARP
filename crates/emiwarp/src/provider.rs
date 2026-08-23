//! Provider profiles and wire-schema mapping.
//!
//! EmiWarp does not implement an HTTP client here. Upstream Warp reaches models
//! two ways, and only one of them is ours to use:
//!
//!   * the **cloud path** — the client `POST`s to Warp's server, which runs the
//!     agent loop and bills the account. EmiWarp never uses this.
//!   * the **local harness path** — Warp spawns a local agent CLI as a child
//!     process (`app/src/ai/agent_sdk/driver/harness/`). Credentials stay on the
//!     machine and nothing is metered.
//!
//! EmiWarp is built on the second. A `ProviderProfile` therefore resolves to a
//! *process invocation plus environment*, not a URL we call ourselves.

use std::collections::BTreeMap;

/// Inference backends EmiWarp knows how to launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Ollama,
    OpenAI,
    Anthropic,
    Gemini,
    /// OpenRouter — one key, many upstream models.
    OpenRouter,
    /// Moonshot AI (Kimi). Serves both an OpenAI- and an Anthropic-shaped API;
    /// EmiWarp drives the Anthropic one so the `claude` CLI works unmodified.
    Kimi,
    /// Zhipu / Z.ai (GLM). Same dual-API story as Kimi.
    Glm,
    /// Any OpenAI-Chat-Completions-compatible server (vLLM, LM Studio,
    /// llama.cpp, LiteLLM, Together, Groq...).
    OpenAiCompatible,
}

impl ProviderKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ollama" => Some(Self::Ollama),
            "openai" => Some(Self::OpenAI),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "gemini" | "google" => Some(Self::Gemini),
            "openrouter" | "open_router" => Some(Self::OpenRouter),
            "kimi" | "moonshot" => Some(Self::Kimi),
            "glm" | "zhipu" | "zai" | "z.ai" => Some(Self::Glm),
            "openai_compatible" | "openai-compatible" | "compatible" | "custom" => {
                Some(Self::OpenAiCompatible)
            }
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
            Self::OpenRouter => "openrouter",
            Self::Kimi => "kimi",
            Self::Glm => "glm",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::Ollama => "http://127.0.0.1:11434/v1",
            Self::OpenAI => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
            Self::Gemini => "https://generativelanguage.googleapis.com/v1beta",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            // Anthropic-shaped endpoints, so the `claude` CLI drives them as-is.
            Self::Kimi => "https://api.moonshot.ai/anthropic",
            Self::Glm => "https://api.z.ai/api/anthropic",
            Self::OpenAiCompatible => "http://127.0.0.1:8000/v1",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::Ollama => "llama3.1:8b",
            Self::OpenAI => "gpt-4o",
            Self::Anthropic => "claude-sonnet-5",
            Self::Gemini => "gemini-2.0-flash",
            Self::OpenRouter => "anthropic/claude-sonnet-5",
            Self::Kimi => "kimi-k2-turbo-preview",
            Self::Glm => "glm-4.6",
            Self::OpenAiCompatible => "default",
        }
    }

    /// Local servers accept an empty key; hosted APIs do not.
    pub fn requires_api_key(self) -> bool {
        match self {
            Self::Ollama | Self::OpenAiCompatible => false,
            Self::OpenAI
            | Self::Anthropic
            | Self::Gemini
            | Self::OpenRouter
            | Self::Kimi
            | Self::Glm => true,
        }
    }

    /// The request/response shape this provider speaks. Mirrors upstream's
    /// `ai::api_keys::CustomEndpointSchema` so profiles round-trip into Warp's
    /// own custom-endpoint UI without a translation layer.
    pub fn schema(self) -> WireSchema {
        match self {
            // Kimi and GLM both publish an Anthropic-compatible surface; using
            // it means the `claude` CLI needs no patching to talk to them.
            Self::Anthropic | Self::Kimi | Self::Glm => WireSchema::AnthropicMessages,
            Self::Gemini => WireSchema::GeminiGenerateContent,
            Self::Ollama | Self::OpenAI | Self::OpenRouter | Self::OpenAiCompatible => {
                WireSchema::OpenAiChatCompletions
            }
        }
    }

    /// Default local CLI that drives this provider as a child-process harness.
    /// Overridable via `EMIWARP_HARNESS_CMD`.
    pub fn default_harness_command(self) -> &'static str {
        match self {
            Self::Anthropic | Self::Kimi | Self::Glm => "claude",
            Self::OpenAI | Self::OpenRouter => "codex",
            Self::Gemini => "gemini",
            // Both speak OpenAI Chat Completions, so the OpenAI CLI drives them
            // once `OPENAI_BASE_URL` is repointed.
            Self::Ollama | Self::OpenAiCompatible => "codex",
        }
    }
}

/// Payload schema spoken on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSchema {
    OpenAiChatCompletions,
    AnthropicMessages,
    GeminiGenerateContent,
}

impl WireSchema {
    /// Upstream's `CustomEndpointSchema` display name, for interop with Warp's
    /// settings UI. Gemini has no upstream variant and degrades to the
    /// OpenAI-compatible shape, which Google's `/openai` endpoint accepts.
    pub fn upstream_display_name(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions | Self::GeminiGenerateContent => {
                "OpenAI Chat Completions"
            }
            Self::AnthropicMessages => "Anthropic Messages",
        }
    }
}

/// A fully resolved provider: what to launch, where to point it, how to auth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderProfile {
    pub kind: ProviderKind,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: String,
    /// Overrides `ProviderKind::default_harness_command`.
    pub harness_command: Option<String>,
}

impl ProviderProfile {
    pub fn ollama_default() -> Self {
        Self {
            kind: ProviderKind::Ollama,
            base_url: ProviderKind::Ollama.default_base_url().to_owned(),
            api_key: None,
            model: ProviderKind::Ollama.default_model().to_owned(),
            harness_command: None,
        }
    }

    pub fn command(&self) -> &str {
        self.harness_command
            .as_deref()
            .unwrap_or_else(|| self.kind.default_harness_command())
    }

    /// Environment overlay applied to the spawned harness process.
    ///
    /// This is the whole substitution mechanism: every supported agent CLI reads
    /// its endpoint and credentials from env, so repointing them at Ollama (or
    /// anything else) needs no patch to the CLI and no HTTP client of our own.
    pub fn harness_env(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        let key = self.api_key.clone().unwrap_or_else(|| "sk-noauth".to_owned());

        match self.kind {
            ProviderKind::Anthropic | ProviderKind::Kimi | ProviderKind::Glm => {
                env.insert("ANTHROPIC_API_KEY".into(), key.clone());
                // Some builds of the CLI read the auth token rather than the
                // key when a custom base URL is set; provide both.
                env.insert("ANTHROPIC_AUTH_TOKEN".into(), key);
                env.insert("ANTHROPIC_BASE_URL".into(), self.base_url.clone());
                env.insert("ANTHROPIC_MODEL".into(), self.model.clone());
            }
            ProviderKind::Gemini => {
                env.insert("GEMINI_API_KEY".into(), key);
                env.insert("GOOGLE_GEMINI_BASE_URL".into(), self.base_url.clone());
                env.insert("GEMINI_MODEL".into(), self.model.clone());
            }
            ProviderKind::OpenAI
            | ProviderKind::OpenRouter
            | ProviderKind::Ollama
            | ProviderKind::OpenAiCompatible => {
                env.insert("OPENAI_API_KEY".into(), key);
                env.insert("OPENAI_BASE_URL".into(), self.base_url.clone());
                env.insert("OPENAI_MODEL".into(), self.model.clone());
            }
        }
        env.insert("EMIWARP_ACTIVE_PROVIDER".into(), self.kind.slug().into());
        env.insert("EMIWARP_ACTIVE_MODEL".into(), self.model.clone());
        env
    }

    /// Chat-completions URL, for callers that want to probe reachability.
    pub fn chat_endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.kind.schema() {
            WireSchema::OpenAiChatCompletions => format!("{base}/chat/completions"),
            WireSchema::AnthropicMessages => format!("{base}/v1/messages"),
            WireSchema::GeminiGenerateContent => {
                format!("{base}/models/{}:generateContent", self.model)
            }
        }
    }
}
