//! Byte splice and replacement line-ending policy.
//!
//! [`splice`] builds the whole new file from the original text and the ranges
//! the matcher returned. It copies every byte outside a range unchanged, so the
//! line endings of untouched lines, a BOM, and any byte the model never showed
//! the tool survive exactly as they were.
//!
//! Only the replacement text is converted. The ending it is converted to comes
//! from [`NewlinePolicy::Preserve`], which reads the ending of the region being
//! replaced and falls back to the file's dominant ending.
//!
//! The final length is computed before the output string is allocated, so an
//! over-budget result is refused without building it.

use std::ops::Range;

use super::config::NewlinePolicy;
use crate::text::{Newline, normalize_newlines};

/// Why a splice was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpliceError {
    /// The exact final length is over the file budget.
    ///
    /// Nothing was allocated: the length is known before the output string is
    /// built.
    TooLarge {
        /// Largest accepted file size, in UTF-8 bytes.
        limit: usize,
        /// Exact size the result would have had, in UTF-8 bytes.
        bytes: usize,
    },
}

/// Returns `original` with every range replaced by `replacement`.
///
/// `ranges` must be ascending, non-overlapping, and inside `original`, which is
/// what the matcher returns. The prefix, the gaps between ranges, and the
/// suffix are copied byte for byte, so a BOM in front of the first range and
/// the ending of every untouched line are left alone.
///
/// `dominant` is the ending the matcher counted for the file. It is used only
/// by [`NewlinePolicy::Preserve`], and only for a range that carries no line
/// ending of its own.
///
/// # Errors
///
/// Returns [`SpliceError::TooLarge`] when the exact final length is above
/// `max_file_bytes`. The length is computed from the original length, the range
/// lengths, and the converted replacement length, so no oversized string is
/// allocated.
pub(crate) fn splice(
    original: &str,
    ranges: &[Range<usize>],
    replacement: &str,
    newline: NewlinePolicy,
    dominant: Newline,
    max_file_bytes: usize,
) -> Result<String, SpliceError> {
    // Pass one counts endings. It does not build the converted replacement:
    // a hostile `replace_all` must be able to miss the budget without allocating.
    let counts = ending_counts(replacement.as_bytes());
    let mut total = original.len();
    let mut need_lf = false;
    let mut need_crlf = false;
    for range in ranges {
        let ending = chosen_ending(newline, dominant, &original[range.clone()]);
        if ending == Newline::CrLf {
            need_crlf = true;
        } else {
            need_lf = true;
        }
        total = total.saturating_sub(range.len());
        total = total.saturating_add(converted_len(replacement.len(), counts, ending));
    }
    if total > max_file_bytes {
        return Err(SpliceError::TooLarge {
            limit: max_file_bytes,
            bytes: total,
        });
    }

    let lf = need_lf.then(|| normalize_newlines(replacement, Newline::Lf));
    let crlf = need_crlf.then(|| normalize_newlines(replacement, Newline::CrLf));
    let mut output = String::with_capacity(total);
    let mut copied = 0;
    for range in ranges {
        let ending = chosen_ending(newline, dominant, &original[range.clone()]);
        let text = if ending == Newline::CrLf {
            crlf.as_deref()
        } else {
            lf.as_deref()
        };
        output.push_str(&original[copied..range.start]);
        if let Some(text) = text {
            output.push_str(text);
        }
        copied = range.end;
    }
    output.push_str(&original[copied..]);
    debug_assert_eq!(output.len(), total);
    Ok(output)
}

/// Ending applied to the replacement for one range.
fn chosen_ending(newline: NewlinePolicy, dominant: Newline, span: &str) -> Newline {
    match newline {
        NewlinePolicy::Lf => Newline::Lf,
        NewlinePolicy::CrLf => Newline::CrLf,
        NewlinePolicy::Preserve => preserved_ending(span, dominant),
    }
}

