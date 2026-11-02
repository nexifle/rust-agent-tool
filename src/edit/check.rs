//! Request and file checks for the edit tool.
//!
//! [`check_request`] runs on the model strings before any host hook, and
//! [`check_file`] runs on the bytes the host returned from
//! `EditSource::open`. Neither touches the disk and neither calls a hook.
//!
//! The checks run in a fixed order, and the first match wins, so one request or
//! one file produces one stable code even when it breaks several rules.

use super::config::EditConfig;

/// Code for an empty `old_text`.
pub const EMPTY_OLD: &str = "AGT_EDIT_EMPTY_OLD";

/// Code for an edit that would change nothing.
pub const NO_CHANGE: &str = "AGT_EDIT_NO_CHANGE";

/// Code for a request or file over its configured byte budget.
pub const TOO_LARGE: &str = "AGT_EDIT_TOO_LARGE";

/// Code for a NUL byte in the file, in `old_text`, or in `new_text`.
pub const BINARY: &str = "AGT_EDIT_BINARY";

/// Code for file bytes that are not valid UTF-8.
pub const NOT_TEXT: &str = "AGT_EDIT_NOT_TEXT";

/// One rejected request or file.
///
/// The tool renders this as an error [`crate::ToolOutput`] by joining
/// [`CheckRejection::code`], `": "`, and [`CheckRejection::message`]. The code
/// is the same string as the exported constant, so a host can match it without
/// parsing the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CheckRejection {
    /// Stable code, one of [`EMPTY_OLD`], [`NO_CHANGE`], [`TOO_LARGE`],
    /// [`BINARY`], or [`NOT_TEXT`].
    pub(crate) code: &'static str,
    /// Human-readable explanation, without the code prefix.
    pub(crate) message: String,
}

/// Checks the two model text fields against `config`.
///
/// The checks run in this order, and the first match wins:
///
/// 1. An empty `old_text` is [`EMPTY_OLD`]. An empty needle has no occurrence
///    identity, and deleting a whole file is not this tool.
/// 2. `old_text` equal to `new_text` is [`NO_CHANGE`]. `replace_all` does not
///    change this: every occurrence would be replaced by itself.
/// 3. An `old_text` over [`EditConfig::max_old_text_bytes`] is [`TOO_LARGE`].
/// 4. A NUL byte in either field while [`EditConfig::allow_nul`] is false is
///    [`BINARY`]. `new_text` becomes file content, so a NUL there turns a text
///    file binary exactly like one already in the file.
///
/// Neither field is trimmed. An empty `new_text` is valid: it deletes the
/// region.
///
/// # Errors
///
/// Returns the first [`CheckRejection`] that matches.
pub(crate) fn check_request(
    old_text: &str,
    new_text: &str,
    config: &EditConfig,
) -> Result<(), CheckRejection> {
    if old_text.is_empty() {
        return Err(CheckRejection {
            code: EMPTY_OLD,
            message:
                "old_text is empty; send the text to replace, or use the write tool for a new file"
                    .to_string(),
        });
    }

    if old_text == new_text {
        return Err(CheckRejection {
            code: NO_CHANGE,
            message: "old_text and new_text are identical; the file was not changed".to_string(),
        });
    }

    let limit = config.max_old_text_bytes();
    if old_text.len() > limit {
        return Err(too_large("old_text", limit, old_text.len()));
    }

    if !config.allow_nul() && (old_text.as_bytes().contains(&0) || new_text.as_bytes().contains(&0))
    {
        return Err(CheckRejection {
            code: BINARY,
            message: "text contains a NUL byte; this tool edits text only".to_string(),
        });
    }

    Ok(())
}

