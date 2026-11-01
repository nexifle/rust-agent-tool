//! Reference process runner. Compiled only with the `runner` feature.
//!
//! This runner closes stdin, drains stdout and stderr until EOF, and kills the
//! process tree on timeout, cancellation, and drop. It does not strip ANSI and
//! does not retry a spawn the sandbox denied (it does not apply a sandbox).

use std::future::Future;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::Waker;
use std::thread;
use std::time::{Duration, Instant};

use process_wrap::std::{ChildWrapper, CommandWrap};

use crate::async_trait;
use crate::exec::capture::Capture;
use crate::exec::host::{ExecRequest, ExecRunner, ExecStatus, HostFailure};

const POLL_EVERY: Duration = Duration::from_millis(50);
#[cfg(unix)]
const SIGTERM: i32 = 15;

/// Reference [`ExecRunner`] built on `std::process` and `process-wrap`.
///
/// Unix children are process-group leaders. Windows children are attached to a
/// job object. Dropping the future kills that tree. When `request.env` is
/// `None` the child inherits this process environment. When it is `Some`, the
/// child receives exactly that map.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdRunner;

#[async_trait]
impl ExecRunner for StdRunner {
    async fn run(
        &self,
        request: ExecRequest,
        capture: &mut Capture,
    ) -> Result<ExecStatus, HostFailure> {
        Running::start(request, capture).await
    }
}

struct Running<'a> {
    capture: &'a mut Capture,
    incoming: Receiver<Vec<u8>>,
    done: Receiver<Result<ExecStatus, HostFailure>>,
    tree: Arc<Mutex<Option<Box<dyn ChildWrapper>>>>,
    waker: Arc<Mutex<Option<Waker>>>,
    defused: bool,
}

impl<'a> Running<'a> {
    fn start(request: ExecRequest, capture: &'a mut Capture) -> Self {
        let (byte_tx, incoming) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let tree = Arc::new(Mutex::new(None));
        let child_slot = Arc::clone(&tree);
        let waker = Arc::new(Mutex::new(None));
        let wake_slot = Arc::clone(&waker);
        thread::spawn(move || {
            let result = supervise(request, byte_tx, &child_slot, &wake_slot);
            let _ = done_tx.send(result);
            wake(&wake_slot);
        });
        Self {
            capture,
            incoming,
            done,
            tree,
            waker,
            defused: false,
        }
    }
}

impl Future for Running<'_> {
    type Output = Result<ExecStatus, HostFailure>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Ok(mut slot) = self.waker.lock() {
            *slot = Some(cx.waker().clone());
        }
        while let Ok(chunk) = self.incoming.try_recv() {
            self.capture.push(&chunk);
        }
        match self.done.try_recv() {
            Ok(result) => {
                self.defused = true;
                while let Ok(chunk) = self.incoming.try_recv() {
                    self.capture.push(&chunk);
                }
                std::task::Poll::Ready(result)
            }
            Err(mpsc::TryRecvError::Empty) => std::task::Poll::Pending,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.defused = true;
                std::task::Poll::Ready(Err(HostFailure::new(
                    "process supervisor stopped without a status",
                )))
            }
        }
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        if self.defused {
            return;
        }
        if let Ok(mut slot) = self.tree.lock()
            && let Some(child) = slot.as_mut()
        {
            let _ = child.start_kill();
        }
        while let Ok(chunk) = self.incoming.try_recv() {
            self.capture.push(&chunk);
        }
    }
}

fn supervise(
    request: ExecRequest,
    bytes: Sender<Vec<u8>>,
    slot: &Mutex<Option<Box<dyn ChildWrapper>>>,
    waker: &Mutex<Option<Waker>>,
) -> Result<ExecStatus, HostFailure> {
    let mut child = match spawn(&request) {
        Ok(child) => child,
        Err(error) => {
            return Ok(ExecStatus::SpawnFailed {
                message: error.message,
            });
        }
    };
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    {
        let mut guard = slot.lock().unwrap_or_else(|poison| poison.into_inner());
        *guard = Some(child);
    }
    let stdout_done = read_pipe(stdout, bytes.clone(), request.chunk_bytes);
    let stderr_done = read_pipe(stderr, bytes, request.chunk_bytes);
    let started = Instant::now();
    let deadline = request.timeout.map(|limit| started + limit);
    let mut kill_at: Option<Instant> = None;
    let mut outcome: Option<ExecStatus> = None;
    loop {
        if kill_at.is_none() && request.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            soft_kill(slot);
            kill_at = Some(Instant::now() + request.drain_timeout);
            outcome = Some(ExecStatus::Cancelled);
        }
        if kill_at.is_none() {
            if let Some(deadline) = deadline
                && Instant::now() >= deadline
            {
                soft_kill(slot);
                kill_at = Some(Instant::now() + request.drain_timeout);
                outcome = Some(ExecStatus::TimedOut);
            }
        } else if Instant::now() >= kill_at.unwrap_or(started) {
            hard_kill(slot);
        }
        let exited = child_try_wait(slot)?;
        if let Some(status) = exited {
            hard_kill(slot);
            let drain_deadline = Instant::now() + request.drain_timeout;
            while Instant::now() < drain_deadline {
                if stdout_done.is_finished() && stderr_done.is_finished() {
                    break;
                }
                thread::sleep(POLL_EVERY);
                wake(waker);
            }
            return Ok(outcome.unwrap_or(status));
        }
        thread::sleep(POLL_EVERY);
        wake(waker);
    }
}

