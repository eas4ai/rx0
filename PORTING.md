# px0 Rust port — working notes

Source: `../px0` (Go, `VERSION 0.1.5`). Target: this crate, plain Rust
(axum + tokio + serde_json + rust-embed). No Suprnova: px0 uses none of
its differentiators (no DB, ORM, auth, queues, Inertia), and a young git
dependency adds provenance risk for zero gain.

## Locked decisions

- Framework: plain Rust stack (axum 0.8, tokio, tower-http).
- API: free to reshape where Rust idioms differ. Consequence: `web/` is
  vendored here and will diverge from the Go copy; call sites move with
  the API. Divergences from Go behavior are noted per slice below.
- Highlighting: theme-compatible (works with the 14 stylesheets), not
  Chroma-token-exact.

## Slice log

### Slice 1 — server shell (done when `cargo test` is green)

CLI flags, router, embedded `web/`, `-dev` disk assets, path sandbox
(`safe_path`/`resolve_path`), `/`, `/static/*`, `/static/themes.css`,
`/api/meta`, JSON error envelope. Deferred: everything else `/api/*`.

Divergences from Go:

- `/api/meta` carries `metrics`/`agent*`/`lspServers` (agent/LSP values
  are empty placeholders until slices 10–11).
- Static 404s return `{"error":"not found"}` (Go serves its text 404).
- `-update` runs the self-update (slice 9).
- `-no-git`, `-no-telemetry`, `-no-lsp`, `-agent`, and `-no-agent`
  are live (slices 9–11).

### Slice 2 — indexing (done when `cargo test` is green)

Full `.gitignore` engine (`src/ignore.rs`: seg-eq/suffix/path-eq/regex
kinds with prefix/must guards, later-wins, subdir re-anchoring) plus the
bounded-concurrency walker (`src/index.rs`), `/api/tree` (with the 300ms
in-flight wait), `/api/reindex` (GET and POST), and meta
files/indexMs/builtAt. Git status overlay still lands with slice 8.

Divergences from Go:

- `builtAt` is UTC (`...Z`); Go emits local-offset time.
- Rust `$` also matches before a trailing newline, so compiled rules use
  `\z` (end-of-haystack) to match Go end-of-text semantics.

Verified by differential test (`/tmp/difftree.py`, since removed):
Go and Rust servers on a fixture with `**`, negations, dir-only rules,
an anchored file pattern, and a nested `.gitignore` returned 16/16
identical `(path, dir, ignored)` tree entries.

### Slice 3 — fuzzy find (done when `cargo test` is green)

`src/fuzzy.rs` ports `fuzzy.go` scoring byte-for-byte (two-pass match,
run/boundary/basename/camel/exact-case/verbatim weights, worker-chunked
ranking, score-desc/path-asc order) plus `GET /api/find` with the
0/1..500 limit clamp.

Verified by differential test (`/tmp/difffind.py`): 10 queries
(incl. empty, case-variant, spaced, no-match) on the Go repo returned
identical `/api/find` bodies from both servers.

### Slice 4 — workspace search (done when `cargo test` is green)

`src/search.rs` ports `search.go`: ASCII-folded literal path, regex/word
modes, glob filter via the ignore rule engine, 8 MB cap, binary skip,
`{Pre,Mid,Post}` snips, declaration patterns (gated behind
`classify_defs`, which only `/api/def` will set), sort + truncate, and
the unindexed-target single-file path. `GET /api/search`.

Divergences from Go:

- Bad-regex errors are 400 with the Rust regex crate's message text,
  which differs from Go's RE2 messages. Status and `{"error"}` shape
  match.
- Client-disconnect cancellation is not plumbed: a search runs to
  completion once started. Bounded and fast; revisit if profiles say
  otherwise.
- Wire key order: Go emits maps key-sorted and structs in declaration
  order. Rust `json!` sorts nested struct keys too, so all envelopes
  are typed `Serialize` structs with fields in exact wire order.

Verified by differential test (`/tmp/diffsearch.py`): 13/14 cases
byte-identical (literal, case, regex, word, glob, def-off, truncation,
empty); the 14th agrees on status 400 and differs only in regex error
text (see above).

