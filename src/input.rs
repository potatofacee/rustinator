use crate::keybindings::BindingTable;
use crate::pane::Pane;
use crate::tabs::{action_to_pane_action, PaneAction};

pub(crate) struct RawTermKey {
    pub key: egui::Key,
    pub mods: egui::Modifiers,
    pub legacy_bytes: Vec<u8>,
}

pub(crate) fn process_keys(
    ctx: &egui::Context,
    bindings: &BindingTable,
    raw_keys: Vec<RawTermKey>,
    targets: &[&Pane],
    scroll_on_keystroke: bool,
) -> Vec<PaneAction> {
    let mut actions: Vec<PaneAction> = Vec::new();

    if targets.is_empty() {
        return actions;
    }

    // Step 1: Check app bindings on raw keys, collecting consumed (key, mods) pairs.
    let consumed: Vec<bool> = raw_keys
        .iter()
        .map(|rk| {
            if let Some(action) = bindings.lookup(rk.key, rk.mods) {
                actions.push(action_to_pane_action(action));
                true
            } else {
                false
            }
        })
        .collect();

    // Step 2: Drain matching Key events from egui so bound keys don't reach widgets.
    // Also check egui-only key events for bindings (e.g. keys that only came through egui).
    ctx.input_mut(|i| {
        i.events.retain(|ev| {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                if let Some(action) = bindings.lookup(*key, *modifiers) {
                    // Only add if not already found via raw keys to avoid duplicates.
                    let already_matched = raw_keys.iter().zip(consumed.iter()).any(|(rk, &c)| {
                        c && rk.key == *key
                            && rk.mods.ctrl == modifiers.ctrl
                            && rk.mods.alt == modifiers.alt
                            && rk.mods.shift == modifiers.shift
                            && rk.mods.mac_cmd == modifiers.mac_cmd
                    });
                    if !already_matched {
                        actions.push(action_to_pane_action(action));
                    }
                    return false;
                }
            }
            true
        });
    });

    // Step 3: Scroll to bottom on any non-consumed input.
    let has_unconsumed = consumed.iter().any(|c| !c);
    if has_unconsumed && scroll_on_keystroke {
        for pane in targets {
            pane.scroll_to_bottom();
        }
    }

    // Step 4: Send all non-consumed raw keys to the terminal. No dedup, no has_text_event check.
    raw_keys
        .iter()
        .zip(consumed.iter())
        .filter(|&(_, c)| !c)
        .for_each(|(rk, _)| {
            for pane in targets {
                pane.send_key(rk.key, rk.mods, Some(rk.legacy_bytes.clone()));
            }
        });

    // Step 5: Handle paste from egui events as fallback for OS-level paste
    // that doesn't arrive as a raw key (e.g. middle-click paste on X11).
    let handled_paste = actions.iter().any(|a| matches!(a, PaneAction::Paste));
    ctx.input(|i| {
        if !handled_paste {
            for event in &i.events {
                if let egui::Event::Paste(text) = event {
                    for pane in targets {
                        pane.send_paste(text);
                    }
                }
            }
        }
    });

    // Step 6: Drain Text and pressed Key events from egui so they don't leak to
    // widgets when the terminal has focus. This prevents the double-send.
    ctx.input_mut(|i| {
        i.events.retain(|ev| match ev {
            egui::Event::Text(_) => false,
            egui::Event::Key { pressed: true, .. } => false,
            egui::Event::Paste(_) => false,
            _ => true,
        });
    });

    actions
}
