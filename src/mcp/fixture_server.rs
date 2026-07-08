//! Test-fixture MCP server, reachable only via the hidden
//! `__mcp_fixture_server` CLI mode (see `main.rs`). Not part of the
//! `local-code` product surface — exists only so
//! `tests/mcp_stdio_integration.rs` can exercise the real stdio transport
//! (spawn + Content-Length framing + JSON-RPC) against a real child
//! process instead of an in-process mock, without shipping a second
//! `[[bin]]` target.

use std::io::{self, Read, Write};

fn read_message(stdin: &mut impl Read) -> Option<Vec<u8>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stdin.read_exact(&mut byte).ok()?;
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        let line_str = String::from_utf8_lossy(&line);
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = len_str.trim().parse().ok();
        }
    }

    let length = content_length?;
    let mut body = vec![0u8; length];
    stdin.read_exact(&mut body).ok()?;
    Some(body)
}

fn write_message(stdout: &mut impl Write, body: &serde_json::Value) {
    let text = serde_json::to_string(body).expect("fixture responses always serialize");
    let header = format!("Content-Length: {}\r\n\r\n", text.len());
    let _ = stdout.write_all(header.as_bytes());
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

/// Runs the fixture MCP server loop against stdin/stdout until the pipe
/// closes. Never returns an error to the caller by design — `main.rs` just
/// runs this and exits.
pub fn run() {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    while let Some(body) = read_message(&mut stdin) {
        let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
            continue;
        };
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or_default();
        let id = request.get("id").and_then(|v| v.as_u64());

        match method {
            "initialize" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    );
                }
            }
            "notifications/initialized" => {}
            "tools/list" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": [{
                                    "name": "echo",
                                    "description": "Echoes back the given text.",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "text": {"type": "string"},
                                            "fail": {"type": "boolean"}
                                        }
                                    }
                                }]
                            }
                        }),
                    );
                }
            }
            "tools/call" => {
                if let Some(id) = id {
                    let arguments = request.pointer("/params/arguments");
                    let should_fail = arguments
                        .and_then(|a| a.get("fail"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let text = arguments
                        .and_then(|a| a.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();

                    write_message(
                        &mut stdout,
                        &serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{"type": "text", "text": text}],
                                "isError": should_fail
                            }
                        }),
                    );
                }
            }
            _ => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32601, "message": "method not found"}
                        }),
                    );
                }
            }
        }
    }
}
