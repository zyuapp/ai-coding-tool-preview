//! How loud the microphone is while dictation listens, kept for the window to draw (dictation-levels.ts).
//! Levels never become workspace state: they arrive many times a second and only the strip and the
//! held microphone's ring draw them.

use std::collections::VecDeque;
use std::time::Instant;

use act_core::domain::platform::DictationPhase;
use futures::StreamExt;
use gpui::{Context, Task};

use super::host::{DictationHost, Listening};

/// How many readings the strip shows, newest on the right.
pub const SAMPLES: usize = 96;

pub struct DictationLevels {
    levels: VecDeque<f64>,
    current: f64,
    phase: Option<DictationPhase>,
    recording_since: Option<Instant>,
    /// The clock where it stopped once the words are being written down.
    frozen: Option<u64>,
    _listening: Option<Listening>,
    _task: Task<()>,
}

impl DictationLevels {
    pub fn new(host: &DictationHost, cx: &mut Context<Self>) -> Self {
        let (sender, mut receiver) = futures::channel::mpsc::unbounded::<f64>();
        let listening = host.on_level(move |level| {
            let _ = sender.unbounded_send(level);
        });
        let task = cx.spawn(async move |this, cx| {
            while let Some(mut level) = receiver.next().await {
                // A burst read late draws once, at its newest reading, with each reading kept.
                let mut burst = vec![level];
                while let Ok(next) = receiver.try_recv() {
                    burst.push(next);
                    level = next;
                }
                let alive = this.update(cx, |this, cx| {
                    for reading in burst {
                        this.push(reading);
                    }
                    this.current = level;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        DictationLevels { levels: VecDeque::from(vec![0.0; SAMPLES]), current: 0.0, phase: None, recording_since: None, frozen: None, _listening: Some(listening), _task: task }
    }

    fn listening(&self) -> bool {
        matches!(self.phase, Some(DictationPhase::Starting | DictationPhase::Recording))
    }

    fn push(&mut self, level: f64) {
        if !self.listening() {
            return;
        }
        self.levels.pop_front();
        self.levels.push_back(level);
    }

    /// Follows the hold's phase: listening starts the strip empty, and the clock runs from when the
    /// microphone first hears. The last readings stay while they are written down.
    pub fn sync(&mut self, phase: Option<DictationPhase>, cx: &mut Context<Self>) {
        if phase == self.phase {
            return;
        }
        let was_listening = self.listening();
        self.phase = phase;
        if self.listening() && !was_listening {
            self.levels = VecDeque::from(vec![0.0; SAMPLES]);
        }
        match phase {
            Some(DictationPhase::Recording) => {
                self.frozen = None;
                self.recording_since = Some(Instant::now());
            }
            Some(DictationPhase::Transcribing) => self.frozen = self.recording_since.take().map(|since| since.elapsed().as_secs()),
            _ => {}
        }
        if phase.is_none() {
            self.current = 0.0;
        }
        cx.notify();
    }

    /// A reading from somewhere other than this computer's helper, such as a paired phone.
    pub fn push_level(&mut self, level: f64, cx: &mut Context<Self>) {
        self.push(level);
        self.current = level;
        cx.notify();
    }

    pub fn levels(&self) -> impl Iterator<Item = f64> + '_ {
        self.levels.iter().copied()
    }

    pub fn current(&self) -> f64 {
        self.current
    }

    /// Whole seconds since the microphone started hearing, as `m:ss`.
    pub fn elapsed(&self) -> String {
        let seconds = self.recording_since.map(|since| since.elapsed().as_secs()).or(self.frozen).unwrap_or(0);
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}
