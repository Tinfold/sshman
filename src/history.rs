//! Remembers servers you have successfully connected to, so they can be
//! picked from a list instead of retyped.
//!
//! Only what is needed to reconnect is stored — user, host, port and which key
//! file was used. **Passwords are never written to disk.**

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::sshconn::ConnectOpts;

/// Beyond this the list stops being a convenience, so the oldest entries fall
/// off the end.
const MAX_ENTRIES: usize = 50;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub user: String,
    pub host: String,
    pub port: u16,
    /// A name you gave this server. Shown instead of `user@host` once set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// Unix seconds of the last successful connection.
    #[serde(default)]
    pub last_connected: i64,
    #[serde(default)]
    pub connections: u32,
}

impl Entry {
    /// What to call this server: your name for it, or its address.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) if !name.trim().is_empty() => name.clone(),
            _ => self.address(),
        }
    }

    /// The address, always — shown alongside a custom name so it stays
    /// possible to tell two similarly named servers apart.
    pub fn address(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }

    pub fn has_name(&self) -> bool {
        self.name.as_ref().is_some_and(|n| !n.trim().is_empty())
    }

    /// Same server? The key file is deliberately not part of the identity, so
    /// switching keys updates the entry instead of duplicating it.
    fn matches(&self, other: &Self) -> bool {
        self.user == other.user && self.host == other.host && self.port == other.port
    }

    pub fn to_opts(&self) -> ConnectOpts {
        ConnectOpts {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password: None,
            key_path: self.key_path.as_ref().map(PathBuf::from),
            key_passphrase: None,
            accept_new_host_key: false,
            replace_host_key: false,
        }
    }
}

#[derive(Default)]
pub struct History {
    pub entries: Vec<Entry>,
    /// `None` when we could not work out where the home directory is; the
    /// history then works for the session but is not persisted.
    path: Option<PathBuf>,
}

