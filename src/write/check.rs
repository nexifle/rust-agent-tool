//! Content policy checks for the write tool.
//!
//! [`check_content`] applies the checks that run before any host hook: the
//! UTF-8 byte budget, the NUL policy, the empty policy, and the omission-marker
//! scan. It works on the original model string and never trims it. Nothing here
//! touches the disk and nothing calls a hook.
//!
//! The checks run in a fixed order, and the first match wins, so one content
//! string produces one stable code even when it breaks several rules.

use crate::write::config::WriteConfig;

/// Code for content longer than [`WriteConfig::max_content_bytes`].
pub const TOO_LARGE: &str = "AGT_WRITE_TOO_LARGE";

/// Code for content that contains a NUL byte while NUL is disallowed.
pub const BINARY: &str = "AGT_WRITE_BINARY";

/// Code for empty content while empty content is disallowed.
pub const EMPTY: &str = "AGT_WRITE_EMPTY";

/// Code for content that contains a configured omission marker.
pub const INCOMPLETE: &str = "AGT_WRITE_INCOMPLETE";

/// One rejected content string.
///
/// The later tool renders this as an error [`crate::ToolOutput`] by joining
/// [`ContentRejection::code`], `": "`, and [`ContentRejection::message`]. The
/// code is the same string as the exported constant, so a host can match it
/// without parsing the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContentRejection {
    /// Stable code, one of [`TOO_LARGE`], [`BINARY`], [`EMPTY`], or
    /// [`INCOMPLETE`].
    pub(crate) code: &'static str,
    /// Human-readable explanation, without the code prefix.
    pub(crate) message: String,
}

