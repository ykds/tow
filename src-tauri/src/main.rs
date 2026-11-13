use serde_json::json;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

fn forward_hook(kind: &str) {
    let Ok(port) = std::env::var("AGENT_WORKSPACE_HOOK_PORT") else { return };
    let Ok(token) = std::env::var("AGENT_WORKSPACE_HOOK_TOKEN") else { return };
    let Ok(session_id) = std::env::var("AGENT_WORKSPACE_SESSION_ID") else { return };
    let Ok(run_id) = std::env::var("AGENT_WORKSPACE_RUN_ID") else { return };
    let mut input = String::new();
    if kind == "complete" {
        input = std::env::args().nth(3).unwrap_or_default();
    } else {
        let _ = std::io::stdin().take(65536).read_to_string(&mut input);
    }
    let parsed: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    if parsed.get("agent_id").and_then(|value| value.as_str()).is_some() {
        if kind == "title-agy" || kind == "stop-codex" { println!("{{}}"); }
        return;
    }
    let prompt = prompt_from_hook(kind, &parsed);
    let transcript_path = parsed.get("transcript_path").and_then(|value| value.as_str());
    let turn_id = parsed.get("turn_id").or_else(|| parsed.get("turn-id"))
        .and_then(|v| v.as_str()).unwrap_or("");
    let event_kind = if kind.starts_with("title-") { "title" }
        else if kind == "stop-codex" { "complete" } else { kind };
    let message = json!({"token": token, "session_id": session_id, "run_id": run_id,
        "kind": event_kind, "turn_id": turn_id, "prompt": prompt});
    send_hook(&port, &message);
    if kind == "title-codex" && !turn_id.is_empty() {
        let submit = json!({"token": token, "session_id": session_id, "run_id": run_id,
            "kind": "submit", "turn_id": turn_id, "transcript_path": transcript_path});
        send_hook(&port, &submit);
    }
    if kind == "title-agy" || kind == "stop-codex" { println!("{{}}"); }
}

fn send_hook(port: &str, message: &serde_json::Value) {
    let Ok(port) = port.parse::<u16>() else { return };
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    if let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
        let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
        let _ = writeln!(stream, "{message}");
    }
}

fn prompt_from_hook<'a>(kind: &str, input: &'a serde_json::Value) -> Option<&'a str> {
    match kind {
        "title-codex" => input.get("prompt"),
        "title-agy" => input.get("lastUserInput"),
        _ => None,
    }.and_then(|value| value.as_str())
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--hook-event") {
        if let Some(kind) = std::env::args().nth(2) {
            forward_hook(&kind);
        }
        return;
    }
    agent_workspace_lib::run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_hooks_use_each_clis_field() {
        let codex = json!({"prompt": "修复登录错误", "turn_id": "turn-1"});
        let agy = json!({"lastUserInput": "补测试", "conversationId": "conversation-1"});
        assert_eq!(prompt_from_hook("title-codex", &codex), Some("修复登录错误"));
        assert_eq!(prompt_from_hook("title-agy", &agy), Some("补测试"));
        assert_eq!(prompt_from_hook("title-agy", &codex), None);
    }
}
