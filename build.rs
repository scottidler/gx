// Simple pattern for git describe -> version
use std::process::Command;

fn main() {
    let git_describe = Command::new("git")
        .args(["describe", "--tags", "--always"])
        .output()
        .and_then(|output| {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                Err(std::io::Error::other("git describe failed"))
            }
        })
        .unwrap_or_else(|_| {
            // Fallback to Cargo.toml version when git describe fails
            env!("CARGO_PKG_VERSION").to_string()
        });

    println!("cargo:rustc-env=GIT_DESCRIBE={git_describe}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
    // `bump --tag-only` (and any tag-only push) writes the new tag straight to
    // packed-refs rather than a loose ref under .git/refs/tags/, so without
    // this trigger a tag-only release rebuilds with a stale GIT_DESCRIBE.
    println!("cargo:rerun-if-changed=.git/packed-refs");
}
