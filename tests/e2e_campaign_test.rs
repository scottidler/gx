//! Phase 10 chunk-B e2e: a scripted MCP client drives the FULL campaign
//! round-trip over stdio against a fixture fleet (tmp repos + bare remotes,
//! never `~/repos`): `create-propose` -> `change-get` (read the diff) ->
//! `create-apply` -> `undo-plan` -> `undo-execute`.
//!
//! This is the design doc's Phase 10 success criterion made concrete: "the
//! scripted client completes the full campaign round-trip". Each step asserts
//! success at BOTH altitudes: the protocol level (no JSON-RPC `error`, no
//! tool-level `isError`) and the domain level (the branch actually lands on
//! the bare remote after apply; undo actually removes it and trues up state).
//!
//! No live LLM: the fake-agent fixture (Phase 7's `write_agent` pattern)
//! stands in for `create.llm.agent-command`, matching every other e2e file in
//! this repo. `undo-plan`'s GitHub reconcile runs against a `gh` shim (Phase
//! 5's `GH_SHIM` pattern from `tests/e2e_llm_apply.rs`) instead of a live
//! network call.
//!
//! JSON-RPC harness duplicated from `tests/mcp_tools_test.rs` (Phase 9), per
//! this repo's no-shared-`tests/common`-module convention.

#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Spawn gx-mcp with isolated XDG dirs + CWD + PATH, complete the
    /// initialize handshake, and send `notifications/initialized`. `path_env`
    /// lets the caller prepend a `gh` shim ahead of the real `PATH` (undo's
    /// GitHub reconcile shells out to `gh`); `remotes` is forwarded as
    /// `GX_TEST_REMOTES`, the env var the shim uses to locate the bare repos.
    fn spawn(
        config_home: &Path,
        data_home: &Path,
        cwd: &Path,
        path_env: &str,
        remotes: &Path,
    ) -> Mcp {
        let mut child = Command::new(gx_binary())
            .args(["mcp", "serve"])
            .env("XDG_CONFIG_HOME", config_home)
            .env("XDG_DATA_HOME", data_home)
            .env("PATH", path_env)
            .env("GX_TEST_REMOTES", remotes)
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
                "clientInfo": {"name": "gx-mcp-phase10-e2e", "version": "0.0.1"},
            }),
        );
        let init = mcp.recv_id(id);
        assert!(init.get("result").is_some(), "initialize failed: {init}");

        mcp.send_raw(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send_raw(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).expect("write");
        self.stdin.flush().expect("flush");
    }

    fn send(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send_raw(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Read lines until the response with `id` arrives; every line must be a
    /// well-formed JSON-RPC 2.0 message.
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
        }
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

/// Parse a successful tool result's JSON payload, panicking with the full
/// response on any refusal (every call in the happy-path round-trip below
/// must succeed).
fn expect_success(resp: &Value, step: &str) -> Value {
    if let Some(msg) = refusal(resp) {
        panic!("{step} must succeed, was refused: {msg}\nfull response: {resp}");
    }
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{step}: no text content: {resp}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{step}: payload not JSON: {e}\n{text}"))
}

// ---------------------------------------------------------------- fixtures

/// A config home enabling every mutating tool, with `create.llm.agent-command`
/// pointed at the fake agent (Phase 7's config-file shape).
fn config_enabling_campaign(dir: &Path, agent: &Path) -> PathBuf {
    let home = dir.join("config");
    let gx = home.join("gx");
    std::fs::create_dir_all(&gx).unwrap();
    let yaml = format!(
        "create:\n  llm:\n    agent-command: \"{}\"\n    timeout-seconds: 60\n\
         mcp:\n  tools:\n    create-propose: true\n    create-apply: true\n    \
         undo-plan: true\n    undo-execute: true\n",
        agent.display()
    );
    std::fs::write(gx.join("gx.yml"), yaml).unwrap();
    home
}

/// A fake agent (Phase 7 pattern): rewrites `data.md`, CWD = the temp worktree.
fn write_agent(dir: &Path) -> PathBuf {
    let agent = dir.join("agent.sh");
    std::fs::write(&agent, "#!/bin/sh\nprintf 'new value\\n' > data.md\n").unwrap();
    std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
    agent
}

/// `gh` shim (Phase 5's `GH_SHIM`, `tests/e2e_llm_apply.rs`): answers the PR
/// search with "none found" and deletes a branch ref from the bare remote
/// named by `$GX_TEST_REMOTES`, so `undo-plan`'s reconcile + `undo-execute`'s
/// branch deletion never touch the real network.
const GH_SHIM: &str = r#"#!/bin/sh
if [ "$1" = "api" ] && [ "$2" = "graphql" ]; then
  printf '%s' '{"data":{"search":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "close" ]; then
  exit 0
fi
if [ "$1" = "api" ]; then
  path="$2"
  case "$path" in
    repos/*/git/refs/heads/*)
      rest="${path#repos/}"
      rest="${rest#*/}"
      repo="${rest%%/*}"
      branch="${path#*refs/heads/}"
      git --git-dir "$GX_TEST_REMOTES/$repo.git" update-ref -d "refs/heads/$branch"
      exit $?
      ;;
  esac
