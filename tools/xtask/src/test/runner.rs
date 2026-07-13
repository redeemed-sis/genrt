use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{artifacts::Profile, qemu};

use super::{
    cases::{Case, Step},
    protocol::{Event, ProtocolState, Record, StreamParser},
};

const SERIAL_CHANNEL_CAPACITY: usize = 64;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Status {
    Passed,
    Failed,
    TimedOut,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct CaseResult {
    pub(super) name: String,
    pub(super) status: Status,
    pub(super) duration_ms: u128,
    pub(super) profile: Profile,
    pub(super) kernel_features: Vec<String>,
    pub(super) exit_reason: String,
    pub(super) failed_step: Option<usize>,
}

struct ChildGuard {
    child: Child,
    reaped: bool,
}

impl ChildGuard {
    fn terminate_and_wait(&mut self) -> Result<()> {
        if self.reaped {
            return Ok(());
        }
        if self
            .child
            .try_wait()
            .context("failed to query QEMU status")?
            .is_none()
            && let Err(kill_error) = self.child.kill()
            && self
                .child
                .try_wait()
                .context("failed to query QEMU status after kill error")?
                .is_none()
        {
            return Err(kill_error).context("failed to kill QEMU");
        }
        self.child.wait().context("failed to reap QEMU")?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.terminate_and_wait();
    }
}

pub(super) fn run_case(
    case: &Case,
    config: &qemu::Config,
    output_dir: &Path,
    timeout_override_secs: u64,
    suite_deadline: Instant,
) -> Result<CaseResult> {
    fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;
    let serial_path = output_dir.join("serial.log");
    let stderr_path = output_dir.join("qemu-stderr.log");
    let serial_file = File::create(&serial_path)
        .with_context(|| format!("failed to create {}", serial_path.display()))?;
    let stderr_file = File::create(&stderr_path)
        .with_context(|| format!("failed to create {}", stderr_path.display()))?;
    let started = Instant::now();

    let mut command = config.command();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().context("failed to spawn QEMU")?;
    let mut guard = ChildGuard {
        child,
        reaped: false,
    };
    let stdin = guard
        .child
        .stdin
        .take()
        .context("failed to capture QEMU stdin")?;
    let stdout = guard
        .child
        .stdout
        .take()
        .context("failed to capture QEMU serial")?;
    let stderr = guard
        .child
        .stderr
        .take()
        .context("failed to capture QEMU stderr")?;
    let (serial_tx, serial_rx) = mpsc::sync_channel(SERIAL_CHANNEL_CAPACITY);
    let serial_thread = spawn_drain(stdout, serial_file, Some(serial_tx));
    let stderr_thread = spawn_drain(stderr, stderr_file, None);

    let case_timeout = Duration::from_secs(case.timeout_secs.min(timeout_override_secs));
    let step_timeout = Duration::from_secs(case.step_timeout_secs.min(timeout_override_secs));
    let deadline = (started + case_timeout).min(suite_deadline);
    let outcome = execute_case(case, stdin, &serial_rx, &mut guard, deadline, step_timeout);

    let termination = guard.terminate_and_wait();
    drop(serial_rx);
    let serial_drain = join_drain(serial_thread, "serial");
    let stderr_drain = join_drain(stderr_thread, "stderr");
    let complete_protocol = validate_complete_serial(case, &serial_path);
    let infrastructure = termination
        .and(serial_drain)
        .and(stderr_drain)
        .and(complete_protocol);
    let outcome = match (outcome, infrastructure) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(err)) => Err(Failure::Failed {
            step: case.steps.len() + 1,
            reason: format!("post-run validation failed: {err:#}"),
        }),
        (Err(failure), Ok(())) => Err(failure),
        (Err(failure), Err(err)) => Err(failure.with_infrastructure(err)),
    };

    let (status, exit_reason, failed_step) = match outcome {
        Ok(()) => (
            Status::Passed,
            "terminal DONE PASS observed".to_owned(),
            None,
        ),
        Err(Failure::Timeout { step, expected }) => (
            Status::TimedOut,
            format!("timeout waiting for {expected}"),
            Some(step),
        ),
        Err(Failure::Failed { step, reason }) => (Status::Failed, reason, Some(step)),
    };
    Ok(CaseResult {
        name: case.name.clone(),
        status,
        duration_ms: started.elapsed().as_millis(),
        profile: case.profile,
        kernel_features: case.kernel_features.clone(),
        exit_reason,
        failed_step,
    })
}

enum Failure {
    Timeout { step: usize, expected: String },
    Failed { step: usize, reason: String },
}

impl Failure {
    fn with_infrastructure(self, error: anyhow::Error) -> Self {
        match self {
            Self::Timeout { step, expected } => Self::Timeout {
                step,
                expected: format!("{expected}; post-run validation also failed: {error:#}"),
            },
            Self::Failed { step, reason } => Self::Failed {
                step,
                reason: format!("{reason}; post-run validation also failed: {error:#}"),
            },
        }
    }
}

