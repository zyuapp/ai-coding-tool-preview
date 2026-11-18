//! Lets the display lock and turn off while the app's work continues to run. The computer may
//! idle-sleep once the screen is locked and nothing is left to do, so a quiet app never drains the
//! battery.

use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the lock is looked at; a lock is only ever noticed this late.
const POLL: Duration = Duration::from_secs(5);

/// Holds off idle sleep while it lives, the way a `prevent-app-suspension` blocker does.
struct SleepBlocker(Child);

impl SleepBlocker {
    fn start() -> Option<Self> {
        let mut command = act_platform::tools::sleep_blocker(std::process::id(), act_core::identity::APP_NAME, "A thread is working");
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok().map(SleepBlocker)
    }
}

impl Drop for SleepBlocker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct State {
    locked: bool,
    blocker: Option<SleepBlocker>,
}

pub struct LockAwake {
    working: Arc<dyn Fn() -> bool + Send + Sync>,
    state: Arc<Mutex<State>>,
    watcher: tokio::task::JoinHandle<()>,
}

impl LockAwake {
    /// Watches the lock from now on. `working` says whether any work is still going. Must be called
    /// within a Tokio runtime.
    pub fn start(working: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        let working: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(working);
        let state = Arc::new(Mutex::new(State { locked: false, blocker: None }));
        let watcher = tokio::spawn({
            let (working, state) = (working.clone(), state.clone());
            async move {
                loop {
                    let locked = tokio::task::spawn_blocking(crate::notify::screen_locked).await.unwrap_or(false);
                    apply(&state, locked, working.as_ref());
                    tokio::time::sleep(POLL).await;
                }
            }
        });
        Self { working, state, watcher }
    }

    /// Looks at the work again, after it may have started or finished.
    pub fn update(&self) {
        let locked = self.state.lock().unwrap().locked;
        if locked {
            apply(&self.state, true, self.working.as_ref());
        }
    }

    pub fn stop(&self) {
        self.watcher.abort();
        let mut state = self.state.lock().unwrap();
        state.locked = false;
        state.blocker = None;
    }
}

impl Drop for LockAwake {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Unlocked, nothing is held; locked, sleep is held off only while work remains.
fn apply(state: &Mutex<State>, locked: bool, working: &(dyn Fn() -> bool + Send + Sync)) {
    let mut state = state.lock().unwrap();
    state.locked = locked;
    if locked && working() {
        if state.blocker.is_none() {
            state.blocker = SleepBlocker::start();
        }
    } else {
        state.blocker = None;
    }
}
