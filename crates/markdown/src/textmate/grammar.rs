//! One grammar's compiled rules and the line tokenizer, ported from vscode-textmate's `grammar.ts`
//! and `tokenizeString.ts`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use super::matcher::{create_matchers, Matcher};
use super::raw::{Captures, GrammarCopy, RawGrammar, RawRule, Repository};
use super::rule::{capture_len, Capture, RegSrc, RegexCache, Rule, RuleKind, Scanner, SourceList, END_RULE, WHILE_RULE};
use super::theme::Theme;

pub(crate) type Style = u8;

/// The scopes a token sits in, innermost last, each with the style it resolves to.
pub(crate) struct Scopes {
    parent: Option<Arc<Scopes>>,
    scope: Arc<str>,
    style: Style,
}

impl Scopes {
    fn push(self: &Arc<Scopes>, path: Option<&str>, theme: &Theme<Style>) -> Arc<Scopes> {
        let Some(path) = path else { return self.clone() };
        let mut result = self.clone();
        for scope in path.split(' ') {
            result = Arc::new(Scopes {
                style: theme.resolve(scope).unwrap_or(result.style),
                scope: Arc::from(scope),
                parent: Some(result.clone()),
            });
        }
        result
    }

    fn names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        let mut node = Some(self);
        while let Some(current) = node {
            names.push(&*current.scope);
            node = current.parent.as_deref();
        }
        names.reverse();
        names
    }
}

/// The rule stack carried from one line to the next. Positions belong to the line that set them
/// (`line`); read from a later line they are -1, as vscode-textmate resets them.
pub(crate) struct State {
    parent: Option<Arc<State>>,
    rule_id: i32,
    enter_pos: i64,
    anchor_pos: i64,
    line: u64,
    begin_captured_eol: bool,
    end_rule: Option<Arc<str>>,
    name_scopes: Arc<Scopes>,
    content_scopes: Arc<Scopes>,
}

impl State {
    fn enter(&self, line: u64) -> i64 {
        if self.line == line { self.enter_pos } else { -1 }
    }

    fn anchor(&self, line: u64) -> i64 {
        if self.line == line { self.anchor_pos } else { -1 }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        self: &Arc<State>,
        rule_id: i32,
        enter_pos: i64,
        anchor_pos: i64,
        line: u64,
        begin_captured_eol: bool,
        name_scopes: Arc<Scopes>,
        content_scopes: Arc<Scopes>,
    ) -> Arc<State> {
        Arc::new(State {
            parent: Some(self.clone()),
            rule_id,
            enter_pos,
            anchor_pos,
            line,
            begin_captured_eol,
            end_rule: None,
            name_scopes,
            content_scopes,
        })
    }

    fn with_content(self: &Arc<State>, content_scopes: Arc<Scopes>) -> Arc<State> {
        if Arc::ptr_eq(&self.content_scopes, &content_scopes) {
            return self.clone();
        }
        Arc::new(State {
            parent: self.parent.clone(),
            rule_id: self.rule_id,
            enter_pos: self.enter_pos,
            anchor_pos: self.anchor_pos,
            line: self.line,
            begin_captured_eol: self.begin_captured_eol,
            end_rule: self.end_rule.clone(),
            name_scopes: self.name_scopes.clone(),
            content_scopes,
        })
    }

    fn with_end_rule(self: &Arc<State>, end_rule: String) -> Arc<State> {
        if self.end_rule.as_deref() == Some(end_rule.as_str()) {
            return self.clone();
        }
        Arc::new(State {
            parent: self.parent.clone(),
            rule_id: self.rule_id,
            enter_pos: self.enter_pos,
            anchor_pos: self.anchor_pos,
            line: self.line,
            begin_captured_eol: self.begin_captured_eol,
            end_rule: Some(Arc::from(end_rule)),
            name_scopes: self.name_scopes.clone(),
            content_scopes: self.content_scopes.clone(),
        })
    }

    fn pop(self: &Arc<State>) -> Arc<State> {
        self.parent.clone().unwrap_or_else(|| self.clone())
    }

