//! What a process guest runs, and the spawner that starts it.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;

/// The command of a process guest: a program, its arguments, environment
/// variables to set and a working directory. A [`Spawner`] decides how it
/// runs.
#[derive(Debug, Clone)]
pub struct GuestCommand {
    program: OsString,
    args: Vec<OsString>,
    envs: Vec<(OsString, OsString)>,
    current_dir: Option<PathBuf>,
}

impl GuestCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self { program: program.as_ref().into(), args: Vec::new(), envs: Vec::new(), current_dir: None }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.args.push(arg.as_ref().into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        self.args.extend(args.into_iter().map(|arg| arg.as_ref().into()));
        self
    }

    /// Sets a variable on top of the environment the spawner gives.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.envs.push((key.as_ref().into(), value.as_ref().into()));
        self
    }

    pub fn current_dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.current_dir = Some(dir.as_ref().into());
        self
    }

    pub fn get_program(&self) -> &OsStr {
        &self.program
    }

    pub fn get_args(&self) -> &[OsString] {
        &self.args
    }

    pub fn get_envs(&self) -> &[(OsString, OsString)] {
        &self.envs
    }

    pub fn get_current_dir(&self) -> Option<&Path> {
        self.current_dir.as_deref()
    }

    /// The command as a [`std::process::Command`], without its stdio.
    pub fn to_std(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).envs(self.envs.iter().map(|(key, value)| (key, value)));
        if let Some(dir) = &self.current_dir {
            command.current_dir(dir);
        }
        command
    }
}

/// Starts the processes of process guests, so a host decides how they run
/// and who owns them. [`StdSpawner`] is the default.
pub trait Spawner: Send + Sync + 'static {
    /// Starts `command` with its stdin and stdout piped; pipe its stderr too
    /// for a tail in the exit report.
    fn spawn(&self, command: &GuestCommand) -> io::Result<GuestProcess>;
}

impl<S: Spawner + ?Sized> Spawner for Arc<S> {
    fn spawn(&self, command: &GuestCommand) -> io::Result<GuestProcess> {
        (**self).spawn(command)
    }
}

/// Runs the command with [`std::process`], in the host's environment plus
/// the command's variables, with all three stdio streams piped.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdSpawner;

impl Spawner for StdSpawner {
    fn spawn(&self, command: &GuestCommand) -> io::Result<GuestProcess> {
        let child = command.to_std().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        GuestProcess::from_child(child)
    }
}

/// A started guest process: its pipes, and the control the guest's instance
/// keeps until the process has ended.
pub struct GuestProcess {
    pub(crate) stdin: Box<dyn Write + Send>,
    pub(crate) stdout: Box<dyn Read + Send>,
    pub(crate) stderr: Option<Box<dyn Read + Send>>,
    pub(crate) child: Box<dyn GuestChild>,
}

impl GuestProcess {
    pub fn new(stdin: impl Write + Send + 'static, stdout: impl Read + Send + 'static, child: impl GuestChild) -> Self {
        Self { stdin: Box::new(stdin), stdout: Box::new(stdout), stderr: None, child: Box::new(child) }
    }

    /// The process's stderr, kept as a tail for the exit report.
    pub fn stderr(mut self, stderr: impl Read + Send + 'static) -> Self {
        self.stderr = Some(Box::new(stderr));
        self
    }

    /// A child spawned with piped stdin and stdout (and stderr, if piped).
    pub fn from_child(mut child: Child) -> io::Result<Self> {
        let missing = |name: &str| io::Error::other(format!("the guest's {name} is not piped"));
        let stdin = child.stdin.take().ok_or_else(|| missing("stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| missing("stdout"))?;
        let stderr = child.stderr.take();
        let process = Self::new(stdin, stdout, child);
        Ok(match stderr {
            Some(stderr) => process.stderr(stderr),
            None => process,
        })
    }
}

/// The control of a guest process, as [`std::process::Child`] has it. The
/// instance kills the process when it is done with it (on dispose, or when
/// the guest fails), and waits for it.
pub trait GuestChild: Send + 'static {
    fn id(&self) -> Option<u32>;

    /// Ends the process, and whatever the spawner started with it. Once it
    /// has ended, this does nothing.
    fn kill(&mut self) -> io::Result<()>;

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;

    /// Waits for the process to end; called again, returns the same status.
    fn wait(&mut self) -> io::Result<ExitStatus>;
}

impl GuestChild for Child {
    fn id(&self) -> Option<u32> {
        Some(Child::id(self))
    }

    fn kill(&mut self) -> io::Result<()> {
        Child::kill(self)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Child::try_wait(self)
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        Child::wait(self)
    }
}
