//! Shared contract for agent tools.
//!
//! A host uses this crate to describe a tool, call it, and report a contract
//! failure. The crate does not read files, write files, or run commands.
//! A host capability is added with the tool that needs it, and is injected
//! when that tool is constructed. It is not part of [`Tool::call`].
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
mod tool;

pub use async_trait::async_trait;
pub use error::{ErrorCode, ToolError};
pub use tool::{CallContext, Tool, ToolOutput, ToolSpec};
