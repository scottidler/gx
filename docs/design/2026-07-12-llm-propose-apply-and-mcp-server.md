# Design Document: LLM Propose-Apply Changes and MCP Server Surface

**Author:** Claude Code (from 2026-07-12 design session)
**Reviewer:** Scott Idler
**Date:** 2026-07-12
**Status:** In Review
**Review Passes Completed:** 5/5
**External review:** Architect (Gemini) + Staff Engineer (Codex), 2026-07-12;
all findings folded (see Resolved Decisions); one escalation answered by
Scott 2026-07-12 (deny_unknown_fields: yes)

## Summary

gx's change types are deterministic (`add` | `delete` | `sub` | `regex`); the
two chunks deferred by the 2026-07-11 rollback/undo-hardening doc's Non-Goals
are (A) `Change::Llm`: stochastic agent-per-repo changes via a propose-apply
patchset flow, and (B) an MCP server so agents drive fleet campaigns over the
protocol instead of the CLI. Both ride the v0.4.1 safety spine (write-ahead
recovery, campaign undo, per-repo + change locks, F12 fail-closed) that was
built precisely so a bad generation or an unattended agent campaign is always
recoverable. One combined doc, per Scott (2026-07-12); the shipped doc said
"separate design doc" for each, and that decision is recorded below.

## Problem Statement

### Background

- gx v0.4.1 (shipped 2026-07-11) hardened rollback/undo: phase-stamped
  write-ahead recovery (`src/transaction.rs`), `gx undo <change-id>` campaign
  reversal owning ALL remote reversal (`src/undo.rs`), remote-safe fail-closed
  `gx rollback execute`, per-repo + change-level locks (`src/lock.rs`), F12
  ("a pushed branch is always recorded in state OR recovery, never neither")
  enforced fail-closed, `gx review sync`, `gx doctor`.
- `Change::Agent(prompt)` was deferred in the 2026-06-11 workflow-safety doc
  (Non-Goals, line 72). Renamed `Change::Llm` / propose-apply patchsets and
  re-deferred in the 2026-07-11 doc (Non-Goals, lines 93-96) along with the
  MCP surface (`mcp-io-rs`), each marked "Separate design doc; ... depends on
  this work."
- The dependency is now satisfied: the review panel proved no remote-mutating
  call is reachable from `rollback`, and undo owns everything remote.

### Problem

