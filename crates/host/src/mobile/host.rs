//! Phone access as settings drives it: kept on across launches, the server started and restarted,
//! Tailscale Serve kept in front of it, pairing codes minted, devices revoked, notices pushed.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use act_core::domain::mobile::{preferred_address, MobilePairingOffer, MobileServerState, MobileServerStatus, TailscaleState, TailscaleStatus};
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::addresses::{allowed_origins, pairing_offer, reachable_addresses, tailscale_address, BIND_HOST};
use super::pairing::PairingStore;
use super::push::push_to_phones;
use super::relay::{MobileRelay, MobileResponse, SendToWindow};
use super::server::{mobile_vocabulary, MobileServer, ServerOptions, WorkspaceHooks, SESSION_GRACE};
use super::tailscale::{read_tailscale, start_tailscale_serve, stop_tailscale_serve};
use super::{now, MOBILE_DEFAULT_PORT};
use crate::notify::ThreadNotice;

const TAILSCALE_RETRY: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(60)];
/// How often Serve is looked at once it works: Tailscale can be reset or 443 handed elsewhere, silently.
const SERVE_CHECK: Duration = Duration::from_secs(60);
const SERVER_RETRY: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(60)];

/// How Tailscale is asked about and driven; a test substitutes its own.
pub trait TailscaleHooks: Send + Sync + 'static {
    fn read(&self, port: Option<u16>, known_name: Option<String>, stop: CancellationToken) -> BoxFuture<'static, TailscaleState>;
    fn start(&self, port: u16, stop: CancellationToken) -> BoxFuture<'static, Result<(), String>>;
    /// Takes Serve down only while it still points at this port.
    fn stop(&self, port: u16, stop: CancellationToken) -> BoxFuture<'static, Result<(), String>>;
}

pub struct RealTailscale;

impl TailscaleHooks for RealTailscale {
    fn read(&self, port: Option<u16>, known_name: Option<String>, stop: CancellationToken) -> BoxFuture<'static, TailscaleState> {
        async move { read_tailscale(port, known_name, &stop).await }.boxed()
    }
    fn start(&self, port: u16, stop: CancellationToken) -> BoxFuture<'static, Result<(), String>> {
        async move { start_tailscale_serve(port, &stop).await }.boxed()
    }
    fn stop(&self, port: u16, stop: CancellationToken) -> BoxFuture<'static, Result<(), String>> {
        async move { stop_tailscale_serve(port, &stop).await }.boxed()
    }
}

pub struct MobileHostOptions {
    pub user_data: PathBuf,
    /// Hands one phone request to the window; false when there is none.
    pub send: SendToWindow,
    /// What settings should now say. Called after anything at all moves.
    pub on_state: Arc<dyn Fn(MobileServerState) + Send + Sync>,
    /// What another computer is handed and may drive. None on a host that takes no computers.
    pub workspace: Option<Arc<dyn WorkspaceHooks>>,
    pub tailscale: Option<Arc<dyn TailscaleHooks>>,
    /// The port to ask for, [`MOBILE_DEFAULT_PORT`] when None.
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MobileSettings {
    enabled: bool,
    magic_dns_name: Option<String>,
}

struct HostState {
    settings: MobileSettings,
    server: Option<MobileServer>,
    tailscale: TailscaleState,
    starting: bool,
    stopping: bool,
    failure: Option<String>,
    tailscale_stop: CancellationToken,
    tailscale_retry: Option<tokio::task::JoinHandle<()>>,
    tailscale_attempts: usize,
    server_retry: Option<tokio::task::JoinHandle<()>>,
    server_attempts: usize,
}

/// The phone bridge for one data folder.
pub struct MobileHost {
    options: MobileHostOptions,
    devices: Arc<Mutex<PairingStore>>,
    relay: Arc<MobileRelay>,
    state: Mutex<HostState>,
    /// Starting and stopping the server run one after another.
    lifecycle: tokio::sync::Mutex<()>,
    /// Tailscale is slow, so it is driven on a turn of its own.
    tailscale_turn: tokio::sync::Mutex<()>,
    me: Weak<MobileHost>,
}

fn backoff(delays: &[Duration], attempt: usize) -> Duration {
    delays[attempt.min(delays.len() - 1)]
}

impl MobileHost {
    fn settings_path(&self) -> PathBuf {
        self.options.user_data.join("mobile.v1.json")
    }

