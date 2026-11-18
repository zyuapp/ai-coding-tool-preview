//! Tailscale: whether it is installed, signed in and issuing certificates, and Serve put in front of
//! the bridge's port. Nothing here throws: a machine without Tailscale is a state, not a failure.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use act_core::domain::mobile::{TailscaleState, TailscaleStatus};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::addresses::{health_path, mobile_instance, BIND_HOST, MOBILE_HEALTH_RESPONSE, MOBILE_INSTANCE_HEADER};
use crate::process::Exec;

const TAILSCALE_TIMEOUT: Duration = Duration::from_secs(10);
/// Serve provisions a certificate on its first call, which is slow the way any certificate is.
const TAILSCALE_SERVE_TIMEOUT: Duration = Duration::from_secs(90);
const TAILSCALE_HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const LOCAL_HEALTH_TIMEOUT: Duration = Duration::from_millis(1_500);
const KNOWN_CLI_PATHS: [&str; 4] = ["/usr/local/bin/tailscale", "/usr/bin/tailscale", "/opt/homebrew/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/tailscale"];

pub const OPERATOR_NEEDED: &str = "Tailscale needs permission. Run: sudo tailscale set --operator=$USER";
pub const CERTS_OFF: &str = "This tailnet does not issue HTTPS certificates yet. Turn on HTTPS in the Tailscale admin console, under DNS, then check again.";
pub const HELD_ELSEWHERE: &str = "Another AI Coding Tool on this computer is using Tailscale. Quit it, then check again.";

fn executable(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(candidate).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The command, or None when this machine has no Tailscale. Remembered, because the answer rarely moves.
pub fn find_tailscale() -> Option<PathBuf> {
    static CACHED: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut cached = CACHED.lock().unwrap();
    if let Some(binary) = cached.as_ref().filter(|binary| executable(binary)) {
        return Some(binary.clone());
    }
    let on_path = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).filter(|directory| directory.is_absolute()).map(|directory| directory.join("tailscale")).find(|candidate| executable(candidate)));
    let found = on_path.into_iter().chain(KNOWN_CLI_PATHS.iter().map(PathBuf::from)).find(|candidate| executable(candidate));
    *cached = found.clone();
    found
}

fn first_line(text: &str) -> Option<String> {
    text.lines().map(str::trim).find(|line| !line.is_empty()).map(str::to_string)
}

fn readable(error: &crate::process::RunError) -> String {
    let stderr = String::from_utf8_lossy(error.stderr()).trim().to_string();
    let text = if stderr.is_empty() { first_line(&error.to_string()) } else { first_line(&stderr) }.unwrap_or_else(|| error.to_string());
    let stripped = without_error_prefix(text.trim());
    if stripped.is_empty() { "Tailscale could not be reached.".into() } else { stripped.to_string() }
}

fn without_error_prefix(text: &str) -> &str {
    match text.get(..6) {
        Some(head) if head.eq_ignore_ascii_case("error:") => text[6..].trim_start(),
        _ => text,
    }
}

fn serve_failure(error: &crate::process::RunError) -> String {
    let said = format!("{}\n{error}", String::from_utf8_lossy(error.stderr())).to_lowercase();
    if said.contains("access denied") || said.contains("--operator") { OPERATOR_NEEDED.into() } else { readable(error) }
}

/// Tailscale can report a startup failure on stdout with a successful exit code.
pub fn parse_tailscale_json(output: &str) -> Result<Value, String> {
    serde_json::from_str(output).map_err(|error| match first_line(output) {
        Some(line) if !line.starts_with('{') && !line.starts_with('[') => line,
        _ => error.to_string(),
    })
}

async fn tailscale(binary: &Path, args: &[&str], timeout: Duration, stop: &CancellationToken) -> Result<String, String> {
    let run = Exec::new(binary).args(args.iter().copied()).timeout(timeout).max_buffer(4 * 1024 * 1024).run();
    tokio::select! {
        output = run => output.map(|output| output.stdout_text()).map_err(|error| readable(&error)),
        _ = stop.cancelled() => Err("Stopped.".into()),
    }
}

async fn tailscale_raw(binary: &Path, args: &[&str], timeout: Duration, stop: &CancellationToken) -> Result<String, crate::process::RunError> {
    let run = Exec::new(binary).args(args.iter().copied()).timeout(timeout).max_buffer(4 * 1024 * 1024).run();
    tokio::select! {
        output = run => output.map(|output| output.stdout_text()),
        _ = stop.cancelled() => Err(crate::process::RunError::Io(std::io::Error::other("Stopped."))),
    }
}

