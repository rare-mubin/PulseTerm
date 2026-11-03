use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    anchored, deferred, div, prelude::*, px, App, ClickEvent, ClipboardItem, Context, Entity,
    Pixels,
    EventEmitter, ExternalPaths, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    MouseDownEvent, PathPromptOptions, Point, SharedString, Subscription, Task, Window,
};
use gpui_component::input::{InputEvent, InputState};

use super::icon::{for_filename, icon, small, Icon};
use super::text_field::text_input;
use crate::host::{FileEntry, Host, Progress, Properties};
use crate::theme::Theme;

pub enum ExplorerEvent {
    /// Double-clicked a file; the workspace opens it in an editor tab -- or, if
    /// it's an image, in a preview tab, since the editor can't show one.
    OpenFile(String),
    /// Asked for the rendered version from the context menu: Markdown drawn as
    /// Markdown, an image as a picture.
    PreviewFile(String),
    /// "Open in terminal" beside the path: a new terminal column whose shell
    /// starts in this directory.
    OpenTerminal(String),
}

impl EventEmitter<ExplorerEvent> for ExplorerView {}

/// One directory's worth of children, once loaded.
enum Node {
    Loading,
    Failed(SharedString),
    Loaded(Vec<FileEntry>),
}

/// What the inline text field is currently for. Renaming replaces a row's label,
/// creating inserts a blank row in the target directory; both are the same
/// mechanic, so they share one field.
#[derive(Clone)]
enum Editing {
    Rename { path: String },
    Create { parent: String, is_dir: bool },
}

#[derive(Clone)]
struct Clipboard {
    path: String,
    is_dir: bool,
}

/// The transfer in flight. The byte count isn't here: it lives in the shared
/// `Progress` the copy updates as it runs, and the panel reads it while
/// painting. `started` is what turns bytes into a speed.
struct Transfer {
    verb: &'static str,
    name: SharedString,
    /// 1-based, for "3 of 7" when several files were dropped at once.
    index: usize,
    count: usize,
    progress: Arc<Progress>,
    started: Instant,
}

#[derive(Clone)]
struct Menu {
    at: Point<Pixels>,
    /// The row it was opened on, or `None` for the empty space below the tree,
    /// where only the "new" and paste actions make sense.
    target: Option<Target>,
}

#[derive(Clone)]
struct Target {
    path: String,
    name: String,
    is_dir: bool,
}

/// The remote file tree, in the shape VS Code's Explorer takes: an editable path
/// bar over a flat scroll list of rows indented by depth.
///
/// Directories are read lazily and cached by path. A tree of nested `Vec`s would
/// mean rebuilding a nested structure on every expand; a flat map keyed by path
/// means expanding a folder inserts one entry, and collapsing keeps what was
/// already fetched so re-expanding is instant.
pub struct ExplorerView {
    pub focus_handle: FocusHandle,
    host: Option<Host>,
    /// The directory the tree is rooted at. Double-clicking a folder moves here
    /// rather than expanding it, which is what makes the path bar meaningful.
    root: Option<String>,
    path_field: Entity<InputState>,
    /// A path waiting to be shown in the path bar. Setting an input's text needs
    /// a `Window`, and `navigate` is reached from places that have none (the
    /// home-directory lookup after connecting), so it is queued here and applied
    /// on the next render.
    pending_path: Option<String>,
    name_field: Entity<InputState>,
    nodes: HashMap<String, Node>,
    expanded: HashSet<String>,
    selected: Option<String>,
    error: Option<SharedString>,
    menu: Option<Menu>,
    editing: Option<Editing>,
    clipboard: Option<Clipboard>,
    confirm_delete: Option<Target>,
    properties: Option<(Target, Option<Properties>)>,
    /// Set while a transfer is running; see `Transfer`.
    transfer: Option<Transfer>,
    /// Where to open instead of the home directory on the next connection,
    /// from the saved session. Consumed on use, so every later connect in the
    /// same run goes home as usual.
    pending_root: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ExplorerView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let path_field = cx.new(|cx| InputState::new(window, cx).placeholder("remote path"));
        let name_field = cx.new(|cx| InputState::new(window, cx).placeholder("name"));

