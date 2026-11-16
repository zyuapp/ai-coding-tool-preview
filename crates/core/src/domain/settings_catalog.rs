//! Every control Settings draws, as data, and the jump panel's search over threads and settings.

use super::engine::{engine_label, AGENT_ENGINES, AgentEngine};
use super::pull_request::{links_pull_request, pull_request_query, PullRequestLink};
use super::shortcuts::shortcut_actions;
use super::sidebar::{SettingsSection, SETTINGS_SECTIONS};
use crate::workspace::handles::starts_title_word;

pub fn settings_page_label(section: SettingsSection) -> &'static str {
    match section {
        SettingsSection::General => "General",
        SettingsSection::Appearance => "Appearance",
        SettingsSection::Usage => "Usage",
        SettingsSection::Engines => "Engines",
        SettingsSection::Projects => "Projects",
        SettingsSection::Worktrees => "Worktrees",
        SettingsSection::Shortcuts => "Shortcuts",
        SettingsSection::ComputerUse => "Computer use",
        SettingsSection::Browser => "Browser",
        SettingsSection::Phone => "Devices",
        SettingsSection::Archive => "Archived threads",
    }
}

fn engine_names() -> String {
    AGENT_ENGINES.iter().map(|engine| engine_label(*engine).to_lowercase()).collect::<Vec<_>>().join(" ")
}

fn page_keywords(section: SettingsSection) -> String {
    match section {
        SettingsSection::General => "cli terminal command notifications chrome dictation voice".into(),
        SettingsSection::Appearance => "theme colours colors dark light font text size".into(),
        SettingsSection::Usage => "plan limits quota tokens spend".into(),
        SettingsSection::Engines => format!("{} sign in account model provider", engine_names()),
        SettingsSection::Projects => "folders add remove rename edit path".into(),
        SettingsSection::Worktrees => "git branch checkout managed disk".into(),
        SettingsSection::Shortcuts => "keyboard keys binding keystroke rebind".into(),
        SettingsSection::ComputerUse => "accessibility screen recording permissions automation apps platform setup linux runtime wayland x11".into(),
        SettingsSection::Browser => "session cookies sign out sites origins clear data import sign in chrome brave firefox edge arc vivaldi opera".into(),
        SettingsSection::Phone => "mobile phone computer remote pairing tailscale device qr serve".into(),
        SettingsSection::Archive => "archived deleted restore threads trash".into(),
    }
}

/// One control or group on a settings page, and the words a search may reach it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsControl {
    pub id: &'static str,
    pub section: SettingsSection,
    pub label: &'static str,
    pub keywords: String,
}

pub fn settings_controls() -> Vec<SettingsControl> {
    use SettingsSection::*;
    let control = |id, section, label, keywords: &str| SettingsControl { id, section, label, keywords: keywords.into() };
    vec![
        control("general.cli", General, crate::identity::CLI_COMMAND, "cli terminal command install shell path open folder"),
        control("general.dictation", General, "Speech model", "dictation voice speak talk microphone mic hold hands-free tap parakeet transcribe"),
        control("general.memory-protection", General, "Memory protection", "memory leak ram process stop automatic"),
        control("general.notifications", General, "Notifications", "alert banner notify sound away desktop phone push"),
        control("general.chrome-browser", General, "Claude in Chrome", "extension browser chrome experimental"),
        control("general.updates", General, "Updates", "check version release upgrade"),
        control("general.licenses", General, "Open source licenses", "legal notices third party copyright"),
        control("usage.plan-limits", Usage, "Plan limits", "quota tokens spend session weekly reset"),
        control("engines.installed", Engines, "Installed engines", &format!("{} cli version update sign in install check", engine_names())),
        control("engines.agent-settings", Engines, "Agent settings", "reload skills mcp hooks config apply"),
        control("appearance.theme", Appearance, "Colours", "theme colors palette dark light auto mode appearance"),
        control("appearance.ui-font", Appearance, "Interface", "font typeface family ui interface sans"),
        control("appearance.mono-font", Appearance, "Code and terminal", "font typeface family monospace code terminal shell diff"),
        control("appearance.reading-size", Appearance, "Conversation text", "text size font bigger smaller reading message"),
        control("appearance.terminal-size", Appearance, "Terminal text", "text size font bigger smaller terminal shell"),
        control("computer-use.availability", ComputerUse, "Computer use", "automation control apps see operate switch"),
        control("computer-use.accessibility", ComputerUse, "Accessibility", "permission macos click type navigate"),
        control("computer-use.screen-recording", ComputerUse, "Screen & System Audio Recording", "permission macos screenshot capture window"),
        control("browser.availability", Browser, "Browser use", "panel pages agent switch web"),
        control("browser.session", Browser, "Session", "cookies sign out clear data sites origins allowed"),
        control("browser.import", Browser, "Import", "sign ins logins cookies brave firefox edge arc vivaldi opera safari"),
        control("phone.availability", Phone, "Phone access", "mobile remote server pair device switch"),
        control("phone.tailscale", Phone, "Tailscale", "vpn network tailnet install https"),
        control("phone.pairing", Phone, "Pair a phone", "pairing code qr connect add mobile"),
        control("phone.phones", Phone, "Phones", "paired mobile revoke remove"),
        control("phone.computers", Phone, "Computers", "remote machine pair link host rename forget mac linux"),
    ]
}

