//! Devices (MobileSettings.tsx, ComputerSettings.tsx): other computers to pair with, phone access over
//! Tailscale, the pairing code and its QR, and the phones already paired.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use act_core::domain::mobile::{address_origin, MobileAddressKind, MobileConnectionState, MobileServerState, MobileServerStatus, PairedDeviceKind, PairedDeviceView, TailscaleStatus};
use act_core::domain::platform::{computer_address, ComputerStatus, MAX_COMPUTER_NAME};
use act_core::state::WorkspaceState;
use act_core::view::settings::{computer_upkeep, UpkeepItem, UpkeepPart, UpkeepTone};
use act_core::AppCommand;
use act_ui::components::button::{Button, ButtonSize, ButtonVariant, FillTone};
use act_ui::components::confirm_action::ConfirmAction;
use act_ui::components::dialog::{dialog_paragraph, Dialog};
use act_ui::components::field::{Switch, TextField};
use act_ui::input::InputEvent;
use act_ui::theme::{ActiveTheme, Tokens};
use act_ui::IconName;
use gpui::{div, img, prelude::*, px, AnyElement, Context, FontWeight, Rgba, SharedString, Task, Window};

use crate::views::letter_spacing::LetterSpacing;
use super::{description, empty, error, glyph, page, pill, spinning_button, status, strong, with_code, Held, Marks, PillTone, Row, Section};
use super::Clip;
use crate::views::common::ink_icon;
use crate::views::root::Root;
use crate::views::timeline_row::copy_button;
use crate::workspace::dispatch;

const PAIRING_CODE_LENGTH: usize = 8;
const TAILSCALE_DOWNLOAD: &str = "https://tailscale.com/download";
const TAILSCALE_DNS: &str = "https://login.tailscale.com/admin/dns";
const TESTFLIGHT_LINK: &str = "https://testflight.apple.com/join/v37MHegn";
const NEEDED_MARK: &str = "phone.step-needed";

#[derive(Default)]
pub struct DevicesState {
    name: Option<(String, Held)>,
    address: Option<Held>,
    address_invalid: bool,
    code: Option<(String, Held)>,
    renaming: Option<(String, Held)>,
    qr: HashMap<String, Arc<gpui::Image>>,
    /// The code a fresh one was last asked for, so a bridge that will not mint is not asked in a loop.
    asked: Option<String>,
    ticker: Option<Task<()>>,
    getting_app: bool,
    shown_step: usize,
}

/// A URL as a QR image: one module of margin, six pixels a module.
fn qr_image(cache: &mut HashMap<String, Arc<gpui::Image>>, url: &str) -> Option<Arc<gpui::Image>> {
    if let Some(image) = cache.get(url) {
        return Some(image.clone());
    }
    let code = qrcode::QrCode::with_error_correction_level(url.as_bytes(), qrcode::EcLevel::M).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let (scale, margin) = (6u32, 1u32);
    let side = (width as u32 + margin * 2) * scale;
    let mut pixels = image::GrayImage::from_pixel(side, side, image::Luma([255]));
    for (index, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let (x, y) = ((index % width) as u32 + margin, (index / width) as u32 + margin);
        for dy in 0..scale {
            for dx in 0..scale {
                pixels.put_pixel(x * scale + dx, y * scale + dy, image::Luma([0]));
            }
        }
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageLuma8(pixels).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).ok()?;
    let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes));
    cache.insert(url.to_string(), image.clone());
    Some(image)
}

fn qr(cache: &mut HashMap<String, Arc<gpui::Image>>, url: &str, size: f32, expired: bool, t: &Tokens) -> AnyElement {
    match qr_image(cache, url) {
        Some(image) => div().flex_none().size(px(size)).rounded(px(12.0)).overflow_hidden().bg(gpui::white()).border_1().border_color(t.line).when(expired, |qr| qr.opacity(0.25)).child(img(image).size_full()).into_any_element(),
        None => div().flex_none().size(px(size)).rounded(px(12.0)).bg(t.wash).into_any_element(),
    }
}

fn reachable(remote: &MobileServerState) -> bool {
    remote.primary.as_ref().is_some_and(|address| address.kind == MobileAddressKind::TailscaleHttps)
}

fn status_label(remote: &MobileServerState) -> &'static str {
    if !remote.enabled {
        return "Off";
    }
    match remote.status {
        MobileServerStatus::Listening if reachable(remote) => "On",
        MobileServerStatus::Listening => "Waiting for Tailscale",
        MobileServerStatus::Starting => "Starting…",
        MobileServerStatus::Error => "Failed to start",
        MobileServerStatus::Off => "Off",
    }
}

