# AI Coding Tool

The native rewrite of [AI Coding Tool](https://github.com/zyuapp/ai-coding-tool), a desktop app for working with Claude Code, Codex and OpenCode, rewritten in Rust with [GPUI](https://www.gpui.rs).

## Status

It replaces the Electron app: it installs under the same name, reads the same threads, projects and settings, and answers the same `aic` terminal command.

## Platforms

- macOS on Apple silicon
- Linux on x86_64 and aarch64

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/zyuapp/ai-coding-tool-preview/main/install.sh | sh
```

macOS gets the signed app in /Applications; Linux gets the AppImage in ~/Applications. You can also download a build from [Releases](https://github.com/zyuapp/ai-coding-tool-preview/releases): the `.dmg` or `.zip` on macOS, the `.AppImage` or `.tar.gz` on Linux.

## Build from source

With a stable Rust toolchain:

```sh
cargo run -p ai-coding-tool
```

On Linux, install the GTK 3, WebKitGTK 4.1, libsoup 3, ALSA, fontconfig, xkbcommon, Wayland and X11 development packages first. See [ARCHITECTURE.md](ARCHITECTURE.md) for the layout and checks.

## License

[MIT](LICENSE). Third-party notices are in [assets/legal](assets/legal); the vendored crates in [vendor](vendor) keep their own licenses.
