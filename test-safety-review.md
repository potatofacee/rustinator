# Rustinator — Correctness Review & Test-Safety Plan

Scope: correctness audit of the rustinator terminal emulator plus a regression-test
plan that pins current behavior **before** the planned refactors (Phase 3 `pane_ui`
`draw_panes` extraction, Phase 4a resize debounce, and the keyboard/menu action-dispatch
unification) touch the files that today have zero tests.

---

## 1. Executive Summary

### Confirmed bugs (each survived a 3-skeptic adversarial panel)

| Severity | Count |
|----------|-------|
| High     | 3     |
| Medium   | 12    |
| Low      | 9     |
| **Total**| **24**|

Two pairs are the **same defect seen from two review dimensions**, so there are ~22
distinct root causes:

- `paste_from_clipboard` read-only/broadcast bypass (medium, "input" lens) **==** read-only
  bypassed by `Action::Paste` (high, "lifecycle" lens) — one site, `tabs.rs:539`.
- `close_tab` redundant focus-in for a higher-indexed tab (low, "layout") **==** for a
  background tab (low, "lifecycle") — one site, `tabs.rs:235-237`.

Two findings were **2-1 panel splits** (one skeptic refuted): restore_layout silent
focus reassignment, and migrate_legacy dropping profiles. Both require hand-edited
state / partial migration and are filed **low**.

The single biggest theme: **`read_only` is enforced at exactly one chokepoint** (the
keystroke target filter in `main.rs:611-615`). Every other input path — clipboard
paste, middle-click primary paste, and mouse-wheel alternate-scroll — bypasses it
(bugs covering `tabs.rs:539`, `tabs.rs:549`, `pane_ui.rs:540`). Fix once at a shared
input gate.

### Test-gap count

- **82 proposed test specs** → **75 distinct tests** after consolidating 7
  overlapping specs (four `tabs.rs` helpers were proposed twice under the lifecycle
  and layout dimensions; `cell_at`/`cell_side` twice under input and url).
- Distribution: **48 P0, 25 P1, 2 P2**.
- ~27 of these require a **zero-behavior-change pure extraction** first (pull a closure
  or arithmetic block out of a giant UI/dispatch function into a free fn so it can be
  unit-tested). Those extractions ARE the safe first move before the refactor proper.

### The 3 riskiest UNTESTED areas

1. **`src/tabs.rs` — 0 tests, 9 of the 24 confirmed bugs live here.** Includes both
   high-severity read-only paste bypasses, a hard panic when the last tab closes, the
   wrong-neighbor focus bug, and four focus-event bugs. It is also the epicenter of the
   dispatch-unification refactor (close/switch/move/focus all funnel through it). Highest
   bug density in the codebase with zero safety net.
2. **`src/pane_ui.rs` — 0 tests, epicenter of the Phase 3 `draw_panes` extraction.**
   Every click-to-cell mapping, selection side, mouse-report dedupe, drag-drop direction,
   URL hit-test, and the read-only wheel-scroll bypass (bug `pane_ui.rs:540`) live in one
   massive function that Phase 3 will cut apart. An off-by-one in `cell_at` silently
   mis-targets every selection, mouse report, and URL click.
3. **`src/pty_event_loop.rs` — 0 tests, directly in Phase 4a's path.** The PTY write
   loop (`State`/`Writing`) has a known empty-buffer busy-loop trap, and `PeekableReceiver`
   guards against dropped frames / premature `stop_sync`. Phase 4a resize debounce reroutes
   message cadence straight through this code. No confirmed bug yet, but the most dangerous
   *untested* concurrency/IO logic to perturb blind.

Honorable mention: **`src/main.rs`** (0 tests) — hosts the font-zoom-anchor bug
(`main.rs:446`), the `apply_prefs` atomic-not-updated latent bug, and `execute_pane_actions`,
which is where the close-last-tab panic actually crashes.

---

## 2. Confirmed Bugs (ranked by severity)

### HIGH

#### H1 — `read_only` bypassed by clipboard paste  ·  `src/tabs.rs:539`
- **Wrong:** `read_only` is enforced only by the keystroke target filter
  (`main.rs:611-614`). `paste_from_clipboard()` writes clipboard text straight to
  `tab.panes.get(&tab.focused)` via `send_paste -> send_bytes`, which only checks `dead`.
  `Action::Paste` (default Ctrl+Shift+V / Cmd+V) dispatches here; `input.rs:82-84`
  deliberately skips the read-only-gated egui-paste send when `Action::Paste` is present,
  so the *only* paste that runs is the ungated one. Reachable via the right-click context
  menu "Paste" and via broadcast-mode keyboard paste.
- **Failure:** Pane set read-only (e.g. a live prod SSH session). User pastes (keybind in
  broadcast mode, or context-menu "Paste"). Clipboard text/commands are injected into the
  pane the user explicitly protected.
- **Fix:** Route paste through the same target selection as keystrokes — filter
  `!read_only`, fan out when broadcast. Best done by extracting `select_input_targets()`
  and calling it from both `paste_from_clipboard` and the keystroke path. Add a
  defense-in-depth read-only gate on a user-input wrapper (NOT raw `send_bytes`, which also
  carries terminal responses).
- **Note:** The "input"-dimension finding *paste ignores broadcast & read-only* (medium) is
  the same site viewed via the broadcast-divergence angle.

#### H2 — `read_only` bypassed by middle-click primary paste  ·  `src/tabs.rs:549`
- **Wrong:** `paste_primary()` (middle-click, `pane_ui.rs:198-199`) sends X primary-selection
  text via `send_paste` with no `read_only` check — same single-point-of-enforcement gap.
- **Failure:** Pane is read-only; a real or accidental middle-button/trackpad click inside it
  injects the primary selection into the protected PTY. Live on Linux (X PRIMARY) and macOS
  (in-process PRIMARY_BUFFER).
- **Fix:** Gate `paste_primary` on `!pane.read_only` (or route through the shared input gate
  from H1).

#### H3 — Kitty disambiguate mode escape-encodes Shift-only text keys  ·  `src/keyboard.rs:26`
- **Wrong:** In `DISAMBIGUATE_ESC_CODES` mode, `emit_csi_u = has_mod || key == Escape`, and
  `has_mod = encode_mods(mods) > 1` is true for **Shift alone**. The kitty protocol requires
  text-producing keys with only Shift to be reported as literal text; CSI u is only for keys
  with a non-Shift modifier (or Esc / functional keys). `pane.send_key()` returns
  `keyboard::encode()`'s bytes early, discarding the legacy text byte.
