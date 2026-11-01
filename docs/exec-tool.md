# Exec tool plan

Implementation plan for the shell execution tool in `rust-agent-tool`: a production-ready command tool for coding agents.

This document is self-contained. An implementer who did not see the research session should follow it as written. The crate version is 0.1 and is not a stable API, but published error-code strings must not be renamed once they ship.

## What this crate already is

`rust-agent-tool` is a library SDK. A host application depends on it and registers tools with its own agent loop. The crate does not select an async runtime.

The shared call contract lives in `src/tool.rs`. The read tool lives in `src/read/`. Filesystem I/O is a host hook (`ReadSource`) injected when the tool is constructed. The exec tool follows that split.

The exec tool must not call `std::fs`, must not open or create paths, must not canonicalize, and must not write a spill file. Working-directory checks, environment construction, and optional output spill are host hooks. Spawning a process is also a host hook in the core crate, because process-group setup needs `unsafe` and this crate forbids `unsafe_code`. An optional `runner` feature ships a reference runner so a host is not required to write one.

Relevant types in `src/`:

| Type | Role |
| --- | --- |
| `Tool` | `spec()`, `call(&CallContext, Value)`, `boxed()`. `Send + Sync`. |
| `ToolSpec` | Name, description, JSON Schema object. Built with `ToolSpec::new`. |
| `CallContext` | Optional call id. Not an I/O port. |
| `ToolOutput` | Text the model sees. `is_error` marks an operational failure. |
| `ToolError` | Contract failure the host must handle. Display form is `AGT_CODE: message`. |
| `ErrorCode` | Stable code. Match on `as_str` because the enum is `#[non_exhaustive]`. |
| `ReadConfig` / `ReadCeilings` | Pattern for budgets: setters reject out-of-range values, they do not clamp. |

Rules already enforced by the crate:

- Host capabilities are stored on the tool value at construction. They are not arguments of `Tool::call`.
- `Err(ToolError)` means the call itself is invalid. The host handles it.
- `Ok(ToolOutput::error_text(...))` means the operation failed in a way the model should see.
- `Ok(ToolOutput::text(...))` is a successful result. A non-zero exit is not this case. See the outcome table below.
- Error codes look like `AGT_<DOMAIN>_<REASON>`. Messages are human-readable. Display is `AGT_CODE: message`.
- Public items need rustdoc (`missing_docs = "deny"`). `unsafe` is forbidden. Clippy `all` is denied.
- Code and docs in the repository are English.

Do not invent a second calling convention. `ExecTool` implements `Tool`.

## Research constraints

These decisions come from Codex, OpenCode, Pi, oh-my-pi, Gemini CLI, Grok Build, and Zed's sandbox and tool-permission docs. They are requirements, not optional inspiration.

### Copy

- One model-facing string `command`. The model does not choose the shell executable and does not pass an environment map.
- Shell family decides argv, not the operating system alone:
  - POSIX (`bash`, `zsh`, `sh`): `[shell, "-c", command]`
  - PowerShell (`pwsh`, `powershell`): `[shell, "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", command]`
  - `cmd`: `[shell, "/c", command]`
