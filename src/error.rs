//! Stable error codes for the tool contract.
//!
//! A code is `AGT_<DOMAIN>_<REASON>`. Add a code when a new failure appears.
//! Do not rename a published code.

use std::error::Error;
use std::fmt;

/// Machine-stable identifier for a contract failure.
///
/// Match on [`ErrorCode::as_str`] when the set of codes may grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The call input cannot be used.
    InputInvalid,
    /// The tool specification is not usable.
    SpecInvalid,
}

impl ErrorCode {
    /// Returns the stable log and wire form, such as `AGT_INPUT_INVALID`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InputInvalid => "AGT_INPUT_INVALID",
            Self::SpecInvalid => "AGT_SPEC_INVALID",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A contract failure the host must handle.
///
/// Operational failures that the model should see are a [`crate::ToolOutput`]
/// with `is_error` set, not this type.
#[derive(Debug)]
pub struct ToolError {
    code: ErrorCode,
    message: String,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ToolError {
    /// Builds an error. A blank message becomes `missing error message`.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: readable_message(message.into()),
            source: None,
        }
    }

    /// The call input cannot be used.
    #[must_use]
    pub fn input_invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InputInvalid, message)
    }

    /// The tool specification is not usable.
    #[must_use]
    pub fn spec_invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::SpecInvalid, message)
    }

    /// Attaches the underlying failure without changing the public code.
    #[must_use]
    pub fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Stable code for matching and logs.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Human-readable explanation, without the code prefix.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for ToolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source.as_ref() as &(dyn Error + 'static))
    }
}

fn readable_message(message: String) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        "missing error message".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, ToolError};

    #[test]
    fn codes_are_stable_unique_and_grepable() {
        let codes = [ErrorCode::InputInvalid, ErrorCode::SpecInvalid];
        let rendered: Vec<&str> = codes.iter().copied().map(ErrorCode::as_str).collect();
        assert_eq!(rendered, ["AGT_INPUT_INVALID", "AGT_SPEC_INVALID"]);

        let mut unique = rendered.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), rendered.len());

        for code in rendered {
            assert!(code.starts_with("AGT_"));
            assert!(code.chars().all(|character| {
                character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
            }));
        }
    }

    #[test]
    fn display_puts_the_code_before_the_message() {
        let error = ToolError::input_invalid("  field \"path\" must be a string  ");
        assert_eq!(error.code(), ErrorCode::InputInvalid);
        assert_eq!(error.message(), "field \"path\" must be a string");
        assert_eq!(
            error.to_string(),
            "AGT_INPUT_INVALID: field \"path\" must be a string"
        );
    }

    #[test]
    fn a_blank_message_stays_readable() {
        let error = ToolError::spec_invalid("   ");
        assert_eq!(error.to_string(), "AGT_SPEC_INVALID: missing error message");
    }

    #[test]
    fn source_is_preserved() {
        let error =
            ToolError::input_invalid("bad input").with_source(std::io::Error::other("disk"));
        let source = std::error::Error::source(&error).expect("source");
        assert_eq!(source.to_string(), "disk");
    }
}
