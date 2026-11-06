//! A running worker sandbox: its framed input queue, the stderr forwarder, the monitor that
//! turns its stdout into supervisor events, and its teardown.

use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::process::{ChildStderr, ChildStdout, ExitStatus};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lade_protocol::worker::WorkerBody;
use lade_protocol::{FrameIoError, FrameReader, Framing, Message, WorkerEnvelope, encode};

use crate::supervision::child::{ChildStdio, SandboxedChild, lock};
use crate::supervision::gateway::Gateway;
use crate::supervision::input::InputQueue;
use crate::supervision::{LaunchBinding, LaunchError, SupervisorEvent, WorkerControl, WorkerEvent};

use super::{
    SHUTDOWN_GRACE, STDERR_LINE_BYTES, STDERR_LINES_PER_SECOND, WRITE_QUEUE_BYTES,
    WRITE_QUEUE_FRAMES, invalid,
};

pub(super) struct Shared {
    pub(super) child: SandboxedChild,
    pub(super) gateway: Mutex<Option<Gateway>>,
}

impl Shared {
    /// Tears the sandbox down (see [`SandboxedChild::terminate`]) and stops its gateway.
    fn finish(&self) -> Option<ExitStatus> {
        let status = self.child.terminate();
        if let Some(mut gateway) = lock(&self.gateway).take() {
            gateway.stop();
        }
        status
    }
}

pub(super) fn spawn_threads(
    shared: Arc<Shared>,
    binding: LaunchBinding,
    events: SyncSender<SupervisorEvent>,
    stdout: ChildStdout,
    stderr: ChildStderr,
) -> io::Result<()> {
    let label = format!("{}/{}", binding.objective_id, binding.execution_generation);
    thread::Builder::new()
        .name("lade-worker-stderr".into())
        .spawn(move || forward_stderr(stderr, &label, &mut io::stderr()))?;
    thread::Builder::new()
        .name("lade-worker-monitor".into())
        .spawn(move || monitor(&shared, &binding, stdout, &events))?;
    Ok(())
}

