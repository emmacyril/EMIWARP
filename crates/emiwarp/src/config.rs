//! Loader for `.env.emiwarp`.
//!
//! Resolution order (first hit wins, per key):
//!   1. process environment  (`EMIWARP_*`)
//!   2. `$EMIWARP_CONFIG` if set
//!   3. `./.env.emiwarp`  (repo-local, for development)
//!   4. `$XDG_CONFIG_HOME/emiwarp/.env.emiwarp` (or `~/.config/emiwarp/...`)
//!
//! Parsing is deliberately minimal — `KEY=value`, `#` comments, optional
//! surrounding quotes. We do not depend on `dotenvy` so that this crate stays
//! dependency-thin and cannot be perturbed by an upstream dependency bump.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::provider::{ProviderKind, ProviderProfile};

/// Everything EmiWarp needs to know that Warp would otherwise ask its servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmiConfig {
    /// The active inference provider.
    pub provider: ProviderProfile,
    /// Skip the login/onboarding splash and boot straight into the workspace.
    pub skip_onboarding: bool,
    /// Product name rendered in the UI chrome.
    pub brand_name: String,
    /// Raw key/value pairs, retained so callers can read forward-compatible keys
    /// without a crate change.
    pub(crate) raw: HashMap<String, String>,
}

impl Default for EmiConfig {
    fn default() -> Self {
        Self {
            provider: ProviderProfile::ollama_default(),
            skip_onboarding: true,
            brand_name: "EmiWarp".to_owned(),
            raw: HashMap::new(),
        }
    }
}

impl EmiConfig {
    /// Loads configuration, falling back to defaults for anything unset.
    ///
    /// This never fails: a malformed or missing config yields defaults plus a
    /// list of diagnostics, because refusing to boot a terminal emulator over a
    /// typo'd env file is a worse failure than running with defaults.
    pub fn load() -> (Self, Vec<Diagnostic>) {
        let mut diags = Vec::new();
        let mut raw = HashMap::new();

        for path in candidate_paths() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    for (lineno, line) in text.lines().enumerate() {
                        match parse_line(line) {
                            Ok(Some((k, v))) => {
                                raw.entry(k).or_insert(v);
                            }
                            Ok(None) => {}
                            Err(msg) => diags.push(Diagnostic::Malformed {
                                path: path.clone(),
                                line: lineno + 1,
                                msg,
                            }),
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => diags.push(Diagnostic::Unreadable {
                    path: path.clone(),
                    msg: e.to_string(),
                }),
            }
        }

        // Process env wins over every file.
        for key in KNOWN_KEYS {
            if let Ok(v) = std::env::var(key) {
                raw.insert((*key).to_owned(), v);
            }
        }

        let get = |k: &str| raw.get(k).map(String::as_str).filter(|s| !s.is_empty());

        let kind = match get("EMIWARP_AI_PROVIDER") {
            Some(s) => match ProviderKind::parse(s) {
                Some(k) => k,
                None => {
                    diags.push(Diagnostic::UnknownProvider(s.to_owned()));
                    ProviderKind::Ollama
                }
            },
            None => ProviderKind::Ollama,
        };

        let provider = ProviderProfile {
            kind,
            base_url: get("EMIWARP_BASE_URL")
                .map(str::to_owned)
                .unwrap_or_else(|| kind.default_base_url().to_owned()),
            api_key: get("EMIWARP_API_KEY").map(str::to_owned),
            model: get("EMIWARP_MODEL_NAME")
                .map(str::to_owned)
                .unwrap_or_else(|| kind.default_model().to_owned()),
            harness_command: get("EMIWARP_HARNESS_CMD").map(str::to_owned),
        };

        if provider.kind.requires_api_key() && provider.api_key.is_none() {
            diags.push(Diagnostic::MissingApiKey(provider.kind));
        }

        let cfg = Self {
            skip_onboarding: flag(&raw, "EMIWARP_SKIP_ONBOARDING", true),
            brand_name: get("EMIWARP_BRAND_NAME")
                .unwrap_or("EmiWarp")
                .to_owned(),
            provider,
            raw,
        };
        (cfg, diags)
    }

    /// Reads an arbitrary key, for forward compatibility with keys added to the
    /// template before they are added to this struct.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.raw.get(key).map(String::as_str)
    }
}

/// Non-fatal problems found while loading config. Surfaced in the UI's log pane
/// rather than swallowed, so a mistyped key is visible instead of mysterious.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diagnostic {
    Malformed {
        path: PathBuf,
        line: usize,
        msg: String,
    },
    Unreadable {
        path: PathBuf,
        msg: String,
    },
    UnknownProvider(String),
    MissingApiKey(ProviderKind),
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed { path, line, msg } => {
                write!(f, "{}:{line}: {msg}", path.display())
            }
            Self::Unreadable { path, msg } => write!(f, "{}: {msg}", path.display()),
            Self::UnknownProvider(s) => {
                write!(f, "unknown EMIWARP_AI_PROVIDER `{s}`; falling back to ollama")
            }
            Self::MissingApiKey(k) => {
                write!(f, "provider `{}` needs EMIWARP_API_KEY", k.slug())
            }
        }
    }
}

const KNOWN_KEYS: &[&str] = &[
    "EMIWARP_AI_PROVIDER",
    "EMIWARP_API_KEY",
    "EMIWARP_BASE_URL",
    "EMIWARP_MODEL_NAME",
    "EMIWARP_HARNESS_CMD",
    "EMIWARP_SKIP_ONBOARDING",
    "EMIWARP_BRAND_NAME",
    "EMIWARP_CONFIG",
];

fn flag(raw: &HashMap<String, String>, key: &str, default: bool) -> bool {
    match raw.get(key).map(|s| s.trim().to_ascii_lowercase()) {
        Some(v) => matches!(v.as_str(), "1" | "true" | "yes" | "on"),
        None => default,
    }
}

fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(explicit) = std::env::var("EMIWARP_CONFIG") {
        if !explicit.is_empty() {
            out.push(PathBuf::from(explicit));
        }
    }
    out.push(PathBuf::from(".env.emiwarp"));
    if let Some(dir) = config_dir() {
        out.push(dir.join("emiwarp").join(".env.emiwarp"));
    }
    out
}

fn config_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| Path::new(&h).join(".config"))
}

/// `KEY=value` with `#` comments and optional matching quotes.
fn parse_line(line: &str) -> Result<Option<(String, String)>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
    let Some((key, value)) = line.split_once('=') else {
        return Err("expected KEY=value".to_owned());
    };
    let key = key.trim();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(format!("invalid key `{key}`"));
    }
    Ok(Some((key.to_owned(), unquote(value.trim()))))
}

fn unquote(v: &str) -> String {
    let bytes = v.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return v[1..v.len() - 1].to_owned();
        }
    }
    // Unquoted values stop at an inline comment.
    match v.split_once(" #") {
        Some((head, _)) => head.trim_end().to_owned(),
        None => v.to_owned(),
    }
}

#[cfg(test)]
pub(crate) fn parse_line_for_test(line: &str) -> Result<Option<(String, String)>, String> {
    parse_line(line)
}
