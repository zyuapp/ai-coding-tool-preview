//! The phone bridge: a local server a paired phone, or another computer running this app, reaches
//! through Tailscale Serve. Phones see a projection of the workspace; computers see it whole.

pub mod addresses;
pub mod host;
pub mod messages;
pub mod pairing;
pub mod pairing_socket;
pub mod push;
pub mod relay;
pub mod server;
pub mod tailscale;
pub mod view;

/// The path the bridge's sockets and health check live under.
pub const MOBILE_APP_PATH: &str = "/m";
/// The port the local server asks for first. Taken, it falls back to whatever the machine offers.
pub use act_core::identity::MOBILE_DEFAULT_PORT;
pub const MOBILE_EVENT_BUFFER: usize = 600;
pub const MOBILE_BUFFER_BYTES: usize = 1024 * 1024;
/// How much a phone's socket may hold unsent before the server stops queueing patches behind it.
pub const MOBILE_CONGESTED_BYTES: usize = 256 * 1024;
pub const MAX_SESSIONS_PER_DEVICE: usize = 4;
pub const MAX_PENDING_SOCKETS: usize = 32;
pub const MOBILE_PING_INTERVAL_MS: i64 = 15 * 1_000;
pub const MOBILE_DEAD_AFTER_MS: i64 = 45 * 1_000;

pub(crate) const MAX_ID_LENGTH: usize = 256;
pub(crate) const MAX_TOKEN_LENGTH: usize = 512;
pub(crate) const MAX_DEVICE_NAME_LENGTH: usize = 128;
pub(crate) const MAX_PATH_LENGTH: usize = 4_096;

/// A complete image strip, plus room for the prompt, annotations and envelope.
pub const MAX_COMPUTER_MESSAGE_BYTES: usize = 6 * crate::attachments::MAX_ATTACHMENT_ENCODED_BYTES + 2 * 1024 * 1024;
/// Transfers share a socket with heartbeats, which wait behind large frames.
pub const COMPUTER_TRANSFER_TIMEOUT_MS: i64 = 5 * 60_000;

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
