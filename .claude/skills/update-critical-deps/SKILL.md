---
name: update-critical-deps
description: Check the Claude Agent SDK, Codex, and the Cua Driver for new releases, then apply and commit the ones that carry no breaking changes and report back on the ones that do.
disable-model-invocation: true
---

# Update critical dependencies

This app links none of the three. It reproduces their wire behaviour in Rust or bundles a prebuilt binary, so an upstream change never fails the build. Each is pinned by version constants:

- **Claude Agent SDK.** `SDK_VERSION` in `crates/engines/src/claude/query.rs` names the SDK whose stream-json driving of `claude` the client reproduces: command line, `initialize` request, and control requests both ways. It is sent to Claude Code as `CLAUDE_AGENT_SDK_VERSION`, so never raise it without porting what changed. Call sites: `crates/engines/src/claude/{query,provider,session,services}.rs`.
- **Claude Code floor.** `CLAUDE_BASELINE_VERSION` in `crates/engines/src/claude/services.rs` is the oldest installed `claude` offered every model. Raise it only when ported SDK behaviour needs a newer Claude Code. The SDK's `package.json` field `claudeCodeVersion` names the Claude Code it ships.
- **Codex.** `CODEX_PROTOCOL_VERSION` in `crates/engines/src/codex/services.rs` is the `codex --version` the app-server protocol knowledge was taken from. An older installed `codex` is turned away, so raising it makes users upgrade. The protocol is hand-written JSON-RPC in `crates/engines/src/codex`; the private Codex home is `home.rs`, `home_config.rs`, `shared_auth.rs` and `config.rs`.
- **Cua Driver.** `cua_version` and `cua_sha256` in `scripts/package-mac.sh`, and `cua_version` with the per-arch `cua_sha256` values in `scripts/package-linux.sh`. Call sites: `crates/host/src/computer_use_host.rs` and the computer-use tools in `crates/host/src/tools`. Its notices are `assets/legal/CUA-MIT.txt`, `assets/legal/CUA-THIRD-PARTY-NOTICES.md`, `assets/legal/CUA-RUST-DEPENDENCIES.html` (macOS) and `assets/legal/linux/CUA-RUST-DEPENDENCIES-LINUX.txt`.

Both packaging scripts and every Cua notice describe one Cua release. Move them together or a package ships a driver its notices do not describe.

The Electron app in `../ai-coding-tool` tracks the same three with its own `update-critical-deps` skill. When it already moved a dependency to the candidate version, its commit (`git -C ../ai-coding-tool log --oneline -- package.json`) shows the call-site changes to port, and its regenerated Codex protocol diff (`src/main/codex/protocol`) is the protocol diff to read.

`~/.local/bin/cua-driver`, the standalone CLI, is a separate install and out of scope. Leave it alone unless asked.

## Resolve setup blockers

If the sandbox blocks downloads, retry through the environment's permission mechanism. If a package or executable is for another platform, obtain the matching host build of the same candidate version. Neither an initial network error nor an absent local tool is enough to declare the update blocked.

Stop an affected update only for the compatibility problems defined below, or when required evidence still cannot be obtained after reasonable recovery attempts. Report the remaining blocker and what was tried. Never weaken checks, invent checksums or source metadata, or hand-edit generated notices to get past a blocker; continue other dependencies that can be completed independently.

## 1. Check

```
grep -n "SDK_VERSION: &str" crates/engines/src/claude/query.rs
grep -n "CLAUDE_BASELINE_VERSION: &str" crates/engines/src/claude/services.rs
grep -n "CODEX_PROTOCOL_VERSION: &str" crates/engines/src/codex/services.rs
grep -n "^cua_version=" scripts/package-mac.sh scripts/package-linux.sh
npm view @anthropic-ai/claude-agent-sdk version
npm view @openai/codex version
npm view @trycua/cua-driver version
```

All current: say so, stop.

Ignore the `check_for_update` MCP tool here. It reports the standalone CLI's version, not this repo's.

## 2. Analyse each new version for breaking changes

All three are pre-1.0, so a patch bump can still break. Never judge from the version number alone. Work in temp dirs so the tree stays untouched while you decide.

1. Read the release notes.
   - Agent SDK: releases on `anthropics/claude-agent-sdk-typescript`.
   - Codex: the matching CLI release on `https://github.com/openai/codex/releases`.
   - Cua Driver: `https://github.com/trycua/cua/releases/tag/cua-driver-rs-v<version>`.
