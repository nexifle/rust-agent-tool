//! The write tool, its host hook, and its configuration.
//!
//! The write tool creates a file or replaces its whole content through a
//! host-injected sink. This module publishes the configuration the tool reads,
//! the content policy that runs before any hook, the host hook itself, and the
//! tool that ties them together: [`WriteConfig`] holds the content byte budget,
//! the NUL and empty content policy, the omission-marker list, and the tool name
//! and descriptions. [`WriteCeilings`] bounds what a config setter may store.
//! [`WriteSink`] decides whether a write may proceed and performs it.
//! [`WriteTool`] runs one call and reports a [`WriteOutcome`].
//!
//! The SDK never opens, creates, or truncates a path. The host owns every
//! filesystem operation, including creating missing parents and canonicalizing
//! the path before it checks containment. The model-facing path stays the path
//! the model sent.
//!
//! Line endings are applied only when the permit asks for them.
//! [`Newline::AsIs`] hands the original string back, while [`Newline::Lf`] and
//! [`Newline::CrLf`] convert after the permit is returned.
//!
//! The operational codes are exported here and from the crate root. They appear
//! in model-facing error text, not in [`crate::ErrorCode`]:
//!
//! - [`TOO_LARGE`] (`AGT_WRITE_TOO_LARGE`)
//! - [`BINARY`] (`AGT_WRITE_BINARY`)
//! - [`EMPTY`] (`AGT_WRITE_EMPTY`)
//! - [`INCOMPLETE`] (`AGT_WRITE_INCOMPLETE`)
//! - [`NOT_FOUND`] (`AGT_WRITE_NOT_FOUND`)
//! - [`IS_DIRECTORY`] (`AGT_WRITE_IS_DIRECTORY`)
//! - [`FORBIDDEN`] (`AGT_WRITE_FORBIDDEN`)
//! - [`OUTSIDE_WORKSPACE`] (`AGT_WRITE_OUTSIDE_WORKSPACE`)
//! - [`REJECTED`] (`AGT_WRITE_REJECTED`)
//! - [`SINK_FAILED`] (`AGT_WRITE_SINK_FAILED`)

mod check;
mod config;
mod input;
mod sink;
mod tool;

pub use check::{BINARY, EMPTY, INCOMPLETE, TOO_LARGE};
pub use config::{WriteCeilings, WriteConfig};
pub use sink::{
    FORBIDDEN, IS_DIRECTORY, NOT_FOUND, OUTSIDE_WORKSPACE, PrepareError, REJECTED, SINK_FAILED,
    SinkFailure, WritePermit, WriteRefusal, WriteSink,
};
pub use tool::{WriteOutcome, WriteTool};

pub use crate::text::Newline;
