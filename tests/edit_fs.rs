//! Host example: `EditSource` and `EditSink` over real files in one directory.
//!
//! This is not part of the SDK. The library still does not call `std::fs`.
//! The host resolves the model path, checks that every canonical component
//! stays inside the workspace, compares the bytes it read with the bytes the
//! tool hands back, and replaces the file through a sibling temp file and a
//! rename.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rust_agent_tool::edit::{
    ApplyError, EditFile, IS_DIRECTORY, NOT_FOUND, OUTSIDE_WORKSPACE, OpenError, STALE,
};
use rust_agent_tool::{
    EditConfig, EditOutcome, EditPermit, EditRefusal, EditSink, EditSource, EditTool, HostFailure,
    async_trait, path_is_within,
};
use serde_json::json;

/// Workspace jail. A model path is resolved against `root` and accepted only
/// when every canonical component stays inside that directory.
struct FsHooks {
    root: PathBuf,
}

impl FsHooks {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Walk `model_path` from the canonical workspace root.
    ///
    /// An absolute path and any `.` or `..` component are refused before the
    /// disk is touched. An existing component is canonicalized, so a symlink
    /// that leaves the workspace is refused instead of followed.
    fn resolve(&self, model_path: &str) -> Result<PathBuf, ResolveError> {
        let raw = Path::new(model_path);
        if raw.is_absolute() {
            return Err(outside());
        }

        let root = fs::canonicalize(&self.root).map_err(io_failure)?;
        let mut current = root.clone();
        let mut components = raw.components().peekable();
        if components.peek().is_none() {
            return Err(missing("path has no file name"));
        }

        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(outside());
            };
            let next = current.join(name);
            let is_last = components.peek().is_none();

            if !next.exists() {
                if !path_is_within(&next, &root) {
                    return Err(outside());
                }
                if is_last {
                    return Err(missing("file does not exist"));
                }
                current = next;
                continue;
            }

            let canonical = fs::canonicalize(&next).map_err(io_failure)?;
            if !path_is_within(&canonical, &root) {
                return Err(outside());
            }
            if is_last {
                if canonical.is_dir() {
                    return Err(ResolveError::Refusal(EditRefusal::IsDirectory));
                }
                return Ok(canonical);
            }
            current = canonical;
        }

        Err(missing("path has no file name"))
    }
}

/// Why the jail did not return a path.
enum ResolveError {
    Refusal(EditRefusal),
    Failed(HostFailure),
}

impl From<ResolveError> for OpenError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::Refusal(refusal) => Self::Refusal(refusal),
            ResolveError::Failed(failure) => Self::Failed(failure),
        }
    }
}

impl From<ResolveError> for ApplyError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::Refusal(refusal) => Self::Refusal(refusal),
            ResolveError::Failed(failure) => Self::Failed(failure),
        }
    }
}

fn outside() -> ResolveError {
    ResolveError::Refusal(EditRefusal::OutsideWorkspace)
}

fn missing(message: &str) -> ResolveError {
    ResolveError::Refusal(EditRefusal::NotFound {
        message: message.to_string(),
    })
}

fn io_failure(error: std::io::Error) -> ResolveError {
    ResolveError::Failed(HostFailure::new(error.to_string()))
}

fn sink_failure(error: std::io::Error) -> ApplyError {
    ApplyError::Failed(HostFailure::new(error.to_string()))
}

#[async_trait]
impl EditSource for FsHooks {
    async fn open(&self, path: &str) -> Result<EditFile, OpenError> {
        let resolved = self.resolve(path)?;
        let bytes = fs::read(&resolved)
            .map_err(|error| OpenError::Failed(HostFailure::new(error.to_string())))?;
        Ok(EditFile { bytes })
    }
}

#[async_trait]
impl EditSink for FsHooks {
    async fn apply(&self, path: &str, before: &str, after: &str) -> Result<EditPermit, ApplyError> {
        let resolved = self.resolve(path)?;
        let current = fs::read(&resolved).map_err(sink_failure)?;
        if current != before.as_bytes() {
            return Err(ApplyError::Stale);
        }

        // Sibling temp file then an atomic rename, so a failure never leaves a
        // half-written file at the path the model named.
        let temp = resolved.with_extension(format!("edit-{}.tmp", unique()));
        fs::write(&temp, after.as_bytes()).map_err(sink_failure)?;
        fs::rename(&temp, &resolved).map_err(|error| {
            let _ = fs::remove_file(&temp);
            sink_failure(error)
        })?;
        Ok(EditPermit::new(after.len()))
    }
}

/// A source that rewrites the file between `open` and `apply`.
struct StaleSource {
    inner: FsHooks,
    /// Absolute path rewritten after `open` already returned the old bytes.
    poison: PathBuf,
    poison_bytes: Vec<u8>,
}

#[async_trait]
impl EditSource for StaleSource {
    async fn open(&self, path: &str) -> Result<EditFile, OpenError> {
        let file = self.inner.open(path).await?;
        fs::write(&self.poison, &self.poison_bytes).expect("poison the file");
        Ok(file)
    }
}

fn unique() -> u64 {
    static TICK: AtomicU64 = AtomicU64::new(0);
    TICK.fetch_add(1, Ordering::Relaxed)
}

fn workspace() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("rust-agent-tool-edit-{nanos}-{}", unique()));
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
        Poll::Pending => panic!("filesystem hooks should not wait"),
    }
}

