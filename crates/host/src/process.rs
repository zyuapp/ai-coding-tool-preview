//! Child processes run the way Node's `execFile` runs them: no shell, a bounded timeout, a bounded
//! output buffer, and an error that keeps whatever the process printed before it failed.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

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

/// One invocation. Built with [`Exec::new`] and finished with [`Exec::run`].
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
        let mut command = Command::new(self.program);
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

/// Starts a launcher detached from this process and reports whether it was there at all.
pub fn launch_detached(command: &str, args: &[String]) -> bool {
    let mut process = std::process::Command::new(command);
    process.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
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

    #[tokio::test]
    async fn writes_input() {
        let output = Exec::new("cat").input(Some(b"hello")).run().await.unwrap();
        assert_eq!(output.stdout, b"hello");
    }
}
