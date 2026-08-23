//! A local implementation of Warp's `/ai/multi-agent` endpoint.
//!
//! # Why this exists
//!
//! Warp Agent is server-bound by design: the client POSTs a protobuf `Request`
//! to `{server_root_url}/ai/multi-agent` and Warp's backend runs the agent loop
//! and calls the model — even when the user supplies their own API keys, those
//! keys are forwarded to Warp rather than used locally. With no account the
//! request fails with "missing authentication credentials", which is why
//! ungating the UI alone left the agent dead.
//!
//! So EmiWarp serves that endpoint itself. `ChannelState` points
//! `server_root_url` at this server on loopback, and the client talks to it
//! exactly as it would to Warp — same protobuf, same SSE framing — while the
//! request is answered by whatever provider the user configured.
//!
//! The protocol types come from upstream's own `warp_multi_agent_api` crate, so
//! this speaks the real contract rather than an approximation of it.
//!
//! ## Wire format
//!
//! `POST /ai/multi-agent` with a protobuf `Request` body, answered with
//! Server-Sent Events whose `data:` field is base64url(protobuf `ResponseEvent`).

use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use axum::{Router, body::Bytes};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE as BASE64_URL_SAFE;
use futures::stream::{self, StreamExt};
use prost::Message as _;

use warp_multi_agent_api as api;

use crate::provider::{ProviderProfile, WireSchema};

/// Port the local agent server listens on. Chosen high and fixed so the client's
/// baked-in server URL is stable across restarts.
pub const AGENT_PORT: u16 = 41777;

static STARTED: OnceLock<bool> = OnceLock::new();

/// Base URL the client should be pointed at.
pub fn base_url() -> String {
    format!("http://127.0.0.1:{AGENT_PORT}")
}

/// Starts the server once per process. Safe to call from anywhere; subsequent
/// calls are no-ops.
///
/// Failure to bind is deliberately non-fatal: a terminal that will not start
/// because its agent port is busy is a worse outcome than a terminal whose
/// agent is unavailable.
pub fn ensure_started() {
    STARTED.get_or_init(|| {
        std::thread::Builder::new()
            .name("emiwarp-agent".into())
            .spawn(|| {
                let rt = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        eprintln!("EmiWarp: agent server runtime failed: {e}");
                        return;
                    }
                };
                rt.block_on(async {
                    if let Err(e) = serve().await {
                        eprintln!("EmiWarp: agent server stopped: {e}");
                    }
                });
            })
            .is_ok()
    });
}

async fn serve() -> std::io::Result<()> {
    let app = Router::new()
        .route("/ai/multi-agent", post(multi_agent))
        .route("/ai/passive-suggestions", post(passive))
        .with_state(());
    let addr = SocketAddr::from(([127, 0, 0, 1], AGENT_PORT));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await
}

/// Passive suggestions are an optimisation, not a feature the user asked for.
/// Answering with an empty stream keeps the client happy without burning tokens.
async fn passive() -> Response {
    sse(vec![
        event_init(&new_id(), &new_id(), &new_id()),
        event_finished_ok(),
    ])
}

async fn multi_agent(State(()): State<()>, _headers: HeaderMap, body: Bytes) -> Response {
    let request = match api::Request::decode(body.as_ref()) {
        Ok(r) => r,
        Err(e) => {
            return error_response(format!("EmiWarp could not decode the request: {e}"));
        }
    };

    let conversation_id = new_id();
    let request_id = new_id();
    let task_id = existing_task_id(&request).unwrap_or_else(new_id);

    let prompt = match extract_prompt(&request) {
        Some(p) if !p.trim().is_empty() => p,
        _ => {
            return error_response(
                "EmiWarp received a request with no user text to answer.".into(),
            );
        }
    };

    let profile = crate::provider().clone();
    let history = extract_history(&request);

    let answer = match backend() {
        Backend::Cli { command } => run_cli(&command, &prompt).await,
        Backend::Api => complete(&profile, &history, &prompt).await,
    };

    match answer {
        Ok(text) => {
            let mut events = vec![event_init(&conversation_id, &request_id, &task_id)];
            if existing_task_id(&request).is_none() {
                events.push(event_create_task(&task_id, &prompt));
            }
            events.push(event_agent_message(&task_id, &request_id, &text));
            events.push(event_finished_ok());
            sse(events)
        }
        Err(e) => error_response(format!(
            "EmiWarp could not reach {} at {}.\n\n{e}\n\nCheck EMIWARP_AI_PROVIDER, \
             EMIWARP_BASE_URL and EMIWARP_API_KEY in .env.emiwarp.",
            profile.kind.slug(),
            profile.base_url
        )),
    }
}

