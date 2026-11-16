//! Where the bridge listens and how a phone reaches it: loopback only, with Tailscale Serve in front.

use std::sync::OnceLock;

use act_core::domain::mobile::{address_origin, MobileAddress, MobileAddressKind, MobilePairingOffer};

use super::pairing::PairingCode;
use super::MOBILE_APP_PATH;

/// The server binds only here: a phone reaches it through Tailscale Serve, never over the LAN.
pub const BIND_HOST: &str = "127.0.0.1";
pub const MOBILE_HEALTH_RESPONSE: &str = "aicodingtool-mobile-v1";
/// Tells this process's server from another copy's behind the same Tailscale name.
pub const MOBILE_INSTANCE_HEADER: &str = "x-aicodingtool-instance";
const PHONE_APP_SCHEME: &str = "aicodingtool";

pub fn health_path() -> String {
    format!("{MOBILE_APP_PATH}/health")
}

pub fn socket_path() -> String {
    format!("{MOBILE_APP_PATH}/socket")
}

/// Where another computer talks, in the window's own inputs rather than the phone's.
pub fn workspace_socket_path() -> String {
    format!("{MOBILE_APP_PATH}/workspace")
}

/// This process's id, which the health probe answers with.
pub fn mobile_instance() -> &'static str {
    static INSTANCE: OnceLock<String> = OnceLock::new();
    INSTANCE.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub fn loopback_address(port: u16) -> MobileAddress {
    MobileAddress { kind: MobileAddressKind::Loopback, host: BIND_HOST.into(), port: port.into() }
}

/// Tailscale Serve terminates TLS on 443 and proxies to the local port, so the name is all a phone needs.
pub fn tailscale_address(magic_dns_name: &str) -> MobileAddress {
    MobileAddress { kind: MobileAddressKind::TailscaleHttps, host: magic_dns_name.into(), port: 443 }
}

/// Every way a phone can reach the running server: the tailnet name while Tailscale serves it, then loopback.
pub fn reachable_addresses(port: u16, magic_dns_name: Option<&str>) -> Vec<MobileAddress> {
    let mut addresses = Vec::new();
    if let Some(name) = magic_dns_name {
        addresses.push(tailscale_address(name));
    }
    addresses.push(loopback_address(port));
    addresses
}

/// The origins a browser may open a socket from; `localhost` is spelled out beside the loopback address.
pub fn allowed_origins(addresses: &[MobileAddress]) -> Vec<String> {
    let mut origins: Vec<String> = Vec::new();
    let mut add = |origin: String| {
        if !origins.contains(&origin) {
            origins.push(origin);
        }
    };
    for address in addresses {
        add(address_origin(address));
        if address.kind == MobileAddressKind::Loopback {
            add(address_origin(&MobileAddress { host: "localhost".into(), ..address.clone() }));
        }
    }
    origins
}

fn encode_component(text: &str) -> String {
    crate::tools::browser::encode_uri_component(text)
}

/// What the QR encodes: the phone app's link around the address and the code, the code in the fragment.
pub fn pairing_url(address: &MobileAddress, code: &str) -> String {
    let link = format!("{}/#pair={}", address_origin(address), encode_component(code));
    format!("{PHONE_APP_SCHEME}://pair?link={}", encode_component(&link))
}

pub fn pairing_offer(address: &MobileAddress, pairing: &PairingCode) -> MobilePairingOffer {
    MobilePairingOffer { code: pairing.code.clone(), expires_at: pairing.expires_at, address: address.clone(), url: pairing_url(address, &pairing.code) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_qr_carries_the_code_inside_the_phone_apps_link() {
        let address = tailscale_address("mac.tail1.ts.net");
        assert_eq!(pairing_url(&address, "AB12"), "aicodingtool://pair?link=https%3A%2F%2Fmac.tail1.ts.net%2F%23pair%3DAB12");
        assert_eq!(allowed_origins(&reachable_addresses(7737, None)), vec!["http://127.0.0.1:7737", "http://localhost:7737"]);
    }
}
