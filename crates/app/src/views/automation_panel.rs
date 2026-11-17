//! The schedule that repeats a thread: its fields read back in words, what it has done, and its
//! controls (AutomationPanel.tsx).

use act_core::domain::automation::{automation_fields, edited_automation_fields, is_one_shot_schedule, schedule_field_count, AutomationFields, AutomationView};
use act_core::state::{dock_subject, WorkspaceState, AUTOMATION_PANEL};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonVariant};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::empty_state::EmptyState;
use act_ui::components::field::TextField;
use act_ui::components::panel_header::PanelHeader;
use act_ui::input::{Autosize, InputEvent, TextInput};
use act_ui::theme::ActiveTheme;
use act_ui::IconName;
use chrono::{Datelike, TimeZone, Timelike};
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, SharedString, Subscription, Window};
use serde_json::json;

use super::root::Root;
use super::sidebar::send;
use crate::workspace::dispatch;

const DAY: i64 = 86_400_000;
const CRON_HINT: &str = "Use cron, like 0 8 * * *, or a date, like 2026-11-01T09:00";
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

/// The fields being edited, and which saved schedule they started from.
pub struct AutomationForm {
    revision: String,
    task: Option<String>,
    schedule: Entity<TextInput>,
    prompt: Entity<TextInput>,
    surface_when: Entity<TextInput>,
    ends_when: Entity<TextInput>,
    /// A new schedule being written before any exists.
    creating: bool,
    focus_schedule: bool,
    /// Where a drag on the prompt's corner began, and how tall the prompt was then.
    prompt_drag: Option<(f32, f32)>,
    ticker: Option<gpui::Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// The prompt's rows before it is dragged taller or shorter, as a textarea's `rows`.
const PROMPT_ROWS: usize = 10;
/// The least the prompt's text area may be dragged to: a multi-line field's 60px less its padding and border.
const PROMPT_MIN_HEIGHT: f32 = 46.0;

/// What a drag on the prompt's corner carries.
struct PromptGrip;

fn prompt_line(cx: &gpui::App) -> f32 {
    cx.theme().metrics.text_control * 1.45
}

/// The prompt, a fixed height that scrolls, with a grip in its corner that drags it taller or shorter.
fn prompt_field(input: &Entity<TextInput>, t: &act_ui::theme::Tokens, cx: &mut Context<Root>) -> gpui::Stateful<gpui::Div> {
    let ink = super::common::with_alpha(t.ink, 0.65);
    let grip = gpui::canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let at = |x: f32, y: f32| bounds.origin + gpui::point(px(x), px(y));
            for (from, to) in [((3.0, 9.0), (9.0, 3.0)), ((7.0, 9.0), (9.0, 7.0))] {
                let mut path = gpui::PathBuilder::stroke(px(1.0));
                path.move_to(at(from.0, from.1));
                path.line_to(at(to.0, to.1));
                if let Ok(path) = path.build() {
                    window.paint_path(path, ink);
                }
            }
        },
    )
    .size_full();
    let start = input.clone();
    div()
        .id("automation-prompt-field")
        .relative()
        .mt(px(9.0))
        .child(TextField::new("automation-prompt", input).label("Prompt"))
        .child(
            div()
                .id("automation-prompt-grip")
                .absolute()
                .right(px(3.0))
                .bottom(px(3.0))
                .size(px(9.0))
                .child(grip)
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(move |root, event: &gpui::MouseDownEvent, _, cx| {
                        let height = start.read(cx).autosize().max_height.map(f32::from).unwrap_or_default();
                        if let Some(form) = &mut root.automation {
                            form.prompt_drag = Some((f32::from(event.position.y), height));
                        }
                    }),
                )
                .on_drag(PromptGrip, |_, _, _, cx| cx.new(|_| gpui::Empty)),
        )
        .on_drag_move(cx.listener(|root, event: &gpui::DragMoveEvent<PromptGrip>, _, cx| {
            let Some(form) = &root.automation else { return };
            let Some((from, height)) = form.prompt_drag else { return };
            let height = (height + f32::from(event.event.position.y) - from).max(PROMPT_MIN_HEIGHT);
            let rows = (height / prompt_line(cx)).ceil() as usize;
            form.prompt.update(cx, |input, cx| input.set_autosize(Autosize { min_rows: rows, max_height: Some(px(height)) }, cx));
        }))
}

fn local(at: i64) -> chrono::DateTime<chrono::Local> {
    chrono::Local.timestamp_millis_opt(at).single().unwrap_or_else(chrono::Local::now)
}

