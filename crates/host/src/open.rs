//! Handing things to the desktop: a source file to an editor, a checkout to another application, a
//! path to the file manager, a page to the browser.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use act_platform::Platform;
use act_platform::process::launch_detached;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub command: String,
    pub args: Vec<String>,
}

type EditorArgs = fn(&str, Option<u32>) -> Vec<String>;

struct Editor {
    locations: fn(Platform) -> &'static [&'static str],
    args: EditorArgs,
}

/// `--goto` takes the line as part of the path. Without one the plain path opens the file.
fn goto_args(file: &str, line: Option<u32>) -> Vec<String> {
    match line {
        Some(line) => vec!["-g".into(), format!("{file}:{line}")],
        None => vec![file.into()],
    }
}

fn suffix_args(file: &str, line: Option<u32>) -> Vec<String> {
    vec![line.map(|line| format!("{file}:{line}")).unwrap_or_else(|| file.into())]
}

fn jetbrains_args(file: &str, line: Option<u32>) -> Vec<String> {
    match line {
        Some(line) => vec!["--line".into(), line.to_string(), file.into()],
        None => vec![file.into()],
    }
}

/// Tried in order, so this doubles as which editor wins when someone has several installed.
const EDITORS: [Editor; 6] = [
    Editor {
        locations: |platform| match platform {
            Platform::Darwin => &["/Applications/Cursor.app/Contents/Resources/app/bin/cursor", "cursor"],
            Platform::Windows => &["cursor.cmd"],
            Platform::Linux => &["cursor"],
        },
        args: goto_args,
    },
    Editor {
        locations: |platform| match platform {
            Platform::Darwin => &["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code", "code"],
            Platform::Windows => &["code.cmd"],
            Platform::Linux => &["code"],
        },
        args: goto_args,
    },
    Editor {
        locations: |platform| match platform {
            Platform::Darwin => &["/Applications/Windsurf.app/Contents/Resources/app/bin/windsurf", "windsurf"],
            Platform::Windows => &["windsurf.cmd"],
            Platform::Linux => &["windsurf"],
        },
        args: goto_args,
    },
    Editor {
        locations: |platform| match platform {
            Platform::Darwin => &["/Applications/Zed.app/Contents/MacOS/cli", "zed"],
            Platform::Windows => &[],
            Platform::Linux => &["zed"],
        },
        args: suffix_args,
    },
    Editor {
        locations: |platform| match platform {
            Platform::Darwin => &["/Applications/Sublime Text.app/Contents/SharedSupport/bin/subl", "subl"],
            Platform::Windows => &["subl.exe"],
            Platform::Linux => &["subl"],
        },
        args: suffix_args,
    },
    Editor {
        locations: |platform| match platform {
            Platform::Darwin | Platform::Linux => &["idea", "webstorm"],
            Platform::Windows => &["idea.bat", "webstorm.bat"],
        },
        args: jetbrains_args,
    },
];

/// Every launcher worth trying on this platform, best first, flattened across editors.
pub fn editor_candidates(platform: Platform, file: &str, line: Option<u32>) -> Vec<Launch> {
    EDITORS.iter().flat_map(|editor| (editor.locations)(platform).iter().map(move |command| Launch { command: command.to_string(), args: (editor.args)(file, line) })).collect()
}

/// The launcher a resolved editor uses next time, so a second click skips the ones that missed.
pub fn editor_launch(platform: Platform, command: &str, file: &str, line: Option<u32>) -> Option<Launch> {
    EDITORS.iter().find(|editor| (editor.locations)(platform).contains(&command)).map(|editor| Launch { command: command.into(), args: (editor.args)(file, line) })
}

/// Opens the file as text rather than by its extension, which macOS reads as video for `.ts`.
pub fn text_handler_launch(platform: Platform, file: &str) -> Option<Launch> {
    match platform {
        Platform::Darwin => Some(Launch { command: "open".into(), args: vec!["-t".into(), file.into()] }),
        Platform::Windows => Some(Launch { command: "notepad.exe".into(), args: vec![file.into()] }),
        Platform::Linux => None,
    }
}