fn status_tone(remote: &MobileServerState) -> PillTone {
    if !remote.enabled || matches!(remote.status, MobileServerStatus::Off | MobileServerStatus::Starting) {
        return PillTone::Neutral;
    }
    if remote.status == MobileServerStatus::Listening && reachable(remote) {
        PillTone::Ready
    } else {
        PillTone::Accent
    }
}

struct Step {
    id: &'static str,
    done: bool,
    name: String,
    hint: Option<&'static str>,
    link: Option<(&'static str, &'static str)>,
}

fn tailscale_steps(remote: &MobileServerState, on: bool) -> Vec<Step> {
    let tailscale = &remote.tailscale;
    let installed = !matches!(tailscale.status, TailscaleStatus::Missing | TailscaleStatus::Unknown);
    let unavailable = tailscale.status == TailscaleStatus::Unavailable;
    let signed_in = tailscale.status == TailscaleStatus::Ready;
    vec![
        Step {
            id: "installed",
            done: installed,
            name: "Installed on this computer".into(),
            hint: if tailscale.status == TailscaleStatus::Unknown {
                Some("Looking for Tailscale…")
            } else if installed {
                None
            } else {
                Some("Install Tailscale, then check again.")
            },
            link: Some(("Download Tailscale", TAILSCALE_DOWNLOAD)),
        },
        Step {
            id: "signed-in",
            done: signed_in,
            name: match (&tailscale.magic_dns_name, signed_in) {
                (Some(name), true) => format!("Signed in as {name}"),
                _ => "Signed in".into(),
            },
            hint: (installed && !signed_in && !unavailable).then_some("Open Tailscale and sign in."),
            link: None,
        },
        Step {
            id: "https",
            done: signed_in && tailscale.certs,
            name: "HTTPS on".into(),
            hint: (signed_in && !tailscale.certs).then_some("Turn on HTTPS in the Tailscale admin console, under DNS."),
            link: Some(("Open admin console", TAILSCALE_DNS)),
        },
        Step {
            id: "serving",
            done: tailscale.serving,
            name: "Serving this computer".into(),
            hint: if !on {
                Some("Turns on with phone access.")
            } else if tailscale.serving || !signed_in || !tailscale.certs {
                None
            } else {
                Some("Not serving yet. Check again.")
            },
            link: None,
        },
    ]
}

fn countdown(remaining_ms: i64) -> String {
    let seconds = (remaining_ms as f64 / 1000.0).ceil() as i64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn device_connection(device: &PairedDeviceView, remote: &MobileServerState) -> Option<MobileConnectionState> {
    let held: Vec<MobileConnectionState> = remote.sessions.iter().filter(|session| session.device_id == device.id).map(|session| session.connection).collect();
    [MobileConnectionState::Live, MobileConnectionState::Resuming, MobileConnectionState::Connecting, MobileConnectionState::Offline].into_iter().find(|state| held.contains(state))
}

fn device_label(device: &PairedDeviceView, connection: Option<MobileConnectionState>, now: i64) -> String {
    match connection {
        Some(MobileConnectionState::Live) => "Connected".into(),
        Some(MobileConnectionState::Resuming) => "Catching up…".into(),
        Some(MobileConnectionState::Connecting) => "Connecting…".into(),
        Some(MobileConnectionState::Offline) => "Reconnecting…".into(),
        None => match device.last_seen_at {
            None => "Never connected".into(),
            Some(seen) => match (now - seen) / 86_400_000 {
                0 => "Seen today".into(),
                1 => "Seen yesterday".into(),
                days => format!("Seen {days} days ago"),
            },
        },
    }
}

fn tone_color(tone: Option<UpkeepTone>, t: &Tokens) -> Rgba {
    match tone {
        Some(UpkeepTone::Live) => t.success_soft,
        Some(UpkeepTone::Progress) | Some(UpkeepTone::Offer) => t.info_soft,
        Some(UpkeepTone::Error) => t.danger_soft,
        Some(UpkeepTone::Quiet) => t.quiet,
        None => t.quiet_strong,
    }
}

fn status_line(parts: &[UpkeepPart], t: &Tokens) -> AnyElement {
    let mut line = div().mt(px(4.0)).flex().flex_wrap().text_size(px(12.0)).line_height(gpui::relative(1.45));
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            line = line.child(div().mx(px(6.0)).text_color(t.quiet).child("·"));
        }
        line = line.child(div().text_color(if part.tone.is_none() { t.quiet_strong } else { tone_color(part.tone, t) }).child(part.text.clone()));
    }
    line.into_any_element()
}

