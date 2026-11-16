//! The browser panel, driven as the thread that is running: writes are commands, reads are the page.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

use super::definition::{bind_tools, names_of, report, BoundTool, ToolDefinition};
use super::schema::{boolean, enumeration, integer, number, string, Args, Field, Input};
use super::server::{Grants, Offers, ToolRun, ToolServer};
use super::types::*;

pub const BROWSER_SERVER_NAME: &str = "aicodingtool-browser";

pub trait BrowserBridge: Send + Sync {
    fn command(&self, write: BrowserWrite) -> BoxFuture<'_, Result<(), String>>;
    fn read(&self, read: BrowserRead) -> BoxFuture<'_, Result<BrowserReadResult, String>>;
}

const DEFAULT_WAIT_MS: u64 = 20_000;
const DEFAULT_TEXT_LIMIT: f64 = 4_000.0;
const BROWSER_ERROR: &str = "Browser error: ";

fn tab_field() -> Field {
    string().max(MAX_ID_LENGTH).optional().describe("Which tab to act in. Defaults to the one the panel is showing.")
}

fn ref_field() -> Field {
    string().min(1).max(MAX_ID_LENGTH).describe("The ref of the element, from the latest snapshot of that tab.")
}

fn wait_ms(seconds: Option<f64>) -> u64 {
    seconds.map_or(DEFAULT_WAIT_MS, |seconds| (seconds.max(0.0) * 1_000.0) as u64).min(MAX_BROWSER_WAIT_MS)
}

/// The URL a search opens, in the engine the panel searches with.
pub fn browser_search_url(query: &str) -> String {
    format!("https://duckduckgo.com/?q={}", encode_uri_component(query.trim()))
}

/// `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` is percent-encoded as UTF-8.
pub fn encode_uri_component(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_default()
}

fn or_untitled(title: &str) -> &str {
    if title.is_empty() { "Untitled" } else { title }
}

fn snapshot_text(snapshot: &BrowserSnapshot) -> String {
    let elements: Vec<String> = snapshot
        .elements
        .iter()
        .map(|element| {
            let value = element.value.as_deref().map(|value| format!(" = {}", quoted(value))).unwrap_or_default();
            format!("[{}] {} {}{value}", element.reference, element.role, quoted(&element.name))
        })
        .collect();
    [
        format!("{} — {}{}", or_untitled(&snapshot.title), snapshot.url, if snapshot.loading { " (still loading)" } else { "" }),
        String::new(),
        if snapshot.text.is_empty() { "(the page has no text)".into() } else { snapshot.text.clone() },
        String::new(),
        if elements.is_empty() { "Nothing on this page can be clicked or typed into.".into() } else { format!("Elements you can act on:\n{}", elements.join("\n")) },
    ]
    .join("\n")
}