fn clock(time: chrono::NaiveTime) -> String {
    let (pm, hour) = time.hour12();
    format!("{hour}:{:02} {}", time.minute(), if pm { "PM" } else { "AM" })
}

/// Within a day of now the clock is enough; anything further needs the date.
fn moment(at: i64, now: i64) -> String {
    let when = local(at);
    if (at - now).abs() < DAY {
        clock(when.time())
    } else {
        format!("{} {}, {}", when.format("%b"), when.day(), clock(when.time()))
    }
}

fn countdown(next: i64, now: i64) -> String {
    let seconds = ((next - now) as f64 / 1000.0).round().max(0.0) as i64;
    if seconds < 60 {
        format!("in {seconds}s")
    } else if seconds < 3_600 {
        format!("in {}m", (seconds as f64 / 60.0).round())
    } else if seconds < 86_400 {
        format!("in {}h", (seconds as f64 / 3_600.0).round())
    } else {
        moment(next, now)
    }
}

/// Paused and missed both have no next run; only one of them is the user's own doing.
pub fn status_label(automation: &AutomationView, now: i64) -> String {
    if automation.automation.paused {
        "Paused".into()
    } else {
        match automation.next_run_at {
            None => "Missed".into(),
            Some(next) => countdown(next, now),
        }
    }
}

/// Why the scheduler would turn the schedule down, or None when it takes it.
fn schedule_problem(schedule: &str) -> Option<String> {
    if !is_one_shot_schedule(schedule) && schedule_field_count(schedule) > 5 {
        return Some("Runs at most once a minute, so use five fields".into());
    }
    let Ok(parsed) = act_host::automation::schedule::Schedule::parse(schedule, None) else { return Some(CRON_HINT.into()) };
    if act_host::automation::scheduler::assert_schedule(schedule, None).is_err() && parsed.next_after(chrono::Utc::now()).is_some() {
        return Some(CRON_HINT.into());
    }
    parsed.next_after(chrono::Utc::now()).is_none().then(|| "Never runs again".into())
}

/// A schedule read back in words, or why it cannot be saved; None for a valid cron too unusual to say.
fn describe(schedule: &str) -> Option<Result<String, String>> {
    let trimmed = schedule.trim();
    if trimmed.is_empty() {
        return Some(Err("Needs a schedule".into()));
    }
    if let Some(problem) = schedule_problem(trimmed) {
        return Some(Err(problem));
    }
    if is_one_shot_schedule(trimmed) {
        let at = act_host::automation::schedule::parse_iso_instant(trimmed, None)?;
        let when = local(at.timestamp_millis());
        return Some(Ok(format!("Once, {} {}, {}", when.format("%b"), when.day(), clock(when.time()))));
    }
    let expanded = match trimmed.to_lowercase().as_str() {
        "@yearly" | "@annually" => "0 0 1 1 *".to_string(),
        "@monthly" => "0 0 1 * *".into(),
        "@weekly" => "0 0 * * 0".into(),
        "@daily" | "@midnight" => "0 0 * * *".into(),
        "@hourly" => "0 * * * *".into(),
        _ => trimmed.to_string(),
    };
    let fields: Vec<&str> = expanded.split_whitespace().collect();
    if fields.len() != 5 {
        return None;
    }
    let (minute, hour, day, month, weekday) = (fields[0], fields[1], fields[2], fields[3], fields[4]);
    let digits = |text: &str| !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit());
    if let Some(every) = minute.strip_prefix("*/").filter(|every| digits(every)) {
        if hour == "*" && day == "*" && month == "*" && weekday == "*" {
            return Some(Ok(format!("Every {every} minutes")));
        }
    }
    if !digits(minute) || day != "*" || month != "*" {
        return None;
    }
    if hour == "*" && weekday == "*" {
        return Some(Ok(format!("Every hour at :{minute:0>2}")));
    }
    if !digits(hour) {
        return None;
    }
    let time = clock(chrono::NaiveTime::from_hms_opt(hour.parse().ok()?, minute.parse().ok()?, 0)?);
    match weekday {
        "*" => Some(Ok(format!("Every day at {time}"))),
        "1-5" => Some(Ok(format!("Weekdays at {time}"))),
        day if day.len() == 1 && ('0'..='6').contains(&day.chars().next()?) => Some(Ok(format!("Every {} at {time}", WEEKDAYS[day.parse::<usize>().ok()?]))),
        _ => None,
    }
}

/// A tick that never ran still has a status, so the moment shown is the status's rather than the run's.
fn last_run(automation: &AutomationView, now: i64) -> String {
    let saved = &automation.automation;
    match saved.last_status_at.or(saved.last_run_at) {
        None => "never run".into(),
        Some(when) => {
            let status = saved.last_status.and_then(|status| serde_json::to_value(status).ok()).and_then(|value| value.as_str().map(String::from)).unwrap_or_else(|| "ran".into());
            format!("{status} at {}", moment(when, now))
        }
    }
}