fn software(items: &[UpkeepItem], t: &Tokens) -> Option<AnyElement> {
    if items.is_empty() {
        return None;
    }
    let count = items.len();
    Some(
        div()
            .mt(px(2.0))
            .pt(px(4.0))
            .border_t_1()
            .border_color(t.line)
            .flex()
            .flex_col()
            .children(items.iter().enumerate().map(|(index, item)| {
                let cell = |width: Option<f32>| {
                    let cell = div().min_h(px(40.0)).flex().items_center().min_w_0().pr(px(18.0)).text_size(px(12.0));
                    match width {
                        Some(width) => cell.w(px(width)).flex_none(),
                        None => cell.flex_1(),
                    }
                };
                let action = item.action.clone();
                div()
                    .flex()
                    .when(index + 1 < count, |row| row.border_b_1().border_color(t.line))
                    .child(cell(Some(136.0)).text_color(t.ink_dim).font_weight(FontWeight(560.0)).child(item.label.clone()))
                    .child(cell(Some(94.0)).text_color(t.quiet_strong).child(item.version.clone().unwrap_or_else(|| "—".into())))
                    .child(cell(None).text_color(tone_color(item.state.tone, t)).child(item.state.text.clone()))
                    .child(cell(Some(110.0)).pr(px(0.0)).justify_end().when_some(action, |cell, action| {
                        let error = item.state.tone == Some(UpkeepTone::Error);
                        let command = action.command.clone();
                        let mut button = Button::new(SharedString::from(format!("upkeep-{}", item.key)), action.label.clone()).on_click(move |_, _, cx| dispatch(command.clone(), cx));
                        if error {
                            button = button.variant(ButtonVariant::Primary).tone(FillTone::Info);
                        }
                        cell.child(div().min_w(px(96.0)).flex().justify_end().child(button))
                    }))
            }))
            .into_any_element(),
    )
}

fn held_field(slot: &mut Option<(String, Held)>, key: &str, text: &str, placeholder: &str, on_event: impl Fn(&mut Root, &gpui::Entity<act_ui::input::TextInput>, &InputEvent, &mut Context<Root>) + 'static, cx: &mut Context<Root>) -> gpui::Entity<act_ui::input::TextInput> {
    if slot.as_ref().is_none_or(|(held_key, _)| held_key != key) {
        *slot = Some((key.to_string(), Held::new(text, placeholder, on_event, cx)));
    }
    slot.as_ref().map(|(_, held)| held.input.clone()).expect("made above")
}

fn commit_name(root: &mut Root, cx: &mut Context<Root>) {
    let name = root.state(cx).computers.name.clone();
    let Some((_, held)) = &root.settings.devices.name else { return };
    let typed = held.text(cx).trim().to_string();
    let input = held.input.clone();
    if typed == name {
        input.update(cx, |input, cx| input.reset(&name, cx));
        return;
    }
    if typed.is_empty() {
        input.update(cx, |input, cx| input.reset(&name, cx));
    }
    dispatch(AppCommand::ComputersRename { name: typed }, cx);
}

