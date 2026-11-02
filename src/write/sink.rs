//! Host write sink contract.
//!
//! The SDK never opens, creates, truncates, or canonicalizes a path. It asks
//! the host for a decision through [`WriteSink::prepare`], then hands the bytes
//! back through [`WriteSink::write`]. The two calls are separate on purpose:
//! [`WriteSink::prepare`] receives the original model content so a content
//! policy can refuse before anything changes, and it answers whether the path
//! already exists and which line ending to use.
//!
//! A policy refusal is an expected answer and becomes a model-facing failure.
//! An unexpected host failure is [`PrepareError::Failed`], or an [`Err`] from
//! [`WriteSink::write`]. The SDK does not catch panics from either method.

use crate::async_trait;
use crate::text::Newline;

/// Code for a path the host refuses to create.
pub const NOT_FOUND: &str = "AGT_WRITE_NOT_FOUND";
/// Code for a path that names a directory.
pub const IS_DIRECTORY: &str = "AGT_WRITE_IS_DIRECTORY";
/// Code for a path the host forbids.
pub const FORBIDDEN: &str = "AGT_WRITE_FORBIDDEN";
/// Code for a path that resolves outside the workspace.
pub const OUTSIDE_WORKSPACE: &str = "AGT_WRITE_OUTSIDE_WORKSPACE";
/// Code for a policy refusal.
pub const REJECTED: &str = "AGT_WRITE_REJECTED";
/// Code for an unexpected host hook failure.
pub const SINK_FAILED: &str = "AGT_WRITE_SINK_FAILED";

/// Host capability that authorizes and performs one file write.
///
/// `path` is the string the model sent. The SDK does not trim it, does not join
/// it to a workspace root, and does not strip `./`. The host resolves it,
/// canonicalizes it, and checks containment before it mutates anything.
///
/// [`WriteSink::prepare`] receives the original model content after the
/// pre-hook content checks. [`WriteSink::write`] receives the string that must
/// hit the file: normalized when the permit asked for [`Newline::Lf`] or
/// [`Newline::CrLf`], otherwise the same string [`WriteSink::prepare`] saw. The
/// host writes those bytes as UTF-8 and does not invent a second encoding. The
/// host creates missing parent directories if it allows them, and it chooses
/// how the file is replaced. This tool does not append a trailing newline. If
/// the host wants one, it adds that byte inside [`WriteSink::write`].
///
/// A policy refusal belongs in [`WriteSink::prepare`], which is why a refusal
/// never reaches [`WriteSink::write`]. [`WriteSink::write`] is only for the
/// mutation and for I/O failures after the host has decided to write.
///
/// `WriteSink: Sync` only means the hook may be shared between threads. It is
/// not a path lock: the SDK does not serialize two calls on one path, so the
/// host does that itself when it needs to. The SDK keeps no memory of a
/// previous write.
///
/// The SDK does not catch a panic from either method and does not install a
/// panic hook. A panic unwinds out of the tool call.
#[async_trait]
pub trait WriteSink: Send + Sync {
    /// Decides whether this write may proceed and how it is written.
    ///
    /// A missing path the host refuses to create is
    /// [`WriteRefusal::NotFound`], not a create. Create is
    /// [`WritePermit::existed`] set to `false`. An unexpected hook failure is
    /// [`PrepareError::Failed`], not a refusal.
    async fn prepare(&self, path: &str, content: &str) -> Result<WritePermit, PrepareError>;

    /// Writes `content` to `path` as UTF-8.
    ///
    /// The host has already accepted the write in [`WriteSink::prepare`]. The
    /// returned [`Err`] means the host cannot tell whether any byte reached the
    /// file, so the tool tells the model the file may have been partially
    /// written.
    async fn write(&self, path: &str, content: &str) -> Result<(), SinkFailure>;
}

/// The host's answer for one [`WriteSink::prepare`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WritePermit {
    /// `false` to create the path, `true` to overwrite an existing file.
    ///
    /// The SDK copies this into the outcome. It does not stat the path.
    pub existed: bool,
    /// Line ending the written content must use.
    pub newline: Newline,
}

impl WritePermit {
    /// Builds a permit from the create-or-overwrite answer and newline choice.
    #[must_use]
    pub const fn new(existed: bool, newline: Newline) -> Self {
        Self { existed, newline }
    }
}

