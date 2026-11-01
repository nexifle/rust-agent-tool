//! Host read source contract.
//!
//! The SDK never opens a file. It asks the host for a node through
//! [`ReadSource::lookup`], then reads bytes through [`FileBody::read_at`].
//! Tests in this module use an in-memory source that does not touch the disk.

use async_trait::async_trait;
use std::error::Error;
use std::fmt;
#[cfg(test)]
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// Host capability that resolves one model path and streams file bytes.
///
/// [`ReadSource::lookup`] does not need to return file bytes. It answers with
/// one [`ReadNode`] value, and only [`ReadNode::File`] carries a body.
///
/// `path` is the string the model sent. The host decides whether to accept
/// relative paths. The SDK does not join it to a root and does not "fix" it.
///
/// Before returning [`ReadNode::File`], the host canonicalizes the path and
/// checks containment. A symlink that resolves outside the workspace is
/// [`ReadNode::OutsideWorkspace`], not [`ReadNode::File`]. The host may use
/// `path_is_within` on the two canonical paths. Checking only the string
/// before symlink resolution is not enough.
///
/// [`FileMeta::byte_len`] is the real file size. The image size check uses it
/// after the magic sample, not before.
///
/// [`FileBody::read_at`] must be safe to call more than once. The window
/// scanner reads sequential chunks.
///
/// If `at < byte_len` and `max > 0`, [`FileBody::read_at`] must return at least
/// one byte or a [`SourceFailure`]. An empty buffer in that position means the
/// body is stuck. The SDK does not retry it; it reports the failure and stops.
/// A short non-empty buffer is only legal at EOF (`at >= byte_len`) or when
/// fewer than `max` bytes remain.
///
/// [`ReadNode::Absent`], [`ReadNode::Directory`], [`ReadNode::OutsideWorkspace`],
/// and [`ReadNode::Forbidden`] must not be followed by a body read. The SDK
/// will not call [`FileBody::read_at`] for those variants.
///
/// Permission and not-found stay distinct. Collapsing them hides a jail bug
/// behind a missing file.
///
/// [`SourceFailure`] is for an unexpected hook failure, such as a disk error or
/// a timeout. It is not the answer for the four expected variants above.
#[async_trait]
pub trait ReadSource: Send + Sync {
    /// Resolves one model-supplied path. Bytes are not read here.
    ///
    /// A missing path is [`ReadNode::Absent`]. A permission problem is
    /// [`ReadNode::Forbidden`]. Only an unexpected host failure is a
    /// [`SourceFailure`]. See the trait documentation for the host rules.
    async fn lookup(&self, path: &str) -> Result<ReadNode, SourceFailure>;
}

/// Result of one [`ReadSource::lookup`] call.
///
/// The four non-file variants are expected answers, not hook failures, and
/// must not be followed by a body read.
pub enum ReadNode {
    /// Nothing exists at the path, or the host cannot see it.
    Absent,
    /// The path names a directory, not a regular file.
    Directory,
    /// The canonical path resolves outside the workspace.
    OutsideWorkspace,
    /// The host refused access to the path.
    Forbidden,
    /// A regular file the host allows the SDK to read.
    File(FileMeta),
}

/// Size and body for one [`ReadNode::File`] answer.
///
/// This is plain data, not an open handle and not something the SDK closes. The
/// host reports the real size in [`FileMeta::byte_len`] and serves bytes through
/// [`FileMeta::body`].
pub struct FileMeta {
    /// Real file size in bytes. See the [`ReadSource`] documentation.
    pub byte_len: u64,
    /// Byte reader for this file.
    pub body: Box<dyn FileBody>,
}

impl FileMeta {
    /// Builds metadata from a size and any [`FileBody`] implementation.
    ///
    /// The body is boxed. `byte_len` is not checked against the body. A body
    /// that serves fewer bytes than promised is broken, and the SDK treats that
    /// as a [`SourceFailure`].
    #[must_use]
    pub fn new(byte_len: u64, body: impl FileBody + 'static) -> Self {
        Self {
            byte_len,
            body: Box::new(body),
        }
    }
}

