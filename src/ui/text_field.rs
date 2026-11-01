use gpui::{prelude::*, Entity, Pixels};
use gpui_component::input::{Input, InputState};
use gpui_component::Sizable;

/// Every single-line field in the app: gpui-component's `Input`, sized to the
/// TUI grid.
///
/// This replaced a hand-rolled field that drew its text as plain children, so a
/// value longer than the box ran straight out over its neighbours, and it had no
/// selection or clipboard at all. `Input` clips and scrolls its text, and brings
/// mouse and shift-arrow selection, ctrl-a/c/x/v, undo and IME -- the same editor
/// state machine the code editor tabs run on, which already ships in the binary.
///
/// Its colours, border and focus ring come from the component theme that
/// `main::apply_component_theme` maps our palette onto; only the size is set
/// here. `xsmall` is the one without vertical padding, and the text size is
/// overridden because the size presets pick their own.
pub fn text_input(state: &Entity<InputState>, font_size: Pixels) -> Input {
    Input::new(state).xsmall().text_size(font_size)
}
