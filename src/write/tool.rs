//! The write tool implementation.
//!
//! [`WriteTool::execute`] is the real entry point. It parses the model input,
//! runs the content policy on the original string, asks the host to approve the
//! write, normalizes line endings when the permit asks for them, and hands the
//! final string to [`WriteSink::write`]. [`WriteTool::call`] is a thin adapter
//! that renders a [`WriteOutcome`] for the model.
//!
//! The tool holds the config and the sink by value. It keeps no memory of an
//! earlier call: a second call on the same path asks the host again.

use serde_json::Value;

use crate::text::normalize_newlines;
use crate::{CallContext, Tool, ToolError, ToolOutput, ToolSpec, async_trait};

use super::check::{TOO_LARGE, check_content};
use super::config::WriteConfig;
use super::input::{input_schema, parse_input};
use super::sink::{PrepareError, SINK_FAILED, WritePermit, WriteSink};

/// Sentence appended only when [`WriteSink::write`] fails.
///
/// The SDK cannot tell whether any byte reached the file, so the model is told
/// to re-read before trusting it.
const PARTIAL_WRITE_NOTE: &str = "The file may have been partially written.";

/// The result of executing one write request.
///
/// [`WriteTool::call`] renders each variant for the model. An operational
/// failure is [`WriteOutcome::Failed`], not a [`ToolError`], so the model can
/// see the code and recover.
///
/// [`WriteSink::write`]: crate::WriteSink::write
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The host wrote the content.
    Wrote {
        /// Whether an existing file was replaced.
        ///
        /// The SDK copies this from the permit. It does not stat the path.
        existed: bool,
        /// UTF-8 bytes written, which is the length of the normalized content.
        bytes: usize,
        /// Success sentence for the model, including the path it sent.
        text: String,
    },
    /// The write failed for an operational reason.
    Failed {
        /// Stable error code, such as [`TOO_LARGE`] or
        /// [`crate::SINK_FAILED`].
        code: &'static str,
        /// Rendered text in the form `AGT_CODE: message`.
        text: String,
    },
}

/// A tool that creates or fully overwrites a file through a [`WriteSink`].
///
/// The tool never opens a file itself. The host injects a sink when the tool is
/// constructed, and the sink answers each approval request and performs each
/// mutation.
///
/// The tool is `Sync` because [`Tool::call`] takes `&self`. That does not
/// serialize two calls: the host serializes writes to one path when it needs
/// to.
#[derive(Debug)]
pub struct WriteTool<S: WriteSink> {
    sink: S,
    config: WriteConfig,
    spec: ToolSpec,
}

impl<S: WriteSink> WriteTool<S> {
    /// Builds a write tool from `config` and a host `sink`.
    ///
    /// The name, description, and parameter descriptions in the registered
    /// specification come from `config`.
    #[must_use]
    pub fn new(config: WriteConfig, sink: S) -> Self {
        let spec = spec_from_config(&config);
        Self { sink, config, spec }
    }

