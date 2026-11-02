//! The edit tool implementation.
//!
//! [`EditTool::execute`] is the real entry point. It parses the model input,
//! runs the request checks on the model strings, asks the host for the file
//! bytes, checks them, matches the text, splices the whole new file, and hands
//! both strings to [`EditSink::apply`]. [`EditTool::call`] is a thin adapter
//! that renders an [`EditOutcome`] for the model.
//!
//! The tool holds the config and both hooks by value. It keeps no memory of an
//! earlier call: a second call on the same path asks the host again.

use serde_json::Value;

use crate::{CallContext, Tool, ToolError, ToolOutput, ToolSpec, async_trait};

use super::check::{CheckRejection, NO_CHANGE, check_file, check_request, too_large};
use super::config::EditConfig;
use super::input::{input_schema, parse_input};
use super::matcher::{MatchResult, MatchTier, Search, find_matches};
use super::sink::{ApplyError, EditRefusal, EditSink, SINK_FAILED, STALE};
use super::source::{EditSource, OpenError};
use super::splice::{SpliceError, splice};

/// Code for an `old_text` that occurs nowhere, at every enabled tier.
pub const NO_MATCH: &str = "AGT_EDIT_NO_MATCH";

/// Code for an `old_text` that occurs more than once without `replace_all`.
pub const NOT_UNIQUE: &str = "AGT_EDIT_NOT_UNIQUE";

/// Code for a `replace_all` fan-out above the configured limit.
pub const TOO_MANY: &str = "AGT_EDIT_TOO_MANY";

/// Code for an unexpected failure inside [`EditSource::open`].
pub const SOURCE_FAILED: &str = "AGT_EDIT_SOURCE_FAILED";

/// Note a not-found path needs when [`EditSource::open`] reported it.
const CREATE_WITH_WRITE_NOTE: &str = "; use the write tool to create a file";

/// Note a not-found path needs when [`EditSink::apply`] reported it.
const DISAPPEARED_NOTE: &str = "; the file disappeared after it was read";

/// Note appended only to a [`SOURCE_FAILED`] message.
///
/// Nothing was written, so the model must not be told to re-read a file that
/// may have been half-written.
const UNCHANGED_NOTE: &str = "; the file was not changed";

/// Sentence appended only when [`EditSink::apply`] fails.
///
/// The SDK cannot tell whether any byte reached the file, so the model is told
/// to re-read before trusting it.
const PARTIAL_WRITE_NOTE: &str = "The file may have been partially written.";

/// The result of executing one edit request.
///
/// [`EditTool::call`] renders each variant for the model. An operational
/// failure is [`EditOutcome::Failed`], not a [`ToolError`], so the model can see
/// the code and recover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOutcome {
    /// The host applied the edit.
    Edited {
        /// Replacements applied. One unless `replace_all` was true.
        matches: usize,
        /// Which tier produced the match.
        tier: MatchTier,
        /// UTF-8 bytes the host reports on disk after the write.
        bytes: usize,
        /// Success sentence for the model, including the path it sent.
        text: String,
    },
    /// The edit failed for an operational reason.
    Failed {
        /// Stable code, such as [`NOT_UNIQUE`] or [`STALE`].
        code: &'static str,
        /// Rendered text in the form `AGT_CODE: message`.
        text: String,
    },
}

/// A tool that replaces one exact region of a file through host hooks.
///
/// The tool never opens or writes a file itself. The host injects a source and
/// a sink when the tool is constructed, and the sink receives the complete
/// before and after strings for its compare-and-swap.
///
/// The tool is `Sync` because [`Tool::call`] takes `&self`. That does not
/// serialize two calls: the host serializes edits to one path when it needs to.
#[derive(Debug)]
pub struct EditTool<O: EditSource, A: EditSink> {
    source: O,
    sink: A,
    config: EditConfig,
    spec: ToolSpec,
}

impl<O: EditSource, A: EditSink> EditTool<O, A> {
    /// Builds an edit tool from `config`, a host `source`, and a host `sink`.
    ///
    /// The source comes before the sink because a call reads before it writes.
    /// The name, description, and parameter descriptions in the registered
    /// specification come from `config`.
    #[must_use]
    pub fn new(config: EditConfig, source: O, sink: A) -> Self {
        let spec = spec_from_config(&config);
        Self {
            source,
            sink,
            config,
            spec,
        }
    }

