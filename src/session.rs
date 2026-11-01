//! What the workspace looked like last time, in `session.json` in the platform
//! data directory (`%APPDATA%/PulseTerm` on Windows, `~/.local/share/PulseTerm`
//! on Linux).
//!
//! Closing the app used to lose the arrangement as well as the connection:
//! which files were open, how the panes were split, where the explorer was
//! pointed. None of that depends on a live SSH session, so none of it has to
//! die with one -- this records the *shape* of the workspace and the target it
//! belonged to, and `Workspace::restore_session` rebuilds it after a connect
//! that matches.
//!
//! **No secret is here either** (see `store::SavedAuth`): the target is host,
//! port and username, enough to know the layout belongs to this machine and to
//! prefill the connect form. Reconnecting still costs a password.

use serde::{Deserialize, Serialize};

use crate::ssh::ConnectParams;
use crate::store::{read_json, write_json};

const FILE: &str = "session.json";

/// One saved tab.
///
/// A terminal carries nothing. An SSH PTY has no state this side of the wire:
/// the remote shell died with the connection, taking its working directory,
/// its history and whatever it was running, so restoring one honestly means
/// opening a fresh shell in the same slot.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SavedItem {
    #[serde(rename = "terminal")]
    Terminal,
    #[serde(rename = "editor")]
    Editor { path: String },
    #[serde(rename = "preview")]
    Preview { path: String },
    #[serde(rename = "settings")]
    Settings,
    /// The file tree, and the directory it was looking at. A window like any
    /// other since the redesign, so it is saved like one rather than in three
    /// fields of its own beside the layout.
    #[serde(rename = "files")]
    Files { path: Option<String> },
}

/// The layout. A strip is written as one horizontal `Axis` of `Pane`s, which
/// is exactly what a strip is, so a file from a build that still had the split
/// tree still loads: `Workspace::flatten_member` reads a nest of any shape as
/// a flat list of columns in reading order.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SavedMember {
    #[serde(rename = "pane")]
    Pane {
        items: Vec<SavedItem>,
        /// Which tab was in front.
        #[serde(default)]
        active: usize,
    },
    #[serde(rename = "axis")]
    Axis {
        /// `true` for a vertical stack, `false` for a row. A bool rather than
        /// gpui's `Axis`, which isn't ours to derive `Serialize` on.
        vertical: bool,
        members: Vec<SavedMember>,
        /// Dead: the divider positions of the old split tree, as flex weights.
        /// Kept so a file written by one of those builds still parses, and
        /// written empty. Weights are not shares, which is why the strip could
        /// not reuse the field -- 1.0 meant "an equal share of this axis"
        /// there and means "the whole viewport" here.
        #[serde(default)]
        flexes: Vec<f32>,
        /// Each column's width as a share of the viewport. Ignored unless
        /// there is one per member, so a hand-edited file cannot produce a
        /// column whose width came from somewhere else in the list.
        #[serde(default)]
        widths: Vec<f32>,
    },
}

/// Which machine the layout belongs to. A layout is only restored onto the same
/// target: the paths in it are that machine's, and reopening another host's
/// files under them would be wrong at best.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum SavedTarget {
    #[serde(rename = "ssh")]
    Ssh {
        host: String,
        port: u16,
        username: String,
    },
    /// This machine. It needs no more than saying so: there is one of it, and
    /// nothing to authenticate against, which is why a local session can be
    /// resumed at launch without asking for anything.
    #[serde(rename = "local")]
    Local,
}

impl SavedTarget {
    pub fn ssh(params: &ConnectParams) -> Self {
        SavedTarget::Ssh {
            host: params.host.clone(),
            port: params.port,
            username: params.username.clone(),
        }
    }

}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// `None` when the last state was disconnected, which is also what makes a
    /// clean exit leave nothing to resume.
    pub target: Option<SavedTarget>,
    pub layout: Option<SavedMember>,
    /// The docked file tree. `None` in a file from before it was docked --
    /// those carried it as a `SavedItem::Files` window in the layout, which a
    /// restore still reads as "docked, at this path".
    pub explorer: Option<SavedExplorer>,
    /// When the file was last written -- i.e. when the session it holds was
    /// closed -- for the home screen's "closed 01:14". Not in the file: it is
    /// the file's own modified time, read alongside it.
    #[serde(skip)]
    pub saved_at: Option<std::time::SystemTime>,
}

/// The file tree's place beside the strip: whether it was open, where it was
/// looking, and how wide it was dragged.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Debug)]
#[serde(default)]
pub struct SavedExplorer {
    pub open: bool,
    pub path: Option<String>,
    pub width: Option<f32>,
}

impl Session {
    pub fn load() -> Self {
        let mut session: Self = read_json(FILE);
        session.saved_at = std::fs::metadata(crate::store::data_dir().join(FILE))
            .and_then(|meta| meta.modified())
            .ok();
        session
    }

