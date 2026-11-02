//! Path checks that never touch the disk.
//!
//! [`path_is_within`] compares two already-canonical paths.
//! [`check_model_path`] validates the path string a model sent, before any host
//! resolves it. Neither function opens, canonicalizes, or stats a path.

use std::path::{Path, is_separator};

/// Maximum accepted length of a model-supplied path, in UTF-8 bytes.
///
/// The limit stops a giant path key in one tool call. It is not a filesystem
/// `NAME_MAX` check, it counts bytes rather than characters, and the host still
/// enforces its own path limits.
pub(crate) const MAX_MODEL_PATH_BYTES: usize = 4096;

/// Returns whether `canonical_path` is `canonical_root` or a path below it.
///
/// Both arguments must already be canonical and absolute. This function does
/// not touch the disk: it does not canonicalize, does not resolve symlinks,
/// and checks the path values as given. This helper does not follow symlinks,
/// so calling it on a logical (non-canonical) path is a host bug: only
/// canonicalization before this call can expose a symlink that leaves the root.
///
/// A path equal to `canonical_root` is inside. Whole components are compared,
/// so `/repo` does not contain `/repo-secret`. A relative path, or a path that
/// still has a `.` or `..` component, is not inside.
///
/// # Examples
///
/// ```
/// # #[cfg(unix)]
/// # {
/// use std::path::Path;
/// use rust_agent_tool::path_is_within;
///
/// assert!(path_is_within(
///     Path::new("/repo/src/main.rs"),
///     Path::new("/repo")
/// ));
/// assert!(!path_is_within(Path::new("/repo-secret"), Path::new("/repo")));
/// # }
/// ```
#[must_use]
pub fn path_is_within(canonical_path: &Path, canonical_root: &Path) -> bool {
    if !canonical_path.is_absolute() || !canonical_root.is_absolute() {
        return false;
    }
    if has_dot_segment(canonical_path) || has_dot_segment(canonical_root) {
        return false;
    }

    let mut path_components = canonical_path.components();
    canonical_root.components().all(|root_component| {
        path_components
            .next()
            .is_some_and(|path_component| path_component == root_component)
    })
}

/// Reason a model-supplied path was rejected by [`check_model_path`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelPathError {
    /// The path is empty or only whitespace.
    Blank,
    /// The path has leading or trailing whitespace.
    Padded,
    /// The path contains a NUL byte.
    Nul,
    /// The path is longer than [`MAX_MODEL_PATH_BYTES`] in UTF-8 bytes.
    TooLong {
        /// Length of the rejected path in UTF-8 bytes.
        bytes: usize,
    },
}

/// Checks a model-supplied path without touching the disk.
///
/// The rules run in order and the first match wins:
///
/// 1. A path that is empty, only whitespace, or padded with whitespace is
///    rejected. The path is never trimmed and then accepted.
/// 2. A path that contains a NUL byte is rejected.
/// 3. A path longer than [`MAX_MODEL_PATH_BYTES`] UTF-8 bytes is rejected.
///
/// The path is not joined to a root, stripped of `./`, or stored. The caller
/// keeps the model string unchanged.
///
/// # Errors
///
/// Returns the first [`ModelPathError`] that matches. The caller turns it into
/// a contract error with a human-readable message.
pub(crate) fn check_model_path(path: &str) -> Result<(), ModelPathError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ModelPathError::Blank);
    }
    if trimmed.len() != path.len() {
        return Err(ModelPathError::Padded);
    }
    if path.as_bytes().contains(&0) {
        return Err(ModelPathError::Nul);
    }
    if path.len() > MAX_MODEL_PATH_BYTES {
        return Err(ModelPathError::TooLong { bytes: path.len() });
    }
    Ok(())
}

/// Whether a raw path segment is `.` or `..`.
///
/// [`Path::components`] normalizes away `.` segments, so the segments are read
/// from the path text to keep the rule enforceable.
fn has_dot_segment(path: &Path) -> bool {
    path.to_string_lossy()
        .split(is_separator)
        .any(|segment| segment == "." || segment == "..")
}

#[cfg(test)]
mod tests {
    use super::{MAX_MODEL_PATH_BYTES, ModelPathError, check_model_path, path_is_within};
    use std::path::{Path, PathBuf};

