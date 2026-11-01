//! The read tool, its budget, and the host source it reads through.
//!
//! [`ReadTool`] is generic over a [`ReadSource`]. The host injects the source
//! when the tool is built. The SDK never opens a file: the host canonicalizes
//! the path and checks containment before it returns [`ReadNode::File`], so a
//! symlink that resolves outside the workspace is
//! [`ReadNode::OutsideWorkspace`]. [`ReadConfig`] bounds lines, bytes, chunks,
//! and image size, and [`ReadCeilings`] bounds what a config setter may store.
//!
//! A read either returns text, an image, a notice, or an operational failure
//! the model should see. Contract failures, such as unusable model input, stay
//! [`crate::ToolError`].

mod classify;
mod config;
mod input;
mod source;
mod tool;
mod window;

pub use config::{ReadCeilings, ReadConfig};
pub use input::{
    DEFAULT_LIMIT_DESCRIPTION, DEFAULT_OFFSET_DESCRIPTION, DEFAULT_PATH_DESCRIPTION,
    DEFAULT_TOOL_DESCRIPTION,
};
pub use source::{FileBody, FileMeta, ReadNode, ReadSource, SourceFailure};
pub use tool::{ReadOutcome, ReadTool};
