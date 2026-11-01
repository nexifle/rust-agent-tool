//! The exec tool. One call builds argv and asks the host runner to start it.
//!
//! Foreground is the default. Background exists only when the host passes
//! [`ExecJobs`]. The SDK does not keep a process table.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde_json::Value;

use crate::ToolError;
use crate::async_trait;
use crate::exec::argv::{self, ShellKind, ShellSpec};
use crate::exec::capture::Capture;
use crate::exec::classify::{self, CommandClass};
use crate::exec::config::ExecConfig;
use crate::exec::host::{
    CommandView, CwdDecision, ExecCwd, ExecEnv, ExecJobs, ExecRequest, ExecRunner, ExecSpill,
    ExecStatus, HostFailure, JobError, JobStatus, JobView,
};
use crate::exec::input::{self, ExecInput, JobAction};
use crate::exec::outcome::{
    CANCELLED, CWD_REJECTED, ExecOutcome, HOST_FAILED, JOB_NOT_FOUND, NONZERO_EXIT, OutcomeParts,
    SANDBOX_DENIED, SPAWN_FAILED, TIMED_OUT,
};
use crate::tool::{CallContext, Tool, ToolOutput, ToolSpec};

/// Shell command tool. Process start, directory checks, and spill files are
/// host hooks.
pub struct ExecTool {
    spec: ToolSpec,
    config: ExecConfig,
    shell: ShellSpec,
    cwd: Arc<dyn ExecCwd>,
    runner: Arc<dyn ExecRunner>,
    env: Option<Arc<dyn ExecEnv>>,
    spill: Option<Arc<dyn ExecSpill>>,
    jobs: Option<Arc<dyn ExecJobs>>,
    cancel: Option<Arc<AtomicBool>>,
}

/// Builds an [`ExecTool`].
pub struct ExecToolBuilder {
    shell: ShellSpec,
    cwd: Arc<dyn ExecCwd>,
    runner: Arc<dyn ExecRunner>,
    config: ExecConfig,
    env: Option<Arc<dyn ExecEnv>>,
    spill: Option<Arc<dyn ExecSpill>>,
    jobs: Option<Arc<dyn ExecJobs>>,
    cancel: Option<Arc<AtomicBool>>,
    name: String,
    description: Option<String>,
}

impl ExecToolBuilder {
    /// Starts a builder. The shell executable was already resolved by the host.
    #[must_use]
    pub fn new(shell: ShellSpec, cwd: Arc<dyn ExecCwd>, runner: Arc<dyn ExecRunner>) -> Self {
        let description_kind = shell.kind();
        Self {
            shell,
            cwd,
            runner,
            config: ExecConfig::new(),
            env: None,
            spill: None,
            jobs: None,
            cancel: None,
            name: "exec".to_string(),
            description: Some(input::default_description(description_kind, false)),
        }
    }

    /// Replaces the budget. The value must already be valid.
    #[must_use]
    pub fn config(mut self, config: ExecConfig) -> Self {
        self.config = config;
        self
    }

    /// Installs an environment hook. Without it the request carries no map and
    /// the runner decides inheritance.
    #[must_use]
    pub fn env(mut self, env: Arc<dyn ExecEnv>) -> Self {
        self.env = Some(env);
        self
    }

    /// Installs a spill hook. Without it omitted bytes are only counted.
    #[must_use]
    pub fn spill(mut self, spill: Arc<dyn ExecSpill>) -> Self {
        self.spill = Some(spill);
        self
    }

    /// Enables background jobs. The schema and description advertise them.
    #[must_use]
    pub fn jobs(mut self, jobs: Arc<dyn ExecJobs>) -> Self {
        self.description = Some(input::default_description(self.shell.kind(), true));
        self.jobs = Some(jobs);
        self
    }

    /// Shares a cancel flag the host can set during `execute`.
    #[must_use]
    pub fn cancel_flag(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = Some(cancel);
        self
    }

    /// Tool name. Empty or whitespace is rejected by [`ExecToolBuilder::build`].
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Replaces the description shown to the model.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Builds the tool.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the name or description
    /// is empty.
    pub fn build(self) -> Result<ExecTool, ToolError> {
        let jobs = self.jobs.is_some();
        let description = self
            .description
            .unwrap_or_else(|| input::default_description(self.shell.kind(), jobs));
        let spec = ToolSpec::new(self.name, description, input::input_schema(jobs))?;
        Ok(ExecTool {
            spec,
            config: self.config,
            shell: self.shell,
            cwd: self.cwd,
            runner: self.runner,
            env: self.env,
            spill: self.spill,
            jobs: self.jobs,
            cancel: self.cancel,
        })
    }
}

