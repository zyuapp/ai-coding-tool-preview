//! Text ordering the way the TypeScript app's `localeCompare` orders it, closely enough for lists.

use std::cmp::Ordering;

/// Root-collation order approximated: punctuation and symbols before digits before letters, letters
/// compared without case first, and lower case ahead of upper case only to break a tie.
pub fn locale_compare(left: &str, right: &str) -> Ordering {
    let primary = left.chars().map(primary_key).cmp(right.chars().map(primary_key));
    primary.then_with(|| left.chars().map(tertiary_key).cmp(right.chars().map(tertiary_key)))
}

fn primary_key(character: char) -> (u8, char) {
    let class = if character.is_whitespace() {
        0
    } else if character.is_alphabetic() {
        3
    } else if character.is_numeric() {
        2
    } else {
        1
    };
    (class, character.to_lowercase().next().unwrap_or(character))
}

fn tertiary_key(character: char) -> u8 {
    u8::from(character.is_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_compare_without_case_and_after_punctuation() {
        let mut names = vec!["b.txt", "A.txt", "a.txt", "_x", "1.txt"];
        names.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(names, vec!["_x", "1.txt", "a.txt", "A.txt", "b.txt"]);
    }
}