// ---------------------------------------------------------------------------
// Request reading
// ---------------------------------------------------------------------------

/// Pulls the newest user query out of the request.
fn extract_prompt(request: &api::Request) -> Option<String> {
    use api::request::input::Type;
    use api::request::input::user_inputs::user_input::Input;

    let input = request.input.as_ref()?;
    match input.r#type.as_ref()? {
        Type::UserInputs(inputs) => inputs.inputs.iter().rev().find_map(|i| match i.input.as_ref() {
            Some(Input::UserQuery(q)) => Some(q.query.clone()),
            Some(Input::CliAgentUserQuery(q)) => {
                q.user_query.as_ref().map(|uq| uq.query.clone())
            }
            _ => None,
        }),
        Type::UserQuery(q) => Some(q.query.clone()),
        _ => None,
    }
}

/// Prior turns, so follow-up questions keep their context.
fn extract_history(request: &api::Request) -> Vec<(String, String)> {
    use api::message::Message as M;

    let mut out = Vec::new();
    let Some(ctx) = request.task_context.as_ref() else {
        return out;
    };
    for task in &ctx.tasks {
        for m in &task.messages {
            match m.message.as_ref() {
                Some(M::UserQuery(q)) => out.push(("user".to_string(), q.query.clone())),
                Some(M::AgentOutput(a)) => {
                    out.push(("assistant".to_string(), a.text.clone()))
                }
                _ => {}
            }
        }
    }
    // Keep the tail: enough for continuity without unbounded prompt growth.
    if out.len() > 20 {
        out.drain(..out.len() - 20);
    }
    out
}

fn existing_task_id(request: &api::Request) -> Option<String> {
    request
        .task_context
        .as_ref()?
        .tasks
        .last()
        .map(|t| t.id.clone())
        .filter(|id| !id.is_empty())
}

// ---------------------------------------------------------------------------
// Response building
// ---------------------------------------------------------------------------

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn event_init(conversation_id: &str, request_id: &str, run_id: &str) -> api::ResponseEvent {
    use api::response_event::{StreamInit, Type};
    api::ResponseEvent {
        r#type: Some(Type::Init(StreamInit {
            conversation_id: conversation_id.to_string(),
            request_id: request_id.to_string(),
            run_id: run_id.to_string(),
        })),
    }
}

fn event_create_task(task_id: &str, prompt: &str) -> api::ResponseEvent {
    use api::client_action::{Action, CreateTask};
    use api::response_event::{ClientActions, Type};

    let description: String = prompt.chars().take(60).collect();
    api::ResponseEvent {
        r#type: Some(Type::ClientActions(ClientActions {
            actions: vec![api::ClientAction {
                action: Some(Action::CreateTask(CreateTask {
                    task: Some(api::Task {
                        id: task_id.to_string(),
                        description,
                        ..Default::default()
                    }),
                })),
            }],
        })),
    }
}

fn event_agent_message(task_id: &str, request_id: &str, text: &str) -> api::ResponseEvent {
    use api::client_action::{Action, AddMessagesToTask};
    use api::response_event::{ClientActions, Type};
    use api::message::{AgentOutput, Message as M};

    api::ResponseEvent {
        r#type: Some(Type::ClientActions(ClientActions {
            actions: vec![api::ClientAction {
                action: Some(Action::AddMessagesToTask(AddMessagesToTask {
                    task_id: task_id.to_string(),
                    messages: vec![api::Message {
                        id: new_id(),
                        task_id: task_id.to_string(),
                        request_id: request_id.to_string(),
                        message: Some(M::AgentOutput(AgentOutput {
                            text: text.to_string(),
                        })),
                        ..Default::default()
                    }],
                })),
            }],
        })),
    }
}

fn event_finished_ok() -> api::ResponseEvent {
    use api::response_event::stream_finished::{Done, Reason};
    use api::response_event::{StreamFinished, Type};
    api::ResponseEvent {
        r#type: Some(Type::Finished(StreamFinished {
            reason: Some(Reason::Done(Done {})),
            ..Default::default()
        })),
    }
}

