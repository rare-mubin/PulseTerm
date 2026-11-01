use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{AnyView, App, Entity, FocusHandle, SharedString};

use crate::terminal::TerminalView;
use crate::ui::editor::EditorView;
use crate::ui::preview::PreviewView;
use crate::ui::settings_view::SettingsView;

/// Stable identity for a tab, preserved as it moves between panes. Splitting a
/// pane or dragging a tab across the workspace re-parents the same item rather
/// than tearing it down and rebuilding it -- which is what keeps the SSH
/// channel behind a terminal alive across the move.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct ItemId(pub u64);

impl ItemId {
    /// One allocator for every kind of tab: ids have to be unique across the
    /// whole workspace, not per kind, because a drag payload carries nothing but
    /// the id and the pane it came from.
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        ItemId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Which kind of tab this is. Settings is a singleton -- opening it twice
/// raises the tab you already have. Editors are deduplicated by path instead,
/// in `Workspace::open_editor`, since one file in two tabs would be two
/// diverging copies of it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemKind {
    Terminal,
    Settings,
    Editor,
    /// A rendered Markdown file or an image. Deduplicated by path, like an
    /// editor, so the same file can't open twice.
    Preview,
}

/// Anything that can occupy a tab.
///
/// Zed models this as an `Item` trait so third-party crates can add tab kinds.
/// An enum stays fully typed for a fraction of the plumbing, and the dispatch is
/// confined to this file, so the cost of a new kind is one variant plus one arm
/// in each method here.
#[derive(Clone)]
pub enum PaneItem {
    Terminal(Entity<TerminalView>),
    Settings(Entity<SettingsView>),
    Editor(Entity<EditorView>),
    Preview(Entity<PreviewView>),
}

impl PaneItem {
    pub fn kind(&self) -> ItemKind {
        match self {
            PaneItem::Terminal(_) => ItemKind::Terminal,
            PaneItem::Settings(_) => ItemKind::Settings,
            PaneItem::Editor(_) => ItemKind::Editor,
            PaneItem::Preview(_) => ItemKind::Preview,
        }
    }

    pub fn id(&self, cx: &App) -> ItemId {
        match self {
            PaneItem::Terminal(view) => view.read(cx).item_id,
            PaneItem::Settings(view) => view.read(cx).item_id,
            PaneItem::Editor(view) => view.read(cx).item_id,
            PaneItem::Preview(view) => view.read(cx).item_id,
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        match self {
            PaneItem::Terminal(view) => view.read(cx).title(),
            PaneItem::Settings(_) => "settings".into(),
            PaneItem::Editor(view) => view.read(cx).title(),
            PaneItem::Preview(view) => view.read(cx).title(),
        }
    }

    /// A vertical hairline the window's content wants continued up through
    /// its header, at this distance from the left -- Settings' sidebar edge.
    pub fn header_divider(&self, cx: &App) -> Option<f32> {
        match self {
            PaneItem::Settings(view) => view.read(cx).sidebar_edge(),
            _ => None,
        }
    }

    /// Whether this window holds work that would be lost. Only an editor
    /// ever can -- its text has never existed anywhere but this process --
    /// and the column header marks it with a dot.
    pub fn is_dirty(&self, cx: &App) -> bool {
        match self {
            PaneItem::Editor(view) => view.read(cx).is_dirty(),
            _ => false,
        }
    }

    /// Why what this window was showing has ended, if it has. A dead shell,
    /// and nothing else: an editor or a preview is as alive as the file behind
    /// it.
    pub fn exit_reason(&self, cx: &App) -> Option<SharedString> {
        match self {
            PaneItem::Terminal(view) => view.read(cx).exit_reason(),
            _ => None,
        }
    }

    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            PaneItem::Terminal(view) => view.read(cx).focus_handle.clone(),
            PaneItem::Settings(view) => view.read(cx).focus_handle.clone(),
            PaneItem::Editor(view) => view.read(cx).focus_handle.clone(),
            PaneItem::Preview(view) => view.read(cx).focus_handle.clone(),
        }
    }

    pub fn to_any_view(&self) -> AnyView {
        match self {
            PaneItem::Terminal(view) => view.clone().into(),
            PaneItem::Settings(view) => view.clone().into(),
            PaneItem::Editor(view) => view.clone().into(),
            PaneItem::Preview(view) => view.clone().into(),
        }
    }
}

impl PartialEq for PaneItem {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PaneItem::Terminal(a), PaneItem::Terminal(b)) => a == b,
            (PaneItem::Settings(a), PaneItem::Settings(b)) => a == b,
            (PaneItem::Editor(a), PaneItem::Editor(b)) => a == b,
            (PaneItem::Preview(a), PaneItem::Preview(b)) => a == b,
            _ => false,
        }
    }
}