    /// The tool specification registered with the agent.
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Runs one edit request.
    ///
    /// The checks run in this order, and the first match wins:
    ///
    /// 1. `parse_input` rejects unusable model JSON with
    ///    [`crate::ErrorCode::InputInvalid`]. Neither hook runs.
    /// 2. `check_request` runs on the model strings. A rejection is
    ///    [`EditOutcome::Failed`] and neither hook runs.
    /// 3. [`EditSource::open`] returns the file bytes. A refusal maps to its
    ///    code; an unexpected failure is [`SOURCE_FAILED`]. The sink does not
    ///    run.
    /// 4. The byte budget, the UTF-8 decode, and the NUL scan run on those
    ///    bytes.
    /// 5. `find_matches` runs the enabled tiers. A miss, an ambiguous match,
    ///    and an occurrence overflow are distinct codes.
    /// 6. `splice` computes the exact final length before it allocates.
    ///    An over-budget result is
    ///    [`super::check::TOO_LARGE`]; a result equal to the original is
    ///    [`NO_CHANGE`]. Neither calls the sink.
    /// 7. [`EditSink::apply`] receives the complete before and after strings. A
    ///    refusal maps to its code, a compare-and-swap mismatch is [`STALE`],
    ///    and an unexpected failure is [`SINK_FAILED`] with a note that the
    ///    file may have been partially written.
    ///
    /// The tool keeps no memory of an earlier call. A second call on the same
    /// path calls both hooks again.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InputInvalid`] when the model input is not
    /// usable. Every operational failure is an [`EditOutcome::Failed`] instead.
    pub async fn execute(&self, input: &Value) -> Result<EditOutcome, ToolError> {
        let input = parse_input(input)?;

        if let Err(rejection) = check_request(&input.old_text, &input.new_text, &self.config) {
            return Ok(failed_rejection(&rejection));
        }

        let file = match self.source.open(&input.path).await {
            Ok(file) => file,
            Err(OpenError::Refusal(refusal)) => {
                return Ok(failed_edit_refusal(&refusal, CREATE_WITH_WRITE_NOTE));
            }
            Err(OpenError::Failed(failure)) => {
                return Ok(failed(
                    SOURCE_FAILED,
                    &format!("{}{UNCHANGED_NOTE}", failure.message),
                ));
            }
        };

        let original = match check_file(&file.bytes, &self.config) {
            Ok(text) => text,
            Err(rejection) => return Ok(failed_rejection(&rejection)),
        };

        let search = Search {
            original,
            needle: &input.old_text,
            mode: self.config.match_mode(),
            replace_all: input.replace_all,
            max_occurrences: self.config.max_occurrences(),
            fuzzy_threshold: self.config.fuzzy_threshold(),
        };
        let (ranges, tier, dominant) = match find_matches(&search) {
            MatchResult::Matched {
                tier,
                ranges,
                dominant,
            } => (ranges, tier, dominant),
            MatchResult::NoMatch => {
                return Ok(failed(
                    NO_MATCH,
                    "old_text was not found; read the file and copy the text exactly",
                ));
            }
            MatchResult::NotUnique {
                count,
                lines,
                capped,
            } => {
                return Ok(failed(
                    NOT_UNIQUE,
                    &not_unique_message(count, &lines, capped),
                ));
            }
            MatchResult::TooMany => {
                return Ok(failed(
                    TOO_MANY,
                    &too_many_message(self.config.max_occurrences()),
                ));
            }
        };

        let after = match splice(
            original,
            &ranges,
            &input.new_text,
            self.config.newline(),
            dominant,
            self.config.max_file_bytes(),
        ) {
            Ok(after) => after,
            Err(SpliceError::TooLarge { limit, bytes }) => {
                return Ok(failed_rejection(&too_large(
                    "the edited file",
                    limit,
                    bytes,
                )));
            }
        };
        if after == original {
            return Ok(failed(
                NO_CHANGE,
                "old_text and new_text are identical; the file was not changed",
            ));
        }

        match self.sink.apply(&input.path, original, &after).await {
            Ok(permit) => Ok(edited(&input.path, ranges.len(), tier, permit.bytes)),
            Err(ApplyError::Refusal(refusal)) => {
                Ok(failed_edit_refusal(&refusal, DISAPPEARED_NOTE))
            }
            Err(ApplyError::Stale) => Ok(failed(STALE, STALE_MESSAGE)),
            Err(ApplyError::Failed(failure)) => Ok(failed(
                SINK_FAILED,
                &format!("{} {PARTIAL_WRITE_NOTE}", failure.message),
            )),
        }
    }
}

/// Message for a compare-and-swap mismatch.
const STALE_MESSAGE: &str = "file changed on disk after it was read; read it again";

/// Builds the success outcome and its sentence.
///
/// A tier other than [`MatchTier::Exact`] appends a note, because a model that
/// landed a lenient or fuzzy match should re-read the file before its next edit.
fn edited(path: &str, matches: usize, tier: MatchTier, bytes: usize) -> EditOutcome {
    let note = match tier {
        MatchTier::Exact => "",
        MatchTier::Lenient => " Matched with the lenient tier.",
        MatchTier::Fuzzy => " Matched with the fuzzy tier.",
    };
    EditOutcome::Edited {
        matches,
        tier,
        bytes,
        text: format!("Edited {path}: replaced {matches} occurrence(s), {bytes} bytes.{note}"),
    }
}