/// UTF-8 length after [`normalize_newlines`] converts a string of `content_len` bytes.
///
/// This is the same table as [`crate::text::normalize_newlines`], counted
/// instead of written: a `\r\n` pair loses one byte on the way to LF, and a
/// lone `\n` or lone `\r` gains one byte on the way to CRLF.
fn converted_len(content_len: usize, counts: (usize, usize, usize), newline: Newline) -> usize {
    let (crlf_pairs, lone_lf, lone_cr) = counts;
    match newline {
        Newline::AsIs => content_len,
        Newline::Lf => content_len - crlf_pairs,
        Newline::CrLf => content_len + lone_lf + lone_cr,
    }
}

/// Counts `\r\n` pairs, lone `\n`, and lone `\r` in one forward pass.
fn ending_counts(bytes: &[u8]) -> (usize, usize, usize) {
    let mut crlf_pairs = 0;
    let mut lone_lf = 0;
    let mut lone_cr = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                crlf_pairs += 1;
                index += 2;
            }
            b'\r' => {
                lone_cr += 1;
                index += 1;
            }
            b'\n' => {
                lone_lf += 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    (crlf_pairs, lone_lf, lone_cr)
}

/// Returns the ending [`NewlinePolicy::Preserve`] applies to a replaced span.
///
/// The first ending in the span wins, so a range that starts on a CRLF line and
/// ends on an LF line is rewritten as CRLF. A span with no ending falls back to
/// the file's dominant ending.
// shortcut: lone CR in a replaced span becomes LF, upgrade if a host needs classic Mac endings.
fn preserved_ending(span: &str, dominant: Newline) -> Newline {
    let bytes = span.as_bytes();
    let Some(index) = bytes
        .iter()
        .position(|byte| *byte == b'\r' || *byte == b'\n')
    else {
        return dominant;
    };
    if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
        Newline::CrLf
    } else {
        Newline::Lf
    }
}

#[cfg(test)]
mod tests {
    use super::{SpliceError, splice};
    use crate::edit::config::NewlinePolicy;
    use crate::text::{Newline, normalize_newlines};
    use std::ops::Range;

    /// One range, wrapped so a call site never builds a one-element literal
    /// array of ranges.
    fn span(start: usize, end: usize) -> Vec<Range<usize>> {
        std::iter::once(start..end).collect()
    }

    /// Splices with a byte budget large enough for every test below.
    fn splice_within(
        original: &str,
        ranges: &[Range<usize>],
        replacement: &str,
        newline: NewlinePolicy,
        dominant: Newline,
    ) -> String {
        splice(original, ranges, replacement, newline, dominant, 1 << 20).expect("within budget")
    }

    #[test]
    fn prefix_and_suffix_are_byte_identical() {
        let original = "keep\r\nchange\nkeep";
        let output = splice_within(
            original,
            &span(6, 12),
            "new",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "keep\r\nnew\nkeep");
        assert!(output.starts_with("keep\r\n"));
        assert!(output.ends_with("keep"));
    }

    #[test]
    fn a_bom_file_keeps_exactly_one_bom() {
        let original = "\u{feff}first\nsecond\n";
        let output = splice_within(
            original,
            &span(3, 8),
            "changed",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "\u{feff}changed\nsecond\n");
        assert_eq!(output.matches('\u{feff}').count(), 1);
    }

    #[test]
    fn a_too_large_result_is_refused_with_the_exact_length() {
        let original = "abc";
        let replacement = "x".repeat(100);
        let error = splice(
            original,
            &span(0, 3),
            &replacement,
            NewlinePolicy::Lf,
            Newline::Lf,
            10,
        )
        .expect_err("the result is over the budget");
        assert_eq!(
            error,
            SpliceError::TooLarge {
                limit: 10,
                bytes: 100
            }
        );
    }

    #[test]
    fn the_length_check_counts_the_converted_replacement() {
        // Four `\n` endings become four `\r\n` pairs: 4 bytes longer.
        let error = splice(
            "abcd",
            &span(0, 4),
            "1\n2\n3\n4\n",
            NewlinePolicy::CrLf,
            Newline::Lf,
            11,
        )
        .expect_err("12 bytes is over an 11-byte budget");
        assert_eq!(
            error,
            SpliceError::TooLarge {
                limit: 11,
                bytes: 12
            }
        );
    }

