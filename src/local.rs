//! This machine: the filesystem under the local pane, and the connection a
//! tab uses when it is pointed here rather than at a server.
//!
//! The plain functions run on the UI thread — they are fast enough that
//! blocking is imperceptible, unlike anything that crosses the network.
//! [`LocalConn`] is the other half: it answers the same questions a server
//! does, so a tab on this machine browses, copies, packs and elevates through
//! exactly the code every other tab uses.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::types::{EntryKind, FileEntry, kind_from_mode, perm_string, sh_quote, sort_entries};

pub fn list_dir(path: &Path) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    let rd = fs::read_dir(path).with_context(|| format!("cannot read {}", path.display()))?;
    for item in rd {
        let item = match item {
            Ok(i) => i,
            Err(_) => continue,
        };
        let name = item.file_name().to_string_lossy().to_string();
        // lstat: we want to show symlinks as symlinks, not as their targets.
        let meta = match fs::symlink_metadata(item.path()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mode = meta.permissions().mode();
        let kind = kind_from_mode(mode);
        let (link_target, points_to_dir) = if kind == EntryKind::Symlink {
            let target = fs::read_link(item.path())
                .ok()
                .map(|p| p.to_string_lossy().to_string());
            let resolves_to_dir = fs::metadata(item.path())
                .map(|m| m.is_dir())
                .unwrap_or(false);
            (target, resolves_to_dir)
        } else {
            (None, false)
        };
        out.push(FileEntry {
            name,
            kind,
            size: meta.len(),
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            perms: perm_string(mode),
            link_target,
            points_to_dir,
        });
    }
    sort_entries(&mut out);
    Ok(out)
}

pub fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir(path).with_context(|| format!("mkdir {}", path.display()))
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    fs::rename(from, to).with_context(|| format!("rename {}", from.display()))
}

pub fn remove(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if meta.is_dir() {
        fs::remove_dir_all(path).with_context(|| format!("rm -r {}", path.display()))
    } else {
        fs::remove_file(path).with_context(|| format!("rm {}", path.display()))
    }
}

/// Total byte count of a file or directory tree, used to size progress bars.
pub fn tree_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if !meta.is_dir() {
        return meta.len();
    }
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(path) {
        for item in rd.flatten() {
            total += tree_size(&item.path());
        }
    }
    total
}

/// Expand a leading `~` and make the path absolute, but do not resolve
/// symlinks — users expect the path they typed.
pub fn expand(input: &str) -> PathBuf {
    let expanded = if input == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    } else if let Some(rest) = input.strip_prefix("~/") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/"))
            .join(rest)
    } else {
        PathBuf::from(input)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(expanded)
    }
}

pub fn set_mode(path: &Path, mode: u32) {
    // Best-effort: a failure here should never abort a transfer.
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777));
}

/// A tab pointed at this machine.
///
/// There is nothing to connect to and nothing to drop, so the interesting
/// part is elevation: `sudo -S` with the password on stdin, the same bargain
/// the SSH side makes, which is what lets a local tab list and write the
/// places your own account cannot reach.
pub struct LocalConn {
    pub user: String,
    pub host: String,
    pub home: String,
    /// Set once sudo mode has been turned on and the password proved.
    sudo_password: Option<String>,
}

