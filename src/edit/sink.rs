//! Host write hook for the edit tool.
//!
//! The SDK never writes, truncates, or locks a path. It hands the host the
//! complete before and after strings through [`EditSink::apply`], and the host
//! performs the write as a compare-and-swap: `after` is written only if the file
//! still equals `before`, which closes the read-then-write race the SDK cannot
//! close itself.
//!
//! A policy refusal is an expected answer and becomes a model-facing failure. A
//! compare-and-swap mismatch is [`ApplyError::Stale`], which is not a refusal
//! and has its own code and recovery instruction. An unexpected host failure is
//! [`ApplyError::Failed`]. The SDK does not catch panics from the hook.

use crate::async_trait;
use crate::host::HostFailure;

use super::source::NOT_FOUND;

/// Code for a path that names a directory.
pub const IS_DIRECTORY: &str = "AGT_EDIT_IS_DIRECTORY";

/// Code for a path the host forbids.
pub const FORBIDDEN: &str = "AGT_EDIT_FORBIDDEN";

/// Code for a path that resolves outside the workspace.
pub const OUTSIDE_WORKSPACE: &str = "AGT_EDIT_OUTSIDE_WORKSPACE";

/// Code for a policy refusal.
pub const REJECTED: &str = "AGT_EDIT_REJECTED";

/// Code for a compare-and-swap mismatch.
pub const STALE: &str = "AGT_EDIT_STALE";

/// Code for an unexpected failure inside [`EditSink::apply`].
pub const SINK_FAILED: &str = "AGT_EDIT_SINK_FAILED";

/// Host capability that performs one compare-and-swap file replacement.
///
/// `path` is the string the model sent, unchanged. `before` is the decoded file
/// text exactly as [`crate::EditSource::open`] returned it, BOM and original line
/// endings included, so the host can compare it byte for byte with the file on
/// disk. `after` is the whole new file in the same form. The host must write
/// `after` only if the file still equals `before`; if it does not, it returns
/// [`ApplyError::Stale`] and writes nothing.
///
/// The host owns atomic replacement, permission preservation, symlink policy,
/// and read-only refusal. The SDK does none of them and takes no lock. `apply`
/// runs at most once per edit and never runs when matching failed.
///
/// [`EditPermit::bytes`] is the final byte count on disk. A host that reformats
/// or re-encodes the text reports what it actually wrote, so the model is not
/// told a stale size.
///
/// `EditSink: Sync` only means the hook may be shared between threads. It is not
/// a path lock: the SDK does not serialize two calls on one path, so the host
/// does that itself when it needs to. The SDK keeps no memory of a previous
/// write.
///
/// The SDK does not catch a panic from this method and does not install a panic
/// hook. A panic unwinds out of the tool call.
#[async_trait]
pub trait EditSink: Send + Sync {
    /// Replaces the file at `path` with `after` if it still equals `before`.
    ///
    /// A policy refusal is [`ApplyError::Refusal`], a compare-and-swap mismatch
    /// is [`ApplyError::Stale`], and an unexpected hook failure is
    /// [`ApplyError::Failed`]. The returned [`Err`] means the host cannot tell
    /// whether any byte reached the file, so the tool tells the model the file
    /// may have been partially written.
    async fn apply(&self, path: &str, before: &str, after: &str) -> Result<EditPermit, ApplyError>;
}

/// The host's report after one [`EditSink::apply`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditPermit {
    /// UTF-8 bytes in the file after the host finished writing.
    ///
    /// The host reports [`str::len`] of `after` when it wrote the string
    /// unchanged, and the real size when it reformatted the text on the way.
    pub bytes: usize,
}

impl EditPermit {
    /// Builds a permit from the final byte count on disk.
    #[must_use]
    pub const fn new(bytes: usize) -> Self {
        Self { bytes }
    }
}