    pub fn save(&self) {
        write_json(FILE, self);
    }

    /// Whether this saved layout is the one to restore onto `target`.
    pub fn matches(&self, target: &SavedTarget) -> bool {
        self.layout.is_some() && self.target.as_ref() == Some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file is the contract between two runs of the app, so the shape of it
    /// is worth pinning: a hand-trimmed or older file has to keep loading.
    #[test]
    fn survives_a_round_trip() {
        let session = Session {
            target: Some(SavedTarget::Ssh {
                host: "example.test".into(),
                port: 22,
                username: "me".into(),
            }),
            layout: Some(SavedMember::Axis {
                vertical: false,
                members: vec![
                    SavedMember::Pane {
                        items: vec![
                            SavedItem::Terminal,
                            SavedItem::Editor {
                                path: "/etc/hosts".into(),
                            },
                        ],
                        active: 1,
                    },
                    SavedMember::Pane {
                        items: vec![
                            SavedItem::Preview {
                                path: "/srv/readme.md".into(),
                            },
                            // How a file from when the tree was a window
                            // carried it. Still has to load: a restore reads
                            // it as "docked, at this path".
                            SavedItem::Files {
                                path: Some("/srv".into()),
                            },
                        ],
                        active: 0,
                    },
                ],
                flexes: Vec::new(),
                widths: vec![1. / 3., 2. / 3.],
            }),
            explorer: Some(SavedExplorer {
                open: true,
                path: Some("/srv".into()),
                width: Some(312.),
            }),
            saved_at: None,
        };

        let json = serde_json::to_string(&session).expect("serialises");
        let back: Session = serde_json::from_str(&json).expect("deserialises");

        assert_eq!(back.target, session.target);
        assert_eq!(back.explorer, session.explorer);
        let Some(SavedMember::Axis {
            vertical,
            members,
            widths,
            ..
        }) = back.layout
        else {
            panic!("the root should still be an axis");
        };
        assert!(!vertical);
        assert_eq!(widths, vec![1. / 3., 2. / 3.]);
        assert_eq!(members.len(), 2);
        let SavedMember::Pane { items, active } = &members[0] else {
            panic!("the first member should still be a pane");
        };
        assert_eq!(*active, 1);
        assert!(matches!(items[0], SavedItem::Terminal));
        assert!(matches!(&items[1], SavedItem::Editor { path } if path == "/etc/hosts"));
        let SavedMember::Pane { items, .. } = &members[1] else {
            panic!("the second member should still be a pane");
        };
        assert!(matches!(&items[1], SavedItem::Files { path } if path.as_deref() == Some("/srv")));
    }

    /// Every field has a default, which is what lets a file written by an older
    /// build -- or one trimmed by hand -- still load instead of being discarded.
    #[test]
    fn loads_a_file_missing_everything() {
        let back: Session = serde_json::from_str("{}").expect("an empty object loads");
        assert!(back.target.is_none());
        assert!(back.layout.is_none());
    }

    /// A layout without a target is not restorable: there is nothing to say
    /// which machine those paths belong to. Nor is one from another host --
    /// and local is a target like any other, never a wildcard.
    #[test]
    fn a_layout_needs_its_own_target_to_match() {
        let params = ConnectParams {
            host: "example.test".into(),
            port: 22,
            username: "me".into(),
            auth: crate::ssh::AuthMethod::Password(String::new()),
        };
        let target = SavedTarget::ssh(&params);
        let layout = Some(SavedMember::Pane {
            items: vec![SavedItem::Terminal],
            active: 0,
        });

        let orphan = Session {
            layout: layout.clone(),
            ..Session::default()
        };
        assert!(!orphan.matches(&target));

        let elsewhere = Session {
            target: Some(SavedTarget::Ssh {
                host: "other.test".into(),
                port: 22,
                username: "me".into(),
            }),
            layout: layout.clone(),
            ..Session::default()
        };
        assert!(!elsewhere.matches(&target));

        let local = Session {
            target: Some(SavedTarget::Local),
            layout: layout.clone(),
            ..Session::default()
        };
        assert!(!local.matches(&target));
        assert!(local.matches(&SavedTarget::Local));

        let mine = Session {
            target: Some(target.clone()),
            layout,
            ..Session::default()
        };
        assert!(mine.matches(&target));
    }

    /// A file written before the tree was docked has no `explorer` field at
    /// all, and must still load rather than fail the whole parse.
    #[test]
    fn a_file_without_the_docked_tree_still_loads() {
        let json = r#"{"target":{"kind":"local"},"layout":{"kind":"pane","items":[{"kind":"terminal"}],"active":0}}"#;
        let back: Session = serde_json::from_str(json).expect("deserialises");
        assert!(back.explorer.is_none());
        assert!(back.layout.is_some());
    }
}