fn error_response(message: String) -> Response {
    // Delivered as an ordinary agent message: the user sees what went wrong in
    // the conversation instead of an opaque transport failure.
    let task_id = new_id();
    let request_id = new_id();
    sse(vec![
        event_init(&new_id(), &request_id, &task_id),
        event_create_task(&task_id, "EmiWarp"),
        event_agent_message(&task_id, &request_id, &message),
        event_finished_ok(),
    ])
}

fn sse(events: Vec<api::ResponseEvent>) -> Response {
    let lines: Vec<Result<String, std::convert::Infallible>> = events
        .into_iter()
        .map(|e| {
            let mut buf = Vec::new();
            let _ = e.encode(&mut buf);
            Ok(format!("data: {}\n\n", BASE64_URL_SAFE.encode(buf)))
        })
        .collect();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream::iter(lines).map(|r| r.map(Bytes::from))))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

// ---------------------------------------------------------------------------
// Provider call
// ---------------------------------------------------------------------------

const SYSTEM_PROMPT: &str = "You are the EmiWarp terminal assistant. You run \
locally on the user's machine. Answer concisely and practically. When a shell \
command is the answer, show it in a fenced code block.";

/// How the agent request is answered.
///
/// `Cli` delegates to an agent CLI already installed on the machine — the
/// approach tools like Traycer take. It is strictly more capable than calling a
/// model directly, because the CLI already implements the agentic loop: running
/// commands, reading and editing files, and asking for permission. EmiWarp does
/// not reimplement any of that.
///
/// `Api` calls the configured endpoint directly. It answers in text only, with
/// no tool use, and exists for providers that have no CLI of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    Cli { command: String },
    Api,
}

/// Picks a backend: an explicitly requested one, else an installed CLI, else the
/// direct API.
pub fn backend() -> Backend {
    let cfg = &crate::runtime().config;
    match cfg.get("EMIWARP_AGENT_MODE").map(str::trim) {
        Some("api") => return Backend::Api,
        Some("cli") => {
            if let Some(c) = preferred_cli() {
                return Backend::Cli { command: c };
            }
            return Backend::Api;
        }
        _ => {}
    }
    match preferred_cli() {
        Some(c) => Backend::Cli { command: c },
        None => Backend::Api,
    }
}

/// The agent CLI to drive, if one is installed and signed in.
fn preferred_cli() -> Option<String> {
    if let Some(explicit) = crate::runtime().config.get("EMIWARP_AGENT_CLI") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return Some(explicit.to_string());
        }
    }
    // Order matters: the first that is installed AND signed in wins.
    let inv = crate::discover();
    for id in ["claude-code", "codex", "gemini", "opencode", "qwen"] {
        if let Some(h) = inv.harnesses.iter().find(|h| h.id == id) {
            if h.usable() {
                return Some(h.command.clone());
            }
        }
    }
    None
}

/// Runs an installed agent CLI headlessly and returns what it said.
///
/// The CLI inherits the provider environment overlay, so it talks to whichever
/// endpoint the user configured rather than its own default.
async fn run_cli(command: &str, prompt: &str) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    let mut cmd = Command::new(command);
    match command {
        // Headless, streaming JSONL. `--verbose` is required for stream-json.
        "claude" => {
            cmd.arg("-p").arg(prompt)
                .arg("--output-format").arg("stream-json")
                .arg("--verbose")
                // Headless has nobody to answer a permission prompt, so an
                // unset mode leaves the agent explaining that it is blocked
                // rather than doing the work. `acceptEdits` lets it read and
                // edit files while still gating command execution; set
                // EMIWARP_AGENT_PERMISSION_MODE=bypassPermissions to remove
                // that gate, understanding it lets the agent run anything.
                .arg("--permission-mode").arg(permission_mode());
        }
        "codex" => {
            cmd.arg("exec").arg(prompt).arg("--json").arg("--skip-git-repo-check");
            if permission_mode() == "bypassPermissions" {
                cmd.arg("--full-auto");
            }
        }
        // Gemini, OpenCode and Qwen all take the prompt positionally in
        // non-interactive mode.
        _ => {
            cmd.arg("-p").arg(prompt);
        }
    }
    for (k, v) in crate::provider().harness_env() {
        cmd.env(k, v);
    }
    // Run where the user is working, so relative paths in the prompt resolve.
    if let Some(dir) = working_dir() {
        cmd.current_dir(dir);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| format!("could not start `{command}`: {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout from the agent CLI")?;

    let mut text = String::new();
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let next = tokio::time::timeout_at(deadline, lines.next_line()).await;
        match next {
            Err(_) => return Err(format!("`{command}` timed out")),
            Ok(Ok(Some(line))) => {
                if let Some(t) = cli_text(&line) {
                    text.push_str(&t);
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return Err(format!("reading `{command}` output failed: {e}")),
        }
    }

    let status = child.wait().await.map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err(format!("`{command}` exited {status} without producing any text"));
    }
    Ok(text)
}

