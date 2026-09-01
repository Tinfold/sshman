//! Saved sets of connections.
//!
//! A workspace is the answer to "the four things I always open together".
//! It records what each tab was connected to, which directory it was showing
//! and the sizes its panes had, so reopening puts you back where you were
//! rather than at four home directories in four identical panes.
//!
//! Containers are stored by **name**, not by the id used while running: an id
//! changes the moment a container is recreated, and a workspace is meant to
//! outlive that.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::backend::Target;
use crate::layout::{Layout, TermId};
use crate::sshconn::ConnectOpts;

/// Where one pane was pointed, by the number the arrangement calls it.
///
/// The arrangement says where a pane was on screen; this says what it was
/// showing, which is the other half of putting a tab back the way it was.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PaneDir {
    pub id: u32,
    pub path: String,
}

/// The directories a tab's panes were in.
///
/// File lists and terminals are numbered separately, so they are listed
/// separately. The first file list is also written down as the tab's `path`,
/// which is what a workspace saved before this said and what everything that
/// means "this tab's directory" still reads.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneDirs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trees: Vec<PaneDir>,
    /// Where each terminal was when the workspace was saved — as well as that
    /// could be known. See [`crate::shell::Shell::cwd`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shells: Vec<PaneDir>,
}

impl PaneDirs {
    pub fn is_empty(&self) -> bool {
        self.trees.is_empty() && self.shells.is_empty()
    }

    /// The directory a file list was in, if it was written down.
    pub fn tree(&self, id: u32) -> Option<&str> {
        self.trees
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.path.as_str())
    }

    /// The same for a terminal.
    pub fn shell(&self, id: u32) -> Option<&str> {
        self.shells
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.path.as_str())
    }
}

/// A command a pane was told to run, by the number the arrangement calls it.
///
/// The arrangement says where the pane was; this says what it was doing.
/// Together they are the difference between a workspace that opens four
/// empty shells and one that opens the four things you actually watch.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PaneRun {
    pub id: TermId,
    pub cmd: String,
}

/// One connection inside a workspace.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Item {
    Ssh {
        user: String,
        host: String,
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        /// The directory this tab was showing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Ports this tab was forwarding, in the shorthand you typed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forwards: Vec<String>,
        /// The panes this tab was showing, terminals among them. Absent in
        /// workspaces saved before panes were remembered, which open at
        /// whatever is on screen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
        /// Which of those terminals were opening files rather than being
        /// typed in. The arrangement says where the terminals were; this says
        /// which of them was the editor.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        editors: Vec<TermId>,
        /// What any of those terminals were told to run, so that opening this
        /// again starts them rather than leaving you four blank prompts.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        runs: Vec<PaneRun>,
        /// What each of those panes was showing.
        #[serde(default, skip_serializing_if = "PaneDirs::is_empty")]
        dirs: PaneDirs,
    },
    /// A tab on the machine sshman is running on. There is nothing to
    /// reconnect, so its directory and panes are the whole of it.
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        editors: Vec<TermId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        runs: Vec<PaneRun>,
        #[serde(default, skip_serializing_if = "PaneDirs::is_empty")]
        dirs: PaneDirs,
    },
    Container {
        /// Container name, so it survives being recreated.
        container: String,
        runtime: String,
        /// The server whose runtime holds it; absent means this machine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<Box<Item>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        forwards: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        editors: Vec<TermId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        runs: Vec<PaneRun>,
        #[serde(default, skip_serializing_if = "PaneDirs::is_empty")]
        dirs: PaneDirs,
    },
}

impl Item {
    /// Build the connection details this item describes.
    pub fn to_target(&self) -> Target {
        match self {
            Self::Local { .. } => Target::Local,
            Self::Ssh {
                user,
                host,
                port,
                key_path,
                ..
            } => Target::Ssh(ConnectOpts {
                host: host.clone(),
                port: *port,
                user: user.clone(),
                password: None,
                key_path: key_path.as_ref().map(PathBuf::from),
                key_passphrase: None,
                accept_new_host_key: false,
                replace_host_key: false,
            }),
            Self::Container {
                container,
                runtime,
                via,
                ..
            } => Target::Docker {
                via: via.as_ref().and_then(|item| match item.to_target() {
                    Target::Ssh(opts) => Some(opts),
                    // A container reached through another container is not a
                    // thing we build, and one on this machine is reached with
                    // no server in the way at all.
                    Target::Docker { .. } | Target::Local => None,
                }),
                container: container.clone(),
                runtime: runtime.clone(),
            },
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Ssh { path, .. } | Self::Container { path, .. } | Self::Local { path, .. } => {
                path.as_deref()
            }
        }
    }

