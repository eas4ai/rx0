# Bottom-drawer terminal — plan

## Goal

Add a bottom-drawer terminal to rx0: a full interactive shell running in the workspace root, toggled from the UI, backed by a PTY on the server.

## Success Criteria

- The user can open a bottom drawer, type in a working shell (`$SHELL`, fallback `sh`), run interactive programs (`vim`, `htop`), resize the window, and close the drawer without killing the shell.
- One session per browser window. Closing the drawer or the tab detaches; reopening reattaches. Stopping the server kills the shell and reaps the child.
- No new network exposure: the terminal rides the existing rx0 server and port, gated like other mutating endpoints.
- `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`, and the web-bundle freshness gate all pass.

## Context And Current Facts

- Frontend is hand-bundled zero-dependency ES modules (`web/src/*.js` → `web/app.js` via `scripts/build-web.js`); there is no npm step and no websocket client code today.
- Backend is Axum 0.8 without the `ws` feature; no `tokio-tungstenite` or PTY crate in `Cargo.toml`. Process spawning is argv-based with timeout kills (`src/agent.rs`), and mutating routes pass the `local_post` Origin/Host gate (`src/server.rs`).
- Layout (`web/index.html`, `web/style.css`) has sidebar, main, and right inspector regions — no bottom region exists. Shortcut sheet lives in `web/src/shortcuts.js`; no backtick binding is taken.
- User decisions already made: full interactive shell (not a command runner), single session (not tabs).

## Constraints And Non-goals

- Non-goals: multiple terminal tabs, persistence across server restarts, terminal search (native text selection suffices), mobile layout.
- The committed `web/app.js` must stay reproducible from `web/src` under plain Node 24 (CI enforces this).

## Key Decisions

- **Frontend: xterm.js, vendored.** xterm.js is the standard browser terminal (VS Code, Hyper, Tabby all use it), its core has zero dependencies, and its API is exactly the needed `onData`/`write` pair. It ships exclusively through npm, which rx0 does not use — so the decision is to vendor the built `@xterm/xterm` JS plus `xterm.css` into `web/` (version-pinned, one-time blob) rather than a CDN link (breaks offline/self-contained) or adding an npm step (breaks the build scheme). Rejected alternative: hand-rolled terminal emulator — unbounded scope for escape-sequence parity.
- **Backend PTY: `portable-pty`.** Cross-platform PTY crate from the wezterm project (`native_pty_system`, `CommandBuilder`, resize support), with Windows ConPTY coverage so the 5-target release matrix keeps working. Rejected alternative: `pty`/`nix` directly — Unix-only, would break the Windows target that just got fixed.
- **Transport: Axum WebSocket (`ws` feature).** No new transport crate: enabling Axum's own `ws` feature plus a `/api/terminal` route keeps the existing server, port, and TLS story. Rejected alternative: polling/long-poll JSON — cannot carry interactive apps (`vim`, `Ctrl+C`) with acceptable latency.
- **Security posture: same gate as agents.** The WS upgrade requires the `local_post` Origin/Host check (browsers send `Origin` on WS handshakes), the shell spawns with cwd jailed to the workspace root, and disconnect closes the PTY and kills the child. Documented risk: anyone who can reach rx0 by IP gets a shell as you — same trust level as agent edits, covered by the existing private-network guidance.

## Recommended Approach

One vertical slice in dependency order: PTY manager → WS route → vendored xterm + drawer UI → settings/docs. Shell defaults to `$SHELL` (fallback `sh`, `powershell.exe` on Windows); drawer toggle on backtick (VS Code convention), resizable via the existing resizer pattern, PTY survives drawer and tab close (socket close detaches; the frontend auto-reconnects) and dies with the server. Font size follows `editor.fontSize`; a `terminal.enabled` setting (default on) hides the feature.

## Work Plan

1. **PTY manager (`src/terminal.rs`)**: `portable-pty` dependency; spawn login shell with cwd at workspace root; `write`, `resize`, and kill-on-drop; unit tests with an `echo`/`exit` round-trip.
2. **WebSocket route (`src/server.rs`)**: enable Axum `ws` feature; `GET /api/terminal` upgrade behind the `local_post` gate; tiny JSON/binary protocol (stdin bytes, resize rows/cols, server→client output chunks); disconnect kills the session.
3. **Drawer UI (`web/src/terminal.js`, `index.html`, `style.css`)**: vendored xterm build + CSS; bottom region with drag resize, backtick toggle, theme hookup, `TERM=xterm-256color`; shortcut sheet entry; rebuild `web/app.js` with the committed script.
4. **Settings and docs**: `terminal.enabled` schema entry; README section (drawer usage, shortcuts, security note); shortcuts sheet entry.

## Validation Plan

- `cargo test` (new manager round-trip tests plus existing 95 + 27), `cargo clippy --all-targets`, `cargo fmt --check`, `node ./scripts/build-web.js && git diff --exit-code -- web/app.js`.
- New integration test: open WS, run `echo` + sized command, assert output bytes and exit propagation; resize message reflected in PTY size; socket close reaps the child (assert via `/api/metrics` or process liveness).
- Manual: open drawer in the browser, run `vim` and `htop`, resize the drawer, close and reopen the drawer (session survives), close the tab (child reaped).
- Highest-risk check: the WS integration test on all three OS families in CI, since PTY behavior differs most across platforms.

## Risks / Rollback

- Vendored xterm blob adds ~150 KB to the repo and the embedded binary; accepted as the price of offline builds.
- `vim`/fullscreen apps depend on correct `rows`/`cols` propagation — covered by the resize test.
- Rollback is a clean revert: new files (`src/terminal.rs`, `web/src/terminal.js`, vendored xterm) plus additive route/setting; nothing existing changes shape.

## Open Questions

None. Defaults assumed where reversible (`$SHELL`, cwd = workspace root, backtick toggle, PTY survives drawer close).

## Sources

- [xterm.js README](https://raw.githubusercontent.com/xtermjs/xterm.js/master/README.md) (zero-dep core, npm-only distribution, `Terminal`/`onData` API, needs external PTY such as node-pty)
- [portable-pty docs](https://docs.rs/portable-pty/latest/portable_pty/) (cross-platform PTY traits, `native_pty_system`, `CommandBuilder`, wezterm origin)