impl ExecTool {
    /// Specification registered with the agent.
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    /// Shell family the description names.
    #[must_use]
    pub fn shell_kind(&self) -> ShellKind {
        self.shell.kind()
    }

    /// Runs one call and returns the structured outcome.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ToolError`] only when the model input is unusable.
    /// Spawn failures, timeouts, non-zero exits, and hook failures are
    /// [`ExecOutcome`] values.
    pub async fn execute(
        &self,
        _ctx: &CallContext,
        input: Value,
    ) -> Result<ExecOutcome, ToolError> {
        let parsed = input::parse(&input, &self.config, self.jobs.is_some())?;
        if parsed.job_id.is_some() {
            return self.control_job(&parsed).await;
        }
        if parsed.background {
            return self.start_background(&parsed).await;
        }
        self.foreground(&parsed).await
    }

    async fn control_job(&self, parsed: &ExecInput) -> Result<ExecOutcome, ToolError> {
        let Some(jobs) = &self.jobs else {
            return self.foreground(parsed).await;
        };
        let id = parsed.job_id.as_deref().unwrap_or("");
        let timeout = parsed.timeout.unwrap_or(self.config.max_timeout());
        let view = match parsed.job_action {
            JobAction::Read => jobs.read(id).await,
            JobAction::Wait => jobs.wait(id, timeout).await,
            JobAction::Kill => jobs.kill(id).await,
        };
        Ok(match view {
            Ok(view) => self.outcome_from_job(id, &view),
            Err(JobError::NotFound) => outcome_error(
                "",
                "(no output)",
                "unknown background job",
                JOB_NOT_FOUND,
                &CommandClass::Opaque,
                None,
                0,
            ),
            Err(JobError::Failed { message }) => outcome_error(
                "",
                "(no output)",
                &message,
                HOST_FAILED,
                &CommandClass::Opaque,
                None,
                0,
            ),
        })
    }

    fn outcome_from_job(&self, id: &str, view: &JobView) -> ExecOutcome {
        let mut capture = Capture::from_config(&self.config);
        capture.push(&view.output);
        let omitted = capture.omitted_bytes();
        let mut body = capture.render_body(None);
        if body == "(no output)" {
            body = format!("Job {id}");
        }
        let (status, code, exit_code) = match &view.status {
            JobStatus::Running => (format!("Job {id} is running"), None, None),
            JobStatus::Exited { code } if *code == 0 => {
                (format!("Exit code: {code}"), None, Some(*code))
            }
            JobStatus::Exited { code } => (
                format!("Exit code: {code}"),
                Some(NONZERO_EXIT),
                Some(*code),
            ),
            JobStatus::TimedOut => ("Timed out".to_string(), Some(TIMED_OUT), view.exit_code),
            JobStatus::Cancelled => ("Cancelled".to_string(), Some(CANCELLED), view.exit_code),
        };
        ExecOutcome::new(OutcomeParts {
            directory: "",
            body: &body,
            status: &status,
            code,
            exit_code,
            omitted_bytes: omitted,
            class: &CommandClass::Opaque,
        })
    }

    async fn start_background(&self, parsed: &ExecInput) -> Result<ExecOutcome, ToolError> {
        let Some(jobs) = &self.jobs else {
            return self.foreground(parsed).await;
        };
        let command = parsed.command.as_deref().unwrap_or("");
        let class = classify::classify(self.shell.kind(), command);
        let directory = match self.prepare_directory(parsed.cwd.as_deref(), &class).await {
            Ok(directory) => directory,
            Err(outcome) => return Ok(outcome),
        };
        let env = match self.prepare_env(command, &class, &directory).await {
            Ok(env) => env,
            Err(failure) => {
                return Ok(outcome_error(
                    &directory,
                    "(no output)",
                    &failure.message,
                    HOST_FAILED,
                    &class,
                    None,
                    0,
                ));
            }
        };
        let request = self.request(command, directory.clone(), env, parsed.timeout);
        match jobs.start(request).await {
            Ok(started) => {
                let mut capture = Capture::from_config(&self.config);
                capture.push(&started.prefix);
                let omitted = capture.omitted_bytes();
                let prefix = capture.render_body(None);
                let body = if prefix == "(no output)" {
                    format!("Backgrounded as job {}", started.id)
                } else {
                    format!("Backgrounded as job {}\n{prefix}", started.id)
                };
                Ok(ExecOutcome::new(OutcomeParts {
                    directory: &directory,
                    body: &body,
                    status: "Backgrounded",
                    code: None,
                    exit_code: None,
                    omitted_bytes: omitted,
                    class: &class,
                }))
            }
            Err(failure) => Ok(outcome_error(
                &directory,
                "(no output)",
                &failure.message,
                HOST_FAILED,
                &class,
                None,
                0,
            )),
        }
    }

