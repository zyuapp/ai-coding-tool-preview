//! Registers a running AppImage as the handler of the app's URL scheme (linux-protocol.ts). An AppImage
//! may have no installed desktop entry, so its current absolute path is written into the user's XDG
//! folders; a moved AppImage is repaired on its next launch.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::cli_install::CLI_URL_SCHEME;

/// The desktop id, window class and icon name the Linux build shares.
pub const LINUX_APP_ID: &str = act_core::identity::BUNDLE_ID;

pub fn desktop_file() -> String {
    format!("{LINUX_APP_ID}.desktop")
}

/// Desktop-entry Exec quoting has its own escaping rules and never invokes a shell.
fn desktop_exec(value: &Path) -> Result<String, String> {
    let value = value.to_str().ok_or("The AppImage path is not safe to register.")?;
    if !Path::new(value).is_absolute() || value.contains(['\r', '\n', '\0']) {
        return Err("The AppImage path is not safe to register.".into());
    }
    let mut quoted = String::from("\"");
    for character in value.replace('%', "%%").chars() {
        if matches!(character, '\\' | '"' | '`' | '$') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    Ok(quoted)
}

pub fn appimage_desktop_entry(appimage: &Path) -> Result<String, String> {
    Ok([
        "[Desktop Entry]".to_string(),
        format!("Name={}", act_core::identity::APP_NAME),
        "Comment=A fast local desktop client for Claude and Codex coding tasks.".into(),
        format!("Exec={} %U", desktop_exec(appimage)?),
        "Terminal=false".into(),
        "Type=Application".into(),
        format!("Icon={LINUX_APP_ID}"),
        "Categories=Development;".into(),
        format!("StartupWMClass={LINUX_APP_ID}"),
        format!("MimeType=x-scheme-handler/{CLI_URL_SCHEME};"),
        "X-AICodingTool-AppImageIntegration=true".into(),
        String::new(),
    ]
    .join("\n"))
}

enum Existing {
    Missing,
    Symlink,
    Other,
    File(Vec<u8>),
}

fn existing(path: &Path) -> std::io::Result<Existing> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Existing::Missing),
        Err(error) => Err(error),
        Ok(meta) if meta.file_type().is_symlink() => Ok(Existing::Symlink),
        Ok(meta) if !meta.is_file() => Ok(Existing::Other),
        Ok(_) => std::fs::read(path).map(Existing::File),
    }
}

/// Entries written before the ownership marker existed still count as ours, so moved AppImages repair.
fn owned_entry(contents: &[u8]) -> bool {
    let text = String::from_utf8_lossy(contents);
    let lines: std::collections::HashSet<&str> = text.lines().collect();
    let class = format!("StartupWMClass={LINUX_APP_ID}");
    let mime = format!("MimeType=x-scheme-handler/{CLI_URL_SCHEME};");
    let name = format!("Name={}", act_core::identity::APP_NAME);
    lines.contains("[Desktop Entry]") && lines.contains(name.as_str()) && lines.contains(class.as_str()) && lines.contains(mime.as_str())
}

fn run(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program).args(args).env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                let output = child.wait_with_output().ok()?;
                return status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned());
            }
            None if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn association_is_current(desktop: &str) -> bool {
    let mime = format!("x-scheme-handler/{CLI_URL_SCHEME}");
    if let Some(stdout) = run("xdg-mime", &["query", "default", &mime], Duration::from_secs(2)) {
        return stdout.trim() == desktop;
    }
    run("gio", &["mime", &mime], Duration::from_secs(2)).and_then(|stdout| stdout.lines().next().map(|line| line.trim().ends_with(&format!(": {desktop}")))).unwrap_or(false)
}

fn associate(desktop: &str) -> Result<(), String> {
    let mime = format!("x-scheme-handler/{CLI_URL_SCHEME}");
    if run("xdg-mime", &["default", desktop, &mime], Duration::from_secs(10)).is_some() || run("gio", &["mime", &mime, desktop], Duration::from_secs(10)).is_some() {
        return Ok(());
    }
    Err(format!("Could not make {desktop} the handler for {mime}."))
}

pub struct Registration {
    pub desktop_path: PathBuf,
    pub icon_path: PathBuf,
    pub files_changed: bool,
}