        // Escape is handled by key-down wrappers in `render`, not here: the input
        // raises no event for it, it just lets the key propagate.
        let subscriptions = vec![
            cx.subscribe_in(&path_field, window, |this, field, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let path = field.read(cx).value().trim().to_string();
                    this.navigate(path, cx);
                }
            }),
            cx.subscribe_in(&name_field, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.commit_edit(cx);
                }
            }),
        ];

        Self {
            focus_handle: cx.focus_handle(),
            host: None,
            root: None,
            path_field,
            pending_path: None,
            name_field,
            nodes: HashMap::new(),
            expanded: HashSet::new(),
            selected: None,
            error: None,
            menu: None,
            editing: None,
            clipboard: None,
            confirm_delete: None,
            properties: None,
            transfer: None,
            pending_root: None,
            _subscriptions: subscriptions,
        }
    }

    /// The directory the tree is rooted at, for the session file.
    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }


    /// Restores what the last session had. Called *before* the connection is
    /// handed over: `set_host` heads for the home directory the moment it
    /// arrives, and the saved path has to be in place to be preferred.
    pub fn restore(&mut self, root: Option<String>) {
        self.pending_root = root;
    }

    /// Points the explorer at a host, or clears it on disconnect.
    pub fn set_host(&mut self, host: Option<Host>, cx: &mut Context<Self>) {
        self.host = host.clone();
        self.nodes.clear();
        self.expanded.clear();
        self.selected = None;
        self.error = None;
        self.root = None;
        self.menu = None;
        self.editing = None;
        self.confirm_delete = None;
        self.properties = None;
        self.transfer = None;

        let Some(host) = host else {
            cx.notify();
            return;
        };

        // A restored session names its own directory, and asking the server
        // for a home directory nobody is going to use is a round trip wasted.
        if let Some(root) = self.pending_root.take() {
            self.navigate(root, cx);
            cx.notify();
            return;
        }

        cx.spawn(async move |this, cx| {
            let home = host.files.home().await;
            let _ = this.update(cx, |this, cx| match home {
                Ok(home) => this.navigate(home, cx),
                Err(error) => {
                    this.error = Some(error.into());
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Moves the tree to `path`, which becomes the new root.
    fn navigate(&mut self, path: String, cx: &mut Context<Self>) {
        if path.is_empty() {
            return;
        }
        self.root = Some(path.clone());
        self.expanded.clear();
        self.nodes.clear();
        self.selected = None;
        self.error = None;
        self.menu = None;
        self.editing = None;
        self.sync_path_field(cx);
        self.load(path, cx);
    }

    /// Whether the tree is this machine's, which is all the wording of the two
    /// transfer actions depends on: copying a file into a remote directory is
    /// an upload, and into a local one it is a copy.
    fn is_local(&self) -> bool {
        self.host.as_ref().is_some_and(|host| host.files.is_local())
    }

    /// Path arithmetic for the attached host. A remote path is POSIX whatever
    /// this client runs on and a local one is Windows', so these cannot be the
    /// free functions they were when there was only one kind of host.
    fn join(&self, parent: &str, name: &str) -> String {
        match &self.host {
            Some(host) => host.files.join(parent, name),
            None => crate::ssh::sftp::join(parent, name),
        }
    }

    fn parent<'a>(&self, path: &'a str) -> Option<&'a str> {
        match &self.host {
            Some(host) => host.files.parent(path),
            None => crate::ssh::sftp::parent(path),
        }
    }

    fn go_up(&mut self, cx: &mut Context<Self>) {
        // Cloned first: `parent` borrows from the path it is given, and that
        // borrow would otherwise still be alive across `navigate`'s `&mut`.
        let root = self.root.clone();
        let Some(parent) = root
            .as_deref()
            .and_then(|path| self.parent(path))
            .map(str::to_string)
        else {
            return;
        };
        self.navigate(parent, cx);
    }

    fn sync_path_field(&mut self, cx: &mut Context<Self>) {
        self.pending_path = Some(self.root.clone().unwrap_or_default());
        cx.notify();
    }

    fn load(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        self.nodes.insert(path.clone(), Node::Loading);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = host.files.read_dir(path.clone()).await;
            let _ = this.update(cx, |this, cx| {
                this.nodes.insert(
                    path,
                    match result {
                        Ok(entries) => Node::Loaded(entries),
                        Err(error) => Node::Failed(error.into()),
                    },
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// Reloads a directory only if it is already on screen.
    fn reload(&mut self, path: &str, cx: &mut Context<Self>) {
        if self.nodes.contains_key(path) {
            self.load(path.to_string(), cx);
        }
    }

    fn toggle(&mut self, path: String, cx: &mut Context<Self>) {
        if self.expanded.contains(&path) {
            self.expanded.remove(&path);
        } else {
            self.expanded.insert(path.clone());
            // Cached from a previous expand, so re-opening a folder is instant.
            if !self.nodes.contains_key(&path) {
                self.load(path, cx);
            }
        }
        cx.notify();
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let open: Vec<String> = self
            .expanded
            .iter()
            .cloned()
            .chain(self.root.clone())
            .collect();
        self.nodes.clear();
        for path in open {
            self.load(path, cx);
        }
        cx.notify();
    }

    fn begin_create(
        &mut self,
        parent: String,
        is_dir: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Creating inside a collapsed folder would put the new row somewhere you
        // can't see it, so open the folder first.
        if Some(&parent) != self.root.as_ref() {
            self.expanded.insert(parent.clone());
            if !self.nodes.contains_key(&parent) {
                self.load(parent.clone(), cx);
            }
        }
        self.menu = None;
        self.editing = Some(Editing::Create { parent, is_dir });
        self.start_editing(String::new(), window, cx);
    }

    fn begin_rename(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.editing = Some(Editing::Rename {
            path: target.path.clone(),
        });
        self.start_editing(target.name.clone(), window, cx);
    }

    fn start_editing(&mut self, initial: String, window: &mut Window, cx: &mut Context<Self>) {
        self.name_field.update(cx, |field, cx| {
            field.set_value(initial, window, cx);
            field.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let Some(host) = self.host.clone() else {
            return;
        };
        let name = self.name_field.read(cx).value().trim().to_string();
        if name.is_empty() {
            cx.notify();
            return;
        }

        let files = host.files.clone();
        let (directory, task) = match editing {
            Editing::Create { parent, is_dir } => {
                let path = files.join(&parent, &name);
                let task = cx.background_spawn(async move {
                    if is_dir {
                        files.create_dir(path).await
                    } else {
                        files.create_file(path).await
                    }
                });
                (parent, task)
            }
            Editing::Rename { path } => {
                let Some(parent) = files.parent(&path).map(str::to_string) else {
                    return;
                };
                let target = files.join(&parent, &name);
                let task = cx.background_spawn(async move { files.rename(path, target).await });
                (parent, task)
            }
        };

        self.after_change(directory, task, cx);
    }

    fn paste(&mut self, into: String, cx: &mut Context<Self>) {
        let (Some(clipboard), Some(host)) = (self.clipboard.clone(), self.host.clone())
        else {
            return;
        };
        self.menu = None;

        // Both separators: on Windows a local path's last '/' is nowhere, and
        // the whole absolute path went to `join` -- which pasted beside the
        // original instead of into the folder.
        let name = clipboard
            .path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("copy")
            .to_string();
        let files = host.files.clone();
        let directory = into.clone();

        let task = cx.background_spawn(async move {
            // Pasting back into the folder something was copied from would
            // collide, so find a free name the way a file manager does.
            let mut candidate = files.join(&into, &name);
            let mut suffix = 1;
            while files.exists(candidate.clone()).await {
                candidate = files.join(&into, &format!("{name} ({suffix})"));
                suffix += 1;
            }
            files.copy(clipboard.path, candidate, clipboard.is_dir).await
        });

        self.after_change(directory, task, cx);
    }

    /// Uploads local files into `directory`, asking the OS for them first.
    fn upload(&mut self, directory: String, cx: &mut Context<Self>) {
        self.menu = None;
        if self.host.is_none() {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Upload".into()),
        });

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.upload_paths(paths, directory, cx));
        })
        .detach();
        cx.notify();
    }

    /// Uploads already-chosen local paths -- the shared tail of the menu action
    /// and of a drop from the OS file manager.
    fn upload_paths(&mut self, paths: Vec<PathBuf>, directory: String, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        // Folders would need a recursive walk of the *source* tree, which is a
        // different job from the recursion in `Files::copy`.
        let chosen: Vec<PathBuf> = paths.into_iter().filter(|path| path.is_file()).collect();
        if chosen.is_empty() {
            self.error = Some("Only files can be transferred, not folders.".into());
            cx.notify();
            return;
        }

        // One file at a time, driven from here rather than a single background
        // task, so the panel can name the file it is on and hand each transfer
        // its own counter.
        let count = chosen.len();
        let local = host.files.is_local();
        let files = host.files.clone();
        let target = directory.clone();
        cx.spawn(async move |this, cx| {
            let mut failure = None;
            for (index, path) in chosen.into_iter().enumerate() {
                let Some(name) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                else {
                    continue;
                };

                let progress = Arc::new(Progress::default());
                let shown = this.update(cx, |this, cx| {
                    this.transfer = Some(Transfer {
                        verb: if local { "copying" } else { "uploading" },
                        name: name.clone().into(),
                        index: index + 1,
                        count,
                        progress: progress.clone(),
                        started: Instant::now(),
                    });
                    cx.notify();
                });
                if shown.is_err() {
                    return;
                }

                if let Err(error) = files
                    .upload(path, files.join(&target, &name), progress)
                    .await
                {
                    failure = Some(error);
                    break;
                }
            }

            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                if let Some(error) = failure {
                    this.error = Some(error.into());
                }
                this.reload(&directory, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Downloads one remote file, asking the OS where to put it.
    fn download(&mut self, target: Target, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(host) = self.host.clone() else {
            return;
        };
        let local = host.files.is_local();
        if target.is_dir {
            self.error = Some("Only files can be transferred, not folders.".into());
            cx.notify();
            return;
        }

        let directory = dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let destination = cx.prompt_for_new_path(&directory, Some(&target.name));

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(destination))) = destination.await else {
                return;
            };

            let progress = Arc::new(Progress::default());
            let shown = this.update(cx, |this, cx| {
                this.transfer = Some(Transfer {
                    verb: if local { "copying" } else { "downloading" },
                    name: target.name.clone().into(),
                    index: 1,
                    count: 1,
                    progress: progress.clone(),
                    started: Instant::now(),
                });
                cx.notify();
            });
            if shown.is_err() {
                return;
            }

            // Nothing on the remote side changed, so nothing is reloaded: this
            // only clears the line and surfaces an error.
            let result = host
                .files
                .download(target.path.clone(), destination, progress)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                if let Err(error) = result {
                    this.error = Some(error.into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn show_properties(&mut self, target: Target, cx: &mut Context<Self>) {
        self.menu = None;
        let Some(host) = self.host.clone() else {
            return;
        };
        self.properties = Some((target.clone(), None));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = host.files.properties(target.path.clone()).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(properties) => {
                        if let Some((current, slot)) = this.properties.as_mut() {
                            if current.path == target.path {
                                *slot = Some(properties);
                            }
                        }
                    }
                    Err(error) => {
                        this.properties = None;
                        this.error = Some(error.into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete(&mut self, target: Target, cx: &mut Context<Self>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        self.confirm_delete = None;
        let Some(parent) = self.parent(&target.path).map(str::to_string) else {
            return;
        };
        let files = host.files.clone();
        let task =
            cx.background_spawn(async move { files.remove(target.path, target.is_dir).await });
        self.after_change(parent, task, cx);
    }

    /// Waits for a mutation, then reloads the directory it happened in, so the
    /// tree reflects it without a manual refresh.
    fn after_change(
        &mut self,
        directory: String,
        task: Task<Result<(), String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.transfer = None;
                if let Err(error) = result {
                    this.error = Some(error.into());
                }
                this.reload(&directory, cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Flattens the expanded tree into the rows to draw, depth-first.
    /// Hidden files are filtered here, at render time, rather than when a
    /// directory is fetched -- so the setting toggles instantly, without
    /// re-reading anything from the server.
    fn rows(&self, show_hidden: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(root) = &self.root {
            self.push_children(root, 0, show_hidden, &mut rows);
        }
        rows
    }

    fn push_children(&self, path: &str, depth: usize, show_hidden: bool, rows: &mut Vec<Row>) {
        // A pending "new file/folder" row sits at the top of its directory,
        // where the thing being created will appear.
        if let Some(Editing::Create { parent, is_dir }) = &self.editing {
            if parent == path {
                rows.push(Row::editor(depth, *is_dir));
            }
        }

        match self.nodes.get(path) {
            Some(Node::Loaded(entries)) => {
                for entry in entries {
                    if !show_hidden && entry.name.starts_with('.') {
                        continue;
                    }
                    let child = self.join(path, &entry.name);
                    let renaming = matches!(
                        &self.editing,
                        Some(Editing::Rename { path }) if path == &child
                    );
                    if renaming {
                        rows.push(Row::editor(depth, entry.is_dir));
                        continue;
                    }

                    let expanded = entry.is_dir && self.expanded.contains(&child);
                    rows.push(Row {
                        path: child.clone(),
                        name: entry.name.clone(),
                        is_dir: entry.is_dir,
                        is_symlink: entry.is_symlink,
                        depth,
                        expanded,
                        kind: RowKind::Entry,
                    });
                    if expanded {
                        self.push_children(&child, depth + 1, show_hidden, rows);
                    }
                }
            }
            Some(Node::Loading) => rows.push(Row::note(depth, RowKind::Loading)),
            Some(Node::Failed(error)) => {
                rows.push(Row::note(depth, RowKind::Failed(error.clone())))
            }
            None => {}
        }
    }
}

#[derive(Clone)]
enum RowKind {
    Entry,
    Editor,
    Loading,
    Failed(SharedString),
}

#[derive(Clone)]
struct Row {
    path: String,
    name: String,
    is_dir: bool,
    is_symlink: bool,
    depth: usize,
    expanded: bool,
    kind: RowKind,
}

impl Row {
    fn note(depth: usize, kind: RowKind) -> Self {
        Self {
            path: String::new(),
            name: String::new(),
            is_dir: false,
            is_symlink: false,
            depth,
            expanded: false,
            kind,
        }
    }

    fn editor(depth: usize, is_dir: bool) -> Self {
        Self {
            is_dir,
            ..Self::note(depth, RowKind::Editor)
        }
    }
}

impl Focusable for ExplorerView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ExplorerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(path) = self.pending_path.take() {
            self.path_field
                .update(cx, |field, cx| field.set_value(path, window, cx));
        }

        // A transfer's byte count lives in a counter the SSH runtime bumps as
        // it copies; nothing notifies us when it moves, so while one is running
        // ask for the next frame and the figures keep ticking. It stops the
        // moment the transfer clears.
        if self.transfer.is_some() {
            window.request_animation_frame();
        }

        let (
            bg,
            border,
            fg,
            fg_muted,
            fg_subtle,
            accent,
            raised,
            danger,
            font_size,
            line_height,
        ) = {
            let t = cx.global::<Theme>();
            (
                t.bg,
                t.border,
                t.fg,
                t.fg_muted,
                t.fg_subtle,
                t.accent,
                t.raised,
                t.danger,
                t.font_size,
                t.line_height,
            )
        };

        let connected = self.host.is_some();
        let root = self.root.clone().unwrap_or_default();
        let has_clipboard = self.clipboard.is_some();
        let selected = self.selected.clone();
        let name_field = self.name_field.clone();

        let show_hidden = cx.global::<crate::store::Settings>().show_hidden;
        // Counted before the rows are turned into elements: the footer says
        // how many are in view, which with folders expanded is not the same
        // as how many are in the directory.
        let row_count = self.rows(show_hidden).len();
        let rows: Vec<_> = self
            .rows(show_hidden)
            .into_iter()
            .enumerate()
            .map(|(ix, row)| {
                let indent = px(6. + row.depth as f32 * 12.);

                match row.kind {
                    RowKind::Loading => div()
                        .pl(indent)
                        .h(line_height)
                        .flex()
                        .items_center()
                        .text_color(fg_subtle)
                        .child("loading…")
                        .into_any_element(),
                    RowKind::Failed(error) => div()
                        .pl(indent)
                        .h(line_height)
                        .flex()
                        .items_center()
                        .text_color(danger)
                        .child(error)
                        .into_any_element(),
                    RowKind::Editor => div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .w_full()
                        .pl(indent)
                        .pr_2()
                        .child(div().w(px(12.)).flex_none())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                // The input lets Escape propagate rather than
                                // raising an event, so it is caught out here.
                                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                    if event.keystroke.key == "escape" {
                                        this.editing = None;
                                        cx.notify();
                                    }
                                }))
                                .child(text_input(&name_field, font_size)),
                        )
                        .into_any_element(),
                    RowKind::Entry => {
                        let is_selected = selected.as_deref() == Some(row.path.as_str());

                        let for_chevron = row.path.clone();
                        let for_row = row.path.clone();
                        let for_menu = Target {
                            path: row.path.clone(),
                            name: row.name.clone(),
                            is_dir: row.is_dir,
                        };
                        let is_dir = row.is_dir;

                        div()
                            .id(("row", ix))
                            .flex()
                            .items_center()
                            .gap_1()
                            .w_full()
                            .h(line_height)
                            .pl(indent)
                            .pr_2()
                            // The design's selection: the raised ground and a
                            // 2px accent rule on the leading edge, the same mark
                            // a focused window header carries.
                            .when(is_selected, |this| {
                                this.bg(raised).border_l_2().border_color(accent)
                            })
                            .hover(|s| s.bg(raised.opacity(0.6)))
                            .text_color(if is_selected { fg } else { fg_muted })
                            // The chevron is the *only* thing that expands a
                            // folder in place. Clicking the row selects it;
                            // double-clicking navigates into it.
                            .child(
                                div()
                                    .id(("chevron", ix))
                                    .w(px(12.))
                                    .h_full()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    // `+` and `-`, as the design writes them:
                                    // characters on the grid, not chevrons.
                                    .text_color(fg_subtle)
                                    .children(row.is_dir.then(|| {
                                        if row.expanded { "-" } else { "+" }
                                    }))
                                    .when(row.is_dir, |this| {
                                        this.hover(|s| s.bg(raised)).on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                                this.toggle(for_chevron.clone(), cx);
                                                // Or the row handler underneath
                                                // would also select/navigate.
                                                cx.stop_propagation();
                                            }),
                                        )
                                    }),
                            )
                            // No file or folder icons: the expander already says
                            // which rows are folders, and a column of
                            // pictograms is noise beside monospace names.
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .when(row.is_dir && !is_selected, |this| this.text_color(fg))
                                    .when(row.is_symlink, |this| this.text_color(fg_subtle))
                                    .child(row.name.clone()),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.selected = Some(for_row.clone());
                                    this.menu = None;
                                    if event.click_count >= 2 {
                                        if is_dir {
                                            this.navigate(for_row.clone(), cx);
                                        } else {
                                            cx.emit(ExplorerEvent::OpenFile(for_row.clone()));
                                        }
                                    }
                                    cx.notify();
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.selected = Some(for_menu.path.clone());
                                    this.menu = Some(Menu {
                                        at: event.position,
                                        target: Some(for_menu.clone()),
                                    });
                                    cx.notify();
                                }),
                            )
                            .into_any_element()
                    }
                }
            })
            .collect();

        let menu = self
            .menu
            .clone()
            .map(|menu| self.render_menu(menu, has_clipboard, root.clone(), cx));
        let confirm = self.confirm_delete.clone();
        let properties = self.properties.clone();

        // A window, not a panel: it fills the column it is in, paints no
        // ground of its own (the column body already did, once -- a second
        // layer doubles a translucent window's opacity), and draws no border,
        // because the seam between columns belongs to the strip.
        div()
            .id("explorer")
            .track_focus(&self.focus_handle)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .min_h_0()
            .text_size(font_size)
            .text_color(fg_muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    // Exactly a window header's height, so the panel's top
                    // line runs level with the headers of the columns beside
                    // it rather than a few pixels off.
                    .h(crate::ui::kit::header(cx.global::<Theme>()))
                    .flex_none()
                    .border_b_1()
                    .border_color(border)
                    // The design's path line: a `PATH` label and the path as
                    // plain text. It is still an input -- type a path, press
                    // enter -- just drawn without a box.
                    .child(
                        div()
                            .pl_1()
                            .flex_none()
                            .text_size(crate::ui::kit::micro(cx.global::<Theme>()))
                            .text_color(fg_subtle)
                            .child(super::kit::label("path")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            // Escape puts the bar back to where we actually are.
                            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "escape" {
                                    this.sync_path_field(cx);
                                }
                            }))
                            .child(text_input(&self.path_field, font_size).appearance(false)),
                    )
                    // A shell in this directory, in a column of its own.
                    .child(
                        div()
                            .id("explorer-terminal")
                            .px_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .hover(|s| s.bg(raised))
                            .tooltip(|window, cx| {
                                gpui_component::tooltip::Tooltip::new("Open in terminal").build(window, cx)
                            })
                            .child(icon(Icon::SquareTerminal, px(13.), fg_subtle))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                if let Some(root) = this.root.clone() {
                                    cx.emit(ExplorerEvent::OpenTerminal(root));
                                }
                            })),
                    )
                    // Up a level and reload, as characters rather than icons.
                    // (Copy path lives in the context menu.)
                    .child(
                        div()
                            .id("explorer-up")
                            .px_1()
                            .text_color(fg_subtle)
                            .hover(|s| s.text_color(fg).bg(raised))
                            .child("\u{2191}")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.go_up(cx))),
                    )
                    .child(
                        div()
                            .id("explorer-refresh")
                            .px_1()
                            .text_color(fg_subtle)
                            .hover(|s| s.text_color(fg).bg(raised))
                            .child("\u{21bb}")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.refresh(cx))),
                    ),
            )
            // Directly under the header and outside the scroll, because a
            // transfer is the panel's state rather than a row in the tree.
            // It used to be the tree's first child, which scrolled it out of
            // sight the moment you looked anywhere in a long directory.
            .children(
                self.transfer
                    .as_ref()
                    .map(|transfer| render_transfer(transfer, fg, fg_subtle, accent, border)),
            )
            .child(
                div()
                    .id("explorer-tree")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(!connected, |this| {
                        this.child(
                            div()
                                .p_2()
                                .text_color(fg_subtle)
                                .child("connect to browse files"),
                        )
                    })
                    .when_some(self.error.clone(), |this, error| {
                        this.child(div().px_2().py_1().text_color(danger).child(error))
                    })
                    .children(rows)
                    // Files dragged in from the OS file manager upload into the
                    // directory currently in view.
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.bg(accent.opacity(0.15))
                    })
                    .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                        let Some(directory) = this.root.clone() else {
                            return;
                        };
                        this.upload_paths(paths.paths().to_vec(), directory, cx);
                    }))
                    // Right-clicking past the last row targets the directory
                    // itself, which is where "new file" is usually asked for.
                    .child(
                        div()
                            .id("explorer-empty")
                            .flex_1()
                            .min_h(px(40.))
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    this.menu = Some(Menu {
                                        at: event.position,
                                        target: None,
                                    });
                                    cx.notify();
                                }),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                    this.menu = None;
                                    cx.notify();
                                }),
                            ),
                    ),
            )
            // The window's own footer: how much is here on the left, and
            // which half of the app fetched it on the right. Both are things
            // you want while looking at a tree and nowhere else, which is why
            // they are on the window rather than in the navigation bar.
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .text_size(crate::ui::kit::micro(cx.global::<Theme>()))
                    .text_color(fg_subtle)
                    .child(crate::ui::kit::label(&match row_count {
                        1 => "1 item".to_string(),
                        n => format!("{n} items"),
                    }))
                    .child(crate::ui::kit::label(match &self.host {
                        Some(host) if host.files.is_local() => "local",
                        Some(_) => "sftp",
                        None => "\u{2014}",
                    })),
            )
            // No resize handle any more. The explorer is a column, and a
            // column is resized at its seam like every other -- one gesture
            // for the whole strip instead of a special one for this panel.
            .children(menu)
            .children(properties.map(|(target, details)| {
                render_properties(target, details, bg, border, fg, fg_muted, fg_subtle, raised, cx)
            }))
            .children(confirm.map(|target| {
                div()
                    .id("confirm-delete")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::black().opacity(0.5))
                    .block_mouse_except_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            // Sized to the panel rather than to its content: the
                            // panel can be as narrow as 180px, and a fixed-width
                            // card would spill straight out of it.
                            .w_full()
                            .min_w_0()
                            .mx_2()
                            .p_2()
                            .bg(bg)
                            .border_1()
                            .border_color(danger)
                            .child(div().text_color(fg).child(if target.is_dir {
                                "Delete this folder and everything in it?"
                            } else {
                                "Delete this file?"
                            }))
                            .child(
                                div()
                                    .min_w_0()
                                    .text_color(fg_muted)
                                    .child(target.name.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        div()
                                            .id("cancel-delete")
                                            .px_2()
                                            .py_0p5()
                                            .border_1()
                                            .border_color(border)
                                            .hover(|s| s.bg(raised))
                                            .child("cancel")
                                            .on_click(cx.listener(
                                                |this, _: &ClickEvent, _, cx| {
                                                    this.confirm_delete = None;
                                                    cx.notify();
                                                },
                                            )),
                                    )
                                    .child(
                                        div()
                                            .id("do-delete")
                                            .px_2()
                                            .py_0p5()
                                            .border_1()
                                            .border_color(danger)
                                            .text_color(danger)
                                            .hover(|s| s.bg(danger.opacity(0.2)))
                                            .child("delete")
                                            .on_click(cx.listener(
                                                move |this, _: &ClickEvent, _, cx| {
                                                    this.delete(target.clone(), cx);
                                                },
                                            )),
                                    ),
                            ),
                    )
            }))
    }
}

