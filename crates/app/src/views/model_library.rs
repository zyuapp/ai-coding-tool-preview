//! The model chip on the composer and the library it opens: providers, models, favorites, effort
//! and permission mode (ComposerSettings.tsx, model-library.ts).

use super::letter_spacing::LetterSpacing;
use std::collections::HashMap;

use act_core::domain::coordination::CoordinationRole;
use act_core::domain::engine::{
    capabilities_for, catalog_has_model, effort_for_model, efforts_for, engine_label, engine_notice, is_favorite, models_for, offered_policies, AgentEngine, EffortSpec, EngineAccess, EngineModel, EngineNotice,
    EngineReadiness, ModelCatalog, ModelSpec, AGENT_ENGINES,
};
use act_core::domain::run::{AgentEffort, ExecutionPolicy};
use act_core::state::WorkspaceState;
use act_core::AppCommand;
use act_ui::components::button::{IconButton, Segmented, SegmentedOption};
use act_ui::components::field::{SearchField, SearchVariant};
use act_ui::input::{self as text_input, InputEvent, TextInput};
use act_ui::overlay::{bounds_cell, bounds_probe, placed, Align, BoundsCell, Placement};
use act_ui::style::{glass, Corners, GlassKind, PaneFill, Raised};
use act_ui::theme::{ActiveTheme, Tone};
use act_ui::tooltip::Tippable;
use act_ui::IconName;
use gpui::{canvas, deferred, div, prelude::*, px, relative, AnyElement, Context, Entity, FontWeight, KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent, SharedString, Subscription, Window};
use serde_json::json;

use super::common::{engine_icon, icon, ink_icon, rimmed, Ellipsis};
use super::root::Root;
use super::sidebar::send;
use crate::workspace::dispatch;

/// The widest the library is drawn; a narrower window takes it to its own width.
const LIBRARY_WIDTH: f32 = 560.0;
const KNOB: f32 = 18.0;

/// What the library picks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryTarget {
    /// A composer: the dock tab's, by its task, or the page's (None).
    Composer(Option<String>),
    /// What a coordinator's new threads run on, from its Overview.
    Overview(String),
}

pub struct LibraryState {
    target: LibraryTarget,
    search: Entity<TextInput>,
    filter: String,
    /// The model row the arrow keys have reached, by its place among the rows that can be picked.
    highlight: Option<usize>,
    /// The row under the pointer, which reads as the highlighted one does.
    hovered: Option<usize>,
    /// Where the effort knob is being dragged, from 0 to 1 along its travel.
    drag: Option<f32>,
    /// The pointer is over the effort slider, which swells its knob.
    slider_hover: bool,
    rail: BoundsCell,
    /// The effort slider, one stop the arrows move along.
    effort_focus: gpui::FocusHandle,
    _subscription: Subscription,
}

struct Heading {
    key: String,
    engine: AgentEngine,
    label: String,
    first: bool,
    models: Vec<ModelSpec>,
}

struct Group {
    heading: Heading,
    selectable: bool,
    hint: Option<Hint>,
}

enum Hint {
    SignIn,
    Notice(EngineNotice),
}

fn headings(catalog: &ModelCatalog) -> Vec<Heading> {
    let mut headings = Vec::new();
    for engine in AGENT_ENGINES {
        let mut providers: Vec<(String, Vec<ModelSpec>)> = Vec::new();
        for spec in models_for(catalog, engine) {
            let provider = spec.provider.clone().unwrap_or_default();
            match providers.iter_mut().find(|(name, _)| *name == provider) {
                Some((_, models)) => models.push(spec.clone()),
                None => providers.push((provider, vec![spec.clone()])),
            }
        }
        if providers.is_empty() {
            providers.push((String::new(), Vec::new()));
        }
        for (index, (provider, models)) in providers.into_iter().enumerate() {
            headings.push(Heading {
                key: if provider.is_empty() { engine.as_str().to_string() } else { format!("{}:{provider}", engine.as_str()) },
                engine,
                label: if provider.is_empty() { engine_label(engine).to_string() } else { provider },
                first: index == 0,
                models,
            });
        }
    }
    headings
}

fn available(catalog: &ModelCatalog, access: &HashMap<AgentEngine, EngineReadiness>, engine: AgentEngine, locked: bool) -> Vec<Group> {
    headings(catalog)
        .into_iter()
        .filter(|heading| !locked || heading.engine == engine)
        .map(|mut heading| {
            let readiness = access.get(&heading.engine).cloned().unwrap_or_else(EngineReadiness::ready);
            let hint = if !heading.first {
                None
            } else if readiness.access == EngineAccess::SignedOut {
                Some(Hint::SignIn)
            } else {
                engine_notice(heading.engine, &readiness).map(Hint::Notice)
            };
            if let Some(models) = &readiness.models {
                heading.models.retain(|spec| models.contains(&spec.id));
            }
            Group { heading, selectable: readiness.access == EngineAccess::Ready, hint }
        })
        .collect()
}

fn model_glyph(engine: AgentEngine, model: &str) -> IconName {
    match (engine, model) {
        (AgentEngine::Claude, "fable") | (AgentEngine::Codex, "gpt-6.1-sol") => IconName::Sparkles,
        (AgentEngine::Claude, "opus") | (AgentEngine::Codex, "gpt-6-astra") => IconName::Brain,
        (AgentEngine::Claude, "sonnet") | (AgentEngine::Codex, "gpt-6.1-terra") => IconName::Gauge,
        (AgentEngine::Claude, "haiku") => IconName::Feather,
        (AgentEngine::Codex, "gpt-6-luna") => IconName::Moon,
        _ => IconName::Brain,
    }
}

fn mode_glyph(policy: ExecutionPolicy) -> IconName {
    match policy {
        ExecutionPolicy::Autonomous => IconName::Zap,
        ExecutionPolicy::Bypass => IconName::ShieldOff,
        ExecutionPolicy::AllowEdits => IconName::FileCheck2,
        ExecutionPolicy::Confirm | ExecutionPolicy::Plan | ExecutionPolicy::Other(_) => IconName::Hand,
    }
}

/// What the composer knows about the agent the next message goes to.
struct Choice {
    task_id: Option<String>,
    engine: AgentEngine,
    model: Option<String>,
    effort: AgentEffort,
    policy: ExecutionPolicy,
    /// Set once the thread exists, from which on it keeps its engine.
    locked: bool,
    role: Option<CoordinationRole>,
    fast_mode: bool,
    /// The library sets the effort too.
    has_effort: bool,
    /// The library sets the permission mode too.
    has_mode: bool,
    /// The coordinator whose threads' model this is, rather than a composer's.
    lead: Option<String>,
}

impl Choice {
    fn pick(&self, engine: AgentEngine, model: String) -> AppCommand {
        match &self.lead {
            Some(lead) => AppCommand::CoordinationSetThreadModel { task_id: lead.clone(), engine: Some(engine), model, effort: None },
            None => AppCommand::TaskSetModel { task_id: self.task_id.clone(), engine, model: Some(model) },
        }
    }

