use std::collections::VecDeque;

use crate::contract::utf16_len;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_CHUNKS: usize = 128;

/// A bounded tail shared by remote viewers. A reader that falls behind restores the screen instead.
#[derive(Default)]
pub(crate) struct OutputBuffer {
    chunks: VecDeque<(u64, String)>,
    bytes: usize,
}

impl OutputBuffer {
    pub fn push(&mut self, sequence: u64, data: String) {
        self.bytes += utf16_len(&data);
        self.chunks.push_back((sequence, data));
        while self.bytes > MAX_BYTES || self.chunks.len() > MAX_CHUNKS {
            let Some((_, dropped)) = self.chunks.pop_front() else { break };
            self.bytes -= utf16_len(&dropped);
        }
    }

    /// Everything after `after`, or `None` when the buffer no longer reaches back that far.
    pub fn read(&self, after: u64, sequence: u64) -> Option<String> {
        if after == sequence {
            return Some(String::new());
        }
        let first = self.chunks.front()?.0;
        if after > sequence || after + 1 < first {
            return None;
        }
        Some(self.chunks.iter().filter(|(chunk, _)| *chunk > after).map(|(_, data)| data.as_str()).collect())
    }

    pub fn reset(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_that_fell_behind_gets_nothing() {
        let mut buffer = OutputBuffer::default();
        for sequence in 1..=200 {
            buffer.push(sequence, "x".into());
        }
        assert_eq!(buffer.read(10, 200), None);
        assert_eq!(buffer.read(198, 200).as_deref(), Some("xx"));
        assert_eq!(buffer.read(200, 200).as_deref(), Some(""));
    }
}