    /// The tool specification registered with the agent.
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Runs one write request.
    ///
    /// The checks run in this order, and the first match wins:
    ///
    /// 1. [`parse_input`] rejects unusable model JSON with
    ///    [`crate::ErrorCode::InputInvalid`]. Neither hook runs.
    /// 2. [`check_content`] runs on the original model string. A rejection is
    ///    [`WriteOutcome::Failed`] and neither hook runs.
    /// 3. [`WriteSink::prepare`] receives the path and the original content. A
    ///    refusal or an unexpected failure is [`WriteOutcome::Failed`] and
    ///    [`WriteSink::write`] does not run.
    /// 4. The content is normalized to the line ending on the permit. The
    ///    original string is borrowed when no conversion is needed.
    /// 5. A normalized length above the configured budget is
    ///    [`WriteOutcome::Failed`] with [`TOO_LARGE`] and
    ///    [`WriteSink::write`] does not run, even though
    ///    [`WriteSink::prepare`] already saw the original string.
    /// 6. [`WriteSink::write`] receives the normalized string. A failure is
    ///    [`WriteOutcome::Failed`] with a note that the file may have been
    ///    partially written.
    ///
    /// The tool keeps no memory of an earlier call. A second call on the same
    /// path asks the host again.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InputInvalid`] when the model input is not
    /// usable. Every operational failure is a [`WriteOutcome::Failed`] instead.
    pub async fn execute(&self, input: &Value) -> Result<WriteOutcome, ToolError> {
        let input = parse_input(input)?;
        let path = input.path.as_str();
        let original = input.content.as_str();

        if let Err(rejection) = check_content(original, &self.config) {
            return Ok(failed(rejection.code, &rejection.message));
        }

        let permit = match self.sink.prepare(path, original).await {
            Ok(permit) => permit,
            Err(PrepareError::Refusal(refusal)) => {
                return Ok(failed(refusal.code(), refusal.message()));
            }
            Err(PrepareError::Failed(failure)) => {
                return Ok(failed(SINK_FAILED, failure.message()));
            }
        };

        let normalized = normalize_newlines(original, permit.newline);
        let content = normalized.as_ref();
        let limit = self.config.max_content_bytes();
        if content.len() > limit {
            let bytes = content.len();
            return Ok(failed(
                TOO_LARGE,
                &format!(
                    "content is {bytes} UTF-8 bytes after newline normalization, \
                     above the configured limit of {limit} bytes"
                ),
            ));
        }

        match self.sink.write(path, content).await {
            Ok(()) => Ok(wrote(path, permit, content.len())),
            Err(failure) => Ok(failed(
                SINK_FAILED,
                &format!("{} {PARTIAL_WRITE_NOTE}", failure.message()),
            )),
        }
    }
}

/// Builds the success sentence and the outcome the host can match.
fn wrote(path: &str, permit: WritePermit, bytes: usize) -> WriteOutcome {
    let verb = if permit.existed {
        "Overwrote"
    } else {
        "Created"
    };
    WriteOutcome::Wrote {
        existed: permit.existed,
        bytes,
        text: format!("{verb} {path} ({bytes} bytes)."),
    }
}

/// Builds a failure outcome from a stable code and a message.
fn failed(code: &'static str, message: &str) -> WriteOutcome {
    WriteOutcome::Failed {
        code,
        text: format!("{code}: {message}"),
    }
}

/// Builds the registered spec from an already validated config.
///
/// [`WriteConfig`] rejects an unusable name or description in its setters, and
/// the input schema is always a JSON object, so the spec constructor cannot
/// fail for a value built through those setters.
fn spec_from_config(config: &WriteConfig) -> ToolSpec {
    let schema = input_schema(config.path_description(), config.content_description());
    ToolSpec::new(config.name(), config.description(), schema)
        .expect("write config name and description are validated by their setters")
}