    fn read_settings(path: &PathBuf) -> MobileSettings {
        let stored = std::fs::read_to_string(path).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()).unwrap_or(Value::Null);
        MobileSettings { enabled: stored.get("enabled") == Some(&Value::Bool(true)), magic_dns_name: stored.get("magicDnsName").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_string) }
    }

    fn write_settings(&self, settings: &MobileSettings) {
        let text = json!({ "version": 1, "enabled": settings.enabled, "magicDnsName": settings.magic_dns_name }).to_string();
        if let Err(error) = std::fs::write(self.settings_path(), text) {
            tracing::error!("Could not write the phone bridge settings: {error}");
        }
    }

    fn tailscale_hooks(&self) -> Arc<dyn TailscaleHooks> {
        self.options.tailscale.clone().unwrap_or_else(|| Arc::new(RealTailscale))
    }

    /// Reads the settings and starts the server when the user left it on. Tailscale is asked about in
    /// the background, so a machine without it never slows the launch.
    pub async fn start(options: MobileHostOptions) -> Arc<Self> {
        let settings = Self::read_settings(&options.user_data.join("mobile.v1.json"));
        let devices = Arc::new(Mutex::new(PairingStore::open(options.user_data.join("mobile-devices.v1.json"), now())));
        let relay = MobileRelay::new(options.send.clone());
        let tailscale = TailscaleState { magic_dns_name: settings.magic_dns_name.clone(), ..TailscaleState::default() };
        let enabled = settings.enabled;
        let host = Arc::new_cyclic(|me| MobileHost {
            options,
            devices,
            relay,
            state: Mutex::new(HostState { settings, server: None, tailscale, starting: false, stopping: false, failure: None, tailscale_stop: CancellationToken::new(), tailscale_retry: None, tailscale_attempts: 0, server_retry: None, server_attempts: 0 }),
            lifecycle: tokio::sync::Mutex::new(()),
            tailscale_turn: tokio::sync::Mutex::new(()),
            me: me.clone(),
        });
        if enabled {
            let _turn = host.lifecycle.lock().await;
            host.start_server().await;
        } else {
            let background = host.clone();
            tokio::spawn(async move {
                let _turn = background.tailscale_turn.lock().await;
                background.refresh_tailscale_state().await;
                drop(_turn);
                background.announce();
            });
        }
        host
    }

    /// Never waits on Tailscale: whatever it is still being asked is aborted.
    pub async fn stop(&self) {
        {
            let mut state = self.state.lock().unwrap();
            state.stopping = true;
            Self::cancel_serve_retry(&mut state);
            Self::cancel_server_retry(&mut state);
            state.tailscale_stop.cancel();
            state.tailscale_stop = CancellationToken::new();
        }
        let _turn = self.lifecycle.lock().await;
        self.stop_server(false).await;
        drop(_turn);
        let _waited = self.tailscale_turn.lock().await;
    }

    fn served_name(state: &HostState) -> Option<String> {
        state.tailscale.magic_dns_name.clone().filter(|_| state.tailscale.serving)
    }

    fn addresses(state: &HostState) -> Vec<act_core::domain::mobile::MobileAddress> {
        match state.server.as_ref().and_then(MobileServer::port) {
            Some(port) => reachable_addresses(port, Self::served_name(state).as_deref()),
            None => Vec::new(),
        }
    }

    /// A tailnet name Tailscale told us about counts even while Serve is reported off.
    fn origins(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let mut addresses = Self::addresses(&state);
        if let Some(name) = &state.tailscale.magic_dns_name {
            addresses.push(tailscale_address(name));
        }
        allowed_origins(&addresses)
    }

    pub fn mobile_state(&self) -> MobileServerState {
        let state = self.state.lock().unwrap();
        let reachable = Self::addresses(&state);
        let primary = preferred_address(&reachable).cloned();
        let mut devices = self.devices.lock().unwrap();
        let pending = devices.pending(now());
        let server_status = state.server.as_ref().map(MobileServer::status).unwrap_or(MobileServerStatus::Off);
        MobileServerState {
            enabled: state.settings.enabled,
            status: if !state.settings.enabled {
                MobileServerStatus::Off
            } else if state.starting {
                MobileServerStatus::Starting
            } else if state.failure.is_some() {
                MobileServerStatus::Error
            } else {
                server_status
            },
            port: state.server.as_ref().and_then(MobileServer::port).map(i64::from),
            addresses: reachable.clone(),
            pairing: pending.as_ref().zip(primary.as_ref()).map(|(code, primary)| pairing_offer(primary, code)),
            primary,
            devices: devices.views(),
            sessions: state.server.as_ref().map(MobileServer::session_views).unwrap_or_default(),
            tailscale: state.tailscale.clone(),
            error: state.failure.clone().or_else(|| state.server.as_ref().and_then(MobileServer::error)),
        }
    }

    fn announce(&self) {
        (self.options.on_state)(self.mobile_state());
    }

    fn make_server(&self) -> MobileServer {
        let me = self.me.clone();
        let origins = self.me.clone();
        MobileServer::new(ServerOptions {
            devices: self.devices.clone(),
            port: self.options.port.unwrap_or(MOBILE_DEFAULT_PORT),
            allowed_origins: Arc::new(move || origins.upgrade().map(|host| host.origins()).unwrap_or_default()),
            relay: self.relay.clone(),
            on_change: Arc::new(move || {
                let Some(host) = me.upgrade() else { return };
                let broken = host.state.lock().unwrap().server.clone().filter(|server| server.status() == MobileServerStatus::Error);
                if let Some(broken) = broken {
                    let recovering = host.clone();
                    tokio::spawn(async move {
                        let _turn = recovering.lifecycle.lock().await;
                        recovering.recover(broken).await;
                    });
                }
                host.announce();
            }),
            workspace: self.options.workspace.clone(),
            session_grace: SESSION_GRACE,
            vocabulary: mobile_vocabulary(),
        })
    }

    /// A server that would not start is tried again for as long as phone access stays on.
    fn start_server(&self) -> BoxFuture<'_, ()> {
        async move {
            {
                let mut state = self.state.lock().unwrap();
                if state.stopping || state.server.is_some() || !state.settings.enabled {
                    return;
                }
                state.starting = true;
                state.failure = None;
            }
            self.announce();
            let started = self.make_server();
            let result = started.start(BIND_HOST).await;
            let (serving, retry) = {
                let mut state = self.state.lock().unwrap();
                state.starting = false;
                match result {
                    Ok(()) => {
                        state.server = Some(started);
                        Self::cancel_server_retry(&mut state);
                    }
                    Err(error) => state.failure = Some(error),
                }
                let enabled = state.settings.enabled;
                (state.server.is_some() && enabled, state.server.is_none() && enabled && !state.stopping)
            };
            self.announce();
            if serving {
                self.schedule_serve(Duration::ZERO);
            } else if retry {
                self.schedule_start();
            }
        }
        .boxed()
    }

    /// A server that lost its port is let go and started again.
    async fn recover(&self, broken: MobileServer) {
        let current = self.state.lock().unwrap().server.as_ref().is_some_and(|server| server.port() == broken.port() && server.status() == MobileServerStatus::Error);
        if !current {
            return;
        }
        self.stop_server(false).await;
        let retry = {
            let mut state = self.state.lock().unwrap();
            state.failure = broken.error();
            state.settings.enabled && !state.stopping
        };
        self.announce();
        if retry {
            self.schedule_start();
        }
    }

    fn schedule_start(&self) {
        let me = self.me.clone();
        let mut state = self.state.lock().unwrap();
        if let Some(retry) = state.server_retry.take() {
            retry.abort();
        }
        let delay = backoff(&SERVER_RETRY, state.server_attempts);
        state.server_attempts += 1;
        state.server_retry = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(host) = me.upgrade() else { return };
            host.state.lock().unwrap().server_retry = None;
            let _turn = host.lifecycle.lock().await;
            host.start_server().await;
        }));
    }

    fn cancel_server_retry(state: &mut HostState) {
        if let Some(retry) = state.server_retry.take() {
            retry.abort();
        }
        state.server_attempts = 0;
    }

    fn cancel_serve_retry(state: &mut HostState) {
        if let Some(retry) = state.tailscale_retry.take() {
            retry.abort();
        }
        state.tailscale_attempts = 0;
    }

    /// Turning phone access off takes Serve down too, but only while Serve still points here.
    async fn stop_server(&self, unserve: bool) {
        let (running, stop) = {
            let mut state = self.state.lock().unwrap();
            Self::cancel_serve_retry(&mut state);
            Self::cancel_server_retry(&mut state);
            state.tailscale_stop.cancel();
            state.tailscale_stop = CancellationToken::new();
            (state.server.take(), state.tailscale_stop.clone())
        };
        let port = running.as_ref().and_then(MobileServer::port);
        if let Some(running) = &running {
            running.stop().await;
        }
        self.relay.fail_all("The phone bridge was turned off.");
        let (true, Some(port)) = (unserve, port) else { return };
        let _turn = self.tailscale_turn.lock().await;
        let action = self.tailscale_hooks().stop(port, stop).await;
        self.refresh_tailscale_state().await;
        if let Err(message) = action {
            self.state.lock().unwrap().tailscale.error = Some(message);
        }
    }

    /// Asks Tailscale to serve the bridge after a pause, once at a time.
    fn schedule_serve(&self, delay: Duration) {
        let me = self.me.clone();
        let mut state = self.state.lock().unwrap();
        if let Some(retry) = state.tailscale_retry.take() {
            retry.abort();
        }
        state.tailscale_retry = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(host) = me.upgrade() else { return };
            host.state.lock().unwrap().tailscale_retry = None;
            let _turn = host.tailscale_turn.lock().await;
            host.serve_if_ready().await;
            drop(_turn);
            host.announce();
        }));
    }

    async fn refresh_tailscale_state(&self) -> TailscaleState {
        let (port, known, stop) = {
            let state = self.state.lock().unwrap();
            (state.server.as_ref().and_then(MobileServer::port), state.tailscale.magic_dns_name.clone(), state.tailscale_stop.clone())
        };
        let read = self.tailscale_hooks().read(port, known, stop.clone()).await;
        let mut state = self.state.lock().unwrap();
        if stop.is_cancelled() {
            return state.tailscale.clone();
        }
        state.tailscale = read;
        if state.tailscale.magic_dns_name.is_some() && state.tailscale.magic_dns_name != state.settings.magic_dns_name {
            state.settings.magic_dns_name = state.tailscale.magic_dns_name.clone();
            let settings = state.settings.clone();
            drop(state);
            self.write_settings(&settings);
            return self.state.lock().unwrap().tailscale.clone();
        }
        state.tailscale.clone()
    }

    /// Keeps Serve in front of the listening server, looking again once a minute once it works, and
    /// on a growing pause while it does not.
    async fn serve_if_ready(&self) {
        let (listening, port) = {
            let state = self.state.lock().unwrap();
            let port = state.server.as_ref().and_then(MobileServer::port);
            if state.stopping || port.is_none() || !state.settings.enabled {
                return;
            }
            (state.server.clone(), port.unwrap())
        };
        let serving = self.serve_listening(listening.as_ref(), port).await;
        let delay = {
            let mut state = self.state.lock().unwrap();
            let same = state.server.as_ref().and_then(MobileServer::port) == Some(port);
            if state.stopping || !same || !state.settings.enabled {
                return;
            }
            if serving {
                state.tailscale_attempts = 0;
                SERVE_CHECK
            } else {
                state.tailscale_attempts += 1;
                backoff(&TAILSCALE_RETRY, state.tailscale_attempts - 1)
            }
        };
        self.schedule_serve(delay);
    }

    async fn serve_listening(&self, listening: Option<&MobileServer>, port: u16) -> bool {
        let still = |host: &Self| host.state.lock().unwrap().server.as_ref().and_then(MobileServer::port) == listening.and_then(MobileServer::port);
        let tailscale = self.refresh_tailscale_state().await;
        if !still(self) {
            return false;
        }
        if tailscale.serving {
            return true;
        }
        if tailscale.status != TailscaleStatus::Ready || !tailscale.certs {
            return false;
        }
        let stop = self.state.lock().unwrap().tailscale_stop.clone();
        let action = self.tailscale_hooks().start(port, stop).await;
        if !still(self) {
            return false;
        }
        let tailscale = self.refresh_tailscale_state().await;
        match action {
            Err(message) => self.state.lock().unwrap().tailscale.error = Some(message),
            Ok(()) => self.devices.lock().unwrap().discard_code(),
        }
        tailscale.serving
    }

    pub async fn set_enabled(&self, enabled: bool) -> MobileServerState {
        let changed = {
            let mut state = self.state.lock().unwrap();
            if state.settings.enabled == enabled && (!enabled || state.server.is_some()) {
                drop(state);
                return self.mobile_state();
            }
            let changed = state.settings.enabled != enabled;
            state.settings.enabled = enabled;
            changed.then(|| state.settings.clone())
        };
        if let Some(settings) = changed {
            self.write_settings(&settings);
        }
        self.announce();
        let _turn = self.lifecycle.lock().await;
        if enabled {
            let running = self.state.lock().unwrap().server.is_some();
            if running {
                self.schedule_serve(Duration::ZERO);
            } else {
                self.start_server().await;
            }
        } else {
            self.stop_server(true).await;
            self.devices.lock().unwrap().discard_code();
            self.state.lock().unwrap().failure = None;
        }
        drop(_turn);
        self.announce();
        self.mobile_state()
    }

    pub fn create_pairing_code(&self) -> Result<MobilePairingOffer, String> {
        let primary = {
            let state = self.state.lock().unwrap();
            preferred_address(&Self::addresses(&state)).cloned()
        };
        let primary = primary.ok_or("Turn the phone bridge on before pairing a phone.")?;
        let code = self.devices.lock().unwrap().mint(now());
        self.announce();
        Ok(pairing_offer(&primary, &code))
    }

    pub fn revoke_device(&self, device_id: &str) -> MobileServerState {
        if self.devices.lock().unwrap().revoke(device_id) {
            let server = self.state.lock().unwrap().server.clone();
            if let Some(server) = server {
                server.drop_device(device_id);
            }
        }
        self.announce();
        self.mobile_state()
    }

    /// Asks Tailscale again and finishes the setup if it now can, starting a server that would not start first.
    pub async fn refresh_tailscale(&self) -> MobileServerState {
        let start = {
            let mut state = self.state.lock().unwrap();
            Self::cancel_serve_retry(&mut state);
            state.settings.enabled && state.server.is_none()
        };
        if start {
            let _turn = self.lifecycle.lock().await;
            self.start_server().await;
            Self::cancel_serve_retry(&mut self.state.lock().unwrap());
        }
        {
            let _turn = self.tailscale_turn.lock().await;
            if self.state.lock().unwrap().server.is_some() {
                self.serve_if_ready().await;
            } else {
                self.refresh_tailscale_state().await;
            }
        }
        self.announce();
        self.mobile_state()
    }

    /// Looks again now: a machine that slept may wake to a Tailscale reset meanwhile, and a server that can start.
    pub fn recheck(&self) {
        let (serving, start) = {
            let state = self.state.lock().unwrap();
            if state.stopping || !state.settings.enabled {
                return;
            }
            (state.server.is_some(), state.server.is_none() && !state.starting)
        };
        if serving {
            Self::cancel_serve_retry(&mut self.state.lock().unwrap());
            self.schedule_serve(Duration::ZERO);
        } else if start {
            let me = self.me.clone();
            tokio::spawn(async move {
                let Some(host) = me.upgrade() else { return };
                let _turn = host.lifecycle.lock().await;
                host.start_server().await;
            });
        }
    }

    /// Hands a notice to the computers on the line.
    pub fn notice_computers(&self, notice: &ThreadNotice) {
        if let Some(server) = self.state.lock().unwrap().server.clone() {
            server.notice(notice);
        }
    }

    /// Hands a notice to the phones that take them while closed, unless phone access is off.
    pub async fn notice_phones(&self, notice: &ThreadNotice) {
        if !self.state.lock().unwrap().settings.enabled {
            return;
        }
        let targets = self.devices.lock().unwrap().push_targets();
        for gone in push_to_phones(targets, notice).await {
            self.devices.lock().unwrap().set_push(&gone, None);
        }
    }

    pub fn announce_computer_name(&self) {
        if let Some(server) = self.state.lock().unwrap().server.clone() {
            server.announce_name();
        }
    }

    /// What every connected phone is shown: `{kind: "snapshot", view}` or `{kind: "patch", patch}`.
    pub fn publish_view(&self, update: &Value) {
        if let Some(server) = self.state.lock().unwrap().server.clone() {
            server.publish(update);
        }
    }

    pub fn answer(&self, response: MobileResponse) {
        self.relay.answer(response);
    }

    /// The pairing store, for the `aic pair` socket and tests.
    pub fn devices(&self) -> Arc<Mutex<PairingStore>> {
        self.devices.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve_args::NoTailscale;

    #[tokio::test]
    async fn phone_access_stays_on_across_launches_and_offers_a_code_only_while_listening() {
        let dir = tempfile::tempdir().unwrap();
        let options = |dir: &std::path::Path| MobileHostOptions { user_data: dir.to_path_buf(), send: Arc::new(|_| false), on_state: Arc::new(|_| {}), workspace: None, tailscale: Some(Arc::new(NoTailscale)), port: Some(0) };
        let host = MobileHost::start(options(dir.path())).await;
        assert!(host.create_pairing_code().is_err());
        let state = host.set_enabled(true).await;
        assert_eq!(state.status, MobileServerStatus::Listening);
        let offer = host.create_pairing_code().unwrap();
        assert!(offer.url.starts_with("aicodingtool://pair?link=http%3A%2F%2F127.0.0.1%3A"));
        assert_eq!(host.mobile_state().pairing.map(|pairing| pairing.code), Some(offer.code));
        host.stop().await;

        let relaunched = MobileHost::start(options(dir.path())).await;
        assert_eq!(relaunched.mobile_state().status, MobileServerStatus::Listening);
        let state = relaunched.set_enabled(false).await;
        assert_eq!((state.status, state.port, state.pairing), (MobileServerStatus::Off, None, None));
        relaunched.stop().await;
    }
}
