# Feature Work Session — Plan (2026-06-30)

Implementation plan for the selected feature clusters: **panes/splits, tabs (+ reorder),
profiles, layouts, grouping, broadcasting**. Bell explicitly excluded (no audible ding).

STATUS (2026-06-30): WF-A through WF-D are DONE — all compiling, 414 unit tests
passing (282 → 414), but NOT yet exercised in the GUI. WF-E (multi-window) is
DEFERRED to v2 per user decision. Per-WF status and test counts are tagged on each
heading below; known follow-ups are listed at the end (section 7).

Execution was a *sequence* of workflows (WF-A … WF-E), one phase at a time, with a
human check between each.

---

## 0. Starting state (as of this doc)

- Architecture refactor: ROADMAP Phases 0–2 done, 3 mostly. `GlWindow` already extracted
  (`gl_window.rs`) — the seam multi-window builds on.
- Correctness: all 22 review bugs fixed; test suite 282 → 342, zero warnings.
- `select_input_targets` (`tabs.rs`) is the single input-routing gate — broadcast scopes
  will extend it, not bypass it.
- Companion docs: `ROADMAP.md` (daily-driver plan), `gap-analysis.md` (parity, 100 missing),
  `test-safety-review.md` (bug/test backlog, burned down).

---

## 1. Why this isn't a flat fan-out

Several chosen features share **new foundational infra**. Building leaves before the infra
they sit on would mean rework and merge conflicts. Four blockers:

| Infra | What it is | Unblocks |
|-------|-----------|----------|
| **Group model** | group id/name on each pane; rework `tab.broadcast: bool` → a scope enum; extend `select_input_targets` | named groups, group/ungroup, 3-way broadcast scopes, split-to-group, group label/icon |
| **Profile-on-spawn** | `Pane` carries `profile_id`; `spawn` resolves command/shell/colors from the profile; profile registry lookup | per-pane profile switch, next/prev, inherit-on-split, custom command, per-pane profile in saved layouts |
| **Tab-bar subsystem** | own the tab-bar render + hit-test in one place; support variable position/scroll/reorder | all 8 tab features + drag-reorder |
| **In-process multi-window** | N top-level windows, each its own `GlWindow` + `TabManager`; event routing by window id; split `App` → per-window state | detach-to-window, multi-window layouts |

Multi-window is the **long pole** (GL context sharing, per-platform event loop). Everything
else is days; this is weeks. Recommendation: do it LAST, or defer to v2 and keep
single-window/multi-tab.

---

## 2. Feature → file / dependency map

Tier 1 = no infra dependency. Tier 2 = needs an infra block first.