impl ExplorerView {
    fn render_menu(
        &self,
        menu: Menu,
        has_clipboard: bool,
        root: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Names the two transfer entries for the host in front of you.
        let local = self.is_local();
        let (bg, border, fg, fg_muted, fg_subtle, raised, danger, accent, row_h, micro) = {
            let t = cx.global::<Theme>();
            (
                // Opaque whatever the window opacity: a menu you can read the
                // desktop through is a menu you can't read.
                t.palette.bg,
                t.border,
                t.fg,
                t.fg_muted,
                t.fg_subtle,
                t.palette.raised,
                t.danger,
                t.accent,
                crate::ui::kit::row(t),
                crate::ui::kit::micro(t),
            )
        };
        let _ = fg_subtle;

        // Actions on a folder apply inside it; on a file, inside its parent.
        let directory = match &menu.target {
            Some(target) if target.is_dir => target.path.clone(),
            Some(target) => self.parent(&target.path)
                .map(str::to_string)
                .unwrap_or_else(|| root.clone()),
            None => root.clone(),
        };
        let target = menu.target.clone();

        // The design's list row, as everywhere else in the app: words only,
        // the row's own height, and on hover the raised ground with the 2px
        // accent rule on the leading edge -- the mark a selected file, a chosen
        // Settings section and a focused window header all carry.
        let item = move |id: &'static str, label: &'static str, _glyph: Icon, tint: gpui::Hsla| {
            div()
                .id(id)
                .group(id)
                .relative()
                .flex()
                .items_center()
                .h(row_h)
                .px_3()
                .text_color(tint)
                .hover(|s| s.bg(raised).text_color(fg))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .group_hover(id, move |s| s.bg(accent)),
                )
                .child(label)
        };
        // What the menu is about, as a tracked label over it.
        let heading = match &menu.target {
            Some(target) => format!(
                "{} \u{b7} {}",
                if target.is_dir { "folder" } else { "file" },
                target.name
            ),
            None => format!("here \u{b7} {directory}"),
        };

