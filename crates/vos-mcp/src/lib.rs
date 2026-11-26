//! Stateless JSON-RPC 2.0 MCP server for VOS over newline-delimited stdio.

mod tools;

use std::io::{self, BufRead, Write};

use serde_json::{Value, json};
use thiserror::Error;
use vos_store::{Store, StoreError};

/// Default MCP protocol revision advertised during handshake.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Errors produced while serving MCP JSON-RPC requests or dispatching tools.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("indexing failed: {0}")]
    Indexer(#[from] vos_indexer::IndexerError),
    #[error("invalid arguments: {0}")]
    InvalidParams(String),
    #[error("unknown tool: {0}")]
    UnknownTool(String),
}

/// Runs the stateless MCP server over process stdin/stdout until EOF.
pub fn serve_stdio(store: &mut Store) -> Result<(), McpError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let reader = stdin.lock();
    let mut writer = stdout.lock();
    serve_reader_writer(store, reader, &mut writer)
}

/// Processes newline-delimited JSON-RPC 2.0 frames from `reader` and writes responses to `writer`.
pub fn serve_reader_writer<R: BufRead, W: Write>(
    store: &mut Store,
    mut reader: R,
    writer: &mut W,
) -> Result<(), McpError> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => handle_request(store, &req),
            Err(err) => Some(json_rpc_error(
                Value::Null,
                -32700,
                &format!("Parse error: {err}"),
            )),
        };

        if let Some(resp) = response {
            let encoded = serde_json::to_string(&resp)?;
            writeln!(writer, "{encoded}")?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Handles a single JSON-RPC 2.0 request or notification. Returns `None` for notifications.
pub fn handle_request(store: &mut Store, req: &Value) -> Option<Value> {
    let id = req.get("id").cloned();
    let method = match req.get("method").and_then(Value::as_str) {
        Some(m) => m,
        None => {
            return id.map(|req_id| json_rpc_error(req_id, -32600, "Invalid Request: missing method"));
        }
    };

    // JSON-RPC notifications have no "id" member and expect no response frame.
    let req_id = id?;
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => Some(json_rpc_ok(req_id, build_initialize_result(&params))),
        "ping" => Some(json_rpc_ok(req_id, json!({}))),
        "tools/list" => Some(json_rpc_ok(
            req_id,
            json!({ "tools": tools::tool_definitions() }),
        )),
        "tools/call" => Some(handle_tools_call(store, req_id, &params)),
        _ => Some(json_rpc_error(
            req_id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

fn build_initialize_result(params: &Value) -> Value {
    let client_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| v.starts_with("202"))
        .unwrap_or(PROTOCOL_VERSION);

    json!({
        "protocolVersion": client_version,
        "capabilities": {
            "tools": {
                "listChanged": false
            }
        },
        "serverInfo": {
            "name": "vos",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "VOS Project Memory System. Start tasks with vos_continue or vos_status to orient in the project, use vos_search / vos_why / vos_graph / vos_show for targeted lookup, and track sessions with vos_session_start and vos_session_stop."
    })
}

fn handle_tools_call(store: &mut Store, req_id: Value, params: &Value) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return json_rpc_error(req_id, -32602, "Invalid params: missing tool name");
    };
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    match tools::execute_tool(store, name, &arguments) {
        Ok(payload) => match serde_json::to_string(&payload) {
            Ok(text) => json_rpc_ok(
                req_id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                }),
            ),
            Err(err) => json_rpc_error(req_id, -32603, &format!("Serialization failure: {err}")),
        },
        Err(McpError::UnknownTool(tool)) => {
            json_rpc_error(req_id, -32602, &format!("Unknown tool: {tool}"))
        }
        Err(err) => json_rpc_ok(
            req_id,
            json!({
                "content": [{ "type": "text", "text": format!("Error: {err}") }],
                "isError": true
            }),
        ),
    }
}

