//! Mechanical regression guard for `build.rs`'s `cargo:rerun-if-changed`
//! directives - not a full build, but proves the packed-refs trigger is
//! present so a future edit can't silently drop it.
//!
//! Manual verification of the underlying scenario (recorded in
//! `docs/design/2026-07-12-llm-propose-apply-and-mcp-server-implementation-notes.md`,
//! Phase 1): in a throwaway repo, tagging then `git pack-refs --all` removes
//! the loose ref file under `.git/refs/tags/` entirely and only
//! `.git/packed-refs` changes mtime - proving `cargo:rerun-if-changed=.git/refs/`
//! alone misses a tag-only release, and that the new `.git/packed-refs`
//! directive is the fix.

#[test]
fn build_rs_watches_packed_refs() {
    let build_rs =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/build.rs")).unwrap();
    assert!(
        build_rs.contains("cargo:rerun-if-changed=../.git/packed-refs"),
        "build.rs must rerun on .git/packed-refs changes, or a tag-only release \
         (bump --tag-only writing straight to packed-refs) embeds a stale GIT_DESCRIBE"
    );
    // The pre-existing triggers must survive alongside the new one.
    assert!(build_rs.contains("cargo:rerun-if-changed=../.git/HEAD"));
    assert!(build_rs.contains("cargo:rerun-if-changed=../.git/refs/"));
}
