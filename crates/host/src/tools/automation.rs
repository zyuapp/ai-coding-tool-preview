//! The run's own automation, and what a scheduled run says it found.

use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{Map, Value};

use super::definition::{bind_tools, names_of, report, BoundTool, ToolDefinition};
use super::schema::{boolean, enumeration, string, Args, Field, Input};
use super::server::{Grants, ToolRun, ToolServer};
use super::types::{CoordinationRole, FindingReport, FindingResult, MAX_DETAIL, MAX_FINDING_KEY, MAX_HEADLINE};
use crate::automation::scheduler::AutomationScheduler;
use crate::automation::{AutomationDraft, AutomationError, AutomationPatch, AutomationView, ExecutionPolicy, MAX_ENDS_WHEN, MAX_SURFACE_WHEN};

pub const AUTOMATION_SERVER_NAME: &str = "aicodingtool-automation";

/// Scoped to the running thread, so a run can only reach its own automation. The draft's task is the run's.
pub trait AutomationBridge: Send + Sync {
    fn read(&self) -> BoxFuture<'_, Result<Option<AutomationView>, String>>;
    fn list(&self) -> BoxFuture<'_, Result<Vec<AutomationView>, String>>;
    fn save(&self, draft: AutomationDraft) -> BoxFuture<'_, Result<AutomationView, String>>;
    fn update(&self, patch: AutomationPatch) -> BoxFuture<'_, Result<AutomationView, String>>;
    fn remove(&self) -> BoxFuture<'_, Result<bool, String>>;
}

/// What a scheduled run says about itself, answered by the window that keeps it.
pub trait FindingBridge: Send + Sync {
    fn notify(&self, report: FindingReport) -> BoxFuture<'_, Result<FindingResult, String>>;
    fn nothing_to_report(&self, checked: String) -> BoxFuture<'_, Result<FindingResult, String>>;
}

fn schedule_field() -> Field {
    string().describe("A five-field cron expression in local time (\"0 8 * * *\" = 8AM daily, \"* * * * *\" = every minute), or an ISO 8601 timestamp for a single run. Seconds are not supported.")
}

fn policy_field() -> Field {
    enumeration(&["confirm", "plan", "allow-edits", "autonomous", "bypass"]).optional().describe(
        "Permission policy for scheduled runs. Defaults to this run's policy and cannot exceed it. Ask the user to change the task's permissions before scheduling work that needs stronger permissions.",
    )
}

fn surface_when_field() -> Field {
    string().max(MAX_SURFACE_WHEN).optional().describe("One sentence naming when a tick of this schedule is worth the user's attention, such as \"an error in the logs was caused by the user's own code.\" Setting it makes the schedule quiet: a run that calls nothing_to_report settles without reaching anyone, and only one that calls notify surfaces. Leave it out and the schedule keeps whatever it surfaces for now, which for a new one is every run; pass an empty sentence to take the quiet off. A one-time run always surfaces, so it takes none.")
}

fn ends_when_field() -> Field {
    string().max(MAX_ENDS_WHEN).optional().describe("One sentence naming what finishes this automation, such as \"the PR is approved.\" Set it when the automation watches something until it is done: its thread shows as running until a tick meets this and calls stop. Leave it out for a routine that repeats until someone stops it, such as a daily brief. Pass an empty sentence to make a watch a routine.")
}

fn prompt_field() -> Field {
    string().describe("The prompt to run on every tick. Write it to stand alone. A watch's stop condition goes in endsWhen, which every tick is told.")
}

fn iso(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_default()
}

pub fn describe(view: &AutomationView) -> String {
    let automation = &view.automation;
    let mut lines = vec![
        format!("schedule: {}{}", automation.schedule, automation.timezone.as_ref().map(|zone| format!(" ({zone})")).unwrap_or_default()),
        format!("paused: {}", automation.paused),
        format!("runs so far: {}", automation.run_count),
        format!("next run: {}", view.next_run_at.filter(|at| *at != 0).map(iso).unwrap_or_else(|| "none".into())),
    ];
    if let Some(sentence) = &automation.surface_when {
        lines.push(format!("surfaces when: {sentence}"));
    }
    if let Some(sentence) = &automation.ends_when {
        lines.push(format!("ends when: {sentence}"));
    }
    let last = match automation.last_run_at.filter(|at| *at != 0) {
        Some(at) => format!("{} ({})", iso(at), automation.last_status.map(|status| status.as_str()).unwrap_or("undefined")),
        None => "never".into(),
    };
    lines.push(format!("last run: {last}"));
    if let Some(policy) = automation.policy {
        lines.push(format!("policy: {}", policy.as_str()));
    }
    lines.push(format!("prompt: {}", automation.prompt));
    lines.join("\n")
}

