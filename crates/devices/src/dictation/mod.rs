//! Holding to talk: the helper that records and transcribes, the speech model's place and download,
//! sound helpers, and the composer's dictation pieces.

pub mod audio;
pub mod effects;
pub mod host;
pub mod levels;
pub mod microphone;
pub mod models;
#[cfg(feature = "onnx")]
pub mod parakeet;
pub mod ui;

pub use host::{DictationHost, DictationHostOptions, DictationModelStatus, Listening};
