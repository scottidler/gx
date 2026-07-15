# gx Shakedown + Production-Readiness Audit: v0.6.1

**Date:** 2026-07-13
**Scope:** CLI shakedown, MCP shakedown, and a cross-org|user credential audit answering: is gx production-ready, safe, and secure for a single SRE making N changes across N repos, including across org|user boundaries?

## Summary verdict

- **Single-org-per-run (all of `~/repos/scottidler/` OR all of `~/repos/tatari-tv/`): production-ready.** Finish-line ops fail closed, reporting is scriptable, subprocess hangs can't wedge a run, MCP is read-only by default. Proven by tests (5x green, break-the-guard) + live shakedown.
- **Mixed-org run (one invocation spanning both `scottidler/*` and `tatari-tv/*`): token side proven safe by code; SSH side safe only by account topology, not by code.** No wrong-identity WRITE is possible on either path. Two residual caveats remain (below).
- **Not "bulletproof."** Two consciously-parked non-goals turn out to be exactly the cross-boundary case, and the code supports that case even though the design doc assumed it wouldn't happen.

## What is proven safe (by code + tests)

| Area | Status | Evidence |
|---|---|---|
| Finish-line confirm gate (`review approve`/`delete`, `cleanup`) fails closed naming `--yes` on non-interactive stdin | ✅ | `confirm_destructive`, break-the-guard tests |
| Preflight-complete-or-abort (one org's discovery error → zero mutations) | ✅ | `discover_all_prs`, `test_discover_all_prs_aborts_whole_batch_when_one_org_errors` |
| `review approve` skips non-mergeable PRs (fail closed on UNKNOWN/CONFLICTING) | ✅ | `Mergeability` enum, `Skipped` state, test |
| `cleanup` proves merge via fetched `origin/<base>` ancestry before `-D` | ✅ | `test_cleanup_preserves_branch_with_commits_absent_from_base` |
| `create` exits non-zero on any repo failure + `--report <path>` JSON | ✅ | `tests/e2e_reporting.rs` |
| Subprocess wall-clock timeout + process-group kill (no wedge) | ✅ | `run_checked`, `subprocess/tests.rs` |
| Panic hook emits ERROR diagnostic | ✅ | `tests/e2e_reporting.rs` |
| **Token resolution is PER-REPO** (each `gh` call resolves the token from that repo's org, freshly; no cache, no process-wide env mutation; missing token = loud fail-closed) | ✅ | `org_of` → `gh_command` → `persona::resolve_token_env`, `github.rs:151-168`, `persona.rs:84-117` |
| No shared/ambient state leak between repos in a run (per-invocation `.env()`, not `set_var`) | ✅ | `github.rs:163-168` |
| MCP fail-closed by default (only read-only tools; mutating tools "tool not found" until explicitly enabled) | ✅ | live: 6 read-only tools; `review-approve`/`create-apply` → `-32602 tool not found` |

## The two residual gaps (the honest caveats for a mixed-org run)

### 1. SSH is NOT per-repo — safe only by account topology, not by code (parked non-goal)
gx forces ONE ambient SSH command onto clone/push/fetch (`GIT_SSH_COMMAND`), read from gx's own cwd, and it OVERRIDES any per-org key already stamped into a repo's local `core.sshCommand` (`ssh.rs:71-89`, `git.rs:790,1225-1230,1582-1586`). The clone URL is always bare `git@github.com:` — no per-org host alias (`ssh.rs:29`).

The design doc's fail-closed claim ("a wrong SSH key is rejected at GitHub, never a wrong-identity write") is TRUE — but because the home (`scottidler`) and work (`escote-tatari`) accounts have **disjoint write access**, so a mismatched key is rejected server-side. That safety is a property of the account topology, **not of gx's code**. It would NOT hold for any repo both accounts can write to (commit authorship is separately governed by `user.name`/`user.email`, which gx never sets).

### 2. `GH_PERSONA` collapses a mixed run onto one persona (parked non-goal)
Per-repo token resolution is defeated if `$GH_PERSONA` is set: precedence #1 forces that single persona for ALL orgs (`persona.rs:88-92`). In a mixed-org run, that means home repos get the work token (or vice versa) → 404s that read as "no access." This is the exact footgun the hardening doc parked as the "`GH_PERSONA` mixed-fleet warning" non-goal — and Scott's own `secrets.md` workflow actively uses `GH_PERSONA=work`/`home`.

## RESOLVED (2026-07-14): cross-org|user boundary is a PERMANENT non-goal

> **gx WILL NEVER SUPPORT OPERATING ACROSS AN ORG|USER BOUNDARY IN A SINGLE RUN.**
> All-`scottidler/*` OR all-`tatari-tv/*` per invocation -- **NEVER both.**
> This is decided and closed. Do not reopen it, do not un-park the two gaps
> below, do not surface it as a caveat in future readiness assessments.

Owner decision, stated flatly (2026-07-14). The two "residual gaps" above (SSH
not-per-repo, `GH_PERSONA` collapse) only manifest in a mixed-org run, which is
a non-goal by fiat -- so they are **not gaps to fix**, they are behaviors
outside the supported envelope. The fact that the code incidentally *permits* a
mixed-org walk is irrelevant: the owner does not do it, and gx makes no promise
about it. Single-org-per-run is the one and only supported mode, and gx is
production-ready there.

## What was NOT found (no manufactured issues)
- No wrong-identity WRITE vector on either token or SSH path.
- No fail-open in any finish-line op.
- No shared-state leak between parallel repo workers.
- No MCP mutating tool reachable without explicit opt-in.

## Shakedown coverage note (faithful)
- CLI: v0.6.1 verified; command tree mapped; read-only paths (`doctor`, `review ls`, `cleanup --list`) run live and clean. The mutating finish-line ops were NOT run live against real repos (read-only-by-default + safety); their fail-closed behavior is proven by the verified break-the-guard tests, not a live mutation.
- MCP: live stdio session — handshake, `tools/list`, read-only `doctor` call, and two disabled-mutating tool calls (both refused).

## Recommendation
None outstanding. The two mixed-org items above are CLOSED as permanent non-goals
(see "RESOLVED" section) -- gx is not required to guard a scenario the owner will
never run. For single-org-per-run (the only supported mode), the tool is
production-ready as proven by the table above.
