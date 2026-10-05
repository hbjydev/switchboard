//! Process transport seam. Pi wire semantics stay in `pi`, outside this module.
use std::{ffi::OsString, future::Future, path::PathBuf, process::Stdio, time::Duration};

use ledger::AttemptId;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

use crate::{BackendError, CancellationToken};

pub struct ProcessSpec {
    pub attempt_id: AttemptId,
    pub binary: OsString,
    pub arguments: Vec<OsString>,
    pub workspace: PathBuf,
    pub shutdown_grace: Duration,
    /// Adapter-supplied abort record; the transport does not interpret its protocol.
    pub abort_record: Option<String>,
}

/// The minimum transport needed by RPC: JSONL I/O and owned process cleanup.
pub trait RpcProcess: Send {
    fn send(&mut self, record: String) -> impl Future<Output = Result<(), BackendError>> + Send;
    fn next_record(&mut self) -> impl Future<Output = Result<Option<String>, BackendError>> + Send;
    fn finish(&mut self, abort: bool) -> impl Future<Output = Result<(), BackendError>> + Send;
}

pub trait ExecutionEnvironment: Send + Sync {
    type Process: RpcProcess;
    fn spawn(
        &self,
        spec: ProcessSpec,
    ) -> impl Future<Output = Result<Self::Process, BackendError>> + Send;
}

#[derive(Default)]
pub struct LocalProcessEnvironment;

enum Control {
    Write(String, oneshot::Sender<Result<(), BackendError>>),
    Finish(bool),
}

/// Dropping this handle signals the supervisor; it retains the child until reaped.
pub struct LocalRpcProcess {
    commands: mpsc::Sender<Control>,
    records: mpsc::Receiver<Result<String, BackendError>>,
    cancellation: CancellationToken,
    supervisor: Option<JoinHandle<Result<(), BackendError>>>,
}

impl Drop for LocalRpcProcess {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl ExecutionEnvironment for LocalProcessEnvironment {
    type Process = LocalRpcProcess;

    async fn spawn(&self, spec: ProcessSpec) -> Result<LocalRpcProcess, BackendError> {
        if !spec.workspace.is_dir() {
            return Err(BackendError::Configuration(
                "workspace must be an existing directory".into(),
            ));
        }
        let mut command = Command::new(&spec.binary);
        command
            .args(&spec.arguments)
            .current_dir(&spec.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let pid = child.id();
        tracing::info!(attempt_id = %spec.attempt_id, ?pid, "agent process startup");
        let stdin = child
            .stdin
            .take()
            .ok_or(BackendError::Protocol("missing process stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(BackendError::Protocol("missing process stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(BackendError::Protocol("missing process stderr"))?;
        let (record_tx, records) = mpsc::channel(16);
        let readers = Readers {
            stdout: tokio::spawn(read_records(stdout, record_tx)),
            stderr: tokio::spawn(async move {
                let mut stderr = stderr;
                // Diagnostics may contain secrets. Drain without logging or parsing them.
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            }),
        };
        let (commands, receiver) = mpsc::channel(1);
        let cancellation = CancellationToken::new();
        let supervisor = tokio::spawn(supervise(
            OwnedChild {
                child,
                pid,
                _readers: readers,
            },
            stdin,
            receiver,
            cancellation.clone(),
            spec,
        ));
        Ok(LocalRpcProcess {
            commands,
            records,
            cancellation,
            supervisor: Some(supervisor),
        })
    }
}

impl RpcProcess for LocalRpcProcess {
    async fn send(&mut self, record: String) -> Result<(), BackendError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Control::Write(record, tx))
            .await
            .map_err(|_error| BackendError::UnexpectedExit)?;
        rx.await.map_err(|_error| BackendError::UnexpectedExit)?
    }

    async fn next_record(&mut self) -> Result<Option<String>, BackendError> {
        self.records.recv().await.transpose()
    }

    async fn finish(&mut self, abort: bool) -> Result<(), BackendError> {
        // Continue draining stdout to a sink during shutdown, even after the
        // protocol consumer has its final result. Never stall child disposal.
        self.records.close();
        let _ = self.commands.send(Control::Finish(abort)).await;
        if let Some(supervisor) = self.supervisor.take() {
            supervisor
                .await
                .map_err(|_error| BackendError::Protocol("process supervisor failed"))??;
        }
        Ok(())
    }
}

struct Readers {
    stdout: JoinHandle<()>,
    stderr: JoinHandle<()>,
}
impl Drop for Readers {
    fn drop(&mut self) {
        self.stdout.abort();
        self.stderr.abort();
    }
}

struct OwnedChild {
    child: Child,
    pid: Option<u32>,
    _readers: Readers,
}
impl OwnedChild {
    fn kill_group(&self) {
        #[cfg(unix)]
        if let Some(pid) = self
            .pid
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Synchronous last resort if the runtime tears down the supervisor.
        self.kill_group();
    }
}

async fn write_record(stdin: &mut ChildStdin, record: &str) -> Result<(), BackendError> {
    stdin.write_all(record.as_bytes()).await?;
    stdin.write_all(b"\n").await?;
    stdin.flush().await?;
    Ok(())
}

async fn supervise(
    mut owned: OwnedChild,
    mut stdin: ChildStdin,
    mut commands: mpsc::Receiver<Control>,
    cancellation: CancellationToken,
    spec: ProcessSpec,
) -> Result<(), BackendError> {
    let abort = drive_process(&mut owned, &mut stdin, &mut commands, &cancellation, &spec).await?;
    shutdown_child(&mut owned, stdin, abort, &spec).await
}

async fn drive_process(
    owned: &mut OwnedChild,
    stdin: &mut ChildStdin,
    commands: &mut mpsc::Receiver<Control>,
    cancellation: &CancellationToken,
    spec: &ProcessSpec,
) -> Result<bool, BackendError> {
    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(true),
            status = owned.child.wait() => {
                let status = status?;
                tracing::info!(attempt_id = %spec.attempt_id, pid = ?owned.pid, %status, "unexpected agent process exit");
                return Err(BackendError::UnexpectedExit);
            }
            command = commands.recv() => {
                if let Some(abort) = handle_control(command, stdin, spec.shutdown_grace).await {
                    return Ok(abort);
                }
            }
        }
    }
}

