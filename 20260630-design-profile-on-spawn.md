# Design: Profile-on-spawn (INFRA-Profile)

WF-C infra block. Status: design, no code. Line numbers verified against source on 2026-06-30.

> Note: the upstream JSON for this piece was not present in the reconcile input. This design is
> reconstructed from ground truth (`src/*.rs`), `gap-analysis.md` (Profile System lines 57-62,
> Layout 76), `20260630-feature-worksession.md` (Profiles lines 70-76), and the cross-references
> in the sibling designs. Where Profile-on-spawn and Group model touch the same `Pane`/`split`
> code, the boundary contract in §9 governs.

## 1. What changes, in one paragraph

Give each `Pane` a **profile name** plus its resolved spawn command, and make `Pane::spawn`
resolve the child command/shell from the profile instead of the hardcoded login-shell block
(`pane.rs:842` / `pane.rs:941`). Profile identity is the **name string** — mirroring the
existing `Config.active_profile: String` keyed against `profiles: Vec<Profile>` (`config.rs:11,18`);
there is no numeric id. A new leaf module `src/profile.rs` owns a *pure* resolver (name ->
`&Profile` -> `ResolvedProfile { defaults, term_bits, command }`) plus next/prev cycling. New
splits inherit the parent pane's profile (always-split-with-profile); new tabs use the active
profile. Per-pane profile switching re-applies appearance live (no respawn), exactly like
Terminator. This also unblocks per-pane profile in saved layouts and custom-command/login-shell
per profile (Gap #22/#24), which feed `spawn` through the same resolver.

## 2. Terminator behavior being mirrored

- Every terminal has a profile (default "default"); switching a terminal's profile re-applies
  appearance (colors/scrollback/etc.) to the *running* terminal and does NOT respawn the child
  — the command only matters at spawn. So a profile switch is a live re-style, never a restart.
- A split inherits the parent terminal's profile ("Always split with profile", gap-analysis.md:61).
- `next_profile`/`previous_profile` cycle the focused terminal through the profile list; both
  are UNBOUND by default in Terminator and reachable via the right-click "Profiles" radio submenu.
- `use_custom_command` + `custom_command` run a command through the shell instead of an
  interactive shell; `login_shell` toggles the `--login` argument (Gap #22/#24).

## 3. Ground-truth facts this design rests on

- `Config.active_profile: String`, `profiles: Vec<Profile>` (`config.rs:11,18`); `Config::active()`
  (`config.rs:443-448`) finds by name and falls back to `profiles[0]` when the name dangles —
  the per-pane resolver reuses this fallback semantics.
- `defaults_from_profile(&Profile) -> PaneDefaults` (`main.rs:174-185`) already maps a profile's
  colors/opacity/palette/selection/clear-wipes into render defaults. Reused unchanged; relocated
  into `profile.rs`.
- `Pane::spawn` (`pane.rs:805-897`) takes `defaults: PaneDefaults` and `term_config: Config`
  (alacritty's `Config`) and **hardcodes** the child command: on non-macOS
  `tty::Shell::new($SHELL, ["--login"])` (`pane.rs:842`); on macOS it sets no shell, letting
  `tty::Options::default()` pick the login shell. `respawn` repeats this (`pane.rs:941`).
- `respawn` (`pane.rs:899-989`) mutates `&mut self` in place (`973-986`); new fields survive it.
- `PaneFactory` (`tabs.rs:174-180`) carries `cell_w, cell_h, term_config, pane_defaults,
  event_loop_proxy`. It is rebuilt per call by `App::pane_factory()` (`main.rs:211-219`) from the
  **active** profile.
- `Profile` (`config.rs:59-88`) has NO command fields yet; the `#[ignore]` tests
  `config_profile_custom_command` (`config.rs:613-617`) and `config_profile_login_shell`
  (`621-624`) name exactly the fields to add.
- `LayoutTemplate::Terminal` is a unit variant carrying no per-pane metadata (`layout.rs:330-338`);
  `layout_template_with_metadata` (`layout.rs:518-522`) is the seam for cwd/command/profile.
- prefs UI: `draw_prefs_profiles` and the profile sub-tabs live in `prefs_ui.rs` (general at
  `224`, the `active_profile` combo in `draw_prefs_global` at `160-208`).

## 4. Data model

### `src/profile.rs` (NEW)
```rust
pub type ProfileName = String;   // identity is the name, like Config.active_profile

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpawnCommand {
    pub program: Option<String>, // None => $SHELL / platform default (preserves today's macOS path)
    pub args: Vec<String>,       // login shell => ["--login"] or []; custom => ["-c", cmd]
}

pub struct ResolvedProfile {
    pub defaults: PaneDefaults,  // via defaults_from_profile
    pub scrolling_history: usize,
    pub semantic_escape_chars: String,
    pub command: SpawnCommand,
}
```
Pure free-functions (each < 40 lines):
```rust
pub fn resolve<'a>(cfg: &'a Config, name: &str) -> &'a Profile   // name -> &Profile, dangling -> active()/[0]
pub fn resolved(cfg: &Config, name: &str) -> ResolvedProfile     // &Profile -> ResolvedProfile bundle
pub fn spawn_command(p: &Profile) -> SpawnCommand                // command rules below
pub fn next_name(cfg: &Config, current: &str) -> ProfileName     // cycle profiles[] by index, wrap
pub fn prev_name(cfg: &Config, current: &str) -> ProfileName
```
`spawn_command` rules (reproduce today's behavior for the default profile):
- `use_custom_command && !custom_command.trim().is_empty()` => `program=None` (=> $SHELL),
  `args=["-c", custom_command]` (Terminator runs custom commands through the shell).
- else (interactive shell): `program=None`, `args = if login_shell { ["--login"] } else { [] }`.
- The platform nuance is preserved by `program=None`: on Linux `spawn` maps `None` -> `$SHELL`
  (today's `pane.rs:841`); on macOS `None` + empty args -> leave `pty_opts.shell` unset (today's
  behavior). `login_shell` defaults `true` so the default profile reproduces `--login` on Linux.

### `src/config.rs` — `Profile` (`config.rs:59-88`)
Add (all `#[serde(default)]`, written on save, missing keys default for old configs):
```rust
#[serde(default)] pub use_custom_command: bool,
#[serde(default)] pub custom_command: String,
#[serde(default = "default_true")] pub login_shell: bool,
```
Add to all three `Profile` construction sites: `new_named` (`config.rs:193-210`), the
`migrate_legacy` literal (`config.rs:406-421`), and `Default` (via `new_named`). Implement the
two `#[ignore]` tests (`config.rs:613-624`).

### `src/pane.rs` — `Pane` (`pane.rs:762-802`)
Add (land together with Group's `group` field — one struct edit):
```rust
pub profile: ProfileName,        // the profile name this pane was spawned with / switched to
pub spawn_command: SpawnCommand, // resolved command, reused by respawn without needing Config
```
Init in the `spawn` literal (`~892`): `profile` and `spawn_command` from new `spawn` params.
`respawn` reuses `self.spawn_command` (no Config needed in the reaper loop — see §6), mirroring
how `clear_wipes_scrollback`/`shared_defaults` already survive respawn (`pane.rs:964-965`).

### `src/tabs.rs` — `PaneFactory` (`tabs.rs:174-180`)
Add `pub profile: ProfileName` and `pub command: SpawnCommand`. The factory becomes
profile-specific: `App::pane_factory_for(name)` resolves a profile name into a full factory;
`App::pane_factory()` == `pane_factory_for(active_profile)`.

## 5. Profile resolution order on spawn / respawn (cross-cutting question (c), part 1)

1. Pick the profile NAME for the new pane:
   - first pane (`App::new`, `main.rs:270`) and `new_tab` (`tabs.rs:404`): `active_profile`.
   - `split`/`split_here`/`SplitAuto`/`OpenTerminalHere`: the focused (parent) pane's `profile`
     (inherit-on-split). The App reads `tab_mgr.active_pane().map(|p| p.profile.clone())`
     (`active_pane` at `tabs.rs:217`).
   - `restore_layout`: per-leaf profile from the layout sidecar if present (§8), else active.
   - profile switch: the chosen name (live re-style, no respawn).
2. Resolve NAME -> `&Profile` via `profile::resolve(cfg, name)` (dangling name -> `active()`/[0]).
3. Derive `ResolvedProfile { defaults, scrolling_history, semantic_escape_chars, command }`.
4. `Pane::spawn` builds `pty_opts.shell` from `command` (replacing `pane.rs:838-843`), and stamps
   `self.profile` + `self.spawn_command`.
5. `respawn` rebuilds `pty_opts.shell` from `self.spawn_command` (replacing `pane.rs:937-942`).

Why store `spawn_command` on the pane rather than re-resolve at respawn: `TabManager::reap_exited`
(`tabs.rs:778-838`) loops internally and is handed a single `PaneFactory` built for the *active*
profile (`tabs.rs:824-829`). A dead pane may hold a *different* profile; re-resolving from the
factory would respawn it with the wrong command. Storing the resolved command on the pane keeps
`respawn` self-contained and `reap_exited` free of `Config` coupling. Cost: one small `Clone`
field. (Alternative — thread `&Config` through `reap_exited` and re-resolve per pane — was
rejected as more coupling for no fidelity gain.)

## 6. New modules and decomposition (NO god classes; every fn < 100 lines)

### `src/profile.rs` — focused owner of profile resolution + cycling
Holds `ProfileName`, `SpawnCommand`, `ResolvedProfile`, and the pure functions in §4. Also the
relocated `defaults_from_profile` (moved from `main.rs:174`; `main.rs` re-exports or calls
`profile::defaults_from_profile`). Each function is small and pure — unit-testable without GL,
PTY, or egui.

### `Pane::spawn` / `Pane::respawn` command block
The hardcoded `#[cfg(not(target_os="macos"))]` shell block (`pane.rs:838-843`, repeated at
`937-942`) becomes a small private `fn apply_spawn_command(opts: &mut tty::Options, cmd:
&SpawnCommand)` (< 20 lines) used by both, so spawn and respawn share one command path and
neither function grows. Net spawn/respawn line counts are roughly unchanged.

### `App::execute_pane_actions` (`main.rs:343-435`) — keep < 100 lines
The split arms (`SplitHorizontal` 353, `SplitVertical` 354, `SplitAuto` 406-412, `OpenTerminalHere`
390) change from `self.tab_mgr.split(dir, &factory)` to a thin App helper:
```rust
fn split_inheriting(&mut self, dir: Direction)        // resolve focused profile -> factory -> tab_mgr.split
fn split_here_inheriting(&mut self)                   // same, for OpenTerminalHere
```
Same arm count (each arm still one line), so `execute_pane_actions` does not grow. The two
profile actions route through ONE combined arm:
```rust
Action::NextProfile | Action::PreviousProfile => self.dispatch_profile_action(action),
```
`App::dispatch_profile_action` (~12 lines) computes the new name via `profile::next_name`/`prev_name`
and calls `App::switch_pane_profile(focused, name)`.

### `App::switch_pane_profile(&mut self, pane_id, name)` (NEW, ~25 lines)
Sets `pane.profile = name`; resolves `ResolvedProfile`; pushes `apply_pane_defaults`
(`main.rs:201`) + `pane.apply_term_config` (`pane.rs:1008`) live; updates `pane.spawn_command`.
No respawn. Reuses the existing live-apply machinery from `apply_prefs` (`main.rs:500-509`).

### `build_context_menu` (`pane_ui.rs:580-640`) — keep < 100 lines
Add a `profiles_menu(ui, pane, cfg, state)` helper (a `ui.menu_button("Profiles", ..)` listing
`cfg.profiles` names as radio items). Selecting a name is data-carrying, so it cannot be an
`Action` (the enum is `Copy`/`Hash`). Use the existing deferred-with-data pattern: add
`pane_view.profile_switch_pending: Option<(PaneId, ProfileName)>` to `PaneViewState`
(`pane_ui.rs:22-27`), applied by the App after `draw_panes` exactly like
`layout_restore_pending` (`main.rs:845-848`). `build_context_menu` gains one `profiles_menu`
call, staying under 100.

## 7. Actions + keybindings + test-harness contract

New `Action` variants: `NextProfile`, `PreviousProfile`. For EACH, update ALL of: `enum Action`
(`keybindings.rs:11-56`); `from_str` (`59-116`, strings `next_profile`/`previous_profile`);
`_assert_exhaustive` (`959-1005`); `all_actions()` (`1007-1052`); `execute_pane_actions`
(`main.rs`, via the combined arm); and reachability (`every_action_reachable_via_binding_or_menu`
`1075-1095`). Both are UNBOUND by default (Terminator parity) and the menu switch is via
`profiles_menu` (not these Actions), so add `NextProfile | PreviousProfile` to the intentional
allowlist in that test alongside `SwitchToTab(_)`/`QuitHotkeyWindow`. Replace the commented
stubs at `keybindings.rs:349-350`.

## 8. Layout serializer connection (cross-cutting question (c), part 2)

`LayoutTemplate::Terminal` is a unit variant; changing it to a struct variant breaks serde
back-compat (an old layout serializes `Terminal` as the bare string `"Terminal"`, which will not
deserialize into a struct variant). Therefore per-pane profile/cwd/command persistence rides on a
**sidecar** on `SavedLayout`, leaving `LayoutTemplate` untouched (zero migration):
```rust
// config.rs SavedLayout (currently name + template, config.rs:45-49)
#[serde(default)] pub terminals: Vec<TerminalMeta>,   // leaf order; empty for old layouts
// new:
pub struct TerminalMeta {
    #[serde(default, skip_serializing_if="Option::is_none")] pub profile: Option<String>,
    #[serde(default, skip_serializing_if="Option::is_none")] pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if="Option::is_none")] pub command: Option<String>,
}
```
`save_layout` (`tabs.rs:709-719`) fills `terminals` by walking `leaves_in_order` and reading each
pane's `profile`/`cwd()` (`pane.rs:1019`). `restore_layout` (`tabs.rs:721-776`) already iterates
leaves via an id iterator; zip the `terminals` meta in the same order to stamp profile/cwd on
spawned panes. **This full layout persistence is the dependent Tier-2 feature, not this infra
block** — the infra's job is only to expose `Pane.profile` so the serializer *can* read it. The
design records the seam and the migration-safe shape; the `layout_template_with_metadata`
`#[ignore]` test (`layout.rs:518-522`) is rewritten to assert the sidecar (its literal hint to
"extend LayoutTemplate::Terminal" is non-binding and is intentionally deviated from for
back-compat).

## 9. Boundary contract with Group model (shared edits)

Both add a per-pane field and both hook `split`/`split_here` (`tabs.rs:252-284`):
- ONE `Pane` struct edit adds `group`, `profile`, `spawn_command`; ONE `spawn` initializer change
  sets all three.
- In `split`/`split_here`, set all inherited state in ONE place: `new.group =
  parent.group.clone()` (Group; pure copy from the focused pane) and `new.profile =
  factory.profile.clone()` + `new.spawn_command = factory.command.clone()` (Profile; from the
  factory the App built for the parent's profile). Group needs no `Config`; Profile's command
  comes from the parent-profile factory.

## 10. Migration

`Profile`'s 3 new fields are `#[serde(default)]`, so old configs load unchanged (`Config` already
has `#[serde(default)]` at `config.rs:8` and a `Default` impl). `SavedLayout.terminals` is
`#[serde(default)]` (empty for old layouts -> identical restore). No `deny_unknown_fields`
anywhere, so downgrade is safe. `Pane.profile`/`spawn_command` are runtime-only. Default-profile
spawn reproduces today's command exactly (`login_shell` default `true`, no custom command).

## 11. Test seams

- `profile::resolve` (pure): exact name hit; dangling name -> active/[0] (reuses `Config::active`
  semantics, mirror its test at `config.rs:879-893`).
- `profile::spawn_command` (pure): default profile => `{None, ["--login"]}`; `login_shell=false`
  => `{None, []}`; custom command => `{None, ["-c", cmd]}`; empty custom string falls back to the
  interactive shell.
- `profile::next_name`/`prev_name` (pure): wraps across `profiles[]`; single profile is a no-op.
- `config`: implement `config_profile_custom_command` + `config_profile_login_shell`
  (`config.rs:613-624`); a round-trip test for the 3 new fields.
- keybindings: a test that `next_profile`/`previous_profile` parse and are in the reachability
  allowlist.
- `SavedLayout` round-trip with and without `terminals` (back-compat).

## 12. Ordered implementation steps (compile/test after each)

1. `config.rs`: add `use_custom_command`/`custom_command`/`login_shell` to `Profile` + all 3
   construction sites; implement the 2 `#[ignore]` tests. `cargo test` — no behavior change yet.
2. Create `src/profile.rs`: `ProfileName`, `SpawnCommand`, `ResolvedProfile`, `resolve`,
   `resolved`, `spawn_command`, `next_name`, `prev_name`; relocate `defaults_from_profile`. Add
   `mod profile;`. Unit tests. `cargo test` — pure, isolated.
3. `pane.rs`: add `profile`/`spawn_command` to `Pane` (together with Group's `group`); add
   `spawn`/`respawn` params + the shared `apply_spawn_command` helper replacing the hardcoded
   blocks (`838-843`, `937-942`). Default args reproduce today. `cargo check`.
4. `tabs.rs`: add `profile`/`command` to `PaneFactory`; `App::pane_factory_for(name)` +
   `pane_factory()`; stamp `new.profile`/`spawn_command` in `split`/`split_here`/`new_tab`.
   `cargo check`.
5. Actions + dispatch: add `NextProfile`/`PreviousProfile` + the harness updates §7; add
   `dispatch_profile_action` + `switch_pane_profile`; route splits through `split_inheriting`.
   `cargo test`.
6. Context menu: `profiles_menu` helper + `pane_view.profile_switch_pending`; apply after
   `draw_panes`. `cargo check` + visual check (switch a pane's profile, colors update live, shell
   keeps running).
7. (Dependent feature, optional in this block) Layout sidecar: `SavedLayout.terminals` +
   `TerminalMeta`; fill in `save_layout`, consume in `restore_layout`; rewrite
   `layout_template_with_metadata`. `cargo test`.

## 13. Open questions

None that Terminator does not settle. Resolved-by-fidelity decisions recorded above:
- Profile switch is a live re-style, not a respawn (Terminator behavior).
- New tab uses the active profile; splits inherit the parent (gap-analysis.md:61 specifies split
  inheritance; Terminator new-tab uses the default profile). rustinator's existing cwd-inheritance
  on new_tab is orthogonal and unchanged.
- Custom command runs via `$SHELL -c` (Terminator behavior).
