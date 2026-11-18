//! The window: the sidebar, the workspace column with its page, the dock, and the layers over them
//! (App.tsx).

use act_core::state::{WorkspaceState, THREAD_START_PROJECT_MENU};
use act_core::AppCommand;
use act_ui::components::menu::Menu;
use act_ui::input::{InputEvent, TextInput};
use act_ui::style::PaneFill;
use act_ui::theme::ActiveTheme;
use gpui::{div, prelude::*, px, AnyElement, Context, DismissEvent, Entity, FocusHandle, Focusable, KeyDownEvent, Subscription, Window};

use super::{composer, conversation, dialogs, dock, empty_thread, find_bar, jump, session_panel, settings, sidebar, toasts, topbar};
use crate::workspace::{dispatch, Surface, Workspace};

/// The one menu open in the window, which every opener replaces.
#[derive(Default)]
pub struct Menus {
    open: Option<(String, Entity<Menu>, Subscription)>,
}

impl Menus {
    pub fn show(&mut self, id: &str, menu: Entity<Menu>, cx: &mut Context<Root>) {
        let subscription = cx.subscribe(&menu, |root, _, _: &DismissEvent, cx| {
            root.menus.open = None;
            cx.notify();
        });
        self.open = Some((id.to_string(), menu, subscription));
        cx.notify();
    }

    pub fn is_open(&self, id: &str) -> bool {
        self.open.as_ref().is_some_and(|(open, _, _)| open == id)
    }

    /// The open menu, if it is the one named.
    pub fn entity(&self, id: &str) -> Option<Entity<Menu>> {
        self.open.as_ref().filter(|(open, _, _)| open == id).map(|(_, menu, _)| menu.clone())
    }

    pub fn close(&mut self, cx: &mut Context<Root>) {
        self.open = None;
        cx.notify();
    }

    pub fn any_open(&self) -> bool {
        self.open.is_some()
    }

    fn element(&self) -> Option<AnyElement> {
        self.open.as_ref().map(|(_, menu, _)| menu.clone().into_any_element())
    }
}

pub struct Root {
    pub workspace: Entity<Workspace>,
    pub focus: FocusHandle,
    pub sidebar: sidebar::SidebarState,
    pub topbar: topbar::TopbarState,
    pub menus: Menus,
    pub conversation: conversation::ConversationState,
    pub composer: composer::ComposerState,
    pub dock: dock::DockState,
    pub settings: settings::SettingsState,
    pub dialogs: dialogs::DialogState,
    pub jump: jump::JumpState,
    pub find: find_bar::FindState,
    pub session: session_panel::SessionState,
    pub library: Option<super::model_library::LibraryState>,
    pub pull_requests: super::pull_request_reads::PullRequestReads,
    pub automation: Option<super::automation_panel::AutomationForm>,
    /// The thread whose name is being typed in its sidebar row or heading.
    pub renaming: Option<String>,
    pub renaming_heading: Option<String>,
    pub(super) rename_input: Option<(Entity<TextInput>, Subscription)>,
    rename_invalid: bool,
    pub pending_focus: Option<Entity<TextInput>>,
    pub(super) parts: Option<super::parts::Parts>,
    /// The overlaid state the thread's views draw while a paired computer is on screen, kept per
    /// window state it was made from. Weak, so the workspace can change its state in place.
    shown: std::cell::RefCell<Option<(std::rc::Weak<WorkspaceState>, Option<std::rc::Rc<WorkspaceState>>)>>,
    /// The layout keyboard focus was last checked against (useComposerFocusRecovery).
    focus_layout: u64,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    pub fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Root {
        let subscriptions = vec![
            cx.observe_in(&workspace, window, |root, _, window, cx| {
                root.workspace_changed(cx);
                root.menu_asked(window, cx);
                cx.notify();
            }),
            cx.subscribe_in(&workspace, window, |root, _, surface: &Surface, window, cx| root.surface(surface.clone(), window, cx)),
            cx.observe_window_activation(window, |_, window, cx| {
                let focused = window.is_window_active();
                act_ui::focus::note_activation(cx);
                dispatch(AppCommand::SetFocused { focused }, cx);
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                let dark = act_ui::theme::system_is_dark(window.appearance());
                dispatch(AppCommand::SystemScheme { dark }, cx);
                let state = crate::workspace::workspace(cx).read(cx).state.clone();
                crate::theme_bridge::apply(&state, cx);
            }),
        ];
        // Ordered before every stop, so Tab from the window itself goes to the first and Shift-Tab to the last.
        let focus = cx.focus_handle().tab_index(isize::MIN);
        let composer = composer::ComposerState::new(window, cx);
        let mut root = Root {
            workspace: workspace.clone(),
            focus,
            sidebar: sidebar::SidebarState::default(),
            topbar: topbar::TopbarState::default(),
            menus: Menus::default(),
            conversation: conversation::ConversationState::new(),
            composer,
            dock: dock::DockState::default(),
            settings: settings::SettingsState::default(),
            dialogs: dialogs::DialogState::default(),
            jump: jump::JumpState::default(),
            find: find_bar::FindState::default(),
            session: session_panel::SessionState::default(),
            library: None,
            pull_requests: Default::default(),
            automation: None,
            renaming: None,
            renaming_heading: None,
            rename_input: None,
            rename_invalid: false,
            pending_focus: None,
            parts: None,
            shown: Default::default(),
            focus_layout: 0,
            _subscriptions: subscriptions,
        };
        super::browser_dock::install(&mut root, &workspace, window, cx);
        root
    }

