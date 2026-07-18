#!/usr/bin/env bash
# check-catalog-boundary.sh -- biting boundary guard for the `catalog` crate.
#
# The intel-catalog cross-org boundary (design doc 2026-07-17-gx-intel-catalog.md,
# Track B1) is compiler-structural: the `catalog` crate depends on `local` ONLY,
# never `remote`, so an intel tool cannot compile a call to persona/github/ssh/
# remote-git. This guard FAILS if `catalog` ever gains a `remote` dependency --
# either declared directly in catalog/Cargo.toml, or appearing anywhere in
# `cargo tree -p catalog`. See the sibling check-local-boundary.sh.
#
# Exit non-zero on the first violation found.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MANIFEST="$ROOT/catalog/Cargo.toml"

if [ ! -f "$MANIFEST" ]; then
  echo "check-catalog-boundary: $MANIFEST not found" >&2
  exit 2
fi

status=0

report() {
  echo "CATALOG BOUNDARY VIOLATION -- $1:"
  echo "$2"
  echo ""
  status=1
}

# 1. A direct `remote` dependency line in catalog/Cargo.toml (deterministic, no
#    network/cargo needed). Matches `remote = "..."` or `remote = { ... }`,
#    including a leading-whitespace or path-key form.
direct=$(grep -nE '^[[:space:]]*remote[[:space:]]*=' "$MANIFEST" || true)
if [ -n "$direct" ]; then
  report "catalog/Cargo.toml declares a 'remote' dependency" "$direct"
fi

# 2. `remote` anywhere in the resolved dependency graph (catches a transitive
#    re-introduction). Best-effort: if cargo tree cannot run in this
#    environment, the deterministic manifest grep above still gates.
if command -v cargo >/dev/null 2>&1; then
  tree=$(CARGO_NET_GIT_FETCH_WITH_CLI=true cargo tree -p catalog 2>/dev/null || true)
  if [ -n "$tree" ]; then
    remote_nodes=$(printf '%s\n' "$tree" | grep -E '(^|[[:space:]])remote v[0-9]' || true)
    if [ -n "$remote_nodes" ]; then
      report "cargo tree -p catalog shows a 'remote' crate node" "$remote_nodes"
    fi
  fi
fi

if [ "$status" -ne 0 ]; then
  echo "The 'catalog' crate must depend on 'local' only, never 'remote': the"
  echo "cross-org intel/operations boundary is compiler-structural. Move any"
  echo "credential/network logic to the remote half, not the catalog crate."
  exit 1
fi

echo "check-catalog-boundary: catalog depends on local only (no 'remote' dependency)"
