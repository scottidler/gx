//! Phase 9 tool-surface tests: a raw JSON-RPC client (the Phase 8 handshake
//! harness, extended) drives the compiled `gx-mcp` binary over stdio and proves
//! the design's success criteria:
//! - read-only tools are listed and callable; mutating tools are ABSENT by
//!   default (config gating; writes impossible by default);
//! - a disabled mutating tool call is REFUSED;
//! - a mutating call is refused for each of: missing token, stale token,
//!   manifest changed since plan, blob changed since plan, and undo state
//!   changed between plan and execute;
//! - stdout carries ONLY JSON-RPC across a tool call (every line asserted).
//!
//! The token/state fixtures are built by calling gx's own lib functions with
//! EXPLICIT paths under a throwaway `XDG_DATA_HOME`, so no test-process env is
//! mutated (the child gets the same dir via `.env`). Self-contained, matching
//! this repo's no-`tests/common`-module convention.

#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use remote::create::manifest::{
    self, FileAction, FileEntry, ProposalManifest, ProposalOutcome, RepoProposal,
};
use remote::state::{ChangeState, RepoChangeStatus};

const TIMEOUT: Duration = Duration::from_secs(15);

fn gx_binary() -> &'static str {
    env!("CARGO_BIN_EXE_gx")
}

// ---------------------------------------------------------------- JSON-RPC harness

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
}