    fn set_effort(&self, effort: AgentEffort) -> AppCommand {
        match &self.lead {
            Some(lead) => AppCommand::CoordinationSetThreadModel { task_id: lead.clone(), engine: Some(self.engine), model: self.model.clone().unwrap_or_default(), effort: Some(effort) },
            None => AppCommand::TaskSetEffort { task_id: self.task_id.clone(), engine: self.engine, effort },
        }
    }
}

fn choice(state: &WorkspaceState, thread: Option<&act_core::domain::thread::Thread>, engine: AgentEngine, model: Option<&str>) -> Choice {
    match thread {
        Some(thread) => Choice {
            task_id: Some(thread.id.clone()),
            engine,
            model: model.map(String::from),
            effort: thread.effort.unwrap_or_else(|| act_core::domain::engine::default_effort_for(engine)),
            policy: thread.execution_policy,
            locked: true,
            role: (thread.role == Some(act_core::domain::thread_role::ThreadRole::Coordinator)).then_some(CoordinationRole::Coordinator),
            fast_mode: thread.fast_mode.unwrap_or(false),
            has_effort: true,
            has_mode: true,
            lead: None,
        },
        None => Choice {
            task_id: None,
            engine,
            model: model.map(String::from),
            effort: state.draft_effort,
            policy: state.draft_policy,
            locked: false,
            role: (state.draft_role == Some(act_core::domain::thread_role::ThreadRole::Coordinator)).then_some(CoordinationRole::Coordinator),
            fast_mode: state.draft_fast_mode,
            has_effort: true,
            has_mode: true,
            lead: None,
        },
    }
}

fn current_choice(composer: &super::composer::ComposerState, state: &WorkspaceState) -> Choice {
    let thread = composer.thread(state);
    let engine = thread.map(|thread| thread.engine).unwrap_or(state.draft_engine);
    let catalog = state.catalog();
    let model = match thread {
        Some(thread) => act_core::domain::engine::thread_model(&catalog, engine, thread.model.as_deref()),
        None => Some(state.draft_model.clone()),
    };
    choice(state, thread, engine, model.as_deref())
}

/// What a coordinator's new threads run on; None when it is gone or its model is one this build does not know.
fn overview_choice(state: &WorkspaceState, lead: &str) -> Option<Choice> {
    let thread = state.thread(lead)?;
    let catalog = state.catalog();
    let chosen = act_core::domain::coordination::thread_model_of(&catalog, thread)?;
    let engine = chosen.engine.filter(|engine| act_core::domain::engine::engine_has_model(&catalog, *engine, &chosen.model))?;
    Some(Choice {
        task_id: Some(lead.to_string()),
        engine,
        model: Some(chosen.model),
        effort: chosen.effort.unwrap_or_else(|| act_core::domain::engine::default_effort_for(engine)),
        policy: thread.execution_policy,
        locked: false,
        role: None,
        fast_mode: false,
        has_effort: chosen.effort.is_some(),
        has_mode: false,
        lead: Some(lead.to_string()),
    })
}

/// What the open library picks for, with the composer it belongs to in place.
fn library_choice(root: &Root, state: &WorkspaceState) -> Option<Choice> {
    match root.library.as_ref().map(|library| &library.target) {
        Some(LibraryTarget::Overview(lead)) => overview_choice(state, lead),
        _ => Some(current_choice(&root.composer, state)),
    }
}

/// The efforts a model offers, shallowest first, and the one a run on it gets.
fn effort_steps(catalog: &ModelCatalog, choice: &Choice) -> (Vec<EffortSpec>, AgentEffort) {
    let Some(model) = &choice.model else { return (Vec::new(), choice.effort) };
    let mut steps = efforts_for(catalog, choice.engine, model, choice.role);
    steps.reverse();
    (steps, effort_for_model(catalog, choice.engine, model, choice.effort, choice.role))
}

fn model_name(catalog: &ModelCatalog, choice: &Choice) -> String {
    let offered = models_for(catalog, choice.engine);
    let selected = offered.iter().find(|spec| Some(&spec.id) == choice.model.as_ref()).or_else(|| choice.model.as_ref().filter(|model| catalog_has_model(catalog, model)).and_then(|_| offered.first()));
    selected.map(|spec| spec.label.clone()).or_else(|| choice.model.clone()).unwrap_or_else(|| "Choose a model".into())
}