- **Failure:** App enables `CSI > 1 u` (fish, neovim). User types `Shift+A` → sent as
  `\x1b[97;2u` instead of `A`; `Shift+2` (US `@`) → `\x1b[50;2u`. Every capital letter and
  shifted symbol is mis-sent; shifted punctuation is genuinely unrecoverable at protocol
  level 1 (no associated-text/alternate-key reporting).
- **Fix:** In the disambiguate branch compute `has_non_shift_mod = ctrl||alt||super` and use
  it (not `has_mod`) to decide CSI-u for text keys; let Shift-only text keys fall through to
  the legacy bytes.

> **Cluster note — single-point `read_only` enforcement.** H1, H2, plus medium M2
> (wheel alternate-scroll) and medium M3 (paste broadcast divergence) are all the same
> structural gap. Introduce one `Pane::send_input()` (or a routing-layer gate) that checks
> `read_only` and is used by send_key, send_paste, paste_primary, and the alternate-scroll
> path; keep raw `send_bytes` for terminal responses (OSC replies, focus events, DA) which
> must NOT be suppressed.

### MEDIUM

#### M1 — Closing the last tab panics on a batched follow-up action  ·  `src/tabs.rs:225`
- **Wrong:** `close_tab` removes the only tab, queues `ViewportCommand::Close` (processed
  async at frame end), and returns with `self.active_tab` left at 0 while `self.tabs` is now
  empty. `execute_pane_actions` (`main.rs:320`) iterates a `Vec<Action>` with no emptiness
  guard between actions; several arms index `self.tab_mgr.tabs[self.tab_mgr.active_tab]`
  directly (e.g. `main.rs:375` RotateCW). The per-update guards only protect the *next* frame.
- **Failure:** Single tab/pane. Two key events coalesce into one governed frame producing
  `actions = [ClosePane, RotateCW]` (fast typing / autorepeat / any main-thread stall).
  ClosePane empties `tabs`; the next action indexes `tabs[0]` → index-out-of-bounds panic,
  crashing the emulator before the window closes. A second batched `ClosePane` panics the
  same way via `active_tab_mut`.
- **Fix:** When `close_tab` empties the Vec, set a "closing" flag and bail out of the rest of
  the action batch (or guard `execute_pane_actions` with `if tabs.is_empty() { break }`
  between actions).

#### M2 — `read_only` bypassed by mouse-wheel alternate-scroll keystrokes  ·  `src/pane_ui.rs:540`
- **Wrong:** On the alt screen with `ALTERNATE_SCROLL` (mode 1007, on by default in
  alacritty_terminal 0.26), wheel ticks are translated to arrow-key escape sequences and sent
  via `pane.send_bytes()` up to 8×, with no `read_only` check. `handle_pane_mouse` runs for
  every hovered pane regardless of read-only.
- **Failure:** Read-only pane running vim/less/man; scrolling injects arrow-key sequences,
  moving the cursor / scrolling the app despite read-only.
- **Fix:** Gate the alternate-scroll send on `!pane.read_only` (shared input gate from H1).

#### M3 — Alt-screen passthrough uses `any()` over all broadcast targets  ·  `src/input.rs:31`
- **Wrong:** `alt_screen = targets.iter().any(|p| p.mode().contains(ALT_SCREEN))`. Correct for
  the single focused pane, but in broadcast mode `targets` is every non-read-only pane, so one
  background alt-screen pane makes `is_alt_screen_passthrough` fire for the nav bindings
  (Ctrl+Tab / Ctrl+PageUp / Ctrl+PageDown).
- **Failure:** Broadcast ON; focused pane A is a shell, background pane B runs vim. Ctrl+PageDown
  → `any()` true → NextTab is never pushed and the key is sent to BOTH panes. Tab never switches.
  Fires on Linux via Ctrl+PageDown and on both platforms via Ctrl+Tab.
- **Fix:** Compute `alt_screen` from the **focused** pane only (the design comment already says
  "when the focused pane is in alt-screen").

#### M4 — `close_pane` sends focus-in to a pane in a background tab  ·  `src/tabs.rs:266`
- **Wrong:** `close_pane(tab_idx, …)` is called by `reap_exited` for ANY tab index. When the
  dead pane was that tab's focused pane it reassigns focus and unconditionally calls
  `send_focus_event(true)` on the successor — even when `tab_idx` is a background tab.
  `send_focus_event` only checks the pane's `FOCUS_IN_OUT` mode, not active-tab state.
- **Failure:** Background tab 2 has pane A (focused, exits) and pane B (vim w/ focus reporting).
  `sleep 5; exit` in A while you're on tab 1 → B gets a spurious `\x1b[I` while backgrounded,
  redraws as if focused, then gets a second focus-in when you actually switch to tab 2.
- **Fix:** Only emit focus-in when `tab_idx == active_tab`.

#### M5 — `close_focused` focuses the wrong neighbor when a right-child leaf is closed  ·  `src/tabs.rs:194`
- **Wrong:** New focus prefers the in-order **successor** `before[i+1]` before the predecessor.
  That successor only occupies the freed region when the closed leaf was a LEFT child. For a
  right-child leaf whose subtree is followed in-order by another leaf, the pane that actually
  expands is the **predecessor**, but the code picks the successor.
- **Failure:** Layout `Split(Split(L1,L2), L3)`, focus L2. Close L2 → inner split collapses to
  L1 (L1 takes the space), but focus jumps to L3.
- **Fix:** Pick the leaf that now occupies the freed rect — follow `remove_leaf`'s promotion
  (predecessor when the removed leaf was a right child), or compare geometry.

#### M6 — `)` is always a URL boundary, truncating URLs with parens  ·  `src/pane.rs:440`
- **Wrong:** `is_url_boundary(')')` is unconditional; `(` is not a boundary. The forward scan
  consumes `(` but stops at the first `)`. No balanced-paren handling (alacritty balances them).
  The truncated string is spawned verbatim to `open`/`xdg-open`.
