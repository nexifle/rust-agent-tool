//! The read tool implementation.

use serde_json::Value;

use crate::{ImagePart, Tool, ToolError, ToolOutput, ToolSpec, async_trait};

use super::classify::{Class, classify};
use super::config::ReadConfig;
use super::input::{
    DEFAULT_LIMIT_DESCRIPTION, DEFAULT_OFFSET_DESCRIPTION, DEFAULT_PATH_DESCRIPTION,
    DEFAULT_TOOL_DESCRIPTION, input_schema, parse_input,
};
use super::source::{FileBody, FileMeta, ReadNode, ReadSource, SourceFailure};
use super::window::{WindowRequest, scan_window};

/// Code for an unexpected host failure while looking up or reading a file.
const AGT_READ_SOURCE_FAILED: &str = "AGT_READ_SOURCE_FAILED";
/// Code for a path that does not exist.
const AGT_READ_NOT_FOUND: &str = "AGT_READ_NOT_FOUND";
/// Code for a path that names a directory.
const AGT_READ_IS_DIRECTORY: &str = "AGT_READ_IS_DIRECTORY";
/// Code for a path the host refused.
const AGT_READ_FORBIDDEN: &str = "AGT_READ_FORBIDDEN";
/// Code for a path that resolves outside the workspace.
const AGT_READ_OUTSIDE_WORKSPACE: &str = "AGT_READ_OUTSIDE_WORKSPACE";
/// Code for an image when the tool is not configured to return images.
const AGT_READ_IMAGE_DISABLED: &str = "AGT_READ_IMAGE_DISABLED";
/// Code for an image above the configured image byte limit.
const AGT_READ_IMAGE_TOO_LARGE: &str = "AGT_READ_IMAGE_TOO_LARGE";
/// Code for a binary file that was not returned.
const AGT_READ_BINARY: &str = "AGT_READ_BINARY";
/// Code for text that is not valid UTF-8.
const AGT_READ_DECODE_FAILED: &str = "AGT_READ_DECODE_FAILED";
/// Notice code for an offset past the last line.
const AGT_READ_OFFSET_PAST_EOF: &str = "AGT_READ_OFFSET_PAST_EOF";

/// The result of executing a read request.
///
/// [`ReadTool::call`] renders each variant for the model. An operational
/// failure is a [`ReadOutcome::Failed`], not a [`ToolError`], so the model can
/// see the code and recover.
#[derive(Debug)]
pub enum ReadOutcome {
    /// UTF-8 text selected by the requested offset and limit.
    Text {
        /// Rendered numbered lines and footer.
        body: String,
        /// Whether more lines exist after `body`.
        truncated: bool,
        /// First line represented by `body`.
        start_line: u64,
        /// Last line represented by `body`.
        end_line: u64,
    },
    /// A read that succeeded without text output.
    Notice {
        /// Optional stable notice code.
        code: Option<&'static str>,
        /// Human-readable description for the model.
        message: String,
    },
    /// Image bytes returned without line-based processing.
    Image {
        /// Detected image media type, such as `image/png`.
        media_type: &'static str,
        /// Complete image bytes.
        bytes: Vec<u8>,
        /// Human-readable description for the model.
        notice: String,
    },
    /// The read failed for an operational reason.
    Failed {
        /// Stable error code.
        code: &'static str,
        /// Human-readable error description.
        message: String,
    },
}

/// A tool that reads text and image files through a [`ReadSource`].
///
/// The tool never opens a file itself. The host injects a source when the tool
/// is constructed, and the source answers each lookup and byte read.
#[derive(Debug)]
pub struct ReadTool<S: ReadSource> {
    source: S,
    config: ReadConfig,
    name: String,
    description: String,
    path_description: String,
    offset_description: String,
    limit_description: String,
    spec: ToolSpec,
}