### Slice 5 — file/raw/close + highlighting (done when `cargo test` is green)

`src/highlight.rs` ports `highlight.go` with syntect in place of Chroma:
line table, 1000-line windows + 400 context, 512 KB window cap, 64 MB
open cap, background full pass under 2 MB, byte-budgeted LRU eviction,
`Evict` on close, image branch, `GET /api/file|raw|close`. Scope stacks
map onto the short class alphabet (comment/string win, then innermost
specific scope); `lsp` brief is the `-no-lsp` shape, `diffAvailable`
false until the git slice.

Divergences from Go:

- Token boundaries differ (different lexer); only class names cross the
  wire, per the theme-compatible bar.
- `lang` names follow syntect grammars (`Markdown` vs `markdown`;
  extensions without a grammar fall back to plain text).
- `/api/raw` reads the whole file; no Range support (Go `ServeFile`).
- Close skips `FreeOSMemory` (no GC to hint).
- Regex ops can split multibyte chars; token edges snap to boundaries
  (regression test included — this once panicked a worker).

Verified by structural differential (`/tmp/difffile.py`): 7 files,
identical totals/maxCols, identical tag-stripped text incl. a deep
window, classes inside the known alphabet.

### Slice 6 — outline/def (done when `cargo test` is green)

`src/symbols.rs` ports `symbols.go`: per-family outline regexes,
markdown headings, noise filter, and `declPatterns` (moved out of
`search.rs` to a single owner). `GET /api/outline|def`; def partitions
whole-word hits into deduped declarations (stable basename-first sort)
and a ref count, with the `-no-lsp` brief.

Verified by differential test (`/tmp/diffoutline.py`): 12 outlines +
10 def queries byte-identical across Go, Rust, JS, shell, and markdown.

### Slice 7 — git diff/gutter + status overlay (done when `cargo test` is green)

`src/git.rs` ports `git.go`: memoized probe, porcelain v2 `-z` status
with repo-offset stripping, `HEAD` diffs, hunk ranges, `-no-git`
switch. The index overlays statuses concurrently with the walk;
`/api/diff|gutter` live, `meta.git` and file `diffAvailable` are real.

Divergences from Go: none found. Hunk numbers are signed (`[]int`
parity: a fully deleted file yields `-1`).

Verified by differential test (`/tmp/diffgit.py`): 12/12 match
(modified, staged-add, untracked, staged-delete, subdir, badges,
meta flag, diffAvailable).

### Slice 8 — markdown preview (done when `cargo test` is green)

