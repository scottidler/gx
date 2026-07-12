use super::*;

#[test]
fn test_colorize_patch_preserves_every_line_in_order() {
    let patch = "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n-old line\n+new line\n context\n";
    let out = colorize_patch(patch);
    assert_eq!(
        out.lines().count(),
        patch.lines().count(),
        "colorizing must not drop or add lines"
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[0].contains("--- a/file"));
    assert!(lines[1].contains("+++ b/file"));
    assert!(lines[2].contains("@@ -1,2 +1,2 @@"));
    assert!(lines[3].contains("-old line"));
    assert!(lines[4].contains("+new line"));
    assert!(lines[5].contains(" context"));
}

#[test]
fn test_colorize_patch_handles_empty_input() {
    assert_eq!(colorize_patch(""), "");
}