impl Mcp {
    /// Spawn gx-mcp with isolated XDG dirs + CWD, complete the initialize
    /// handshake, and send `notifications/initialized`.
    fn spawn(config_home: &Path, data_home: &Path, cwd: &Path) -> Mcp {
        let mut child = Command::new(gx_binary())
            .args(["mcp", "serve"])
            .env("XDG_CONFIG_HOME", config_home)
            .env("XDG_DATA_HOME", data_home)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gx-mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let reader = BufReader::new(stdout);
        let mut mcp = Mcp {
            child,
            stdin,
            reader,
            next_id: 1,
        };

        let id = mcp.send(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "gx-mcp-phase9-test", "version": "0.0.1"},
            }),
        );
        let init = mcp.recv_id(id);
        assert!(init.get("result").is_some(), "initialize failed: {init}");

        // notifications/initialized: no id, no response expected.
        mcp.send_raw(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send_raw(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write");
        self.stdin.flush().expect("flush");
    }

    /// Send a request with a fresh id; returns the id.
    fn send(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Read lines until the response with `id` arrives; every line must be a
    /// well-formed JSON-RPC 2.0 message (the "stdout is only JSON-RPC" proof).
    fn recv_id(&mut self, id: i64) -> Value {
        let start = Instant::now();
        loop {
            assert!(start.elapsed() < TIMEOUT, "timed out awaiting id {id}");
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read line");
            assert!(n > 0, "child closed stdout before responding to {id}");
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(trimmed)
                .unwrap_or_else(|e| panic!("stdout line not JSON-RPC: {e}\nline: {trimmed}"));
            assert_eq!(
                value.get("jsonrpc").and_then(Value::as_str),
                Some("2.0"),
                "every stdout line must be jsonrpc 2.0: {value}"
            );
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                return value;
            }
            // A notification or other-id message: keep reading.
        }
    }

    fn list_tools(&mut self) -> Vec<String> {
        let id = self.send("tools/list", json!({}));
        let resp = self.recv_id(id);
        resp["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        let id = self.send("tools/call", json!({"name": name, "arguments": args}));
        self.recv_id(id)
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The refusal message if the response is a refusal (protocol error OR a
/// tool-level `isError` result), else `None` (success).
fn refusal(resp: &Value) -> Option<String> {
    if let Some(err) = resp.get("error") {
        return Some(err["message"].as_str().unwrap_or("").to_string());
    }
    let result = resp.get("result")?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Some(
            result["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        );
    }
    None
}

/// Parse a successful tool result's JSON payload (the `Content::json` text).
fn success_json(resp: &Value) -> Value {
    let result = &resp["result"];
    assert_ne!(
        result.get("isError").and_then(Value::as_bool),
        Some(true),
        "expected success, got error: {resp}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text content: {resp}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("payload not JSON: {e}\n{text}"))
}

// ---------------------------------------------------------------- fixtures

fn empty_config(dir: &Path) -> PathBuf {
    // No gx/gx.yml -> Config::default -> mutating tools disabled.
    let home = dir.join("config-empty");
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// A config home whose gx.yml enables the given mutating tools.
fn config_enabling(dir: &Path, tools: &[&str]) -> PathBuf {
    let home = dir.join("config-enabled");
    let gx = home.join("gx");
    std::fs::create_dir_all(&gx).unwrap();
    let mut yaml = String::from("mcp:\n  tools:\n");
    for t in tools {
        yaml.push_str(&format!("    {t}: true\n"));
    }
    std::fs::write(gx.join("gx.yml"), yaml).unwrap();
    home
}

/// Write a proposal manifest + blobs for `change_id` under `data_home`, exactly
/// where the child's `XDG_DATA_HOME` resolves it. Returns the confirm token.
fn write_proposal(
    data_home: &Path,
    change_id: &str,
    slug: &str,
    base_sha: &str,
    files: &[(&str, FileAction, &[u8])],
) -> String {
    let dir = data_home.join("gx").join("proposals").join(change_id);
    let mut entries = Vec::new();
    for (path, action, bytes) in files {
        if *action != FileAction::Delete {
            manifest::write_blob(&dir, slug, path, bytes).unwrap();
        }
        entries.push(FileEntry {
            path: path.to_string(),
            action: *action,
            mode: "100644".to_string(),
            sha256: (*action != FileAction::Delete).then(|| local::hash::sha256_hex(bytes)),
            size: bytes.len() as u64,
        });
    }
    manifest::write_patch(&dir, slug, "--- display patch ---\n").unwrap();
    let m = ProposalManifest::new(
        change_id.to_string(),
        "test prompt".to_string(),
        "fake-agent".to_string(),
        vec![RepoProposal {
            slug: slug.to_string(),
            base_sha: base_sha.to_string(),
            outcome: ProposalOutcome::Proposed,
            error: None,
            files: entries,
        }],
    );
    let (_path, token) = manifest::write_manifest(&dir, &m).unwrap();
    token
}

/// Write a change state file directly where `StateManager` looks
/// (`<data_home>/gx/changes/<id>.json`), so the child loads it.
fn write_state(data_home: &Path, state: &ChangeState) {
    let dir = data_home.join("gx").join("changes");
    std::fs::create_dir_all(&dir).unwrap();
    let json = serde_json::to_string_pretty(state).unwrap();
    std::fs::write(dir.join(format!("{}.json", state.change_id)), json).unwrap();
}

fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git spawn");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_stdout(args: &[&str], dir: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git spawn");
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repo at `workspace/app` on `main` with a bare remote, tracking `data.md`.
/// Returns (repo_path, head_sha).
fn make_repo(workspace: &Path, remotes: &Path) -> (PathBuf, String) {
    let bare = remotes.join("app.git");
    std::fs::create_dir_all(remotes).unwrap();
    git(
        &["init", "--quiet", "--bare", bare.to_str().unwrap()],
        remotes,
    );
    let repo = workspace.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["init", "--quiet", "--initial-branch=main"], &repo);
    git(&["config", "user.email", "t@e.com"], &repo);
    git(&["config", "user.name", "T"], &repo);
    git(&["config", "commit.gpgsign", "false"], &repo);
    std::fs::write(repo.join("data.md"), "old value\n").unwrap();
    git(&["add", "-A"], &repo);
    git(&["commit", "--quiet", "-m", "init"], &repo);
    git(&["remote", "add", "origin", bare.to_str().unwrap()], &repo);
    git(&["push", "--quiet", "-u", "origin", "main"], &repo);
    git(&["remote", "set-head", "origin", "main"], &repo);
    let head = git_stdout(&["rev-parse", "HEAD"], &repo);
    (repo, head)
}

// ---------------------------------------------------------------- read-only + gating

#[test]
fn test_readonly_listed_and_mutating_absent_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_config(tmp.path());
    let data = tmp.path().join("data");
    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());

    let tools = mcp.list_tools();
    for ro in [
        "status",
        "repo-discover",
        "change-list",
        "change-get",
        "review-status",
        "doctor",
    ] {
        assert!(
            tools.contains(&ro.to_string()),
            "read-only tool {ro} must be listed: {tools:?}"
        );
    }
    for mutating in [
        "create-propose",
        "create-apply",
        "undo-plan",
        "undo-execute",
    ] {
        assert!(
            !tools.contains(&mutating.to_string()),
            "mutating tool {mutating} must be ABSENT by default: {tools:?}"
        );
    }
}

#[test]
fn test_call_readonly_change_list_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_config(tmp.path());
    let data = tmp.path().join("data");
    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());

    let resp = mcp.call("change-list", json!({}));
    assert!(
        refusal(&resp).is_none(),
        "change-list should succeed: {resp}"
    );
    let payload = success_json(&resp);
    assert!(
        payload.is_array(),
        "change-list returns an array: {payload}"
    );
    assert_eq!(
        payload.as_array().unwrap().len(),
        0,
        "no changes in a fresh data dir"
    );
}

#[test]
fn test_call_readonly_repo_discover_finds_fixture() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_config(tmp.path());
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("ws");
    let remotes = tmp.path().join("remotes");
    make_repo(&workspace, &remotes);

    // CWD = workspace so discovery finds the fixture repo.
    let mut mcp = Mcp::spawn(&cfg, &data, &workspace);
    let resp = mcp.call("repo-discover", json!({"patterns": []}));
    let payload = success_json(&resp);
    let slugs: Vec<String> = payload
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["slug"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !slugs.is_empty(),
        "repo-discover must find the fixture repo: {payload}"
    );
}

