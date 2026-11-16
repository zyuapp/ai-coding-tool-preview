//! Compiled rules, their regular expressions and the scanners that search them, as in
//! vscode-textmate's `rule.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use onig::{MatchParam, Regex, RegexOptions, Region, SearchOptions, Syntax};

pub(crate) const END_RULE: i32 = -1;
pub(crate) const WHILE_RULE: i32 = -2;

/// A capture group's byte span, or `None` when the group did not take part.
pub(crate) type Capture = Option<(usize, usize)>;

pub(crate) fn capture_len(capture: &Capture) -> usize {
    capture.map_or(0, |(start, end)| end - start)
}

pub(crate) fn capture_text<'a>(line: &'a str, capture: Option<&Capture>) -> &'a str {
    match capture {
        Some(Some((start, end))) => line.get(*start..*end).unwrap_or_default(),
        _ => "",
    }
}

fn has_back_references(source: &str) -> bool {
    let bytes = source.as_bytes();
    bytes.windows(2).any(|pair| pair[0] == b'\\' && pair[1].is_ascii_digit())
}

/// `$1`, `${1:/downcase}` and `${1:/upcase}` in a scope name, filled from the match.
pub(crate) fn has_captures(name: &str) -> bool {
    replace_captures_with(name, |_, _| Some(String::new())).1
}

pub(crate) fn replace_captures(name: &str, line: &str, captures: &[Capture]) -> String {
    replace_captures_with(name, |index, command| {
        let capture = captures.get(index)?;
        let mut text = capture_text(line, Some(capture));
        while let Some(rest) = text.strip_prefix('.') {
            text = rest;
        }
        Some(match command {
            Some("downcase") => text.to_lowercase(),
            Some("upcase") => text.to_uppercase(),
            _ => text.to_string(),
        })
    })
    .0
}

fn replace_captures_with(name: &str, mut fill: impl FnMut(usize, Option<&str>) -> Option<String>) -> (String, bool) {
    let bytes = name.as_bytes();
    let mut out = String::with_capacity(name.len());
    let mut found = false;
    let mut at = 0;
    let mut copied = 0;
    while at < bytes.len() {
        if bytes[at] != b'$' {
            at += 1;
            continue;
        }
        let digits = bytes[at + 1..].iter().take_while(|byte| byte.is_ascii_digit()).count();
        let mut matched = None;
        if digits > 0 {
            matched = Some((at + 1 + digits, &name[at + 1..at + 1 + digits], None));
        } else if bytes.get(at + 1) == Some(&b'{') {
            let inner = at + 2;
            let digits = bytes[inner..].iter().take_while(|byte| byte.is_ascii_digit()).count();
            if digits > 0 && bytes.get(inner + digits..inner + digits + 2) == Some(b":/") {
                let rest = &name[inner + digits + 2..];
                for command in ["downcase", "upcase"] {
                    if rest.starts_with(command) && rest.as_bytes().get(command.len()) == Some(&b'}') {
                        matched = Some((inner + digits + 2 + command.len() + 1, &name[inner..inner + digits], Some(command)));
                    }
                }
            }
        }
        match matched {
            Some((end, index, command)) => {
                found = true;
                out.push_str(&name[copied..at]);
                match index.parse::<usize>().ok().and_then(|index| fill(index, command)) {
                    Some(text) => out.push_str(&text),
                    None => out.push_str(&name[at..end]),
                }
                at = end;
                copied = end;
            }
            None => at += 1,
        }
    }
    out.push_str(&name[copied..]);
    (out, found)
}

