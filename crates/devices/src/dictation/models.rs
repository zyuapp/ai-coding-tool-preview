//! Where each computer keeps the speech model its dictation helper reads, and which files make it whole
//! (dictation-models.ts).

use std::path::{Path, PathBuf};

/// The ONNX export of Parakeet TDT 0.6B v2 the ONNX helper runs.
pub const PARAKEET_PREPROCESSOR: &str = "nemo128.onnx";
pub const PARAKEET_ENCODER: &str = "encoder-model.int8.onnx";
pub const PARAKEET_DECODER: &str = "decoder_joint-model.int8.onnx";
pub const PARAKEET_VOCABULARY: &str = "vocab.txt";
pub const ONNX_MODEL_FILES: [&str; 4] = [PARAKEET_PREPROCESSOR, PARAKEET_ENCODER, PARAKEET_DECODER, PARAKEET_VOCABULARY];

/// Parakeet v2 for Core ML, as FluidAudio lays it out.
pub const MAC_MODEL_FILES: [&str; 5] = ["Preprocessor.mlmodelc", "Encoder.mlmodelc", "Decoder.mlmodelc", "JointDecision.mlmodelc", "parakeet_vocab.json"];

#[derive(Clone, Debug, PartialEq)]
pub struct DictationModelPlace {
    /// The folder the helper reads unless it is handed another.
    pub folder: PathBuf,
    /// What must be in the folder for the model to load.
    pub files: Vec<&'static str>,
}

impl DictationModelPlace {
    pub fn complete_in(&self, folder: &Path) -> bool {
        self.files.iter().all(|file| folder.join(file).exists())
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// `$XDG_DATA_HOME` when it is absolute, else `~/.local/share`.
pub fn linux_data_home() -> PathBuf {
    match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        Some(named) if named.is_absolute() => named,
        _ => home().join(".local/share"),
    }
}

/// The ONNX model's folder in the user's data folder.
pub fn onnx_model_folder() -> PathBuf {
    linux_data_home().join("aicodingtool/models/parakeet-tdt-0.6b-v2-int8")
}

/// Where this computer's model goes, or None where no helper runs.
pub fn dictation_model_place(os: &str, arch: &str) -> Option<DictationModelPlace> {
    match (os, arch) {
        ("macos", "aarch64") => Some(DictationModelPlace {
            folder: home().join("Library/Application Support/FluidAudio/Models/parakeet-tdt-0.6b-v2-coreml"),
            files: MAC_MODEL_FILES.to_vec(),
        }),
        ("linux", "x86_64" | "aarch64") => Some(DictationModelPlace { folder: onnx_model_folder(), files: ONNX_MODEL_FILES.to_vec() }),
        _ => None,
    }
}

/// This computer's place for the model.
pub fn this_computers_place() -> Option<DictationModelPlace> {
    dictation_model_place(std::env::consts::OS, std::env::consts::ARCH)
}