/// Byte reader for one file.
///
/// A body may stream from disk, memory, or a remote filesystem. It must be
/// safe to call [`FileBody::read_at`] more than once.
#[async_trait]
pub trait FileBody: Send + Sync {
    /// Read at most `max` bytes starting at absolute byte offset `at`.
    ///
    /// Reading past EOF returns a short buffer, not an error. A short non-empty
    /// buffer is legal when fewer than `max` bytes remain before `byte_len`. A
    /// short buffer earlier than that, including an empty one while `at <
    /// byte_len` and `max > 0`, is a broken body. The SDK does not retry it.
    /// See the [`ReadSource`] documentation for the full contract.
    async fn read_at(&self, at: u64, max: usize) -> Result<Vec<u8>, SourceFailure>;
}

/// Unexpected failure from a host hook, such as a disk error or a timeout.
///
/// This type is not the answer for [`ReadNode::Absent`],
/// [`ReadNode::Directory`], [`ReadNode::OutsideWorkspace`], or
/// [`ReadNode::Forbidden`]; those are expected variants. The message is
/// human-readable and carries no `AGT_` code, because the tool layer adds the
/// code later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFailure {
    /// Human-readable failure text without a code prefix.
    pub message: String,
}

impl SourceFailure {
    /// Builds a failure from `message`.
    ///
    /// Surrounding whitespace is trimmed. A blank message becomes
    /// `missing source failure message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        let message = message.trim();
        Self {
            message: if message.is_empty() {
                "missing source failure message".to_string()
            } else {
                message.to_string()
            },
        }
    }
}

impl fmt::Display for SourceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for SourceFailure {}

/// In-memory [`ReadSource`] for tests. It does not touch the filesystem.
///
/// Nodes are keyed by the exact path string. A lookup of a path that was never
/// inserted returns [`ReadNode::Absent`], because the model path was not in the
/// map; that is not a [`SourceFailure`].
///
/// Every [`FileBody::read_at`] call made through a body from this source is
/// recorded, so tests can assert the ranges the SDK read. Bodies share one call
/// log and one failure slot with the source, so a failure set with
/// [`MemorySource::fail_reads_with`] also applies to bodies looked up earlier.
///
/// A lookup failure set with [`MemorySource::fail_lookup_with`] is stored
/// unchanged and never clears itself: every later lookup fails until a new
/// message replaces it.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct MemorySource {
    nodes: Mutex<HashMap<String, StoredNode>>,
    read_calls: Arc<Mutex<Vec<(u64, usize)>>>,
    read_failure: Arc<Mutex<Option<String>>>,
    lookup_failure: Mutex<Option<String>>,
}

/// One stored answer, with file bytes kept so every lookup can build a body.
#[cfg(test)]
#[derive(Debug, Clone)]
enum StoredNode {
    Absent,
    Directory,
    OutsideWorkspace,
    Forbidden,
    File { byte_len: u64, bytes: Arc<Vec<u8>> },
}

#[cfg(test)]
impl MemorySource {
    /// An empty source. Every path is [`ReadNode::Absent`] until inserted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts an [`ReadNode::Absent`] answer for `path`.
    pub fn insert_absent(&self, path: impl Into<String>) {
        self.insert(path.into(), StoredNode::Absent);
    }

    /// Inserts a [`ReadNode::Directory`] answer for `path`.
    pub fn insert_directory(&self, path: impl Into<String>) {
        self.insert(path.into(), StoredNode::Directory);
    }

    /// Inserts a [`ReadNode::Forbidden`] answer for `path`.
    pub fn insert_forbidden(&self, path: impl Into<String>) {
        self.insert(path.into(), StoredNode::Forbidden);
    }

    /// Inserts a [`ReadNode::OutsideWorkspace`] answer for `path`.
    pub fn insert_outside(&self, path: impl Into<String>) {
        self.insert(path.into(), StoredNode::OutsideWorkspace);
    }

    /// Inserts a readable file for `path` that serves `bytes`.
    ///
    /// The reported [`FileMeta::byte_len`] is `bytes.len()`.
    pub fn insert_file(&self, path: impl Into<String>, bytes: Vec<u8>) {
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let node = StoredNode::File {
            byte_len,
            bytes: Arc::new(bytes),
        };
        self.insert(path.into(), node);
    }

    /// Inserts a file for `path` that serves `bytes` but reports `byte_len`.
    ///
    /// Use this to model an oversized file without allocating its full size.
    /// A body that cannot serve the promised bytes is a broken body, which is
    /// exactly what a stuck-read test wants.
    pub(crate) fn insert_file_with_len(
        &self,
        path: impl Into<String>,
        bytes: Vec<u8>,
        byte_len: u64,
    ) {
        let node = StoredNode::File {
            byte_len,
            bytes: Arc::new(bytes),
        };
        self.insert(path.into(), node);
    }