#[test]
fn test_disabled_mutating_tool_call_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_config(tmp.path()); // create-propose disabled by default
    let data = tmp.path().join("data");
    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());

    let resp = mcp.call("create-propose", json!({"prompt": "x", "patterns": []}));
    let msg = refusal(&resp).expect("disabled tool call must be refused");
    // rmcp rejects a disabled/absent route with "tool not found".
    assert!(
        msg.contains("tool not found") || msg.contains("not found"),
        "refusal should indicate the tool is unavailable, got: {msg}"
    );
}

#[test]
fn test_enabling_a_mutating_tool_lists_it() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_enabling(tmp.path(), &["create-propose"]);
    let data = tmp.path().join("data");
    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());

    let tools = mcp.list_tools();
    assert!(
        tools.contains(&"create-propose".to_string()),
        "an enabled mutating tool must be listed: {tools:?}"
    );
    // The others stay disabled (default).
    assert!(!tools.contains(&"create-apply".to_string()));
}

// ---------------------------------------------------------------- create-apply refusals

#[test]
fn test_create_apply_refused_missing_token() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_enabling(tmp.path(), &["create-apply"]);
    let data = tmp.path().join("data");
    write_proposal(
        &data,
        "GX-t1",
        "org/app",
        "deadbeef",
        &[("a.txt", FileAction::Add, b"hi")],
    );

    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());
    let resp = mcp.call("create-apply", json!({"change_id": "GX-t1", "token": ""}));
    let msg = refusal(&resp).expect("empty token must be refused");
    assert!(
        msg.contains("token"),
        "refusal should cite the token: {msg}"
    );
}

#[test]
fn test_create_apply_refused_stale_token() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_enabling(tmp.path(), &["create-apply"]);
    let data = tmp.path().join("data");
    write_proposal(
        &data,
        "GX-t2",
        "org/app",
        "deadbeef",
        &[("a.txt", FileAction::Add, b"hi")],
    );

    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());
    let resp = mcp.call(
        "create-apply",
        json!({"change_id": "GX-t2", "token": "0000000000000000"}),
    );
    let msg = refusal(&resp).expect("a stale/wrong token must be refused");
    assert!(
        msg.contains("token"),
        "refusal should cite the token: {msg}"
    );
}

#[test]
fn test_create_apply_refused_when_manifest_changed_since_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_enabling(tmp.path(), &["create-apply"]);
    let data = tmp.path().join("data");
    // Capture the correct token, then mutate the manifest on disk.
    let token = write_proposal(
        &data,
        "GX-t3",
        "org/app",
        "deadbeef",
        &[("a.txt", FileAction::Add, b"hi")],
    );
    let manifest_path = data
        .join("gx")
        .join("proposals")
        .join("GX-t3")
        .join("manifest.json");
    let mut bytes = std::fs::read(&manifest_path).unwrap();
    bytes.extend_from_slice(b"\n"); // a single trailing byte changes the hash
    std::fs::write(&manifest_path, &bytes).unwrap();

    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());
    let resp = mcp.call(
        "create-apply",
        json!({"change_id": "GX-t3", "token": token}),
    );
    let msg = refusal(&resp).expect("a manifest changed since plan must be refused");
    assert!(
        msg.contains("token"),
        "refusal should cite the token mismatch: {msg}"
    );
}

