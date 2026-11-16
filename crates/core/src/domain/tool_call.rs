//! What a tool call did, in one line, and a reply read as one line of plain words.

use serde_json::{Map as JsonMap, Value};

use super::engine::AgentEngine;
use crate::util::{js_len, js_slice_to};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolFamily {
    Shell,
    Read,
    Write,
    Search,
    Web,
    Agent,
    Thread,
    Automation,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub family: ToolFamily,
    pub label: String,
    /// The label says what the call did, so a row shows it even where a run of one tool would drop the name.
    pub verb: bool,
    /// Marks the argument as what it is: `$` for a command.
    pub sigil: Option<String>,
    /// Empty when the call carried nothing worth naming.
    pub argument: String,
    /// The thread the call is about, as the call named it.
    pub thread: Option<String>,
}

fn family_of(engine: Option<AgentEngine>, name: &str) -> Option<ToolFamily> {
    use ToolFamily::*;
    let family = match engine? {
        AgentEngine::Claude => match name {
            "Bash" | "BashOutput" | "KillShell" => Shell,
            "Read" | "NotebookRead" => Read,
            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => Write,
            "Grep" | "Glob" | "ToolSearch" => Search,
            "WebFetch" | "WebSearch" => Web,
            "Agent" | "Task" | "Skill" | "Workflow" | "SendMessage" => Agent,
            _ => return None,
        },
        AgentEngine::Codex => match name {
            "command_execution" => Shell,
            "file_change" => Write,
            "web_search" => Web,
            "mcp_tool_call" => Other,
            _ => return None,
        },
        AgentEngine::Opencode => match name {
            "shell" => Shell,
            "read" => Read,
            "write" | "edit" | "apply_patch" | "patch" | "multiedit" => Write,
            "glob" | "grep" | "list" => Search,
            "webfetch" | "websearch" => Web,
            "subagent" | "task" | "skill" => Agent,
            _ => return None,
        },
        AgentEngine::Other(_) => return None,
    };
    Some(family)
}

fn naming_keys(engine: Option<AgentEngine>) -> &'static [&'static str] {
    match engine {
        Some(AgentEngine::Claude) => &["command", "file_path", "notebook_path", "pattern", "query", "url", "path", "description", "prompt", "skill", "name"],
        Some(AgentEngine::Codex) => &["command", "path", "query"],
        Some(AgentEngine::Opencode) => &["command", "filePath", "pattern", "url", "query", "path", "description", "prompt", "name"],
        None | Some(AgentEngine::Other(_)) => &["command", "file_path", "path", "pattern", "url", "query"],
    }
}

const ARGUMENT_LIMIT: usize = 240;

fn text(input: &JsonMap<String, Value>, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).filter(|value| !value.trim().is_empty()).map(String::from)
}

struct OwnTool {
    family: ToolFamily,
    label: &'static str,
    argument: fn(&JsonMap<String, Value>) -> Option<String>,
    thread: bool,
}

fn none(_: &JsonMap<String, Value>) -> Option<String> {
    None
}

