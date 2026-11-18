//! A dock tab's shell: where it runs, how it ended, and the terminal itself (TerminalPanel.tsx).

use std::collections::HashMap;

use act_core::domain::terminal::{TerminalSession, TerminalStatus};
use act_core::domain::platform::ComputerStatus;
use act_core::events::ViewEvent;
use act_core::AppCommand;
use act_terminal::theme::{TerminalFont, TerminalTheme};
use act_terminal::view::{TerminalView, TerminalViewEvent};
use act_ui::components::button::{Button, ButtonSize, ButtonVariant};
use act_ui::theme::{ActiveTheme, Theme};
use act_ui::IconName;
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FontWeight, Hsla, Subscription, Window};
use serde_json::json;

use super::common::{icon, Ellipsis};
use super::root::Root;
use super::sidebar::send;
use crate::effects::window_desktop;
use crate::workspace::{dispatch, report};

/// The views the dock has drawn, kept while their shells live so scrollback and selection survive
/// switching tabs. A paired computer's shell is followed only while its tab is drawn.
#[derive(Default)]
pub struct Terminals {
    views: HashMap<String, Held>,
    themed: Option<String>,
    /// Why a followed remote shell is not live, by terminal: connecting, failing, or closed.
    remote_status: HashMap<String, String>,
}

struct Held {
    view: Entity<TerminalView>,
    /// The paired computer whose shell this view follows.
    computer: Option<String>,
    _events: Subscription,
}

fn hsla(color: gpui::Rgba) -> Hsla {
    color.into()
}

pub fn terminal_theme(theme: &Theme) -> TerminalTheme {
    let t = &theme.tokens;
    // The viewport paints the surface edge to edge; the shell's own ground stays clear over it, as
    // xterm's does, so a translucent surface is not laid down twice.
    TerminalTheme {
        background: Hsla { a: 0.0, ..hsla(t.code_surface) },
        foreground: hsla(t.code_ink),
        cursor: hsla(t.ink),
        cursor_accent: hsla(t.code_surface),
        selection: hsla(t.terminal_selection),
        find_match: hsla(t.find_match),
        find_active: hsla(t.find_active),
        ansi: t.ansi.map(hsla),
    }
}

pub fn terminal_font(theme: &Theme) -> TerminalFont {
    TerminalFont { family: theme.typography.mono_family.clone(), size: px(theme.metrics.terminal_text) }
}

impl Terminals {
    pub fn view(&self, terminal_id: &str) -> Option<&Entity<TerminalView>> {
        self.views.get(terminal_id).map(|held| &held.view)
    }

    /// Drops views whose shells the workspace no longer holds, and every followed remote shell
    /// but the one shown.
    pub fn retain(&mut self, alive: &[String], shown: Option<&str>) {
        self.views.retain(|id, held| alive.contains(id) && (held.computer.is_none() || shown == Some(id.as_str())));
        let views = &self.views;
        self.remote_status.retain(|id, _| views.contains_key(id));
    }

    /// Stops following a remote shell, which starts over from its screen when shown again.
    fn drop_remote(&mut self, terminal_id: &str) {
        if self.views.get(terminal_id).is_some_and(|held| held.computer.is_some()) {
            self.views.remove(terminal_id);
            self.remote_status.remove(terminal_id);
        }
    }

    fn ensure(&mut self, terminal_id: &str, computer: Option<&str>, window: &mut Window, cx: &mut Context<Root>) -> Option<Entity<TerminalView>> {
        match self.views.get(terminal_id) {
            Some(held) if held.computer.as_deref() == computer => return Some(held.view.clone()),
            Some(_) => {
                self.views.remove(terminal_id);
                self.remote_status.remove(terminal_id);
            }
            None => {}
        }
        let screen = match computer {
            Some(_) => act_terminal::screen::Screen::new(80, 24),
            None => window_desktop::terminal_screen(terminal_id, cx)?,
        };
        let theme = cx.theme().clone();
        let view = cx.new(|cx| {
            let mut view = TerminalView::new(screen, window, cx);
            view.set_theme(terminal_theme(&theme), cx);
            view.set_font(terminal_font(&theme), cx);
            view
        });
        let id = terminal_id.to_string();
        let subscription = cx.subscribe(&view, move |root, _, event: &TerminalViewEvent, cx| match event {
            TerminalViewEvent::Input(data) => dispatch(AppCommand::TerminalInput { terminal_id: id.clone(), data: data.clone() }, cx),
            TerminalViewEvent::Resize { cols, rows } => dispatch(AppCommand::TerminalResize { terminal_id: id.clone(), cols: i64::from(*cols), rows: i64::from(*rows) }, cx),
            TerminalViewEvent::FindResults(results) => {
                let event = ViewEvent::FindResults {
                    target: act_core::domain::find::FindTarget::Terminal { terminal_id: id.clone() },
                    results: act_core::domain::find::FindResults { matches: results.matches as i64, index: Some(results.index as i64), counting: None },
                };
                report(event, cx);
            }
            TerminalViewEvent::RemoteStatus(status) => {
                let terminals = &mut root.dock.terminals;
                let changed = match status {
                    Some(status) => terminals.remote_status.insert(id.clone(), status.clone()).as_ref() != Some(status),
                    None => terminals.remote_status.remove(&id).is_some(),
                };
                if changed {
                    cx.notify();
                }
            }
        });
        if let Some(computer) = computer {
            let read = window_desktop::remote_terminal(computer.to_string(), terminal_id.to_string(), cx);
            view.update(cx, |view, cx| view.follow_remote(read, cx));
        }
        view.update(cx, |view, cx| view.focus(window, cx));
        self.views.insert(terminal_id.to_string(), Held { view: view.clone(), computer: computer.map(String::from), _events: subscription });
        Some(view)
    }

