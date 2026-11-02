//! Host read hook for the edit tool.
//!
//! The SDK never opens or canonicalizes a path. It asks the host for the whole
//! file as raw bytes through [`EditSource::open`], decodes them itself, and
//! reports everything it finds under a stable code.
//!
//! A policy refusal is an expected answer and becomes a model-facing failure.
//! An unexpected host failure is [`OpenError::Failed`]. The SDK does not catch
//! panics from the hook.

use crate::async_trait;
use crate::host::HostFailure;

use super::sink::EditRefusal;

/// Code for a path the host reports as absent.
pub const NOT_FOUND: &str = "AGT_EDIT_NOT_FOUND";

/// Host capability that reads one file for an edit.
///
/// `path` is the string the model sent. The SDK does not trim it, does not join
/// it to a workspace root, and does not strip `./`. The host resolves it,
/// canonicalizes it, and checks containment. A symlink that leaves the
/// workspace is [`EditRefusal::OutsideWorkspace`].
///
/// [`EditSource::open`] returns the whole file as raw bytes and never
/// lossy-decodes them: the SDK decodes with [`std::str::from_utf8`], and invalid
/// UTF-8 becomes `AGT_EDIT_NOT_TEXT`. The bytes are the exact file content,
/// including any UTF-8 BOM and its original line endings, and they are also what
/// is handed back to [`crate::EditSink::apply`] as `before`, so the host can
/// compare them byte for byte with the file on disk.
///
/// A host with its own size limit refuses with [`EditRefusal::Rejected`], which
/// the model sees as `AGT_EDIT_REJECTED`; the configured `max_file_bytes` check
/// is the second, independent limit and produces `AGT_EDIT_TOO_LARGE` when the
/// bytes do arrive. The host should enforce its own read cap before it builds
/// the byte vector, because the SDK cannot undo an allocation the host already
/// made.
///
/// `EditSource: Sync` only means the hook may be shared between threads. It is
/// not a path lock: the SDK does not serialize two calls on one path, so the
/// host does that itself when it needs to. The SDK keeps no memory of a previous
/// open.
///
/// The SDK does not catch a panic from this method and does not install a panic
/// hook. A panic unwinds out of the tool call.
#[async_trait]
pub trait EditSource: Send + Sync {
    /// Reads the whole file at `path` as raw bytes.
    ///
    /// A missing path is [`EditRefusal::NotFound`]. A policy refusal is any
    /// other [`EditRefusal`]. An unexpected hook failure is
    /// [`OpenError::Failed`], not a refusal.
    async fn open(&self, path: &str) -> Result<EditFile, OpenError>;
}

/// The bytes [`EditSource::open`] returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditFile {
    /// Whole file content as raw bytes, exactly as it is on disk.
    ///
    /// The SDK decodes these bytes and refuses invalid UTF-8; the host must not
    /// decode, truncate, or rewrite them.
    pub bytes: Vec<u8>,
}

/// Why [`EditSource::open`] did not return the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The host refused the read as a policy decision.
    Refusal(EditRefusal),
    /// The hook failed unexpectedly.
    Failed(HostFailure),
}

#[cfg(test)]
mod tests {
    use super::{EditFile, NOT_FOUND};

    #[test]
    fn the_code_is_the_published_string() {
        assert_eq!(NOT_FOUND, "AGT_EDIT_NOT_FOUND");
    }

    #[test]
    fn the_exec_module_and_the_crate_root_name_one_host_failure() {
        // One type, so a host writes one error adapter for every hook.
        let failure: crate::exec::HostFailure = crate::host::HostFailure::new("disk on fire");
        assert_eq!(failure.message, "disk on fire");
    }

    #[test]
    fn an_edit_file_holds_the_bytes_it_was_given() {
        let file = EditFile {
            bytes: b"one\r\ntwo\r\n".to_vec(),
        };
        assert_eq!(file.bytes, b"one\r\ntwo\r\n");
    }
}