/// Checks the bytes `EditSource::open` returned and decodes them.
///
/// The checks run in this order, and the first match wins:
///
/// 1. More than [`EditConfig::max_file_bytes`] bytes is [`TOO_LARGE`]. The
///    decode and the NUL scan do not run after this check.
/// 2. Bytes that are not valid UTF-8 are [`NOT_TEXT`]. The bytes are never
///    decoded with replacement characters: that would silently rewrite bytes
///    the model never saw.
/// 3. A NUL byte while [`EditConfig::allow_nul`] is false is [`BINARY`].
///
/// # Errors
///
/// Returns the first [`CheckRejection`] that matches. `Ok` borrows the decoded
/// text out of `bytes`.
pub(crate) fn check_file<'a>(
    bytes: &'a [u8],
    config: &EditConfig,
) -> Result<&'a str, CheckRejection> {
    let limit = config.max_file_bytes();
    if bytes.len() > limit {
        return Err(too_large("file", limit, bytes.len()));
    }

    let text = std::str::from_utf8(bytes).map_err(|_| CheckRejection {
        code: NOT_TEXT,
        message: "file is not valid UTF-8 text".to_string(),
    })?;

    if !config.allow_nul() && bytes.contains(&0) {
        return Err(CheckRejection {
            code: BINARY,
            message: "text contains a NUL byte; this tool edits text only".to_string(),
        });
    }

    Ok(text)
}