        // `deferred` paints after every ancestor, which is what lets the menu
        // escape the panel's bounds -- laid out inside a 280px column and a
        // scrolling tree, it was otherwise clipped on both axes. `anchored`
        // positions in *window* coordinates, so the raw mouse position is used
        // as-is, and `snap_to_window_with_margin` flips it back inside the
        // window when the click lands near an edge.
        deferred(
            anchored()
                .position(menu.at)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("explorer-menu")
                        .w(px(220.))
                        .pb_1()
                        .bg(bg)
                        .border_1()
                        .border_color(border)
                        .text_color(fg_muted)
                        // Keeps clicks from falling through to the tree beneath.
                        .occlude()
                        // A click anywhere else -- a terminal, the bar, another
                        // part of the tree -- puts the menu away, as a menu
                        // should; it used to stay up until the tree was clicked.
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            if this.menu.take().is_some() {
                                cx.notify();
                            }
                        }))
                        .child(
                            div()
                                .px_3()
                                .py(px(6.))
                                .mb_1()
                                .border_b_1()
                                .border_color(border)
                                .text_size(micro)
                                .text_color(fg_subtle)
                                .truncate()
                                .child(crate::ui::kit::label(&heading)),
                        )
                        .child({
                            let directory = directory.clone();
                            item("menu-new-file", "New File", Icon::FilePlus, fg_muted).on_click(
                                cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.begin_create(directory.clone(), false, window, cx)
                                }),
                            )
                        })
                        .child({
                            let directory = directory.clone();
                            item("menu-new-folder", "New Folder", Icon::FolderPlus, fg_muted)
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    this.begin_create(directory.clone(), true, window, cx)
                                }))
                        })
                        .when_some(target.clone(), |this, target| {
                            let for_rename = target.clone();
                            let for_copy = target.clone();
                            this.child(div().my_1().h(px(1.)).bg(border))
                                .child(
                                    item("menu-rename", "Rename", Icon::PenLine, fg_muted)
                                        .on_click(cx.listener(
                                            move |this, _: &ClickEvent, window, cx| {
                                                this.begin_rename(&for_rename, window, cx)
                                            },
                                        )),
                                )
                                .child(item("menu-copy", "Copy", Icon::Copy, fg_muted).on_click(
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.clipboard = Some(Clipboard {
                                            path: for_copy.path.clone(),
                                            is_dir: for_copy.is_dir,
                                        });
                                        this.menu = None;
                                        cx.notify();
                                    }),
                                ))
                        })
                        .child({
                let directory = directory.clone();
                item(
                    "menu-upload",
                    if local { "Copy Files In…" } else { "Upload Files…" },
                    Icon::Upload,
                    fg_muted,
                )
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.upload(directory.clone(), cx)
                    }),
                )
            })
            .when(has_clipboard, |this| {
                            let directory = directory.clone();
                            this.child(
                                item("menu-paste", "Paste", Icon::ClipboardPaste, fg_muted)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.paste(directory.clone(), cx)
                                    })),
                            )
                        })
                        .when_some(target, |this, target| {
                            let for_path = target.clone();
                            this.child(
                                item("menu-copy-path", "Copy Path", Icon::Link, fg_muted).on_click(
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            for_path.path.clone(),
                                        ));
                                        this.menu = None;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .when(crate::ui::preview::can_preview(&target.name), |this| {
                                let for_preview = target.clone();
                                this.child(
                                    item("menu-preview", "Preview", Icon::Eye, fg_muted).on_click(
                                        cx.listener(move |this, _: &ClickEvent, _, cx| {
                                            this.menu = None;
                                            cx.emit(ExplorerEvent::PreviewFile(
                                                for_preview.path.clone(),
                                            ));
                                            cx.notify();
                                        }),
                                    ),
                                )
                            })
                            .child({
                                let for_download = target.clone();
                                item(
                                    "menu-download",
                                    if local { "Copy Out…" } else { "Download…" },
                                    Icon::Download,
                                    fg_muted,
                                )
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.download(for_download.clone(), cx)
                                    }))
                            })
                            .child(div().my_1().h(px(1.)).bg(border))
                            .child({
                                let for_properties = target.clone();
                                item("menu-properties", "Properties", Icon::Info, fg_muted)
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.show_properties(for_properties.clone(), cx)
                                    }))
                            })
                            .child(
                                item("menu-delete", "Delete", Icon::Trash2, danger).on_click(
                                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                                        this.menu = None;
                                        this.confirm_delete = Some(target.clone());
                                        cx.notify();
                                    }),
                                ),
                            )
                        }),
                ),
        )
    }
}

