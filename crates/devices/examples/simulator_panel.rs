//! The Simulator panel on its own, driving a real device: it lists the Mac's simulators, boots one,
//! streams its screen, and sends what the panel's controls ask for. A tiny stand-in for the workspace
//! reducer keeps the panel's state.
//!
//! `cargo run -p act_devices --example simulator_panel -- [--device "iPhone 16 Pro"] [--rotate]
//!  [--tap X,Y] [--menu] [--quit-after SECONDS]`
//!
//! Quitting shuts down a device this example booted.

use std::rc::Rc;
use std::time::Duration;

use act_core::domain::platform::{default_simulator, SimulatorDeviceState, SimulatorInput, SimulatorOrientation, SimulatorState};
use act_core::AppCommand;
use act_devices::simulator::{SimulatorHost, SimulatorHostOptions, SimulatorPanel};
use act_ui::theme::Environment;
use act_ui::{Assets, Theme};
use futures::StreamExt;
use gpui::{div, prelude::*, px, size, App, Application, Bounds, Context, Entity, Window, WindowBounds, WindowOptions};

enum Update {
    Command(AppCommand),
    Listed(SimulatorState),
    Booted(String),
    Failed(String, String),
    Turned(String, SimulatorOrientation),
}

struct Example {
    panel: Entity<SimulatorPanel>,
    state: SimulatorState,
    udid: Option<String>,
    wanted: Option<String>,
    host: SimulatorHost,
    sender: futures::channel::mpsc::UnboundedSender<Update>,
    _task: gpui::Task<()>,
}

impl Example {
    fn refresh(&self) {
        let host = self.host.clone();
        let sender = self.sender.clone();
        act_host::runtime().spawn(async move {
            let (availability, devices) = host.list().await.unwrap_or_else(|reason| (act_core::domain::platform::SimulatorAvailability::Unsupported { reason }, Vec::new()));
            let _ = sender.unbounded_send(Update::Listed(SimulatorState { availability, devices, ..Default::default() }));
        });
    }

    fn boot(&mut self, udid: String) {
        self.udid = Some(udid.clone());
        if self.state.devices.iter().any(|device| device.udid == udid && device.state == SimulatorDeviceState::Booted) {
            return;
        }
        self.state.booting.push(udid.clone());
        self.state.errors.shift_remove(&udid);
        let host = self.host.clone();
        let sender = self.sender.clone();
        act_host::runtime().spawn(async move {
            let update = match host.boot(&udid).await {
                Ok(()) => Update::Booted(udid),
                Err(message) => Update::Failed(udid, message),
            };
            let _ = sender.unbounded_send(update);
        });
    }

    fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        match update {
            Update::Command(AppCommand::SimulatorSelect { udid }) => self.boot(udid),
            Update::Command(AppCommand::SimulatorRefresh) => self.refresh(),
            Update::Command(AppCommand::SimulatorInput { udid: Some(udid), input, .. }) => {
                let host = self.host.clone();
                let sender = self.sender.clone();
                let sent = host.input(&udid, input.clone());
                act_host::runtime().spawn(async move {
                    match sent.await {
                        Ok(()) => {
                            if let SimulatorInput::Rotate { orientation } = input {
                                let _ = sender.unbounded_send(Update::Turned(udid, orientation));
                            }
                        }
                        Err(message) => eprintln!("simulator refused: {message}"),
                    }
                });
            }
            Update::Command(_) => {}
            Update::Listed(listed) => {
                let booting = std::mem::take(&mut self.state.booting);
                let errors = std::mem::take(&mut self.state.errors);
                let orientations = std::mem::take(&mut self.state.orientations);
                self.state = SimulatorState { booting, errors, orientations, ..listed };
                if self.udid.is_none() {
                    let wanted = self.wanted.clone();
                    let picked = wanted
                        .as_deref()
                        .and_then(|wanted| self.state.devices.iter().find(|device| device.name == wanted || device.udid == wanted))
                        .or_else(|| default_simulator(&self.state.devices, None))
                        .map(|device| device.udid.clone());
                    if let Some(udid) = picked {
                        self.boot(udid);
                    }
                }
            }
            Update::Booted(udid) => {
                self.state.booting.retain(|item| item != &udid);
                self.refresh();
            }
            Update::Failed(udid, message) => {
                self.state.booting.retain(|item| item != &udid);
                self.state.errors.insert(udid, message);
            }
            Update::Turned(udid, orientation) => {
                self.state.orientations.insert(udid, orientation);
            }
        }
        let (state, udid) = (self.state.clone(), self.udid.clone());
        self.panel.update(cx, |panel, cx| panel.set(state, udid, true, cx));
        cx.notify();
    }
}

