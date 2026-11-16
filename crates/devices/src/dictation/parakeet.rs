//! Parakeet TDT through ONNX Runtime, from the ONNX export onnx-asr reads (parakeet-onnx.ts): a
//! preprocessor turns sound into mel features, the encoder turns those into frames, and a joint decoder
//! walks the frames, choosing a token and how many frames to skip at each step.

use std::path::Path;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::{Tensor, ValueType};

use super::models::{PARAKEET_DECODER, PARAKEET_ENCODER, PARAKEET_PREPROCESSOR, PARAKEET_VOCABULARY};

pub const PARAKEET_SAMPLE_RATE: u32 = 16_000;
/// Tokens one frame may emit before the walk moves on regardless.
const MAX_TOKENS_PER_FRAME: usize = 10;
const BLANK: &str = "<blk>";
/// SentencePiece's word boundary.
const SPACE: &str = "▁";

/// Each line of vocab.txt is a piece and its id.
pub fn parakeet_vocabulary(text: &str) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let Some(at) = line.rfind(' ').filter(|at| *at > 0) else { continue };
        let Ok(id) = line[at + 1..].trim_end_matches('\r').parse::<usize>() else { continue };
        if pieces.len() <= id {
            pieces.resize(id + 1, String::new());
        }
        pieces[id] = line[..at].to_string();
    }
    pieces
}

/// Pieces back into words.
pub fn parakeet_text(pieces: &[&str]) -> String {
    pieces.concat().replace(SPACE, " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn argmax(values: &[f32]) -> usize {
    let mut best = 0;
    for (index, value) in values.iter().enumerate().skip(1) {
        if *value > values[best] {
            best = index;
        }
    }
    best
}

/// The size of a fixed axis an input declares, or the fallback when it doesn't say.
fn declared(session: &Session, input: &str, axis: usize, fallback: i64) -> i64 {
    session
        .inputs()
        .iter()
        .find(|outlet| outlet.name() == input)
        .and_then(|outlet| match outlet.dtype() {
            ValueType::Tensor { shape, .. } => shape.get(axis).copied().filter(|size| *size > 0),
            _ => None,
        })
        .unwrap_or(fallback)
}

/// The three graphs, loaded once; each transcription after that reuses them.
pub struct Parakeet {
    preprocessor: Session,
    encoder: Session,
    decoder: Session,
    vocabulary: Vec<String>,
    blank: usize,
    layers: i64,
    hidden: i64,
    _environment: ort::environment::Environment,
}

fn failed(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// ONNX Runtime's shared library: the one `ORT_DYLIB_PATH` names, else the one shipped beside this
/// program, else the system's.
#[cfg(target_os = "linux")]
fn runtime_library() -> std::path::PathBuf {
    const NAME: &str = "libonnxruntime.so.1";
    if let Some(path) = std::env::var_os("ORT_DYLIB_PATH").filter(|path| !path.is_empty()) {
        return path.into();
    }
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(NAME)));
    beside.filter(|path| path.is_file()).unwrap_or_else(|| NAME.into())
}

impl Parakeet {
    pub fn load(folder: &Path, threads: usize) -> Result<Self, String> {
        #[cfg(target_os = "linux")]
        let runtime = ort::init_from(runtime_library()).map_err(failed)?;
        #[cfg(not(target_os = "linux"))]
        let runtime = ort::init();
        let environment = runtime.with_name("aic-dictation").build().map_err(failed)?;
        let open = |name: &str| -> Result<Session, String> {
            Session::builder(&environment)
                .map_err(failed)?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(failed)?
                .with_intra_threads(threads)
                .map_err(failed)?
                .with_inter_threads(1)
                .map_err(failed)?
                .commit_from_file(folder.join(name))
                .map_err(failed)
        };
        let preprocessor = open(PARAKEET_PREPROCESSOR)?;
        let encoder = open(PARAKEET_ENCODER)?;
        let decoder = open(PARAKEET_DECODER)?;
        let vocabulary = parakeet_vocabulary(&std::fs::read_to_string(folder.join(PARAKEET_VOCABULARY)).map_err(failed)?);
        let blank = vocabulary.iter().position(|piece| piece == BLANK).ok_or("The speech model's vocabulary has no blank.")?;
        let layers = declared(&decoder, "input_states_1", 0, 2);
        let hidden = declared(&decoder, "input_states_1", 2, 640);
        Ok(Parakeet { preprocessor, encoder, decoder, vocabulary, blank, layers, hidden, _environment: environment })
    }