/// A policy refusal the model should see and recover from.
///
/// This is not a hook crash. A blank message is replaced by the documented
/// fallback, and the SDK never interprets the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditRefusal {
    /// The path is absent.
    ///
    /// From [`crate::EditSource::open`] this points the model at the write tool.
    /// From [`EditSink::apply`] it means the file disappeared after it was read.
    NotFound {
        /// Human-readable reason. A blank message renders as `path not found`.
        message: String,
    },
    /// The path names a directory, not a regular file.
    IsDirectory,
    /// The host refuses access to the path.
    Forbidden,
    /// The canonical path resolves outside the workspace.
    OutsideWorkspace,
    /// A policy refused the edit.
    ///
    /// This is the extension point for read-before-edit, plan mode, a
    /// generated-file guard, and a denied approval.
    Rejected {
        /// Human-readable reason. A blank message renders as `edit rejected`.
        message: String,
    },
}

impl EditRefusal {
    /// Stable code for this refusal.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => NOT_FOUND,
            Self::IsDirectory => IS_DIRECTORY,
            Self::Forbidden => FORBIDDEN,
            Self::OutsideWorkspace => OUTSIDE_WORKSPACE,
            Self::Rejected { .. } => REJECTED,
        }
    }

    /// Human-readable reason, with the documented blank-message fallbacks.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::NotFound { message } => trimmed_or(message, "path not found"),
            Self::Rejected { message } => trimmed_or(message, "edit rejected"),
            Self::IsDirectory => "path is a directory",
            Self::Forbidden => "permission denied",
            Self::OutsideWorkspace => "path resolves outside the workspace",
        }
    }
}

/// Why [`EditSink::apply`] did not return a permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The host refused the write as a policy decision.
    Refusal(EditRefusal),
    /// The file no longer equals `before`, so nothing was written.
    Stale,
    /// The hook failed unexpectedly.
    Failed(HostFailure),
}

/// Returns `message` trimmed, or `fallback` when it is blank.
fn trimmed_or<'a>(message: &'a str, fallback: &'static str) -> &'a str {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        fallback
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EditPermit, EditRefusal, FORBIDDEN, IS_DIRECTORY, NOT_FOUND, OUTSIDE_WORKSPACE, REJECTED,
        SINK_FAILED, STALE,
    };

    #[test]
    fn the_codes_are_the_published_strings() {
        assert_eq!(NOT_FOUND, "AGT_EDIT_NOT_FOUND");
        assert_eq!(IS_DIRECTORY, "AGT_EDIT_IS_DIRECTORY");
        assert_eq!(FORBIDDEN, "AGT_EDIT_FORBIDDEN");
        assert_eq!(OUTSIDE_WORKSPACE, "AGT_EDIT_OUTSIDE_WORKSPACE");
        assert_eq!(REJECTED, "AGT_EDIT_REJECTED");
        assert_eq!(STALE, "AGT_EDIT_STALE");
        assert_eq!(SINK_FAILED, "AGT_EDIT_SINK_FAILED");
    }

    #[test]
    fn a_permit_carries_the_final_byte_count() {
        assert_eq!(EditPermit::new(12).bytes, 12);
        assert_eq!(EditPermit { bytes: 0 }.bytes, 0);
    }

    #[test]
    fn refusals_map_to_their_codes_and_messages() {
        let cases = [
            (
                EditRefusal::NotFound {
                    message: String::new(),
                },
                NOT_FOUND,
                "path not found",
            ),
            (
                EditRefusal::IsDirectory,
                IS_DIRECTORY,
                "path is a directory",
            ),
            (EditRefusal::Forbidden, FORBIDDEN, "permission denied"),
            (
                EditRefusal::OutsideWorkspace,
                OUTSIDE_WORKSPACE,
                "path resolves outside the workspace",
            ),
            (
                EditRefusal::Rejected {
                    message: "  ".to_string(),
                },
                REJECTED,
                "edit rejected",
            ),
        ];
        for (refusal, code, message) in cases {
            assert_eq!(refusal.code(), code);
            assert_eq!(refusal.message(), message);
        }
    }

    #[test]
    fn a_host_message_is_trimmed_and_kept() {
        let refusal = EditRefusal::NotFound {
            message: "  gone with the wind  ".to_string(),
        };
        assert_eq!(refusal.code(), NOT_FOUND);
        assert_eq!(refusal.message(), "gone with the wind");
    }
}
