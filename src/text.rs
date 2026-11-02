//! Text helpers shared by the write tool and a later edit tool.
//!
//! [`normalize_newlines`] converts line endings with one forward pass over
//! bytes and allocates only when a conversion changes something. The
//! omission-marker scan, [`first_omission_marker`], is a case-sensitive
//! substring search with no regular expressions.
//!
//! Neither helper touches the disk.

use std::borrow::Cow;

/// Line-ending policy for the content a host writes.
///
/// The host chooses one on a write permit. The write tool applies it to the
/// content after the permit is returned, and the host writes the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Newline {
    /// Keep every line ending as sent, and do not allocate a converted string.
    AsIs,
    /// End every line with a single `\n`.
    Lf,
    /// End every line with `\r\n`. An existing pair is not doubled.
    CrLf,
}

/// Returns `content` with its line endings converted to `newline`.
///
/// The conversion is one forward pass over bytes:
///
/// | Input sequence | [`Newline::Lf`] | [`Newline::CrLf`] |
/// | --- | --- | --- |
/// | `\r\n` | `\n` | `\r\n` (unchanged, not doubled) |
/// | lone `\r` | `\n` | `\r\n` |
/// | lone `\n` | `\n` | `\r\n` |
///
/// [`Newline::AsIs`] returns [`Cow::Borrowed`] without scanning. A conversion
/// returns [`Cow::Borrowed`] when it would not change a byte, so [`Newline::Lf`]
/// allocates only when a `\r` exists and [`Newline::CrLf`] allocates only when
/// a lone `\n` or a lone `\r` exists.
#[must_use]
pub(crate) fn normalize_newlines(content: &str, newline: Newline) -> Cow<'_, str> {
    match newline {
        Newline::AsIs => Cow::Borrowed(content),
        Newline::Lf => {
            if content.as_bytes().contains(&b'\r') {
                Cow::Owned(rewrite_newlines(content, "\n"))
            } else {
                Cow::Borrowed(content)
            }
        }
        Newline::CrLf => {
            if needs_crlf_conversion(content.as_bytes()) {
                Cow::Owned(rewrite_newlines(content, "\r\n"))
            } else {
                Cow::Borrowed(content)
            }
        }
    }
}

/// Returns whether converting `content` to CRLF would change a byte.
///
/// An existing `\r\n` pair is left alone, so it is not a reason to allocate.
fn needs_crlf_conversion(content: &[u8]) -> bool {
    let mut index = 0;
    while index < content.len() {
        match content[index] {
            b'\r' if content.get(index + 1) == Some(&b'\n') => index += 2,
            b'\r' | b'\n' => return true,
            _ => index += 1,
        }
    }
    false
}

/// Rewrites every line ending in `content` to `replacement`.
///
/// Runs of bytes between line endings are copied from `content` as string
/// slices, so multi-byte UTF-8 sequences are never split.
fn rewrite_newlines(content: &str, replacement: &str) -> String {
    let bytes = content.as_bytes();
    let mut output = String::with_capacity(content.len());
    let mut copied = 0;
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                output.push_str(&content[copied..index]);
                output.push_str(replacement);
                index += if bytes.get(index + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
                copied = index;
            }
            b'\n' => {
                output.push_str(&content[copied..index]);
                output.push_str(replacement);
                index += 1;
                copied = index;
            }
            _ => index += 1,
        }
    }

    output.push_str(&content[copied..]);
    output
}

/// Returns the first marker in `markers` that occurs in `content`.
///
/// Matching is a case-sensitive substring search, not a regular expression.
/// The list order decides the winner: the first marker in `markers` whose text
/// occurs in `content` is returned, even when a later marker occurs earlier in
/// the file. An empty list returns `None`.
///
/// The return value is the marker only. The surrounding file text is never
/// included, because a marker can sit next to content the model was about to
/// write into a file, including a secret.
#[must_use]
pub(crate) fn first_omission_marker<'a, M>(content: &str, markers: &'a [M]) -> Option<&'a str>
where
    M: AsRef<str>,
{
    for marker in markers {
        let marker = marker.as_ref();
        if content.contains(marker) {
            return Some(marker);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Newline, first_omission_marker, normalize_newlines};
    use std::borrow::Cow;

    /// Asserts the conversion borrowed its input rather than allocating.
    ///
    /// Matching `Cow::Borrowed` is the proof. Same bytes on an owned string is
    /// not: a clone can hold identical text.
    #[track_caller]
    fn assert_borrowed(output: Cow<'_, str>, expected: &str) {
        assert!(
            matches!(output, Cow::Borrowed(_)),
            "expected Cow::Borrowed, got an owned string"
        );
        assert_eq!(output.as_ref(), expected);
    }

    #[test]
    fn as_is_is_borrowed_and_keeps_every_byte() {
        let input = "a\r\nb\rc\nd";
        let output = normalize_newlines(input, Newline::AsIs);
        assert_borrowed(output, "a\r\nb\rc\nd");
    }

    #[test]
    fn lf_converts_crlf_and_lone_cr() {
        let output = normalize_newlines("a\r\nb\rc", Newline::Lf);
        assert!(matches!(output, Cow::Owned(_)));
        assert_eq!(output, "a\nb\nc");
    }

    #[test]
    fn crlf_converts_lone_lf_and_keeps_existing_pairs() {
        let output = normalize_newlines("a\nb\r\nc", Newline::CrLf);
        assert!(matches!(output, Cow::Owned(_)));
        assert_eq!(output, "a\r\nb\r\nc");
    }

    #[test]
    fn crlf_converts_a_lone_cr() {
        let output = normalize_newlines("a\rb", Newline::CrLf);
        assert!(matches!(output, Cow::Owned(_)));
        assert_eq!(output, "a\r\nb");
    }

    #[test]
    fn an_existing_crlf_is_not_doubled() {
        let output = normalize_newlines("a\r\nb\nc", Newline::CrLf);
        assert!(matches!(output, Cow::Owned(_)));
        assert_eq!(output, "a\r\nb\r\nc");
        assert!(!output.contains("\r\r\n"));
    }

    #[test]
    fn lf_without_a_cr_is_borrowed() {
        let output = normalize_newlines("a\nb\nc", Newline::Lf);
        assert_borrowed(output, "a\nb\nc");
    }

    #[test]
    fn crlf_with_only_existing_pairs_is_borrowed() {
        let output = normalize_newlines("a\r\nb\r\nc", Newline::CrLf);
        assert_borrowed(output, "a\r\nb\r\nc");
    }

    #[test]
    fn multibyte_text_survives_conversion() {
        let output = normalize_newlines("héllo\r\nwörld\r!", Newline::CrLf);
        assert!(matches!(output, Cow::Owned(_)));
        assert_eq!(output, "héllo\r\nwörld\r\n!");
    }

    #[test]
    fn the_first_matching_marker_wins() {
        let markers = ["second", "first"];
        assert_eq!(
            first_omission_marker("first and second", &markers),
            Some("second")
        );
    }

    #[test]
    fn no_marker_match_returns_none() {
        let markers = ["existing code", "rest of the file"];
        assert_eq!(first_omission_marker("hello", &markers), None);
    }

    #[test]
    fn matching_is_case_sensitive() {
        let markers = ["rest of the file"];
        assert_eq!(first_omission_marker("REST OF THE FILE", &markers), None);
    }

    #[test]
    fn an_empty_marker_list_matches_nothing() {
        let markers: [&str; 0] = [];
        assert_eq!(first_omission_marker("anything", &markers), None);
    }
}