- A command that cannot be classified as a flat literal or a flat compound is `Opaque`. Opaque must not be treated as a single safe command. Auto-allow is the host's decision, and it may use this classification. The SDK does not allow or deny.
- Compound commands (`&&`, `||`, `;`, `|`) are not approved because their first segment would be safe. The host sees every segment.
- Timeout is a wall clock for the call, not an inactivity timer. Gemini resets its timer on output; that lets a slow flood run forever. Do not copy that.
- Cancellation and timeout kill the process tree, not only the direct child. Unix: process group. Windows: a Job Object. `kill(pid)` is not enough.
- Stdin of the child is closed. Commands such as `rg` with no path otherwise wait for input until the timeout.
- Stdout and stderr are both drained for the whole lifetime of the child. Stop appending to the model window when the budget is full, but keep reading, or the child blocks on a full pipe.
- Truncation is a signal. The model sees how many bytes were omitted. A partial capture is not the full output.
- Merge stdout and stderr in read order. Document that the merge is not a total order. Do not allocate two full copies.
- A non-zero exit is a finished command. Return it as an operational error result (`is_error`) with the exit code in the text. Do not return `Err(ToolError)`. OpenCode treats non-zero as success metadata; this crate follows Pi and oh-my-pi and marks it `is_error`, because the model must notice the failure.
- Timeout, cancel, spawn failure, and a rejected working directory are operational results, not contract errors.
- Empty command text is a contract error.
- `timeout_ms: 0` from the model is rejected unless the host explicitly enables unbounded timeouts on the config. Silent "no timeout" is how a call pins a thread.
- Byte budgets, not character budgets and not tokens. Gemini's 16 MB cap is a JavaScript string length. A tokenizer does not belong in this crate.
- Cut on a UTF-8 boundary. Pi discards a split multibyte tail. Do that. Do not insert U+FFFD for a partial code unit at the cut.
- Default window is head plus tail, not tail only and not head only. Build failures and test failures are at the end; preambles are at the start. oh-my-pi and Codex unified exec both keep both ends. The default inline budget matches the read tool: 48 KiB.
- No persistent shell in this release. Codex snapshots, oh-my-pi's brush session, and Grok's fd 3/4 replay all keep environment and aliases across calls. That is host state. One call is one process. `cd` does not survive. The result tells the model that the next call is a new process and repeats the working directory that was used.

### Do not copy

- Sandbox implementation (Seatbelt, Landlock, Bubblewrap, restricted tokens). The host wraps the runner. The SDK must not retry unsandboxed after a sandbox denial. Codex does that only under specific approval modes; doing it here would undo the host.
- Approval prompts and policy files. The host decides. The SDK only classifies.
- tree-sitter, a PowerShell AST, or a hand-rolled full shell parser. They are large, and a parser bug becomes an allow-list bypass. Ship a small classifier that fails closed to `Opaque`.
- A process table, a UI, or a PTY inside the SDK. Background work uses the same `ExecTool` through an optional `ExecJobs` hook. The SDK does not store processes. A host that does not pass the hook gets foreground only. Named services and PTY stay out.
- Git Bash as the Windows default. Grok documents that MSYS rewrites arguments that start with `/` (`msbuild /t:Build`, `cl /nologo`). The host picks the executable. The SDK only documents the preference: PowerShell 7, then Windows PowerShell, then `cmd`. Git Bash is explicit only.
- Shell discovery inside the SDK (`$SHELL`, `which`, `getpwuid`, `/etc/shells`). That reads the filesystem and the environment. The host passes a `ShellSpec` with an executable path it already resolved.
- Reading `std::env` to build the child environment. Codex and Grok filter secrets. The host's `ExecEnv` returns the map. If the hook is absent, the reference runner inherits the parent environment and the docs say so. The core tool never reads the environment itself.
- An `env` field on the model input. oh-my-pi refuses it. `LD_PRELOAD`, `BASH_ENV`, and `PATH` are approval bypasses.
- Spill-to-disk inside the SDK. OpenCode, Pi, Grok, and oh-my-pi write the full output to a file. That is filesystem policy. Offer an optional `ExecSpill` hook. With no hook, the result reports omitted byte counts and does not invent a path.
- Output-inactivity timeouts, login shells (`bash -lc` sources rc files and can run arbitrary code), and `ExecutionPolicy Bypass` as a default. Pi's PowerShell tool uses Bypass. This crate uses `-NoProfile -NonInteractive` only. The host can wrap the executable if it needs another policy.
- A second tool named `powershell`. One tool, one `ShellSpec`. The model writes PowerShell only when the host selected that family. The description text must say which family is active.

### What the SDK allocates

The SDK's memory for one call is bounded by configuration, independent of what the command prints:

- one head buffer, at most `head_bytes`
- one tail ring, at most `max_output_bytes - head_bytes`
- one read chunk, `chunk_bytes`, reused
- the command string and the rendered result

Do not buffer the full merged output and then truncate. Drop the middle while streaming.

The child process, its memory, and its output volume are outside the tool. The tool must not make that worse by retaining a second copy.

## Architecture

