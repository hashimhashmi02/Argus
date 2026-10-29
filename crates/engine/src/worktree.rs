//! Git worktrees for sessions that ask for one.
//!
//! One worktree per branch. Two agents editing one folder overwrite each
//! other's work; two agents in two worktrees of the same repository do not, and
//! each one's changes stay on a branch of its own until someone merges them.
//!
//! Worktrees outlive their sessions. Killing an agent does not delete what it
//! wrote, and a second session on the same branch is put back in the same
//! folder rather than refused.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Long enough for a checkout of a large repository, short enough that a git
/// that is stuck -- on a lock, a hook, a credential prompt that slipped through
/// -- fails the spawn instead of hanging it.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// The folder of the worktree for `branch` of the repository containing
/// `dir`, made under `root` if it does not exist yet.
///
/// The branch is created from the repository's HEAD if there is no such
/// branch. If any worktree already has it checked out -- one Argus made
/// earlier, or the main checkout itself -- that worktree is used: git would
/// refuse a second, and the caller asked to work on that branch.
pub(crate) async fn prepare(dir: &Path, branch: &str, root: &Path) -> Result<PathBuf, String> {
    git(dir, &["check-ref-format", "--branch", branch])
        .await
        .map_err(|_| format!("`{branch}` is not a valid branch name"))?;

    let top = git(dir, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|e| format!("{} is not in a git repository ({e})", dir.display()))?;
    let top = native(Path::new(top.trim()));

    let listing = git(&top, &["worktree", "list", "--porcelain"]).await?;
    if let Some(path) = checked_out_at(&listing, branch) {
        return Ok(native(&path));
    }

    let repo_name = top
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let path = root.join(folder_name(&repo_name)).join(folder_name(branch));
    if path.exists() {
        return Err(format!(
            "{} already exists but is not a worktree of {branch}; remove it or pick another branch",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }

    let target = path.to_string_lossy().into_owned();
    let exists = git(
        &top,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .await
    .is_ok();
    let args: Vec<&str> = if exists {
        vec!["worktree", "add", &target, branch]
    } else {
        vec!["worktree", "add", "-b", branch, &target]
    };
    git(&top, &args).await?;
    Ok(path)
}

/// Git prints `C:/src/app`. Recorded as `C:\src\app`, so the same folder is
/// always the same string -- in the list, in the state file, to a resume.
fn native(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Where `branch` is checked out, from `git worktree list --porcelain`.
fn checked_out_at(listing: &str, branch: &str) -> Option<PathBuf> {
    let want = format!("refs/heads/{branch}");
    let mut path = None;
    for line in listing.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if line.strip_prefix("branch ") == Some(want.as_str()) {
            return path;
        }
    }
    None
}

/// A branch or repository name as one folder name: `feature/login` becomes
/// `feature-login`, and nothing Windows forbids in a path survives.
fn folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    // Windows drops trailing dots and spaces from a folder name, which would
    // make the path recorded here differ from the one on disk.
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        "-".into()
    } else {
        trimmed.into()
    }
}

/// Run git in `dir` and return its standard output.
///
/// Never interactive: no stdin, no terminal prompt, no credential dialog, and a
/// deadline. The user's own git config is kept, though -- a worktree checked
/// out without their `core.autocrlf` would not match their main checkout.
async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // The engine usually runs without a console. Without this, every git call
    // would flash a console window of its own on the user's screen.
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let output = tokio::time::timeout(GIT_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("git {} took longer than {GIT_TIMEOUT:?}", args.join(" ")))?
        .map_err(|e| format!("could not run git: {e}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("git {}: {}", args.join(" "), stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_worktree_a_branch_is_checked_out_in() {
        let listing = "worktree C:/src/app\nHEAD 1111\nbranch refs/heads/main\n\n\
                       worktree C:/wt/app/feature-login\nHEAD 2222\nbranch refs/heads/feature/login\n\n\
                       worktree C:/wt/app/detached\nHEAD 3333\ndetached\n";
        assert_eq!(
            checked_out_at(listing, "feature/login"),
            Some(PathBuf::from("C:/wt/app/feature-login"))
        );
        assert_eq!(
            checked_out_at(listing, "main"),
            Some(PathBuf::from("C:/src/app"))
        );
        assert_eq!(
            checked_out_at(listing, "feature"),
            None,
            "a prefix is not a match"
        );
    }

    #[test]
    fn branch_names_become_safe_folder_names() {
        assert_eq!(folder_name("feature/login"), "feature-login");
        assert_eq!(folder_name("fix:a*b?"), "fix-a-b-");
        assert_eq!(folder_name("v1.2."), "v1.2");
        assert_eq!(folder_name("..."), "-");
    }
}