/// A web page opens in the browser; a link to one of its lines is about the source, so stays in the editor.
pub fn opens_in_browser(file: &str, line: Option<u32>) -> bool {
    let lower = file.to_lowercase();
    line.is_none() && [".htm", ".html", ".xhtm", ".xhtml"].iter().any(|extension| lower.ends_with(extension))
}

fn file_url(file: &Path) -> String {
    let mut url = String::from("file://");
    for byte in file.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

use act_platform::tools::open as system_open;

/// Opens a web page outside the app.
pub fn open_url(url: &str) -> bool {
    system_open(url)
}

/// Shows a path in the platform's file manager.
pub fn reveal_in_file_manager(path: &Path) -> bool {
    act_platform::tools::reveal(path)
}

/// The launcher that answered last time, so later clicks skip the ones that already missed.
fn resolved_editor() -> &'static Mutex<Option<String>> {
    static RESOLVED: Mutex<Option<String>> = Mutex::new(None);
    &RESOLVED
}

/// Opens a source file for reading, at its line where the editor it finds takes one.
pub fn open_in_editor(file: &Path, line: Option<u32>) {
    let platform = Platform::current();
    let shown = file.to_string_lossy().into_owned();
    if opens_in_browser(&shown, line) && system_open(&file_url(file)) {
        return;
    }
    let known = resolved_editor().lock().unwrap().clone();
    if let Some(command) = known {
        if let Some(launch) = editor_launch(platform, &command, &shown, line) {
            if launch_detached(&launch.command, &launch.args) {
                return;
            }
        }
        *resolved_editor().lock().unwrap() = None;
    }
    for candidate in editor_candidates(platform, &shown, line) {
        if launch_detached(&candidate.command, &candidate.args) {
            *resolved_editor().lock().unwrap() = Some(candidate.command);
            return;
        }
    }
    if let Some(handler) = text_handler_launch(platform, &shown) {
        if launch_detached(&handler.command, &handler.args) {
            return;
        }
    }
    if !system_open(&shown) {
        reveal_in_file_manager(file);
    }
}

/// What an application is for, which is how the list groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExternalAppKind {
    Editor,
    Terminal,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalApp {
    pub id: String,
    pub label: String,
    pub kind: ExternalAppKind,
}

/// An installed application, with its own icon as PNG bytes where this machine keeps one readable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub app: ExternalApp,
    pub icon: Option<Vec<u8>>,
}

/// One place an application might be, and the command that opens a folder there. A candidate with a
/// `probe` exists only if that path does; one without is looked up on PATH instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppCandidate {
    pub id: String,
    pub probe: Option<String>,
    /// The bundle the icon is read from. Only macOS keeps one where this can find it.
    pub icon: Option<String>,
    pub launch: Launch,
}