fn escape_regex(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if "-\\{}*+?|^$.,[]()#".contains(ch) || ch.is_whitespace() || ch == '\u{feff}' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// One pattern's source and the rule it stands for. `\A` and `\G` make four variants: each anchor
/// is kept only where it can match, and replaced by a character no line holds elsewhere.
#[derive(Clone)]
pub(crate) struct RegSrc {
    pub source: String,
    pub rule_id: i32,
    pub has_anchor: bool,
    pub has_back_references: bool,
    anchors: Option<Box<[String; 4]>>,
}

impl RegSrc {
    pub fn new(pattern: &str, rule_id: i32) -> RegSrc {
        let bytes = pattern.as_bytes();
        let mut source = String::with_capacity(pattern.len());
        let mut has_anchor = false;
        let mut copied = 0;
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'\\' && at + 1 < bytes.len() {
                match bytes[at + 1] {
                    b'z' => {
                        source.push_str(&pattern[copied..at]);
                        source.push_str("$(?!\\n)(?<!\\n)");
                        copied = at + 2;
                    }
                    b'A' | b'G' => has_anchor = true,
                    _ => {}
                }
                at += 2;
            } else {
                at += 1;
            }
        }
        source.push_str(&pattern[copied..]);
        let has_back_references = has_back_references(&source);
        let mut result = RegSrc { source, rule_id, has_anchor, has_back_references, anchors: None };
        result.build_anchors();
        result
    }

    fn build_anchors(&mut self) {
        if !self.has_anchor {
            return;
        }
        let mut variants: [String; 4] = Default::default();
        let source = &self.source;
        let bytes = source.as_bytes();
        let mut copied = 0;
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'\\' && at + 1 < bytes.len() {
                let next = bytes[at + 1];
                if next == b'A' || next == b'G' {
                    for (index, variant) in variants.iter_mut().enumerate() {
                        let allow_a = index & 2 != 0;
                        let allow_g = index & 1 != 0;
                        let keep = if next == b'A' { allow_a } else { allow_g };
                        variant.push_str(&source[copied..at + 1]);
                        variant.push_str(if keep { if next == b'A' { "A" } else { "G" } } else { "\u{FFFF}" });
                    }
                    copied = at + 2;
                }
                at += 2;
            } else {
                at += 1;
            }
        }
        for variant in variants.iter_mut() {
            variant.push_str(&source[copied..]);
        }
        self.anchors = Some(Box::new(variants));
    }

    pub fn set_source(&mut self, source: &str) {
        if self.source == source {
            return;
        }
        self.source = source.to_string();
        self.build_anchors();
    }

    pub fn resolve_anchors(&self, allow_a: bool, allow_g: bool) -> &str {
        match &self.anchors {
            Some(variants) => &variants[(allow_a as usize) << 1 | allow_g as usize],
            None => &self.source,
        }
    }

    /// The end pattern with `\1`… replaced by what the begin pattern captured, escaped.
    pub fn resolve_back_references(&self, line: &str, captures: &[Capture]) -> String {
        let bytes = self.source.as_bytes();
        let mut out = String::with_capacity(self.source.len());
        let mut copied = 0;
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at] == b'\\' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
                let digits = bytes[at + 1..].iter().take_while(|byte| byte.is_ascii_digit()).count();
                out.push_str(&self.source[copied..at]);
                let index = self.source[at + 1..at + 1 + digits].parse::<usize>().unwrap_or(usize::MAX);
                out.push_str(&escape_regex(capture_text(line, captures.get(index))));
                at += 1 + digits;
                copied = at;
            } else {
                at += 1;
            }
        }
        out.push_str(&self.source[copied..]);
        out
    }
}

/// Compiled regular expressions by source, shared by every grammar. A pattern Oniguruma rejects
/// is skipped, as Shiki's forgiving engine skips one it cannot compile.
#[derive(Default)]
pub(crate) struct RegexCache {
    compiled: HashMap<String, Option<Arc<Regex>>>,
}

impl RegexCache {
    fn get(&mut self, source: &str) -> Option<Arc<Regex>> {
        if let Some(found) = self.compiled.get(source) {
            return found.clone();
        }
        let regex = Regex::with_options(source, RegexOptions::REGEX_OPTION_CAPTURE_GROUP, Syntax::default())
            .ok()
            .map(Arc::new);
        self.compiled.insert(source.to_string(), regex.clone());
        regex
    }
}

struct LastSearch {
    text: u64,
    from: usize,
    found: Option<(usize, Vec<Capture>)>,
}

/// Several patterns searched together: the one matching earliest wins, the first listed on a tie.
/// A pattern's last search is reused while it still answers the next one, except where `\G` ties
/// the match to where the search starts.
pub(crate) struct Scanner {
    regexes: Vec<Option<Arc<Regex>>>,
    rule_ids: Vec<i32>,
    cacheable: Vec<bool>,
    last: Vec<Option<LastSearch>>,
}

impl Scanner {
    fn new(sources: &[&str], rule_ids: Vec<i32>, cache: &mut RegexCache) -> Scanner {
        Scanner {
            regexes: sources.iter().map(|source| cache.get(source)).collect(),
            last: sources.iter().map(|_| None).collect(),
            cacheable: sources.iter().map(|source| !source.contains("\\G")).collect(),
            rule_ids,
        }
    }

    /// The winning rule and its captures, searching `text` (the string numbered `text_id`) from `start`.
    pub fn find(&mut self, text: &str, text_id: u64, start: usize) -> Option<(i32, Vec<Capture>)> {
        let mut best: Option<(usize, usize)> = None;
        let mut region = Region::new();
        for index in 0..self.regexes.len() {
            let Some(regex) = &self.regexes[index] else { continue };
            let reuse = self.cacheable[index] && self.last[index].as_ref().is_some_and(|last| {
                last.text == text_id && last.from <= start && last.found.as_ref().is_none_or(|(at, _)| *at >= start)
            });
            if !reuse {
                region.clear();
                let found = match regex.search_with_param(
                    text,
                    start,
                    text.len(),
                    SearchOptions::SEARCH_OPTION_NONE,
                    Some(&mut region),
                    MatchParam::default(),
                ) {
                    Ok(Some(at)) => Some((at, (0..region.len()).map(|group| region.pos(group)).collect())),
                    _ => None,
                };
                self.last[index] = Some(LastSearch { text: text_id, from: start, found });
            }
            let Some((at, _)) = self.last[index].as_ref().and_then(|last| last.found.as_ref()) else { continue };
            let at = *at;
            if at == start {
                best = Some((index, at));
                break;
            }
            if best.is_none_or(|(_, best_at)| at < best_at) {
                best = Some((index, at));
            }
        }
        let (index, _) = best?;
        let captures = self.last[index].as_ref()?.found.as_ref()?.1.clone();
        Some((self.rule_ids[index], captures))
    }
}

