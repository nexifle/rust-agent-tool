//! Streaming text line window.
//!
//! The scanner walks a slice of text with a cursor and renders one window of
//! numbered lines plus a footer. It performs no I/O, does not collect a line
//! per file, and never builds a `Vec` of every line. Only the current line is
//! inspected, and only its capped payload reaches the output string.
//!
//! This is the in-memory core of the read tool. The tool layer turns a
//! [`WindowOutcome`] into model-facing text and adds an error code when
//! [`WindowOutcome::decode_error`] is set.
//!
//! The scanner also accepts a prefix of a file. When
//! [`WindowRequest::entire_file`] is false it never treats the end of the
//! slice as EOF; it leaves an unterminated trailing line unconsumed and asks
//! for more bytes through [`WindowOutcome::needs_more`].

use std::fmt::Write as _;

/// Bytes of the output budget reserved for the footer.
///
/// The body budget is `max_output_bytes - FOOTER_RESERVE`. The footer, the
/// no-line notice, the past-end notice, and the capped-limit sentence all fit
/// inside this reserve.
pub(crate) const FOOTER_RESERVE: usize = 256;

/// Width used for line numbers that fit, matching `cat -n`.
const NUMBER_WIDTH: usize = 6;

/// Suffix appended to a line whose payload exceeded `max_line_bytes`.
const LINE_TRUNCATED_MARKER: &str = " [line truncated]";

/// One request for a line window over text already held in memory.
///
/// The caller has already validated `offset`, clamped `limit`, and checked
/// that `max_output_bytes` is at least 320. The scanner does not repeat those
/// checks.
pub(crate) struct WindowRequest {
    /// Text to scan, either the entire file or a prefix of it.
    ///
    /// The scanner walks this slice with a cursor. It never copies every line
    /// and never needs a second copy of the file.
    pub bytes: Vec<u8>,
    /// When true, `bytes` is the entire file. When false, `bytes` is only a prefix
    /// and the file continues, so the end of the slice is not EOF.
    pub entire_file: bool,
    /// How many complete lines exist before the first byte of `bytes`.
    ///
    /// The first line in `bytes` is numbered `lines_before + 1`. Zero means the
    /// slice starts at line 1.
    pub lines_before: u64,
    /// When true, the first line in `bytes` was already cut to `max_line_bytes`
    /// by the caller, so it is marked truncated even though the stored bytes fit.
    pub force_first_line_truncated: bool,
    /// 1-based first line to return. The caller validated it is at least 1.
    pub offset: u64,
    /// Maximum number of lines to return.
    ///
    /// The caller clamped it to `max_lines` and validated it is at least 1.
    pub limit: u32,
    /// Limit the model sent before clamping, at least 1.
    ///
    /// When it is greater than `limit`, the chosen footer gains one capped
    /// sentence.
    pub requested_limit: u32,
    /// Configured maximum for line count.
    ///
    /// Used only in the capped sentence.
    pub max_lines: u32,
    /// Per-line payload cap in bytes, at least 1.
    pub max_line_bytes: u32,
    /// Total output cap in bytes, at least 320. The footer reserve is part of
    /// this value.
    pub max_output_bytes: usize,
}