impl<S: ReadSource> ReadTool<S> {
    /// Builds a read tool with the default name, descriptions, and `config`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the configured binary
    /// sample is larger than a read chunk or smaller than 12 bytes.
    pub fn new(source: S, config: ReadConfig) -> Result<Self, ToolError> {
        let sample = config.binary_sample_bytes();
        if sample > config.chunk_bytes() {
            return Err(ToolError::spec_invalid(
                "read binary sample must not exceed the read chunk size",
            ));
        }
        if sample < 12 {
            return Err(ToolError::spec_invalid(
                "read binary sample must be at least 12 bytes",
            ));
        }

        let name = "read".to_string();
        let description = DEFAULT_TOOL_DESCRIPTION.to_string();
        let path_description = DEFAULT_PATH_DESCRIPTION.to_string();
        let offset_description = DEFAULT_OFFSET_DESCRIPTION.to_string();
        let limit_description = DEFAULT_LIMIT_DESCRIPTION.to_string();
        let spec = ToolSpec::new(
            name.clone(),
            description.clone(),
            input_schema(&path_description, &offset_description, &limit_description),
        )?;

        Ok(Self {
            source,
            config,
            name,
            description,
            path_description,
            offset_description,
            limit_description,
            spec,
        })
    }

    /// Replaces the tool name and rebuilds the spec.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `name` is blank or
    /// contains whitespace.
    pub fn with_name(mut self, name: impl Into<String>) -> Result<Self, ToolError> {
        let name = name.into();
        if name.trim().is_empty() || name.chars().any(char::is_whitespace) {
            return Err(ToolError::spec_invalid(
                "tool name must be non-empty and must not contain whitespace",
            ));
        }
        self.name = name;
        self.spec = self.build_spec()?;
        Ok(self)
    }

    /// Replaces the tool description and rebuilds the spec.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `description` is blank.
    pub fn with_description(mut self, description: impl Into<String>) -> Result<Self, ToolError> {
        let description = description.into();
        let description = description.trim();
        if description.is_empty() {
            return Err(ToolError::spec_invalid(
                "tool description must not be empty",
            ));
        }
        self.description = description.to_string();
        self.spec = self.build_spec()?;
        Ok(self)
    }

    /// Replaces the description of one input parameter and rebuilds the spec.
    ///
    /// Only `path`, `offset`, and `limit` are accepted.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `description` is blank or
    /// when `parameter` is not one of the three known fields.
    pub fn with_parameter_description(
        mut self,
        parameter: &str,
        description: impl Into<String>,
    ) -> Result<Self, ToolError> {
        let description = description.into();
        let description = description.trim();
        if description.is_empty() {
            return Err(ToolError::spec_invalid(
                "parameter description must not be empty",
            ));
        }
        let description = description.to_string();
        match parameter {
            "path" => self.path_description = description,
            "offset" => self.offset_description = description,
            "limit" => self.limit_description = description,
            other => {
                return Err(ToolError::spec_invalid(format!(
                    "unknown read parameter: {other}"
                )));
            }
        }
        self.spec = self.build_spec()?;
        Ok(self)
    }