async fn serve_config(binary: &Path, stop: &CancellationToken) -> Result<Value, String> {
    let output = tailscale(binary, &["serve", "status", "--json"], TAILSCALE_TIMEOUT, stop).await?;
    parse_tailscale_json(if output.trim().is_empty() { "{}" } else { &output })
}

fn trim_dns_name(name: Option<&Value>) -> Option<String> {
    name.and_then(Value::as_str).filter(|name| name.len() > 1).map(|name| name.trim_end_matches('.').to_string())
}

/// Whether Serve is pointed at our port.
pub fn serves_port(config: &Value, port: u16) -> bool {
    let target = format!("http://127.0.0.1:{port}");
    config.get("Web").and_then(Value::as_object).is_some_and(|web| {
        web.values().any(|host| host.get("Handlers").and_then(Value::as_object).is_some_and(|handlers| handlers.values().any(|handler| handler.get("Proxy").and_then(Value::as_str) == Some(target.as_str()))))
    })
}

/// The local ports Serve hands HTTPS on 443 to.
pub fn served_ports(config: &Value) -> Vec<u16> {
    let mut ports = Vec::new();
    let Some(web) = config.get("Web").and_then(Value::as_object) else { return ports };
    for (address, host) in web {
        if !address.ends_with(":443") {
            continue;
        }
        let Some(handlers) = host.get("Handlers").and_then(Value::as_object) else { continue };
        for handler in handlers.values() {
            let Some(proxy) = handler.get("Proxy").and_then(Value::as_str) else { continue };
            let rest = proxy.strip_prefix("http://127.0.0.1:").or_else(|| proxy.strip_prefix("http://localhost:"));
            if let Some(port) = rest.map(|rest| rest.trim_end_matches('/')).filter(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit())).and_then(|rest| rest.parse::<u16>().ok()) {
                if !ports.contains(&port) {
                    ports.push(port);
                }
            }
        }
    }
    ports
}

fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap_or_default()
}

/// The instance a health probe reached, or None when what answered is not this app at all.
async fn health_instance(url: &str, timeout: Duration, stop: &CancellationToken) -> Option<String> {
    let request = async {
        let response = client().get(url).header("cache-control", "no-store").timeout(timeout).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let instance = response.headers().get(MOBILE_INSTANCE_HEADER).and_then(|value| value.to_str().ok()).unwrap_or("").to_string();
        (response.text().await.ok()? == MOBILE_HEALTH_RESPONSE).then_some(instance)
    };
    tokio::select! {
        found = request => found,
        _ = stop.cancelled() => None,
    }
}

/// Whether HTTPS for the tailnet name still terminates at this process, not merely at this app.
pub async fn reaches_mobile_server(magic_dns_name: &str, stop: &CancellationToken) -> bool {
    health_instance(&format!("https://{magic_dns_name}{}", health_path()), TAILSCALE_HEALTH_TIMEOUT, stop).await.as_deref() == Some(mobile_instance())
}

/// Whether another running copy of this app listens on a local port.
pub async fn another_instance_listens(port: u16, stop: &CancellationToken) -> bool {
    health_instance(&format!("http://{BIND_HOST}:{port}{}", health_path()), LOCAL_HEALTH_TIMEOUT, stop).await.is_some_and(|instance| instance != mobile_instance())
}

/// Whether 443 belongs to another running copy of this app, whose paired devices taking it would strand.
pub async fn held_by_another_instance(config: &Value, port: u16, stop: &CancellationToken) -> bool {
    for served in served_ports(config) {
        if served != port && another_instance_listens(served, stop).await {
            return true;
        }
    }
    false
}

fn empty(status: TailscaleStatus) -> TailscaleState {
    TailscaleState { status, ..TailscaleState::default() }
}

/// Where Tailscale stands. A name this machine was known by survives a daemon that would not answer.
pub async fn read_tailscale(port: Option<u16>, known_name: Option<String>, stop: &CancellationToken) -> TailscaleState {
    let Some(binary) = find_tailscale() else { return empty(TailscaleStatus::Missing) };
    let status = match tailscale(&binary, &["status", "--json", "--peers=false"], TAILSCALE_TIMEOUT, stop).await.and_then(|output| parse_tailscale_json(&output)) {
        Ok(status) => status,
        Err(error) => {
            if let (Some(_), Some(name)) = (port, known_name.as_deref()) {
                if reaches_mobile_server(name, stop).await {
                    return TailscaleState { status: TailscaleStatus::Ready, magic_dns_name: Some(name.to_string()), certs: true, serving: true, error: None };
                }
            }
            return TailscaleState { status: TailscaleStatus::Unavailable, magic_dns_name: known_name, error: Some(error), ..TailscaleState::default() };
        }
    };
    let state = TailscaleState {
        status: if status.get("BackendState").and_then(Value::as_str) == Some("Running") { TailscaleStatus::Ready } else { TailscaleStatus::LoggedOut },
        magic_dns_name: trim_dns_name(status.get("Self").and_then(|me| me.get("DNSName"))).or(known_name),
        certs: status.get("CertDomains").and_then(Value::as_array).is_some_and(|domains| domains.iter().any(Value::is_string)),
        serving: false,
        error: None,
    };
    let Some(port) = port.filter(|_| state.status == TailscaleStatus::Ready) else { return state };
    match serve_config(&binary, stop).await {
        Ok(config) => TailscaleState { serving: serves_port(&config, port), ..state },
        Err(_) => {
            if let Some(name) = state.magic_dns_name.clone() {
                if reaches_mobile_server(&name, stop).await {
                    return TailscaleState { certs: true, serving: true, ..state };
                }
            }
            state
        }
    }
}