/// Result of one [`scan_window`] call.
pub(crate) struct WindowOutcome {
    /// Rendered numbered lines and footer, or a standalone notice.
    ///
    /// It is empty when [`WindowOutcome::decode_error`] is true and when
    /// [`WindowOutcome::needs_more`] is true, because neither is a final
    /// result the model should see.
    pub body: String,
    /// True when the window left out remaining lines because the line limit
    /// or the byte budget stopped the scan, or because no line fit.
    ///
    /// A window that reached the end of the file is not truncated even when an
    /// individual line carries the line truncation marker.
    pub truncated: bool,
    /// File line number of the first emitted line.
    ///
    /// When no line was emitted, this is `offset`. For the no-line, decode,
    /// and [`WindowOutcome::needs_more`] outcomes, [`WindowOutcome::end_line`]
    /// is one less than this value.
    pub start_line: u64,
    /// File line number of the last emitted line, or the total line count for
    /// the past-end notice.
    ///
    /// It is `offset - 1` when no line was emitted.
    pub end_line: u64,
    /// Total line count, known only when the scan reached the end of the file.
    ///
    /// `None` when the scan stopped early because of the limit or the budget,
    /// and `None` whenever [`WindowOutcome::needs_more`] is true.
    ///
    /// The read tool does not need the count because [`WindowOutcome::body`]
    /// already states it, but callers that report totals use this field.
    pub total_lines: Option<u64>,
    /// True when `offset` is past the last line.
    ///
    /// Then [`WindowOutcome::body`] is the past-end notice and the caller adds
    /// the offset-past-end code. This is not an error outcome.
    pub offset_past_eof: bool,
    /// True when the scanned text was not valid UTF-8.
    ///
    /// Then [`WindowOutcome::body`] is empty, no replacement character is
    /// used, and the caller reports the decode failure.
    pub decode_error: bool,
    /// True when the file continues and this slice did not fill the window.
    ///
    /// `body` is empty and is not a result for the model. The caller appends
    /// another chunk and scans again. A limit stop or a budget stop is final,
    /// so this is false even when the slice ended exactly on the last emitted
    /// line. An unfinished trailing line also sets this. In that case
    /// [`WindowOutcome::consumed`] stops at that line's first byte: the scanner
    /// has not consumed every byte it was given.
    ///
    /// When it is true, [`WindowOutcome::truncated`] is false,
    /// [`WindowOutcome::offset_past_eof`] is false,
    /// [`WindowOutcome::decode_error`] is false, and
    /// [`WindowOutcome::total_lines`] is `None`.
    pub needs_more: bool,
    /// Bytes the scanner finished.
    ///
    /// It counts only whole lines: the byte after the last line the scan
    /// finished reading, whether or not it was emitted. This excludes a
    /// trailing partial line when `entire_file` is false, and it excludes a
    /// line rejected by the output budget (the cursor moves back to that
    /// line's first byte). For a fully scanned file it equals `bytes.len()`.
    pub consumed: usize,
    /// Bytes before `offset` that a caller may drop before the next scan.
    ///
    /// Lines inside the requested window stay in the buffer so a later chunk
    /// can render them again. Zero when the slice already starts at `offset`.
    pub skipped: usize,
}

/// Why the scan loop stopped.
#[derive(Clone, Copy)]
enum StopReason {
    /// The slice ended with no unfinished line.
    EndOfInput,
    /// A prefix ended in a line without a `\n`; its bytes stay unconsumed.
    PartialLine,
    /// The line limit was reached.
    Limit,
    /// The next rendered line would not fit the body budget.
    Budget,
}

