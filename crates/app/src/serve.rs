//! `aic serve` and `aic pair`: the app run headless, every service it has reachable only through the
//! phone bridge, and the pairing code a server hands out (src/main/serve.ts).

use std::sync::Arc;

use act_host::mobile::pairing_socket::{request_pairing, serve_pairing_socket};
use act_host::serve_args::{pairing_line, parse_serve_arguments, ServeCommand};
use act_store::profile::Profile;
use gpui::{App, AppContext};
use serde_json::json;

use crate::effects::computers::BridgeOptions;
use crate::effects::Host;
use crate::workspace::Workspace;

/// Whether the arguments ask for the headless server rather than the window.
pub fn asked(arguments: &[String]) -> bool {
    matches!(arguments.first().map(String::as_str), Some("serve" | "pair"))
}

fn say(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
}

/// Runs `serve` or `pair`; the exit code says how it ended.
pub fn main(arguments: Vec<String>) -> i32 {
    let parsed = parse_serve_arguments(&arguments);
    let packaged = (crate::startup::packaged() && !parsed.dev) || parsed.installed;
    let Some(default) = Profile::resolve_for(packaged) else {
        eprintln!("Could not find the home folder.");
        return 1;
    };
    let profile = match &parsed.user_data {
        Some(folder) => Profile::at(std::path::absolute(folder).unwrap_or_else(|_| folder.into()), default.worktrees_root.clone()),
        None => default,
    };
    let runtime = act_host::runtime();
    if parsed.command == ServeCommand::Pair {
        return match runtime.block_on(request_pairing(&profile.serve_socket())) {
            Ok(offer) => {
                say(pairing_text(&offer));
                0
            }
            Err(error) => {
                eprintln!("No {} is serving here. Start one with `{} serve`. ({error})", act_core::identity::APP_NAME, act_core::identity::CLI_COMMAND);
                1
            }
        };
    }
    runtime.block_on(act_platform::search_path::adopt_login_shell_path());
    if parsed.user_data.is_none() {
        crate::startup::import_electron_profile(&profile, packaged);
    }
    if let Err(error) = profile.ensure() {
        eprintln!("Could not create {}: {error}", profile.user_data.display());
        return 1;
    }
    let lock = match act_host::instance_lock::claim_serve_lock(&profile.user_data) {
        Ok(lock) => lock,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let launched_update = crate::startup::packaged().then(|| act_host::user_data::read_app_launch(&profile.user_data, crate::startup::APP_VERSION)).flatten();
    let Some(services) = crate::startup::open_services(&profile) else { return 1 };
    let local = parsed.local;
    let port = parsed.port;
    gpui_platform::headless().run(move |cx: &mut App| {
        let host = Host::new(profile.clone(), services, cx);
        host.set_headless();
        let workspace = cx.new(|cx| Workspace::new(host.clone(), launched_update.clone(), cx));
        crate::workspace::install(workspace.clone(), cx);
        let quit_host = host.clone();
        let quit_workspace = workspace.clone();
        Workspace::start(&workspace, cx);
        // Browser and computer-use surfaces need a desktop; shells can be viewed from a paired computer.
        for command in [json!({ "type": "view.set-browser-tools", "enabled": false }), json!({ "type": "view.set-computer-use", "enabled": false })] {
            match act_core::AppCommand::from_json(&command) {
                Ok(command) => host.sink.command(command),
                Err(error) => eprintln!("Could not turn off a desktop surface: {error}"),
            }
        }
        host.system.start(&host.sink, crate::startup::packaged(), false);
        // A phone offers to listen once it hears the speech model is ready.
        crate::effects::dictation::host(&host.sink);
        let bridge = BridgeOptions { port, local, say: Some(Arc::new(|line| say(line))) };
        host.computers.start(&host, false, bridge);
        let serving = host.clone();
        act_host::runtime().spawn(async move { announce(serving, local).await });
        let (quit_tx, quit_rx) = futures::channel::oneshot::channel::<&'static str>();
        act_host::runtime().spawn(async move {
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok();
            let reason = tokio::select! {
                _ = tokio::signal::ctrl_c() => "SIGINT",
                _ = async { match terminate.as_mut() { Some(signal) => { signal.recv().await; } None => std::future::pending().await } } => "SIGTERM",
                _ = async { match hangup.as_mut() { Some(signal) => { signal.recv().await; } None => std::future::pending().await } } => "SIGHUP",
            };
            let _ = quit_tx.send(reason);
        });
        cx.spawn(async move |cx| {
            let Ok(reason) = quit_rx.await else { return };
            say(format!("Stopping ({reason})…"));
            let _ = cx.update(|cx| quit_workspace.update(cx, |workspace, _| workspace.flush()));
            quit_host.shutdown_now();
            drop(lock);
            std::process::exit(0);
        })
        .detach();
    });
    0
}

/// The pairing link drawn as a QR for the phone's camera, above the code to type instead. Output that
/// is not a terminal, such as a log, gets the line alone, because the drawing is colour codes there.
pub fn pairing_text(offer: &act_core::domain::mobile::MobilePairingOffer) -> String {
    let line = pairing_line(offer, act_core::util::now());
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return line;
    }
    match terminal_qr(&offer.url) {
        Some(qr) => format!("{qr}\n{line}"),
        None => line,
    }
}