    /// 16 kHz mono samples into words.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String, String> {
        let length = samples.len();
        let features = self
            .preprocessor
            .run(ort::inputs![
                "waveforms" => Tensor::from_array(([1usize, length], samples.to_vec())).map_err(failed)?,
                "waveforms_lens" => Tensor::from_array(([1usize], vec![length as i64])).map_err(failed)?,
            ])
            .map_err(failed)?;
        let (feature_shape, feature_data) = features["features"].try_extract_tensor::<f32>().map_err(failed)?;
        let feature_shape: Vec<usize> = feature_shape.iter().map(|size| *size as usize).collect();
        let feature_data = feature_data.to_vec();
        let (_, feature_lengths) = features["features_lens"].try_extract_tensor::<i64>().map_err(failed)?;
        let feature_lengths = feature_lengths.to_vec();
        drop(features);

        let encoded = self
            .encoder
            .run(ort::inputs![
                "audio_signal" => Tensor::from_array((feature_shape, feature_data)).map_err(failed)?,
                "length" => Tensor::from_array(([feature_lengths.len()], feature_lengths)).map_err(failed)?,
            ])
            .map_err(failed)?;
        let (frame_shape, frames) = encoded["outputs"].try_extract_tensor::<f32>().map_err(failed)?;
        let (width, stride) = (frame_shape.get(1).copied().unwrap_or(0) as usize, frame_shape.get(2).copied().unwrap_or(0) as usize);
        let frames = frames.to_vec();
        let (_, lengths) = encoded["encoded_lengths"].try_extract_tensor::<i64>().map_err(failed)?;
        let frame_count = lengths.first().copied().unwrap_or(0).max(0) as usize;
        drop(encoded);

        let state_size = (self.layers * self.hidden) as usize;
        let state_shape = [self.layers as usize, 1, self.hidden as usize];
        let mut states = (vec![0.0f32; state_size], vec![0.0f32; state_size]);
        let tokens = self.vocabulary.len();
        let mut said: Vec<usize> = Vec::new();
        let mut frame_tokens = 0;
        let mut frame = vec![0.0f32; width];
        let mut at = 0;
        while at < frame_count {
            for (index, value) in frame.iter_mut().enumerate() {
                *value = frames[index * stride + at];
            }
            let last = said.last().copied().unwrap_or(self.blank) as i32;
            let step = self
                .decoder
                .run(ort::inputs![
                    "encoder_outputs" => Tensor::from_array(([1usize, width, 1], frame.clone())).map_err(failed)?,
                    "targets" => Tensor::from_array(([1usize, 1], vec![last])).map_err(failed)?,
                    "target_length" => Tensor::from_array(([1usize], vec![1i32])).map_err(failed)?,
                    "input_states_1" => Tensor::from_array((state_shape, states.0.clone())).map_err(failed)?,
                    "input_states_2" => Tensor::from_array((state_shape, states.1.clone())).map_err(failed)?,
                ])
                .map_err(failed)?;
            let (_, logits) = step["outputs"].try_extract_tensor::<f32>().map_err(failed)?;
            let token = argmax(&logits[..tokens.min(logits.len())]);
            let skip = if logits.len() > tokens { argmax(&logits[tokens..]) } else { 0 };
            if token != self.blank {
                let (_, first) = step["output_states_1"].try_extract_tensor::<f32>().map_err(failed)?;
                let (_, second) = step["output_states_2"].try_extract_tensor::<f32>().map_err(failed)?;
                states = (first.to_vec(), second.to_vec());
                said.push(token);
                frame_tokens += 1;
            }
            if skip > 0 {
                at += skip;
                frame_tokens = 0;
            } else if token == self.blank || frame_tokens == MAX_TOKENS_PER_FRAME {
                at += 1;
                frame_tokens = 0;
            }
        }
        let pieces: Vec<&str> = said.iter().map(|id| self.vocabulary.get(*id).map(String::as_str).unwrap_or("")).collect();
        Ok(parakeet_text(&pieces))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_lines_place_pieces_by_id() {
        let pieces = parakeet_vocabulary("<unk> 0\n▁the 2\n▁a b 1\nbad\n");
        assert_eq!(pieces, ["<unk>", "▁a b", "▁the"]);
        assert_eq!(parakeet_text(&["▁hello", "▁wor", "ld", "."]), "hello world.");
    }
}
