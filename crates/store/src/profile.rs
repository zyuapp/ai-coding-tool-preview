//! Where a profile keeps its data (`src/main/user-data.ts`). The installed app reads the Electron
//! app's profile and checkouts; a source build has its own (`act_core::identity`).

use std::path::{Path, PathBuf};

pub use act_core::identity::{DEV_PROFILE_NAME, PROFILE_NAME};
use act_core::identity::{DEV_WORKTREES_HOME, WORKTREES_HOME};

/// Points the app at another profile folder, such as a scratch copy for testing.
pub const PROFILE_DIR_ENV: &str = "ACT_PROFILE_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// The app's name for the profile, shown where the app names itself.
    pub name: String,
    pub user_data: PathBuf,
    /// Checkouts live outside app data: no space in the path, and out of backups.
    pub worktrees_root: PathBuf,
}

impl Profile {
    /// A source build's profile; see `resolve_for`.
    pub fn resolve() -> Option<Profile> {
        Profile::resolve_for(false)
    }

    /// The profile `ACT_PROFILE_DIR` names, else the installed or source build's default. New
    /// checkouts go under the home directory's `WORKTREES_HOME` or `DEV_WORKTREES_HOME`. None when
    /// neither the home directory nor the override is known.
    pub fn resolve_for(packaged: bool) -> Option<Profile> {
        let home = home_dir();
        let name = if packaged { PROFILE_NAME } else { DEV_PROFILE_NAME };
        let user_data = match std::env::var_os(PROFILE_DIR_ENV).filter(|value| !value.is_empty()) {
            Some(dir) => absolute(Path::new(&dir)),
            None => app_data_dir(home.as_deref())?.join(name),
        };
        let worktrees_root = home.unwrap_or_else(|| user_data.clone()).join(if packaged { WORKTREES_HOME } else { DEV_WORKTREES_HOME }).join("worktrees");
        Some(Profile { name: name.to_owned(), user_data, worktrees_root })
    }

    /// A profile at an explicit folder, with its checkouts under `worktrees_root`.
    pub fn at(user_data: impl Into<PathBuf>, worktrees_root: impl Into<PathBuf>) -> Profile {
        Profile { name: PROFILE_NAME.to_owned(), user_data: user_data.into(), worktrees_root: worktrees_root.into() }
    }

    /// Creates the profile folder, which every file in it is written under.
    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.user_data)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.user_data.join(name)
    }

    pub fn tasks_database(&self) -> PathBuf {
        self.file("tasks.v3.sqlite")
    }

    /// The window's key-value storage.
    pub fn window_storage(&self) -> PathBuf {
        self.file(crate::json_storage::WINDOW_STORAGE_FILE)
    }

    pub fn window_theme(&self) -> PathBuf {
        self.file("window-theme.v1.json")
    }

    pub fn window_placement(&self) -> PathBuf {
        self.file("window-placement.v1.json")
    }

    /// Paired computers and this computer's chosen name.
    pub fn computers(&self) -> PathBuf {
        self.file("computers.v1.json")
    }

    pub fn workspaces(&self) -> PathBuf {
        self.file("workspaces.v1.json")
    }

    pub fn launched_version(&self) -> PathBuf {
        self.file("launched-version")
    }

    pub fn update_relaunch(&self) -> PathBuf {
        self.file("update-relaunch.json")
    }

    pub fn mobile(&self) -> PathBuf {
        self.file("mobile.v1.json")
    }

    pub fn mobile_devices(&self) -> PathBuf {
        self.file("mobile-devices.v1.json")
    }

    pub fn attachments(&self) -> PathBuf {
        self.file("attachments")
    }

    pub fn message_images(&self) -> PathBuf {
        self.file("message-images")
    }

    pub fn generated_images(&self) -> PathBuf {
        self.file("generated-images")
    }

    /// Where threads with no project run.
    pub fn projectless(&self) -> PathBuf {
        self.file("projectless")
    }

    /// The Codex home the app runs Codex under.
    pub fn codex_private(&self) -> PathBuf {
        self.file("codex-private")
    }

    pub fn serve_socket(&self) -> PathBuf {
        self.file("serve.sock")
    }

    /// Where the app kept checkouts before, and where an imported profile's were made: still its
    /// own, listed and removable, never created in.
    pub fn legacy_worktrees_roots(&self) -> Vec<PathBuf> {
        legacy_worktrees_roots(&self.user_data, &self.worktrees_root)
    }

    /// Every root the app's checkouts may be under, which a project folder may never be inside.
    pub fn worktrees_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.worktrees_root.clone()];
        roots.extend(self.legacy_worktrees_roots());
        roots
    }
}

/// The roots `legacy_worktrees_roots` names for a profile folder and its current checkout root.
pub fn legacy_worktrees_roots(user_data: &Path, worktrees_root: &Path) -> Vec<PathBuf> {
    let mut roots = vec![user_data.join("worktrees")];
    roots.extend(crate::import::imported_worktrees_roots(user_data));
    let mut unique: Vec<PathBuf> = Vec::new();
    for root in roots {
        if root != worktrees_root && !unique.contains(&root) {
            unique.push(root);
        }
    }
    unique
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(PathBuf::from)
}

/// The platform's per-user application data directory, as Electron's `appData` names it.
pub fn app_data_dir(home: Option<&Path>) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        return Some(home?.join("Library").join("Application Support"));
    }
    match std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(home?.join(".config")),
    }
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_names_the_profile_folder() {
        std::env::set_var(PROFILE_DIR_ENV, "/tmp/act-profile-test");
        let profile = Profile::resolve().unwrap();
        std::env::remove_var(PROFILE_DIR_ENV);
        assert_eq!(profile.tasks_database(), PathBuf::from("/tmp/act-profile-test/tasks.v3.sqlite"));
        assert_eq!(profile.worktrees_roots()[1], PathBuf::from("/tmp/act-profile-test/worktrees"));
        assert!(profile.worktrees_root.ends_with(".aicodingtool-preview-dev/worktrees"));
        let default = Profile::resolve_for(true).unwrap();
        assert!(default.user_data.ends_with(PROFILE_NAME));
        assert!(default.worktrees_root.ends_with(".aicodingtool/worktrees"));
    }
}