fi
echo "gh shim: unexpected invocation: $*" >&2
exit 1
"#;

fn write_gh_shim(dir: &Path) -> PathBuf {
    let gh = dir.join("gh");
    std::fs::write(&gh, GH_SHIM).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir.to_path_buf()
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

/// A repo at `workspace/app` on `main` with a bare remote, tracking `data.md`
/// ("old value").
fn make_repo(workspace: &Path, remotes: &Path) {
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
}

/// `git show <branch>:<path>` in `repo` (the applied content lives on the
/// pushed branch; the finalized worktree is back on the original branch).
fn git_show(repo: &Path, branch: &str, path: &str) -> String {
    let out = Command::new("git")
        .args(["show", &format!("{branch}:{path}")])
        .current_dir(repo)
        .output()
        .expect("git show spawn");
    assert!(
        out.status.success(),
        "git show {branch}:{path} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn branch_on_bare(remotes: &Path, branch: &str) -> bool {
    Command::new("git")
        .args([
            "--git-dir",
            remotes.join("app.git").to_str().unwrap(),
            "rev-parse",
            "--verify",
            &format!("refs/heads/{branch}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}

// ---------------------------------------------------------------- the round-trip

#[test]
fn test_scripted_client_completes_the_full_campaign_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    let remotes = tmp.path().join("remotes");
    let data_home = tmp.path().join("data");
    let scripts = tmp.path().join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    make_repo(&workspace, &remotes);

    let agent = write_agent(&scripts);
    let cfg = config_enabling_campaign(tmp.path(), &agent);
    let gh_shim_dir = write_gh_shim(&scripts);
    let path_env = format!(
        "{}:{}",
        gh_shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let mut mcp = Mcp::spawn(&cfg, &data_home, &workspace, &path_env, &remotes);

    // 1. create-propose: run the fake agent per repo, mint the confirm token.
    let propose = mcp.call(
        "create-propose",
        json!({"prompt": "make data.md say new value", "patterns": []}),
    );
    let propose = expect_success(&propose, "create-propose");
    assert_eq!(propose["proposed"].as_u64(), Some(1), "propose: {propose}");
    assert_eq!(propose["failed"].as_u64(), Some(0), "propose: {propose}");
    let change_id = propose["change_id"].as_str().unwrap().to_string();
    let token = propose["token"].as_str().unwrap().to_string();
    assert!(!token.is_empty(), "create-propose must mint a token");
    let repos = propose["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1, "propose: {propose}");
    assert_eq!(repos[0]["outcome"].as_str(), Some("proposed"));
    let slug = repos[0]["slug"].as_str().unwrap().to_string();

    // 2. change-get: read the full per-repo diff (create-propose returns only
    // a files/diff-stat summary; change-get is the full-diff fetch).
    let detail = mcp.call("change-get", json!({"change_id": change_id}));
    let detail = expect_success(&detail, "change-get");
    let proposal = &detail["proposal"];
    assert_eq!(proposal["change_id"].as_str(), Some(change_id.as_str()));
    let proposal_repos = proposal["repos"].as_array().unwrap();
    assert_eq!(proposal_repos.len(), 1, "change-get: {detail}");
    let patch = proposal_repos[0]["patch"]
        .as_str()
        .expect("change-get must return the full diff");
    assert!(
        patch.contains("new value"),
        "change-get diff must show the proposed content: {patch}"
    );

    // 3. create-apply: writes the proposal's blobs through the real pipeline,
    // commits, and pushes the branch (no PR: MCP create-apply never opens one).
    let apply = mcp.call(
        "create-apply",
        json!({"change_id": change_id, "token": token}),
    );
    let apply = expect_success(&apply, "create-apply");
    assert_eq!(apply["applied"].as_u64(), Some(1), "apply: {apply}");
    assert_eq!(
        apply["drifted_or_failed"].as_u64(),
        Some(0),
        "apply: {apply}"
    );
    let apply_repos = apply["repos"].as_array().unwrap();
    assert_eq!(apply_repos.len(), 1, "apply: {apply}");
    assert_eq!(apply_repos[0]["status"].as_str(), Some("Committed"));
    assert!(apply_repos[0]["pr_url"].is_null(), "apply: {apply}");
    assert!(
        branch_on_bare(&remotes, &change_id),
        "create-apply must push the change-id branch to the bare remote"
    );
    // The pipeline's finalize() switches the worktree back to the original
    // branch (`main`) once the GX branch is pushed, so the applied content
    // lives on the pushed branch, not the checked-out working tree.
    let data_md = git_show(&workspace.join("app"), &change_id, "data.md");
    assert_eq!(
        data_md, "new value\n",
        "the applied blob must land on the pushed branch"
    );

    // 4. undo-plan: reconcile against the gh shim, mint the undo token.
    let plan = mcp.call("undo-plan", json!({"change_id": change_id}));
    let plan = expect_success(&plan, "undo-plan");
    let undo_token = plan["token"].as_str().unwrap().to_string();
    assert!(
        !undo_token.is_empty(),
        "undo-plan must mint a token: {plan}"
    );
    assert_eq!(plan["actionable"].as_u64(), Some(1), "undo-plan: {plan}");
    let plan_entries = plan["plan"].as_array().unwrap();
    assert_eq!(plan_entries[0]["slug"].as_str(), Some(slug.as_str()));

    // 5. undo-execute: reverses the pushed campaign (branch gone remote +
    // local, state trued up to Abandoned, proposal artifacts removed).
    let undo = mcp.call(
        "undo-execute",
        json!({"change_id": change_id, "token": undo_token}),
    );
    let undo = expect_success(&undo, "undo-execute");
    let undo_repos = undo["repos"].as_array().unwrap();
    assert_eq!(undo_repos.len(), 1, "undo-execute: {undo}");
    assert_eq!(undo_repos[0]["outcome"].as_str(), Some("Undone"));
    assert!(
        !branch_on_bare(&remotes, &change_id),
        "undo-execute must delete the pushed branch from the remote"
    );

    let state_path = data_home
        .join("gx")
        .join("changes")
        .join(format!("{change_id}.json"));
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    assert_eq!(
        state["status"], "Abandoned",
        "state must be Abandoned: {state}"
    );

    let proposal_dir = data_home.join("gx").join("proposals").join(&change_id);
    assert!(
        !proposal_dir.exists(),
        "undo-execute must remove the proposal artifacts (retention)"
    );
}