/// The Properties panel: an overlay inside the explorer showing one path's
/// attributes.
#[allow(clippy::too_many_arguments)]
fn render_properties(
    target: Target,
    details: Option<Properties>,
    bg: gpui::Hsla,
    border: gpui::Hsla,
    fg: gpui::Hsla,
    fg_muted: gpui::Hsla,
    fg_subtle: gpui::Hsla,
    raised: gpui::Hsla,
    cx: &mut Context<ExplorerView>,
) -> impl IntoElement {
    let row = move |label: &'static str, value: String| {
        div()
            .flex()
            .gap_2()
            .child(
                div()
                    .w(px(72.))
                    .flex_none()
                    .text_color(fg_subtle)
                    .child(label),
            )
            .child(div().flex_1().min_w_0().text_color(fg_muted).child(value))
    };

    div()
        .id("properties")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(gpui::black().opacity(0.5))
        .block_mouse_except_scroll()
        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
            this.properties = None;
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .w_full()
                .min_w_0()
                .mx_2()
                .p_2()
                .bg(bg)
                .border_1()
                .border_color(border)
                // Clicks inside the card shouldn't dismiss it.
                .occlude()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .pb_1()
                        .border_b_1()
                        .border_color(border)
                        .child(icon(
                            if target.is_dir {
                                Icon::Folder
                            } else {
                                for_filename(&target.name)
                            },
                            small(),
                            fg_muted,
                        ))
                        .child(div().flex_1().min_w_0().text_color(fg).child(target.name.clone())),
                )
                .child(row("path", target.path.clone()))
                .when_some(details, |this, details| {
                    this.child(row(
                        "type",
                        if details.is_symlink {
                            "symlink".to_string()
                        } else if details.is_dir {
                            "folder".to_string()
                        } else {
                            "file".to_string()
                        },
                    ))
                    .when(!details.is_dir, |this| {
                        this.child(row("size", format_size(details.size)))
                    })
                    .child(row(
                        "modified",
                        details.modified.map(format_time).unwrap_or_else(unknown),
                    ))
                    .child(row(
                        "accessed",
                        details.accessed.map(format_time).unwrap_or_else(unknown),
                    ))
                    .child(row(
                        "owner",
                        format_owner(&details.user, details.uid, &details.group, details.gid),
                    ))
                    .child(row(
                        "mode",
                        details
                            .permissions
                            .map(format_permissions)
                            .unwrap_or_else(unknown),
                    ))
                })
                .child(
                    div().flex().justify_end().pt_1().child(
                        div()
                            .id("close-properties")
                            .px_2()
                            .py_0p5()
                            .border_1()
                            .border_color(border)
                            .hover(|s| s.bg(raised))
                            .child("close")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.properties = None;
                                cx.notify();
                            })),
                    ),
                ),
        )
}

