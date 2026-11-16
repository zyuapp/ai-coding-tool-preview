//! Notices for phones whose app is closed, sent through the relay that holds the APNs key.

use std::time::Duration;

use serde_json::{json, Value};

use super::pairing::{PairedDevice, PhonePush};
use crate::notify::ThreadNotice;

pub const PUSH_RELAY_URL: &str = "https://aicodingtool-relay.zyuapp.workers.dev/v1/push";
const TITLE_MAX: usize = 120;
const BODY_MAX: usize = 300;
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

fn clip(text: &str, max: usize) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.encode_utf16().count() <= max {
        return line;
    }
    format!("{}…", act_core::util::js_slice_to(&line, max - 1).trim_end())
}

/// The relay's request for one phone. A thread's notices share a group, each replacing the one before.
pub fn push_request(device: &PairedDevice, push: &PhonePush, notice: &ThreadNotice) -> Value {
    let title = clip(&notice.title, TITLE_MAX);
    let thread: String = format!("{}:{}", device.id, notice.task_id).chars().take(128).collect();
    json!({
        "token": push.token,
        "environment": push.environment,
        "title": if title.is_empty() { act_core::identity::APP_NAME.to_string() } else { title },
        "body": clip(&notice.headline, BODY_MAX),
        "thread": thread,
        "data": { "taskId": notice.task_id, "deviceId": device.id },
    })
}

/// Hands a notice to every phone that takes them. A phone Apple says is gone is answered with its id,
/// for the caller to stop asking; any other failure means the notice is simply not delivered.
pub async fn push_to_phones(targets: Vec<(PairedDevice, PhonePush)>, notice: &ThreadNotice) -> Vec<String> {
    let client = reqwest::Client::builder().timeout(SEND_TIMEOUT).build().unwrap_or_default();
    let sends = targets.into_iter().map(|(device, push)| {
        let client = client.clone();
        let body = push_request(&device, &push, notice);
        async move {
            match client.post(PUSH_RELAY_URL).json(&body).send().await {
                Ok(response) if response.status().as_u16() == 410 => Some(device.id),
                Ok(response) if !response.status().is_success() => {
                    tracing::error!("The push relay refused a notice (HTTP {}).", response.status().as_u16());
                    None
                }
                Ok(_) => None,
                Err(error) => {
                    tracing::error!("Could not reach the push relay: {error}");
                    None
                }
            }
        }
    });
    futures::future::join_all(sends).await.into_iter().flatten().collect()
}
