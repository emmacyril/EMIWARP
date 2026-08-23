//! Discovery of already-installed agent CLIs and local model servers.
//!
//! # Ambient auth
//!
//! The important idea, borrowed from how tools like Traycer wire this up: for a
//! provider you reach through its own CLI, **EmiWarp stores no credential at
//! all**. If `claude` is installed and logged in, spawning it inherits that
//! session — including whatever subscription backs it. The subscription belongs
//! to the CLI and to the user, not to EmiWarp, and no token passes through here.
//!
//! Providers with no CLI of their own ([`AuthMode::ApiKey`]) need a key from
//! `.env.emiwarp`. Local servers ([`AuthMode::None`]) need nothing.
//!
//! Discovery is best-effort and never fails: an undetected provider is reported
//! as not installed, never as an error.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::provider::ProviderKind;

const PROBE_TIMEOUT: Duration = Duration::from_millis(400);

/// How a provider proves who it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// The CLI carries its own login. EmiWarp stores nothing and inherits the
    /// session by spawning it.
    Ambient,
    /// Needs an API key from configuration.
    ApiKey,
    /// No authentication (local server).
    None,
}

impl AuthMode {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Ambient => "signs in through its own CLI; EmiWarp stores no credential",
            Self::ApiKey => "needs EMIWARP_API_KEY",
            Self::None => "no authentication",
        }
    }
}

/// How to detect that a CLI has been signed in.
///
/// Every variant checks only for *existence*. EmiWarp never reads credential
/// material — the whole point of ambient auth is that the secret stays with the
/// CLI that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginProbe {
    /// A file under `$HOME`.
    HomeFile(&'static str),
    /// A macOS Keychain generic-password item, falling back to a `$HOME` file on
    /// other platforms. Claude Code stores its token in the Keychain on macOS,
    /// so a file check alone reports a signed-in user as signed out.
    KeychainOrFile {
        service: &'static str,
        file: &'static str,
    },
    /// Presence of the executable is taken as sufficient.
    Installed,
}

impl LoginProbe {
    fn satisfied(self, installed: bool) -> bool {
        match self {
            Self::Installed => installed,
            Self::HomeFile(rel) => home_has(rel),
            Self::KeychainOrFile { service, file } => {
                if cfg!(target_os = "macos") && macos_keychain_has(service) {
                    return true;
                }
                home_has(file)
            }
        }
    }
}

fn home_has(rel: &str) -> bool {
    home().map(|h| h.join(rel).exists()).unwrap_or(false)
}

/// Existence check only — deliberately no `-w`, so no secret is ever read.
#[cfg(target_os = "macos")]
fn macos_keychain_has(service: &str) -> bool {
    Command::new("security")
        .args(["find-generic-password", "-s", service])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn macos_keychain_has(_service: &str) -> bool {
    false
}

/// A CLI harness EmiWarp knows how to drive.
#[derive(Debug, Clone, Copy)]
pub struct HarnessSpec {
    /// Stable id used in config and reports.
    pub id: &'static str,
    /// Executable name looked up on `PATH`.
    pub command: &'static str,
    /// Human-facing name.
    pub label: &'static str,
    pub auth: AuthMode,
    /// How to tell whether the user has completed a login.
    pub login_probe: LoginProbe,
    /// Providers reachable through this harness.
    pub providers: &'static [ProviderKind],
}

/// Harnesses EmiWarp can drive, in preference order.
pub const HARNESSES: &[HarnessSpec] = &[
    HarnessSpec {
        id: "claude-code",
        command: "claude",
        label: "Claude Code",
        auth: AuthMode::Ambient,
        login_probe: LoginProbe::KeychainOrFile {
            service: "Claude Code-credentials",
            file: ".claude/.credentials.json",
        },
        // Kimi and GLM publish Anthropic-shaped APIs, so this CLI drives them
        // too once its base URL is repointed.
        providers: &[ProviderKind::Anthropic, ProviderKind::Kimi, ProviderKind::Glm],
    },
    HarnessSpec {
        id: "codex",
        command: "codex",
        label: "Codex",
        auth: AuthMode::Ambient,
        login_probe: LoginProbe::HomeFile(".codex/auth.json"),
        providers: &[
            ProviderKind::OpenAI,
            ProviderKind::OpenRouter,
            ProviderKind::Ollama,
            ProviderKind::OpenAiCompatible,
        ],
    },
    HarnessSpec {
        id: "gemini",
        command: "gemini",
        label: "Gemini CLI",
        auth: AuthMode::Ambient,
        login_probe: LoginProbe::HomeFile(".gemini/oauth_creds.json"),
        providers: &[ProviderKind::Gemini],
    },
    HarnessSpec {
        id: "opencode",
        command: "opencode",
        label: "OpenCode",
        auth: AuthMode::Ambient,
        login_probe: LoginProbe::HomeFile(".local/share/opencode/auth.json"),
        providers: &[ProviderKind::OpenRouter, ProviderKind::OpenAiCompatible],
    },
    HarnessSpec {
        id: "qwen",
        command: "qwen",
        label: "Qwen Code",
        auth: AuthMode::Ambient,
        login_probe: LoginProbe::HomeFile(".qwen/oauth_creds.json"),
        providers: &[ProviderKind::OpenAiCompatible],
    },
];

/// A harness as found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredHarness {
    pub id: String,
    pub label: String,
    pub command: String,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub signed_in: bool,
    pub auth: AuthMode,
    pub providers: Vec<ProviderKind>,
}

impl DiscoveredHarness {
    /// Ready to run a turn right now with no further configuration.
    pub fn usable(&self) -> bool {
        self.path.is_some() && (self.auth != AuthMode::Ambient || self.signed_in)
    }
}