/// Permission posture for the spawned CLI.
fn permission_mode() -> String {
    crate::runtime()
        .config
        .get("EMIWARP_AGENT_PERMISSION_MODE")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("acceptEdits")
        .to_string()
}

/// Directory the agent runs in.
fn working_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = crate::runtime().config.get("EMIWARP_AGENT_CWD") {
        let d = d.trim();
        if !d.is_empty() {
            return Some(std::path::PathBuf::from(d));
        }
    }
    std::env::current_dir().ok()
}

/// Extracts assistant text from one JSONL line of an agent CLI's output.
fn cli_text(line: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;

    // Claude Code: {"type":"assistant","message":{"content":[{"type":"text",...}]}}
    if v.get("type").and_then(|t| t.as_str()) == Some("assistant") {
        let blocks = v.get("message")?.get("content")?.as_array()?;
        let joined: String = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if !joined.is_empty() {
            return Some(joined);
        }
    }

    // Codex: {"msg":{"type":"agent_message","message":"..."}}
    if let Some(msg) = v.get("msg") {
        if msg.get("type").and_then(|t| t.as_str()) == Some("agent_message") {
            if let Some(s) = msg.get("message").and_then(|m| m.as_str()) {
                return Some(s.to_string());
            }
        }
    }

    None
}

async fn complete(
    profile: &ProviderProfile,
    history: &[(String, String)],
    prompt: &str,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| e.to_string())?;

    let url = profile.chat_endpoint();
    let key = profile.api_key.clone().unwrap_or_default();

    let (body, req) = match profile.kind.schema() {
        WireSchema::AnthropicMessages => {
            let mut messages: Vec<serde_json::Value> = history
                .iter()
                .map(|(r, c)| serde_json::json!({"role": r, "content": c}))
                .collect();
            messages.push(serde_json::json!({"role": "user", "content": prompt}));
            let body = serde_json::json!({
                "model": profile.model,
                "max_tokens": 4096,
                "system": SYSTEM_PROMPT,
                "messages": messages,
            });
            let req = client
                .post(&url)
                .header("x-api-key", &key)
                .header("anthropic-version", "2023-06-01");
            (body, req)
        }
        _ => {
            let mut messages =
                vec![serde_json::json!({"role": "system", "content": SYSTEM_PROMPT})];
            messages.extend(
                history
                    .iter()
                    .map(|(r, c)| serde_json::json!({"role": r, "content": c})),
            );
            messages.push(serde_json::json!({"role": "user", "content": prompt}));
            let body = serde_json::json!({
                "model": profile.model,
                "messages": messages,
                "stream": false,
            });
            let mut req = client.post(&url);
            if !key.is_empty() {
                req = req.bearer_auth(&key);
            }
            (body, req)
        }
    };

    let resp = req
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("provider returned {status}: {}", truncate(&text, 400)));
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid JSON from provider: {e}"))?;
    extract_text(&json).ok_or_else(|| {
        format!("could not find text in the provider's reply: {}", truncate(&text, 400))
    })
}

/// Pulls the assistant text out of either wire schema's response shape.
fn extract_text(json: &serde_json::Value) -> Option<String> {
    // OpenAI-compatible
    if let Some(s) = json
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
    {
        return Some(s.to_string());
    }
    // Anthropic Messages
    if let Some(arr) = json.get("content").and_then(|c| c.as_array()) {
        let joined: String = arr
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() } else { format!("{}…", &s[..n]) }
}