/// Scans one window of lines out of `request`.
///
/// The scan starts at the first byte and walks forward, so it must count the
/// lines before `offset`, but it never renders them. It stops as soon as the
/// line limit is reached or the next rendered line would not fit the body
/// budget. It reads to the end of the slice only when `offset` is past the
/// last line or when the slice is a prefix that ends before the window is
/// settled.
///
/// # Prefix scans
///
/// When [`WindowRequest::entire_file`] is false the end of the slice is not
/// EOF. A trailing line without a `\n` is not finished, so it is not counted
/// and its bytes are left unconsumed: [`WindowOutcome::consumed`] stops at the
/// start of that line. When the scan runs out of input before it can settle
/// the window, it sets [`WindowOutcome::needs_more`] and clears the body
/// instead of rendering a footer, because a partial window is not a final
/// answer. A stop for the limit or the byte budget is final and keeps its
/// usual footer even on a prefix.
///
/// # Line rules
///
/// Lines are split on `\n`. A `\r` immediately before `\n` is dropped; any
/// other `\r` is an ordinary byte. With an entire file, the final line counts
/// even without a trailing newline. Invalid UTF-8 anywhere the scan reaches is
/// reported through [`WindowOutcome::decode_error`] instead of a body.
#[must_use]
pub(crate) fn scan_window(request: WindowRequest) -> WindowOutcome {
    let WindowRequest {
        bytes,
        entire_file,
        lines_before,
        force_first_line_truncated,
        offset,
        limit,
        requested_limit,
        max_lines,
        max_line_bytes,
        max_output_bytes,
    } = request;

    let line_cap = usize::try_from(max_line_bytes).unwrap_or(usize::MAX);
    let body_budget = max_output_bytes.saturating_sub(FOOTER_RESERVE);
    let mut output = String::new();
    let reserve = body_budget.min(isize::MAX as usize);
    let _ = output.try_reserve(reserve);

    let mut cursor = 0;
    let mut line = lines_before;
    let mut emitted = 0_u32;
    let mut start_line = 0_u64;
    let mut end_line = 0_u64;
    let mut skipped = 0_usize;
    let mut stop = StopReason::EndOfInput;

    while cursor < bytes.len() {
        if emitted == limit {
            stop = StopReason::Limit;
            break;
        }

        let line_start = cursor;
        let Some((payload_end, next_cursor)) = split_line(&bytes, cursor, entire_file) else {
            stop = StopReason::PartialLine;
            break;
        };
        line += 1;

        let Ok(text) = std::str::from_utf8(&bytes[cursor..payload_end]) else {
            return WindowOutcome {
                body: String::new(),
                truncated: false,
                start_line: offset,
                end_line: offset.saturating_sub(1),
                total_lines: None,
                offset_past_eof: false,
                decode_error: true,
                needs_more: false,
                consumed: cursor,
                skipped,
            };
        };

        cursor = next_cursor;

        if line < offset {
            skipped = cursor;
            continue;
        }

        let (payload, marked) = cap_payload(text, line_cap);
        let marked = marked || (force_first_line_truncated && line == lines_before + 1);
        let marker = if marked { LINE_TRUNCATED_MARKER } else { "" };
        let width = number_width(line);
        let separator = usize::from(emitted > 0);
        let rendered = width + 1 + payload.len() + marker.len();
        if output.len() + separator + rendered > body_budget {
            // The line was inspected but not finished, so its bytes are not
            // consumed.
            cursor = line_start;
            stop = StopReason::Budget;
            break;
        }

        if emitted > 0 {
            output.push('\n');
        }
        let _ = write!(output, "{line:>width$}\t{payload}{marker}");
        emitted += 1;
        if emitted == 1 {
            start_line = line;
        }
        end_line = line;
    }

    if emitted == limit && cursor == bytes.len() && !entire_file {
        stop = StopReason::Limit;
    }

    if !entire_file
        && emitted < limit
        && matches!(stop, StopReason::EndOfInput | StopReason::PartialLine)
    {
        // The scanner consumed every finished byte it was given and the file
        // continues. The caller appends another chunk and scans again, so there
        // is no footer, no line total, and no result to show the model yet.
        return WindowOutcome {
            body: String::new(),
            truncated: false,
            start_line: offset,
            end_line: offset.saturating_sub(1),
            total_lines: None,
            offset_past_eof: false,
            decode_error: false,
            needs_more: true,
            consumed: cursor,
            skipped,
        };
    }

    let reached_eof = matches!(stop, StopReason::EndOfInput);
    let total = line;

    if emitted == 0 {
        if reached_eof && offset > total {
            let _ = write!(
                output,
                "File has {total} lines. Offset {offset} is past the end."
            );
            append_capped(&mut output, limit, requested_limit, max_lines);
            return WindowOutcome {
                body: output,
                truncated: false,
                start_line: offset,
                end_line: total,
                total_lines: Some(total),
                offset_past_eof: true,
                decode_error: false,
                needs_more: false,
                consumed: cursor,
                skipped,
            };
        }

        // A budget stop with no emitted line means a line was seen and did not
        // fit. A limit stop cannot happen before any line because the limit is
        // at least 1.
        let _ = write!(
            output,
            "No line fit in the output budget. Continue with offset={offset}."
        );
        append_capped(&mut output, limit, requested_limit, max_lines);
        return WindowOutcome {
            body: output,
            truncated: true,
            start_line: offset,
            end_line: offset.saturating_sub(1),
            total_lines: None,
            offset_past_eof: false,
            decode_error: false,
            needs_more: false,
            consumed: cursor,
            skipped,
        };
    }

    output.push('\n');
    if reached_eof {
        let _ = write!(output, "Showing lines {start_line}-{end_line} of {total}.");
        append_capped(&mut output, limit, requested_limit, max_lines);
        return WindowOutcome {
            body: output,
            truncated: false,
            start_line,
            end_line,
            total_lines: Some(total),
            offset_past_eof: false,
            decode_error: false,
            needs_more: false,
            consumed: cursor,
            skipped,
        };
    }

    let next = end_line.saturating_add(1);
    let _ = write!(
        output,
        "Showing lines {start_line}-{end_line}. More lines exist. Continue with offset={next}."
    );
    append_capped(&mut output, limit, requested_limit, max_lines);
    WindowOutcome {
        body: output,
        truncated: true,
        start_line,
        end_line,
        total_lines: None,
        offset_past_eof: false,
        decode_error: false,
        needs_more: false,
        consumed: cursor,
        skipped,
    }
}

