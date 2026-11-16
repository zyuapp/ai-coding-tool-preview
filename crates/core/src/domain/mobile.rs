//! The bridge a phone reaches this computer through, as settings draws it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MobileAddressKind {
    Loopback,
    TailscaleHttps,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MobileAddress {
    pub kind: MobileAddressKind,
    pub host: String,
    pub port: i64,
}

pub fn address_secure(address: &MobileAddress) -> bool {
    address.kind == MobileAddressKind::TailscaleHttps
}

pub fn address_origin(address: &MobileAddress) -> String {
    let secure = address_secure(address);
    let scheme = if secure { "https" } else { "http" };
    let bare = (secure && address.port == 443) || (!secure && address.port == 80);
    if bare {
        format!("{scheme}://{}", address.host)
    } else {
        format!("{scheme}://{}:{}", address.host, address.port)
    }
}

pub fn preferred_address(addresses: &[MobileAddress]) -> Option<&MobileAddress> {
    [MobileAddressKind::TailscaleHttps, MobileAddressKind::Loopback].iter().find_map(|kind| addresses.iter().find(|address| address.kind == *kind))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MobileServerStatus {
    #[default]
    Off,
    Starting,
    Listening,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MobileConnectionState {
    Offline,
    Connecting,
    Resuming,
    Live,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobilePairingOffer {
    pub code: String,
    pub expires_at: i64,
    pub address: MobileAddress,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PairedDeviceKind {
    Phone,
    Computer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDeviceView {
    pub id: String,
    pub kind: PairedDeviceKind,
    pub name: String,
    pub paired_at: i64,
    pub last_seen_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MobileSessionView {
    pub id: String,
    pub kind: PairedDeviceKind,
    pub device_id: String,
    pub started_at: i64,
    pub last_seen_at: i64,
    pub sequence: i64,
    pub connection: MobileConnectionState,
    pub device_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TailscaleStatus {
    #[default]
    Unknown,
    Missing,
    Unavailable,
    LoggedOut,
    Ready,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailscaleState {
    pub status: TailscaleStatus,
    pub magic_dns_name: Option<String>,
    pub certs: bool,
    pub serving: bool,
    pub error: Option<String>,
}

/// Everything the desktop settings section draws about the phone bridge.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MobileServerState {
    pub enabled: bool,
    pub status: MobileServerStatus,
    pub port: Option<i64>,
    pub addresses: Vec<MobileAddress>,
    pub primary: Option<MobileAddress>,
    pub devices: Vec<PairedDeviceView>,
    pub sessions: Vec<MobileSessionView>,
    pub tailscale: TailscaleState,
    pub pairing: Option<MobilePairingOffer>,
    pub error: Option<String>,
}