/// A policy refusal the model should see and recover from.
///
/// This is not a hook crash. The message is not interpreted by the SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRefusal {
    /// The host will not create the path.
    ///
    /// This is not "file does not exist, please create it"; create is
    /// [`WritePermit::existed`] set to `false`. It means the host refuses to
    /// invent the path, for example because a parent directory is missing and
    /// the host does not create parents.
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
    /// A policy refused the write.
    ///
    /// This is the extension point for read-before-write, plan mode, a
    /// generated-file guard, and a denied approval.
    Rejected {
        /// Human-readable reason. A blank message renders as `write rejected`.
        message: String,
    },
}

impl WriteRefusal {
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

    /// Human-readable reason, with the documented blank-message fallback.
    pub(crate) fn message(&self) -> &str {
        match self {
            Self::NotFound { message } => trimmed_or(message, "path not found"),
            Self::Rejected { message } => trimmed_or(message, "write rejected"),
            Self::IsDirectory => "path is a directory",
            Self::Forbidden => "permission denied",
            Self::OutsideWorkspace => "path resolves outside the workspace",
        }
    }
}

/// Why [`WriteSink::prepare`] did not return a permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareError {
    /// The host refused the write as a policy decision.
    Refusal(WriteRefusal),
    /// The hook failed unexpectedly.
    Failed(SinkFailure),
}

/// Unexpected failure inside [`WriteSink::prepare`] or [`WriteSink::write`].
///
/// The type carries one human-readable message and no `AGT_` code, because the
/// tool layer adds the code later. Do not put the `Debug` form of an
/// [`std::io::Error`] into this message unless that text is what the host wants
/// the model to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkFailure {
    message: String,
}

impl SinkFailure {
    /// Builds a failure from `message`.
    ///
    /// Surrounding whitespace is trimmed. A blank message becomes
    /// `host hook failed`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: readable_hook_message(message.into()),
        }
    }

    /// Human-readable failure text without a code prefix.
    ///
    /// A blank stored message still reads as `host hook failed`.
    #[must_use]
    pub fn message(&self) -> &str {
        trimmed_or(&self.message, "host hook failed")
    }
}

/// Trims `message`, or returns the blank-hook fallback.
fn readable_hook_message(message: String) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "host hook failed".to_string()
    } else {
        trimmed.to_string()
    }
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
        FORBIDDEN, IS_DIRECTORY, NOT_FOUND, OUTSIDE_WORKSPACE, REJECTED, SinkFailure, WritePermit,
        WriteRefusal,
    };
    use crate::text::Newline;

    #[test]
    fn a_permit_carries_the_host_answer() {
        let permit = WritePermit::new(true, Newline::CrLf);
        assert!(permit.existed);
        assert_eq!(permit.newline, Newline::CrLf);

        let permit = WritePermit::new(false, Newline::AsIs);
        assert!(!permit.existed);
        assert_eq!(permit.newline, Newline::AsIs);
    }

    #[test]
    fn sink_failure_trims_and_falls_back() {
        assert_eq!(SinkFailure::new("  disk error  ").message(), "disk error");
        assert_eq!(SinkFailure::new("").message(), "host hook failed");
        assert_eq!(SinkFailure::new("   ").message(), "host hook failed");
    }

    #[test]
    fn refusals_map_to_their_codes_and_messages() {
        let cases = [
            (
                WriteRefusal::NotFound {
                    message: String::new(),
                },
                NOT_FOUND,
                "path not found",
            ),
            (
                WriteRefusal::IsDirectory,
                IS_DIRECTORY,
                "path is a directory",
            ),
            (WriteRefusal::Forbidden, FORBIDDEN, "permission denied"),
            (
                WriteRefusal::OutsideWorkspace,
                OUTSIDE_WORKSPACE,
                "path resolves outside the workspace",
            ),
            (
                WriteRefusal::Rejected {
                    message: "  ".to_string(),
                },
                REJECTED,
                "write rejected",
            ),
        ];
        for (refusal, code, message) in cases {
            assert_eq!(refusal.code(), code);
            assert_eq!(refusal.message(), message);
        }
    }

    #[test]
    fn a_host_message_is_trimmed_and_kept() {
        let refusal = WriteRefusal::NotFound {
            message: "  missing parent directory  ".to_string(),
        };
        assert_eq!(refusal.code(), NOT_FOUND);
        assert_eq!(refusal.message(), "missing parent directory");
    }
}
