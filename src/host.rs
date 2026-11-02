//! The one failure type shared by every host hook.
//!
//! [`HostFailure`] carries a human-readable message and no `AGT_` code, because
//! the tool layer adds the code that tells the model what to do next. The exec
//! tool and the edit tool both use it, so a host that implements several hooks
//! writes one error adapter, not one per tool.

/// Unexpected failure inside a host hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFailure {
    /// Human-readable reason.
    pub message: String,
}

impl HostFailure {
    /// Builds a failure. A blank message becomes `host hook failed`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        let trimmed = message.trim();
        Self {
            message: if trimmed.is_empty() {
                "host hook failed".to_string()
            } else {
                trimmed.to_string()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HostFailure;

    #[test]
    fn a_message_is_trimmed_and_kept() {
        assert_eq!(HostFailure::new("  disk on fire  ").message, "disk on fire");
        assert_eq!(
            HostFailure::new(String::from("spawn failed")).message,
            "spawn failed"
        );
    }

    #[test]
    fn a_blank_message_becomes_the_documented_fallback() {
        for message in ["", "   ", "\t\n"] {
            assert_eq!(HostFailure::new(message).message, "host hook failed");
        }
    }
}