/// The chip: model · effort · mode, opening the library; then Fast where the engine offers it.
pub fn trigger(root: &mut Root, state: &WorkspaceState, engine: AgentEngine, model: Option<&str>, width: f32, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let catalog = state.catalog();
    let choice = choice(state, root.composer.thread(state), engine, model);
    let label = model_name(&catalog, &choice);
    let (steps, effort) = effort_steps(&catalog, &choice);
    let shown = steps.iter().find(|spec| spec.id == effort);
    let effort_label = (!steps.is_empty()).then(|| shown.map(|spec| spec.label.clone()).unwrap_or_else(|| effort.as_str().to_string())).filter(|_| width > 400.0);
    let delegates = shown.is_some_and(|spec| spec.delegates);
    let danger = choice.policy == ExecutionPolicy::Bypass;
    let open = root.library.as_ref().is_some_and(|library| library.target == LibraryTarget::Composer(root.composer.task.clone()));
    let probe = root.composer.settings_probe.clone();
    let lit = open || root.composer.chip_hover;
    let detail_color = if lit { t.ink_faint } else { t.muted };
    let mut detail = div().flex().items_center().gap(px(6.0)).text_color(detail_color).whitespace_nowrap();
    detail = detail.child(div().text_color(t.quiet_soft).child("·"));
    if let Some(effort) = effort_label {
        detail = detail.child(div().text_color(if delegates { t.accent_soft } else { detail_color }).child(effort)).child(div().text_color(t.quiet_soft).child("·"));
    }
    let mode_color = if danger { t.danger_soft } else { detail_color };
    detail = detail.child(div().flex().items_center().gap(px(4.0)).text_color(mode_color).child(icon(mode_glyph(choice.policy), 12.0, mode_color)).child(choice.policy.label()));
    let fast = capabilities_for(engine).fast_mode;
    let fast_on = choice.fast_mode;
    let fast_task = choice.task_id.clone();
    let compact = width <= 470.0;
    let chip_focus = root.composer.chip_focus.get_or_insert_with(|| cx.focus_handle().tab_stop(true)).clone();
    div()
        .flex()
        .items_center()
        .gap(px(if width <= 300.0 { 0.0 } else { 4.0 }))
        .min_w_0()
        .child(
            div()
                .id("model-menu")
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Summary, spoken_setting(&label, &steps, shown.map(|spec| spec.label.as_str()).unwrap_or(effort.as_str()), choice.policy.label())).expanded(open))
                .track_focus(&chip_focus)
                .focus_ring(true)
                .relative()
                .flex_shrink()
                .min_w_0()
                .min_h(px(30.0))
                .flex()
                .items_center()
                .gap(px(6.0))
                .border_1()
                .border_color(gpui::transparent_black())
                .px(px(if width <= 300.0 { 6.0 } else { 8.0 }))
                .rounded(px(8.0))
                .text_size(px(m.text_control))
                .line_height(px(20.0))
                .text_color(if lit { t.ink_soft } else { t.ink_faint })
                .cursor_pointer()
                .when(lit, |trigger| trigger.bg(t.surface_hover))
                .child(div().id("model-menu-label").min_w_0().ellipsis().child(label.clone()).tip_overflow(label))
                .child(detail.flex_none())
                .child(super::common::turning("chevron", if open { 180.0 } else { 0.0 }, 140).child(icon(IconName::ChevronDown, 14.0, t.quiet)))
                .child(bounds_probe(&probe))
                .on_hover(root.composer_listener(cx, |root, hovered: &bool, _, cx| {
                    root.composer.chip_hover = *hovered;
                    cx.notify();
                }))
                .on_click(root.composer_listener(cx, |root, _, window, cx| root.toggle_library(window, cx))),
        )
        .when(fast, |row| {
            let lit_color = if fast_on { t.accent_soft } else { t.muted };
            row.child(
                div()
                    .id("fast-mode")
                    .accessible(gpui::AccessInfo::new(gpui::AccessRole::ToggleButton, "Fast mode").toggled(fast_on))
                    .flex_none()
                    .min_h(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(4.0))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .map(|button| if compact { button.w(px(30.0)) } else { button.pl(px(8.0)).pr(px(10.0)) })
                    .rounded_full()
                    .text_size(px(m.text_meta))
                    .font_weight(FontWeight(560.0))
                    .line_height(gpui::relative(1.2))
                    .cursor_pointer()
                    .transition(act_ui::style::touch())
                    .text_color(lit_color)
                    .map(|button| if fast_on { rimmed(button, &t, 15.0).rounded_full().bg(t.accent_surface) } else { button.hover(|style| style.bg(t.surface_hover).text_color(t.ink_dim)) })
                    .child(if fast_on { super::glyphs::zap(14.0, lit_color).into_any_element() } else { ink_icon(IconName::Zap, 14.0).into_any_element() })
                    .when(!compact, |button| button.child("Fast"))
                    .tip(format!("Fast mode {} · faster responses use more of your plan", if fast_on { "on" } else { "off" }))
                    .on_click(move |_, _, cx| dispatch(AppCommand::TaskSetFastMode { task_id: fast_task.clone(), fast_mode: !fast_on }, cx)),
            )
        })
}

/// The rows a pick may land on, in the order the arrows walk them.
fn pickable(choice: &Choice, state: &WorkspaceState, library: &LibraryState, cx: &gpui::App) -> Vec<(AgentEngine, String)> {
    let access: HashMap<AgentEngine, EngineReadiness> = AGENT_ENGINES.iter().map(|engine| (*engine, state.engine_readiness(*engine))).collect();
    let groups = available(&state.catalog(), &access, choice.engine, choice.locked);
    let query = library.search.read(cx).text().trim().to_lowercase();
    shown_groups(&groups, &state.favorite_models, &library.filter, &query).into_iter().filter(|(group, _)| group.selectable).flat_map(|(group, models)| models.into_iter().map(move |spec| (group.heading.engine, spec.id.clone()))).collect()
}

fn shown_groups<'a>(groups: &'a [Group], favorites: &[EngineModel], filter: &str, query: &str) -> Vec<(&'a Group, Vec<&'a ModelSpec>)> {
    groups
        .iter()
        .filter(|group| !query.is_empty() || filter == "all" || filter == "favorites" || filter == group.heading.key)
        .map(|group| {
            let models = group
                .heading
                .models
                .iter()
                .filter(|spec| if !query.is_empty() { format!("{} {} {} {}", group.heading.label, spec.label, spec.id, spec.description).to_lowercase().contains(query) } else { filter != "favorites" || is_favorite(favorites, group.heading.engine, &spec.id) })
                .collect::<Vec<_>>();
            (group, models)
        })
        .filter(|(_, models)| !models.is_empty() || (query.is_empty() && filter != "favorites"))
        .collect()
}

impl Root {
    pub fn toggle_library(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        self.toggle_library_for(LibraryTarget::Composer(self.composer.task.clone()), window, cx);
    }

    pub fn library_open(&self, target: &LibraryTarget) -> bool {
        self.library.as_ref().is_some_and(|library| &library.target == target)
    }

    /// Opens the library for `target`, or closes it when it is already open there.
    pub fn toggle_library_for(&mut self, target: LibraryTarget, window: &mut Window, cx: &mut Context<Root>) {
        if self.library.as_ref().is_some_and(|library| library.target == target) {
            self.close_library(window, cx);
            return;
        }
        let state = self.state(cx);
        let choice = match &target {
            LibraryTarget::Overview(lead) => overview_choice(&state, lead),
            LibraryTarget::Composer(_) => Some(current_choice(&self.composer, &state)),
        };
        let Some(choice) = choice else { return };
        if !choice.locked {
            dispatch(AppCommand::EngineRead { refresh: None }, cx);
        }
        let catalog = state.catalog();
        let filter = if choice.locked {
            let own: Vec<Heading> = headings(&catalog).into_iter().filter(|heading| heading.engine == choice.engine).collect();
            own.iter().find(|heading| heading.models.iter().any(|spec| Some(&spec.id) == choice.model.as_ref())).or(own.first()).map(|heading| heading.key.clone()).unwrap_or_else(|| "all".into())
        } else if !state.favorite_models.is_empty() {
            "favorites".into()
        } else {
            "all".into()
        };
        let search = cx.new(|cx| TextInput::new(cx).placeholder("Find a model or provider…"));
        let scope = target.clone();
        let subscription = cx.subscribe_in(&search, window, move |root, _, event: &InputEvent, window, cx| {
            root.in_library_target(&scope, |root| match event {
                InputEvent::Changed => {
                    if let Some(library) = &mut root.library {
                        library.highlight = None;
                        library.hovered = None;
                    }
                    cx.notify();
                }
                InputEvent::Submit => root.pick_from_library(window, cx),
                _ => {}
            });
        });
        search.update(cx, |search, _| search.focus(window));
        let effort_focus = cx.focus_handle().tab_stop(true);
        self.library = Some(LibraryState { target, search, filter, highlight: None, hovered: None, drag: None, slider_hover: false, rail: bounds_cell(), effort_focus, _subscription: subscription });
        cx.notify();
    }

