//! Git worktree isolation for one sub-agent node.
//!
//! Two nodes that write the same path must not see each other's edits, so each
//! gets its own checkout of the repository on its own branch. `git worktree add`
//! mutates `.git/worktrees` in the shared repository, so **creation against one
//! repository is not safe to run concurrently**: the executor serialises it
//! behind a lock (see `Executor`), and any other caller must do the same.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harness_core::path::canonicalize;
use harness_core::{HarnessError, Result};

/// A checkout created for one node. The executor keeps it when the node
/// succeeded and discards it when the node failed; there is no `Drop` impl, so a
/// kept checkout survives until the caller removes it.
#[derive(Debug)]
pub struct Worktree {
    repo: PathBuf,
    path: PathBuf,
    branch: String,
}

impl Worktree {
    /// `git worktree add <path> -b <branch>` against `repo`. The branch name is
    /// derived from `name` and made unique.
    pub fn create(repo: &Path, name: &str) -> Result<Self> {
        let repo = canonicalize(repo);
        ensure_repository(&repo)?;
        ensure_has_commits(&repo)?;

        let slug = slug_of(name);
        let suffix = unique_suffix();
        let branch = format!("harness/{slug}-{suffix}");
        let path = worktree_base().join(format!("{slug}-{suffix}"));

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(HarnessError::Io)?;
        }

        let output = run_git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_string_lossy().as_ref(),
            ],
        )?;

        if !output.status.success() {
            return Err(HarnessError::Other(format!(
                "`git worktree add` failed for `{}`: {}",
                path.display(),
                last_meaningful_line(&output.stderr)
            )));
        }

        Ok(Self { repo, path, branch })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the worktree and its branch. Best effort: a failure to clean up
    /// is logged, not propagated, because it must not mask the run's real result.
    pub fn remove(self) {
        // `--force` because a node may have left a file open on Windows, which
        // makes an ordinary remove refuse and would strand the checkout.
        let removal = run_git(
            &self.repo,
            &[
                "worktree",
                "remove",
                "--force",
                self.path.to_string_lossy().as_ref(),
            ],
        );
        match removal {
            Ok(output) if output.status.success() => {}
            Ok(output) => tracing::warn!(
                "could not remove worktree `{}`: {}",
                self.path.display(),
                last_meaningful_line(&output.stderr)
            ),
            Err(err) => {
                tracing::warn!("could not remove worktree `{}`: {err}", self.path.display())
            }
        }

        match run_git(&self.repo, &["branch", "-D", &self.branch]) {
            Ok(output) if output.status.success() => {}
            Ok(output) => tracing::warn!(
                "could not delete branch `{}`: {}",
                self.branch,
                last_meaningful_line(&output.stderr)
            ),
            Err(err) => tracing::warn!("could not delete branch `{}`: {err}", self.branch),
        }
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }
}

fn ensure_repository(repo: &Path) -> Result<()> {
    let output = run_git(repo, &["rev-parse", "--git-dir"])?;
    if output.status.success() {
        return Ok(());
    }
    Err(HarnessError::Other(format!(
        "`{}` is not a git repository, so a worktree cannot be created there",
        repo.display()
    )))
}

fn ensure_has_commits(repo: &Path) -> Result<()> {
    let output = run_git(repo, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    if output.status.success() {
        return Ok(());
    }
    Err(HarnessError::Other(format!(
        "`{}` has no commits yet, so there is no revision to base a worktree on",
        repo.display()
    )))
}

fn run_git(repo: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| {
            HarnessError::Other(format!("could not run git in `{}`: {err}", repo.display()))
        })
}

/// Where worktrees are placed. Kept out of the repository so a checkout does not
/// appear as untracked content in the tree it was made from.
fn worktree_base() -> PathBuf {
    std::env::temp_dir().join("harness-worktrees")
}

fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "node".to_string()
    } else {
        slug
    }
}

/// A suffix unique across processes and within one: branch names and paths must
/// not collide when two nodes are created in the same nanosecond.
fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}-{counter}", std::process::id())
}

fn last_meaningful_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .unwrap_or("no diagnostic from git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git must be on PATH for the worktree tests");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn hermetic_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path();
        git(dir, &["init"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Harness Test"]);
        git(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("seed.txt"), "seed").expect("write seed");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "seed"]);
        tmp
    }

    #[test]
    fn two_worktrees_are_isolated_from_each_other_and_the_repo() {
        let repo = hermetic_repo();
        let root = repo.path();

        let first = Worktree::create(root, "node one").expect("first worktree");
        let second = Worktree::create(root, "node two").expect("second worktree");

        assert_ne!(first.path(), second.path());
        assert_ne!(first.branch(), second.branch());
        assert!(first.branch().starts_with("harness/"));
        assert!(first.path().is_dir(), "{:?}", first.path());
        assert!(first.path().join("seed.txt").exists());

        std::fs::write(first.path().join("shared.txt"), "from first").expect("write first");
        std::fs::write(second.path().join("shared.txt"), "from second").expect("write second");

        let in_first =
            std::fs::read_to_string(first.path().join("shared.txt")).expect("read first");
        let in_second =
            std::fs::read_to_string(second.path().join("shared.txt")).expect("read second");
        assert_eq!(in_first, "from first");
        assert_eq!(in_second, "from second");

        assert!(
            !root.join("shared.txt").exists(),
            "a worktree write must not appear in the repository"
        );
        assert!(
            !second.path().join("only-in-first.txt").exists(),
            "the second worktree must not see files unique to the first"
        );

        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();
        first.remove();
        second.remove();
        assert!(!first_path.exists(), "removal must delete the checkout");
        assert!(!second_path.exists());
    }

    #[test]
    fn a_non_repository_is_refused_with_its_path() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let err = Worktree::create(tmp.path(), "node").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("not a git repository"), "{message}");

        let named = canonicalize(tmp.path());
        assert!(message.contains(&named.display().to_string()), "{message}");
    }

    #[test]
    fn a_repository_without_commits_is_refused_with_its_path() {
        let tmp = tempfile::tempdir().expect("temp dir");
        git(tmp.path(), &["init"]);
        let err = Worktree::create(tmp.path(), "node").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("no commits yet"), "{message}");
    }

    #[test]
    fn the_branch_name_is_a_safe_slug() {
        assert_eq!(slug_of("Add Parser!"), "add-parser");
        assert_eq!(slug_of("  spaced  out  "), "spaced-out");
        assert_eq!(slug_of("!!!"), "node");
        assert_eq!(slug_of(""), "node");
    }

    #[test]
    fn suffixes_are_unique() {
        let first = unique_suffix();
        let second = unique_suffix();
        assert_ne!(first, second);
    }
}
