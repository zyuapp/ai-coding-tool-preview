//! How a thread is named inside a draft, and what that name expands to when the draft is sent.

use crate::domain::project::project_name;
use crate::domain::thread::thread_last_output_at;
use crate::state::WorkspaceState;
use crate::workspace::activity::thread_activity;

const SLUG_WORDS: usize = 4;
const SLUG_CHARS: usize = 40;

/// A thread the `@` menu offers, and that a handle in a draft resolves back to.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadHandleOption {
    pub id: String,
    pub title: String,
    /// `slug`, or `project/slug` for a thread outside the current project.
    pub handle: String,
    pub project: Option<String>,
    pub in_scope: bool,
    pub running: bool,
    pub last_activity_at: i64,
}

pub fn thread_slug(text: &str) -> String {
    let lowered: String = text.to_lowercase().chars().filter(|ch| *ch != '\'' && *ch != '\u{2019}').collect();
    let spaced: String = lowered.chars().map(|ch| if ch.is_ascii_lowercase() || ch.is_ascii_digit() { ch } else { ' ' }).collect();
    let words: Vec<&str> = spaced.split_whitespace().take(SLUG_WORDS).collect();
    let joined = words.join("-");
    let clipped: String = joined.chars().take(SLUG_CHARS).collect();
    let trimmed = clipped.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "thread".into()
    } else {
        trimmed
    }
}

/// The `@word` the caret sits in, as the query typed and where its `@` starts. `caret` is a char index.
pub fn handle_token_at(text: &str, caret: usize) -> Option<(String, usize)> {
    let before: Vec<char> = text.chars().take(caret).collect();
    let mut start = before.len();
    while start > 0 && !before[start - 1].is_whitespace() && before[start - 1] != '@' {
        start -= 1;
    }
    if start == 0 || before[start - 1] != '@' {
        return None;
    }
    let at = start - 1;
    if at > 0 && !before[at - 1].is_whitespace() {
        return None;
    }
    let query: String = before[start..].iter().collect();
    Some((query.to_lowercase(), at))
}

/// Handles for a set of threads, newest activity first; a shared slug is broken by the older one's id.
pub fn thread_handles(mut threads: Vec<ThreadHandleOption>) -> Vec<ThreadHandleOption> {
    threads.sort_by(|left, right| right.last_activity_at.cmp(&left.last_activity_at));
    let mut taken = std::collections::HashSet::new();
    for thread in threads.iter_mut() {
        let slug = thread_slug(&thread.title);
        let qualified = match (&thread.project, thread.in_scope) {
            (Some(project), false) if !project.is_empty() => format!("{}/{slug}", thread_slug(project)),
            _ => slug,
        };
        let handle = if taken.contains(&qualified) {
            let id: String = thread.id.chars().filter(|ch| ch.is_ascii_alphanumeric()).collect();
            let tail: String = id.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
            format!("{qualified}-{}", tail.to_lowercase())
        } else {
            qualified
        };
        taken.insert(handle.clone());
        thread.handle = handle;
    }
    threads
}

/// Whether a query starts a word of a title.
pub fn starts_title_word(title: &str, query: &str) -> bool {
    let wanted = query.to_lowercase();
    if wanted.is_empty() || !wanted.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()) {
        return false;
    }
    let lower = title.to_lowercase();
    let mut at_word_start = true;
    for (index, ch) in lower.char_indices() {
        let in_word = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if in_word && at_word_start && lower[index..].starts_with(&wanted) {
            return true;
        }
        at_word_start = !in_word;
    }
    false
}

pub fn thread_handle_matches(option: &ThreadHandleOption, query: &str) -> bool {
    query.is_empty() || option.handle.contains(&query.to_lowercase()) || starts_title_word(&option.title, query)
}

/// In-project threads first, then the rest, each group newest first.
pub fn rank_thread_handles(options: &[ThreadHandleOption], query: &str, limit: usize) -> Vec<ThreadHandleOption> {
    let mut inside = Vec::new();
    let mut outside = Vec::new();
    for option in options {
        if (query.is_empty() && !option.in_scope) || !thread_handle_matches(option, query) {
            continue;
        }
        if option.in_scope {
            inside.push(option.clone());
        } else {
            outside.push(option.clone());
        }
        if inside.len() == limit {
            break;
        }
    }
    inside.into_iter().chain(outside).take(limit).collect()
}

/// The link a thread reads as once it leaves the composer.
pub fn thread_reference(id: &str, title: &str) -> String {
    let title: String = title.chars().filter(|ch| *ch != '[' && *ch != ']').collect();
    format!("[{title}](aicodingtool://thread/{id})")
}

pub fn thread_link(thread_id: &str) -> String {
    format!("<aicodingtool://thread/{thread_id}>")
}

