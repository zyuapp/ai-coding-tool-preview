//! Who holds a data folder: the desktop app (one window per folder, later launches hand their
//! arguments to it) or `aic serve`. Each would write over the other, so one of them yields.
//!
//! The desktop lock is Chromium's `SingletonLock`, a symlink to `<host>-<pid>`, so the Electron build
//! and this one recognise each other's hold on the same folder.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use act_platform::fs::symlink;
use act_platform::ipc::{self, hostname, Listener};
use act_platform::process::alive;

const SERVE_LOCK: &str = "serve.lock";
const APP_LOCK: &str = "SingletonLock";
const APP_SOCKET: &str = "SingletonSocket";

fn holder_of(lock: &Path) -> Option<u32> {
    fs::read_to_string(lock).ok()?.trim().parse().ok().filter(|holder| *holder > 0)
}

/// The process serving from this folder, or None when none is; a lock a dead one left counts for nothing.
pub fn serving_process(user_data: &Path) -> Option<u32> {
    holder_of(&user_data.join(SERVE_LOCK)).filter(|holder| *holder != std::process::id() && alive(*holder))
}

/// Whole and in one step: the lock is a hard link to a file already holding the process id.
fn claim(lock: &Path) -> std::io::Result<bool> {
    let staging = PathBuf::from(format!("{}.{}", lock.to_string_lossy(), std::process::id()));
    fs::write(&staging, std::process::id().to_string())?;
    let linked = fs::hard_link(&staging, lock);
    let _ = fs::remove_file(&staging);
    match linked {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// The pid a `<host>-<pid>` lock names, if this host made it.
fn local_holder(target: &str) -> Option<u32> {
    let (host, pid) = target.rsplit_once('-')?;
    (host == hostname()).then(|| pid.parse().ok()).flatten()
}

/// Whether the desktop app has this folder open. A lock naming a dead process here is one a crash left.
pub fn desktop_open(user_data: &Path) -> bool {
    let lock = user_data.join(APP_LOCK);
    match fs::read_link(&lock) {
        Ok(target) => match local_holder(&target.to_string_lossy()) {
            Some(pid) => alive(pid),
            None => true,
        },
        Err(_) => lock.exists(),
    }
}

/// The turn to clear a dead server's lock: a link naming the starter that has it, made in one step.
fn take_turn(turn: &Path) -> std::io::Result<bool> {
    match symlink(std::process::id().to_string(), turn) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

fn turn_abandoned(turn: &Path) -> bool {
    fs::read_link(turn).ok().and_then(|holder| holder.to_string_lossy().parse::<u32>().ok()).is_some_and(|holder| holder > 0 && !alive(holder))
}

/// Clears a dead server's lock and takes it, one starter at a time.
fn reclaim(user_data: &Path, lock: &Path) -> std::io::Result<bool> {
    let turn = PathBuf::from(format!("{}.reclaim", lock.to_string_lossy()));
    if !take_turn(&turn)? {
        return Ok(false);
    }
    let result = (|| {
        if serving_process(user_data).is_some() {
            return Ok(false);
        }
        let _ = fs::remove_file(lock);
        claim(lock)
    })();
    let _ = fs::remove_file(&turn);
    result
}

/// The serve lock while held; dropping it gives the folder back.
#[derive(Debug)]
pub struct ServeLock {
    lock: PathBuf,
}

impl Drop for ServeLock {
    fn drop(&mut self) {
        if holder_of(&self.lock) == Some(std::process::id()) {
            let _ = fs::remove_file(&self.lock);
        }
    }
}

/// Takes the folder for a server. A second server, or the desktop app, on the same data would write over it.
pub fn claim_serve_lock(user_data: &Path) -> Result<ServeLock, String> {
    if desktop_open(user_data) {
        return Err("The desktop app is open on this data folder. Quit it before serving, or serve from another machine.".into());
    }
    let lock = user_data.join(SERVE_LOCK);
    let claimed = claim(&lock).map_err(|error| error.to_string())?;
    if claimed || (serving_process(user_data).is_none() && reclaim(user_data, &lock).map_err(|error| error.to_string())?) {
        return Ok(ServeLock { lock });
    }
    if let Some(holder) = serving_process(user_data) {
        return Err(format!("{} is already serving from this folder (process {holder}).", act_core::identity::APP_NAME));
    }
    let turn = PathBuf::from(format!("{}.reclaim", lock.to_string_lossy()));
    if turn_abandoned(&turn) {
        return Err(format!("A server starting from this folder died before it finished. Remove {} and try again.", turn.to_string_lossy()));
    }
    Err(concat!("Another ", act_core::app_name!(), " is starting to serve from this folder.").into())
}

/// What claiming the desktop lock found.
pub enum DesktopClaim {
    /// This process holds the folder; later launches' arguments arrive on the listener.
    Claimed(DesktopLock),
    /// Another window holds it and was handed this launch's arguments.
    Forwarded,
    /// Another window holds it and could not be reached.
    Held,
}

/// The desktop lock while held. Dropping it removes the lock and the socket.
pub struct DesktopLock {
    lock: PathBuf,
    socket_link: PathBuf,
    socket_dir: PathBuf,
    listener: Option<Listener>,
}

impl DesktopLock {
    /// Delivers every later launch's arguments to `receive`, on a thread of its own.
    pub fn listen(&mut self, receive: impl Fn(Vec<String>) + Send + 'static) {
        let Some(listener) = self.listener.take() else { return };
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() {
                    if let Ok(arguments) = serde_json::from_str::<Vec<String>>(line.trim_end()) {
                        receive(arguments);
                    }
                }
            }
        });
    }
}

