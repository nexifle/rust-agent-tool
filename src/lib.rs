//! Shared contract for agent tools.
//!
//! A host uses this crate to describe a tool, call it, and report a contract
//! failure. A host capability is added with the tool that needs it, and is
//! injected when that tool is constructed. It is not part of [`Tool::call`].
//!
//! The crate ships [`ReadTool`], whose filesystem I/O is injected through
//! [`ReadSource`]. The SDK does not open paths, does not call `std::fs`, and
//! does not canonicalize. The host canonicalizes the path and checks
//! containment before it returns [`ReadNode::File`], so a symlink that resolves
//! outside the workspace is [`ReadNode::OutsideWorkspace`], not `File`.
//! [`path_is_within`] compares two already-canonical paths and does not touch
//! the disk.
//!
//! The crate does not write files, run commands, list directories, decode or
//! resize images, or count tokens. An [`Err`] carrying a [`ToolError`] is only
//! a contract failure ([`ErrorCode::InputInvalid`] or
//! [`ErrorCode::SpecInvalid`]). Operational read codes (`AGT_READ_*`) appear in
//! [`ToolOutput`] text.
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
mod path;
pub mod read;
mod tool;

pub use async_trait::async_trait;
pub use error::{ErrorCode, ToolError};
pub use path::path_is_within;
pub use read::{
    DEFAULT_LIMIT_DESCRIPTION, DEFAULT_OFFSET_DESCRIPTION, DEFAULT_PATH_DESCRIPTION,
    DEFAULT_TOOL_DESCRIPTION, FileBody, FileMeta, ReadCeilings, ReadConfig, ReadNode, ReadOutcome,
    ReadSource, ReadTool, SourceFailure,
};
pub use tool::{CallContext, ImagePart, Tool, ToolOutput, ToolSpec};
