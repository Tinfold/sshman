//! Noticing that a directory has changed without being asked.
//!
//! A file list is a photograph of a directory taken when it was read, and
//! directories do not hold still: a build drops files in one, a shell in the
//! pane below deletes some, someone else's `mv` renames one. Having to press
//! `R` to find that out makes the list something you cannot trust without
//! checking, which is worse than a list that is simply a little behind.
//!
//! So the panes are watched, by the cheapest means that answers the question
//! on each side:
//!
//! * On this machine, by the directory's own timestamp. Adding, deleting and
//!   renaming all move it, and reading it is one `stat` — cheap enough to do
//!   several times a second and never notice.
//! * On a server, by asking for the listing and hashing it, since there is no
//!   `stat` that costs less than a round trip anyway. The worker answers with
//!   nothing at all when the hash matches, so an unchanged directory costs one
//!   message and no work in the UI.
//!
//! Neither side is a subscription, so a change is seen within a poll rather
//! than the instant it happens. That is the trade for having no watches to
//! set up, nothing to leak, and the same code path for a directory on this
//! machine and one three networks away.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Duration;

use crate::types::FileEntry;

/// How often the directories on this machine are looked at. Brisk, because
/// looking is a single `stat` of the directory itself.
pub const LOCAL: Duration = Duration::from_millis(500);

/// How often one of them is read in full instead, to catch a file that
/// changed without the directory around it changing — a log being written to,
/// say, which moves its size but nothing about the directory holding it.
pub const LOCAL_DEEP: Duration = Duration::from_secs(3);

/// The most entries a pane will be re-read in full for. Past this, the
/// directory's timestamp is the only thing watched: a full read costs a
/// `stat` per file, and no size column is worth a stutter in a directory of
/// a hundred thousand files. Changes to the directory itself are still seen
/// as promptly as anywhere else.
pub const DEEP_LIMIT: usize = 2000;

/// How often the server is asked whether the directory on screen still looks
/// the way it did. A round trip, so it is asked at a walking pace, and only
/// about the tab you are actually looking at.
pub const REMOTE: Duration = Duration::from_secs(4);

/// A directory's own timestamp and size: what changes when an entry is added,
/// removed or renamed.
pub type Stamp = (i64, i64, u64);

/// Stamp a directory, or `None` when it cannot be stat'd — which is itself a
/// change worth noticing, since it means the directory on screen has gone.
pub fn stamp(dir: &Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(dir).ok()?;
    Some((meta.mtime(), meta.mtime_nsec(), meta.size()))
}

/// A cheap stand-in for "this listing would look exactly the same".
///
/// Every field the pane draws goes in, so a size that grew or a mode that
/// changed counts as a change even though the names are the same. Both sides
/// of any comparison are hashed in the same process, which is all this has to
/// be true of.
pub fn signature(entries: &[FileEntry]) -> u64 {
    let mut hasher = DefaultHasher::new();
    entries.len().hash(&mut hasher);
    for entry in entries {
        entry.name.hash(&mut hasher);
        (entry.kind as u8).hash(&mut hasher);
        entry.size.hash(&mut hasher);
        entry.mtime.hash(&mut hasher);
        entry.perms.hash(&mut hasher);
        entry.link_target.hash(&mut hasher);
        entry.points_to_dir.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EntryKind;

    fn entry(name: &str, size: u64) -> FileEntry {
        FileEntry {
            name: name.into(),
            kind: EntryKind::File,
            size,
            mtime: 1,
            perms: "-rw-r--r--".into(),
            link_target: None,
            points_to_dir: false,
        }
    }

    #[test]
    fn the_same_listing_hashes_the_same_way() {
        let one = vec![entry("a", 1), entry("b", 2)];
        let two = vec![entry("a", 1), entry("b", 2)];
        assert_eq!(signature(&one), signature(&two));
    }

    #[test]
    fn a_file_that_grew_is_a_change() {
        let before = vec![entry("log", 10)];
        let after = vec![entry("log", 11)];
        assert_ne!(signature(&before), signature(&after));
    }

    #[test]
    fn adding_deleting_and_renaming_are_all_changes() {
        let before = vec![entry("a", 1)];
        assert_ne!(signature(&before), signature(&[]), "deleted");
        assert_ne!(
            signature(&before),
            signature(&[entry("a", 1), entry("b", 1)]),
            "added"
        );
        assert_ne!(signature(&before), signature(&[entry("c", 1)]), "renamed");
    }

    #[test]
    fn a_directory_that_is_not_there_has_no_stamp() {
        assert!(stamp(Path::new("/definitely/not/here")).is_none());
    }

    #[test]
    fn a_new_file_moves_the_directory_stamp() {
        let dir = std::env::temp_dir().join(format!("sshman-watch-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();

        let before = stamp(&dir).expect("the directory is there");
        // Filesystems with coarse timestamps need the write to land in a
        // later tick for this to be visible at all.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(dir.join("new"), b"x").unwrap();
        assert_ne!(before, stamp(&dir).expect("still there"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