    /// Runs `f` with the composer the library belongs to in place.
    fn in_library_target<R>(&mut self, target: &LibraryTarget, f: impl FnOnce(&mut Root) -> R) -> Option<R> {
        match target {
            LibraryTarget::Composer(task) => self.in_composer(task.as_deref(), f),
            LibraryTarget::Overview(_) => Some(f(self)),
        }
    }

    /// Closing hands the keys back to the chip that opened it.
    fn close_library(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        if let Some(library) = self.library.take() {
            if matches!(library.target, LibraryTarget::Composer(_)) {
                match &self.composer.chip_focus {
                    Some(chip) => window.focus(chip),
                    None => self.composer.input.update(cx, |input, _| input.focus(window)),
                }
            }
        }
        cx.notify();
    }

    /// Enter: the highlighted model, which leaves the library open to fit its effort; else the first
    /// one shown, which closes it.
    fn pick_from_library(&mut self, window: &mut Window, cx: &mut Context<Root>) {
        let state = self.state(cx);
        let Some(choice) = library_choice(self, &state) else { return };
        let Some(library) = &self.library else { return };
        let rows = pickable(&choice, &state, library, cx);
        match library.highlight.and_then(|index| rows.get(index)) {
            Some((engine, model)) => {
                dispatch(choice.pick(*engine, model.clone()), cx);
                if !choice.has_effort {
                    self.close_library(window, cx);
                }
            }
            None => {
                if let Some((engine, model)) = rows.first() {
                    dispatch(choice.pick(*engine, model.clone()), cx);
                }
                self.close_library(window, cx);
            }
        }
    }

    fn library_arrow(&mut self, step: i64, cx: &mut Context<Root>) {
        let state = self.state(cx);
        let Some(choice) = library_choice(self, &state) else { return };
        let Some(library) = &self.library else { return };
        let count = pickable(&choice, &state, library, cx).len() as i64;
        if count == 0 {
            return;
        }
        let next = match library.highlight {
            None => {
                if step > 0 {
                    0
                } else {
                    count - 1
                }
            }
            Some(index) => (index as i64 + step).rem_euclid(count),
        };
        if let Some(library) = &mut self.library {
            library.highlight = Some(next as usize);
        }
        cx.notify();
    }

    /// ← and → step the effort from anywhere in the library but a search being typed.
    fn library_effort_step(&mut self, step: i64, cx: &mut Context<Root>) -> bool {
        let Some(library) = &self.library else { return false };
        if !library.search.read(cx).text().is_empty() {
            return false;
        }
        self.library_effort_slide(step, cx)
    }

    /// Moves the effort one stop along, saying whether there is an effort to move.
    fn library_effort_slide(&mut self, step: i64, cx: &mut Context<Root>) -> bool {
        let state = self.state(cx);
        let Some(choice) = library_choice(self, &state).filter(|choice| choice.has_effort) else { return false };
        let (steps, value) = effort_steps(&state.catalog(), &choice);
        let Some(index) = steps.iter().position(|spec| spec.id == value) else { return !steps.is_empty() };
        let next = index as i64 + step;
        if next >= 0 && (next as usize) < steps.len() {
            dispatch(choice.set_effort(steps[next as usize].id), cx);
        }
        true
    }

    fn effort_drag(&mut self, x: gpui::Pixels, release: bool, cx: &mut Context<Root>) {
        let Some(library) = &mut self.library else { return };
        let Some(bounds) = library.rail.get() else { return };
        let travel = (f32::from(bounds.size.width) - KNOB).max(1.0);
        let fraction = ((f32::from(x - bounds.left()) - KNOB / 2.0) / travel).clamp(0.0, 1.0);
        if !release {
            library.drag = Some(fraction);
            cx.notify();
            return;
        }
        // Letting go settles where the knob was last dragged to, as the pointer-up carries no move of its own.
        let Some(fraction) = library.drag.take() else { return };
        let state = self.state(cx);
        let Some(choice) = library_choice(self, &state) else { return };
        let (steps, value) = effort_steps(&state.catalog(), &choice);
        if steps.len() > 1 {
            let nearest = (fraction * (steps.len() - 1) as f32).round() as usize;
            if let Some(spec) = steps.get(nearest).filter(|spec| spec.id != value) {
                dispatch(choice.set_effort(spec.id), cx);
            }
        }
        cx.notify();
    }
}