```
model JSON
    │
    ▼
parse input ──────────── empty command, bad types, timeout out of range ──► Err(ToolError)
    │
    ▼
classify command ─────── Literal | Compound { segments } | Opaque
    │                     (no spawn; host may refuse before the runner)
    ▼
ExecCwd::check ───────── Reject ──► Ok(error ToolOutput) AGT_EXEC_CWD_REJECTED
    │                   Failure ──► Ok(error ToolOutput) AGT_EXEC_HOST_FAILED
    ▼
ExecEnv::prepare ────── optional; Failure ──► AGT_EXEC_HOST_FAILED
    │
    ▼
build argv from ShellSpec + command
    │
    ▼
ExecRunner::run ──────── streams stdout and stderr into the capture window
    │                   spawn / timeout / cancel / sandbox denied
    ▼
optional ExecSpill for bytes the window dropped
    │
    ▼
Ok(rendered text) ────── non-zero exit, timeout, and cancel set is_error
```

`ExecTool::call` is a thin adapter. `ExecTool::execute` returns `ExecOutcome`. Hosts that need the exit code, the classification, or the omitted counts use `execute`. `Box<dyn Tool>` uses `call`.

The runner is responsible for starting the process, draining both pipes until EOF, applying the wall-clock deadline, and killing the tree. The SDK owns the capture window and the text rendered for the model. The runner pushes bytes into a `Capture` the SDK provides. The runner does not format the model-facing string.

Foreground is the default. A dropped foreground future kills the tree. Background is the same tool with an optional `ExecJobs` hook, described below. The SDK still does not keep a process table.

### Files

| Path | Responsibility |
| --- | --- |
| `src/exec/mod.rs` | Module docs and re-exports. |
| `src/exec/config.rs` | `ExecConfig`, `ExecCeilings`. No I/O. |
| `src/exec/input.rs` | JSON Schema and input parsing. No I/O. |
| `src/exec/classify.rs` | `Literal`, `Compound`, `Opaque`. No I/O. |
| `src/exec/argv.rs` | `ShellKind` to argv. No I/O. |
| `src/exec/capture.rs` | Head/tail window, UTF-8 cuts, omitted counts. No I/O. |
| `src/exec/host.rs` | `ExecCwd`, `ExecEnv`, `ExecSpill`, `ExecRunner`, `ExecJobs`, failure types. |
| `src/exec/outcome.rs` | `ExecOutcome` and the model-facing render. |
| `src/exec/tool.rs` | `ExecTool`. |
| `src/exec/runner.rs` | Compiled only with feature `runner`. Reference `StdRunner`. |
| `src/lib.rs` | Re-export the public exec types. |

Do not put operational exec codes on `ErrorCode`. Read does not. Contract failures stay `AGT_INPUT_INVALID` and `AGT_SPEC_INVALID`. Operational codes are `&'static str` on the outcome, same as `AGT_READ_*`.

## Host hooks

```rust
#[async_trait]
pub trait ExecCwd: Send + Sync {
    /// `cwd` is `None` when the model omitted it.
    /// The host uses its own default directory in that case.
    async fn check(&self, cwd: Option<&str>) -> Result<CwdDecision, HostFailure>;
}

pub enum CwdDecision {
    /// Directory the runner must use. Already canonical, already allowed.
    /// The SDK does not inspect this string.
    Accept { directory: String },
    /// The model should see a refusal. Not a hook crash.
    Reject { message: String },
}

#[async_trait]
pub trait ExecEnv: Send + Sync {
    /// Owned map passed to the child. The SDK does not merge `std::env`.
    async fn prepare(&self, command: &CommandView) -> Result<Vec<(String, String)>, HostFailure>;
}

#[async_trait]
pub trait ExecSpill: Send + Sync {
    /// Bytes dropped from the model window. The SDK does not open a file.
    /// Failure here is reported as a notice, not a failed command.
    async fn write(&self, bytes: &[u8]) -> Result<SpillNote, HostFailure>;
}

pub struct SpillNote {
    /// Host-defined locator, shown to the model. Often a path. Not opened by the SDK.
    pub locator: String,
}

#[async_trait]
pub trait ExecRunner: Send + Sync {
    async fn run(&self, request: ExecRequest, capture: &mut Capture) -> Result<ExecStatus, HostFailure>;
}

pub struct HostFailure {
    pub message: String,
}
```

`CommandView` is borrowed data the host may use for its own approval: the raw command, the classification, the accepted directory, and the shell kind. It is not a place to smuggle environment variables back into the SDK.