    async fn foreground(&self, parsed: &ExecInput) -> Result<ExecOutcome, ToolError> {
        let command = parsed.command.as_deref().unwrap_or("");
        let class = classify::classify(self.shell.kind(), command);
        let directory = match self.prepare_directory(parsed.cwd.as_deref(), &class).await {
            Ok(directory) => directory,
            Err(outcome) => return Ok(outcome),
        };
        let env = match self.prepare_env(command, &class, &directory).await {
            Ok(env) => env,
            Err(failure) => {
                return Ok(outcome_error(
                    &directory,
                    "(no output)",
                    &failure.message,
                    HOST_FAILED,
                    &class,
                    None,
                    0,
                ));
            }
        };
        let timeout = parsed.timeout;
        let request = self.request(command, directory.clone(), env, timeout);
        let mut capture = Capture::from_config(&self.config);
        let status = match self.runner.run(request, &mut capture).await {
            Ok(status) => status,
            Err(failure) => {
                return Ok(outcome_error(
                    &directory,
                    "(no output)",
                    &failure.message,
                    HOST_FAILED,
                    &class,
                    None,
                    capture.omitted_bytes(),
                ));
            }
        };
        let omitted = capture.omitted_bytes();
        let (locator, spill_error) = self.spill_locator(omitted).await;
        let mut body = capture.render_body(locator.as_deref());
        if let Some(message) = spill_error {
            body.push_str("\nSpill failed: ");
            body.push_str(&message);
        }
        Ok(status_outcome(
            &directory,
            &body,
            &status,
            &class,
            omitted,
            timeout.unwrap_or(Duration::ZERO),
        ))
    }

    async fn spill_locator(&self, omitted: u64) -> (Option<String>, Option<String>) {
        if omitted == 0 {
            return (None, None);
        }
        let Some(spill) = &self.spill else {
            return (None, None);
        };
        match spill.write(&[]).await {
            Ok(note) => (Some(note.locator), None),
            Err(failure) => (None, Some(failure.message)),
        }
    }

    async fn prepare_directory(
        &self,
        cwd: Option<&str>,
        class: &CommandClass,
    ) -> Result<String, ExecOutcome> {
        match self.cwd.check(cwd).await {
            Ok(CwdDecision::Accept { directory }) => Ok(directory),
            Ok(CwdDecision::Reject { message }) => Err(outcome_error(
                "",
                "(no output)",
                &message,
                CWD_REJECTED,
                class,
                None,
                0,
            )),
            Err(failure) => Err(outcome_error(
                "",
                "(no output)",
                &failure.message,
                HOST_FAILED,
                class,
                None,
                0,
            )),
        }
    }

    async fn prepare_env(
        &self,
        command: &str,
        class: &CommandClass,
        directory: &str,
    ) -> Result<Option<Vec<(String, String)>>, HostFailure> {
        let Some(env) = &self.env else {
            return Ok(None);
        };
        let view = CommandView {
            command,
            class: class.clone(),
            directory,
            shell: self.shell.kind(),
        };
        env.prepare(&view).await.map(Some)
    }

    fn request(
        &self,
        command: &str,
        directory: String,
        env: Option<Vec<(String, String)>>,
        timeout: Option<Duration>,
    ) -> ExecRequest {
        ExecRequest {
            argv: argv::argv(&self.shell, command),
            directory,
            env,
            timeout,
            drain_timeout: self.config.drain_timeout(),
            chunk_bytes: self.config.chunk_bytes(),
            cancel: self
                .cancel
                .clone()
                .unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
        }
    }
}

fn outcome_error(
    directory: &str,
    body: &str,
    status: &str,
    code: &'static str,
    class: &CommandClass,
    exit_code: Option<i32>,
    omitted_bytes: u64,
) -> ExecOutcome {
    ExecOutcome::new(OutcomeParts {
        directory,
        body,
        status,
        code: Some(code),
        exit_code,
        omitted_bytes,
        class,
    })
}