fn tool(root: &Path) -> EditTool<FsHooks, FsHooks> {
    EditTool::new(
        EditConfig::new(),
        FsHooks::new(root.to_path_buf()),
        FsHooks::new(root.to_path_buf()),
    )
}

fn failed_code(outcome: &EditOutcome) -> &'static str {
    match outcome {
        EditOutcome::Failed { code, .. } => code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_parent_segment_is_outside_the_workspace_and_creates_nothing() {
    // The workspace is a child of the temp directory, so `../outside.txt` names
    // a path next to it that no other test can already own.
    let base = TempWorkspace(workspace());
    let root = base.0.join("ws");
    fs::create_dir(&root).expect("workspace");

    let outcome = run(tool(&root).execute(&json!({
        "path": "../outside.txt",
        "old_text": "a",
        "new_text": "b"
    })))
    .expect("input");

    assert_eq!(failed_code(&outcome), OUTSIDE_WORKSPACE);
    assert!(!base.0.join("outside.txt").exists());
}

#[cfg(unix)]
#[test]
fn a_symlink_that_leaves_the_workspace_is_refused() {
    let root = TempWorkspace(workspace());
    let outside = TempWorkspace(workspace());
    std::os::unix::fs::symlink(&outside.0, root.0.join("escape")).expect("symlink");

    let outcome = run(tool(&root.0).execute(&json!({
        "path": "escape/notes.txt",
        "old_text": "a",
        "new_text": "b"
    })))
    .expect("input");

    assert_eq!(failed_code(&outcome), OUTSIDE_WORKSPACE);
    assert!(fs::read_dir(&outside.0).expect("outside").next().is_none());
}

#[test]
fn a_directory_is_not_a_file_and_a_missing_file_is_not_found() {
    let root = TempWorkspace(workspace());
    fs::create_dir(root.0.join("dir")).expect("dir");
    let tool = tool(&root.0);

    let directory = run(tool.execute(&json!({
        "path": "dir",
        "old_text": "a",
        "new_text": "b"
    })))
    .expect("input");
    assert_eq!(failed_code(&directory), IS_DIRECTORY);

    let absent = run(tool.execute(&json!({
        "path": "gone.txt",
        "old_text": "a",
        "new_text": "b"
    })))
    .expect("input");
    assert_eq!(failed_code(&absent), NOT_FOUND);
    assert!(!root.0.join("gone.txt").exists());
}

#[test]
fn an_lf_edit_leaves_crlf_on_untouched_lines() {
    let root = TempWorkspace(workspace());
    fs::write(root.0.join("win.txt"), b"one\r\ntwo\r\nthree\r\n").expect("seed");

    let outcome = run(tool(&root.0).execute(&json!({
        "path": "win.txt",
        "old_text": "one\ntwo\n",
        "new_text": "ONE\nTWO\n"
    })))
    .expect("input");

    match outcome {
        EditOutcome::Edited { matches, text, .. } => {
            assert_eq!(matches, 1);
            assert_eq!(text, "Edited win.txt: replaced 1 occurrence(s), 17 bytes.");
        }
        other => panic!("expected an edit, got {other:?}"),
    }
    assert_eq!(
        fs::read(root.0.join("win.txt")).expect("bytes"),
        b"ONE\r\nTWO\r\nthree\r\n"
    );
}

#[test]
fn a_stale_compare_and_swap_writes_nothing() {
    let root = TempWorkspace(workspace());
    let path = root.0.join("stale.txt");
    fs::write(&path, b"alpha\n").expect("seed");

    let source = StaleSource {
        inner: FsHooks::new(root.0.clone()),
        poison: path.clone(),
        poison_bytes: b"mutated\n".to_vec(),
    };
    let tool = EditTool::new(EditConfig::new(), source, FsHooks::new(root.0.clone()));

    let outcome = run(tool.execute(&json!({
        "path": "stale.txt",
        "old_text": "alpha",
        "new_text": "ALPHA"
    })))
    .expect("input");

    match outcome {
        EditOutcome::Failed { code, text } => {
            assert_eq!(code, STALE);
            assert_eq!(
                text,
                "AGT_EDIT_STALE: file changed on disk after it was read; read it again"
            );
        }
        other => panic!("expected a stale failure, got {other:?}"),
    }
    assert_eq!(fs::read(&path).expect("bytes"), b"mutated\n");
}

#[test]
fn replace_all_rewrites_every_non_overlapping_hit() {
    let root = TempWorkspace(workspace());
    fs::write(root.0.join("log.txt"), b"todo one\ntodo two\ntodo three\n").expect("seed");

    let outcome = run(tool(&root.0).execute(&json!({
        "path": "log.txt",
        "old_text": "todo",
        "new_text": "done",
        "replace_all": true
    })))
    .expect("input");

    match outcome {
        EditOutcome::Edited {
            matches,
            bytes,
            text,
            ..
        } => {
            assert_eq!(matches, 3);
            assert_eq!(bytes, 29);
            assert_eq!(text, "Edited log.txt: replaced 3 occurrence(s), 29 bytes.");
        }
        other => panic!("expected an edit, got {other:?}"),
    }
    assert_eq!(
        fs::read(root.0.join("log.txt")).expect("bytes"),
        b"done one\ndone two\ndone three\n"
    );
}