    /// Keeps every view on the current theme and type.
    fn restyle(&mut self, cx: &mut Context<Root>) {
        let theme = cx.theme().clone();
        let key = format!("{}|{}|{}", theme.info.id, theme.typography.mono_family, theme.metrics.terminal_text);
        if self.themed.as_deref() == Some(key.as_str()) {
            return;
        }
        self.themed = Some(key);
        window_desktop::set_terminal_theme(&terminal_theme(&theme), cx);
        for Held { view, .. } in self.views.values() {
            view.update(cx, |view, cx| {
                view.set_theme(terminal_theme(&theme), cx);
                view.set_font(terminal_font(&theme), cx);
            });
        }
    }
}

/// A note in the status line (`.terminal-status em`): how the shell ended, or why it is not live.
fn status_badge(glyph: IconName, label: String, (border, fill, ink): (gpui::Rgba, gpui::Rgba, gpui::Rgba), theme: &Theme) -> impl IntoElement {
    div()
        .flex_shrink(1.)
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(5.0))
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(border)
        .bg(fill)
        .text_color(ink)
        .font_family(theme.typography.ui_family.clone())
        .font_weight(FontWeight(550.0))
        .child(div().flex_none().child(icon(glyph, 12.0, ink)))
        .child(div().min_w_0().ellipsis().child(label))
}

pub fn render(root: &mut Root, state: &act_core::WorkspaceState, terminal: &TerminalSession, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let theme = cx.theme().clone();
    let t = &theme.tokens;
    root.dock.terminals.restyle(cx);
    // A shell on the paired computer on screen is read from there, and only while its line is up.
    let computer = state.computers.active.as_ref().and_then(|id| state.computers.paired.iter().find(|computer| &computer.link.id == id)).map(|computer| &computer.link);
    let connected = computer.is_none_or(|link| link.status == ComputerStatus::Connected);
    let view = if connected {
        root.dock.terminals.ensure(&terminal.id, computer.map(|link| link.id.as_str()), window, cx)
    } else {
        root.dock.terminals.drop_remote(&terminal.id);
        None
    };
    let unplugged = computer.and_then(|link| if connected { root.dock.terminals.remote_status.get(&terminal.id).cloned() } else { Some(link.error.clone().unwrap_or_else(|| "Disconnected — reconnecting…".into())) });
    let exited = terminal.status == TerminalStatus::Exited;
    let clean = terminal.error.is_none() && terminal.exit_code == Some(0);
    let mut status = div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .border_b_1()
        .border_color(t.line_soft)
        .pt(px(5.0))
        .pb(px(5.0))
        .pl(px(11.0))
        .pr(px(8.0))
        .text_color(t.muted)
        .font_family(theme.typography.mono_family.clone())
        .text_size(px(12.0))
        .child(div().min_w_0().ellipsis().child(match computer {
            Some(link) => format!("{} · {}", link.name, terminal.cwd),
            None => terminal.cwd.clone(),
        }));
    if let Some(message) = unplugged {
        status = status.child(status_badge(IconName::Unplug, message, (t.danger_line, t.danger_surface, t.danger), &theme));
    }
    if exited {
        let label = terminal.error.clone().unwrap_or_else(|| format!("Exited{}", terminal.exit_code.map(|code| format!(" ({code})")).unwrap_or_default()));
        let (border, fill, ink, glyph) = if clean { (t.line_control, t.wash, t.ink_dim, IconName::Check) } else { (t.danger_line, t.danger_surface, t.danger, IconName::TriangleAlert) };
        status = status.child(status_badge(glyph, label, (border, fill, ink), &theme));
    }
    let id = terminal.id.clone();
    let cwd = terminal.cwd.clone();
    // A button takes the interface font, not the monospace path beside it.
    status = status.child(div().ml_auto().font_family(theme.typography.ui_family.clone()).child(if exited {
        Button::new("terminal-restart", "Restart shell").icon(IconName::RotateCw).icon_size(12.0).variant(ButtonVariant::Primary).size(ButtonSize::Sm).on_click(move |_, _, cx| dispatch(AppCommand::TerminalRestart { terminal_id: id.clone() }, cx)).into_any_element()
    } else {
        Button::new("terminal-new-shell", "New shell").icon(IconName::Plus).icon_size(12.0).size(ButtonSize::Sm).on_click(move |_, _, cx| send(json!({ "type": "terminal.open", "cwd": cwd }), cx)).into_any_element()
    }));
    // The terminal view pads itself as `.terminal-viewport`; only a waiting note takes the padding here.
    let padded = view.is_none();
    let mut viewport = div().id("terminal-viewport").relative().flex_1().min_h_0().when(padded, |viewport| viewport.pt(px(8.0)).pr(px(4.0)).pb(px(8.0)).pl(px(10.0))).bg(t.code_surface);
    match view {
        Some(view) => {
            let focus = view.clone();
            viewport = viewport.on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| focus.update(cx, |view, cx| view.focus(window, cx))).child(view)
        }
        None if !exited && computer.is_none() => viewport = viewport.child(div().text_color(t.quiet).text_size(px(12.0)).child("Starting the shell…")),
        None => {}
    }
    div().size_full().flex().flex_col().overflow_hidden().child(status).child(viewport).into_any_element()
}