impl Drop for DesktopLock {
    fn drop(&mut self) {
        let ours = format!("{}-{}", hostname(), std::process::id());
        if fs::read_link(&self.lock).is_ok_and(|target| target.to_string_lossy() == ours) {
            let _ = fs::remove_file(&self.lock);
            let _ = fs::remove_file(&self.socket_link);
        }
        let _ = fs::remove_dir_all(&self.socket_dir);
    }
}

/// Claims the folder for this window, or hands `arguments` to the window that already has it.
pub fn claim_desktop_lock(user_data: &Path, arguments: &[String]) -> std::io::Result<DesktopClaim> {
    fs::create_dir_all(user_data)?;
    let lock = user_data.join(APP_LOCK);
    let socket_link = user_data.join(APP_SOCKET);
    let ours = format!("{}-{}", hostname(), std::process::id());
    for _ in 0..2 {
        match symlink(&ours, &lock) {
            Ok(()) => {
                // A socket path is short, so the socket lives in a temporary folder the lock links to.
                let socket_dir = std::env::temp_dir().join(format!("aic-{}", uuid::Uuid::new_v4().simple()));
                fs::create_dir_all(&socket_dir)?;
                let socket = socket_dir.join("S");
                let listener = ipc::bind(&socket)?;
                let _ = fs::remove_file(&socket_link);
                symlink(&socket, &socket_link)?;
                return Ok(DesktopClaim::Claimed(DesktopLock { lock, socket_link, socket_dir, listener: Some(listener) }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let target = fs::read_link(&lock).map(|target| target.to_string_lossy().into_owned()).unwrap_or_default();
                match local_holder(&target) {
                    // A crash left the lock behind; clear it and try once more.
                    Some(pid) if !alive(pid) => {
                        let _ = fs::remove_file(&lock);
                        let _ = fs::remove_file(&socket_link);
                    }
                    _ => {
                        return Ok(match ipc::connect(&socket_link) {
                            Ok(mut stream) => {
                                let mut line = serde_json::to_string(arguments).unwrap_or_else(|_| "[]".into());
                                line.push('\n');
                                if stream.write_all(line.as_bytes()).is_ok() { DesktopClaim::Forwarded } else { DesktopClaim::Held }
                            }
                            Err(_) => DesktopClaim::Held,
                        });
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(DesktopClaim::Held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn a_second_launch_hands_its_arguments_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let DesktopClaim::Claimed(mut lock) = claim_desktop_lock(dir.path(), &[]).unwrap() else { panic!("first claim") };
        assert!(desktop_open(dir.path()));
        let (sender, received) = mpsc::channel();
        lock.listen(move |arguments| sender.send(arguments).unwrap());
        assert!(matches!(claim_desktop_lock(dir.path(), &["/repo".into()]).unwrap(), DesktopClaim::Forwarded));
        assert_eq!(received.recv_timeout(Duration::from_secs(2)).unwrap(), vec!["/repo".to_string()]);
        drop(lock);
        assert!(!desktop_open(dir.path()));
    }

    #[test]
    fn a_crashed_window_leaves_a_lock_that_counts_for_nothing() {
        let dir = tempfile::tempdir().unwrap();
        symlink(format!("{}-999999", hostname()), dir.path().join(APP_LOCK)).unwrap();
        assert!(!desktop_open(dir.path()));
        assert!(matches!(claim_desktop_lock(dir.path(), &[]).unwrap(), DesktopClaim::Claimed(_)));
    }

    #[test]
    fn serving_refuses_an_open_desktop_and_reclaims_a_dead_servers_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SERVE_LOCK), "999999").unwrap();
        let lock = claim_serve_lock(dir.path()).unwrap();
        assert_eq!(holder_of(&dir.path().join(SERVE_LOCK)), Some(std::process::id()));
        drop(lock);
        assert!(!dir.path().join(SERVE_LOCK).exists());
        let _desktop = claim_desktop_lock(dir.path(), &[]).unwrap();
        assert!(claim_serve_lock(dir.path()).unwrap_err().contains("desktop app is open"));
    }
}