    fn has_same_rule_as(self: &Arc<State>, other: &State, line: u64) -> bool {
        let mut node = Some(self.clone());
        while let Some(current) = node {
            if current.enter(line) != other.enter(line) {
                break;
            }
            if current.rule_id == other.rule_id {
                return true;
            }
            node = current.parent.clone();
        }
        false
    }
}

/// A line's tokens as start offsets and styles; a token runs to the next one's start.
pub(crate) struct LineTokens {
    pub tokens: Vec<(usize, Style)>,
    last_end: usize,
}

impl LineTokens {
    fn produce(&mut self, scopes: &Scopes, end: usize) {
        if self.last_end >= end {
            return;
        }
        if self.tokens.last().is_none_or(|(_, style)| *style != scopes.style) {
            self.tokens.push((self.last_end, scopes.style));
        }
        self.last_end = end;
    }
}

struct Injection {
    matcher: Matcher,
    rule_id: i32,
    priority: i32,
}

struct Found {
    rule_id: i32,
    captures: Vec<Capture>,
    priority: bool,
}

fn start_of(captures: &[Capture]) -> usize {
    captures.first().copied().flatten().map_or(0, |(start, _)| start)
}

fn end_of(captures: &[Capture]) -> usize {
    captures.first().copied().flatten().map_or(0, |(_, end)| end)
}

/// What tokenizing needs besides the grammar itself.
pub(crate) struct Context<'a> {
    pub sources: &'a HashMap<String, Arc<RawGrammar>>,
    pub regexes: &'a mut RegexCache,
    pub theme: &'a Theme<Style>,
    pub counter: &'a mut u64,
}

impl Context<'_> {
    fn next_id(&mut self) -> u64 {
        *self.counter += 1;
        *self.counter
    }
}

pub(crate) struct Grammar {
    copy: GrammarCopy,
    included: HashMap<String, GrammarCopy>,
    rules: Vec<Option<Rule>>,
    ids: HashMap<usize, i32>,
    lists: Vec<Option<SourceList>>,
    while_lists: Vec<Option<SourceList>>,
    root_id: i32,
    injections: Vec<Injection>,
}

const INCLUDE_DEPTH: usize = 256;

impl Grammar {
    pub fn new(raw: &RawGrammar) -> Grammar {
        Grammar {
            copy: GrammarCopy::new(raw, None),
            included: HashMap::new(),
            rules: vec![None],
            ids: HashMap::new(),
            lists: Vec::new(),
            while_lists: Vec::new(),
            root_id: -1,
            injections: Vec::new(),
        }
    }

    fn rule(&self, id: i32) -> Option<&Rule> {
        self.rules.get(usize::try_from(id).ok()?)?.as_ref()
    }

    fn register(&mut self) -> i32 {
        self.rules.push(None);
        (self.rules.len() - 1) as i32
    }

    fn external(&mut self, scope: &str, base: Option<Arc<RawRule>>, sources: &HashMap<String, Arc<RawGrammar>>) -> Option<Repository> {
        if let Some(copy) = self.included.get(scope) {
            return Some(copy.repository.clone());
        }
        let raw = sources.get(scope)?;
        let copy = GrammarCopy::new(raw, base);
        let repository = copy.repository.clone();
        self.included.insert(scope.to_string(), copy);
        Some(repository)
    }

