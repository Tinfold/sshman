//! Copying and moving files without leaving the filesystem they are on.
//!
//! Like the `tar` commands next door these are plain POSIX shell strings, so
//! the same builder serves both sides: locally through `sh -c`, remotely
//! through the connection. A paste therefore behaves identically whichever
//! pane it happens in, including under sudo.
//!
//! Nothing is ever overwritten. The command checks every destination name
//! first and stops with a message naming the one that is in the way, which is
//! the only sane default for a key that can be pressed by accident.

use crate::types::sh_quote;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Leave the originals where they are.
    Copy,
    /// Take them with us.
    Move,
}

impl Action {
    fn tool(self) -> &'static str {
        match self {
            // `-a` on a copy: a duplicate of a file is expected to carry the
            // original's mode, ownership and timestamps.
            Self::Copy => "cp -a",
            Self::Move => "mv",
        }
    }

    pub fn past_tense(self) -> &'static str {
        match self {
            Self::Copy => "copied",
            Self::Move => "moved",
        }
    }
}

/// Command that puts `names` (relative to `dir`) into `dest`.
///
/// It runs with `dir` as the working directory, so the names stay relative and
/// a directory full of oddly named files needs no special handling beyond the
/// quoting every name already gets.
pub fn paste_command(dir: &str, names: &[String], dest: &str, action: Action) -> String {
    let mut items = String::new();
    for name in names {
        items.push(' ');
        items.push_str(&sh_quote(name));
    }
    // The loop variable is expanded inside quotes and appended to the quoted
    // destination, so neither a space nor a `$` in either can escape.
    format!(
        "cd {dir} && for n in{items}; do \
         if [ -e {dest}/\"$n\" ]; then echo \"$n is already there\" >&2; exit 1; fi; \
         done && {tool} --{items} {dest}/",
        dir = sh_quote(dir),
        dest = sh_quote(dest),
        tool = action.tool(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_from_one_directory_into_another() {
        let cmd = paste_command(
            "/home/me",
            &["notes.txt".into(), "src".into()],
            "/tmp/out",
            Action::Copy,
        );
        assert!(cmd.starts_with("cd '/home/me' && "), "{cmd}");
        assert!(
            cmd.ends_with("cp -a -- 'notes.txt' 'src' '/tmp/out'/"),
            "{cmd}"
        );
    }

    #[test]
    fn moving_uses_mv_and_keeps_the_same_shape() {
        let cmd = paste_command("/a", &["one".into()], "/b", Action::Move);
        assert!(cmd.ends_with("mv -- 'one' '/b'/"), "{cmd}");
        assert!(cmd.contains("is already there"), "the guard is still there");
    }

    #[test]
    fn every_name_is_checked_before_anything_is_written() {
        let cmd = paste_command("/a", &["x".into(), "y".into()], "/b", Action::Copy);
        let guard = cmd.find("for n in").unwrap();
        let write = cmd.find("cp -a").unwrap();
        assert!(guard < write, "the check has to come first: {cmd}");
        assert!(cmd.contains("exit 1"), "and it has to stop the copy: {cmd}");
    }

    #[test]
    fn names_and_paths_that_would_run_as_shell_are_quoted() {
        let cmd = paste_command("/a b", &["; rm -rf /".into()], "/dest $HOME", Action::Copy);
        assert!(cmd.contains("'; rm -rf /'"), "{cmd}");
        assert!(cmd.contains("'/a b'"), "{cmd}");
        assert!(cmd.contains("'/dest $HOME'"), "{cmd}");
        // The one unquoted expansion is the loop variable, and it is in
        // double quotes so a space in a file name stays part of the name.
        assert!(cmd.contains("\"$n\""), "{cmd}");
    }
}