Requirement sources: `Change::Agent(prompt)` deferral (Scott-approved
2026-06-11 doc); `Change::Llm` + `mcp-io-rs` deferrals (Scott-approved
2026-07-11 doc); Scott, 2026-07-11 session ("did we roll mcp-io-rs into this
code? we have an mcp capable interface now?"); combined-doc decision and
session handoff (Scott, 2026-07-12).

1. **No stochastic change type.** Every change's content is known up front.
   An LLM-generated change is not: the same prompt yields different patches
   per repo and per run. There is no propose -> present/diff -> apply flow,
   no persisted proposal artifact, and the half-built diff plumbing
   (`diff_parts` in `src/create.rs`) is computed then never read.
2. **No protocol surface.** An agent driving a fleet campaign today must
   shell out to the gx CLI and screen-scrape stdout. gx's core functions
   (`process_create_command`, `process_undo_command`, `execute_recovery`)
   take `&Cli` and `println!` directly, so they cannot back a stdio MCP
   transport (any stray stdout byte corrupts JSON-RPC).
3. **The lock primitive is not airtight at agent concurrency.** `RepoLock`'s
   rename-based stale-reclaim showed a rare 2-winner under pathological
   contention (`test_concurrent_reclaim_never_loses_the_winning_live_lock`,
   observed "left 2, right 1"). Implementation notes (2026-07-11, lines
   664-671) flagged `std::fs::File::lock()` as the airtight alternative,
   rejected then only as unrequested deviation from that doc, not on merit.
   LLM/MCP-driven runs raise concurrency; the handoff (2026-07-12) delegates
   the switch decision to this doc.
4. **Housekeeping riding along (handoff, 2026-07-12):** `build.rs` watches
   `.git/HEAD` + `.git/refs/` but `bump` writes tags to `.git/packed-refs`,
   so a tag-only release embeds a stale `GIT_DESCRIBE`.

### Goals

- `gx create ... llm "<prompt>"` runs an agent per repo in isolation,
  presents the diffs, and on confirmation drives the UNCHANGED
  branch/commit/push/PR pipeline; `gx undo <change-id>` reverses an applied
  campaign with the existing remote-reversal machinery unchanged. A bare
  (unapplied) proposal gets a new LOCAL-ONLY undo arm: delete artifacts,
  true up state, never touch a remote.
- A proposal is a persisted artifact: review now, apply later
  (`gx apply <change-id>`), survive process death, refuse loudly if the
  target repo drifted since proposal.
- A bad generation costs nothing: every failure mode (agent nonzero exit,
  timeout, garbage output) leaves the real worktree byte-identical.
- An MCP client can discover repos, propose a change, read the diffs, and
  apply, with mutating tools gated by config and a two-step confirm-token
  protocol; stdout carries only JSON-RPC.
- Locks are airtight under arbitrary concurrent stress: no double-winner,
  ever; a dead holder's lock is reacquirable immediately.

### Non-Goals

- A shared cross-repo MCP I/O crate (the `mcp-io-rs` reading of the name).
  Parked: extract from `gx-mcp` when a second consumer exists. This doc
  builds a gx workspace member only.
- HTTP/SSE MCP transports. stdio only; revisit if a remote driver appears.
- Replacing git/gh shell-outs with libgit2/octocrab (re-affirmed from both
  prior docs).
- Timeouts on git/gh shell-outs. The agent invocation gets a timeout; git/gh
  keep today's behavior. Parked: revisit if a hang is ever observed.
- Managing the agent CLI's auth/config. Ambient credentials assumed; Phase 0
  proves the assumption.
- Fleet resume and Windows-beyond-not-regressing (both re-affirmed).
- Multi-agent orchestration, retries-with-feedback, or prompt iteration
  loops. One generation per repo per propose; re-propose is the retry.

## Proposed Solution

### Overview

**Chunk A in one line: the agent edits a throwaway worktree, gx diffs it,
and the diff becomes a deterministic patchset that rides the existing
pipeline.** Stochastic generation is quarantined to a propose step that can
touch nothing real; everything after the patchset exists is exactly as
deterministic as `sub`/`regex` today, so recovery, undo, locks, and F12 apply
unmodified.

**Chunk B in one line: split core from display, then put an rmcp stdio
server (workspace member `gx-mcp`) in front of the cores, with per-tool
config gating and a plan -> confirm-token protocol replacing the TTY
prompts.** Precedent harvested from two in-house rmcp servers:
`multi-account-github-mcp` (server/tool-router layout, CLI-wrapping shape)
and `second-brain`'s oracle (config-gated methods, current rmcp idioms).

Eleven phases (0-10), ordered deterministic/cheap first, LLM/expensive
last: spike + housekeeping + lock (0-2), core/display split (3), chunk A
(4-7), chunk B (8-10). Phases 0-7 are a shippable chunk-A release on their
own. Single flat `v*` tag per repo throughout; no cross-repo blast radius
(gx only; `gx-mcp` is a new workspace member in this repo).

### Architecture

#### Chunk A: propose -> present -> apply

Structural point first: propose/present/confirm is a FLEET-level barrier,
but `process_single_repo` runs each repo end-to-end inside one rayon
`par_iter`. So `Change::Llm` is handled at the orchestration level, not
inside the per-repo match:

- **Propose pass** (new, fleet-parallel): generate + persist a proposal per
  repo.
- **Present + confirm** (fleet barrier): render diffs, one consent gate.
- **Apply pass**: each proposal converts to an internal deterministic
  `Change::Patchset(proposal)` (never CLI-exposed; no `CreateAction` maps
  to it) that rides the UNCHANGED `process_single_repo` pipeline (stash,
  switch, pull, apply, branch, commit, push, PR). Everything downstream of
  the patchset is exactly as deterministic as `sub`/`regex`; recovery,
  undo, locks, F12 apply unmodified.

Propose (per repo, fleet-parallel):

1. `RepoLock` as today.
2. Create a temp worktree of the repo's pristine head
   (`git worktree add --detach <tmp> <base-sha>`), outside the real worktree.
3. Run the configured agent command with CWD = temp worktree, prompt
   delivered per the config template, wall-clock timeout enforced, process
   group killed on expiry.
4. `git add -A` in the temp worktree (captures untracked adds and
   deletions), then `git diff --cached <base-sha>` produces the display
   patch; the post-change file contents, deletions, and modes are captured
   as the apply payload (see Data Model).
5. Empty diff = "no change proposed" per-repo outcome, not an error. Agent
   nonzero exit | timeout | unreadable worktree state = loud per-repo error.
6. Persist the proposal artifact + manifest; record repo status `Proposed`
   with `base_sha`; remove the temp worktree (all paths, including error
   paths).

Present: per-repo colored diff to the terminal (finally consuming the
`diff_parts` plumbing), plus a fleet summary (repos proposed | empty |
failed). This is the new confirmation gate: content-based, after generation,
the meaningful consent moment for stochastic changes (blast-radius confirm
still runs up front as today).

Apply (per repo, via `Change::Patchset` inside `process_single_repo`):

1. After the pipeline's stash/switch/pull, verify head SHA == manifest
   `base_sha`; mismatch = loud per-repo refusal ("repo drifted since
   proposal; re-propose"). The check sits post-pull deliberately: pull can
   advance head past the proposal's base.
2. Write each file through the EXISTING write seam: register
   `RestoreBackup`/`RemoveCreatedFile` write-ahead, then write the full
   post-change content from the proposal blob (delete for deletions,
   restore mode). NO patch/hunk application against the real worktree: gx
   never reimplements `patch`; the diff is for humans, the blobs are for
   apply.
3. Branch/stage/commit/push/PR exactly as today
   (`commit_changes_with_rollback`, pushed safe-point state save, finalize).

Apply-pass semantics: `gx apply` takes the `ChangeLock` (same as create);
partial apply is a normal per-repo outcome, exactly like today's per-repo
failures. A drifted or failed repo stays `Proposed` with its error
recorded and listed in the summary; the remedy is a fresh propose for the
stragglers. `gx apply` on a change-id whose proposal artifacts are missing
is a loud error naming the expected path.

One-shot vs split:

- `gx create ... llm "<prompt>"`: propose all -> present -> confirm (TTY
  gate #5, `--yes` for non-interactive, fail-closed like the existing four)
  -> apply all. The default.
- `gx create ... llm "<prompt>" --propose`: stop after persisting proposals.
- `gx apply <change-id>`: apply a persisted proposal set (mirrors
  `gx undo <change-id>`; re-presents the diffs, same confirm gate).
- Dry-run for llm == `--propose` (proposal IS the dry run; the
  apply-then-rollback dry-run dance is for deterministic changes only).

#### Chunk B: gx-mcp

- Workspace conversion: gx `Cargo.toml` becomes a `[workspace]` with members
  `gx` (existing lib+bin, path unchanged concerns handled in-phase) and
  `gx-mcp` (new bin crate, path dep on gx lib).
- Core/display split precedes it: `process_create_command`,
  `process_undo_command`, `execute_recovery` (and the read surfaces status |
  review | doctor as needed) become core fns returning the structured
  results they already build internally (`Vec<CreateResult>`,
  `Vec<UndoOutcome>`, ...); thin CLI wrappers own ALL `println!`. gx-mcp
  calls cores, never wrappers.
- Server: rmcp (current version via `cargo add`) + tokio + schemars, stdio
  transport, `#[tool_router]`/`#[tool]` layout harvested from
  `multi-account-github-mcp`; blocking gx cores run under
  `tokio::task::spawn_blocking`.
- Logging: file only (existing gx logging config), NEVER stdout/stderr noise
  on the transport.
- Curated tool surface (initial):
  - read-only: `status`, `repo_discover`, `change_list`, `change_get`
    (state + proposal diffs), `review_status`, `doctor`
  - mutating: `create_propose` (prompt + repo patterns -> change-id,
    per-repo diffs, confirm token), `create_apply` (change-id + token),
    `undo_plan` (change-id -> plan + token), `undo_execute` (change-id +
    token)
  - deliberately absent: rollback execute (recovery repair stays a human
    surface), review purge, cleanup
- Config gating (oracle pattern, sanctioned per general.md carve-out): every
  tool has `enabled:` under `mcp:` in `~/.config/gx/gx.yml`; read-only tools
  default true, mutating tools default FALSE. Writes impossible by default.
- Confirm-token protocol: plan/propose tools return a token = short hash of
  the persisted plan/proposal manifest. Execute tools require change-id +
  token; mismatch (state changed since plan) = refusal. An MCP client
  physically cannot skip seeing the plan. The four CLI TTY gates are
  untouched; the cores gain a caller-supplied confirmation input instead of
  prompting.

#### Lock primitive (decided here, delegated by the 2026-07-12 handoff)

Switch `RepoLock`/`ChangeLock` acquisition to `std::fs::File::try_lock()`
(OS advisory lock, stable Rust 1.89):

- Auto-release on process death eliminates staleness as a concept: the
  entire reclaim machinery (`reclaim_if_stale`, `is_stale_lock_content`,
  `process_alive`, rename dance, post-create re-verify) is deleted, not
  fixed.
- `try_lock()` (WouldBlock) preserves today's fail-fast no-queueing
  semantics. (Exact error-matching ergonomics verified at implementation;
  flagged unproven by research.)
- Open the lock file WITHOUT truncation (`OpenOptions` create + read/write,
  no `truncate`; never `File::create`, which truncates): a contender must
  never clobber a live holder's metadata. Holder JSON (pid/cwd/command) is
  written only AFTER the lock is held, for error messages.
- **Never unlink a lock file** (panel must-fix, 2026-07-12): today's `Drop`
  calls `fs::remove_file` (`src/lock.rs:75`, `:106`). Kept under flock that
  reintroduces the exact 2-winner: A holds, B opens+locks-pending the same
  path, A drops and unlinks the inode, C creates a fresh file at the path
  and locks the NEW inode while B holds the old one. Under flock, `Drop`
  only unlocks/closes; lock files persist harmlessly (unlocked =
  acquirable). The Phase 2 deletion checklist includes removing the
  `remove_file`-on-drop, with a regression test for this exact interleave.
- The `File` handle IS the lock: it must live for the lock's full lifetime
  (RAII guard owns it). Child processes (the spawned agent) must not
  inherit it; `O_CLOEXEC` is the Rust default, asserted by a test, not
  assumed.
- Advisory locks on network filesystems are the classic caveat;
  `$XDG_DATA_HOME` is local. Non-Linux stops needing the never-reclaim
  special case.
- Prior rejection was procedural ("the design doc explicitly prescribes the
  rename-based reclaim"), not on merit; this doc is the say-so. The 2-winner
  under contention is a real observed failure and agent concurrency is this
  doc's premise.

### Data Model

- `RepoChangeStatus` gains `Proposed` (before `BranchCreated`).
  `CHANGE_STATE_VERSION` bumps; `deny_unknown_fields` + the version field
  mean an older gx reading newer state fails loudly (fail closed, correct).
- Proposal artifacts: `$XDG_DATA_HOME/gx/proposals/<change-id>/`
  - `manifest.json`: the CANONICAL reviewed object. change-id, prompt,
    agent command line (resolved), created-at, per-repo entries (slug,
    `base_sha`, outcome: proposed | empty | failed + error, and per file:
    path, action add | modify | delete, mode, **blob sha256 + size**).
  - `<org>/<repo>.patch`: unified diff per repo, display only.
  - `<org>/<repo>/files/<path>`: full post-change content per
    added/modified file, the apply payload (files, not in-state JSON:
    payloads can be large, state stays small and scannable).
  - **Token binds the applied bytes** (panel must-fix, 2026-07-12): confirm
    token = truncated hash over canonical `manifest.json` bytes, and the
    manifest carries every blob's sha256, so no blob can change after
    review without invalidating the token. Apply verifies each blob's hash
    under `RepoLock` immediately before writing; hash mismatch = loud
    per-repo refusal, nothing written. For undo, the plan is not persisted:
    `undo-plan` computes the plan and its token; `undo-execute` recomputes
    and refuses on token mismatch (state changed between plan and execute).
- Payload fidelity matrix (panel must-fix, 2026-07-12; `Change::Patchset`
  must not silently become a more permissive write path than `sub`/`regex`):
  - SUPPORTED: regular files, any content including binary; executable-bit
    changes (a mode-only change still captures its blob, unchanged content,
    so apply has no special case: write blob + set mode through the seam,
    which preserves modes per the shipped F3 fix).
  - REJECTED at propose, loud per-repo failed outcome naming the path:
    symlinks, gitlinks/submodule changes, non-UTF-8 paths. Revisit any of
    these only on an observed real campaign that needs them.
- Retention: proposal dir removed when the change reaches `CleanedUp` (and
  by `gx undo`); `gx doctor` reports orphaned proposal dirs.
- `Proposed` undo arm (panel must-fix, 2026-07-12): `undo.rs` classifies
  states exhaustively and has no `Proposed` arm today. `gx undo` on a
  bare proposal is LOCAL-ONLY: delete proposal artifacts, mark the repo
  `CleanedUp`, never attempt any remote operation (there is nothing
  remote to reverse).
- Config additions (`~/.config/gx/gx.yml`):

```yaml
create:
  llm:
    agent-command: "claude -p --output-format text"   # prompt appended; CWD = temp worktree
    timeout-seconds: 300
mcp:
  tools:
    status: true
    create-propose: false
    create-apply: false
    undo-plan: false
    undo-execute: false
    # read-only default true, mutating default false; example ships annotated
```

- `Config` gains `#[serde(deny_unknown_fields)]` (house rule, currently
  missing): a typo'd `llm:` key must fail loudly, not be ignored. Behavior
  change, disclosed: every existing config file carrying an extra/legacy
  key starts failing loudly on next run. Approved by Scott, 2026-07-12
  (panel escalation, answered: rides Phase 1).

### API Design

CLI (space-separated multi-value flags, case-insensitive enums, per house
rules):

```
gx create -p <patterns> [--yes] llm "<prompt>"        # propose+present+confirm+apply
gx create -p <patterns> llm "<prompt>" --propose      # stop at persisted proposal
gx apply <change-id> [--yes]                          # apply persisted proposal
gx undo <change-id>                                   # unchanged; works on llm campaigns
```

MCP tools (names kebab-case; schemas via schemars):

```
create-propose { prompt, patterns[] } -> { change-id, token, repos: [{slug, outcome, files, diff-stat}] }
create-apply   { change-id, token }   -> { repos: [{slug, status, pr-url?}] }
undo-plan      { change-id }          -> { token, plan: [...] }
undo-execute   { change-id, token }   -> { repos: [{slug, outcome}] }
status | repo-discover | change-list | change-get | review-status | doctor  (read-only)
```

`create-propose` returns per-repo summaries (files + diff-stat), NOT full
diffs: fleet-sized diffs would blow protocol response limits. `change-get`
fetches one repo's full diff; a driver reads diffs repo-by-repo before
confirming.

Core signatures (shape, not final): every mutating flow splits into a
plan/propose core and an execute core. The wrapper (CLI or MCP) sits
between: CLI renders + prompts (or honors `--yes`), MCP returns the plan
and demands the token back. Cores take explicit params + a `Confirmation`
(`Token(hash)` | `AlreadyConfirmed`), never `&Cli`, never a callback,
never a prompt.

### Implementation Plan

#### Phase 0: Spike: headless agent produces an appliable patchset
**Model:** sonnet
- Zero code. In a scratch repo (never under `~/repos`): run
  `claude -p "<edit instruction>"` with CWD = a detached temp worktree;
  capture invocation shape, latency, exit codes, whether files were edited
  in place; `git diff` the worktree and `git apply --check` the patch in a
  fresh clone. Repeat with a deliberately failing prompt and a timeout kill.
- Record the working command template (becomes the config default) in the
  implementation notes.
- **Success criteria:** a captured transcript shows the agent edited the
  temp worktree; the extracted patch applies clean via `git apply --check`
  in a fresh clone; a killed-at-timeout run leaves everything outside the
  temp worktree untouched.

#### Phase 1: Housekeeping: build.rs + config strictness
**Model:** sonnet
- `build.rs`: add `cargo:rerun-if-changed=.git/packed-refs`.
- `Config`: add `#[serde(deny_unknown_fields)]` (+ nested config structs);
  fix anything it flushes out; update the shipped annotated example.
- **Success criteria:** after a tag-only `bump --tag-only`-style tag, a
  rebuild embeds the new tag in `GIT_DESCRIBE`; a config file with a typo'd
  key fails loudly naming the key.

#### Phase 2: Lock primitive: File::try_lock
**Model:** opus
- Replace O_EXCL create + rename-reclaim with non-truncating
  `OpenOptions` open + `try_lock()`; delete `reclaim_if_stale`,
  `is_stale_lock_content`, `process_alive`, `RECLAIM_COUNTER`, post-create
  re-verify, AND the `remove_file`-on-drop (`Drop` only unlocks/closes);
  keep holder JSON for error messages; keep `GX_TEST_LOCK_DELAY_MS`.
- Rewrite `src/lock/tests.rs` staleness tests as liveness tests;
  `tests/lock_contention_test.rs` (two-process) stays and must stay green.
- **Success criteria:** contention stress shows exactly one winner per run
  across 100 repeated runs; kill -9 the holder -> immediate reacquire by a
  new process; same-process double-open returns WouldBlock while the guard
  lives; a spawned child does NOT inherit the lock fd (O_CLOEXEC asserted);
  the unlink-interleave regression test (A drop, B pending, C fresh-inode)
  proves single-holder; break the lock call to prove the contention test
  bites.

#### Phase 3: Core/display split
**Model:** sonnet
- `process_create_command` | `process_undo_command` | `execute_recovery`
  (+ status/review/doctor read paths as needed by the MCP tool list) split
  into core fns returning structured results and CLI wrappers owning all
  terminal output; cores take explicit params + `Confirmation` input, not
  `&Cli`.
- Surface per-repo diff on `CreateResult` (today computed and discarded).
- Module boundary is named, not vibes: each split lands the core in a
  `src/<mod>/core.rs` submodule; those submodules (and only those) carry
  the deny attributes.
- **Success criteria:** every `src/<mod>/core.rs` carries
  `#![deny(clippy::print_stdout, clippy::print_stderr)]` and CI proves it
  (mechanical, not grep-and-hope); existing e2e output byte-identical; all
  four TTY gates still fail closed on non-TTY.

#### Phase 4: Change::Llm propose
**Model:** opus
- `Change::Llm(prompt)` variant + orchestration-level propose pass;
  temp-worktree generation flow (worktree add --detach, agent under timeout
  with process-group kill, `git add -A` + cached diff, worktree remove in
  all paths); proposal artifact (manifest + display patch + apply blobs);
  `Proposed` status + `CHANGE_STATE_VERSION` bump; `create.llm` config
  (agent-command, timeout-seconds).
- **Success criteria:** propose mutates nothing under the real worktree
  (byte-identical assertion in test); manifest + patches + blobs round-trip
  reload with hashes verifying; agent timeout kills the whole process group
  within tolerance; empty diff recorded as empty outcome, not error;
  payload matrix bites: a binary file round-trips, a mode-only change
  applies, a symlink proposal is a loud per-repo failed outcome naming the
  path.

#### Phase 5: Change::Llm apply
**Model:** opus
- Internal `Change::Patchset` riding `process_single_repo`: proposal blobs
  through the existing backup/write seam; post-pull base_sha drift refusal;
  then the unchanged branch/commit/push/PR pipeline; `gx apply <change-id>`
  verb; proposal retention (cleanup on CleanedUp, doctor reports orphans).
- **Success criteria:** crash-injection points (`GX_CRASH_POINT`) pass for
  an Llm change identically to a sub change; `gx undo <change-id>` fully
  reverses an applied llm campaign (e2e); `gx undo` on a bare unapplied
  proposal removes artifacts and state locally with zero gh/git-remote
  invocations (asserted, not assumed); drifted base_sha refuses per-repo
  with a loud error and touches nothing; a blob tampered after propose is
  refused by hash check with nothing written.

#### Phase 6: CLI surface + present gate
**Model:** sonnet
- `llm` subcommand under `gx create` (clap), `--propose`, `gx apply`;
  present step renders per-repo diffs + fleet summary; confirm gate #5 with
  `--yes`, fail-closed non-TTY.
- Docs: README section; annotated config example.
- **Success criteria:** non-TTY without `--yes` errors naming `--yes`;
  `--propose` then `gx apply <change-id>` end-to-end equals the one-shot
  path's result.

#### Phase 7: Chunk A e2e with a fake agent
**Model:** sonnet
- Deterministic fake-agent script as the configured `agent-command` (test
  fixture); e2e matrix: happy path, garbage patch, agent nonzero, timeout,
  empty diff, drift-then-refuse, undo-after-apply.
- **Success criteria:** every failure mode is a loud per-repo error with the
  real worktree byte-identical after; the happy-path e2e survives
  `GX_CRASH_POINT` injection at each phase stamp.

#### Phase 8: Workspace conversion + gx-mcp scaffold
**Model:** sonnet
- Convert to `[workspace]`; `gx-mcp` bin member (rmcp + tokio + schemars via
  `cargo add`); empty server that serves zero tools over stdio; `otto ci`
  adapted for the workspace.
- **Success criteria:** both crates build and the existing suite is green
  unchanged; the gx bin package keeps the name `gx` (config dir derives
  from `CARGO_PKG_NAME`, `src/config.rs:239`, so `~/.config/gx/gx.yml`
  must keep resolving); `cargo run`/`cargo install --path .` at the root
  still build the gx bin (`default-members`); `.otto.yml`, `ci.yml`, and
  `binary-release.yml` paths to `target/release/gx` still resolve; `bump`
  updates the workspace version under the single flat `v*` tag; an MCP
  client handshake against `gx-mcp` succeeds (initialize + empty tool
  list).

#### Phase 9: MCP server tools
**Model:** opus
- Tool router with the curated surface; cores under `spawn_blocking`;
  per-tool `enabled:` gating (read-only default true, mutating default
  false); confirm-token protocol; file-only logging.
- **Success criteria:** a real MCP client lists and calls a read-only tool;
  a mutating tool with `enabled: false` (default) is absent/refused; a
  mutating call is refused for EACH of: missing token, stale token,
  manifest changed since plan, blob changed since plan, state changed
  concurrently between plan and execute; stdout carries only JSON-RPC bytes
  (asserted by a transcript-capture test).

#### Phase 10: Chunk B e2e + shakedown
**Model:** sonnet
- e2e: scripted MCP client drives propose -> read diffs -> apply -> undo on
  a fixture fleet (tmp repos, never `~/repos`).
- `/cli-shakedown` of the new CLI surface; operator step called out
  explicitly: registering `gx-mcp` in a client's MCP config is manual.
- **Success criteria:** the scripted client completes the full campaign
  round-trip; shakedown doc committed under `docs/`.

## Acceptance Criteria

- [ ] e2e proves: `gx create ... llm` on a fixture fleet produces per-repo
  diffs, applies on confirm, records state, and `gx undo <change-id>`
  reverses the campaign completely (branches + PRs gone, state trued up).
- [ ] A persisted proposal applies later via `gx apply <change-id>` with no
  regeneration; a drifted repo refuses loudly and is untouched.
- [ ] Every chunk-A failure mode (agent nonzero, timeout, garbage patch)
  leaves the real worktree byte-identical and reports a loud per-repo error.
- [ ] An MCP client over stdio can run read-only tools but cannot execute
  any mutating tool without both `enabled: true` config and a valid confirm
  token; a captured transport transcript contains only JSON-RPC.
- [ ] Lock stress: N repeated contention runs produce exactly one winner
  each; kill -9 of a holder is followed by immediate reacquisition.

## Resolved Decisions

- **One combined doc** (Scott, 2026-07-12), overriding the shipped doc's
  "separate design doc" per chunk. Rationale: they interlock (MCP is the
  natural driver for LLM changes) and stress the same concurrency/safety
  seams; phases keep the chunks independently shippable anyway.
- **Lock switch to `File::try_lock()`** decided in this doc; delegation
  recorded in the 2026-07-12 handoff ("Decide in your design whether to
  switch the primitive"). Prior rejection was procedural, not on merit.
- **`gx-mcp` workspace member, not `mcp-io-rs` shared crate.** The Non-Goals
  name implied a shared MCP I/O crate; house rule "write as if more are
  coming, but only implement one" says extract the shared crate when a
  second consumer exists.
- **Agent edits a temp worktree; gx computes the diff.** Never ask the agent
  to emit a patch format; never let it touch the real worktree.
- **Diff for display, blobs for apply.** The proposal persists full
  post-change file contents; gx never applies hunks to the real worktree.
- **MCP mutating tools: config-disabled by default AND token-gated.** Two
  independent gates; enabling a tool never removes the plan step.
- **Panel review folded, 2026-07-12** (Architect/Gemini + Staff
  Engineer/Codex; reconciled by the panel with code verification). All four
  must-fixes accepted: token now hash-binds every blob (manifest is the
  canonical reviewed object, apply verifies per-blob sha256 under lock);
  payload fidelity matrix decided (regular files + modes in, symlinks |
  gitlinks | non-UTF-8 paths rejected loudly at propose); lock switch
  corrected (non-truncating open everywhere, `Drop` never unlinks, fd
  lifetime + O_CLOEXEC asserted by tests); `Proposed` gets a local-only
  undo arm. Panel affirmations on record: propose->apply preserves the
  safety spine, post-pull drift check is correctly placed with no local
  TOCTOU under `RepoLock`, and the agent-not-sandboxed park is a settled,
  reasoned decision (not to be re-raised).

## Alternatives Considered

### Agent emits a patch directly
- **Description:** prompt the agent CLI to output a unified diff; gx applies it.
- **Pros:** no temp worktree; one process, no git plumbing.
- **Cons:** patch-format fidelity from a stochastic generator is exactly the
  failure mode to avoid; hunks drift, context lines lie, `git apply` rejects
  or (worse) fuzzes.
- **Why not chosen:** letting the agent edit real files and diffing the
  result makes the patchset deterministic regardless of agent output
  quality. The agent does what agents are good at; git does what git is
  good at.

### Agent edits the real worktree under transaction backups
- **Description:** run the agent in the actual repo, registering backups first.
- **Cons:** can't register write-ahead backups for files you can't predict;
  violates the spine's ordering invariant; an agent wandering the real
  worktree is the nightmare scenario.
- **Why not chosen:** per-repo isolation was the stated requirement in both
  deferrals.

### Patchset stored inside ChangeState JSON
- **Cons:** patchsets can be large; state files stay small, scannable, and
  cheap to load for `status`/`review`.
- **Why not chosen:** sibling artifact files with a manifest; state holds
  the pointer.

### Implicit `yes` on MCP mutating tools
- **Description:** MCP tools pass the equivalent of `--yes` per call.
- **Cons:** fail-open; a driver never has to look at what it is about to do.
- **Why not chosen:** two-step plan -> confirm-token forces every mutation
  through a plan the client provably received. Writes impossible by default.

### JSON output mode instead of an MCP server
- **Description:** after the core/display split, add `--format json` and
  let agents drive the plain CLI.
- **Pros:** no new crate, no tokio, no protocol surface.
- **Cons:** no tool discovery/schemas, no per-tool gating, confirmation
  degenerates to `--yes` (fail-open), every driver reinvents parsing.
- **Why not chosen:** Scott asked for MCP capability by name (mcp-io-rs,
  both the Non-Goals entry and the 2026-07-11 session question). The
  core/display split makes a JSON output mode cheap to add later, but it is
  not in scope.

### Keep the rename-based reclaim
- **Pros:** shipped, tested, no churn.
- **Cons:** observed 2-winner under contention; reclaim complexity exists
  only to compensate for a primitive the OS provides properly.
- **Why not chosen:** agent-driven concurrency is this doc's premise;
  "airtight" was the stated bar in the phase-7 notes.

### Two separate design docs
- **Why not chosen:** Scott chose combined, 2026-07-12. Recorded so the
  shipped doc's "separate" wording is not re-litigated.

## Technical Considerations

### Dependencies
- Chunk A: zero new crates (`std::process::Command`, `similar`, `tempfile`
  already present).
- Chunk B: `rmcp`, `tokio`, `schemars` via `cargo add` (never pinned from
  memory; both local precedents pin different rmcp versions).
- Rust >= 1.89 for `File::try_lock` (MSRV note in Cargo.toml if one is
  declared).

### Performance
- Propose is agent-latency-bound (minutes-per-repo possible); rayon
  parallelism as today, `jobs` config applies. Timeout default 300s
  per repo.
- Temp worktrees are `git worktree add` (cheap, shares object store), not
  full clones.

### Security
- The agent runs with ambient user credentials in a throwaway worktree of
  already-local repos. gx captures ONLY the worktree diff; the present gate
  shows every byte before anything lands, and push/PR sit behind confirm.
  Prompt-injection blast radius for the CAPTURED change = a bad diff you
  get to read first.
- Stated honestly: the agent process itself is not sandboxed. A temp
  worktree shares the repo's object store, and the agent holds the user's
  ambient credentials, so a hostile prompt could in principle make the
  agent act outside the worktree (its own `git push`, network calls). This
  is the same trust level as running `claude -p` in your shell, which is
  the status quo this feature wraps. Sandboxing the agent (network-off,
  restricted FS) is parked: revisit if gx ever runs prompts not authored by
  the operator.
- MCP server is local stdio only: no port, no network surface, client
  spawns it. Mutating tools default-disabled in config AND token-gated.
- Tokens/creds: gh token flow unchanged (`token-path` per org); nothing new
  stored.

### Testing Strategy
- House shape: unit tests in `src/<mod>/tests.rs`, e2e in `tests/` spawning
  the real binary, poison-tolerant `crate::test_utils::env_lock()` for env
  tests, inert-unless-set hooks (`GX_CRASH_POINT`, `GX_TEST_LOCK_DELAY_MS`).
- Fake agent = deterministic script fixture configured as `agent-command`;
  no live LLM in CI, Phase 0 covers the live proof once.
- Tests must bite: break the lock to prove the contention test fails; break
  the token check to prove the MCP refusal test fails.

### Rollout Plan
- Single repo, single flat `v*` tag. Phases land as individual commits, each
  `otto ci` green; ship via the standard bump flow when both chunks (or a
  coherent prefix: phases 0-7 are a shippable chunk-A release) are done.
- Operator steps: config additions are opt-in (llm defaults work without
  config; MCP mutating tools require explicit enable); registering gx-mcp
  in an MCP client config is manual and documented.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Agent CLI behavior drifts (flags, output) | Med | Med | agent-command is config, not code; Phase 0 records the working template; fake agent isolates CI |
| Patchset applies but is semantically wrong | Med | Med | present-gate shows full diffs before apply; undo reverses the whole campaign; per-repo granularity |
| rmcp API churn between design and build | Med | Low | `cargo add` current at build time; precedent repos show both old and new idioms; scaffold phase absorbs it |
| `File::try_lock` platform semantics surprise | Low | High | two-process contention test + kill-9 test are the proof; non-Linux keeps fail-fast without reclaim |
| Workspace conversion breaks tooling (otto, bump, build.rs) | Low | Med | dedicated scaffold phase with full-suite green as its gate; single flat tag unchanged |
| Temp worktree leaks on crash | Med | Low | worktrees under a gx-owned tmp root; `gx doctor` reports leftovers; `git worktree prune` on cleanup |

## Open Questions

None. (Lock switch, crate name, confirmation model, patchset custody,
timeout scope, and one-vs-two docs closed in Resolved Decisions; all seven
panel findings dispositioned; the `deny_unknown_fields` escalation was
answered by Scott, 2026-07-12: yes, it rides Phase 1.)

## References

- `docs/design/2026-07-11-rollback-undo-hardening.md` (+ implementation
  notes; Non-Goals lines 93-96; lock notes lines 664-671)
- `docs/design/2026-06-11-workflow-safety-hardening.md` (Non-Goals line 72)
- `docs/shakedown-v0.4.0.md`
- Session handoff: `/tmp/gx-handoff-llm-mcp-design.md` (2026-07-12)
- Precedent: `~/repos/scottidler/multi-account-github-mcp` (rmcp server
  wrapping a CLI), `~/repos/scottidler/second-brain` oracle crate +
  `docs/design/2026-06-06-configurable-retrieval-pipeline.md` (config-gated
  MCP methods)
- Seams: `src/create.rs`, `src/transaction.rs`, `src/undo.rs`,
  `src/state.rs`, `src/lock.rs`, `src/github.rs`, `src/config.rs`,
  `build.rs`
