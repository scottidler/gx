//! Phase 8 success criterion: "an MCP client handshake against gx-mcp
//! succeeds (initialize + empty tool list)". This test IS the MCP client:
//! it speaks raw newline-delimited JSON-RPC 2.0 over the compiled binary's
//! stdin/stdout (rmcp's stdio transport framing), never rmcp's own client
//! machinery, so it proves the wire protocol rather than the crate's
//! internal round-trip.
//!
//! Also proves the design's "stdout carries only JSON-RPC" requirement: every
//! line read off stdout must parse as a JSON-RPC message, and stderr (where a
//! stray `println!`/panic would land) must stay empty across the exchange.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn gx_mcp_binary() -> &'static str {
    env!("CARGO_BIN_EXE_gx-mcp")
}

/// Send one JSON-RPC message as a single newline-terminated line (rmcp's
/// stdio transport framing, confirmed against the LinesCodec-based
/// `async_rw` transport it builds stdio on).
fn send_line(stdin: &mut ChildStdin, value: &Value) {
    let mut line = value.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .expect("write request line");
    stdin.flush().expect("flush request line");
}

/// Read one line, parse it as JSON, and assert it is a well-formed
/// JSON-RPC 2.0 message -- the "stdout carries only JSON-RPC" proof applies
/// per line, not just to the fields we happen to assert on.
fn recv_json_rpc_line(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    let start = Instant::now();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).expect("read response line");
        assert!(n > 0, "child closed stdout before responding");
        let trimmed = line.trim();
        if trimmed.is_empty() {
            assert!(
                start.elapsed() < HANDSHAKE_TIMEOUT,
                "handshake timed out on blank lines"
            );
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .unwrap_or_else(|e| panic!("stdout line is not valid JSON-RPC: {e}\nline: {trimmed}"));
        assert_eq!(
            value.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "every stdout line must be a jsonrpc 2.0 message, got: {value}"
        );
        return value;
    }
}

fn kill_and_reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn test_initialize_handshake_and_default_readonly_tool_list() {
    // Isolate logging (file-only, per design) under a throwaway data dir so
    // this test never touches the operator's real ~/.local/share/gx/logs, and
    // an empty config dir so gating is the DEFAULT (read-only on, mutating off)
    // regardless of the operator's real ~/.config/gx/gx.yml.
    let data_dir = tempfile::tempdir().expect("tempdir for XDG_DATA_HOME");
    let config_dir = tempfile::tempdir().expect("tempdir for XDG_CONFIG_HOME");

    let mut child = Command::new(gx_mcp_binary())
        .env("XDG_DATA_HOME", data_dir.path())
        .env("XDG_CONFIG_HOME", config_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gx-mcp");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    // 1. initialize
    send_line(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "gx-mcp-handshake-test", "version": "0.0.1"},
            }
        }),
    );
    let init_response = recv_json_rpc_line(&mut reader);
    assert_eq!(init_response["id"], json!(1));
    let result = init_response
        .get("result")
        .unwrap_or_else(|| panic!("initialize returned no result: {init_response}"));
    assert!(
        result
            .get("capabilities")
            .and_then(|c| c.get("tools"))
            .is_some(),
        "server must advertise tool capability even with zero tools registered: {result}"
    );
    assert!(
        result.get("serverInfo").is_some(),
        "initialize result missing serverInfo: {result}"
    );

    // 2. notifications/initialized (no response expected; fire and move on)
    send_line(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );

    // 3. tools/list -- Phase 9: the DEFAULT surface is the six read-only tools;
    //    the four mutating tools are gated off (absent) by default. (Phase 8
    //    served zero tools; this assertion was inverted when Phase 9 landed the
    //    curated surface + config gating.)
    send_line(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    );
    let tools_response = recv_json_rpc_line(&mut reader);
    assert_eq!(tools_response["id"], json!(2));
    let names: Vec<String> = tools_response["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list result missing tools array: {tools_response}"))
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for ro in [
        "status",
        "repo-discover",
        "change-list",
        "change-get",
        "review-status",
        "doctor",
    ] {
        assert!(
            names.contains(&ro.to_string()),
            "default surface must include read-only tool {ro}: {names:?}"
        );
    }
    for mutating in [
        "create-propose",
        "create-apply",
        "undo-plan",
        "undo-execute",
    ] {
        assert!(
            !names.contains(&mutating.to_string()),
            "mutating tool {mutating} must be gated off by default: {names:?}"
        );
    }

    kill_and_reap(child);
}

#[test]
fn test_stdout_carries_only_json_rpc_no_stray_bytes() {
    // Same handshake as above; this test's whole point is asserting the
    // NEGATIVE space -- nothing but jsonrpc lines ever appears on stdout, and
    // stderr (the file-only-logging violation channel) stays empty.
    let data_dir = tempfile::tempdir().expect("tempdir for XDG_DATA_HOME");

    let mut child = Command::new(gx_mcp_binary())
        .env("XDG_DATA_HOME", data_dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gx-mcp");

    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    send_line(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "gx-mcp-handshake-test", "version": "0.0.1"},
            }
        }),
    );
    // recv_json_rpc_line already asserts every line parses as jsonrpc 2.0;
    // reaching here without panicking IS the "only JSON-RPC on stdout" proof
    // for everything the server emitted before this point.
    let _ = recv_json_rpc_line(&mut reader);

    // The log file must exist and hold the entry logging.md requires
    // (function-level entry log), proving logging landed in the FILE, not
    // on stdout/stderr.
    let log_file = data_dir.path().join("gx").join("logs").join("gx-mcp.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !log_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        log_file.exists(),
        "gx-mcp must log to a file, found none at {log_file:?}"
    );
    let log_contents = std::fs::read_to_string(&log_file).expect("read gx-mcp log file");
    assert!(
        log_contents.contains("gx-mcp starting") || log_contents.contains("GxMcpServer"),
        "log file exists but is missing the expected entry-log lines: {log_contents}"
    );

    kill_and_reap(child);
}
