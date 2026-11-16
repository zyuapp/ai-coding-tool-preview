//! Copying another browser's sign-ins into the panel: finding the browser profiles on this machine,
//! the sites one holds cookies for, and reading those cookies out.

pub mod cookies;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use act_core::domain::terminal::{BrowserImportSite, BrowserImportSource};

use cookies::{chromium_key, read_cookie_hosts, read_cookies, ChromiumKey, CookieFamily, ReadError, SameSite, StoredCookie};

/// A browser this machine may have. `roots` are its user-data folders, every packaging of it
/// included. `keychain` names its macOS key; `libsecret` and `kwallet` name where Linux keeps it.
#[derive(Clone, Debug)]
pub struct BrowserKind {
    pub key: &'static str,
    pub name: &'static str,
    pub family: CookieFamily,
    pub roots: Vec<PathBuf>,
    pub keychain: Option<String>,
    pub libsecret: Option<&'static str>,
    pub kwallet: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct ResolvedSource {
    pub source: BrowserImportSource,
    pub kind: BrowserKind,
    pub file: PathBuf,
}

/// Why an import step failed. A blocked read is the system refusing until Full Disk Access is on.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportError {
    Blocked(String),
    Failed(String),
}

impl ImportError {
    pub fn message(&self) -> &str {
        match self {
            ImportError::Blocked(message) | ImportError::Failed(message) => message,
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message())
    }
}

fn host_name(host: &str) -> String {
    host.strip_prefix('.').unwrap_or(host).to_lowercase()
}

/// Folds cookie hosts into the sites a person recognises: a host joins the site of any parent host
/// the profile also holds cookies for. A cookie can never be set on a public suffix, so no unrelated
/// sites ever fold together.
pub fn import_sites(hosts: &[(String, i64)]) -> Vec<BrowserImportSite> {
    let mut names: Vec<(String, i64)> = hosts.iter().map(|(host, cookies)| (host_name(host), *cookies)).filter(|(name, _)| !name.is_empty()).collect();
    names.sort_by_key(|(name, _)| name.split('.').count());
    let mut order: Vec<String> = Vec::new();
    let mut sites: HashMap<String, i64> = HashMap::new();
    for (name, cookies) in names {
        let labels: Vec<&str> = name.split('.').collect();
        let parent = (1..labels.len()).map(|index| labels[index..].join(".")).find(|candidate| sites.contains_key(candidate)).unwrap_or(name);
        if !sites.contains_key(&parent) {
            order.push(parent.clone());
        }
        *sites.entry(parent).or_insert(0) += cookies;
    }
    let mut result: Vec<BrowserImportSite> = order.into_iter().map(|domain| BrowserImportSite { cookies: sites[&domain], domain }).collect();
    result.sort_by(|left, right| left.domain.cmp(&right.domain));
    result
}

/// Whether a cookie set for `host` belongs to `site`.
pub fn cookie_on_site(host: &str, site: &str) -> bool {
    let name = host_name(host);
    name == site || name.ends_with(&format!(".{site}"))
}

