//! Holding to talk without Core ML (dictation-linux-helper.ts): records the microphone through PipeWire,
//! PulseAudio or ALSA, and transcribes speech with Parakeet through ONNX Runtime. It speaks the macOS
//! helper's lines, so `DictationHost` drives both alike.
//!
//! In:  {"op":"download"} {"op":"start","id"} {"op":"stop","id"} {"op":"cancel","id"} {"op":"transcribe","id","path"}
//! Out: {"type":"model","state":"ready"|"downloading"|"failed","message"?,"progress"?}
//!      {"type":"started","id"} {"type":"level","id","value"} {"type":"text","id","text"}
//!      {"type":"cancelled","id"} {"type":"error","id"?,"message"}

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use act_devices::dictation::audio::{audio_level, pcm16_samples, resampled, wav_samples};
use act_devices::dictation::models::{onnx_model_folder, ONNX_MODEL_FILES, PARAKEET_DECODER, PARAKEET_ENCODER, PARAKEET_PREPROCESSOR, PARAKEET_VOCABULARY};
use act_devices::dictation::parakeet::{Parakeet, PARAKEET_SAMPLE_RATE as SAMPLE_RATE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Five minutes; anything past it is dropped.
const MAX_RECORDING_BYTES: usize = SAMPLE_RATE as usize * 2 * 300;
const LEVEL_INTERVAL: Duration = Duration::from_millis(50);
/// How long a recorder has to hear anything before the next one is tried.
const FIRST_SOUND: Duration = Duration::from_millis(2_500);
const STOP_GRACE: Duration = Duration::from_millis(1_000);

/// Parakeet TDT 0.6B v2's ONNX export, pinned to one revision, with each file's size and checksum.
const MODEL_SOURCE: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/resolve/0bbb45a3365852604aef28b538a8f066f4ccaa85";
const MODEL_DOWNLOADS: [(&str, u64, &str); 4] = [
    (PARAKEET_PREPROCESSOR, 139_764, "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f"),
    (PARAKEET_ENCODER, 652_184_014, "3e0581fda6ab843888b51e56d7ee78b6d5bc3237ec113af1f732d1d5286aa155"),
    (PARAKEET_DECODER, 8_998_286, "a449f49acd68979d418651dd2dcb737cc0f1bf0225e009e29ee326354edbf7d3"),
    (PARAKEET_VOCABULARY, 9_384, "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d"),
];
/// Where a download is put together, inside the model's folder so a cancelled one goes with it.
const STAGING: &str = ".download";

/// Raw 16 kHz mono 16-bit sound on stdout, from whichever sound system is there, in this order.
const RECORDERS: [(&str, &[&str]); 3] = [
    ("pw-record", &["--rate", "16000", "--channels", "1", "--format", "s16", "--raw", "-"]),
    ("parecord", &["--raw", "--rate=16000", "--channels=1", "--format=s16le"]),
    ("arecord", &["-q", "-t", "raw", "-f", "S16_LE", "-r", "16000", "-c", "1"]),
];
const NO_RECORDER: &str = "Dictation needs pw-record, parecord or arecord to hear the microphone.";

static OUTPUT: Mutex<()> = Mutex::new(());

fn emit(message: Value) {
    let _lock = OUTPUT.lock().unwrap();
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn fail(id: Option<&str>, message: &str) {
    match id {
        Some(id) => emit(json!({ "type": "error", "id": id, "message": message })),
        None => emit(json!({ "type": "error", "message": message })),
    }
}

fn model_complete(folder: &Path) -> bool {
    ONNX_MODEL_FILES.iter().all(|file| std::fs::metadata(folder.join(file)).map(|meta| meta.len() > 0).unwrap_or(false))
}

type Job = (Vec<f32>, mpsc::Sender<Result<String, String>>);

/// The model on a thread of its own, loading or loaded; transcriptions wait for it there. A model that
/// failed once stays failed on its thread, so the next hold starts a fresh one.
struct Model {
    jobs: mpsc::Sender<Job>,
    alive: Arc<AtomicBool>,
}

impl Model {
    fn start(folder: PathBuf) -> Model {
        let (jobs, incoming) = mpsc::channel::<Job>();
        let alive = Arc::new(AtomicBool::new(true));
        let running = alive.clone();
        std::thread::spawn(move || {
            let threads = std::thread::available_parallelism().map(|count| count.get() / 2).unwrap_or(1).clamp(1, 4);
            let mut model = Parakeet::load(&folder, threads).map_err(|error| format!("The speech model could not run: {error}"));
            for (samples, reply) in incoming {
                let outcome = match &mut model {
                    Ok(model) => model.transcribe(&samples).map_err(|error| format!("The speech model could not run: {error}")),
                    Err(error) => Err(error.clone()),
                };
                let failed = outcome.is_err();
                let _ = reply.send(outcome);
                if failed {
                    break;
                }
            }
            running.store(false, Ordering::Release);
        });
        Model { jobs, alive }
    }
}

struct Helper {
    folder: PathBuf,
    model: Option<Model>,
    recording: Option<Recording>,
}

impl Helper {
    /// Starts loading the model once, off this thread, so recording goes ahead while it loads.
    fn load_model(&mut self) -> Result<&Model, String> {
        if !model_complete(&self.folder) {
            return Err("The speech model is not downloaded.".into());
        }
        if !self.model.as_ref().is_some_and(|model| model.alive.load(Ordering::Acquire)) {
            self.model = Some(Model::start(self.folder.clone()));
        }
        Ok(self.model.as_ref().unwrap())
    }

    /// Under a quarter second is nothing said; under a second is padded with silence, which Parakeet needs.
    fn transcribe(&mut self, sound: &[f32], rate: u32) -> Result<String, String> {
        let mut samples = resampled(sound, rate, SAMPLE_RATE);
        if samples.len() < SAMPLE_RATE as usize / 4 {
            return Ok(String::new());
        }
        if samples.len() < SAMPLE_RATE as usize {
            samples.resize(SAMPLE_RATE as usize, 0.0);
        }
        let (reply, answer) = mpsc::channel();
        let model = self.load_model()?;
        model.jobs.send((samples, reply)).map_err(|_| "The speech model stopped.".to_string())?;
        answer.recv().map_err(|_| "The speech model stopped.".to_string())?
    }

    fn start(&mut self, id: &str) {
        if self.recording.is_some() {
            return fail(Some(id), "Dictation is already recording.");
        }
        if let Err(error) = self.load_model() {
            return fail(Some(id), &error);
        }
        match Recording::open(id) {
            Ok(recording) => {
                self.recording = Some(recording);
                emit(json!({ "type": "started", "id": id }));
            }
            Err(error) => fail(Some(id), &error),
        }
    }

    fn stop(&mut self, id: &str) {
        let Some(recording) = self.recording.take_if(|recording| recording.id == id) else {
            return fail(Some(id), "Nothing is recording.");
        };
        let samples = recording.stop();
        match self.transcribe(&samples, SAMPLE_RATE) {
            Ok(text) => emit(json!({ "type": "text", "id": id, "text": text })),
            Err(error) => fail(Some(id), &error),
        }
    }

    fn cancel(&mut self, id: &str) {
        if let Some(recording) = self.recording.take_if(|recording| recording.id == id) {
            recording.stop();
        }
        emit(json!({ "type": "cancelled", "id": id }));
    }

    fn transcribe_file(&mut self, id: &str, file: &str) {
        let outcome = std::fs::read(file)
            .map_err(|error| error.to_string())
            .and_then(|bytes| wav_samples(&bytes).map_err(|error| error.to_string()))
            .and_then(|(samples, rate)| self.transcribe(&samples, rate));
        match outcome {
            Ok(text) => emit(json!({ "type": "text", "id": id, "text": text })),
            Err(error) => fail(Some(id), &error),
        }
    }
}

/// One recording: its sound so far, and the levels it reports while it lasts.
struct Recording {
    id: String,
    child: Child,
    sound: Arc<Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

fn recorder(command: &str, args: &[&str]) -> std::io::Result<Child> {
    let mut process = Command::new(command);
    process.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // A recorder this helper leaves behind would keep the microphone open, so it goes with it.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe and only sets this child's own parent-death signal.
        unsafe {
            process.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
    }
    process.spawn()
}

impl Recording {
    /// Tries each recorder until one hears something.
    fn open(id: &str) -> Result<Recording, String> {
        let mut refusal: Option<String> = None;
        for (command, args) in RECORDERS {
            let mut child = match recorder(command, args) {
                Ok(child) => child,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    refusal.get_or_insert(format!("The microphone could not start: {error}"));
                    continue;
                }
            };
            let mut stdout = child.stdout.take().expect("piped stdout");
            let mut stderr = child.stderr.take().expect("piped stderr");
            let said = Arc::new(Mutex::new(String::new()));
            let collecting = said.clone();
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = stderr.read_to_string(&mut text);
                *collecting.lock().unwrap() = text;
            });
            let sound = Arc::new(Mutex::new(Vec::new()));
            let (heard, first) = mpsc::channel::<()>();
            let keeping = sound.clone();
            let hold = id.to_string();
            let reader = std::thread::spawn(move || {
                let mut chunk = vec![0u8; 8192];
                let mut pending: Vec<u8> = Vec::new();
                let mut last_level = Instant::now() - LEVEL_INTERVAL;
                let mut live = false;
                loop {
                    let read = match stdout.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => read,
                    };
                    if !live {
                        live = true;
                        let _ = heard.send(());
                    }
                    {
                        let mut sound = keeping.lock().unwrap();
                        let room = MAX_RECORDING_BYTES.saturating_sub(sound.len());
                        sound.extend_from_slice(&chunk[..read.min(room)]);
                    }
                    pending.extend_from_slice(&chunk[..read]);
                    if last_level.elapsed() >= LEVEL_INTERVAL {
                        last_level = Instant::now();
                        let level = audio_level(&pcm16_samples(&pending));
                        pending.clear();
                        emit(json!({ "type": "level", "id": hold, "value": level }));
                    }
                }
            });
            match first.recv_timeout(FIRST_SOUND) {
                Ok(()) => return Ok(Recording { id: id.to_string(), child, sound, reader: Some(reader) }),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    refusal.get_or_insert("The microphone did not start.".into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.wait();
                    std::thread::sleep(Duration::from_millis(50));
                    let text = said.lock().unwrap().clone();
                    let last = text.trim().lines().last().map(str::to_string);
                    refusal.get_or_insert(match last {
                        Some(last) => format!("The microphone could not start: {last}"),
                        None => "The microphone could not start.".into(),
                    });
                }
            }
        }
        Err(refusal.unwrap_or_else(|| NO_RECORDER.into()))
    }

    /// Stops the recorder and waits for the last of its sound.
    fn stop(mut self) -> Vec<f32> {
        // SAFETY: signals this recorder's own process, which this helper started and still owns.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGINT) };
        let until = Instant::now() + STOP_GRACE;
        while Instant::now() < until {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let sound = self.sound.lock().unwrap();
        pcm16_samples(&sound)
    }
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);

