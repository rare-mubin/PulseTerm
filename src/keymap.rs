//! The rebindable commands, and the user's changes to them (`keys.json`).
//!
//! Settings -> Keys (frame 12) edits this. Every app command that sits on
//! ctrl+shift is listed here once, with its id, its group and its default
//! keys; `main.rs` keeps only the bindings that are not the user's to change
//! -- Escape inside a dialog, Tab passing through to the shell -- because
//! rebinding those would break the dialog or the terminal rather than move a
//! shortcut.
//!
//! **Rebinding appends; it never clears.** gpui's `clear_key_bindings` would
//! also wipe gpui-component's own (copy and paste in every text field, the
//! editor's keys). Instead a change disables the keys a command no longer
//! uses with `NoAction` and binds the ones it now does, *after* everything
//! else -- and gpui ranks a later binding above an earlier one in the same
//! context (`Keymap::bindings_for_input`), so the newest word wins. A session
//! of heavy rebinding grows the keymap by a few entries; a restart rebuilds it
//! from `keys.json` at its plain size.
//!
//! `keys.json` holds only what differs from the defaults: one list of
//! keystrokes per changed command, empty for "unbound". It is one small file
//! read at launch, alongside `settings.json`.

use std::collections::BTreeMap;

use gpui::{App, Global, KeyBinding, NoAction};
use serde::{Deserialize, Serialize};

use crate::store::{read_json, write_json};
use crate::workspace::{
    CloseTab, ConsumeWindow, CycleColumnWidth, DecreaseFontSize, Disconnect, ExpelWindow,
    FocusColumnLeft, FocusColumnRight, FocusWindowDown, FocusWindowUp, IncreaseFontSize,
    MoveColumnLeft, MoveColumnRight, MoveWindowDown, MoveWindowUp, NewTerminal, OpenConnections,
    OpenRemoteDesktop,
    OpenSettings, ResetFontSize, StackTerminal, ToggleExplorer, ToggleOverview,
};

const FILE: &str = "keys.json";

/// Every command here lives in the `Workspace` key context.
const CONTEXT: &str = "Workspace";

/// One rebindable command.
pub struct Command {
    /// Stable, shown on the Keys page and used as the key in `keys.json`.
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub defaults: &'static [&'static str],
    /// Builds the binding for one keystroke. A function rather than a stored
    /// action because `KeyBinding::new` is generic over the action type.
    bind: fn(&str) -> KeyBinding,
}

macro_rules! command {
    ($id:literal, $label:literal, $group:literal, [$($key:literal),*], $action:expr) => {
        Command {
            id: $id,
            label: $label,
            group: $group,
            defaults: &[$($key),*],
            bind: |keys| KeyBinding::new(keys, $action, Some(CONTEXT)),
        }
    };
}

/// The commands, in the order and groups the Keys page shows them (frame 12).
pub const COMMANDS: &[Command] = &[
    command!("tiling.focus.left", "Focus column left", "tiling", ["ctrl-shift-left", "ctrl-shift-tab"], FocusColumnLeft),
    command!("tiling.focus.right", "Focus column right", "tiling", ["ctrl-shift-right", "ctrl-tab"], FocusColumnRight),
    command!("tiling.focus.up", "Focus window above", "tiling", ["ctrl-shift-up"], FocusWindowUp),
    command!("tiling.focus.down", "Focus window below", "tiling", ["ctrl-shift-down"], FocusWindowDown),
    command!("tiling.column.new", "New column", "tiling", ["ctrl-shift-t"], NewTerminal),
    command!("tiling.column.stack", "Stack into column", "tiling", ["ctrl-shift-d"], StackTerminal),
    command!("tiling.column.left", "Move column left", "tiling", ["ctrl-shift-alt-left"], MoveColumnLeft),
    command!("tiling.column.right", "Move column right", "tiling", ["ctrl-shift-alt-right"], MoveColumnRight),
    command!("tiling.window.up", "Move window up", "tiling", ["ctrl-shift-alt-up"], MoveWindowUp),
    command!("tiling.window.down", "Move window down", "tiling", ["ctrl-shift-alt-down"], MoveWindowDown),
    command!("tiling.window.expel", "Expel window", "tiling", ["ctrl-shift-o"], ExpelWindow),
    command!("tiling.window.consume", "Consume window", "tiling", ["ctrl-shift-u"], ConsumeWindow),
    command!("tiling.column.width", "Cycle column width", "tiling", ["ctrl-shift-r"], CycleColumnWidth),
    command!("window.close", "Close window", "window", ["ctrl-shift-w"], CloseTab),
    command!("window.overview", "Overview", "window", ["ctrl-shift-space"], ToggleOverview),
    command!("window.files", "File tree", "window", ["ctrl-shift-b"], ToggleExplorer),
    command!("window.command", "Command bar", "window", ["ctrl-shift-k"], OpenConnections),
    command!("session.disconnect", "Disconnect", "session", ["ctrl-shift-q"], Disconnect),
    command!("session.remote", "Remote desktop", "session", ["ctrl-shift-m"], OpenRemoteDesktop),
    // `<` too: on Linux shift-, arrives as `<`, and without it Settings has
    // no working shortcut there.
    command!("session.settings", "Settings", "session", ["ctrl-shift-,", "ctrl-<", "ctrl-shift-<"], OpenSettings),
    // Both spellings of plus: on some layouts shift-= arrives as `+`.
    command!("view.zoom.in", "Bigger type", "text & view", ["ctrl-=", "ctrl-+", "ctrl-shift-="], IncreaseFontSize),
    command!("view.zoom.out", "Smaller type", "text & view", ["ctrl--"], DecreaseFontSize),
    command!("view.zoom.reset", "Reset type size", "text & view", ["ctrl-0"], ResetFontSize),
];

