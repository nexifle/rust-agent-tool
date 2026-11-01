//! Configuration, argv, and classification for the exec tool.
//!
//! This module does not spawn processes. It holds the pieces a host needs
//! before a spawn: [`ExecConfig`] and [`ExecCeilings`] bound one call,
//! [`ShellSpec`] names the shell family and [`argv`] turns a command into the
//! argument vector, and [`classify`] labels a command as [`CommandClass`]
//! without deciding whether to run it.
//!
//! The SDK never discovers a shell, never reads the environment, and never
//! writes a file. The host resolves the shell executable and passes it to
//! [`ShellSpec::new`]. A command that cannot be proven literal is
//! [`CommandClass::Opaque`], and the host decides what an opaque command may do.
//!
//! Nothing in the default build performs I/O or runs a process. The `runner`
//! feature adds a reference runner that does. [`Capture`] counts bytes that
//! fall out of the head/tail window and does not read a pipe or write a file.
//!
//! A host that already canonicalized its jail can accept only that directory:
//!
//! ```
//! use rust_agent_tool::async_trait;
//! use rust_agent_tool::exec::{CwdDecision, ExecCwd, HostFailure, ShellKind, ShellSpec};
//!
//! struct Jail {
//!     root: String,
//! }
//!
//! #[async_trait]
//! impl ExecCwd for Jail {
//!     async fn check(&self, cwd: Option<&str>) -> Result<CwdDecision, HostFailure> {
//!         let requested = cwd.unwrap_or(self.root.as_str());
//!         if requested == self.root {
//!             Ok(CwdDecision::Accept {
//!                 directory: self.root.clone(),
//!             })
//!         } else {
//!             Ok(CwdDecision::Reject {
//!                 message: "directory is outside the jail".to_string(),
//!             })
//!         }
//!     }
//! }
//!
//! let shell = ShellSpec::new("/bin/sh", ShellKind::Posix).expect("shell");
//! assert_eq!(shell.kind(), ShellKind::Posix);
//! let _jail = Jail {
//!     root: "/work".to_string(),
//! };
//! ```

mod argv;
mod capture;
mod classify;
mod config;
mod host;
mod input;
mod outcome;
#[cfg(feature = "runner")]
mod runner;
mod tool;

pub use argv::{ShellKind, ShellSpec, argv};
pub use capture::Capture;
pub use classify::{CommandClass, MAX_COMMAND_BYTES, MAX_SEGMENTS, classify};
pub use config::{ExecCeilings, ExecConfig};
pub use host::{
    CommandView, CwdDecision, ExecCwd, ExecEnv, ExecJobs, ExecRequest, ExecRunner, ExecSpill,
    ExecStatus, HostFailure, JobError, JobStatus, JobView, SpillNote, StartedJob,
};
pub use input::{ExecInput, JobAction, default_description, input_schema};
pub use outcome::{
    CANCELLED, CWD_REJECTED, ExecOutcome, HOST_FAILED, JOB_NOT_FOUND, NONZERO_EXIT, SANDBOX_DENIED,
    SPAWN_FAILED, TIMED_OUT,
};
#[cfg(feature = "runner")]
pub use runner::StdRunner;
pub use tool::{ExecTool, ExecToolBuilder};