/// The progress strip for a running transfer: what it is on the left, how far
/// along on the right, and a 2px bar under both.
///
/// The bar is the only thing that says the percentage, which is why the words
/// beside it do not. A rule fills the gap between the two so the eye has a
/// line to run along, the way the two ends of a contents page are joined.
fn render_transfer(
    transfer: &Transfer,
    fg: gpui::Hsla,
    fg_subtle: gpui::Hsla,
    accent: gpui::Hsla,
    border: gpui::Hsla,
) -> impl IntoElement {
    let done = transfer.progress.transferred();
    let total = transfer.progress.total();
    // Floored, so the first frames can't divide by ~0 and report a terabyte a
    // second.
    let seconds = transfer.started.elapsed().as_secs_f64().max(0.05);
    let speed = (done as f64 / seconds) as u64;

    // "3 OF 7" only while there are several; otherwise the bytes moved so far,
    // which is the one number a single file has that the bar doesn't already
    // draw. The word is upper case to match the rest of the small type, but
    // the unit is left alone: `MIB/S` would be a different unit from `MiB/s`,
    // and a size is not somewhere to be stylish.
    let measure = if transfer.count > 1 {
        format!("{} OF {}", transfer.index, transfer.count)
    } else {
        short_size(done)
    };
    let reading = format!("{measure} · {}/s", short_size(speed));

    // No position without a size: a server that won't stat the file leaves the
    // bar empty rather than inventing one, and the reading still carries bytes
    // and speed.
    let fraction = if total > 0 {
        (done as f32 / total as f32).clamp(0., 1.)
    } else {
        0.
    };

    div()
        .flex_none()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                // The name gives way, the reading does not: the panel is
                // narrow, and of the two it is the reading that is moving.
                // A truncated name is still the file you picked; a clipped
                // reading is a number you cannot read.
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(fg)
                        .child(format!("{} — {}", transfer.verb, transfer.name)),
                )
                // Nothing to join when the name has taken the room, so the
                // rule collapses to nothing of its own accord.
                .child(div().flex_1().min_w(px(0.)).h(px(1.)).bg(border))
                .child(div().flex_none().text_color(fg_subtle).child(reading)),
        )
        .child(
            div()
                .w_full()
                .h(px(2.))
                .bg(border)
                .child(div().h_full().w(gpui::relative(fraction)).bg(accent)),
        )
}