/// Keys the terminal handles itself, before any binding sees them -- listed on
/// the Keys page so the whole keyboard is in one place, but not rebindable.
pub const FIXED: &[(&str, &str, &str)] = &[
    ("Copy / paste in a terminal", "text.copy / text.paste", "ctrl-shift-c / ctrl-shift-v"),
    ("Save file", "editor.save", "ctrl-s"),
];

/// The user's changes, as stored: command id to its keystrokes.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub overrides: BTreeMap<String, Vec<String>>,
}

impl Global for Keys {}

impl Keys {
    pub fn load() -> Self {
        read_json(FILE)
    }

    fn save(&self) {
        write_json(FILE, self);
    }

    /// The keystrokes a command answers to now.
    pub fn keys_for(&self, command: &Command) -> Vec<String> {
        match self.overrides.get(command.id) {
            Some(keys) => keys.clone(),
            None => command.defaults.iter().map(|k| k.to_string()).collect(),
        }
    }

    pub fn is_changed(&self, command: &Command) -> bool {
        self.overrides.contains_key(command.id)
    }

    /// The command already using `keystroke`, other than `except`.
    pub fn holder(&self, keystroke: &str, except: &str) -> Option<&'static Command> {
        COMMANDS
            .iter()
            .filter(|command| command.id != except)
            .find(|command| self.keys_for(command).iter().any(|k| k == keystroke))
    }

    /// How many commands share a keystroke with another -- the Keys page's
    /// CONFLICTS count. Saving a change swaps instead of colliding, so this
    /// only goes above zero through a hand-edited `keys.json`.
    pub fn conflicts(&self) -> usize {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for command in COMMANDS {
            for key in self.keys_for(command) {
                *seen.entry(key).or_default() += 1;
            }
        }
        seen.values().filter(|n| **n > 1).count()
    }
}

