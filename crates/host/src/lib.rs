//! Host-side services: git and worktrees, files, automations, the app's MCP tool server, and the
//! desktop around the window. Each service is async on Tokio and independent of `act_core`, so the
//! effect dispatcher wires them to effects mechanically.

pub mod attachments;
pub mod automation;
pub mod bundle_icon;
pub mod cli_install;
pub mod coalesce;
pub mod computer_use_host;
pub mod computers;
pub mod dock;
pub mod files;
pub mod fs_copy;
pub mod git;
pub mod image_download;
pub mod instance_lock;
pub mod linux_protocol;
pub mod lock_awake;
pub mod message_images;
pub mod message_links;
pub mod mobile;
pub mod notify;
pub mod open;
pub mod paths;
pub mod resolve_run;
pub mod serve_args;
pub mod services;
pub mod session_monitor;
pub mod text;
pub mod thread_requests;
pub mod timers;
pub mod tools;
pub mod updates;
pub mod user_data;
pub mod workspace_patches;
pub mod workspaces;
pub mod worktrees;

/// The Tokio runtime host services run on. GPUI has an executor of its own, so the window spawns host
/// work here and awaits the `JoinHandle`, which any executor can poll.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(4).thread_name("act-host").enable_all().build().expect("the host runtime starts"))
}