    /// The tool specification registered with the agent.
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Runs one read request.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InputInvalid`] when the model input is not
    /// usable. Every operational failure is an [`ReadOutcome::Failed`] instead.
    pub async fn execute(&self, input: &Value) -> Result<ReadOutcome, ToolError> {
        let input = parse_input(input, &self.config)?;
        let path = input.path.as_str();

        let node = match self.source.lookup(path).await {
            Ok(node) => node,
            Err(failure) => return Ok(source_failed(&failure)),
        };

        let FileMeta { byte_len, body } = match node {
            ReadNode::Absent => {
                return Ok(ReadOutcome::Failed {
                    code: AGT_READ_NOT_FOUND,
                    message: format!("Path was not found: {path}."),
                });
            }
            ReadNode::Directory => {
                return Ok(ReadOutcome::Failed {
                    code: AGT_READ_IS_DIRECTORY,
                    message: format!(
                        "Path is a directory. Read does not list directories: {path}."
                    ),
                });
            }
            ReadNode::OutsideWorkspace => {
                return Ok(ReadOutcome::Failed {
                    code: AGT_READ_OUTSIDE_WORKSPACE,
                    message: format!("Path resolves outside the workspace: {path}."),
                });
            }
            ReadNode::Forbidden => {
                return Ok(ReadOutcome::Failed {
                    code: AGT_READ_FORBIDDEN,
                    message: format!("Permission denied: {path}."),
                });
            }
            ReadNode::File(meta) => meta,
        };

        if byte_len == 0 {
            return Ok(ReadOutcome::Notice {
                code: None,
                message: "File exists but is empty.".to_string(),
            });
        }

        let sample_len = std::cmp::min(byte_len, self.config.binary_sample_bytes() as u64) as usize;
        let body_ref = body.as_ref();
        let sample = match read_limited(body_ref, 0, sample_len, byte_len).await {
            Ok(bytes) => bytes,
            Err(outcome) => return Ok(outcome),
        };

        match classify(byte_len, &sample) {
            Class::Empty => Ok(ReadOutcome::Notice {
                code: None,
                message: "File exists but is empty.".to_string(),
            }),
            Class::Image { media_type } => Ok(self
                .read_image(body_ref, path, byte_len, sample, media_type)
                .await),
            Class::Binary => Ok(ReadOutcome::Failed {
                code: AGT_READ_BINARY,
                message: format!("File is binary and was not returned: {path}."),
            }),
            Class::Utf8 => Ok(self
                .read_text(body_ref, path, byte_len, sample, &input)
                .await),
        }
    }

    /// Reads the remaining bytes of an image after the sample was classified.
    async fn read_image(
        &self,
        body: &dyn FileBody,
        path: &str,
        byte_len: u64,
        sample: Vec<u8>,
        media_type: &'static str,
    ) -> ReadOutcome {
        if !self.config.images() {
            return ReadOutcome::Failed {
                code: AGT_READ_IMAGE_DISABLED,
                message: format!("This read tool is not configured to return images: {path}."),
            };
        }
        let max_image = self.config.max_image_bytes();
        if byte_len > max_image || buffer_would_exceed(sample.len() as u64, max_image) {
            return image_too_large(byte_len.max(sample.len() as u64), max_image, path);
        }

        let mut buffer = sample;
        while (buffer.len() as u64) < byte_len {
            let at = buffer.len() as u64;
            let remaining = byte_len - at;
            let want = std::cmp::min(self.config.chunk_bytes() as u64, remaining) as usize;
            let bytes = match read_limited(body, at, want, byte_len).await {
                Ok(bytes) => bytes,
                Err(outcome) => return outcome,
            };
            if buffer_would_exceed(buffer.len() as u64 + bytes.len() as u64, max_image) {
                return image_too_large(buffer.len() as u64 + bytes.len() as u64, max_image, path);
            }
            buffer.extend_from_slice(&bytes);
        }

        let len = buffer.len() as u64;
        ReadOutcome::Image {
            media_type,
            bytes: buffer,
            notice: format!("Image {media_type}, {len} bytes. Line offset and limit do not apply."),
        }
    }

    /// Streams and scans UTF-8 text after the sample was classified.
    async fn read_text(
        &self,
        body: &dyn FileBody,
        path: &str,
        byte_len: u64,
        sample: Vec<u8>,
        input: &super::input::ReadInput,
    ) -> ReadOutcome {
        let mut buffer = sample;
        let mut file_at = buffer.len() as u64;
        let mut lines_before = 0_u64;
        let mut force_first_line_truncated = false;
        let line_cap = usize::try_from(self.config.max_line_bytes()).unwrap_or(usize::MAX);

        loop {
            let entire_file = file_at >= byte_len;
            let outcome = scan_window(WindowRequest {
                bytes: buffer.clone(),
                offset: input.offset,
                limit: input.limit,
                requested_limit: input.requested_limit,
                max_lines: self.config.max_lines(),
                max_line_bytes: self.config.max_line_bytes(),
                max_output_bytes: self.config.max_output_bytes(),
                entire_file,
                lines_before,
                force_first_line_truncated,
            });

            debug_assert!(outcome.consumed <= buffer.len());
            if outcome.needs_more {
                if entire_file {
                    return ReadOutcome::Failed {
                        code: AGT_READ_SOURCE_FAILED,
                        message: format!(
                            "Read failed: source returned incomplete data for {path}."
                        ),
                    };
                }
                if outcome.skipped > 0 {
                    let skipped = outcome.skipped.min(buffer.len());
                    if buffer[..skipped].contains(&b'\n') {
                        force_first_line_truncated = false;
                    }
                    lines_before += buffer[..skipped]
                        .iter()
                        .filter(|byte| **byte == b'\n')
                        .count() as u64;
                    buffer.drain(..skipped);
                } else if buffer.len() > line_cap && !buffer.contains(&b'\n') {
                    let mut end = line_cap.min(buffer.len());
                    while end > 0 && std::str::from_utf8(&buffer[..end]).is_err() {
                        end -= 1;
                    }
                    buffer.truncate(end);
                    force_first_line_truncated = true;
                    match skip_until_newline(
                        body,
                        &mut buffer,
                        &mut file_at,
                        byte_len,
                        self.config.chunk_bytes(),
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(outcome) => return outcome,
                    }
                    continue;
                }
                let remaining = byte_len - file_at;
                let want = std::cmp::min(self.config.chunk_bytes() as u64, remaining) as usize;
                let bytes = match read_limited(body, file_at, want, byte_len).await {
                    Ok(bytes) => bytes,
                    Err(outcome) => return outcome,
                };
                file_at += bytes.len() as u64;
                buffer.extend_from_slice(&bytes);
                continue;
            }

            if outcome.decode_error {
                return ReadOutcome::Failed {
                    code: AGT_READ_DECODE_FAILED,
                    message: format!("File is not valid UTF-8. Bytes were not replaced: {path}."),
                };
            }
            if outcome.offset_past_eof {
                debug_assert!(outcome.total_lines.is_some());
                return ReadOutcome::Notice {
                    code: Some(AGT_READ_OFFSET_PAST_EOF),
                    message: outcome.body,
                };
            }
            debug_assert!(
                outcome.truncated || outcome.total_lines.is_some() || outcome.body.is_empty()
            );
            return ReadOutcome::Text {
                body: outcome.body,
                truncated: outcome.truncated,
                start_line: outcome.start_line,
                end_line: outcome.end_line,
            };
        }
    }

    fn build_spec(&self) -> Result<ToolSpec, ToolError> {
        ToolSpec::new(
            self.name.clone(),
            self.description.clone(),
            input_schema(
                &self.path_description,
                &self.offset_description,
                &self.limit_description,
            ),
        )
    }
}

/// Builds the failure shown for an unexpected source hook error.
fn source_failed(failure: &SourceFailure) -> ReadOutcome {
    ReadOutcome::Failed {
        code: AGT_READ_SOURCE_FAILED,
        message: format!("Read failed: {failure}."),
    }
}

/// Builds the failure shown for an image above the configured limit.
fn image_too_large(byte_len: u64, max: u64, path: &str) -> ReadOutcome {
    ReadOutcome::Failed {
        code: AGT_READ_IMAGE_TOO_LARGE,
        message: format!(
            "Image is {byte_len} bytes, which is above the limit of {max} bytes: {path}."
        ),
    }
}

/// True when `len` is above the image cap.
fn buffer_would_exceed(len: u64, max_image: u64) -> bool {
    len > max_image
}

/// Discards the rest of a line that was already cut to the line cap.
///
/// Bytes up to and including the newline are not stored, except one `\n` so the
/// capped prefix is a finished line. Bytes after that newline stay in `buffer`.
async fn skip_until_newline(
    body: &dyn FileBody,
    buffer: &mut Vec<u8>,
    file_at: &mut u64,
    byte_len: u64,
    chunk_bytes: usize,
) -> Result<(), ReadOutcome> {
    loop {
        if *file_at >= byte_len {
            return Ok(());
        }
        let remaining = byte_len - *file_at;
        let want = std::cmp::min(chunk_bytes as u64, remaining) as usize;
        let bytes = read_limited(body, *file_at, want, byte_len).await?;
        *file_at += bytes.len() as u64;
        if let Some(index) = bytes.iter().position(|byte| *byte == b'\n') {
            buffer.push(b'\n');
            buffer.extend_from_slice(&bytes[index + 1..]);
            return Ok(());
        }
    }
}

/// Reads at most `max` bytes at `at`.
///
/// The returned buffer is never longer than `max` or the bytes remaining before
/// `byte_len`. A short non-empty buffer before that point is a stuck body and
/// is not retried.
async fn read_limited(
    body: &dyn FileBody,
    at: u64,
    max: usize,
    byte_len: u64,
) -> Result<Vec<u8>, ReadOutcome> {
    if max == 0 || at >= byte_len {
        return Ok(Vec::new());
    }
    let remaining = usize::try_from(byte_len - at).unwrap_or(usize::MAX);
    let allowed = max.min(remaining);
    let mut bytes = body
        .read_at(at, allowed)
        .await
        .map_err(|failure| source_failed(&failure))?;
    if bytes.len() > allowed {
        bytes.truncate(allowed);
    }
    if bytes.len() < allowed {
        return Err(ReadOutcome::Failed {
            code: AGT_READ_SOURCE_FAILED,
            message: "Read failed: source made no progress before the end of the file.".to_string(),
        });
    }
    Ok(bytes)
}

#[async_trait]
impl<S: ReadSource + 'static> Tool for ReadTool<S> {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, _ctx: &crate::CallContext, input: Value) -> Result<ToolOutput, ToolError> {
        match self.execute(&input).await? {
            ReadOutcome::Text { body, .. } => Ok(ToolOutput::text(body)),
            ReadOutcome::Notice {
                code: Some(code),
                message,
            } => Ok(ToolOutput::text(format!("{code}: {message}"))),
            ReadOutcome::Notice {
                code: None,
                message,
            } => Ok(ToolOutput::text(message)),
            ReadOutcome::Image {
                media_type,
                bytes,
                notice,
            } => Ok(ToolOutput::text(notice).with_image(ImagePart::new(media_type, bytes))),
            ReadOutcome::Failed { code, message } => {
                Ok(ToolOutput::error_text(format!("{code}: {message}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use serde_json::json;

    use super::*;
    use crate::CallContext;
    use crate::read::source::MemorySource;

    fn run<F: std::future::Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("read future should not wait"),
        }
    }

    fn read_tool(source: MemorySource) -> ReadTool<MemorySource> {
        ReadTool::new(source, ReadConfig::default()).expect("tool should be constructed")
    }

    fn failed(outcome: ReadOutcome) -> (&'static str, String) {
        match outcome {
            ReadOutcome::Failed { code, message } => (code, message),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    fn notice(outcome: ReadOutcome) -> (Option<&'static str>, String) {
        match outcome {
            ReadOutcome::Notice { code, message } => (code, message),
            other => panic!("expected a notice, got {other:?}"),
        }
    }

    #[test]
    fn rejects_samples_larger_than_chunks() {
        // The chunk setter does not recheck the stored 8 KiB sample.
        let config = ReadConfig::default()
            .with_chunk_bytes(12)
            .expect("chunk floor is 12");
        let error = ReadTool::new(MemorySource::new(), config).expect_err("sample is larger");
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));
    }

    #[test]
    fn rejects_invalid_read_input() {
        let tool = read_tool(MemorySource::new());
        let error = run(tool.execute(&json!({"path": ""}))).expect_err("blank path");
        assert_eq!(
            error.to_string(),
            "AGT_INPUT_INVALID: field \"path\" must not be empty or whitespace only"
        );
    }

    #[test]
    fn reports_empty_files_without_reading() {
        let source = MemorySource::new();
        source.insert_file("empty.txt", Vec::new());
        let (code, message) =
            notice(run(read_tool(source).execute(&json!({"path": "empty.txt"}))).expect("outcome"));
        assert_eq!(code, None);
        assert_eq!(message, "File exists but is empty.");
    }

    #[test]
    fn reports_all_non_file_nodes() {
        let source = MemorySource::new();
        source.insert_absent("missing.txt");
        source.insert_directory("dir");
        source.insert_forbidden("secret.txt");
        source.insert_outside("../escape.txt");
        let tool = read_tool(source);

        let cases = [
            (
                "missing.txt",
                "AGT_READ_NOT_FOUND",
                "Path was not found: missing.txt.",
            ),
            (
                "dir",
                "AGT_READ_IS_DIRECTORY",
                "Path is a directory. Read does not list directories: dir.",
            ),
            (
                "secret.txt",
                "AGT_READ_FORBIDDEN",
                "Permission denied: secret.txt.",
            ),
            (
                "../escape.txt",
                "AGT_READ_OUTSIDE_WORKSPACE",
                "Path resolves outside the workspace: ../escape.txt.",
            ),
        ];
        for (path, expected_code, expected_message) in cases {
            let outcome = run(tool.execute(&json!({ "path": path }))).expect("outcome");
            let (code, message) = failed(outcome);
            assert_eq!(code, expected_code);
            assert_eq!(message, expected_message);
        }
    }

    #[test]
    fn reports_binary_nul_files() {
        let source = MemorySource::new();
        source.insert_file("binary.dat", vec![b'A', 0, b'B', b'C']);
        let (code, message) = failed(
            run(read_tool(source).execute(&json!({"path": "binary.dat"}))).expect("outcome"),
        );
        assert_eq!(code, "AGT_READ_BINARY");
        assert_eq!(message, "File is binary and was not returned: binary.dat.");
    }

    #[test]
    fn returns_images_when_enabled() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let source = MemorySource::new();
        source.insert_file("image.png", png.clone());
        let outcome =
            run(read_tool(source).execute(&json!({"path": "image.png"}))).expect("outcome");
        match outcome {
            ReadOutcome::Image {
                media_type,
                bytes,
                notice,
            } => {
                assert_eq!(media_type, "image/png");
                assert_eq!(bytes, png);
                assert_eq!(
                    notice,
                    "Image image/png, 12 bytes. Line offset and limit do not apply."
                );
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn rejects_images_over_the_size_limit() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let too_big = 6 * 1024 * 1024;
        let source = MemorySource::new();
        let mut png = png;
        png.resize(8 * 1024, 0);
        source.insert_file_with_len("big.png", png, too_big);
        let (code, message) =
            failed(run(read_tool(source).execute(&json!({"path": "big.png"}))).expect("outcome"));
        assert_eq!(code, "AGT_READ_IMAGE_TOO_LARGE");
        assert_eq!(
            message,
            "Image is 6291456 bytes, which is above the limit of 5242880 bytes: big.png."
        );
    }

    #[test]
    fn rejects_images_when_disabled() {
        let png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        let source = MemorySource::new();
        source.insert_file("image.png", png);
        let config = ReadConfig::default().with_images(false);
        let tool = ReadTool::new(source, config).expect("tool should be constructed");
        let (code, message) =
            failed(run(tool.execute(&json!({"path": "image.png"}))).expect("outcome"));
        assert_eq!(code, "AGT_READ_IMAGE_DISABLED");
        assert_eq!(
            message,
            "This read tool is not configured to return images: image.png."
        );
    }

    #[test]
    fn reports_stuck_empty_reads() {
        let source = MemorySource::new();
        source.insert_file_with_len("stuck.txt", b"hello".to_vec(), 100);
        let (code, message) =
            failed(run(read_tool(source).execute(&json!({"path": "stuck.txt"}))).expect("outcome"));
        assert_eq!(code, "AGT_READ_SOURCE_FAILED");
        assert!(message.contains("made no progress"), "message: {message}");
    }

    #[test]
    fn reads_utf8_text() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello\nworld\n".to_vec());
        let outcome =
            run(read_tool(source).execute(&json!({"path": "hello.txt"}))).expect("outcome");
        match outcome {
            ReadOutcome::Text {
                body,
                truncated,
                start_line,
                end_line,
            } => {
                assert_eq!(
                    body,
                    "     1\thello\n     2\tworld\nShowing lines 1-2 of 2."
                );
                assert!(!truncated);
                assert_eq!(start_line, 1);
                assert_eq!(end_line, 2);
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn splits_multibyte_utf8_across_chunks() {
        // Eleven ASCII bytes, then the two bytes of `é`, then a newline. The
        // 12-byte sample ends inside the character.
        let mut bytes = vec![b'a'; 11];
        bytes.extend_from_slice(&[0xc3, 0xa9, b'\n']);
        let source = MemorySource::new();
        source.insert_file("accent.txt", bytes);
        let config = ReadConfig::default()
            .with_chunk_bytes(12)
            .expect("chunk floor is 12")
            .with_binary_sample_bytes(12)
            .expect("sample fits the chunk");
        let tool = ReadTool::new(source, config).expect("tool should be constructed");
        let outcome = run(tool.execute(&json!({"path": "accent.txt"}))).expect("outcome");
        match outcome {
            ReadOutcome::Text { body, .. } => {
                assert_eq!(body, "     1\taaaaaaaaaaa\u{e9}\nShowing lines 1-1 of 1.");
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn reports_lookup_source_failures() {
        let source = MemorySource::new();
        source.fail_lookup_with("disk unavailable");
        let (code, message) =
            failed(run(read_tool(source).execute(&json!({"path": "file.txt"}))).expect("outcome"));
        assert_eq!(code, "AGT_READ_SOURCE_FAILED");
        assert_eq!(message, "Read failed: disk unavailable.");
    }

    #[test]
    fn caps_limits_with_a_sentence() {
        let source = MemorySource::new();
        source.insert_file("many.txt", b"one\ntwo\nthree\n".to_vec());
        let config = ReadConfig::default()
            .with_max_lines(1)
            .expect("line floor is 1");
        let tool = ReadTool::new(source, config).expect("tool should be constructed");
        let outcome =
            run(tool.execute(&json!({"path": "many.txt", "limit": 9999}))).expect("outcome");
        match outcome {
            ReadOutcome::Text {
                body, truncated, ..
            } => {
                assert!(truncated);
                assert_eq!(
                    body,
                    "     1\tone\nShowing lines 1-1. More lines exist. Continue with offset=2. Requested limit was capped at 1."
                );
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn description_setters_rebuild_the_spec() {
        let tool = read_tool(MemorySource::new());
        assert_eq!(tool.spec().name(), "read");
        assert_eq!(tool.spec().description(), DEFAULT_TOOL_DESCRIPTION);

        let tool = tool
            .with_name("read_file")
            .expect("name should be valid")
            .with_description("Read a file.")
            .expect("description should be valid")
            .with_parameter_description("path", "The file to read.")
            .expect("parameter should be valid")
            .with_parameter_description("offset", "First line.")
            .expect("parameter should be valid")
            .with_parameter_description("limit", "Line count.")
            .expect("parameter should be valid");

        assert_eq!(tool.spec().name(), "read_file");
        assert_eq!(tool.spec().description(), "Read a file.");
        let schema = tool.spec().input_schema();
        assert_eq!(
            schema["properties"]["path"]["description"],
            json!("The file to read.")
        );
        assert_eq!(
            schema["properties"]["offset"]["description"],
            json!("First line.")
        );
        assert_eq!(
            schema["properties"]["limit"]["description"],
            json!("Line count.")
        );
    }

    #[test]
    fn description_setters_reject_invalid_values() {
        let error = read_tool(MemorySource::new())
            .with_name("two words")
            .expect_err("whitespace name");
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));

        let error = read_tool(MemorySource::new())
            .with_name("   ")
            .expect_err("blank name");
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));

        let error = read_tool(MemorySource::new())
            .with_description("   ")
            .expect_err("blank description");
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));

        let error = read_tool(MemorySource::new())
            .with_parameter_description("unknown", "text")
            .expect_err("unknown parameter");
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));

        let error = read_tool(MemorySource::new())
            .with_parameter_description("path", "  ")
            .expect_err("blank parameter description");
        assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));
    }

    #[test]
    fn call_maps_outcomes_to_text_or_error() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello\n".to_vec());
        let tool = read_tool(source);

        let output = run(tool.call(&CallContext::new(), json!({"path": "hello.txt"})))
            .expect("call should succeed");
        assert!(!output.is_error());
        assert_eq!(output.content(), "     1\thello\nShowing lines 1-1 of 1.");

        let output = run(tool.call(&CallContext::new(), json!({"path": "missing.txt"})))
            .expect("call should succeed");
        assert!(output.is_error());
        assert_eq!(
            output.content(),
            "AGT_READ_NOT_FOUND: Path was not found: missing.txt."
        );
    }

    #[test]
    fn descriptions_are_trimmed_before_they_are_stored() {
        let tool = read_tool(MemorySource::new())
            .with_description("  Read a file.  ")
            .expect("description")
            .with_parameter_description("path", "  The file.  ")
            .expect("parameter");
        assert_eq!(tool.spec().description(), "Read a file.");
        assert_eq!(
            tool.spec().input_schema()["properties"]["path"]["description"],
            json!("The file.")
        );
    }

    fn small_chunks(config: ReadConfig) -> ReadConfig {
        config
            .with_chunk_bytes(12)
            .expect("chunk")
            .with_binary_sample_bytes(12)
            .expect("sample")
    }

    #[test]
    fn line_numbers_continue_across_chunks() {
        let source = MemorySource::new();
        source.insert_file(
            "wide.txt",
            b"aaaaaaaaaaa\nbbbbbbbbbbb\nccccccccccc\n".to_vec(),
        );
        let tool = ReadTool::new(source, small_chunks(ReadConfig::default())).expect("tool");
        let outcome = run(tool.execute(&json!({"path": "wide.txt"}))).expect("outcome");
        match outcome {
            ReadOutcome::Text {
                body, truncated, ..
            } => {
                assert!(!truncated);
                assert!(body.contains("     1\taaaaaaaaaaa"));
                assert!(body.contains("     2\tbbbbbbbbbbb"));
                assert!(body.contains("     3\tccccccccccc"));
                assert!(body.contains("of 3"));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn a_middle_window_does_not_read_past_its_chunk() {
        let source = MemorySource::new();
        source.insert_file(
            "wide.txt",
            b"aaaaaaaaaaa\nbbbbbbbbbbb\nccccccccccc\n".to_vec(),
        );
        let tool = ReadTool::new(source, small_chunks(ReadConfig::default())).expect("tool");
        let outcome = run(tool.execute(&json!({
            "path": "wide.txt",
            "offset": 2,
            "limit": 1
        })))
        .expect("outcome");
        match outcome {
            ReadOutcome::Text { body, .. } => {
                assert!(body.contains("     2\tbbbbbbbbbbb"));
                assert!(!body.contains("ccccccccccc"));
            }
            other => panic!("expected text, got {other:?}"),
        }
        assert!(
            tool.source.read_calls().iter().all(|call| call.0 < 24),
            "reads past the chunk of line 2: {:?}",
            tool.source.read_calls()
        );
    }

    #[test]
    fn offset_past_eof_counts_lines_across_chunks() {
        let source = MemorySource::new();
        source.insert_file(
            "wide.txt",
            b"aaaaaaaaaaa\nbbbbbbbbbbb\nccccccccccc\n".to_vec(),
        );
        let tool = ReadTool::new(source, small_chunks(ReadConfig::default())).expect("tool");
        let outcome =
            run(tool.execute(&json!({"path": "wide.txt", "offset": 9}))).expect("outcome");
        let (code, message) = notice(outcome);
        assert_eq!(code, Some("AGT_READ_OFFSET_PAST_EOF"));
        assert!(message.contains("File has 3 lines"), "{message}");
    }

    #[test]
    fn a_long_line_is_capped_without_keeping_the_rest() {
        let source = MemorySource::new();
        source.insert_file("long.txt", vec![b'x'; 50_000]);
        let config = ReadConfig::default()
            .with_chunk_bytes(64)
            .expect("chunk")
            .with_binary_sample_bytes(12)
            .expect("sample")
            .with_max_line_bytes(8)
            .expect("line cap");
        let tool = ReadTool::new(source, config).expect("tool");
        let outcome = run(tool.execute(&json!({"path": "long.txt"}))).expect("outcome");
        match outcome {
            ReadOutcome::Text { body, .. } => {
                assert!(body.contains("[line truncated]"), "{body}");
                assert!(body.contains("of 1"), "{body}");
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    struct OneShot {
        byte_len: u64,
        payload: Vec<u8>,
    }

    #[async_trait]
    impl ReadSource for OneShot {
        async fn lookup(&self, path: &str) -> Result<ReadNode, SourceFailure> {
            if path != "file" {
                return Ok(ReadNode::Absent);
            }
            Ok(ReadNode::File(FileMeta::new(
                self.byte_len,
                SliceBody {
                    payload: self.payload.clone(),
                },
            )))
        }
    }

    struct SliceBody {
        payload: Vec<u8>,
    }

    #[async_trait]
    impl FileBody for SliceBody {
        async fn read_at(&self, at: u64, max: usize) -> Result<Vec<u8>, SourceFailure> {
            if max == 0 {
                return Ok(Vec::new());
            }
            let start = usize::try_from(at).unwrap_or(self.payload.len());
            if start >= self.payload.len() {
                return Ok(Vec::new());
            }
            let mut end = start + 1;
            if max == 1 {
                end = start + 1;
            }
            let generous = (start + self.payload.len()).min(self.payload.len());
            if max > 1 {
                end = generous;
            }
            Ok(self.payload[start..end].to_vec())
        }
    }

    #[test]
    fn a_short_read_before_the_end_is_a_source_failure() {
        let tool = ReadTool::new(
            OneShot {
                byte_len: 100,
                payload: b"hello world".to_vec(),
            },
            ReadConfig::default(),
        )
        .expect("tool");
        let outcome = run(tool.execute(&json!({"path": "file"}))).expect("outcome");
        let (code, _) = failed(outcome);
        assert_eq!(code, "AGT_READ_SOURCE_FAILED");
    }

    #[test]
    fn an_over_long_image_read_is_truncated_to_the_declared_size() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        png.extend(std::iter::repeat_n(0, 40));
        let tool = ReadTool::new(
            OneShot {
                byte_len: 8,
                payload: png,
            },
            ReadConfig::default(),
        )
        .expect("tool");
        let outcome = run(tool.execute(&json!({"path": "file"}))).expect("outcome");
        match outcome {
            ReadOutcome::Image { bytes, notice, .. } => {
                assert_eq!(bytes.len(), 8);
                assert_eq!(
                    notice,
                    "Image image/png, 8 bytes. Line offset and limit do not apply."
                );
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }
}