impl History {
    /// Read the saved list. A missing or corrupt file is not an error worth
    /// bothering the user about — you simply start with an empty list.
    pub fn load() -> Self {
        let path = history_path();
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str::<Vec<Entry>>(&text).ok())
            .unwrap_or_default();
        let mut history = Self { entries, path };
        history.sort();
        history
    }

    fn sort(&mut self) {
        self.entries
            .sort_by(|a, b| b.last_connected.cmp(&a.last_connected));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// Record a successful connection, moving it to the top of the list.
    /// Returns an error only if it could not be written; the in-memory list is
    /// updated either way.
    /// Record a successful connection. `name` replaces any stored name when
    /// given, and leaves it alone when not — so connecting without retyping a
    /// name does not quietly erase it.
    pub fn record(&mut self, opts: &ConnectOpts, name: Option<String>) -> std::io::Result<()> {
        let entry = Entry {
            user: opts.user.clone(),
            host: opts.host.clone(),
            port: opts.port,
            name: name.filter(|n| !n.trim().is_empty()),
            key_path: opts
                .key_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            last_connected: now(),
            connections: 1,
        };
        match self.entries.iter_mut().find(|e| e.matches(&entry)) {
            Some(existing) => {
                existing.connections = existing.connections.saturating_add(1);
                existing.last_connected = entry.last_connected;
                existing.key_path = entry.key_path;
                if entry.name.is_some() {
                    existing.name = entry.name;
                }
            }
            None => self.entries.push(entry),
        }
        self.sort();
        self.entries.truncate(MAX_ENTRIES);
        self.save()
    }

    /// Give a saved server a name, or clear it with an empty string.
    pub fn rename(&mut self, index: usize, name: &str) -> Option<&Entry> {
        let entry = self.entries.get_mut(index)?;
        entry.name = Some(name.trim().to_string()).filter(|n| !n.is_empty());
        let _ = self.save();
        self.entries.get(index)
    }

    /// The name stored for a server, if it has one.
    pub fn name_for(&self, opts: &ConnectOpts) -> Option<String> {
        self.entries
            .iter()
            .find(|e| e.user == opts.user && e.host == opts.host && e.port == opts.port)
            .and_then(|e| e.name.clone())
    }

    pub fn remove(&mut self, index: usize) -> Option<Entry> {
        if index >= self.entries.len() {
            return None;
        }
        let removed = self.entries.remove(index);
        let _ = self.save();
        Some(removed)
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Write to a sibling then rename, so an interrupted write cannot leave
        // a truncated list behind.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        restrict_permissions(&tmp);
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn history_path() -> Option<PathBuf> {
    // Terminal tools live in ~/.config on both Linux and macOS, whatever
    // `dirs::config_dir()` says about Application Support.
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("sshman").join("hosts.json"))
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) {}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "just now", "3 hours ago", "12 Mar" — short enough for a list column.
pub fn relative_time(epoch: i64) -> String {
    if epoch <= 0 {
        return "never".into();
    }
    let delta = now() - epoch;
    match delta {
        d if d < 0 => "just now".into(),
        d if d < 60 => "just now".into(),
        d if d < 3600 => format!("{}m ago", d / 60),
        d if d < 86_400 => format!("{}h ago", d / 3600),
        d if d < 7 * 86_400 => format!("{}d ago", d / 86_400),
        _ => chrono::DateTime::from_timestamp(epoch, 0)
            .map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%-d %b")
                    .to_string()
            })
            .unwrap_or_else(|| "long ago".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history_of(entries: Vec<Entry>) -> History {
        let mut h = History {
            entries,
            path: None, // no path: nothing touches the real filesystem
        };
        h.sort();
        h
    }

    fn entry(user: &str, host: &str, port: u16, last: i64) -> Entry {
        Entry {
            user: user.into(),
            host: host.into(),
            port,
            name: None,
            key_path: None,
            last_connected: last,
            connections: 1,
        }
    }

    fn opts(user: &str, host: &str, port: u16) -> ConnectOpts {
        ConnectOpts {
            host: host.into(),
            port,
            user: user.into(),
            ..Default::default()
        }
    }

    #[test]
    fn most_recent_first() {
        let h = history_of(vec![
            entry("a", "old", 22, 100),
            entry("b", "new", 22, 900),
            entry("c", "mid", 22, 500),
        ]);
        let hosts: Vec<&str> = h.entries.iter().map(|e| e.host.as_str()).collect();
        assert_eq!(hosts, ["new", "mid", "old"]);
    }

    #[test]
    fn reconnecting_updates_instead_of_duplicating() {
        let mut h = history_of(vec![entry("me", "web01", 22, 100)]);
        h.record(&opts("me", "web01", 22), None).unwrap();
        assert_eq!(h.len(), 1, "same server must not be listed twice");
        assert_eq!(h.entries[0].connections, 2);
        assert!(h.entries[0].last_connected > 100);
    }

    #[test]
    fn a_different_port_is_a_different_server() {
        let mut h = history_of(vec![entry("me", "web01", 22, 100)]);
        h.record(&opts("me", "web01", 2222), None).unwrap();
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn newly_recorded_server_goes_to_the_top() {
        let mut h = history_of(vec![entry("me", "old", 22, 100)]);
        h.record(&opts("me", "fresh", 22), None).unwrap();
        assert_eq!(h.entries[0].host, "fresh");
    }

    #[test]
    fn passwords_are_never_persisted() {
        let mut h = history_of(vec![]);
        let mut o = opts("me", "web01", 22);
        o.password = Some("hunter2".into());
        h.record(&o, None).unwrap();
        let json = serde_json::to_string(&h.entries).unwrap();
        assert!(!json.contains("hunter2"), "serialised history: {json}");
        assert!(h.entries[0].to_opts().password.is_none());
    }

    #[test]
    fn removing_an_entry_is_bounds_checked() {
        let mut h = history_of(vec![entry("me", "a", 22, 1)]);
        assert!(h.remove(5).is_none());
        assert_eq!(h.remove(0).unwrap().host, "a");
        assert!(h.is_empty());
    }

    #[test]
    fn labels_hide_the_default_port() {
        assert_eq!(entry("me", "web01", 22, 0).label(), "me@web01");
        assert_eq!(entry("me", "web01", 2222, 0).label(), "me@web01:2222");
    }

    #[test]
    fn a_name_replaces_the_address_in_the_label() {
        let mut h = history_of(vec![entry("me", "web01", 22, 1)]);
        h.rename(0, "production web");
        assert_eq!(h.entries[0].label(), "production web");
        assert_eq!(
            h.entries[0].address(),
            "me@web01",
            "the address is still there"
        );
        assert!(h.entries[0].has_name());
    }

    #[test]
    fn an_empty_name_clears_it() {
        let mut h = history_of(vec![entry("me", "web01", 22, 1)]);
        h.rename(0, "temporary");
        h.rename(0, "   ");
        assert!(!h.entries[0].has_name());
        assert_eq!(h.entries[0].label(), "me@web01");
    }

    #[test]
    fn reconnecting_without_a_name_keeps_the_one_you_gave() {
        let mut h = history_of(vec![entry("me", "web01", 22, 1)]);
        h.rename(0, "production web");
        h.record(&opts("me", "web01", 22), None).unwrap();
        assert_eq!(
            h.entries[0].label(),
            "production web",
            "a plain reconnect must not wipe the name"
        );
        h.record(&opts("me", "web01", 22), Some("renamed".into()))
            .unwrap();
        assert_eq!(h.entries[0].label(), "renamed");
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(relative_time(0), "never");
        assert_eq!(relative_time(now() - 30), "just now");
        assert_eq!(relative_time(now() - 3 * 3600), "3h ago");
        assert_eq!(relative_time(now() - 2 * 86_400), "2d ago");
    }
}
