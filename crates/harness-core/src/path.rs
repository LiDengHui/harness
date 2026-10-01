//! Path resolution helpers.
//!
//! Two Windows-specific problems are solved here:
//!   * `std::fs::canonicalize` returns verbatim paths (`\\?\D:\work\...`), which
//!     leak into output and break comparisons against ordinary paths.
//!   * Containment checks must not be fooled by `..` in paths that do not exist
//!     yet, which is the normal case for tool output being written.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use crate::error::{HarnessError, Result};

/// Canonicalizes `path`, falling back to an absolutized form when it must not
/// exist. Verbatim prefixes are stripped so results compare cleanly.
pub fn canonicalize(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(resolved) => strip_verbatim(resolved),
        Err(_) => lexical_normalize(&absolutize(path)),
    }
}

/// Makes `path` absolute against the process working directory.
pub fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// Removes a Windows verbatim (`\\?\`) prefix, converting `\\?\UNC\` back to `\\`.
#[cfg(windows)]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix("UNC\\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(rest);
    }
    path
}

#[cfg(not(windows))]
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Resolves `.` and `..` textually. Used only when no ancestor exists on disk.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalizes the deepest existing ancestor of `path` and appends the rest.
///
/// This resolves symlinks and `..` correctly even for paths that are about to be
/// created, which is what makes the workspace containment check trustworthy.
fn resolve_existing_ancestor(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();

    loop {
        if let Ok(resolved) = std::fs::canonicalize(&probe) {
            let mut out = strip_verbatim(resolved);
            for part in tail.iter().rev() {
                out.push(part);
            }
            return out;
        }

        match (probe.parent(), probe.file_name()) {
            (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
                tail.push(name.to_os_string());
                probe = parent.to_path_buf();
            }
            _ => return lexical_normalize(path),
        }
    }
}

/// True when `candidate` sits inside `root` after resolution.
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    let root = canonicalize(root);
    let candidate = resolve_existing_ancestor(&absolutize(candidate));
    candidate.starts_with(&root)
}

/// Resolves `candidate` relative to `root` and rejects escapes.
///
/// This is the guard every filesystem tool goes through: a relative path is
/// joined to the workspace root, `..` and symlinks are resolved, and anything
/// that lands outside the root is refused.
pub fn resolve_within(root: &Path, candidate: &Path) -> Result<PathBuf> {
    let root = canonicalize(root);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let resolved = resolve_existing_ancestor(&joined);

    if resolved.starts_with(&root) {
        Ok(resolved)
    } else {
        Err(HarnessError::PathEscape(candidate.to_path_buf()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalization_collapses_dot_segments() {
        assert_eq!(
            lexical_normalize(Path::new("/work/a/./b/../c")),
            PathBuf::from("/work/a/c")
        );
    }

    #[test]
    fn relative_candidates_resolve_under_the_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize(tmp.path());

        let resolved = resolve_within(&root, Path::new("sub/dir/file.txt")).unwrap();
        assert!(resolved.starts_with(&root));
        assert!(resolved.ends_with("file.txt"));
    }

    #[test]
    fn parent_directory_escapes_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize(tmp.path());

        let err = resolve_within(&root, Path::new("../outside.txt")).unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");

        // Escaping through a directory that does not exist yet must also fail.
        let err = resolve_within(&root, Path::new("a/../../outside.txt")).unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");
    }

    #[test]
    fn absolute_paths_outside_the_root_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize(&tmp.path().join("workspace"));
        std::fs::create_dir_all(&root).unwrap();

        let outside = canonicalize(tmp.path()).join("elsewhere.txt");
        let err = resolve_within(&root, &outside).unwrap_err();
        assert!(matches!(err, HarnessError::PathEscape(_)), "{err}");
    }

    #[test]
    fn containment_is_component_wise_not_prefix_wise() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("work");
        std::fs::create_dir_all(&root).unwrap();
        let sibling = tmp.path().join("workshop");
        std::fs::create_dir_all(&sibling).unwrap();

        assert!(is_within(&root, &root.join("file.txt")));
        assert!(!is_within(&root, &sibling.join("file.txt")));
    }

    #[test]
    fn verbatim_prefixes_are_stripped_on_windows() {
        let stripped = strip_verbatim(PathBuf::from(r"\\?\D:\work\file.txt"));
        assert_eq!(stripped, PathBuf::from(r"D:\work\file.txt"));

        let unc = strip_verbatim(PathBuf::from(r"\\?\UNC\server\share\file.txt"));
        assert_eq!(unc, PathBuf::from(r"\\server\share\file.txt"));
    }

    #[test]
    fn nonexistent_paths_do_not_fail_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let root = canonicalize(tmp.path());

        let resolved = resolve_within(&root, Path::new("deep/not/created/yet.txt")).unwrap();
        assert!(resolved.starts_with(&root));
    }
}