/// Two modules a character, with a one-module light border, as the `qrcode` package's small terminal
/// renderer draws it.
fn terminal_qr(text: &str) -> Option<String> {
    const LINE_SETUP: &str = "\x1b[47m\x1b[30m";
    const RESET: &str = "\x1b[0m";
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let size = code.width() as i32;
    let colors = code.to_colors();
    // 0 outside the border, 1 light, 2 dark.
    let pixel = |x: i32, y: i32| -> u8 {
        if x > size || y > size || x < -1 || y < -1 {
            0
        } else if x >= size || y >= size || x < 0 || y < 0 {
            1
        } else if colors[(y * size + x) as usize] == qrcode::Color::Dark {
            2
        } else {
            1
        }
    };
    let cell = |top: u8, bottom: u8| -> String {
        match (top, bottom) {
            (0, 0) => format!("{RESET} {LINE_SETUP}"),
            (0, 1) => format!("{RESET}\x1b[37m▄{LINE_SETUP}"),
            (0, _) => format!("{RESET}\x1b[30m▄{LINE_SETUP}"),
            (1, 0) => format!("{RESET}\x1b[37m▀{LINE_SETUP}"),
            (1, 1) => " ".into(),
            (1, _) => "▄".into(),
            (_, 0) => format!("{RESET}\x1b[30m▀{LINE_SETUP}"),
            (_, 1) => "▀".into(),
            _ => "█".into(),
        }
    };
    let mut output = String::from(LINE_SETUP);
    let mut y = -1;
    while y < size + 1 {
        for x in -1..=size {
            output.push_str(&cell(pixel(x, y), pixel(x, y + 1)));
        }
        output.push_str(&format!("{RESET}\n{LINE_SETUP}"));
        y += 2;
    }
    output.push_str(RESET);
    Some(output)
}

/// Turns the bridge on, opens the pairing socket `aic pair` asks, and says where to reach it.
async fn announce(host: Arc<Host>, local: bool) {
    let mobile = loop {
        if let Some(mobile) = host.computers.mobile() {
            break mobile;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let pairing = mobile.clone();
    let pair: act_host::mobile::pairing_socket::Pair = Arc::new(move || {
        let mobile = pairing.clone();
        Box::pin(async move { mobile.create_pairing_code() })
    });
    match serve_pairing_socket(&host.profile.serve_socket(), pair).await {
        Ok(stop) => host.computers.hold_pairing_socket(stop),
        Err(error) => say(format!("Could not open the pairing socket: {error}")),
    }
    let state = mobile.set_enabled(true).await;
    host.sink.json(json!({ "type": "remote.changed", "remote": state }));
    let state = mobile.mobile_state();
    if let Some(port) = state.port.filter(|_| matches!(state.status, act_core::domain::mobile::MobileServerStatus::Listening)) {
        say(format!("Listening on port {port}{}", if local { "." } else { ". Waiting for Tailscale to serve it." }));
    }
    if !local && state.tailscale.status == act_core::domain::mobile::TailscaleStatus::Missing {
        say("Tailscale is not installed on this computer, so nothing can reach the bridge.");
    }
    say(format!("{} is serving from {}. Press Ctrl+C to stop.", act_core::identity::APP_NAME, host.profile.user_data.display()));
}
