# AI Coding Tool Preview

Native Rust rewrite of [AI Coding Tool](https://github.com/zyuapp/ai-coding-tool) (Electron + React + TypeScript), checked out beside this repository as `../ai-coding-tool` for the comparison scripts. The UI uses GPUI 0.2 (vendored) and must look and behave like the Electron app.

## Identity
`act_core::identity` holds every name and id that tells this app apart from the Electron app, so the two run side by side: app name, bundle and Linux app id, URL scheme, terminal command, profile folders, checkout roots, update repository, phone bridge port and user agent. The packaging scripts read the same constants. A first launch with an empty profile copies the Electron profile (`act_store::import`, read only) and keeps its checkout roots as the app's own. Promoting the app to replace the Electron one means changing these constants (and the import, which would then read the profile the app itself owns).

Shared with the Electron app on purpose: the MCP server and app plugin names agents see, in-app `aicodingtool://thread/` links in messages, the phone app's `aicodingtool://pair` scheme, the phone bridge's health answer (each copy recognises the other), and git snapshot refs. Only one copy on a machine can hold Tailscale Serve's HTTPS port 443; the other says so.

## Control path
Same as the original:
- Every interaction is an `AppCommand` (`act_core::commands`). Commands from outside the window (agents' MCP tools, paired phones and computers) pass `act_core::validation` first.
- `act_core::reduce(&mut State, Input) -> Vec<Effect>` is the only writer of workspace state.
- The reducer describes external work as `Effect` values (`act_core::effects`).
- The window's `Workspace` entity (`crates/app/src/workspace`) runs inputs through the reducer and hands effects to the host (`crates/app/src/effects`, one module per capability, as `src/host/*-effects.ts`), which answers through `Sink` with `WorkspaceEvent`s. Effects that belong to a view rather than the host come back as `workspace::Surface`.
- Views (`crates/app/src/views`) render state and dispatch commands. They contain no behaviour: what they read from the host goes through `effects::window_desktop`, the counterpart of `main/window-desktop.ts` (branches, commits, commands, diff patches, attachments, terminal screens, and frame calls such as the terminal theme). Anything that writes or opens something is a command.

## Crates
| crate | ports | role |
|---|---|---|
| `act_core` | `src/domain`, `src/contracts`, `src/application` | types, commands, validation, events, effects, reducer, view projections (`view::*`) |
| `act_store` | `src/main/task-database-*`, `json-storage` | SQLite store, schema-compatible with `tasks.v3.sqlite`; profile JSON files |
| `act_engines` | `src/main/{agent,claude,codex,opencode}` | Claude Code, Codex and OpenCode processes and their protocols |
| `act_host` | `src/host`, rest of `src/main` | services effects call: git, worktrees, files, attachments, automations, MCP tools, paired computers, phone bridge |
| `act_terminal` | xterm usage | PTY, VT emulation, GPUI terminal view |
| `act_markdown` | react-markdown + shiki | markdown parsing and GPUI rendering, highlighting, selection |
| `act_ui` | `src/renderer/ui`, `styles/*.css`, `themes.css` | theme tokens, fonts, Lucide icons, primitives, live views and stepped motion |
| `act_browser` | `browser-host.ts`, browser panel | WebKit pages (wry on macOS, WebKitGTK on Linux), site approvals, browser tools, sign-in import |
| `act_devices` | simulator, dictation, glass, capture | iOS simulator panel, dictation, window glass, screenshot context |
| `ai-coding-tool` (`crates/app`) | `src/renderer`, `src/main/main.ts` | window, workspace, effect dispatch, views, CLI (`serve`, `pair`) |

Dependencies point downward only: app → browser/devices/ui/markdown/terminal/host → engines/store → core.

## Shared state
The window holds one `Rc<WorkspaceState>`; the reducer changes it in place. Threads and messages are shared between versions (`act_core::shared::Shared`, copy-on-write), so snapshots (the persistence baseline, the coordination facts taken before an input) copy pointers, not threads. `Workspace::revision` moves on every change and `outline_revision` on every change but text streaming into existing messages; views key what they derive on these and keep only weak handles to the state across updates.

## Drawing
- `Root` renders the window. The sidebar, topbar, composer, transcript and find bars are cached views (`views/parts.rs`), drawn again whenever `Root` notifies, so a scroll lays out and paints only its own part. A change only the transcript shows calls `Root::redraw_transcript` rather than `cx.notify`.
- Streaming: text streaming into the thread on screen (`Surface::Streamed`) redraws only the transcript and find bars; into a thread off screen, nothing. The sidebar model and the transcript's thread names wait for `outline_revision`.
- Live views (`act_ui::components::live`) redraw alone while the rest of the window replays its last frame. Spinners and text sweeps step through `stepped_progress`, waking the window once per step; a caret blinks the same way. An idle window draws no frames.
- The sidebar lays out only the rows near the visible part of each list (`content-visibility: auto`), from a model kept until the state changes. The transcript is a `gpui::List` whose rows not yet measured count at an estimate.
- Long messages (`act_markdown::chunk`, from 12k characters) are drawn in groups of top-level blocks. A group remembers its height per width and is built and laid out only while on screen; elsewhere it is a box of its known height, or of an estimate (its last height reflowed, or its text's bulk) while a few groups a frame are measured off screen. The list keeps the reading place as estimates give way to heights and across resizes: groups offer themselves as scroll anchors (`Window::offer_scroll_anchor`) and report heights that differ from their box (`Window::list_item_resized`).

## Vendored crates
Patched in through `[patch.crates-io]`. `scripts/test-vendor.sh [gpui|taffy]` runs their tests with the features the app builds them with.

`vendor/gpui` is GPUI 0.2.2 with additions:
- Page zoom (`gpui::set_page_zoom`, `Window::set_page_zoom`): layout pixels map to `zoom` window points, as Chromium page zoom does. Border widths snap to whole pixels at the zoom (`snap_border_width`).
- Text: letter spacing (`FontFeatures::tracking`); `LineWrapper::truncate_line` keeps the lines it shaped; a text layout remembers the inherited colour and decorations it was shaped with, so hover, press and focus styles recolour it at paint. Underlines can be dotted or dashed and sit at a set distance below the baseline (`UnderlineStyle::line`, `offset`; `text_decoration_dotted`, `text_underline_offset`).
- Layout: boxes snap where they land, from the root's position (`round_layout_at`); `Window::unrounded_layout_origin` gives where layout placed a node before rounding, so a subtree laid out on its own snaps as it would inside the layout; a scroller's overflow is measured before rounding.
- Lists: rows not yet measured count at an estimate (`ListState::splice_estimated`, `remeasure_items`); `bounds_for_item` includes the top padding; scroll anchoring as a browser does it (`offer_scroll_anchor`, `list_item_resized`): a list lays its items out again within the frame, at most three times, when a shown item's height changed or its anchor moved.
- Live views (`AnyView::cached(..).live()`): notifying one does not dirty its ancestors; cached views around it replay their last frame. Each frame records its cached views (`Frame::views`); the view cache key includes the fade mask, opacity and paint transform.
- Transitions (`InteractiveElement::transition`, `gpui::Transition`): an element with an id eases its fill, border, text colour, opacity, transform and underline colour toward each new value its style takes (hover, press, focus or its own), each property on its own timing (`only`, `and`), asking for frames only while one runs. `enter` eases an element in from a start style when it first appears, as a from-only `@keyframes`. `act_ui::style::touch()` is the page's `--touch-time`/`--touch-ease`.
- Transforms (`Styled::transform`, `ElementTransform`): scale about the centre, translate, and (for glyphs and SVGs) rotate, at paint, as CSS `transform` does: `Window::paint_transform` maps every primitive, content mask, fade mask, hitbox and accessibility bound on the CPU before the scene, so both renderers draw it; hit testing uses the transform last painted. `Window::layout_content_mask` is the mask in layout space, for culling.
- Accessibility (`accessible`, `accessible_name`, `Window::push_access_node`, `with_access_node`): a Chromium-like tree served to VoiceOver on macOS and to AT-SPI on Linux through AccessKit, from one shared tree (`platform::access_tree`). After an assistive press, hover follows the real pointer again.
- Keyboard focus as a browser has it: clickable elements are Tab stops (`skip_tab_stop` opts out), the focus ring shows only for keyboard focus (`focus_visible`), `tab_order` groups, `inert`, `focus_trap`; Enter presses on key down and Space on key up; `keep_focus_on_click`.
- macOS: a zoomed window reports `WindowBounds::Maximized`; a window with nothing to draw stops its display link (`pause_frames`, `frame_waker`); `GPUI_DRAW_OCCLUDED=1` keeps covered windows drawing, for background screenshots.
- Linux: the Blade renderer has the Metal renderer's backdrop passes (hidden, holes, replaced); on X11 a window keeps its focus while an embedded child window holds the keyboard.
- Linux fonts: `.SystemUIFont` is Chromium's Linux `sans-serif` (Arial or a metric-compatible face, else fontconfig's sans-serif), and the generic names `sans-serif`, `serif` and `monospace` resolve through fontconfig.

`vendor/taffy` is taffy 0.9.0: flex gaps stay between items when auto margins take the free space; a `flex: none` item with a negative margin keeps its size in intrinsic sizing; nodes with `snap_origin` sit on whole pixels. `vendor/xattr` is xattr 0.2.3 with `ENODATA` on Linux.

## Linux web views
GTK runs on GPUI's main thread (`act_browser::linux::gtk_loop`). Hidden pages (Mermaid, screenshot marks) live in a GTK offscreen window (`act_browser::hidden`). On X11, browser tabs and visuals are WebKitGTK views in X windows inside GPUI's (`act_browser::linux::embed`); on Wayland they render in GTK offscreen windows whose frames the app paints and to which it hands pointer and keys (`act_browser::linux::offscreen`), with `<select>` lists shown as the app's own menus. GTK's accessibility bridge runs only while a page is open.

## Building and checks
Shaders compile at runtime (`runtime_shaders`), so no Metal toolchain is needed.
- `cargo run -p ai-coding-tool`; `ACT_PROFILE_DIR` picks a profile, `ACT_WINDOW_SIZE=WxH` the window size.
- `cargo test --workspace`, and `scripts/test-vendor.sh` for the vendored crates.
- `scripts/reducer-diff/run.sh` fuzzes the reducer against the TypeScript one; `scripts/command-diff/run.sh` compares command validation with it.
- `scripts/package-mac.sh` builds the `.app`, signed with `MAC_SIGN_IDENTITY` or ad hoc; `scripts/package-linux.sh [x86_64|aarch64]` the Linux tarball (and AppImage when `appimagetool` is on PATH), in Docker when not on Linux.
- `scripts/profile.sh <dir>` copies the Electron dev profile (or `ELECTRON_PROFILE`) for `ACT_PROFILE_DIR`; `scripts/shot.sh <process-name-or-pid> out.png` screenshots a window. Compare against the Electron dev app (`npm start` in `../ai-coding-tool`).
- `.github/workflows/release.yml` builds, signs, notarizes and publishes a release with the update feeds `act_host::updates` reads; `scripts/release-feed.sh` writes those feeds.
- `python3 -I scripts/third-party-notices.py` regenerates `assets/legal/THIRD-PARTY-NOTICES.txt` from `Cargo.lock` (`--check` fails when it is stale); `--cua-driver <trycua/cua checkout>` regenerates the Linux Cua Driver's crate report. `assets/legal/linux` ships only in the Linux packages. The in-app reader lists the files in `assets/legal` (`crates/app/src/licenses.rs`).