    fn compile_id(&mut self, desc: &Arc<RawRule>, repository: &Repository, sources: &HashMap<String, Arc<RawGrammar>>) -> i32 {
        let key = Arc::as_ptr(desc) as usize;
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = self.register();
        self.ids.insert(key, id);
        let rule = if let Some(pattern) = desc.match_.as_deref().filter(|pattern| !pattern.is_empty()) {
            let captures = self.compile_captures(desc.captures.as_ref(), repository, sources);
            Rule::new(desc.name.as_deref(), None, RuleKind::Match { regex: RegSrc::new(pattern, id), captures })
        } else if desc.begin.is_none() {
            let merged;
            let repository = match &desc.repository {
                Some(local) => {
                    let mut map = (**repository).clone();
                    map.extend(local.iter().map(|(key, rule)| (key.clone(), rule.clone())));
                    merged = Arc::new(map);
                    &merged
                }
                None => repository,
            };
            let included;
            let patterns = match (&desc.patterns, &desc.include) {
                (None, Some(include)) if !include.is_empty() => {
                    included = Some(vec![Arc::new(RawRule { include: Some(include.clone()), ..RawRule::default() })]);
                    &included
                }
                _ => &desc.patterns,
            };
            let (patterns, missing) = self.compile_patterns(patterns.as_deref(), repository, sources);
            Rule::new(desc.name.as_deref(), desc.content_name.as_deref(), RuleKind::IncludeOnly { patterns, missing })
        } else {
            let begin = RegSrc::new(desc.begin.as_deref().unwrap_or_default(), id);
            if let Some(while_) = desc.while_.as_deref().filter(|pattern| !pattern.is_empty()) {
                let begin_captures = self.compile_captures(desc.begin_captures.as_ref().or(desc.captures.as_ref()), repository, sources);
                let while_captures = self.compile_captures(desc.while_captures.as_ref().or(desc.captures.as_ref()), repository, sources);
                let (patterns, missing) = self.compile_patterns(desc.patterns.as_deref(), repository, sources);
                Rule::new(
                    desc.name.as_deref(),
                    desc.content_name.as_deref(),
                    RuleKind::BeginWhile { begin, begin_captures, while_: RegSrc::new(while_, WHILE_RULE), while_captures, patterns, missing },
                )
            } else {
                let begin_captures = self.compile_captures(desc.begin_captures.as_ref().or(desc.captures.as_ref()), repository, sources);
                let end = desc.end.as_deref().filter(|pattern| !pattern.is_empty()).unwrap_or("\u{FFFF}");
                let end_captures = self.compile_captures(desc.end_captures.as_ref().or(desc.captures.as_ref()), repository, sources);
                let (patterns, missing) = self.compile_patterns(desc.patterns.as_deref(), repository, sources);
                Rule::new(
                    desc.name.as_deref(),
                    desc.content_name.as_deref(),
                    RuleKind::BeginEnd {
                        begin,
                        begin_captures,
                        end: RegSrc::new(end, END_RULE),
                        end_captures,
                        end_last: desc.apply_end_pattern_last,
                        patterns,
                        missing,
                    },
                )
            }
        };
        self.rules[id as usize] = Some(rule);
        id
    }

    fn compile_captures(&mut self, captures: Option<&Captures>, repository: &Repository, sources: &HashMap<String, Arc<RawGrammar>>) -> Vec<Option<i32>> {
        let Some(captures) = captures else { return Vec::new() };
        let size = captures.iter().map(|(index, _)| *index).max().unwrap_or(0) + 1;
        let mut out = vec![None; size];
        for (index, desc) in captures {
            let retokenize = if desc.patterns.is_some() { self.compile_id(desc, repository, sources) } else { 0 };
            let id = self.register();
            self.rules[id as usize] = Some(Rule::new(desc.name.as_deref(), desc.content_name.as_deref(), RuleKind::Capture { retokenize }));
            out[*index] = Some(id);
        }
        out
    }

    fn compile_patterns(&mut self, patterns: Option<&[Arc<RawRule>]>, repository: &Repository, sources: &HashMap<String, Arc<RawGrammar>>) -> (Vec<i32>, bool) {
        let Some(patterns) = patterns else { return (Vec::new(), false) };
        let mut out = Vec::new();
        for pattern in patterns {
            let id = match pattern.include.as_deref().filter(|include| !include.is_empty()) {
                Some(include @ ("$base" | "$self")) => match repository.get(include).cloned() {
                    Some(desc) => self.compile_id(&desc, repository, sources),
                    None => -1,
                },
                Some(include) if include.starts_with('#') => match repository.get(&include[1..]).cloned() {
                    Some(desc) => self.compile_id(&desc, repository, sources),
                    None => -1,
                },
                Some(include) => {
                    let (scope, rule) = match include.split_once('#') {
                        Some((scope, rule)) => (scope, Some(rule)),
                        None => (include, None),
                    };
                    match self.external(scope, repository.get("$base").cloned(), sources) {
                        Some(external) => match external.get(rule.unwrap_or("$self")).cloned() {
                            Some(desc) => self.compile_id(&desc, &external, sources),
                            None => -1,
                        },
                        None => -1,
                    }
                }
                None => self.compile_id(pattern, repository, sources),
            };
            if id == -1 {
                continue;
            }
            if self.rule(id).is_some_and(Rule::is_empty_include) {
                continue;
            }
            out.push(id);
        }
        let missing = patterns.len() != out.len();
        (out, missing)
    }

