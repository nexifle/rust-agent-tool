//! Model-facing result of one exec call.
//!
//! Operational codes are stable strings. They are not variants of
//! [`crate::ErrorCode`]. A contract failure is still [`crate::ToolError`].

use crate::ToolOutput;
use crate::exec::classify::CommandClass;

/// The process finished with a non-zero status.
pub const NONZERO_EXIT: &str = "AGT_EXEC_NONZERO_EXIT";
/// The wall-clock deadline fired and the tree was killed.
pub const TIMED_OUT: &str = "AGT_EXEC_TIMED_OUT";
/// The call was cancelled and the tree was killed.
pub const CANCELLED: &str = "AGT_EXEC_CANCELLED";
/// The host rejected the working directory.
pub const CWD_REJECTED: &str = "AGT_EXEC_CWD_REJECTED";
/// The process could not be started.
pub const SPAWN_FAILED: &str = "AGT_EXEC_SPAWN_FAILED";
/// The sandbox denied the spawn. The SDK does not retry unsandboxed.
pub const SANDBOX_DENIED: &str = "AGT_EXEC_SANDBOX_DENIED";
/// The host has no job with the given id.
pub const JOB_NOT_FOUND: &str = "AGT_EXEC_JOB_NOT_FOUND";
/// A host hook failed unexpectedly.
pub const HOST_FAILED: &str = "AGT_EXEC_HOST_FAILED";

const NEXT_PROCESS: &str = "Next call starts a new process.";

/// Structured result of [`crate::exec::ExecTool::execute`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutcome {
    text: String,
    is_error: bool,
    code: Option<&'static str>,
    exit_code: Option<i32>,
    truncated: bool,
    omitted_bytes: u64,
    class: CommandClass,
    directory: String,
}

impl ExecOutcome {
    /// Builds an outcome from parts the tool already decided.
    #[must_use]
    pub fn new(parts: OutcomeParts<'_>) -> Self {
        let status = match parts.code {
            Some(code) => format!("{code}: {}", parts.status),
            None => parts.status.to_string(),
        };
        let mut sections = Vec::new();
        if !parts.directory.is_empty() {
            sections.push(format!("Working directory: {}", parts.directory));
        }
        sections.push(parts.body.to_string());
        sections.push(status);
        sections.push(NEXT_PROCESS.to_string());
        Self {
            text: sections.join("\n"),
            is_error: parts.code.is_some(),
            code: parts.code,
            exit_code: parts.exit_code,
            truncated: parts.omitted_bytes > 0,
            omitted_bytes: parts.omitted_bytes,
            class: parts.class.clone(),
            directory: parts.directory.to_string(),
        }
    }

    /// Text the model sees.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the model should treat the operation as failed.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.is_error
    }

    /// Operational code, when the operation failed.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        self.code
    }

    /// Exit code when the process produced one.
    #[must_use]
    pub const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Whether the capture window dropped bytes.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }

    /// Bytes dropped from the model window.
    #[must_use]
    pub const fn omitted_bytes(&self) -> u64 {
        self.omitted_bytes
    }

    /// Classification of the command that was requested.
    #[must_use]
    pub fn class(&self) -> &CommandClass {
        &self.class
    }

    /// Directory the host accepted, or empty when the call never got that far.
    #[must_use]
    pub fn directory(&self) -> &str {
        &self.directory
    }

    /// Tool contract output. Errors are operational, not [`crate::ToolError`].
    #[must_use]
    pub fn to_output(&self) -> ToolOutput {
        if self.is_error {
            ToolOutput::error_text(self.text.clone())
        } else {
            ToolOutput::text(self.text.clone())
        }
    }
}

/// Pieces [`ExecOutcome::new`] renders. The omission marker belongs in `body`.
#[derive(Debug, Clone, Copy)]
pub struct OutcomeParts<'a> {
    /// Accepted directory, or empty.
    pub directory: &'a str,
    /// Capture body, job line, or `(no output)`.
    pub body: &'a str,
    /// Status sentence without the code prefix.
    pub status: &'a str,
    /// Operational code. `None` is success.
    pub code: Option<&'static str>,
    /// Exit code when known.
    pub exit_code: Option<i32>,
    /// Bytes the window dropped.
    pub omitted_bytes: u64,
    /// Command classification.
    pub class: &'a CommandClass,
}
