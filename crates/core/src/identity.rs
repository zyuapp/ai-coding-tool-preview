//! The names and ids the installed app goes by: the Electron AI Coding Tool's name, profile,
//! checkouts, command and links, so it replaces that app. Source builds keep a profile of their own.
//! `scripts/package-*.sh` and the release workflow read the `pub const` lines below, so keep each
//! on one line as a plain string literal.

/// The app's name as a literal, for `concat!`.
#[macro_export]
macro_rules! app_name {
    () => {
        "AI Coding Tool"
    };
}

/// The terminal command's name as a literal, for `concat!`.
#[macro_export]
macro_rules! cli_command {
    () => {
        "aic"
    };
}

pub const APP_NAME: &str = "AI Coding Tool";
/// The macOS bundle identifier, which is also the Linux desktop id, window class and icon name.
pub const BUNDLE_ID: &str = "com.zyuapp.aicodingtool";
/// The scheme the OS hands to this app, which the terminal command opens folders through.
pub const URL_SCHEME: &str = "aicodingtool";
pub const CLI_COMMAND: &str = "aic";
/// The installed Linux executable's name.
pub const LINUX_EXECUTABLE: &str = "aicodingtool";
/// The release archives' file name prefix.
pub const ARTIFACT_NAME: &str = "AICodingTool";
/// The profile folders under the platform's application data directory: installed, then source build.
pub const PROFILE_NAME: &str = "AI Coding Tool";
pub const DEV_PROFILE_NAME: &str = "AI Coding Tool Preview Dev";
/// Where new checkouts go under the home directory: installed, then source build.
pub const WORKTREES_HOME: &str = ".aicodingtool";
pub const DEV_WORKTREES_HOME: &str = ".aicodingtool-preview-dev";
/// The GitHub repository whose releases the updater reads.
pub const REPOSITORY: &str = "zyuapp/ai-coding-tool-preview";
pub const RELEASES_URL: &str = "https://github.com/zyuapp/ai-coding-tool-preview/releases";
/// The phone bridge's preferred local port.
pub const MOBILE_DEFAULT_PORT: u16 = 7737;
/// What HTTP requests the app makes call it.
pub const USER_AGENT: &str = concat!("AICodingTool/", env!("CARGO_PKG_VERSION"));

#[cfg(test)]
mod tests {
    #[test]
    fn the_macros_name_the_constants() {
        assert_eq!(app_name!(), super::APP_NAME);
        assert_eq!(cli_command!(), super::CLI_COMMAND);
        assert_eq!(super::RELEASES_URL, format!("https://github.com/{}/releases", super::REPOSITORY));
    }
}
