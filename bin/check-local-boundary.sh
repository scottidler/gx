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

# 2a. fetch/pull/ls-remote/clone have NO local-only homonym, so any quoted
#     occurrence is a violation -- check unconditionally. test_utils.rs is
#     test-only scaffolding that performs a LOCAL `git clone --bare` from a temp
#     path (no network) and is the one excluded file.
netverbs=$(grep -rnE '"(fetch|pull|ls-remote|clone)"' "$SRC" --include='*.rs' \
  | grep -v '/test_utils.rs:' \
  || true)
if [ -n "$netverbs" ]; then
  report "remote git network verb in local/src" "$netverbs"
fi

# 2b. "push": the ONLY legitimate local push is `git stash push`, where the args
#     are the adjacent tokens "stash", "push". Flag any quoted "push" NOT in that
#     adjacency. This is tighter than a whole-line `grep -v stash`, which a real
#     network verb sharing a line with the word "stash" would evade (audit finding,
#     Track B0 implementation audit 2026-07-18).
pushes=$(grep -rnE '"push"' "$SRC" --include='*.rs' \
  | grep -vE '"stash"[[:space:]]*,[[:space:]]*"push"' \
  | grep -v '/test_utils.rs:' \
  || true)
if [ -n "$pushes" ]; then
  report "remote git push in local/src" "$pushes"
fi

if [ "$status" -ne 0 ]; then
  echo "The 'local' crate must stay credential-free: no ssh/github/persona, no"
  echo "gh shell-out, and no fetch/pull/ls-remote/clone/push. Move the offending"
  echo "logic to the remote half (gx::git / Phase 3 remote crate)."
  exit 1
fi

echo "check-local-boundary: local/src is clean (no credential or network-git constructs)"