    fn ensure_compiled(&mut self, sources: &HashMap<String, Arc<RawGrammar>>) {
        if self.root_id != -1 {
            return;
        }
        let repository = self.copy.repository.clone();
        let this = self.copy.this.clone();
        self.root_id = self.compile_id(&this, &repository, sources);
        let mut injections = Vec::new();
        for (selector, rule) in self.copy.injections.clone() {
            let rule_id = self.compile_id(&rule, &repository, sources);
            for (matcher, priority) in create_matchers(&selector) {
                injections.push(Injection { matcher, rule_id, priority });
            }
        }
        injections.sort_by_key(|injection| injection.priority);
        self.injections = injections;
    }

    fn collect(&self, id: i32, out: &mut SourceList, depth: usize) {
        let Some(rule) = self.rule(id) else { return };
        match &rule.kind {
            RuleKind::Match { regex, .. } => out.push(regex.clone()),
            RuleKind::IncludeOnly { patterns, .. } if depth < INCLUDE_DEPTH => {
                for pattern in patterns {
                    self.collect(*pattern, out, depth + 1);
                }
            }
            RuleKind::BeginEnd { begin, .. } | RuleKind::BeginWhile { begin, .. } => out.push(begin.clone()),
            _ => {}
        }
    }

    fn scanner<'a>(&'a mut self, id: i32, end: Option<&str>, allow_a: bool, allow_g: bool, regexes: &mut RegexCache) -> Option<&'a mut Scanner> {
        let index = usize::try_from(id).ok()?;
        if self.lists.len() <= index {
            self.lists.resize_with(self.rules.len().max(index + 1), || None);
        }
        let rule = self.rule(id)?;
        let end_slot = match &rule.kind {
            RuleKind::BeginEnd { end, end_last, .. } if end.has_back_references => Some(*end_last),
            _ => None,
        };
        if self.lists[index].is_none() {
            let mut list = SourceList::default();
            match &rule.kind {
                RuleKind::Match { regex, .. } => list.push(regex.clone()),
                RuleKind::IncludeOnly { patterns, .. } | RuleKind::BeginWhile { patterns, .. } => {
                    for pattern in patterns {
                        self.collect(*pattern, &mut list, 0);
                    }
                }
                RuleKind::BeginEnd { patterns, end, end_last, .. } => {
                    for pattern in patterns {
                        self.collect(*pattern, &mut list, 0);
                    }
                    if *end_last { list.push(end.clone()) } else { list.unshift(end.clone()) }
                }
                RuleKind::Capture { .. } => {}
            }
            self.lists[index] = Some(list);
        }
        let list = self.lists[index].as_mut()?;
        if let (Some(end_last), Some(end)) = (end_slot, end) {
            let slot = if end_last { list.len() - 1 } else { 0 };
            list.set_source(slot, end);
        }
        Some(list.scanner(allow_a, allow_g, regexes))
    }

    fn while_scanner<'a>(&'a mut self, id: i32, end: Option<&str>, allow_a: bool, allow_g: bool, regexes: &mut RegexCache) -> Option<&'a mut Scanner> {
        let index = usize::try_from(id).ok()?;
        if self.while_lists.len() <= index {
            self.while_lists.resize_with(self.rules.len().max(index + 1), || None);
        }
        let RuleKind::BeginWhile { while_, .. } = &self.rule(id)?.kind else { return None };
        let while_ = while_.clone();
        let back_references = while_.has_back_references;
        if self.while_lists[index].is_none() {
            let mut list = SourceList::default();
            list.push(while_);
            self.while_lists[index] = Some(list);
        }
        let list = self.while_lists[index].as_mut()?;
        if back_references {
            list.set_source(0, end.unwrap_or("\u{FFFF}"));
        }
        Some(list.scanner(allow_a, allow_g, regexes))
    }

    /// Tokenizes one line (without its line break) from the state the previous line left.
    pub fn tokenize_line(&mut self, line: &str, previous: Option<&Arc<State>>, cx: &mut Context, time_limit: Option<std::time::Duration>) -> (Vec<(usize, Style)>, Arc<State>) {
        self.ensure_compiled(cx.sources);
        let line_id = cx.next_id();
        let (state, first_line) = match previous {
            Some(state) => (state.clone(), false),
            None => {
                let name = self.rule(self.root_id).and_then(|rule| rule.name(None)).unwrap_or_else(|| Arc::from("unknown"));
                let scopes = Arc::new(Scopes { style: cx.theme.resolve(&name).unwrap_or(0), scope: name, parent: None });
                let state = Arc::new(State {
                    parent: None,
                    rule_id: self.root_id,
                    enter_pos: -1,
                    anchor_pos: -1,
                    line: line_id,
                    begin_captured_eol: false,
                    end_rule: None,
                    name_scopes: scopes.clone(),
                    content_scopes: scopes,
                });
                (state, true)
            }
        };
        let text = format!("{line}\n");
        let text_id = cx.next_id();
        let mut tokens = LineTokens { tokens: Vec::new(), last_end: 0 };
        let deadline = time_limit.map(|limit| Instant::now() + limit);
        let mut run = Run { grammar: self, cx, line: line_id };
        let state = run.tokenize_string(&text, text_id, first_line, 0, state, &mut tokens, true, deadline);
        let length = text.len();
        if tokens.tokens.last().is_some_and(|(start, _)| *start == length - 1) {
            tokens.tokens.pop();
        }
        if tokens.tokens.is_empty() {
            tokens.tokens.push((0, state.content_scopes.style));
        }
        (tokens.tokens, state)
    }
}