#[async_trait]
impl<S: WriteSink + 'static> Tool for WriteTool<S> {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &CallContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.execute(&input).await? {
            WriteOutcome::Wrote { text, .. } => Ok(ToolOutput::text(text)),
            WriteOutcome::Failed { text, .. } => Ok(ToolOutput::error_text(text)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::task::{Context, Poll, Waker};

    use serde_json::json;

    use super::*;
    use crate::CallContext;
    use crate::write::config::WriteConfig;
    use crate::write::sink::{
        FORBIDDEN, IS_DIRECTORY, NOT_FOUND, OUTSIDE_WORKSPACE, REJECTED, SinkFailure, WriteRefusal,
    };
    use crate::write::{BINARY, EMPTY, INCOMPLETE};

    /// Runs a future that the in-memory sink never parks.
    fn run<F: std::future::Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("write future should not wait"),
        }
    }

    /// Calls recorded by [`RecordingSink`], in call order.
    #[derive(Debug, Default)]
    struct Recorded {
        prepares: Vec<(String, String)>,
        writes: Vec<(String, String)>,
    }

    /// In-memory [`WriteSink`] that records every call and scripts its answer.
    #[derive(Debug)]
    struct RecordingSink {
        permit: WritePermit,
        prepare_error: Option<PrepareError>,
        write_error: Option<SinkFailure>,
        recorded: Mutex<Recorded>,
    }

    impl RecordingSink {
        /// A sink that permits a create with [`Newline::AsIs`].
        fn new() -> Self {
            Self {
                permit: WritePermit::new(false, crate::write::Newline::AsIs),
                prepare_error: None,
                write_error: None,
                recorded: Mutex::new(Recorded::default()),
            }
        }

        /// Changes the permit the sink returns.
        fn permitting(mut self, existed: bool, newline: crate::write::Newline) -> Self {
            self.permit = WritePermit::new(existed, newline);
            self
        }

        /// Makes every `prepare` return a refusal.
        fn refusing(mut self, refusal: WriteRefusal) -> Self {
            self.prepare_error = Some(PrepareError::Refusal(refusal));
            self
        }

        /// Makes every `prepare` return an unexpected failure.
        fn prepare_failed(mut self, failure: SinkFailure) -> Self {
            self.prepare_error = Some(PrepareError::Failed(failure));
            self
        }

        /// Makes every `write` return an unexpected failure.
        fn write_failed(mut self, failure: SinkFailure) -> Self {
            self.write_error = Some(failure);
            self
        }

        /// Number of `prepare` calls recorded so far.
        fn prepare_count(&self) -> usize {
            lock(&self.recorded).prepares.len()
        }

        /// Number of `write` calls recorded so far.
        fn write_count(&self) -> usize {
            lock(&self.recorded).writes.len()
        }

        /// The content of the last `write` call, if any.
        fn last_written(&self) -> Option<String> {
            lock(&self.recorded)
                .writes
                .last()
                .map(|(_, content)| content.clone())
        }

        /// The content of the last `prepare` call, if any.
        fn last_prepared(&self) -> Option<String> {
            lock(&self.recorded)
                .prepares
                .last()
                .map(|(_, content)| content.clone())
        }
    }

    #[async_trait]
    impl WriteSink for RecordingSink {
        async fn prepare(&self, path: &str, content: &str) -> Result<WritePermit, PrepareError> {
            lock(&self.recorded)
                .prepares
                .push((path.to_string(), content.to_string()));
            match &self.prepare_error {
                Some(error) => Err(error.clone()),
                None => Ok(self.permit),
            }
        }

        async fn write(&self, path: &str, content: &str) -> Result<(), SinkFailure> {
            lock(&self.recorded)
                .writes
                .push((path.to_string(), content.to_string()));
            match &self.write_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    /// Locks `mutex`, recovering the value if a previous holder panicked.
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_tool_with(sink: RecordingSink, config: WriteConfig) -> WriteTool<RecordingSink> {
        WriteTool::new(config, sink)
    }

    fn write_tool(sink: RecordingSink) -> WriteTool<RecordingSink> {
        write_tool_with(sink, WriteConfig::new())
    }

    fn failed(outcome: WriteOutcome) -> (&'static str, String) {
        match outcome {
            WriteOutcome::Failed { code, text } => (code, text),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn wrote(outcome: WriteOutcome) -> (bool, usize, String) {
        match outcome {
            WriteOutcome::Wrote {
                existed,
                bytes,
                text,
            } => (existed, bytes, text),
            other => panic!("expected a success, got {other:?}"),
        }
    }

    #[test]
    fn input_failures_do_not_call_either_hook() {
        let sink = RecordingSink::new();
        let tool = write_tool(sink);

        let cases = [
            json!({"path": "file.txt"}),
            json!({"content": "text"}),
            json!({"path": " foo ", "content": "text"}),
            json!({"path": "file.txt", "content": "text", "append": true}),
            json!({"path": "a\u{0}b", "content": "text"}),
            json!({"path": "a".repeat(4097), "content": "text"}),
            json!({"path": 1, "content": "text"}),
            json!({"path": "file.txt", "content": true}),
        ];
        for input in cases {
            let error = run(tool.execute(&input)).expect_err("input should be rejected");
            assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        }
        assert_eq!(tool.sink.prepare_count(), 0);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn empty_content_is_written_by_default() {
        let tool = write_tool(RecordingSink::new());
        let (existed, bytes, text) = wrote(
            run(tool.execute(&json!({"path": "empty.txt", "content": ""}))).expect("outcome"),
        );

        assert!(!existed);
        assert_eq!(bytes, 0);
        assert_eq!(text, "Created empty.txt (0 bytes).");
        assert_eq!(tool.sink.prepare_count(), 1);
        assert_eq!(tool.sink.write_count(), 1);
        assert_eq!(tool.sink.last_written().as_deref(), Some(""));
    }

    #[test]
    fn empty_content_is_rejected_before_the_hook_when_disallowed() {
        let sink = RecordingSink::new();
        let tool = write_tool_with(sink, WriteConfig::new().with_allow_empty(false));
        let (code, text) = failed(
            run(tool.execute(&json!({"path": "empty.txt", "content": ""}))).expect("outcome"),
        );

        assert_eq!(code, EMPTY);
        assert_eq!(code, "AGT_WRITE_EMPTY");
        assert!(text.starts_with("AGT_WRITE_EMPTY: "));
        assert_eq!(tool.sink.prepare_count(), 0);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn whitespace_only_content_is_not_empty() {
        let sink = RecordingSink::new();
        let tool = write_tool_with(sink, WriteConfig::new().with_allow_empty(false));
        let (_, bytes, text) = wrote(
            run(tool.execute(&json!({"path": "spaces.txt", "content": "   "}))).expect("outcome"),
        );

        assert_eq!(bytes, 3);
        assert_eq!(text, "Created spaces.txt (3 bytes).");
        assert_eq!(tool.sink.prepare_count(), 1);
        assert_eq!(tool.sink.write_count(), 1);
    }

    #[test]
    fn content_over_the_budget_skips_the_hook() {
        let sink = RecordingSink::new();
        let config = WriteConfig::new()
            .with_max_content_bytes(8)
            .expect("8 is a valid budget");
        let tool = write_tool_with(sink, config);
        let (code, text) = failed(
            run(tool.execute(&json!({"path": "big.txt", "content": "123456789"})))
                .expect("outcome"),
        );

        assert_eq!(code, TOO_LARGE);
        assert!(text.contains('8'), "message must include the limit: {text}");
        assert!(text.contains('9'), "message must include the size: {text}");
        assert_eq!(tool.sink.prepare_count(), 0);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn a_crlf_permit_that_grows_past_the_budget_skips_only_write() {
        let sink = RecordingSink::new().permitting(false, crate::write::Newline::CrLf);
        // Four bytes in, six bytes out once each `\n` becomes `\r\n`.
        let config = WriteConfig::new()
            .with_max_content_bytes(4)
            .expect("4 is a valid budget");
        let tool = write_tool_with(sink, config);
        let (code, text) = failed(
            run(tool.execute(&json!({"path": "grow.txt", "content": "a\nb\n"}))).expect("outcome"),
        );

        assert_eq!(code, TOO_LARGE);
        assert!(text.contains('4'), "message must include the limit: {text}");
        assert_eq!(tool.sink.prepare_count(), 1);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn a_nul_byte_is_rejected_before_the_hook() {
        let sink = RecordingSink::new();
        let tool = write_tool(sink);
        let (code, _) = failed(
            run(tool.execute(&json!({"path": "bin.dat", "content": "a\u{0}b"}))).expect("outcome"),
        );

        assert_eq!(code, BINARY);
        assert_eq!(tool.sink.prepare_count(), 0);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn nul_content_reaches_write_when_the_host_allows_it() {
        let sink = RecordingSink::new();
        let tool = write_tool_with(sink, WriteConfig::new().with_allow_nul(true));
        let (_, bytes, _) = wrote(
            run(tool.execute(&json!({"path": "bin.dat", "content": "a\u{0}b"}))).expect("outcome"),
        );

        assert_eq!(bytes, 3);
        assert_eq!(tool.sink.prepare_count(), 1);
        assert_eq!(tool.sink.write_count(), 1);
        assert_eq!(tool.sink.last_written().as_deref(), Some("a\u{0}b"));
    }

    #[test]
    fn the_default_marker_list_accepts_placeholder_text() {
        let tool = write_tool(RecordingSink::new());
        let (_, _, text) = wrote(
            run(tool.execute(&json!({
                "path": "notes.txt",
                "content": "rest of the file"
            })))
            .expect("outcome"),
        );

        assert_eq!(text, "Created notes.txt (16 bytes).");
        assert_eq!(tool.sink.prepare_count(), 1);
    }

    #[test]
    fn a_configured_marker_skips_the_hook() {
        let sink = RecordingSink::new();
        let config = WriteConfig::new()
            .with_omission_markers(["rest of the file"])
            .expect("valid marker");
        let tool = write_tool_with(sink, config);
        let (code, text) = failed(
            run(tool.execute(&json!({
                "path": "notes.txt",
                "content": "fn main() {}\n// rest of the file\n"
            })))
            .expect("outcome"),
        );

        assert_eq!(code, INCOMPLETE);
        assert!(text.contains("rest of the file"), "message: {text}");
        assert_eq!(tool.sink.prepare_count(), 0);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn every_refusal_maps_to_its_code_and_never_writes() {
        let cases = [
            (
                WriteRefusal::NotFound {
                    message: "missing parent".to_string(),
                },
                NOT_FOUND,
                "AGT_WRITE_NOT_FOUND: missing parent",
            ),
            (
                WriteRefusal::NotFound {
                    message: String::new(),
                },
                NOT_FOUND,
                "AGT_WRITE_NOT_FOUND: path not found",
            ),
            (
                WriteRefusal::IsDirectory,
                IS_DIRECTORY,
                "AGT_WRITE_IS_DIRECTORY: path is a directory",
            ),
            (
                WriteRefusal::Forbidden,
                FORBIDDEN,
                "AGT_WRITE_FORBIDDEN: permission denied",
            ),
            (
                WriteRefusal::OutsideWorkspace,
                OUTSIDE_WORKSPACE,
                "AGT_WRITE_OUTSIDE_WORKSPACE: path resolves outside the workspace",
            ),
            (
                WriteRefusal::Rejected {
                    message: "user denied the approval".to_string(),
                },
                REJECTED,
                "AGT_WRITE_REJECTED: user denied the approval",
            ),
            (
                WriteRefusal::Rejected {
                    message: "   ".to_string(),
                },
                REJECTED,
                "AGT_WRITE_REJECTED: write rejected",
            ),
        ];

        for (refusal, expected_code, expected_text) in cases {
            let tool = write_tool(RecordingSink::new().refusing(refusal));
            let output = run(tool.call(
                &CallContext::new(),
                json!({"path": "file.txt", "content": "text"}),
            ))
            .expect("call should not return a contract error");

            assert!(output.is_error());
            assert!(output.content().starts_with(expected_code));
            assert_eq!(output.content(), expected_text);
            assert_eq!(tool.sink.prepare_count(), 1);
            assert_eq!(tool.sink.write_count(), 0);
        }
    }

    #[test]
    fn a_prepare_failure_is_a_sink_failure_without_the_partial_note() {
        let tool =
            write_tool(RecordingSink::new().prepare_failed(SinkFailure::new("disk on fire")));
        let (code, text) = failed(
            run(tool.execute(&json!({"path": "file.txt", "content": "text"}))).expect("outcome"),
        );

        assert_eq!(code, SINK_FAILED);
        assert_eq!(code, "AGT_WRITE_SINK_FAILED");
        assert_eq!(text, "AGT_WRITE_SINK_FAILED: disk on fire");
        assert!(!text.contains("partially written"), "text: {text}");
        assert_eq!(tool.sink.prepare_count(), 1);
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn a_blank_prepare_failure_uses_the_documented_fallback() {
        let tool = write_tool(RecordingSink::new().prepare_failed(SinkFailure::new("")));
        let (code, text) = failed(
            run(tool.execute(&json!({"path": "file.txt", "content": "text"}))).expect("outcome"),
        );

        assert_eq!(code, SINK_FAILED);
        assert_eq!(text, "AGT_WRITE_SINK_FAILED: host hook failed");
        assert!(!text.contains("partially written"), "text: {text}");
        assert_eq!(tool.sink.write_count(), 0);
    }

    #[test]
    fn a_create_and_an_overwrite_render_different_verbs_and_keep_the_path() {
        let sink = RecordingSink::new().permitting(false, crate::write::Newline::AsIs);
        let tool = write_tool(sink);
        let (existed, bytes, text) = wrote(
            run(tool.execute(&json!({"path": "src/new.rs", "content": "hello"}))).expect("outcome"),
        );
        assert!(!existed);
        assert_eq!(bytes, 5);
        assert_eq!(text, "Created src/new.rs (5 bytes).");

        let output = run(tool.call(
            &CallContext::new(),
            json!({"path": "src/new.rs", "content": "hello"}),
        ))
        .expect("call");
        assert!(!output.is_error());
        assert_eq!(output.content(), "Created src/new.rs (5 bytes).");

        let sink = RecordingSink::new().permitting(true, crate::write::Newline::AsIs);
        let tool = write_tool(sink);
        let (existed, bytes, text) = wrote(
            run(tool.execute(&json!({"path": "src/old.rs", "content": "héllo"}))).expect("outcome"),
        );
        assert!(existed);
        assert_eq!(bytes, 6);
        assert_eq!(text, "Overwrote src/old.rs (6 bytes).");

        let output = run(tool.call(
            &CallContext::new(),
            json!({"path": "src/old.rs", "content": "héllo"}),
        ))
        .expect("call");
        assert!(!output.is_error());
        assert_eq!(output.content(), "Overwrote src/old.rs (6 bytes).");
    }

    #[test]
    fn a_second_call_asks_the_host_again() {
        let sink = RecordingSink::new().permitting(true, crate::write::Newline::AsIs);
        let tool = write_tool(sink);
        let input = json!({"path": "file.txt", "content": "text"});

        wrote(run(tool.execute(&input)).expect("outcome"));
        wrote(run(tool.execute(&input)).expect("outcome"));

        assert_eq!(tool.sink.prepare_count(), 2);
        assert_eq!(tool.sink.write_count(), 2);
    }

    #[test]
    fn as_is_hands_the_original_bytes_to_write() {
        let sink = RecordingSink::new().permitting(false, crate::write::Newline::AsIs);
        let tool = write_tool(sink);
        let (_, bytes, _) = wrote(
            run(tool.execute(&json!({
                "path": "file.txt",
                "content": "a\r\nb\rc\nd"
            })))
            .expect("outcome"),
        );

        assert_eq!(bytes, 8);
        assert_eq!(tool.sink.last_written().as_deref(), Some("a\r\nb\rc\nd"));
        // `prepare` always sees the original string, even when nothing changes.
        assert_eq!(tool.sink.last_prepared().as_deref(), Some("a\r\nb\rc\nd"));
    }

    #[test]
    fn an_lf_permit_converts_crlf_and_lone_cr() {
        let sink = RecordingSink::new().permitting(false, crate::write::Newline::Lf);
        let tool = write_tool(sink);
        let (_, bytes, _) = wrote(
            run(tool.execute(&json!({"path": "file.txt", "content": "a\r\nb\rc"})))
                .expect("outcome"),
        );

        assert_eq!(bytes, 5);
        assert_eq!(tool.sink.last_written().as_deref(), Some("a\nb\nc"));
        // The host approved the original string, not the converted one.
        assert_eq!(tool.sink.last_prepared().as_deref(), Some("a\r\nb\rc"));
    }

    #[test]
    fn a_crlf_permit_converts_and_does_not_double_existing_pairs() {
        let sink = RecordingSink::new().permitting(false, crate::write::Newline::CrLf);
        let tool = write_tool(sink);
        let (_, bytes, _) = wrote(
            run(tool.execute(&json!({"path": "file.txt", "content": "a\nb\r\nc"})))
                .expect("outcome"),
        );

        assert_eq!(bytes, 7);
        let written = tool.sink.last_written().expect("write ran");
        assert_eq!(written, "a\r\nb\r\nc");
        assert!(
            !written.contains("\r\r\n"),
            "existing pair was doubled: {written:?}"
        );
    }

    #[test]
    fn a_crlf_permit_turns_a_lone_cr_into_a_pair() {
        let sink = RecordingSink::new().permitting(false, crate::write::Newline::CrLf);
        let tool = write_tool(sink);
        let (_, _, _) = wrote(
            run(tool.execute(&json!({"path": "file.txt", "content": "a\rb"}))).expect("outcome"),
        );

        assert_eq!(tool.sink.last_written().as_deref(), Some("a\r\nb"));
    }

    #[test]
    fn a_write_failure_carries_the_partial_note() {
        let tool = write_tool(RecordingSink::new().write_failed(SinkFailure::new("disk full")));
        let output = run(tool.call(
            &CallContext::new(),
            json!({"path": "file.txt", "content": "text"}),
        ))
        .expect("call should not return a contract error");

        assert!(output.is_error());
        assert!(output.content().starts_with("AGT_WRITE_SINK_FAILED: "));
        assert!(output.content().contains("disk full"));
        assert!(
            output.content().contains("partially written"),
            "text: {}",
            output.content()
        );
        assert_eq!(tool.sink.prepare_count(), 1);
        assert_eq!(tool.sink.write_count(), 1);
    }

    #[test]
    fn the_spec_matches_the_parser_shape() {
        let tool = write_tool(RecordingSink::new());
        let spec = tool.spec();
        let schema = spec.input_schema();

        assert_eq!(spec.name(), "write");
        assert!(!spec.description().is_empty());
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["path", "content"]));
        assert_eq!(schema["properties"]["path"]["type"], json!("string"));
        assert_eq!(schema["properties"]["content"]["type"], json!("string"));
        assert!(schema["properties"].get("append").is_none());

        // The parser accepts the same clean shape and rejects an unknown field.
        assert!(
            run(tool.execute(&json!({"path": "a.txt", "content": "text"}))).is_ok(),
            "parser rejected the schema shape"
        );
        let error = run(tool.execute(&json!({"path": "a.txt", "content": "text", "append": true})))
            .expect_err("unknown field");
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
    }

    #[test]
    fn a_configured_description_reaches_the_spec() {
        let config = WriteConfig::new()
            .with_description("Rewrite one file.")
            .expect("valid description")
            .with_parameter_description("content", "The whole new file.")
            .expect("content is known");
        let tool = write_tool_with(RecordingSink::new(), config);

        assert_eq!(tool.spec().description(), "Rewrite one file.");
        assert_eq!(
            tool.spec().input_schema()["properties"]["content"]["description"],
            json!("The whole new file.")
        );
    }
}
