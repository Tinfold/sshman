//! Saved sets of connections.
//!
//! A workspace is the answer to "the four things I always open together".
//! It records what each tab was connected to and which directory it was
//! showing, so reopening puts you back where you were rather than at four
//! home directories.
//!
//! Containers are stored by **name**, not by the id used while running: an id
//! changes the moment a container is recreated, and a workspace is meant to
//! outlive that.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::backend::Target;
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
    },
}

impl Item {
    /// Build the connection details this item describes.
    pub fn to_target(&self) -> Target {
        match self {
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
                    // thing we build.
                    Target::Docker { .. } => None,
                }),
                container: container.clone(),
                runtime: runtime.clone(),
            },
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Ssh { path, .. } | Self::Container { path, .. } => path.as_deref(),
        }
    }

    /// The ports this connection was carrying when it was saved.
    pub fn forwards(&self) -> &[String] {
        match self {
            Self::Ssh { forwards, .. } | Self::Container { forwards, .. } => forwards,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Ssh { name, .. } => name.as_deref(),
            Self::Container { .. } => None,
        }
    }

    /// One line describing this item, for the workspace list.
    pub fn describe(&self) -> String {
        match self {
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
        }
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