impl LocalConn {
    pub fn open() -> Self {
        let home = dirs::home_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "/".into());
        Self {
            user: whoami(),
            host: hostname(),
            home,
            sudo_password: None,
        }
    }

    /// Run a command through the shell, as root when asked.
    ///
    /// The password goes in on stdin and never onto a command line, where
    /// every other process on the machine could read it.
    pub fn run(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let (program, args) = argv(&shell, cmd, elevated);
        let mut command = Command::new(program);
        command.args(args);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run {shell}"))?;
        // Taken either way, so it is closed: a command reading stdin must see
        // the end of it rather than waiting for input that is never coming.
        if let Some(mut stdin) = child.stdin.take()
            && elevated
            && let Some(secret) = &self.sudo_password
        {
            let _ = writeln!(stdin, "{secret}");
        }
        let out = child.wait_with_output().context("waiting for the shell")?;
        Ok((
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            out.status.code().unwrap_or(-1),
        ))
    }

    fn must(&self, cmd: &str, elevated: bool) -> Result<()> {
        let (out, err, code) = self.run(cmd, elevated)?;
        if code != 0 {
            let text = if err.trim().is_empty() { &out } else { &err };
            bail!("{}", first_line(text));
        }
        Ok(())
    }

    pub fn list(&self, path: &str, elevated: bool) -> Result<Vec<FileEntry>> {
        // Reading the filesystem directly is both faster and more exact than
        // parsing `ls`; that path is only needed where our own account cannot
        // look, which is the whole point of elevation.
        match elevated {
            false => list_dir(Path::new(path)),
            true => crate::sshconn::list_by_running(path, |cmd| self.run(cmd, true)),
        }
    }

    pub fn resolve_dir(&self, path: &str, elevated: bool) -> Result<String> {
        let expanded = expand(&self.expand_tilde(path));
        // `stat` rather than `read_dir`: a directory your account cannot open
        // still has to be nameable, or sudo mode could never be pointed at one.
        match fs::metadata(&expanded) {
            Ok(meta) if meta.is_dir() => Ok(expanded.display().to_string()),
            Ok(_) => bail!("not a directory: {path}"),
            // Unreadable to us, so ask root whether it is there at all.
            Err(_) if elevated => {
                let cmd = format!("cd {} && pwd", sh_quote(&expanded.display().to_string()));
                let (out, _, code) = self.run(&cmd, true)?;
                if code == 0 && !out.trim().is_empty() {
                    return Ok(out.trim().to_string());
                }
                bail!("no such directory: {path}")
            }
            Err(e) => Err(anyhow!("{path}: {e}")),
        }
    }

    fn expand_tilde(&self, path: &str) -> String {
        if path == "~" {
            return self.home.clone();
        }
        match path.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", self.home.trim_end_matches('/')),
            None => path.to_string(),
        }
    }

    pub fn mkdir(&self, path: &str, elevated: bool) -> Result<()> {
        match elevated {
            false => mkdir(Path::new(path)),
            true => self.must(&format!("mkdir {}", sh_quote(path)), true),
        }
    }

    pub fn rename(&self, from: &str, to: &str, elevated: bool) -> Result<()> {
        match elevated {
            false => rename(Path::new(from), Path::new(to)),
            true => self.must(&format!("mv -- {} {}", sh_quote(from), sh_quote(to)), true),
        }
    }

    pub fn remove(&self, path: &str, elevated: bool) -> Result<()> {
        match elevated {
            false => remove(Path::new(path)),
            true => self.must(&format!("rm -rf -- {}", sh_quote(path)), true),
        }
    }

    /// Copy inside this machine, which is what a transfer amounts to here.
    ///
    /// Plain `cp -Rd`, not `cp -a`, for the same reason the sudo staging uses
    /// it: a file already at the destination keeps its own mode, owner and
    /// group and only its contents change, so saving an edit cannot quietly
    /// rewrite the permissions of the file being edited. A new file is created
    /// with the source's mode.
    ///
    /// Progress is reported once at the end — there is no network to wait on,
    /// and a bar that fills in one step is honest about that.
    pub fn copy(
        &self,
        from: &str,
        into: &str,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        self.must(
            &format!("cp -Rd -- {} {}/", sh_quote(from), sh_quote(into)),
            elevated,
        )?;
        progress(tree_size(Path::new(from)));
        Ok(())
    }

    /// Prove the password before the UI claims root access is on.
    pub fn check_sudo(&self) -> Result<()> {
        let (out, err, code) = self.run("id -u", true)?;
        if code != 0 || out.trim() != "0" {
            let text = if err.trim().is_empty() { &out } else { &err };
            bail!("{}", first_line(text));
        }
        Ok(())
    }

    pub fn set_sudo_password(&mut self, secret: Option<String>) {
        self.sudo_password = secret;
    }
}

/// What to run, and with what.
///
/// `sudo -S` takes the password on stdin so it never reaches a command line,
/// where any other process on the machine could read it, and `-p ''` keeps
/// sudo's prompt out of the output we parse.
fn argv(shell: &str, cmd: &str, elevated: bool) -> (String, Vec<String>) {
    match elevated {
        false => (shell.to_string(), vec!["-c".into(), cmd.into()]),
        true => (
            "sudo".into(),
            vec![
                "-S".into(),
                "-p".into(),
                String::new(),
                shell.into(),
                "-c".into(),
                cmd.into(),
            ],
        ),
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "you".into())
}

