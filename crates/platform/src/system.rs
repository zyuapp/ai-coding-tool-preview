//! What the computer has: its memory and the processes running on it.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessSample {
    pub pid: u32,
    pub parent: u32,
    pub bytes: u64,
    /// When the process started, which tells it from a later one that reuses its pid.
    pub born: String,
}

/// Every process on the computer.
pub fn processes() -> std::io::Result<Vec<ProcessSample>> {
    let output = crate::process::std_command("/bin/ps").args(["-axo", "pid=,ppid=,rss=,lstart="]).env("LC_ALL", "C").stdin(std::process::Stdio::null()).output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("ps failed"));
    }
    Ok(parse_process_samples(&String::from_utf8_lossy(&output.stdout)))
}

/// `lstart` tells a live process from another that later reuses its pid, on macOS and Linux.
pub fn parse_process_samples(output: &str) -> Vec<ProcessSample> {
    let mut samples = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 8 {
            continue;
        }
        let (Ok(pid), Ok(parent), Ok(rss)) = (fields[0].parse::<u32>(), fields[1].parse::<u32>(), fields[2].parse::<u64>()) else { continue };
        let born = &fields[3..];
        let shaped = born[0].len() == 3 && born[1].len() == 3 && born[0].chars().all(|c| c.is_ascii_alphabetic()) && born[1].chars().all(|c| c.is_ascii_alphabetic())
            && born[2].len() <= 2 && born[2].chars().all(|c| c.is_ascii_digit())
            && born[3].len() == 8 && born[3].chars().filter(|c| *c == ':').count() == 2
            && born[4].len() == 4 && born[4].chars().all(|c| c.is_ascii_digit());
        if !shaped || pid <= 1 {
            continue;
        }
        samples.push(ProcessSample { pid, parent, bytes: rss * 1024, born: born.join(" ") });
    }
    samples
}

/// Memory that can be had without swapping, reclaimable caches included, since free RAM alone would
/// stop healthy workloads. `u64::MAX` when it cannot be read, which is never an emergency.
pub fn available_memory() -> u64 {
    crate::os::available_memory().unwrap_or(u64::MAX)
}

/// The computer's installed memory.
pub fn total_memory() -> Option<u64> {
    crate::os::total_memory()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_samples_read_ps_output() {
        let samples = parse_process_samples("  412     1  20480 Sat Nov 14 17:06:25 2026\nbad line\n    1     0  100 Sat Nov 14 17:06:25 2026\n");
        assert_eq!(samples, vec![ProcessSample { pid: 412, parent: 1, bytes: 20480 * 1024, born: "Sat Nov 14 17:06:25 2026".into() }]);
    }
}