/// How a tab reads in a list.
pub fn describe_tab(tab: &BrowserTab) -> String {
    let name = if !tab.title.is_empty() { tab.title.as_str() } else if !tab.url.is_empty() { tab.url.as_str() } else { "Blank tab" };
    let mut parts = vec![format!("{name} [{}]", tab.id), tab.url.clone()];
    if tab.loading {
        parts.push("loading".into());
    }
    parts.extend(tab.error.clone());
    parts.into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

fn wait_text(condition: &str, value: Option<&str>, matched: bool, elapsed_ms: u64, url: &str) -> String {
    let target = value.map(|value| format!("{condition} {}", quoted(value))).unwrap_or_else(|| condition.to_string());
    if matched {
        format!("Finished waiting for {target} after {elapsed_ms}ms.")
    } else {
        format!("Timed out waiting for {target} after {elapsed_ms}ms. The page is {url}.")
    }
}

/// Every read answers the same way, so a page nobody has allowed yet reads as the ask it is.
pub fn read_text(result: &BrowserReadResult) -> String {
    match result {
        BrowserReadResult::Snapshot { snapshot } => snapshot_text(snapshot),
        BrowserReadResult::Shot { shot } => format!("{} — {}\nPicture saved to {} ({}x{}). Open that file to see the page.", or_untitled(&shot.title), shot.url, shot.path, shot.width, shot.height),
        BrowserReadResult::Console { url, title, entries, latest_sequence, omitted, .. } => {
            let lines: Vec<String> = entries
                .iter()
                .map(|entry| {
                    let source = entry.source.as_ref().filter(|source| !source.is_empty()).map(|source| format!(" ({source}{})", entry.line.filter(|line| *line != 0).map(|line| format!(":{line}")).unwrap_or_default())).unwrap_or_default();
                    format!("[{}] {} {}{source}", entry.sequence, entry.level.to_uppercase(), entry.message)
                })
                .collect();
            let mut text = vec![format!("{} — {url}", or_untitled(title)), format!("Console cursor: {latest_sequence}")];
            if *omitted > 0 {
                text.push(format!("{omitted} older matching entries omitted."));
            }
            text.push(if lines.is_empty() { "No matching console messages.".into() } else { lines.join("\n") });
            text.join("\n")
        }
        BrowserReadResult::Network { url, title, entries, latest_sequence, omitted, .. } => {
            let lines: Vec<String> = entries
                .iter()
                .map(|entry| {
                    let result = entry.error.clone().or_else(|| entry.status.map(|status| status.to_string())).unwrap_or_else(|| "unknown".into());
                    let cached = if entry.from_cache == Some(true) { " cached" } else { "" };
                    format!("[{}] {} {result} {}ms {}{cached} {}", entry.sequence, entry.method, entry.duration_ms, entry.resource_type, entry.url)
                })
                .collect();
            let mut text = vec![format!("{} — {url}", or_untitled(title)), format!("Network cursor: {latest_sequence}")];
            if *omitted > 0 {
                text.push(format!("{omitted} older matching entries omitted."));
            }
            text.push(if lines.is_empty() { "No matching completed requests.".into() } else { lines.join("\n") });
            text.join("\n")
        }
        BrowserReadResult::Wait { condition, value, matched, elapsed_ms, url, .. } => wait_text(condition, value.as_deref(), *matched, *elapsed_ms, url),
        BrowserReadResult::Tabs { tabs } => {
            if tabs.is_empty() {
                "The browser panel has no tab open.".into()
            } else {
                tabs.iter().map(describe_tab).collect::<Vec<_>>().join("\n")
            }
        }
        BrowserReadResult::AwaitingApproval { url } => format!("AICodingTool is asking the user to allow {url}. Nothing loads until they answer, so tell them what you need it for and wait, or ask them to open it themselves."),
        BrowserReadResult::NoTab => "The browser panel has no tab open.".into(),
    }
}

/// Reading after a write is what the caller wants: the page as it stands once the action settled.
async fn settled_page(bridge: &dyn BrowserBridge, tab_id: Option<String>, seconds: Option<f64>) -> Result<String, String> {
    let read = BrowserRead::Snapshot { tab_id: tab_id.filter(|tab| !tab.is_empty()), text_limit: Some(DEFAULT_TEXT_LIMIT), timeout_ms: wait_ms(seconds) };
    Ok(read_text(&bridge.read(read).await?))
}

fn tab_of(args: &serde_json::Map<String, Value>) -> Option<String> {
    args.text("tabId").filter(|tab| !tab.is_empty())
}

fn with_tab(mut command: Value, tab: Option<String>) -> Value {
    if let Some(tab) = tab {
        command["tabId"] = json!(tab);
    }
    command
}

fn search_fits(query: &str) -> bool {
    browser_search_url(query).len() <= MAX_URL_LENGTH
}

fn open_run(bridge: Arc<dyn BrowserBridge>, url: String, args: serde_json::Map<String, Value>) -> BoxFuture<'static, super::definition::ToolResult> {
    async move {
        report(BROWSER_ERROR, async {
            let new_tab = args.flag("newTab") == Some(true);
            let mut command = json!({ "type": "browser.open", "url": url });
            if new_tab {
                command["newTab"] = json!(true);
            }
            bridge.command(with_tab(command, tab_of(&args))).await?;
            settled_page(bridge.as_ref(), if new_tab { None } else { tab_of(&args) }, args.float("waitSeconds")).await
        })
        .await
    }
    .boxed()
}