    #[test]
    fn preserve_writes_the_ending_the_replaced_span_uses() {
        let original = "a\r\nb\nc\r\n";
        // A CRLF span converts the replacement to CRLF.
        let output = splice_within(
            original,
            &span(0, 3),
            "x\ny",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "x\r\nyb\nc\r\n");

        // An LF span in the same file stays LF.
        let output = splice_within(
            original,
            &span(3, 5),
            "x\ny",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "a\r\nx\nyc\r\n");
    }

    #[test]
    fn preserve_falls_back_to_the_dominant_ending() {
        let original = "one\r\ntwo\r\n";
        let output = splice_within(
            original,
            &span(5, 8),
            "x\ny",
            NewlinePolicy::Preserve,
            Newline::CrLf,
        );
        assert_eq!(output, "one\r\nx\r\ny\r\n");

        let output = splice_within(
            original,
            &span(5, 8),
            "x\ny",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "one\r\nx\ny\r\n");
    }

    #[test]
    fn preserve_turns_a_lone_cr_in_the_span_into_lf() {
        let original = "a\rb\nc\n";
        let output = splice_within(
            original,
            &span(0, 2),
            "x\ry",
            NewlinePolicy::Preserve,
            Newline::CrLf,
        );
        assert_eq!(output, "x\nyb\nc\n");
    }

    #[test]
    fn explicit_policies_convert_only_the_replacement() {
        let original = "a\nb\nc\n";
        let output = splice_within(
            original,
            &span(2, 3),
            "x\ny",
            NewlinePolicy::CrLf,
            Newline::Lf,
        );
        assert_eq!(output, "a\nx\r\ny\nc\n");

        let original = "a\r\nb\r\nc\r\n";
        let output = splice_within(
            original,
            &span(3, 4),
            "x\r\ny",
            NewlinePolicy::Lf,
            Newline::CrLf,
        );
        assert_eq!(output, "a\r\nx\ny\r\nc\r\n");
    }

    #[test]
    fn replace_all_splices_every_range_once() {
        let original = "x\nkeep\nx\nkeep\nx\n";
        let output = splice_within(
            original,
            &[0..1, 7..8, 14..15],
            "y",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "y\nkeep\ny\nkeep\ny\n");
    }

    #[test]
    fn a_deleting_replacement_leaves_no_gap() {
        let original = "keep\ndrop\nkeep\n";
        let output = splice_within(
            original,
            &span(5, 10),
            "",
            NewlinePolicy::Preserve,
            Newline::Lf,
        );
        assert_eq!(output, "keep\nkeep\n");
    }

    /// Deterministic generator, so a failure is reproducible without a crate.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((self.0 >> 33) as usize) % bound
        }
    }

    #[test]
    fn bytes_outside_the_range_survive_a_splice() {
        let mut lcg = Lcg(0xC0FFEE);
        for _ in 0..64 {
            let mut original = String::new();
            if lcg.below(2) == 0 {
                original.push('\u{feff}');
            }
            for _ in 0..lcg.below(6) {
                original.push_str(if lcg.below(2) == 0 { "ab\r\n" } else { "cd\n" });
            }

            // The matcher never returns a range over the BOM, so neither does
            // this generator.
            let floor = if original.starts_with('\u{feff}') {
                3
            } else {
                0
            };
            let start = floor + lcg.below(original.len() - floor + 1);
            let end = start + lcg.below(original.len() - start + 1);
            let output = splice_within(
                &original,
                &span(start, end),
                "Z",
                NewlinePolicy::Preserve,
                Newline::Lf,
            );

            assert_eq!(&output[..start], &original[..start]);
            assert_eq!(output[start..start + 1], *"Z");
            assert_eq!(&output[start + 1..], &original[end..]);
            assert_eq!(
                output.starts_with('\u{feff}'),
                original.starts_with('\u{feff}')
            );
            assert_eq!(
                normalize_newlines(&output, Newline::Lf)
                    .matches('\u{feff}')
                    .count(),
                usize::from(original.starts_with('\u{feff}'))
            );
        }
    }
}
