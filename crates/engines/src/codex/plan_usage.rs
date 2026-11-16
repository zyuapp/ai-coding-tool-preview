//! Codex's plan limits, as the usage panel and a limited turn read them.

use std::time::Duration;

use serde_json::{json, Value};

use crate::types::{LimitWindow, PlanUsage, UsageLimit, UsagePeriod, UsageWindow};

use super::app_server::{client_info, AppServerClient};
use super::server::{codex_app_server, ServerOptions};

const READ_TIMEOUT: Duration = Duration::from_secs(20);
const LIMIT_READ_TIMEOUT: Duration = Duration::from_secs(10);

fn reset_instant(seconds: &Value) -> Option<String> {
    let seconds = seconds.as_f64().filter(|value| value.is_finite())?;
    chrono::DateTime::from_timestamp_millis((seconds * 1_000.0) as i64)
        .map(|instant| instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn window_name(primary: bool, minutes: Option<f64>) -> String {
    match minutes {
        Some(300.0) => "Current session".into(),
        Some(10_080.0) => "Current week".into(),
        Some(minutes) if minutes.is_finite() && minutes > 0.0 => {
            if minutes % 1_440.0 == 0.0 {
                format!("Current {}-day window", minutes / 1_440.0)
            } else if minutes % 60.0 == 0.0 {
                format!("Current {}-hour window", minutes / 60.0)
            } else {
                format!("Current {minutes}-minute window")
            }
        }
        _ => if primary {
            "Primary limit"
        } else {
            "Secondary limit"
        }
        .into(),
    }
}

fn usage_window(
    id: &str,
    label: &str,
    primary: bool,
    value: &Value,
    ordinary: bool,
) -> Option<UsageWindow> {
    let used = value["usedPercent"]
        .as_f64()
        .filter(|used| used.is_finite())?;
    let raw_minutes = value["windowDurationMins"].as_f64();
    let minutes = raw_minutes.filter(|minutes| minutes.is_finite() && *minutes > 0.0);
    let period = match (ordinary, minutes) {
        (true, Some(minutes)) => Some(if minutes >= 10_080.0 {
            UsagePeriod::Week
        } else {
            UsagePeriod::Session
        }),
        _ => None,
    };
    let kind = if primary { "primary" } else { "secondary" };
    Some(UsageWindow {
        id: format!("{id}:{kind}"),
        label: format!("{label}{}", window_name(primary, raw_minutes)),
        utilization: Some(used),
        resets_at: reset_instant(&value["resetsAt"]),
        model: None,
        period,
        minutes,
    })
}

fn snapshots(response: &Value) -> Vec<(String, Value)> {
    let named: Vec<(String, Value)> = response["rateLimitsByLimitId"]
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(_, snapshot)| snapshot.is_object())
                .map(|(id, snapshot)| (id.clone(), snapshot.clone()))
                .collect()
        })
        .unwrap_or_default();
    if !named.is_empty() {
        return named;
    }
    vec![(
        response["rateLimits"]["limitId"]
            .as_str()
            .unwrap_or("codex")
            .to_string(),
        response["rateLimits"].clone(),
    )]
}

/// The plan limit Codex turned a turn away for: of the ordinary limit's used-up windows, the one
/// that lifts last.
pub fn codex_usage_limit(response: &Value) -> Option<UsageLimit> {
    let mut found: Option<UsageLimit> = None;
    for value in [
        &response["rateLimits"]["primary"],
        &response["rateLimits"]["secondary"],
    ] {
        let (Some(used), Some(resets)) = (
            value["usedPercent"].as_f64(),
            value["resetsAt"].as_f64().filter(|r| r.is_finite()),
        ) else {
            continue;
        };
        if used < 100.0 {
            continue;
        }
        let resets_at = resets * 1_000.0;
        if found.is_some_and(|found| found.resets_at >= resets_at) {
            continue;
        }
        let window = if value["windowDurationMins"]
            .as_f64()
            .is_some_and(|minutes| minutes <= 300.0)
        {
            LimitWindow::Session
        } else {
            LimitWindow::Weekly
        };
        found = Some(UsageLimit { resets_at, window });
    }
    found
}

