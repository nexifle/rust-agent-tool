# rust-agent-tool

Shared contract for agent tools. Engineered by [Nexifle Lab](https://nexifle.com).

`rust-agent-tool` lets a Rust host describe a tool, call it, and report a contract failure. Tool implementations and host applications share that boundary so each new tool does not invent its own calling convention.

The crate ships `ReadTool`, `ExecTool`, `WriteTool`, and `EditTool`. Read filesystem I/O is injected through `ReadSource`, so the SDK does not open paths, does not call `std::fs`, and does not canonicalize. The host canonicalizes the path and checks containment before it returns `ReadNode::File`, so a symlink that resolves outside the workspace is `ReadNode::OutsideWorkspace`, not `File`. A command runs only when the host supplies an `ExecRunner`. The SDK builds argv and a capture window; it does not search `PATH`, read `std::env`, or write a spill file. The optional `runner` feature adds `StdRunner`, a reference process runner. A file is written only through `WriteSink`: the SDK does not call `std::fs`, does not create parent directories, and does not canonicalize. A region of an existing file is replaced only through two host hooks: `EditSource` returns the whole file as raw bytes and `EditSink` performs a compare-and-swap write, so the SDK still never touches the filesystem and the host owns resolution, containment, and atomic replacement. The crate still does not list directories, decode or resize images, or count tokens.

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

## Read tool

`ReadTool` reads text and image files through a host-supplied `ReadSource`. Construct it with `ReadTool::new(source, ReadConfig::new())`; the config defaults to 2000 lines, a 48 KiB output budget, and a 5 MiB image limit. The host implements `ReadSource` and decides which paths are allowed. Before it returns `ReadNode::File`, it canonicalizes the path and checks containment; `path_is_within` compares two already-canonical paths and does not touch the disk.

Text windows use a 1-based `offset` and a `limit` in lines, both optional. Images come back as `ImagePart` bytes, not base64, and line windows do not apply to them. An operational read failure is a `ReadOutcome::Failed` rendered as `ToolOutput::error_text` with an `AGT_READ_*` code, so the model can see and recover from it. `Err(ToolError)` is only a contract failure (`AGT_INPUT_INVALID` or `AGT_SPEC_INVALID`).

```rust,ignore
use rust_agent_tool::{async_trait, ReadConfig, ReadNode, ReadSource, ReadTool, SourceFailure};

// The host owns the filesystem; the SDK never opens a path itself.
struct WorkspaceSource;

#[async_trait]
impl ReadSource for WorkspaceSource {
    async fn lookup(&self, _path: &str) -> Result<ReadNode, SourceFailure> {
        // Canonicalize the path, check containment, then return ReadNode::File.
        Ok(ReadNode::Absent)
    }
}

let tool = ReadTool::new(WorkspaceSource, ReadConfig::new()).expect("valid config");
```

## API

| Type | Role |
| --- | --- |
| `ToolSpec` | Name, description, and the JSON object used as the input schema. |
| `CallContext` | Optional call id for one invocation. |
| `Tool` | `spec`, `call`, and `boxed` for `Box<dyn Tool>`. |
| `ToolOutput` | Text returned to the model. `is_error` marks an operational failure. |
| `ToolError` | Contract failure. Display form is `AGT_CODE: message`. |
| `ErrorCode` | Stable code. Match on `as_str` so new codes do not break hosts. |
| `ReadTool` | Tool over a `ReadSource`. `new`, `execute`, and description setters. |
| `ReadConfig` | Line, byte, chunk, and image limits for one read tool. |
| `ReadSource` | Host capability that resolves a path and streams file bytes. |
| `ReadOutcome` | Text, notice, image, or operational failure from one read. |
| `ImagePart` | Media type label and raw image bytes returned beside the text. |
| `path_is_within` | Whether one already-canonical path is inside another. |
| `ExecTool` | Tool over host `ExecCwd`, `ExecRunner`, and optional job, env, and spill hooks. |
| `ExecConfig` | Output, timeout, and chunk limits for one exec tool. |
| `StdRunner` | Reference runner. Only with the `runner` feature. |
| `WriteTool` | Tool over a `WriteSink`. `new`, `execute`, and `call`. |
| `WriteConfig` | Content byte budget, NUL and empty policy, and omission markers. |
| `WriteSink` | Host capability that approves a write, then performs it. |
| `WriteOutcome` | Created or overwrote, or an operational write failure. |
| `EditTool` | Tool over an `EditSource` and an `EditSink`. `new`, `execute`, and `call`. |
| `EditConfig` | File, `old_text`, and fan-out limits, plus the match and newline policy. |
| `EditSource` | Host capability that returns one file as raw bytes. |
| `EditSink` | Host capability that performs a compare-and-swap replacement. |
| `EditOutcome` | Edited region, or an operational edit failure. |

`async_trait` is re-exported. Implementors do not need a direct dependency on that crate.

## Error codes

| Code | When |
| --- | --- |
| `AGT_INPUT_INVALID` | The call input cannot be used. |
| `AGT_SPEC_INVALID` | The tool specification is not usable. |

These are the only `ToolError` codes. Operational read codes (`AGT_READ_*`), exec codes (`AGT_EXEC_NONZERO_EXIT`, `AGT_EXEC_TIMED_OUT`, `AGT_EXEC_CANCELLED`, `AGT_EXEC_CWD_REJECTED`, `AGT_EXEC_SPAWN_FAILED`, `AGT_EXEC_SANDBOX_DENIED`, `AGT_EXEC_JOB_NOT_FOUND`, `AGT_EXEC_HOST_FAILED`), write codes (`AGT_WRITE_*`), and edit codes (`AGT_EDIT_*`) appear in `ToolOutput` text instead, so the model can see them. Published codes are not renamed. A new failure gets a new code.

## Compatibility

Rust 1.99.1 or newer is required. The crate uses edition 2024 and contains no `unsafe` code.

## License

Copyright (c) 2026 [Nexifle Lab](https://nexifle.com). Licensed under the [MIT license](LICENSE).