fn catalog(home: &Path, config_home: Option<PathBuf>) -> Vec<BrowserKind> {
    if cfg!(target_os = "macos") {
        let support = home.join("Library").join("Application Support");
        let chromium = |key: &'static str, name: &'static str, folder: &str| BrowserKind {
            key,
            name,
            family: CookieFamily::Chromium,
            roots: vec![support.join(folder)],
            keychain: Some(format!("{name} Safe Storage")),
            libsecret: None,
            kwallet: None,
        };
        return vec![
            chromium("chrome", "Chrome", "Google/Chrome"),
            chromium("brave", "Brave", "BraveSoftware/Brave-Browser"),
            chromium("edge", "Microsoft Edge", "Microsoft Edge"),
            chromium("arc", "Arc", "Arc/User Data"),
            chromium("vivaldi", "Vivaldi", "Vivaldi"),
            chromium("opera", "Opera", "com.operasoftware.Opera"),
            chromium("chromium", "Chromium", "Chromium"),
            BrowserKind { key: "firefox", name: "Firefox", family: CookieFamily::Firefox, roots: vec![support.join("Firefox")], keychain: None, libsecret: None, kwallet: None },
        ];
    }
    if cfg!(target_os = "linux") {
        let config = config_home.filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| home.join(".config"));
        let flatpak = |app: &str, rest: &[&str]| rest.iter().fold(home.join(".var").join("app").join(app), |path, part| path.join(part));
        let chromium = |key: &'static str, name: &'static str, roots: Vec<PathBuf>, libsecret: &'static str, kwallet: &'static str| BrowserKind {
            key,
            name,
            family: CookieFamily::Chromium,
            roots,
            keychain: None,
            libsecret: Some(libsecret),
            kwallet: Some(kwallet),
        };
        return vec![
            chromium("chrome", "Chrome", vec![config.join("google-chrome"), flatpak("com.google.Chrome", &["config", "google-chrome"])], "chrome", "Chrome"),
            chromium(
                "brave",
                "Brave",
                vec![
                    config.join("BraveSoftware").join("Brave-Browser"),
                    flatpak("com.brave.Browser", &["config", "BraveSoftware", "Brave-Browser"]),
                    home.join("snap/brave/current/.config/BraveSoftware/Brave-Browser"),
                ],
                "brave",
                "Brave",
            ),
            // Edge and Opera keep their key under Chromium's name on Linux, and Vivaldi under Chrome's.
            chromium("edge", "Microsoft Edge", vec![config.join("microsoft-edge"), flatpak("com.microsoft.Edge", &["config", "microsoft-edge"])], "chromium", "Chromium"),
            chromium("vivaldi", "Vivaldi", vec![config.join("vivaldi"), flatpak("com.vivaldi.Vivaldi", &["config", "vivaldi"])], "chrome", "Chrome"),
            chromium("opera", "Opera", vec![config.join("opera")], "chromium", "Chromium"),
            chromium(
                "chromium",
                "Chromium",
                vec![config.join("chromium"), home.join("snap/chromium/common/chromium"), flatpak("org.chromium.Chromium", &["config", "chromium"])],
                "chromium",
                "Chromium",
            ),
            BrowserKind {
                key: "firefox",
                name: "Firefox",
                family: CookieFamily::Firefox,
                roots: vec![
                    home.join(".mozilla/firefox"),
                    config.join("mozilla/firefox"),
                    home.join("snap/firefox/common/.mozilla/firefox"),
                    flatpak("org.mozilla.firefox", &[".mozilla", "firefox"]),
                    flatpak("org.mozilla.firefox", &["config", "mozilla", "firefox"]),
                ],
                keychain: None,
                libsecret: None,
                kwallet: None,
            },
        ];
    }
    Vec::new()
}

fn chromium_cookie_file(profile: &Path) -> Option<PathBuf> {
    [profile.join("Network").join("Cookies"), profile.join("Cookies")].into_iter().find(|file| file.exists())
}

fn chromium_profiles(kind: &BrowserKind, root: &Path, root_index: usize) -> Vec<ResolvedSource> {
    let mut found = Vec::new();
    let add = |found: &mut Vec<ResolvedSource>, folder: &str, profile: &str| {
        if let Some(file) = chromium_cookie_file(&root.join(folder)) {
            found.push(ResolvedSource {
                source: BrowserImportSource { id: format!("{}:{root_index}:{folder}", kind.key), browser: kind.name.to_string(), profile: profile.to_string() },
                kind: kind.clone(),
                file,
            });
        }
    };
    let state = std::fs::read_to_string(root.join("Local State")).ok().and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    if let Some(cache) = state.as_ref().and_then(|state| state.pointer("/profile/info_cache")).and_then(|cache| cache.as_object()) {
        for (folder, info) in cache {
            if folder.contains('/') || folder.contains('\\') || folder.starts_with('.') {
                continue;
            }
            let name = info.get("name").and_then(|name| name.as_str()).filter(|name| !name.is_empty()).unwrap_or(folder);
            add(&mut found, folder, name);
        }
    }
    // No list of profiles leaves the usual one, or the folder itself the way Opera keeps it.
    if found.is_empty() {
        add(&mut found, "Default", "Default");
    }
    if found.is_empty() {
        add(&mut found, ".", "Default");
    }
    found
}

/// The [Profile…] sections of a Firefox profiles.ini, the default one first.
pub fn firefox_profile_entries(ini: &str) -> Vec<HashMap<String, String>> {
    let mut sections: Vec<HashMap<String, String>> = Vec::new();
    for line in ini.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.len() > 2 {
            let mut section = HashMap::new();
            section.insert("section".to_string(), trimmed[1..trimmed.len() - 1].to_string());
            sections.push(section);
        } else if let Some(current) = sections.last_mut() {
            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                if !key.is_empty() {
                    current.insert(key.to_string(), value.trim().to_string());
                }
            }
        }
    }
    let installed: HashSet<String> = sections
        .iter()
        .filter(|section| section["section"].starts_with("Install"))
        .filter_map(|section| section.get("Default").cloned())
        .filter(|default| !default.is_empty())
        .collect();
    let mut profiles: Vec<HashMap<String, String>> =
        sections.into_iter().filter(|section| section["section"].starts_with("Profile") && section.get("Path").is_some_and(|path| !path.is_empty())).collect();
    profiles.sort_by_key(|section| !installed.contains(&section["Path"]));
    profiles
}

