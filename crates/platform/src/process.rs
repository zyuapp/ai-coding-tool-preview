//! Child processes: every one starts through [`command`] or [`std_command`], which find the program on
//! the held search path and give the child that path and the app's other child variables. Also how
//! children are grouped and signalled, and [`Exec`], which runs one the way Node's `execFile` does.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::sync::RwLock;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

/// Variables every child gets on top of this process's own, besides the search path.
static CHILD_ENV: RwLock<Vec<(&'static str, String)>> = RwLock::new(Vec::new());

/// Gives every child started from now on `key`, without changing this process's own environment.
pub fn set_child_env(key: &'static str, value: impl Into<String>) {
    let value = value.into();
    let mut variables = CHILD_ENV.write().unwrap();
    match variables.iter_mut().find(|(name, _)| *name == key) {
        Some(entry) => entry.1 = value,
        None => variables.push((key, value)),
    }
}

/// What children get on top of this process's environment: the search path, then the app's own.
pub fn child_env() -> Vec<(&'static str, String)> {
    let mut variables = vec![("PATH", crate::search_path::get())];
    variables.extend(CHILD_ENV.read().unwrap().iter().cloned());
    variables
}

/// This process's environment as its children see it.
pub fn environment() -> HashMap<String, String> {
    let mut environment: HashMap<String, String> = std::env::vars().collect();
    environment.extend(child_env().into_iter().map(|(key, value)| (key.to_string(), value)));
    environment
}

/// A bare command name found on the search path; a path, or a name found nowhere, as given.
fn resolve(program: &OsStr) -> Cow<'_, OsStr> {
    let bare = !program.to_string_lossy().chars().any(std::path::is_separator);
    match program.to_str().filter(|_| bare).and_then(crate::search_path::find) {
        Some(found) => Cow::Owned(found.into_os_string()),
        None => Cow::Borrowed(program),
    }
}

/// A child process with the app's search path and child variables.
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::from(std_command(program))
}

/// [`command`] for code that waits on its child synchronously.
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(resolve(program.as_ref()));
    command.envs(child_env());
    command
}

/// The user's own shell and the arguments that make it a login shell, for a terminal.
pub fn terminal_shell() -> (String, &'static [&'static str]) {
    crate::sys::terminal_shell()
}

/// What a child is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
}

/// Signals one process. False when it is gone.
pub fn signal(pid: u32, signal: Signal) -> bool {
    crate::sys::signal(pid, signal)
}

/// Signals the process group `pid` leads. False when there is no such group.
pub fn signal_group(pid: u32, signal: Signal) -> bool {
    crate::sys::signal_group(pid, signal)
}

/// Signals the group `pid` leads, or the process alone when it leads none.
pub fn signal_group_or_process(pid: u32, how: Signal) {
    if !signal_group(pid, how) {
        signal(pid, how);
    }
}

/// Whether a process with this id exists, under any user.
pub fn alive(pid: u32) -> bool {
    crate::sys::alive(pid)
}

/// The signal that ended a process, if one did.
pub fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    crate::sys::exit_signal(status)
}

/// A signal's name as Node reports it, or its number when it has none here.
pub fn signal_name(number: i32) -> String {
    crate::sys::signal_name(number).map(String::from).unwrap_or_else(|| number.to_string())
}

/// Starting a child in a group or session of its own, so signalling the group reaches what it starts.
pub trait Detach {
    /// A process group the child leads.
    fn own_group(&mut self) -> &mut Self;
    /// A session of its own, without the launching terminal.
    fn own_session(&mut self) -> &mut Self;
}

impl Detach for std::process::Command {
    fn own_group(&mut self) -> &mut Self {
        crate::sys::own_group(self);
        self
    }

    fn own_session(&mut self) -> &mut Self {
        crate::sys::new_session(self);
        self
    }
}

impl Detach for tokio::process::Command {
    fn own_group(&mut self) -> &mut Self {
        crate::sys::own_group(self.as_std_mut());
        self
    }

