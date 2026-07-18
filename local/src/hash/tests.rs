use super::*;

// FIPS 180-4 known-answer vectors: these BITE - a single wrong constant,
// rotation, or padding byte changes the digest, so a broken impl fails here.

#[test]
fn test_sha256_empty() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn test_sha256_abc() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn test_sha256_two_block_message() {
    // 56 bytes: forces a second padding block (length straddles the 448-bit
    // boundary), exercising the multi-chunk path.
    let msg = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    assert_eq!(
        sha256_hex(msg),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}

#[test]
fn test_sha256_binary_bytes() {
    // Non-UTF-8 bytes hash fine (blobs can be binary, design payload matrix).
    let bytes: Vec<u8> = (0u8..=255).collect();
    let hex = sha256_hex(&bytes);
    assert_eq!(hex.len(), 64);
    // Deterministic across runs.
    assert_eq!(hex, sha256_hex(&bytes));
    // Known digest for the 0..=255 byte sequence.
    assert_eq!(
        hex,
        "40aff2e9d2d8922e47afd4648e6967497158785fbd1da870e7110266bf944880"
    );
}

#[test]
fn test_sha256_hex_len_and_lowercase() {
    let hex = sha256_hex(b"gx");
    assert_eq!(hex.len(), 64);
    assert!(hex
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}