/// Checks one content string against `config`.
///
/// The checks run in this order, and the first match wins:
///
/// 1. `content.len()` (UTF-8 bytes) above
///    [`WriteConfig::max_content_bytes`] is [`TOO_LARGE`]. The NUL and marker
///    scans do not run after this check.
/// 2. A NUL byte while [`WriteConfig::allow_nul`] is false is [`BINARY`]. The
///    scan is skipped entirely when NUL bytes are allowed.
/// 3. Empty content while [`WriteConfig::allow_empty`] is false is [`EMPTY`].
///    The check is `is_empty`, so whitespace-only content is not empty.
/// 4. A hit on the omission-marker list is [`INCOMPLETE`]. The scan is skipped
///    when the list is empty. The message names the marker and never includes
///    the surrounding file text.
///
/// The content is not trimmed, and the same string is what a later step passes
/// to the host.
///
/// # Errors
///
/// Returns the first [`ContentRejection`] that matches. `Ok(())` means every
/// configured content rule accepted the string.
pub(crate) fn check_content(content: &str, config: &WriteConfig) -> Result<(), ContentRejection> {
    let limit = config.max_content_bytes();
    if content.len() > limit {
        return Err(ContentRejection {
            code: TOO_LARGE,
            message: format!(
                "content is {} UTF-8 bytes, above the configured limit of {limit} bytes",
                content.len()
            ),
        });
    }

    if !config.allow_nul() && content.as_bytes().contains(&0) {
        return Err(ContentRejection {
            code: BINARY,
            message:
                "content contains a NUL byte; write is refused unless the host allows NUL bytes"
                    .to_string(),
        });
    }

    if !config.allow_empty() && content.is_empty() {
        return Err(ContentRejection {
            code: EMPTY,
            message: "content is empty and this tool rejects empty writes".to_string(),
        });
    }

    let markers = config.omission_markers();
    if !markers.is_empty()
        && let Some(marker) = crate::text::first_omission_marker(content, markers)
    {
        return Err(ContentRejection {
            code: INCOMPLETE,
            message: format!(
                "content contains the omission marker \"{marker}\"; write the whole file instead of a placeholder"
            ),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BINARY, ContentRejection, EMPTY, INCOMPLETE, TOO_LARGE, check_content};
    use crate::write::config::WriteConfig;

    fn rejection(content: &str, config: &WriteConfig) -> ContentRejection {
        check_content(content, config).expect_err("content should be rejected")
    }

    fn assert_accepted(content: &str, config: &WriteConfig) {
        assert_eq!(
            check_content(content, config),
            Ok(()),
            "content should be accepted: {content:?}"
        );
    }

    #[test]
    fn the_codes_are_the_published_strings() {
        assert_eq!(TOO_LARGE, "AGT_WRITE_TOO_LARGE");
        assert_eq!(BINARY, "AGT_WRITE_BINARY");
        assert_eq!(EMPTY, "AGT_WRITE_EMPTY");
        assert_eq!(INCOMPLETE, "AGT_WRITE_INCOMPLETE");
    }

    #[test]
    fn empty_content_with_the_default_config_is_accepted() {
        assert_accepted("", &WriteConfig::new());
    }

    #[test]
    fn empty_content_is_rejected_when_empty_is_disallowed() {
        let config = WriteConfig::new().with_allow_empty(false);
        let rejection = rejection("", &config);
        assert_eq!(rejection.code, EMPTY);
        assert!(!rejection.message.is_empty());
    }

    #[test]
    fn whitespace_only_content_is_not_empty() {
        let config = WriteConfig::new().with_allow_empty(false);
        assert_accepted("   ", &config);
        assert_accepted("\n\t", &config);
    }

    #[test]
    fn content_at_the_limit_is_accepted_and_one_byte_over_is_too_large() {
        let config = WriteConfig::new()
            .with_max_content_bytes(8)
            .expect("8 is a valid budget");

        assert_accepted("12345678", &config);

        let rejection = rejection("123456789", &config);
        assert_eq!(rejection.code, TOO_LARGE);
        assert!(
            rejection.message.contains("8"),
            "message must include the limit: {}",
            rejection.message
        );
        assert!(
            rejection.message.contains("9"),
            "message must include the actual size: {}",
            rejection.message
        );
    }

    #[test]
    fn the_limit_counts_utf8_bytes_not_characters() {
        let config = WriteConfig::new()
            .with_max_content_bytes(4)
            .expect("4 is a valid budget");

        // Three scalars, six UTF-8 bytes.
        let content = "ééé";
        assert_eq!(content.chars().count(), 3);
        assert_eq!(content.len(), 6);

        let rejection = rejection(content, &config);
        assert_eq!(rejection.code, TOO_LARGE);
        assert!(rejection.message.contains("4"), "message: {rejection:?}");
    }

    #[test]
    fn a_nul_byte_is_rejected_by_default() {
        let rejection = rejection("before\0after", &WriteConfig::new());
        assert_eq!(rejection.code, BINARY);
        assert!(
            rejection.message.contains("NUL"),
            "message: {}",
            rejection.message
        );
    }

    #[test]
    fn nul_is_accepted_when_allow_nul_is_on() {
        let config = WriteConfig::new().with_allow_nul(true);
        assert_accepted("before\0after", &config);
    }

    #[test]
    fn too_large_is_checked_before_nul_and_markers() {
        let config = WriteConfig::new()
            .with_max_content_bytes(4)
            .expect("4 is a valid budget")
            .with_omission_markers(["rest of the file"])
            .expect("valid marker");
        let content = "rest of the file\0";

        let rejection = rejection(content, &config);
        assert_eq!(rejection.code, TOO_LARGE);
    }

    #[test]
    fn the_default_marker_list_rejects_nothing() {
        assert_accepted("rest of the file", &WriteConfig::new());
        assert_accepted("... existing code ...", &WriteConfig::new());
    }

    #[test]
    fn a_configured_marker_is_rejected_and_named() {
        let config = WriteConfig::new()
            .with_omission_markers(["rest of the file"])
            .expect("valid marker");

        let rejection = rejection("fn main() {}\n// rest of the file\n", &config);
        assert_eq!(rejection.code, INCOMPLETE);
        assert!(
            rejection.message.contains("rest of the file"),
            "message must include the marker: {}",
            rejection.message
        );
    }

    #[test]
    fn the_marker_message_does_not_include_surrounding_file_text() {
        let config = WriteConfig::new()
            .with_omission_markers(["rest of the file"])
            .expect("valid marker");

        let rejection = rejection("const TOKEN = \"secret\";\n// rest of the file", &config);
        assert_eq!(rejection.code, INCOMPLETE);
        assert!(
            !rejection.message.contains("secret"),
            "message must not leak nearby content: {}",
            rejection.message
        );
    }

    #[test]
    fn the_first_marker_in_the_list_wins() {
        let config = WriteConfig::new()
            .with_omission_markers(["second", "first"])
            .expect("valid markers");

        let rejection = rejection("first and second", &config);
        assert_eq!(rejection.code, INCOMPLETE);
        assert!(
            rejection.message.contains("second"),
            "the list order decides the winner: {}",
            rejection.message
        );
    }

    #[test]
    fn marking_is_case_sensitive() {
        let config = WriteConfig::new()
            .with_omission_markers(["rest of the file"])
            .expect("valid marker");
        assert_accepted("REST OF THE FILE", &config);
    }

    #[test]
    fn empty_content_hits_the_empty_rule_before_the_marker_scan() {
        let config = WriteConfig::new()
            .with_allow_empty(false)
            .with_omission_markers(["rest of the file"])
            .expect("valid marker");

        let rejection = rejection("", &config);
        assert_eq!(rejection.code, EMPTY);
    }
}