fn own_tool(name: &str) -> Option<OwnTool> {
    let bare = match split_mcp(name) {
        Some((server, tool)) => {
            if !(server == "aicodingtool" || server.starts_with("aicodingtool-")) {
                return None;
            }
            tool
        }
        None => name,
    };
    use ToolFamily::*;
    let tool = |family, label, argument: fn(&JsonMap<String, Value>) -> Option<String>, thread| OwnTool { family, label, argument, thread };
    Some(match bare {
        "list_threads" => tool(Thread, "List threads", |input| text(input, "search"), false),
        "read_thread" => tool(Thread, "Read thread", none, true),
        "wait_for_thread" => tool(Thread, "Wait for thread", none, true),
        "start_thread" => tool(Thread, "Start thread", |input| text(input, "title").or_else(|| text(input, "prompt")), false),
        "message_thread" => tool(Thread, "Message thread", |input| text(input, "text"), true),
        "set_thread_role" => tool(Thread, "Set role", |input| text(input, "role").or_else(|| Some("No role".into())), true),
        "dismiss_thread" => tool(Thread, "Dismiss thread", none, true),
        "archive_thread" => tool(Thread, "Archive thread", none, true),
        "stop_thread" => tool(Thread, "Stop thread", none, true),
        "move_to_worktree" => tool(
            Thread,
            "Move to worktree",
            |input| if input.get("worktreeId").and_then(Value::as_str) == Some("new") { Some("New worktree".into()) } else { text(input, "worktreeId").or_else(|| Some("Project checkout".into())) },
            true,
        ),
        "delete_worktree" => tool(Thread, "Delete worktree", |input| text(input, "worktreeId"), false),
        "report_status" => tool(Thread, "Report status", |input| Some([text(input, "state"), text(input, "summary")].into_iter().flatten().collect::<Vec<_>>().join(" · ")), false),
        "raise_decision" => tool(Thread, "Ask for a decision", |input| text(input, "question"), false),
        "remember" => tool(Thread, "Remember", |input| text(input, "text"), false),
        "forget" => tool(Thread, "Forget", |input| text(input, "noteId"), false),
        "set_purpose" => tool(Thread, "Set purpose", |input| text(input, "purpose"), false),
        "propose_threads" => tool(
            Thread,
            "Propose threads",
            |input| input.get("threads").and_then(Value::as_array).map(|threads| threads.iter().filter_map(|thread| thread.as_object().and_then(|thread| text(thread, "title"))).collect::<Vec<_>>().join(", ")),
            false,
        ),
        "notify" => tool(Automation, "Notify", |input| text(input, "headline"), false),
        "nothing_to_report" => tool(Automation, "Nothing to report", |input| text(input, "checked"), false),
        "schedule" => tool(Automation, "Schedule", |input| text(input, "schedule"), false),
        "status" => tool(Automation, "Automation status", none, false),
        "update" => tool(Automation, "Update automation", none, false),
        "stop" => tool(Automation, "Stop automation", none, false),
        "list_all" => tool(Automation, "List automations", none, false),
        "terminal_list" => tool(Read, "List terminals", none, false),
        "terminal_read" => tool(Read, "Read terminal", |input| text(input, "match"), false),
        "browser_open" => tool(Web, "Open page", |input| text(input, "url"), false),
        "browser_search" => tool(Web, "Search", |input| text(input, "query"), false),
        "browser_read" => tool(Web, "Read page", none, false),
        "browser_screenshot" => tool(Web, "Screenshot", none, false),
        "browser_console" => tool(Web, "Console", none, false),
        "browser_network" => tool(Web, "Network", none, false),
        "browser_wait" => tool(Web, "Wait", |input| text(input, "value"), false),
        "browser_click" => tool(Web, "Click", none, false),
        "browser_type" => tool(Web, "Type", |input| text(input, "text"), false),
        "browser_back" => tool(Web, "Back", none, false),
        "browser_tabs" => tool(Web, "List tabs", none, false),
        "browser_close_tab" => tool(Web, "Close tab", none, false),
        "simulator_open" => tool(Other, "Open simulator", |input| text(input, "device"), false),
        "simulator_read" => tool(Read, "Read simulator", none, false),
        "simulator_tap" => tool(Other, "Tap", |input| text(input, "ref"), false),
        "simulator_swipe" => tool(Other, "Swipe", none, false),
        "simulator_type" => tool(Other, "Type", |input| text(input, "text"), false),
        "simulator_button" => tool(Other, "Press button", |input| text(input, "button"), false),
        "simulator_rotate" => tool(Other, "Rotate", |input| text(input, "orientation"), false),
        "request_setup" => tool(Other, "Set up computer use", none, false),
        _ => return None,
    })
}

/// `mcp__server__tool` split into its server and tool, matching the shortest server name.
fn split_mcp(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let at = rest.find("__")?;
    let (server, tool) = (&rest[..at], &rest[at + 2..]);
    (!server.is_empty() && !tool.is_empty()).then_some((server, tool))
}