/// The name this machine calls itself, for the tab. Not worth a dependency:
/// the kernel will tell us, and anything without a hostname gets a label that
/// still reads correctly.
fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .or_else(|| fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "this machine".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sshman-local-{}-{name}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A tab on this machine, without touching the real environment.
    fn conn(home: &Path) -> LocalConn {
        LocalConn {
            user: "me".into(),
            host: "here".into(),
            home: home.display().to_string(),
            sudo_password: None,
        }
    }

    #[test]
    fn the_password_goes_on_stdin_and_never_on_a_command_line() {
        let (program, args) = argv("/bin/sh", "id -u", true);
        assert_eq!(program, "sudo");
        assert_eq!(args, ["-S", "-p", "", "/bin/sh", "-c", "id -u"]);
        assert!(
            !args.iter().any(|a| a.contains("password")),
            "nothing about the secret belongs here: {args:?}"
        );

        let (program, args) = argv("/bin/sh", "id -u", false);
        assert_eq!((program.as_str(), args.len()), ("/bin/sh", 2));
    }

    #[test]
    fn a_command_that_reads_stdin_sees_the_end_of_it() {
        // stdin is a pipe we hold; only the password ever goes in. If it were
        // left open, anything reading it would wait for input that is never
        // coming — `timeout` turns that hang into a failure rather than a
        // test that never returns.
        let dir = scratch("stdin");
        let c = conn(&dir);
        let (out, _, code) = c.run("timeout 5 cat", false).unwrap();
        assert_eq!(code, 0, "reading stdin must not hang");
        assert!(out.is_empty(), "and there is nothing in it: {out:?}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn it_lists_what_is_there() {
        let dir = scratch("list");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let c = conn(&dir);

        let entries = c.list(&dir.display().to_string(), false).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.txt"], "directories first");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_is_resolved_from_a_tilde_or_a_relative_path() {
        let dir = scratch("resolve");
        fs::create_dir(dir.join("sub")).unwrap();
        let c = conn(&dir);

        assert_eq!(
            c.resolve_dir("~", false).unwrap(),
            dir.display().to_string()
        );
        assert_eq!(
            c.resolve_dir("~/sub", false).unwrap(),
            dir.join("sub").display().to_string()
        );
        // A file is not somewhere a pane can go, and neither is nothing.
        fs::write(dir.join("f"), b"x").unwrap();
        assert!(
            c.resolve_dir(&dir.join("f").display().to_string(), false)
                .is_err()
        );
        assert!(c.resolve_dir("/definitely-not-here", false).is_err());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn it_makes_renames_and_removes_things() {
        let dir = scratch("edit");
        let c = conn(&dir);
        let path = |name: &str| dir.join(name).display().to_string();

        c.mkdir(&path("one"), false).unwrap();
        assert!(dir.join("one").is_dir());
        c.rename(&path("one"), &path("two"), false).unwrap();
        assert!(dir.join("two").is_dir() && !dir.join("one").exists());
        c.remove(&path("two"), false).unwrap();
        assert!(!dir.join("two").exists());

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_transfer_is_a_copy_that_keeps_the_originals_permissions() {
        let dir = scratch("copy");
        let c = conn(&dir);
        fs::create_dir(dir.join("dst")).unwrap();
        let script = dir.join("run.sh");
        fs::write(&script, b"echo hi\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o750)).unwrap();

        let mut moved = 0;
        c.copy(
            &script.display().to_string(),
            &dir.join("dst").display().to_string(),
            false,
            &mut |n| moved += n,
        )
        .unwrap();

        let landed = dir.join("dst/run.sh");
        assert_eq!(fs::read_to_string(&landed).unwrap(), "echo hi\n");
        assert_eq!(
            fs::metadata(&landed).unwrap().permissions().mode() & 0o777,
            0o750,
            "an executable copied across is still executable"
        );
        assert_eq!(moved, 8, "the progress bar is told what moved");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copying_over_a_file_leaves_its_permissions_alone() {
        // The same bargain every other backend makes: saving an edit replaces
        // the contents and nothing else.
        let dir = scratch("copy-over");
        let c = conn(&dir);
        fs::create_dir(dir.join("dst")).unwrap();
        let target = dir.join("dst/conf");
        fs::write(&target, b"old\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        // What an editor hands back: same contents, whatever mode its umask
        // gave the temp file.
        let edited = dir.join("conf");
        fs::write(&edited, b"new\n").unwrap();
        fs::set_permissions(&edited, fs::Permissions::from_mode(0o644)).unwrap();

        c.copy(
            &edited.display().to_string(),
            &dir.join("dst").display().to_string(),
            false,
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "new\n");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600,
            "the file being written to keeps its own permissions"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_copy_that_cannot_happen_says_why() {
        let dir = scratch("copy-fail");
        let c = conn(&dir);
        let err = c
            .copy(
                &dir.join("not-here").display().to_string(),
                &dir.display().to_string(),
                false,
                &mut |_| {},
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not-here"), "{err}");
        fs::remove_dir_all(&dir).ok();
    }
}
