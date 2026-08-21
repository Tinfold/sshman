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
use crate::layout::Layout;
use crate::sshconn::ConnectOpts;

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
        /// The pane sizes this tab was showing. Absent in workspaces saved
        /// before sizes were remembered, which open at whatever is on screen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
    },
    /// A tab on the machine sshman is running on. There is nothing to
    /// reconnect, so its directory and sizes are the whole of it.
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        layout: Option<Layout>,
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

    /// The pane sizes to open this tab with, if they were written down.
    /// Read from a file, so they are clamped before anyone draws with them.
    pub fn layout(&self) -> Option<Layout> {
        match self {
            Self::Ssh { layout, .. }
            | Self::Container { layout, .. }
            | Self::Local { layout, .. } => layout.map(Layout::sane),
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

    /// Save a set of connections under a name, replacing any workspace already
    /// using it. Returns whether an existing one was replaced.
    pub fn save(
        &mut self,
        name: &str,
        local_path: Option<String>,
        items: Vec<Item>,
    ) -> std::io::Result<bool> {
        let name = name.trim().to_string();
        let workspace = Workspace {
            name: name.clone(),
            local_path,
            items,
            saved_at: now(),
        };
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
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("sshman").join("workspaces.json"))
}

#[cfg(unix)]
fn restrict_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &std::path::Path) {}

fn now() -> i64 {
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
        }
    }

    #[test]
    fn a_tab_on_this_machine_is_saved_with_the_rest() {
        let item = Item::Local {
            path: Some("/var/log".into()),
            name: Some("logs".into()),
            layout: Some(Layout {
                split_pct: 35,
                shell_height: 14,
            }),
        };
        let text = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&text).unwrap();

        assert!(matches!(back.to_target(), Target::Local));
        assert_eq!(back.path(), Some("/var/log"));
        assert_eq!(back.name(), Some("logs"));
        assert_eq!(back.layout().unwrap().split_pct, 35);
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
        };
        assert_eq!(item.describe(), "this machine");
    }

    #[test]
    fn pane_sizes_survive_the_round_trip_through_json() {
        let item = Item::Ssh {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            key_path: None,
            name: None,
            path: Some("/etc".into()),
            forwards: Vec::new(),
            layout: Some(Layout {
                split_pct: 70,
                shell_height: 20,
            }),
        };
        let text = serde_json::to_string(&item).unwrap();
        let back: Item = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.layout(),
            Some(Layout {
                split_pct: 70,
                shell_height: 20,
            })
        );
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
    fn sizes_that_would_hide_a_pane_are_brought_back_into_range() {
        let item = Item::Ssh {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            key_path: None,
            name: None,
            path: None,
            forwards: Vec::new(),
            // A hand-edited file, or one from a version that allowed more.
            layout: Some(Layout {
                split_pct: 99,
                shell_height: 1,
            }),
        };
        let got = item.layout().unwrap();
        assert!(got.split_pct <= 80 && got.shell_height >= 3, "{got:?}");
    }

    #[test]
    fn saving_then_finding_by_name() {
        let mut w = ws();
        w.save(
            "prod",
            Some("/tmp".into()),
            vec![ssh_item("web01", Some("/etc"))],
        )
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
        w.save("prod", None, vec![ssh_item("web01", None)]).unwrap();
        let replaced = w
            .save(
                "prod",
                None,
                vec![ssh_item("web02", None), ssh_item("db", None)],
            )
            .unwrap();
        assert!(replaced);
        assert_eq!(w.len(), 1);
        assert_eq!(w.find("prod").unwrap().items.len(), 2);
    }

    #[test]
    fn entries_are_listed_alphabetically() {
        let mut w = ws();
        for name in ["staging", "Alpha", "prod"] {
            w.save(name, None, vec![]).unwrap();
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
        w.save("only", None, vec![]).unwrap();
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
            },
        ];
        let json = serde_json::to_string(&items).unwrap();
        let back: Vec<Item> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, items);
    }

    #[test]
    fn summaries_read_naturally() {
        let mut w = ws();
        w.save("a", None, vec![]).unwrap();
        w.save("b", None, vec![ssh_item("x", None)]).unwrap();
        w.save("c", None, vec![ssh_item("x", None), ssh_item("y", None)])
            .unwrap();
        assert_eq!(w.find("a").unwrap().summary(), "empty");
        assert_eq!(w.find("b").unwrap().summary(), "1 connection");
        assert_eq!(w.find("c").unwrap().summary(), "2 connections");
    }
}