    /// The ports this connection was carrying when it was saved.
    pub fn forwards(&self) -> &[String] {
        match self {
            Self::Ssh { forwards, .. } | Self::Container { forwards, .. } => forwards,
            // Nothing to forward to: the tab is already where the ports are.
            Self::Local { .. } => &[],
        }
    }

    /// The panes to open this tab with, if they were written down. Read from
    /// a file, so they are made sense of before anyone draws with them.
    pub fn layout(&self) -> Option<Layout> {
        match self {
            Self::Ssh { layout, .. }
            | Self::Container { layout, .. }
            | Self::Local { layout, .. } => layout.clone().map(Layout::sane),
        }
    }

    /// What each of this tab's panes was showing. Empty for a workspace saved
    /// before panes said where they were, which opens them all in the tab's
    /// own directory the way it always did.
    pub fn dirs(&self) -> &PaneDirs {
        match self {
            Self::Ssh { dirs, .. } | Self::Container { dirs, .. } | Self::Local { dirs, .. } => {
                dirs
            }
        }
    }

    /// Which of this tab's terminals were opening files.
    pub fn editors(&self) -> &[TermId] {
        match self {
            Self::Ssh { editors, .. }
            | Self::Container { editors, .. }
            | Self::Local { editors, .. } => editors,
        }
    }

    /// What this tab's terminals were told to run. Empty for a workspace
    /// saved before panes could be given a command, which opens them as
    /// plain shells the way it always did.
    pub fn runs(&self) -> &[PaneRun] {
        match self {
            Self::Ssh { runs, .. } | Self::Container { runs, .. } | Self::Local { runs, .. } => {
                runs
            }
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Ssh { name, .. } | Self::Local { name, .. } => name.as_deref(),
            Self::Container { .. } => None,
        }
    }

