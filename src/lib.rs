//! Shared contract for agent tools.
//!
//! A host uses this crate to describe a tool, call it, and report a contract
//! failure. A host capability is added with the tool that needs it, and is
//! injected when that tool is constructed. It is not part of [`Tool::call`].
//!
//! The crate ships [`ReadTool`], [`ExecTool`], and [`WriteTool`]. Read
//! filesystem I/O is injected through [`ReadSource`]: the SDK does not open
//! paths, does not call `std::fs`, and does not canonicalize. The host
//! canonicalizes the path and checks containment before it returns
//! [`ReadNode::File`], so a symlink that resolves outside the workspace is
//! [`ReadNode::OutsideWorkspace`], not `File`. [`path_is_within`] compares two
//! already-canonical paths and does not touch the disk.
//!
//! A command runs only when the host supplies an [`ExecRunner`]: the SDK builds
//! argv and a capture window, and the runner spawns the process. The SDK does
//! not search `PATH`, does not read `std::env`, and does not write a spill file.
//!
//! A file is written only through [`WriteSink`]: the SDK does not call
//! `std::fs`, does not open, create, or truncate a path, does not create parent
//! directories, and does not canonicalize. The host approves each write in
//! [`WriteSink::prepare`] and performs it in [`WriteSink::write`]. The SDK keeps
//! no session memory of earlier writes and holds no path lock.
//!
//! The crate does not list directories, decode or resize images, or count
//! tokens. An [`Err`] carrying a [`ToolError`] is only a contract failure
//! ([`ErrorCode::InputInvalid`] or [`ErrorCode::SpecInvalid`]). Operational read
//! codes (`AGT_READ_*`), exec codes (`AGT_EXEC_*`), and write codes
//! (`AGT_WRITE_*`) appear in [`ToolOutput`] text, not in [`ErrorCode`].

//!
//! The next example defines a tool without a read source; [`ReadTool`] is the
//! read tool.
//!
//! ```
//! use rust_agent_tool::{async_trait, CallContext, Tool, ToolOutput, ToolSpec};
//! use serde_json::{json, Value};
//!
//! struct Echo {
//!     spec: ToolSpec,
//! }
//!
//! #[async_trait]
//! impl Tool for Echo {
//!     fn spec(&self) -> &ToolSpec {
//!         &self.spec
//!     }
//!
//!     async fn call(
//!         &self,
//!         _ctx: &CallContext,
//!         input: Value,
//!     ) -> Result<ToolOutput, rust_agent_tool::ToolError> {
//!         let text = input.get("text").and_then(Value::as_str).unwrap_or("");
//!         Ok(ToolOutput::text(text))
//!     }
//! }
//!
//! let echo = Echo {
//!     spec: ToolSpec::new("echo", "Return the text field.", json!({"type": "object"})).unwrap(),
//! };
//! let tools: Vec<Box<dyn Tool>> = vec![echo.boxed()];
//! assert_eq!(tools[0].spec().name(), "echo");
//! ```

mod error;
pub mod exec;
mod path;
pub mod read;
mod text;
mod tool;
pub mod write;

pub use async_trait::async_trait;
pub use error::{ErrorCode, ToolError};
#[cfg(feature = "runner")]
pub use exec::StdRunner;
pub use exec::{
    CANCELLED, CWD_REJECTED, Capture, CommandClass, CommandView, CwdDecision, ExecCeilings,
    ExecConfig, ExecCwd, ExecEnv, ExecInput, ExecJobs, ExecOutcome, ExecRequest, ExecRunner,
    ExecSpill, ExecStatus, ExecTool, ExecToolBuilder, HOST_FAILED, HostFailure, JOB_NOT_FOUND,
    JobAction, JobError, JobStatus, JobView, NONZERO_EXIT, SANDBOX_DENIED, SPAWN_FAILED, ShellKind,
    ShellSpec, SpillNote, StartedJob, TIMED_OUT,
};
pub use path::path_is_within;
pub use read::{
    DEFAULT_LIMIT_DESCRIPTION, DEFAULT_OFFSET_DESCRIPTION, DEFAULT_PATH_DESCRIPTION,
    DEFAULT_TOOL_DESCRIPTION, FileBody, FileMeta, ReadCeilings, ReadConfig, ReadNode, ReadOutcome,
    ReadSource, ReadTool, SourceFailure,
};
pub use tool::{CallContext, ImagePart, Tool, ToolOutput, ToolSpec};
pub use write::{
    BINARY, EMPTY, FORBIDDEN, INCOMPLETE, IS_DIRECTORY, NOT_FOUND, Newline, OUTSIDE_WORKSPACE,
    PrepareError, REJECTED, SINK_FAILED, SinkFailure, TOO_LARGE, WriteCeilings, WriteConfig,
    WriteOutcome, WritePermit, WriteRefusal, WriteSink, WriteTool,
};