    /// Makes every later [`ReadSource::lookup`] return a [`SourceFailure`]
    /// with `message`, until a new message replaces it.
    ///
    /// The failure is not cleared after one lookup.
    pub(crate) fn fail_lookup_with(&self, message: impl Into<String>) {
        let message = message.into();
        *lock(&self.lookup_failure) = Some(message);
    }

    /// Ranges passed to [`FileBody::read_at`], in call order.
    #[must_use]
    pub fn read_calls(&self) -> Vec<(u64, usize)> {
        lock(&self.read_calls).clone()
    }

    /// Makes every later [`FileBody::read_at`] call return a [`SourceFailure`]
    /// with `message`, until [`MemorySource::clear_read_failures`] is called.
    ///
    /// Bodies looked up before this call share the failure slot, so they fail
    /// too.
    pub fn fail_reads_with(&self, message: impl Into<String>) {
        let message = message.into();
        *lock(&self.read_failure) = Some(message);
    }

    /// Clears the failure set by [`MemorySource::fail_reads_with`].
    pub fn clear_read_failures(&self) {
        *lock(&self.read_failure) = None;
    }

    /// Stores one node under `path`, replacing any previous node.
    fn insert(&self, path: String, node: StoredNode) {
        lock(&self.nodes).insert(path, node);
    }
}

#[cfg(test)]
#[async_trait]
impl ReadSource for MemorySource {
    async fn lookup(&self, path: &str) -> Result<ReadNode, SourceFailure> {
        if let Some(message) = lock(&self.lookup_failure).clone() {
            return Err(SourceFailure::new(message));
        }

        let stored = lock(&self.nodes).get(path).cloned();
        Ok(match stored {
            None | Some(StoredNode::Absent) => ReadNode::Absent,
            Some(StoredNode::Directory) => ReadNode::Directory,
            Some(StoredNode::OutsideWorkspace) => ReadNode::OutsideWorkspace,
            Some(StoredNode::Forbidden) => ReadNode::Forbidden,
            Some(StoredNode::File { byte_len, bytes }) => ReadNode::File(FileMeta {
                byte_len,
                body: Box::new(MemoryBody {
                    bytes,
                    read_calls: Arc::clone(&self.read_calls),
                    read_failure: Arc::clone(&self.read_failure),
                }),
            }),
        })
    }
}

/// Body created by [`MemorySource::lookup`] for an inserted file.
#[cfg(test)]
#[derive(Debug)]
struct MemoryBody {
    bytes: Arc<Vec<u8>>,
    read_calls: Arc<Mutex<Vec<(u64, usize)>>>,
    read_failure: Arc<Mutex<Option<String>>>,
}

#[cfg(test)]
#[async_trait]
impl FileBody for MemoryBody {
    async fn read_at(&self, at: u64, max: usize) -> Result<Vec<u8>, SourceFailure> {
        lock(&self.read_calls).push((at, max));

        if max == 0 {
            return Ok(Vec::new());
        }

        let failure = lock(&self.read_failure).clone();
        if let Some(message) = failure {
            return Err(SourceFailure::new(message));
        }

        let len = self.bytes.len();
        let Ok(start) = usize::try_from(at) else {
            return Ok(Vec::new());
        };
        if start >= len {
            return Ok(Vec::new());
        }

        let end = start.saturating_add(max).min(len);
        Ok(self.bytes[start..end].to_vec())
    }
}