/// What is wrong with an engine, said as Settings says it, with its fix to copy and a way there.
fn readiness_hint(notice: &EngineNotice, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let mut hint = div().flex().flex_wrap().items_center().gap(px(6.0)).w_full().px(px(9.0)).pt(px(2.0)).pb(px(8.0)).text_size(px(m.text_meta)).text_color(t.quiet_strong).child(notice.message.clone());
    if let Some(fix) = notice.fix.clone().filter(|_| !notice.updatable) {
        let copy = fix.clone();
        hint = hint.child(IconButton::new(SharedString::from(format!("copy-fix-{fix}")), IconName::Copy, format!("Copy {fix}")).tip("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))));
    }
    hint.child(
        div()
            .id("readiness-settings")
            .text_color(t.link)
            .cursor_pointer()
            .hover(|style| style.underline())
            .child("Settings")
            .on_click(cx.listener(|root, _, window, cx| {
                root.close_library(window, cx);
                send(json!({ "type": "view.set-settings-open", "open": true, "section": "engines" }), cx);
            })),
    )
    .into_any_element()
}

/// The library, drawn over the window against its chip.
pub fn popover(root: &mut Root, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let chip_drawn = std::mem::take(&mut root.dock.overview.model_drawn);
    let target = root.library.as_ref()?.target.clone();
    let anchor = match &target {
        // The library goes with the Overview's chip.
        LibraryTarget::Overview(_) => match root.dock.overview.model_probe.as_ref().and_then(|probe| probe.get()).filter(|_| chip_drawn && library_choice(root, state).is_some()) {
            Some(anchor) => Some(anchor),
            None => {
                root.library = None;
                return None;
            }
        },
        LibraryTarget::Composer(_) => None,
    };
    root.in_library_target(&target, |root| scoped_popover(root, state, anchor, window, cx)).flatten()
}

fn scoped_popover(root: &mut Root, state: &WorkspaceState, anchor: Option<gpui::Bounds<gpui::Pixels>>, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    let library = root.library.as_ref()?;
    // The chip's probe sits inside its 1px border.
    let anchor = anchor.or_else(|| root.composer.settings_probe.get().map(|bounds| bounds.dilate(px(1.0))))?;
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    let m = &theme.metrics;
    let choice = library_choice(root, state)?;
    let catalog = state.catalog();
    let access: HashMap<AgentEngine, EngineReadiness> = AGENT_ENGINES.iter().map(|engine| (*engine, state.engine_readiness(*engine))).collect();
    let groups = available(&catalog, &access, choice.engine, choice.locked);
    let favorites: &[EngineModel] = &state.favorite_models;
    let query = library.search.read(cx).text().trim().to_lowercase();
    let search_focused = library.search.read(cx).is_focused(window);
    let filter = library.filter.clone();
    let highlight = library.highlight;
    let hovered = library.hovered;
    let pinned = |group: &Group, spec: &ModelSpec| is_favorite(favorites, group.heading.engine, &spec.id);
    let total: usize = groups.iter().map(|group| group.heading.models.len()).sum();
    let favorite_count: usize = groups.iter().map(|group| group.heading.models.iter().filter(|spec| pinned(group, spec)).count()).sum();
    let shown = shown_groups(&groups, favorites, &filter, &query);
    let count: usize = shown.iter().map(|(_, models)| models.len()).sum();
    let searching = !query.is_empty();
    let heading = if searching {
        "Search results".to_string()
    } else if filter == "favorites" {
        "Favorites".into()
    } else if filter == "all" {
        "All models".into()
    } else {
        groups.iter().find(|group| group.heading.key == filter).map(|group| group.heading.label.clone()).unwrap_or(filter.clone())
    };
    let viewport = window.viewport_size();
    let width = LIBRARY_WIDTH.min(f32::from(viewport.width) - 16.0);
    let narrow = width <= 520.0;
    let tiny = width <= 400.0;

    let rail_item = |id: String, label: String, glyph: IconName, count: usize, selected: bool, filter: String, cx: &mut Context<Root>| {
        let t = cx.theme().tokens.clone();
        let m = cx.theme().metrics.clone();
        let base = div().relative().flex().flex_none().items_center().gap(px(9.0)).px(px(if narrow { 10.0 } else { 9.0 })).py(px(if narrow { 7.0 } else { 6.0 })).rounded(px(7.0)).text_size(px(m.text_menu)).cursor_pointer();
        let base = if selected { base.bg(t.surface_active).rim(t.glass_rim, Corners::Radius(7.0)).text_color(t.ink).font_weight(FontWeight(560.0)) } else { base.text_color(t.muted).hover(|style| style.bg(t.wash)) };
        base.id(SharedString::from(format!("rail-{id}")))
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "").toggled(selected))
            .child(icon(glyph, 16.0, if selected { t.ink } else { t.muted }))
            .child(div().when(!narrow, |label| label.flex_1()).child(label))
            .child(div().text_size(px(m.text_meta)).text_color(t.quiet).tabular_nums().child(count.to_string()))
            .on_click(root.composer_listener(cx, move |root, _, _, cx| {
                if let Some(library) = &mut root.library {
                    library.filter = filter.clone();
                    library.highlight = None;
                    library.hovered = None;
                    library.search.update(cx, |search, cx| search.reset("", cx));
                }
                cx.notify();
            }))
    };
    let mut rail = div().id("model-rail").accessible(gpui::AccessInfo::new(gpui::AccessRole::Navigation, "Filter models")).flex().gap(px(if narrow { 2.0 } else { 1.0 })).p(px(if narrow { 8.0 } else { 6.0 }));
    rail = if narrow { rail.flex_row().overflow_x_scroll().border_b_1().border_color(t.line_soft) } else { rail.w(px(164.0)).flex_none().flex_col().overflow_y_scroll().border_r_1().border_color(t.line_soft) };
    rail = rail
        .child(rail_item("favorites".into(), "Favorites".into(), IconName::Star, favorite_count, !searching && filter == "favorites", "favorites".into(), cx))
        .child(rail_item("all".into(), "All models".into(), IconName::Grid2X2, total, !searching && filter == "all", "all".into(), cx))
        .child(if narrow { div().flex_none().w(px(1.0)).my(px(6.0)).mx(px(4.0)).bg(t.line_soft) } else { div().flex_none().h(px(1.0)).mx(px(9.0)).my(px(5.0)).bg(t.line_soft) });
    for group in &groups {
        let glyph = if group.heading.key == group.heading.engine.as_str() { engine_icon(group.heading.engine) } else { IconName::Sparkles };
        rail = rail.child(rail_item(group.heading.key.clone(), group.heading.label.clone(), glyph, group.heading.models.len(), !searching && filter == group.heading.key, group.heading.key.clone(), cx));
    }

    let mut results = div().id("model-results").flex_1().min_w_0().min_h_0().flex().flex_col().px(px(8.0)).py(px(if narrow { 10.0 } else { 6.0 })).overflow_y_scroll();
    results = results.child(div().flex().justify_between().gap(px(10.0)).px(px(10.0)).pt(px(5.0)).pb(px(6.0)).text_size(px(m.text_meta)).text_color(t.quiet_strong).child(heading).child(div().text_color(t.quiet_soft).child(format!("{count} models"))));
    let several = shown.len() > 1;
    let mut pick_index = 0usize;
    // A model's name and what it is for share a baseline.
    let [name_shift, description_shift] = baseline_shifts([(m.text_control, None), (m.text_meta, Some(m.text_meta * 1.35))], window);
    for (group, models) in &shown {
        if several {
            results = results.child(div().px(px(10.0)).pt(px(10.0)).pb(px(4.0)).text_color(t.quiet).text_size(px(m.text_badge)).font_weight(FontWeight(560.0)).child(group.heading.label.clone()));
        }
        for spec in models {
            let selected = group.heading.engine == choice.engine && Some(&spec.id) == choice.model.as_ref();
            let favorite = is_favorite(favorites, group.heading.engine, &spec.id);
            let pick = choice.pick(group.heading.engine, spec.id.clone());
            let closes = !choice.has_effort;
            let selectable = group.selectable;
            let highlighted = selectable && highlight == Some(pick_index);
            let index = pick_index;
            let lit = highlighted || (selectable && hovered == Some(index));
            if selectable {
                pick_index += 1;
            }
            let star_engine = group.heading.engine;
            let star_model = spec.id.clone();
            let mut row = div()
                .id(SharedString::from(format!("model-{}-{}", group.heading.engine.as_str(), spec.id)))
                .accessible(gpui::AccessInfo::new(gpui::AccessRole::Button, "").selected(selected).active(highlighted && search_focused).disabled(!selectable))
                .relative()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(10.0))
                .min_h(px(34.0))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(8.0))
                .text_size(px(m.text_control))
                .child(div().flex_none().w(px(14.0)).child(if selected { icon(IconName::Check, 14.0, t.ink).into_any_element() } else { div().into_any_element() }))
                .when(!tiny, |row| row.child(icon(model_glyph(group.heading.engine, &spec.id), 16.0, if lit { t.ink } else { t.quiet_strong })))
                .child(div().flex_none().relative().top(px(name_shift)).text_color(if selected || lit { t.ink } else { t.ink_dim }).font_weight(FontWeight(500.0)).child(spec.label.clone()))
                .child(div().min_w_0().relative().top(px(description_shift)).ellipsis().text_size(px(m.text_meta)).line_height(gpui::relative(1.35)).text_color(if lit { t.ink } else { t.quiet_strong }).id(SharedString::from(format!("model-description-{}-{}", group.heading.engine.as_str(), spec.id))).child(spec.description.clone()).tip_overflow(spec.description.clone()));
            if !selectable {
                row = row.opacity(0.5);
            } else {
                if lit {
                    row = row.pane(t.glass_pick).rim(t.glass_rim, Corners::Radius(8.0));
                }
                row = row.cursor_pointer().on_hover(cx.listener(move |root, hovered: &bool, _, cx| {
                    let Some(library) = &mut root.library else { return };
                    let next = if *hovered { Some(index) } else { library.hovered.filter(|at| *at != index) };
                    if library.hovered != next {
                        library.hovered = next;
                        cx.notify();
                    }
                }));
            }
            row = row.on_click(root.composer_listener(cx, move |root, _, window, cx| {
                if selectable {
                    dispatch(pick.clone(), cx);
                    if closes {
                        root.close_library(window, cx);
                    }
                }
            }));
            let star = IconButton::new(SharedString::from(format!("star-{}-{}", group.heading.engine.as_str(), spec.id)), IconName::Star, format!("{} {}", if favorite { "Unpin" } else { "Pin" }, spec.label))
                .tip(if favorite { "Unpin from Favorites" } else { "Pin to Favorites" })
                .glyph(if favorite { super::glyphs::star(16.0, t.accent_text).into_any_element() } else { icon(IconName::Star, 16.0, t.quiet).into_any_element() })
                .on_click(move |_, _, cx| dispatch(AppCommand::SetModelFavorite { engine: star_engine, model: star_model.clone(), favorite: !favorite }, cx));
            results = results.child(div().flex().items_center().gap(px(2.0)).child(row).child(star));
        }
        match &group.hint {
            Some(Hint::SignIn) => {
                let engine = group.heading.engine;
                let label = group.heading.label.clone();
                results = results.child(
                    div()
                        .id(SharedString::from(format!("sign-in-{}", group.heading.key)))
                        .px(px(9.0))
                        .pt(px(2.0))
                        .pb(px(8.0))
                        .text_size(px(m.text_meta))
                        .text_color(t.link)
                        .cursor_pointer()
                        .hover(|style| style.underline())
                        .child(format!("Sign in to use {label}"))
                        .on_click(root.composer_listener(cx, move |root, _, window, cx| {
                            dispatch(AppCommand::EngineSignIn { engine }, cx);
                            root.close_library(window, cx);
                        })),
                );
            }
            Some(Hint::Notice(notice)) => results = results.child(readiness_hint(notice, cx)),
            None => {}
        }
    }
    if count == 0 {
        let empty = if searching { "No models found. Try another name or provider." } else if filter == "favorites" { "Pin models from All models to keep them here." } else { "No models available." };
        results = results.child(div().flex_none().my(px(12.0)).px(px(8.0)).py(px(6.0)).text_size(px(m.text_meta)).line_height(relative(1.45)).text_color(t.quiet).child(empty));
    }

    let model_name = model_name(&catalog, &choice);
    let (steps, effort) = effort_steps(&catalog, &choice);
    // The setting's name and its detail share a baseline.
    let axis = |name: &'static str, detail: Option<String>, line_height: Option<f32>| {
        let [name_top, detail_top] = baseline_group([(m.text_menu, line_height), (m.text_meta, line_height)], window).0;
        let mut axis = div().flex().items_start().gap(px(5.0)).min_w_0().text_color(t.ink_soft).text_size(px(m.text_menu)).font_weight(FontWeight(500.0)).when_some(line_height, |axis, height| axis.line_height(px(height)));
        axis = if narrow { axis } else { axis.w(px(164.0)).flex_none().pl(px(15.0)) };
        if let Some(detail) = detail {
            axis = axis.child(div().flex_none().mt(px(name_top)).child(name)).child(div().min_w_0().mt(px(detail_top)).text_size(px(m.text_meta)).text_color(t.quiet).font_weight(FontWeight(400.0)).ellipsis().child(format!("· {detail}")));
        } else {
            axis = axis.child(name);
        }
        axis
    };
    let setting = || if narrow { div().flex().flex_col().gap(px(4.0)).px(px(16.0)).py(px(10.0)) } else { div().flex().pt(px(8.0)).pr(px(8.0)).pb(px(8.0)) };
    let effort_body = if steps.is_empty() {
        if choice.model.is_some() {
            div().py(px(6.0)).text_size(px(m.text_meta)).text_color(t.quiet).child(format!("{model_name} has no effort setting")).into_any_element()
        } else {
            div().into_any_element()
        }
    } else {
        effort_slider(root, &steps, effort, tiny, window, cx).into_any_element()
    };
    let effort_row = setting().items_start().child(axis("Effort", Some(model_name.clone()), Some(22.0))).child(div().flex_1().min_w_0().when(!narrow, |body| body.ml(px(8.0))).child(effort_body));
    let policy = choice.policy;
    let options: Vec<SegmentedOption> = offered_policies(choice.engine)
        .into_iter()
        .map(|option| {
            let mut segment = SegmentedOption::new(option.as_str(), option.label()).tip(option.description());
            if !tiny {
                segment = segment.icon(mode_glyph(option));
            }
            if option == ExecutionPolicy::Bypass {
                segment = segment.tone(Tone::Danger);
            }
            segment
        })
        .collect();
    let task_id = choice.task_id.clone();
    let mode_row = setting().items_center().border_t_1().border_color(t.line_soft).child(axis("Mode", None, None)).child(
        div().flex_1().min_w_0().when(!narrow, |body| body.ml(px(8.0))).child(Segmented::new("library-modes", options, policy.as_str()).fill().on_change(move |value, _, cx| {
            send(json!({ "type": "task.set-policy", "taskId": task_id, "policy": value.to_string() }), cx);
        })),
    );
    let settings = if choice.has_effort || choice.has_mode {
        div().flex_none().flex().flex_col().border_t_1().border_color(t.line_soft).when(choice.has_effort, |settings| settings.child(effort_row)).when(choice.has_mode, |settings| settings.child(mode_row))
    } else {
        div()
            .flex_none()
            .flex()
            .justify_between()
            .gap(px(12.0))
            .px(px(16.0))
            .py(px(10.0))
            .border_t_1()
            .border_color(t.line_soft)
            .text_color(t.quiet)
            .text_size(px(m.text_badge))
            .child(if choice.locked { format!("This thread uses {}.", engine_label(choice.engine)) } else { "↑ ↓ navigate · Enter select".into() })
            .when(!narrow, |footer| footer.child(div().flex().items_center().gap(px(6.0)).child(icon(IconName::Star, 12.0, t.quiet)).child("Pin your go-to models")))
    };

    let search = div()
        .relative()
        .flex_none()
        .child(SearchField::new("model-search", &library.search).variant(SearchVariant::Library))
        .child(div().absolute().top(px(4.0)).right(px(8.0)).child(IconButton::new("close-library", IconName::X, "Close model picker").on_click(root.composer_listener(cx, |root, _, window, cx| root.close_library(window, cx)))));
    let body = if narrow { div().flex().flex_col() } else { div().flex() }.flex_1().min_h(px(120.0)).max_h(px(480.0)).child(rail).child(results);
    let placement = Placement::Anchor { anchor, align: Align::Start };
    let room = f32::from(placement.room(viewport));
    let mut card = glass(div(), t, GlassKind::Popover, Some(13.0), None);
    if t.glass_blur {
        // `.model-library` is a thicker pane than other menus, so the composer under it barely shows.
        card = card.bg(act_ui::theme::Pane { fill: act_ui::color::alpha(t.glass_menu.fill, 0.9 + 0.06 * t.glass_tint), sheen: true }.background().tiled_from_padding_box());
    }
    let card = card
        .id("model-library")
        .map(act_ui::style::glass_in)
        .occlude()
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Dialog, "Choose model, effort and mode"))
        .w(px(width))
        .min_h(px(room.min(340.0)))
        .max_h(px(room.min(720.0)))
        .overflow_hidden()
        .flex()
        .flex_col()
        .on_mouse_down_out(root.composer_listener(cx, move |root, event: &gpui::MouseDownEvent, _, cx| {
            // A press on the chip is its own toggle.
            if root.library.as_ref().is_some_and(|library| library.drag.is_none()) && !anchor.contains(&event.position) {
                root.library = None;
                cx.notify();
            }
        }))
        .capture_action(root.composer_listener(cx, |root, _: &text_input::Left, _, cx| {
            if root.library_effort_step(-1, cx) {
                cx.stop_propagation();
            }
        }))
        .capture_action(root.composer_listener(cx, |root, _: &text_input::Right, _, cx| {
            if root.library_effort_step(1, cx) {
                cx.stop_propagation();
            }
        }))
        .on_key_down(root.composer_listener(cx, |root, event: &KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
            "escape" => {
                act_ui::focus::note_keystroke(&event.keystroke, window, cx);
                root.close_library(window, cx);
                cx.stop_propagation();
            }
            "down" => {
                root.library_arrow(1, cx);
                cx.stop_propagation();
            }
            "up" => {
                root.library_arrow(-1, cx);
                cx.stop_propagation();
            }
            _ => {}
        }))
        .child(search)
        .child(body)
        .child(settings);
    Some(deferred(placed(placement, card)).with_priority(1).into_any_element())
}

