# rust-agent-tool

[![crates.io](https://img.shields.io/crates/v/rust-agent-tool.svg)](https://crates.io/crates/rust-agent-tool)
[![docs.rs](https://docs.rs/rust-agent-tool/badge.svg)](https://docs.rs/rust-agent-tool)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Agent tool contract with host-injected read, exec, write, and edit tools.

A host describes a tool, calls it, and reports a contract failure through one shared boundary. Filesystem and process access stay on the host: this crate does not call `std::fs`, search `PATH`, read `std::env`, canonicalize paths, or pick an async runtime.

Version 0.1 is not a stable API. Expect breaks before 1.0.

## Install

```toml
[dependencies]
rust-agent-tool = "0.1.0"
```

The optional `runner` feature adds `StdRunner`, a reference process runner:

```toml
rust-agent-tool = { version = "0.1.0", features = ["runner"] }
```

MSRV is 1.99.1 (edition 2024). The crate contains no `unsafe` code.

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
    spec: ToolSpec::new("echo", "Return the text field.", json!({"type": "object"}))
        .expect("valid spec"),
};
assert_eq!(echo.spec().name(), "echo");
```

`ToolSpec::new` rejects an empty name, whitespace in the name, an empty description, and an input schema that is not a JSON object. A host capability is stored on the tool at construction. It is not an argument of `Tool::call`.

`ToolOutput::error_text` is an operational failure the model should see. `Err(ToolError)` is a contract failure the host must handle (`AGT_INPUT_INVALID`, `AGT_SPEC_INVALID`). Operational codes (`AGT_READ_*`, `AGT_EXEC_*`, `AGT_WRITE_*`, `AGT_EDIT_*`) stay in the output text. Published codes are not renamed.

## Tools

| Tool | Host injects | SDK does not |
| --- | --- | --- |
| `ReadTool` | `ReadSource` | Open paths. The host canonicalizes and checks containment before `ReadNode::File`. |
| `ExecTool` | `ExecCwd`, `ExecRunner`, optional job, env, and spill hooks | Spawn a process, unless the `runner` feature is enabled. |
| `WriteTool` | `WriteSink` | Create directories or write bytes. |
| `EditTool` | `EditSource`, `EditSink` | Replace a region. The sink compare-and-swap closes the read-then-write race. |

`path_is_within` compares two already-canonical paths and does not touch the disk. The crate does not list directories, decode or resize images, or count tokens.

`async_trait` is re-exported. Implementors do not need their own dependency on that crate.

Full API: <https://docs.rs/rust-agent-tool>.

## License

Copyright (c) 2026 [Nexifle Lab](https://nexifle.com). Licensed under the [MIT license](LICENSE).
