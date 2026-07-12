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

## Phase 3: Core/display split

### Design decisions
- New `src/confirm.rs` (`pub mod confirm` in `lib.rs`/`main.rs`) defines the
  `Confirmation` enum exactly to the doc's API-Design shape (`Token(String)` |
  `AlreadyConfirmed`), shared by all three split cores rather than duplicated
  per module — a plan/propose core and an execute core will both need it from
  Phase 4 onward, and a single definition keeps the seam consistent.
- Three split targets, matching the phase bullet's named functions:
  `process_create_command` → `src/create/core.rs::execute_create` (+CLI
  wrapper `create.rs::process_create_command`); `process_undo_command` →
  `src/undo/core.rs::{plan_undo, execute_undo}` (+wrapper
  `undo.rs::process_undo_command`); the CLI-level `execute_recovery` in
  `rollback.rs` (which mixed validate+print+confirm+dispatch) →
  `src/rollback/core.rs::{validate_recovery_state, execute_recovery}` (+
  wrapper `rollback.rs`'s private `execute_recovery`). `Transaction::execute_recovery`
  in `src/transaction.rs` was ALREADY print-free (verified: zero `println!`/
  `print!` in the whole file) and needed no split — it is the engine
  `rollback::core::execute_recovery` calls into.
  Read paths (status/review/doctor) were NOT split: nothing in this phase's
  success criteria or the MCP tool list required it yet (Phase 9 wires those
  tools when gx-mcp exists to call them), so splitting them now would be
  unrequested scope.
- `mod core;` is PRIVATE in all three wrappers (not `pub mod core;`): nothing
  outside the crate calls into these cores yet (gx-mcp doesn't exist until
  Phase 8). Each wrapper does `pub use core::{the types the wrapper/output.rs/
  main.rs already reference by name};` so `create::CreateResult`,
  `undo::UndoPlan`, etc. keep resolving unchanged. Phase 8/9 flips the
  relevant `mod core;` to `pub mod core;` when gx-mcp needs
  `gx::create::core::execute_create` etc. from a different workspace member.
- `CreateResult` gains `pub diff: Option<String>` (`src/create/core.rs`),
  populated by a new `join_diff(&diff_parts)` helper at every construction
  site inside `process_single_repo` (including `dry_run_error`, which now
  takes `diff_parts: &[String]` so an error result surfaces whatever partial
  diff had already been computed, `None` before any mutation started).
  Nothing renders it yet — Phase 4's present step and a future MCP
  `change-get` tool are the first consumers; the CLI wrapper only logs a
  `debug!` count of how many results carry a diff (never `println!`s it), so
  stdout is unaffected.
- `rollback::core::execute_recovery` deliberately does NOT re-acquire the
  per-repo lock or re-run validation: the CLI wrapper (`rollback.rs`) still
  loads the recovery state, acquires the `RepoLock`, prints the plan, runs
  `core::validate_recovery_state` and aborts BEFORE calling core if `!force`
  and validation errored, then prompts. Core is only ever invoked once the
  wrapper has already confirmed, exactly mirroring the create/undo pattern
  (core never re-decides an abort the wrapper already made) — see Deviations
  for why the lock is NOT held via an explicit parameter into core.
- `undo::core::plan_undo` returns `Option<UndoPlanSet>`: `None` reproduces the
  exact "no change state AND no recovery files" short-circuit the original
  `process_undo_command` had before ever calling `build_plan`/printing a plan
  header — preserving that the "nothing to undo" message is the ONLY output
  in that case (no plan header, no reconcile network calls).
- Every split core logs its own `debug!` entry naming its key params
  (function-level logging rule), including the `Confirmation` it received
  (`{confirmation:?}`) so a diagnosis never needs to guess which gate a
  caller went through.

### Deviations
- **`Confirmation::Token` is unused by any wrapper's real call path in this
  phase** (none of create/undo/rollback persists a hashable manifest yet), so
  every wrapper always passes `crate::confirm::already_confirmed()` rather
  than a literal `Confirmation::AlreadyConfirmed`. `already_confirmed()`
  honors `GX_TEST_CONFIRM_TOKEN` (inert unless set, matching the existing
  `GX_CRASH_POINT`/`GX_TEST_LOCK_DELAY_MS` hooks) so it can return
  `Confirmation::Token(hash)` too. Why this exists: `gx` compiles its ENTIRE
  module tree twice from identical source — once as the `gx` lib target
  (`lib.rs`), once as the `gx` bin target (`main.rs`, which declares its own
  parallel `mod` tree rather than depending on the lib crate, pre-existing
  architecture, not touched here). A `bin` crate has no external consumer, so
  `pub` items unused outside `#[cfg(test)]` are genuinely dead code in that
  target's non-test build and `-D warnings` (the `check`/`clippy` otto tasks)
  fails on it; a `Token(hash)` that nothing but a test ever constructs trips
  this. `already_confirmed()` is real, always-compiled (non-test) code that
  constructs `Confirmation::Token`, satisfying the dead-code check honestly
  (matching an established house pattern) instead of an `#[allow(dead_code)]`
  (banned) or a fabricated non-test caller. Every core still receives and
  logs whatever `Confirmation` it's given identically for both variants;
  Phase 4/5/9 gives `Token` its first REAL caller and its first real check.
- **The `gx` lib-vs-bin duplicate module tree is pre-existing** (confirmed:
  `Cargo.toml` declares `[lib] path = "src/lib.rs"` and `[[bin]] path =
  "src/main.rs"`, and `main.rs` declares its own full `mod` tree instead of
  `use gx::...`), not introduced by this phase. It is the reason any
  forward-looking pub type added without an immediate non-test production
  caller trips `-D warnings` in the bin target specifically (the lib target
  never warns: pub items there are correctly treated as the external API).
  Not fixed here — a `main.rs`-depends-on-`gx`-lib conversion is a
  substantial, unrelated refactor; flagged as an Open Question below rather
  than undertaken as a side effect of Phase 3.
- **`undo/tests.rs` moved wholesale to `undo/core/tests.rs`** with NO content
  changes (verified every test in it exercises a function that moved into
  `undo::core`: `classify_action`, `build_plan`, `needs_action`,
  `finalize_state`, `undo_one`, `revert_merged` via its `run_revert` helper —
  none touch `print_plan`/`confirm_undo`/`render_results`), so a `git mv` +
  re-pointing the `#[cfg(test)] mod tests;` declaration was sufficient; no
  test bodies needed splitting between wrapper and core.
- **`create.rs`'s previously-inline `#[cfg(test)] mod tests { ... }` block is
  now a proper `src/create/core/tests.rs` submodule** (Rust 2018+ style, per
  house convention) as a direct byproduct of relocating the functions it
  tests — not a separate cleanup pass. Same for `rollback/core/tests.rs`
  (new; `rollback.rs`'s own pre-existing inline tests for the UNRELATED
  `format_duration`/`parse_duration` helpers were left exactly where they
  were, out of this phase's scope).
- **Two new tests in `create/core/tests.rs` needed a bare-remote fixture**
  (`init_repo_with_bare_remote`) rather than a bare `init_git_repo`:
  `process_single_repo`'s `get_head_branch` call requires an `origin` remote
  to resolve the head branch (pre-existing behavior, not new), so a
  same-phase test exercising the happy path needs the same fixture the
  existing Phase-4 tests already used.
- **`crash::tests::test_crash_hook_call_sites_are_exactly_the_wired_points`
  updated** to expect `src/create/core.rs` instead of `src/create.rs` for the
  five non-`mid-finalize` crash hooks (they moved verbatim with
  `process_single_repo`/`commit_changes_with_rollback`); `src/transaction.rs`
  still wires `mid-finalize` unchanged. This is the exact "invert a test that
  pinned prior behavior by name" case, not a weakening — the test still
  proves the crash hooks are wired at exactly six points in exactly two
  files, just naming the new file.
- `otto ci`'s `whitespace -r` lint step made no changes this phase (verified
  clean before commit).

### Tradeoffs
- `rollback::core::execute_recovery` NOT taking the `RepoLock` as an explicit
  parameter (a "witness" that the caller already holds it) vs. re-acquiring
  the lock inside core — chose neither: the wrapper's `_lock` local simply
  outlives the synchronous call into `core::execute_recovery` in the same
  stack frame, so the lock's protection window is UNCHANGED from before the
  split (held from load through print/validate/confirm/execute) without core
  needing to know about locking at all. Re-acquiring inside core would have
  shrunk the protected window (lock only held during the confirmed execute,
  not the print+prompt before it) — a real, if likely-benign, concurrency
  behavior change that the phase's "byte-identical, refactor-only" mandate
  doesn't license without a deliberate call.
- `CreateResult.diff` as `Option<String>` (joined `diff_parts.join("\n")`) vs.
  keeping `Vec<String>` on the struct — chose the joined string: it is
  already the exact display-ready form every existing `diff_parts.push(...)`
  call site produces, and a future consumer (present step, `change-get`) can
  re-split on `"\n"` if it ever needs the per-file granularity; there was no
  concrete consumer in this phase to design the richer shape against.
- Kept `dry_run_error`'s new `diff_parts: &[String]` parameter required
  (rather than optional/defaulted) so every one of its ~10 call sites in
  `process_single_repo` states explicitly what diff state existed at that
  point, instead of a silent default that could drift out of sync with the
  surrounding code as future phases add more error paths.

### Open questions
- `main.rs` duplicating the entire `gx` lib module tree instead of depending
  on the `gx` lib crate (`use gx::...`) is pre-existing and out of this
  phase's scope, but it will keep tripping `-D warnings` on the bin target
  for any forward-looking pub API added without an immediate non-test
  production caller (this phase hit it for `Confirmation::Token`; Phase 4's
  propose/apply and Phase 8/9's gx-mcp scaffolding are likely to hit it
  again). Worth a dedicated cleanup (`main.rs` becomes a thin shell over
  `use gx::*`, matching the documented Shell/Core Split convention) at some
  point — not proposing it as part of this doc's remaining phases without
  Scott's say-so.

## Phase 4: Change::Llm propose

### Design decisions
- **`Change::Llm(String)`** added to the `Change` enum (`src/create/core.rs`);
  handled at the ORCHESTRATION level (`create::core::propose::execute_propose`),
  NOT in the per-repo `process_single_repo` match — propose/present/confirm is a
  fleet barrier (design Chunk A). The per-repo match gained a defensive
  `Change::Llm(_)` arm that returns a loud `Err` ("must go through the propose
  pass") so reaching it (a routing bug) fails loudly instead of silently.
- **Propose orchestration entry point:** `create::core::propose::execute_propose(
  repos, change_id, prompt, config, parallel_jobs) -> Result<ProposeSummary>`.
  It takes the `ChangeLock` (propose writes `changes/<id>.json` + proposal
  artifacts, exactly like a committing create), resolves agent-command + timeout
  from config, runs `propose_single_repo` fleet-parallel via a rayon pool,
  writes the canonical manifest, records `Proposed` state, and returns a summary.
  Wired to the CLI via `create.rs::run_llm_propose`, reached from
  `process_create_command` when `change` is `Change::Llm`.
- **Per-repo propose flow** (`propose_single_repo`, print-free core): `RepoLock`
  -> `git worktree add --detach <tmp/wt> <base_sha>` OUTSIDE the real worktree
  (base_sha = current HEAD) -> run the agent (CWD = temp worktree, prompt
  appended as final arg, wall-clock timeout, process-group kill) -> `git add -A`
  + `git diff --cached --raw -z <base_sha>` to capture -> persist blobs + patch +
  manifest entry -> **remove the temp worktree on EVERY path (incl. all errors),
  then the `TempDir` drops.** The real worktree is never touched.
- **Process-group kill** (`run_agent`): child spawned with
  `CommandExt::process_group(0)` so pgid == child pid; stdio redirected to a log
  FILE (sibling to the worktree, never inside it, so `git add -A` can't capture
  it — and it sidesteps the pipe-buffer drain deadlock); a poll loop enforces the
  deadline and on expiry runs `/bin/kill -KILL -<pgid>` to fell the whole group
  (grandchildren included). Phase 0 proved `setsid`+signal-to-group; std's
  `process_group(0)` is the crate-free equivalent, and `kill` the group-signal
  primitive std doesn't expose (libc would be a new dep chunk A forbids).
- **Proposal artifact layout** (`src/create/core/manifest.rs`), EXACTLY per Data
  Model, under `$XDG_DATA_HOME/gx/proposals/<change-id>/`:
  `manifest.json` (canonical `ProposalManifest`: version, change_id, prompt,
  agent_command, created_at, `repos: [{slug, base_sha, outcome, error, files:
  [{path, action add|modify|delete, mode, sha256, size}]}]`),
  `<slug>.patch` (display only), `<slug>/files/<rel-path>` (full post-change
  bytes = apply payload). Manifest field naming matches the sibling
  `changes/<id>.json` (serde default snake_case, `version` + `deny_unknown_fields`).
- **Token binds the applied bytes:** `manifest::compute_token(bytes)` = first
  `TOKEN_HEX_LEN` (16) hex chars of `hash::sha256_hex(bytes)`; `write_manifest`
  serializes the manifest (repos sorted by slug, files by path -> canonical),
  writes it atomically, and returns the token over the EXACT bytes written. Since
  the manifest carries every blob's `sha256`, the token transitively binds every
  blob. Tests prove flipping one blob hash changes the token.
- **`RepoChangeStatus::Proposed`** added BEFORE `BranchCreated`;
  `ChangeState::mark_proposed(slug, base_sha, files, local_path)` records it.
  Only `Proposed` repos are written to change state; empty/failed outcomes live
  only in the manifest (recording a failed PROPOSE as `Failed` would conflate it
  with a failed CREATE that may have pushed a branch).
- **`CHANGE_STATE_VERSION` 1 -> 2** for the new `Proposed` variant. Verified the
  fail-closed guarantee: an older gx's serde has no `Proposed` variant, so it
  fails loudly on `"status": "Proposed"` (unknown variant), and
  `deny_unknown_fields` (already on the state structs from Phase 1) catches any
  new field — no silent mis-load.
- **SHA-256 hand-rolled** in `src/hash.rs` (FIPS 180-4, validated against the
  empty/`abc`/two-block/0..=255 known-answer vectors). The design pins chunk A to
  ZERO new crates AND names `sha256`; `lock.rs::fnv1a_hex` sets the house
  precedent of hand-rolling a pinned deterministic hash rather than adding a dep.
  This is content-integrity binding, not adversarial secrecy.
- **Config `create.llm`** (`src/config.rs`): `agent-command` (default
  `"claude -p --output-format text --permission-mode acceptEdits"`, the Phase 0
  correction) and `timeout-seconds` (default 300). `LlmConfig` added under
  `CreateConfig` (which already had `deny_unknown_fields`); accessors
  `Config::llm_agent_command()` / `llm_timeout_seconds()`. Shipped example
  `gx.yml` and `docs/configuration.md` updated to show `create.llm` with the
  CORRECT agent-command and the "acceptEdits is required" rationale.
- **New git helpers** (`src/git.rs`): `worktree_add_detached`, `worktree_remove`
  (force, best-effort), `stage_all`, `diff_cached_patch` (display), and
  `diff_cached_raw_z` (raw NUL-terminated bytes so the mode metadata + non-UTF-8
  path bytes survive for the payload-matrix check).
- **Payload fidelity matrix** enforced in `capture_changes` at propose, from the
  `--raw -z` modes: symlink (`120000`) and gitlink/submodule (`160000`) are
  rejected as a loud per-repo `failed` outcome NAMING THE PATH; a non-UTF-8 path
  is rejected NAMING the lossy path. Regular files (any content incl. binary) are
  read raw (no lossy UTF-8 round-trip) and captured; a mode-only change records
  the destination mode + the (unchanged) blob so apply needs no special case.
  Validation happens BEFORE any artifact is written, so a rejected repo persists
  nothing.
- **Honest dead-code pattern** (bin target, per Phase 3): `Change::Llm` is
  constructed and `execute_propose` is called from `create.rs` via the
  inert-unless-set `GX_LLM_PROPOSE_PROMPT` hook (the CLI `llm` subcommand is
  Phase 6). Every `ProposeSummary` field is read in `run_llm_propose`. Phase-5-
  only reader fns (`load_manifest`, `recompute_token`) were deliberately NOT
  added (they'd have no non-test caller and trip `-D warnings`); tests exercise
  the round-trip via `serde_json::from_slice` + `compute_token` directly.

### Deviations
- **Config default agent-command carries `--permission-mode acceptEdits`** (the
  Phase 0 deviation, folded in here as instructed): the design's bare
  `claude -p --output-format text` (line 322 / Phase 4) cannot edit files
  headlessly, so every propose would be a false "empty".
- **SHA-256 is hand-rolled rather than a `sha2` dependency** — same effect
  (`sha256` per Data Model), correct seam given the "zero new crates" constraint
  and the `fnv1a_hex` precedent. Flagged rather than silently adding a crate.
- **`undo::classify_action` / `undo.rs::state_label` gained `Proposed` arms as
  fail-safe STUBS**, not the real behavior: `classify_action` maps `Proposed` ->
  `AlreadyGone` (never touches a remote — correct, a bare proposal has nothing
  remote), `state_label` -> `"proposed"`. The design's local-only `Proposed`
  undo arm (delete artifacts, mark `CleanedUp`) is Phase 5; adding the variant
  now forced these exhaustive matches to compile. No test pins the stub as
  correct behavior.
- **Re-propose overwrites the proposal dir in place** (no pre-delete): stale
  blobs from a larger prior propose could linger but are harmless (apply reads
  the manifest, not a dir listing). `gx doctor` orphan reporting is Phase 5.

### Tradeoffs
- **std `process_group(0)` + `/bin/kill -KILL -<pgid>`** vs. a `libc`/`nix`
  `setsid`+`kill` — chose the crate-free path: it honors "zero new crates," and
  the group kill is proven by a test whose fake agent spawns a grandchild that
  outlives the parent (the test asserts the grandchild pid is dead after
  timeout). Cost: Unix-only (`std::os::unix`), acceptable since the agent flow is
  inherently a Unix shell-out and CI is Linux (design: non-Linux "not
  regressing").
- **agent-command is whitespace-split** (not shell-parsed): matches gx's
  space-separated CLI convention and the default has no quoted args; the prompt
  is a SEPARATE argv entry so prompts with spaces/quotes are safe regardless.
- **Redirect agent stdio to a log file** vs. piping + concurrent drain — chose
  the file: no pipe-buffer deadlock (rust.md), and a preview of the log gives
  error context; the log lives OUTSIDE the worktree so it never pollutes the diff.
- **Empty/failed repos not written to change state** (manifest only) vs.
  recording all: keeps `undo`/`status` from treating a failed propose like a
  pushed branch; the present step (Phase 6) reads the manifest for the full
  fleet summary.

### Open questions
- None new. (The pre-existing `main.rs` lib/bin duplicate-module-tree debt noted
  in Phase 3 still applies; this phase used the honest inert-hook pattern to work
  within it rather than undertaking that refactor.)

## Phase 5: Change::Llm apply

### Design decisions
- **`Change::Patchset { proposal_dir, manifest: Arc<ProposalManifest> }`** added
  to the `Change` enum (`src/create/core.rs`), INTERNAL-only: never CLI-exposed,
  no `CreateAction` maps to it, constructed solely by `apply::execute_apply`. It
  rides the UNCHANGED `process_single_repo` pipeline. `Arc` so the fleet-shared
  `&Change` clones cheaply across rayon workers.
- **`apply_patchset_change`** (`src/create/core.rs`) is the per-repo apply, run
  from the `Change::Patchset` match arm AFTER the pipeline's stash/switch/pull
  (so `get_head_sha` sees the post-pull head). Two nothing-written refusals guard
  the write, both under the caller-held `RepoLock`: (1) post-pull drift
  (`HEAD != base_sha`), (2) per-blob sha256 + size verification of EVERY
  add/modify blob BEFORE any write (read the verified bytes into memory first,
  then mutate). Only then does it write through the EXISTING seam - register
  `RestoreBackup`/`RemoveCreatedFile` write-ahead, then delete / write the full
  post-change bytes. NO hunk application: blobs are the payload, the diff is
  display-only.
- **`file::write_bytes_with_git_mode`** (`src/file.rs`) is the one new seam
  primitive: atomic write + explicit mode from the proposal's git mode string
  (`100644`/`100755`). Needed because `atomic_write` alone preserves the
  file-on-disk's CURRENT mode, so a mode-only/exec-bit change from the proposal
  would otherwise be lost. Binary-safe (no UTF-8 round-trip). This is how a
  mode-only change "just works" (design payload matrix) - the F3 seam preserves
  modes, this sets the *target* mode.
- **`apply::execute_apply`** (`src/create/core/apply.rs`) is the `gx apply
  <change-id>` core (mirrors `gx undo`'s core). It: loads the manifest, recomputes
  the token from the RAW on-disk `manifest.json` bytes, gates on a caller-supplied
  `Confirmation::Token`, resolves the `Proposed` repos from change state (trusting
  the recorded slug as authoritative), constructs `Change::Patchset`, and calls
  `execute_create` (which owns the `ChangeLock`, state, F12). A missing proposal
  is a LOUD error naming the expected `manifest.json` path.
- **`manifest::load_manifest` + `recompute_token`** (`src/create/core/manifest.rs`)
  added as the first real callers (Phase 4 deferred them). `recompute_token`
  hashes `std::fs::read(manifest.json)` via `compute_token`, NOT a re-serialized
  struct (the Phase 4 handoff item): re-serializing could differ byte-for-byte
  and yield a token propose never emitted.
- **Partial-apply state reconciliation** (`apply::execute_apply` step 6):
  `execute_create` saves a FRESH state holding only the repos that COMMITTED. A
  repo that drifted/failed before committing is re-marked `Proposed` (with its
  error) under a re-acquired `ChangeLock`, so it is not lost from state and `gx
  undo` cannot miss it. "Committed" is `action in {Committed, PrCreated}` - a
  committed repo with a trailing PR/stash error still counts as applied (the
  branch landed), so it is NOT wrongly reverted to `Proposed`.
- **Real `Proposed` local-only undo arm** (`src/undo/core.rs`): new
  `UndoAction::CleanupProposal`; `classify_action(Proposed) -> CleanupProposal`
  (replaced Phase 4's fail-safe `AlreadyGone` stub); `undo_one`'s new arm calls
  `crate::create::manifest::remove_proposal_dir(change_id)` (local fs, idempotent)
  and marks the repo `CleanedUp` - touching NO remote. `is_remote_mutating`
  leaves it false, and `plan_undo` SKIPS the GitHub reconcile when the change is
  ENTIRELY bare proposals (nothing pushed to reconcile), so a bare-proposal undo
  is provably zero-gh/zero-git.
- **Retention** (design Data Model): `manifest::remove_proposal_dir` (gx-owned
  artifact, `std::fs::remove_dir_all` like the transaction recovery/backup
  lifecycle, NOT `rkvr` which is the user-facing purge). Called by the bare
  `CleanupProposal` arm, by `execute_undo` once a change reaches `Abandoned`
  (covers applied campaigns), and by `cleanup_single_change` when the change
  state is fully removed. `gx doctor` reports orphaned proposal dirs (a
  `proposals/<id>/` with no `changes/<id>.json`) and `--purge`s via `rkvr`.
- **`pub use core::manifest`** in `src/create.rs` so the retention callers outside
  `create` (`undo`, `cleanup`, `doctor`) reach the helpers through a stable
  `crate::create::manifest` path even though `core` stays private.
- **CLI seam `create::process_apply_command` + `GX_LLM_APPLY_CHANGE_ID` hook**
  (`src/create.rs`), the same inert-unless-set, dead-code-honest pattern as Phase
  4's `GX_LLM_PROPOSE_PROMPT`: gives the bin target a real non-test caller AND
  lets the e2e drive apply through the REAL binary. Phase 6 replaces the hook with
  the `gx apply` clap verb + present gate on top of `process_apply_command`.

### Deviations
- **`Change::Patchset` carries the manifest + dir, not the exact
  `Change::Llm(prompt)`-shaped payload the doc's prose implies.** Same effect,
  correct seam: `process_single_repo` takes ONE `&Change` for the whole fleet but
  each repo's proposal differs, so the variant carries the whole manifest and the
  per-repo entry is looked up by slug. This is the only way to ride the unchanged
  per-repo pipeline.
- **A minimal apply CLI path (`process_apply_command` + env hook) is added now,
  not deferred wholesale to Phase 6.** The phase brief permits a test-drivable
  seam; the polished clap verb + present/confirm re-display remain Phase 6. The
  env hook is inert in production.
- **`Confirmation::Token` gains its first real check here** (Phase 3 threaded it
  inert): `execute_apply` verifies a supplied token against the recomputed
  manifest token. CLI passes `AlreadyConfirmed` (or `Token` via the existing
  `GX_TEST_CONFIRM_TOKEN` hook); Phase 9's MCP `create-apply` supplies the real
  round-tripped token.

### Tradeoffs
- **Reuse `execute_create` wholesale (then reconcile) vs. a bespoke apply
  orchestrator** - chose reuse: the design's whole premise is that apply is
  exactly as deterministic as `sub` downstream of the patchset, so riding the
  identical pipeline is what makes crash/undo/lock/F12 parity FREE (proven by the
  crash-matrix e2e). The cost is the post-hoc state reconciliation for drifted
  stragglers, which is small and localized.
- **Skip the reconcile only for an ALL-proposals change** (not per-repo) - a
  mixed change still reconciles for its applied repos; only a change with zero
  pushed work skips gh entirely. This is the minimal, correct condition for the
  "zero remote invocations" guarantee without weakening reconcile for real
  campaigns.
- **Per-blob verification reads bytes into memory before writing** vs. verify-
  then-reread-on-write: holding the verified bytes guarantees the exact bytes
  hashed are the exact bytes written (no reread TOCTOU), at the cost of buffering
  a repo's payload - bounded by one repo's changed files, acceptable.

### Open questions
- None. (Cross-repo/system-mutating bullets: none in this phase. The `main.rs`
  lib/bin duplicate-module-tree debt from Phase 3 persists and was again worked
  within via the inert-hook pattern, not undertaken here.)

## Phase 6: CLI surface + present gate

### Design decisions
- **`gx create -p <patterns> [--yes] llm "<prompt>" [--propose]`** (`src/cli.rs`):
  `Llm { prompt, propose }` added to `cli::CreateAction` (the clap subcommand
  enum under `create`); `propose` is a plain bool flag (no value), matching the
  design's one-shot-vs-split shape exactly. `gx apply <change-id> [--pr] [--yes]`
  added as a new top-level `Commands::Apply` variant, `change_id` validated with
  the same `validate_change_id` (`GX-` prefix) as `create --change-id`/`undo`.
- **`main.rs`** wires both: `cli::CreateAction::Llm { prompt, .. } =>
  create::Change::Llm(prompt.clone())`, with `propose_only` extracted via a
  `matches!` guard alongside the existing action match (so it's available to
  `process_create_command` without restructuring the match itself); `Commands::
  Apply` calls `create::process_apply_command` directly - a fresh top-level
  entry point, not routed through `process_create_command`.
- **The blast-radius confirm now runs for `llm` too** (`create.rs::run_llm`,
  replacing Phase 4's `run_llm_propose`): design doc's Present section says
  "blast-radius confirm still runs up front as today", but Phase 4's propose
  CLI seam (env-hook only) never called `confirm_blast_radius` at all - propose
  runs an agent per repo (real cost, real time), so it deserves the identical
  up-front gate a committing `sub`/`regex`/`add`/`delete` gets, using the same
  `confirm_threshold` config and the same `confirm_blast_radius` helper. This
  closes a real gap, not a refactor of existing behavior.
- **Confirm gate #5** (`create.rs::confirm_apply`): the content-based gate
  after present, shared verbatim by the one-shot `llm` flow and `gx apply`.
  Fails closed exactly like the four existing TTY gates (`confirm_blast_radius`,
  undo's confirm, rollback execute's confirm, review purge's confirm): `--yes`
  bypasses; a non-interactive stdin without `--yes` is a loud `Err` naming
  `--yes`; only an interactive TTY without `--yes` prompts.
- **Present step** (`create.rs::present_diffs`): for every `Proposed` repo,
  reads the persisted `<slug>.patch` (Phase 4's display artifact) and prints it
  through `colorize_patch` (new, `+`/`-` lines green/red, `+++`/`---` headers
  bold, `@@` hunk headers cyan - same visual language `crate::diff::
  generate_diff` uses for `sub`/`regex`); `Empty`/`Failed` repos get a one-line
  note; a fleet summary (`N repositories: P proposed | E empty | F failed`)
  follows. This is the FIRST real consumer of the `diff_parts`/`CreateResult.
  diff` plumbing surfaced in Phase 3 for the deterministic path, and of the
  Phase 4 `<slug>.patch` artifact for the llm path - both were computed and
  discarded/unread until now.
- **The confirm token round-trips for real** (`Confirmation::Token`, first
  wired to a real caller here): the one-shot flow passes
  `Confirmation::Token(summary.token)` (the token `execute_propose` just
  minted) into `execute_apply`; `gx apply` passes `Confirmation::Token(token)`
  where `token` is `core::manifest::recompute_token(&dir)` computed from the
  SAME on-disk `manifest.json` bytes just presented. Either way, apply's
  existing (Phase 5) token check refuses if the manifest changed between
  present and apply - closing the loop the design's "token binds the applied
  bytes" must-fix started.
- **The `GX_LLM_PROPOSE_PROMPT` / `GX_LLM_APPLY_CHANGE_ID` env hooks are
  DELETED**, not left inert-alongside: `process_create_command` now routes
  `Change::Llm` straight to `run_llm` (no forward-hook branch), and
  `process_apply_command` gained a `pr`/`yes` signature reached only from
  `Commands::Apply`. `tests/e2e_llm_apply.rs` (Phase 5's crash-matrix + undo
  e2e) was updated to drive the real `create ... llm ... --propose` / `apply
  <id> --yes` verbs instead of the env hooks - the exact "replace that test
  seam with the real clap verb" instruction, applied to both hooks for
  consistency (leaving one hook alive while the other used the real verb would
  have left two different CLI-reachability stories for propose vs apply).
- **New e2e file `tests/e2e_llm_cli.rs`** (self-contained, matching the
  no-shared-`tests/common`-module convention already in this repo): three
  tests - (1) one-shot `llm` confirm gate #5 fails closed on non-TTY without
  `--yes`, (2) `gx apply` confirm gate #5 fails closed on non-TTY without
  `--yes`, (3) `--propose` then a separate `gx apply` produces the identical
  pushed-branch content and recorded per-repo state as the one-shot flow (same
  deterministic fake agent, two independent fixtures so the flows can't
  interfere). Tests (1) and (2) use `-p app` matching exactly one repo (under
  the default `confirm-threshold: 5`) specifically so the up-front
  blast-radius gate auto-proceeds and the ONLY thing under test is gate #5 in
  isolation.
- **New unit tests** `src/create/tests.rs` (new submodule, `create.rs` had none
  before - Phase 3 moved every prior inline test into `core`): `colorize_patch`
  is pure and easy to pin directly - asserts every line survives in order
  (headers, hunk marker, +/-/context lines) and that empty input round-trips
  to empty output.

### Deviations
- **`gx apply` gained a `--pr` flag**, which the design's literal API-Design
  CLI block (`gx apply <change-id> [--yes]`) does not show. Same effect,
  correct seam: Phase 5's own `process_apply_command` already threaded a `pr:
  Option<&PR>` parameter into `execute_apply` with the comment "PR creation is
  a Phase 6 flag" - the implementation handoff, not the doc's terse CLI
  summary, is the more precise signal here. Without it, the split flow
  (`--propose` then `gx apply`) could never open a PR, while the one-shot flow
  could (via `create`'s existing `--pr`) - an arbitrary capability gap between
  the two paths the design explicitly says must produce the "identical" apply
  pass.
- **Both env hooks removed, not just `GX_LLM_APPLY_CHANGE_ID`.** The phase
  brief named only the apply hook explicitly; `GX_LLM_PROPOSE_PROMPT` was
  removed too, for the reason above (consistency; Phase 7's "full matrix"
  needs one coherent CLI-entry-point story for both halves of the flow, not
  one real verb and one leftover env shim).
- **Present-step formatting uses plain `println!`/`print!` with `colored`
  string wrapping**, not `crate::output::display_unified_results` /
  `UnifiedDisplay` (the trait deterministic changes render through): a
  proposal's diff is per-repo unstructured patch TEXT (not a `CreateResult`
  with typed fields), so there is no natural `UnifiedDisplay` row to build:
  print_diffs is its own small renderer, matching the existing precedent of
  `run_llm_propose`'s (Phase 4) bespoke summary print rather than forcing the
  unified-result machinery onto a shape it wasn't built for.

### Tradeoffs
- **Confirm gate #5 shares one function (`confirm_apply`) across the one-shot
  flow and `gx apply`** vs. two near-identical copies - chose one function: the
  prompt text, `--yes` bypass, and fail-closed non-TTY error are identical by
  design ("same confirm gate"); a single function makes that identity
  structural rather than a comment promising two call sites stay in sync.
- **`run_llm` (one function) owns discover -> blast-radius confirm -> propose
  -> present -> confirm#5 -> apply** for the one-shot flow, rather than
  splitting propose-only and apply-only into always-separate helpers that
  `run_llm` merely calls twice - chose the single function: the one-shot path
  IS the design's literal sentence ("propose all -> present -> confirm -> apply
  all"), and `process_apply_command` already exists as the standalone,
  independently-callable apply entry point for the split flow, so there's no
  duplicated logic - only duplicated *rendering* (present_diffs,
  render_apply_report, confirm_apply), which are already shared helpers.
- **`present_diffs` takes `&[core::manifest::RepoProposal]` (the manifest's own
  type) rather than a `Vec<CreateResult>`-shaped adapter** - chose the direct
  manifest type: it's what both call sites already have in hand (a fresh
  `ProposeSummary.repos` in `run_llm`, a freshly-loaded `ProposalManifest.repos`
  in `process_apply_command`), and building a `CreateResult` shim purely to
  reuse `display_unified_results` would have meant inventing dummy
  `CreateAction`/`base_sha`/etc. fields with no real meaning at the propose/
  present stage (before any repo has been touched by apply).

### Open questions
- None new. (Cross-repo/system-mutating bullets: none in this phase. The
  `main.rs` lib/bin duplicate-module-tree debt from Phase 3 persists,
  unaffected by this phase's changes.)