/// What a person reads a tool as.
pub fn tool_label(name: &str) -> String {
    if let Some(own) = own_tool(name) {
        return own.label.into();
    }
    match split_mcp(name) {
        Some((server, tool)) => format!("{server} · {}", tool.replace('_', " ")),
        None => name.into(),
    }
}

pub fn tool_family(engine: Option<AgentEngine>, name: &str) -> ToolFamily {
    family_of(engine, name).or_else(|| own_tool(name).map(|own| own.family)).unwrap_or(if name.starts_with("browser_") { ToolFamily::Web } else { ToolFamily::Other })
}

fn short_path(value: &str) -> String {
    let segments: Vec<&str> = value.split('/').filter(|part| !part.is_empty()).collect();
    if !value.starts_with('/') || segments.len() <= 2 {
        return value.into();
    }
    format!("…/{}", segments[segments.len() - 2..].join("/"))
}

fn one_line(value: &str) -> String {
    let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if js_len(&flat) > ARGUMENT_LIMIT {
        format!("{}…", js_slice_to(&flat, ARGUMENT_LIMIT - 1))
    } else {
        flat
    }
}

fn naming_value(keys: &[&str], input: &JsonMap<String, Value>) -> Option<(String, String)> {
    for key in keys {
        if let Some(value) = text(input, key) {
            return Some((key.to_string(), value.trim().to_string()));
        }
    }
    let strings: Vec<(&String, &str)> = input.iter().filter_map(|(key, value)| value.as_str().filter(|value| !value.trim().is_empty()).map(|value| (key, value))).collect();
    (strings.len() == 1).then(|| (strings[0].0.clone(), strings[0].1.trim().to_string()))
}

/// What a tool call did, from its name and the JSON it was given.
pub fn describe_tool_call(engine: Option<AgentEngine>, name: &str, detail: Option<&str>) -> ToolCall {
    let input = detail.and_then(|detail| serde_json::from_str::<Value>(detail).ok()).and_then(|value| value.as_object().cloned());
    if let Some(own) = own_tool(name) {
        let input = input.unwrap_or_default();
        let argument = (own.argument)(&input).map(|argument| one_line(&argument)).unwrap_or_default();
        let thread = if own.thread { text(&input, "threadId").map(|id| id.trim().to_string()) } else { None };
        return ToolCall { family: own.family, label: own.label.into(), verb: true, sigil: None, argument, thread };
    }
    let family = tool_family(engine, name);
    let label = tool_label(name);
    let Some(input) = input else { return ToolCall { family, label, verb: false, sigil: None, argument: String::new(), thread: None } };
    let Some((key, value)) = naming_value(naming_keys(engine), &input) else { return ToolCall { family, label, verb: false, sigil: None, argument: String::new(), thread: None } };
    let is_path = key.to_lowercase().ends_with("path");
    let argument = one_line(&if is_path { short_path(&value) } else { value });
    let scope = match (family, text(&input, "path")) {
        (ToolFamily::Search, Some(path)) => format!(" in {}", short_path(path.trim())),
        _ => String::new(),
    };
    ToolCall { family, label, verb: false, sigil: (family == ToolFamily::Shell).then(|| "$".into()), argument: format!("{argument}{scope}"), thread: None }
}

/// A line that only fences code or rules a section off, which says nothing.
fn bare(line: &str) -> bool {
    if line.starts_with("```") || line.starts_with("~~~") {
        return true;
    }
    let marks: String = line.chars().filter(|ch| !ch.is_whitespace()).collect();
    marks.len() >= 3 && line.trim_end() == line && (marks.chars().all(|ch| ch == '-') || marks.chars().all(|ch| ch == '*') || marks.chars().all(|ch| ch == '_')) && line.chars().all(|ch| "-*_ \t".contains(ch))
}