impl Render for Example {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        act_ui::root(cx)
            .capture_any_mouse_down(|_, _, cx| act_ui::focus::note_pointer(cx))
            .child(div().size_full().child(self.panel.clone()))
    }
}

fn argument(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let wanted = argument(&args, "--device").or_else(|| Some("iPhone 16 Pro".into()));
    let rotate = args.iter().any(|arg| arg == "--rotate");
    let open_menu = args.iter().any(|arg| arg == "--menu");
    let read = args.iter().any(|arg| arg == "--read");
    let tap = argument(&args, "--tap").and_then(|value| {
        let (x, y) = value.split_once(',')?;
        Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))
    });
    let quit_after = argument(&args, "--quit-after").and_then(|value| value.parse::<u64>().ok());
    let host = {
        let _guard = act_host::runtime().enter();
        SimulatorHost::new(SimulatorHostOptions::default())
    };
    let quitting = host.clone();

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        act_ui::init(cx);
        Theme::set_environment(cx, Environment::macos_glass());
        let bounds = Bounds::centered(None, size(px(730.0), px(960.0)), cx);
        let host_for_window = host.clone();
        let window: gpui::WindowHandle<Example> = cx
            .open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, |window, cx| {
                cx.new(|cx: &mut Context<Example>| {
                    let (sender, mut receiver) = futures::channel::mpsc::unbounded::<Update>();
                    let commands = sender.clone();
                    let on_command: act_devices::simulator::CommandHandler = Rc::new(move |command, _, _| {
                        let _ = commands.unbounded_send(Update::Command(command));
                    });
                    let panel = cx.new(|cx| SimulatorPanel::new(host_for_window.clone(), on_command, window, cx));
                    let task = cx.spawn(async move |this, cx| {
                        while let Some(update) = receiver.next().await {
                            if this.update(cx, |this, cx| this.apply(update, cx)).is_err() {
                                break;
                            }
                        }
                    });
                    let example = Example { panel, state: SimulatorState::default(), udid: None, wanted: wanted.clone(), host: host_for_window.clone(), sender, _task: task };
                    example.refresh();
                    example
                })
            })
            .unwrap();
        cx.activate(true);

        let quit_host = quitting.clone();
        cx.on_app_quit(move |_| {
            quit_host.stop_with_window();
            async {}
        })
        .detach();

        cx.spawn(async move |cx| {
            let executor = cx.background_executor().clone();
            let timer = |seconds: u64| executor.timer(Duration::from_secs(seconds));
            // Gestures the flags ask for, once the device has had time to boot and stream.
            if rotate || tap.is_some() || open_menu || read {
                timer(25).await;
                let _ = window.update(cx, |example, window, cx| {
                    let Some(udid) = example.udid.clone() else { return };
                    if read {
                        // What a run's simulator_read answers with.
                        let host = example.host.clone();
                        let device = example.state.devices.iter().find(|device| device.udid == udid).cloned();
                        act_host::runtime().spawn(async move {
                            let Some(device) = device else { return };
                            match act_devices::simulator::reads::snapshot(&host, &device, true).await {
                                Ok(result) => println!("{}", act_host::tools::simulator::read_text(&result)),
                                Err(error) => println!("read failed: {error}"),
                            }
                        });
                    }
                    if open_menu {
                        example.panel.update(cx, |panel, cx| panel.open_device_menu(window, cx));
                    }
                    if let Some((x, y)) = tap {
                        example.apply(Update::Command(AppCommand::SimulatorInput { task_id: None, udid: Some(udid.clone()), input: SimulatorInput::Tap { x, y, points: None } }), cx);
                    }
                    if rotate {
                        example.apply(
                            Update::Command(AppCommand::SimulatorInput { task_id: None, udid: Some(udid), input: SimulatorInput::Rotate { orientation: SimulatorOrientation::LandscapeRight } }),
                            cx,
                        );
                    }
                });
            }
            if let Some(seconds) = quit_after {
                timer(seconds).await;
                let _ = cx.update(|cx| cx.quit());
            }
        })
        .detach();
    });
}