struct Execution<'a> {
    serial_rx: &'a Receiver<Vec<u8>>,
    child: &'a mut ChildGuard,
    parser: StreamParser,
    protocol: ProtocolState,
    pending: VecDeque<Record>,
    case_deadline: Instant,
    step_timeout: Duration,
}

impl Execution<'_> {
    fn wait_for(
        &mut self,
        step: usize,
        producer: &str,
        event: Event,
        subject: &str,
        detail: Option<&str>,
    ) -> Result<(), Failure> {
        let expected = format!("{producer}:{event:?}:{subject}:{detail:?}");
        let deadline = self.case_deadline.min(Instant::now() + self.step_timeout);
        loop {
            if let Some(record) = self.pending.front() {
                if record.producer == producer
                    && record.event == event
                    && record.subject == subject
                    && record.detail.as_deref() == detail
                {
                    self.pending.pop_front();
                    return Ok(());
                }
                return Err(Failure::Failed {
                    step,
                    reason: format!("unexpected protocol record {record:?}, expected {expected}"),
                });
            }
            self.receive(step, &expected, deadline)?;
        }
    }

    fn receive(&mut self, step: usize, expected: &str, deadline: Instant) -> Result<(), Failure> {
        let now = Instant::now();
        if now >= deadline {
            return Err(Failure::Timeout {
                step,
                expected: expected.to_owned(),
            });
        }
        let wait = (deadline - now).min(Duration::from_millis(100));
        match self.serial_rx.recv_timeout(wait) {
            Ok(chunk) => {
                for parsed in self.parser.push(&chunk) {
                    let record = parsed.map_err(|err| Failure::Failed {
                        step,
                        reason: format!("malformed test protocol record: {err:#}"),
                    })?;
                    self.protocol
                        .observe(&record)
                        .map_err(|err| Failure::Failed {
                            step,
                            reason: format!("test protocol violation: {err:#}"),
                        })?;
                    self.pending.push_back(record);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = self.child.child.try_wait() {
                    return Err(Failure::Failed {
                        step,
                        reason: format!("QEMU exited before {expected}: {status}"),
                    });
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Failure::Failed {
                    step,
                    reason: format!("serial stream closed before {expected}"),
                });
            }
        }
        Ok(())
    }
}

fn execute_case(
    case: &Case,
    mut stdin: impl Write,
    serial_rx: &Receiver<Vec<u8>>,
    child: &mut ChildGuard,
    case_deadline: Instant,
    step_timeout: Duration,
) -> Result<(), Failure> {
    let mut execution = Execution {
        serial_rx,
        child,
        parser: StreamParser::default(),
        protocol: ProtocolState::new(&case.supervisor, &case.suite),
        pending: VecDeque::new(),
        case_deadline,
        step_timeout,
    };
    execution.wait_for(0, &case.supervisor, Event::Ready, &case.suite, None)?;

    for (index, step) in case.steps.iter().enumerate() {
        let step_number = index + 1;
        match step {
            Step::SendLine { value } => send_line(&mut stdin, value, step_number)?,
            Step::Expect {
                producer,
                event,
                subject,
                detail,
            } => {
                let event = Event::parse(event).map_err(|err| Failure::Failed {
                    step: step_number,
                    reason: format!("invalid case event: {err:#}"),
                })?;
                execution.wait_for(step_number, producer, event, subject, detail.as_deref())?;
            }
            Step::ExpectCase { producer, subject } => {
                execution.wait_for(step_number, producer, Event::CaseStart, subject, None)?;
                execution.wait_for(step_number, producer, Event::Pass, subject, None)?;
            }
            Step::Challenge {
                command,
                producer,
                subject,
            } => {
                let nonce = challenge_nonce(&case.name, step_number);
                send_line(&mut stdin, &format!("{command} {nonce}"), step_number)?;
                execution.wait_for(step_number, producer, Event::CaseStart, subject, None)?;
                execution.wait_for(step_number, producer, Event::Pass, subject, Some(&nonce))?;
            }
        }
    }

    let terminal_step = case.steps.len() + 1;
    execution.wait_for(
        terminal_step,
        &case.supervisor,
        Event::Done,
        &case.suite,
        Some("PASS"),
    )?;
    if !execution.protocol.terminal() {
        return Err(Failure::Failed {
            step: terminal_step,
            reason: "terminal protocol state was not reached".to_owned(),
        });
    }
    Ok(())
}

