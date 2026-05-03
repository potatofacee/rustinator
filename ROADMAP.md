# Rustinator: Path to Daily Driver

A pragmatic plan to go from the current working-but-fragile state to something
stable enough to be a primary terminal emulator on macOS and Linux.

---

## Current State Assessment

### What works and should not be touched unnecessarily

| Component | File | Lines | Verdict |
|-----------|------|-------|---------|
| Split tree | `layout.rs` | 486 | Solid. Pure data structure, exactly what both iTerm2 and Terminator converge on. |
| PTY/terminal wrapper | `pane.rs` | 1188 | Good. Clean delegation to alacritty_terminal, Frame snapshot pattern, dirty tracking. |
| GPU renderer | `renderer.rs` | 582 | Good. Instanced bg+glyph passes, shelf atlas allocator, subpixel AA via dual-source blending. |
| Config system | `config.rs` | 685 | Good. Profiles, TOML serde, legacy migration, sensible defaults. |
| Shell integration | `shell_integration.rs` | 149 | Good. zsh ZDOTDIR trick + bash BASH_ENV. Sets window title via OSC 0. |
| Tab management | `tabs.rs` | 628 | Adequate. TabManager is the right abstraction. Some bloat from PaneAction dispatch. |
| Mouse protocol | `mouse.rs` | 145 | Good. Covers SGR, normal, and UTF-8 encoding modes. |
| Keyboard encoding | `keyboard.rs` | 377 | Good. Kitty protocol support, legacy CSI encoding. |
| Key event capture | `input.rs` | 113 | Good. Converts winit raw keys to terminal input. |
| Hotkey support | `hotkey.rs` | 103 | Adequate. Global hotkey via global-hotkey crate. |
| Color presets | `presets.rs` | 164 | Fine as-is. |
| Font loading | `font.rs` | 110 | Fine. Thin wrapper around crossfont. |

### What is causing the "fix one thing, break another" problem

**1. The God Object: `App` in `main.rs` (655 lines)**

`App` has 20+ fields and is the hub for everything: tab management, font state,
renderer state, config, prefs UI, keybindings, dialog state, window title, zoom
tracking, fullscreen, hotkey state, cursor blink, pending raw keys. Every feature
change touches this struct because everything lives here.

