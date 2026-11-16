//! The names and ids that tell this app apart from the Electron AI Coding Tool, so the two run side
//! by side with their own data, commands, links and updates. `scripts/package-*.sh` and the release
//! workflow read the `pub const` lines below, so keep each on one line as a plain string literal.

/// The app's name as a literal, for `concat!`.
#[macro_export]
macro_rules! app_name {
    () => {
        "AI Coding Tool Preview"
    };
}

/// The terminal command's name as a literal, for `concat!`.
#[macro_export]
macro_rules! cli_command {
    () => {
        "aic-preview"
    };
}

pub const APP_NAME: &str = "AI Coding Tool Preview";
/// The macOS bundle identifier, which is also the Linux desktop id, window class and icon name.
pub const BUNDLE_ID: &str = "com.zyuapp.aicodingtool.preview";
/// The scheme the OS hands to this app, which the terminal command opens folders through.
pub const URL_SCHEME: &str = "aicodingtool-preview";
pub const CLI_COMMAND: &str = "aic-preview";
/// The installed Linux executable's name.
pub const LINUX_EXECUTABLE: &str = "aicodingtool-preview";
/// The release archives' file name prefix.
pub const ARTIFACT_NAME: &str = "AICodingToolPreview";
/// The profile folders under the platform's application data directory: installed, then source build.
pub const PROFILE_NAME: &str = "AI Coding Tool Preview";
pub const DEV_PROFILE_NAME: &str = "AI Coding Tool Preview Dev";
/// Where new checkouts go under the home directory: installed, then source build.
pub const WORKTREES_HOME: &str = ".aicodingtool-preview";
pub const DEV_WORKTREES_HOME: &str = ".aicodingtool-preview-dev";
/// The GitHub repository whose releases the updater reads.
pub const REPOSITORY: &str = "zyuapp/ai-coding-tool-preview";
pub const RELEASES_URL: &str = "https://github.com/zyuapp/ai-coding-tool-preview/releases";
/// The phone bridge's preferred local port; the Electron app takes 7737.
pub const MOBILE_DEFAULT_PORT: u16 = 7747;
/// What HTTP requests the app makes call it.
pub const USER_AGENT: &str = concat!("AICodingToolPreview/", env!("CARGO_PKG_VERSION"));

#[cfg(test)]
mod tests {
    #[test]
    fn the_macros_name_the_constants() {
        assert_eq!(app_name!(), super::APP_NAME);
        assert_eq!(cli_command!(), super::CLI_COMMAND);
        assert_eq!(super::RELEASES_URL, format!("https://github.com/{}/releases", super::REPOSITORY));
    }
}
