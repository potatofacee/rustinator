# Agentic Task 1 — Detect additional URL schemes in the link matcher

**Difficulty: EASY**

## Goal

The terminal's URL matcher currently recognizes only `http://` and `https://`
links. Extend it to also detect `mailto:`, `file://`, `ssh://`, `ftp://`, bare
email addresses (`user@example.com`), and bare `www.` domains. Six unit tests
for these cases already exist but are marked `#[ignore]`. Your job is to make the
matcher detect these forms, then remove the `#[ignore]` attributes so the tests
run and pass. Do not change the test bodies or their expected strings.

## Files + functions to touch (all in `src/pane.rs`)

- `is_url_boundary(c: char) -> bool` — **line 430**. Characters that terminate a
  URL run. You may need to keep `@`, `:`, `/`, `.` as non-boundary so emails and
  schemes scan correctly (they already are non-boundary today). Review before
  editing; you likely do not need to change this.
- `match_urls_in_row(row, chars, col_lookup, out)` — **line 440**. This is the
  real matcher used by both production hit-testing and the tests. Currently it
  only checks the `http`/`https` prefix arrays (lines 441-453). Add detection for
  the new schemes and bare forms here.
- `scan_urls(cells)` — **line 476** (a `#[cfg(test)]` helper). It just groups
  cells by row and calls `match_urls_in_row`. You should not need to edit it; the
  tests call it.

### The six `#[ignore]`d tests to un-ignore (lines 1610-1667)

Remove only the `#[ignore = "..."]` line above each:

- `scan_urls_detects_email` — line 1611 — target substring `user@example.com`
- `scan_urls_detects_mailto` — line 1621 — must `starts_with("mailto:")`
- `scan_urls_detects_file_uri` — line 1631 — must `starts_with("file:///")`
- `scan_urls_detects_ssh_uri` — line 1641 — must `starts_with("ssh://")`
- `scan_urls_detects_ftp_uri` — line 1651 — must `starts_with("ftp://")`
- `scan_urls_detects_bare_www` — line 1661 — target substring `www.example.com`

Each test asserts `urls.len() == 1` for its input string, so the matcher must
emit exactly one match per input and must not double-count (e.g. a `mailto:`
match must not also fire the bare-email rule for the same span).

## Acceptance criteria (behavioral — observed at the matcher used by the live hit-test path)

"Done" is NOT "added scheme strings to an array." Done is: each new form,
when run through `match_urls_in_row` (the exact function the production
hover/Ctrl-click hit-test calls via `scan_url_at`), actually resolves to a
single `UrlMatch` with the correct span and `url`. The fixed, non-editable
acceptance set is the **six pre-written `#[ignore]`d tests** listed above —
they already exist in `src/pane.rs` and define done. You may not edit their
bodies or expected strings; you make them pass by changing source only.
Un-ignoring a test without making the matcher genuinely detect the form (or
weakening an assertion) is NOT done.

- All six previously-ignored tests run and pass.
- The scheme matches (`mailto:`, `file://`, `ssh://`, `ftp://`) preserve the full
  scheme prefix in `UrlMatch.url`.
- Bare email and bare `www.` are detected as a single match each.
- No double-counting: each test input yields exactly one `UrlMatch`.
- The existing `http`/`https` tests (`scan_urls_finds_https` L1585,
  `scan_urls_finds_http` L1593, `scan_urls_none_in_plain_text` L1601, and the
  `scan_urls_*` group at L1816+) still pass unchanged.

## How to verify

```
cargo test --lib scan_urls
```

This runs every `scan_urls*` test, including the six you un-ignored. All must
pass (0 ignored remaining in that group except the OSC-8 stub
`url_match_has_hyperlink_flag`, which belongs to Task 2 — leave it ignored).

Then confirm nothing else broke:

```
cargo test --lib
cargo build
```

## Constraints

- Do not modify any test body or its expected strings — only delete the
  `#[ignore]` lines for the six tests above.
- Do not touch `url_match_has_hyperlink_flag` (line 1671) — that is Task 2.
- Keep changes inside `match_urls_in_row` (and `is_url_boundary` only if truly
  required). No new dependencies, no new public API, no regex crate.
- Keep all currently-passing tests green.