/// A command's first key as a person reads it -- `Ctrl+Shift+K` -- for a
/// tooltip, or nothing when it is unbound. Follows any rebinding.
pub fn describe(cx: &App, id: &str) -> Option<String> {
    let command = COMMANDS.iter().find(|c| c.id == id)?;
    let keys = cx.try_global::<Keys>()?;
    let first = keys.keys_for(command).into_iter().next()?;
    let keystroke = gpui::Keystroke::parse(&first).ok()?;
    let m = keystroke.modifiers;
    let mut parts: Vec<String> = Vec::new();
    for (on, name) in [(m.control, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift"), (m.platform, "Super")] {
        if on {
            parts.push(name.into());
        }
    }
    let mut key = keystroke.key.clone();
    if let Some(first) = key.get(0..1) {
        key = first.to_uppercase() + &key[1..];
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// Binds every command to its current keys. Called once at launch, after the
/// fixed bindings in `main.rs`.
pub fn install(cx: &mut App) {
    let keys = Keys::load();
    let bindings: Vec<KeyBinding> = COMMANDS
        .iter()
        .flat_map(|command| {
            keys.keys_for(command)
                .into_iter()
                .filter_map(|key| bind(command, &key))
                .collect::<Vec<_>>()
        })
        .collect();
    cx.bind_keys(bindings);
    cx.set_global(keys);
}

/// Gives `id` the single keystroke `keystroke`, or none when `None`
/// (unbound). A keystroke another command holds is taken from it and that
/// command gets `id`'s old one in return -- the swap the design's "saving
/// swaps it" describes -- so the keyboard never has two commands on one chord.
pub fn assign(cx: &mut App, id: &str, keystroke: Option<String>) {
    let Some(command) = COMMANDS.iter().find(|c| c.id == id) else {
        return;
    };
    let mut keys = cx.global::<Keys>().clone();
    let before = keys.keys_for(command);
    let mut touched = vec![command];
    let mut removed: Vec<String> = before.clone();

    if let Some(keystroke) = keystroke.clone() {
        if let Some(holder) = keys.holder(&keystroke, id) {
            let theirs: Vec<String> = keys
                .keys_for(holder)
                .into_iter()
                .filter(|k| *k != keystroke)
                .chain(before.first().cloned())
                .collect();
            set_override(&mut keys, holder, theirs);
            touched.push(holder);
        }
        removed.retain(|k| *k != keystroke);
    }
    set_override(&mut keys, command, keystroke.into_iter().collect());
    rebind(cx, &keys, &touched, &removed);
    keys.save();
    cx.set_global(keys);
}

/// Back to the defaults: one command, or every command when `id` is `None`.
pub fn reset(cx: &mut App, id: Option<&str>) {
    let mut keys = cx.global::<Keys>().clone();
    let touched: Vec<&'static Command> = COMMANDS
        .iter()
        .filter(|c| id.is_none_or(|id| c.id == id) && keys.is_changed(c))
        .collect();
    let removed: Vec<String> = touched.iter().flat_map(|c| keys.keys_for(c)).collect();
    for command in &touched {
        keys.overrides.remove(command.id);
    }
    rebind(cx, &keys, &touched, &removed);
    keys.save();
    cx.set_global(keys);
}

fn set_override(keys: &mut Keys, command: &Command, list: Vec<String>) {
    let defaults: Vec<String> = command.defaults.iter().map(|k| k.to_string()).collect();
    if list == defaults {
        keys.overrides.remove(command.id);
    } else {
        keys.overrides.insert(command.id.to_string(), list);
    }
}

/// Disables the keystrokes that were given up, then binds the touched
/// commands' current ones -- in that order, so a keystroke moved from one
/// command to another ends up on the new one (a later binding wins).
fn rebind(cx: &mut App, keys: &Keys, touched: &[&'static Command], removed: &[String]) {
    let mut bindings: Vec<KeyBinding> = removed
        .iter()
        .filter(|key| gpui::Keystroke::parse(key).is_ok())
        .map(|key| KeyBinding::new(key, NoAction, Some(CONTEXT)))
        .collect();
    for command in touched {
        bindings.extend(keys.keys_for(command).iter().filter_map(|key| bind(command, key)));
    }
    cx.bind_keys(bindings);
}

/// A binding for one keystroke, or nothing when the stored text is not a
/// keystroke gpui can parse -- `keys.json` is a file a person can edit, and a
/// typo there must not panic inside `KeyBinding::new`.
fn bind(command: &Command, key: &str) -> Option<KeyBinding> {
    gpui::Keystroke::parse(key).ok()?;
    Some((command.bind)(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_is_a_keystroke_and_none_collide() {
        let mut seen = std::collections::HashSet::new();
        for command in COMMANDS {
            for key in command.defaults {
                assert!(gpui::Keystroke::parse(key).is_ok(), "{key} does not parse");
                assert!(seen.insert(*key), "{key} is bound twice");
            }
        }
        assert_eq!(Keys::default().conflicts(), 0);
    }

    #[test]
    fn overrides_replace_defaults_and_report_holders() {
        let mut keys = Keys::default();
        let new_column = COMMANDS.iter().find(|c| c.id == "tiling.column.new").unwrap();
        keys.overrides
            .insert(new_column.id.into(), vec!["ctrl-shift-n".into()]);
        assert_eq!(keys.keys_for(new_column), vec!["ctrl-shift-n".to_string()]);
        assert!(keys.is_changed(new_column));
        assert_eq!(
            keys.holder("ctrl-shift-n", "window.close").map(|c| c.id),
            Some("tiling.column.new")
        );
        assert!(keys.holder("ctrl-shift-t", "").is_none());
    }
}