`ExecRequest` contains the argv, the accepted directory, the environment from `ExecEnv` (or empty, see below), the deadline, and a cancellation flag. It does not contain the model JSON.

Contract the host must honor, documented on the traits:

- `CwdDecision::Accept` means the directory exists, is a directory, and is inside the host's jail after symlink resolution. Checking the string before resolution is not enough. The same bug class as the Zed read advisory applies here: `cd` through a symlink escapes a string prefix.
- `Reject` is for a policy refusal. `HostFailure` is for an unexpected hook error. Do not collapse them.
- The runner closes the child's stdin.
- The runner kills the process tree on timeout and on cancellation, then waits until both pipes reach EOF, up to a short drain bound in `ExecConfig`. Killing and then abandoning the pipes deadlocks the capture.
- The runner reports sandbox denial as `ExecStatus::Denied`. It does not spawn again without the sandbox.
- `ExecEnv` is optional on `ExecTool`. When it is absent, `ExecRequest` carries no map, and the runner decides inheritance. The reference runner inherits. A host that needs a secret filter must pass `ExecEnv`.
- `ExecSpill` is optional. When it is absent, omitted bytes are counted and discarded.

`ShellSpec` is plain data, set at construction:

```rust
pub struct ShellSpec {
    pub executable: String,
    pub kind: ShellKind,
}

pub enum ShellKind {
    Posix,
    PowerShell,
    Cmd,
}
```

The SDK does not search `PATH`. An empty `executable` fails `ExecTool` construction with `AGT_SPEC_INVALID`.

## Classification

The classifier is a quote-aware scan, not an evaluator. It never expands variables, never follows aliases, and never treats a string as safe to run.

| Class | Meaning |
| --- | --- |
| `Literal` | One command word sequence. No unquoted shell operator. Quotes are balanced. |
| `Compound` | Two or more literal segments joined only by unquoted `&&`, `\|\|`, `;`, or `\|`. Each segment is itself literal. |
| `Opaque` | Anything else, including a scan that gets lost. |

`Opaque` includes, whether or not a smarter parser could allow them:

- `$(...)`, backticks, process substitution, `${...}`
- redirections and heredocs
- parentheses, braces, `&` backgrounding, newlines used as separators
- an unterminated quote or an unfinished escape
- more than 32 segments, or a command longer than 64 KiB

Those limits are denial-of-service caps for the scan, not shell limits. Over the cap, return `Opaque`. Do not error the call: the host may still want to run an opaque command after a human approves it.

PowerShell and `cmd` use the same function only as a conservative scan. It does not understand PowerShell's call operator (`&`) or `cmd` escape rules. For those kinds, any unquoted `&`, `|`, or newline is `Opaque`, and the scan must not report `Compound` unless the operator set for that kind is implemented and tested. Until that set exists, `ShellKind::PowerShell` and `ShellKind::Cmd` always classify as `Opaque` except for a single line with no `&`, `|`, `<`, `>`, or newline, which is `Literal`. Do not pretend `;` splits PowerShell the way it splits bash.

The classifier result is available to `ExecEnv` and is included in `ExecOutcome`. The SDK does not skip `ExecRunner` because the class is `Opaque`.

## Capture

`Capture` is the only buffer.

1. Read a chunk of at most `chunk_bytes` from whichever pipe is ready. The runner does this. `Capture::push` only accepts bytes.
2. Fill `head` until `head_bytes`.
3. Further bytes enter a ring of `tail_bytes = max_output_bytes - head_bytes`. Overflow of the ring increments `omitted_bytes` and, if a spill hook is installed, is handed to the tool layer after the process ends (the hook is async; `Capture` itself stays synchronous and stores at most one extra `tail_bytes` of dropped bytes to spill, or spills incrementally if the hook is called by the tool between pushes). Prefer incremental spill from the tool layer so the dropped set is not stored twice. The simplest correct approach: `Capture` records only the window and the omitted count. Spill is best-effort of the dropped stream only when the runner calls a synchronous `FnMut(&[u8])` the tool installed. Do not retain dropped bytes in the SDK.
4. When rendering, if a cut lands inside a UTF-8 sequence, move the cut back to the previous char boundary and count the skipped bytes as omitted.
5. A line longer than `max_line_bytes` is cut inside the window with a `…` and the rest of that line still counts toward omitted bytes once the window is full. While the window is not full, skip storing past `max_line_bytes` for that line and count them omitted. This stops one line from using the entire budget.
6. The rendered body is `head`, then a one-line marker if `omitted_bytes > 0`, then `tail`. The marker is `[omitted {n} bytes]`. If spill returned a locator, append ` full output: {locator}`.