fn status_outcome(
    directory: &str,
    body: &str,
    status: &ExecStatus,
    class: &CommandClass,
    omitted: u64,
    timeout: Duration,
) -> ExecOutcome {
    let millis = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let (line, code, exit_code) = match status {
        ExecStatus::Exited { code, detail } if *code == 0 => {
            let line = match detail {
                Some(detail) => format!("Exit code: {code} ({detail})"),
                None => format!("Exit code: {code}"),
            };
            (line, None, Some(*code))
        }
        ExecStatus::Exited { code, detail } => {
            let line = match detail {
                Some(detail) => format!("Exit code: {code} ({detail})"),
                None => format!("Exit code: {code}"),
            };
            (line, Some(NONZERO_EXIT), Some(*code))
        }
        ExecStatus::TimedOut => (
            format!("Timed out after {millis} ms"),
            Some(TIMED_OUT),
            None,
        ),
        ExecStatus::Cancelled => ("Cancelled".to_string(), Some(CANCELLED), None),
        ExecStatus::SpawnFailed { message } => (message.clone(), Some(SPAWN_FAILED), None),
        ExecStatus::Denied { message } => (message.clone(), Some(SANDBOX_DENIED), None),
    };
    ExecOutcome::new(OutcomeParts {
        directory,
        body,
        status: &line,
        code,
        exit_code,
        omitted_bytes: omitted,
        class,
    })
}

#[async_trait]
impl Tool for ExecTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &CallContext, input: Value) -> Result<ToolOutput, ToolError> {
        Ok(self.execute(ctx, input).await?.to_output())
    }
}

