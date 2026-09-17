# rx0

rx0 is a fast, ultra-light, remote-first code navigator that runs in your browser. You point it at a directory, and it serves a single-page UI for navigating, searching, reviewing, and editing code through a coding agent. One static binary holds the server and the UI. No database, no account, no runtime dependencies.

## Optimized for reads

More and more code is written by agents, CLIs, and background orchestrators. That leaves developers reviewing, auditing, and navigating far more than typing. rx0 is built for that side of the work: a zero-latency window into the repository, especially on remote machines. When something needs to change, you select it and hand it to the coding agent you already use. rx0 runs the harness and reloads what moved.

Where rx0 fits best:

- **Verifying agent output**: trace references, inspect live diffs against `HEAD`, and send a fix back to the agent from the diff without leaving your terminal flow.
- **Remote and cloud inspection**: run it on a server, VM, or CI runner and browse from your local browser. No SSH keys, no port-forwarding churn, no remote daemons.
- **Auditing large repositories**: read 50,000-file codebases on a laptop without background indexers spinning up fans.
- **Sidecar to terminal editors**: keep Vim, Neovim, or Helix for typing, and use rx0 as the graphical inspection and diff console.

## Install

Build from source (requires Rust and Node 24):

```bash
git clone https://github.com/eas4ai/rx0.git
cd rx0
cargo install --path .
```