Do not strip ANSI in the library. Stripping needs a state machine across chunks and hides diagnostic color from hosts that want it. Document that the host runner may strip before `push`. The reference runner does not strip.

## Configuration

Same rules as `ReadConfig`. `ExecConfig::new` uses the defaults. Each setter returns `Result<Self, ToolError>` with `AGT_SPEC_INVALID` when the value is below the floor or above the active ceiling. `with_ceilings` replaces the ceilings and revalidates. Do not clamp.

| Knob | Floor | Default | Default ceiling |
| --- | --- | --- | --- |
| `max_output_bytes` | 320 | 48 KiB | 1 MiB |
| `head_bytes` | 0 | 16 KiB | must be `<= max_output_bytes` |
| `max_line_bytes` | 32 | 2_000 | 64 KiB |
| `chunk_bytes` | 256 | 8 KiB | 64 KiB |
| `default_timeout` | 1 ms | 30 s | 10 min |
| `max_timeout` | `default_timeout` | 10 min | 24 h |
| `drain_timeout` | 1 ms | 2 s | 30 s |
| `allow_unbounded_timeout` | — | false | — |

`head_bytes == 0` means tail-only. `head_bytes == max_output_bytes` means head-only. Both are legal. The default keeps both ends.

The model field `timeout_ms` is optional. Omitted means `default_timeout`. A value above `max_timeout` is `AGT_INPUT_INVALID`, not a clamp. `0` is `AGT_INPUT_INVALID` unless `allow_unbounded_timeout` is true, in which case the runner receives no deadline. The reference runner still honors cancellation when there is no deadline.

`drain_timeout` is how long the runner may block in pipe reads after the tree has been killed. It is not the command timeout.

## Model input

JSON Schema, required `command` only:

| Field | Type | Rule |
| --- | --- | --- |
| `command` | string | Required. Trimmed. Empty after trim is `AGT_INPUT_INVALID`. No maximum other than the 64 KiB classify cap; over that, the command still runs if the host's runner accepts it, and the class is `Opaque`. |
| `cwd` | string | Optional. Not interpreted. Passed to `ExecCwd`. |
| `timeout_ms` | integer | Optional. See configuration. JSON floats are `AGT_INPUT_INVALID`. |

Unknown fields are ignored, matching typical model output. Duplicate meaning is not an error.

There is no `description`, `env`, or `pty` field. A description is a UI concern. PTY is out of scope. `background` and `job_id` exist only when the host passed `ExecJobs`. Without that hook they are unknown fields and are ignored, so a model that sends them still runs the command in the foreground. With the hook, the schema lists them and the tool description says how to poll and kill.

The tool description is host-supplied at construction, with a default that names `ShellKind` and states that the process is not a persistent shell. `ToolSpec::new` still rejects an empty description.

Default tool name: `exec`.

## Outcomes

`execute` returns `Result<ExecOutcome, ToolError>`. `ToolError` is only bad input or a bug in construction that escaped (construction should already have failed).

| Outcome | `is_error` | Code |
| --- | --- | --- |
| Exit 0 | no | none |
| Exit non-zero | yes | `AGT_EXEC_NONZERO_EXIT` |
| Timed out, tree killed | yes | `AGT_EXEC_TIMED_OUT` |
| Cancelled, tree killed | yes | `AGT_EXEC_CANCELLED` |
| Working directory rejected | yes | `AGT_EXEC_CWD_REJECTED` |
| Spawn failed (not found, permission) | yes | `AGT_EXEC_SPAWN_FAILED` |
| Runner reported sandbox denial | yes | `AGT_EXEC_SANDBOX_DENIED` |
| Unknown background job id | yes | `AGT_EXEC_JOB_NOT_FOUND` |
| Hook or runner failed unexpectedly | yes | `AGT_EXEC_HOST_FAILED` |

