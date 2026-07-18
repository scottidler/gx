use super::*;
use local::hash::sha256_hex;
use tempfile::TempDir;

fn sample_manifest() -> ProposalManifest {
    ProposalManifest::new(
        "GX-test".to_string(),
        "make it better".to_string(),
        "fake-agent --flag".to_string(),
        vec![
            RepoProposal {
                slug: "org/zeta".to_string(),
                base_sha: "sha-zeta".to_string(),
                outcome: ProposalOutcome::Proposed,
                error: None,
                files: vec![
                    FileEntry {
                        path: "b.txt".to_string(),
                        action: FileAction::Modify,
                        mode: "100644".to_string(),
                        sha256: Some(sha256_hex(b"bbb")),
                        size: 3,
                    },
                    FileEntry {
                        path: "a.txt".to_string(),
                        action: FileAction::Add,
                        mode: "100644".to_string(),
                        sha256: Some(sha256_hex(b"aaaa")),
                        size: 4,
                    },
                ],
            },
            RepoProposal {
                slug: "org/alpha".to_string(),
                base_sha: "sha-alpha".to_string(),
                outcome: ProposalOutcome::Empty,
                error: None,
                files: vec![],
            },
        ],
    )
}

#[test]
fn test_manifest_new_sorts_repos_and_files() {
    let m = sample_manifest();
    // Repos sorted by slug.
    assert_eq!(m.repos[0].slug, "org/alpha");
    assert_eq!(m.repos[1].slug, "org/zeta");
    // Files within a repo sorted by path.
    assert_eq!(m.repos[1].files[0].path, "a.txt");
    assert_eq!(m.repos[1].files[1].path, "b.txt");
}

#[test]
fn test_compute_token_is_truncated_and_deterministic() {
    let bytes = b"canonical manifest bytes";
    let t1 = compute_token(bytes);
    let t2 = compute_token(bytes);
    assert_eq!(t1, t2, "token must be deterministic over the same bytes");
    assert_eq!(t1.len(), TOKEN_HEX_LEN);
    // It is the prefix of the full SHA-256 hex.
    assert!(sha256_hex(bytes).starts_with(&t1));
}

#[test]
fn test_token_changes_when_a_blob_hash_changes() {
    // The token binds every blob: flip one file's sha256 and the token differs
    // (this is the property apply relies on to refuse a tampered payload).
    let mut m = sample_manifest();
    let bytes1 = serde_json::to_vec_pretty(&m).unwrap();
    let token1 = compute_token(&bytes1);

    m.repos[1].files[0].sha256 = Some(sha256_hex(b"TAMPERED"));
    let bytes2 = serde_json::to_vec_pretty(&m).unwrap();
    let token2 = compute_token(&bytes2);

    assert_ne!(
        token1, token2,
        "changing a blob's sha256 must invalidate the token"
    );
}

#[test]
fn test_write_manifest_roundtrips_with_token_verifying() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("GX-test");
    let m = sample_manifest();

    let (path, token) = write_manifest(&dir, &m).unwrap();
    assert!(path.exists(), "manifest.json must exist");

    // Reload the exact persisted bytes and confirm the token reproduces (the
    // apply-side verification Phase 5 performs).
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        compute_token(&bytes),
        token,
        "token must re-derive from the persisted manifest bytes"
    );

    // Round-trip deserialize.
    let back: ProposalManifest = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, m, "manifest must round-trip byte-for-byte");
    assert_eq!(back.version, PROPOSAL_MANIFEST_VERSION);
}

#[test]
fn test_blob_roundtrips_and_hash_verifies() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("GX-blob");
    let content = b"blob payload bytes";
    let slug = "org/repo";

    write_blob(&dir, slug, "sub/dir/file.txt", content).unwrap();
    let bpath = blob_path(&dir, slug, "sub/dir/file.txt");
    assert!(bpath.exists());

    let read_back = std::fs::read(&bpath).unwrap();
    assert_eq!(read_back, content, "blob must round-trip");
    assert_eq!(
        sha256_hex(&read_back),
        sha256_hex(content),
        "reloaded blob hash must verify against the manifest's recorded hash"
    );
}

#[test]
fn test_binary_blob_roundtrips_byte_identical() {
    // A binary (non-UTF-8) blob must survive the write/read path unchanged - no
    // lossy UTF-8 round-trip on the payload.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("GX-bin");
    let content: Vec<u8> = (0u8..=255).collect();

    write_blob(&dir, "org/repo", "data.bin", &content).unwrap();
    let read_back = std::fs::read(blob_path(&dir, "org/repo", "data.bin")).unwrap();
    assert_eq!(read_back, content);
}

#[test]
fn test_manifest_deny_unknown_fields() {
    // A stray key fails loudly (fail closed), matching the state file's contract.
    let json = r#"{
        "version": 1,
        "change_id": "x",
        "prompt": "p",
        "agent_command": "a",
        "created_at": "2026-07-12T00:00:00Z",
        "repos": [],
        "bogus": true
    }"#;
    let err = serde_json::from_str::<ProposalManifest>(json).unwrap_err();
    assert!(
        err.to_string().contains("bogus") || err.to_string().contains("unknown field"),
        "unexpected error: {err}"
    );
}