enum Location {
    /// A macOS bundle, searched for in every standard folder. `launch` replaces the plain folder-open.
    Bundle { bundle: &'static str, launch: Option<fn(&str, &str) -> Launch> },
    /// A launcher on PATH by a bare name, or at an absolute path.
    Command { command: &'static str, args: Option<fn(&str) -> Vec<String>>, icon: Option<&'static str> },
}

struct CataloguedApp {
    id: &'static str,
    label: &'static str,
    kind: ExternalAppKind,
    locations: fn(Platform) -> Vec<Location>,
}

const fn bundle(bundle: &'static str) -> Location {
    Location::Bundle { bundle, launch: None }
}

const fn command(command: &'static str) -> Location {
    Location::Command { command, args: None, icon: None }
}

const fn command_with(command: &'static str, args: fn(&str) -> Vec<String>) -> Location {
    Location::Command { command, args: Some(args), icon: None }
}

macro_rules! locations {
    ($($platform:ident => [$($location:expr),* $(,)?]),* $(,)?) => {
        |platform| {
            $(if platform == Platform::$platform {
                return vec![$($location),*];
            })*
            Vec::new()
        }
    };
}

/// Editors first, because reading a checkout is the common reason to leave the app, and the file
/// manager last because it always answers.
const APPS: &[CataloguedApp] = &[
    CataloguedApp { id: "cursor", label: "Cursor", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Cursor.app")], Windows => [command("cursor.cmd")], Linux => [command("cursor")]) },
    CataloguedApp { id: "vscode", label: "Visual Studio Code", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Visual Studio Code.app")], Windows => [command("code.cmd")], Linux => [command("code")]) },
    CataloguedApp { id: "vscode-insiders", label: "Visual Studio Code Insiders", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Visual Studio Code - Insiders.app")], Windows => [command("code-insiders.cmd")], Linux => [command("code-insiders")]) },
    CataloguedApp { id: "vscodium", label: "VSCodium", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("VSCodium.app")], Windows => [command("codium.cmd")], Linux => [command("codium")]) },
    CataloguedApp { id: "windsurf", label: "Windsurf", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Windsurf.app")], Windows => [command("windsurf.cmd")], Linux => [command("windsurf")]) },
    CataloguedApp { id: "zed", label: "Zed", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Zed.app")], Linux => [command("zed")]) },
    CataloguedApp { id: "sublime", label: "Sublime Text", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Sublime Text.app")], Windows => [command("subl.exe")], Linux => [command("subl")]) },
    CataloguedApp { id: "intellij", label: "IntelliJ IDEA", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("IntelliJ IDEA.app")], Windows => [command("idea.bat")], Linux => [command("idea")]) },
    CataloguedApp { id: "webstorm", label: "WebStorm", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("WebStorm.app")], Windows => [command("webstorm.bat")], Linux => [command("webstorm")]) },
    CataloguedApp { id: "pycharm", label: "PyCharm", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("PyCharm.app"), bundle("PyCharm CE.app")], Windows => [command("pycharm64.exe")], Linux => [command("pycharm"), command("pycharm.sh")]) },
    CataloguedApp { id: "goland", label: "GoLand", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("GoLand.app")], Windows => [command("goland64.exe")], Linux => [command("goland"), command("goland.sh")]) },
    CataloguedApp { id: "clion", label: "CLion", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("CLion.app")], Windows => [command("clion64.exe")], Linux => [command("clion"), command("clion.sh")]) },
    CataloguedApp { id: "rider", label: "Rider", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Rider.app")], Windows => [command("rider64.exe")], Linux => [command("rider"), command("rider.sh")]) },
    CataloguedApp { id: "phpstorm", label: "PhpStorm", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("PhpStorm.app")], Windows => [command("phpstorm64.exe")], Linux => [command("phpstorm"), command("phpstorm.sh")]) },
    CataloguedApp { id: "rubymine", label: "RubyMine", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("RubyMine.app")], Windows => [command("rubymine64.exe")], Linux => [command("rubymine"), command("rubymine.sh")]) },
    CataloguedApp { id: "rustrover", label: "RustRover", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("RustRover.app")], Windows => [command("rustrover64.exe")], Linux => [command("rustrover"), command("rustrover.sh")]) },
    CataloguedApp {
        id: "xcode",
        label: "Xcode",
        kind: ExternalAppKind::Editor,
        locations: locations!(Darwin => [Location::Bundle { bundle: "Xcode.app", launch: Some(|bundle, folder| Launch { command: format!("{bundle}/Contents/Developer/usr/bin/xed"), args: vec!["--project".into(), folder.into()] }) }]),
    },
    CataloguedApp { id: "android-studio", label: "Android Studio", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("Android Studio.app")], Windows => [command("studio64.exe")], Linux => [command("studio"), command("studio.sh"), command("android-studio")]) },
    CataloguedApp { id: "github-desktop", label: "GitHub Desktop", kind: ExternalAppKind::Editor, locations: locations!(Darwin => [bundle("GitHub Desktop.app")], Windows => [command("github.exe")], Linux => [command("github-desktop")]) },
    CataloguedApp { id: "terminal", label: "Terminal", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("Terminal.app")]) },
    CataloguedApp { id: "iterm", label: "iTerm", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("iTerm.app")]) },
    CataloguedApp { id: "warp", label: "Warp", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("Warp.app")], Linux => [command("warp-terminal")]) },
    CataloguedApp { id: "ghostty", label: "Ghostty", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("Ghostty.app")], Linux => [command_with("ghostty", |folder| vec![format!("--working-directory={folder}")])]) },
    CataloguedApp { id: "cmux", label: "cmux", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("cmux.app")]) },
    CataloguedApp { id: "kitty", label: "kitty", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("kitty.app")], Linux => [command_with("kitty", |folder| vec!["--directory".into(), folder.into()])]) },
    CataloguedApp { id: "wezterm", label: "WezTerm", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("WezTerm.app")], Linux => [command_with("wezterm", |folder| vec!["start".into(), "--cwd".into(), folder.into()])]) },
    CataloguedApp { id: "alacritty", label: "Alacritty", kind: ExternalAppKind::Terminal, locations: locations!(Darwin => [bundle("Alacritty.app")], Linux => [command_with("alacritty", |folder| vec!["--working-directory".into(), folder.into()])]) },
    CataloguedApp {
        id: "tabby",
        label: "Tabby",
        kind: ExternalAppKind::Terminal,
        locations: locations!(
            Darwin => [Location::Bundle { bundle: "Tabby.app", launch: Some(|bundle, folder| Launch { command: format!("{bundle}/Contents/MacOS/Tabby"), args: vec!["open".into(), folder.into()] }) }],
            Windows => [command_with("Tabby.exe", |folder| vec!["open".into(), folder.into()])],
            Linux => [command_with("tabby", |folder| vec!["open".into(), folder.into()])],
        ),
    },
    CataloguedApp { id: "windows-terminal", label: "Windows Terminal", kind: ExternalAppKind::Terminal, locations: locations!(Windows => [command_with("wt.exe", |folder| vec!["-d".into(), folder.into()])]) },
    CataloguedApp { id: "gnome-terminal", label: "GNOME Terminal", kind: ExternalAppKind::Terminal, locations: locations!(Linux => [command_with("gnome-terminal", |folder| vec![format!("--working-directory={folder}")])]) },
    CataloguedApp { id: "konsole", label: "Konsole", kind: ExternalAppKind::Terminal, locations: locations!(Linux => [command_with("konsole", |folder| vec!["--workdir".into(), folder.into()])]) },
    CataloguedApp { id: "finder", label: "Finder", kind: ExternalAppKind::Files, locations: locations!(Darwin => [Location::Command { command: "open", args: None, icon: Some("/System/Library/CoreServices/Finder.app") }]) },
    CataloguedApp { id: "explorer", label: "File Explorer", kind: ExternalAppKind::Files, locations: locations!(Windows => [command("explorer.exe")]) },
    CataloguedApp { id: "file-manager", label: "File manager", kind: ExternalAppKind::Files, locations: locations!(Linux => [command("xdg-open")]) },
];