const AUTOMATION_ERROR: &str = "Automation error: ";

fn policy_of(args: &Map<String, Value>) -> Option<ExecutionPolicy> {
    args.text("policy").as_deref().and_then(ExecutionPolicy::parse)
}

pub fn finding_tools() -> Vec<ToolDefinition<dyn FindingBridge>> {
    vec![
        ToolDefinition {
            name: "notify",
            description: "Surface this scheduled run to the user, saying what you found. Call it the moment you find something, not at the end: a run that is stopped afterwards keeps what it already raised. Call it once per finding; they accumulate. Nothing later retracts it, and a quiet run that never calls it settles without reaching anyone.".into(),
            input: Input::new(vec![
                ("headline", string().min(1).max(MAX_HEADLINE).describe("One line naming what you found. It is what the user reads in the sidebar, so write the finding itself rather than that you looked.")),
                ("detail", string().max(MAX_DETAIL).optional().describe("The finding in full, in plain text: it lands in the thread under the headline, as written.")),
                ("key", string().min(1).max(MAX_FINDING_KEY).optional().describe("What this finding is about, worded the same way on every run, so the same one is not raised twice while the user has yet to read it.")),
            ]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(AUTOMATION_ERROR, async {
                        let finding = FindingReport { headline: args.text("headline").unwrap_or_default(), detail: args.text("detail"), key: args.text("key") };
                        // Whatever went wrong, the run has said it found something, and silence is no longer its to buy.
                        bridge.notify(finding).await.map(|result| result.note).map_err(|message| format!("{message} This run found something either way, so it must not call nothing_to_report."))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "nothing_to_report",
            description: "Say this run looked and found nothing worth surfacing, which is the only thing that lets a quiet scheduled run settle without disturbing the user. Say nothing at all and the run surfaces as an ordinary one, which is what a run that could not do its job should do.".into(),
            input: Input::new(vec![("checked", string().min(1).max(MAX_HEADLINE).describe("What you actually checked, in a few words. A run that could not check what it came for has something to report, not nothing."))]),
            read_only: false,
            run: |bridge, args| async move { report(AUTOMATION_ERROR, async { bridge.nothing_to_report(args.text("checked").unwrap_or_default()).await.map(|result| result.note) }).await }.boxed(),
        },
    ]
}

pub fn automation_tools() -> Vec<ToolDefinition<dyn AutomationBridge>> {
    vec![
        ToolDefinition {
            name: "schedule",
            description: "Turn this task into a recurring automation that re-runs a prompt on a schedule. Use when the user asks to repeat, babysit, poll, or watch something on a cadence. The task keeps one automation, so calling this again replaces it. Runs never overlap: a tick that arrives while the previous run is still going is dropped.".into(),
            input: Input::new(vec![
                ("prompt", prompt_field()),
                ("schedule", schedule_field()),
                ("timezone", string().optional().describe("IANA timezone such as \"America/Los_Angeles\". Defaults to the machine's local time.")),
                ("policy", policy_field()),
                ("surfaceWhen", surface_when_field()),
                ("endsWhen", ends_when_field()),
            ]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(AUTOMATION_ERROR, async {
                        let draft = AutomationDraft {
                            task_id: String::new(),
                            prompt: args.text("prompt").unwrap_or_default(),
                            schedule: args.text("schedule").unwrap_or_default(),
                            timezone: args.text("timezone"),
                            policy: policy_of(&args),
                            surface_when: args.text("surfaceWhen"),
                            ends_when: args.text("endsWhen"),
                            paused: None,
                        };
                        Ok(format!("Automation scheduled.\n{}", describe(&bridge.save(draft).await?)))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "status",
            description: "Read this task's automation: schedule, next run, how many times it has run, and its prompt.".into(),
            input: Input::empty(),
            read_only: true,
            run: |bridge, _| async move { report(AUTOMATION_ERROR, async { Ok(bridge.read().await?.map(|view| describe(&view)).unwrap_or_else(|| "This task has no automation.".into())) }).await }.boxed(),
        },
        ToolDefinition {
            name: "update",
            description: "Change this task's existing automation without recreating it. Pass only the fields that change. Pause instead of stopping when the user wants to keep the automation around.".into(),
            input: Input::new(vec![
                ("prompt", prompt_field().optional()),
                ("schedule", schedule_field().optional()),
                ("timezone", string().optional().describe("IANA timezone for the schedule.")),
                ("policy", policy_field()),
                ("surfaceWhen", surface_when_field()),
                ("endsWhen", ends_when_field()),
                ("paused", boolean().optional().describe("Pause or resume without discarding the automation.")),
            ]),
            read_only: false,
            run: |bridge, args| {
                async move {
                    report(AUTOMATION_ERROR, async {
                        let patch = AutomationPatch {
                            prompt: args.text("prompt"),
                            schedule: args.text("schedule"),
                            timezone: args.text("timezone"),
                            policy: policy_of(&args),
                            surface_when: args.text("surfaceWhen"),
                            ends_when: args.text("endsWhen"),
                            paused: args.flag("paused"),
                        };
                        Ok(format!("Automation updated.\n{}", describe(&bridge.update(patch).await?)))
                    })
                    .await
                }
                .boxed()
            },
        },
        ToolDefinition {
            name: "stop",
            description: "Delete this task's automation so it stops firing. Call this from inside a scheduled run the moment its stop condition is met — nothing else ends an automation, so skipping this leaves it running forever.".into(),
            input: Input::empty(),
            read_only: false,
            run: |bridge, _| async move { report(AUTOMATION_ERROR, async { Ok(if bridge.remove().await? { "Automation stopped and removed." } else { "This task has no automation." }.to_string()) }).await }.boxed(),
        },
        ToolDefinition {
            name: "list_all",
            description: "List every automation across all tasks, not just this one.".into(),
            input: Input::empty(),
            read_only: true,
            run: |bridge, _| {
                async move {
                    report(AUTOMATION_ERROR, async {
                        let automations = bridge.list().await?;
                        if automations.is_empty() {
                            return Ok("No automations exist.".to_string());
                        }
                        Ok(automations.iter().map(|view| format!("task {}\n{}", view.automation.task_id, describe(view))).collect::<Vec<_>>().join("\n\n"))
                    })
                    .await
                }
                .boxed()
            },
        },
    ]
}

/// A coordinator has no automation of its own; it starts a thread to schedule recurring work.
pub fn bound_automation_tools(bridge: Arc<dyn AutomationBridge>, role: Option<CoordinationRole>) -> Vec<BoundTool> {
    let mut definitions = automation_tools();
    if role == Some(CoordinationRole::Coordinator) {
        definitions.retain(|tool| tool.read_only);
    }
    bind_tools(bridge, &definitions)
}

/// The bridge as the scheduler answers it, for the thread a run belongs to. Writes are held to the
/// run's own ceiling, which `authority` reads afresh and refuses once the run may no longer write.
pub struct ScheduledAutomations {
    pub scheduler: AutomationScheduler,
    pub task_id: String,
    pub authority: Arc<dyn Fn() -> Result<ExecutionPolicy, AutomationError> + Send + Sync>,
}

impl AutomationBridge for ScheduledAutomations {
    fn read(&self) -> BoxFuture<'_, Result<Option<AutomationView>, String>> {
        async move { Ok(self.scheduler.for_thread(&self.task_id)) }.boxed()
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<AutomationView>, String>> {
        async move { Ok(self.scheduler.list()) }.boxed()
    }

    fn save(&self, draft: AutomationDraft) -> BoxFuture<'_, Result<AutomationView, String>> {
        async move {
            let draft = AutomationDraft { task_id: self.task_id.clone(), ..draft };
            self.scheduler.save(draft, Some(self.authority.as_ref())).await.map_err(|error| error.0)
        }
        .boxed()
    }

    fn update(&self, patch: AutomationPatch) -> BoxFuture<'_, Result<AutomationView, String>> {
        async move { self.scheduler.update(&self.task_id, patch, Some(self.authority.as_ref())).await.map_err(|error| error.0) }.boxed()
    }

    fn remove(&self) -> BoxFuture<'_, Result<bool, String>> {
        async move { self.scheduler.remove(&self.task_id).await.map_err(|error| error.0) }.boxed()
    }
}

const AUTOMATION_INSTRUCTIONS: &str = "This task can schedule itself. When the user asks to repeat, babysit, poll, or watch something on a cadence, use the schedule tool instead of looping yourself or reaching for cron.";

pub fn server() -> ToolServer {
    let mut definitions = names_of(&automation_tools());
    definitions.extend(names_of(&finding_tools()));
    ToolServer {
        name: AUTOMATION_SERVER_NAME,
        definitions,
        // Scheduled runs have nobody to approve anything, and these tools only reach the run's own automation.
        grants: Grants::All,
        serve: |run: &ToolRun| {
            let automations = run.automations.clone()?;
            let mut tools = bound_automation_tools(automations, run.coordination_role);
            if let Some(findings) = run.findings.clone() {
                tools.extend(bind_tools(findings, &finding_tools()));
            }
            Some(tools)
        },
        brief: |offers, _, _| if offers.has("schedule") { vec![AUTOMATION_INSTRUCTIONS.to_string()] } else { Vec::new() },
    }
}