/// Compact binary size for the progress line (`4.2 MiB`). `format_size` below
/// is the Properties panel's, which also spells out the exact byte count.
fn short_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn unknown() -> String {
    "unknown".to_string()
}

/// Binary units, because that is what a file manager on a Unix host shows.
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {} ({bytes} B)", UNITS[unit])
    }
}

/// SFTP reports times as Unix seconds; shown in local time, since that is the
/// clock the person reading it is on.
fn format_time(seconds: u32) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_opt(seconds as i64, 0).single() {
        Some(time) => time.format("%Y-%m-%d %H:%M").to_string(),
        None => unknown(),
    }
}

fn format_owner(
    user: &Option<String>,
    uid: Option<u32>,
    group: &Option<String>,
    gid: Option<u32>,
) -> String {
    // Only the older SFTP protocol versions send names; newer ones send ids and
    // leave the lookup to the client, which has no passwd file to consult.
    let owner = user
        .clone()
        .or_else(|| uid.map(|uid| uid.to_string()))
        .unwrap_or_else(unknown);
    let group = group
        .clone()
        .or_else(|| gid.map(|gid| gid.to_string()))
        .unwrap_or_else(unknown);
    format!("{owner}:{group}")
}

/// Unix mode bits as `rwxr-xr-x`, the form anyone reading a remote path expects.
fn format_permissions(mode: u32) -> String {
    let bit = |shift: u32, flag: char| {
        if mode >> shift & 1 == 1 {
            flag
        } else {
            '-'
        }
    };
    let mut out = String::with_capacity(13);
    for offset in [6, 3, 0] {
        out.push(bit(offset + 2, 'r'));
        out.push(bit(offset + 1, 'w'));
        out.push(bit(offset, 'x'));
    }
    out.push_str(&format!(" ({:04o})", mode & 0o7777));
    out
}
