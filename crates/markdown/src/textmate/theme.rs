//! The theme as vscode-textmate resolves it: a trie of dotted scope segments where the deepest
//! segment a scope reaches decides its colour, and a scope nothing names keeps its parent's.

use std::collections::HashMap;

pub(crate) struct Theme<T: Copy> {
    root: Node<T>,
}

struct Node<T: Copy> {
    value: Option<T>,
    children: HashMap<String, Node<T>>,
}

impl<T: Copy> Node<T> {
    fn insert(&mut self, scope: &str, value: T) {
        if scope.is_empty() {
            self.value = Some(value);
            return;
        }
        let (head, tail) = scope.split_once('.').unwrap_or((scope, ""));
        let inherited = self.value;
        self.children
            .entry(head.to_string())
            .or_insert_with(|| Node { value: inherited, children: HashMap::new() })
            .insert(tail, value);
    }
}

impl<T: Copy> Theme<T> {
    /// Rules in theme order, each a comma-separated selector list and its value.
    pub fn new(rules: &[(&str, T)]) -> Theme<T> {
        let mut parsed: Vec<(&str, usize, T)> = Vec::new();
        for (index, (selectors, value)) in rules.iter().enumerate() {
            for selector in selectors.trim_matches(',').split(',') {
                let scope = selector.trim().rsplit(' ').next().unwrap_or_default();
                parsed.push((scope, index, *value));
            }
        }
        parsed.sort_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(&b.1)));
        let mut root = Node { value: None, children: HashMap::new() };
        for (scope, _, value) in parsed {
            root.insert(scope, value);
        }
        Theme { root }
    }

    /// The value a scope takes, or `None` when it keeps the enclosing scope's.
    pub fn resolve(&self, scope: &str) -> Option<T> {
        let mut node = &self.root;
        let mut rest = scope;
        while !rest.is_empty() {
            let (head, tail) = rest.split_once('.').unwrap_or((rest, ""));
            match node.children.get(head) {
                Some(child) => {
                    node = child;
                    rest = tail;
                }
                None => break,
            }
        }
        node.value
    }
}
