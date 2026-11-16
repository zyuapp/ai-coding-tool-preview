//! A device's screen while a view watches it: the host streams it, a thread of its own decodes it,
//! and the newest picture waits for the view's next frame (use-simulator-stream.ts).

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use super::frames::SimulatorFrame;
use super::host::{FrameSubscription, SimulatorHost};

#[cfg(target_os = "macos")]
pub use super::decoder::Picture;

/// Where a stream stands, for the view to say.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamStatus {
    Connecting,
    /// The screen's size in pixels, as the device draws it upright.
    Showing { width: u32, height: u32 },
    Failed(String),
}

struct Shared {
    #[cfg(target_os = "macos")]
    latest: Mutex<Option<Picture>>,
    status: Mutex<StreamStatus>,
    turns: AtomicU8,
    /// A picture or status arrived that the view has not drawn yet.
    fresh: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn changed(&self) {
        if !self.fresh.swap(true, Ordering::AcqRel) {
            (self.notify)();
        }
    }

    fn set_status(&self, status: StreamStatus) {
        let mut current = self.status.lock().unwrap();
        if *current != status {
            *current = status;
            drop(current);
            self.changed();
        }
    }
}

/// Watches one device until dropped. `notify` runs on another thread whenever there is something new
/// to draw, at most once until the view reads it.
pub struct ScreenStream {
    udid: String,
    host: SimulatorHost,
    shared: Arc<Shared>,
    _subscription: FrameSubscription,
}

impl ScreenStream {
    pub fn start(host: &SimulatorHost, udid: &str, notify: impl Fn() + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            #[cfg(target_os = "macos")]
            latest: Mutex::new(None),
            status: Mutex::new(StreamStatus::Connecting),
            turns: AtomicU8::new(0),
            fresh: AtomicBool::new(false),
            notify: Box::new(notify),
        });
        let (subscription, frames) = host.subscribe_frames(udid);
        let decoding = shared.clone();
        std::thread::Builder::new().name(format!("simulator-{udid}")).spawn(move || decode(frames, decoding)).ok();
        let watching = shared.clone();
        let watcher = host.clone();
        let device = udid.to_string();
        act_host::runtime().spawn(async move {
            if let Err(error) = watcher.watch(&device).await {
                watching.set_status(StreamStatus::Failed(error));
            }
        });
        ScreenStream { udid: udid.to_string(), host: host.clone(), shared, _subscription: subscription }
    }

    pub fn udid(&self) -> &str {
        &self.udid
    }

    /// How far clockwise to turn the device's portrait picture so it reads upright.
    pub fn set_turns(&self, turns: u8) {
        self.shared.turns.store(turns % 4, Ordering::Relaxed);
    }

    pub fn status(&self) -> StreamStatus {
        self.shared.status.lock().unwrap().clone()
    }

    /// The newest picture, already turned; marks it drawn.
    #[cfg(target_os = "macos")]
    pub fn picture(&self) -> Option<Picture> {
        self.shared.fresh.store(false, Ordering::Release);
        self.shared.latest.lock().unwrap().clone()
    }

    pub fn mark_drawn(&self) {
        self.shared.fresh.store(false, Ordering::Release);
    }
}

impl Drop for ScreenStream {
    fn drop(&mut self) {
        let host = self.host.clone();
        let udid = self.udid.clone();
        act_host::runtime().spawn(async move { host.unwatch(&udid).await });
    }
}

#[cfg(target_os = "macos")]
fn decode(frames: std::sync::mpsc::Receiver<SimulatorFrame>, shared: Arc<Shared>) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;
    let mut decoder = super::decoder::Decoder::new();
    let mut rotator: Option<super::decoder::Rotator> = None;
    let mut upright: Option<Picture> = None;
    let mut shown_turns = 0u8;
    loop {
        let frame = match frames.recv_timeout(Duration::from_millis(50)) {
            Ok(frame) => Some(frame),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let mut redraw = false;
        match frame {
            Some(SimulatorFrame::Config { width, height, data, .. }) => {
                if decoder.configure(&data, width, height) {
                    shared.set_status(StreamStatus::Showing { width, height });
                }
            }
            Some(SimulatorFrame::Chunk { key, data, .. }) => {
                if let Some(picture) = decoder.decode(&data, key) {
                    upright = Some(picture);
                    redraw = true;
                }
            }
            None => {}
        }
        // A turn shows at once on the picture already held, rather than when the device next redraws.
        let turns = shared.turns.load(Ordering::Relaxed);
        if turns != shown_turns {
            shown_turns = turns;
            redraw = true;
        }
        let Some(picture) = upright.as_ref().filter(|_| redraw) else { continue };
        let shown = if turns == 0 {
            Some(picture.clone())
        } else {
            if rotator.as_ref().map(|rotator| rotator.turns()) != Some(turns) {
                rotator = super::decoder::Rotator::new(turns);
            }
            rotator.as_mut().and_then(|rotator| rotator.rotate(picture))
        };
        if let Some(shown) = shown {
            *shared.latest.lock().unwrap() = Some(shown);
            shared.changed();
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn decode(frames: std::sync::mpsc::Receiver<SimulatorFrame>, shared: Arc<Shared>) {
    while frames.recv().is_ok() {}
    shared.set_status(StreamStatus::Failed("The simulator is only on macOS.".into()));
}
