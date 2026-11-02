//! The edit tool, its host hooks, and its configuration.
//!
//! The edit tool replaces one exact region of an existing file through two
//! host-injected hooks. This module publishes the configuration the tool reads,
//! the checks that run before and after the read hook, the hooks themselves,
//! the matching and splicing engine, and the tool that ties them together.
//!
//! The tool does not create a file. A missing path is
//! [`NOT_FOUND`] and points the model at the write tool.
//!
//! The SDK never opens, creates, truncates, or canonicalizes a path.
//! [`EditSource::open`] returns the whole file as raw bytes and [`EditSink::apply`]
//! performs a compare-and-swap write, so the model-facing path stays the path the
//! model sent and the host owns resolution, containment, atomic replacement, and
//! every permission decision. The SDK keeps no session memory of earlier reads
//! and takes no path lock.
//!
//! Matching runs on an LF view of the file because the read tool hands the model
//! LF line endings, and every hit is mapped back to an original byte offset, so
//! the splice copies untouched bytes exactly. A CRLF file, a mixed-ending file,
//! and a file with a UTF-8 BOM all survive an edit byte for byte outside the
//! replaced region. Exact matching is always on; the lenient tier runs on
//! [`MatchMode::Lenient`] and above, and the fuzzy tier on [`MatchMode::Fuzzy`]
//! only, each only when every cheaper tier missed.
//!
//! The operational codes are exported here. They appear in [`EditOutcome`] text,
//! not in [`crate::ErrorCode`]:
//!
//! - [`EMPTY_OLD`] (`AGT_EDIT_EMPTY_OLD`)
//! - [`NO_CHANGE`] (`AGT_EDIT_NO_CHANGE`)
//! - [`TOO_LARGE`] (`AGT_EDIT_TOO_LARGE`)
//! - [`BINARY`] (`AGT_EDIT_BINARY`)
//! - [`NOT_TEXT`] (`AGT_EDIT_NOT_TEXT`)
//! - [`NOT_FOUND`] (`AGT_EDIT_NOT_FOUND`)
//! - [`IS_DIRECTORY`] (`AGT_EDIT_IS_DIRECTORY`)
//! - [`FORBIDDEN`] (`AGT_EDIT_FORBIDDEN`)
//! - [`OUTSIDE_WORKSPACE`] (`AGT_EDIT_OUTSIDE_WORKSPACE`)
//! - [`REJECTED`] (`AGT_EDIT_REJECTED`)
//! - [`NO_MATCH`] (`AGT_EDIT_NO_MATCH`)
//! - [`NOT_UNIQUE`] (`AGT_EDIT_NOT_UNIQUE`)
//! - [`TOO_MANY`] (`AGT_EDIT_TOO_MANY`)
//! - [`STALE`] (`AGT_EDIT_STALE`)
//! - [`SOURCE_FAILED`] (`AGT_EDIT_SOURCE_FAILED`)
//! - [`SINK_FAILED`] (`AGT_EDIT_SINK_FAILED`)
//!
//! Only these are re-exported from the crate root: the tool, its config and
//! ceilings, both hooks with their payload types, the refusal and outcome types,
//! and the two policy enums. The codes, [`OpenError`], and [`ApplyError`] stay in
//! this module, so every edit code has one import path:
//! `use rust_agent_tool::edit::NOT_UNIQUE;`.

mod check;
mod config;
mod input;
mod matcher;
mod sink;
mod source;
mod splice;
mod text_unfold;
mod tool;

pub use crate::host::HostFailure;
pub use check::{BINARY, EMPTY_OLD, NO_CHANGE, NOT_TEXT, TOO_LARGE};
pub use config::{EditCeilings, EditConfig, MatchMode, NewlinePolicy};
pub use matcher::MatchTier;
pub use sink::{
    ApplyError, EditPermit, EditRefusal, EditSink, FORBIDDEN, IS_DIRECTORY, OUTSIDE_WORKSPACE,
    REJECTED, SINK_FAILED, STALE,
};
pub use source::{EditFile, EditSource, NOT_FOUND, OpenError};
pub use tool::{EditOutcome, EditTool, NO_MATCH, NOT_UNIQUE, SOURCE_FAILED, TOO_MANY};