2. Diff the wire surface between the pinned and the candidate version.
   - Agent SDK: `npm pack` both versions, unpack, and diff `sdk.d.ts`, `agentSdkTypes.d.ts` and `sdk-tools.d.ts`. In the bundled `sdk.mjs`, compare the `claude` command-line flags, the `initialize` request fields, control request subtypes, and environment variables it sets. Note any `claudeCodeVersion` change.
   - Codex: `npm pack @openai/codex@<version>-darwin-arm64` for both versions, run each `vendor/aarch64-apple-darwin/bin/codex app-server generate-ts --out <temp-output>`, and diff the outputs.
   - Cua Driver: shallow-clone `trycua/cua` at both `cua-driver-rs-v<version>` tags and diff `libs/cua-driver/rust`: the CLI and daemon interface, socket protocol, tool names and parameters, permissions, and signing.
3. Read every hit against the call sites listed above.

For Codex, protocol compatibility is not enough. Read the private-home files and inspect the candidate's config, auth and storage changes for new home-relative inputs, keychain identity changes, renamed storage flags, symlink replacement, and login/logout behaviour. Keep the shared-input list explicit: never share the whole home, sessions, databases or logs to make a check pass. Do not switch credential backends, copy real tokens, or edit real Codex settings during validation.

Breaking means a method, field, flag or tool the app uses was removed, renamed, or changed incompatibly; a default changed in a way that changes behaviour; a new required config or permission step; or the port needs a higher `CLAUDE_BASELINE_VERSION`. An added optional field the app only receives or ignores is not breaking.

**If it breaks, stop that dependency's update.** Leave its pins unchanged. Report the dependency, version, breakage, affected call sites, and required migration. The user decides whether to take it on.

**If it is clean, continue** one dependency at a time.

## 3. Apply

Agent SDK: port every wire change the diff showed into the call sites, then set `SDK_VERSION`. Raise `CLAUDE_BASELINE_VERSION` only under the breaking-change rule above.

Codex: port every protocol change the app's call sites touch, then set `CODEX_PROTOCOL_VERSION`.

Cua Driver: set `cua_version` in both packaging scripts. Download the macOS and both Linux archives from the release (the URLs are in the scripts), record each SHA-256 from the downloaded file, and never carry a checksum forward. Then refresh the notices:

- `CUA-MIT.txt` and `CUA-THIRD-PARTY-NOTICES.md` from the release tag's license and third-party notices files.
- `assets/legal/linux/CUA-RUST-DEPENDENCIES-LINUX.txt`: `python3 -I scripts/third-party-notices.py --cua-driver <checkout at the release tag>`.
- `CUA-RUST-DEPENDENCIES.html` is generated by the Electron app (`npm run generate:native-licenses`). Copy it from `../ai-coding-tool/assets/legal` only when that app is on the same Cua Driver version; otherwise update the Electron app first or report the gap.

Review every changed file under `assets/legal`.

## 4. Verify

```
python3 -I scripts/third-party-notices.py --check
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
```

`cargo test` runs `protocol_fuzz`, which drives both engines through `fake_engine.py`. When a stand-in's version line is older than a raised pin, update it there.

When wire or protocol code changed, run the live tests. They spend real turns on cheap models and need the command signed in; Codex tests use a private home under `/tmp`. Put the candidate binary first on `PATH` when it differs from the installed one, and report which versions were exercised.

```
cargo test -p act_engines --test claude_live -- --ignored --nocapture
cargo test -p act_engines --test claude_scenarios -- --ignored --nocapture --test-threads=2
cargo test -p act_engines --test codex_live -- --ignored --nocapture
cargo test -p act_engines --test codex_scenarios -- --ignored --nocapture --test-threads=1
```

For the Cua Driver, run `zsh scripts/package-mac.sh`, check `./target/cua-driver/<version>/cua-driver --version`, and drive one computer-use action in the packaged app. A missing sign-in, binary, or permission is a verification gap, not a passing check.

Before committing, run `git diff --check` and inspect `git diff --name-only`.

## 5. Commit

Make one commit per dependency. Stage only that dependency's files:

- Agent SDK: the ported `crates/engines/src/claude` files and any `fake_engine.py` change. Commit as `Move the agent SDK to <version>`.
- Codex: the ported `crates/engines/src/codex` files and any `fake_engine.py` change. Commit as `Move Codex to <version>`.
- Cua Driver: `scripts/package-mac.sh`, `scripts/package-linux.sh`, every changed Cua notice, and any host or tool changes. Commit as `Move the Cua Driver to <version>`.

When updating more than one, finish and commit each dependency before touching the next. The working tree usually carries unrelated in-flight edits. Never stage them. Don't push unless asked.