    /// One line describing this item, for the workspace list.
    pub fn describe(&self) -> String {
        match self {
            Self::Local { name, .. } => match name {
                Some(name) if !name.trim().is_empty() => name.clone(),
                _ => "this machine".into(),
            },
            Self::Ssh {
                user,
                host,
                port,
                name,
                ..
            } => match name {
                Some(name) if !name.trim().is_empty() => name.clone(),
                _ if *port == 22 => format!("{user}@{host}"),
                _ => format!("{user}@{host}:{port}"),
            },
            Self::Container { container, via, .. } => match via.as_deref() {
                Some(Self::Ssh { host, .. }) => format!("{container} on {host}"),
                _ => container.clone(),
            },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Workspace {
    pub name: String,
    /// The local pane's directory when this was saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// Which of this machine's terminals were opening files. The panes they
    /// were in are in each tab's own arrangement, since the tabs are what
    /// decide which of this machine's panes they show.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_editors: Vec<TermId>,
    /// And what this machine's terminals were told to run. Shared between the
    /// tabs the same way the panes themselves are.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local_runs: Vec<PaneRun>,
    /// What this machine's panes were showing. Shared between the tabs the
    /// same way the panes themselves are, so it belongs to the workspace
    /// rather than to any one of them.
    #[serde(default, skip_serializing_if = "PaneDirs::is_empty")]
    pub local_dirs: PaneDirs,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub saved_at: i64,
}

impl Workspace {
    pub fn summary(&self) -> String {
        match self.items.len() {
            0 => "empty".into(),
            1 => "1 connection".into(),
            n => format!("{n} connections"),
        }
    }
}

/// What a workspace holding the last session is called, wherever one is
/// listed or reported.
pub const SESSION_NAME: &str = "previous session";

/// The session as it was last seen, kept so it can be opened again.
///
/// The same shape as a workspace — the same tabs, the same panes, the same
/// directories — but written down without being asked for. Quitting is not
/// the only way sshman stops, and a crash or a closed terminal gives no
/// chance to write anything, so this is not saved on the way out: it is kept
/// up to date as you work, and whatever is on disk when the process ends is
/// what comes back.
///
/// It lives in a file of its own rather than among the workspaces, so that a
/// list you curated is never rearranged by something you did not ask for.
pub struct Session;

impl Session {
    /// The session sshman was in last time, if there was one.
    pub fn load() -> Option<Workspace> {
        let text = std::fs::read_to_string(Self::path()?).ok()?;
        let mut workspace: Workspace = serde_json::from_str(&text).ok()?;
        workspace.name = SESSION_NAME.to_string();
        Some(workspace)
    }

    /// Write down where things stand. Called as the session changes rather
    /// than as it ends.
    pub fn save(workspace: &Workspace) -> std::io::Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(workspace)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Written beside then renamed, so a session ending mid-write cannot
        // leave half a file to come back to.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        restrict_permissions(&tmp);
        std::fs::rename(&tmp, path)
    }

    /// Throw away what was kept.
    pub fn forget() {
        if let Some(path) = Self::path() {
            let _ = std::fs::remove_file(path);
        }
    }

    fn path() -> Option<PathBuf> {
        Some(config_dir()?.join("session.json"))
    }
}

#[derive(Default)]
pub struct Workspaces {
    pub entries: Vec<Workspace>,
    path: Option<PathBuf>,
}

impl Workspaces {
    pub fn load() -> Self {
        let path = workspaces_path();
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<Vec<Workspace>>(&text).ok())
            .unwrap_or_default();
        let mut ws = Self { entries, path };
        ws.sort();
        ws
    }

    fn sort(&mut self) {
        self.entries
            .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, index: usize) -> Option<&Workspace> {
        self.entries.get(index)
    }

    pub fn find(&self, name: &str) -> Option<&Workspace> {
        let needle = name.trim().to_lowercase();
        self.entries
            .iter()
            .find(|w| w.name.to_lowercase() == needle)
    }

    /// Save a set of connections under its name, replacing any workspace
    /// already using it. Returns whether an existing one was replaced.
    ///
    /// Takes the whole thing rather than a field at a time, so that what a
    /// workspace is made of is written down once — in
    /// [`crate::app::App::snapshot`], which is also what the session is
    /// written from. Two lists of fields to keep in step is one list too
    /// many: the pane directories were nearly added to one and not the other,
    /// and the commands panes run would have been next.
    pub fn save(&mut self, workspace: Workspace) -> std::io::Result<bool> {
        let mut workspace = workspace;
        workspace.name = workspace.name.trim().to_string();
        workspace.saved_at = now();
        let name = workspace.name.clone();
        let replaced = match self
            .entries
            .iter_mut()
            .find(|w| w.name.to_lowercase() == name.to_lowercase())
        {
            Some(existing) => {
                *existing = workspace;
                true
            }
            None => {
                self.entries.push(workspace);
                false
            }
        };
        self.sort();
        self.write()?;
        Ok(replaced)
    }

    pub fn remove(&mut self, index: usize) -> Option<Workspace> {
        if index >= self.entries.len() {
            return None;
        }
        let removed = self.entries.remove(index);
        let _ = self.write();
        Some(removed)
    }

    fn write(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Written beside then renamed, so an interrupted write cannot leave a
        // truncated list behind.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        restrict_permissions(&tmp);
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn workspaces_path() -> Option<PathBuf> {
    Some(config_dir()?.join("workspaces.json"))
}

/// Where everything sshman remembers between sessions lives.
fn config_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("sshman"))
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

/// Seconds since the epoch, for saying when something was written down.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Workspaces {
        Workspaces {
            entries: Vec::new(),
            path: None, // nothing touches the real filesystem
        }
    }

    /// A workspace with nothing in it but a name and some connections.
    fn named(name: &str, local: Option<&str>, items: Vec<Item>) -> Workspace {
        Workspace {
            name: name.into(),
            local_path: local.map(String::from),
            local_editors: Vec::new(),
            local_runs: Vec::new(),
            local_dirs: PaneDirs::default(),
            items,
            saved_at: 0,
        }
    }

