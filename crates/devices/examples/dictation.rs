//! Dictation on its own.
//!
//! `cargo run -p act_devices --example dictation -- --transcribe speech.wav [--helper PATH] [--models DIR]`
//! hands a WAV to the speech helper, as a phone's recording is, and prints the words.
//!
//! `cargo run -p act_devices --example dictation -- [--phase recording|transcribing] [--hands-free] [--away]`
//! draws the composer bar's microphone and level strip, fed with a made-up voice so no microphone is opened.

use std::rc::Rc;
use std::time::Duration;

use act_core::domain::platform::{DictationModel, DictationPhase};
use act_devices::dictation::levels::DictationLevels;
use act_devices::dictation::ui::{ComposerDictation, DictationStrip, MicButton, Talk};
use act_devices::dictation::{DictationHost, DictationHostOptions};
use act_ui::theme::{ActiveTheme, Environment};
use act_ui::{Assets, Theme};
use base64::Engine;
use gpui::{div, prelude::*, px, size, App, Bounds, Context, Entity, Window, WindowBounds, WindowOptions};

fn argument(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|arg| arg == name).and_then(|at| args.get(at + 1)).cloned()
}

fn transcribe(file: &str, helper: Option<String>, models: Option<String>) {
    let runtime = act_host::runtime();
    let mut options = DictationHostOptions::desktop();
    if let Some(helper) = helper {
        options.helper = Some(helper.into());
    }
    if let Some(models) = &models {
        options.models = Some(models.into());
        // A folder of the ONNX export is checked against the ONNX files.
        if options.helper.as_ref().is_some_and(|helper| helper.ends_with("aic-dictation-onnx")) {
            options.place = Some(Some(act_devices::dictation::models::DictationModelPlace {
                folder: models.into(),
                files: act_devices::dictation::models::ONNX_MODEL_FILES.to_vec(),
            }));
        }
    }
    let host = {
        let _guard = runtime.enter();
        DictationHost::new(options)
    };
    let _status = host.on_model(|status| println!("model: {:?}{}", status.model, status.message.as_deref().map(|message| format!(" ({message})")).unwrap_or_default()));
    let audio = base64::engine::general_purpose::STANDARD.encode(std::fs::read(file).expect("the WAV file reads"));
    let started = std::time::Instant::now();
    match runtime.block_on(host.transcribe_audio(&audio)) {
        Ok(text) => println!("text ({:.1}s): {text}", started.elapsed().as_secs_f32()),
        Err(error) => println!("failed: {error}"),
    }
    host.stop_helper();
}

struct Composer {
    mic: Entity<MicButton>,
    levels: Entity<DictationLevels>,
    phase: Option<DictationPhase>,
    hands_free: bool,
    away: bool,
    log: Vec<String>,
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme().tokens.clone();
        let strip = self.phase.map(|phase| DictationStrip::new(phase, self.away || self.mic.read(cx).away(), self.hands_free, self.levels.clone()));
        act_ui::root(cx).p(px(40.0)).flex().flex_col().gap(px(16.0)).child(
            div()
                .w(px(700.0))
                .rounded(px(18.0))
                .border_1()
                .border_color(if self.away { t.danger_line } else if self.phase.is_some() { t.accent_line } else { t.line })
                .bg(t.surface)
                .p(px(12.0))
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(div().px(px(8.0)).text_color(t.quiet).child("Ask Claude anything"))
                .child(div().flex().items_center().gap(px(8.0)).children(strip).when(self.phase.is_none(), |bar| bar.child(div().flex_1())).child(self.mic.clone())),
        )
        .child(div().text_color(t.muted).text_size(px(12.0)).children(self.log.iter().map(|line| div().child(line.clone()))))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(file) = argument(&args, "--transcribe") {
        transcribe(&file, argument(&args, "--helper"), argument(&args, "--models"));
        return;
    }
    let phase = match argument(&args, "--phase").as_deref() {
        Some("transcribing") => Some(DictationPhase::Transcribing),
        Some("none") => None,
        _ => Some(DictationPhase::Recording),
    };
    let hands_free = args.iter().any(|arg| arg == "--hands-free");
    let away = args.iter().any(|arg| arg == "--away");
    let host = {
        let _guard = act_host::runtime().enter();
        DictationHost::new(DictationHostOptions::desktop())
    };
    gpui_platform::application().with_assets(Assets).run(move |cx: &mut App| {
        act_ui::init(cx);
        Theme::set_environment(cx, Environment::macos_glass());
        let bounds = Bounds::centered(None, size(px(780.0), px(260.0)), cx);
        cx.open_window(WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() }, |_, cx| {
            cx.new(|cx: &mut Context<Composer>| {
                let levels = cx.new(|cx| DictationLevels::new(&host, cx));
                let this = cx.entity().downgrade();
                let on_talk = Rc::new(move |talk: Talk, _: &mut Window, cx: &mut App| {
                    let _ = this.update(cx, |composer, cx| {
                        composer.log.push(format!("{:?} → {:?}", talk, talk.command()));
                        cx.notify();
                    });
                });
                let mic = cx.new(|cx| MicButton::new(levels.clone(), on_talk, cx));
                mic.update(cx, |mic, cx| mic.set(ComposerDictation { model: DictationModel::Ready, phase, hands_free, binding: Some("⌥Space".into()) }, false, cx));
                // A made-up voice: syllables rising and falling, so the strip and the ring move.
                let feeding = levels.clone();
                cx.spawn(async move |_, cx| {
                    let mut tick = 0u32;
                    loop {
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                        tick += 1;
                        let t = tick as f64 * 0.05;
                        let level = (0.45 + 0.4 * (t * 7.0).sin().abs() * (t * 1.3).sin().abs()).min(1.0);
                        feeding.update(cx, |levels, cx| levels.push_level(level, cx));
                    }
                })
                .detach();
                Composer { mic, levels, phase, hands_free, away, log: Vec::new() }
            })
        })
        .unwrap();
        cx.activate(true);
    });
}
