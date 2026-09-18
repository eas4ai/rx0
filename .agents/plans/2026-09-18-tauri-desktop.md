## Goal

Ship rx0 as a Tauri v2 desktop app (system webviews via wry) with full feature parity to the browser build: same Rust backend, same web UI, installers for Windows, macOS, and Linux from the release flow.

## Success Criteria

- `npm run desktop:dev` opens a local desktop window serving the current checkout; `npm run desktop:build` produces an installer.
- GitHub releases carry installers (nsis, dmg, appimage) alongside the existing raw binaries; checksums and `rx0 --update` keep working for the raw binaries.
- On all three dogfood machines, the installed app opens a folder and the terminal drawer, context menu, LSP, agent edit, and settings all behave exactly as in the browser.
- The browser path (dev server, committed bundle, CI freshness gate) is byte-for-byte unaffected.
- In-app updates work through the Tauri updater once signing keys exist (user-owned prerequisite, see U6).

## Context And Current Facts

- Backend is one axum router (HTTP + WS on a single port) with the UI embedded via rust-embed (`src/server.rs:188-205`); `serve_until(listener, state, shutdown)` (`src/server.rs:1520`) is a clean seam. Release `rx0` prints `rx0 {VERSION} serving {root} at {url}` on stdout (`src/main.rs:231-234`).
- Frontend is same-origin only (`new URL(path, location.origin)` in `web/src/state.js:7`); no absolute API hosts anywhere. No CSP meta in `web/index.html`. So a webview pointed at the backend URL needs zero frontend changes.
- ConPTY/powershell behavior under a real terminal was validated on `Xpstablet` this week (DSR queries, `-Command` limits); xterm.js answers what a headless harness cannot. No new terminal work is needed for desktop.
- Athas (`~/workspace2/athas`) is a shipped Tauri v2 reference: `src-tauri/tauri.conf.json` (windows, CSP, `main-capability`, bundle targets `app/dmg/nsis`, updater artifacts), `crates/terminal` (multi-session manager with reader threads — a reference only; rx0 stays single-session by design), per-platform configs.
- Release flow (`.github/workflows/release.yml`) builds 5 raw-binary triples and publishes via `gh release upload` with `checksums.txt`; the `macos-13` runner is dead and Intel macOS builds on `macos-14` runners.
- Branding source exists: `assets/cover.png` (+ `.jpg`). No icons, no `src-tauri`, no Tauri deps in `Cargo.toml`/`package.json`. Root `Cargo.toml` is a single package (no `[workspace]`), so a nested `src-tauri` crate needs workspace handling.
- The Tauri v2 sidecar pattern is: `bundle.externalBin` entries plus per-triple suffixed binaries, spawned from Rust via `tauri_plugin_shell::ShellExt::sidecar()`.

## Constraints And Non-goals

- Tauri v2 + wry system webviews. No CEF (`cef-rs` not ready), no Electrobun, no GPUI (all previously decided).
- Full monty: no feature cuts. Work units below are build order, not shippable subsets.
- Binaries only, no crate publishes. Node 24, npm, Rstack toolchain stay as-is.
- Non-goals: multi-session terminal, file-association/deep-link handling, mobile targets, changes to the HTTP API or web UI, touching the raw-binary release assets.

## Key Decisions

1. **Sidecar, not integration.** The Tauri shell spawns the existing `rx0` binary and loads its URL. Rejected: compiling rx0 into the Tauri binary (merges runtimes, kills the standalone browser product) and re-implementing the 40+ routes plus WS terminal as Tauri `invoke` commands (gratuitous rewrite).
2. **Webview loads the sidecar URL.** `http://127.0.0.1:{port}/` keeps same-origin, so no frontend or CORS changes. Rejected: serving `web/` over the asset protocol with API calls to localhost (mixed origins, would force absolute URLs and CORS config).
3. **Port handoff via stdout.** Sidecar starts with `--port 0 --no-open`; the host parses the `serving ... at {url}` line from `CommandEvent::Stdout` and navigates the main window to it. Deterministic, no port race, no new flags on `rx0`.
4. **Workspace selection.** On launch: optional CLI path arg, else the remembered folder (`store` plugin), else a folder picker (`dialog` plugin); the sidecar (re)starts with that TARGET. Single-instance plugin focuses the running window instead of spawning a second server.
5. **Tight capabilities.** `core:window`, `core:event`, `dialog:allow-open`, `store`, `single-instance`, `shell:allow-spawn` scoped to the sidecar binary and its `--port 0 --no-open` args, updater permissions. No blanket `fs`/`clipboard` scopes; the app talks HTTP like the browser does.
6. **Installers where bundlers run natively.** `nsis` (win-x64), `dmg` (mac-arm64), `appimage` (linux-x64) — exactly the three dogfood machines. All five raw-binary triples keep shipping as before; no Intel-mac or ARM-Linux installers until hosted runners allow (GitHub-side limit, not a feature cut).
7. **Updates split by artifact.** Raw binaries keep `rx0 --update` + `checksums.txt` untouched. Installers update via `tauri-plugin-updater` with a generated `latest.json`; requires a signing keypair whose private half lives in GitHub Secrets (user-owned, see U6).

