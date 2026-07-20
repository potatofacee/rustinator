# Per-profile editable ANSI palette (2026-06-10)

## Problem

Profiles expose only foreground/background/cursor. The 16 ANSI colors (what
`ls`, vim themes, prompts actually use) are a hardcoded xterm-ish table in
`pane.rs` (`const ANSI`, line ~32). ANSI blue (color 4, `#0000ee`) on a black
background is nearly unreadable — the motivating complaint (directory listings).
Users need to edit and save each of the 16 colors per profile.

## How colors flow today

1. `config.rs` `ColorsConfig { foreground, background, cursor }` — hex strings
   per profile.
2. `main.rs` builds `PaneDefaults { fg, bg, cursor, bg_opacity }` from the
   active profile (two sites: `App::new`, `apply_prefs`).
3. `pane.rs` `resolve_color()` maps each cell's color: alacritty's runtime
   palette override (`palette[n]`, set only via OSC 4 escapes) wins; otherwise
   `named_default()` / `indexed_default()` fall back to `PaneDefaults` for
   fg/bg/cursor and the hardcoded `ANSI` table for colors 0-15. Indexed colors
   16-255 are computed (6x6x6 cube + grayscale ramp) — those stay computed.
4. `prefs_ui.rs` draws the Colors section: preset dropdown (presets.rs:
   fg/bg/cursor triples) + three `hex_color_row`s (label + egui
   `color_edit_button_srgb` + hex text field).
5. `apply_prefs` already live-applies: updates `pane.defaults`, marks panes
   dirty. Palette rides the same path — no new apply mechanism needed.

## Design

### config.rs

Extend `ColorsConfig` with alacritty-style named fields (self-documenting for
hand-edited config, matches the reference implementation):

```toml
[profiles.colors.normal]
black = "#000000"
red = "#cd0000"
green = "#00cd00"
yellow = "#cdcd00"
blue = "#5c7cff"     # default brightened vs xterm #0000ee — readable on black
magenta = "#cd00cd"
cyan = "#00cdcd"
white = "#e5e5e5"

[profiles.colors.bright]
black = "#7f7f7f"
red = "#ff0000"
...
```

- New struct `AnsiColors { black, red, green, yellow, blue, magenta, cyan,
  white: String }`, `#[serde(default = ...)]` per field so partial configs
  parse. `ColorsConfig` gains `normal: AnsiColors`, `bright: AnsiColors` with
  defaults matching the current `ANSI` table (decide: keep `#0000ee` as the
  shipped default for compatibility, or brighten normal blue to something
  readable like `#5c7cff`. Recommend brightening blue only — it is the
  motivating bug and alacritty/iTerm defaults are similarly brighter).
- `Profile::palette_rgb() -> [[u8; 3]; 16]` — parse each hex, fall back to the
  old constant per-entry on parse failure (mirror `foreground_rgb` style).
- Round-trip serde test: set a palette color, save, reload, assert.

### pane.rs

- `PaneDefaults` gains `palette: [[u8; 3]; 16]` (stays `Copy`).
- `Default for PaneDefaults` uses the existing `ANSI` table.
- `named_default()`: indices 0-15 read `defaults.palette[idx]` instead of
  `ANSI`.
- `indexed_default()`: gains a `defaults` param; i < 16 reads
  `defaults.palette[i]`; 16-255 unchanged (computed). Update its unit tests
  (pane.rs ~1324) for the new signature.
- `const ANSI` stays as the canonical fallback/default table (config.rs
  defaults reference the same values — keep one source: export `pub(crate)
  const ANSI` from pane.rs and have config.rs use it for defaults/fallbacks).

### main.rs

- Both `PaneDefaults` construction sites add `palette:
  profile.palette_rgb()`.
- `apply_prefs` pane loop already copies `self.pane_defaults` into
  `pane.defaults` and dirties — live apply works with no further change.

### prefs_ui.rs

- Colors section, below the existing three rows: two labeled rows of 8
  `color_edit_button_srgb` swatches ("Normal", "Bright"), each with the ANSI
  name as hover tooltip (black, red, green, yellow, blue, magenta, cyan,
  white). Swatch edits parse/format via the existing `parse_hex`/`format_hex`
  helpers and write the hex string back into the draft profile (same pattern
  as `hex_color_row`, no text field per swatch — 16 text fields too noisy;
  the three existing rows keep theirs).
- "Reset palette" button restoring the 16 defaults.
- Preset dropdown (presets.rs) stays fg/bg/cursor-only and must NOT touch the
  palette when applied (presets gaining full palettes = separate follow-up).

## Scope additions (user, 2026-06-10)

"Each color that might be rendered gets a selectable color, plus the border."

- `focus_border` (currently hardcoded `#7070c0` dim blue, pane_ui.rs:919) and
  `broadcast_border` (`#c05050`, pane_ui.rs:917) — new ColorsConfig hex fields
  with those defaults; pane_ui reads them from the active profile per frame
  (parse_hex on 6 chars, negligible).
- Selection colors: today selection inverts fg/bg (pane.rs:936-938). New
  optional `selection_background` / `selection_foreground` hex fields, empty
  string = keep invert behavior (terminator/iTerm precedent). Plumbed as
  `Option<[u8; 3]>` pair on PaneDefaults; snapshot uses them for selected
  cells (cursor invert untouched). Prefs UI: "Custom selection colors"
  checkbox; enabling seeds #4060c0 / #ffffff, disabling writes "" (= invert).
- Profile helpers: `focus_border_rgb()`, `broadcast_border_rgb()`,
  `selection_bg_rgb() -> Option`, `selection_fg_rgb() -> Option`.

### Out of scope (explicit)

- Color-scheme presets with full palettes (Solarized etc) — follow-up.
- Non-terminal chrome: tab bar, scrollbar, drag-drop zone highlight grays.
- Bold/dim color overrides.
- OSC 4 behavior unchanged (runtime overrides still win over config).

## Implementation order

Two waves, builder agents, review + `cargo check` + `cargo test` after each
wave (rule: zero behavior change for configs that don't set palette keys,
except the deliberate default-blue decision):

1. Wave 1 (parallel, independent):
   - Agent A: config.rs (AnsiColors, ColorsConfig fields, palette_rgb,
     defaults, round-trip test).
   - Agent B: pane.rs (PaneDefaults.palette, named_default/indexed_default,
     test updates, export ANSI).
2. Wave 2 (parallel, depend on wave 1):
   - Agent C: main.rs (two PaneDefaults sites).
   - Agent D: prefs_ui.rs (swatch grid + reset button).
3. Verify: cargo check, cargo test, manual: `ls --color` blue readable after
   editing normal.blue in prefs + Apply; restart picks saved value up.

## Open decision for user

Ship brighter default for normal blue (`#5c7cff`-ish) or keep `#0000ee` and
rely on the user editing it? Recommend brightening — it is the reported pain
and matches what other terminals ship.