fn computers_section(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let computers = &state.computers;
    let searching = computers.searching;
    let look = spinning_button("computers-look", if searching { "Looking…" } else { "Look again" }, IconName::RefreshCw, searching).disabled(searching).on_click(|_, _, cx| dispatch(AppCommand::ComputersDiscover, cx));
    let mut section = Section::setting("phone.computers")
        .first()
        .description(with_code(&[("Another computer running AI Coding Tool, or", false), (concat!(act_core::cli_command!(), " serve"), true), (", shows its threads here beside your own.", false)], cx))
        .action(look);

    let name = computers.name.clone();
    let name_input = held_field(
        &mut root.settings.devices.name,
        "this",
        &name,
        "",
        |root, input, event, cx| match event {
            InputEvent::Blur | InputEvent::Submit => commit_name(root, cx),
            InputEvent::Changed => {
                let text = input.read(cx).text().to_string();
                if text.chars().count() > MAX_COMPUTER_NAME {
                    let cut: String = text.chars().take(MAX_COMPUTER_NAME).collect();
                    input.update(cx, |input, cx| input.reset(&cut, cx));
                }
            }
            _ => {}
        },
        cx,
    );
    if !name_input.read(cx).is_focused(window) && name_input.read(cx).text() != name {
        let text = name.clone();
        name_input.update(cx, |input, cx| input.reset(&text, cx));
    }
    let escape_input = name_input.clone();
    let escape_name = name.clone();
    let name_entry = div()
        .flex()
        .flex_col()
        .gap(px(7.0))
        .child(strong("This computer's name", &t))
        .child(div().w(px(212.0)).on_key_down(move |event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" && escape_input.read(cx).text() != escape_name {
                cx.stop_propagation();
                let name = escape_name.clone();
                escape_input.update(cx, |input, cx| input.reset(&name, cx));
            }
        }).child(TextField::new("computer-name", &name_input)))
        .into_any_element();
    section = section.row(Row::titled(name_entry).lead(glyph(IconName::Monitor, t.muted)).build(marks, &t));

    let upkeep = computer_upkeep(&computers.paired, state.app_build.as_ref());
    let paired_hosts: std::collections::HashSet<&str> = computers.paired.iter().map(|computer| computer.link.host.as_str()).collect();
    for computer in &computers.paired {
        let link = &computer.link;
        let upkept = upkeep.iter().find(|item| item.id == link.id);
        let editing = root.settings.devices.renaming.as_ref().is_some_and(|(id, _)| *id == link.id);
        let title: AnyElement = if editing {
            let input = root.settings.devices.renaming.as_ref().map(|(_, held)| held.input.clone()).expect("checked");
            div()
                .w(px(212.0))
                .h(px(24.0))
                .flex()
                .items_center()
                .px(px(6.0))
                .rounded(px(6.0))
                .border_1()
                .border_color(t.line_strong)
                .bg(t.surface_input)
                .text_color(t.ink)
                .text_size(px(14.0))
                .on_key_down(cx.listener(|root, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        root.settings.devices.renaming = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(input)
                .into_any_element()
        } else {
            let id = link.id.clone();
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap_x(px(8.0))
                .min_w_0()
                .child(div().id(SharedString::from(format!("computer-title-{}", link.id))).cursor_text().text_color(t.ink_dim).text_size(px(14.0)).font_weight(FontWeight(570.0)).map(|title| {
                    // A dotted underline shows the name can be renamed while it or Rename is pointed at.
                    fn dotted<S: Styled>(style: S, color: gpui::Hsla) -> S {
                        style.underline().text_decoration_dotted().text_underline_offset(px(4.0)).text_decoration_color(color)
                    }
                    let lit = t.quiet_strong;
                    dotted(title, gpui::transparent_black())
                        .transition(gpui::Transition::ease(120).only(&[gpui::TransitionProperty::Decoration]))
                        .hover(move |style| dotted(style, lit.into()))
                        .group_hover(SharedString::from(format!("computer-rename-{}", link.id)), move |style| dotted(style, lit.into()))
                }).child(link.name.clone()).on_click(cx.listener(move |root, event: &gpui::ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        start_rename(root, &id, window, cx);
                    }
                })))
                .when(link.host != link.name, |title| title.child(div().min_w_0().text_color(t.quiet_strong).font_family(mono.clone()).text_size(px(12.0)).child(link.host.clone())))
                .into_any_element()
        };
        let parts = upkept.map(|upkeep| upkeep.status.clone()).unwrap_or_else(|| {
            if link.status == ComputerStatus::Connected {
                vec![UpkeepPart { text: "Connected".into(), tone: Some(UpkeepTone::Live) }]
            } else {
                let away = UpkeepPart { text: if link.status == ComputerStatus::Connecting { "Reconnecting…".into() } else { "Offline".into() }, tone: None };
                match &link.error {
                    Some(error) => vec![away, UpkeepPart { text: error.clone(), tone: None }],
                    None => vec![away],
                }
            }
        });
        let mut row = Row::titled(title).lead(glyph(IconName::Monitor, t.muted)).describe(status_line(&parts, &t)).action_column(150.0, 6.0);
        let armed = marks.armed(&format!("forget-{}", link.id));
        let asking = armed.get();
        if link.status != ComputerStatus::Connected && upkept.is_none() && !asking {
            let id = link.id.clone();
            row = row.action(Button::new(SharedString::from(format!("reconnect-{}", link.id)), "Reconnect").on_click(move |_, _, cx| dispatch(AppCommand::ComputersReconnect { id: Some(id.clone()) }, cx)));
        }
        if let Some(action) = upkept.and_then(|upkeep| upkeep.action.clone()).filter(|_| !asking) {
            let command = action.command.clone();
            row = row.action(Button::new(SharedString::from(format!("upkeep-action-{}", link.id)), action.label.clone()).on_click(move |_, _, cx| dispatch(command.clone(), cx)));
        }
        let id = link.id.clone();
        if !asking {
        row = row.action(Button::new(SharedString::from(format!("rename-{}", link.id)), "").group(SharedString::from(format!("computer-rename-{}", link.id))).icon(IconName::Pencil).icon_size(16.0).tip("Rename").name(format!("Rename {}", link.name)).disabled(editing).on_click(cx.listener(move |root, _, window, cx| start_rename(root, &id, window, cx))));
        }
        let id = link.id.clone();
        row = row.action(ConfirmAction::new(SharedString::from(format!("forget-{}", link.id)), "", "Remove", move |_, cx| dispatch(AppCommand::ComputersForget { id: id.clone() }, cx)).name(format!("Remove {}", link.name)).variant(ButtonVariant::Danger).icon(IconName::Trash2).tip("Remove").armed_into(&armed));
        if let Some(items) = upkept.and_then(|upkeep| software(&upkeep.items, &t)) {
            row = row.footer(items);
        }
        section = section.row(row.build(marks, &t));
    }

    let pairing = computers.pairing.clone();
    let offered: Vec<_> = computers.found.iter().filter(|computer| !paired_hosts.contains(computer.host.as_str())).cloned().collect();
    for computer in &offered {
        let mut row = Row::new(computer.name.clone(), &t).lead(glyph(IconName::Monitor, t.muted)).line(format!("{}{}", computer.host, if computer.os.is_empty() { String::new() } else { format!(" · {}", computer.os) }), &t);
        if pairing.as_ref().is_some_and(|pairing| pairing.host == computer.host) {
            row = row.describe(code_entry(root, pairing.as_ref().expect("checked"), cx));
        } else {
            let (host, name) = (computer.host.clone(), computer.name.clone());
            row = row.action(Button::new(SharedString::from(format!("pair-{}", computer.host)), "Pair").disabled(pairing.as_ref().is_some_and(|pairing| pairing.busy)).on_click(move |_, _, cx| dispatch(AppCommand::ComputersPair { host: host.clone(), name: name.clone(), code: String::new() }, cx)));
        }
        section = section.row(row.build(marks, &t));
    }

    if root.settings.devices.address.is_none() {
        root.settings.devices.address = Some(Held::new("", "host:port", |root, input, event, cx| match event {
            InputEvent::Changed => root.settings.devices.address_invalid = false,
            InputEvent::Submit => submit_address(root, &input.read(cx).text().to_string(), cx),
            _ => {}
        }, cx));
    }
    let address_input = root.settings.devices.address.as_ref().map(|held| held.input.clone()).expect("made above");
    let busy = pairing.as_ref().is_some_and(|pairing| pairing.busy);
    if address_input.read(cx).is_disabled() != busy {
        address_input.update(cx, |input, cx| input.set_disabled(busy, cx));
    }
    let typed = address_input.read(cx).text().trim().to_string();
    let submit = address_input.clone();
    let mut address = div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_end()
                .gap(px(8.0))
                .child(div().flex().flex_col().gap(px(7.0)).child(div().text_color(t.ink).text_size(px(17.0)).child("Or pair by address")).child(div().w(px(232.0)).child(TextField::new("computer-address", &address_input).mono(true).invalid(root.settings.devices.address_invalid))))
                .child(Button::new("computer-address-pair", "Pair").disabled(typed.is_empty() || busy).on_click(cx.listener(move |root, _, _, cx| {
                    let text = submit.read(cx).text().to_string();
                    submit_address(root, &text, cx);
                }))),
        );
    if root.settings.devices.address_invalid {
        address = address.child(div().mt(px(6.0)).text_color(t.danger_soft).text_size(px(14.0)).child("Not a valid address"));
    }
    if let Some(pairing) = pairing.as_ref().filter(|pairing| !offered.iter().any(|computer| computer.host == pairing.host)) {
        address = address.child(code_entry(root, pairing, cx));
    }
    section = section.row(Row::titled(address.into_any_element()).lead(glyph(IconName::Plus, t.muted)).build(marks, &t));

    if let Some(message) = &computers.search_error {
        section = section.row(error(message.clone(), &t));
    } else if !searching && offered.is_empty() && computers.paired.is_empty() {
        section = section.row(empty("No other computer on your tailnet is serving AI Coding Tool.", &t));
    }
    section.build(marks, &t)
}

