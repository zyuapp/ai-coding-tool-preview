//! Version strings as engine commands print them.

use std::cmp::Ordering;

/// The first dotted number in what a `--version` flag printed, whatever the command wrapped it in.
pub fn read_version(output: &str) -> Option<String> {
    let bytes = output.as_bytes();
    let mut start = 0;
    while start < bytes.len() {
        if bytes[start].is_ascii_digit() {
            let mut end = start;
            let mut parts = 0;
            loop {
                let digits_start = end;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                if end == digits_start {
                    break;
                }
                parts += 1;
                if end + 1 < bytes.len() && bytes[end] == b'.' && bytes[end + 1].is_ascii_digit() {
                    end += 1;
                } else {
                    break;
                }
            }
            if parts >= 2 {
                return Some(output[start..end].to_string());
            }
            start = end.max(start + 1);
        } else {
            start += 1;
        }
    }
    None
}

/// Less when `left` is older, equal when they match, greater when `left` is newer.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let parse = |value: &str| -> Vec<f64> {
        value
            .split('.')
            .map(|part| part.parse::<f64>().unwrap_or(f64::NAN))
            .collect()
    };
    let (left, right) = (parse(left), parse(right));
    for index in 0..left.len().max(right.len()) {
        let difference =
            left.get(index).copied().unwrap_or(0.0) - right.get(index).copied().unwrap_or(0.0);
        if difference < 0.0 {
            return Ordering::Less;
        }
        if difference > 0.0 {
            return Ordering::Greater;
        }
    }
    Ordering::Equal
}

/// A version that could not be read counts as old enough to warn about.
pub fn is_older_than(version: Option<&str>, baseline: &str) -> bool {
    match version {
        None => true,
        Some(version) => compare_versions(version, baseline) == Ordering::Less,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_first_dotted_number() {
        assert_eq!(
            read_version("2.1.322 (Claude Code)").as_deref(),
            Some("2.1.322")
        );
        assert_eq!(
            read_version("codex-cli 0.167.2\n").as_deref(),
            Some("0.167.2")
        );
        assert_eq!(read_version("opencode v2.2.7").as_deref(), Some("2.2.7"));
        assert_eq!(read_version("build 7 of 1.2").as_deref(), Some("1.2"));
        assert_eq!(read_version("none"), None);
    }

    #[test]
    fn compares_numerically() {
        assert!(is_older_than(Some("2.1.9"), "2.1.10"));
        assert!(!is_older_than(Some("2.2"), "2.1.10"));
        assert!(is_older_than(None, "1.0.0"));
    }
}