/// A local inference server found listening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServer {
    pub label: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Local servers worth probing: (label, host, port, base-url suffix).
const LOCAL_SERVERS: &[(&str, &str, u16, &str)] = &[
    ("Ollama", "127.0.0.1", 11434, "/v1"),
    ("LM Studio", "127.0.0.1", 1234, "/v1"),
    ("llama.cpp", "127.0.0.1", 8080, "/v1"),
    ("vLLM", "127.0.0.1", 8000, "/v1"),
];

/// Everything found on this machine.
#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub harnesses: Vec<DiscoveredHarness>,
    pub servers: Vec<LocalServer>,
}

impl Inventory {
    /// Harnesses that could run a turn immediately.
    pub fn usable_harnesses(&self) -> impl Iterator<Item = &DiscoveredHarness> {
        self.harnesses.iter().filter(|h| h.usable())
    }

    /// A provider that would work right now with no configuration, if any.
    ///
    /// A running local server wins over a signed-in CLI: it costs nothing to
    /// call and needs no account.
    pub fn suggested_provider(&self) -> Option<ProviderKind> {
        if let Some(server) = self.servers.first() {
            return Some(server.kind);
        }
        self.usable_harnesses()
            .find_map(|h| h.providers.first().copied())
    }

    /// Human-readable summary for the settings UI or `--doctor` output.
    pub fn report(&self) -> String {
        let mut out = String::new();
        out.push_str("Agent CLIs\n");
        if self.harnesses.is_empty() {
            out.push_str("  (none found on PATH)\n");
        }
        for h in &self.harnesses {
            let state = match (h.path.is_some(), h.signed_in) {
                (false, _) => "not installed".to_owned(),
                (true, true) => format!(
                    "ready{}",
                    h.version
                        .as_deref()
                        .map(|v| format!(" ({v})"))
                        .unwrap_or_default()
                ),
                (true, false) => "installed, not signed in".to_owned(),
            };
            out.push_str(&format!("  {:<14} {}\n", h.label, state));
        }

        out.push_str("\nLocal servers\n");
        if self.servers.is_empty() {
            out.push_str("  (none listening)\n");
        }
        for s in &self.servers {
            out.push_str(&format!("  {:<14} {}", s.label, s.base_url));
            if !s.models.is_empty() {
                out.push_str(&format!("  [{}]", s.models.join(", ")));
            }
            out.push('\n');
        }
        out
    }
}

/// Scans `PATH` and localhost. Cheap enough to call at startup: each probe is
/// bounded by a short timeout and they are all independent.
pub fn discover() -> Inventory {
    Inventory {
        harnesses: HARNESSES.iter().map(discover_harness).collect(),
        servers: LOCAL_SERVERS
            .iter()
            .filter_map(|&(label, host, port, suffix)| probe_server(label, host, port, suffix))
            .collect(),
    }
}

fn discover_harness(spec: &HarnessSpec) -> DiscoveredHarness {
    let path = which(spec.command);
    DiscoveredHarness {
        id: spec.id.to_owned(),
        label: spec.label.to_owned(),
        command: spec.command.to_owned(),
        version: path.as_ref().and_then(|_| cli_version(spec.command)),
        signed_in: spec.login_probe.satisfied(path.is_some()),
        path,
        auth: spec.auth,
        providers: spec.providers.to_vec(),
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Resolves an executable on `PATH` without shelling out.
fn which(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{command}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Best-effort `--version`. Returns `None` rather than blocking if the CLI
/// misbehaves — a version string is decoration, not a gate.
fn cli_version(command: &str) -> Option<String> {
    let out = Command::new(command).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| l.chars().take(40).collect())
}

fn probe_server(label: &str, host: &str, port: u16, suffix: &str) -> Option<LocalServer> {
    let addr: SocketAddr = (host, port).to_socket_addrs().ok()?.next()?;
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).ok()?;

    let kind = if port == 11434 {
        ProviderKind::Ollama
    } else {
        ProviderKind::OpenAiCompatible
    };

    Some(LocalServer {
        label: label.to_owned(),
        kind,
        base_url: format!("http://{host}:{port}{suffix}"),
        models: if kind == ProviderKind::Ollama {
            ollama_models(addr).unwrap_or_default()
        } else {
            Vec::new()
        },
    })
}

/// Lists locally-installed Ollama models.
///
/// A hand-rolled HTTP/1.0 GET rather than a dependency: this crate stays
/// dependency-thin on purpose so an upstream version bump can never perturb it,
/// and the request is one fixed line to loopback.
fn ollama_models(addr: SocketAddr) -> Option<Vec<String>> {
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(PROBE_TIMEOUT)).ok()?;
    stream
        .write_all(b"GET /api/tags HTTP/1.0\r\nHost: localhost\r\nAccept: application/json\r\n\r\n")
        .ok()?;

    let mut body = Vec::new();
    // Cap the read: a malformed or unexpectedly large response must not be able
    // to stall startup or balloon memory.
    stream.take(256 * 1024).read_to_end(&mut body).ok()?;
    let text = String::from_utf8_lossy(&body);
    let json = text.split_once("\r\n\r\n").map(|(_, b)| b)?;

    let mut names: Vec<String> = json
        .match_indices("\"name\":")
        .filter_map(|(i, _)| {
            let rest = &json[i + 7..];
            let start = rest.find('"')? + 1;
            let end = rest[start..].find('"')? + start;
            Some(rest[start..end].to_owned())
        })
        .collect();
    names.sort();
    names.dedup();
    names.truncate(12);
    Some(names)
}