#[cfg(test)]
mod tests {
    use super::ExecToolBuilder;
    use crate::CallContext;
    use crate::async_trait;
    use crate::exec::ShellSpec;
    use crate::exec::argv::ShellKind;
    use crate::exec::capture::Capture;
    use crate::exec::config::ExecConfig;
    use crate::exec::host::{
        CwdDecision, ExecCwd, ExecJobs, ExecRequest, ExecRunner, ExecSpill, ExecStatus,
        HostFailure, JobError, JobStatus, JobView, SpillNote, StartedJob,
    };
    use crate::exec::outcome::{JOB_NOT_FOUND, NONZERO_EXIT, TIMED_OUT};
    use serde_json::json;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    fn ready<F>(future: F) -> F::Output
    where
        F: Future,
    {
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test future should not wait"),
        }
    }

    struct FixedCwd;

    #[async_trait]
    impl ExecCwd for FixedCwd {
        async fn check(&self, _cwd: Option<&str>) -> Result<CwdDecision, HostFailure> {
            Ok(CwdDecision::Accept {
                directory: "/work".to_string(),
            })
        }
    }

    struct RejectCwd;

    #[async_trait]
    impl ExecCwd for RejectCwd {
        async fn check(&self, _cwd: Option<&str>) -> Result<CwdDecision, HostFailure> {
            Ok(CwdDecision::Reject {
                message: "outside the jail".to_string(),
            })
        }
    }

    struct ScriptRunner {
        status: Mutex<ExecStatus>,
        output: Vec<u8>,
        runs: AtomicUsize,
    }

    #[async_trait]
    impl ExecRunner for ScriptRunner {
        async fn run(
            &self,
            _request: ExecRequest,
            capture: &mut Capture,
        ) -> Result<ExecStatus, HostFailure> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            capture.push(&self.output);
            Ok(self.status.lock().expect("runner lock").clone())
        }
    }

    struct FailRunner;

    #[async_trait]
    impl ExecRunner for FailRunner {
        async fn run(
            &self,
            _request: ExecRequest,
            _capture: &mut Capture,
        ) -> Result<ExecStatus, HostFailure> {
            Err(HostFailure::new("runner exploded"))
        }
    }

    struct NoteSpill {
        called: AtomicBool,
    }

    #[async_trait]
    impl ExecSpill for NoteSpill {
        async fn write(&self, _bytes: &[u8]) -> Result<SpillNote, HostFailure> {
            self.called.store(true, Ordering::SeqCst);
            Ok(SpillNote {
                locator: "/tmp/full.txt".to_string(),
            })
        }
    }

    struct MemoryJobs {
        started: AtomicUsize,
        reads: AtomicUsize,
    }

    #[async_trait]
    impl ExecJobs for MemoryJobs {
        async fn start(&self, _request: ExecRequest) -> Result<StartedJob, HostFailure> {
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(StartedJob {
                id: "job-7".to_string(),
                prefix: Vec::new(),
            })
        }

        async fn read(&self, id: &str) -> Result<JobView, JobError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if id != "job-7" {
                return Err(JobError::NotFound);
            }
            Ok(JobView {
                status: JobStatus::Running,
                output: b"still going".to_vec(),
                exit_code: None,
            })
        }

        async fn wait(&self, id: &str, _timeout: Duration) -> Result<JobView, JobError> {
            self.read(id).await
        }

        async fn kill(&self, id: &str) -> Result<JobView, JobError> {
            if id != "job-7" {
                return Err(JobError::NotFound);
            }
            Ok(JobView {
                status: JobStatus::Exited { code: 0 },
                output: b"done".to_vec(),
                exit_code: Some(0),
            })
        }
    }

    fn shell() -> ShellSpec {
        ShellSpec::new("/bin/sh", ShellKind::Posix).expect("shell")
    }

    fn tool(runner: Arc<dyn ExecRunner>) -> super::ExecTool {
        ExecToolBuilder::new(shell(), Arc::new(FixedCwd), runner)
            .build()
            .expect("tool")
    }

    #[test]
    fn exit_zero_names_directory_and_new_process() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: b"hi".to_vec(),
            runs: AtomicUsize::new(0),
        });
        let outcome =
            ready(tool(runner).execute(&CallContext::new(), json!({"command": "echo hi"})))
                .expect("input");
        assert!(!outcome.is_error());
        assert!(outcome.text().contains("Working directory: /work"));
        assert!(outcome.text().contains("Exit code: 0"));
        assert!(outcome.text().contains("Next call starts a new process."));
        assert!(outcome.text().contains("hi"));
    }

    #[test]
    fn nonzero_is_operational_error() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 2,
                detail: None,
            }),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let outcome = ready(tool(runner).execute(&CallContext::new(), json!({"command": "false"})))
            .expect("input");
        assert!(outcome.is_error());
        assert_eq!(outcome.code(), Some(NONZERO_EXIT));
        assert!(outcome.to_output().is_error());
    }

    #[test]
    fn timeout_status_is_not_a_contract_error() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::TimedOut),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let outcome = ready(tool(runner).execute(
            &CallContext::new(),
            json!({"command": "sleep 9", "timeout_ms": 10}),
        ))
        .expect("input");
        assert_eq!(outcome.code(), Some(TIMED_OUT));
        assert!(outcome.text().contains("Timed out after 10 ms"));
    }

    #[test]
    fn rejected_cwd_does_not_run() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let runs = Arc::clone(&runner);
        let built = ExecToolBuilder::new(shell(), Arc::new(RejectCwd), runner)
            .build()
            .expect("tool");
        let outcome = ready(built.execute(&CallContext::new(), json!({"command": "echo hi"})))
            .expect("input");
        assert!(outcome.text().contains("AGT_EXEC_CWD_REJECTED"));
        assert!(outcome.text().contains("outside the jail"));
        assert_eq!(runs.runs.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn runner_failure_is_host_failed() {
        let outcome = ready(
            tool(Arc::new(FailRunner)).execute(&CallContext::new(), json!({"command": "echo hi"})),
        )
        .expect("input");
        assert!(outcome.text().contains("AGT_EXEC_HOST_FAILED"));
        assert!(outcome.text().contains("runner exploded"));
    }

    #[test]
    fn spill_note_only_when_output_was_omitted() {
        let spill = Arc::new(NoteSpill {
            called: AtomicBool::new(false),
        });
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: vec![b'a'; 400],
            runs: AtomicUsize::new(0),
        });
        let config = ExecConfig::new()
            .with_head_bytes(320)
            .expect("head")
            .with_max_output_bytes(320)
            .expect("max");
        let built = ExecToolBuilder::new(shell(), Arc::new(FixedCwd), runner)
            .config(config)
            .spill(spill.clone())
            .build()
            .expect("tool");
        let outcome = ready(built.execute(&CallContext::new(), json!({"command": "echo hi"})))
            .expect("input");
        assert!(spill.called.load(Ordering::SeqCst));
        assert!(
            outcome.text().contains("full output: /tmp/full.txt"),
            "{}",
            outcome.text()
        );
    }

    #[test]
    fn spill_hook_is_idle_when_nothing_was_omitted() {
        let spill = Arc::new(NoteSpill {
            called: AtomicBool::new(false),
        });
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: b"ab".to_vec(),
            runs: AtomicUsize::new(0),
        });
        let built = ExecToolBuilder::new(shell(), Arc::new(FixedCwd), runner)
            .spill(spill.clone())
            .build()
            .expect("tool");
        let outcome = ready(built.execute(&CallContext::new(), json!({"command": "echo hi"})))
            .expect("input");
        assert!(!spill.called.load(Ordering::SeqCst));
        assert!(!outcome.text().contains("full output"));
    }

    #[test]
    fn background_fields_stay_foreground_without_jobs() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let runs = Arc::clone(&runner);
        let outcome = ready(tool(runner).execute(
            &CallContext::new(),
            json!({"command": "echo hi", "background": true, "job_id": "nope"}),
        ))
        .expect("input");
        assert_eq!(outcome.exit_code(), Some(0));
        assert_eq!(runs.runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn jobs_start_read_and_unknown_id() {
        let jobs = Arc::new(MemoryJobs {
            started: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
        });
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let built = ExecToolBuilder::new(shell(), Arc::new(FixedCwd), runner)
            .jobs(jobs.clone())
            .build()
            .expect("tool");
        let started = ready(built.execute(
            &CallContext::new(),
            json!({"command": "echo hi", "background": true}),
        ))
        .expect("start");
        assert!(started.text().contains("Backgrounded as job job-7"));
        let first = ready(built.execute(
            &CallContext::new(),
            json!({"job_id": "job-7", "job_action": "read"}),
        ))
        .expect("read");
        let second = ready(built.execute(
            &CallContext::new(),
            json!({"job_id": "job-7", "job_action": "read"}),
        ))
        .expect("read again");
        assert!(first.text().contains("still going"));
        assert!(second.text().contains("still going"));
        assert_eq!(jobs.reads.load(Ordering::SeqCst), 2);
        let killed = ready(built.execute(
            &CallContext::new(),
            json!({"job_id": "job-7", "job_action": "kill"}),
        ))
        .expect("kill");
        assert_eq!(killed.exit_code(), Some(0));
        let missing = ready(built.execute(&CallContext::new(), json!({"job_id": "missing"})))
            .expect("missing");
        assert_eq!(missing.code(), Some(JOB_NOT_FOUND));
    }

    #[test]
    fn opaque_command_still_runs() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let runs = Arc::clone(&runner);
        let outcome =
            ready(tool(runner).execute(&CallContext::new(), json!({"command": "echo $(id)"})))
                .expect("input");
        assert!(!outcome.is_error());
        assert_eq!(runs.runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn input_rejects_empty_float_and_unbounded_timeout() {
        let runner = Arc::new(ScriptRunner {
            status: Mutex::new(ExecStatus::Exited {
                code: 0,
                detail: None,
            }),
            output: Vec::new(),
            runs: AtomicUsize::new(0),
        });
        let built = tool(runner);
        let empty = ready(built.execute(&CallContext::new(), json!({"command": "  "})));
        assert!(empty.unwrap_err().to_string().contains("AGT_INPUT_INVALID"));
        let float = ready(built.execute(
            &CallContext::new(),
            json!({"command": "echo hi", "timeout_ms": 1.5}),
        ));
        assert!(float.unwrap_err().to_string().contains("AGT_INPUT_INVALID"));
        let zero = ready(built.execute(
            &CallContext::new(),
            json!({"command": "echo hi", "timeout_ms": 0}),
        ));
        assert!(zero.unwrap_err().to_string().contains("AGT_INPUT_INVALID"));
        let config = ExecConfig::new().with_allow_unbounded_timeout(true);
        let open = ExecToolBuilder::new(
            shell(),
            Arc::new(FixedCwd),
            Arc::new(ScriptRunner {
                status: Mutex::new(ExecStatus::Exited {
                    code: 0,
                    detail: None,
                }),
                output: Vec::new(),
                runs: AtomicUsize::new(0),
            }),
        )
        .config(config)
        .build()
        .expect("tool");
        let outcome = ready(open.execute(
            &CallContext::new(),
            json!({"command": "echo hi", "timeout_ms": 0}),
        ))
        .expect("unbounded");
        assert!(!outcome.is_error());
        let huge = ready(built.execute(
            &CallContext::new(),
            json!({"command": "echo hi", "timeout_ms": 86_400_000u64}),
        ));
        assert!(huge.unwrap_err().to_string().contains("AGT_INPUT_INVALID"));
    }
}