/// Builds the ambiguity message, which reports line numbers and never file text.
///
/// `count` is a lower bound when the scan stopped at the report cap, so the
/// message says `10 or more` rather than claiming a total.
fn not_unique_message(count: usize, lines: &[usize], capped: bool) -> String {
    let occurrences = if capped {
        format!("{count} or more")
    } else {
        count.to_string()
    };
    let lines: Vec<String> = lines.iter().map(usize::to_string).collect();
    format!(
        "old_text occurs {occurrences} times at lines: {}. Please provide more context in \
         old_text to uniquely identify the location.",
        lines.join(", ")
    )
}

/// Builds the fan-out message for a `replace_all` over the configured cap.
fn too_many_message(max_occurrences: usize) -> String {
    format!(
        "old_text occurs more than {max_occurrences} times; send a more specific old_text or edit \
         in smaller batches"
    )
}

/// Builds a failure outcome from a stable code and a message.
fn failed(code: &'static str, message: &str) -> EditOutcome {
    EditOutcome::Failed {
        code,
        text: format!("{code}: {message}"),
    }
}

/// Builds a failure outcome from a request or file rejection.
fn failed_rejection(rejection: &CheckRejection) -> EditOutcome {
    failed(rejection.code, &rejection.message)
}

/// Builds a failure outcome from a host refusal, adding where it was decided.
///
/// A not-found path means different things from the two hooks, so `note`
/// carries the recovery instruction that fits the stage.
fn failed_edit_refusal(refusal: &EditRefusal, note: &str) -> EditOutcome {
    let message = refusal.message();
    let text = if matches!(refusal, EditRefusal::NotFound { .. }) {
        format!("{message}{note}")
    } else {
        message.to_string()
    };
    failed(refusal.code(), &text)
}

/// Builds the registered spec from an already validated config.
///
/// [`EditConfig`] rejects an unusable name or description in its setters, and
/// the input schema is always a JSON object, so the spec constructor cannot fail
/// for a value built through those setters.
fn spec_from_config(config: &EditConfig) -> ToolSpec {
    ToolSpec::new(config.name(), config.description(), input_schema(config))
        .expect("edit config name and description are validated by their setters")
}

#[async_trait]
impl<O: EditSource + 'static, A: EditSink + 'static> Tool for EditTool<O, A> {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &CallContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.execute(&input).await? {
            EditOutcome::Edited { text, .. } => Ok(ToolOutput::text(text)),
            EditOutcome::Failed { text, .. } => Ok(ToolOutput::error_text(text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
    use std::task::{Context, Poll, Waker};

    use serde_json::json;

    use super::*;
    use crate::CallContext;
    use crate::edit::check::{BINARY, EMPTY_OLD, NOT_TEXT, TOO_LARGE};
    use crate::edit::config::{EditConfig, MatchMode};
    use crate::edit::sink::{
        ApplyError, EditPermit, FORBIDDEN, IS_DIRECTORY, OUTSIDE_WORKSPACE, REJECTED,
    };
    use crate::edit::source::{EditFile, NOT_FOUND};
    use crate::host::HostFailure;

    /// Runs a future that the in-memory hooks never park.
    fn run<F: std::future::Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("edit future should not wait"),
        }
    }

    /// Calls recorded by [`RecordingHost`], in call order.
    #[derive(Debug, Default)]
    struct Recorded {
        /// Paths passed to `open`.
        opens: Vec<String>,
        /// `(path, before, after)` passed to `apply`.
        applies: Vec<(String, String, String)>,
    }

    /// In-memory [`EditSource`] and [`EditSink`] that record every call and
    /// script their answers.
    ///
    /// The record is shared through an [`Arc`], so one value serves as both
    /// hooks of the same tool and the test reads every call from one place.
    #[derive(Debug, Clone)]
    struct RecordingHost {
        bytes: Vec<u8>,
        open_error: Option<OpenError>,
        apply_error: Option<ApplyError>,
        permit_bytes: usize,
        recorded: Arc<Mutex<Recorded>>,
    }

    impl RecordingHost {
        /// A host whose file holds `text` and whose apply reports `text.len()`.
        fn new(text: &str) -> Self {
            Self {
                bytes: text.as_bytes().to_vec(),
                open_error: None,
                apply_error: None,
                permit_bytes: text.len(),
                recorded: Arc::default(),
            }
        }

        /// A host whose file holds arbitrary bytes, which may not be UTF-8.
        fn with_bytes(bytes: impl Into<Vec<u8>>) -> Self {
            let bytes = bytes.into();
            Self {
                bytes,
                open_error: None,
                apply_error: None,
                permit_bytes: 0,
                recorded: Arc::default(),
            }
        }