/// The patterns a rule searches for, with a scanner per anchor variant built when first needed.
#[derive(Default)]
pub(crate) struct SourceList {
    items: Vec<RegSrc>,
    has_anchors: bool,
    scanners: [Option<Scanner>; 5],
}

impl SourceList {
    pub fn push(&mut self, item: RegSrc) {
        self.has_anchors |= item.has_anchor;
        self.items.push(item);
    }

    pub fn unshift(&mut self, item: RegSrc) {
        self.has_anchors |= item.has_anchor;
        self.items.insert(0, item);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn set_source(&mut self, index: usize, source: &str) {
        if self.items[index].source != source {
            self.scanners = Default::default();
            self.items[index].set_source(source);
        }
    }

    pub fn scanner(&mut self, allow_a: bool, allow_g: bool, cache: &mut RegexCache) -> &mut Scanner {
        let slot = if self.has_anchors { (allow_a as usize) << 1 | allow_g as usize } else { 4 };
        if self.scanners[slot].is_none() {
            let sources: Vec<&str> = self
                .items
                .iter()
                .map(|item| if self.has_anchors { item.resolve_anchors(allow_a, allow_g) } else { item.source.as_str() })
                .collect();
            let rule_ids = self.items.iter().map(|item| item.rule_id).collect();
            self.scanners[slot] = Some(Scanner::new(&sources, rule_ids, cache));
        }
        self.scanners[slot].as_mut().expect("scanner was just built")
    }
}

pub(crate) enum RuleKind {
    Capture { retokenize: i32 },
    Match { regex: RegSrc, captures: Vec<Option<i32>> },
    IncludeOnly { patterns: Vec<i32>, missing: bool },
    BeginEnd {
        begin: RegSrc,
        begin_captures: Vec<Option<i32>>,
        end: RegSrc,
        end_captures: Vec<Option<i32>>,
        end_last: bool,
        patterns: Vec<i32>,
        missing: bool,
    },
    BeginWhile {
        begin: RegSrc,
        begin_captures: Vec<Option<i32>>,
        while_: RegSrc,
        while_captures: Vec<Option<i32>>,
        patterns: Vec<i32>,
        missing: bool,
    },
}

pub(crate) struct Rule {
    pub name: Option<Arc<str>>,
    name_capturing: bool,
    pub content_name: Option<Arc<str>>,
    content_capturing: bool,
    pub kind: RuleKind,
}

impl Rule {
    pub fn new(name: Option<&str>, content_name: Option<&str>, kind: RuleKind) -> Rule {
        Rule {
            name_capturing: name.is_some_and(has_captures),
            name: name.map(Arc::from),
            content_capturing: content_name.is_some_and(has_captures),
            content_name: content_name.map(Arc::from),
            kind,
        }
    }

    pub fn name(&self, line: Option<(&str, &[Capture])>) -> Option<Arc<str>> {
        match (&self.name, line) {
            (Some(name), Some((line, captures))) if self.name_capturing => {
                Some(Arc::from(replace_captures(name, line, captures)))
            }
            (name, _) => name.clone(),
        }
    }

    pub fn content_name(&self, line: &str, captures: &[Capture]) -> Option<Arc<str>> {
        match &self.content_name {
            Some(name) if self.content_capturing => Some(Arc::from(replace_captures(name, line, captures))),
            name => name.clone(),
        }
    }

    /// Rules that include nothing they asked for are left out of the patterns that include them.
    pub fn is_empty_include(&self) -> bool {
        match &self.kind {
            RuleKind::IncludeOnly { patterns, missing }
            | RuleKind::BeginEnd { patterns, missing, .. }
            | RuleKind::BeginWhile { patterns, missing, .. } => *missing && patterns.is_empty(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_and_back_references_resolve_like_vscode_textmate() {
        let src = RegSrc::new(r"\G(\w+)\A\z", 1);
        assert_eq!(src.resolve_anchors(false, true), "\\G(\\w+)\\\u{FFFF}$(?!\\n)(?<!\\n)");
        assert_eq!(src.resolve_anchors(true, false), "\\\u{FFFF}(\\w+)\\A$(?!\\n)(?<!\\n)");
        let end = RegSrc::new(r"^\1$", -1);
        assert!(end.has_back_references);
        assert_eq!(end.resolve_back_references("a.b x", &[Some((0, 5)), Some((0, 3))]), r"^a\.b$");
        assert_eq!(replace_captures("entity.${1:/downcase}.$2", "AB.cd", &[None, Some((0, 2)), Some((2, 5))]), "entity.ab.cd");
    }
}