fn text(input: &Entity<TextInput>, cx: &gpui::App) -> String {
    input.read(cx).text().to_string()
}

impl Root {
    fn automation_form(&mut self, task: Option<String>, automation: Option<&AutomationView>, fields: AutomationFields, cx: &mut Context<Root>) {
        let revision = automation.map(|automation| format!("{}:{}", automation.automation.id, automation.automation.updated_at)).unwrap_or_default();
        let stale = self.automation.as_ref().is_none_or(|form| form.revision != revision || form.task != task);
        if !stale {
            return;
        }
        let creating = automation.is_none() && self.automation.as_ref().is_some_and(|form| form.creating && form.task == task);
        let observe = |input: &Entity<TextInput>, cx: &mut Context<Root>| cx.subscribe(input, |root, _, event: &InputEvent, cx| if matches!(event, InputEvent::Changed) { root.automation_edited(cx) });
        let schedule = cx.new(|cx| TextInput::new(cx).with_text(&fields.schedule).placeholder("0 8 * * *"));
        let prompt_height = PROMPT_ROWS as f32 * prompt_line(cx);
        let prompt = cx.new(|cx| TextInput::new(cx).with_text(&fields.prompt).multiline(Autosize { min_rows: PROMPT_ROWS, max_height: Some(px(prompt_height)) }).placeholder(if automation.is_some() { "" } else { "What each run should do" }));
        let surface_when = cx.new(|cx| TextInput::new(cx).with_text(&fields.surface_when).placeholder("Every run"));
        let ends_when = cx.new(|cx| TextInput::new(cx).with_text(&fields.ends_when).placeholder("Repeats until removed"));
        let subscriptions = [&schedule, &prompt, &surface_when, &ends_when].into_iter().map(|input| observe(input, cx)).collect();
        self.automation = Some(AutomationForm { revision, task, schedule, prompt, surface_when, ends_when, creating, focus_schedule: false, prompt_drag: None, ticker: None, _subscriptions: subscriptions });
    }

    fn form_fields(&self, cx: &gpui::App) -> Option<AutomationFields> {
        let form = self.automation.as_ref()?;
        Some(AutomationFields { schedule: text(&form.schedule, cx), prompt: text(&form.prompt, cx), surface_when: text(&form.surface_when, cx), ends_when: text(&form.ends_when, cx) })
    }

    /// Keeps unsaved edits in the workspace, so closing the panel loses none of them.
    fn automation_edited(&mut self, cx: &mut Context<Root>) {
        let state = self.state(cx);
        let Some(fields) = self.form_fields(cx) else { return };
        let Some(task) = self.automation.as_ref().and_then(|form| form.task.clone()) else { return };
        if let Some(automation) = state.automations.iter().find(|automation| automation.automation.task_id == task) {
            let base = automation_fields(Some(&automation.automation));
            let held = state.automation_edits.get(&task);
            let same = match held {
                Some(edit) => edit.fields == fields && edit.base == base,
                None => fields == base,
            };
            if !same {
                dispatch(AppCommand::EditAutomation { task_id: task, base, fields }, cx);
            }
        }
        cx.notify();
    }

    fn cancel_create(&mut self, cx: &mut Context<Root>) {
        if let Some(form) = &mut self.automation {
            form.creating = false;
            for input in [&form.schedule, &form.prompt, &form.surface_when, &form.ends_when] {
                input.update(cx, |input, cx| input.reset("", cx));
            }
        }
        cx.notify();
    }
}

/// What the save buttons send: a patch of what changed, or a new draft.
struct Pending {
    can_save: bool,
    command: serde_json::Value,
}

fn pending(task: Option<&str>, automation: Option<&AutomationView>, fields: &AutomationFields, readout_error: bool, one_shot: bool) -> Pending {
    match automation {
        Some(view) => {
            let saved = &view.automation;
            let surface_changed = !one_shot && fields.surface_when != saved.surface_when.clone().unwrap_or_default();
            let end_changed = fields.ends_when != saved.ends_when.clone().unwrap_or_default();
            let dirty = fields.schedule != saved.schedule || fields.prompt != saved.prompt || surface_changed || end_changed;
            let mut patch = json!({ "schedule": fields.schedule, "prompt": fields.prompt });
            if surface_changed {
                patch["surfaceWhen"] = json!(fields.surface_when);
            }
            if end_changed {
                patch["endsWhen"] = json!(fields.ends_when);
            }
            Pending { can_save: dirty && !readout_error, command: json!({ "type": "automation.update", "taskId": task, "patch": patch }) }
        }
        None => {
            let mut draft = json!({ "schedule": fields.schedule.trim(), "prompt": fields.prompt.trim() });
            if !one_shot && !fields.surface_when.trim().is_empty() {
                draft["surfaceWhen"] = json!(fields.surface_when.trim());
            }
            if !fields.ends_when.trim().is_empty() {
                draft["endsWhen"] = json!(fields.ends_when.trim());
            }
            Pending { can_save: !fields.prompt.trim().is_empty() && !readout_error, command: json!({ "type": "automation.save", "draft": draft }) }
        }
    }
}