fn send_line(output: &mut impl Write, value: &str, step: usize) -> Result<(), Failure> {
    output
        .write_all(value.as_bytes())
        .and_then(|()| output.write_all(b"\n"))
        .and_then(|()| output.flush())
        .map_err(|err| Failure::Failed {
            step,
            reason: format!("failed to send line: {err}"),
        })
}

fn challenge_nonce(case: &str, step: usize) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut digest = Sha256::new();
    digest.update(case.as_bytes());
    digest.update(step.to_le_bytes());
    digest.update(std::process::id().to_le_bytes());
    digest.update(now.to_le_bytes());
    format!("{:x}", digest.finalize())[..8].to_owned()
}

fn spawn_drain<R: Read + Send + 'static>(
    mut input: R,
    mut file: File,
    mut chunks: Option<SyncSender<Vec<u8>>>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            let read = input
                .read(&mut buffer)
                .context("failed to drain QEMU output")?;
            if read == 0 {
                break;
            }
            let bytes = &buffer[..read];
            file.write_all(bytes).context("failed to write QEMU log")?;
            if let Some(sender) = &chunks
                && sender.send(bytes.to_vec()).is_err()
            {
                chunks = None;
            }
        }
        file.flush().context("failed to flush QEMU log")
    })
}

fn join_drain(handle: thread::JoinHandle<Result<()>>, stream: &str) -> Result<()> {
    handle
        .join()
        .map_err(|_| anyhow!("{stream} drain thread panicked"))?
        .with_context(|| format!("{stream} drain failed"))
}

fn validate_complete_serial(case: &Case, path: &Path) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to reopen {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut parser = StreamParser::default();
    let mut protocol = ProtocolState::new(&case.supervisor, &case.suite);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .with_context(|| format!("failed to reread {}", path.display()))?;
        if count == 0 {
            break;
        }
        for record in parser.push(&buffer[..count]) {
            protocol.observe(&record?)?;
        }
    }
    parser.finish()?;
    protocol.validate_complete()
}

pub(super) fn write_result(path: PathBuf, result: &CaseResult) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(result).context("failed to serialize case result")?;
    fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

pub(super) fn report_failure(result: &CaseResult, case_dir: &Path) -> Result<()> {
    if matches!(result.status, Status::Passed) {
        return Ok(());
    }
    let serial = read_bounded_tail(&case_dir.join("serial.log"), 64 * 1024).unwrap_or_default();
    let text = String::from_utf8_lossy(&serial);
    let mut lines = text.lines().rev().take(25).collect::<Vec<_>>();
    lines.reverse();
    eprintln!("case {}: {}", result.name, result.exit_reason);
    eprintln!("serial tail:\n{}", lines.join("\n"));
    eprintln!("full log: {}", case_dir.join("serial.log").display());
    Err(anyhow!("QEMU case {} failed", result.name))
}

fn read_bounded_tail(path: &Path, capacity: usize) -> Result<Vec<u8>> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut tail = VecDeque::with_capacity(capacity);
    let mut buffer = [0u8; 4096];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if count == 0 {
            break;
        }
        for byte in &buffer[..count] {
            if tail.len() == capacity {
                tail.pop_front();
            }
            tail.push_back(*byte);
        }
    }
    Ok(tail.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::cases::InitImage;

    fn test_case() -> Case {
        Case {
            name: "tail".to_owned(),
            suite: "suite".to_owned(),
            supervisor: "sup".to_owned(),
            profile: Profile::Debug,
            log_level: "info".to_owned(),
            kernel_features: Vec::new(),
            init: InitImage::KernelContract,
            timeout_secs: 1,
            step_timeout_secs: 1,
            steps: Vec::new(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("genrt-runner-{name}-{}", std::process::id()))
    }

    #[test]
    fn challenge_nonce_is_bounded_hex() {
        let nonce = challenge_nonce("case", 1);
        assert_eq!(nonce.len(), 8);
        assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn complete_log_validation_rejects_records_after_done() {
        let path = temp_path("late-record.log");
        fs::write(
            &path,
            b"\x1eGTRT/1|sup|000001|READY|suite\n\x1eGTRT/1|sup|000002|DONE|suite|PASS\n\x1eGTRT/1|sup|000003|FAIL|late|BAD\n",
        )
        .unwrap();
        assert!(validate_complete_serial(&test_case(), &path).is_err());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn drain_keeps_logging_after_parser_disconnects() {
        let path = temp_path("complete-drain.log");
        let file = File::create(&path).unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        let handle = spawn_drain(std::io::Cursor::new(b"complete serial"), file, Some(sender));
        join_drain(handle, "test").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete serial");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn failure_tail_memory_is_bounded() {
        let path = temp_path("bounded-tail.log");
        fs::write(&path, vec![b'x'; 8192]).unwrap();
        assert_eq!(read_bounded_tail(&path, 1024).unwrap().len(), 1024);
        let _ = fs::remove_file(path);
    }
}
