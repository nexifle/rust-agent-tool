//! Host hooks for the exec tool.
//!
//! The SDK does not spawn a process, read the environment, canonicalize a
//! directory, or open a spill file. The host supplies those capabilities here.
//! A runner that does not kill the process tree when a foreground call is
//! dropped is wrong.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::async_trait;
use crate::exec::argv::ShellKind;
use crate::exec::capture::Capture;
use crate::exec::classify::CommandClass;
pub use crate::host::HostFailure;

/// Directory the runner must use, or a policy refusal the model should see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdDecision {
    /// Directory the runner must use.
    ///
    /// The host has already canonicalized this path and checked that it is
    /// inside the jail after symlink resolution. The SDK does not inspect it.
    Accept {
        /// Absolute directory the child process uses.
        directory: String,
    },
    /// The model should see a refusal. This is not a hook crash.
    Reject {
        /// Human-readable reason.
        message: String,
    },
}

/// Where the host stored bytes the model window dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillNote {
    /// Host-defined locator, shown to the model. The SDK does not open it.
    pub locator: String,
}

/// Borrowed view of the command a hook may use for its own approval.
///
/// This is not a place to pass environment variables back into the SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandView<'a> {
    /// Raw command text after trim.
    pub command: &'a str,
    /// Classification. `Opaque` does not skip the runner.
    pub class: CommandClass,
    /// Directory from [`CwdDecision::Accept`].
    pub directory: &'a str,
    /// Shell family selected at construction.
    pub shell: ShellKind,
}

/// What the runner must start. This is not the model JSON.
#[derive(Debug)]
pub struct ExecRequest {
    /// Argv from [`crate::exec::argv`]. The first element is the shell executable.
    pub argv: Vec<String>,
    /// Accepted working directory. The runner must not canonicalize it again.
    pub directory: String,
    /// Exact child environment. `None` means the runner decides inheritance.
    ///
    /// The reference runner inherits the parent environment when this is `None`
    /// and uses exactly this map when it is `Some`.
    pub env: Option<Vec<(String, String)>>,
    /// Wall-clock deadline. `None` means no deadline. Cancellation still applies.
    pub timeout: Option<Duration>,
    /// How long the runner may read pipes after the tree has been killed.
    pub drain_timeout: Duration,
    /// Largest pipe read. The runner must not read more than this per chunk.
    pub chunk_bytes: usize,
    /// Cooperative cancel flag. The runner must observe it at least every 50 ms.
    ///
    /// Dropping the foreground future must still kill the tree even if nobody
    /// sets this flag. A host may share the same flag to cancel early.
    pub cancel: Arc<AtomicBool>,
}

/// How a foreground run finished.
///
/// [`ExecStatus::Denied`] means the sandbox refused the spawn. The runner must
/// not start the process again without the sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecStatus {
    /// The process exited. A signal death uses a non-zero code and may name the
    /// signal in `detail` when the runner knows it.
    Exited {
        /// Process exit code.
        code: i32,
        /// Optional signal name or other runner detail.
        detail: Option<String>,
    },
    /// The wall-clock deadline fired and the tree was killed.
    TimedOut,
    /// The cancel flag fired and the tree was killed.
    Cancelled,
    /// The process could not be started.
    SpawnFailed {
        /// Human-readable reason, such as not found or permission denied.
        message: String,
    },
    /// The sandbox denied the spawn. Do not retry unsandboxed.
    Denied {
        /// Human-readable reason.
        message: String,
    },
}

/// A job the host accepted. The SDK does not remember it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedJob {
    /// Host-allocated id.
    pub id: String,
    /// Output the host already has. The SDK truncates it with [`Capture`].
    pub prefix: Vec<u8>,
}

/// Why a job hook could not return a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobError {
    /// No job with that id.
    NotFound,
    /// The hook failed unexpectedly.
    Failed {
        /// Human-readable reason.
        message: String,
    },
}

/// Lifecycle of a host-owned job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobStatus {
    /// The job is still running.
    Running,
    /// The job exited.
    Exited {
        /// Process exit code.
        code: i32,
    },
    /// The host's job deadline fired.
    TimedOut,
    /// The host killed the job.
    Cancelled,
}

/// One look at a job. The host caps what it retains.
///
/// The SDK keeps at most one [`Capture`] window per call and does not store
/// the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobView {
    /// Current lifecycle.
    pub status: JobStatus,
    /// Bytes the host chose to return. The SDK truncates them.
    pub output: Vec<u8>,
    /// Exit code when the job has finished with one.
    pub exit_code: Option<i32>,
}

/// Checks the working directory before a spawn.
#[async_trait]
pub trait ExecCwd: Send + Sync {
    /// `cwd` is `None` when the model omitted it.
    ///
    /// [`CwdDecision::Accept`] means the directory exists, is a directory, and
    /// is inside the host jail after symlink resolution. Checking the string
    /// before resolution is not enough.
    async fn check(&self, cwd: Option<&str>) -> Result<CwdDecision, HostFailure>;
}

/// Builds the child environment. The SDK never reads `std::env`.
#[async_trait]
pub trait ExecEnv: Send + Sync {
    /// Owned map passed to the child. The SDK does not merge the parent environment.
    async fn prepare(
        &self,
        command: &CommandView<'_>,
    ) -> Result<Vec<(String, String)>, HostFailure>;
}

/// Optional copy of bytes the model window dropped.
///
/// Failure is a notice, not a failed command. The SDK does not open a file.
#[async_trait]
pub trait ExecSpill: Send + Sync {
    /// Accepts dropped bytes. The capture window does not retain them, so a
    /// caller that already discarded the middle passes whatever slice it still
    /// holds (often empty) and still returns a [`SpillNote`].
    async fn write(&self, bytes: &[u8]) -> Result<SpillNote, HostFailure>;
}

/// Starts one foreground process and drains both pipes into `capture`.
///
/// The runner closes the child's stdin. On timeout or cancel it kills the
/// process tree, then keeps reading until both pipes reach EOF or
/// [`ExecRequest::drain_timeout`] elapses. Killing and abandoning the pipes
/// deadlocks the capture. A dropped foreground future must kill the tree.
#[async_trait]
pub trait ExecRunner: Send + Sync {
    /// Runs `request` until it exits, times out, or is cancelled.
    async fn run(
        &self,
        request: ExecRequest,
        capture: &mut Capture,
    ) -> Result<ExecStatus, HostFailure>;
}

/// Host-owned process table. Absent means foreground only.
///
/// The SDK does not spawn a detached child and does not assign process ids.
/// A job the host has accepted is the host's process. Dropping the tool call
/// must not kill it.
#[async_trait]
pub trait ExecJobs: Send + Sync {
    /// Detaches `request`. The command timeout is the job deadline, enforced
    /// by the host. The SDK does not start a timer that outlives the call.
    async fn start(&self, request: ExecRequest) -> Result<StartedJob, HostFailure>;

    /// Current window. Must not consume or drop the job.
    async fn read(&self, id: &str) -> Result<JobView, JobError>;

    /// Waits until the job exits or `timeout` elapses. This is the call
    /// timeout, not a new job deadline.
    async fn wait(&self, id: &str, timeout: Duration) -> Result<JobView, JobError>;

    /// Kills the tree and returns the final status.
    async fn kill(&self, id: &str) -> Result<JobView, JobError>;
}
