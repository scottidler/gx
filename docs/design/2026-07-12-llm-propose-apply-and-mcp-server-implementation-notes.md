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

## Phase 2: Lock primitive: File::try_lock

### Design decisions
- Both lock kinds now hold an OS advisory lock via `std::fs::File::try_lock()`
  (stable since Rust 1.89) on a non-truncating `OpenOptions` open —
  `src/lock.rs::acquire_lock_file`. `create(true).truncate(false)` (never
  `File::create`, which truncates) so a contender never clobbers a live
  holder's metadata; holder JSON is (re)written only AFTER the lock is held
  (`write_holder`, which `set_len(0)` + rewrites, safe because we hold the
  exclusive lock).
- `try_lock` error-matching ergonomics (flagged unproven by research):
  the API is `File::try_lock(&self) -> Result<(), std::fs::TryLockError>`
  where `TryLockError` has exactly two variants — `WouldBlock` and
  `Error(std::io::Error)`. `WouldBlock` -> the fail-fast "Locked by another
  gx process (…)" error (preserves today's no-queueing semantics);
  `Error(e)` -> propagate with context. Verified empirically with a throwaway
  `rustc` probe before writing the match, not from memory.
- Confirmed empirically (probe) that std uses **flock (per open file
  description)**, not POSIX `fcntl` (per process): two separate `open()`s in
  the SAME process contend, which is exactly what makes the same-process
  double-open `WouldBlock` success criterion hold. Had std used fcntl locks,
  same-process re-acquire would have silently succeeded.
- `Drop` for both guards only logs + drops the owned `File` (releasing the OS
  lock). No custom unlock call is needed — dropping the `File` handle unlocks.
- The `File` is stored as a `_file` field on each guard (RAII drop-guard
  exception in `rules/rust.md`); the guard owns it for the lock's whole life.
- MSRV: added `rust-version = "1.89"` to `Cargo.toml` (design Technical
  Considerations) since `File::try_lock` requires it.

### Deviations
- **Two lock tests were hardened against a fork/exec fd-inheritance race, not
  merely ported.** `File::try_lock` uses flock on the open file description;
  when ANY concurrent suite test `fork()`s a subprocess (git, spawned gx), the
  child transiently dup's every open fd — including in-flight lock fds — until
  its `exec()` closes the O_CLOEXEC ones microseconds later. That transient
  made `test_contention_stress_*` and `test_spawned_child_*` flaky under full
  concurrent CI load (green in isolation, ~1-in-2 fail under load). Hardening
  (flaky tests get hardened, not retried): (a) contention stress uses a FRESH
  repo/lock path per run so a prior run's transiently-leaked fd can't bleed
  into the next; (b) the O_CLOEXEC test polls the reacquire for a bounded 3s
  window — a real inheritance bug holds the lock for the child's full 30s life
  (poll never succeeds -> bites), while the ms-scale external transient clears
  well inside the window. Same effect as the spec's tests, correct seam.
- `src/lock/tests.rs` staleness tests rewritten as liveness tests per the
  phase spec: `test_stale_lock_is_reclaimed` -> `test_lock_reacquirable_after_
  holder_drops`; the three `test_is_stale_lock_content_*` and
  `test_concurrent_reclaim_never_loses_the_winning_live_lock` tests (which
  referenced the deleted reclaim fns) removed; `kill_9_holder_releases_lock_
  immediately_for_next_process` added to `tests/lock_contention_test.rs` as the
  cross-process dead-holder analog.

### Tradeoffs
- Bounded-poll O_CLOEXEC assertion vs a single one-shot reacquire — chose the
  bounded poll: a one-shot is what made the test flaky (it can land inside an
  unrelated fork/exec transient). The poll still bites the real bug because a
  genuinely-inherited fd holds the lock far longer than the poll window
  (proven: with the parent's `drop` removed, the poll fails deterministically).
- Left the lock FILE on disk after drop (never unlink) rather than cleaning it
  up — the panel must-fix. Persisted unlocked lock files are harmless
  (acquirable) and unlinking reintroduces the 2-winner interleave; the
  regression test `test_drop_never_unlinks_and_reopens_same_inode` pins it via
  the inode staying constant across drop+reacquire.

### Open questions
- None.
