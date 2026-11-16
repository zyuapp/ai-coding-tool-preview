//! The simulator helper's stdout: `[u32 big-endian length][u8 kind][payload]`, one after another.

use serde_json::{Map, Value};

/// A piece of a device's screen stream: the H.264 decoder setup, or one encoded frame of it.
#[derive(Clone, Debug, PartialEq)]
pub enum SimulatorFrame {
    /// An avcC record and the picture size it describes.
    Config { udid: String, width: u32, height: u32, data: Vec<u8> },
    /// One AVCC access unit; `timestamp` is in microseconds.
    Chunk { udid: String, key: bool, timestamp: i64, data: Vec<u8> },
}

impl SimulatorFrame {
    pub fn udid(&self) -> &str {
        match self {
            SimulatorFrame::Config { udid, .. } | SimulatorFrame::Chunk { udid, .. } => udid,
        }
    }
}

/// What the helper says: a JSON answer or event, or a piece of a screen stream.
#[derive(Clone, Debug, PartialEq)]
pub enum HelperMessage {
    Json(Map<String, Value>),
    Frame(SimulatorFrame),
}

const JSON_KIND: u8 = 0;
const CHUNK_KIND: u8 = 1;
const CONFIG_KIND: u8 = 2;
/// Far beyond any frame a phone's screen encodes to, so a corrupt length is caught rather than awaited.
const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("The simulator helper sent a malformed frame.")]
pub struct MalformedFrame;

fn binary(kind: u8, payload: &[u8]) -> Option<SimulatorFrame> {
    if payload.len() < 2 {
        return None;
    }
    let header_length = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    if payload.len() < 2 + header_length {
        return None;
    }
    let header: Map<String, Value> = serde_json::from_slice(&payload[2..2 + header_length]).ok()?;
    let udid = header.get("udid")?.as_str()?.to_string();
    let data = payload[2 + header_length..].to_vec();
    let number = |key: &str| header.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN);
    if kind == CONFIG_KIND {
        return Some(SimulatorFrame::Config { udid, width: number("width") as u32, height: number("height") as u32, data });
    }
    Some(SimulatorFrame::Chunk { udid, key: header.get("key") == Some(&Value::Bool(true)), timestamp: number("ts") as i64, data })
}

/// Splits the helper's stdout however the pipe chunks it.
#[derive(Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
    start: usize,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a chunk of stdout and hands each whole message to `deliver`. A message that does not parse is
    /// dropped; the next keyframe recovers the stream. A length no frame could have is an error.
    pub fn push(&mut self, chunk: &[u8], mut deliver: impl FnMut(HelperMessage)) -> Result<(), MalformedFrame> {
        if self.start > 0 && self.start == self.buffer.len() {
            self.buffer.clear();
            self.start = 0;
        }
        self.buffer.extend_from_slice(chunk);
        loop {
            let available = self.buffer.len() - self.start;
            if available < 4 {
                break;
            }
            let at = self.start;
            let length = u32::from_be_bytes([self.buffer[at], self.buffer[at + 1], self.buffer[at + 2], self.buffer[at + 3]]) as usize;
            if !(1..=MAX_FRAME_BYTES).contains(&length) {
                return Err(MalformedFrame);
            }
            if available < 4 + length {
                break;
            }
            let body = &self.buffer[at + 4..at + 4 + length];
            let (kind, payload) = (body[0], &body[1..]);
            match kind {
                JSON_KIND => {
                    if let Ok(Value::Object(json)) = serde_json::from_slice::<Value>(payload) {
                        deliver(HelperMessage::Json(json));
                    }
                }
                CHUNK_KIND | CONFIG_KIND => {
                    if let Some(frame) = binary(kind, payload) {
                        deliver(HelperMessage::Frame(frame));
                    }
                }
                _ => {}
            }
            self.start += 4 + length;
        }
        // Keeps the buffer from growing without bound when messages straddle chunks.
        if self.start > 1 << 20 {
            self.buffer.drain(..self.start);
            self.start = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        out.push(kind);
        out.extend_from_slice(payload);
        out
    }

    fn frame(kind: u8, header: &str, body: &[u8]) -> Vec<u8> {
        let mut payload = (header.len() as u16).to_be_bytes().to_vec();
        payload.extend_from_slice(header.as_bytes());
        payload.extend_from_slice(body);
        message(kind, &payload)
    }

    #[test]
    fn messages_split_anywhere_arrive_whole_and_in_order() {
        let mut bytes = message(0, br#"{"event":"ready"}"#);
        bytes.extend(frame(2, r#"{"udid":"A","width":10,"height":20}"#, &[1, 2, 3]));
        bytes.extend(frame(1, r#"{"udid":"A","key":true,"ts":42}"#, &[9; 300]));
        for split in [1, 3, 5, 17, 64, bytes.len()] {
            let mut reader = FrameReader::new();
            let mut seen = Vec::new();
            for chunk in bytes.chunks(split) {
                reader.push(chunk, |message| seen.push(message)).unwrap();
            }
            assert_eq!(seen.len(), 3, "split {split}");
            assert!(matches!(&seen[0], HelperMessage::Json(json) if json["event"] == "ready"));
            assert_eq!(seen[1], HelperMessage::Frame(SimulatorFrame::Config { udid: "A".into(), width: 10, height: 20, data: vec![1, 2, 3] }));
            assert!(matches!(&seen[2], HelperMessage::Frame(SimulatorFrame::Chunk { key: true, timestamp: 42, data, .. }) if data.len() == 300));
        }
    }

    #[test]
    fn a_bad_message_is_dropped_but_an_impossible_length_fails() {
        let mut reader = FrameReader::new();
        let mut bytes = message(0, b"not json");
        bytes.extend(message(0, br#"{"id":1}"#));
        let mut seen = 0;
        reader.push(&bytes, |_| seen += 1).unwrap();
        assert_eq!(seen, 1);
        assert!(reader.push(&[0, 0, 0, 0], |_| {}).is_err());
    }
}