/// Appends the capped-limit sentence when the applied limit was clamped.
///
/// The sentence starts with a space and stays inside [`FOOTER_RESERVE`].
fn append_capped(output: &mut String, limit: u32, requested_limit: u32, max_lines: u32) {
    if limit < requested_limit {
        let _ = write!(output, " Requested limit was capped at {max_lines}.");
    }
}

/// Splits the line starting at `start`.
///
/// Returns the end of the payload and the start of the next line. The payload
/// end excludes the `\n` and, for a CRLF line, the `\r` immediately before it.
/// A final line without a newline ends at the slice end when `entire_file` is
/// true; when it is false that line is unfinished and `None` is returned.
fn split_line(bytes: &[u8], start: usize, entire_file: bool) -> Option<(usize, usize)> {
    let Some(found) = bytes[start..].iter().position(|byte| *byte == b'\n') else {
        if entire_file {
            return Some((bytes.len(), bytes.len()));
        }
        return None;
    };

    let newline = start + found;
    let mut payload_end = newline;
    if payload_end > start && bytes[payload_end - 1] == b'\r' {
        payload_end -= 1;
    }
    Some((payload_end, newline + 1))
}

/// Width of the right-aligned line number column for `line`.
///
/// Numbers wider than [`NUMBER_WIDTH`] are printed in full.
fn number_width(line: u64) -> usize {
    let digits = line.ilog10() as usize + 1;
    digits.max(NUMBER_WIDTH)
}