    fn ssh_item(host: &str, path: Option<&str>) -> Item {
        Item::Ssh {
            user: "me".into(),
            host: host.into(),
            port: 22,
            key_path: None,
            name: None,
            path: path.map(String::from),
            forwards: Vec::new(),
            layout: None,
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        }
    }

    #[test]
    fn a_tab_on_this_machine_is_saved_with_the_rest() {
        let item = Item::Local {
            path: Some("/var/log".into()),
            name: Some("logs".into()),
            layout: Some(Layout::sides(35)),
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        let text = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&text).unwrap();

        assert!(matches!(back.to_target(), Target::Local));
        assert_eq!(back.path(), Some("/var/log"));
        assert_eq!(back.name(), Some("logs"));
        assert_eq!(back.layout(), Some(Layout::sides(35)));
        assert_eq!(back.describe(), "logs");
        assert!(
            back.forwards().is_empty(),
            "there is no tunnel to a machine you are already on"
        );
    }

    #[test]
    fn an_unnamed_local_tab_describes_itself() {
        let item = Item::Local {
            path: None,
            name: None,
            layout: None,
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        assert_eq!(item.describe(), "this machine");
    }

    #[test]
    fn the_panes_a_tab_was_showing_survive_the_round_trip_through_json() {
        use crate::layout::{Dir, Side, Slot};
        let mut arranged = Layout::sides(70);
        arranged.split(
            Slot::files(Side::Remote),
            Dir::Down,
            Slot::term(Side::Remote, 1),
            60,
        );
        let item = Item::Ssh {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            key_path: None,
            name: None,
            path: Some("/etc".into()),
            forwards: Vec::new(),
            layout: Some(arranged.clone()),
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        let text = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&text).unwrap();
        assert_eq!(back.layout(), Some(arranged));
    }

    #[test]
    fn the_terminals_a_tab_had_are_part_of_what_is_saved() {
        use crate::layout::{Dir, Side, Slot};
        let mut arranged = Layout::default();
        let shell = Slot::term(Side::Remote, 2);
        arranged.split(Slot::files(Side::Remote), Dir::Down, shell, 70);

        let item = Item::Ssh {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            key_path: None,
            name: None,
            path: Some("/srv".into()),
            forwards: Vec::new(),
            layout: Some(arranged),
            editors: vec![2],
            runs: vec![PaneRun {
                id: 3,
                cmd: "tail -f /var/log/nginx/access.log".into(),
            }],
            dirs: PaneDirs::default(),
        };
        let back: Item = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();

        assert!(
            back.layout().expect("the panes").contains(shell),
            "the terminal's pane survives the round trip"
        );
        assert_eq!(back.editors(), [2], "and which of them opened files");
        // And what a pane was told to run, which is the other half of
        // opening a tab as you left it rather than as four blank prompts.
        let told = |id: TermId| back.runs().iter().find(|r| r.id == id);
        assert_eq!(
            told(3).map(|r| r.cmd.as_str()),
            Some("tail -f /var/log/nginx/access.log")
        );
        assert!(told(2).is_none(), "a pane you type in has nothing to run");
    }

    #[test]
    fn where_every_pane_was_pointed_survives_the_round_trip() {
        let item = Item::Ssh {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            key_path: None,
            name: None,
            path: Some("/srv".into()),
            forwards: Vec::new(),
            layout: None,
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs {
                trees: vec![
                    PaneDir {
                        id: 0,
                        path: "/srv".into(),
                    },
                    PaneDir {
                        id: 4,
                        path: "/var/log".into(),
                    },
                ],
                shells: vec![PaneDir {
                    id: 2,
                    path: "/etc/nginx".into(),
                }],
            },
        };
        let back: Item = serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
        assert_eq!(back.dirs().tree(0), Some("/srv"));
        assert_eq!(back.dirs().tree(4), Some("/var/log"));
        assert_eq!(back.dirs().shell(2), Some("/etc/nginx"));
        assert_eq!(back.dirs().tree(2), None, "the two are numbered apart");
        assert_eq!(back.dirs().shell(9), None);
    }

    #[test]
    fn a_workspace_from_before_panes_said_where_they_were_still_opens() {
        let text = r#"{"kind": "ssh", "user": "me", "host": "web01", "port": 22,
                       "path": "/srv"}"#;
        let item: Item = serde_json::from_str(text).expect("old items must still load");
        assert!(item.dirs().is_empty());
        assert_eq!(
            item.dirs().tree(0),
            None,
            "so every pane opens at the tab's own directory, as it always did"
        );
        assert_eq!(item.path(), Some("/srv"));
    }

