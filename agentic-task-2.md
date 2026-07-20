# Agentic Task 2 — Make OSC-8 explicit hyperlinks clickable (gap #42)

**Difficulty: MEDIUM**

## Goal

Terminals support OSC-8 escape sequences that attach an explicit target URI to a
run of cells (e.g. a build tool prints "see docs" where the visible text is
"docs" but the link points at `https://...`). The alacritty `Term` already parses
and stores these — `set_hyperlink` is wired in `src/term_handler.rs:332`, and
each cell exposes `cell.hyperlink() -> Option<Hyperlink>` (verified in
`alacritty_terminal-0.26.0/src/term/cell.rs:219`; `Hyperlink::uri()` returns the
target and `Hyperlink::id()` returns the run identifier). But nothing carries
that data into our snapshot, so OSC-8 links are not hoverable or clickable. Add
the explicit target to the cell snapshot, reconstruct each contiguous link run on
hit-test, and prefer the explicit target over the heuristic `http://`-style
matcher when a cell carries one.

## Files + functions to touch

### `src/pane.rs`

- `struct UrlMatch` — **line 309**. Add a field so callers can tell an explicit
  OSC-8 match from a heuristic one. The stub test (below) expects a boolean
  `is_hyperlink` field. Add `pub is_hyperlink: bool` and set it on every
  `UrlMatch` construction site (currently one, in `match_urls_in_row` ~L461 —
  set it `false` there).
- `struct CellSnapshot` — **line 334**. Add a field carrying the cell's explicit
  link target, e.g. `pub hyperlink: Option<String>` (store `uri().to_string()`;
  optionally also keep the link `id()` if you group runs by id). Empty/`None` for
  the common case so no allocation when absent.
- Snapshot fill loop — the `cells.push(CellSnapshot { ... })` at **line 1170**
  (inside `fn snapshot` starting **line 1044**). Populate the new field from
  `indexed.cell.hyperlink()`.
- `scan_url_at(cells, row, col)` — **line 496**. This is the hit-test used on
  hover/Ctrl-click. Change it to prefer an explicit hyperlink: if the cell at
  `(row, col)` carries `Some(hyperlink)`, build a `UrlMatch` spanning the
  contiguous run of cells on that row sharing the same link id (or same target if
  you did not store the id), with `is_hyperlink: true`, and return it. Only fall
  back to the existing `match_urls_in_row` heuristic when the cell has no explicit
  link.

### `src/pane_ui.rs`

- Hover/click block — **lines 350-387** (`url_at_pointer` L363, `url_highlight`
  L371, `handled_by_url` click handler L377). This already calls `scan_url_at` and
  opens `url.url` via `open`/`xdg-open`. Because you route explicit links through
  `scan_url_at`, this code should work unchanged — verify it opens the explicit
  target, not the visible text. Adjust only if the highlight span needs the run
  bounds you now return.

### Test stub to finish

- `url_match_has_hyperlink_flag` — `src/pane.rs:1671`. Currently:
  `#[ignore = "..."]` + `panic!("add is_hyperlink: bool to UrlMatch ...")`.
  Replace the panic body with a real assertion and remove the `#[ignore]`. The
  test should construct a `CellSnapshot` row carrying an explicit hyperlink, call
  `scan_url_at`, and assert the returned `UrlMatch` has `is_hyperlink == true` and
  `url` equal to the explicit target. Also add (or assert in the same test) that a
  heuristic `http://` match returns `is_hyperlink == false`.

## Acceptance criteria (behavioral — the click path resolves the explicit URI)

"Done" is NOT "added an `is_hyperlink` field" or "stored a `hyperlink` on the
snapshot." Those are dead code until something READS them on the live path.
Done is the observable end-to-end behavior:

> Hit-testing a cell that carries an OSC-8 target (via `scan_url_at`, the exact
> function `pane_ui.rs` calls on Ctrl-hover/Ctrl-click ~L350-387) returns a
> `UrlMatch` whose `url` equals the **explicit** target from
> `cell.hyperlink().uri()` — NOT the visible label text and NOT a heuristic
> `http://` text match — spanning the full contiguous link run, with
> `is_hyperlink == true`. The explicit target is preferred over any heuristic
> match on the same cell.

The integration point is `scan_url_at`. A field that nothing reads on that path
is unfinished, regardless of whether it compiles or your own test touches it.