/// Caps `text` to `max_line_bytes` without splitting a character.
///
/// Returns the capped payload and whether the marker must be appended. When
/// the cap lands inside a multibyte character, the payload is shortened to the
/// last character boundary that fits.
fn cap_payload(text: &str, max_line_bytes: usize) -> (&str, bool) {
    if text.len() <= max_line_bytes {
        return (text, false);
    }

    let mut end = max_line_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

#[cfg(test)]
mod tests {
    use super::{WindowOutcome, WindowRequest, number_width, scan_window};

    /// Builds a request for a whole file with budgets generous enough for a
    /// case to override only the field it exercises.
    fn request(bytes: &[u8]) -> WindowRequest {
        WindowRequest {
            bytes: bytes.to_vec(),
            entire_file: true,
            lines_before: 0,
            force_first_line_truncated: false,
            offset: 1,
            limit: 2000,
            requested_limit: 2000,
            max_lines: 2000,
            max_line_bytes: 2000,
            max_output_bytes: 48 * 1024,
        }
    }

    /// Builds a request for a prefix of a larger file.
    fn prefix_request(bytes: &[u8]) -> WindowRequest {
        let mut req = request(bytes);
        req.entire_file = false;
        req
    }

    /// Runs one scan and asserts the text is valid UTF-8.
    ///
    /// Every caller uses an entire file or a stop that is final, so the scan
    /// must not ask for more bytes.
    fn scan(request: WindowRequest) -> WindowOutcome {
        let outcome = scan_window(request);
        assert!(!outcome.decode_error, "unexpected decode failure");
        assert!(!outcome.needs_more, "unexpected request for more bytes");
        outcome
    }

    #[test]
    fn crlf_newlines_are_stripped() {
        let outcome = scan(request(b"one\r\ntwo\r\n"));

        assert_eq!(
            outcome.body,
            "     1\tone\n     2\ttwo\nShowing lines 1-2 of 2."
        );
        assert!(!outcome.truncated);
        assert_eq!(outcome.total_lines, Some(2));
        assert_eq!(outcome.start_line, 1);
        assert_eq!(outcome.end_line, 2);
        assert!(!outcome.offset_past_eof);
        assert!(!outcome.needs_more);
        assert_eq!(outcome.consumed, 10);
    }

    #[test]
    fn a_long_line_is_marked_and_later_lines_still_return() {
        let mut req = request(b"abcdefghij\nnext");
        req.max_line_bytes = 4;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\tabcd [line truncated]\n     2\tnext\nShowing lines 1-2 of 2."
        );
        assert!(!outcome.truncated);
        assert_eq!(outcome.total_lines, Some(2));
    }

    #[test]
    fn a_middle_window_reports_the_next_offset() {
        let mut req = request(b"one\ntwo\nthree");
        req.offset = 2;
        req.limit = 1;
        req.requested_limit = 1;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     2\ttwo\nShowing lines 2-2. More lines exist. Continue with offset=3."
        );
        assert!(outcome.truncated);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.start_line, 2);
        assert_eq!(outcome.end_line, 2);
    }

    #[test]
    fn a_full_file_has_no_continue_hint() {
        let outcome = scan(request(b"a\nb\nc"));

        assert_eq!(
            outcome.body,
            "     1\ta\n     2\tb\n     3\tc\nShowing lines 1-3 of 3."
        );
        assert!(!outcome.truncated);
        assert_eq!(outcome.total_lines, Some(3));
        assert_eq!(outcome.start_line, 1);
        assert_eq!(outcome.end_line, 3);
        assert!(!outcome.needs_more);
        assert_eq!(outcome.consumed, 5);
    }

    #[test]
    fn a_line_that_cannot_fit_returns_the_no_line_footer() {
        let mut req = request(&[b'x'; 100]);
        req.max_output_bytes = 320;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "No line fit in the output budget. Continue with offset=1."
        );
        assert!(outcome.truncated);
        assert_eq!(outcome.start_line, 1);
        assert_eq!(outcome.end_line, 0);
        assert_eq!(outcome.total_lines, None);
        assert!(!outcome.offset_past_eof);
    }

    #[test]
    fn the_footer_fits_when_the_next_line_does_not() {
        let mut bytes = b"abc\n".to_vec();
        bytes.extend_from_slice(&[b'x'; 60]);
        let mut req = request(&bytes);
        req.max_output_bytes = 320;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\tabc\nShowing lines 1-1. More lines exist. Continue with offset=2."
        );
        assert!(outcome.truncated);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.end_line, 1);
    }

    #[test]
    fn an_offset_past_the_end_reports_the_real_line_count() {
        let mut req = request(b"one\ntwo\nthree");
        req.offset = 5;
        let outcome = scan(req);

        assert_eq!(outcome.body, "File has 3 lines. Offset 5 is past the end.");
        assert!(outcome.offset_past_eof);
        assert!(!outcome.truncated);
        assert_eq!(outcome.total_lines, Some(3));
        assert_eq!(outcome.start_line, 5);
        assert_eq!(outcome.end_line, 3);
    }

    #[test]
    fn an_empty_slice_has_zero_lines() {
        let outcome = scan(request(b""));

        assert_eq!(outcome.body, "File has 0 lines. Offset 1 is past the end.");
        assert!(outcome.offset_past_eof);
        assert!(!outcome.truncated);
        assert_eq!(outcome.total_lines, Some(0));
        assert_eq!(outcome.end_line, 0);
        assert!(!outcome.needs_more);
        assert_eq!(outcome.consumed, 0);
    }

    #[test]
    fn an_entire_file_consumes_every_byte() {
        let bytes = b"a\nbb\nccc";
        let outcome = scan(request(bytes));

        assert_eq!(
            outcome.body,
            "     1\ta\n     2\tbb\n     3\tccc\nShowing lines 1-3 of 3."
        );
        assert!(!outcome.needs_more);
        assert_eq!(outcome.consumed, bytes.len());
    }

    #[test]
    fn a_fully_consumed_prefix_asks_for_more_instead_of_a_footer() {
        let outcome = scan_window(prefix_request(b"one\ntwo\n"));

        assert!(outcome.needs_more);
        assert!(outcome.body.is_empty());
        assert!(!outcome.truncated);
        assert!(!outcome.offset_past_eof);
        assert!(!outcome.decode_error);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.start_line, 1);
        assert_eq!(outcome.end_line, 0);
        assert_eq!(outcome.consumed, 8);
    }

    #[test]
    fn the_same_bytes_as_an_entire_file_have_a_total() {
        let outcome = scan(request(b"one\ntwo\n"));

        assert_eq!(
            outcome.body,
            "     1\tone\n     2\ttwo\nShowing lines 1-2 of 2."
        );
        assert!(!outcome.needs_more);
        assert_eq!(outcome.total_lines, Some(2));
        assert_eq!(outcome.consumed, 8);
    }

    #[test]
    fn a_prefix_without_a_final_newline_leaves_the_partial_line_unconsumed() {
        let outcome = scan_window(prefix_request(b"one\ntwo"));

        assert!(outcome.needs_more);
        assert!(outcome.body.is_empty());
        assert!(!outcome.truncated);
        assert!(!outcome.offset_past_eof);
        assert!(!outcome.decode_error);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.consumed, 4);
    }

    #[test]
    fn a_prefix_with_no_finished_line_asks_for_more_rather_than_past_end() {
        let mut req = prefix_request(b"one\ntwo\n");
        req.offset = 5;
        let outcome = scan_window(req);

        assert!(outcome.needs_more);
        assert!(outcome.body.is_empty());
        assert!(!outcome.offset_past_eof);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.consumed, 8);
    }

    #[test]
    fn a_prefix_with_only_a_partial_line_asks_for_more() {
        let outcome = scan_window(prefix_request(b"one"));

        assert!(outcome.needs_more);
        assert!(outcome.body.is_empty());
        assert_eq!(outcome.consumed, 0);
    }

    #[test]
    fn an_empty_prefix_asks_for_more() {
        let outcome = scan_window(prefix_request(b""));

        assert!(outcome.needs_more);
        assert!(outcome.body.is_empty());
        assert!(!outcome.offset_past_eof);
        assert_eq!(outcome.consumed, 0);
    }

    #[test]
    fn a_limit_stop_on_a_prefix_is_final() {
        let mut req = prefix_request(b"one\ntwo\nthree\n");
        req.limit = 1;
        req.requested_limit = 1;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\tone\nShowing lines 1-1. More lines exist. Continue with offset=2."
        );
        assert!(!outcome.needs_more);
        assert!(outcome.truncated);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.consumed, 4);
    }

    #[test]
    fn a_prefix_partial_line_after_a_limit_stop_does_not_change_it() {
        let mut req = prefix_request(b"one\ntwo");
        req.limit = 1;
        req.requested_limit = 1;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\tone\nShowing lines 1-1. More lines exist. Continue with offset=2."
        );
        assert!(!outcome.needs_more);
        assert!(outcome.truncated);
        assert_eq!(outcome.consumed, 4);
    }

    #[test]
    fn a_budget_stop_on_a_prefix_is_final() {
        let mut req = prefix_request(b"one\ntwo\nthree\n");
        req.max_output_bytes = 320;
        req.max_line_bytes = 1;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\to [line truncated]\n     2\tt [line truncated]\nShowing lines 1-2. More lines exist. Continue with offset=3."
        );
        assert!(!outcome.needs_more);
        assert!(outcome.truncated);
        assert_eq!(outcome.consumed, 8);
    }

    #[test]
    fn a_prefix_line_that_cannot_fit_uses_the_no_line_footer_not_needs_more() {
        let mut bytes = vec![b'x'; 100];
        bytes.push(b'\n');
        let mut req = prefix_request(&bytes);
        req.max_output_bytes = 320;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "No line fit in the output budget. Continue with offset=1."
        );
        assert!(!outcome.needs_more);
        assert!(outcome.truncated);
        assert!(!outcome.offset_past_eof);
        assert_eq!(outcome.consumed, 0);
    }

    #[test]
    fn a_prefix_line_without_a_newline_and_no_room_asks_for_more() {
        let mut req = prefix_request(&[b'x'; 100]);
        req.max_output_bytes = 320;
        let outcome = scan_window(req);

        // Without the newline the line is not finished, so this is not a
        // budget stop and the caller must supply more bytes.
        assert!(outcome.needs_more);
        assert!(outcome.body.is_empty());
        assert_eq!(outcome.consumed, 0);
    }

    #[test]
    fn a_capped_request_is_named_in_the_footer() {
        let mut req = request(b"one\ntwo\nthree");
        req.limit = 2;
        req.requested_limit = 1_000_000;
        req.max_lines = 2;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\tone\n     2\ttwo\nShowing lines 1-2. More lines exist. Continue with offset=3. Requested limit was capped at 2."
        );
        assert!(outcome.truncated);
        assert_eq!(outcome.total_lines, None);
    }

    #[test]
    fn a_capped_request_still_ends_cleanly_at_eof() {
        let mut req = request(b"one\ntwo");
        req.limit = 2;
        req.requested_limit = 5;
        req.max_lines = 2;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\tone\n     2\ttwo\nShowing lines 1-2 of 2. Requested limit was capped at 2."
        );
        assert!(!outcome.truncated);
        assert_eq!(outcome.total_lines, Some(2));
    }

    #[test]
    fn the_no_line_footer_carries_the_capped_sentence() {
        let mut req = request(&[b'x'; 100]);
        req.max_output_bytes = 320;
        req.limit = 2;
        req.requested_limit = 10;
        req.max_lines = 2;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "No line fit in the output budget. Continue with offset=1. Requested limit was capped at 2."
        );
        assert!(outcome.truncated);
        assert_eq!(outcome.end_line, 0);
    }

    #[test]
    fn the_past_end_notice_carries_the_capped_sentence() {
        let mut req = request(b"one\ntwo");
        req.offset = 4;
        req.limit = 2;
        req.requested_limit = 10;
        req.max_lines = 2;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "File has 2 lines. Offset 4 is past the end. Requested limit was capped at 2."
        );
        assert!(outcome.offset_past_eof);
        assert!(!outcome.truncated);
    }

    #[test]
    fn invalid_utf8_reports_a_decode_error_without_replacement() {
        let outcome = scan_window(request(b"ok\n\xff\n"));

        assert!(outcome.decode_error);
        assert!(outcome.body.is_empty());
        assert!(!outcome.body.contains('\u{FFFD}'));
        assert!(!outcome.truncated);
        assert!(!outcome.offset_past_eof);
        assert!(!outcome.needs_more);
        assert_eq!(outcome.total_lines, None);
        assert_eq!(outcome.consumed, 3);
    }

    #[test]
    fn an_incomplete_sequence_at_eof_is_a_decode_error() {
        let outcome = scan_window(request(b"ok\n\xc3"));

        assert!(outcome.decode_error);
        assert!(outcome.body.is_empty());
        assert!(!outcome.needs_more);
    }

    #[test]
    fn a_multibyte_character_is_kept_when_it_fits() {
        let outcome = scan(request(b"\xc3\xa9\n"));

        assert_eq!(outcome.body, "     1\t\u{e9}\nShowing lines 1-1 of 1.");
    }

    #[test]
    fn a_cut_never_splits_a_multibyte_character() {
        let mut req = request(b"a\xc3\xa9");
        req.max_line_bytes = 2;
        let outcome = scan(req);

        assert_eq!(
            outcome.body,
            "     1\ta [line truncated]\nShowing lines 1-1 of 1."
        );
    }

    #[test]
    fn a_bare_carriage_return_stays_in_the_payload() {
        let outcome = scan(request(b"a\rb\n"));

        assert_eq!(outcome.body, "     1\ta\rb\nShowing lines 1-1 of 1.");
    }

    #[test]
    fn line_one_is_six_columns_wide_before_the_tab() {
        let outcome = scan(request(b"hi\n"));
        let (number, rest) = outcome.body.split_once('\t').expect("a tab");

        assert_eq!(number, "     1");
        assert_eq!(number.len(), 6);
        assert!(rest.starts_with("hi\n"));
    }

    #[test]
    fn wide_line_numbers_are_not_padded() {
        assert_eq!(number_width(1), 6);
        assert_eq!(number_width(999_999), 6);
        assert_eq!(number_width(1_000_000), 7);
    }

    #[test]
    fn a_limit_that_ends_exactly_at_a_prefix_is_final() {
        let mut req = prefix_request(b"one\n");
        req.limit = 1;
        req.requested_limit = 1;
        let outcome = scan(req);

        assert!(outcome.body.contains("     1\tone"));
        assert!(outcome.body.contains("More lines exist"));
        assert!(outcome.truncated);
        assert_eq!(outcome.consumed, 4);
    }
}
