//! Shared value types: directory entries and the small formatting helpers the
//! UI uses to render them.

use chrono::{Local, TimeZone};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

/// One row in a file pane. Deliberately flat and owned so it can be shipped
/// across the worker channel without borrowing anything.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: i64,
    pub perms: String,
    pub link_target: Option<String>,
    /// True when this is a symlink that resolves to a directory, so `Enter`
    /// can descend into it.
    pub points_to_dir: bool,
}

impl FileEntry {
    pub fn is_dir_like(&self) -> bool {
        self.kind == EntryKind::Dir || (self.kind == EntryKind::Symlink && self.points_to_dir)
    }

    pub fn is_hidden(&self) -> bool {
        self.name.starts_with('.')
    }
}

/// Sort directories first, then case-insensitively by name.
pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir_like()
            .cmp(&a.is_dir_like())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    if n < 1024 {
        return format!("{n}B");
    }
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{v:.0}{}", UNITS[i])
    } else {
        format!("{v:.1}{}", UNITS[i])
    }
}

pub fn fmt_time(epoch: i64) -> String {
    match Local.timestamp_opt(epoch, 0).single() {
        Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        None => "-".repeat(16),
    }
}

/// Render a POSIX mode word as `drwxr-xr-x`.
pub fn perm_string(mode: u32) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        0o140000 => 's',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 0o4 != 0 { 'r' } else { '-' });
        s.push(if bits & 0o2 != 0 { 'w' } else { '-' });
        s.push(if bits & 0o1 != 0 { 'x' } else { '-' });
    }
    // setuid / setgid / sticky overlay the execute column
    let special = (mode >> 9) & 0o7;
    if special & 0o4 != 0 {
        s.replace_range(3..4, if mode & 0o100 != 0 { "s" } else { "S" });
    }
    if special & 0o2 != 0 {
        s.replace_range(6..7, if mode & 0o010 != 0 { "s" } else { "S" });
    }
    if special & 0o1 != 0 {
        s.replace_range(9..10, if mode & 0o001 != 0 { "t" } else { "T" });
    }
    s
}

pub fn kind_from_mode(mode: u32) -> EntryKind {
    match mode & 0o170000 {
        0o040000 => EntryKind::Dir,
        0o100000 => EntryKind::File,
        0o120000 => EntryKind::Symlink,
        0 => EntryKind::File, // some servers omit the type bits entirely
        _ => EntryKind::Other,
    }
}

// ---- remote (POSIX) path helpers -------------------------------------------
// Remote paths are always POSIX strings; we never route them through PathBuf so
// that behaviour stays identical regardless of the machine running the client.

pub fn rjoin(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        return name.to_string();
    }
    if base == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

pub fn rparent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => trimmed[..i].to_string(),
    }
}

/// A path broken into the pieces it is made of, each with the directory that
/// piece names: `/etc/nginx` is `/` then `etc` then `nginx`, leading to `/`,
/// `/etc` and `/etc/nginx`.
///
/// This is what makes the path in a pane's title a trail you can click your
/// way back along. Local paths are POSIX here as well — sshman runs on
/// systems where they are — so one function does for both sides.
pub fn crumbs(path: &str) -> Vec<(String, String)> {
    let trimmed = path.trim_end_matches('/');
    // The root is a piece of every absolute path, and the whole of one path.
    let mut out = match path.starts_with('/') {
        true => vec![("/".to_string(), "/".to_string())],
        false => Vec::new(),
    };
    let mut so_far = String::new();
    for name in trimmed.split('/').filter(|p| !p.is_empty()) {
        so_far = rjoin(&so_far, name);
        out.push((name.to_string(), so_far.clone()));
    }
    out
}

