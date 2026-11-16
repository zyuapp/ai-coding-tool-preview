//! TextMate grammars tokenized the way Shiki tokenizes them: a port of vscode-textmate on
//! Oniguruma, reading the same grammar files the Electron app bundles.

mod grammar;
mod matcher;
mod raw;
mod rule;
mod theme;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub(crate) use grammar::{State, Style};
use grammar::{Context, Grammar};
use raw::RawGrammar;
use rule::RegexCache;
use theme::Theme;

/// Shiki's `tokenizeTimeLimit`: a line that takes longer keeps the tokens found so far.
const LINE_TIME_LIMIT: Duration = Duration::from_millis(500);

pub(crate) struct Registry {
    sources: HashMap<String, Arc<RawGrammar>>,
    languages: HashMap<String, String>,
    grammars: HashMap<String, Grammar>,
    regexes: RegexCache,
    theme: Theme<Style>,
    counter: u64,
}

impl Registry {
    pub fn new(grammars: &[&str], theme: &[(&str, Style)]) -> Registry {
        let mut sources = HashMap::new();
        let mut languages = HashMap::new();
        for json in grammars {
            let Some(raw) = RawGrammar::parse(json) else { continue };
            languages.insert(raw.name.clone(), raw.scope_name.clone());
            for alias in &raw.aliases {
                languages.insert(alias.clone(), raw.scope_name.clone());
            }
            sources.insert(raw.scope_name.clone(), Arc::new(raw));
        }
        Registry {
            sources,
            languages,
            grammars: HashMap::new(),
            regexes: RegexCache::default(),
            theme: Theme::new(theme),
            counter: 0,
        }
    }

    /// The scope name of the grammar a language name or alias asks for.
    pub fn scope(&self, language: &str) -> Option<&str> {
        self.languages.get(language).map(String::as_str)
    }

    /// One line's tokens as (start, style), each running to the next start or the line's end.
    pub fn tokenize_line(&mut self, scope: &str, line: &str, previous: Option<&Arc<State>>) -> Option<(Vec<(usize, Style)>, Arc<State>)> {
        if !self.grammars.contains_key(scope) {
            let raw = self.sources.get(scope)?;
            self.grammars.insert(scope.to_string(), Grammar::new(raw));
        }
        let grammar = self.grammars.get_mut(scope)?;
        let mut cx = Context { sources: &self.sources, regexes: &mut self.regexes, theme: &self.theme, counter: &mut self.counter };
        Some(grammar.tokenize_line(line, previous, &mut cx, Some(LINE_TIME_LIMIT)))
    }
}
