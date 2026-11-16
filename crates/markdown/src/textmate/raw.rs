//! TextMate grammars as read from their JSON, before any rule is compiled.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Map, Value};

pub(crate) type Captures = Vec<(usize, Arc<RawRule>)>;
pub(crate) type Repository = Arc<HashMap<String, Arc<RawRule>>>;

/// One rule object of a grammar. Its address is its identity: a rule is compiled once per grammar
/// that reads it, as vscode-textmate keys compiled rules by the object.
#[derive(Default)]
pub(crate) struct RawRule {
    pub include: Option<String>,
    pub name: Option<String>,
    pub content_name: Option<String>,
    pub match_: Option<String>,
    pub captures: Option<Captures>,
    pub begin: Option<String>,
    pub begin_captures: Option<Captures>,
    pub end: Option<String>,
    pub end_captures: Option<Captures>,
    pub while_: Option<String>,
    pub while_captures: Option<Captures>,
    pub patterns: Option<Vec<Arc<RawRule>>>,
    pub repository: Option<HashMap<String, Arc<RawRule>>>,
    pub apply_end_pattern_last: bool,
}

/// A grammar file: its rules plus what the registry needs to find it.
pub(crate) struct RawGrammar {
    pub name: String,
    pub scope_name: String,
    pub aliases: Vec<String>,
    pub patterns: Option<Vec<Arc<RawRule>>>,
    pub repository: HashMap<String, Arc<RawRule>>,
    pub injections: Vec<(String, Arc<RawRule>)>,
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_string)
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

fn captures(value: Option<&Value>) -> Option<Captures> {
    let object = value?.as_object()?;
    let mut list: Captures = object
        .iter()
        .filter_map(|(key, rule)| Some((key.parse::<usize>().ok()?, Arc::new(rule_from(rule.as_object()?)))))
        .collect();
    list.sort_by_key(|(index, _)| *index);
    Some(list)
}

fn patterns(value: Option<&Value>) -> Option<Vec<Arc<RawRule>>> {
    let list = value?.as_array()?;
    Some(
        list.iter()
            .map(|item| Arc::new(item.as_object().map(rule_from).unwrap_or_default()))
            .collect(),
    )
}

fn repository(value: Option<&Value>) -> Option<HashMap<String, Arc<RawRule>>> {
    let object = value?.as_object()?;
    Some(
        object
            .iter()
            .filter_map(|(key, rule)| Some((key.clone(), Arc::new(rule_from(rule.as_object()?)))))
            .collect(),
    )
}

fn rule_from(object: &Map<String, Value>) -> RawRule {
    RawRule {
        include: string(object, "include"),
        name: string(object, "name"),
        content_name: string(object, "contentName"),
        match_: string(object, "match"),
        captures: captures(object.get("captures")),
        begin: string(object, "begin"),
        begin_captures: captures(object.get("beginCaptures")),
        end: string(object, "end"),
        end_captures: captures(object.get("endCaptures")),
        while_: string(object, "while"),
        while_captures: captures(object.get("whileCaptures")),
        patterns: patterns(object.get("patterns")),
        repository: repository(object.get("repository")),
        apply_end_pattern_last: truthy(object.get("applyEndPatternLast")),
    }
}

impl RawGrammar {
    pub fn parse(json: &str) -> Option<RawGrammar> {
        let value: Value = serde_json::from_str(json).ok()?;
        let object = value.as_object()?;
        let injections = object
            .get("injections")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(selector, rule)| Some((selector.clone(), Arc::new(rule_from(rule.as_object()?)))))
                    .collect()
            })
            .unwrap_or_default();
        Some(RawGrammar {
            name: string(object, "name")?,
            scope_name: string(object, "scopeName")?,
            aliases: object
                .get("aliases")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(|alias| alias.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            patterns: patterns(object.get("patterns")),
            repository: repository(object.get("repository")).unwrap_or_default(),
            injections,
        })
    }
}

/// A rule tree copied whole, so the copy's rules are new objects (vscode-textmate's `clone`).
fn deep_clone(rule: &RawRule) -> RawRule {
    let list = |captures: &Option<Captures>| {
        captures.as_ref().map(|list| list.iter().map(|(index, rule)| (*index, Arc::new(deep_clone(rule)))).collect())
    };
    RawRule {
        include: rule.include.clone(),
        name: rule.name.clone(),
        content_name: rule.content_name.clone(),
        match_: rule.match_.clone(),
        captures: list(&rule.captures),
        begin: rule.begin.clone(),
        begin_captures: list(&rule.begin_captures),
        end: rule.end.clone(),
        end_captures: list(&rule.end_captures),
        while_: rule.while_.clone(),
        while_captures: list(&rule.while_captures),
        patterns: clone_patterns(&rule.patterns),
        repository: rule.repository.as_ref().map(clone_repository),
        apply_end_pattern_last: rule.apply_end_pattern_last,
    }
}

fn clone_patterns(patterns: &Option<Vec<Arc<RawRule>>>) -> Option<Vec<Arc<RawRule>>> {
    patterns.as_ref().map(|list| list.iter().map(|rule| Arc::new(deep_clone(rule))).collect())
}

fn clone_repository(repository: &HashMap<String, Arc<RawRule>>) -> HashMap<String, Arc<RawRule>> {
    repository.iter().map(|(key, rule)| (key.clone(), Arc::new(deep_clone(rule)))).collect()
}

/// A grammar as one including grammar reads it (`initGrammar`): its own copy of every rule, with
/// `$self` standing for its top level and `$base` for the including grammar's.
pub(crate) struct GrammarCopy {
    pub repository: Repository,
    pub injections: Vec<(String, Arc<RawRule>)>,
    pub this: Arc<RawRule>,
}

impl GrammarCopy {
    pub fn new(raw: &RawGrammar, base: Option<Arc<RawRule>>) -> GrammarCopy {
        let mut repository = clone_repository(&raw.repository);
        let this = Arc::new(RawRule {
            patterns: clone_patterns(&raw.patterns),
            name: Some(raw.scope_name.clone()),
            ..RawRule::default()
        });
        repository.insert("$self".into(), this.clone());
        repository.insert("$base".into(), base.unwrap_or_else(|| this.clone()));
        GrammarCopy {
            repository: Arc::new(repository),
            injections: raw.injections.iter().map(|(selector, rule)| (selector.clone(), Arc::new(deep_clone(rule)))).collect(),
            this,
        }
    }
}