    fn own_session(&mut self) -> &mut Self {
        crate::sys::new_session(self.as_std_mut());
        self
    }
}

/// Starts a launcher detached from this process and reports whether it was there at all.
pub fn launch_detached(command: &str, args: &[String]) -> bool {
    let mut process = std_command(command);
    process.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).own_group();
    match process.spawn() {
        Ok(mut child) => {
            // Reaped on a thread of its own so a launcher that exits never lingers as a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            true
        }
        Err(_) => false,
    }
}

/// Starts `program` with `args` once this process has exited, detached from it, so the new process
/// can claim what this one holds.
pub fn start_after_exit(program: &str, args: &[String]) -> std::io::Result<()> {
    let quoted = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let mut start = quoted(program);
    for argument in args {
        start.push(' ');
        start.push_str(&quoted(argument));
    }
    let script = format!("while kill -0 {} 2>/dev/null; do sleep 0.2; done; {start} >/dev/null 2>&1 &", std::process::id());
    let mut command = std_command("/bin/sh");
    command.args(["-c", &script]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).own_group();
    command.spawn().map(|_| ())
}

/// `execFile`'s default output ceiling.
pub const DEFAULT_MAX_BUFFER: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The program is not installed, or not on the PATH.
    #[error("{program} was not found")]
    NotFound { program: String },
    #[error("{program} timed out")]
    TimedOut { program: String, stdout: Vec<u8>, stderr: Vec<u8> },
    /// One stream outgrew the buffer. The process is killed, as Node kills it.
    #[error("{program} printed more than the buffer holds")]
    MaxBuffer { program: String },
    #[error("{program} exited with {code:?}")]
    Exit { program: String, code: Option<i32>, stdout: Vec<u8>, stderr: Vec<u8> },
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl RunError {
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            RunError::Exit { code, .. } => *code,
            _ => None,
        }
    }

    pub fn stdout(&self) -> &[u8] {
        match self {
            RunError::Exit { stdout, .. } | RunError::TimedOut { stdout, .. } => stdout,
            _ => &[],
        }
    }

    pub fn stderr(&self) -> &[u8] {
        match self {
            RunError::Exit { stderr, .. } | RunError::TimedOut { stderr, .. } => stderr,
            _ => &[],
        }
    }
}

/// One invocation, run as Node's `execFile` runs it: no shell, a bounded timeout, a bounded output
/// buffer, and an error that keeps whatever the process printed before it failed.
pub struct Exec<'a> {
    program: &'a OsStr,
    args: Vec<&'a OsStr>,
    cwd: Option<&'a Path>,
    input: Option<&'a [u8]>,
    timeout: Option<Duration>,
    max_buffer: usize,
    envs: Vec<(&'a str, &'a str)>,
}

impl<'a> Exec<'a> {
    pub fn new(program: &'a (impl AsRef<OsStr> + ?Sized)) -> Self {
        Self { program: program.as_ref(), args: Vec::new(), cwd: None, input: None, timeout: None, max_buffer: DEFAULT_MAX_BUFFER, envs: Vec::new() }
    }