fn submit_address(root: &mut Root, typed: &str, cx: &mut Context<Root>) {
    match computer_address(typed) {
        Some(host) => dispatch(AppCommand::ComputersPair { host: host.clone(), name: host, code: String::new() }, cx),
        None => root.settings.devices.address_invalid = true,
    }
    cx.notify();
}

fn start_rename(root: &mut Root, id: &str, window: &mut Window, cx: &mut Context<Root>) {
    let state = root.state(cx);
    let Some(computer) = state.computers.paired.iter().find(|computer| computer.link.id == id) else { return };
    let link_id = id.to_string();
    let held = Held::new(&computer.link.name, &computer.link.name, move |root, input, event, cx| {
        if matches!(event, InputEvent::Submit | InputEvent::Blur) {
            if root.settings.devices.renaming.as_ref().is_some_and(|(id, _)| *id == link_id) {
                let name = input.read(cx).text().trim().to_string();
                root.settings.devices.renaming = None;
                dispatch(AppCommand::ComputersLabel { id: link_id.clone(), name }, cx);
            }
        }
    }, cx);
    root.pending_focus = Some(held.input.clone());
    let _ = window;
    root.settings.devices.renaming = Some((id.to_string(), held));
    cx.notify();
}

/// The code the other computer printed, typed here, which is the whole of pairing.
fn code_entry(root: &mut Root, pairing: &act_core::domain::platform::ComputerPairing, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let host = pairing.host.clone();
    let name = pairing.name.clone();
    let fresh = root.settings.devices.code.as_ref().is_none_or(|(key, _)| *key != host);
    let submit_host = host.clone();
    let submit_name = name.clone();
    let input = held_field(&mut root.settings.devices.code, &host, "", "XXXXXXXX", move |root, input, event, cx| {
        // Shown in capitals, as the code is printed.
        if *event == InputEvent::Changed {
            let text = input.read(cx).text().to_string();
            let upper = text.to_uppercase();
            if upper != text {
                input.update(cx, |input, cx| input.set_text(&upper, cx));
            }
        }
        if *event == InputEvent::Submit {
            let code = input.read(cx).text().trim().to_uppercase();
            let busy = root.state(cx).computers.pairing.as_ref().is_some_and(|pairing| pairing.busy);
            if code.chars().count() >= PAIRING_CODE_LENGTH && !busy {
                dispatch(AppCommand::ComputersPair { host: submit_host.clone(), name: submit_name.clone(), code }, cx);
            }
        }
    }, cx);
    if fresh {
        root.pending_focus = Some(input.clone());
    }
    if input.read(cx).is_disabled() != pairing.busy {
        input.update(cx, |input, cx| input.set_disabled(pairing.busy, cx));
    }
    let code = input.read(cx).text().trim().to_uppercase();
    let ready = code.chars().count() >= PAIRING_CODE_LENGTH && !pairing.busy;
    let mut entry = div()
        .mt(px(10.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .child(div().flex().flex_col().gap(px(5.0)).child(div().text_color(t.muted).text_size(px(12.0)).child(format!("Code shown by {name}"))).child(div().w(px(130.0)).font_weight(FontWeight(600.0)).letter_spacing(0.12).child(TextField::new("computer-code", &input).mono(true))))
        .child(
            div()
                .flex()
                .gap(px(8.0))
                .child(Button::new("computer-code-pair", if pairing.busy { "Pairing…" } else { "Pair" }).disabled(!ready).on_click(move |_, _, cx| dispatch(AppCommand::ComputersPair { host: host.clone(), name: name.clone(), code: code.clone() }, cx)))
                .child(Button::new("computer-code-cancel", "Cancel").disabled(pairing.busy).on_click(|_, _, cx| dispatch(AppCommand::ComputersCancelPairing, cx))),
        );
    if let Some(message) = &pairing.error {
        entry = entry.child(error(message.clone(), &t));
    }
    entry.into_any_element()
}

fn tailscale_section(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let remote = &state.remote;
    let on = remote.enabled && remote.status == MobileServerStatus::Listening;
    let steps = tailscale_steps(remote, on);
    let needed = steps.iter().find(|step| !step.done && step.hint.is_some() && !(step.id == "serving" && !on)).map(|step| step.id);
    let checking = state.remote_checking;
    let mut section = Section::setting("phone.tailscale")
        .description(description("The phone reaches this computer over your tailnet, so both need Tailscale signed into the same account."))
        .action(spinning_button("tailscale-check", if checking { "Checking…" } else { "Check again" }, IconName::RefreshCw, checking).disabled(checking).on_click(|_, _, cx| dispatch(AppCommand::RemoteRefresh, cx)));
    let address = remote.addresses.iter().find(|address| address.kind == MobileAddressKind::TailscaleHttps);
    for step in &steps {
        let is_needed = Some(step.id) == needed;
        let title = div()
            .text_size(px(14.0))
            .when(step.done, |title| title.text_color(t.ink_dim).font_weight(FontWeight(570.0)))
            .when(!step.done, |title| title.text_color(t.quiet_strong).font_weight(FontWeight(500.0)))
            .child(step.name.clone())
            .into_any_element();
        let mut row = Row::titled(title).compact().status(step.done, &t);
        if is_needed {
            row = row.mark(NEEDED_MARK);
        }
        if step.done && step.id == "serving" {
            if let Some(address) = address {
                row = row.describe(div().mt(px(3.0)).flex().child(div().px(px(5.0)).py(px(1.0)).rounded(px(5.0)).bg(t.wash).text_color(t.ink_dim).font_family(mono.clone()).text_size(px(12.0)).clip().child(address_origin(address))));
            }
        }
        if !step.done {
            if let Some(hint) = step.hint {
                row = row.describe(div().mt(px(3.0)).text_color(if is_needed { t.accent_soft } else { t.quiet_strong }).text_size(px(12.0)).line_height(gpui::relative(1.45)).child(hint));
            }
        }
        if let (true, Some((label, href))) = (is_needed, step.link) {
            row = row.action(Button::new(SharedString::from(format!("tailscale-link-{}", step.id)), label).on_click(move |_, _, cx| dispatch(AppCommand::LinkOpen { url: href.to_string() }, cx)));
        }
        section = section.row(row.build(marks, &t));
    }
    if let Some(message) = &remote.tailscale.error {
        section = section.row(error(message.clone(), &t));
    }
    let _ = (root, window);
    section.build(marks, &t)
}

fn pairing_section(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let remote = &state.remote;
    let ready = remote.enabled && remote.status == MobileServerStatus::Listening && reachable(remote);
    let now = act_core::util::now();
    let remaining = remote.pairing.as_ref().map(|pairing| (pairing.expires_at - now).max(0)).unwrap_or(0);
    let live = remote.pairing.is_some() && remaining > 0;
    if ready && !live {
        let key = remote.pairing.as_ref().map(|pairing| pairing.code.clone()).unwrap_or_else(|| "none".into());
        if root.settings.devices.asked.as_deref() != Some(key.as_str()) {
            root.settings.devices.asked = Some(key);
            dispatch(AppCommand::RemoteCreatePairingCode, cx);
        }
    }
    if remote.pairing.is_some() && root.settings.devices.ticker.is_none() {
        root.settings.devices.ticker = Some(cx.spawn(async |root, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if root.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }));
    } else if remote.pairing.is_none() {
        root.settings.devices.ticker = None;
    }
    let mut section = Section::setting("phone.pairing").description(description("Scan the code with the phone's camera. Each code works once and lasts two minutes.")).action(
        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(Button::new("phone-get-app", "Get the app").on_click(cx.listener(|root, _, _, cx| {
                root.settings.devices.getting_app = true;
                cx.notify();
            })))
            .child(Button::new("phone-new-code", "New code").track_focus(&marks.stop("phone.pairing", cx)).disabled(!ready).on_click(|_, _, cx| dispatch(AppCommand::RemoteCreatePairingCode, cx))),
    );
    if !ready {
        section = section.row(empty("Turn phone access on and finish the Tailscale steps to pair a phone.", &t));
    } else if let Some(pairing) = &remote.pairing {
        let image = qr(&mut root.settings.devices.qr, &pairing.url, 176.0, !live, &t);
        let mut copy = div()
            .min_w_0()
            .flex()
            .flex_col()
            .child(div().text_color(t.ink).font_family(mono.clone()).text_size(px(26.0)).font_weight(FontWeight(640.0)).line_height(gpui::relative(1.2)).letter_spacing(0.16).child(pairing.code.clone()));
        copy = if live {
            copy.child(div().mt(px(7.0)).text_color(t.quiet_strong).text_size(px(12.0)).child(format!("Expires in {}.", countdown(remaining))))
        } else {
            copy.child(div().mt(px(7.0)).text_color(t.accent_soft).text_size(px(12.0)).child("This code has expired. Making a new one…"))
        };
        copy = copy.child(div().mt(px(11.0)).flex().items_center().gap(px(6.0)).min_w_0().child(div().min_w_0().px(px(5.0)).py(px(1.0)).rounded(px(5.0)).bg(t.wash).text_color(t.ink_dim).font_family(mono).text_size(px(12.0)).clip().child(pairing.url.clone())).child(copy_button("pairing-copy", pairing.url.clone(), "Copy pairing link", None, window, cx)));
        section = section.row(div().flex().items_center().gap(px(24.0)).border_t_1().border_color(t.line).py(px(20.0)).child(image).child(copy));
    } else {
        section = section.row(empty("Making a code…", &t));
    }
    section.build(marks, &t)
}

fn phones_section(state: &WorkspaceState, marks: &Marks, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let remote = &state.remote;
    let devices: Vec<&PairedDeviceView> = remote.devices.iter().filter(|device| device.kind == PairedDeviceKind::Phone).collect();
    let connected = devices.iter().filter(|device| device_connection(device, remote) == Some(MobileConnectionState::Live)).count();
    let summary = if devices.is_empty() { "None paired".to_string() } else { format!("{connected} of {} connected", devices.len()) };
    let mut section = Section::setting("phone.phones")
        .description(description("A connected phone keeps this computer awake. Closing this window stops phone access. Removing a phone cuts it off at once."))
        .action(div().text_color(t.quiet_strong).text_size(px(12.0)).whitespace_nowrap().child(summary));
    if devices.is_empty() {
        section = section.row(empty("No phone has paired yet.", &t));
    }
    let now = act_core::util::now();
    for device in devices {
        let connection = device_connection(device, remote);
        let live = connection == Some(MobileConnectionState::Live);
        let id = device.id.clone();
        section = section.row(
            Row::new(device.name.clone(), &t)
                .lead(status(live, true, Some(if live { IconName::Check } else { IconName::Smartphone }), &t))
                .describe(div().mt(px(5.0)).text_size(px(12.0)).line_height(gpui::relative(1.45)).text_color(if live { t.success_soft } else { t.quiet_strong }).child(device_label(device, connection, now)))
                .action(ConfirmAction::new(SharedString::from(format!("phone-remove-{}", device.id)), "Remove", format!("Remove {}", device.name), move |_, cx| dispatch(AppCommand::RemoteRevokeDevice { device_id: id.clone() }, cx)).name(format!("Remove {}", device.name)).variant(ButtonVariant::Danger))
                .build(marks, &t),
        );
    }
    section.build(marks, &t)
}

/// The dialog over the whole window with the TestFlight link as a QR, while it is open.
pub fn overlay(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) -> Option<AnyElement> {
    root.settings.devices.getting_app.then(|| get_app_dialog(root, window, cx))
}

fn get_app_dialog(root: &mut Root, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let mono = cx.theme().typography.mono_family.clone();
    let close = cx.listener(|root: &mut Root, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Root>| {
        root.settings.devices.getting_app = false;
        cx.notify();
    });
    let entity = cx.entity().downgrade();
    let image = qr(&mut root.settings.devices.qr, TESTFLIGHT_LINK, 208.0, false, &t);
    Dialog::new("phone-get-app-dialog", move |_, cx| {
        let _ = entity.update(cx, |root, cx| {
            root.settings.devices.getting_app = false;
            cx.notify();
        });
    })
    .title("Get the iPhone app")
    .width(px(380.0))
    .child(dialog_paragraph("Scan with the iPhone's camera to install the beta from TestFlight.", cx))
    .child(div().flex().justify_center().child(image))
    .child(
        div()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.0))
            .child(div().px(px(5.0)).py(px(1.0)).rounded(px(5.0)).bg(t.wash).text_color(t.ink_dim).font_family(mono).text_size(px(12.0)).child(TESTFLIGHT_LINK.trim_start_matches("https://")))
            .child(copy_button("testflight-copy", TESTFLIGHT_LINK.to_string(), "Copy TestFlight link", None, window, cx)),
    )
    .action(Button::new("phone-get-app-done", "Done").size(ButtonSize::Lg).on_click(close))
    .into_any_element()
}