struct Run<'g, 'c, 'a> {
    grammar: &'g mut Grammar,
    cx: &'c mut Context<'a>,
    line: u64,
}

impl Run<'_, '_, '_> {
    fn push_scopes(&self, base: &Arc<Scopes>, path: Option<&str>) -> Arc<Scopes> {
        base.push(path, self.cx.theme)
    }

    #[allow(clippy::too_many_arguments)]
    fn tokenize_string(
        &mut self,
        text: &str,
        text_id: u64,
        mut first_line: bool,
        mut line_pos: usize,
        mut stack: Arc<State>,
        tokens: &mut LineTokens,
        check_while: bool,
        deadline: Option<Instant>,
    ) -> Arc<State> {
        let length = text.len();
        let mut anchor: i64 = -1;
        if check_while {
            let result = self.check_while_conditions(text, text_id, first_line, line_pos, stack, tokens);
            stack = result.0;
            line_pos = result.1;
            anchor = result.2;
            first_line = result.3;
        }
        loop {
            if deadline.is_some_and(|deadline| Instant::now() > deadline) {
                return stack;
            }
            let Some(found) = self.match_rule_or_injections(text, text_id, first_line, line_pos, &stack, anchor) else {
                tokens.produce(&stack.content_scopes, length);
                return stack;
            };
            let captures = found.captures;
            let (start, end) = (start_of(&captures), end_of(&captures));
            let advanced = end > line_pos;
            if found.rule_id == END_RULE {
                let end_captures = match self.grammar.rule(stack.rule_id).map(|rule| &rule.kind) {
                    Some(RuleKind::BeginEnd { end_captures, .. }) => end_captures.clone(),
                    _ => Vec::new(),
                };
                tokens.produce(&stack.content_scopes, start);
                stack = stack.with_content(stack.name_scopes.clone());
                self.handle_captures(text, first_line, &stack, tokens, &end_captures, &captures);
                tokens.produce(&stack.content_scopes, end);
                let popped = stack.clone();
                stack = popped.pop();
                anchor = popped.anchor(self.line);
                if !advanced && popped.enter(self.line) == line_pos as i64 {
                    stack = popped;
                    tokens.produce(&stack.content_scopes, length);
                    return stack;
                }
            } else {
                let Some(rule) = self.grammar.rule(found.rule_id) else { return stack };
                let name = rule.name(Some((text, &captures)));
                enum Pushed {
                    Begin { captures: Vec<Option<i32>>, content: Option<Arc<str>>, end: Option<String> },
                    Match { captures: Vec<Option<i32>> },
                }
                let pushed = match &rule.kind {
                    RuleKind::BeginEnd { begin_captures, end, .. } => Pushed::Begin {
                        captures: begin_captures.clone(),
                        content: rule.content_name(text, &captures),
                        end: end.has_back_references.then(|| end.resolve_back_references(text, &captures)),
                    },
                    RuleKind::BeginWhile { begin_captures, while_, .. } => Pushed::Begin {
                        captures: begin_captures.clone(),
                        content: rule.content_name(text, &captures),
                        end: while_.has_back_references.then(|| while_.resolve_back_references(text, &captures)),
                    },
                    RuleKind::Match { captures, .. } => Pushed::Match { captures: captures.clone() },
                    _ => Pushed::Match { captures: Vec::new() },
                };
                tokens.produce(&stack.content_scopes, start);
                let before = stack.clone();
                let name_scopes = self.push_scopes(&stack.content_scopes, name.as_deref());
                stack = stack.push(found.rule_id, line_pos as i64, anchor, self.line, end == length, name_scopes.clone(), name_scopes.clone());
                match pushed {
                    Pushed::Begin { captures: begin_captures, content, end: resolved } => {
                        self.handle_captures(text, first_line, &stack, tokens, &begin_captures, &captures);
                        tokens.produce(&stack.content_scopes, end);
                        anchor = end as i64;
                        let content_scopes = self.push_scopes(&name_scopes, content.as_deref());
                        stack = stack.with_content(content_scopes);
                        if let Some(resolved) = resolved {
                            stack = stack.with_end_rule(resolved);
                        }
                        if !advanced && before.has_same_rule_as(&stack, self.line) {
                            stack = stack.pop();
                            tokens.produce(&stack.content_scopes, length);
                            return stack;
                        }
                    }
                    Pushed::Match { captures: match_captures } => {
                        self.handle_captures(text, first_line, &stack, tokens, &match_captures, &captures);
                        tokens.produce(&stack.content_scopes, end);
                        stack = stack.pop();
                        if !advanced {
                            stack = stack.pop();
                            tokens.produce(&stack.content_scopes, length);
                            return stack;
                        }
                    }
                }
            }
            if end > line_pos {
                line_pos = end;
                first_line = false;
            }
        }
    }

    fn check_while_conditions(
        &mut self,
        text: &str,
        text_id: u64,
        mut first_line: bool,
        mut line_pos: usize,
        mut stack: Arc<State>,
        tokens: &mut LineTokens,
    ) -> (Arc<State>, usize, i64, bool) {
        let mut anchor: i64 = if stack.begin_captured_eol { 0 } else { -1 };
        let mut while_rules = Vec::new();
        let mut node = Some(stack.clone());
        while let Some(current) = node {
            if matches!(self.grammar.rule(current.rule_id).map(|rule| &rule.kind), Some(RuleKind::BeginWhile { .. })) {
                while_rules.push(current.clone());
            }
            node = current.parent.clone();
        }
        while let Some(while_node) = while_rules.pop() {
            let end_rule = while_node.end_rule.clone();
            let found = self
                .grammar
                .while_scanner(while_node.rule_id, end_rule.as_deref(), first_line, line_pos as i64 == anchor, self.cx.regexes)
                .and_then(|scanner| scanner.find(text, text_id, line_pos));
            match found {
                Some((rule_id, captures)) => {
                    if rule_id != WHILE_RULE {
                        stack = while_node.pop();
                        break;
                    }
                    if !captures.is_empty() {
                        let while_captures = match self.grammar.rule(while_node.rule_id).map(|rule| &rule.kind) {
                            Some(RuleKind::BeginWhile { while_captures, .. }) => while_captures.clone(),
                            _ => Vec::new(),
                        };
                        let (start, end) = (start_of(&captures), end_of(&captures));
                        tokens.produce(&while_node.content_scopes, start);
                        self.handle_captures(text, first_line, &while_node, tokens, &while_captures, &captures);
                        tokens.produce(&while_node.content_scopes, end);
                        anchor = end as i64;
                        if end > line_pos {
                            line_pos = end;
                            first_line = false;
                        }
                    }
                }
                None => {
                    stack = while_node.pop();
                    break;
                }
            }
        }
        (stack, line_pos, anchor, first_line)
    }

    fn match_rule_or_injections(&mut self, text: &str, text_id: u64, first_line: bool, line_pos: usize, stack: &Arc<State>, anchor: i64) -> Option<Found> {
        let allow_g = line_pos as i64 == anchor;
        let end_rule = stack.end_rule.clone();
        let matched = self
            .grammar
            .scanner(stack.rule_id, end_rule.as_deref(), first_line, allow_g, self.cx.regexes)
            .and_then(|scanner| scanner.find(text, text_id, line_pos))
            .map(|(rule_id, captures)| Found { rule_id, captures, priority: false });
        if self.grammar.injections.is_empty() {
            return matched;
        }
        let injected = self.match_injections(text, text_id, first_line, line_pos, stack, allow_g);
        let Some(injected) = injected else { return matched };
        let Some(matched) = matched else { return Some(injected) };
        let matched_at = start_of(&matched.captures);
        let injected_at = start_of(&injected.captures);
        if injected_at < matched_at || (injected.priority && injected_at == matched_at) {
            return Some(injected);
        }
        Some(matched)
    }

    fn match_injections(&mut self, text: &str, text_id: u64, first_line: bool, line_pos: usize, stack: &Arc<State>, allow_g: bool) -> Option<Found> {
        let scopes = stack.content_scopes.names();
        let mut best: Option<(usize, Found)> = None;
        for index in 0..self.grammar.injections.len() {
            let injection = &self.grammar.injections[index];
            if !injection.matcher.matches(&scopes) {
                continue;
            }
            let (rule_id, priority) = (injection.rule_id, injection.priority);
            let Some((matched_rule, captures)) = self
                .grammar
                .scanner(rule_id, None, first_line, allow_g, self.cx.regexes)
                .and_then(|scanner| scanner.find(text, text_id, line_pos))
            else {
                continue;
            };
            let at = start_of(&captures);
            if best.as_ref().is_some_and(|(best_at, _)| at >= *best_at) {
                continue;
            }
            best = Some((at, Found { rule_id: matched_rule, captures, priority: priority == -1 }));
            if at == line_pos {
                break;
            }
        }
        best.map(|(_, found)| found)
    }

    fn handle_captures(&mut self, text: &str, first_line: bool, stack: &Arc<State>, tokens: &mut LineTokens, rules: &[Option<i32>], captures: &[Capture]) {
        if rules.is_empty() {
            return;
        }
        let count = rules.len().min(captures.len());
        let mut local: Vec<(Arc<Scopes>, usize)> = Vec::new();
        let max_end = end_of(captures);
        for index in 0..count {
            let Some(rule_id) = rules[index] else { continue };
            let capture = captures[index];
            if capture_len(&capture) == 0 {
                continue;
            }
            let Some((start, end)) = capture else { continue };
            if start > max_end {
                break;
            }
            while let Some((scopes, local_end)) = local.last().cloned() {
                if local_end > start {
                    break;
                }
                tokens.produce(&scopes, local_end);
                local.pop();
            }
            match local.last() {
                Some((scopes, _)) => tokens.produce(scopes, start),
                None => tokens.produce(&stack.content_scopes, start),
            }
            let Some(rule) = self.grammar.rule(rule_id) else { continue };
            let retokenize = match rule.kind {
                RuleKind::Capture { retokenize } => retokenize,
                _ => 0,
            };
            let name = rule.name(Some((text, captures)));
            if retokenize != 0 {
                let content = rule.content_name(text, captures);
                let name_scopes = self.push_scopes(&stack.content_scopes, name.as_deref());
                let content_scopes = self.push_scopes(&name_scopes, content.as_deref());
                let pushed = stack.push(retokenize, start as i64, -1, self.line, false, name_scopes, content_scopes);
                let sub = &text[..end];
                let sub_id = self.cx.next_id();
                self.tokenize_string(sub, sub_id, first_line && start == 0, start, pushed, tokens, false, None);
                continue;
            }
            if let Some(name) = name {
                let base = local.last().map_or_else(|| stack.content_scopes.clone(), |(scopes, _)| scopes.clone());
                local.push((self.push_scopes(&base, Some(&name)), end));
            }
        }
        while let Some((scopes, end)) = local.pop() {
            tokens.produce(&scopes, end);
        }
    }
}
