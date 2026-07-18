#!/usr/bin/env bash
# check-local-boundary.sh -- biting boundary guard for the `local` crate.
#
# The `local` crate is credential-free by construction (Track B0 of the gx
# lib decomposition). This guard FAILS if any credential-bound or network-git
# construct appears under local/src, so the intel-catalog cross-org boundary
# (Track B1) stays CI-structural rather than a convention. `cargo tree` alone is
# insufficient -- it misses source-level `Command::new("gh")` shell-outs and raw
# git network verbs. See docs/design/2026-07-17-gx-lib-decomposition.md (Phase 2).
#
# Exit non-zero on the first violation class found (prints every offending line).
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/local/src"

if [ ! -d "$SRC" ]; then
  echo "check-local-boundary: $SRC not found" >&2
  exit 2
fi

status=0

report() {
  echo "BOUNDARY VIOLATION -- $1:"
  echo "$2"
  echo ""
  status=1
}

# 1. Credential modules (ssh/github/persona) and `gh` shell-outs must never be
#    reachable from local/src -- not even from test helpers.
creds=$(grep -rnE 'Command::new\("gh"\)|\b(ssh|github|persona)::' "$SRC" --include='*.rs' || true)
if [ -n "$creds" ]; then
  report "credential module or gh shell-out in local/src" "$creds"
fi

# 2. Remote/network git verbs passed as quoted args. The ONLY legitimate quoted
#    "push" in production local is `git stash push` (excluded by the `stash`
#    match). test_utils.rs is test-only scaffolding that performs a LOCAL
#    `git clone --bare` from a temp path (no network) and is excluded here.
verbs=$(grep -rnE '"(fetch|pull|ls-remote|clone|push)"' "$SRC" --include='*.rs' \
  | grep -v 'stash' \
  | grep -v '/test_utils.rs:' \
  || true)
if [ -n "$verbs" ]; then
  report "remote git network verb in local/src" "$verbs"
fi

if [ "$status" -ne 0 ]; then
  echo "The 'local' crate must stay credential-free: no ssh/github/persona, no"
  echo "gh shell-out, and no fetch/pull/ls-remote/clone/push. Move the offending"
  echo "logic to the remote half (gx::git / Phase 3 remote crate)."
  exit 1
fi

echo "check-local-boundary: local/src is clean (no credential or network-git constructs)"
