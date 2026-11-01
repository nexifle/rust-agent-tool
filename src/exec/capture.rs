//! Head and tail window for command output.
//!
//! [`Capture`] does not read pipes, strip ANSI, or keep bytes that fall out of
//! the window. The runner calls [`Capture::push`]. Dropped bytes are counted in
//! [`Capture::omitted_bytes`] and then discarded.

use crate::ToolError;
use crate::exec::config::ExecConfig;

const ELLIPSIS: &[u8] = "…".as_bytes();

/// Streaming head/tail buffer bounded by the exec output budget.
///
/// `head_bytes == 0` keeps only the tail. `head_bytes == max_output_bytes`
/// keeps only the head. A line longer than `max_line_bytes` cannot fill the
/// window by itself.
#[derive(Debug, Clone)]
pub struct Capture {
    max_output_bytes: usize,
    head_cap: usize,
    max_line_bytes: usize,
    head: Vec<u8>,
    tail: Vec<u8>,
    head_closed: bool,
    omitted: u64,
    line_stored: usize,
    skipping_line: bool,
    line_marked: bool,
    pending: Vec<u8>,
}

impl Capture {
    /// Builds a window. `head_bytes` must be at most `max_output_bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `head_bytes` is larger
    /// than `max_output_bytes`.
    pub fn new(
        max_output_bytes: usize,
        head_bytes: usize,
        max_line_bytes: usize,
    ) -> Result<Self, ToolError> {
        if head_bytes > max_output_bytes {
            return Err(ToolError::spec_invalid(
                "head_bytes must be less than or equal to max_output_bytes",
            ));
        }
        Ok(Self {
            max_output_bytes,
            head_cap: head_bytes,
            max_line_bytes,
            head: Vec::new(),
            tail: Vec::new(),
            omitted: 0,
            line_stored: 0,
            skipping_line: false,
            line_marked: false,
            pending: Vec::new(),
            head_closed: head_bytes == 0,
        })
    }

    /// Builds a window from an [`ExecConfig`] that already enforces the head cap.
    #[must_use]
    pub fn from_config(config: &ExecConfig) -> Self {
        Self::new(
            config.max_output_bytes(),
            config.head_bytes(),
            config.max_line_bytes(),
        )
        .expect("exec config keeps head_bytes within max_output_bytes")
    }

    /// Appends bytes. The window never grows past `max_output_bytes`.
    pub fn push(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.pending.push(*byte);
            match classify_pending(&self.pending) {
                Pending::More => {}
                Pending::Char => {
                    let unit = std::mem::take(&mut self.pending);
                    self.accept_unit(&unit);
                }
                Pending::Bad => {
                    self.omitted += 1;
                    let rest = self.pending[1..].to_vec();
                    self.pending.clear();
                    self.push(&rest);
                }
            }
        }
    }

    /// Bytes dropped from the model window.
    #[must_use]
    pub fn omitted_bytes(&self) -> u64 {
        self.omitted
    }

    /// Filled prefix. Always valid UTF-8.
    #[must_use]
    pub fn head(&self) -> &[u8] {
        &self.head
    }

    /// Filled suffix. Always valid UTF-8.
    #[must_use]
    pub fn tail(&self) -> &[u8] {
        &self.tail
    }

    /// Model-facing body, without the working directory or the status line.
    ///
    /// An empty window renders as `(no output)`. When bytes were omitted, the
    /// marker is `[omitted {n} bytes]`. A spill locator is appended on that
    /// marker only.
    #[must_use]
    pub fn render_body(&mut self, spill_locator: Option<&str>) -> String {
        self.discard_pending();
        if self.head.is_empty() && self.tail.is_empty() {
            return "(no output)".to_string();
        }
        let mut out = String::new();
        out.push_str(utf8_window(&self.head));
        if self.omitted > 0 {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("[omitted {} bytes]", self.omitted));
            if let Some(locator) = spill_locator {
                out.push_str(" full output: ");
                out.push_str(locator);
            }
            if !self.tail.is_empty() {
                out.push('\n');
            }
        }
        out.push_str(utf8_window(&self.tail));
        out
    }

    fn discard_pending(&mut self) {
        if !self.pending.is_empty() {
            self.omitted += self.pending.len() as u64;
            self.pending.clear();
        }
    }

    fn accept_unit(&mut self, unit: &[u8]) {
        if unit == b"\n" {
            if !self.skipping_line && self.line_stored < self.max_line_bytes {
                self.store(unit);
            } else {
                self.omitted += unit.len() as u64;
            }
            self.line_stored = 0;
            self.skipping_line = false;
            self.line_marked = false;
            return;
        }
        if self.skipping_line {
            self.omitted += unit.len() as u64;
            return;
        }
        let content_budget = self.max_line_bytes.saturating_sub(ELLIPSIS.len());
        if self.line_stored.saturating_add(unit.len()) <= content_budget
            || self.max_line_bytes < ELLIPSIS.len()
                && self.line_stored.saturating_add(unit.len()) <= self.max_line_bytes
        {
            self.store(unit);
            self.line_stored += unit.len();
            return;
        }
        if !self.line_marked
            && self.line_stored.saturating_add(ELLIPSIS.len()) <= self.max_line_bytes
        {
            self.store(ELLIPSIS);
            self.line_stored += ELLIPSIS.len();
            self.line_marked = true;
        }
        self.omitted += unit.len() as u64;
        self.skipping_line = true;
    }

    fn store(&mut self, unit: &[u8]) {
        if !self.head_closed && self.head.len() + unit.len() <= self.head_cap {
            self.head.extend_from_slice(unit);
            return;
        }
        self.head_closed = true;
        let tail_limit = self.max_output_bytes.saturating_sub(self.head.len());
        if unit.len() > tail_limit {
            self.omitted += unit.len() as u64;
            return;
        }
        while self.tail.len() + unit.len() > tail_limit {
            let width = utf8_width(self.tail.first().copied().unwrap_or(0));
            let width = width.min(self.tail.len()).max(1);
            self.tail.drain(..width);
            self.omitted += u64::try_from(width).unwrap_or(u64::MAX);
        }
        self.tail.extend(unit);
    }
}

