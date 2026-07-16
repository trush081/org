//! Embeds the git commit SHA this binary was built from, so `org update` can
//! compare it against the remote's latest commit without recompiling first.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=ORG_GIT_SHA={sha}");
    // Workspace root's .git lives one level up from this (cli) package.
    println!("cargo:rerun-if-changed=../.git/HEAD");
}
