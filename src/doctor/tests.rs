use super::*;

#[test]
fn test_version_compare_pads_shorter() {
    // The [A25] fix: "2.20" and "2.20.0" must compare equal (>= true).
    assert!(version_compare("2.20", "2.20.0"));
    assert!(version_compare("2.20.0", "2.20"));
}

#[test]
fn test_version_compare_ordering() {
    assert!(version_compare("2.34.1", "2.20.0"));
    assert!(!version_compare("1.9.0", "2.0.0"));
    assert!(version_compare("2.0.0", "2.0.0"));
    assert!(version_compare("2.0.1", "2.0.0"));
    assert!(!version_compare("2.0.0", "2.0.1"));
}

#[test]
fn test_extract_version() {
    assert_eq!(extract_version("git version 2.34.1"), "2.34.1");
    assert_eq!(extract_version("gh version 2.40.1 (2023-12-13)"), "2.40.1");
    assert_eq!(extract_version("no version here at all xyz"), "here");
    assert_eq!(extract_version(""), "unknown");
}