/// Where macOS keeps applications, beyond the user's own folder, which is searched last.
const BUNDLE_FOLDERS: [&str; 3] = ["/Applications", "/System/Applications", "/System/Applications/Utilities"];

fn candidates_for(app: &CataloguedApp, platform: Platform, home: &str, folder: &str) -> Vec<AppCandidate> {
    (app.locations)(platform)
        .into_iter()
        .flat_map(|location| match location {
            Location::Bundle { bundle, launch } => BUNDLE_FOLDERS
                .iter()
                .map(|base| base.to_string())
                .chain([format!("{home}/Applications")])
                .map(|base| {
                    let path = format!("{base}/{bundle}");
                    let launch = match launch {
                        Some(launch) => launch(&path, folder),
                        None => Launch { command: "open".into(), args: vec!["-a".into(), path.clone(), folder.into()] },
                    };
                    AppCandidate { id: app.id.into(), probe: Some(path.clone()), icon: Some(path), launch }
                })
                .collect::<Vec<_>>(),
            Location::Command { command, args, icon } => vec![AppCandidate {
                id: app.id.into(),
                probe: command.starts_with('/').then(|| command.to_string()),
                icon: icon.map(String::from),
                launch: Launch { command: command.into(), args: args.map(|args| args(folder)).unwrap_or_else(|| vec![folder.into()]) },
            }],
        })
        .collect()
}

/// Every application the catalog knows on this platform, in the order the list draws them.
pub fn external_apps(platform: Platform) -> Vec<ExternalApp> {
    APPS.iter().filter(|app| !(app.locations)(platform).is_empty()).map(|app| ExternalApp { id: app.id.into(), label: app.label.into(), kind: app.kind }).collect()
}

/// Every place one application might be on this platform, best first.
pub fn app_candidates(app_id: &str, platform: Platform, home: &str, folder: &str) -> Vec<AppCandidate> {
    APPS.iter().find(|app| app.id == app_id).map(|app| candidates_for(app, platform, home, folder)).unwrap_or_default()
}