/// Locks `mutex`, recovering the value if a previous holder panicked.
#[cfg(test)]
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::{FileBody, FileMeta, MemorySource, ReadNode, ReadSource, SourceFailure};
    use async_trait::async_trait;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn ready<F>(future: F) -> F::Output
    where
        F: Future,
    {
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test future should not wait"),
        }
    }

    fn lookup(source: &MemorySource, path: &str) -> ReadNode {
        ready(source.lookup(path)).expect("lookup should not fail")
    }

    fn file(source: &MemorySource, path: &str) -> FileMeta {
        match lookup(source, path) {
            ReadNode::File(meta) => meta,
            _ => panic!("expected a file node for {path}"),
        }
    }

    fn read(meta: &FileMeta, at: u64, max: usize) -> Vec<u8> {
        ready(meta.body.read_at(at, max)).expect("read should not fail")
    }

    #[test]
    fn unknown_paths_are_absent() {
        let source = MemorySource::new();

        assert!(matches!(
            lookup(&source, "never/inserted.txt"),
            ReadNode::Absent
        ));
        assert!(source.read_calls().is_empty());
    }

    #[test]
    fn non_file_nodes_are_answered_without_a_read() {
        let source = MemorySource::new();
        source.insert_absent("gone.txt");
        source.insert_directory("dir");
        source.insert_forbidden("secret.txt");
        source.insert_outside("../escape.txt");

        assert!(matches!(lookup(&source, "gone.txt"), ReadNode::Absent));
        assert!(matches!(lookup(&source, "dir"), ReadNode::Directory));
        assert!(matches!(lookup(&source, "secret.txt"), ReadNode::Forbidden));
        assert!(matches!(
            lookup(&source, "../escape.txt"),
            ReadNode::OutsideWorkspace
        ));
        assert!(source.read_calls().is_empty());
    }

    #[test]
    fn file_lookup_reports_the_size_without_reading() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());

        let meta = file(&source, "hello.txt");
        assert_eq!(meta.byte_len, 5);
        assert!(source.read_calls().is_empty());
    }

    #[test]
    fn a_read_serves_the_requested_range_and_records_it() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());
        let meta = file(&source, "hello.txt");

        assert_eq!(read(&meta, 0, 4).as_slice(), b"hell");
        assert_eq!(source.read_calls(), vec![(0, 4)]);
    }

    #[test]
    fn a_read_past_eof_is_empty_and_not_an_error() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());
        let meta = file(&source, "hello.txt");

        assert!(read(&meta, 5, 4).is_empty());
        assert!(read(&meta, 99, 4).is_empty());
    }

    #[test]
    fn a_read_at_the_last_byte_returns_it() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());
        let meta = file(&source, "hello.txt");

        assert_eq!(read(&meta, 4, 4).as_slice(), b"o");
        assert_eq!(source.read_calls(), vec![(4, 4)]);
    }

    #[test]
    fn a_body_is_repeatable() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());
        let meta = file(&source, "hello.txt");

        assert_eq!(read(&meta, 1, 2).as_slice(), b"el");
        assert_eq!(read(&meta, 1, 2).as_slice(), b"el");
        assert_eq!(source.read_calls(), vec![(1, 2), (1, 2)]);
    }

    #[test]
    fn a_zero_max_read_is_empty_and_recorded() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());
        let meta = file(&source, "hello.txt");

        assert!(read(&meta, 0, 0).is_empty());
        assert_eq!(source.read_calls(), vec![(0, 0)]);
    }

    #[test]
    fn read_failures_can_be_injected_and_cleared() {
        let source = MemorySource::new();
        source.insert_file("hello.txt", b"hello".to_vec());
        let meta = file(&source, "hello.txt");

        source.fail_reads_with("disk");
        let failure = ready(meta.body.read_at(0, 2)).expect_err("injected failure");
        assert_eq!(failure.to_string(), "disk");
        assert_eq!(failure.message, "disk");

        source.clear_read_failures();
        assert_eq!(read(&meta, 2, 2).as_slice(), b"ll");
        assert_eq!(source.read_calls(), vec![(0, 2), (2, 2)]);
    }

    /// A body that needs no filesystem, used to test [`FileMeta::new`].
    struct StaticBody(&'static [u8]);

    #[async_trait]
    impl FileBody for StaticBody {
        async fn read_at(&self, at: u64, max: usize) -> Result<Vec<u8>, SourceFailure> {
            let at = usize::try_from(at).unwrap_or(self.0.len());
            let rest = self.0.get(at..).unwrap_or(&[]);
            Ok(rest.get(..max).unwrap_or(rest).to_vec())
        }
    }

    #[test]
    fn file_meta_can_be_built_without_a_filesystem() {
        let meta = FileMeta::new(5, StaticBody(b"hello"));

        assert_eq!(meta.byte_len, 5);
        assert_eq!(read(&meta, 1, 2).as_slice(), b"el");
    }

    #[test]
    fn source_failure_trims_and_falls_back() {
        let trimmed = SourceFailure::new("  disk error  ");
        assert_eq!(trimmed.message, "disk error");
        assert_eq!(trimmed.to_string(), "disk error");

        let blank = SourceFailure::new("   ");
        assert_eq!(blank.to_string(), "missing source failure message");
    }

    #[test]
    fn source_failure_is_a_std_error() {
        let failure = SourceFailure::new("timeout");
        let error: &dyn std::error::Error = &failure;

        assert_eq!(error.to_string(), "timeout");
        assert!(std::error::Error::source(error).is_none());
    }
}