    /// Draws the transcript again, leaving the cached sidebar, topbar and composer as they are.
    pub fn redraw_transcript(&self, cx: &mut Context<Self>) {
        match &self.parts {
            Some(parts) => parts.redraw_transcript(cx),
            None => cx.notify(),
        }
    }

    pub fn state(&self, cx: &gpui::App) -> std::rc::Rc<WorkspaceState> {
        self.workspace.read(cx).state.clone()
    }

    /// What the thread on screen is drawn from: a paired computer's state under this window's own
    /// settings and drafts while that computer is on screen, else the window's own.
    pub fn shown(&self, cx: &gpui::App) -> std::rc::Rc<WorkspaceState> {
        let window = self.state(cx);
        let mut held = self.shown.borrow_mut();
        if let Some((_, shown)) = held.as_ref().filter(|(from, _)| std::ptr::eq(from.as_ptr(), &*window)) {
            return shown.clone().unwrap_or(window);
        }
        let shown = act_core::view::overlay::overlaid_state(&window).map(std::rc::Rc::new);
        *held = Some((std::rc::Rc::downgrade(&window), shown.clone()));
        shown.unwrap_or(window)
    }

    fn workspace_changed(&mut self, cx: &mut Context<Self>) {
        let (state, shown) = (self.state(cx), self.shown(cx));
        self.composer.sync(&state, &shown, cx);
    }