Rendered text, in this order, each section omitted when it does not apply:

1. `Working directory: {directory}`
2. The capture body, or `(no output)`.
3. The omission marker, if any.
4. One status line: `Exit code: N`, `Timed out after {ms} ms`, `Cancelled`, or the host failure message. The status line always includes the code in the form `AGT_EXEC_...: ...` for error outcomes, so logs can grep it.

`ExecOutcome` also exposes `exit_code: Option<i32>`, `truncated: bool`, `omitted_bytes: u64`, `class: CommandClass`, and `directory: String` for hosts that do not want to parse the text.

A signal death is `AGT_EXEC_NONZERO_EXIT` with a message that names the signal when the runner knows it. Do not invent a signal on Windows when the runner only has an exit code. The reference runner maps a timeout to `AGT_EXEC_TIMED_OUT` rather than to the raw signal.

## Background

One tool. The host does not write a second tool. Background is off unless the host passes `ExecJobs` at construction.

The SDK does not spawn a detached child, does not assign process ids, and does not remember jobs. `ExecJobs` is the whole process table. The reference runner has no job support. A host that wants background implements the hook and, inside it, uses the same `ExecRunner` or its own supervisor. Kill-on-drop applies only to a foreground `ExecRunner::run`. A job the host has accepted is the host's process. Dropping the tool call must not kill it.

When the hook is present, input gains:

| Field | Type | Rule |
| --- | --- | --- |
| `background` | bool | Optional. `true` asks to detach. Ignored when `job_id` is set. |
| `job_id` | string | Optional. Selects an existing job. `command` is then optional. |
| `job_action` | string | Optional with `job_id`. `read` (default), `wait`, or `kill`. Anything else is `AGT_INPUT_INVALID`. |

Call shapes:

1. No `job_id`, `background` omitted or false. Foreground path. Unchanged.
2. No `job_id`, `background: true`. The tool builds the same argv, cwd, and env as a foreground call, then calls `ExecJobs::start`. The hook returns a `JobId` the host allocated, plus whatever prefix output it already has. The outcome is success text `Backgrounded as job {id}` unless start fails. The command timeout is the job's deadline, counted from start, enforced by the host. The SDK does not start a timer that outlives the call.
3. `job_id` set. `ExecJobs::read`, `wait`, or `kill`. `read` returns the current window without consuming the job. `wait` waits until the job exits or until `timeout_ms` of this call (the call timeout, not a new job deadline). `kill` kills the tree and returns the final status. An unknown id is `AGT_EXEC_JOB_NOT_FOUND`.

`ExecJobs` methods:

```rust
#[async_trait]
pub trait ExecJobs: Send + Sync {
    async fn start(&self, request: ExecRequest) -> Result<StartedJob, HostFailure>;
    async fn read(&self, id: &str) -> Result<JobView, JobError>;
    async fn wait(&self, id: &str, timeout: Duration) -> Result<JobView, JobError>;
    async fn kill(&self, id: &str) -> Result<JobView, JobError>;
}
```

`JobError` is `NotFound` or `Failed { message }`. `NotFound` renders `AGT_EXEC_JOB_NOT_FOUND`. `Failed` renders `AGT_EXEC_HOST_FAILED`. `JobView` carries status (`Running`, `Exited`, `TimedOut`, `Cancelled`), the capture window the host already truncated or a byte slice the SDK truncates with `Capture`, and the exit code when finished. The host may stream into its own store. The SDK applies the same head/tail render if the host passes raw bytes. The host must cap what it retains. The SDK will not retain more than one window per call.

Do not auto-background when a foreground timeout fires. Timeout still kills. Auto-background hides a stuck command. A host that wants that policy calls `start` itself from a custom runner. It is not the default.

Schema and description are built from whether `ExecJobs` is installed, so a host that did not opt in does not advertise background to the model.

## Reference runner

Feature name: `runner`. Default features stay empty so the core crate does not pull process code.

