//! Path containment for two already-canonical paths.
//!
//! The check is pure: it compares path components and never touches the disk.

use std::path::{Path, is_separator};

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
    use super::path_is_within;
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
}