fn firefox_profiles(kind: &BrowserKind, root: &Path, root_index: usize) -> Vec<ResolvedSource> {
    let Ok(ini) = std::fs::read_to_string(root.join("profiles.ini")) else { return Vec::new() };
    firefox_profile_entries(&ini)
        .into_iter()
        .filter_map(|entry| {
            let path = entry["Path"].clone();
            let folder = if entry.get("IsRelative").map(String::as_str) == Some("0") { PathBuf::from(&path) } else { root.join(&path) };
            let file = folder.join("cookies.sqlite");
            if !file.exists() {
                return None;
            }
            let profile = entry
                .get("Name")
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| folder.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default());
            Some(ResolvedSource { source: BrowserImportSource { id: format!("{}:{root_index}:{path}", kind.key), browser: kind.name.to_string(), profile }, kind: kind.clone(), file })
        })
        .collect()
}

pub fn discover_in(home: &Path, config_home: Option<PathBuf>) -> Vec<ResolvedSource> {
    catalog(home, config_home)
        .iter()
        .flat_map(|kind| {
            kind.roots.iter().enumerate().filter(|(_, root)| root.exists()).flat_map(move |(index, root)| match kind.family {
                CookieFamily::Firefox => firefox_profiles(kind, root, index),
                CookieFamily::Chromium => chromium_profiles(kind, root, index),
            })
        })
        .collect()
}

pub fn discover() -> Vec<ResolvedSource> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    discover_in(&home, std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
}

/// The browser profiles on this machine that hold cookies.
pub fn list_import_sources() -> Vec<BrowserImportSource> {
    discover().into_iter().map(|resolved| resolved.source).collect()
}

/// An id is only ever matched against what is on disk now, so nothing a caller sends becomes a path.
fn resolve(source_id: &str) -> Result<ResolvedSource, ImportError> {
    discover()
        .into_iter()
        .find(|resolved| resolved.source.id == source_id)
        .ok_or_else(|| ImportError::Failed("That browser profile is no longer on this computer.".into()))
}

/// A refused read on macOS names what to allow.
fn reading_error(source: &ResolvedSource, error: std::io::Error) -> ImportError {
    let denied = matches!(error.kind(), std::io::ErrorKind::PermissionDenied) || matches!(error.raw_os_error(), Some(1) | Some(13));
    if cfg!(target_os = "macos") && denied {
        ImportError::Blocked(format!("macOS blocked reading {}. Turn on {} in Full Disk Access, then try again.", source.source.browser, act_core::identity::APP_NAME))
    } else {
        ImportError::Failed(error.to_string())
    }
}

pub fn list_import_sites(source_id: &str) -> Result<Vec<BrowserImportSite>, ImportError> {
    let source = resolve(source_id)?;
    let hosts = read_cookie_hosts(source.kind.family, &source.file).map_err(|error| reading_error(&source, error))?;
    Ok(import_sites(&hosts))
}

enum Ran {
    Output(String),
    Failed,
    Missing,
}

/// What a command printed, whether it failed, or that it is not installed.
fn run(command: &str, args: &[&str], timeout: Duration) -> Ran {
    let child = Command::new(command).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ran::Missing,
        Err(_) => return Ran::Failed,
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(stdout) = stdout.as_mut() {
            let _ = stdout.read_to_string(&mut text);
        }
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break None,
        }
    };
    let text = reader.join().unwrap_or_default();
    match status {
        Some(status) if status.success() => Ran::Output(text.strip_suffix('\n').unwrap_or(&text).to_string()),
        _ => Ran::Failed,
    }
}

fn output(ran: Ran) -> Option<String> {
    match ran {
        Ran::Output(text) => Some(text),
        _ => None,
    }
}

/// The KDE wallet Chromium stores its key in, which the user may have renamed from the default.
fn network_wallet() -> String {
    for service in ["kwalletd6", "kwalletd5"] {
        let destination = format!("--dest=org.kde.{service}");
        let path = format!("/modules/{service}");
        if let Some(name) = output(run("dbus-send", &["--session", "--print-reply=literal", &destination, &path, "org.kde.KWallet.networkWallet"], Duration::from_secs(5))) {
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }
    }
    "kdewallet".into()
}

