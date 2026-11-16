//! Closed string unions the stored JSON uses.

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $text:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text { $($text => Some($name::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(text: &str) -> Result<Self, String> {
                Self::parse(text).ok_or_else(|| format!("unknown {}: {text}", stringify!($name)))
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
                Self::parse(&text).ok_or_else(|| serde::de::Error::custom(format!("unknown {}: {text}", stringify!($name))))
            }
        }
    };
}

string_enum!(
    /// The backends that can run a thread.
    AgentEngine { Claude = "claude", Codex = "codex", Opencode = "opencode" }
);

string_enum!(
    /// How much reasoning a run asks for.
    AgentEffort { Low = "low", Medium = "medium", High = "high", Xhigh = "xhigh", Max = "max", Ultra = "ultra" }
);

string_enum!(ExecutionPolicy {
    Confirm = "confirm",
    Plan = "plan",
    AllowEdits = "allow-edits",
    Autonomous = "autonomous",
    Bypass = "bypass",
});

string_enum!(ThreadRole {
    Coordinator = "coordinator",
    Implementer = "implementer",
    Reviewer = "reviewer",
    Researcher = "researcher",
});

string_enum!(
    /// How the thread's newest settled run ended.
    ThreadOutcome { Finished = "finished", Failed = "failed", Stopped = "stopped" }
);

string_enum!(ContinuationStatus { None = "none", Available = "available", Invalid = "invalid" });

string_enum!(MessageKind { User = "user", Assistant = "assistant", Tool = "tool", System = "system" });

string_enum!(MessageTone { Error = "error" });

string_enum!(SubagentStatus {
    Working = "working",
    Idle = "idle",
    Completed = "completed",
    Failed = "failed",
    Stopped = "stopped",
});

string_enum!(ActivityKind { Text = "text", Tool = "tool" });

string_enum!(
    /// How a thread's work lands.
    Delivery { PullRequest = "pull-request", Commit = "commit", Report = "report" }
);

string_enum!(CoordinationState { Working = "working", Blocked = "blocked", Done = "done", Failed = "failed" });

string_enum!(PullRequestState { Draft = "draft", Open = "open", Merged = "merged", Closed = "closed" });

string_enum!(PullRequestChecks { Pending = "pending", Passing = "passing", Failing = "failing" });

string_enum!(PullRequestReview { Approved = "approved", ChangesRequested = "changes-requested", Required = "required" });

string_enum!(LimitWindow { Session = "session", Weekly = "weekly" });

string_enum!(
    /// `Missed` is not a run: it marks a one-shot whose moment passed without one.
    AutomationRunStatus {
        Succeeded = "succeeded",
        Failed = "failed",
        Cancelled = "cancelled",
        Skipped = "skipped",
        Missed = "missed",
    }
);

string_enum!(DiffSide { Old = "old", New = "new" });