/// Builds the documented [`TOO_LARGE`] rejection.
///
/// `subject` names which budget was exceeded, so the model can tell an
/// oversized `old_text` from an oversized file and from a fan-out that would
/// make the file too large.
pub(crate) fn too_large(subject: &str, limit: usize, bytes: usize) -> CheckRejection {
    CheckRejection {
        code: TOO_LARGE,
        message: format!(
            "{subject} is {bytes} UTF-8 bytes, above the configured limit of {limit} bytes"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BINARY, CheckRejection, EMPTY_OLD, NO_CHANGE, NOT_TEXT, TOO_LARGE, check_file,
        check_request, too_large,
    };
    use crate::edit::config::EditConfig;

    fn rejected(old_text: &str, new_text: &str, config: &EditConfig) -> CheckRejection {
        check_request(old_text, new_text, config).expect_err("request should be rejected")
    }

    #[test]
    fn the_codes_are_the_published_strings() {
        assert_eq!(EMPTY_OLD, "AGT_EDIT_EMPTY_OLD");
        assert_eq!(NO_CHANGE, "AGT_EDIT_NO_CHANGE");
        assert_eq!(TOO_LARGE, "AGT_EDIT_TOO_LARGE");
        assert_eq!(BINARY, "AGT_EDIT_BINARY");
        assert_eq!(NOT_TEXT, "AGT_EDIT_NOT_TEXT");
    }

    #[test]
    fn a_clean_request_is_accepted() {
        let config = EditConfig::new();
        assert_eq!(check_request("old", "new", &config), Ok(()));
        // An empty replacement deletes the region.
        assert_eq!(check_request("old", "", &config), Ok(()));
    }

    #[test]
    fn an_empty_old_text_is_rejected_first() {
        let config = EditConfig::new();
        let rejection = rejected("", "", &config);
        assert_eq!(rejection.code, EMPTY_OLD);
        assert_eq!(
            rejection.message,
            "old_text is empty; send the text to replace, or use the write tool for a new file"
        );

        // Empty wins over identical: both are true for two empty strings.
        let rejection = rejected("", "new", &config);
        assert_eq!(rejection.code, EMPTY_OLD);
    }

    #[test]
    fn identical_fields_are_no_change() {
        let config = EditConfig::new();
        let rejection = rejected("same", "same", &config);
        assert_eq!(rejection.code, NO_CHANGE);
        assert_eq!(
            rejection.message,
            "old_text and new_text are identical; the file was not changed"
        );
    }

    #[test]
    fn old_text_at_the_budget_is_accepted_and_one_byte_over_is_too_large() {
        let config = EditConfig::new()
            .with_max_old_text_bytes(4)
            .expect("4 is a valid budget");
        assert_eq!(check_request("abcd", "x", &config), Ok(()));

        let rejection = rejected("abcde", "x", &config);
        assert_eq!(rejection.code, TOO_LARGE);
        assert!(rejection.message.contains("old_text"), "{rejection:?}");
        assert!(rejection.message.contains('4'), "{rejection:?}");
        assert!(rejection.message.contains('5'), "{rejection:?}");
    }

    #[test]
    fn the_old_text_budget_counts_utf8_bytes_not_characters() {
        let config = EditConfig::new()
            .with_max_old_text_bytes(4)
            .expect("4 is a valid budget");
        // Three scalars, six UTF-8 bytes.
        let rejection = rejected("ééé", "x", &config);
        assert_eq!(rejection.code, TOO_LARGE);
        assert!(rejection.message.contains('6'), "{rejection:?}");
    }

    #[test]
    fn a_nul_byte_in_either_field_is_binary() {
        let config = EditConfig::new();
        for (old_text, new_text) in [("before\0after", "x"), ("old", "before\0after")] {
            let rejection = rejected(old_text, new_text, &config);
            assert_eq!(rejection.code, BINARY);
            assert_eq!(
                rejection.message,
                "text contains a NUL byte; this tool edits text only"
            );
        }
    }

    #[test]
    fn nul_is_accepted_when_allow_nul_is_on() {
        let config = EditConfig::new().with_allow_nul(true);
        assert_eq!(check_request("a\0b", "c\0d", &config), Ok(()));
    }

    #[test]
    fn an_oversized_old_text_is_checked_before_the_nul_scan() {
        let config = EditConfig::new()
            .with_max_old_text_bytes(4)
            .expect("4 is a valid budget");
        let rejection = rejected("abcd\0", "x", &config);
        assert_eq!(rejection.code, TOO_LARGE);
    }

    #[test]
    fn a_clean_file_decodes_to_its_text() {
        let text = check_file(b"one\ntwo\n", &EditConfig::new()).expect("valid text");
        assert_eq!(text, "one\ntwo\n");
    }

    #[test]
    fn a_file_at_the_budget_is_accepted_and_one_byte_over_is_too_large() {
        let config = EditConfig::new()
            .with_max_file_bytes(4)
            .expect("4 is a valid budget");
        assert_eq!(check_file(b"abcd", &config), Ok("abcd"));

        let rejection = check_file(b"abcde", &config).expect_err("over budget");
        assert_eq!(rejection.code, TOO_LARGE);
        assert!(rejection.message.contains("file"), "{rejection:?}");
        assert!(rejection.message.contains('4'), "{rejection:?}");
        assert!(rejection.message.contains('5'), "{rejection:?}");
        assert!(!rejection.message.contains("old_text"), "{rejection:?}");
    }

    #[test]
    fn invalid_utf8_is_not_text_and_is_never_lossy_decoded() {
        let rejection = check_file(b"ok\xFFok", &EditConfig::new()).expect_err("invalid UTF-8");
        assert_eq!(rejection.code, NOT_TEXT);
        assert_eq!(rejection.message, "file is not valid UTF-8 text");
    }

    #[test]
    fn a_nul_byte_in_the_file_is_binary() {
        let rejection = check_file(b"a\0b", &EditConfig::new()).expect_err("NUL byte");
        assert_eq!(rejection.code, BINARY);

        let config = EditConfig::new().with_allow_nul(true);
        assert_eq!(check_file(b"a\0b", &config), Ok("a\0b"));
    }

    #[test]
    fn an_oversized_file_is_checked_before_the_decode() {
        let config = EditConfig::new()
            .with_max_file_bytes(2)
            .expect("2 is a valid budget");
        // Over budget and invalid UTF-8: the size check wins.
        assert_eq!(
            check_file(b"\xFF\xFF\xFF", &config)
                .expect_err("over budget")
                .code,
            TOO_LARGE
        );
    }

    #[test]
    fn the_too_large_message_names_the_subject_and_both_numbers() {
        let rejection = too_large("the edited file", 10, 12);
        assert_eq!(rejection.code, TOO_LARGE);
        assert!(
            rejection.message.contains("the edited file"),
            "{rejection:?}"
        );
        assert!(rejection.message.contains("12"), "{rejection:?}");
        assert!(rejection.message.contains("10"), "{rejection:?}");
    }
}