fn json_rpc_ok(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn json_rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;

    use tempfile::TempDir;

    use super::*;

    fn setup_store() -> (TempDir, Store) {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("demo");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("README.md"), "# Demo\n- [ ] Document MCP\n").unwrap();
        fs::write(
            proj.join("src/lib.rs"),
            "pub fn mcp_ping() -> &'static str { \"pong\" }\n",
        )
        .unwrap();
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("demo", Some(proj.as_path()), Some("Demo project"))
            .unwrap();
        (tmp, store)
    }

    #[test]
    fn stdio_loop_handles_handshake_notifications_tools_and_eof() {
        let (_tmp, mut store) = setup_store();
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"vos_index","arguments":{"project":"demo"}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"vos_continue","arguments":{"project":"demo"}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"vos_search","arguments":{"query":"mcp_ping","project":"demo"}}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"vos_status","arguments":{"project":"missing-proj"}}}"#,
            r#"{not json"#,
        ]
        .join("\n");

        let mut out = Vec::new();
        serve_reader_writer(&mut store, Cursor::new(input.as_bytes()), &mut out).unwrap();

        let text = String::from_utf8(out).unwrap();
        let lines: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // Notification yields no output line -> 7 responses total.
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[0]["id"], 1);
        assert_eq!(lines[0]["result"]["protocolVersion"], "2025-03-26");

        let tools = lines[1]["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "vos_continue"));
        assert!(tools.iter().any(|t| t["name"] == "vos_session_stop"));

        assert_eq!(lines[2]["result"]["isError"], false);
        let idx_payload: Value =
            serde_json::from_str(lines[2]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(idx_payload["added"], 2);

        let cont_payload: Value =
            serde_json::from_str(lines[3]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(cont_payload["resume"]["project"], "demo");
        assert!(cont_payload["recovery_prediction"]["difficulty"].is_string());

        let search_payload: Value =
            serde_json::from_str(lines[4]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(search_payload[0]["path"], "src/lib.rs");

        // Logical domain failure surfaces as isError: true rather than crashing server.
        assert_eq!(lines[5]["result"]["isError"], true);
        assert!(lines[5]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("project not found"));

        // Malformed JSON yields JSON-RPC -32700.
        assert_eq!(lines[6]["error"]["code"], -32700);
    }

    #[test]
    fn mcp_supports_sessions_why_graph_and_drift() {
        let (_tmp, mut store) = setup_store();
        let idx_req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "vos_index", "arguments": { "all": true } }
        });
        assert!(handle_request(&mut store, &idx_req).is_some());

        let start_req = json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "vos_session_start", "arguments": { "project": "demo", "note": "MCP test" } }
        });
        let start_resp = handle_request(&mut store, &start_req).unwrap();
        assert_eq!(start_resp["result"]["isError"], false);

        let why_req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "vos_why", "arguments": { "target": "src/lib.rs", "project": "demo" } }
        });
        let why_resp = handle_request(&mut store, &why_req).unwrap();
        assert_eq!(why_resp["result"]["isError"], false);

        let graph_req = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "vos_graph", "arguments": { "project": "demo" } }
        });
        let graph_resp = handle_request(&mut store, &graph_req).unwrap();
        assert_eq!(graph_resp["result"]["isError"], false);

        let stop_req = json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "vos_session_stop", "arguments": { "project": "demo", "quality": 5, "note": "Done" } }
        });
        let stop_resp = handle_request(&mut store, &stop_req).unwrap();
        assert_eq!(stop_resp["result"]["isError"], false);
    }

    #[test]
    fn mcp_supports_init_coverage_snapshot_evolution_and_doctor() {
        let tmp = TempDir::new().unwrap();
        let proj = tmp.path().join("fullmcp");
        fs::create_dir_all(proj.join("src")).unwrap();
        fs::write(proj.join("README.md"), "# FullMCP\nSee `src/lib.rs`.\n").unwrap();
        fs::write(proj.join("src/lib.rs"), "pub fn v() {}\n").unwrap();
        fs::write(
            proj.join("vos.rules.toml"),
            "[[rule]]\nname = \"pure\"\nfrom = \"src\"\nforbid_import = \"rusqlite\"\n",
        )
        .unwrap();

        let mut store = Store::open_in_memory().unwrap();
        let init_req = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "vos_init", "arguments": { "path": proj.to_str().unwrap(), "setup_mcp": false } }
        });
        let init_resp = handle_request(&mut store, &init_req).unwrap();
        assert_eq!(init_resp["result"]["isError"], false);

        for (id, tool, args) in [
            (2, "vos_coverage", json!({ "project": "fullmcp" })),
            (3, "vos_evolution", json!({ "project": "fullmcp", "since": "7d" })),
            (4, "vos_changed_since", json!({ "project": "fullmcp", "since": "7d" })),
            (5, "vos_related", json!({ "target": "src/lib.rs", "project": "fullmcp" })),
            (6, "vos_snapshot", json!({ "action": "create", "project": "fullmcp", "label": "s1" })),
            (7, "vos_doctor", json!({ "repair": false })),
            (8, "vos_tag", json!({ "target": "src/lib.rs", "project": "fullmcp", "tags": ["core"] })),
            (9, "vos_tags", json!({ "project": "fullmcp" })),
            (10, "vos_untag", json!({ "target": "src/lib.rs", "project": "fullmcp", "tags": ["core"] })),
            (11, "vos_symbols", json!({ "file": "src/lib.rs", "project": "fullmcp" })), 
            (12, "vos_slice", json!({ "target": "src/lib.rs", "project": "fullmcp", "symbol": "v" })), 
            (13, "vos_archaeology", json!({ "target": "src/lib.rs", "project": "fullmcp" })),
            (14, "vos_fact", json!({ "action": "add", "project": "fullmcp", "text": "v() returns unit", "file": "src/lib.rs", "author": "mcp" })), 
            (15, "vos_claim", json!({ "project": "fullmcp", "area": "src", "agent": "mcp-1" })), 
            (16, "vos_handoff", json!({ "project": "fullmcp", "from": "mcp-1", "summary": "verified v1.4 recall" })), 
            (17, "vos_release", json!({ "project": "fullmcp" })), 
            (18, "vos_recall", json!({ "query": "v1.4" })),
            (19, "vos_affected_tests", json!({ "project": "fullmcp", "targets": ["src/lib.rs"] })),
            (20, "vos_preflight", json!({ "project": "fullmcp", "targets": ["src/lib.rs"] })),
            (21, "vos_pack", json!({ "project": "fullmcp", "goal": "v", "budget": 1000 })),
            (22, "vos_calibrate", json!({ "project": "fullmcp" })),
            (23, "vos_constellation", json!({})),
        ] {
            let req = json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": { "name": tool, "arguments": args }
            });
            let resp = handle_request(&mut store, &req).unwrap();
            assert_eq!(resp["result"]["isError"], false, "tool {tool} failed: {resp}");
        }

        let guard_req = json!({
            "jsonrpc": "2.0", "id": 21, "method": "tools/call",
            "params": { "name": "vos_guard", "arguments": { "project": "fullmcp" } }
        });
        let guard_resp = handle_request(&mut store, &guard_req).unwrap();
        assert_eq!(guard_resp["result"]["isError"], false);
        let guard_payload: Value = serde_json::from_str(
            guard_resp["result"]["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(guard_payload["passed"], true);
    }
}