//! End-to-end probe: start the local agent server, speak Warp's exact protocol
//! to it, and check a real model answered.
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE as B64;
use prost::Message as _;
use warp_multi_agent_api as api;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    emiwarp::agent_server::ensure_started();
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let p = emiwarp::provider();
    println!("provider : {:?}  model: {}", p.kind, p.model);
    println!("backend  : {:?}", emiwarp::agent_server::backend());

    // Exactly what the client sends.
    let req = api::Request {
        input: Some(api::request::Input {
            r#type: Some(api::request::input::Type::UserInputs(
                api::request::input::UserInputs {
                    inputs: vec![api::request::input::user_inputs::UserInput {
                        input: Some(
                            api::request::input::user_inputs::user_input::Input::UserQuery(
                                api::request::input::UserQuery {
                                    query: std::env::args().nth(1).unwrap_or_else(|| "Reply with exactly: EMIWARP_AGENT_OK".into()),
                                    ..Default::default()
                                },
                            ),
                        ),
                    }],
                },
            )),
            ..Default::default()
        }),
        ..Default::default()
    };

    let url = format!("{}/ai/multi-agent", emiwarp::agent_server::base_url());
    let body = req.encode_to_vec();
    let resp = match reqwest::Client::new().post(&url).body(body).send().await {
        Ok(r) => r,
        Err(e) => { println!("FAIL: could not reach the agent server: {e}"); std::process::exit(1); }
    };
    println!("HTTP     : {}", resp.status());

    let text = resp.text().await.unwrap_or_default();
    let mut got = String::new();
    let mut kinds = Vec::new();
    for line in text.lines().filter(|l| l.starts_with("data: ")) {
        let raw = match B64.decode(line[6..].trim()) { Ok(b) => b, Err(_) => continue };
        let Ok(ev) = api::ResponseEvent::decode(raw.as_slice()) else { continue };
        match ev.r#type {
            Some(api::response_event::Type::Init(_)) => kinds.push("init"),
            Some(api::response_event::Type::Finished(_)) => kinds.push("finished"),
            Some(api::response_event::Type::ClientActions(a)) => {
                kinds.push("client_actions");
                for act in a.actions {
                    if let Some(api::client_action::Action::AddMessagesToTask(m)) = act.action {
                        for msg in m.messages {
                            if let Some(api::message::Message::AgentOutput(o)) = msg.message {
                                got.push_str(&o.text);
                            }
                        }
                    }
                }
            }
            None => {}
        }
    }
    println!("events   : {}", kinds.join(" -> "));
    println!("reply    : {}", got.trim());
    let ok = !got.trim().is_empty();
    println!("\n{}", if ok { "PASS — the agent answered through Warp's protocol" }
                     else { "FAIL — no text came back" });
    std::process::exit(if ok { 0 } else { 1 });
}