    pub fn args<S: AsRef<OsStr> + ?Sized + 'a>(mut self, args: impl IntoIterator<Item = &'a S>) -> Self {
        self.args.extend(args.into_iter().map(|arg| arg.as_ref()));
        self
    }

    pub fn cwd(mut self, cwd: &'a Path) -> Self {
        self.cwd = Some(cwd);
        self
    }

    pub fn input(mut self, input: Option<&'a [u8]>) -> Self {
        self.input = input;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn max_buffer(mut self, max_buffer: usize) -> Self {
        self.max_buffer = max_buffer;
        self
    }

    pub fn env(mut self, key: &'a str, value: &'a str) -> Self {
        self.envs.push((key, value));
        self
    }

    pub async fn run(self) -> Result<Output, RunError> {
        let program = self.program.to_string_lossy().into_owned();
        let mut command = command(self.program);
        command.args(&self.args).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        command.stdin(if self.input.is_some() { Stdio::piped() } else { Stdio::null() });
        if let Some(cwd) = self.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &self.envs {
            command.env(key, value);
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(RunError::NotFound { program }),
            Err(error) => return Err(error.into()),
        };

        let stdin = child.stdin.take();
        let input = self.input.map(|bytes| bytes.to_vec());
        let writer = async move {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                // A process may exit before reading its input; that shows in its exit code instead.
                let _ = stdin.write_all(&input).await;
                let _ = stdin.shutdown().await;
            }
        };
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let limit = self.max_buffer;
        let work = async {
            let ((), out, err) = tokio::join!(writer, read_bounded(&mut stdout, limit), read_bounded(&mut stderr, limit));
            let (out, out_overflow) = out?;
            let (err, err_overflow) = err?;
            if out_overflow || err_overflow {
                return Ok::<_, std::io::Error>(None);
            }
            Ok(Some((out, err)))
        };

        let collected = match self.timeout {
            Some(timeout) => match tokio::time::timeout(timeout, work).await {
                Ok(result) => result?,
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(RunError::TimedOut { program, stdout: Vec::new(), stderr: Vec::new() });
                }
            },
            None => work.await?,
        };
        let Some((stdout, stderr)) = collected else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(RunError::MaxBuffer { program });
        };
        let status = child.wait().await?;
        if status.success() {
            Ok(Output { stdout, stderr })
        } else {
            Err(RunError::Exit { program, code: status.code(), stdout, stderr })
        }
    }
}

/// Reads to the end, or stops once the stream passes `limit` and says so.
async fn read_bounded(reader: &mut (impl AsyncRead + Unpin), limit: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            return Ok((buffer, false));
        }
        if buffer.len() + read > limit {
            return Ok((buffer, true));
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keeps_output_of_a_failed_process() {
        let error = Exec::new("sh").args(["-c", "printf out; printf err >&2; exit 3"]).run().await.unwrap_err();
        assert_eq!(error.exit_code(), Some(3));
        assert_eq!(error.stdout(), b"out");
        assert_eq!(error.stderr(), b"err");
    }

    #[tokio::test]
    async fn refuses_output_past_the_buffer() {
        let error = Exec::new("sh").args(["-c", "yes | head -c 5000"]).max_buffer(1000).run().await.unwrap_err();
        assert!(matches!(error, RunError::MaxBuffer { .. }));
    }

    #[tokio::test]
    async fn stops_a_process_past_its_timeout() {
        let error = Exec::new("sleep").args(["5"]).timeout(Duration::from_millis(100)).run().await.unwrap_err();
        assert!(matches!(error, RunError::TimedOut { .. }));
    }

    #[test]
    fn a_child_is_found_on_and_sees_the_held_search_path() {
        let folder = tempfile::tempdir().unwrap();
        let probe = folder.path().join("act-search-path-probe");
        std::fs::write(&probe, "#!/bin/sh\nprintf '%s' \"$PATH\"\n").unwrap();
        crate::fs::set_executable(&probe).unwrap();
        let held = folder.path().to_string_lossy().into_owned();
        {
            let mut path = crate::search_path::held().write().unwrap();
            *path = crate::search_path::merge_search_paths([Some(held.as_str()), Some(path.as_str())]);
        }
        let output = std_command("act-search-path-probe").output().unwrap();
        assert!(String::from_utf8_lossy(&output.stdout).starts_with(&held));
        assert!(!std::env::var("PATH").unwrap_or_default().contains(&held));
        assert_eq!(environment().get("PATH"), Some(&crate::search_path::get()));
    }

    #[tokio::test]
    async fn writes_input() {
        let output = Exec::new("cat").input(Some(b"hello")).run().await.unwrap();
        assert_eq!(output.stdout, b"hello");
    }
}