    /// A menu the workspace asked to show (⌘⇧N's project picker) opens once its trigger has been
    /// drawn; the request is then spent, so the next ask opens it again.
    fn menu_asked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state(cx).open_menu.as_deref() != Some(THREAD_START_PROJECT_MENU) {
            return;
        }
        cx.defer(|cx| dispatch(AppCommand::SetMenu { menu: None }, cx));
        self.open_project_picker_when_drawn(3, window, cx);
    }

    fn open_project_picker_when_drawn(&mut self, frames: u8, window: &mut Window, cx: &mut Context<Self>) {
        cx.on_next_frame(window, move |root, window, cx| {
            if root.menus.is_open(THREAD_START_PROJECT_MENU) {
                return;
            }
            if root.composer.start.project_drawn() {
                root.open_start_project_picker(window, cx);
            } else if frames > 0 {
                root.open_project_picker_when_drawn(frames - 1, window, cx);
                cx.notify();
            }
        });
    }

    fn surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        match surface {
            Surface::CloseWindow => window.remove_window(),
            Surface::FocusWindow => {
                cx.activate(true);
                window.activate_window();
            }
            Surface::FindInTerminal { terminal_id, query, forward } => self.dock.find_in_terminal(&terminal_id, &query, forward, cx),
            Surface::StopFindInTerminal { terminal_id } => self.dock.stop_find_in_terminal(&terminal_id, cx),
            Surface::Streamed { tasks } => {
                // Streamed text shows only in the transcript, the find bars counting its matches and the
                // dock, which is drawn with the window every frame; a thread shown in neither draws nothing.
                let shown = self.shown(cx);
                let tab = act_core::state::shown_thread_tab(&shown);
                let on_screen = tasks.iter().any(|task| shown.current_id.as_ref() == Some(task) || tab.as_ref() == Some(task));
                match &self.parts {
                    Some(parts) if on_screen => parts.redraw_streamed(cx),
                    Some(_) => {}
                    None => cx.notify(),
                }
            }
        }
    }

    /// Puts a thread's name in a field in place.
    pub fn start_rename(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let state = self.state(cx);
        let Some(thread) = state.thread(thread_id) else { return };
        if self.renaming_heading.as_deref() != Some(thread_id) {
            self.renaming = Some(thread_id.to_string());
        }
        let title = thread.title.clone();
        let input = cx.new(|cx| TextInput::new(cx).with_text(&title).placeholder(title.clone()));
        let id = thread_id.to_string();
        // A name is required: Enter on a blank field marks it, and a blank field left keeps the old name.
        let subscription = cx.subscribe(&input, move |root, input, event: &InputEvent, cx| match event {
            InputEvent::Changed => {
                // The heading's field is as wide as its text.
                if root.rename_invalid || root.renaming_heading.is_some() {
                    root.rename_invalid = false;
                    cx.notify();
                }
            }
            InputEvent::Submit | InputEvent::Blur => {
                let title = input.read(cx).text().to_string();
                if title.trim().is_empty() {
                    if matches!(event, InputEvent::Submit) {
                        root.rename_invalid = true;
                        cx.notify();
                        return;
                    }
                } else {
                    dispatch(AppCommand::TaskRename { task_id: id.clone(), title }, cx);
                }
                if matches!(event, InputEvent::Submit) {
                    if root.renaming_heading.is_some() {
                        root.topbar.refocus_title = true;
                    } else {
                        root.sidebar.refocus_row = root.renaming.clone();
                    }
                    act_ui::focus::show_rings(cx);
                }
                root.stop_rename(cx);
            }
            _ => {}
        });
        self.rename_input = Some((input.clone(), subscription));
        self.pending_focus = Some(input);
        cx.notify();
    }

    pub fn stop_rename(&mut self, cx: &mut Context<Self>) {
        self.rename_invalid = false;
        self.renaming = None;
        self.renaming_heading = None;
        self.rename_input = None;
        cx.notify();
    }

    /// The field a name is typed in, sized like the row it replaces.
    pub fn rename_field(&mut self, _thread_id: &str, cx: &mut Context<Self>) -> AnyElement {
        self.rename_box(false, cx)
    }

    /// The field the topbar's title is typed in, in the heading's own font.
    pub fn heading_rename_field(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.rename_box(true, cx)
    }

    fn rename_box(&mut self, heading: bool, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.theme().tokens.clone();
        let Some((input, _)) = &self.rename_input else { return div().into_any_element() };
        div()
            .flex_1()
            .min_w(px(120.0))
            .h(px(if heading { 28.0 } else { 26.0 }))
            .flex()
            .items_center()
            .px(px(6.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(if self.rename_invalid { t.danger } else { t.line_input })
            .when(self.rename_invalid, |field| field.shadow(vec![gpui::BoxShadow { color: t.danger.into(), offset: gpui::point(px(0.0), px(0.0)), blur_radius: px(0.0), spread_radius: px(1.0), inset: false }]))
            .bg(t.surface_input)
            .text_color(t.ink)
            // The field holds focus while it is shown, so it wears the keyboard's ring whenever rings show.
            .when(act_ui::focus::keyboard_focus_visible(cx), |field| field.relative().border_color(t.focus_ring).child(div().absolute().inset_0().rounded(px(5.0)).border_1().border_color(t.focus_ring)))
            .map(|field| if heading { act_ui::style::letter_spacing(field, 0.0) } else { field.text_size(px(14.0)) })
            .on_key_down(cx.listener(move |root, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if heading {
                        root.topbar.refocus_title = true;
                    } else {
                        root.sidebar.refocus_row = root.renaming.clone();
                    }
                    act_ui::focus::note_keystroke(&event.keystroke, window, cx);
                    root.rename_input = None;
                    root.stop_rename(cx);
                    cx.stop_propagation();
                }
            }))
            .child(input.clone())
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.state(cx);
        if settings::capture_key(&state, &event.keystroke, cx) {
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" {
            if self.menus.any_open() {
                return;
            }
            dispatch(AppCommand::Escape, cx);
            return;
        }
        let modifiers = &event.keystroke.modifiers;
        if event.keystroke.key == "tab" && !(modifiers.platform || modifiers.control || modifiers.alt) {
            // Left to propagate, so the keystroke observers see it and show the focus ring.
            if modifiers.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
            return;
        }
        if let Some(command) = crate::keys::shortcut_command(&state, &event.keystroke) {
            cx.stop_propagation();
            dispatch(command, cx);
            window.refresh();
            return;
        }
        // With the keys on the page or its text, the scrolling keys move the transcript last pressed in.
        let bare = !(modifiers.platform || modifiers.control || modifiers.alt);
        if bare && !window.focus_on_tab_stop(cx) && self.conversation.keys_scroll.get() && self.conversation.scroll_by_key(&event.keystroke.key, modifiers.shift) {
            cx.stop_propagation();
            self.redraw_transcript(cx);
        }
    }
}

