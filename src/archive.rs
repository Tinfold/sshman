//! Building `tar` command lines.
//!
//! The same commands run locally through `sh -c` and remotely through the SSH
//! channel, so they are plain POSIX shell strings rather than argument lists.
//! Everything that comes from a file name is shell-quoted.
//!
//! Compression is chosen from the file name and passed as an explicit flag
//! rather than relying on `tar -a`, which GNU tar has and older BSD tar does
//! not. Extraction needs no flag at all: both tars detect compression from the
//! file itself.

use crate::types::sh_quote;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
    Xz,
}

impl Compression {
    /// The single-letter `tar` flag that creates this format.
    fn create_flag(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Gzip => "z",
            Self::Bzip2 => "j",
            Self::Xz => "J",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::None => "uncompressed",
            Self::Gzip => "gzip",
            Self::Bzip2 => "bzip2",
            Self::Xz => "xz",
        }
    }
}

/// Which archive format a name implies, or `None` when it is not one we build.
pub fn format_of(name: &str) -> Option<Compression> {
    let lower = name.to_lowercase();
    // Longest suffixes first: `.tar.gz` must win over `.gz`.
    for (suffix, compression) in [
        (".tar.gz", Compression::Gzip),
        (".tar.bz2", Compression::Bzip2),
        (".tar.xz", Compression::Xz),
        (".tgz", Compression::Gzip),
        (".tbz", Compression::Bzip2),
        (".tbz2", Compression::Bzip2),
        (".txz", Compression::Xz),
        (".tar", Compression::None),
    ] {
        if lower.ends_with(suffix) {
            return Some(compression);
        }
    }
    None
}

/// True when this looks like something `tar` can open.
pub fn is_archive(name: &str) -> bool {
    format_of(name).is_some()
}

/// The name without its archive suffix — the natural directory to unpack into.
pub fn stem_of(name: &str) -> String {
    let lower = name.to_lowercase();
    for suffix in [
        ".tar.gz", ".tar.bz2", ".tar.xz", ".tgz", ".tbz2", ".tbz", ".txz", ".tar",
    ] {
        if lower.ends_with(suffix) {
            return name[..name.len() - suffix.len()].to_string();
        }
    }
    name.to_string()
}

/// Suggest an archive name for a selection.
pub fn suggested_name(items: &[String], dir_name: &str) -> String {
    let base = match items {
        [only] => stem_of(only),
        _ if !dir_name.is_empty() => dir_name.to_string(),
        _ => "archive".to_string(),
    };
    format!("{base}.tar.gz")
}

/// Where the `tar` will run, which decides what flags it understands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tar {
    /// Whatever `tar` this machine has.
    Local,
    /// The server's `tar`, which we must assume is GNU tar.
    Remote,
}

/// Command that packs `names` (relative to `dir`) into `archive`.
///
/// It runs with `dir` as the working directory so the archive holds relative
/// paths — absolute ones would unpack into the wrong place and make tar
/// complain about stripping the leading slash.
///
/// On macOS, bsdtar stores extended attributes as AppleDouble members, which
/// GNU tar on the far end happily unpacks as literal `._name` files beside
/// every real one. `--no-mac-metadata` stops that; it is a bsdtar flag, so it
/// is only ever passed to the local tar, and only on macOS.
pub fn create_command(dir: &str, archive: &str, names: &[String], tar: Tar) -> String {
    let compression = format_of(archive).unwrap_or(Compression::Gzip);
    let mac_flag = if tar == Tar::Local && cfg!(target_os = "macos") {
        " --no-mac-metadata"
    } else {
        ""
    };
    let mut cmd = format!(
        "cd {} && tar{} -c{}f {} --",
        sh_quote(dir),
        mac_flag,
        compression.create_flag(),
        sh_quote(archive)
    );
    for name in names {
        cmd.push(' ');
        cmd.push_str(&sh_quote(name));
    }
    cmd
}

/// Command that unpacks `archive` (in `dir`) into `dest`.
///
/// `dest` is created first: unpacking into a directory of its own is what
/// stops an archive without a single top-level folder from scattering files
/// across the pane.
pub fn extract_command(dir: &str, archive: &str, dest: &str) -> String {
    format!(
        "cd {} && mkdir -p {} && tar -xf {} -C {}",
        sh_quote(dir),
        sh_quote(dest),
        sh_quote(archive),
        sh_quote(dest)
    )
}

