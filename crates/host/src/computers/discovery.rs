//! The computers on this tailnet running this app: every online peer, asked whether it serves the bridge.

use std::time::Duration;

use act_core::domain::platform::DiscoveredComputer;

use crate::mobile::addresses::{health_path, MOBILE_HEALTH_RESPONSE};
use crate::mobile::tailscale::tailnet_peers;

/// A probe crosses the tailnet once; a peer that takes longer is not serving.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Whether a peer answers as this app's own bridge.
pub async fn serves_app(host: &str) -> bool {
    let client = reqwest::Client::builder().timeout(PROBE_TIMEOUT).build().unwrap_or_default();
    let Ok(response) = client.get(format!("https://{host}{}", health_path())).header("cache-control", "no-store").send().await else { return false };
    response.status().is_success() && response.text().await.is_ok_and(|text| text == MOBILE_HEALTH_RESPONSE)
}

pub async fn discover_computers() -> Result<Vec<DiscoveredComputer>, String> {
    let online: Vec<_> = tailnet_peers().await?.into_iter().filter(|peer| peer.online).collect();
    let probes = online.iter().map(|peer| serves_app(&peer.host));
    let serving = futures::future::join_all(probes).await;
    let mut found: Vec<DiscoveredComputer> = online.into_iter().zip(serving).filter(|(_, serves)| *serves).map(|(peer, _)| DiscoveredComputer { host: peer.host, name: peer.name, os: peer.os }).collect();
    found.sort_by(|left, right| crate::text::locale_compare(&left.name, &right.name));
    Ok(found)
}
