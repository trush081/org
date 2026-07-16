//! `org update` — rebuild and reinstall the binary.
//!
//! Default: pull the latest from the GitHub repo and `cargo install` it, so an
//! installed copy can update itself with no local checkout. `--local` installs
//! from a source tree instead (handy for testing unpushed changes).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The canonical repo `org update` pulls from by default.
const REPO_URL: &str = "https://github.com/trush081/org.git";

/// The commit this binary was built from, embedded by `build.rs`. `"unknown"`
/// for binaries built before this existed, or built without a `.git` dir
/// present (e.g. from a source tarball).
const LOCAL_SHA: &str = env!("ORG_GIT_SHA");

/// Run (or print) the appropriate install command.
///
/// `local` selects install-from-checkout mode; `source` optionally points that
/// at a specific checkout (otherwise it's auto-resolved). In the default
/// (non-local) mode we install straight from [`REPO_URL`].
///
/// `check` skips the install entirely and just reports whether an update is
/// available. Ignored in local mode, where there's no fixed "latest" to
/// compare against.
pub fn run(local: bool, source: Option<PathBuf>, dry_run: bool, check: bool) -> Result<()> {
    // A --source path implies local mode (you can't point a git install at a dir).
    let local = local || source.is_some();

    if !local {
        match remote_sha(REPO_URL) {
            Ok(remote) if remote == LOCAL_SHA => {
                println!("org is already up to date ({}).", short(&remote));
                return Ok(());
            }
            Ok(remote) => {
                println!(
                    "Update available: {} -> {}.",
                    short(LOCAL_SHA),
                    short(&remote)
                );
                if check {
                    return Ok(());
                }
            }
            Err(e) => {
                // Can't tell without a network round-trip to GitHub; don't block
                // the update on it, just say so and fall through to installing.
                println!("Couldn't check for updates ({e}); updating anyway.");
                if check {
                    return Ok(());
                }
            }
        }
    }

    let (program, args, what) = if local {
        let checkout = resolve_source(source)?;
        // `cargo install --path` needs a *package* dir, not the virtual
        // workspace root. The `org` bin lives in the `cli` package.
        let package = cli_package_dir(&checkout);
        (
            "cargo",
            install_from_path(&package),
            format!("local checkout {}", checkout.display()),
        )
    } else {
        ("cargo", install_from_git(REPO_URL), REPO_URL.to_string())
    };

    if dry_run {
        println!("{program} {}", args.join(" "));
        return Ok(());
    }

    println!("Updating org from {what}...");
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

/// Ask GitHub for the remote's current `HEAD` commit SHA, without cloning.
/// `git ls-remote` just does the ref-advertisement round-trip of the fetch
/// protocol, so it's fast even though it hits the network.
fn remote_sha(url: &str) -> Result<String> {
    let output = Command::new("git")
        .args(["ls-remote", url, "HEAD"])
        .output()
        .context("running `git ls-remote` — is git on your PATH?")?;

    if !output.status.success() {
        bail!(
            "git ls-remote {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sha = stdout
        .split_whitespace()
        .next()
        .with_context(|| format!("unexpected output from `git ls-remote {url} HEAD`"))?;
    Ok(sha.to_string())
}

/// First 7 chars of a commit SHA, for display.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// Find the org source tree (the repo root) for `--local`, in precedence order:
/// 1. `--source <path>` flag,
/// 2. `$ORG_SRC` env var,
/// 3. the workspace root recorded at compile time (works when this binary was
///    built from a local checkout that still exists).
fn resolve_source(flag: Option<PathBuf>) -> Result<PathBuf> {
    let candidate = flag
        .or_else(|| std::env::var_os("ORG_SRC").map(PathBuf::from))
        .unwrap_or_else(compiled_workspace_root);

    // Validate it's actually the org checkout by checking for the cli package.
    if !cli_package_dir(&candidate).join("Cargo.toml").exists() {
        bail!(
            "no org checkout at {} (expected {}/cli/Cargo.toml). Pass --source <path> \
             or set $ORG_SRC, or drop --local to update from GitHub.",
            candidate.display(),
            candidate.display()
        );
    }
    Ok(candidate)
}

/// Workspace root as known at build time. `CARGO_MANIFEST_DIR` is the `cli`
/// crate dir; its parent is the workspace root.
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

/// `cargo install` args to build from a local package directory.
fn install_from_path(package: &Path) -> Vec<String> {
    vec![
        "install".into(),
        "--path".into(),
        package.to_string_lossy().into_owned(),
        "--bin".into(),
        "org".into(),
        "--force".into(),
    ]
}

/// `cargo install` args to build from the git repo. For a virtual workspace the
/// package is named as a positional CRATE argument (`org`) — `cargo install`
/// has no `-p` flag; the root manifest isn't itself a package.
fn install_from_git(url: &str) -> Vec<String> {
    vec![
        "install".into(),
        "--git".into(),
        url.into(),
        "org".into(), // positional CRATE: which package to install from the repo
        "--bin".into(),
        "org".into(),
        "--force".into(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_install_targets_the_cli_package() {
        let pkg = cli_package_dir(Path::new("/tmp/org"));
        let args = install_from_path(&pkg);
        assert!(args.contains(&"--path".to_string()));
        assert!(args.contains(&"/tmp/org/cli".to_string()));
        assert!(args.contains(&"--force".to_string()));
    }

    #[test]
    fn git_install_names_repo_and_package() {
        let args = install_from_git(REPO_URL);
        assert!(args.contains(&"--git".to_string()));
        assert!(args.contains(&REPO_URL.to_string()));
        // Package named as a positional CRATE arg (cargo install has no -p).
        assert!(!args.contains(&"-p".to_string()));
        assert!(args.contains(&"org".to_string()));
        assert!(args.contains(&"--force".to_string()));
    }

    #[test]
    fn short_truncates_to_seven_chars() {
        assert_eq!(short("1eab65cba421a9bc2670d0945cec74e906e408ab"), "1eab65c");
    }

    #[test]
    fn short_passes_through_shorter_strings() {
        assert_eq!(short("unknown"), "unknown");
        assert_eq!(short("abc"), "abc");
    }

    #[test]
    fn remote_sha_reads_the_real_repo() {
        // Hits the network (github.com) — same class of dependency `cargo
        // install --git` already has, just cheaper (ref advertisement only).
        let sha = remote_sha(REPO_URL).expect("git ls-remote should succeed");
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn resolve_source_rejects_a_non_checkout() {
        let err = resolve_source(Some(PathBuf::from("/"))).unwrap_err();
        assert!(err.to_string().contains("no org checkout"));
    }

    #[test]
    fn compiled_root_resolves_to_a_real_cli_package() {
        let root = compiled_workspace_root();
        assert!(cli_package_dir(&root).join("Cargo.toml").exists());
    }
}