- **Failure:** `https://en.wikipedia.org/wiki/Foo_(bar)_baz` → opens `…/Foo_(bar`, which 404s.
- **Fix:** Balance parens — allow `)` inside the URL when an unmatched `(` precedes it in the
  span (alacritty's approach).

#### M7 — Scheme/`www` detection is case-sensitive  ·  `src/pane.rs:457`
- **Wrong:** Scheme literals and the `www.` check compare lowercase bytes against the raw cell
  char with no case folding. `HTTP://`, `HTTPS://`, `WWW.`, `Mailto:` never match (and lack `@`,
  so the email branch misses too). alacritty/iTerm2 match schemes case-insensitively.
- **Failure:** `HTTPS://EXAMPLE.COM` / `Visit WWW.Example.com` get no Ctrl-hover highlight and
  Ctrl-click does nothing.
- **Fix:** Compare scheme/`www` prefixes with `eq_ignore_ascii_case`.

#### M8 — HIDDEN (conceal) text leaks when DIM is also set  ·  `src/pane.rs:1237`
- **Wrong:** `snapshot()` applies INVERSE → HIDDEN (`fg=bg`) → DIM (`fg*=0.66`). DIM runs AFTER
  HIDDEN, so a HIDDEN+DIM cell ends with `fg = 0.66*bg != bg`, breaking the conceal invariant
  the renderer relies on (`fg==bg`). The concealed glyph and its None-colored underline/strikeout
  render at ~66% brightness over bg.
- **Failure:** `ESC[2;8m` (dim+conceal) then a password on the default `#1a1a1a` bg → text is
  faintly visible. `ESC[7;2;8m` (inverse+dim+conceal) is more blatant.
- **Fix:** Apply DIM **before** HIDDEN, or skip DIM when HIDDEN is set.

#### M9 — Cmd/Super + named-key combos send stray control bytes (macOS)  ·  `src/input.rs:149`
- **Wrong:** `encode_raw_key` runs the `encode_named_key()` block (139-147) BEFORE the
  `if mods.mac_cmd` byte-less guard (149). `encode_named_key` ignores cmd/super, so any Named key
  with shift/alt/ctrl all false returns its unmodified legacy bytes and returns early. Only
  Character keys correctly fall through to the guard.
- **Failure:** Cmd+Left sends `\x1b[D`, Cmd+Backspace sends `0x7f`, Cmd+Enter sends `\r`,
  Cmd+Up/Down send arrows — instead of being inert shortcuts. (Cmd+PageUp/Down are bound to
  Prev/NextTab and are correctly consumed.)
- **Fix:** Move the `mac_cmd` guard above the `encode_named_key` block (or short-circuit
  `encode_named_key` when `mac_cmd` is set).

#### M10 — Prefs font-size change leaves zoom anchor stale  ·  `src/main.rs:446`
- **Wrong:** `apply_prefs()` reloads the font via `reload_font(...)`, which updates only
  `cell_w/cell_h/ctx` and never touches `FontState.base_size`/`size_override`. `base_size` is
  set once in `App::new`. After a Prefs size change, `base_size` keeps the OLD size.
- **Failure:** Config 12 → Prefs set 18 (renders 18). Ctrl+0 resets to 12; Ctrl+= computes
  `unwrap_or(12)=12 → 13`, so the font *shrinks* from 18 to 13.
- **Fix:** After `reload_font` on a size change, set `base_size` to the new size and clear
  `size_override`.

#### M11 — `respawn` omits `working_directory`; restarted pane starts at `/`  ·  `src/pane.rs:816`
- **Wrong:** `spawn()` sets `pty_opts.working_directory` to the requested dir or, as a macOS
  fallback, `$HOME` (to avoid the `/` cwd a Dock-launched .app inherits from launchd).
  `respawn()` builds `pty_opts` but never sets `working_directory`, so the new child inherits
  the process cwd. (Verified: with `login -l`, the login shell does not cd to `$HOME` on its own.)
- **Failure:** macOS .app from Dock (cwd `/`), `ExitAction=Restart`, shell exits → respawned
  shell starts in `/` instead of `$HOME`.
- **Fix:** Set `respawn`'s `working_directory` the same way `spawn` does; factor the resolution
  (requested dir → `$HOME` fallback) into a shared helper.

#### M12 — `migrate_legacy` discards `[[profiles]]` when any legacy field present  ·  `src/config.rs:414`  *(2-1 panel)*
- **Wrong:** When any legacy `font/colors/scrollback` field is present, `migrate_legacy` does
  `self.profiles = vec![single_default]`, unconditionally overwriting profiles parsed from
  `[[profiles]]`.
- **Failure:** A hand-edited config with a leftover `[colors]` block plus two `[[profiles]]`
  entries loses both profiles on load. The app never writes such a file itself (legacy fields are
  `skip_serializing`), so this needs manual/partial-migration input — hence **low/medium** and
  the one refutation.
- **Fix:** Only replace `self.profiles` from legacy fields when `self.profiles` is empty.

### LOW

#### L1 — Legacy (X10) mouse release drops modifier bits  ·  `src/mouse.rs:107`
- **Wrong:** In the non-SGR branch, a release event forces `cb_out = 3`, discarding the
  shift/alt/ctrl bits OR'd into `cb`. xterm/alacritty keep the modifier bits on release
  (`3 + mods`). The SGR branch is fine (uses `cb`). Shift is gated out upstream, but Alt-click
  and Ctrl-click on a non-URL cell reach this path.
- **Fix:** Emit `3 + mods` (or `cb` with the button bits set to 3) on legacy release.

#### L2 — `close_tab` re-sends focus-in to the already-focused active pane  ·  `src/tabs.rs:235-237`
- **Wrong:** When `close_tab(idx)` is called with `idx != active_tab` (close button on a
  background tab, or reap of a background single-pane tab), `active_tab` and its focused pane
  never lost focus, yet `close_tab` unconditionally calls `send_focus_event(true)` on the active
  pane — an unpaired `\x1b[I` to an app with focus reporting on. (Merges the two same-site
  findings filed under layout and lifecycle.)
- **Fix:** Only emit focus-in when the active tab actually changed (sibling methods all guard
  `old != new`).

#### L3 — Configured `selection_foreground` ignored when `selection_background` unset  ·  `src/pane.rs:1248`
- **Wrong:** The selection-color match reads `selection_fg` only inside the `Some(selection_bg)`
  arm; the `None` arm does a classic fg/bg invert and never reads `selection_fg`.
- **Failure:** Profile sets `selection_foreground=#ffff00`, leaves `selection_background` empty →
  the configured yellow is dropped.
- **Fix:** Consult both Options independently; apply `selection_fg` even when `selection_bg` is None.

#### L4 — OSC 12 (dynamic cursor color) ignored  ·  `src/pane.rs:1179`
- **Wrong:** `cursor_color` is built from `self.defaults.cursor` and never consults
  `content.colors[NamedColor::Cursor]`, unlike every cell color (which goes through
  `resolve_color`). alacritty stores OSC-12 in `colors[Cursor]`; the overlay ignores it.
- **Failure:** App emits `OSC 12;#ff0000` → the Beam/Underline/HollowBlock cursor stays the
  profile color.
- **Fix:** Resolve cursor color from the live palette with the profile default as fallback.

#### L5 — `restore_layout` reassigns focus silently  ·  `src/tabs.rs:626`  *(2-1 panel)*
- **Wrong:** Every other focus-changing method emits paired focus-out/in; `restore_layout`
  reassigns `tab.focused` to `leaves.first()` when the old focus was trimmed but emits no focus
  events. (The search_pane "dangle" is a guarded no-op — `run_search` early-returns on a missing
  pane and ids are monotonic — which is why one skeptic refuted the compound claim; the
  focus-in omission itself is real.)
- **Fix:** Emit `send_focus_event(true)` on the newly focused pane (and focus-out on the old),
  mirroring `close_pane`.

#### L6 — Double-width (CJK/IDN/emoji) chars truncate URLs at the WIDE_CHAR_SPACER  ·  `src/pane.rs:475`
- **Wrong:** A wide glyph emits two snapshot cells; the spacer's `c` is `' '`. `is_url_boundary(' ')`
  is true, so the forward scan halts on the spacer after the first wide char.
- **Failure:** `http://例え.jp/x` → matches `http://例`, losing the rest.
- **Fix:** Skip WIDE_CHAR_SPACER cells during the URL scan (carry the spacer flag in CellSnapshot,
  or detect and skip).

#### L7 — Trailing sentence punctuation swallowed into the URL  ·  `src/pane.rs:440`
- **Wrong:** `is_url_boundary` excludes `.` `!` `?` `:` (needed mid-URL) but does not strip a
  single trailing one. `,` and `)` ARE stripped — inconsistent.
- **Failure:** `See https://example.com.` opens `https://example.com.`; `…com:` yields an
  empty-port URL.
- **Fix:** Strip a single trailing `.`/`!`/`?`/`,` from the matched span before returning.

---

## 3. Missing-Test Plan (by module, P0 → P2)

**Conventions.** `kind`: unit (pure, no IO) or integration (needs a test seam). "Extraction"
means a *zero-behavior-change* pure-fn lift that must land as its own verified commit before
the test (and before the refactor). "Unblocks" names the refactor the test makes safe.
All tests are `cargo`-runnable; integration tests that need a fake-Pane / `EventLoopSender`
seam are flagged.

> **Consolidation:** four `tabs.rs` helpers and the two `pane_ui` geometry helpers were each
> proposed twice (lifecycle+layout, input+url). They are merged below into one canonical test
> apiece; alternate proposed names noted in parentheses.

### `src/tabs.rs`  *(0 tests today — top priority)*

**P0**

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `select_input_targets_broadcast_excludes_read_only` | `select_input_targets` (NEW, shared by `main.rs` targets + `paste_from_clipboard`) | broadcast fans to every non-read-only pane: `(true,1,[(1,f),(2,t),(3,f)]) == [1,3]` | Dispatch unification; **fixes H1/M3** when paste is routed through it |
| `select_input_targets_focused_only_empty_when_read_only` | `select_input_targets` (NEW) | `(false,2,[(1,f),(2,f)])==[2]`; `(false,1,[(1,t)])==[]` (read-only focused → no target) | Dispatch unification |
| `pick_new_focus_prefers_successor_then_predecessor_then_first` (aka `pick_neighbor`) | `pick_new_focus` (NEW, from `close_focused` 193-202) | successor `([1,2,3],Some(0),[2,3])==Some(2)`; predecessor `(…,Some(2),[1,2])==Some(2)`; first `(…,None,[1,2])==Some(1)`; none `(…,Some(1),[])==None`; **documents M5**: `([1,2,3],Some(1),[1,3])==Some(3)` (current buggy behavior, locked so the fix is a deliberate change) | Dispatch unification |
| `focus_transition_guards_noop_and_pairs` | `focus_transition` (NEW, from `set_focused_pane`) | `(5,5,true)==None`; `(5,6,false)==None`; `(5,6,true)==Some((5,6))` (exactly one out-then-in pair) | Dispatch unification (new focus entry points) |
| `wrap_index_wraps_and_noops` (aka `next_tab_index`) | `wrap_index` (NEW, dedupes `switch_tab`/`cycle_focus`/`move_tab`) | `(2,1,3)==0`; `(0,-1,3)==2`; `(0,5,3)==2`; `(0,-4,3)==2`; `(0,1,1)==0`; `(2,3,3)==2` (full-cycle → old index, drives no-op guard) | Dispatch unification (Next/Prev/Move tab + FocusNext/Prev) |
| `adjust_active_tab_fixup` (aka `adjust_active`) | `adjust_active_tab` (NEW, from `close_tab` 229-233) | clamp `(2,0,2)==1`; decrement-below `(2,0,3)==1`; clamp-off-end `(3,3,3)==2`; higher-closed unchanged `(1,2,3)==1` | Dispatch unification + reap |
| `adjust_search_pane_reindexes_and_clears` (aka `shift_search_pane`) | `adjust_search_pane` (NEW, from `close_tab` 216-223) | same-tab `(Some((1,7)),1)==(None,true)`; higher `(Some((2,7)),0)==(Some((1,7)),false)`; lower unchanged; `None` → `(None,false)` | Dispatch unification + reap |
| `reconcile_ids_under_over_and_equal` | `reconcile_ids` (NEW, from `restore_layout` 596-617) | under `([1,2],4)==([1,2],2,[])`; over `([1,2,3,4],2)==([1,2],0,[3,4])`; equal `([1,2,3],3)==([1,2,3],0,[])` | SaveLayout/RestoreLayout dispatch |

**P1**

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `reap_step_maps_exit_action_to_lifecycle_op` | `reap_step` (NEW, from `reap_exited`) | `Close→Close`, `Hold→Hold`, `Restart→Restart` (arm selection only) | Phase 4a lifecycle edits |
| `restore_layout_rolls_back_spawned_panes_on_failure` | `restore_layout` rollback | **(integration, needs spawn-stub seam)** on spawn failure every spawned id is removed and `layout/focused/zoomed` are byte-for-byte unchanged | Phase 3/4 pane spawning |
| `reap_exited_hold_clears_flag_in_one_pass` | `reap_exited` Hold | **(integration, needs fake-Pane seam)** loop terminates, pane present, `exited` cleared, pane marked dead; re-run is a no-op (guards spin-forever) | Phase 4a + dispatch |
| `reap_exited_close_single_pane_closes_tab` | `reap_exited` Close | **(integration, fake-Pane seam)** exited lone pane empties its tab → `close_tab` fires, `tabs.len()` 2→1, loop exits cleanly | Dispatch unification |

### `src/pane_ui.rs`  *(0 tests today — Phase 3 epicenter)*

**P0** — add a `#[cfg(test)] mod tests` (none exists).

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `cell_at_maps_clamps_and_ppp_scales` (merges input+url specs) | `cell_at` (pure, present) | interior `pos(25,30)→(2,1)`; above-left clamps to `(0,0)`; ppp=2 doubles effective coord (`pos(15,0)→(1,0)` vs `(0,0)` at ppp=1) | Phase 3 `draw_panes` extraction |
| `cell_side_half_cell_threshold_inclusive_right` (merges input+url) | `cell_side` (pure, present) | `4.9%10 → Left`, `5.0 → Right` (inclusive `>=`), across cells `14.9→Left`, `15.0→Right` | Phase 3 |
| `motion_report_suppresses_same_cell_reports_changed` | `motion_report` (NEW, from motion block 451-460) | same cell `(drag,true,Some((5,5)),(5,5))==(false,Some((5,5)))`; changed `…(6,5))==(true,Some((6,5)))`; no-button records but stays silent | Phase 3 (inline last_cell dedupe) |

**P1**

| Test | Target | Asserts | Unblocks |
|------|--------|---------|----------|
| `drop_zone_direction_nearest_edge_and_center_tiebreak` | `drop_zone_direction` (pure) | left→`(Vertical,true)`; bottom→`(Horizontal,false)`; exact center deterministically `(Vertical,true)` via first `min==dist_left` branch | Phase 3 drag-drop split |

### `src/pane.rs`  *(55 tests, but lifecycle/snapshot/url gaps)*

**P0 — lifecycle**

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `event_proxy_wakeup_coalesces_dirty_and_new_output` | `EventProxy::send_event` Wakeup (no refactor; `EventProxy::new(None, ws)`) | two Wakeups w/ visible=false → `dirty` & `has_new_output` & `wake_pending` true; second send coalesced; no panic | Phase 4a wake/dirty path |
| `event_proxy_exit_sets_exited_only` | Exit arm | `exited` true, `dirty` untouched (Exit must not enter the output-coalesce path) | Phase 4a lifecycle |
| `encode_paste_normalizes_crlf_and_lf_to_cr` | `encode_paste` (NEW, from `send_paste`) | `("a\r\nb\nc",false)==b"a\rb\rc"` (locks CRLF-before-LF replacement order) | Paste dispatch |
| `encode_paste_wraps_in_bracketed_markers` | `encode_paste` (NEW) | `("a\nb",true)==b"\x1b[200~a\rb\x1b[201~"`; `("x",false)` has no markers | Paste dispatch |
| `focus_event_bytes_gates_on_focus_in_out` | `focus_event_bytes` (NEW, from `send_focus_event`) | empty→None; FOCUS_IN_OUT+focused→`\x1b[I`; +unfocused→`\x1b[O` | Dispatch + 6 tab/close focus sites |

**P0 — snapshot**

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `apply_cell_attrs_inverse_hidden_selection_layering` | `apply_cell_attrs` (NEW, from snapshot 1231-1259) | INVERSE swaps; INVERSE\|HIDDEN → `fg==bg==A`; selection `None`→invert; selection `Some(bg)`+`Some(fg)`→profile colors win; selection `Some(bg)`+`None`→cell fg kept | Phase 3 / snapshot tidy |
| `resolve_color_fallbacks_when_palette_unset` | `resolve_color` (pure, present) | `Background`→bg; `Foreground` as bg (`!is_fg`) → bg not fg; `Foreground` as fg → fg; `Indexed(196)` → `(255,0,0)` | Phase 3 color pipeline |

**P0 — url** (add to existing test mod)

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `scan_url_at_runspan_middle_click_bounded_by_distinct_uris` | `scan_url_at` | mid-run click (col 4 of run A): leftward+forward run-span extension, `start_col==2`,`end_col==7`, not merged with distinct-URI neighbors | Phase 3 cell-collection/order |
| `scan_url_at_explicit_hyperlink_beats_heuristic` | `scan_url_at` (precedence 580 vs 609) | cell with both OSC-8 link and heuristic-matchable text → returns the explicit URI, single-cell span | Phase 3 phase order |
| `match_urls_email_span_columns_exact` | `match_urls_in_row` email (529-536) | `"x user@a.com"` → `start_col==2`,`end_col==12` (note: gap's 13 is an off-by-one) | Phase 3 hit-test extraction |
| `scan_url_at_picks_correct_url_among_multiple_and_boundary_space_none` | `scan_url_at` per-column find (618) | two URLs on a row: click col 8→A, col 20→B, boundary space col 13→None | Phase 3 hit-test |
| `url_at_click_resolves_wide_spacer_then_scans_hyperlink` | `url_at_click` (NEW = `resolve_wide_click_col`∘`scan_url_at`) | click on wide-glyph spacer col resolves to base col before scan → matches the hyperlink; raw `scan_url_at` without resolve returns None | Phase 3 mouse extraction |

**P1**

| Test | Target (extraction) | Asserts |
|------|--------------------|---------|
| `event_proxy_bell_sets_dirty_not_output_flags` | Bell arm | dirty true; `has_new_output`/`wake_pending` false |
| `event_proxy_title_and_reset_title_set_state` | Title/ResetTitle | title set then cleared; dirty true |
| `apply_cell_attrs_hidden_dim_conceals` | `apply_cell_attrs` | invariant `fg==bg` for HIDDEN\|DIM — **documents M8** (currently fails: land `#[ignore]`-with-reason or fix ordering first) |
| `resolve_color_spec_passthrough_and_palette_override` | `resolve_color` | `Spec` ignores palette/defaults; Named/Indexed honor palette override |
| `named_default_named_color_mapping` | `named_default` | Red→palette[1]; Bright/DimForeground→fg aliases; Background→bg; Cursor→cursor; BrightWhite→palette[15] |
| `cursor_overlay_for_shape_mapping_and_row_clamp` | `cursor_overlay_for` (NEW, from 1191-1219) | Beam/Underline/Block/HollowBlock map; Hidden→None; `row==lines`/`row==-1`→None; `row==lines-1`→Some |
| `match_urls_bare_www_span_columns_exact` | `match_urls_in_row` www (500-503) | `"go www.a.com"` → `start_col==3`,`end_col==12` |

### `src/main.rs`  *(0 tests today)*

**P0** — add a `#[cfg(test)] mod tests`.

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `defaults_from_profile_maps_colors_opacity_and_empty_selection` | `defaults_from_profile` (pure, present) | empty selection hex → None (not parsed); opacity 0.5; ANSI blue → palette[4]; `clear_wipes_scrollback` propagates; fg/bg defaults flow | Phase 3 preview/apply share this fn |
| `apply_pane_defaults_updates_atomic_not_just_struct` | `apply_pane_defaults` (NEW, dedupe `apply_prefs`/`push_pane_defaults`) | applying defaults flips the `clear_wipes_scrollback` AtomicBool the PTY thread reads — not only the struct (guards today's latent bug where `apply_prefs` updates the struct but relies on the preview push for the atomic) | Phase 3 preview extraction |

**P1**

| Test | Target (extraction) | Asserts |
|------|--------------------|---------|
| `should_restore_preview_truth_table` | `should_restore_preview` (NEW, dedupe 498-500 & 560) | `(false,true)==true`; `(true,true)==false`; `(_,false)==false` (locks the two copies to one definition so Cancel/window-X always restores colors) |

### `src/pty_event_loop.rs`  *(0 tests today — Phase 4a path)*

**P0** — add a `#[cfg(test)] mod tests` (use a generic `i32` channel where `Msg` lacks `PartialEq`).

| Test | Target | Asserts | Unblocks |
|------|--------|---------|----------|
| `state_write_queue_load_consume_and_needs_write_cycle` | `State::{ensure_next,goto_next,take_current,set_current,needs_write}` | full enqueue→consume cycle; `needs_write` flips false only when writing None AND list empty; `set_current` round-trips | Phase 4a message cadence |
| `writing_partial_write_accounting_and_empty_source` | `Writing::{new,advance,remaining_bytes,finished}` | `"abc"` → advance(1)→`"bc"` → advance(2)→finished; empty source finished immediately (pins `written >= len`, not `>`) | Write-loop cleanup |
| `peekable_receiver_peek_nonconsuming_then_recv_drains_in_order` | `PeekableReceiver::{new,peek,recv}` | `peek` caches non-consuming; `recv` returns peeked first then drains in order; empty (still-connected) → None/None, no panic | Phase 4a Resize coalescing (no frame loss / premature stop_sync) |

**P1**

| Test | Target | Asserts |
|------|--------|---------|
| `state_empty_input_yields_immediately_finished_writing` | `State::ensure_next` + `Writing::finished` | empty `Cow` → finished with `b""` (documents the busy-loop trap that `Notifier::notify` avoids by dropping empty sends) |

### `src/term_handler.rs`  *(7 tests)*

**P0**

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `wipes_scrollback_true_only_for_primary_all_with_flag` | `wipes_scrollback` (NEW, from `clear_screen` 208-210) | all 8 `(flag,is_all,alt)` combos; true ONLY for `(true,true,false)` (alt-screen guard + flag + ClearMode::All) | Phase 3 extraction / vte re-sync |
| `resolve_query_color_prefers_override_else_default` | `resolve_query_color` (NEW, from `dynamic_color_sequence` 293-295) | `Some(rgb)` wins over index-257 bg default; `None` → `default_color_for_index`; palette fallback for idx 1 | Phase 3 / query-reply rerouting |

**P2**

| Test | Target | Asserts |
|------|--------|---------|
| `dynamic_color_sequence_queues_reply_with_supplied_prefix_and_terminator` | `dynamic_color_sequence` | **(integration, needs `EventLoopSender::for_test` seam)** queues `Msg::Input` == `color_reply("11", bg, "\x07")` (prefix/terminator threaded through) |

### `src/mouse.rs`  *(7 tests — legacy X10 branch uncovered)*

**P0**

| Test | Target | Asserts | Unblocks |
|------|--------|---------|----------|
| `encode_legacy_x10_press_and_release_offsets` | `encode` non-SGR branch (105-114) | Press Left → `[1b,'[','M',32,33,33]`; Release `[3]==35` (release forces code 3, +32) | Phase 4a coordinate/release handling |
| `encode_legacy_x10_clamps_coords_at_255` | `encode` 223-clamp (112-113) | col 300 → byte `255` (clamp 223 +32); row → `40` | Coordinate refactor |

### `src/input.rs`  *(81 tests — classification gaps)*

**P0**

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `classify_keys_alt_screen_passes_only_nav_bindings` | `classify_keys` (NEW, from `process_keys` 31-46) | alt_screen=true: `[Ctrl+Tab, Ctrl+Shift+C]` → `actions==[Copy]`, `consumed==[false,true]` (nav passes through, non-nav still consumed) | Dispatch unification |
| `classify_keys_off_alt_screen_consumes_nav_and_passes_unbound` | `classify_keys` (NEW) | alt_screen=false: `[Ctrl+Tab, A]` → `actions==[FocusNext]`, `consumed==[true,false]` | Dispatch unification |
| `alt_screen_passthrough_matches_only_nav_combos` | `is_alt_screen_passthrough` (pure, present) | (FocusNext,Tab,ctrl)/(PrevTab,PageUp,ctrl)/(NextTab,PageDown,ctrl)==true; ctrl+shift / ctrl+alt / (Copy,C) / plain == false | Dispatch unification (**guards M3 fix region**) |
| `ctrl_letter_byte_maps_az_and_rejects_nonletters` | `ctrl_letter_byte` (NEW, from `encode_raw_key` 157-172) | `'c'→0x03`, `'a'→0x01`, `'z'→0x1a`, `'C'→0x03`, `'6'→None`, `'['→None` | Dispatch / encode reorder |

**P1**

| Test | Target (extraction) | Asserts |
|------|--------------------|---------|
| `should_replay_egui_paste_gated_by_paste_action` | `should_replay_egui_paste` (NEW, from `process_keys` ~82) | `[Paste]→false`, `[Copy]→true`, `[]→true` (don't double-paste when Paste is a bound action) |

### `src/layout.rs`  *(13 tests — ratio/divider/gap gaps)*

**P0**

| Test | Target | Asserts | Unblocks |
|------|--------|---------|----------|
| `set_ratio_min_cells_clamps_band_from_pixels` | `set_ratio_min_cells` | min_frac=0.1 band [0.1,0.9]: 0.01→0.1, 0.99→0.9, 0.5→0.5 | Phase 4a resize debounce |
| `set_ratio_min_cells_degenerate_container_falls_back_to_center` | `set_ratio_min_cells` | tiny container → band collapses to [0.5,0.5]; zero px → else-branch [0.05,0.95] | Phase 4a |
| `set_ratio_clamps_out_of_range` | `set_ratio` | 2.0→0.95, -1.0→0.05 | Phase 4a |
| `set_ratio_walks_path_and_noops_on_leaf` | `set_ratio` | `&[0]` into a Leaf no-ops; `&[1]` sets only the right Split | Phase 4a + Phase 3 hit-test |
| `walk_dividers_paths_and_vertical_center` | `walk_dividers` | 2 handles, paths `[]` and `[0]`, leaf emits none; root center.x ≈ 50 | Phase 3 divider hit-test |
| `walk_rects_vertical_gap_channel` | `walk_rects` | gap=4 → each child width ≈ 48, 4px channel (no existing test uses gap≠0) | Phase 3 pane sizing |
| `remove_leaf_promotes_multileaf_sibling_subtree` | `remove_leaf` | `Split(L1,Split(2,3))`, remove 1 → surviving root still a Split, dir/ratio of inner preserved, order `[2,3]` | Close/reap |

**P1**

| Test | Target | Asserts |
|------|--------|---------|
| `walk_rects_clamps_inverted_rect_on_oversize_gap` | `walk_rects` | width 2, gap 10 → both child widths ≥ 0 (edge clamps, never negative) |
| `layout_template_serde_round_trip_preserves_structure` | `to_template`/`build` | asymmetric 3-leaf tree + ratios round-trips; `build` consumes exactly leaf_count ids |

### `src/renderer.rs`  *(0 tests today)*

**P0** — add a `#[cfg(test)] mod tests`.

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `shelf_place_wrap_reset_and_exact_fit` | `shelf_place` (NEW, from `shelf_alloc` 310-328) | exact fit no wrap; over-row → next shelf; over-atlas → reset to origin; every result `x+w≤size`,`y+h≤size` | Atlas/packing refactor (GPU corruption otherwise invisible) |

**P1**

| Test | Target (extraction) | Asserts |
|------|--------------------|---------|
| `to_rgba_rgb_and_rgba_unpremultiply` | `to_rgba` | Rgb→alpha 255; Rgba un-premultiply `(128,0,0,128)→(255,0,0,128)`; `a==0`→`(0,0,0,0)`; opaque round-trips |
| `to_rgba_short_src_clamps_without_panic` | `to_rgba` clamp | short src → no panic, only valid pixels written, tail zeroed |
| `should_emit_glyph_empty_and_hidden_color_drop` | `should_emit_glyph` (NEW, from `build_glyph_instance` 469-494) | empty→drop; hidden color emoji→drop; hidden non-color text→emit; visible→emit |

### `src/font.rs`  *(0 tests today)*

**P0** — add a `#[cfg(test)] mod tests`.

| Test | Target | Asserts | Unblocks |
|------|--------|---------|----------|
| `font_style_index_matches_all_ordering` | `FontStyle::index`/`ALL` | `ALL[i].index()==i` for all i; explicit `Regular/Bold/Italic/BoldItalic` order; `len()==4` (keeps `keys[]`/glyph_cache aligned) | Font-loading refactor |

**P1**

| Test | Target | Asserts |
|------|--------|---------|
| `font_style_slant_weight_mapping` | `FontStyle::slant_weight` | Regular→(Normal,Normal); Bold→(Normal,Bold); Italic→(Italic,Normal); BoldItalic→(Italic,Bold) |

### `src/config.rs`  *(37 tests — ensure_consistent gaps)*

**P0**

| Test | Target | Asserts | Unblocks |
|------|--------|---------|----------|
| `ensure_consistent_repoints_active_to_first_when_name_missing` | `ensure_consistent` | active_profile "X" with only profile "Y" → repointed to "Y"; `active().name=="Y"` (no silent stale fallthrough) | serde-default / migration ordering |

**P1**

| Test | Target | Asserts |
|------|--------|---------|
| `ensure_consistent_seeds_default_when_profiles_empty` | `ensure_consistent` | empty profiles → one "Default", active repointed, `active()` no panic |
| `config_profile_exit_action_restart_roundtrips` | `ExitAction` serde | `"restart"` ↔ `ExitAction::Restart`; `default()==Close` |

### `src/prefs_ui.rs`  *(0 tests today)*

**P0** — add a `#[cfg(test)] mod tests`.

| Test | Target (extraction) | Asserts | Unblocks |
|------|--------------------|---------|----------|
| `delete_profile_keeps_active_valid_and_clamps_selected` | `delete_profile` (NEW, from delete branch 247-253) | deleting active mid-list repoints active to a survivor; deleting last clamps `selected` to 0 (no dangling active / OOB index) | Phase 3 prefs-closure extraction |

**P1**

| Test | Target (extraction) | Asserts |
|------|--------------------|---------|
| `add_profile_generates_unique_name_and_selects_it` | `add_profile` (NEW) | collision → "New Profile 2", then "New Profile 3"; selects the new one |
| `prefs_open_derives_selected_profile_from_active` | `PrefsState::open` | active "C" of [A,B,C] → `selected_profile==2`; resets transient UI state; draft cloned |

**P2**

| Test | Target | Asserts |
|------|--------|---------|
| `prefs_open_falls_back_to_zero_when_active_missing` | `PrefsState::open` fallback | active "ghost" → `selected_profile==0` (no panic/OOB) |

### `src/keybindings.rs`  *(71 tests — dispatch-coverage gap)*

**P0**

| Test | Target (extractions) | Asserts | Unblocks |
|------|---------------------|---------|----------|
| `every_action_reachable_via_binding_or_menu` | `all_actions` (NEW) + `context_menu_actions` (NEW) | every dispatchable `Action` is reachable via a default linux binding OR a context-menu entry, except an explicit allowlist (`SwitchToTab`, `QuitHotkeyWindow`) | The action-dispatch unification; catches an Action that loses both its binding and menu entry |

---

## 4. Recommended Sequence — P0 tests to write FIRST

Write these **before any refactor touches the untested files**. Each "extraction" lands as
its own zero-behavior-change commit (extract → `cargo test` green) and the unit test goes in
the same commit; only then does the real refactor proceed. Ordered so each block pins the
files its imminent refactor will disturb.

**Block A — Lock the PTY write loop before Phase 4a** (`pty_event_loop.rs`, 0 tests; no extraction, just add a test mod):
1. `state_write_queue_load_consume_and_needs_write_cycle`
2. `writing_partial_write_accounting_and_empty_source`
3. `peekable_receiver_peek_nonconsuming_then_recv_drains_in_order`

**Block B — Lock the pane lifecycle + ratio math before Phase 4a** (`pane.rs` proxy is testable as-is; `layout.rs` pure):
4. `event_proxy_wakeup_coalesces_dirty_and_new_output`
5. `event_proxy_exit_sets_exited_only`
6. `set_ratio_clamps_out_of_range`
7. `set_ratio_min_cells_clamps_band_from_pixels`
8. `set_ratio_min_cells_degenerate_container_falls_back_to_center`
9. `set_ratio_walks_path_and_noops_on_leaf`
10. `walk_dividers_paths_and_vertical_center`
11. `walk_rects_vertical_gap_channel`
12. `remove_leaf_promotes_multileaf_sibling_subtree`

**Block C — Lock pane_ui geometry + pane.rs snapshot/url before Phase 3 `draw_panes` extraction** (extractions: `motion_report`, `apply_cell_attrs`, `url_at_click`):
13. `cell_at_maps_clamps_and_ppp_scales`
14. `cell_side_half_cell_threshold_inclusive_right`
15. `motion_report_suppresses_same_cell_reports_changed`
16. `apply_cell_attrs_inverse_hidden_selection_layering`
17. `resolve_color_fallbacks_when_palette_unset`
18. `scan_url_at_runspan_middle_click_bounded_by_distinct_uris`
19. `scan_url_at_explicit_hyperlink_beats_heuristic`
20. `match_urls_email_span_columns_exact`
21. `scan_url_at_picks_correct_url_among_multiple_and_boundary_space_none`
22. `url_at_click_resolves_wide_spacer_then_scans_hyperlink`
23. `wipes_scrollback_true_only_for_primary_all_with_flag` (`term_handler.rs`)
24. `resolve_query_color_prefers_override_else_default` (`term_handler.rs`)
25. `shelf_place_wrap_reset_and_exact_fit` (`renderer.rs`)
26. `font_style_index_matches_all_ordering` (`font.rs`)

**Block D — Lock tabs.rs + input.rs + main.rs before dispatch unification** (extractions: `select_input_targets`, `pick_new_focus`, `focus_transition`, `wrap_index`, `adjust_active_tab`, `adjust_search_pane`, `reconcile_ids`, `classify_keys`, `ctrl_letter_byte`, `encode_paste`, `focus_event_bytes`, `apply_pane_defaults`):
27. `select_input_targets_broadcast_excludes_read_only`
28. `select_input_targets_focused_only_empty_when_read_only`
29. `pick_new_focus_prefers_successor_then_predecessor_then_first`
30. `focus_transition_guards_noop_and_pairs`
31. `wrap_index_wraps_and_noops`
32. `adjust_active_tab_fixup`
33. `adjust_search_pane_reindexes_and_clears`
34. `reconcile_ids_under_over_and_equal`
35. `classify_keys_alt_screen_passes_only_nav_bindings`
36. `classify_keys_off_alt_screen_consumes_nav_and_passes_unbound`
37. `alt_screen_passthrough_matches_only_nav_combos`
38. `ctrl_letter_byte_maps_az_and_rejects_nonletters`
39. `encode_paste_normalizes_crlf_and_lf_to_cr`
40. `encode_paste_wraps_in_bracketed_markers`
41. `focus_event_bytes_gates_on_focus_in_out`
42. `defaults_from_profile_maps_colors_opacity_and_empty_selection`
43. `apply_pane_defaults_updates_atomic_not_just_struct`

**Block E — Remaining P0 standalones** (independent of the three refactors but cheap insurance):
44. `encode_legacy_x10_press_and_release_offsets` (`mouse.rs`)
45. `encode_legacy_x10_clamps_coords_at_255` (`mouse.rs`)
46. `ensure_consistent_repoints_active_to_first_when_name_missing` (`config.rs`)
47. `delete_profile_keeps_active_valid_and_clamps_selected` (`prefs_ui.rs`)
48. `every_action_reachable_via_binding_or_menu` (`keybindings.rs`)

> **Why this order.** Blocks A/B precede Phase 4a (resize debounce reroutes message cadence
> through the PTY write loop and rewrites ratio writes). Block C precedes Phase 3 (the
> `draw_panes` cut-apart moves every geometry/snapshot/URL helper). Block D precedes the
> dispatch unification (close/switch/move/focus + key classification all funnel through
> `tabs.rs`/`input.rs`/`main.rs`). Several Block-D extractions (`select_input_targets`,
> `pick_new_focus`, `apply_pane_defaults`) are also the **mechanism for fixing** H1/M3, M5,
> and the apply_prefs atomic bug — so the safety test and the fix can land back-to-back.