pub fn shortcut_setting_id(action_id: &str) -> String {
    format!("shortcut.{action_id}")
}

/// One thing the jump panel can offer: a settings page, or a control on one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsJumpOption {
    pub id: String,
    pub section: SettingsSection,
    pub setting_id: Option<String>,
    pub title: String,
    pub page: Option<String>,
    pub keywords: String,
}

/// Every page, then every control, then every shortcut.
pub fn settings_jump_options() -> Vec<SettingsJumpOption> {
    let mut options: Vec<SettingsJumpOption> = SETTINGS_SECTIONS
        .iter()
        .map(|section| {
            let key = serde_json::to_value(section).ok().and_then(|value| value.as_str().map(String::from)).unwrap_or_default();
            SettingsJumpOption { id: format!("settings:{key}"), section: *section, setting_id: None, title: settings_page_label(*section).into(), page: None, keywords: page_keywords(*section) }
        })
        .collect();
    options.extend(settings_controls().into_iter().map(|control| SettingsJumpOption {
        id: format!("settings:{}", control.id),
        section: control.section,
        setting_id: Some(control.id.into()),
        title: control.label.into(),
        page: Some(settings_page_label(control.section).into()),
        keywords: control.keywords,
    }));
    options.extend(shortcut_actions().iter().map(|action| SettingsJumpOption {
        id: format!("settings:{}", shortcut_setting_id(action.id)),
        section: SettingsSection::Shortcuts,
        setting_id: Some(shortcut_setting_id(action.id)),
        title: action.label.into(),
        page: Some(settings_page_label(SettingsSection::Shortcuts).into()),
        keywords: format!("shortcut keyboard {} {}", action.group, action.description),
    }));
    options
}

pub const MATCH_RANKS: usize = 3;

/// The rank a name takes for a lower-cased query, or None when the query is not in it.
pub fn match_rank(name: &str, wanted: &str) -> Option<usize> {
    let lower = name.to_lowercase();
    if lower.starts_with(wanted) {
        return Some(0);
    }
    if starts_title_word(name, wanted) {
        return Some(1);
    }
    lower.contains(wanted).then_some(2)
}

/// Items any of whose names answer a query, best rank first, each rank in its own order.
pub fn ranked_matches<T: Clone>(items: &[T], query: &str, names: impl Fn(&T) -> Vec<String>) -> Vec<T> {
    let wanted = query.trim().to_lowercase();
    if wanted.is_empty() {
        return items.to_vec();
    }
    let mut ranks: Vec<Vec<T>> = vec![Vec::new(); MATCH_RANKS];
    for item in items {
        if let Some(best) = names(item).iter().filter_map(|name| match_rank(name, &wanted)).min() {
            ranks[best].push(item.clone());
        }
    }
    ranks.concat()
}

pub const SETTINGS_JUMP_ROWS: usize = 5;

/// The settings a query names; an empty query offers none.
pub fn rank_settings_jumps(query: &str, rows: usize) -> Vec<SettingsJumpOption> {
    let wanted = query.trim().to_lowercase();
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut ranked: Vec<Vec<SettingsJumpOption>> = vec![Vec::new(); MATCH_RANKS + 1];
    for option in settings_jump_options() {
        let rank = match_rank(&option.title, &wanted).or_else(|| {
            let words = format!("{} {}", option.title, option.keywords);
            wanted.split_whitespace().all(|word| starts_title_word(&words, word)).then_some(MATCH_RANKS)
        });
        if let Some(rank) = rank {
            ranked[rank].push(option);
        }
    }
    ranked.concat().into_iter().take(rows).collect()
}

/// A thread the panel offers.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadJumpOption {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    pub engine: AgentEngine,
    pub pull_request: Option<PullRequestLink>,
    pub ranked_at: i64,
}

pub const THREAD_JUMP_ROWS: usize = 12;