pub fn render(root: &mut Root, state: &WorkspaceState, owner: &str, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = theme.tokens.clone();
    let m = theme.metrics.clone();
    let scheduled = dock_subject(state, owner, AUTOMATION_PANEL);
    let task = scheduled.clone().or_else(|| state.current_id.clone());
    let automation = task.as_ref().and_then(|task| state.automations.iter().find(|automation| &automation.automation.task_id == task)).cloned();
    let edit = automation.as_ref().and_then(|automation| state.automation_edits.get(&automation.automation.task_id));
    let fields = edited_automation_fields(automation.as_ref().map(|view| &view.automation), edit);
    root.automation_form(task.clone(), automation.as_ref(), fields, cx);
    let now = act_core::util::now();
    let ticking = automation.as_ref().is_some_and(|automation| automation.next_run_at.is_some());
    if let Some(form) = &mut root.automation {
        if ticking && form.ticker.is_none() {
            form.ticker = Some(cx.spawn(async |root, cx| loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                if root.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }));
        } else if !ticking {
            form.ticker = None;
        }
    }
    let status = automation.as_ref().map(|automation| status_label(automation, now));
    let mut header = PanelHeader::new("automation-header", "Automation");
    if let Some(status) = status.clone() {
        header = header.action(div().text_size(px(m.text_meta)).text_color(if status == "Paused" { t.muted } else { t.info_soft }).child(status));
    }
    let form = root.automation.as_ref().expect("form");
    if automation.is_none() && !form.creating {
        return div()
            .id("automation-panel")
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(
                div().flex_1().flex().items_center().justify_center().child(
                    EmptyState::new().icon(IconName::AlarmClock).title("No automation yet").action(Button::new("automation-new", "New schedule").icon(IconName::Plus).icon_size(14.0).variant(ButtonVariant::Soft).on_click(cx.listener(|root, _, _, cx| {
                        if let Some(form) = &mut root.automation {
                            form.creating = true;
                            form.focus_schedule = true;
                        }
                        cx.notify();
                    }))),
                ),
            )
            .into_any_element();
    }
    let (schedule_input, prompt_input, surface_input, ends_input) = (form.schedule.clone(), form.prompt.clone(), form.surface_when.clone(), form.ends_when.clone());
    if form.focus_schedule {
        schedule_input.update(cx, |input, _| input.focus(window));
        if let Some(form) = &mut root.automation {
            form.focus_schedule = false;
        }
    }
    let fields = AutomationFields { schedule: text(&schedule_input, cx), prompt: text(&prompt_input, cx), surface_when: text(&surface_input, cx), ends_when: text(&ends_input, cx) };
    let one_shot = is_one_shot_schedule(&fields.schedule);
    let readout = describe(&fields.schedule);
    let error = matches!(readout, Some(Err(_)));
    let pending = pending(scheduled.as_deref(), automation.as_ref(), &fields, error, one_shot);

    let mut schedule_field = TextField::new("automation-schedule", &schedule_input).label("Schedule").mono(true);
    match &readout {
        Some(Ok(text)) => schedule_field = schedule_field.hint(text.clone()),
        Some(Err(text)) => schedule_field = schedule_field.error(Some(SharedString::from(text.clone()))),
        None => {}
    }
    let field = |element: AnyElement| div().mt(px(9.0)).child(element);
    let mut body = div()
        .flex()
        .flex_col()
        .child(field(schedule_field.into_any_element()))
        .child(prompt_field(&prompt_input, &t, cx));
    if !one_shot {
        body = body.child(field(TextField::new("automation-surface", &surface_input).label("Notify me when").into_any_element()));
    }
    body = body.child(field(TextField::new("automation-ends", &ends_input).label("Stop when").into_any_element()));

    let save_command = pending.command.clone();
    let can_save = pending.can_save;
    if let Some(view) = &automation {
        let saved = &view.automation;
        let thread = state.thread(&saved.task_id);
        let mut facts: Vec<(&str, String)> = vec![
            ("Runs", saved.run_count.to_string()),
            ("Last run", last_run(view, now)),
            ("Last found", thread.and_then(|thread| thread.last_finding_at).map(|at| moment(at, now)).unwrap_or_else(|| "Nothing yet".into())),
        ];
        if let Some(checked) = thread.and_then(|thread| thread.last_checked.as_ref()) {
            facts.push(("Last checked", format!("{} {}", checked.note, moment(checked.at, now))));
        }
        if let Some(overrun) = saved.overrun_count.filter(|count| *count > 0) {
            facts.push(("Dropped", format!("{overrun} for overrunning")));
        }
        body = body.child(
            div().mt(px(12.0)).flex().flex_col().gap(px(3.0)).text_size(px(m.text_meta)).children(facts.into_iter().map(|(label, value)| div().flex().gap(px(8.0)).child(div().w(px(92.0)).flex_none().text_color(t.quiet_strong).child(label)).child(div().flex_1().min_w_0().text_color(t.muted_strong).child(value)))),
        );
        let paused = saved.paused;
        let target = scheduled.clone();
        let run_target = scheduled.clone();
        let pause_target = scheduled.clone();
        let delete_target = scheduled.clone();
        body = body.child(
            div()
                .mt(px(9.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(div().mr_auto().child(Button::new("automation-save", "Save").variant(ButtonVariant::Primary).disabled(!can_save).tip(super::chrome::key_tip("Save", "Mod+Enter")).on_click(move |_, _, cx| send(save_command.clone(), cx))))
                .child(Button::new("automation-run", "Run now").on_click(move |_, _, cx| send(json!({ "type": "automation.run-now", "taskId": run_target }), cx)))
                .child(
                    Button::new("automation-pause", if paused { "Resume" } else { "Pause" })
                        .name(if paused { "Resume automation" } else { "Pause automation" })
                        .icon(if paused { IconName::Play } else { IconName::Pause })
                        .icon_size(14.0)
                        .tip(if paused { "Resume automation" } else { "Pause automation" })
                        .on_click(move |_, _, cx| send(json!({ "type": "automation.update", "taskId": pause_target, "patch": { "paused": !paused } }), cx)),
                )
                .child(ConfirmAction::new("automation-remove", "", "Remove", move |_, cx| send(json!({ "type": "automation.delete", "taskId": delete_target }), cx)).icon(IconName::Trash2).tip("Remove").name("Remove automation")),
        );
        let _ = target;
    } else {
        body = body.child(
            div()
                .mt(px(9.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .child(div().mr_auto().child(Button::new("automation-create", "Create").variant(ButtonVariant::Primary).disabled(!can_save).tip(super::chrome::key_tip("Create", "Mod+Enter")).on_click(move |_, _, cx| send(save_command.clone(), cx))))
                .child(Button::new("automation-cancel", "Cancel").on_click(cx.listener(|root, _, _, cx| root.cancel_create(cx)))),
        );
    }
    let key_command = pending.command.clone();
    let existing = automation.is_some();
    div()
        .id("automation-panel")
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(header)
        .child(
            div()
                .id("automation-body")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .px(px(22.0))
                .pt(px(16.0))
                .pb(px(22.0))
                .on_key_down(cx.listener(move |root, event: &gpui::KeyDownEvent, _, cx| {
                    let keys = &event.keystroke;
                    if keys.key == "enter" && keys.modifiers.platform {
                        cx.stop_propagation();
                        if can_save {
                            send(key_command.clone(), cx);
                        }
                    } else if keys.key == "escape" && !existing {
                        cx.stop_propagation();
                        root.cancel_create(cx);
                    }
                }))
                .child(body),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::describe;

    #[test]
    fn schedules_read_back_in_words() {
        assert!(matches!(describe("*/15 * * * *"), Some(Ok(text)) if text == "Every 15 minutes"));
        assert!(matches!(describe("5 * * * *"), Some(Ok(text)) if text == "Every hour at :05"));
        assert!(matches!(describe("0 8 * * 1-5"), Some(Ok(text)) if text == "Weekdays at 8:00 AM"));
        assert!(matches!(describe("30 17 * * 2"), Some(Ok(text)) if text == "Every Tuesday at 5:30 PM"));
        assert!(matches!(describe("0 0 * * * *"), Some(Err(text)) if text.starts_with("Runs at most once a minute")));
        assert!(matches!(describe("nonsense"), Some(Err(_))));
        assert!(matches!(describe(""), Some(Err(text)) if text == "Needs a schedule"));
        assert!(describe("0 9 1 * *").is_none());
    }
}
