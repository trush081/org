//! `org update` — rebuild and reinstall the binary from its source checkout.
//!
//! With no remote repo yet, "update" means: go to the org source tree, run
//! `cargo install` to rebuild the latest code, and replace the binary on PATH.
//! When the project later publishes to a git URL, only `install_command` changes
//! (point `--path` at `--git <url>` instead) — the rest stays.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the source checkout and run (or print) the install command.
pub fn run(source_flag: Option<PathBuf>, dry_run: bool) -> Result<()> {
    let source = resolve_source(source_flag)?;

    // `cargo install --path` needs a *package* dir, not the virtual workspace
    // root. The `org` binary lives in the `cli` package, so install from there.
    let package = cli_package_dir(&source);
    let (program, args) = install_command(&package);

    if dry_run {
        println!("{program} {}", args.join(" "));
        return Ok(());
    }

    println!("Updating org from {}...", source.display());
    let status = Command::new(program)
        .args(&args)
        .status()
        .with_context(|| format!("running `{program}` — is cargo on your PATH?"))?;

    if !status.success() {
        bail!("update failed: `{program}` exited with {status}");
    }
    println!("org updated. Run `org --version` to confirm.");
    Ok(())
}

/// Find the org source tree (the repo root), in precedence order:
/// 1. `--source <path>` flag,
/// 2. `$ORG_SRC` env var,
/// 3. the workspace root recorded at compile time (so it "just works" when this
///    binary was built from a local checkout that still exists).
fn resolve_source(flag: Option<PathBuf>) -> Result<PathBuf> {
    let candidate = flag
        .or_else(|| std::env::var_os("ORG_SRC").map(PathBuf::from))
        .unwrap_or_else(compiled_workspace_root);

    // Validate it's actually the org checkout by checking for the cli package,
    // not just any Cargo.toml — gives a precise error if the path is wrong.
    if !cli_package_dir(&candidate).join("Cargo.toml").exists() {
        bail!(
            "no org checkout at {} (expected {}/cli/Cargo.toml). Pass --source <path> \
             or set $ORG_SRC to your org checkout.",
            candidate.display(),
            candidate.display()
        );
    }
    Ok(candidate)
}

/// Workspace root as known at build time. `CARGO_MANIFEST_DIR` is the `cli`
/// crate dir; its parent is the workspace root. Baked into the binary, so it
/// points at wherever this build was compiled from.
fn compiled_workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The `cli` package dir inside a checkout — what `cargo install --path` needs.
fn cli_package_dir(source: &Path) -> PathBuf {
    source.join("cli")
}

/// The command that reinstalls `org`. Split out so it's unit-testable and so the
/// future git-based form is a one-line change here.
fn install_command(source: &Path) -> (&'static str, Vec<String>) {
    (
        "cargo",
        vec![
            "install".into(),
            "--path".into(),
            source.to_string_lossy().into_owned(),
            "--bin".into(),
            "org".into(),
            "--force".into(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_targets_the_cli_package() {
        let pkg = cli_package_dir(Path::new("/tmp/org"));
        let (program, args) = install_command(&pkg);
        assert_eq!(program, "cargo");
        // It must install from the cli package dir (not the workspace root),
        // forcing a replace of the existing binary.
        assert!(args.contains(&"--path".to_string()));
        assert!(args.contains(&"/tmp/org/cli".to_string()));
        assert!(args.contains(&"--force".to_string()));
    }

    #[test]
    fn resolve_source_rejects_a_non_checkout() {
        // A dir with no cli/Cargo.toml should be rejected with a helpful error.
        let err = resolve_source(Some(PathBuf::from("/"))).unwrap_err();
        assert!(err.to_string().contains("no org checkout"));
    }

    #[test]
    fn compiled_root_resolves_to_a_real_cli_package() {
        // The path baked in at build time must contain the cli package, which
        // is what `cargo install --path` will be pointed at.
        let root = compiled_workspace_root();
        assert!(cli_package_dir(&root).join("Cargo.toml").exists());
    }
}