/// The efforts on a track the knob slides along while dragged, settling on the nearest effort when
/// let go; a press anywhere on the slider, names included, starts the drag. Each effort is named
/// under its stop.
fn effort_slider(root: &Root, steps: &[EffortSpec], value: AgentEffort, tiny: bool, window: &Window, cx: &mut Context<Root>) -> impl IntoElement {
    let t = cx.theme().tokens.clone();
    let m = cx.theme().metrics.clone();
    let Some(library) = &root.library else { return div() };
    let last = steps.len().saturating_sub(1);
    let index = steps.iter().position(|spec| spec.id == value).unwrap_or(0);
    let at = |step: usize| if last == 0 { 0.0 } else { step as f32 / last as f32 };
    let drag = library.drag;
    let shown = drag.map(|fraction| (fraction * last as f32).round() as usize).unwrap_or(index);
    let delegates = steps.get(shown).is_some_and(|spec| spec.delegates);
    let fraction = drag.unwrap_or(at(index));
    let swell = if drag.is_some() { 1.28 } else if library.slider_hover { 1.1 } else { 1.0 };
    let rail_probe = library.rail.clone();
    let focus = library.effort_focus.clone();
    let ringed = focus.is_focused(window) && act_ui::focus::keyboard_focus_visible(cx);
    // The stops sit where the knob's centre travels: half a knob in from each end.
    let place = move |fraction: f32| relative(fraction);
    let mut rail = div().relative().h(px(4.0)).rounded_full().bg(super::common::with_alpha(t.ink, 0.13)).child(bounds_probe(&rail_probe));
    for step in 0..steps.len() {
        rail = rail.child(div().absolute().top(px(1.0)).left(place(at(step))).ml(px(KNOB / 2.0 - 1.0 - KNOB * at(step))).size(px(2.0)).rounded_full().bg(t.quiet));
    }
    rail = rail
        .child(div().absolute().top_0().bottom_0().left_0().w(place(fraction)).mr(px(0.0)).min_w(px(KNOB / 2.0)).rounded_full().bg(if delegates { gpui::solid_background(t.accent) } else { act_ui::theme::Pane::sheened(t.ink_faint).background() }))
        .child({
            // The knob slides between stops (`transition: left 180ms`) unless dragged, and swells
            // about its centre (`transform: scale()`), each eased on its own.
            let travel = rail_probe.get().map(|rail| rail.size.width - px(KNOB));
            let glide = gpui::Transition::cubic(180, 0.2, 0.8, 0.2, 1.0);
            let face = div()
                .id("effort-knob-face")
                .size_full()
                .rounded_full()
                .transition(glide.only(&[gpui::TransitionProperty::Transform]).and(gpui::Transition::ease(160).only(&[gpui::TransitionProperty::Background])))
                .transform(gpui::ElementTransform::scale(swell))
                .bg(if drag.is_some() {
                    gpui::solid_background(super::common::with_alpha(t.surface_inverse, 0.3))
                } else {
                    gpui::linear_gradient(180.0, gpui::linear_color_stop(white(0.28), 0.0), gpui::linear_color_stop(white(0.0), 1.0)).over_color(t.surface_inverse)
                })
                .when(drag.is_some(), |knob| knob.backdrop_filter(gpui::BackdropFilter::blur(px(1.5)).saturate(1.9).brightness(1.12)))
                .shadow(if ringed {
                    let mut shadows = vec![gpui::BoxShadow { color: t.focus_ring.into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(2.0) }];
                    shadows.extend(t.shadow_thumb.box_shadows());
                    shadows
                } else {
                    knob_shadows(&t.shadow_thumb)
                })
                // The focus ring's shadow list replaces the knob's own, inset light included.
                .when(!ringed, |knob| knob.child(knob_light()));
            let knob = div().id("effort-knob").absolute().top(px(-7.0)).size(px(KNOB));
            match travel {
                Some(travel) => knob
                    .left_0()
                    .when(drag.is_none(), |knob| knob.transition(glide.only(&[gpui::TransitionProperty::Transform])))
                    .transform(gpui::ElementTransform::translate(travel * fraction, px(0.0))),
                None => knob.left(place(fraction)).ml(px(-KNOB * fraction)),
            }
            .child(face)
        });
    let mut stops = div().relative().h(px(16.0)).mt(px(9.0));
    let count = steps.len();
    for (step, spec) in steps.iter().enumerate() {
        if tiny && step != shown {
            continue;
        }
        let current = step == shown;
        let stop = div()
            .id(SharedString::from(format!("effort-{step}")))
            .skip_tab_stop()
            .absolute()
            .text_size(px(m.text_meta))
            .line_height(px(16.0))
            .whitespace_nowrap()
            .transition(gpui::Transition::ease(140))
            .text_color(if current { if delegates { t.accent_bright } else { t.ink } } else { t.quiet })
            .when(current, |stop| stop.font_weight(FontWeight(560.0)))
            .hover(|style| style.text_color(t.ink_soft))
            .child(spec.label.clone())
            .when_some(spec.description.clone(), |stop, description| stop.tip(description));
        let stop = if step == 0 {
            stop.left_0()
        } else if step + 1 == count {
            stop.right_0()
        } else {
            // Centred on its stop (translateX(-50%)).
            let half = super::text_fit::line_width(&spec.label, m.text_meta, window) / 2.0;
            stop.left(place(at(step))).ml(px(KNOB / 2.0 - KNOB * at(step) - half))
        };
        stops = stops.child(div().absolute().inset_0().child(stop));
    }
    let dragging = drag.is_some();
    let track = div()
        .id("effort-slider")
        .track_focus(&focus)
        .accessible(gpui::AccessInfo::new(gpui::AccessRole::Slider, "Effort").value(steps.get(shown).map(|spec| spec.label.clone()).unwrap_or_default()))
        // Left and Right step the effort, as the page's slider does.
        .on_key_down(root.composer_listener(cx, |root, event: &KeyDownEvent, _, cx| {
            let step = match event.keystroke.key.as_str() {
                "left" => -1,
                "right" => 1,
                _ => return,
            };
            cx.stop_propagation();
            root.library_effort_slide(step, cx);
        }))
        .pt(px(9.0))
        .cursor(if dragging { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand })
        .on_hover(root.composer_listener(cx, |root, hovered: &bool, _, cx| {
            if let Some(library) = root.library.as_mut().filter(|library| library.slider_hover != *hovered) {
                library.slider_hover = *hovered;
                cx.notify();
            }
        }))
        .on_mouse_down(
            MouseButton::Left,
            root.composer_listener(cx, move |root, event: &gpui::MouseDownEvent, _, cx| {
                if last > 0 {
                    root.effort_drag(event.position.x, false, cx);
                }
            }),
        )
        // A click whose release lands before the drag's window listeners are drawn still settles.
        .on_mouse_up(MouseButton::Left, root.composer_listener(cx, |root, event: &MouseUpEvent, _, cx| root.effort_drag(event.position.x, true, cx)))
        .child(rail)
        .child(stops);
    let mut slider = div().child(track);
    if dragging {
        slider = slider.child(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    window.on_mouse_event(move |event: &MouseMoveEvent, _, window, cx| {
                        let root = window.root::<Root>().flatten();
                        if let Some(root) = root {
                            root.update(cx, |root, cx| root.effort_drag(event.position.x, false, cx));
                        }
                    });
                    window.on_mouse_event(move |event: &MouseUpEvent, _, window, cx| {
                        let root = window.root::<Root>().flatten();
                        if let Some(root) = root {
                            root.update(cx, |root, cx| root.effort_drag(event.position.x, true, cx));
                        }
                    });
                },
            )
            .absolute()
            .size_0(),
        );
    }
    slider
}