fn wake(waker: &Mutex<Option<Waker>>) {
    if let Some(waker) = waker.lock().ok().and_then(|mut slot| slot.take()) {
        waker.wake();
    }
}

fn spawn(request: &ExecRequest) -> Result<Box<dyn ChildWrapper + Send>, HostFailure> {
    let (program, args) = request
        .argv
        .split_first()
        .ok_or_else(|| HostFailure::new("argv must include the shell executable"))?;
    let mut command = Command::new(program);
    command.args(args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.current_dir(&request.directory);
    if let Some(env) = &request.env {
        command.env_clear();
        command.envs(
            env.iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
    }
    let mut wrapped = CommandWrap::from(command);
    #[cfg(unix)]
    wrapped.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(process_wrap::std::JobObject);
    let child = wrapped
        .spawn()
        .map_err(|error| HostFailure::new(error.to_string()))?;
    Ok(child)
}

fn read_pipe(
    pipe: Option<impl Read + Send + 'static>,
    bytes: Sender<Vec<u8>>,
    chunk_bytes: usize,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return;
        };
        let mut chunk = vec![0_u8; chunk_bytes.max(1)];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    if bytes.send(chunk[..count].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    })
}

fn child_try_wait(
    slot: &Mutex<Option<Box<dyn ChildWrapper>>>,
) -> Result<Option<ExecStatus>, HostFailure> {
    let mut guard = slot.lock().unwrap_or_else(|poison| poison.into_inner());
    let Some(child) = guard.as_mut() else {
        return Err(HostFailure::new("child process was not stored"));
    };
    match child.try_wait() {
        Ok(Some(status)) => Ok(Some(exit_status(status))),
        Ok(None) => Ok(None),
        Err(error) => Err(HostFailure::new(error.to_string())),
    }
}

fn exit_status(status: std::process::ExitStatus) -> ExecStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let code = status.code().unwrap_or(128 + signal);
            return ExecStatus::Exited {
                code,
                detail: Some(format!("signal {signal}")),
            };
        }
    }
    ExecStatus::Exited {
        code: status.code().unwrap_or(1),
        detail: None,
    }
}

fn soft_kill(slot: &Mutex<Option<Box<dyn ChildWrapper>>>) {
    #[cfg(unix)]
    {
        if let Ok(mut guard) = slot.lock()
            && let Some(child) = guard.as_mut()
        {
            let _ = child.signal(SIGTERM);
        }
    }
    #[cfg(windows)]
    hard_kill(slot);
}