async fn handle_control(
    command: Option<Control>,
    stdin: &mut ChildStdin,
    grace: Duration,
) -> Option<bool> {
    match command {
        Some(Control::Finish(abort)) => Some(abort),
        Some(Control::Write(record, response)) => {
            let result = tokio::time::timeout(grace, write_record(stdin, &record))
                .await
                .unwrap_or(Err(BackendError::Timeout));
            let _ = response.send(result);
            None
        }
        None => Some(true),
    }
}

async fn shutdown_child(
    owned: &mut OwnedChild,
    mut stdin: ChildStdin,
    abort: bool,
    spec: &ProcessSpec,
) -> Result<(), BackendError> {
    if abort {
        tracing::info!(attempt_id = %spec.attempt_id, pid = ?owned.pid, "agent abort requested");
        if let Some(record) = &spec.abort_record {
            let _ =
                tokio::time::timeout(spec.shutdown_grace, write_record(&mut stdin, record)).await;
        }
    }
    drop(stdin); // Close input to request orderly shutdown.
    let status =
        if let Ok(status) = tokio::time::timeout(spec.shutdown_grace, owned.child.wait()).await {
            status?
        } else {
            owned.kill_group();
            owned.child.kill().await?;
            owned.child.wait().await?
        };
    tracing::info!(attempt_id = %spec.attempt_id, pid = ?owned.pid, %status, "agent process exit");
    if !abort && !status.success() {
        return Err(BackendError::UnexpectedExit);
    }
    Ok(())
}

/// Bound each wire record, including unknown/verbose tool events, before parsing.
const MAX_RECORD_BYTES: u64 = 8 * 1024 * 1024;
async fn read_records(
    stdout: tokio::process::ChildStdout,
    tx: mpsc::Sender<Result<String, BackendError>>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut bytes = Vec::new();
        let result = (&mut reader)
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut bytes)
            .await;
        let record = match result {
            Ok(0) => return,
            Ok(size) if u64::try_from(size).unwrap_or(u64::MAX) > MAX_RECORD_BYTES => {
                Err(BackendError::Protocol("RPC record exceeds size limit"))
            }
            Ok(_) if !bytes.ends_with(b"\n") => {
                Err(BackendError::Protocol("unterminated JSONL record"))
            }
            Ok(_) => String::from_utf8(bytes)
                .map_err(|_error| BackendError::Protocol("non-UTF8 RPC record")),
            Err(error) => Err(error.into()),
        };
        let failed = record.is_err();
        if tx.send(record).await.is_err() {
            let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
            return;
        }
        if failed {
            return;
        }
    }
}
