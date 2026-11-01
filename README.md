# rust-agent-tool

Shared contract for agent tools. Engineered by [Nexifle Lab](https://nexifle.com).

`rust-agent-tool` lets a Rust host describe a tool, call it, and report a contract failure. Tool implementations and host applications share that boundary so each new tool does not invent its own calling convention.

The crate does not read files, write files, run commands, or decide which paths a host may touch. It also does not include read, edit, write, or execution tools.

Version 0.1 is not a stable API. Expect breaks before 1.0.

## Installation

This crate is not published to crates.io. Depend on a local checkout:

```toml
[dependencies]
rust-agent-tool = { path = "../rust-agent-tool" }
```

Do not add a `git` dependency until this repository has a public remote. There is no install URL to copy yet.

The crate does not select an async runtime. The host brings its own.

## Usage

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
    spec: ToolSpec::new(
        "echo",
        "Return the text field.",
        json!({"type": "object"}),
    )
    .expect("valid spec"),
};
let tools: Vec<Box<dyn Tool>> = vec![echo.boxed()];
assert_eq!(tools[0].spec().name(), "echo");
```

`ToolSpec::new` rejects an empty name, a name that contains whitespace, an empty description, and an input schema that is not a JSON object. `Tool::call` receives the call context and the model input. A host capability, when a tool needs one, is stored on the tool value at construction. It is not an argument of `call`.

`ToolOutput::error_text` is for an operational failure the model should see. `Err(ToolError)` is for a contract failure the host must handle.

## API

| Type | Role |
| --- | --- |
| `ToolSpec` | Name, description, and the JSON object used as the input schema. |
| `CallContext` | Optional call id for one invocation. |
| `Tool` | `spec`, `call`, and `boxed` for `Box<dyn Tool>`. |
| `ToolOutput` | Text returned to the model. `is_error` marks an operational failure. |
| `ToolError` | Contract failure. Display form is `AGT_CODE: message`. |
| `ErrorCode` | Stable code. Match on `as_str` so new codes do not break hosts. |

`async_trait` is re-exported. Implementors do not need a direct dependency on that crate.

## Error codes

| Code | When |
| --- | --- |
| `AGT_INPUT_INVALID` | The call input cannot be used. |
| `AGT_SPEC_INVALID` | The tool specification is not usable. |

Published codes are not renamed. A new failure gets a new code.

## Compatibility

Rust 1.99.1 or newer is required. The crate uses edition 2024 and contains no `unsafe` code.

## License

Copyright (c) 2026 [Nexifle Lab](https://nexifle.com). Licensed under the [MIT license](LICENSE).