        /// Makes every `open` return a refusal.
        fn refusing_open(mut self, refusal: EditRefusal) -> Self {
            self.open_error = Some(OpenError::Refusal(refusal));
            self
        }

        /// Makes every `open` return an unexpected failure.
        fn open_failed(mut self, failure: HostFailure) -> Self {
            self.open_error = Some(OpenError::Failed(failure));
            self
        }

        /// Makes every `apply` return a refusal.
        fn refusing_apply(mut self, refusal: EditRefusal) -> Self {
            self.apply_error = Some(ApplyError::Refusal(refusal));
            self
        }

        /// Makes every `apply` report a compare-and-swap mismatch.
        fn stale(mut self) -> Self {
            self.apply_error = Some(ApplyError::Stale);
            self
        }

        /// Makes every `apply` return an unexpected failure.
        fn apply_failed(mut self, failure: HostFailure) -> Self {
            self.apply_error = Some(ApplyError::Failed(failure));
            self
        }

        /// Reports a byte count other than the length of `after`.
        fn reporting(mut self, bytes: usize) -> Self {
            self.permit_bytes = bytes;
            self
        }

        /// Number of `open` calls recorded so far.
        fn open_count(&self) -> usize {
            lock(&self.recorded).opens.len()
        }

        /// Number of `apply` calls recorded so far.
        fn apply_count(&self) -> usize {
            lock(&self.recorded).applies.len()
        }

        /// The `(before, after)` pair of the last `apply` call, if any.
        fn last_applied(&self) -> Option<(String, String)> {
            lock(&self.recorded)
                .applies
                .last()
                .map(|(_, before, after)| (before.clone(), after.clone()))
        }
    }

    #[async_trait]
    impl EditSource for RecordingHost {
        async fn open(&self, path: &str) -> Result<EditFile, OpenError> {
            lock(&self.recorded).opens.push(path.to_string());
            match &self.open_error {
                Some(error) => Err(error.clone()),
                None => Ok(EditFile {
                    bytes: self.bytes.clone(),
                }),
            }
        }
    }

    #[async_trait]
    impl EditSink for RecordingHost {
        async fn apply(
            &self,
            path: &str,
            before: &str,
            after: &str,
        ) -> Result<EditPermit, ApplyError> {
            lock(&self.recorded).applies.push((
                path.to_string(),
                before.to_string(),
                after.to_string(),
            ));
            match &self.apply_error {
                Some(error) => Err(error.clone()),
                None => Ok(EditPermit::new(self.permit_bytes)),
            }
        }
    }

    /// Locks `mutex`, recovering the value if a previous holder panicked.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn edit_tool_with(
        host: RecordingHost,
        config: EditConfig,
    ) -> EditTool<RecordingHost, RecordingHost> {
        EditTool::new(config, host.clone(), host)
    }

    fn edit_tool(text: &str) -> EditTool<RecordingHost, RecordingHost> {
        edit_tool_with(RecordingHost::new(text), EditConfig::new())
    }

    /// A plain edit request against `path`.
    fn request(path: &str, old_text: &str, new_text: &str) -> Value {
        json!({"path": path, "old_text": old_text, "new_text": new_text})
    }