### Fixed acceptance test — do not edit

This is the test we hand you. It is the gate; you make it pass by wiring source,
not by changing the test. It simulates a Ctrl-click/hover at the cell coords of
an OSC-8 hyperlink and asserts the resolved target is the explicit URI, not the
heuristic. (API confirmed against the tree: `CellSnapshot { col, row, c, fg, bg,
style, underline, underline_color, strikeout, wide, hidden, zerowidth,
hyperlink }`, `pub(crate) fn scan_url_at(cells: &[CellSnapshot], row: i32, col:
i32) -> Option<UrlMatch>`, `UrlMatch.is_hyperlink: bool`, `cell.hyperlink()` at
pane.rs:1298, `set_hyperlink` at term_handler.rs:332.)

```rust
#[test]
fn osc8_click_resolves_explicit_target() {
    // Visible label "docs" on cols 5..9 carries an explicit OSC-8 target whose
    // URI differs from the visible text. A Ctrl-click anywhere in the run must
    // resolve to the explicit URI, span the whole run, and flag is_hyperlink.
    let uri = "https://explicit.example/page".to_string();
    let mut cells: Vec<CellSnapshot> = Vec::new();
    for (i, &ch) in [' ', 's', 'e', 'e', ' ', 'd', 'o', 'c', 's'].iter().enumerate() {
        let hyperlink = if (5..=8).contains(&i) { Some(uri.clone()) } else { None };
        cells.push(CellSnapshot {
            col: i as i32, row: 0, c: ch,
            fg: [1.0; 4], bg: [0.0, 0.0, 0.0, 1.0],
            style: FontStyle::Regular,
            underline: Underline::None, underline_color: [1.0; 4],
            strikeout: false, wide: false, hidden: false,
            zerowidth: Vec::new(), hyperlink,
        });
    }

    // Click on 'd' (col 5) and on 's' (col 8): both land inside the same run and
    // must resolve to the EXPLICIT uri, span cols 5..9, and be is_hyperlink=true.
    for clicked_col in [5, 8] {
        let m = scan_url_at(&cells, 0, clicked_col)
            .expect("OSC-8 cell must hit-test to a UrlMatch");
        assert!(m.is_hyperlink, "explicit OSC-8 link must set is_hyperlink");
        assert_eq!(m.url, "https://explicit.example/page",
            "must resolve to cell.hyperlink().uri(), not the label or a heuristic");
        assert_eq!((m.start_col, m.end_col), (5, 9),
            "must span the full contiguous link run");
    }

    // A cell with no explicit link must still resolve via the heuristic matcher
    // with is_hyperlink=false (Task 1 behavior preserved).
    let plain = cells_from_str(0, "visit https://heuristic.example now");
    let h = scan_url_at(&plain, 0, 6).expect("heuristic URL must still match");
    assert!(!h.is_hyperlink, "heuristic match must be is_hyperlink=false");
}
```

- `UrlMatch` has an `is_hyperlink: bool`; `CellSnapshot` carries the explicit
  target; the snapshot loop populates it from `cell.hyperlink()`.
- For a row of cells carrying an explicit OSC-8 target, `scan_url_at` returns a
  single `UrlMatch` spanning the full link run, `url` = the explicit target,
  `is_hyperlink == true`.
- A cell with no explicit link still resolves via the heuristic matcher with
  `is_hyperlink == false` (Task 1 behavior preserved).
- `url_match_has_hyperlink_flag` is no longer `#[ignore]`d and passes.
- Ctrl-click on an explicit link opens its target URI, not the visible label.

## How to verify

```
cargo test --lib url_match_has_hyperlink_flag
cargo test --lib scan_url
cargo test --lib
cargo build
```

All must pass. If a manual check is cheap, run the app and Ctrl-hover an OSC-8
link to confirm the highlight covers the whole label and the click opens the
target — but the unit test above is the required gate.

## Constraints

- Keep `CellSnapshot.hyperlink` `None` for non-link cells (no per-cell allocation
  in the common case).
- Do not regress Task 1: heuristic `http`/`https`/scheme matching and all
  existing `scan_urls*` tests stay green.
- Prefer explicit target over heuristic only when the cell actually carries one;
  never overwrite a heuristic match's `url` with label text.
- No new dependencies. Keep the diff confined to the functions/structs listed.