fn hard_kill(slot: &Mutex<Option<Box<dyn ChildWrapper>>>) {
    if let Ok(mut guard) = slot.lock()
        && let Some(child) = guard.as_mut()
    {
        let _ = child.start_kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::StdRunner;
    use crate::exec::capture::Capture;
    use crate::exec::host::{ExecRequest, ExecRunner, ExecStatus};
    use std::fs;
    use std::path::PathBuf;
    use std::pin::pin;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::task::{Context, Poll, Waker};
    use std::thread;
    use std::time::{Duration, Instant};

    fn sh_missing() -> bool {
        !PathBuf::from("/bin/sh").exists()
    }

    fn request(command: &str, timeout: Option<Duration>, directory: &str) -> ExecRequest {
        ExecRequest {
            argv: vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()],
            directory: directory.to_string(),
            env: None,
            timeout,
            drain_timeout: Duration::from_millis(400),
            chunk_bytes: 8 * 1024,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    fn drive(command: &str, timeout: Option<Duration>, capture: &mut Capture) -> ExecStatus {
        let directory = std::env::temp_dir();
        let future = StdRunner.run(
            request(command, timeout, directory.to_str().expect("temp dir")),
            capture,
        );
        let mut future = pin!(future);
        let started = Instant::now();
        loop {
            let waker = Waker::noop();
            let mut context = Context::from_waker(waker);
            match future.as_mut().poll(&mut context) {
                Poll::Ready(result) => return result.expect("runner"),
                Poll::Pending => {
                    if started.elapsed() > Duration::from_secs(20) {
                        panic!("runner did not finish");
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    #[test]
    fn echo_round_trip() {
        if sh_missing() {
            return;
        }
        let mut capture = Capture::new(1024, 256, 200).expect("capture");
        let status = drive(
            "echo hello-exec",
            Some(Duration::from_secs(5)),
            &mut capture,
        );
        assert!(matches!(status, ExecStatus::Exited { code: 0, .. }));
        assert!(capture.render_body(None).contains("hello-exec"));
    }

    #[test]
    fn output_past_the_window_is_counted_not_kept() {
        if sh_missing() {
            return;
        }
        let mut capture = Capture::new(512, 128, 64).expect("capture");
        let status = drive(
            "i=0; while [ \"$i\" -lt 400 ]; do echo hello-hello-hello-hello; i=$((i+1)); done",
            Some(Duration::from_secs(15)),
            &mut capture,
        );
        assert!(matches!(status, ExecStatus::Exited { code: 0, .. }));
        assert!(capture.omitted_bytes() > 0);
        assert!(capture.head().len() + capture.tail().len() <= 512);
    }

    #[test]
    fn a_child_that_ignores_term_still_dies() {
        if sh_missing() {
            return;
        }
        let mut capture = Capture::new(256, 64, 64).expect("capture");
        let started = Instant::now();
        let status = drive(
            "trap '' TERM; while :; do :; done",
            Some(Duration::from_millis(300)),
            &mut capture,
        );
        assert!(matches!(status, ExecStatus::TimedOut), "{status:?}");
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn closed_stdin_does_not_wait_for_the_timeout() {
        if sh_missing() {
            return;
        }
        let mut capture = Capture::new(256, 64, 64).expect("capture");
        let started = Instant::now();
        let status = drive(
            "read line; echo done",
            Some(Duration::from_secs(8)),
            &mut capture,
        );
        assert!(!matches!(status, ExecStatus::TimedOut), "{status:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_background_child_cannot_hold_the_call_open() {
        if sh_missing() {
            return;
        }
        let mut capture = Capture::new(1024, 256, 200).expect("capture");
        let started = Instant::now();
        let status = drive(
            "sleep 30 & echo hi",
            Some(Duration::from_millis(800)),
            &mut capture,
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert!(
            matches!(
                status,
                ExecStatus::Exited { code: 0, .. } | ExecStatus::TimedOut
            ),
            "{status:?}"
        );
        assert!(capture.render_body(None).contains("hi"));
    }

    #[test]
    fn dropping_the_future_kills_the_process_group() {
        if sh_missing() {
            return;
        }
        let directory = std::env::temp_dir();
        let pid_path = directory.join(format!("rust-agent-tool-exec-{}.pid", std::process::id()));
        let _ = fs::remove_file(&pid_path);
        let command = format!("echo $$ > '{}'; sleep 30", pid_path.display());
        let mut capture = Capture::new(256, 64, 64).expect("capture");
        let pid: u32 = {
            let future = StdRunner.run(
                request(
                    &command,
                    Some(Duration::from_secs(40)),
                    directory.to_str().expect("temp"),
                ),
                &mut capture,
            );
            let mut future = pin!(future);
            let started = Instant::now();
            loop {
                let waker = Waker::noop();
                let mut context = Context::from_waker(waker);
                match future.as_mut().poll(&mut context) {
                    Poll::Ready(result) => panic!("sleep finished early: {result:?}"),
                    Poll::Pending
                        if pid_path.exists()
                            && fs::read_to_string(&pid_path)
                                .unwrap_or_default()
                                .trim()
                                .parse::<u32>()
                                .is_ok() =>
                    {
                        break;
                    }
                    Poll::Pending => {
                        if started.elapsed() > Duration::from_secs(5) {
                            panic!("pid file was not written");
                        }
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            }
            thread::sleep(Duration::from_millis(100));
            fs::read_to_string(&pid_path)
                .expect("pid file")
                .trim()
                .parse()
                .expect("pid")
        };
        let gone = Instant::now();
        loop {
            let alive = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            if gone.elapsed() > Duration::from_secs(3) {
                panic!("process {pid} is still alive");
            }
            thread::sleep(Duration::from_millis(30));
        }
        let _ = fs::remove_file(&pid_path);
    }
}