pub fn browser_tools() -> Vec<ToolDefinition<dyn BrowserBridge>> {
    vec![
        ToolDefinition {
            name: "browser_open",
            description: "Open a page in the AICodingTool browser panel and read it back. The panel shares one browser session with the user, so any site they are signed into is already signed in here, and they watch the same tabs you drive: leave the pages they opened alone. A page the user has never visited is theirs to allow first, so say what you need and wait rather than retrying. To search, use browser_search.".into(),
            input: Input::new(vec![
                ("url", string().min(1).max(MAX_URL_LENGTH).describe("The page to open. Include the scheme for anything that is not an ordinary domain.")),
                ("newTab", boolean().optional().describe("Open another tab instead of reusing the one on screen.")),
                ("tabId", tab_field()),
                ("waitSeconds", number().optional().describe("How long to wait for the page to finish loading. Defaults to 20.")),
            ]),
            read_only: false,
            run: |bridge, args| {
                let url = args.text("url").unwrap_or_default();
                open_run(bridge, url, args)
            },
        },
        ToolDefinition {
            name: "browser_search",
            description: "Search the web in the AICodingTool browser panel and read the results. Use this rather than opening a search engine yourself: Google sometimes answers the panel with its bot page instead of results.".into(),
            input: Input::new(vec![
                ("query", string().refine(search_fits, "Too long to search for.").describe("What to search for.")),
                ("newTab", boolean().optional().describe("Search in another tab instead of the one on screen.")),
                ("tabId", tab_field()),
                ("waitSeconds", number().optional().describe("How long to wait for the results to load. Defaults to 20.")),
            ]),
            read_only: false,
            run: |bridge, args| {
                let url = browser_search_url(&args.text("query").unwrap_or_default());
                open_run(bridge, url, args)
            },
        },
        ToolDefinition {
            name: "browser_read",
            description: "Read the page a tab is showing: its text, and the elements you can act on. Act on the refs this hands back; they are only valid until the next read of that tab, so read again after anything changes the page.".into(),
            input: Input::new(vec![
                ("tabId", tab_field()),
                ("textLimit", number().nonnegative().optional().describe("How many characters of page text to return. Defaults to 4000.")),
                ("waitSeconds", number().optional().describe("How long to wait for the page to settle first. Defaults to 20.")),
            ]),
            read_only: true,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let read = BrowserRead::Snapshot { tab_id: tab_of(&args), text_limit: Some(args.float("textLimit").unwrap_or(DEFAULT_TEXT_LIMIT)), timeout_ms: wait_ms(args.float("waitSeconds")) };
                        Ok(read_text(&bridge.read(read).await?))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_screenshot",
            description: "Take a picture of a tab and read back where it was saved. Use it to check how a page looks, which the page text cannot tell you. The picture is a PNG file on this machine, so open that file to see it. The tab does not have to be the one on screen: a page the panel is not showing is captured just the same, and the user sees nothing move.".into(),
            input: Input::new(vec![
                ("tabId", tab_field()),
                ("fullPage", boolean().optional().describe("Capture the whole page instead of only the part that fits the window.")),
                ("waitSeconds", number().optional().describe("How long to wait for the page to settle first. Defaults to 20.")),
            ]),
            read_only: true,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let read = BrowserRead::Screenshot { tab_id: tab_of(&args), full_page: (args.flag("fullPage") == Some(true)).then_some(true), timeout_ms: wait_ms(args.float("waitSeconds")) };
                        Ok(read_text(&bridge.read(read).await?))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_console",
            description: "Read recent console messages from a tab, including runtime errors and rejected promises Chromium reports there. Each answer includes a cursor; pass it as since next time to read only newer messages.".into(),
            input: Input::new(vec![
                ("tabId", tab_field()),
                ("since", integer().nonnegative().optional().describe("Return messages after this console cursor.")),
                ("limit", integer().min(1).max(200).optional().describe("Maximum messages to return. Defaults to 50.")),
                ("minimumLevel", enumeration(&["debug", "info", "warning", "error"]).optional().describe("Lowest severity to return. Defaults to debug.")),
            ]),
            read_only: true,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let read = BrowserRead::Console { tab_id: tab_of(&args), since: args.float("since").map(|since| since as u64), limit: args.float("limit").map(|limit| limit as u64), minimum_level: args.text("minimumLevel") };
                        Ok(read_text(&bridge.read(read).await?))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_network",
            description: "Read recent completed requests from a tab with method, status or error, resource type, cache use, and duration. Each answer includes a cursor; pass it as since next time to read only newer requests.".into(),
            input: Input::new(vec![
                ("tabId", tab_field()),
                ("since", integer().nonnegative().optional().describe("Return requests after this network cursor.")),
                ("limit", integer().min(1).max(200).optional().describe("Maximum requests to return. Defaults to 50.")),
                ("failuresOnly", boolean().optional().describe("Return only errors and responses with status 400 or higher.")),
            ]),
            read_only: true,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let read = BrowserRead::Network {
                            tab_id: tab_of(&args),
                            since: args.float("since").map(|since| since as u64),
                            limit: args.float("limit").map(|limit| limit as u64),
                            failures_only: (args.flag("failuresOnly") == Some(true)).then_some(true),
                        };
                        Ok(read_text(&bridge.read(read).await?))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_wait",
            description: "Wait until page text, an element name, or the URL contains a value; until text or an element disappears; or until network activity has stopped for 500ms. Reads the page after a successful wait.".into(),
            input: Input::new(vec![
                ("condition", enumeration(&["text", "text-gone", "element", "element-gone", "url", "network-idle"])),
                ("value", string().max(MAX_SEARCH_TEXT).optional().describe("Text to find. Omit only for network-idle.")),
                ("tabId", tab_field()),
                ("timeoutSeconds", number().optional().describe("How long to wait. Defaults to 20.")),
            ]),
            read_only: true,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let condition = args.text("condition").unwrap_or_default();
                        let value = args.text("value").map(|value| value.trim().to_string());
                        if condition == "network-idle" && value.is_some() {
                            return Err("network-idle takes no value.".to_string());
                        }
                        if condition != "network-idle" && value.as_deref().is_none_or(str::is_empty) {
                            return Err(format!("{condition} needs a value."));
                        }
                        let waited = bridge.read(BrowserRead::Wait { condition, value, tab_id: tab_of(&args), timeout_ms: wait_ms(args.float("timeoutSeconds")) }).await?;
                        match &waited {
                            BrowserReadResult::Wait { matched: true, .. } => Ok(format!("{}\n\n{}", read_text(&waited), settled_page(bridge.as_ref(), tab_of(&args), Some(0.0)).await?)),
                            _ => Ok(read_text(&waited)),
                        }
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_click",
            description: "Click an element in the page and read back what it became.".into(),
            input: Input::new(vec![("ref", ref_field()), ("tabId", tab_field()), ("waitSeconds", number().optional().describe("How long to wait for the click's page load. Defaults to 20."))]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let command = json!({ "type": "browser.act", "action": { "kind": "click", "ref": args.text("ref") } });
                        bridge.command(with_tab(command, tab_of(&args))).await?;
                        settled_page(bridge.as_ref(), tab_of(&args), args.float("waitSeconds")).await
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_type",
            description: "Type into a field in the page, optionally submitting it, then read back what it became.".into(),
            input: Input::new(vec![
                ("ref", ref_field()),
                ("text", string().max(MAX_PROMPT_LENGTH).describe("What the field should contain. This replaces whatever it holds.")),
                ("submit", boolean().optional().describe("Submit the field's form afterwards, the way pressing Enter would.")),
                ("tabId", tab_field()),
                ("waitSeconds", number().optional().describe("How long to wait for a page load the submit causes. Defaults to 20.")),
            ]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let mut action = json!({ "kind": "type", "ref": args.text("ref"), "text": args.text("text") });
                        if args.flag("submit") == Some(true) {
                            action["submit"] = json!(true);
                        }
                        bridge.command(with_tab(json!({ "type": "browser.act", "action": action }), tab_of(&args))).await?;
                        settled_page(bridge.as_ref(), tab_of(&args), args.float("waitSeconds")).await
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_back",
            description: "Go back, or forward, in a tab's own history and read the page it lands on.".into(),
            input: Input::new(vec![("forward", boolean().optional().describe("Go forward instead of back.")), ("tabId", tab_field())]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        let delta = if args.flag("forward") == Some(true) { 1 } else { -1 };
                        bridge.command(with_tab(json!({ "type": "browser.go", "delta": delta }), tab_of(&args))).await?;
                        settled_page(bridge.as_ref(), tab_of(&args), None).await
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "browser_tabs",
            description: "List the pages the browser panel has open. Use it to find the tab id of a page the user is looking at.".into(),
            input: Input::empty(),
            read_only: true,
            run: |bridge, _| async move { report(BROWSER_ERROR, async { Ok(read_text(&bridge.read(BrowserRead::Tabs).await?)) }).await }.boxed(),
        },
        ToolDefinition {
            name: "browser_close_tab",
            description: "Close a tab in the browser panel. Only close tabs you opened; the user's own pages are theirs.".into(),
            input: Input::new(vec![("tabId", string().min(1).max(MAX_ID_LENGTH).describe("The tab to close, as browser_tabs reports it."))]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(BROWSER_ERROR, async {
                        bridge.command(json!({ "type": "browser.close-tab", "tabId": args.text("tabId") })).await?;
                        Ok(read_text(&bridge.read(BrowserRead::Tabs).await?))
                    })
                    .await
                }
                .boxed()
            },
        },
    ]
}

/// `read_only` keeps only the tools that change nothing the user can see.
pub fn bound_browser_tools(bridge: Arc<dyn BrowserBridge>, read_only: bool) -> Vec<BoundTool> {
    let mut definitions = browser_tools();
    if read_only {
        definitions.retain(|tool| tool.read_only);
    }
    bind_tools(bridge, &definitions)
}

const BROWSER_INSTRUCTIONS: &str = "The AICodingTool browser panel is a real browser sharing one session with the user, so every site they have signed into is signed in for you: use the browser panel tools, such as browser_open, browser_read and browser_screenshot, rather than curl or Bash for anything behind a login, and rather than guessing at a page you can read.";
const BROWSER_READING_INSTRUCTIONS: &str = "The AICodingTool browser panel is a real browser sharing one session with the user: read the pages they have open there with browser_tabs, browser_read and browser_screenshot rather than guessing at them. Opening, clicking and typing in it are work for a thread.";

pub fn server() -> ToolServer {
    ToolServer {
        name: BROWSER_SERVER_NAME,
        definitions: names_of(&browser_tools()),
        // Reading a page the panel already holds changes nothing; opening one and acting in it does.
        grants: Grants::ReadOnly,
        // A coordinator changes nothing itself, so it reads the browser panel but never drives it.
        serve: |run: &ToolRun| Some(bound_browser_tools(run.browser.clone()?, run.coordination_role == Some(CoordinationRole::Coordinator))),
        brief: |offers: &Offers, _, _| {
            if !offers.any() {
                return Vec::new();
            }
            vec![if offers.has("browser_open") { BROWSER_INSTRUCTIONS } else { BROWSER_READING_INSTRUCTIONS }.to_string()]
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_urls_encode_like_encode_uri_component() {
        assert_eq!(browser_search_url("  rust & gpui's (docs) é "), "https://duckduckgo.com/?q=rust%20%26%20gpui's%20(docs)%20%C3%A9");
    }

    #[test]
    fn a_snapshot_lists_what_can_be_acted_on() {
        let snapshot = BrowserSnapshot {
            tab_id: "t".into(),
            url: "https://x".into(),
            title: String::new(),
            loading: true,
            text: String::new(),
            elements: vec![BrowserElement { reference: "e1".into(), role: "textbox".into(), name: "Search \"q\"".into(), value: Some("hi".into()) }],
        };
        assert_eq!(
            read_text(&BrowserReadResult::Snapshot { snapshot }),
            "Untitled — https://x (still loading)\n\n(the page has no text)\n\nElements you can act on:\n[e1] textbox \"Search \\\"q\\\"\" = \"hi\""
        );
    }
}