Dependency: [`process-wrap`](https://docs.rs/process-wrap) (Watchexec). It creates a process group on Unix and a Job Object on Windows behind its own `unsafe`. This crate still forbids `unsafe`.

The reference runner:

- uses `std::process::Command` through `process-wrap`
- sets `stdin` to null, `stdout` and `stderr` to piped
- sets the current directory to `CwdDecision::Accept`'s string and does not canonicalize again
- spawns the pipe readers on `std::thread`, not on a Tokio runtime
- polls the child with `try_wait` and a short sleep, or `wait` on a thread, so `run` stays async without a runtime dependency (`async_trait` plus a blocking section)
- on deadline or cancel: signal the tree to terminate, wait `drain_timeout` while still reading, then kill
- does not inherit a caller-supplied `ExecEnv` incorrectly: if the request contains a map, use exactly that map; if it does not, inherit

Do not add `tokio`, `async-process`, `nix`, `rustix`, `portable-pty`, `tree-sitter`, or `tree-sitter-bash`.

`which` and shell discovery stay out. Tests that need a shell use `sh` on Unix and `cmd` on Windows via the path the test supplies (`/bin/sh`, `cmd.exe`). Skip a test when that executable is missing. Do not download a shell.

## Cancellation

`CallContext` has no cancellation token today. Do not overload it.

`ExecRunner::run` takes the deadline inside `ExecRequest`. Cooperative cancel is a `Cancellation` value on the request, built by the host and stored on `ExecTool` only if the host wants one shared flag. Prefer a per-call token created inside `execute` and dropped when `execute` returns, plus `ExecRequest::cancel` as `Arc<AtomicBool>` the host can also set if it passed an outer flag in.

The minimum for v1: `execute` sets the flag when the timeout fires inside the SDK's own timer, and the runner watches that flag. The host may also set the same flag to cancel. Document that dropping the future does not cancel the child unless the runner selects on the flag. The reference runner checks the flag at least every 50 ms. A dropped future must still kill the tree: the reference runner ties the child's life to a guard that kills on drop. That is the production requirement. A host runner that skips the guard is wrong, and the trait docs say so.

## Testing

Tests do not need a real process until the `runner` feature.

Without a process:

- argv for all three shell kinds
- classifier: quotes hiding `&&`, unterminated quotes, substitution, redirect, segment cap, PowerShell does not become `Compound`
- capture: head/tail marker, UTF-8 cut, long line, omitted count, zero-output render
- config floors and ceilings reject
- input: empty command, float timeout, `timeout_ms: 0` rejected and accepted when enabled
- without `ExecJobs`, `background` and `job_id` do not change the foreground path
- with a fake `ExecJobs`: start returns the host id, read does not drop the job, kill returns a finished view, unknown id is `AGT_EXEC_JOB_NOT_FOUND`
- a fake runner: non-zero exit, timeout status, cwd reject, host failure, spill note included only when the hook returns one

With `runner`, ignored or cfg-gated on CI that has the shell:

- `echo` round trip on `/bin/sh` or `cmd /c`
- a command that writes more than the window does not grow memory with the output (assert the omitted count and that the process exits; do not assert RSS)
- a child that ignores a soft kill still dies because the tree is killed before `drain_timeout` ends
- stdin is closed: a command that reads stdin exits instead of hanging until the command timeout, within a bound tighter than `default_timeout` if the test sets a long timeout
- dropping the run future does not leave the child (process-group or job is gone)

No test reads the repository filesystem except the crate's own `Cargo.toml` is unnecessary. Do not use the read tool's workspace jail in exec tests. A fake `ExecCwd` is enough for unit tests.

## Implementation order

1. `ExecConfig`, `ShellSpec`, argv, classifier, and their tests.
2. `Capture` and render tests.
3. Host traits, including `ExecJobs`, and `ExecTool::execute` against a fake runner. Wire `call`. Foreground stays the default when the job hook is absent.
4. Operational codes and the outcome text. Export from `lib.rs`. Update the crate docs that say the crate does not run commands: it runs them only through a host runner.
5. Feature `runner` and the process tests above.
6. One example in the crate docs showing a host `ExecCwd` that accepts only a directory it already canonicalized, and a `ShellSpec` for POSIX.

Out of scope for this change, even if a host will want them next: sandbox backends, approval storage, PTY, persistent shell sessions, shell snapshots, direnv, auto-background on timeout, and a separate PowerShell tool. Background jobs are in scope only as the `ExecJobs` hook above.
