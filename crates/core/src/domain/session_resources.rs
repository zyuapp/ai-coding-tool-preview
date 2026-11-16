//! Memory protection: a thread stopped because its engine's processes held too much memory.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceCleanup {
    Stopping,
    Complete,
    Incomplete,
}

/// A saved interruption, independent of whichever process the next session starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStop {
    pub at: i64,
    pub bytes: i64,
    pub cleanup: ResourceCleanup,
}

/// Resident memory includes native allocations and tool processes, beyond an engine's JS heap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemorySample {
    pub at: i64,
    pub bytes: u64,
    pub available: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryVerdict {
    Critical,
    Pressure,
}

const MIB: u64 = 1024 * 1024;

/// Reclaimable system memory below which the computer is under pressure.
pub fn pressure_line(total: u64) -> u64 {
    (512 * MIB).max(total / 16)
}

/// Only system pressure stops a thread, however large it grows: critical headroom at once, otherwise
/// sustained pressure while it holds an eighth of memory.
pub fn unsafe_memory(samples: &[MemorySample], total: u64) -> Option<MemoryVerdict> {
    let last = samples.last()?;
    if last.bytes == 0 {
        return None;
    }
    if last.available < 256 * MIB && last.bytes >= 64 * MIB {
        return Some(MemoryVerdict::Critical);
    }
    let recent = &samples[samples.len().saturating_sub(4)..];
    if recent.len() < 4 || last.at - recent[0].at < 15_000 {
        return None;
    }
    let line = pressure_line(total);
    recent.iter().all(|sample| sample.available < line && sample.bytes >= total / 8).then_some(MemoryVerdict::Pressure)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * MIB;

    fn samples(bytes: u64, available: u64, every: i64) -> Vec<MemorySample> {
        (0..4).map(|index| MemorySample { at: index * every, bytes, available }).collect()
    }

    #[test]
    fn only_sustained_pressure_or_critical_headroom_stops_a_thread() {
        let total = 32 * GIB;
        assert_eq!(unsafe_memory(&samples(20 * GIB, 8 * GIB, 5_000), total), None);
        assert_eq!(unsafe_memory(&samples(5 * GIB, GIB, 5_000), total), Some(MemoryVerdict::Pressure));
        assert_eq!(unsafe_memory(&samples(5 * GIB, GIB, 4_000), total), None);
        assert_eq!(unsafe_memory(&samples(3 * GIB, GIB, 5_000), total), None);
        assert_eq!(unsafe_memory(&[MemorySample { at: 0, bytes: 100 * MIB, available: 200 * MIB }], total), Some(MemoryVerdict::Critical));
    }
}