enum Pending {
    More,
    Char,
    Bad,
}

fn classify_pending(pending: &[u8]) -> Pending {
    let Some(&first) = pending.first() else {
        return Pending::More;
    };
    if first < 0x80 {
        return Pending::Char;
    }
    let need = utf8_width(first);
    if need == 1 || pending.len() > 4 {
        return Pending::Bad;
    }
    if pending[1..].iter().any(|byte| byte & 0xC0 != 0x80) {
        return Pending::Bad;
    }
    if pending.len() < need {
        return Pending::More;
    }
    if std::str::from_utf8(pending).is_ok() {
        Pending::Char
    } else {
        Pending::Bad
    }
}

fn utf8_window(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).expect("capture window stays on char boundaries")
}

fn utf8_width(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first & 0xE0 == 0xC0 {
        2
    } else if first & 0xF0 == 0xE0 {
        3
    } else if first & 0xF8 == 0xF0 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::Capture;

    #[test]
    fn head_tail_marker_counts_dropped_bytes() {
        let mut capture = Capture::new(20, 8, 100).unwrap();
        capture.push(b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(capture.head(), b"abcdefgh");
        assert_eq!(capture.tail(), b"opqrstuvwxyz");
        assert_eq!(capture.omitted_bytes(), 6);
        let body = capture.render_body(None);
        assert!(body.contains("[omitted 6 bytes]"), "{body}");
        assert!(body.starts_with("abcdefgh"));
        assert!(body.ends_with("opqrstuvwxyz"));
    }

    #[test]
    fn utf8_cut_does_not_split_or_insert_replacement() {
        let mut capture = Capture::new(3, 1, 32).unwrap();
        capture.push(&[0xC3, 0xA9, b'Z']);
        assert!(std::str::from_utf8(capture.head()).is_ok());
        assert!(std::str::from_utf8(capture.tail()).is_ok());
        let body = capture.render_body(None);
        assert!(!body.contains('\u{FFFD}'), "{body}");
        assert!(body.contains('é'), "{body}");
    }

    #[test]
    fn invalid_lead_byte_is_omitted_not_stored() {
        let mut capture = Capture::new(64, 16, 200).unwrap();
        capture.push(&[0xC3, b'A', b'B']);
        assert!(std::str::from_utf8(capture.head()).is_ok());
        assert!(std::str::from_utf8(capture.tail()).is_ok());
        assert_eq!(capture.omitted_bytes(), 1);
        let body = capture.render_body(None);
        assert!(!body.contains('\u{FFFD}'), "{body}");
        assert!(body.contains("AB"), "{body}");
    }

    #[test]
    fn long_line_does_not_fill_the_window() {
        let mut capture = Capture::new(200, 100, 32).unwrap();
        let mut input = vec![b'a'; 100];
        input.push(b'\n');
        input.extend_from_slice(b"OK\n");
        capture.push(&input);
        assert!(capture.omitted_bytes() > 0);
        let body = capture.render_body(None);
        assert!(body.contains("OK"), "{body}");
        assert!(body.contains('…'), "{body}");
        assert!(capture.head().len() + capture.tail().len() < 100);
    }

    #[test]
    fn empty_render_is_a_notice() {
        let mut capture = Capture::new(48, 16, 32).unwrap();
        assert_eq!(capture.render_body(None), "(no output)");
    }

    #[test]
    fn spill_locator_only_when_bytes_were_omitted() {
        let mut capture = Capture::new(4, 4, 32).unwrap();
        capture.push(b"abcd");
        assert_eq!(capture.render_body(Some("/tmp/out")), "abcd");
        capture.push(b"ef");
        let body = capture.render_body(Some("/tmp/out"));
        assert!(
            body.contains("[omitted 2 bytes] full output: /tmp/out"),
            "{body}"
        );
    }

    #[test]
    fn zero_head_keeps_only_the_tail() {
        let mut capture = Capture::new(4, 0, 32).unwrap();
        capture.push(b"abcdef");
        assert!(capture.head().is_empty());
        assert_eq!(capture.tail(), b"cdef");
        assert_eq!(capture.omitted_bytes(), 2);
    }

    #[test]
    fn full_head_keeps_no_tail() {
        let mut capture = Capture::new(4, 4, 32).unwrap();
        capture.push(b"abcdef");
        assert_eq!(capture.head(), b"abcd");
        assert!(capture.tail().is_empty());
        assert_eq!(capture.omitted_bytes(), 2);
    }

    #[test]
    fn head_above_max_is_spec_invalid() {
        let error = Capture::new(10, 11, 32).unwrap_err();
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID"), "{error}");
    }
}