/// The threads a query names, best match first; a named pull request puts its threads first.
pub fn rank_thread_jumps(options: &[ThreadJumpOption], query: &str, rows: usize) -> Vec<ThreadJumpOption> {
    let wanted = query.trim().to_lowercase();
    if wanted.is_empty() {
        return options.iter().take(rows).cloned().collect();
    }
    let pull_request = pull_request_query(&wanted);
    let best = if pull_request.is_some() { 0 } else { 1 };
    let mut ranked: Vec<Vec<ThreadJumpOption>> = vec![Vec::new(); MATCH_RANKS + 1];
    for option in options {
        let linked = match (&pull_request, &option.pull_request) {
            (Some(query), Some(link)) => links_pull_request(link, query),
            _ => false,
        };
        if linked {
            ranked[0].push(option.clone());
        } else {
            let Some(rank) = match_rank(&option.title, &wanted) else { continue };
            ranked[rank + 1].push(option.clone());
        }
        if ranked[best].len() >= rows {
            break;
        }
    }
    ranked.concat().into_iter().take(rows).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(query: &str) -> Vec<Option<String>> {
        rank_settings_jumps(query, SETTINGS_JUMP_ROWS).into_iter().map(|option| option.setting_id).collect()
    }

    fn some(id: &str) -> Option<String> {
        Some(id.into())
    }

    #[test]
    fn settings_answer_to_every_word_of_a_query() {
        assert_eq!(ids("text size"), vec![None, some("appearance.reading-size"), some("appearance.terminal-size")]);
        assert!(ids("text purple").is_empty());
        assert!(ids("   ").is_empty());
    }

    #[test]
    fn a_page_beats_a_control_and_a_name_beats_a_keyword() {
        let titles: Vec<String> = rank_settings_jumps("browser", SETTINGS_JUMP_ROWS).into_iter().map(|option| option.title).collect();
        assert_eq!(titles, ["Browser", "Browser use", "Claude in Chrome"]);
        assert_eq!(ids("font"), vec![None, some("appearance.ui-font"), some("appearance.mono-font"), some("appearance.reading-size"), some("appearance.terminal-size")]);
        assert_eq!(ids("cli")[..2], [None, some("general.cli")]);
        assert_eq!(ids("tailscale"), vec![some("phone.tailscale"), None]);
        assert_eq!(ids("installed engines")[0], some("engines.installed"));
        for query in ["platform setup", "linux runtime"] {
            let found = rank_settings_jumps(query, SETTINGS_JUMP_ROWS);
            assert_eq!(found.iter().map(|option| (option.section, option.setting_id.clone())).collect::<Vec<_>>(), vec![(SettingsSection::ComputerUse, None)], "{query}");
        }
        let deny = &rank_settings_jumps("deny", SETTINGS_JUMP_ROWS)[0];
        assert_eq!((deny.section, deny.setting_id.as_deref(), deny.page.as_deref()), (SettingsSection::Shortcuts, Some("shortcut.run.deny"), Some("Shortcuts")));
        assert_eq!(rank_settings_jumps("e", 3).len(), 3);
        assert_eq!(settings_jump_options().len(), SETTINGS_SECTIONS.len() + settings_controls().len() + shortcut_actions().len());
    }

    fn options(titles: &[&str]) -> Vec<ThreadJumpOption> {
        titles.iter().enumerate().map(|(index, title)| ThreadJumpOption { id: format!("task-{index}"), title: (*title).into(), project: None, engine: AgentEngine::Claude, pull_request: None, ranked_at: (titles.len() - index) as i64 }).collect()
    }

    fn titles(options: Vec<ThreadJumpOption>) -> Vec<String> {
        options.into_iter().map(|option| option.title).collect()
    }

    #[test]
    fn threads_rank_by_where_the_name_lands_and_stay_newest_first() {
        let threads = options(&["Dock the browser panel", "Panel find", "Rework the panel tabs", "Ship the dock"]);
        assert_eq!(titles(rank_thread_jumps(&threads, "  ", 2)), ["Dock the browser panel", "Panel find"]);
        assert_eq!(titles(rank_thread_jumps(&threads, "panel", THREAD_JUMP_ROWS)), ["Panel find", "Dock the browser panel", "Rework the panel tabs"]);
        assert_eq!(titles(rank_thread_jumps(&threads, "dock", THREAD_JUMP_ROWS)), ["Dock the browser panel", "Ship the dock"]);
        assert_eq!(titles(rank_thread_jumps(&threads, "SHIP", THREAD_JUMP_ROWS)), ["Ship the dock"]);
        assert!(rank_thread_jumps(&threads, "worktree", THREAD_JUMP_ROWS).is_empty());
        let mut names: Vec<String> = (0..500).map(|index| format!("Panel {index}")).collect();
        names.push("Reopen the panel".into());
        let many = options(&names.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(titles(rank_thread_jumps(&many, "panel", 3)), ["Panel 0", "Panel 1", "Panel 2"]);
    }

    #[test]
    fn a_pull_request_finds_the_threads_working_on_it_above_any_name() {
        let mut linked = options(&["Ship 42 fixes", "Dock the browser panel", "Panel find"]);
        let link = |url: &str| serde_json::from_value::<PullRequestLink>(serde_json::json!({ "number": 42, "url": url })).ok();
        linked[1].pull_request = link("https://github.com/acme/app/pull/42");
        linked[2].pull_request = link("https://github.com/acme/other/pull/42");
        assert_eq!(titles(rank_thread_jumps(&linked, "42", THREAD_JUMP_ROWS)), ["Dock the browser panel", "Panel find", "Ship 42 fixes"]);
        assert_eq!(titles(rank_thread_jumps(&linked, "#42", THREAD_JUMP_ROWS)), ["Dock the browser panel", "Panel find"]);
        assert_eq!(titles(rank_thread_jumps(&linked, " https://github.com/Acme/App/pull/42/files ", THREAD_JUMP_ROWS)), ["Dock the browser panel"]);
        assert_eq!(titles(rank_thread_jumps(&linked, "4", THREAD_JUMP_ROWS)), ["Ship 42 fixes"]);
    }
}