/// Where each platform keeps the password a Chromium browser encrypts cookies with. macOS asks the
/// user before handing it over. Linux falls back to the fixed password Chromium uses without a keyring.
fn chromium_keys(kind: &BrowserKind, version: &str, missing: &mut Vec<&'static str>) -> Result<Vec<ChromiumKey>, String> {
    if cfg!(target_os = "macos") {
        let service = kind.keychain.clone().unwrap_or_default();
        return match output(run("security", &["find-generic-password", "-w", "-s", &service], Duration::from_secs(120))) {
            Some(password) => Ok(vec![chromium_key(&password, 1003)]),
            None => Err(format!("macOS did not share {}'s key. Choose Allow when it asks.", kind.name)),
        };
    }
    if version == "v10" {
        return Ok(vec![chromium_key("peanuts", 1)]);
    }
    let wallet = network_wallet();
    let secret = run("secret-tool", &["lookup", "application", kind.libsecret.unwrap_or_default()], Duration::from_secs(30));
    if matches!(secret, Ran::Missing) {
        missing.push("secret-tool");
    }
    let kwallet = kind.kwallet.unwrap_or_default();
    let entry = format!("{kwallet} Safe Storage");
    let folder = format!("{kwallet} Keys");
    let passwords = [output(secret), output(run("kwallet-query", &["--read-password", &entry, "--folder", &folder, &wallet], Duration::from_secs(30))), Some(String::new())];
    let mut seen = HashSet::new();
    Ok(passwords.into_iter().flatten().filter(|password| seen.insert(password.clone())).map(|password| chromium_key(&password, 1)).collect())
}

/// The cookies for those sites, read from that profile and decrypted, and how many could not be.
/// Blocking: macOS may hold the keychain read until the user answers.
pub fn read_import_cookies(source_id: &str, sites: &[String]) -> Result<(Vec<StoredCookie>, usize), ImportError> {
    let source = resolve(source_id)?;
    let mut missing = Vec::new();
    let kind = source.kind.clone();
    let read = read_cookies(source.kind.family, &source.file, sites, &mut |version| chromium_keys(&kind, version, &mut missing));
    let (cookies, skipped) = match read {
        Ok(read) => read,
        Err(ReadError::Io(error)) => return Err(reading_error(&source, error)),
        Err(ReadError::Keys(message)) => return Err(ImportError::Failed(message)),
    };
    if cookies.is_empty() && skipped > 0 && missing.contains(&"secret-tool") {
        return Err(ImportError::Failed(format!("{} keeps its key in the system keyring. Install secret-tool (libsecret-tools) and try again.", source.source.browser)));
    }
    Ok((cookies, skipped))
}

/// One cookie as the panel's cookie store takes it.
#[derive(Clone, Debug, PartialEq)]
pub struct CookieDetails {
    pub url: String,
    pub name: String,
    pub value: String,
    /// Only for a domain cookie: a host-only cookie leaves it out, which keeps it host-only.
    pub domain: Option<String>,
    pub host: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    pub expires: Option<i64>,
}

/// What the panel needs to set one cookie the way the other browser held it.
pub fn cookie_details(cookie: &StoredCookie) -> CookieDetails {
    let host = cookie.host.strip_prefix('.').unwrap_or(&cookie.host).to_string();
    let path = if cookie.path.starts_with('/') { cookie.path.as_str() } else { "/" };
    CookieDetails {
        url: format!("{}://{host}{path}", if cookie.secure { "https" } else { "http" }),
        name: cookie.name.clone(),
        value: cookie.value.clone(),
        domain: cookie.host.starts_with('.').then(|| cookie.host.clone()),
        host,
        path: cookie.path.clone(),
        secure: cookie.secure,
        http_only: cookie.http_only,
        same_site: cookie.same_site,
        expires: cookie.expires,
    }
}

/// Opens the setting a blocked read asks the user to change.
pub fn open_full_disk_access() {
    if cfg!(target_os = "macos") {
        let _ = Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles").spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdomains_fold_into_their_site() {
        let sites = import_sites(&[("accounts.google.com".into(), 2), (".google.com".into(), 3), ("mail.google.com".into(), 1), ("github.com".into(), 4)]);
        assert_eq!(sites, vec![BrowserImportSite { domain: "github.com".into(), cookies: 4 }, BrowserImportSite { domain: "google.com".into(), cookies: 6 }]);
        assert!(cookie_on_site(".mail.google.com", "google.com"));
        assert!(!cookie_on_site("notgoogle.com", "google.com"));
    }

    #[test]
    fn firefox_default_profile_comes_first() {
        let ini = "[Profile0]\nName=old\nPath=a.old\n\n[Profile1]\nName=main\nPath=b.main\n\n[Install123]\nDefault=b.main\n";
        let entries = firefox_profile_entries(ini);
        assert_eq!(entries.iter().map(|entry| entry["Name"].as_str()).collect::<Vec<_>>(), vec!["main", "old"]);
    }
}
