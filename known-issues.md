# Known Issues

Tracked, intentional limitations — not bugs to be surprised by. Each entry: what
the user sees, why it happens, the lift to fix, and the recommendation. Severity
reflects impact on common terminal/TUI output.

Last updated: 2026-06-29.

---

## Rendering

### Emoji ZWJ sequences are not shaped (med)

**Symptom:** Multi-codepoint emoji — family `👨‍👩‍👧`, country flags, skin-tone and
gender variants — render as the base emoji plus stray/dropped joiner glyphs, not
as the single intended glyph. Single-codepoint emoji and the base of a sequence
render fine and in true color.

**Cause:** The pipeline rasterizes one codepoint at a time. A ZWJ sequence is
`base + ZWJ + base + ...`; the joiners and trailing bases are carried as
zero-width overlay chars (so the data is no longer *dropped*, fixing the earlier
silent-loss bug), but they are stacked at the cell origin rather than substituted
into one ligature glyph. Correct rendering requires a shaping engine
(GSUB/GPOS) to turn the codepoint run into one positioned glyph ID.

**Lift to fix (large, multi-day):**
1. Add a shaper (`rustybuzz` / HarfBuzz) to map a grapheme's codepoint run to
   glyph IDs.
2. Rekey the glyph pipeline from `char` to glyph-ID: `font.rs::rasterize`,
   `renderer.rs` glyph cache (`HashMap<(char, FontStyle)>`), and the atlas key
   all assume `char`. crossfont 0.9 rasterizes by char only and exposes no
   rasterize-by-glyph-ID entrypoint, so this means forking crossfont or moving
   emoji to a glyph-ID-capable face lib (`swash` / `ab_glyph` / freetype).
3. Reassemble graphemes (base + zerowidth) at snapshot time and map the shaped
   result back onto the grid cell. Terminals are cell-based, shapers are
   run-based — the impedance mismatch is the real cost, not the shaper itself.

**Recommendation:** Skip. Touches the rendering core; no grid corruption today,
emoji just look wrong. If even the partial overlay draw is distracting, a cheap
interim is to draw a single placeholder glyph for unshaped clusters instead of
stacking overlays. The same machinery would also enable arbitrary complex-script
shaping (Arabic, Indic) if that ever matters.

### Curly / dotted / dashed underlines are rect approximations (low)

**Symptom:** Undercurl (squiggly, used by LSP/compiler error output) is drawn as
a faceted triangle-wave of small rectangles, not a smooth anti-aliased sine.
Dotted/dashed are stipple rects with fixed period. May look chunky or alias at
small font sizes.

**Cause:** All underline styles reuse the existing background-quad (`BgInstance`)
path — no dedicated decoration shader. Style differences are encoded as rect
size/spacing/height.

**Lift to fix (small, ~half day):** A dedicated decoration fragment shader that
computes the curve with proper AA (sine into a distance function), passed the
underline style + cell rect. One small new GL program, isolated to `renderer.rs`,
low risk.

**Recommendation:** Do it only if undercurl looks bad in practice — verify in the
real app first. Single/double underline and strikeout already look correct.

### DIM (SGR 2) does not dim color emoji (low)

**Symptom:** A faint/dim color emoji renders at full brightness.

**Cause:** DIM is applied in the snapshot by scaling the cell **foreground** color
by ~0.66. Color emoji ignore fg entirely — they draw their own bitmap RGBA via
the shader's color-glyph branch — so the dim factor never reaches them.

**Lift to fix (small, ~half day):** Thread a brightness multiplier into
`GlyphInstance` (same pattern as the `color_glyph` flag), then multiply
`texel.rgb` by it in the emoji branch of `GLYPH_FRAG`. New per-instance vertex
attribute + VAO wiring + one shader line.

**Recommendation:** Skip. Dim color emoji is near-nonexistent in real output.

---

## Input / protocol

### Kitty keyboard protocol: no key-release events (low)

**Symptom:** A TUI that requests key *release* reporting (kitty
`REPORT_EVENT_TYPES`) does not receive release events; only press is encoded.

**Cause:** The kitty CSI-u encoder emits on press only (conceded in a code comment
in `keyboard.rs`). Press-side disambiguation and modifier reporting work.

**Lift to fix (medium):** Track and encode key-up transitions with the event-type
suffix when the mode is active. Needs key-state tracking the input layer does not
currently keep.

**Recommendation:** Skip until a target app actually needs it — rare.

---

## Notes

- The two big correctness fixes that prompted this audit are **resolved**, not
  listed here: terminal query replies (DA/DSR/OSC color — the glow hang) and
  UTF-8 locale injection (multibyte mojibake).
- OSC 8 hyperlinks (formerly listed here) are now **resolved** (2026-06-29):
  `CellSnapshot.hyperlink` carries the explicit target and `scan_url_at` prefers
  it over the heuristic match (pane.rs:579-607, 1298). Heuristic URL scheme
  detection (mailto/file/ssh/ftp/email/www) also landed.
- Wide-char (CJK) handling — width, click hit-testing, background, and underline/
  strikeout spanning — is **complete**, not a known issue.
- All rendering fixes are GPU-path and cannot be unit-tested; runtime verification
  is tracked in `tasks.md`.