### Panes / Splits (all Tier 1)
| Feature | Files | Effort | Notes |
|---------|-------|--------|-------|
| Scaled zoom (font scales to fill) | `tabs.rs` (zoom state) / `pane_ui.rs` (paint) / `font.rs` | M | `Action::ScaledZoom` stub exists (Gap #15) |
| Recursive balance (equalize ratios) | `layout.rs` + trigger in `pane_ui.rs` (Super+dbl-click) | S | Gap #45 `rebalance_recursive` |
| Cell-aware keyboard resize | `tabs.rs::resize_split` + `layout.rs` ratio math | S | `set_ratio` min-cells clamp already tested this session |
| DnD drop-zone highlight overlay | `pane_ui.rs` | S | `drag_source_pane` already tracked; add overlay paint |
| External Ctrl+right-drag DnD | `window.rs` (winit drop events) + `pane_ui.rs` | M | accept text/URI drops into a pane |

### Tabs (Tier 2 on Tab-bar subsystem, except detach)
| Feature | Files | Effort | Notes |
|---------|-------|--------|-------|
| Drag-reorder ("re-organize tabs") | tab-bar UI + `tabs.rs` move | M | high-want |
| Right / middle-click on tab | tab-bar UI + context menu | S | middle = close (Gap exists), right = menu |
| Tab position (top/bottom/left/right/hidden) | `config.rs` + tab-bar layout | M | Gap #28 |
| Scrollable tab bar | tab-bar UI | M | overflow handling |
| Homogeneous tab bar | tab-bar UI | S | equal-width tabs |
| Close-button config | `config.rs` + tab-bar UI | S | |
| New-tab-after-current | `tabs.rs` insert index + `config.rs` | S | |
| Inline label edit (dbl-click) | tab-bar UI + `dialogs.rs` | M | |
| Detach tab → new window | **multi-window infra** | L | Tier 3 |

> The tab-bar render currently lives inside the main UI path — WF-A pins its exact location
> and the WF-C "Tab-bar subsystem" extraction gives all of the above one clean owner.

### Profiles (Tier 2 on Profile-on-spawn)
| Feature | Files | Effort | Notes |
|---------|-------|--------|-------|
| Custom command per profile | `config.rs` (`Profile.command`) + `pane.rs::spawn` | M | Gap #22; pairs with login-shell-per-profile |
| Per-pane profile switch | `pane.rs` (`profile_id`) + `tabs.rs`/`main.rs` apply | M | |
| Next/prev profile keybinds | `keybindings.rs` + `main.rs` dispatch | S | trivial once switch exists |
| Inherit-on-split | `tabs.rs::split*` passes parent profile | S | |

### Layout (Tier 2)
| Feature | Files | Effort | Notes |
|---------|-------|--------|-------|
| Per-pane cwd/cmd/profile in LayoutTemplate | `layout.rs` (serialize) + `tabs.rs::restore_layout` | M | Gap #4; needs Profile-on-spawn + cwd (have cwd) |
| Startup layout from config | `config.rs` + `main.rs` boot | S | |
| Layout launcher window (Alt+L) | `dialogs.rs` / `window.rs` | M | picker UI |
| Multi-window layouts | **multi-window infra** | L | Tier 3 |

### Grouping / Broadcast (Tier 2 on Group model)
| Feature | Files | Effort | Notes |
|---------|-------|--------|-------|
| Named groups | group store (`tabs.rs`/`pane.rs` or new `groups.rs`) | M | foundational; part of INFRA-Group |
| Group / ungroup (all / tab / win) | `Action` + `tabs.rs` | M | Gap #13 |
| 3-way broadcast scopes (all/group/off) | `tabs.rs` (scope enum) + `input.rs`/`select_input_targets` | M | reworks the per-tab bool |
| Split-to-group (inherit parent group) | `tabs.rs::split*` | S | |
| Insert terminal number/name | `pane_ui.rs` paint + `input.rs` | S | Super+N (Gap #27) |
| Group label + broadcast icon in titlebar | `pane_ui.rs::paint_pane` title bar | S | |

---

## 3. Workflow sequence

Each implementation workflow reuses the **proven pattern** from this session:
one owner per file (zero collisions), implementers forbidden from running cargo,
a single `cargo check` + `cargo test` verify pass after the barrier, tests land
*with* the feature. Worktree isolation only where two features must touch the same file.

### WF-A · Design (run first) — read/design only, safe — DONE (2026-06-30)
Parallel designers, one per infra block (Group model, Profile-on-spawn, Tab-bar subsystem,
Multi-window), each emitting a concrete design: data structures, exact touched files/symbols,
serde/migration impact, public API, test seams. A reconciler resolves overlaps —
specifically: where the group id lives, how broadcast-scope composes with
`select_input_targets`, profile resolution order on spawn/respawn, and where the tab-bar
render currently lives. **Output:** design docs saved to repo; this plan's file map gets
corrected to ground truth.

### WF-B · Tier-1 leaves — fast visible wins — DONE (2026-06-30, 355 tests)
5 features, file-partitioned parallel + verify: scaled zoom, recursive balance,
cell-aware resize, DnD overlay, external DnD. Independent of all infra. Good first
real-code workflow; proves the pipeline before the heavy infra.

### WF-C · Build infra (worktree-isolated) — DONE (2026-06-30): C1 Group model (368), C2 Profile-on-spawn (385), C3 Tab-bar subsystem (399)
Group model + Profile-on-spawn (largely disjoint files → parallel) and the Tab-bar
subsystem extraction. Each lands with tests + verify. This is the gating phase —
everything in WF-D waits on it.

### WF-D · Tier-2 features (fan out per infra) — DONE (2026-06-30, 414 tests)
With infra files stable, partition cleanly by owner:
- Profiles trio + custom command (on Profile-on-spawn)
- Grouping + 3-way broadcast + split-to-group + group label (on Group model)
- Layout persistence + startup layout + layout launcher (on Profile-on-spawn + serialize)
- The 8 tab features + drag-reorder (on Tab-bar subsystem)

### WF-E · Multi-window (own deep effort) — optional / last — DEFERRED to v2 (user decision)
Detach-to-window + multi-window layouts. Highest risk (GL context sharing, per-platform
event loop). Recommend deferring; if cut for v1, T1 (detach) and L2 (multi-window layouts)
stay open and everything else still ships.

---

## 4. Recommended order

```
WF-A (design)         -> gates everything; cheap, safe
WF-B (Tier-1 leaves)  -> fast wins, independent, runs anytime after A (or even before)
WF-C (infra)          -> Group model, Profile-on-spawn, Tab-bar
WF-D (Tier-2 features)-> the bulk; per-infra fan-out
WF-E (multi-window)   -> last, or defer to v2
```

Alternative: run **WF-B before WF-A** if you want visible movement immediately — the
Tier-1 leaves don't depend on the design pass.

---

## 5. Open decisions

- Multi-window (WF-E): build now, or defer and ship single-window? (Recommend defer.)
- Group identity: per-pane group id vs per-tab — WF-A picks; affects broadcast-scope shape.
- Tab position left/right (vertical tab bar) is a bigger UI lift than top/bottom — include
  in first pass or top/bottom only?
- Custom command per profile: also wire login-shell-per-profile (Gap #24) at the same time
  (same spawn code path)?

---

## 6. Out of scope (this session)

Bell, plugins, D-Bus/remotinator, i18n, flatpak, sixel, background image, search toggles,
CLI args — tracked in `gap-analysis.md`, not part of this plan.

Aside (resolved, not a task): word-delete on mac = **Option+Backspace** already works
(`encode_named_key` emits ESC+DEL). Cmd+Backspace is intentionally inert.

---

## 7. Known follow-ups (2026-06-30)

Noted, not blocking — none of these are marked done:

- **Recursive-balance trigger is macOS-only.** The Super+double-click trigger reads
  egui's native modifiers, which don't carry Super on Linux, so the gesture only
  fires on macOS. (`layout::rebalance_recursive` itself is platform-neutral.)
- **`App::ui()` is ~115 lines** and could be decomposed; left as-is for now.
- **"New group" suggestion name is prefilled by the opener** — the dialog shows a
  prefilled suggestion (e.g. the next "N" name) rather than computing it internally.

Plus the standing v2 deferral: detach-tab-to-window + multi-window + multi-window
layouts (WF-E), design in `20260630-design-multi-window.md`.
