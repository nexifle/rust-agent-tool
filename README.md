# rust-agent-tool

Shared contract for agent tools.

Another Rust program depends on this crate to describe a tool, call it, and report a contract failure. The crate does not read files, write files, or run commands, and it does not ship a read, edit, write, or execution tool.

## Requirements

Rust 1.99.1 or newer. That is the stable release this foundation was checked against. It includes the 1.99.1 fix for FFI `bool` returns, which could otherwise be observed as a value other than 0 or 1.

`rust-toolchain.toml` tracks the stable channel. Bump `rust-version` in `Cargo.toml` when a newer stable includes a security fix that this crate must require.

## Contract

| Type | Role |
| --- | --- |
| `ToolSpec` | Name, description, and a JSON object used as the input schema. |
| `CallContext` | Optional call id for one invocation. Not an I/O port. |
| `Tool` | `spec` and `call`. `boxed` erases the concrete tool into `Box<dyn Tool>`. |
| `ToolOutput` | Text for the model. `is_error` means the operation failed and the model should see that. |
| `ToolError` | Contract failure. Display form is `AGT_CODE: message`. |

`Tool::call` takes the context and the model input. Host capabilities are stored on the tool value when a future tool needs them. They are not added to this trait in advance.

## Error codes

| Code | When |
| --- | --- |
| `AGT_INPUT_INVALID` | The call input cannot be used. |
| `AGT_SPEC_INVALID` | The tool specification is not usable. |

Codes are stable. Add a new code. Do not rename a published code.

## Example

```rust
use rust_agent_tool::{async_trait, CallContext, Tool, ToolOutput, ToolSpec};
use serde_json::{json, Value};

struct Echo {
    spec: ToolSpec,
}

#[async_trait]
impl Tool for Echo {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(
        &self,
        _ctx: &CallContext,
        input: Value,
    ) -> Result<ToolOutput, rust_agent_tool::ToolError> {
        let text = input.get("text").and_then(Value::as_str).unwrap_or("");
        Ok(ToolOutput::text(text))
    }
}

let echo = Echo {
    spec: ToolSpec::new("echo", "Return the text field.", json!({"type": "object"})).unwrap(),
};
let tools: Vec<Box<dyn Tool>> = vec![echo.boxed()];
assert_eq!(tools[0].spec().name(), "echo");
```