    /// An absolute root that does not need to exist on disk.
    fn repo_root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\repo")
        } else {
            PathBuf::from("/repo")
        }
    }

    /// A path next to [`repo_root`] whose name starts with `repo`.
    fn sibling(name: &str) -> PathBuf {
        repo_root()
            .parent()
            .expect("an absolute root has a parent")
            .join(name)
    }

    #[test]
    fn a_path_equal_to_the_root_is_inside() {
        let root = repo_root();
        assert!(path_is_within(&root, &root));
    }

    #[test]
    fn a_child_path_is_inside() {
        let root = repo_root();
        assert!(path_is_within(&root.join("src"), &root));
        assert!(path_is_within(&root.join("src").join("main.rs"), &root));
    }

    #[test]
    fn a_sibling_with_the_root_as_name_prefix_is_not_inside() {
        let root = repo_root();
        let secret = sibling("repo-secret");
        assert!(!path_is_within(&secret, &root));
        assert!(!path_is_within(&secret.join("file"), &root));
    }

    #[test]
    fn a_relative_path_is_not_inside() {
        let root = repo_root();
        assert!(!path_is_within(Path::new(""), &root));
        assert!(!path_is_within(Path::new("."), &root));
        assert!(!path_is_within(Path::new("repo/src"), &root));

        let child = root.join("src");
        assert!(!path_is_within(&child, Path::new("repo")));
    }

    #[test]
    fn a_parent_component_is_rejected() {
        let root = repo_root();
        assert!(!path_is_within(&root.join(".."), &root));
        let escaped = root.join("src").join("..").join("main.rs");
        assert!(!path_is_within(&escaped, &root));
        assert!(!path_is_within(&root.join("src"), &root.join("..")));
    }

    #[test]
    fn a_current_component_is_rejected() {
        let root = repo_root();
        assert!(!path_is_within(&root.join("."), &root));
        let dotted = root.join("src").join(".").join("main.rs");
        assert!(!path_is_within(&dotted, &root));
        assert!(!path_is_within(&root.join("src"), &root.join(".")));
    }

    #[test]
    fn an_unrelated_absolute_path_is_not_inside() {
        let root = repo_root();
        assert!(!path_is_within(&sibling("other"), &root));
        assert!(!path_is_within(&sibling("other").join("file"), &root));
    }

    #[test]
    fn a_parent_of_the_root_is_not_inside() {
        let root = repo_root();
        let parent = root.parent().expect("an absolute root has a parent");
        assert!(!path_is_within(parent, &root));
    }

    #[cfg(unix)]
    #[test]
    fn the_unix_root_contains_every_absolute_path() {
        let root = Path::new("/");
        assert!(path_is_within(root, root));
        assert!(path_is_within(Path::new("/repo/src"), root));
    }

    #[test]
    fn a_clean_model_path_is_accepted() {
        for path in ["a", "src/lib.rs", "my notes.txt", "./a", "a/b c/d"] {
            assert_eq!(check_model_path(path), Ok(()));
        }
    }

    #[test]
    fn an_empty_or_whitespace_only_path_is_rejected() {
        for path in ["", " ", "   ", "\t", "\n", " \t\n "] {
            assert_eq!(check_model_path(path), Err(ModelPathError::Blank));
        }
    }

    #[test]
    fn a_padded_path_is_rejected() {
        for path in [" foo", "foo ", " foo ", "\tfoo", "foo\n"] {
            assert_eq!(check_model_path(path), Err(ModelPathError::Padded));
        }
    }

    #[test]
    fn a_path_with_nul_is_rejected() {
        for path in ["\0", "a\0b", "a/b\0"] {
            assert_eq!(check_model_path(path), Err(ModelPathError::Nul));
        }
    }

    #[test]
    fn a_path_at_the_byte_cap_is_accepted() {
        let path = "a".repeat(MAX_MODEL_PATH_BYTES);
        assert_eq!(check_model_path(&path), Ok(()));
    }

    #[test]
    fn a_path_over_the_byte_cap_is_rejected() {
        let path = "a".repeat(MAX_MODEL_PATH_BYTES + 1);
        assert_eq!(
            check_model_path(&path),
            Err(ModelPathError::TooLong {
                bytes: MAX_MODEL_PATH_BYTES + 1
            })
        );
    }

    #[test]
    fn the_byte_cap_counts_utf8_bytes_not_scalars() {
        let at_cap = "é".repeat(MAX_MODEL_PATH_BYTES / 2);
        assert_eq!(at_cap.len(), MAX_MODEL_PATH_BYTES);
        assert_eq!(check_model_path(&at_cap), Ok(()));

        let over = format!("{at_cap}é");
        assert!(over.chars().count() <= MAX_MODEL_PATH_BYTES);
        assert_eq!(
            check_model_path(&over),
            Err(ModelPathError::TooLong {
                bytes: MAX_MODEL_PATH_BYTES + 2
            })
        );
    }

    #[test]
    fn whitespace_is_rejected_before_the_nul_and_length_rules() {
        let padded_long = format!(" {}", "a".repeat(MAX_MODEL_PATH_BYTES));
        assert_eq!(check_model_path(&padded_long), Err(ModelPathError::Padded));
        assert_eq!(check_model_path(" a\0b"), Err(ModelPathError::Padded));

        let long_nul = format!("a\0{}", "a".repeat(MAX_MODEL_PATH_BYTES));
        assert_eq!(check_model_path(&long_nul), Err(ModelPathError::Nul));
    }
}