    fn failed(outcome: EditOutcome) -> (&'static str, String) {
        match outcome {
            EditOutcome::Failed { code, text } => (code, text),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn edited(outcome: EditOutcome) -> (usize, MatchTier, usize, String) {
        match outcome {
            EditOutcome::Edited {
                matches,
                tier,
                bytes,
                text,
            } => (matches, tier, bytes, text),
            other => panic!("expected a success, got {other:?}"),
        }
    }

    /// A host with one line `alpha` and one line `beta`.
    fn two_line_host() -> RecordingHost {
        RecordingHost::new("alpha\nbeta\n")
    }

    #[test]
    fn input_failures_do_not_call_either_hook() {
        let tool = edit_tool("alpha\n");
        let long_path = "a".repeat(4097);
        let cases = [
            json!({}),
            json!({"path": "a.txt", "old_text": "a"}),
            json!({"path": "a.txt", "new_text": "b"}),
            json!({"old_text": "a", "new_text": "b"}),
            json!({"path": 1, "old_text": "a", "new_text": "b"}),
            json!({"path": "a.txt", "old_text": 1, "new_text": "b"}),
            json!({"path": "a.txt", "old_text": "a", "new_text": 1}),
            json!({"path": "a.txt", "old_text": "a", "new_text": "b", "replace_all": null}),
            json!({"path": "a.txt", "old_text": "a", "new_text": "b", "replace_all": "yes"}),
            json!({"path": "a.txt", "old_text": "a", "new_text": "b", "occurrence": 2}),
            json!({"path": " ", "old_text": "a", "new_text": "b"}),
            json!({"path": " a.txt", "old_text": "a", "new_text": "b"}),
            json!({"path": "a.txt ", "old_text": "a", "new_text": "b"}),
            json!({"path": "a\u{0}b", "old_text": "a", "new_text": "b"}),
            json!({"path": long_path, "old_text": "a", "new_text": "b"}),
        ];
        for input in cases {
            let error = run(tool.execute(&input)).expect_err("input should be rejected");
            assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        }
        assert_eq!(tool.source.open_count(), 0);
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn an_empty_old_text_is_rejected_before_the_hooks() {
        let tool = edit_tool("alpha\n");
        let (code, text) = failed(run(tool.execute(&request("a.txt", "", "x"))).expect("outcome"));

        assert_eq!(code, EMPTY_OLD);
        assert_eq!(
            text,
            "AGT_EDIT_EMPTY_OLD: old_text is empty; send the text to replace, or use the write \
             tool for a new file"
        );
        assert_eq!(tool.source.open_count(), 0);
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_nul_byte_in_either_text_field_is_binary_before_the_hooks() {
        for (old_text, new_text) in [("al\0pha", "x"), ("alpha", "be\0ta")] {
            let tool = edit_tool("alpha\n");
            let (code, text) =
                failed(run(tool.execute(&request("a.txt", old_text, new_text))).expect("outcome"));

            assert_eq!(code, BINARY);
            assert_eq!(
                text,
                "AGT_EDIT_BINARY: text contains a NUL byte; this tool edits text only"
            );
            assert_eq!(tool.source.open_count(), 0);
            assert_eq!(tool.sink.apply_count(), 0);
        }
    }

    #[test]
    fn a_nul_byte_is_accepted_when_the_host_allows_it() {
        let host = RecordingHost::new("al\0pha\n");
        let tool = edit_tool_with(
            host,
            EditConfig::new()
                .with_allow_nul(true)
                .with_match_mode(MatchMode::Exact),
        );
        let (matches, _, _, _) =
            edited(run(tool.execute(&request("a.txt", "al\0pha", "beta"))).expect("outcome"));

        assert_eq!(matches, 1);
        assert_eq!(tool.sink.apply_count(), 1);
        assert_eq!(
            tool.sink.last_applied().map(|(_, after)| after).as_deref(),
            Some("beta\n")
        );
    }

    #[test]
    fn identical_text_fields_are_no_change_and_replace_all_does_not_matter() {
        for replace_all in [false, true] {
            let tool = edit_tool("alpha\nalpha\n");
            let input = json!({
                "path": "a.txt",
                "old_text": "alpha",
                "new_text": "alpha",
                "replace_all": replace_all
            });
            let (code, text) = failed(run(tool.execute(&input)).expect("outcome"));

            assert_eq!(code, NO_CHANGE);
            assert_eq!(
                text,
                "AGT_EDIT_NO_CHANGE: old_text and new_text are identical; the file was not changed"
            );
            assert_eq!(tool.source.open_count(), 0);
            assert_eq!(tool.sink.apply_count(), 0);
        }
    }

    #[test]
    fn every_open_refusal_maps_to_its_code_and_never_applies() {
        let cases = [
            (
                EditRefusal::NotFound {
                    message: "no such file".to_string(),
                },
                NOT_FOUND,
                "AGT_EDIT_NOT_FOUND: no such file; use the write tool to create a file",
            ),
            (
                EditRefusal::NotFound {
                    message: "  ".to_string(),
                },
                NOT_FOUND,
                "AGT_EDIT_NOT_FOUND: path not found; use the write tool to create a file",
            ),
            (
                EditRefusal::IsDirectory,
                IS_DIRECTORY,
                "AGT_EDIT_IS_DIRECTORY: path is a directory",
            ),
            (
                EditRefusal::Forbidden,
                FORBIDDEN,
                "AGT_EDIT_FORBIDDEN: permission denied",
            ),
            (
                EditRefusal::OutsideWorkspace,
                OUTSIDE_WORKSPACE,
                "AGT_EDIT_OUTSIDE_WORKSPACE: path resolves outside the workspace",
            ),
            (
                EditRefusal::Rejected {
                    message: "read the file first".to_string(),
                },
                REJECTED,
                "AGT_EDIT_REJECTED: read the file first",
            ),
            (
                EditRefusal::Rejected {
                    message: String::new(),
                },
                REJECTED,
                "AGT_EDIT_REJECTED: edit rejected",
            ),
        ];

        for (refusal, expected_code, expected_text) in cases {
            let host = two_line_host().refusing_open(refusal);
            let tool = edit_tool_with(host, EditConfig::new());
            let output = run(tool.call(&CallContext::new(), request("a.txt", "alpha", "gamma")))
                .expect("call should not return a contract error");

            assert!(output.is_error());
            assert_eq!(output.content(), expected_text);
            assert_eq!(output.content().split(':').next(), Some(expected_code));
            assert_eq!(tool.source.open_count(), 1);
            assert_eq!(tool.sink.apply_count(), 0);
        }
    }

    #[test]
    fn an_open_failure_is_a_source_failure_without_the_partial_write_note() {
        let host = two_line_host().open_failed(HostFailure::new("disk on fire"));
        let tool = edit_tool_with(host, EditConfig::new());
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "alpha", "gamma"))).expect("outcome"));

        assert_eq!(code, SOURCE_FAILED);
        assert_eq!(code, "AGT_EDIT_SOURCE_FAILED");
        assert_eq!(
            text,
            "AGT_EDIT_SOURCE_FAILED: disk on fire; the file was not changed"
        );
        assert!(!text.contains("partially written"), "text: {text}");
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_file_over_the_budget_is_too_large_and_never_applies() {
        let host = RecordingHost::new("123456789");
        let config = EditConfig::new()
            .with_max_file_bytes(8)
            .expect("8 is a valid budget");
        let tool = edit_tool_with(host, config);
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "123", "x"))).expect("outcome"));

        assert_eq!(code, TOO_LARGE);
        assert!(text.contains("file"), "message: {text}");
        assert!(text.contains('8'), "message: {text}");
        assert!(text.contains('9'), "message: {text}");
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn invalid_utf8_is_not_text_and_never_applies() {
        let tool = edit_tool_with(
            RecordingHost::with_bytes(b"ok\xFFok".to_vec()),
            EditConfig::new(),
        );
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "ok", "x"))).expect("outcome"));

        assert_eq!(code, NOT_TEXT);
        assert_eq!(text, "AGT_EDIT_NOT_TEXT: file is not valid UTF-8 text");
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_nul_byte_in_the_file_is_binary_and_never_applies() {
        let tool = edit_tool("a\0b\n");
        let (code, text) = failed(run(tool.execute(&request("a.txt", "a", "x"))).expect("outcome"));

        assert_eq!(code, BINARY);
        assert_eq!(
            text,
            "AGT_EDIT_BINARY: text contains a NUL byte; this tool edits text only"
        );
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_missing_old_text_is_no_match_and_never_applies() {
        let tool = edit_tool("alpha\nbeta\n");
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "gamma", "delta"))).expect("outcome"));

        assert_eq!(code, NO_MATCH);
        assert_eq!(
            text,
            "AGT_EDIT_NO_MATCH: old_text was not found; read the file and copy the text exactly"
        );
        assert_eq!(tool.source.open_count(), 1);
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn two_occurrences_are_not_unique_with_their_line_numbers() {
        let tool = edit_tool("alpha\nbeta\nalpha\n");
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "alpha", "gamma"))).expect("outcome"));

        assert_eq!(code, NOT_UNIQUE);
        assert_eq!(
            text,
            "AGT_EDIT_NOT_UNIQUE: old_text occurs 2 times at lines: 1, 3. Please provide more \
             context in old_text to uniquely identify the location."
        );
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_capped_count_reads_as_ten_or_more() {
        let file = "alpha\n".repeat(12);
        let tool = edit_tool(&file);
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "alpha", "gamma"))).expect("outcome"));

        assert_eq!(code, NOT_UNIQUE);
        assert!(text.contains("10 or more"), "text: {text}");
        assert!(text.contains("lines: 1, 2"), "text: {text}");
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_replace_all_over_the_cap_is_too_many_and_never_applies() {
        let file = "x\n".repeat(5);
        let host = RecordingHost::new(&file);
        let config = EditConfig::new()
            .with_max_occurrences(4)
            .expect("4 is a valid cap");
        let tool = edit_tool_with(host, config);
        let input = json!({
            "path": "a.txt",
            "old_text": "x",
            "new_text": "y",
            "replace_all": true
        });

        let (code, text) = failed(run(tool.execute(&input)).expect("outcome"));
        assert_eq!(code, TOO_MANY);
        assert_eq!(
            text,
            "AGT_EDIT_TOO_MANY: old_text occurs more than 4 times; send a more specific old_text \
             or edit in smaller batches"
        );
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_replace_all_at_the_cap_still_applies() {
        let file = "x\nx\nx\nx\n";
        let host = RecordingHost::new(file);
        let config = EditConfig::new()
            .with_max_occurrences(4)
            .expect("4 is a valid cap");
        let tool = edit_tool_with(host, config);
        let input = json!({
            "path": "a.txt",
            "old_text": "x",
            "new_text": "y",
            "replace_all": true
        });

        let (matches, tier, _, text) = edited(run(tool.execute(&input)).expect("outcome"));
        assert_eq!(matches, 4);
        assert_eq!(tier, MatchTier::Exact);
        assert_eq!(text, "Edited a.txt: replaced 4 occurrence(s), 8 bytes.");
        assert_eq!(
            tool.sink.last_applied().map(|(_, after)| after).as_deref(),
            Some("y\ny\ny\ny\n")
        );
    }

    #[test]
    fn apply_stale_is_reported_with_its_own_code() {
        let tool = edit_tool_with(two_line_host().stale(), EditConfig::new());
        let (code, text) =
            failed(run(tool.execute(&request("a.txt", "alpha", "gamma"))).expect("outcome"));

        assert_eq!(code, STALE);
        assert_eq!(
            text,
            "AGT_EDIT_STALE: file changed on disk after it was read; read it again"
        );
        assert_eq!(tool.sink.apply_count(), 1);
    }

    #[test]
    fn every_apply_refusal_maps_to_its_code() {
        let cases = [
            (
                EditRefusal::NotFound {
                    message: "gone".to_string(),
                },
                NOT_FOUND,
                "AGT_EDIT_NOT_FOUND: gone; the file disappeared after it was read",
            ),
            (
                EditRefusal::IsDirectory,
                IS_DIRECTORY,
                "AGT_EDIT_IS_DIRECTORY: path is a directory",
            ),
            (
                EditRefusal::Forbidden,
                FORBIDDEN,
                "AGT_EDIT_FORBIDDEN: permission denied",
            ),
            (
                EditRefusal::OutsideWorkspace,
                OUTSIDE_WORKSPACE,
                "AGT_EDIT_OUTSIDE_WORKSPACE: path resolves outside the workspace",
            ),
            (
                EditRefusal::Rejected {
                    message: "approval denied".to_string(),
                },
                REJECTED,
                "AGT_EDIT_REJECTED: approval denied",
            ),
        ];

        for (refusal, expected_code, expected_text) in cases {
            let tool = edit_tool_with(two_line_host().refusing_apply(refusal), EditConfig::new());
            let output = run(tool.call(&CallContext::new(), request("a.txt", "alpha", "gamma")))
                .expect("call should not return a contract error");

            assert!(output.is_error());
            assert_eq!(output.content(), expected_text);
            assert_eq!(output.content().split(':').next(), Some(expected_code));
            assert_eq!(tool.source.open_count(), 1);
            assert_eq!(tool.sink.apply_count(), 1);
        }
    }

    #[test]
    fn an_apply_failure_is_a_sink_failure_with_the_partial_write_note() {
        let host = two_line_host().apply_failed(HostFailure::new("disk full"));
        let tool = edit_tool_with(host, EditConfig::new());
        let output = run(tool.call(&CallContext::new(), request("a.txt", "alpha", "gamma")))
            .expect("call should not return a contract error");

        assert!(output.is_error());
        assert_eq!(
            output.content(),
            "AGT_EDIT_SINK_FAILED: disk full The file may have been partially written."
        );
        assert!(output.content().contains("partially written"));
        assert_eq!(tool.sink.apply_count(), 1);
    }

    #[test]
    fn an_edited_file_over_the_budget_is_too_large_and_never_applies() {
        let host = RecordingHost::new("alpha\n");
        // The file is 6 bytes, so it passes the read check; the fan-out makes
        // the result larger than the 6-byte budget.
        let config = EditConfig::new()
            .with_max_file_bytes(6)
            .expect("6 is a valid budget");
        let tool = edit_tool_with(host, config);
        let (code, text) = failed(
            run(tool.execute(&request("a.txt", "alpha", "a much longer line"))).expect("outcome"),
        );

        assert_eq!(code, TOO_LARGE);
        assert!(text.contains("the edited file"), "message: {text}");
        assert!(text.contains('6'), "message: {text}");
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_lenient_match_whose_replacement_equals_the_original_is_no_change() {
        // The file line has trailing spaces the model's copy drops, so only the
        // lenient tier finds it, and the replacement reproduces the file bytes.
        let host = RecordingHost::new("let value = 1;   \n");
        let config = EditConfig::new().with_match_mode(MatchMode::Lenient);
        let tool = edit_tool_with(host, config);
        let (code, text) = failed(
            run(tool.execute(&request("a.txt", "let value = 1;\n", "let value = 1;   \n")))
                .expect("outcome"),
        );

        assert_eq!(code, NO_CHANGE);
        assert_eq!(
            text,
            "AGT_EDIT_NO_CHANGE: old_text and new_text are identical; the file was not changed"
        );
        assert_eq!(tool.source.open_count(), 1);
        assert_eq!(tool.sink.apply_count(), 0);
    }

    #[test]
    fn a_lenient_match_reports_its_tier_and_keeps_the_original_indentation() {
        let host = RecordingHost::new("  let value = 1;   \n  keep();\n");
        let config = EditConfig::new().with_match_mode(MatchMode::Lenient);
        let tool = edit_tool_with(host, config);
        let (matches, tier, _, text) = edited(
            run(tool.execute(&request("a.txt", "let value = 1;\n", "let value = 2;\n")))
                .expect("outcome"),
        );

        assert_eq!(matches, 1);
        assert_eq!(tier, MatchTier::Lenient);
        assert_eq!(
            text,
            "Edited a.txt: replaced 1 occurrence(s), 30 bytes. Matched with the lenient tier."
        );
        assert_eq!(
            tool.sink.last_applied().map(|(_, after)| after).as_deref(),
            Some("  let value = 2;\n  keep();\n")
        );
    }

    #[test]
    fn a_fuzzy_match_reports_its_tier() {
        let host = RecordingHost::new("abcdefghij0123456788\n");
        let config = EditConfig::new().with_match_mode(MatchMode::Fuzzy);
        let tool = edit_tool_with(host, config);
        let (matches, tier, _, text) = edited(
            run(tool.execute(&request("a.txt", "abcdefghij0123456789", "replaced")))
                .expect("outcome"),
        );

        assert_eq!(matches, 1);
        assert_eq!(tier, MatchTier::Fuzzy);
        assert!(
            text.ends_with(" Matched with the fuzzy tier."),
            "text: {text}"
        );
    }

    #[test]
    fn an_exact_edit_reports_the_permit_byte_count_and_both_strings() {
        let host = RecordingHost::new("alpha\nbeta\n").reporting(11);
        let tool = edit_tool_with(host, EditConfig::new());
        let (matches, tier, bytes, text) =
            edited(run(tool.execute(&request("notes.txt", "alpha", "gamma"))).expect("outcome"));

        assert_eq!(matches, 1);
        assert_eq!(tier, MatchTier::Exact);
        assert_eq!(bytes, 11);
        assert_eq!(
            text,
            "Edited notes.txt: replaced 1 occurrence(s), 11 bytes."
        );
        assert_eq!(
            tool.sink.last_applied(),
            Some(("alpha\nbeta\n".to_string(), "gamma\nbeta\n".to_string()))
        );
    }

    #[test]
    fn a_second_execute_calls_both_hooks_again() {
        let tool = edit_tool("alpha\nbeta\n");
        let input = request("a.txt", "alpha", "gamma");

        edited(run(tool.execute(&input)).expect("outcome"));
        edited(run(tool.execute(&input)).expect("outcome"));

        assert_eq!(tool.source.open_count(), 2);
        assert_eq!(tool.sink.apply_count(), 2);
    }

    #[test]
    fn a_crlf_needle_matches_a_crlf_file_and_untouched_endings_stay() {
        let host = RecordingHost::new("one\r\ntwo\r\nthree\r\n");
        let tool = edit_tool_with(host, EditConfig::new());
        let (matches, tier, _, _) = edited(
            run(tool.execute(&request("a.txt", "two\r\nthree", "changed"))).expect("outcome"),
        );

        assert_eq!(matches, 1);
        assert_eq!(tier, MatchTier::Exact);
        let after = tool.sink.last_applied().expect("apply ran").1;
        assert_eq!(after, "one\r\nchanged\r\n");
        assert_eq!(after.matches("\r\n").count(), 2);
        assert!(!after.contains("\r\r\n"));
    }

    #[test]
    fn the_spec_matches_the_parser_for_the_published_shape() {
        let tool = edit_tool("alpha\n");
        let spec = tool.spec();
        let schema = spec.input_schema();

        assert_eq!(spec.name(), "edit");
        assert!(!spec.description().is_empty());
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["path", "old_text", "new_text"]));
        assert_eq!(schema["properties"]["path"]["type"], json!("string"));
        assert_eq!(schema["properties"]["old_text"]["type"], json!("string"));
        assert_eq!(schema["properties"]["new_text"]["type"], json!("string"));
        assert_eq!(
            schema["properties"]["replace_all"]["type"],
            json!("boolean")
        );
        assert_eq!(schema["properties"]["replace_all"]["default"], json!(false));

        // The parser accepts the schema shape and rejects an unknown field.
        assert!(run(tool.execute(&request("a.txt", "alpha", "gamma"))).is_ok());
        let error = run(tool.execute(&json!({
            "path": "a.txt",
            "old_text": "alpha",
            "new_text": "gamma",
            "occurrence": 2
        })))
        .expect_err("unknown field");
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
    }

    #[test]
    fn custom_parameter_descriptions_reach_the_spec() {
        let config = EditConfig::new()
            .with_description("Replace one region.")
            .expect("valid description")
            .with_parameter_description("old_text", "The text to find.")
            .expect("old_text is known")
            .with_parameter_description("replace_all", "Every match.")
            .expect("replace_all is known");
        let tool = edit_tool_with(two_line_host(), config);

        assert_eq!(tool.spec().description(), "Replace one region.");
        let schema = tool.spec().input_schema();
        assert_eq!(
            schema["properties"]["old_text"]["description"],
            json!("The text to find.")
        );
        assert_eq!(
            schema["properties"]["replace_all"]["description"],
            json!("Every match.")
        );
    }
}