`src/markdown.rs` ports `markdown.go` with pulldown-cmark and a custom
renderer: GFM tables/footnotes/strikethrough/tasklists/autolinks, raw
HTML passthrough, GitHub slugs (incl. link-destination counting, found
by probing Go), `data-line` on exactly Go's block set, `<pre
class="md-code" data-line data-lang>` fences highlighted with the code
view's classes, `GET /api/markdown` with the 415/404/413 guards.

Divergences from Go:

- Fence token streams differ (syntect vs Chroma); shapes identical.
- Loose-list item newlines and unreferenced-footnote rendering are
  unprobed guesses; hard breaks emit `<br />`.
- Fence info takes the first word as language (Chroma alias matching
  is broader).

Verified by differential test (`/tmp/diffmd.py`): AGENT and
CONTRIBUTING byte-identical; probe + README + architecture match
everywhere except fence token spans.

### Slice 9 — settings/metrics/telemetry/update (done when `cargo test` is green)

`src/settings.rs` ports `settings.go`: the verbatim settings schema,
merged/defaults/raw reads, agent↔`agent.harness` and
models↔`agent.models` bridges, null-deletes, `writeSettings`, and the
raw-text save. `src/metrics.rs` ports `metrics.go` (`/proc` RSS/CPU/
threads with the 200ms CPU gate). `src/telemetry.rs` ports
`telemetry.go` (PostHog queue + worker, opt-outs, buckets,
`~/.px0/anonymous_id`, `session_started/ended/stopped`). `src/update.rs`
ports `update.go` (exact `compareSemver`, `PX0_UPDATE_URL` fetch,
`checksums.txt` verification, exact-match `px0-<ver>-<goos>-<goarch>`
assets, daily state file, atomic swap) against
`eas4ai/px0-rust`. `GET+POST /api/settings` (with the `localPost`
Origin gate), `GET /api/metrics`, meta `metrics`/`agent*`/`lspServers`
fields, `--update` execution, background daily check, and
`session_started` tracking are wired in `server.rs`/`main.rs`.

Divergences from Go:

- Fence token streams differ (syntect vs Chroma); shapes identical.
  (Unchanged from slice 8; re-verified after the heading-frame cleanup.)
- `localPost` compares the raw Origin string against Host instead of
  parsing the URL; equivalent for well-formed origins.
- `runSelfUpdate` stages via `.px0-update-<pid>` in the binary dir and
  falls back to rename-into-place rather than Go's `CreateTemp` names;
  same atomicity, different temp names.

Verified by `cargo test` (63 lib + 20 api), `cargo clippy
--all-targets` with 0 warnings, `cargo fmt --check` clean, and a
`/tmp/diffmd.py` re-run (AGENT/CONTRIBUTING still byte-identical,
README fence-token-only diff unchanged).

### Slice 10 — LSP suite (done when `cargo test` is green)

`src/lsp.rs` ports the JSON-RPC client (`lsp.go`): framed stdio,
pending-call routing, stub replies to server-initiated requests,
`$/progress` indexing tracking, negotiated position encodings, and the
`shutdown`/`exit`/kill sequence. `src/lspnav.rs` ports `lspnav.go`
(NavHit resolve with the editor cache, Location/LocationLink decode,
definition/references/symbols walks, hover cards with the same
markdown flattening). `src/lspservers.rs` ports the 13-server registry
verbatim, background discovery with installer-dir lookup, spawn-on-use
with condvar waiters, bounded crash respawns, and the external-file
allowlist. `src/lspsetup.rs` ports install recipes/jobs with tail-log
capture. `src/calls.rs` ports call trails. All nine `/api/lsp/*`
routes, the file/def LSP briefs, `CloseDoc` on close, the allowlist in
every resolve site, and real `lspServers` in meta are wired.

Divergences from Go:

- Blocking client ops run on a thread pool with an `Instant` deadline
  instead of a context; timeouts still send `$/cancelRequest`.
- `lookPathIn` returns `Option` instead of `(string, bool)`.
- `locate` carries `#[allow(clippy::too_many_arguments)]` to keep Go's
  parameter order one-to-one.

Verified by `cargo test` (83 lib + 21 api), `cargo clippy
--all-targets` with 0 warnings, live `rust-analyzer` hover/def/refs/
symbols/calls against the binary, and a Go-vs-Rust
`/api/lsp/symbols` differential (22/22 identical).

### Slice 11 — agent suite (done when `cargo test` is green)

`src/agent.rs` ports `agent.go`: the eight harness presets verbatim,
per-harness background model discovery (`agy`/`cursor-agent`/`claude`/
`opencode` parsers), spec resolution with model-flag insertion,
settings persistence outside the workspace, overlap-gated parallel
dispatch with 10-minute timeout and kill-on-cancel, git
size+mtime snapshots for change detection, highlight/LSP settle, and
the prompt/line-ref/shell-quote helpers. All five `/api/agent/*`
routes, real `agent*` meta fields, `-agent` pinning (fatal on a bad
spec), `--no-agent`, and shutdown cancel are wired.

Divergences from Go:

- Harness output streams into the tail log only; the `│`-prefixed
  terminal echo is skipped (the Rust binary does no `uiStatus`
  narration anywhere).
- The `force` parameter is accepted and ignored, exactly like Go's
  `Start`, which never consults it either (`errAgentDirty` maps to
  409 in the handler but no path produces it, as in Go).
- Numeric query params that fail to parse are 422 (axum) rather than
  Go's `Atoi`-ignores-errors 0; the UI always sends valid numbers.

Verified by `cargo test` (93 lib + 27 api), `cargo clippy
--all-targets` with 0 warnings, and a live binary run: real harness
discovery (6 installed found, aider/goose correctly missing, claude
models refreshed from its own CLI), template select, dispatched edit,
`changed: [keep.go]`, and prompt contents.

## Planned slices

12. Hardening (dist matrix, perf gates).
