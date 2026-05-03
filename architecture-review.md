# Architecture Review

Audit of Rustinator's codebase, identifying structural and correctness issues
to fix before adding more features.

## Critical (fix before more feature work)

### 1. Key input pipeline has three independent translation paths

- `translate_key_event()` (window.rs:1153) -- only fires when Ctrl or Alt held,
  converts to egui events for keybinding matching
- `encode_raw_key()` (window.rs:1181) -- captures raw terminal bytes from winit,
  runs as a separate path
- `encode_named_key()` (window.rs:1269) -- encodes functional keys but only takes
  shift/alt/ctrl parameters, completely ignoring mac_cmd/super

These are stitched together in `forward_input()` where raw_keys are matched back
to egui events by key+mods equality. This is the root cause of:
- Home/End broken on macOS (CSI vs SS3 encoding)
- Cmd+Arrow word-skip not working
- Any future modifier-dependent key behavior being fragile

**Fix:** Single path from winit event to terminal. Keybindings intercept first;
anything not consumed goes to the PTY encoder. One encoder, one modifier
representation.

### 2. main.rs god object (2661 lines, 37+ fields)

App struct owns everything: tabs, panes, layout, input, rendering, config,
context menus, scrollbar state, font loading.

**Fix:** Decompose into TabManager, PrefsState, DialogState, input functions,
pane_ui rendering. [IN PROGRESS -- phases 1-4 complete, phase 5 running]

### 3. Hotkey window duplicates entire App

`HotkeyWindowState` in window.rs creates a second full `App` instance with its
own independent key handling pipeline, config, font, renderer -- everything.

**Fix:** Share state between main window and hotkey window, or use a single App
that manages both window surfaces.

## Significant

### 4. winit_mods_to_egui modifier mismatch

Our `winit_mods_to_egui()` (window.rs:1376) sets:
```
command: ctrl || (super_ && macos)
```

But egui_winit's built-in conversion sets:
```
command: super_   (on macOS)
```

When both code paths produce egui events for the same keypress, the modifier
fields disagree. This causes silent key drops when the keybinding lookup
doesn't find a match.

**Fix:** Use one consistent modifier translation, or intercept before egui_winit
processes the event.

### 5. Home/End use wrong escape sequence on macOS

`encode_named_key()` sends CSI sequences (`\x1b[H` / `\x1b[F`) for Home/End.
macOS xterm-256color terminfo expects SS3 (`\x1bOH` / `\x1bOF`).

**Fix:** Check terminal mode (application cursor mode / DECCKM) and emit SS3
when appropriate. This should be part of the key pipeline unification (#1).

### 6. Dual action dispatch

Keybinding actions and context menu actions go through separate codepaths that
partially overlap. Both produce `PaneAction` values but wire them differently.

**Fix:** Unify into a single action dispatch -- both sources produce
`Vec<PaneAction>`, one executor processes them.

### 7. No application cursor mode (DECCKM) support in encode_named_key

Arrow keys should use SS3 (`\x1bOA`) instead of CSI (`\x1b[A`) when the
terminal has application cursor mode active. `encode_named_key` has no access
to terminal mode flags.

**Fix:** Pass `TermMode` into the key encoder so it can check DECCKM. Part of
key pipeline unification (#1).

### 8. Kitty keyboard protocol double-counts Super modifier

In keyboard.rs, `encode_mods()` checks both `mac_cmd` and `command`:
```rust
if mods.mac_cmd || mods.command { m |= 0b1000; }
```

On macOS, egui sets both `mac_cmd: true` AND `command: true` for the Super key.
The bitwise OR means it's not double-applied here, but the logic is fragile and
unclear about intent.

**Fix:** Pick one source of truth for the Super modifier.

### 9. Shell integration writes scripts to /tmp

`shell_integration.rs` writes shell integration scripts to `/tmp/`. This is
world-readable, raceable, and may conflict with other users.

**Fix:** Use a per-user temp directory or XDG runtime dir.

### 10. No resize throttling

Rapid window resizes flood the PTY with SIGWINCH signals and resize operations.
No debouncing or throttling.

**Fix:** Debounce resize events (e.g., 50ms delay before forwarding to PTY).

## Minor

### 11. Missing DECSET responses
Some DECSET queries that terminals expect responses to are silently ignored.

### 12. Bracketed paste edge cases
No handling for nested bracket sequences in pasted content.

### 13. Font fallback chain incomplete
Missing fallback for emoji and CJK characters.

### 14. Scrollback search unimplemented
Search UI exists (dialogs.rs) but the actual regex/text search through scrollback
is minimal.

### 15. Title-setting escape sequences incomplete
OSC 0/1/2 partially handled; OSC 7 (CWD), OSC 8 (hyperlinks) not implemented.

### 16. Bell handling
No visual or audible bell implementation.

### 17. Alternate screen buffer edge cases
Some applications that switch to alternate screen may not restore correctly.

### 18. Mouse protocol gaps
SGR mouse mode supported, but some edge cases in button encoding and
coordinate overflow not handled.