pub fn rbasename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Wrap a string so a POSIX shell sees it as one literal argument.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Shorten in the middle so both the start and the end stay readable.
pub fn ellipsize(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let keep = max - 1;
    let tail = keep / 2;
    let head = keep - tail;
    let chars: Vec<char> = s.chars().collect();
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[len - tail..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_breaks_into_the_directories_it_names() {
        assert_eq!(
            crumbs("/etc/nginx/sites"),
            [
                ("/", "/"),
                ("etc", "/etc"),
                ("nginx", "/etc/nginx"),
                ("sites", "/etc/nginx/sites"),
            ]
            .map(|(a, b)| (a.to_string(), b.to_string()))
        );
    }

    #[test]
    fn the_root_is_a_path_of_one_piece() {
        assert_eq!(crumbs("/"), [("/".to_string(), "/".to_string())]);
        assert!(crumbs("").is_empty());
    }

    #[test]
    fn a_trailing_slash_does_not_add_a_piece() {
        assert_eq!(crumbs("/etc/"), crumbs("/etc"));
        assert_eq!(crumbs("//etc//nginx"), crumbs("/etc/nginx"));
    }

    #[test]
    fn every_piece_leads_where_it_says() {
        for (name, path) in crumbs("/home/me/work/src") {
            assert!(
                path.ends_with(&name) || name == "/",
                "{name} leads to {path}"
            );
        }
        let deep = crumbs("/home/me/work/src");
        assert_eq!(deep.last().unwrap().1, "/home/me/work/src");
        assert_eq!(rparent(&deep.last().unwrap().1), deep[deep.len() - 2].1);
    }

    #[test]
    fn remote_paths_join_and_split() {
        assert_eq!(rjoin("/etc", "hosts"), "/etc/hosts");
        assert_eq!(rjoin("/", "etc"), "/etc");
        assert_eq!(rjoin("/etc/", "hosts"), "/etc/hosts");
        assert_eq!(rjoin("/etc", "/absolute"), "/absolute");

        assert_eq!(rparent("/etc/nginx/nginx.conf"), "/etc/nginx");
        assert_eq!(rparent("/etc"), "/");
        assert_eq!(rparent("/"), "/");

        assert_eq!(rbasename("/etc/nginx/"), "nginx");
        assert_eq!(rbasename("/etc/hosts"), "hosts");
        assert_eq!(rbasename("/"), "");
    }

    #[test]
    fn quoting_survives_single_quotes() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        // A name crafted to break out of quoting must stay one argument.
        assert_eq!(sh_quote("'; rm -rf /; '"), r"''\''; rm -rf /; '\'''");
    }

    #[test]
    fn permissions_render_like_ls() {
        assert_eq!(perm_string(0o040755), "drwxr-xr-x");
        assert_eq!(perm_string(0o100644), "-rw-r--r--");
        assert_eq!(perm_string(0o120777), "lrwxrwxrwx");
        assert_eq!(perm_string(0o104755), "-rwsr-xr-x", "setuid");
        assert_eq!(perm_string(0o041777), "drwxrwxrwt", "sticky bit");
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(512), "512B");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1536), "1.5K");
        assert_eq!(human_size(1024 * 1024 * 300), "300M");
    }

    #[test]
    fn long_names_shrink_from_the_middle() {
        assert_eq!(ellipsize("short", 10), "short");
        let out = ellipsize("averylongfilename.tar.gz", 12);
        assert_eq!(out.chars().count(), 12);
        assert!(out.starts_with("avery") && out.ends_with(".gz"));
    }

    #[test]
    fn directories_sort_before_files() {
        let mk = |name: &str, kind: EntryKind| FileEntry {
            name: name.into(),
            kind,
            size: 0,
            mtime: 0,
            perms: String::new(),
            link_target: None,
            points_to_dir: false,
        };
        let mut v = vec![
            mk("zeta.txt", EntryKind::File),
            mk("Beta", EntryKind::Dir),
            mk("alpha.txt", EntryKind::File),
            mk("apps", EntryKind::Dir),
        ];
        sort_entries(&mut v);
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["apps", "Beta", "alpha.txt", "zeta.txt"]);
    }
}
