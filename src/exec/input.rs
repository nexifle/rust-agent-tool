//! Model input for the exec tool.
//!
//! Unknown fields are ignored. `background` and `job_id` change the call only
//! when the host installed [`crate::exec::ExecJobs`].

use std::time::Duration;

use serde_json::{Value, json};

use crate::ToolError;
use crate::exec::argv::ShellKind;
use crate::exec::config::ExecConfig;

/// What to do with an existing job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobAction {
    /// Return the current window without consuming the job.
    Read,
    /// Wait until the job exits or the call timeout elapses.
    Wait,
    /// Kill the tree and return the final status.
    Kill,
}

/// Parsed model input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecInput {
    /// Trimmed command. `None` only when a `job_id` was selected.
    pub command: Option<String>,
    /// Working directory string, not interpreted here.
    pub cwd: Option<String>,
    /// Call deadline. `None` is an unbounded timeout the host explicitly allowed.
    pub timeout: Option<Duration>,
    /// Whether the model asked to detach. Ignored when `job_id` is set.
    pub background: bool,
    /// Existing job, when selected.
    pub job_id: Option<String>,
    /// Action for `job_id`. Defaults to [`JobAction::Read`].
    pub job_action: JobAction,
}

/// JSON Schema for the model input.
///
/// `jobs` adds `background`, `job_id`, and `job_action`. Without that hook the
/// schema does not advertise them, and the parser ignores them.
#[must_use]
pub fn input_schema(jobs: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "command".to_string(),
        json!({"type": "string", "description": "Shell command text. The host chooses the shell."}),
    );
    properties.insert(
        "cwd".to_string(),
        json!({"type": "string", "description": "Working directory. The host checks it."}),
    );
    properties.insert(
        "timeout_ms".to_string(),
        json!({"type": "integer", "minimum": 0, "description": "Wall-clock limit in milliseconds."}),
    );
    let mut required = vec!["command".to_string()];
    if jobs {
        properties.insert(
            "background".to_string(),
            json!({"type": "boolean", "description": "Detach the command. Ignored when job_id is set."}),
        );
        properties.insert(
            "job_id".to_string(),
            json!({"type": "string", "description": "Existing job to read, wait, or kill."}),
        );
        properties.insert(
            "job_action".to_string(),
            json!({
                "type": "string",
                "enum": ["read", "wait", "kill"],
                "description": "What to do with job_id. Defaults to read."
            }),
        );
        required.clear();
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": true
    })
}

/// Default tool description. Names the shell family and states that the
/// process is not a persistent shell.
#[must_use]
pub fn default_description(kind: ShellKind, jobs: bool) -> String {
    let family = match kind {
        ShellKind::Posix => "POSIX (bash, zsh, or sh)",
        ShellKind::PowerShell => "PowerShell",
        ShellKind::Cmd => "cmd.exe",
    };
    let mut text = format!(
        "Run one {family} command. Each call is a new process: cd and shell variables do not survive. The host chooses the executable."
    );
    if jobs {
        text.push_str(
            " Set background to true to detach and receive a job id. Call again with job_id to read, wait, or kill that job. job_action is read, wait, or kill.",
        );
    }
    text
}

/// Parses model JSON.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::InputInvalid`] when `command` is missing or
/// blank, `timeout_ms` is not an integer, `timeout_ms` is `0` while unbounded
/// timeouts are off, `timeout_ms` exceeds the configured maximum, or
/// `job_action` is not `read`, `wait`, or `kill`.
pub fn parse(
    input: &Value,
    config: &ExecConfig,
    jobs_enabled: bool,
) -> Result<ExecInput, ToolError> {
    let root = input
        .as_object()
        .ok_or_else(|| ToolError::input_invalid("exec input must be a JSON object"))?;
    let job_id = if jobs_enabled {
        optional_string(root.get("job_id"), "job_id")?
    } else {
        None
    };
    let command = match root.get("command") {
        None if job_id.is_some() => None,
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(ToolError::input_invalid("command must not be empty"));
            }
            Some(trimmed.to_string())
        }
        Some(_) => {
            return Err(ToolError::input_invalid("command must be a string"));
        }
        None => {
            return Err(ToolError::input_invalid("command is required"));
        }
    };
    let cwd = optional_string(root.get("cwd"), "cwd")?;
    let timeout = timeout_of(root.get("timeout_ms"), config)?;
    let background = if jobs_enabled {
        match root.get("background") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(ToolError::input_invalid("background must be a boolean"));
            }
        }
    } else {
        false
    };
    let job_action = if jobs_enabled {
        match root.get("job_action") {
            None => JobAction::Read,
            Some(Value::String(text)) => match text.as_str() {
                "read" => JobAction::Read,
                "wait" => JobAction::Wait,
                "kill" => JobAction::Kill,
                _ => {
                    return Err(ToolError::input_invalid(
                        "job_action must be read, wait, or kill",
                    ));
                }
            },
            Some(_) => {
                return Err(ToolError::input_invalid("job_action must be a string"));
            }
        }
    } else {
        JobAction::Read
    };
    Ok(ExecInput {
        command,
        cwd,
        timeout,
        background: background && job_id.is_none(),
        job_id,
        job_action,
    })
}

fn optional_string(value: Option<&Value>, field: &str) -> Result<Option<String>, ToolError> {
    match value {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(ToolError::input_invalid(format!(
            "{field} must be a string"
        ))),
    }
}

fn timeout_of(value: Option<&Value>, config: &ExecConfig) -> Result<Option<Duration>, ToolError> {
    let Some(value) = value else {
        return Ok(Some(config.default_timeout()));
    };
    if value.is_f64() && !value.is_i64() {
        return Err(ToolError::input_invalid(
            "timeout_ms must be an integer, not a float",
        ));
    }
    let millis = value
        .as_u64()
        .ok_or_else(|| ToolError::input_invalid("timeout_ms must be a non-negative integer"))?;
    if millis == 0 {
        if config.allow_unbounded_timeout() {
            return Ok(None);
        }
        return Err(ToolError::input_invalid(
            "timeout_ms must be greater than 0",
        ));
    }
    let timeout = Duration::from_millis(millis);
    if timeout > config.max_timeout() {
        return Err(ToolError::input_invalid(
            "timeout_ms exceeds the configured maximum",
        ));
    }
    Ok(Some(timeout))
}