/// `align-items: baseline` for lines of different sizes: each line's top below the group's top so
/// all share one baseline, and the group's height. A line is its text size and line height (None
/// for `normal`), in the window's font.
pub(super) fn baseline_group<const N: usize>(lines: [(f32, Option<f32>); N], window: &Window) -> ([f32; N], f32) {
    let (lines, height) = baseline_lines(lines, window);
    (lines.map(|(top, _)| top), height)
}

/// The same lines each centred in a row instead (`items_center`): how far down each must move to
/// sit where the baseline group would put it.
pub(super) fn baseline_shifts<const N: usize>(lines: [(f32, Option<f32>); N], window: &Window) -> [f32; N] {
    let (lines, height) = baseline_lines(lines, window);
    lines.map(|(top, line)| top + line / 2.0 - height / 2.0)
}

/// Each line's top in its baseline group and its line height, and the group's height.
fn baseline_lines<const N: usize>(lines: [(f32, Option<f32>); N], window: &Window) -> ([(f32, f32); N], f32) {
    let scale = window.scale_factor();
    let style = window.text_style();
    let font_id = window.text_system().resolve_font(&style.font());
    let boxes = lines.map(|(size, line_height)| {
        let mut line = style.clone();
        line.line_height = line_height.map(|height| px(height).into()).unwrap_or_else(gpui::line_height_normal);
        let height = line.resolve_line_height(px(size), window.rem_size(), scale, window.text_system());
        let (ascent, descent) = (window.text_system().ascent(font_id, px(size)), window.text_system().descent(font_id, px(size)));
        (f32::from(gpui::line_baseline(height, ascent, descent, scale)), f32::from(height))
    });
    let above = boxes.iter().map(|(baseline, _)| *baseline).fold(0.0, f32::max);
    let below = boxes.iter().map(|(baseline, height)| height - baseline).fold(0.0, f32::max);
    (boxes.map(|(baseline, height)| (above - baseline, height)), above + below)
}