/// What opens a markdown line rather than saying something.
fn without_opening(line: &str) -> &str {
    let mut rest = line;
    while let Some(after) = rest.strip_prefix('>') {
        rest = after.strip_prefix(' ').unwrap_or(after);
    }
    let hashes = rest.chars().take_while(|ch| *ch == '#').count();
    if (1..=6).contains(&hashes) && rest[hashes..].starts_with(char::is_whitespace) {
        return rest[hashes..].trim_start();
    }
    let item = if rest.starts_with(['-', '*', '+']) {
        Some(1)
    } else {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && rest[digits..].starts_with(['.', ')'])).then_some(digits + 1)
    };
    if let Some(width) = item {
        if rest[width..].starts_with(char::is_whitespace) {
            let mut after = rest[width..].trim_start();
            for task in ["[ ]", "[x]", "[X]"] {
                if let Some(tail) = after.strip_prefix(task) {
                    if tail.starts_with(char::is_whitespace) {
                        after = tail.trim_start();
                    }
                }
            }
            return after;
        }
    }
    rest
}

/// Text with its inline markdown read as words: code spans as written, links and emphasis by their words.
fn inline_text(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '`' {
            let ticks = chars[index..].iter().take_while(|c| **c == '`').count();
            let fence: String = std::iter::repeat_n('`', ticks).collect();
            let rest: String = chars[index + ticks..].iter().collect();
            if let Some(end) = rest.find(&fence) {
                if end > 0 {
                    out.push_str(&rest[..end]);
                    index += ticks + rest[..end].chars().count() + ticks;
                    continue;
                }
            }
        }
        if ch == '[' || (ch == '!' && chars.get(index + 1) == Some(&'[')) {
            let open = if ch == '!' { index + 1 } else { index };
            if let Some(close) = chars[open..].iter().position(|c| *c == ']').map(|at| open + at) {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = chars[close + 1..].iter().position(|c| *c == ')').map(|at| close + 1 + at) {
                        let words: String = chars[open + 1..close].iter().collect();
                        out.push_str(&inline_text(&words));
                        index = end + 1;
                        continue;
                    }
                }
            }
        }
        let marker = ["**", "~~", "__", "*", "_"].into_iter().find(|marker| chars[index..].starts_with(&marker.chars().collect::<Vec<_>>()));
        if let Some(marker) = marker {
            let width = marker.chars().count();
            let underscore = marker.starts_with('_');
            let word_before = index > 0 && (chars[index - 1].is_alphanumeric() || chars[index - 1] == '_');
            let opens = chars.get(index + width).is_some_and(|next| !next.is_whitespace()) && !(underscore && word_before);
            if opens {
                let body_start = index + width;
                let pattern: Vec<char> = marker.chars().collect();
                let mut search = body_start + 1;
                while search + width <= chars.len() {
                    if chars[search..search + width] == pattern[..] && !chars[search - 1].is_whitespace() {
                        let word_after = chars.get(search + width).is_some_and(|next| next.is_alphanumeric() || *next == '_');
                        if !(underscore && word_after) {
                            let words: String = chars[body_start..search].iter().collect();
                            out.push_str(&inline_text(&words));
                            index = search + width;
                            break;
                        }
                    }
                    search += 1;
                }
                if index >= body_start {
                    continue;
                }
            }
        }
        out.push(ch);
        index += 1;
    }
    out
}

fn plain_line(line: &str) -> String {
    let trimmed = line.trim();
    if bare(trimmed) {
        return String::new();
    }
    inline_text(without_opening(trimmed)).trim().to_string()
}