fn reason(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// One file into the staging folder, checked against its pinned checksum as it arrives.
fn fetch_model_file(file: &str, sha256: &str, staging: &Path, arrived: &mut dyn FnMut(u64)) -> Result<(), String> {
    let response = ureq::get(&format!("{MODEL_SOURCE}/{file}")).call().map_err(|error| match error {
        ureq::Error::StatusCode(status) => format!("The download was refused ({status})."),
        other => reason(other),
    })?;
    let mut reader = response.into_body().into_reader();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(staging.join(file)).map_err(reason)?;
    let mut hash = Sha256::new();
    let mut chunk = vec![0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut chunk).map_err(reason)?;
        if read == 0 {
            break;
        }
        hash.update(&chunk[..read]);
        output.write_all(&chunk[..read]).map_err(reason)?;
        arrived(read as u64);
    }
    output.flush().map_err(reason)?;
    if hex::encode(hash.finalize()) != sha256 {
        return Err("The downloaded speech model didn't match its checksum.".into());
    }
    Ok(())
}

/// Reports whole percents while the files arrive; the model is ready once every one is checked and in place.
fn download(folder: PathBuf) {
    if DOWNLOADING.swap(true, Ordering::AcqRel) {
        return;
    }
    emit(json!({ "type": "model", "state": "downloading", "progress": 0 }));
    let staging = folder.join(STAGING);
    let total: u64 = MODEL_DOWNLOADS.iter().map(|(_, bytes, _)| bytes).sum();
    let mut received = 0u64;
    let mut reported = 0u64;
    let mut arrived = |bytes: u64| {
        received += bytes;
        let percent = (received * 100 / total).min(99);
        if percent != reported {
            reported = percent;
            emit(json!({ "type": "model", "state": "downloading", "progress": percent as f64 / 100.0 }));
        }
    };
    let outcome = (|| -> Result<(), String> {
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(reason)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700));
        }
        for (file, _, sha256) in MODEL_DOWNLOADS {
            fetch_model_file(file, sha256, &staging, &mut arrived)?;
        }
        for (file, _, _) in MODEL_DOWNLOADS {
            std::fs::rename(staging.join(file), folder.join(file)).map_err(reason)?;
        }
        let _ = std::fs::remove_dir_all(&staging);
        Ok(())
    })();
    match outcome {
        Ok(()) => emit(json!({ "type": "model", "state": "ready" })),
        Err(message) => {
            let _ = std::fs::remove_dir_all(&staging);
            emit(json!({ "type": "model", "state": "failed", "message": message }));
        }
    }
    DOWNLOADING.store(false, Ordering::Release);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let folder = args.iter().position(|arg| arg == "--models").and_then(|at| args.get(at + 1)).map(PathBuf::from).unwrap_or_else(onnx_model_folder);
    let folder = std::path::absolute(&folder).unwrap_or(folder);
    // Requests run one at a time, in the order they arrive; a download runs beside them.
    let (requests, incoming) = mpsc::channel::<Value>();
    let worker_folder = folder.clone();
    let worker = std::thread::spawn(move || {
        let mut helper = Helper { folder: worker_folder, model: None, recording: None };
        for request in incoming {
            let id = request.get("id").and_then(Value::as_str);
            match (request.get("op").and_then(Value::as_str), id) {
                (Some("start"), Some(id)) => helper.start(id),
                (Some("stop"), Some(id)) => helper.stop(id),
                (Some("cancel"), Some(id)) => helper.cancel(id),
                (Some("transcribe"), Some(id)) => match request.get("path").and_then(Value::as_str) {
                    Some(path) => helper.transcribe_file(id, path),
                    None => fail(Some(id), "No file to transcribe."),
                },
                (Some(op), id) => fail(id, &format!("Unknown request {op}.")),
                (None, id) => fail(id, "Unknown request undefined."),
            }
        }
        if let Some(recording) = helper.recording.take() {
            recording.stop();
        }
    });
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<Value>(&line) {
            Ok(request @ Value::Object(_)) => {
                if request.get("op").and_then(Value::as_str) == Some("download") {
                    let folder = folder.clone();
                    std::thread::spawn(move || download(folder));
                } else if requests.send(request).is_err() {
                    break;
                }
            }
            _ => fail(None, "Unreadable request."),
        }
    }
    drop(requests);
    let _ = worker.join();
}
