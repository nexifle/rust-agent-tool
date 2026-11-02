//! Host example: a `WriteSink` that writes real files inside one directory.
//!
//! This is not part of the SDK. The library still does not call `std::fs`.
//! The host resolves the model path, checks that the canonical path stays
//! inside the workspace, then writes the bytes the tool already normalized.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rust_agent_tool::{
    Newline, PrepareError, SinkFailure, WriteConfig, WriteOutcome, WritePermit, WriteRefusal,
    WriteSink, WriteTool, async_trait, path_is_within,
};
use serde_json::json;

/// Workspace jail. A model path is resolved against `root` and written only
/// when every canonical prefix stays inside that directory.
struct FsSink {
    root: PathBuf,
    /// When false, a missing parent is [`WriteRefusal::NotFound`] instead of `mkdir`.
    create_parents: bool,
}

impl FsSink {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            create_parents: true,
        }
    }

    /// Walk `model_path` from the canonical workspace root.
    ///
    /// `.` and `..` are refused before any directory is created. An existing
    /// symlink is canonicalized at that component, so a link that leaves the
    /// workspace never becomes a parent the host creates directories under.
    fn resolve(&self, model_path: &str) -> Result<Resolved, PrepareError> {
        let raw = Path::new(model_path);
        if raw.is_absolute() {
            return Err(outside());
        }

        let mut current = self.root_canon()?;
        let mut components = raw.components().peekable();
        if components.peek().is_none() {
            return Err(not_found("path has no file name"));
        }

        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(outside());
            };
            let next = current.join(name);
            let is_last = components.peek().is_none();

            if next.exists() {
                let canonical = fs::canonicalize(&next).map_err(io_failure)?;
                if !path_is_within(&canonical, &self.root_canon()?) {
                    return Err(outside());
                }
                if is_last {
                    if canonical.is_dir() {
                        return Err(PrepareError::Refusal(WriteRefusal::IsDirectory));
                    }
                    return Ok(Resolved {
                        path: canonical,
                        existed: true,
                    });
                }
                current = canonical;
                continue;
            }

            if !path_is_within(&next, &self.root_canon()?) {
                return Err(outside());
            }
            if is_last {
                return Ok(Resolved {
                    path: next,
                    existed: false,
                });
            }
            if !self.create_parents {
                return Err(not_found("parent directory does not exist"));
            }
            fs::create_dir(&next).map_err(io_failure)?;
            current = next;
        }

        Err(not_found("path has no file name"))
    }

    fn root_canon(&self) -> Result<PathBuf, PrepareError> {
        fs::canonicalize(&self.root).map_err(io_failure)
    }
}

struct Resolved {
    path: PathBuf,
    existed: bool,
}

fn outside() -> PrepareError {
    PrepareError::Refusal(WriteRefusal::OutsideWorkspace)
}

fn not_found(message: &str) -> PrepareError {
    PrepareError::Refusal(WriteRefusal::NotFound {
        message: message.to_string(),
    })
}

fn io_failure(error: std::io::Error) -> PrepareError {
    PrepareError::Failed(SinkFailure::new(error.to_string()))
}

#[async_trait]
impl WriteSink for FsSink {
    async fn prepare(&self, path: &str, _content: &str) -> Result<WritePermit, PrepareError> {
        let resolved = self.resolve(path)?;
        Ok(WritePermit::new(resolved.existed, Newline::AsIs))
    }

    async fn write(&self, path: &str, content: &str) -> Result<(), SinkFailure> {
        let resolved = self.resolve(path).map_err(|error| match error {
            PrepareError::Failed(failure) => failure,
            PrepareError::Refusal(_) => {
                SinkFailure::new("path was refused after prepare accepted it")
            }
        })?;
        fs::write(&resolved.path, content.as_bytes())
            .map_err(|error| SinkFailure::new(error.to_string()))
    }
}

fn workspace() -> PathBuf {
    static TICK: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let n = TICK.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("rust-agent-tool-write-{nanos}-{n}"));
    fs::create_dir_all(&path).expect("workspace");
    fs::canonicalize(&path).expect("workspace canonical")
}

struct TempWorkspace(PathBuf);

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = std::pin::pin!(future);
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("filesystem sink should not wait"),
    }
}