/// Command that lists what an archive holds, for a look before unpacking.
pub fn list_command(dir: &str, archive: &str) -> String {
    format!("cd {} && tar -tvf {}", sh_quote(dir), sh_quote(archive))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_tar_family() {
        assert_eq!(format_of("backup.tar.gz"), Some(Compression::Gzip));
        assert_eq!(format_of("backup.tgz"), Some(Compression::Gzip));
        assert_eq!(format_of("backup.TAR.GZ"), Some(Compression::Gzip));
        assert_eq!(format_of("backup.tar.bz2"), Some(Compression::Bzip2));
        assert_eq!(format_of("backup.tar.xz"), Some(Compression::Xz));
        assert_eq!(format_of("backup.tar"), Some(Compression::None));
        assert_eq!(format_of("notes.txt"), None);
        assert_eq!(format_of("archive.zip"), None, "not something tar creates");
        // A bare .gz is a single compressed file, not a tar archive.
        assert_eq!(format_of("data.gz"), None);
    }

    #[test]
    fn stem_drops_the_whole_suffix() {
        assert_eq!(stem_of("backup.tar.gz"), "backup");
        assert_eq!(stem_of("backup.tgz"), "backup");
        assert_eq!(stem_of("my.data.tar.xz"), "my.data");
        assert_eq!(stem_of("plain"), "plain");
    }

    #[test]
    fn suggests_a_name_from_the_selection() {
        assert_eq!(suggested_name(&["src".into()], "project"), "src.tar.gz");
        assert_eq!(
            suggested_name(&["a".into(), "b".into()], "project"),
            "project.tar.gz",
            "several items are named after the directory holding them"
        );
        // Re-archiving an archive should not stack suffixes.
        assert_eq!(suggested_name(&["old.tar.gz".into()], "d"), "old.tar.gz");
    }

    #[test]
    fn create_picks_the_flag_from_the_name() {
        let one = ["a".to_string()];
        assert!(create_command("/tmp", "out.tar.gz", &one, Tar::Remote).contains("-czf"));
        assert!(create_command("/tmp", "out.tar", &one, Tar::Remote).contains("tar -cf"));
        assert!(create_command("/tmp", "o.tar.bz2", &one, Tar::Remote).contains("-cjf"));
        assert!(create_command("/tmp", "o.tar.xz", &one, Tar::Remote).contains("-cJf"));
    }

    #[test]
    fn mac_metadata_is_suppressed_locally_only() {
        let one = ["a".to_string()];
        let remote = create_command("/tmp", "o.tar.gz", &one, Tar::Remote);
        assert!(
            !remote.contains("--no-mac-metadata"),
            "GNU tar on the server would reject it: {remote}"
        );
        let local = create_command("/tmp", "o.tar.gz", &one, Tar::Local);
        assert_eq!(
            local.contains("--no-mac-metadata"),
            cfg!(target_os = "macos"),
            "only bsdtar has this flag: {local}"
        );
    }

    #[test]
    fn commands_quote_every_name() {
        let cmd = create_command(
            "/tmp/my dir",
            "it's.tar.gz",
            &["a b".into(), "c'd".into()],
            Tar::Remote,
        );
        assert!(cmd.contains("cd '/tmp/my dir'"), "{cmd}");
        assert!(cmd.contains(r"'it'\''s.tar.gz'"), "{cmd}");
        assert!(cmd.contains("'a b'"), "{cmd}");
        assert!(cmd.contains(r"'c'\''d'"), "{cmd}");
        // `--` keeps a name starting with a dash from being read as a flag.
        assert!(cmd.contains(" -- "), "{cmd}");
    }

    #[test]
    fn extract_creates_its_destination() {
        let cmd = extract_command("/srv", "app.tar.gz", "app");
        assert!(cmd.contains("mkdir -p 'app'"), "{cmd}");
        assert!(cmd.contains("tar -xf 'app.tar.gz' -C 'app'"), "{cmd}");
        // No compression flag: both GNU and BSD tar detect it from the file.
        assert!(!cmd.contains("-xzf"), "{cmd}");
    }
}