## Recommended Approach

Thin Tauri launcher shell in `src-tauri/` (own crate, `rx0-desktop` binary): resolve TARGET, spawn the sidecar, parse its URL from stdout, show the main window at that URL, tear the sidecar down on exit. Everything else — backend, UI, tests, release binaries — is reused untouched. Borrow Athas's `tauri.conf.json`/capability shape and bundle-target set; borrow nothing from its terminal crate (different design, intentionally).

## Work Plan

- **U1 — Scaffold.** `src-tauri/` crate (`tauri` v2, `tauri-plugin-shell`, `-dialog`, `-store`, `-single-instance`, `-updater`, `-window-state`), `tauri.conf.json` + per-OS overrides, `capabilities/main.json`, `frontendDist: web`, `beforeBuildCommand: npm run build`, root `[workspace]` with `exclude = ["src-tauri"]`, `npm run desktop:dev/build` scripts (`@tauri-apps/cli` via npm). Validation: `npm run desktop:build` succeeds locally; `git status` shows only additive paths.
- **U2 — Sidecar lifecycle.** Setup hook: spawn `binaries/rx0-sidecar` with `--port 0 --no-open`, parse `serving ... at {url}` from stdout events, navigate main window, kill on exit; single-instance focus. Validation: dev window shows the editor served from the sidecar (assert URL host is loopback); killing the window ends the sidecar (no orphan `rx0` process).
- **U3 — Workspace selection.** CLI path arg, remembered folder, folder picker fallback; restart sidecar on change. Validation: each entry path opens the right tree; remembered folder survives restart.
- **U4 — Branding.** `tauri icon assets/cover.png`, `productName: rx0`, identifier `ai.eas4.rx0` (assumption, confirm at approval), window title/size/min-size. Validation: icons render in the built installer on all three OSes.
- **U5 — Release pipeline.** Triple-suffix sidecar prep script, per-OS `tauri build` jobs, installers uploaded next to raw binaries, checksums still cover `rx0-*`. Validation: dry-run workflow on a tag builds all three installers; `rx0 --update` still verifies a raw binary.
- **U6 — Updater (needs signing keys).** Generate the updater signing keypair; private key to GitHub Secrets, pubkey into config; `latest.json` published per release. Validation: staged release offers and applies an update on one box before enabling generally.
- **U7 — Dogfood matrix (validation, no code).** Install on all three unlocked machines; exercise drawer + Windows terminal, context menu, LSP def/refs, agent edit with a real harness, settings round-trip, restart persistence. Highest-risk step: first Windows nsis install + ConPTY drawer inside WebView2.

## Validation Plan

- Existing gates stay green and unchanged: `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`, `npm test`, `npm run lint`, bundle-freshness check — on Linux, Windows, and Mac.
- New: `npm run desktop:build` per OS in CI; sidecar boot assert (spawn → `serving` line within 10 s → kill, no orphans).
- Manual: U7 matrix on the three dogfood boxes; screenshots of the installed app before CI is re-enabled.

## Risks / Rollback

- macOS Gatekeeper on unsigned dmgs (smooth install needs a Developer ID — same user-owned key story as U6); unsigned builds still run via right-click Open.
- Linux bundler system deps (webkit2gtk) only affect CI images, not the Rust code.
- `tauri build` duplicates `web/` (~450 KB) into the bundle; accepted, documented in config comment.
- Rollback is trivial: desktop work is additive (`src-tauri/`, scripts, workflow deltas); revert those paths and the browser product is exactly as before. No migrations, no API changes.

## Open Questions

None blocking. Assumptions to confirm at approval: identifier `ai.eas4.rx0`; updater/signing keys owned by Shawn (blocks U6 only, everything else proceeds); installer set limited to the three natively-bundled triples.

## Sources

- [Tauri v2 sidecar guide](https://v2.tauri.app/develop/sidecar/) — `externalBin`, `-TARGET_TRIPLE` suffix naming, `ShellExt::sidecar()` spawn.
- Workspace: `src/server.rs:188-205,1463-1528`, `src/main.rs:231-234`, `web/src/state.js:7`, `assets/cover.png`, `.github/workflows/release.yml`, `~/workspace2/athas/src-tauri/tauri.conf.json` + `capabilities/main.json`.
