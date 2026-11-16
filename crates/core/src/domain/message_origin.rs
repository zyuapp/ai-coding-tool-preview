//! Who put a user message in a thread when the user did not type it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum MessageOrigin {
    /// Another thread, with the title it had when it sent the message.
    Thread {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        computer: Option<String>,
    },
    Automation { run_number: i64 },
    /// A coordinator's batch of news from its threads.
    Coordination,
    /// Written by a build that kept only a label.
    Legacy { label: String },
}

/// A thread on another computer saying who it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSender {
    pub thread_id: String,
    pub title: String,
    pub computer: String,
}

const FROM: &str = "From ";
const ON: &str = " on ";
const SEPARATOR: &str = " · ";
const COORDINATION: &str = "Thread updates";

fn thread_name(title: &str, computer: Option<&str>) -> String {
    match computer {
        Some(computer) => format!("{title}{ON}{computer}"),
        None => title.to_string(),
    }
}

pub fn origin_label(origin: &MessageOrigin) -> String {
    match origin {
        MessageOrigin::Thread { thread_id, title, computer } => {
            let id = thread_id.as_ref().map(|id| format!("{SEPARATOR}{id}")).unwrap_or_default();
            format!("{FROM}{}{id}", thread_name(title, computer.as_deref()))
        }
        MessageOrigin::Automation { run_number } => format!("Automation run #{run_number}"),
        MessageOrigin::Coordination => COORDINATION.into(),
        MessageOrigin::Legacy { label } => label.clone(),
    }
}

/// The short name a folded row gives the sender.
pub fn sender_label(origin: Option<&MessageOrigin>) -> Option<String> {
    match origin? {
        MessageOrigin::Thread { title, computer, .. } => Some(format!("{FROM}{}", thread_name(title, computer.as_deref()))),
        MessageOrigin::Coordination => Some(COORDINATION.into()),
        _ => None,
    }
}

/// The origin a message carries, with the label that travels beside it.
pub fn origin_fields(origin: Option<MessageOrigin>) -> (Option<MessageOrigin>, Option<String>) {
    match origin {
        Some(origin) => {
            let label = origin_label(&origin);
            (Some(origin), Some(label))
        }
        None => (None, None),
    }
}

pub fn message_label(origin: Option<&MessageOrigin>, detail: Option<&str>) -> Option<String> {
    origin.map(origin_label).or_else(|| detail.map(String::from))
}

/// Whether a queued message is the user's own words, theirs to edit or have back.
pub fn is_users_own(origin: Option<&MessageOrigin>, detail: Option<&str>, answers: Option<&str>) -> bool {
    origin.is_none() && detail.is_none() && answers.is_none()
}

/// Where a message an older build labelled came from, read from the label it wrote.
pub fn legacy_origin(detail: &str) -> MessageOrigin {
    if detail == COORDINATION {
        return MessageOrigin::Coordination;
    }
    if let Some(number) = detail.strip_prefix("Automation run #") {
        if !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()) {
            if let Ok(run_number) = number.parse() {
                return MessageOrigin::Automation { run_number };
            }
        }
    }
    if let Some(sender) = detail.strip_prefix(FROM) {
        if let Some(end) = sender.rfind(SEPARATOR) {
            let thread_id = &sender[end + SEPARATOR.len()..];
            if !thread_id.is_empty() && !thread_id.chars().any(char::is_whitespace) {
                return MessageOrigin::Thread { thread_id: Some(thread_id.into()), title: sender[..end].into(), computer: None };
            }
        }
        return MessageOrigin::Thread { thread_id: None, title: sender.into(), computer: None };
    }
    MessageOrigin::Legacy { label: detail.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_labels_read_back_as_origins() {
        assert_eq!(legacy_origin("Thread updates"), MessageOrigin::Coordination);
        assert_eq!(legacy_origin("Automation run #4"), MessageOrigin::Automation { run_number: 4 });
        assert_eq!(legacy_origin("From Fix CI · abc"), MessageOrigin::Thread { thread_id: Some("abc".into()), title: "Fix CI".into(), computer: None });
        assert_eq!(legacy_origin("From Fix · a b"), MessageOrigin::Thread { thread_id: None, title: "Fix · a b".into(), computer: None });
    }
}