/// When the plan limit a turn ran into lifts, read from a live session.
pub async fn read_codex_usage_limit(client: &AppServerClient) -> Option<UsageLimit> {
    let response = tokio::time::timeout(
        LIMIT_READ_TIMEOUT,
        client.request("account/rateLimits/read", Value::Null),
    )
    .await
    .ok()?
    .ok()?;
    codex_usage_limit(&response)
}

fn limit_name(id: &str, snapshot: &Value) -> String {
    let name = snapshot["limitName"].as_str().unwrap_or(id);
    if name.eq_ignore_ascii_case("codex") {
        "Codex".into()
    } else {
        name.to_string()
    }
}

/// Codex's account windows in the shape the Claude reader returns.
pub fn to_codex_plan_usage(response: &Value, account_plan: Option<String>) -> PlanUsage {
    let mut limits = snapshots(response);
    limits.sort_by(|(left_id, left), (right_id, right)| {
        let (left_name, right_name) = (limit_name(left_id, left), limit_name(right_id, right));
        (left_name != "Codex")
            .cmp(&(right_name != "Codex"))
            .then_with(|| left_name.to_lowercase().cmp(&right_name.to_lowercase()))
            .then_with(|| left_id.cmp(right_id))
    });
    let several = limits.len() > 1;
    let mut windows = Vec::new();
    for (id, snapshot) in &limits {
        let ordinary = limit_name(id, snapshot) == "Codex";
        let name = if several {
            format!("{} · ", limit_name(id, snapshot))
        } else {
            String::new()
        };
        windows.extend(usage_window(
            id,
            &name,
            true,
            &snapshot["primary"],
            ordinary,
        ));
        windows.extend(usage_window(
            id,
            &name,
            false,
            &snapshot["secondary"],
            ordinary,
        ));
    }
    if windows.is_empty() {
        return PlanUsage::NotApplicable;
    }
    let subscription = limits
        .iter()
        .find_map(|(_, snapshot)| snapshot["planType"].as_str().map(String::from))
        .or(account_plan);
    PlanUsage::Available {
        subscription,
        windows,
    }
}

fn late(_: tokio::time::error::Elapsed) -> anyhow::Error {
    anyhow::anyhow!("Codex did not answer in time.")
}

/// Reads Codex plan limits over a short-lived app-server connection with no thread or run.
pub async fn read_codex_plan_usage() -> PlanUsage {
    let command = match codex_app_server(
        &[],
        ServerOptions {
            shared_home: true,
            ..Default::default()
        },
    )
    .await
    {
        Ok(command) => command,
        Err(error) => {
            return PlanUsage::Unavailable {
                message: error.to_string(),
            }
        }
    };
    let client = AppServerClient::connect(command);
    let reading = async {
        tokio::time::timeout(READ_TIMEOUT, client.initialize(client_info()))
            .await
            .map_err(late)??;
        let account = tokio::time::timeout(
            READ_TIMEOUT,
            client.request("account/read", json!({ "refreshToken": false })),
        )
        .await
        .map_err(late)??;
        if account["account"]["type"] != "chatgpt" {
            return Ok(PlanUsage::NotApplicable);
        }
        let response = tokio::time::timeout(
            READ_TIMEOUT,
            client.request("account/rateLimits/read", Value::Null),
        )
        .await
        .map_err(late)??;
        Ok::<_, anyhow::Error>(to_codex_plan_usage(
            &response,
            account["account"]["planType"].as_str().map(String::from),
        ))
    }
    .await;
    client.close().await;
    reading.unwrap_or_else(|error| PlanUsage::Unavailable {
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_windows_and_finds_the_limit_that_lifts_last() {
        let response = json!({ "rateLimits": {
            "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": 10 },
            "secondary": { "usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 20 },
            "planType": "plus",
        } });
        assert_eq!(
            codex_usage_limit(&response),
            Some(UsageLimit {
                resets_at: 20_000.0,
                window: LimitWindow::Weekly
            })
        );
        let PlanUsage::Available {
            windows,
            subscription,
        } = to_codex_plan_usage(&response, None)
        else {
            panic!()
        };
        assert_eq!(subscription.as_deref(), Some("plus"));
        assert_eq!(windows[0].label, "Current session");
        assert_eq!(windows[0].period, Some(UsagePeriod::Session));
        assert_eq!(windows[1].label, "Current week");
        assert_eq!(
            windows[0].resets_at.as_deref(),
            Some("1970-01-01T00:00:10.000Z")
        );
    }
}