/// The launcher's absolute path, found the way a shell finds it, or None when PATH has none.
pub fn on_path(command: &str) -> Option<PathBuf> {
    act_platform::search_path::folders().into_iter().map(|folder| folder.join(command)).find(|target| target.exists())
}

/// Whether a candidate is on this machine, without starting anything.
fn exists(candidate: &AppCandidate) -> bool {
    match &candidate.probe {
        Some(probe) => Path::new(probe).exists(),
        None => on_path(&candidate.launch.command).is_some(),
    }
}

/// Long enough that opening the list twice costs one scan, short enough to see a new install.
const SCAN_TTL: Duration = Duration::from_secs(30);

fn last_scan() -> &'static Mutex<Option<(Instant, Vec<InstalledApp>)>> {
    static SCAN: Mutex<Option<(Instant, Vec<InstalledApp>)>> = Mutex::new(None);
    &SCAN
}

/// Every application on this machine that can take a folder, in catalog order. Blocking; run off the UI thread.
pub fn list_installed_apps() -> Vec<InstalledApp> {
    if let Some((at, apps)) = last_scan().lock().unwrap().as_ref() {
        if at.elapsed() < SCAN_TTL {
            return apps.clone();
        }
    }
    let platform = Platform::current();
    let home = crate::paths::home().to_string_lossy().into_owned();
    let apps: Vec<InstalledApp> = external_apps(platform)
        .into_iter()
        .filter_map(|app| {
            let candidate = app_candidates(&app.id, platform, &home, "/").into_iter().find(exists)?;
            let icon = candidate.icon.as_deref().and_then(|bundle| crate::bundle_icon::bundle_icon(Path::new(bundle)));
            Some(InstalledApp { app, icon })
        })
        .collect();
    *last_scan().lock().unwrap() = Some((Instant::now(), apps.clone()));
    apps
}

/// Opens a checkout in one application, trying each place it might be until one answers.
pub fn open_folder_in_app(app_id: &str, folder: &Path) -> Result<(), String> {
    let platform = Platform::current();
    let name = external_apps(platform).into_iter().find(|app| app.id == app_id).map(|app| app.label);
    let candidates = app_candidates(app_id, platform, &crate::paths::home().to_string_lossy(), &folder.to_string_lossy());
    let Some(name) = name.filter(|_| !candidates.is_empty()) else { return Err(concat!(act_core::app_name!(), " does not know that application.").into()) };
    for candidate in candidates {
        if exists(&candidate) && launch_detached(&candidate.launch.command, &candidate.launch.args) {
            return Ok(());
        }
    }
    Err(format!("{name} is not installed on this machine."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editors_take_the_line_their_own_way() {
        let candidates = editor_candidates(Platform::Darwin, "/a.rs", Some(7));
        assert_eq!(candidates[0].args, vec!["-g", "/a.rs:7"]);
        let zed = candidates.iter().find(|candidate| candidate.command == "zed").unwrap();
        assert_eq!(zed.args, vec!["/a.rs:7"]);
        assert_eq!(editor_launch(Platform::Linux, "idea", "/a.rs", Some(3)).unwrap().args, vec!["--line", "3", "/a.rs"]);
        assert!(opens_in_browser("/x/index.HTML", None) && !opens_in_browser("/x/index.html", Some(1)));
    }

    #[test]
    fn a_bundle_is_looked_for_in_every_folder_with_the_users_own_last() {
        let candidates = app_candidates("zed", Platform::Darwin, "/Users/me", "/repo");
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[3].probe.as_deref(), Some("/Users/me/Applications/Zed.app"));
        assert_eq!(candidates[0].launch.args, vec!["-a", "/Applications/Zed.app", "/repo"]);
        let xcode = app_candidates("xcode", Platform::Darwin, "/Users/me", "/repo");
        assert_eq!(xcode[0].launch, Launch { command: "/Applications/Xcode.app/Contents/Developer/usr/bin/xed".into(), args: vec!["--project".into(), "/repo".into()] });
        assert!(app_candidates("xcode", Platform::Linux, "/h", "/repo").is_empty());
    }
}