/// The first line of a reply that says something, as plain words clipped to `max`.
pub fn reply_line(reply: &str, max: usize) -> Option<String> {
    for line in reply.split('\n') {
        let line = plain_line(line);
        if !line.is_empty() {
            return Some(if js_len(&line) > max { format!("{}…", js_slice_to(&line, max - 1)) } else { line });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_lead_with_their_argument() {
        let call = describe_tool_call(Some(AgentEngine::Claude), "Bash", Some(r#"{"command":"ls   -la"}"#));
        assert_eq!(call.argument, "ls -la");
        assert_eq!(call.sigil.as_deref(), Some("$"));
        let read = describe_tool_call(Some(AgentEngine::Claude), "Read", Some(r#"{"file_path":"/Users/me/repo/src/main.rs"}"#));
        assert_eq!(read.argument, "…/src/main.rs");
        assert_eq!(tool_label("mcp__github__create_issue"), "github · create issue");
        assert_eq!(tool_label("mcp__aicodingtool-threads__read_thread"), "Read thread");
    }

    fn call(engine: Option<AgentEngine>, name: &str, detail: &str) -> ToolCall {
        describe_tool_call(engine, name, Some(detail))
    }

    const CLAUDE: Option<AgentEngine> = Some(AgentEngine::Claude);
    const CODEX: Option<AgentEngine> = Some(AgentEngine::Codex);

    #[test]
    fn a_call_reads_by_its_argument_and_family() {
        let bash = call(CLAUDE, "Bash", r#"{"command":"git status --short"}"#);
        assert_eq!((bash.family, bash.label.as_str(), bash.sigil.as_deref(), bash.argument.as_str()), (ToolFamily::Shell, "Bash", Some("$"), "git status --short"));
        assert_eq!(call(CLAUDE, "WebSearch", r#"{"query":"electron menu roles"}"#).family, ToolFamily::Web);
        assert_eq!(call(CLAUDE, "Task", r#"{"description":"audit the reducer","prompt":"…"}"#).argument, "audit the reducer");
        assert_eq!(call(CLAUDE, "Read", r#"{"file_path":"src/renderer/styles.css"}"#).argument, "src/renderer/styles.css");
        assert_eq!(call(CLAUDE, "Read", r#"{"file_path":"/etc/hosts"}"#).argument, "/etc/hosts");
        assert_eq!(call(CLAUDE, "Grep", r#"{"pattern":"work-row","path":"src/renderer"}"#).argument, "work-row in src/renderer");
        assert_eq!(call(CLAUDE, "Grep", r#"{"pattern":"work-row"}"#).argument, "work-row");
        assert_eq!(call(CLAUDE, "Bash", "{\"command\":\"cat <<'EOF' > f\\n  one\\n  two\\nEOF\"}").argument, "cat <<'EOF' > f one two EOF");
        let long = call(CLAUDE, "Bash", &format!(r#"{{"command":"{}"}}"#, "x".repeat(400))).argument;
        assert_eq!((crate::util::js_len(&long), long.ends_with('…')), (240, true));
        assert_eq!(call(CLAUDE, "Bash", "not json").argument, "");
        assert_eq!(call(CLAUDE, "ListAgents", "{}").argument, "");
        assert_eq!(call(CLAUDE, "Edit", r#"{"old_string":"a","new_string":"b"}"#).argument, "");
        assert_eq!(tool_family(CLAUDE, "browser_click"), ToolFamily::Web);
        assert_eq!(tool_family(CLAUDE, "mcp__aicodingtool-terminal__terminal_read"), ToolFamily::Read);
        assert_eq!(tool_family(CLAUDE, "MultiEdit"), ToolFamily::Write);
    }

    #[test]
    fn codex_and_unknown_engines_read_by_their_common_argument() {
        let command = call(CODEX, "command_execution", r#"{"command":"npm test"}"#);
        assert_eq!((command.family, command.sigil.as_deref(), command.argument.as_str()), (ToolFamily::Shell, Some("$"), "npm test"));
        assert_eq!(call(CODEX, "file_change", r#"{"path":"/Users/x/repo/src/main/index.ts"}"#).argument, "…/main/index.ts");
        assert_eq!(tool_family(CODEX, "file_change"), ToolFamily::Write);
        assert_eq!(call(CODEX, "web_search", r#"{"query":"codex app-server"}"#).family, ToolFamily::Web);
        assert_eq!(tool_family(CODEX, "mcp_tool_call"), ToolFamily::Other);
        assert_eq!(tool_family(CODEX, "browser_click"), ToolFamily::Web);
        let unknown = call(None, "run_shell", r#"{"command":"ls -la"}"#);
        assert_eq!((unknown.family, unknown.argument.as_str()), (ToolFamily::Other, "ls -la"));
        assert_eq!(call(None, "browser_open", r#"{"url":"https://example.com"}"#).family, ToolFamily::Web);
    }

    #[test]
    fn the_apps_own_tools_read_as_a_verb_and_what_they_acted_on() {
        let start = r#"{"title":"Review the refactor","prompt":"You are the reviewer…","role":"reviewer"}"#;
        for (engine, name) in [(CLAUDE, "mcp__aicodingtool-threads__start_thread"), (CODEX, "start_thread"), (Some(AgentEngine::Opencode), "start_thread")] {
            let started = call(engine, name, start);
            assert_eq!((started.family, started.label.as_str(), started.verb, started.argument.as_str()), (ToolFamily::Thread, "Start thread", true, "Review the refactor"));
        }
        assert_eq!(call(CLAUDE, "mcp__aicodingtool-threads__start_thread", r#"{"prompt":"Fix\nthe dock"}"#).argument, "Fix the dock");
        let id = "dedaef8d-9c0c-4f99-a08f-52122b6b5875";
        let wait = call(CLAUDE, "mcp__aicodingtool-threads__wait_for_thread", &format!(r#"{{"threadId":"{id}","timeoutSeconds":900}}"#));
        assert_eq!((wait.label.as_str(), wait.argument.as_str(), wait.thread.as_deref()), ("Wait for thread", "", Some(id)));
        let message = call(CODEX, "message_thread", &format!(r#"{{"threadId":"{id}","text":"Round 4.\nFixed: all"}}"#));
        assert_eq!((message.label.as_str(), message.argument.as_str(), message.thread.as_deref()), ("Message thread", "Round 4. Fixed: all", Some(id)));
        assert_eq!(call(CLAUDE, "mcp__aicodingtool-threads__move_to_worktree", r#"{"worktreeId":"new"}"#).argument, "New worktree");
        assert_eq!(call(CLAUDE, "mcp__aicodingtool-threads__move_to_worktree", r#"{"worktreeId":null}"#).argument, "Project checkout");
        let notify = call(CLAUDE, "mcp__aicodingtool-automation__notify", r#"{"headline":"CI is red","detail":"…","key":"ci"}"#);
        assert_eq!((notify.family, notify.label.as_str(), notify.argument.as_str()), (ToolFamily::Automation, "Notify", "CI is red"));
        let open = call(CLAUDE, "mcp__aicodingtool-browser__browser_open", r#"{"url":"https://example.com","newTab":true}"#);
        assert_eq!((open.family, open.label.as_str(), open.argument.as_str()), (ToolFamily::Web, "Open page", "https://example.com"));
        let click = call(CODEX, "browser_click", r#"{"ref":"e12"}"#);
        assert_eq!((click.label.as_str(), click.verb, click.argument.as_str()), ("Click", true, ""));
        assert_eq!(call(CLAUDE, "mcp__aicodingtool-terminal__terminal_read", "{}").label, "Read terminal");
        assert_eq!(tool_label("mcp__cua-driver__get_window_state"), "cua-driver · get window state");
        assert_eq!(tool_label("mcp__cua-driver__status"), "cua-driver · status");
        assert!(!call(CLAUDE, "mcp__cua-driver__status", "{}").verb);
        assert_eq!(tool_label("mcp__aicodingtool-threads__archive_thread"), "Archive thread");
    }

    #[test]
    fn reply_lines_read_markdown_as_words() {
        assert_eq!(reply_line("```\n\n## **Done**: fixed `a_b` in [file](x)", 120).as_deref(), Some("Done: fixed a_b in file"));
        assert_eq!(reply_line("- [x] shipped it", 120).as_deref(), Some("shipped it"));
        assert_eq!(reply_line("snake_case_name stays", 120).as_deref(), Some("snake_case_name stays"));
    }
}