/// Turns every handle that names a thread into a link. One that names nothing is left as typed.
pub fn expand_thread_handles(text: &str, options: &[ThreadHandleOption]) -> String {
    if !text.contains('@') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        let starts = chars[index] == '@' && (index == 0 || chars[index - 1].is_whitespace());
        if starts {
            let mut end = index + 1;
            while end < chars.len() && !chars[end].is_whitespace() && chars[end] != '@' {
                end += 1;
            }
            if end > index + 1 {
                let token: String = chars[index + 1..end].iter().collect();
                let trailing_len = token.chars().rev().take_while(|ch| ".,;:!?)]".contains(*ch)).count();
                let core: String = token.chars().take(token.chars().count() - trailing_len).collect();
                let trailing: String = token.chars().skip(token.chars().count() - trailing_len).collect();
                if let Some(option) = options.iter().find(|option| option.handle == core.to_lowercase()) {
                    out.push_str(&thread_reference(&option.id, &option.title));
                    out.push_str(&trailing);
                    index = end;
                    continue;
                }
                out.push('@');
                out.push_str(&token);
                index = end;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// The threads the `@` menu offers in the draft keyed `draft_key`.
pub fn thread_handle_options(state: &WorkspaceState, draft_key: &str) -> Vec<ThreadHandleOption> {
    let forked = state.side_chat_ids();
    let working = thread_activity(state).working;
    let caller = state.thread(draft_key);
    let project_id = match caller {
        Some(caller) => caller.project_id.clone(),
        None => state.draft_project_id.clone(),
    };
    let options = state
        .threads
        .iter()
        .filter(|thread| thread.id != draft_key && !forked.contains(&thread.id) && thread.archived_at.is_none())
        .map(|thread| ThreadHandleOption {
            id: thread.id.clone(),
            title: thread.title.clone(),
            handle: String::new(),
            project: thread.project_id.as_deref().and_then(|id| state.project(id)).map(project_name),
            in_scope: thread.project_id == project_id,
            running: working.contains(&thread.id),
            last_activity_at: thread_last_output_at(thread),
        })
        .collect();
    thread_handles(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &str, title: &str, handle: &str) -> ThreadHandleOption {
        ThreadHandleOption { id: id.into(), title: title.into(), handle: handle.into(), project: None, in_scope: true, running: false, last_activity_at: 0 }
    }

    #[test]
    fn slugs_keep_apostrophes_inside_words() {
        assert_eq!(thread_slug("Fix the dock's grip!"), "fix-the-docks-grip");
        assert_eq!(thread_slug("!!!"), "thread");
    }

    fn listed(id: &str, title: &str, project: &str, in_scope: bool, last_activity_at: i64) -> ThreadHandleOption {
        ThreadHandleOption { project: Some(project.into()), in_scope, last_activity_at, ..option(id, title, "") }
    }

    #[test]
    fn a_handle_opens_only_after_whitespace() {
        assert_eq!(handle_token_at("@sink", 5), Some(("sink".into(), 0)));
        assert_eq!(handle_token_at("compare with @si", 16), Some(("si".into(), 13)));
        assert_eq!(handle_token_at("@", 1), Some((String::new(), 0)));
        assert_eq!(handle_token_at("mail me at someone@example.com", 30), None);
        assert_eq!(handle_token_at("@sink and more", 14), None);
    }

    #[test]
    fn a_handle_names_the_project_outside_scope_and_a_shared_slug_takes_the_id() {
        let handles = thread_handles(vec![listed("t-1", "Sink the mode choices", "app", true, 3), listed("t-9f2c", "Sink the mode choices", "app", true, 1), listed("t-3", "Sink the mode choices", "site", false, 2)]);
        assert_eq!(handles.iter().map(|handle| handle.handle.as_str()).collect::<Vec<_>>(), ["sink-the-mode-choices", "site/sink-the-mode-choices", "sink-the-mode-choices-9f2c"]);
        let options = thread_handles(vec![listed("here", "Raise the dock", "app", true, 2), listed("away", "Raise the panel", "site", false, 3)]);
        let ids = |query: &str, limit: usize| rank_thread_handles(&options, query, limit).into_iter().map(|option| option.id).collect::<Vec<_>>();
        assert_eq!(ids("", usize::MAX), ["here"]);
        assert_eq!(ids("raise", usize::MAX), ["here", "away"]);
        assert_eq!(ids("raise", 1), ["here"]);
        assert_eq!(ids("panel", usize::MAX), ["away"]);
    }

    #[test]
    fn handles_expand_keeping_trailing_punctuation() {
        let options = vec![option("t1", "Fix [dock]", "fix-dock")];
        assert_eq!(expand_thread_handles("see @fix-dock. and me@fix-dock", &options), "see [Fix dock](aicodingtool://thread/t1). and me@fix-dock");
        assert_eq!(expand_thread_handles("@nobody here", &options), "@nobody here");
    }
}
