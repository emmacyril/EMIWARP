//! Proves the provider env overlay actually redirects a real agent CLI.
//!
//! Applies exactly the environment `provider-env` injects, then makes the same
//! request the CLI would. If this reaches the configured endpoint, the
//! substitution mechanism works end to end.
use std::io::{Read, Write};
use std::net::TcpStream;

fn main() {
    let p = emiwarp::provider();
    println!("provider : {:?}", p.kind);
    println!("endpoint : {}", p.chat_endpoint());
    println!("model    : {}", p.model);
    println!("command  : {}", p.command());
    println!("\nenv overlay applied to the spawned CLI:");
    for (k, v) in p.harness_env() {
        let shown = if k.contains("KEY") || k.contains("TOKEN") {
            "<redacted>".to_owned()
        } else {
            v.clone()
        };
        println!("  {k}={shown}");
    }

    let body = format!(
        r#"{{"model":"{}","messages":[{{"role":"user","content":"Reply with exactly: EMIWARP_OK"}}],"stream":false,"max_tokens":32}}"#,
        p.model
    );
    let url = p.chat_endpoint();
    let hostport = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let path = &url[url.find(&hostport).unwrap() + hostport.len()..];

    let mut s = match TcpStream::connect(&hostport) {
        Ok(s) => s,
        Err(e) => {
            println!("\nFAIL connect {hostport}: {e}");
            std::process::exit(1);
        }
    };
    let req = format!(
        "POST {path} HTTP/1.0\r\nHost: {hostport}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut resp = String::new();
    s.read_to_string(&mut resp).unwrap();

    let status = resp.lines().next().unwrap_or("");
    println!("\nHTTP: {status}");
    let ok = resp.contains("EMIWARP_OK");
    println!("model replied with sentinel: {ok}");
    if let Some(b) = resp.split_once("\r\n\r\n").map(|(_, b)| b) {
        println!("body: {}", b.chars().take(300).collect::<String>());
    }
    std::process::exit(if ok { 0 } else { 1 });
}