#[test]
fn a_host_creates_and_then_overwrites_a_real_file() {
    let root = TempWorkspace(workspace());
    let tool = WriteTool::new(WriteConfig::new(), FsSink::new(root.0.clone()));

    let created = run(tool.execute(&json!({
        "path": "notes/hello.txt",
        "content": "hello"
    })))
    .expect("input");
    match created {
        WriteOutcome::Wrote {
            existed,
            bytes,
            text,
            ..
        } => {
            assert!(!existed);
            assert_eq!(bytes, 5);
            assert_eq!(text, "Created notes/hello.txt (5 bytes).");
        }
        other => panic!("expected create, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(root.0.join("notes/hello.txt")).expect("read back"),
        "hello"
    );

    let overwrote = run(tool.execute(&json!({
        "path": "notes/hello.txt",
        "content": "replaced"
    })))
    .expect("input");
    match overwrote {
        WriteOutcome::Wrote { existed, bytes, .. } => {
            assert!(existed);
            assert_eq!(bytes, 8);
        }
        other => panic!("expected overwrite, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(root.0.join("notes/hello.txt")).expect("read back"),
        "replaced"
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_that_leaves_the_workspace_is_refused() {
    let root = TempWorkspace(workspace());
    let outside = TempWorkspace(workspace());
    std::os::unix::fs::symlink(&outside.0, root.0.join("escape")).expect("symlink");

    let tool = WriteTool::new(WriteConfig::new(), FsSink::new(root.0.clone()));
    let outcome = run(tool.execute(&json!({"path": "escape", "content": "nope"}))).expect("input");
    match outcome {
        WriteOutcome::Failed { code, .. } => {
            assert_eq!(code, rust_agent_tool::OUTSIDE_WORKSPACE);
        }
        other => panic!("expected refusal, got {other:?}"),
    }
    assert!(fs::read_dir(&outside.0).expect("outside").next().is_none());
}

#[test]
fn a_directory_is_not_a_file() {
    let root = TempWorkspace(workspace());
    fs::create_dir(root.0.join("dir")).expect("dir");
    let tool = WriteTool::new(WriteConfig::new(), FsSink::new(root.0.clone()));
    let outcome = run(tool.execute(&json!({"path": "dir", "content": "nope"}))).expect("input");
    match outcome {
        WriteOutcome::Failed { code, .. } => assert_eq!(code, rust_agent_tool::IS_DIRECTORY),
        other => panic!("expected directory refusal, got {other:?}"),
    }
}

#[test]
fn a_parent_segment_never_leaves_the_workspace() {
    let root = TempWorkspace(workspace());
    let tool = WriteTool::new(WriteConfig::new(), FsSink::new(root.0.clone()));
    let outcome =
        run(tool.execute(&json!({"path": "../outside.txt", "content": "nope"}))).expect("input");
    match outcome {
        WriteOutcome::Failed { code, .. } => {
            assert_eq!(code, rust_agent_tool::OUTSIDE_WORKSPACE);
        }
        other => panic!("expected refusal, got {other:?}"),
    }
}

#[test]
fn crlf_bytes_are_what_hits_the_disk() {
    let root = TempWorkspace(workspace());
    let tool = WriteTool::new(WriteConfig::new(), CrLf(FsSink::new(root.0.clone())));
    let outcome = run(tool.execute(&json!({"path": "win.txt", "content": "a\nb"}))).expect("input");
    match outcome {
        WriteOutcome::Wrote { bytes, .. } => assert_eq!(bytes, 4),
        other => panic!("expected write, got {other:?}"),
    }
    assert_eq!(fs::read(root.0.join("win.txt")).expect("bytes"), b"a\r\nb");
}

/// Same jail as [`FsSink`], but the permit asks the SDK to rewrite newlines.
struct CrLf(FsSink);

#[async_trait]
impl WriteSink for CrLf {
    async fn prepare(&self, path: &str, content: &str) -> Result<WritePermit, PrepareError> {
        let mut permit = self.0.prepare(path, content).await?;
        permit.newline = Newline::CrLf;
        Ok(permit)
    }

    async fn write(&self, path: &str, content: &str) -> Result<(), SinkFailure> {
        self.0.write(path, content).await
    }
}