pub fn render(root: &mut Root, state: &WorkspaceState, marks: &Marks, window: &mut Window, cx: &mut Context<Root>) -> AnyElement {
    let t = cx.theme().tokens.clone();
    let remote = &state.remote;
    let listening = remote.enabled && remote.status == MobileServerStatus::Listening;
    let ready = listening && reachable(remote);
    let computers = computers_section(root, state, marks, window, cx);
    let badge: AnyElement = if listening && !ready {
        div()
            .id("phone-status-jump")
            .flex()
            .items_center()
            .gap(px(5.0))
            .px(px(9.0))
            .py(px(5.0))
            .rounded_full()
            .bg(t.accent_surface)
            .text_color(t.accent_soft)
            .text_size(px(12.0))
            .font_weight(FontWeight(600.0))
            .cursor_pointer()
            .hover(|style| style.text_color(t.accent_text))
            .child(status_label(remote))
            .child(ink_icon(IconName::ArrowDown, 12.0))
            .on_click(cx.listener(|root, _, window, cx| {
                root.settings.devices.shown_step += 1;
                cx.on_next_frame(window, |root, _, cx| {
                    root.settings.scroll_to_mark(NEEDED_MARK);
                    cx.notify();
                });
                cx.notify();
            }))
            .into_any_element()
    } else {
        pill(status_label(remote), status_tone(remote), &t)
    };
    let enabled = remote.enabled;
    let mut access = Section::new("Access").badge(badge).row(
        Row::setting("phone.availability", &t)
            .line("Lets the iPhone app reach this computer through Tailscale. Turning it off drops every phone.", &t)
            .on_title_click(move |_, cx| dispatch(AppCommand::RemoteSetEnabled { enabled: !enabled }, cx))
            .action(Switch::new("phone-access-switch", enabled, "Phone access").track_focus(&marks.stop("phone.availability", cx)).on_change(|value: &bool, _, cx| dispatch(AppCommand::RemoteSetEnabled { enabled: *value }, cx)))
            .build(marks, &t),
    );
    if let Some(message) = &remote.error {
        access = access.row(error(message.clone(), &t));
    }
    let tailscale = tailscale_section(root, state, marks, window, cx);
    let pairing = pairing_section(root, state, marks, window, cx);
    let phones = phones_section(state, marks, cx);
    let body = page("Devices", Some(description("Your threads from the iPhone app or another computer, anywhere your tailnet reaches.")), None, window, cx)
        .child(computers)
        .child(access.build(marks, &t))
        .child(tailscale)
        .child(pairing)
        .child(phones);
    body.into_any_element()
}