Or download a prebuilt binary for Linux, macOS, or Windows from the [releases page](https://github.com/eas4ai/rx0/releases) and place it on your `PATH`. After that, `rx0 --update` upgrades the binary in place.

## Run

```bash
rx0                       # view the current directory
rx0 ~/src/kernel          # view another repository
rx0 web/src/main.js       # view a file in its project
rx0 src/main.rs:42        # open a file at a line number
```

rx0 indexes the workspace, serves the UI at `http://127.0.0.1:7777`, and opens your browser.

### Remote and cloud workspaces

Run rx0 where the code lives and view it in your local browser over Tailscale, WireGuard, a reverse proxy, or a tunnel. One port carries everything.

```bash
# Bind all interfaces on a remote machine or cloud instance
rx0 --host 0.0.0.0 --port 7777 ~/work/repo

# Headless mode without opening a browser
rx0 --no-open --port 8080 /workspace
```

Anyone who can reach rx0 by IP address can dispatch agent edits as you, so keep `--host 0.0.0.0` on private networks. Opened through a hostname (reverse proxy or tunnel domain), editing is refused. See [Guards](#guards).

### Update

```bash
rx0 --update
```

rx0 also checks once a day in the background without delaying startup, and prints a notice when a newer release exists. Downloads are verified against the release `checksums.txt` and smoke-tested before the binary is replaced.

### CLI flags

| Flag | Default | Description |
| ---- | ------- | ----------- |
| `--port <N>` | `7777` | Port to listen on (`0` picks a free port) |
| `--host <H>` | `127.0.0.1` | Address to bind |
| `--no-open` | `false` | Do not launch the browser automatically |
| `--no-lsp` | `false` | Disable language servers, use regex outlines |
| `--no-git` | `false` | Disable git awareness (badges and diff view) |
| `--agent <H>` | none | Pin the coding harness: `claude`, `gemini`, `cursor-agent`, `agy`, `opencode`, `codex`, `aider`, or a command template containing `{prompt}` |
| `--no-agent` | `false` | Do not offer harness editing |
| `--no-telemetry` | `false` | Disable anonymous usage telemetry |
| `--no-color` | `false` | Strip ANSI sequences from terminal output |
| `--quiet` | `false` | Suppress narration (errors still print) |
| `--verbose` | `false` | Log requests and internal activity |
| `--dev <DIR>` | none | Serve the UI from a source directory instead of the embedded copy |
| `--update` | `false` | Check for and install the latest release |
| `--version`, `-v` | `false` | Print the version and exit |

## Features

- **Fast navigation**: fuzzy file search (`Mod+P`), workspace symbol search (`Mod+Shift+O`), and workspace regex search (`Mod+Shift+F`) with literal, regex, whole-word, and glob-scoped modes.
- **Remote-first, zero SSH hassle**: one port, no remote daemons, no extension hosts.
- **Syntax highlighting**: tokenized rendering with windowed line fetching, themed by 14 built-in stylesheets (GitHub Dark, Tokyo Night, Catppuccin, Dracula, Gruvbox, Nord, Solarized, and more).
- **Git awareness and visual diffs**: status badges (`M`, `A`, `D`, `U`, `R`), dirty-folder propagation, a changed-files filter, and split or unified diffs against `HEAD` (`Mod+D`).
- **Virtualized file view**: only visible rows are mounted, so very large files scroll as cheaply as small ones.
- **Rendered Markdown**: GFM preview with highlighted code fences. `Alt+M` switches between preview and source while preserving scroll.
- **Settings modal**: `Mod+,` opens a VS Code-style editor with live preview and raw JSON sync, stored per-user in `~/.rx0/settings.json`, never in the repository.
- **Self-contained**: a single binary embeds the UI. No Electron, no Node at runtime, no cloud calls except telemetry and update checks.

## Language servers (optional)

rx0 works fully without language servers, using fuzzy search and regex outlines. When a server is installed, you get semantic go-to-definition (`F12`), hover types and docs, references, and call trails. rx0 auto-detects servers on your `PATH`, spawns them lazily on first request, and shuts them down on exit. Disable everything with `--no-lsp`, or click **LSP: set up** in the status bar to install what your workspace needs.

| Language | Server | Quick install |
| -------- | ------ | ------------- |
| Go | `gopls` | `go install golang.org/x/tools/gopls@latest` |
| Rust | `rust-analyzer` | `rustup component add rust-analyzer` |
| TypeScript / JavaScript | `typescript-language-server` | `npm install -g typescript-language-server typescript` |
| Python | `pyright` / `pylsp` / `ruff` | `npm install -g pyright` or `pipx install python-lsp-server` |
| C / C++ | `clangd` | `sudo apt install clangd` or `brew install llvm` |
| Zig | `zls` | `brew install zls` |
| Lua | `lua-language-server` | `brew install lua-language-server` |
| Ruby | `solargraph` | `gem install solargraph` |
| Java | `jdtls` | `brew install jdtls` |
| C# | `omnisharp` | Install OmniSharp on `PATH` |
| LaTeX | `texlab` | `brew install texlab` |

## Edit with a coding agent (optional)

rx0 has no text editor. It hands changes to a coding harness installed on your machine, then reloads what the harness changed.

| Harness | Default model | Command rx0 runs |
| ------- | ------------- | ---------------- |
| Claude Code | `haiku` | `claude --permission-mode acceptEdits --model haiku -p {prompt}` |
| Gemini CLI | `gemini-2.5-flash-lite` | `gemini --approval-mode auto_edit -m gemini-2.5-flash-lite -p {prompt}` |
| Cursor Agent | `gemini-3.6-flash-minimal` | `cursor-agent --force --model gemini-3.6-flash-minimal -p {prompt}` |
| Antigravity | `gemini-3.6-flash-low` | `agy --dangerously-skip-permissions --mode accept-edits --model gemini-3.6-flash-low -p {prompt}` |
| OpenCode | `opencode/big-pickle` | `opencode run -m opencode/big-pickle {prompt}` |
| OpenAI Codex | `gpt-5-codex` | `codex exec --ask-for-approval never -m gpt-5-codex {prompt}` |
| Aider | `claude-3-7-sonnet` | `aider --yes-always --no-auto-commits --model claude-3-7-sonnet --message {prompt}` |

Each harness defaults to its fastest, most economical model, and you can pick any available model from the harness menu.

### How an edit works

1. Select code in the source view or either side of the diff view.
1. Pick **Edit with Agent** from the right-click menu, the footer selection bar, or press `Alt+E`.
1. The first time, choose a harness and model. rx0 remembers the choice in `~/.rx0/settings.json`, never in your repository.
1. Describe the change and press `Enter`. rx0 sends the harness the instruction plus the file, line range, and selected lines.
1. Progress streams into the job log while the harness runs. Several edits can run at once.
1. When the harness exits, rx0 reloads the files it changed. Each tab keeps its view: source stays source, diff stays diff.

The footer always shows the harness and model in use. Click it to switch.

### When something goes wrong

A failed harness reports its error inline under your instruction, with stdout and stderr attached (usually an invalid API key or a missing binary). Nothing is lost: the composer stays open with your instruction intact.

### Guards

- Concurrent edits may not overlap: a range touching an edit already in flight is refused, since two harnesses rewriting the same lines cannot be reviewed.
- Closing a tab with a running edit asks for confirmation first.
- Mutating requests are accepted only from rx0's own page, opened by IP address or `localhost`. Through a hostname they are refused.
- Nothing runs until you pick a harness. `--agent` pins one for the session, and `--no-agent` turns editing off.

## Settings (`~/.rx0/settings.json`)

Open settings with `Mod+,`, the gear icon in the status bar, or the command palette (`Mod+Shift+P`, then **Preferences: Open Settings**). The graphical editor and the raw JSON stay in sync, and visual settings apply live without a reload. Any modified value shows a `Modified` badge with a one-click reset.

Key settings:

| Setting | Default | Options | Description |
| ------- | ------- | ------- | ----------- |
| `editor.fontSize` | `13.5` | `9.0`–`32.0` | Viewer font size in pixels |
| `editor.fontFamily` | JetBrains Mono stack | CSS font stack | Viewer font family |
| `editor.lineHeight` | `21` | `14`–`48` | Viewer line height in pixels |
| `editor.tabSize` | `4` | `2`, `4`, `8` | Spaces per tab |
| `editor.wordWrap` | `"on"` | `"on"`, `"off"` | Soft-wrap at the editor boundary |
| `editor.lineNumbers` | `"on"` | `"on"`, `"off"` | Gutter line numbers |
| `editor.cursorStyle` | `"line"` | `"line"`, `"block"`, `"underline"` | Cursor style |
| `editor.cursorBlinking` | `"smooth"` | `"blink"`, `"smooth"`, `"solid"` | Cursor animation |
| `editor.renderLineHighlight` | `"line"` | `"line"`, `"none"` | Current-line highlight |
| `editor.occurrencesHighlight` | `true` | `true`, `false` | Highlight selected-word occurrences |
| `editor.scrollBeyondLastLine` | `true` | `true`, `false` | Scroll past the file end |
| `editor.bracketPairColorization` | `true` | `true`, `false` | Rainbow brackets and matching |
| `editor.renderWhitespace` | `"selection"` | `"none"`, `"boundary"`, `"selection"`, `"all"` | Whitespace rendering |
| `editor.minimap.enabled` | `true` | `true`, `false` | Search-hit indicators in the minimap |
| `workbench.colorTheme` | `"github-dark"` | 14 built-in themes | Workbench theme |
| `diffEditor.renderSideBySide` | `true` | `true`, `false` | Split versus unified diff |
| `diffEditor.ignoreTrimWhitespace` | `true` | `true`, `false` | Ignore whitespace-only diffs |
| `git.gutterIndicators` | `true` | `true`, `false` | Gutter change indicators |
| `explorer.compactFolders` | `true` | `true`, `false` | Collapse single-child directory chains |
| `explorer.autoReveal` | `true` | `true`, `false` | Scroll the tree to the active file |
| `files.exclude` | `**/.git, **/node_modules, ...` | Glob patterns | Exclusions for trees and searches |
| `search.smartCase` | `true` | `true`, `false` | Case-sensitive only when the query has uppercase |
| `search.maxResults` | `1000` | `50`–`10000` | Cap for workspace search results |
| `markdown.preview.open` | `true` | `true`, `false` | Open Markdown in preview by default |
| `lsp.enabled` | `true` | `true`, `false` | Master switch for language servers |
| `lsp.hover.enabled` | `true` | `true`, `false` | Hover documentation cards |
| `agent.harness` | `""` | `claude`, `gemini`, `agy`, etc. | Preferred coding harness |
| `agent.timeoutSeconds` | `120` | `10`–`600` | Cap for agent edit runs |
| `agent.autoAcceptEdits` | `false` | `true`, `false` | Accept agent diffs without confirmation |
| `telemetry.enabled` | `true` | `true`, `false` | Anonymous usage metrics |

## Keyboard shortcuts

`Mod` is `Cmd` on macOS and `Ctrl` elsewhere. The in-app sheet (`?`), footer hints, and tooltips label each key for your keyboard.

| Keys | Action |
| ---- | ------ |
| `Mod+,` | Open settings |
| `Mod+K` | Universal palette / quick open |
| `Mod+P` | Go to file |
| `Mod+Shift+P` | Command palette |
| `Mod+Shift+O` | Go to symbol in file |
| `Mod+Shift+F` | Full workspace search |
| `Mod+F` | Find in the open file, seeded with the selection |
| `Mod+G` | Jump to line |
| `Mod+D` | Toggle the git diff of the open file |
| `F12`, `Mod+Click` | Go to definition |
| `Shift+F12` | Find all references |
| `Alt+Shift+H` | Call trail: callers and callees, expandable level by level |
| `Hover` | Type signature and documentation |
| `Alt+Left` / `Alt+Right` | Navigate back / forward |
| `Mod+B` | Toggle the sidebar |
| `Mod+J` | Toggle the right inspector (symbols and references) |
| `Alt+Z` | Toggle word wrap |
| `Alt+M` | Toggle Markdown preview |
| `Alt+C` / `Alt+A` / `Alt+U` | With code selected: copy reference, copy with context, find usages |
| `Alt+E` | With code selected: edit with your coding agent |
| `Right click` | Selection actions in a menu at the pointer |
| `Double click` | Highlight all occurrences |
| `Enter` / `Shift+Enter` | Next / previous match |
| `Ctrl+Tab`, `Alt+1`–`Alt+9` | Switch tabs, select tab by position |
| `Alt+W` | Close tab |
| `Alt+Shift+T` | Reopen closed tab |
| `Esc` | Dismiss |
| `?` | Show all shortcuts |

## Philosophy

- **Optimized for reads**: authoring belongs to agents, CLIs, and editors. rx0 is the reader: open anything instantly and hand changes to the harness you choose, never through a save button.
- **Remote-first without SSH**: one port serves local directories and cloud instances alike, with no remote daemons or session upkeep.
- **Private and sandboxed**: zero accounts and zero cloud dependencies. Code and queries stay on the running machine, file access is jailed to the workspace, and cross-site and DNS-rebinding attacks are refused at the gate.

### Telemetry and privacy

rx0 records lightweight, anonymous session metrics (via PostHog) to count daily and monthly users and measure session length: a `session_started` event with the indexed-file bucket, index time, and git/LSP availability, plus a `session_ended` event with the stop reason.

rx0 never collects feature interactions, frontend events, browser fingerprints, code, diffs, file or repository names, symbols, signatures, search queries, personal data, or accounts.

To opt out, use `--no-telemetry`, set `DO_NOT_TRACK=1` or `RX0_TELEMETRY=0`, or flip `telemetry.enabled` in settings.

## Develop

```bash
cargo test              # 95 lib + 27 API tests
cargo clippy --all-targets
cargo fmt --check
node ./scripts/build-web.js   # rebuild web/app.js from web/src
```

The web bundle builds with the pure-Node bundler in `scripts/build-web.js` (Node 24, no dependencies). `RX0_USE_BUN=1` selects the optional Bun fast path. CI fails if `web/app.js` drifts from `web/src`. `PORTING.md` holds the working notes from the Rust port, including the behaviors that intentionally differ from the Go original.

## Attribution

rx0 is a Rust port of [px0](https://github.com/eas4ai/px0), which pioneered the design: a zero-setup browser UI for navigating code. The port keeps the feature set and the API shape, reimplemented in Rust on Axum and Tokio. Thanks to the px0 authors for the original.
