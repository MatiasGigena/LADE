//! A spawnable sandbox command, the pipes its program inherits, and its start gate.

use std::io::{self, PipeReader, PipeWriter, Write};
use std::process::{Child, Command};
use std::sync::{Mutex, PoisonError};

static SPAWN: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub struct SandboxCommand {
    pub(super) command: Command,
    pub(super) inherit: Vec<PipeReader>,
    pub(super) gate: Option<PipeWriter>,
}

impl From<Command> for SandboxCommand {
    fn from(command: Command) -> Self {
        Self {
            command,
            inherit: Vec::new(),
            gate: None,
        }
    }
}

impl SandboxCommand {
    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    /// The command that spawns the sandbox, for inspecting its arguments.
    #[must_use]
    pub fn command(&self) -> &Command {
        &self.command
    }

    /// Spawns the sandbox; its program waits until the returned gate is opened.
    ///
    /// # Errors
    ///
    /// Returns the operating system's error when the descriptors cannot be passed on or the process
    /// cannot be spawned.
    pub fn spawn(self) -> io::Result<(Child, Gate)> {
        let Self {
            mut command,
            inherit,
            gate,
        } = self;
        spawn_inheriting(&inherit, || command.spawn()).map(|child| (child, Gate(gate)))
    }

    /// Spawns a supervisor that runs this sandbox itself, instead of the sandbox directly:
    /// `supervisor` builds that process from the sandbox command, and the sandbox's inherited
    /// descriptors and gate carry over to it.
    ///
    /// # Errors
    ///
    /// Fails as [`SandboxCommand::spawn`] does.
    pub fn spawn_via(
        self,
        supervisor: impl FnOnce(&Command) -> Command,
    ) -> io::Result<(Child, Gate)> {
        let Self {
            command,
            inherit,
            gate,
        } = self;
        let mut supervisor = supervisor(&command);
        spawn_inheriting(&inherit, || supervisor.spawn()).map(|child| (child, Gate(gate)))
    }
}

fn spawn_inheriting(
    inherit: &[PipeReader],
    spawn: impl FnOnce() -> io::Result<Child>,
) -> io::Result<Child> {
    let _serial = SPAWN.lock().unwrap_or_else(PoisonError::into_inner);
    for fd in inherit {
        rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty())?;
    }
    let spawned = spawn();
    for fd in inherit {
        let _ = rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC);
    }
    spawned
}

#[derive(Debug)]
pub struct Gate(Option<PipeWriter>);

impl Gate {
    /// Lets the sandboxed program start. Opening a gate twice does nothing.
    ///
    /// # Errors
    ///
    /// Returns the error from writing to the gate pipe, for example when the sandbox already exited.
    pub fn open(&mut self) -> io::Result<()> {
        match self.0.take() {
            Some(mut writer) => writer.write_all(b"go\n"),
            None => Ok(()),
        }
    }
}
