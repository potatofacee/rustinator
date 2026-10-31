# rustinator patches to arboard 3.6.1

This is arboard 3.6.1 as published on crates.io (`Cargo.toml`, `README.md`,
`rustfmt.toml`, the licence files, `src/` and `examples/`; the crates.io
bookkeeping files, `.github/` and `tools/` are left out). It is wired in with
`[patch.crates-io]` in rustinator's `Cargo.toml`, so egui-winit's arboard
resolves to it as well.

Only `src/platform/linux/x11.rs` is changed, and only its read path. Every
change there is marked `// rustinator patch:`. The macOS, Windows and Wayland
code is untouched.

## Why

rustinator mostly runs in an Xvnc session, where the CLIPBOARD owner is often
the VNC server's clipboard bridge, and it answers late or not at all while the
viewer is idle. Upstream's `get_text` asks the owner for six targets in turn
(UTF8_STRING, text/plain;charset=utf-8, text/plain;charset=UTF-8, STRING,
TEXT, text/plain). Each request waits up to `LONG_TIMEOUT_DUR` (4 s), and a
timeout moves on to the next target, so an owner that never answers costs
6 × 4 s = 24 s per paste.

## Patch 1: negotiate text formats with TARGETS (ICCCM 2.6.2)

`Clipboard::get_text` calls the new `Inner::read_text` instead of `Inner::read`:

1. It asks the X server for the selection owner (`selection_owner`, split out
   of `is_owner`). If we own it, it reads our own data as before
   (`read_own`, split out of `read` unchanged). If there is no owner, it
   returns `ContentNotAvailable` at once, because the server would refuse
   every request.
2. It sends one `TARGETS` request, with the usual 4 s timeout. The reply
   property is read with type `ATOM`.
   - Timeout: the owner is unresponsive, so it returns `ContentNotAvailable`
     at once.
   - A list of atoms (`parse_targets`): it picks the first of `get_text`'s
     formats, in the same preference order as before, that the owner offers
     (`pick_target`). It then makes exactly one request for that format. If
     the owner offers none of them, it returns `ContentNotAvailable` without
     another request.
   - Refused (property `None`), malformed (empty, or not a whole number of
     32-bit atoms), or an error (such as a reply of the wrong type): it asks
     for UTF8_STRING, then STRING. It moves on only when the owner refuses one,
     never after a timeout.

To support this, `read_single`'s body became `convert`, which returns
`Conversion::{Data, Refused, TimedOut}` instead of folding a refusal and a
timeout into `ContentNotAvailable`. It also takes a `reply_type`: the property
type accepted in the reply, which is the target itself for data and `ATOM` for
`TARGETS`. That type is also used for the reply's INCR segments
(`handle_read_property_notify`). `read_single` is now a wrapper that keeps the
old contract, so `read`, and through it `get_html`, `get_image` and
`get_file_list`, behaves as before.

The handler's refusal case became `ReadSelNotifyResult::Refused` instead of
`Err(ContentNotAvailable)`. `convert` maps it to `Conversion::Refused`, and
`read_single` maps that back to `Err(ContentNotAvailable)`.

INCR handling, timeouts, the 1 ms poll and everything on the serving side are
unchanged.

Worst cases for a text read:

| Owner | Upstream | Patched |
|---|---|---|
| None | 6 requests, each refused at once by the server | 1 GetSelectionOwner round trip, no request |
| Never answers | ~24 s | ~4 s (TARGETS timeout) |
| Answers TARGETS, never answers the data request | ~24 s | TARGETS time + 4 s |
| Slow but answers each request within 4 s | that delay, once | that delay, twice (< 8 s) |
| Normal | 1 round trip | 2 round trips (TARGETS, then data) |

## Patch 2: ignore stale SelectionNotify events

`handle_read_selection_notify` used to treat a SelectionNotify whose target
differed from the current request as "not available", which failed the
current request. With upstream's six-target loop, a slow owner answering an
earlier target after its timeout made arboard drop the target it was then
waiting for. Now such an event is logged and ignored, and the wait goes on.
A refusal (property `None`) counts only when it is for the current target.

The target is what identifies a request. The requests of one read each ask
for a different target, and every read opens its own connection and window,
so replies to an older read can't arrive. The `time` field can't tell
requests apart, because arboard asks with `CurrentTime` and owners echo it
back.

With patch 1, a read sends its next request only after the previous one was
answered, and a timeout ends the read. So within a text read, a stale event
can now only be a duplicate. The fix still matters for `read`, and it is
correct on its own.

## Tests

`x11.rs` has a `tests` module for `parse_targets` and `pick_target`, which
need no X server. rustinator's `cargo test` doesn't run them: arboard is not
a workspace member, and `cargo test -p arboard` needs arboard's dev-dependency
(`env_logger` 0.10) in the lockfile. To run them, copy this directory
somewhere else, drop the `[[example]]` tables, the `env_logger`
dev-dependency and the `wl-clipboard-rs` optional dependency (and its two
features) from the copy's `Cargo.toml`, drop the `env_logger::builder()` line
in `src/lib.rs`'s tests, add an empty `[workspace]`, copy in rustinator's
`Cargo.lock`, and run
`cargo test --offline --lib platform::linux::x11::tests`.

## Re-applying on an arboard upgrade

1. Replace this directory with the new release, keeping this file.
2. Re-apply the `// rustinator patch:` hunks in
   `src/platform/linux/x11.rs`. To get them as a diff, diff this copy against
   the pristine `arboard-3.6.1/src/platform/linux/x11.rs` from the cargo
   registry.
3. In rustinator's `Cargo.lock`, the `arboard` entry must have no `source` or
   `checksum` line, which marks it as the path crate. Check with
   `cargo tree -i arboard`.
4. Run `cargo check`, `cargo test` and
   `cargo check --target aarch64-apple-darwin --all-targets`.

If upstream takes these changes, drop the vendored copy and the
`[patch.crates-io]` entry.

## Cargo.toml: silence upstream warnings

As a path dependency, cargo no longer caps this crate's lints, so upstream's
`deprecated` (image::io::Reader, objc2 APIs) and `unused_unsafe` warnings
(osx.rs) appeared on every rustinator build. A `[lints.rust]` table at the end
of `Cargo.toml` allows just those two. Drop it on an upgrade if upstream has
fixed them.
