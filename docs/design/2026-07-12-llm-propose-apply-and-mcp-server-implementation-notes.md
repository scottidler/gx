# Implementation Notes: LLM Propose-Apply and MCP Server

Running, append-only record of how the implementation diverges from or
interprets `docs/design/2026-07-12-llm-propose-apply-and-mcp-server.md`.
One section per phase; all four buckets filled ("None." where empty).

## Phase 0: Spike: headless agent produces an appliable patchset

Zero code. Live `claude -p` (v2.1.207) run in a scratch git repo under the
session scratchpad (never `~/repos`), against detached temp worktrees.

### Design decisions
- Spike run inline by the orchestrator (not the phase-implementer agent) —
  it is a judgment-heavy live-agent probe with hang/auth risk, and its whole
  point is observing real behavior. — how-to-execute-a-plan Phase 0.
- Command template proven working:
  `claude -p "<prompt>" --output-format text --permission-mode acceptEdits`
  with CWD = the detached temp worktree.

### Deviations
- **The design's config default `agent-command: "claude -p --output-format
  text"` is insufficient and must gain `--permission-mode acceptEdits`.**
  Design doc line 322 (Config additions) and Phase 4. In print (`-p`) mode
  Claude Code will NOT edit files without an edit-granting permission mode;
  with the doc's bare command the agent replies with prose but writes
  nothing, so every propose would be a false "empty" outcome. Fix: Phase 1
  ships the annotated example with `--permission-mode acceptEdits` and Phase 4
  uses it as the built-in default. (`--dangerously-skip-permissions` also
  works but is broader than needed; `acceptEdits` is the least-privilege
  choice for "edit files in a throwaway worktree".)

### Tradeoffs
- `acceptEdits` vs `--dangerously-skip-permissions` — chose `acceptEdits`:
  it grants file edits (what propose needs) without also auto-approving
  arbitrary Bash/network, keeping the throwaway-worktree blast radius as
  small as the design's Security section assumes.

### Spike results (evidence)
- Happy path: exit 0, 48s latency, agent edited `greeting.py` in place;
  stdout carried only the agent's text summary (clean for the MCP transport
  concern, though propose does not run over the transport).
- `git add -A` + `git diff --cached <base-sha>` produced a unified patch;
  `git apply --check` passed and `git apply` applied clean in a fresh clone.
  Note: diffing `--cached <sha>` emits `c/`…`i/` path prefixes (not `a/`…`b/`);
  `git apply` handles them. gx uses blobs for apply anyway (diff is
  display-only per Resolved Decisions), so the prefix is cosmetic.
- Timeout kill: `setsid timeout -s KILL 5 claude -p …` returned 124 after
  5s; the real source repo was byte-identical (clean `status`, empty
  `diff --stat`). `setsid` + a signal to the group is the pattern Phase 4
  should use for process-group kill.
- Empty diff: a "modify nothing, reply DONE" prompt exited 0 in 5s with an
  empty cached diff — recorded as the valid "empty" outcome, not an error.

### Open questions
- None. The environmental assumption (ambient `claude` credentials, headless
  edit works) is proven for this operator's machine; the fake-agent fixture
  (Phase 7) isolates CI from live-LLM flakiness.

## Phase 1: Housekeeping: build.rs + config strictness

### Design decisions
- `build.rs` gains `cargo:rerun-if-changed=.git/packed-refs` alongside the
  existing `.git/HEAD` / `.git/refs/` triggers — `build.rs:20-23`. Manually
  verified in a throwaway repo (not this one, never `~/repos`): tagging then
  `git pack-refs --all` deletes the loose ref file under `.git/refs/tags/`
  entirely and only `.git/packed-refs` changes mtime, so the pre-existing
  `.git/refs/` watch alone misses a tag-only release. `tests/build_script_test.rs`
  is a mechanical regression guard (asserts the directive string is present in
  `build.rs`) so the trigger can't be silently deleted later; the live
  packed-refs behavior itself is a build-time/git-mechanism fact, not
  something worth re-proving on every `cargo test` run.
- `#[serde(default, deny_unknown_fields)]` added to `Config` and every nested
  config struct (`GithubConfig`, `CreateConfig`, `OutputConfig`,
  `RepoDiscoveryConfig`, `LoggingConfig`, `RemoteStatusConfig`) —
  `src/config.rs`. `OutputVerbosity` is a unit-variant enum, not a struct;
  `deny_unknown_fields` doesn't apply to it and needs no change.
- Fixed a second, more serious silent-swallow in `Config::load` while
  flushing this out: the default-location branch (no `--config` flag,
  `$XDG_CONFIG_HOME/gx/gx.yml` found but failing to parse) logged a `warn!`
  and fell through to `Config::default()` — a typo'd key at a user's real
  config path ran silently with defaults, exactly the bug this phase exists
  to close. Now it propagates the parse error via `.context(...)` identically
  to the explicit `--config` path. `test_load_at_default_location_fails_loudly_on_typo`
  proves this with `XDG_CONFIG_HOME` pointed at a temp dir.
- `docs/configuration.md`'s "Configuration Validation" bullet "Warn about
  unknown configuration keys" was directly contradicted by this change
  (now a hard error, not a warning) — updated to say so.

### Deviations
- **Folded in the Phase 0 deviation as instructed:** did not add the
  `create.llm` / `mcp` config structs (that's Phase 4/9), so the shipped
  `gx.yml` example and `docs/configuration.md` schema are unchanged by this
  phase — they carry no `llm:`/`mcp:` keys yet, so there is nothing to
  reconcile against `deny_unknown_fields` today. The `--permission-mode
  acceptEdits` correction will land with the `create.llm` config in Phase 4,
  where the key is first introduced.
- `tests/integration_tests.rs::test_config_file_option` fed a bogus config
  (`parallelism: 2` / `max_depth: 5` — neither a real key; the real ones are
  `jobs` and nested `repo-discovery.max-depth`) that only ever worked because
  unknown fields were silently ignored. Fixed to use real keys
  (`jobs: "2"` / `repo-discovery.max-depth: 5`) — the exact "existing config
  keys that were silently ignored" case this phase's instructions called out.
- `otto ci`'s lint step (`whitespace -r`) auto-cleaned one pre-existing
  trailing-whitespace line in `docs/shakedown-v0.4.0.md`, unrelated to this
  phase's work; included in this commit rather than left dirty.
- This phase's commit also lands the design doc and its implementation-notes
  file, both untracked at the start of this phase (no prior phase had
  committed them) — folded in here rather than left as permanently-untracked
  working-tree state.

### Tradeoffs
- Mechanical `build.rs`-content assertion vs. a full `cargo build` + git-tag
  integration test — chose the mechanical assertion: a real build-triggering
  test would need to spawn a nested cargo build against a scratch git repo
  (slow, and `build.rs`'s `git describe` runs against `CARGO_MANIFEST_DIR`,
  not an arbitrary scratch dir, so a full repro would require symlink/env
  tricks disproportionate to what's being protected). The manual check above
  is the evidence; the test is the regression guard.
- Removing the `warn!`-then-default fallback narrows `Config::load`'s
  contract (a config file that exists but fails to parse is now always fatal,
  never silently downgraded) — kept it that way rather than special-casing
  "file exists but well-known-safe-to-ignore" errors, since no such case
  exists and inventing one reopens the silent-swallow hole.

### Open questions
- None.
