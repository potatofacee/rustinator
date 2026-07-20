# Agentic Task 3 — Emoji ZWJ / multi-codepoint grapheme shaping

**Difficulty: HARD (multi-day; expect to land it in stages)**

## Goal

Multi-codepoint emoji — ZWJ sequences like family `👨‍👩‍👧`, country flags,
skin-tone and gender variants — currently render as the base emoji plus
stray/dropped joiner glyphs, because the pipeline rasterizes one `char` at a time.
A correct fix reassembles each grapheme cluster (base + zero-width joiners /
variation selectors / modifiers) and shapes the codepoint run into a single
positioned glyph, then rekeys the glyph cache/atlas from `char` to glyph-ID. This
is documented as a large, multi-day item in `known-issues.md` ("Emoji ZWJ
sequences are not shaped"). Read that entry first — it is the spec.

## Files + functions to touch

### `src/font.rs`

- `FontStyle` enum — **line 7** (with `index()` at L32, `ALL` at L15). The cache
  key uses `FontStyle`; you will extend the key, not this enum.
- `rasterize(&mut self, c: char, style: FontStyle)` — **line 99**. Today it calls
  `self.rasterizer.get_glyph(GlyphKey { character: c, ... })` (crossfont 0.9
  rasterizes by `char` only — it exposes **no** rasterize-by-glyph-ID
  entrypoint). To shape, you must add a shaper (`rustybuzz`) to map a cluster's
  codepoint run to glyph IDs, and a way to rasterize **by glyph ID**. Since
  crossfont cannot do that, either (a) fork/extend the rasterizer path, or
  (b) move emoji clusters onto a glyph-ID-capable face library (`swash` /
  `ab_glyph` / freetype) loaded from the same font file. Pick one and add a
  `rasterize_cluster` / `rasterize_glyph(glyph_id, style)` entrypoint here.

### `src/renderer.rs`

- Glyph cache — `glyph_cache: HashMap<(char, FontStyle), AtlasEntry>` at
  **line 72** (init L181, `clear()` L339). Rekey from `(char, FontStyle)` to a key
  that can express a shaped glyph, e.g. `(GlyphId, FontStyle)` or an enum
  `{ Char(char) | Glyph(u32) }` + `FontStyle`. Update all insert/get sites.
- `get_or_insert_glyph(&mut self, c: char, style, font)` — **line 199**. Rework so
  a shaped cluster looks up by glyph ID (rasterizing + uploading to the atlas on
  miss) while plain single-char cells keep the existing fast path. Preserve the
  empty-glyph / zero-size short-circuits (L213-235) and the `color` flag for
  color-emoji bitmaps.

### `src/pane.rs`

- Grapheme reassembly at snapshot time. `CellSnapshot` (**line 334**) already
  carries `c: char` plus `zerowidth: Vec<char>` (the joiners/VS/modifiers
  alacritty parked on the cell, populated via `extract_zerowidth` at L382 in the
  snapshot loop ~L1184). Reassemble `[c] + zerowidth` into the cluster's codepoint
  run and feed it to the shaper so the renderer can draw one glyph at the cell
  origin instead of stacking overlays. Terminals are cell-based and shapers are
  run-based — mapping the shaped result back onto the single grid cell is the real
  cost here; handle the common case (one visible cluster per WIDE cell) first.

## Acceptance criteria (behavioral — verification here is VISUAL, be honest about it)

"Done" is NOT "added a shaper dep" or "added a `rasterize_cluster` entrypoint
that nothing calls." Done is the observable render: a ZWJ sequence draws as ONE
shaped glyph at the cell origin with no stray joiner/base overlays, while plain
text and single-codepoint emoji are unchanged. The goal behavior lives at the
renderer's draw path, so a new shaper/rasterizer/cache-key that the snapshot ->
`get_or_insert_glyph` -> atlas path does not actually route clusters through is
unfinished, even if it compiles.

**Honest note on verification: there is no clean self-verify for this task.**
Shaped-glyph correctness needs a GPU + font context, so it CANNOT be proven by a
unit test. The required acceptance gate is therefore:

1. Implement the reassembly + shaped lookup + atlas keying for real, on the live
   draw path (not behind an unused entrypoint).
2. A **human visually confirms** the family/flag emoji render as single glyphs
   via the printf check below.

Do NOT fake a green test to claim done — do not write a test that asserts on a
fabricated/hardcoded glyph, stubs the shaper, or asserts `true`. The only
legitimate unit test here is the narrow, GPU-free one called for under "How to
verify": that `c + zerowidth` reassembles into the expected codepoint run and
derives a distinct, stable cache key. That test does NOT prove the feature; it
only guards reassembly. In your final report, document your approach and state
your exact stopping point: what is genuinely shaped-and-rendered vs. reassembled
vs. placeholdered, and what remains for the human visual pass.

- A ZWJ sequence (e.g. `👨‍👩‍👧`) reassembled from `c + zerowidth` is shaped to a
  single glyph ID and rendered as one glyph at the cell origin — no stray joiner
  or trailing-base overlays stacked at the origin.
- Single-codepoint emoji and plain text render exactly as before (no regression in
  metrics, color, or position).
- The glyph cache/atlas is keyed so two different clusters that shape to different
  glyph IDs do not collide, and identical clusters hit the cache.
- `cargo build` succeeds and the full test suite stays green.

## How to verify

```
cargo build
cargo test --lib
```

Add at least one unit test near the existing cluster test
`snapshot_cell_carries_zerowidth_cluster` (`src/pane.rs:1404`) that asserts a ZWJ
input reassembles into the expected multi-codepoint run (the shaping/atlas step
itself needs a GPU/font context, so unit-test the reassembly + key derivation;
shaped-glyph correctness is verified visually).

Visual check (required for this task since rendering can't be fully unit-tested):
run the app and print a family emoji and a flag:

```
printf '\U0001F468‍\U0001F469‍\U0001F467  \U0001F1FA\U0001F1F8\n'
```

Confirm each renders as one glyph, not base + visible joiners.

## Constraints

- Land it in stages and keep `cargo build` + `cargo test --lib` green at each
  commit. Do not leave the tree non-compiling.
- Do not regress single-char or plain-text rendering — the `char` fast path must
  stay fast and allocation-free for the common single-scalar cell.
- Adding `rustybuzz` (and possibly `swash`/`ab_glyph`) to `Cargo.toml` is expected
  and allowed for this task; justify the choice in the commit message. Keep new
  deps minimal.
- If full shaping cannot be completed, the documented cheap interim (draw one
  placeholder glyph per unshaped cluster instead of stacking overlays — see
  `known-issues.md`) is an acceptable partial landing, but state clearly in your
  final report what is shaped vs. placeholdered.