    #[test]
    fn the_last_session_is_kept_apart_from_the_saved_ones() {
        // It is a workspace in every other way, so it must read as one.
        let session = Workspace {
            name: SESSION_NAME.into(),
            local_path: Some("/tmp".into()),
            local_editors: Vec::new(),
            local_runs: Vec::new(),
            local_dirs: PaneDirs {
                trees: vec![PaneDir {
                    id: 0,
                    path: "/tmp".into(),
                }],
                shells: Vec::new(),
            },
            items: vec![ssh_item("web01", Some("/etc"))],
            saved_at: now(),
        };
        let back: Workspace =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(back.name, SESSION_NAME);
        assert_eq!(back.items[0].path(), Some("/etc"));
        assert_eq!(back.local_dirs.tree(0), Some("/tmp"));
    }

    #[test]
    fn a_workspace_from_before_terminals_were_saved_still_opens() {
        // No editors listed at all, which is what every workspace written
        // before this says.
        let text = r#"{"kind": "ssh", "user": "me", "host": "web01", "port": 22}"#;
        let item: Item = serde_json::from_str(text).expect("old items must still load");
        assert!(item.editors().is_empty());
    }

    #[test]
    fn a_workspace_saved_before_sizes_were_remembered_still_opens() {
        let text = r#"[{
            "name": "prod",
            "local_path": "/tmp",
            "items": [{"kind": "ssh", "user": "me", "host": "web01", "port": 22}],
            "saved_at": 0
        }]"#;
        let list: Vec<Workspace> = serde_json::from_str(text).expect("old files must still load");
        assert_eq!(list[0].items.len(), 1);
        assert_eq!(
            list[0].items[0].layout(),
            None,
            "with no sizes to speak of, rather than a failure to parse"
        );
    }

    #[test]
    fn an_arrangement_that_would_hide_a_pane_is_brought_back_into_range() {
        let item = Item::Ssh {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            key_path: None,
            name: None,
            path: None,
            forwards: Vec::new(),
            // A hand-edited file, or one from a version that allowed more.
            layout: Some(serde_json::from_str(r#"{"split_pct": 99, "shell_height": 1}"#).unwrap()),
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        let got = item.layout().unwrap();
        let area = ratatui::layout::Rect::new(0, 0, 100, 30);
        for (_, rect) in got.areas(area).panes {
            assert!(rect.width >= 8, "{rect:?}");
        }
    }

    #[test]
    fn saving_then_finding_by_name() {
        let mut w = ws();
        w.save(named(
            "prod",
            Some("/tmp"),
            vec![ssh_item("web01", Some("/etc"))],
        ))
        .unwrap();
        let found = w.find("prod").expect("saved workspace");
        assert_eq!(found.items.len(), 1);
        assert_eq!(found.local_path.as_deref(), Some("/tmp"));
        assert_eq!(found.items[0].path(), Some("/etc"));
        // Looking up is case-insensitive; nobody remembers the capitals.
        assert!(w.find("PROD").is_some());
        assert!(w.find("  prod  ").is_some());
    }

    #[test]
    fn saving_the_same_name_replaces_rather_than_duplicates() {
        let mut w = ws();
        w.save(named("prod", None, vec![ssh_item("web01", None)]))
            .unwrap();
        let replaced = w
            .save(named(
                "prod",
                None,
                vec![ssh_item("web02", None), ssh_item("db", None)],
            ))
            .unwrap();
        assert!(replaced);
        assert_eq!(w.len(), 1);
        assert_eq!(w.find("prod").unwrap().items.len(), 2);
    }

    #[test]
    fn entries_are_listed_alphabetically() {
        let mut w = ws();
        for name in ["staging", "Alpha", "prod"] {
            w.save(named(name, None, vec![])).unwrap();
        }
        let names: Vec<&str> = w.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "prod", "staging"]);
    }

    #[test]
    fn an_ssh_item_rebuilds_its_connection() {
        let item = Item::Ssh {
            user: "deploy".into(),
            host: "web01".into(),
            port: 2222,
            key_path: Some("/home/me/.ssh/id_ed25519".into()),
            name: Some("production".into()),
            path: Some("/srv".into()),
            forwards: vec!["3000".into(), "8080:db:5432".into()],
            layout: None,
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        match item.to_target() {
            Target::Ssh(opts) => {
                assert_eq!(opts.user, "deploy");
                assert_eq!(opts.port, 2222);
                assert_eq!(
                    opts.key_path.unwrap().to_string_lossy(),
                    "/home/me/.ssh/id_ed25519"
                );
                assert!(opts.password.is_none(), "passwords are never stored");
                assert!(
                    !opts.accept_new_host_key,
                    "host-key grants are never stored"
                );
            }
            other => panic!("expected an SSH target, got {other:?}"),
        }
        assert_eq!(item.describe(), "production");
        assert_eq!(item.forwards(), ["3000", "8080:db:5432"]);
    }

    #[test]
    fn a_container_on_a_server_rebuilds_both_halves() {
        let item = Item::Container {
            container: "webapp".into(),
            runtime: "podman".into(),
            via: Some(Box::new(ssh_item("server1", None))),
            path: Some("/data".into()),
            forwards: Vec::new(),
            layout: None,
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        match item.to_target() {
            Target::Docker {
                via,
                container,
                runtime,
            } => {
                assert_eq!(container, "webapp");
                assert_eq!(runtime, "podman");
                assert_eq!(via.expect("the server").host, "server1");
            }
            other => panic!("expected a container target, got {other:?}"),
        }
        assert_eq!(item.describe(), "webapp on server1");
    }

    #[test]
    fn a_local_container_has_no_server() {
        let item = Item::Container {
            container: "db".into(),
            runtime: "docker".into(),
            via: None,
            path: None,
            forwards: Vec::new(),
            layout: None,
            editors: Vec::new(),
            runs: Vec::new(),
            dirs: PaneDirs::default(),
        };
        match item.to_target() {
            Target::Docker { via, .. } => assert!(via.is_none()),
            other => panic!("expected a container target, got {other:?}"),
        }
        assert_eq!(item.describe(), "db");
    }

    #[test]
    fn removing_is_bounds_checked() {
        let mut w = ws();
        w.save(named("only", None, vec![])).unwrap();
        assert!(w.remove(5).is_none());
        assert_eq!(w.remove(0).unwrap().name, "only");
        assert!(w.is_empty());
    }

    #[test]
    fn round_trips_through_json() {
        let items = vec![
            ssh_item("web01", Some("/etc")),
            Item::Container {
                container: "cache".into(),
                runtime: "docker".into(),
                via: Some(Box::new(ssh_item("web01", None))),
                path: Some("/data".into()),
                forwards: vec!["9000".into()],
                layout: None,
                editors: Vec::new(),
                runs: Vec::new(),
                dirs: PaneDirs::default(),
            },
        ];
        let json = serde_json::to_string(&items).unwrap();
        let back: Vec<Item> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, items);
    }

    #[test]
    fn summaries_read_naturally() {
        let mut w = ws();
        w.save(named("a", None, vec![])).unwrap();
        w.save(named("b", None, vec![ssh_item("x", None)])).unwrap();
        w.save(named(
            "c",
            None,
            vec![ssh_item("x", None), ssh_item("y", None)],
        ))
        .unwrap();
        assert_eq!(w.find("a").unwrap().summary(), "empty");
        assert_eq!(w.find("b").unwrap().summary(), "1 connection");
        assert_eq!(w.find("c").unwrap().summary(), "2 connections");
    }
}