impl Focusable for Root {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Root {
    /// Live visuals are native pages over the window: anything drawn over the transcript hides them,
    /// and they stop short of what floats over its foot.
    fn place_visuals(&self, state: &WorkspaceState, window: &mut Window, cx: &mut Context<Self>) {
        let viewing = state.viewing_image.is_some() || super::viewer::open(cx);
        let covered = viewing
            || state.settings_shown()
            || state.jump.is_some()
            || state.project_add.is_some()
            || state.fork_name.is_some()
            || state.archive_prompt.is_some()
            || state.worktree_settings.confirming.is_some()
            || state.review_picker.is_some()
            || self.menus.any_open()
            || self.library.is_some()
            || self.composer.annotator.is_some()
            || self.composer_menu_open(cx)
            || (super::sidebar::floats(state, window) && state.sidebar_open && !state.zen);
        let occluders = [self.composer.probe.get(), self.conversation.approval_bounds()].into_iter().flatten().collect();
        let holes = self.conversation.scroll_button_bounds().into_iter().collect();
        super::visual::begin_frame(covered, occluders, holes, window, cx);
    }
}

impl Root {
    /// Once the dock, sidebar, settings or dock tab change, keys left on nothing reachable go back
    /// to the composer, unless a page in the dock holds them.
    fn recover_focus(&mut self, state: &WorkspaceState, dock_open: bool, window: &mut Window, cx: &mut Context<Self>) {
        use std::hash::{Hash, Hasher};
        let (owner, dock) = act_core::state::front_dock(state);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (dock_open, state.sidebar_open, state.settings_shown(), &dock.tab, state.dock_focus.as_ref().map(|focus| (&focus.tab, focus.count))).hash(&mut hasher);
        let layout = hasher.finish();
        if layout == self.focus_layout {
            return;
        }
        self.focus_layout = layout;
        let page_took_keys = state.dock_focus.as_ref().is_some_and(|focus| focus.owner == owner && focus.tab == dock.tab && dock.browser_tabs.iter().any(|page| page.id == focus.tab && !page.url.is_empty()));
        if page_took_keys || state.settings_shown() {
            return;
        }
        cx.on_next_frame(window, |root, window, cx| {
            if window.focus_stranded(cx) || root.focus.is_focused(window) {
                dispatch(AppCommand::FocusComposer { recover: Some(true) }, cx);
            }
        });
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        act_ui::focus::sync(cx);
        if let Some(input) = self.pending_focus.take() {
            input.update(cx, |input, cx| {
                input.select_all_text(cx);
                input.focus(window, cx);
            });
        }
        // Focus given to something not drawn yet, such as a menu opened by the click that woke the
        // window, gets one more frame to appear before the window takes the keyboard back.
        cx.on_next_frame(window, |root, window, cx| {
            if root.focus.contains_focused(window, cx) {
                return;
            }
            let held = window.focused(cx);
            cx.on_next_frame(window, move |root, window, cx| {
                if !root.focus.contains_focused(window, cx) && window.focused(cx) == held {
                    window.focus(&root.focus, cx);
                }
            });
        });
        let state = self.state(cx);
        // The sidebar draws Priority and Threads always, each with its expanded toggle while open.
        act_ui::style::set_frame_expanded(state.sections.priority || state.sections.threads);
        crate::desktop_capture::sync(&state, cx);
        super::pull_request_reads::sync(self, &state, cx);
        self.place_visuals(&state, window, cx);
        let theme = cx.theme().clone();
        let t = &theme.tokens;
        let m = &theme.metrics;
        if !state.restored {
            return act_ui::root(cx).id("app-shell").bg(t.frame_clear).into_any_element();
        }
        // The thread, its composer and its dock are the computer's on screen; the rest is this window's.
        let shown = self.shown(cx);
        let dock_open = act_core::state::front_dock(&shown).1.open && !state.zen;
        self.recover_focus(&shown, dock_open, window, cx);
        if !dock_open || state.settings_shown() {
            self.dock.hidden(cx);
        }
        if !dock_open {
            self.dock.hold_review(&shown, cx);
        }
        let drafting = shown.current_thread().is_none();
        let session_open = state.session_panel_open && !dock_open && !state.zen;

        let parts = self.parts.get_or_insert_with(|| super::parts::Parts::new(cx));
        let (sidebar_part, topbar_part, composer_part, transcript_part) = (parts.sidebar(), parts.topbar(), parts.composer(), parts.transcript());
        let page_content = if drafting { empty_thread::render(self, &shown, window, cx).into_any_element() } else { transcript_part.into_any_element() };
        let mut page = div()
            .relative()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .rounded(px(m.page_radius))
            .when(t.page.a > 0.0, |page| page.bg(t.page).elevation(&t.shadow_page))
            .child(page_content);
        if !drafting {
            page = page.child(div().absolute().inset_0().child(composer_part));
        }
        if session_open {
            page = page.child(session_panel::render(self, &shown, window, cx));
        }
        if let Some(parts) = self.parts.as_ref() {
            page = page.child(parts.find(false));
        }
        let topbar = div().flex_none().w_full().h(px(m.chrome_head)).child(topbar_part);
        let main = div().relative().flex_1().min_w_0().h_full().flex().flex_col().child(topbar).child(page).children(super::banners::render(&shown, window, cx));
        let main = composer::drop_target(self, main, cx);
        let viewport = f32::from(window.viewport_size().width);
        self.follow_floating_sidebar(&state, window, cx);
        let sidebar_floats = sidebar::floats(&state, window);
        let sidebar_width = sidebar::docked_width(self, &state, window);
        let workspace_width = viewport - sidebar_width - 2.0 * m.frame_gap;
        let expanded = act_core::state::front_dock(&shown).1.expanded;
        // Overlays drawn after the workspace stay after it in the Tab order.
        let mut column = div().tab_order(-1).inert(state.settings_shown()).relative().flex_1().min_w_0().h_full().flex().flex_row().mx(px(m.frame_gap));
        if dock_open && expanded {
            self.conversation.tuck_away(cx);
            column = column.child(div().size_full().pb(px(m.frame_gap)).child(dock::render(self, &shown, window, cx)));
        } else {
            column = column.child(div().flex_1().min_w_0().h_full().pb(px(m.frame_gap)).child(main));
            if dock_open {
                let width = self.dock.track_width(viewport, workspace_width, viewport - m.frame_gap);
                // Opening slides the dock in from the right edge at its own width (`.right-dock` transform).
                let track = div()
                    .id("right-dock-track")
                    .when(!t.reduced_motion, |track| track.enter(act_ui::style::panel_slide(300), move |style| style.transform(gpui::ElementTransform::translate(px(width), px(0.0)))))
                    .flex_none()
                    .w(px(width))
                    .h_full()
                    .pl(px(m.frame_gap))
                    .pb(px(m.frame_gap));
                column = column.child(track.child(dock::render(self, &shown, window, cx)));
            }
        }

        let mut shell = act_ui::root(cx)
            .id("app-shell")
            .accessible(gpui::AccessInfo::new(gpui::AccessRole::Main, ""))
            .key_context("Workspace")
            .track_focus(&self.focus)
            // A press over an occluding sheet, menu or dialog misses the shell's own hitbox, so the
            // pointer is heard window-wide.
            .child(gpui::canvas(|_, _, _| (), |_, _, window, _| {
                window.on_mouse_event(|_: &gpui::MouseDownEvent, phase, _, cx| {
                    if phase == gpui::DispatchPhase::Capture {
                        act_ui::focus::note_pointer(cx);
                    }
                });
            }).absolute().size_0())
            .on_key_down(cx.listener(Self::key_down))
            .on_drag_move(cx.listener(Self::resize_sidebar))
            .on_key_up(cx.listener(|root, event, _, cx| root.dictation_key_up(event, cx)))
            .on_modifiers_changed(cx.listener(|root, event, _, cx| root.dictation_modifiers(event, cx)))
            .flex()
            .flex_row()
            .overflow_hidden()
            .bg(t.frame_clear);
        // Over a material the settings sheet has no ground of its own, so the window behind it steps out of the way.
        let behind_settings = state.settings_shown() && cx.theme().environment.material != act_ui::theme::WindowMaterial::None;
        if !behind_settings && !sidebar_floats {
            shell = shell.child(sidebar::slot(self, &state, window).child(sidebar_part)).child(column);
        } else if !behind_settings {
            shell = shell.child(column);
            if state.sidebar_open && !state.zen {
                shell = shell.child(sidebar::backdrop(self, &state, &shown, window, cx));
            }
            shell = shell.child(sidebar::slot(self, &state, window).child(sidebar_part));
        }
        if state.settings_shown() {
            shell = shell.child(settings::render(self, &state, window, cx));
        } else {
            self.settings.closed();
        }
        let dialogs = dialogs::render(self, &state, window, cx);
        // A modal or Settings covers the composer, and a meter it hides hears no hover-out, so its cards close.
        if state.settings_shown() || !dialogs.is_empty() || state.jump.is_some() {
            self.composer.plan_card = false;
            self.composer.context_card = false;
        }
        shell = shell.children(dialogs);
        if state.jump.is_some() {
            shell = shell.child(jump::render(self, &state, window, cx));
        } else {
            self.jump.closed(window, cx);
        }
        if !behind_settings {
            shell = shell.child(toasts::render(&state));
        }
        shell = shell.children(super::viewer::render(self, &state, window, cx));
        shell = shell.children(super::annotator::render(self, window, cx));
        if let Some(library) = super::model_library::popover(self, &shown, window, cx) {
            shell = shell.child(library);
        }
        if let Some(menu) = self.menus.element() {
            shell = shell.child(menu);
        }
        let shell = shell.child(act_ui::tooltip::tooltip_layer(window, cx));
        match self.parts.as_ref() {
            Some(parts) => gpui::image_cache(parts.images()).size_full().child(shell).into_any_element(),
            None => shell.into_any_element(),
        }
    }
}