#[test]
fn test_create_apply_refused_when_blob_changed_since_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_enabling(tmp.path(), &["create-apply"]);
    let data = tmp.path().join("data");
    let workspace = tmp.path().join("ws");
    let remotes = tmp.path().join("remotes");
    let (repo, head) = make_repo(&workspace, &remotes);
    let slug = "org/app";

    // Manifest modifies data.md; token matches (manifest untouched). base_sha =
    // repo HEAD so the drift check passes and apply reaches the BLOB check.
    let token = write_proposal(
        &data,
        "GX-t4",
        slug,
        &head,
        &[("data.md", FileAction::Modify, b"new value\n")],
    );
    // Tamper the persisted blob AFTER the manifest is written: the manifest (and
    // thus the token) is unchanged, but the blob no longer hashes to what the
    // manifest recorded -> per-repo hash-mismatch refusal, nothing written.
    let blob = manifest::blob_path(
        &data.join("gx").join("proposals").join("GX-t4"),
        slug,
        "data.md",
    );
    std::fs::write(&blob, b"TAMPERED\n").unwrap();

    // Change state: the repo is Proposed at `repo` so apply resolves it.
    let mut state = ChangeState::new("GX-t4".to_string(), Some("test".to_string()));
    state.mark_proposed(
        slug,
        head.clone(),
        vec!["data.md".to_string()],
        Some(repo.display().to_string()),
    );
    assert_eq!(state.repositories[slug].status, RepoChangeStatus::Proposed);
    write_state(&data, &state);

    let mut mcp = Mcp::spawn(&cfg, &data, &workspace);
    let resp = mcp.call(
        "create-apply",
        json!({"change_id": "GX-t4", "token": token}),
    );
    // Token matches, so this is a SUCCESS response whose report shows the repo
    // failed the blob check (nothing applied).
    let payload = success_json(&resp);
    assert_eq!(
        payload["applied"].as_u64(),
        Some(0),
        "nothing applied: {payload}"
    );
    assert!(
        payload["drifted_or_failed"].as_u64().unwrap_or(0) >= 1,
        "the tampered-blob repo must be a per-repo failure: {payload}"
    );
    let repo_err = payload["repos"][0]["error"].as_str().unwrap_or("");
    assert!(
        !repo_err.is_empty(),
        "the failed repo must carry an error: {payload}"
    );
    // The real worktree is untouched (nothing written).
    let data_md = std::fs::read_to_string(repo.join("data.md")).unwrap();
    assert_eq!(
        data_md, "old value\n",
        "the real worktree must be byte-identical"
    );
}

// ---------------------------------------------------------------- undo-execute refusal (case 5)

#[test]
fn test_undo_execute_refused_when_state_changed_between_plan_and_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config_enabling(tmp.path(), &["undo-plan", "undo-execute"]);
    let data = tmp.path().join("data");

    // A bare-proposal change (all repos Proposed): undo is local-only, so
    // plan_undo is deterministic and needs no remote reconcile.
    write_proposal(
        &data,
        "GX-t5",
        "org/one",
        "deadbeef",
        &[("a.txt", FileAction::Add, b"one")],
    );
    let mut state = ChangeState::new("GX-t5".to_string(), None);
    state.mark_proposed(
        "org/one",
        "deadbeef".to_string(),
        vec!["a.txt".to_string()],
        Some("/tmp/org/one".to_string()),
    );
    write_state(&data, &state);

    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());

    // 1. undo-plan -> token bound to the current (1-repo) plan.
    let plan_resp = mcp.call("undo-plan", json!({"change_id": "GX-t5"}));
    let plan = success_json(&plan_resp);
    let token = plan["token"].as_str().unwrap().to_string();
    assert!(!token.is_empty(), "undo-plan must mint a token: {plan}");

    // 2. State changes out from under the plan: a second repo becomes Proposed.
    state.mark_proposed(
        "org/two",
        "cafef00d".to_string(),
        vec!["b.txt".to_string()],
        Some("/tmp/org/two".to_string()),
    );
    write_state(&data, &state);

    // 3. undo-execute with the STALE token -> refused (plan changed).
    let exec_resp = mcp.call(
        "undo-execute",
        json!({"change_id": "GX-t5", "token": token}),
    );
    let msg = refusal(&exec_resp).expect("a stale undo plan must be refused");
    assert!(
        msg.contains("changed") || msg.contains("re-run undo-plan"),
        "refusal should say the plan changed: {msg}"
    );
}

// ---------------------------------------------------------------- stdout hygiene

#[test]
fn test_stdout_carries_only_jsonrpc_across_a_tool_call() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = empty_config(tmp.path());
    let data = tmp.path().join("data");
    let mut mcp = Mcp::spawn(&cfg, &data, tmp.path());

    // Every line `recv_id` reads is asserted to be a jsonrpc-2.0 message; a full
    // handshake + list + call exchange reaching here without a panic IS the
    // "stdout carries only JSON-RPC" proof for everything emitted so far.
    let _ = mcp.list_tools();
    let resp = mcp.call("doctor", json!({}));
    assert!(refusal(&resp).is_none(), "doctor should succeed: {resp}");

    // The log landed in the FILE (not stdout/stderr): file-only logging held.
    let log_file = data.join("gx").join("logs").join("gx.log");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !log_file.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        log_file.exists(),
        "gx-mcp must log to a file at {log_file:?}"
    );
}
