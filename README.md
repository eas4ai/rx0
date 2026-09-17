# rx0

rx0 is a fast, ultra-light code navigator that runs in the browser. You point it at a directory, and it serves a single-page UI for exploring, searching, and understanding the code. One static binary holds the server and the UI. No database, no build step at runtime, no account.

## Install

Build from source (requires Rust and Node 24):

```bash
cargo install --path .
```

Or download a prebuilt binary from the [releases page](https://github.com/eas4ai/rx0/releases) and place it on your `PATH`. After that, `rx0 --update` upgrades the binary in place from new releases.

## Run

```bash
rx0 ~/my-project
```

rx0 indexes the directory, serves the UI at `http://127.0.0.1:7777`, and opens your browser. Pass a file instead of a directory to open that file. Useful flags:

- `--port 0` picks a free port and prints the URL.
- `--host 0.0.0.0` exposes the server on the network. The default `127.0.0.1` keeps it local.
- `--no-open` starts the server without launching a browser.
- `--no-lsp` disables language servers.
- `--no-git` disables git awareness.
- `--agent <name>` pins the coding harness used for edits. `--no-agent` hides editing.
- `--no-telemetry` disables anonymous usage telemetry.
- `--quiet` suppresses narration. `--verbose` logs requests and internal activity.
- `--dev <dir>` serves the UI from a source directory instead of the embedded copy.
- `--update` checks for and installs the latest release.
- `--version` prints the version.

## Browse and search

- The sidebar shows the workspace tree with git status badges and a changed-files filter. It respects `.gitignore` and skips binary and oversized files.
- `Mod+P` (go to file) fuzzy-matches paths as you type.
- `Mod+Shift+F` (search in files) searches file contents with literal, regex, whole-word, and glob-scoped modes. `Mod+K` opens the command palette.
- `Mod+F` finds within the open file. `Mod+G` jumps to a line. `Alt+W` closes a tab, and `Alt+Shift+T` reopens the last closed one.
- `Mod+Shift+O` (go to symbol) jumps to a symbol anywhere in the workspace.

## Read code

- Files render with syntax highlighting across 14 bundled themes, with a Markdown preview toggle (`Alt+M`) for documentation.
- The outline panel lists the symbols in the open file. The right inspector shows symbols and references for the selection.
- `F12` (or `Mod+Click`) goes to definition. `Shift+F12` finds all references. `Alt+Shift+H` shows the call trail of callers and callees.
- Hover shows documentation where the language server provides it.

## Language servers

rx0 starts language servers on demand and shuts them down on exit. The setup panel (`/api/lsp/setup`) reports which servers your workspace needs, and rx0 can install the missing ones from the UI. Supported servers cover common languages including Rust (`rust-analyzer`), Go (`gopls`), TypeScript, and Python, with per-language install recipes.

## Review changes

- Open files show per-line git blame state in the gutter, and the diff view (`Mod+D`) renders unstaged and staged changes side by side or inline.
- The tree marks modified, staged, untracked, renamed, and deleted files so you can see the state of the checkout at a glance.

## Edit through a coding harness

rx0 never writes code itself. It composes an instruction anchored to your selection, hands it to a coding harness already installed on your machine, and reloads what changed. Built-in presets cover `claude`, `codex`, `opencode`, `gemini`, `cursor-agent`, `aider`, and `agy`, and you can define your own. Long runs execute as background jobs that you can watch and cancel from the UI.

## Settings and data

- `Mod+,` opens settings: theme, telemetry, harness selection, and the raw `settings.json` editor.
- Configuration lives in `~/.rx0/settings.json` (or `$XDG_CONFIG_HOME/rx0/settings.json` when set), never in a workspace.
- Anonymous usage telemetry (PostHog) is on by default and contains no code or paths. Turn it off with `--no-telemetry`, `RX0_TELEMETRY=0`, or the `telemetry.enabled` setting.
- `rx0` checks for updates once a day and prints a notice. Downloads are verified against the release `checksums.txt` and smoke-tested before the binary is replaced.

## Security model

rx0 binds to loopback by default. Requests that change the machine (settings, language-server install and start, harness selection, edits, job cancellation) are accepted only as `POST` requests from rx0's own page: the `Origin` header must match, and the `Host` must be `localhost` or an IP address, which blocks cross-site and DNS-rebinding attacks. File access is sandboxed to the workspace root, except for absolute paths a language server names as definition targets (standard libraries and module caches).

## Develop

```bash
cargo test            # 95 lib + 27 API tests
cargo clippy --all-targets
cargo fmt --check
node ./scripts/build-web.js   # rebuild web/app.js from web/src
```

The web bundle is built with the pure-Node bundler in `scripts/build-web.js`, which needs nothing beyond Node 24. Set `RX0_USE_BUN=1` only if you want the optional Bun fast path; both produce the same UI. CI fails if a commit leaves `web/app.js` out of sync with `web/src`.

`PORTING.md` holds the working notes from the Rust port, including the behaviors that intentionally differ from the Go original.

## Attribution

rx0 is a Rust port of [px0](https://github.com/px0-ai/px0) by px0-ai, which pioneered the design: a zero-setup browser UI for navigating code. The port keeps the feature set and the API shape, reimplemented in Rust on Axum and Tokio. Thanks to the px0 authors for the original.