pub(super) enum Ending {
    Eof,
    Violation(&'static str),
    ReceiverGone,
}

pub(super) fn monitor(
    shared: &Shared,
    binding: &LaunchBinding,
    stdout: ChildStdout,
    events: &SyncSender<SupervisorEvent>,
) {
    let mut reader = FrameReader::new(stdout, Framing::Worker);
    let event = |event| SupervisorEvent {
        binding: binding.clone(),
        event,
    };
    let ending = loop {
        match reader.read_frame() {
            Ok(Some(frame)) => match WorkerEnvelope::decode(&frame) {
                Ok(envelope) if envelope.body.is_from_worker() => {
                    if events
                        .send(event(WorkerEvent::Body(envelope.body)))
                        .is_err()
                    {
                        break Ending::ReceiverGone;
                    }
                }
                Ok(_) => break Ending::Violation("service_body_from_worker"),
                Err(e) => break Ending::Violation(e.as_str()),
            },
            Ok(None) | Err(FrameIoError::Io(_)) => break Ending::Eof,
            Err(FrameIoError::Wire(e)) => break Ending::Violation(e.as_str()),
        }
    };
    drop(reader);
    match ending {
        Ending::Violation(code) => {
            shared.child.kill();
            let _ = events.send(event(WorkerEvent::ProtocolViolation(code)));
        }
        Ending::ReceiverGone => shared.child.kill(),
        Ending::Eof => drop(shared.child.wait_exit(SHUTDOWN_GRACE)),
    }
    let status = shared.finish();
    let _ = events.send(event(WorkerEvent::Exited {
        code: status.and_then(|s| s.code()),
        signal: status.and_then(|s| s.signal()),
    }));
}

pub(crate) fn forward_stderr(mut from: impl Read, label: &str, out: &mut impl Write) {
    let mut line: Vec<u8> = Vec::with_capacity(STDERR_LINE_BYTES);
    let mut cut = false;
    let mut window = Instant::now();
    let mut emitted = 0u32;
    let mut dropped = 0u64;
    let mut emit = |line: &[u8], cut: bool| {
        if window.elapsed() >= Duration::from_secs(1) {
            if dropped > 0 {
                let _ = writeln!(
                    out,
                    "lade-worker[{label}]: ({dropped} stderr lines dropped)"
                );
            }
            window = Instant::now();
            emitted = 0;
            dropped = 0;
        }
        if emitted >= STDERR_LINES_PER_SECOND {
            dropped += 1;
            return;
        }
        emitted += 1;
        let text: String = String::from_utf8_lossy(line)
            .chars()
            .map(|c| if c.is_control() && c != '\t' { '?' } else { c })
            .collect();
        let _ = writeln!(
            out,
            "lade-worker[{label}]: {text}{}",
            if cut { " …" } else { "" }
        );
    };
    let mut buffer = [0u8; 4096];
    loop {
        let n = match from.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &byte in &buffer[..n] {
            if byte == b'\n' {
                emit(&line, cut);
                line.clear();
                cut = false;
            } else if line.len() < STDERR_LINE_BYTES {
                line.push(byte);
            } else {
                cut = true;
            }
        }
    }
    if !line.is_empty() {
        emit(&line, cut);
    }
}

pub struct SandboxWorker {
    shared: Arc<Shared>,
    input: InputQueue,
    terminated: bool,
}

impl SandboxWorker {
    /// Starts the writer, stderr and monitor threads of a spawned worker sandbox. On failure the
    /// worker's sandbox is torn down when its last handle drops.
    pub(super) fn start(
        shared: Arc<Shared>,
        pipes: ChildStdio,
        binding: LaunchBinding,
        events: SyncSender<SupervisorEvent>,
    ) -> Result<Self, LaunchError> {
        let ChildStdio {
            stdin: Some(stdin),
            stdout: Some(stdout),
            stderr: Some(stderr),
        } = pipes
        else {
            return Err(invalid("missing worker pipes"));
        };
        let input = InputQueue::start(
            "lade-worker-write",
            stdin,
            WRITE_QUEUE_FRAMES,
            WRITE_QUEUE_BYTES,
            "worker_queue_full",
        )
        .map_err(LaunchError::Io)?;
        spawn_threads(Arc::clone(&shared), binding, events, stdout, stderr)
            .map_err(LaunchError::Io)?;
        Ok(Self {
            shared,
            input,
            terminated: false,
        })
    }
}

impl fmt::Debug for SandboxWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxWorker")
            .field("terminated", &self.terminated)
            .finish_non_exhaustive()
    }
}

impl WorkerControl for SandboxWorker {
    fn send(&mut self, body: WorkerBody) -> io::Result<()> {
        if body.is_from_worker() {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "not a service-to-worker body",
            ));
        }
        let gone = || io::Error::new(ErrorKind::BrokenPipe, "worker_link_closed");
        if self.terminated || self.input.failed() || self.shared.child.poll_exit().is_some() {
            return Err(gone());
        }
        let frame = encode(&WorkerEnvelope::new(body), Framing::Worker)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, e.as_str()))?;
        self.input.push(frame, gone)
    }

    fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        if self.shared.child.poll_exit().is_none() {
            let _ = self.send(WorkerBody::Shutdown);
        }
        self.terminated = true;
        self.input.close();
        self.shared.child.wait_exit(SHUTDOWN_GRACE);
        self.shared.finish();
    }
}

impl Drop for SandboxWorker {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_is_bounded_and_sanitized() {
        let mut input = Vec::new();
        input.extend_from_slice(b"hello\x1b[31m\n");
        input.extend(std::iter::repeat_n(b'a', 2000));
        input.push(b'\n');
        for i in 0..300 {
            input.extend_from_slice(format!("line {i}\n").as_bytes());
        }
        let mut out = Vec::new();
        forward_stderr(&input[..], "o/1", &mut out);
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "lade-worker[o/1]: hello?[31m");
        assert!(lines[1].ends_with(" …"));
        assert!(lines[1].len() < STDERR_LINE_BYTES + 40);
        assert_eq!(lines.len(), STDERR_LINES_PER_SECOND as usize);
    }
}