/// Puts HTTPS on 443 in front of the local port, unless another running copy of this app holds it.
pub async fn start_tailscale_serve(port: u16, stop: &CancellationToken) -> Result<(), String> {
    let Some(binary) = find_tailscale() else { return Err("Tailscale is not installed on this computer.".into()) };
    let ready = read_tailscale(None, None, stop).await;
    if ready.status == TailscaleStatus::Ready && !ready.certs {
        return Err(CERTS_OFF.into());
    }
    let config = serve_config(&binary, stop).await.unwrap_or(Value::Null);
    if held_by_another_instance(&config, port, stop).await {
        return Err(HELD_ELSEWHERE.into());
    }
    let target = format!("http://{BIND_HOST}:{port}");
    tailscale_raw(&binary, &["serve", "--bg", "--https=443", &target], TAILSCALE_SERVE_TIMEOUT, stop).await.map(|_| ()).map_err(|error| serve_failure(&error))
}

/// Takes 443 down only while it still points at this port.
pub async fn stop_tailscale_serve(port: u16, stop: &CancellationToken) -> Result<(), String> {
    let Some(binary) = find_tailscale() else { return Ok(()) };
    let config = serve_config(&binary, stop).await?;
    if !served_ports(&config).contains(&port) {
        return Ok(());
    }
    tailscale_raw(&binary, &["serve", "--https=443", "off"], TAILSCALE_TIMEOUT, stop).await.map(|_| ()).map_err(|error| serve_failure(&error))
}

/// A machine on the tailnet. `id` is Tailscale's stable node ID, which outlives a renamed host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailnetPeer {
    pub id: String,
    pub host: String,
    pub name: String,
    pub os: String,
    pub online: bool,
}

pub fn peers_of(status: &Value) -> Vec<TailnetPeer> {
    let Some(peers) = status.get("Peer").and_then(Value::as_object) else { return Vec::new() };
    peers
        .values()
        .filter_map(|peer| {
            let host = peer.get("DNSName").and_then(Value::as_str).map(|name| name.trim_end_matches('.').to_string()).filter(|host| !host.is_empty())?;
            let name = peer.get("HostName").and_then(Value::as_str).filter(|name| !name.is_empty()).map(str::to_string).unwrap_or_else(|| host.clone());
            Some(TailnetPeer { id: peer.get("ID").and_then(Value::as_str).unwrap_or("").to_string(), name, host, os: peer.get("OS").and_then(Value::as_str).unwrap_or("").to_string(), online: peer.get("Online") == Some(&Value::Bool(true)) })
        })
        .collect()
}

/// Every other machine on this tailnet, as Tailscale reports it now.
pub async fn tailnet_peers() -> Result<Vec<TailnetPeer>, String> {
    let binary = find_tailscale().ok_or("Tailscale is not installed on this computer.")?;
    let output = tailscale(&binary, &["status", "--json"], TAILSCALE_TIMEOUT, &CancellationToken::new()).await?;
    Ok(peers_of(&parse_tailscale_json(&output)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_which_ports_serve_fronts() {
        let config = json!({ "Web": { "mac.ts.net:443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:7737" } } }, "mac.ts.net:8443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:9000" } } } } });
        assert!(serves_port(&config, 7737));
        assert!(!serves_port(&config, 7738));
        assert_eq!(served_ports(&config), vec![7737]);
        assert_eq!(parse_tailscale_json("failed to connect to local tailscaled\n").unwrap_err(), "failed to connect to local tailscaled");
    }

    #[test]
    fn strips_the_error_prefix_from_any_text() {
        assert_eq!(without_error_prefix("Error: not logged in"), "not logged in");
        assert_eq!(without_error_prefix("tОшибка сети"), "tОшибка сети");
    }
}