/// Writes the desktop entry and icon under `data_home` (default `~/.local/share`), then makes the
/// entry the scheme's handler, through `associate_with` when given.
pub fn register_appimage(appimage: &Path, home: &Path, icon: &[u8], data_home: Option<&Path>, associate_with: Option<&dyn Fn(&str) -> Result<(), String>>) -> Result<Registration, String> {
    if !home.is_absolute() {
        return Err("The home directory is not absolute.".into());
    }
    let data_home = data_home.filter(|dir| dir.is_absolute()).map(Path::to_path_buf).unwrap_or_else(|| home.join(".local/share"));
    let applications = data_home.join("applications");
    let icons = data_home.join("icons/hicolor/256x256/apps");
    let desktop_name = desktop_file();
    let desktop_path = applications.join(&desktop_name);
    let icon_path = icons.join(format!("{LINUX_APP_ID}.png"));
    let contents = appimage_desktop_entry(appimage)?.into_bytes();
    let error = |error: std::io::Error| error.to_string();
    let current_desktop = existing(&desktop_path).map_err(error)?;
    let current_icon = existing(&icon_path).map_err(error)?;

    let write_desktop = !matches!(&current_desktop, Existing::File(bytes) if *bytes == contents);
    if write_desktop {
        match &current_desktop {
            Existing::Other => return Err(format!("Refusing to replace a non-file Linux integration path: {}", desktop_path.display())),
            Existing::File(bytes) if !owned_entry(bytes) => return Err(format!("Refusing to overwrite an unrelated desktop entry: {}", desktop_path.display())),
            _ => {}
        }
    }
    let write_icon = !matches!(&current_icon, Existing::File(bytes) if bytes == icon);
    if write_icon && matches!(current_icon, Existing::Other) {
        return Err(format!("Refusing to replace a non-file Linux integration path: {}", icon_path.display()));
    }

    let nonce = format!("{}.{}", std::process::id(), uuid::Uuid::new_v4().simple());
    let stage = |target: &Path, bytes: &[u8]| -> Result<(), String> {
        std::fs::create_dir_all(target.parent().unwrap()).map_err(error)?;
        let staged = target.with_file_name(format!("{}.{nonce}.tmp", target.file_name().unwrap().to_string_lossy()));
        let result = write_new(&staged, bytes).and_then(|_| std::fs::rename(&staged, target)).map_err(error);
        let _ = std::fs::remove_file(&staged);
        result
    };
    if write_icon {
        stage(&icon_path, icon)?;
    }
    if write_desktop {
        stage(&desktop_path, &contents)?;
    }
    match associate_with {
        Some(associate_with) => associate_with(&desktop_name)?,
        None if !association_is_current(&desktop_name) => associate(&desktop_name)?,
        None => {}
    }
    Ok(Registration { desktop_path, icon_path, files_changed: write_desktop || write_icon })
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_is_quoted_and_escaped() {
        let entry = appimage_desktop_entry(Path::new("/opt/My $App \"x\" 100%.AppImage")).unwrap();
        assert!(entry.contains(r#"Exec="/opt/My \$App \"x\" 100%%.AppImage" %U"#));
        assert!(appimage_desktop_entry(Path::new("relative.AppImage")).is_err());
        assert!(appimage_desktop_entry(Path::new("/a\nb.AppImage")).is_err());
    }

    #[test]
    fn writes_once_repairs_a_moved_image_and_keeps_unrelated_entries() {
        let home = std::env::temp_dir().join(format!("act-linux-protocol-{}", uuid::Uuid::new_v4().simple()));
        let associated = std::cell::Cell::new(0);
        let count = |_: &str| -> Result<(), String> {
            associated.set(associated.get() + 1);
            Ok(())
        };
        let first = register_appimage(Path::new("/opt/a.AppImage"), &home, b"icon", None, Some(&count)).unwrap();
        assert!(first.files_changed);
        assert!(register_appimage(Path::new("/opt/a.AppImage"), &home, b"icon", None, Some(&count)).is_ok_and(|again| !again.files_changed));
        let moved = register_appimage(Path::new("/opt/b.AppImage"), &home, b"icon", None, Some(&count)).unwrap();
        assert!(moved.files_changed);
        assert!(std::fs::read_to_string(&moved.desktop_path).unwrap().contains("/opt/b.AppImage"));

        std::fs::write(&moved.desktop_path, "[Desktop Entry]\nName=Someone else\n").unwrap();
        assert!(register_appimage(Path::new("/opt/c.AppImage"), &home, b"icon", None, Some(&count)).is_err());
        assert_eq!(associated.get(), 3);
        let _ = std::fs::remove_dir_all(&home);
    }
}
