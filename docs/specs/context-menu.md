# Editor context menu — spec

## Goal

Extend the editor's right-click menu from four clipboard/edit actions into a grouped navigate-plus-edit menu, with honest disabled states, keyboard access, and no visual redesign.

## Success Criteria

- Right-clicking a selection shows seven items in two groups: Navigate (Go to Definition, Find Usages, Call Trail, Reveal in Tree) and Edit (Copy Ref, Copy with Context, Edit Inline).
- Edit Inline is disabled with an explanatory hint when no coding harness is installed, instead of silently doing nothing.
- The menu is keyboard-operable (arrows, Enter, Esc) and keeps the existing edge-flipping placement.
- `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`, and the web-bundle freshness gate pass. No backend changes.

## Context And Current Facts

- `web/src/selbar.js` owns the menu today: `SEL_MENU_ITEMS` (4 items), `openSelMenu` (rebuilds buttons each open, flips at viewport edges), `runSelectionAction` dispatch, and a `contextmenu` handler that only takes over right-clicks on a selection (`web/src/selbar.js:196-260`).
- All three new actions accept a word argument and degrade gracefully: `gotoDefinition` (`web/src/lsp.js:57`) falls back past an unready server, `showCalls` (`web/src/calls.js:42`) offers install/start when no server exists, `revealFile` (`web/src/tree.js:71`) no-ops visibly when the path is absent.
- Harness availability is `S.meta.agents` with per-harness `installed` flags, loaded asynchronously by `loadAgentAsync` (`web/src/agent.js:31,95-99`).
- Menu styling reuses `.sel-menu-item`, `.footer-kbd`, and theme vars; shortcut labels render through `keyLabel`.
- User decision already made: Navigate + edit set (no Copy File Path, no Send to Terminal).

## Constraints And Non-goals

- Non-goals: submenu nesting, icons, menu search, touch long-press behavior changes, any backend or API work.
- The menu stays a single flat list under ~8 items; if it ever grows past that, split before adding scroll.

## Key Decisions

- **Grouping with one separator.** Navigate first (the selection names something to go to), Edit second (things done with the text). One `role="separator"` between them. Rejected: interleaved ordering (hides the two intents) and section headers (visual noise for seven items).
- **Disabled, not hidden, with reasons.** Edit Inline renders `aria-disabled` plus a hint line when `S.meta.agents` is loaded and nothing is installed; while the harness list is still loading, the item stays enabled (optimistic, avoids flicker). Hidden items teach users the menu is unpredictable; a reason teaches the fix.
- **Keyboard nav, minimal.** Up/Down moves, Enter runs, Esc closes and returns focus to the editor. No type-ahead, no mnemonics — seven items do not need them.
- **No new visual language.** Existing item styles, theme vars, and `kbd` hints; the separator reuses `--line`. Per taste rules: no icons, no accent restyle, motion limited to the existing show/hide (no entrance animation on a high-frequency control).

## Recommended Approach

Extend `selbar.js` in place: grow `SEL_MENU_ITEMS` with group metadata, render one separator, thread availability into `openSelMenu`, add a small key handler while open. Wire the three new actions through the existing `runSelectionAction` dispatch so footer buttons and menu stay the same code path.

## Work Plan

1. **Items and groups (`web/src/selbar.js`)**: add `go-def` (F12), `calls` (Alt+Shift+H), `reveal` (no shortcut) with `group: 'navigate'`; tag the existing four `group: 'edit'`; render a separator between groups; extend `runSelectionAction` with the three cases (`gotoDefinition(word)`, `showCalls(word)`, `revealFile(currentFile)`).
2. **Disabled states**: `Edit Inline` disabled unless an installed harness is known (`S.meta.agents` loaded and non-empty installed, or harness list not yet loaded); hint text names the fix ("install a harness"); `aria-disabled` plus skipped in arrow nav.
3. **Keyboard access**: Up/Down/Enter/Esc handling scoped to the open menu; focus returns to the editor on close; existing outside-click and `mousedown` selection-preservation behavior unchanged.
4. **Docs**: shortcut sheet entries for the menu items that have keys (F12 and Call Trail already listed; verify); README shortcuts table gains nothing new (all covered) — add one line under the agent section only if the disabled-state hint needs explaining. Rebuild `web/app.js` with the committed script.

## Validation Plan

- `cargo test`, `cargo clippy --all-targets`, `cargo fmt --check`, `npm test`, `npm run lint`, `npm run build && git diff --exit-code -- web/app.js`.
- Manual matrix in the browser: right-click with selection (7 items, 2 groups), without harness installed (Edit Inline disabled with hint), with harness (enabled), each nav item on a symbol and on plain text, edge-flip near viewport corners, arrow/Enter/Esc operation, footer buttons unchanged.
- Highest-risk check: the disabled-state timing (agents list loads async after boot) — verify both before-load (enabled) and after-load-empty (disabled) states.

## Risks / Rollback

- `revealFile` needs the open file's path alongside the word: the selection object already carries it; no signature changes required.
- Rollback is a revert of `selbar.js` (+ bundle rebuild); no other module changes shape.

## Open Questions

None.