The symptom: adding a field or changing a method in App ripples into window.rs
(which borrows App mutably), pane_ui.rs (which takes a PaneViewCtx that borrows
half of App's fields), and tabs.rs (which App calls for everything).

**2. Duplicate Event Handling in `window.rs` (2,140 lines)**

Three nearly-identical window implementations: main window, prefs window, and
hotkey window. Each has its own:
- GL surface setup (~30 lines, identical)
- egui state management (~20 lines, identical)
- paint method (~40 lines, near-identical)
- keyboard/mouse/zoom handling (~80 lines, copy-pasted between main and hotkey)

The symptom: a fix to keyboard handling in the main window must be manually
replicated to the hotkey window. Miss one and you get divergent behavior.

**3. Split Action Dispatch**

`PaneAction` is handled in two completely separate codepaths:

- **Path A**: `App::execute_pane_actions()` (line 199) -- called from `logic()`,
  handles keyboard-triggered actions.
- **Path B**: deferred actions in `App::ui()` (line 602) -- handles context menu
  and UI-triggered actions.

These two handlers have different subsets of PaneAction arms, with different
behavior for the same action in some cases. When you add a new action, you have
to add it in both places -- or figure out which one applies. Get it wrong and
the action silently does nothing from one trigger source.

**4. Tangled Borrowing in `pane_ui.rs` (835 lines)**

`PaneViewCtx` borrows half the App by reference to draw panes. This forces a
specific borrow pattern where the caller must carefully destructure App to avoid
overlapping borrows. The function `draw_panes` is 488 lines and handles:
- Layout tree walking
- Divider interaction (drag to resize)
- Mouse click/drag/selection/scroll
- Mouse protocol forwarding
- URL detection and Ctrl+click
- Context menu rendering
- Drag-and-drop between panes
- Scrollbar painting
- Terminal cell painting (via GL callback)

Any change to any of these features risks breaking the others because they share
mutable state within the same giant function.

---

## The Plan

### Guiding Principles

1. **No rewrite.** Every phase produces a working, compilable binary. We refactor
   in place, one concern at a time.
2. **Stabilize before adding.** No new features until the architecture can absorb
   them without cascading breakage.
3. **Test what moves.** When extracting code, the existing behavior is the spec.
   Add targeted tests for any logic that changes shape.
4. **One direction at a time.** Each phase addresses exactly one structural
   problem. Resist the urge to "clean up while we're in there."

---

### Phase 0: Unify Action Dispatch

**Goal:** One codepath for all actions, regardless of whether they come from
keyboard, context menu, or any future source.

**The problem in detail:** `execute_pane_actions` and the deferred-action block
in `ui()` are two partial implementations of the same dispatch table. Some
actions only work from one path.

**Steps:**

1. Move ALL PaneAction handling into `execute_pane_actions`. Remove the match
   block from `ui()` and have it just push actions onto the same Vec that
   keyboard input produces.

2. Eliminate `action_to_pane_action` in `tabs.rs`. Merge `Action` (keybindings)
   and `PaneAction` into a single `Action` enum. There's no reason for two
   identical enums with a 1:1 translation function.

3. Make `draw_panes` return `Vec<Action>` instead of `Vec<PaneAction>`, and
   process them in the same place as keyboard actions.

**Result:** Any action works identically regardless of trigger source. Adding a
new action means adding one enum variant and one match arm.

**Risk:** Low. This is mechanical restructuring with no behavior change.

---

### Phase 1: Break Up the God Object

**Goal:** `App` becomes a thin coordinator, not a state bag.

**Steps:**

1. **Extract `FontState`:**
   ```
   struct FontState {
       ctx: FontContext,        // no more Arc<Mutex<>>
       cell_w: f32,
       cell_h: f32,
       base_size: f32,
       size_override: Option<f32>,
       scale_factor: f32,
   }
   ```
   Move `adjust_font_size`, `reset_font_size`, `apply_font_size`, `reload_font`
   onto this struct. The font context and renderer are only ever accessed on the
   main thread, so `Arc<Mutex<>>` is unnecessary overhead and a source of
   lock-ordering confusion.

2. **Extract `RendererState`:**
   ```
   struct RendererState {
       renderer: Renderer,     // no more Arc<Mutex<>>
       gl: Arc<glow::Context>,
   }
   ```

3. **Extract `InputState`:**
   ```
   struct InputState {
       bindings: BindingTable,
       pending_raw_keys: Vec<RawTermKey>,
       cursor_blink_epoch: Instant,
   }
   ```

4. **Flatten `App` to coordinator:**
   ```
   struct App {
       tabs: TabManager,
       font: FontState,
       renderer: RendererState,
       input: InputState,
       config: Config,
       prefs: PrefsState,
       dialogs: DialogState,
       pane_defaults: PaneDefaults,
       term_config: TermConfig,
       // window-level flags
       current_title: String,
       fullscreen_pending: bool,
       hotkey_changed: bool,
   }
   ```

**Result:** Each sub-struct owns its concern. Methods on FontState don't need to
know about TabManager. Adding a font feature doesn't risk breaking tab logic.

**Risk:** Medium. Lots of mechanical moves, but each step compiles independently.

---

### Phase 2: Collapse Window Duplication

**Goal:** One reusable window/surface type, three instances with different
behavior.

**Steps:**

1. Extract a `GlWindow` struct that encapsulates:
   - Window + GL surface + egui state + painter
   - `paint()`, `resize()`, `swap_buffers()`, `handle_event()`

2. Replace `PrefsWindowState`, `HotkeyWindowState`, and the inline main window
   code with `GlWindow` instances that differ only in their paint closure and
   event filter.

3. Move `encode_raw_key`, `encode_named_key`, and related keyboard encoding out
   of `window.rs` into `input.rs` where they belong. These are input concerns,
   not window concerns.

**Result:** `window.rs` drops from 2,140 to ~800 lines. A keyboard fix applies
to all windows automatically. Adding a new window type (e.g., floating find
bar) is trivial.

**Risk:** Medium. GL context sharing between windows is finicky. Test on both
macOS (CGL) and Linux (GLX/EGL) after this phase.

---

### Phase 3: Untangle pane_ui.rs

**Goal:** The 488-line `draw_panes` function becomes a series of focused,
testable functions.

**Steps:**

1. Extract `handle_pane_mouse()` -- all mouse click/drag/selection/scroll logic
   for a single pane. Takes the pane, the pointer state, and returns actions.

2. Extract `handle_pane_scroll()` -- scroll wheel handling, separate from
   selection.

3. Extract `paint_scrollbar()` -- the scrollbar is currently inline in
   `paint_pane`. Pull it out.

4. Move the context menu definition into its own function. The menu is currently
   interleaved with the main interaction loop.

5. Extract `handle_drag_drop()` -- the drag-source/drop-zone logic.

**Result:** Each interaction concern is a function you can read in isolation.
Changing URL click handling doesn't risk breaking selection behavior because
they're in different functions with explicit inputs and outputs.

**Risk:** Low. These are pure extractions with no behavior change.

---

### Phase 4: Daily-Driver Hardening

This is where we stop refactoring structure and fix the things that make a
terminal annoying to use as your daily driver.

**4a. Resize correctness**

The current resize path: `pane_ui` computes new cols/lines from the rect,
calls `pane.resize()`, which locks the terminal, resizes the grid, and sends
SIGWINCH via the PTY channel. The problem: this happens every frame during a
window drag, producing a storm of resize events.

Fix: debounce resize. Only send SIGWINCH after the size has been stable for
~50ms. Keep a `pending_resize: Option<(usize, usize, Instant)>` on the pane.

**4b. Process lifecycle edge cases**

Current `reap_exited` scans all panes every frame. This is correct but the
ExitAction::Restart path creates a new pane with a fresh ID, which means any
external reference to the old pane (search dialog, zoom state) becomes stale.

Fix: when restarting, reuse the same PaneId. Create a `Pane::respawn()` method
that keeps the ID and UI state but replaces the PTY and terminal.

**4c. Config application without breaking panes**

`apply_prefs` currently sets `pane.cols = 0; pane.lines = 0` to force a
resize on font change. This works but causes a visible flash as every pane
briefly becomes 0x0.

Fix: compute the new dimensions immediately from the current rect and apply
them in one step. Store the last-known rect per pane.

**4d. Reliable paste on macOS**

The clipboard path goes through `arboard` which has known issues with macOS
pasteboard access from non-main threads. Since paste is always triggered from
the main thread, this should work, but verify with large pastes (>64KB) and
binary content.

**4e. Focus tracking**

When switching tabs, the old tab's panes should receive focus-out events and
the new tab's focused pane should receive focus-in. Currently `notify_focus`
only fires on OS-level window focus changes.

---

### Phase 5: Missing Features for Daily Use

Only after phases 0-4 are complete. These are the features you'll miss within
the first week of daily driving.

| Feature | Effort | Notes |
|---------|--------|-------|
| Inactive pane dimming | Small | Already have `focused` flag; apply alpha overlay |
| Audible/visual bell | Small | alacritty_terminal sends Bell event; play sound or flash |
| Custom shell command per profile | Small | Add to Profile, pass to Pane::spawn |
| Working OSC 52 (clipboard set from shell) | Small | alacritty_terminal parses it; wire to arboard |
| Clickable file paths (not just URLs) | Medium | Extend scan_urls with regex for file:line patterns |
| Selection text highlight rendering | Small | Already works via selection in snapshot, but verify edge cases |
| Per-pane scrollbar position config | Small | Config field, already have scrollbar_visible |
| Tab reorder by drag | Medium | Already have tab bar; add drag interaction |
| Session restore on restart | Medium | Serialize layout + cwds to config on exit, restore on launch |
| Sixel/Kitty image protocol | Large | Would need a texture-per-image approach in the renderer |

---

## Recommended Execution Order

```
Phase 0 (action dispatch)     ~2 sessions    low risk, high payoff
Phase 1 (break up App)        ~3 sessions    medium risk, unblocks everything
Phase 2 (window dedup)        ~2 sessions    medium risk, stops keyboard bugs
Phase 3 (untangle pane_ui)    ~2 sessions    low risk, stops mouse bugs
Phase 4 (hardening)           ~3 sessions    targeted fixes, daily-driver quality
Phase 5 (features)            ongoing        as needed
```

After Phase 4, you have a terminal you can daily-drive. Phase 5 items can be
added incrementally without the "fix one, break another" problem because the
architecture supports it.

---

## What NOT To Do

1. **Don't switch away from egui/glow.** The rendering stack works. egui gives
   you free UI widgets (dialogs, prefs, context menus) that would take months
   to reimplement in raw OpenGL. The terminal cell rendering already bypasses
   egui via PaintCallback.

2. **Don't replace alacritty_terminal.** It handles VT100 parsing, the grid,
   selection, search, scrollback, the Kitty keyboard protocol. Writing your
   own would be a multi-month detour.

3. **Don't add a plugin system yet.** Terminator's plugin system is simple but
   it works because the core is stable. Stabilize first.

4. **Don't split into workspace crates prematurely.** Module boundaries (the
   Phase 1 extractions) give you the same separation without the build system
   overhead. When you have 30+ files, reconsider.

5. **Don't chase feature parity with Terminator.** You need a daily driver,
   not a clone. Implement features when you personally miss them.