fn white(alpha: f32) -> gpui::Rgba {
    gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: alpha }
}

/// The knob's outer shadows: a 0.5px hairline at a fixed weight, then `--shadow-thumb`'s layers
/// with the far one deepened from 0.22 to 0.28.
fn knob_shadows(thumb: &act_ui::theme::Elevation) -> Vec<gpui::BoxShadow> {
    let mut shadows = thumb.box_shadows();
    if let [near, far] = shadows.as_mut_slice() {
        let scale = near.color.a / 0.4;
        let hairline = gpui::BoxShadow { color: gpui::Hsla { a: 0.2, ..near.color }, offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(0.5) };
        far.color.a = 0.28 * scale;
        shadows.insert(0, hairline);
    }
    shadows
}

/// The knob's inner light: a 1px white line along its top over a faint white ring.
fn knob_light() -> impl IntoElement {
    let shadows = [
        gpui::BoxShadow { color: white(0.12).into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0) },
        gpui::BoxShadow { color: white(0.5).into(), offset: gpui::point(px(0.0), px(1.0)), blur_radius: px(0.0), spread_radius: px(0.0) },
    ];
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| window.paint_inset_shadows(bounds, gpui::Corners::all(bounds.size.width.min(bounds.size.height) / 2.0), &shadows),
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// How the model chip reads aloud: which settings it shows, then their values (ComposerSettings.tsx).
fn spoken_setting(model: &str, steps: &[EffortSpec], effort: &str, mode: &str) -> String {
    if steps.is_empty() {
        format!("Model and mode: {model}, {mode}")
    } else {
        format!("Model, effort and mode: {model}, {effort}, {mode}")
    }
}
