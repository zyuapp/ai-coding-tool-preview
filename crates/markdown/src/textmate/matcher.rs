//! Scope selectors as injections use them (`createMatchers` in vscode-textmate).

pub(crate) enum Matcher {
    Names(Vec<String>),
    Not(Option<Box<Matcher>>),
    All(Vec<Matcher>),
    Any(Vec<Matcher>),
}

impl Matcher {
    pub fn matches(&self, scopes: &[&str]) -> bool {
        match self {
            Matcher::Names(names) => names_match(names, scopes),
            Matcher::Not(inner) => inner.as_ref().is_some_and(|inner| !inner.matches(scopes)),
            Matcher::All(list) => list.iter().all(|matcher| matcher.matches(scopes)),
            Matcher::Any(list) => list.iter().any(|matcher| matcher.matches(scopes)),
        }
    }
}

fn scope_matches(scope: &str, selector: &str) -> bool {
    scope == selector
        || (scope.len() > selector.len() && scope.starts_with(selector) && scope.as_bytes()[selector.len()] == b'.')
}

fn names_match(names: &[String], scopes: &[&str]) -> bool {
    if scopes.len() < names.len() {
        return false;
    }
    let mut from = 0;
    names.iter().all(|name| {
        for (index, scope) in scopes.iter().enumerate().skip(from) {
            if scope_matches(scope, name) {
                from = index + 1;
                return true;
            }
        }
        false
    })
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.' || byte == b':'
}

/// The selector's tokens: `L:`/`R:`, names, and `, | - ( )`.
fn tokens(input: &str) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if (byte == b'L' || byte == b'R') && bytes.get(at + 1) == Some(&b':') {
            out.push(&input[at..at + 2]);
            at += 2;
        } else if word(byte) {
            let start = at;
            at += 1;
            while at < bytes.len() && (word(bytes[at]) || bytes[at] == b'-') {
                at += 1;
            }
            out.push(&input[start..at]);
        } else if matches!(byte, b',' | b'|' | b'-' | b'(' | b')') {
            out.push(&input[at..at + 1]);
            at += 1;
        } else {
            at += 1;
        }
    }
    out
}

fn is_identifier(token: Option<&str>) -> bool {
    token.is_some_and(|token| token.bytes().any(word))
}

struct Parser<'a> {
    tokens: Vec<&'a str>,
    at: usize,
}

impl<'a> Parser<'a> {
    fn token(&self) -> Option<&'a str> {
        self.tokens.get(self.at).copied()
    }

    fn next(&mut self) {
        self.at += 1;
    }

    fn operand(&mut self) -> Option<Matcher> {
        match self.token() {
            Some("-") => {
                self.next();
                Some(Matcher::Not(self.operand().map(Box::new)))
            }
            Some("(") => {
                self.next();
                let inner = self.inner();
                if self.token() == Some(")") {
                    self.next();
                }
                Some(inner)
            }
            token if is_identifier(token) => {
                let mut names = Vec::new();
                while is_identifier(self.token()) {
                    names.push(self.token().unwrap_or_default().to_string());
                    self.next();
                }
                Some(Matcher::Names(names))
            }
            _ => None,
        }
    }

    fn conjunction(&mut self) -> Matcher {
        let mut list = Vec::new();
        while let Some(matcher) = self.operand() {
            list.push(matcher);
        }
        Matcher::All(list)
    }

    fn inner(&mut self) -> Matcher {
        let mut list = Vec::new();
        loop {
            list.push(self.conjunction());
            if matches!(self.token(), Some("|" | ",")) {
                while matches!(self.token(), Some("|" | ",")) {
                    self.next();
                }
            } else {
                break;
            }
        }
        Matcher::Any(list)
    }
}

/// Each comma-separated selector with its priority: -1 for `L:`, 1 for `R:`, else 0.
pub(crate) fn create_matchers(selector: &str) -> Vec<(Matcher, i32)> {
    let mut parser = Parser { tokens: tokens(selector), at: 0 };
    let mut results = Vec::new();
    while parser.token().is_some() {
        let mut priority = 0;
        if let Some(token) = parser.token().filter(|token| token.len() == 2 && token.ends_with(':')) {
            priority = match token {
                "R:" => 1,
                "L:" => -1,
                _ => 0,
            };
            parser.next();
        }
        results.push((parser.conjunction(), priority));
        if parser.token() != Some(",") {
            break;
        }
        parser.next();
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_injection_selector_excludes_its_listed_scopes() {
        let selector = "R:text.html - (comment.block, text.html meta.embedded, meta.tag.*.*.html)";
        let matchers = create_matchers(selector);
        assert_eq!(matchers.len(), 1);
        let (matcher, priority) = &matchers[0];
        assert_eq!(*priority, 1);
        assert!(matcher.matches(&["text.html.basic", "meta.tag.block.any.html"]));
        assert!(!matcher.matches(&["text.html.basic", "comment.block.html"]));
        assert!(!matcher.matches(&["text.html.basic", "meta.embedded.block.html", "source.js"]));
        assert!(!matcher.matches(&["source.js"]));
    }
}
