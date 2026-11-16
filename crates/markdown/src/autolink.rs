//! GFM autolink literals (`www.`, `http(s)://`, and bare emails), which remark-gfm links and
//! pulldown-cmark leaves as text.

use std::ops::Range;

/// A literal link found in a run of text: its byte range and the href it becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Literal {
    pub range: Range<usize>,
    pub href: String,
}

fn boundary_before(text: &str, at: usize) -> bool {
    match text[..at].chars().next_back() {
        None => true,
        Some(ch) => ch.is_whitespace() || matches!(ch, '*' | '_' | '~' | '('),
    }
}

/// The length of a valid domain at the start of `text`: labels of letters, digits, `-` and `_`
/// joined by dots, with no underscore in the last two labels.
fn domain_len(text: &str, need_dot: bool) -> Option<usize> {
    let mut end = 0;
    let mut labels: Vec<Range<usize>> = Vec::new();
    let mut label_start = 0;
    for (index, ch) in text.char_indices() {
        if ch.is_alphanumeric() || ch == '-' || ch == '_' {
            end = index + ch.len_utf8();
        } else if ch == '.' && end == index && index > label_start {
            labels.push(label_start..index);
            label_start = index + 1;
        } else {
            break;
        }
    }
    if end > label_start {
        labels.push(label_start..end);
    } else if !labels.is_empty() {
        // A trailing dot ends the domain before it.
        end = labels.last().map_or(0, |label| label.end);
    }
    if labels.is_empty() || (need_dot && labels.len() < 2) {
        return None;
    }
    let tail = labels.len().saturating_sub(2);
    if labels[tail..]
        .iter()
        .any(|label| text[label.clone()].contains('_'))
    {
        return None;
    }
    Some(end)
}

/// Trims what GFM leaves off the end of a literal link: trailing punctuation, an unbalanced `)`,
/// and an entity-like `&name;`.
fn trim_tail(link: &str) -> usize {
    let mut end = link.len();
    loop {
        let current = &link[..end];
        let Some(last) = current.chars().next_back() else {
            return end;
        };
        if matches!(
            last,
            '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"'
        ) {
            end -= 1;
        } else if last == ')' {
            let open = current.matches('(').count();
            let close = current.matches(')').count();
            if close > open {
                end -= 1;
            } else {
                return end;
            }
        } else if last == ';' {
            let body = &current[..current.len() - 1];
            let name_len = body.len()
                - body
                    .trim_end_matches(|ch: char| ch.is_ascii_alphanumeric())
                    .len();
            if name_len > 0 && body[..body.len() - name_len].ends_with('&') {
                end = body.len() - name_len - 1;
            } else {
                return end;
            }
        } else {
            return end;
        }
    }
}

fn path_len(text: &str) -> usize {
    text.find(|ch: char| ch.is_whitespace() || ch == '<')
        .unwrap_or(text.len())
}

fn url_at(text: &str, at: usize) -> Option<Literal> {
    let rest = &text[at..];
    let lower: String = rest
        .chars()
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let (scheme_len, need_dot, prefix) = if lower.starts_with("https://") {
        (8, false, "")
    } else if lower.starts_with("http://") {
        (7, false, "")
    } else if lower.starts_with("www.") {
        (0, true, "http://")
    } else {
        return None;
    };
    if !boundary_before(text, at) {
        return None;
    }
    let domain = domain_len(&rest[scheme_len..], need_dot)?;
    let after_domain = scheme_len + domain;
    let end = after_domain + path_len(&rest[after_domain..]);
    let end = trim_tail(&rest[..end]);
    if end <= scheme_len {
        return None;
    }
    Some(Literal {
        range: at..at + end,
        href: format!("{prefix}{}", &rest[..end]),
    })
}

fn is_local_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '.' | '+' | '-' | '_')
}

fn email_at(text: &str, at_sign: usize) -> Option<Literal> {
    let local_start = text[..at_sign]
        .char_indices()
        .rev()
        .take_while(|(_, ch)| is_local_char(*ch))
        .last()?
        .0;
    if text[..local_start]
        .chars()
        .next_back()
        .is_some_and(|ch| ch.is_alphanumeric() || ch == '/' || ch == '@')
    {
        return None;
    }
    let after = &text[at_sign + 1..];
    let mut end = 0;
    let mut dots = 0;
    for (index, ch) in after.char_indices() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            end = index + 1;
        } else if ch == '.'
            && after[index + 1..].starts_with(|next: char| next.is_ascii_alphanumeric())
        {
            dots += 1;
        } else {
            break;
        }
    }
    if dots == 0 || end == 0 || after[..end].ends_with(['-', '_']) {
        return None;
    }
    let range = local_start..at_sign + 1 + end;
    Some(Literal {
        href: format!("mailto:{}", &text[range.clone()]),
        range,
    })
}

/// Every literal link in `text`, in order and not overlapping.
pub fn find_literals(text: &str) -> Vec<Literal> {
    let mut found = Vec::new();
    if !(text.contains("://")
        || text.contains("www.")
        || text.contains("WWW.")
        || text.contains('@'))
    {
        return found;
    }
    let mut index = 0;
    while index < text.len() {
        let ch = text[index..].chars().next().unwrap_or(' ');
        let literal = match ch {
            'h' | 'H' | 'w' | 'W' => url_at(text, index),
            '@' => email_at(text, index).filter(|literal| {
                found
                    .last()
                    .is_none_or(|last: &Literal| last.range.end <= literal.range.start)
            }),
            _ => None,
        };
        if let Some(literal) = literal {
            index = literal.range.end;
            found.push(literal);
        } else {
            index += ch.len_utf8();
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hrefs(text: &str) -> Vec<(String, String)> {
        find_literals(text)
            .into_iter()
            .map(|literal| (text[literal.range].to_string(), literal.href))
            .collect()
    }

    #[test]
    fn literal_links_drop_trailing_punctuation() {
        assert_eq!(
            hrefs("Read https://example.com/docs."),
            vec![(
                "https://example.com/docs".into(),
                "https://example.com/docs".into()
            )]
        );
        assert_eq!(
            hrefs("(see www.commonmark.org/a_b)"),
            vec![(
                "www.commonmark.org/a_b".into(),
                "http://www.commonmark.org/a_b".into()
            )]
        );
        assert_eq!(
            hrefs("https://en.wikipedia.org/wiki/Rust_(language)"),
            vec![(
                "https://en.wikipedia.org/wiki/Rust_(language)".into(),
                "https://en.wikipedia.org/wiki/Rust_(language)".into()
            )]
        );
        assert_eq!(
            hrefs("www.a.com/&hl;"),
            vec![("www.a.com/".into(), "http://www.a.com/".into())]
        );
    }

    #[test]
    fn emails_and_non_links() {
        assert_eq!(
            hrefs("mail foo.bar@example.com now"),
            vec![(
                "foo.bar@example.com".into(),
                "mailto:foo.bar@example.com".into()
            )]
        );
        assert!(hrefs("not a link: xhttps://a.com or www. alone").is_empty());
        assert!(hrefs("user@host").is_empty());
    }
}
