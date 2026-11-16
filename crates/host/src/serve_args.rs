//! `aic serve` and `aic pair`: the app run headless, reachable only through the bridge.

use act_core::domain::mobile::{MobilePairingOffer, TailscaleState, TailscaleStatus};
use futures::future::BoxFuture;
use futures::FutureExt;
use tokio_util::sync::CancellationToken;

use crate::mobile::host::TailscaleHooks;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeCommand {
    Serve,
    Pair,
}

/// What `aic serve` and `aic pair` were asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeArguments {
    pub command: ServeCommand,
    /// A source checkout's own data folder, apart from the installed app's.
    pub dev: bool,
    /// A source checkout serving the installed app's data folder, as `aic serve` would.
    pub installed: bool,
    /// A data folder named outright.
    pub user_data: Option<String>,
    /// The port to ask for; 0 takes whatever is free.
    pub port: Option<u16>,
    /// Loopback only: Tailscale is left alone.
    pub local: bool,
}

pub fn parse_serve_arguments(argv: &[String]) -> ServeArguments {
    let mut parsed = ServeArguments { command: ServeCommand::Serve, dev: false, installed: false, user_data: None, port: None, local: false };
    let mut arguments = argv.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "serve" => parsed.command = ServeCommand::Serve,
            "pair" => parsed.command = ServeCommand::Pair,
            "--dev" => parsed.dev = true,
            "--installed" => parsed.installed = true,
            "--local" => parsed.local = true,
            "--user-data" => parsed.user_data = arguments.next().cloned(),
            "--port" => parsed.port = arguments.next().and_then(|port| port.parse().ok()),
            _ => {}
        }
    }
    parsed
}

/// The line a terminal shows for a pairing code.
pub fn pairing_line(offer: &MobilePairingOffer, now: i64) -> String {
    let minutes = ((offer.expires_at - now) as f64 / 60_000.0).round() as i64;
    format!("Pair with code {} (expires in {minutes} minutes) or open {}", offer.code, offer.url)
}

/// Tailscale as a host that must not touch it sees it: absent, so the bridge stays on the loopback.
pub struct NoTailscale;

impl TailscaleHooks for NoTailscale {
    fn read(&self, _port: Option<u16>, _known_name: Option<String>, _stop: CancellationToken) -> BoxFuture<'static, TailscaleState> {
        futures::future::ready(TailscaleState { status: TailscaleStatus::Missing, ..TailscaleState::default() }).boxed()
    }
    fn start(&self, _port: u16, _stop: CancellationToken) -> BoxFuture<'static, Result<(), String>> {
        futures::future::ready(Err("Tailscale is off for this server.".to_string())).boxed()
    }
    fn stop(&self, _port: u16, _stop: CancellationToken) -> BoxFuture<'static, Result<(), String>> {
        futures::future::ready(Ok(())).boxed()
    }
}
