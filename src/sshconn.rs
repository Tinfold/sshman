//! The SSH/SFTP layer.
//!
//! Two things here are worth knowing before reading the code:
//!
//! 1. **Sudo listing and sudo transfers do not use SFTP.** The SFTP subsystem
//!    runs as the login user, so root-only paths are invisible to it no matter
//!    what we ask. When sudo mode is on we shell out (`sudo -S`) to list, and
//!    we stage transfers through a temporary directory the login user owns.
//! 2. **The sudo password is fed on stdin, never mixed with file data.** sudo
//!    reads its password with a buffered read that can swallow whatever follows
//!    it, so piping a file body after the password is unreliable. Staging via
//!    `/tmp` keeps the two streams entirely separate.

use std::fs::File as StdFile;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use ssh2::{
    CheckResult, HashType, KeyboardInteractivePrompt, KnownHostFileKind, KnownHostKeyFormat,
    OpenFlags, OpenType, Prompt, Session, Sftp,
};

use crate::types::{
    EntryKind, FileEntry, kind_from_mode, perm_string, rbasename, rjoin, sh_quote, sort_entries,
};

const CHUNK: usize = 128 * 1024;

#[derive(Clone, Debug, Default)]
pub struct ConnectOpts {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub key_path: Option<PathBuf>,
    pub key_passphrase: Option<String>,
    /// Set once the user has eyeballed and accepted an unknown host key.
    pub accept_new_host_key: bool,
    /// Set only after the user has been shown the mismatch and explicitly
    /// confirmed replacing the recorded key. Never set automatically.
    pub replace_host_key: bool,
}

/// Connect failures the UI needs to react to differently from a generic error.
#[derive(Debug)]
pub enum ConnectError {
    /// Host is not in known_hosts. Carries the fingerprint to show the user.
    UnknownHostKey {
        fingerprint: String,
        keytype: String,
    },
    /// Host key changed — we refuse and never offer to auto-accept.
    HostKeyMismatch {
        fingerprint: String,
    },
    Auth(String),
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownHostKey { fingerprint, .. } => {
                write!(f, "unknown host key {fingerprint}")
            }
            Self::HostKeyMismatch { fingerprint } => {
                write!(f, "HOST KEY MISMATCH (offered {fingerprint})")
            }
            Self::Auth(m) => write!(f, "authentication failed: {m}"),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}

/// Answers every keyboard-interactive challenge with the same password. That is
/// what a password login looks like on servers that route passwords through
/// PAM's keyboard-interactive method.
struct PasswordPrompter(String);

impl KeyboardInteractivePrompt for PasswordPrompter {
    fn prompt<'a>(&mut self, _u: &str, _i: &str, prompts: &[Prompt<'a>]) -> Vec<String> {
        prompts.iter().map(|_| self.0.clone()).collect()
    }
}

pub struct Conn {
    sess: Session,
    sftp: Sftp,
    pub user: String,
    pub host: String,
    pub port: u16,
    pub home: String,
    pub sudo_password: Option<String>,
    stage_counter: u64,
}

/// Open a TCP connection, complete the SSH handshake, verify the host key and
/// authenticate — everything up to the point where channels can be opened.
///
/// Shared by the file-transfer connection and the separate connection the
/// interactive shell runs on, so both apply identical host-key and auth rules.
pub fn establish(opts: &ConnectOpts) -> Result<Session, ConnectError> {
    let addr = format!("{}:{}", opts.host, opts.port)
        .to_socket_addrs()
        .map_err(|e| ConnectError::Other(format!("cannot resolve {}: {e}", opts.host)))?
        .next()
        .ok_or_else(|| ConnectError::Other(format!("no address for {}", opts.host)))?;

    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(15))
        .map_err(|e| ConnectError::Other(format!("connect {addr}: {e}")))?;
    tcp.set_nodelay(true).ok();

    let mut sess = Session::new().map_err(|e| ConnectError::Other(e.to_string()))?;
    // Generous, but finite: a hung server should not wedge the worker thread
    // forever. Transfers keep the connection producing data.
    sess.set_timeout(60_000);
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .map_err(|e| ConnectError::Other(format!("SSH handshake failed: {e}")))?;

    verify_host_key(
        &sess,
        &opts.host,
        opts.port,
        opts.accept_new_host_key,
        opts.replace_host_key,
    )?;
    authenticate(&sess, opts)?;
    sess.set_keepalive(true, 30);
    Ok(sess)
}

impl Conn {
    pub fn connect(opts: &ConnectOpts) -> Result<Self, ConnectError> {
        let sess = establish(opts)?;

        let sftp = sess
            .sftp()
            .map_err(|e| ConnectError::Other(format!("SFTP subsystem unavailable: {e}")))?;
        let home = sftp
            .realpath(Path::new("."))
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| format!("/home/{}", opts.user));

        Ok(Self {
            sess,
            sftp,
            user: opts.user.clone(),
            host: opts.host.clone(),
            port: opts.port,
            home,
            sudo_password: None,
            stage_counter: 0,
        })
    }

    /// Cheap round trip that answers "is this connection still usable?".
    ///
    /// The normal timeout is generous so that slow transfers are not cut off,
    /// which is far too long to wait when merely asking whether the socket is
    /// alive — so it is shortened for the probe and put straight back.
    pub fn is_alive(&self) -> bool {
        self.sess.set_timeout(5_000);
        let alive = self.sftp.realpath(Path::new(".")).is_ok();
        self.sess.set_timeout(60_000);
        alive
    }

    // ---- command execution -------------------------------------------------

    /// Run a command and collect its output. `(stdout, stderr, exit code)`.
    ///
    /// Through `/bin/sh` rather than straight at the channel, because a
    /// channel `exec` is run by the *login user's shell* — and every command
    /// in this file is written in POSIX shell. On a server whose user has
    /// `fish` or `csh` as their login shell, `if …; then …; fi` is not a
    /// script with a bug in it, it is not a script at all: fish answers
    /// `expected end of the statement`, which is a message about sshman that
    /// looks like a message about the key you were installing.
    ///
    /// The login shell is still what a *terminal* gets — see
    /// [`crate::shell`], which opens its channel itself. That is the one
    /// place the user's own shell is the point rather than an obstacle.
    pub fn exec(&self, cmd: &str) -> Result<(String, String, i32)> {
        let mut ch = self.sess.channel_session().context("open channel")?;
        ch.exec(&format!("/bin/sh -c {}", sh_quote(cmd)))
            .context("exec")?;
        let mut out = Vec::new();
        ch.read_to_end(&mut out).ok();
        let mut err = Vec::new();
        ch.stderr().read_to_end(&mut err).ok();
        ch.wait_close().ok();
        let code = ch.exit_status().unwrap_or(-1);
        Ok((
            String::from_utf8_lossy(&out).to_string(),
            String::from_utf8_lossy(&err).to_string(),
            code,
        ))
    }

    /// Run a command under `sudo`, feeding the stored password on stdin.
    /// `-p ''` suppresses the prompt so it never lands in the output.
    pub fn exec_sudo(&self, cmd: &str) -> Result<(String, String, i32)> {
        let wrapped = format!("sudo -S -p '' -- /bin/sh -c {}", sh_quote(cmd));
        let mut ch = self.sess.channel_session().context("open channel")?;
        ch.exec(&wrapped).context("exec sudo")?;
        if let Some(pw) = &self.sudo_password {
            ch.write_all(format!("{pw}\n").as_bytes()).ok();
        }
        ch.flush().ok();
        ch.send_eof().ok();
        let mut out = Vec::new();
        ch.read_to_end(&mut out).ok();
        let mut err = Vec::new();
        ch.stderr().read_to_end(&mut err).ok();
        ch.wait_close().ok();
        let code = ch.exit_status().unwrap_or(-1);
        Ok((
            String::from_utf8_lossy(&out).to_string(),
            String::from_utf8_lossy(&err).to_string(),
            code,
        ))
    }

    pub fn run(&self, cmd: &str, sudo: bool) -> Result<(String, String, i32)> {
        if sudo {
            self.exec_sudo(cmd)
        } else {
            self.exec(cmd)
        }
    }

    /// Check that sudo works with the stored password before we let the user
    /// believe sudo mode is active.
    pub fn check_sudo(&self) -> Result<()> {
        let (_, err, code) = self.exec_sudo("id -u")?;
        if code != 0 {
            let msg = err.trim();
            bail!(if msg.is_empty() {
                "sudo refused (exit status non-zero)".to_string()
            } else {
                msg.to_string()
            });
        }
        Ok(())
    }

    /// Run a shell command and fail loudly if it did not succeed. Used for the
    /// staging steps, where a silent failure would produce a confusing
    /// half-finished transfer.
    fn must(&self, cmd: &str, sudo: bool) -> Result<String> {
        let (out, err, code) = self.run(cmd, sudo)?;
        if code != 0 {
            let msg = if err.trim().is_empty() {
                out.trim().to_string()
            } else {
                err.trim().to_string()
            };
            bail!("{msg}");
        }
        Ok(out)
    }

    /// Append `public_key` to the login user's `authorized_keys`, creating
    /// `~/.ssh` with the permissions sshd insists on. Returns whether it was
    /// actually added (false meaning it was already there).
    ///
    /// This is `ssh-copy-id` in one command. `grep -qxF` matches the whole
    /// line literally, so re-running it never duplicates an entry, and the
    /// key is shell-quoted so its contents cannot be interpreted.
    pub fn install_public_key(&self, public_key: &str) -> Result<bool> {
        let key = public_key.trim();
        if key.is_empty() {
            bail!("the public key is empty");
        }
        let quoted = sh_quote(key);
        let script = format!(
            "umask 077 && mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys \
             && chmod 700 ~/.ssh && chmod 600 ~/.ssh/authorized_keys \
             && if grep -qxF {quoted} ~/.ssh/authorized_keys; then echo __PRESENT__; \
                else printf '%s\\n' {quoted} >> ~/.ssh/authorized_keys && echo __ADDED__; fi"
        );
        let (out, err, code) = self.exec(&script)?;
        if code != 0 {
            let msg = if err.trim().is_empty() {
                out.trim()
            } else {
                err.trim()
            };
            bail!("{msg}");
        }
        Ok(out.contains("__ADDED__"))
    }

    // ---- directory listing -------------------------------------------------

    pub fn list(&self, path: &str, sudo: bool) -> Result<Vec<FileEntry>> {
        if sudo {
            self.list_via_shell(path)
        } else {
            self.list_via_sftp(path)
        }
    }

    fn list_via_sftp(&self, path: &str) -> Result<Vec<FileEntry>> {
        let items = self
            .sftp
            .readdir(Path::new(path))
            .with_context(|| format!("cannot read {path}"))?;
        let mut out = Vec::with_capacity(items.len());
        for (p, stat) in items {
            let name = rbasename(&p.to_string_lossy());
            if name == "." || name == ".." || name.is_empty() {
                continue;
            }
            let mode = stat.perm.unwrap_or(0);
            let kind = kind_from_mode(mode);
            // Only symlinks need the extra round trips.
            let (link_target, points_to_dir) = if kind == EntryKind::Symlink {
                let full = rjoin(path, &name);
                let target = self
                    .sftp
                    .readlink(Path::new(&full))
                    .ok()
                    .map(|t| t.to_string_lossy().to_string());
                let is_dir = self
                    .sftp
                    .stat(Path::new(&full))
                    .map(|s| s.is_dir())
                    .unwrap_or(false);
                (target, is_dir)
            } else {
                (None, false)
            };
            out.push(FileEntry {
                name,
                kind,
                size: stat.size.unwrap_or(0),
                mtime: stat.mtime.unwrap_or(0) as i64,
                perms: perm_string(mode),
                link_target,
                points_to_dir,
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    /// Sudo listing. Prefers GNU `find -printf` because it emits unambiguous
    /// tab-delimited fields; falls back to parsing `ls -la` on systems whose
    /// find lacks `-printf` (BSD, busybox).
    fn list_via_shell(&self, path: &str) -> Result<Vec<FileEntry>> {
        list_by_running(path, |cmd| self.exec_sudo(cmd))
    }

    /// Resolve a path typed into the "go to directory" prompt, expanding `~`
    /// and collapsing `..`.
    ///
    /// The path is shell-quoted before it reaches `cd`, so a directory called
    /// `; reboot` is just a directory. That quoting also stops the shell from
    /// expanding `~` itself, so we do that part here.
    pub fn resolve_dir(&self, path: &str, sudo: bool) -> Result<String> {
        let expanded = self.expand_tilde(path);
        let cmd = format!("cd {} 2>/dev/null && pwd", sh_quote(&expanded));
        let (out, _, code) = self.run(&cmd, sudo)?;
        if code == 0 && !out.trim().is_empty() {
            return Ok(out.trim().to_string());
        }
        // Fall back to SFTP's canonicaliser for accounts with no usable shell.
        // It happily canonicalises paths that do not exist, so confirm the
        // result is really a directory before handing it back.
        let real = self
            .sftp
            .realpath(Path::new(&expanded))
            .with_context(|| format!("no such directory: {path}"))?;
        let stat = self
            .sftp
            .stat(&real)
            .with_context(|| format!("no such directory: {path}"))?;
        if !stat.is_dir() {
            bail!("not a directory: {path}");
        }
        Ok(real.to_string_lossy().to_string())
    }

    fn expand_tilde(&self, path: &str) -> String {
        if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            rjoin(&self.home, rest)
        } else {
            path.to_string()
        }
    }

    // ---- mutations ---------------------------------------------------------

    pub fn mkdir(&self, path: &str, sudo: bool) -> Result<()> {
        if sudo {
            self.must(&format!("mkdir {}", sh_quote(path)), true)?;
            Ok(())
        } else {
            self.sftp
                .mkdir(Path::new(path), 0o755)
                .with_context(|| format!("mkdir {path}"))
        }
    }

    pub fn rename(&self, from: &str, to: &str, sudo: bool) -> Result<()> {
        if sudo {
            self.must(&format!("mv -- {} {}", sh_quote(from), sh_quote(to)), true)?;
            Ok(())
        } else {
            self.sftp
                .rename(Path::new(from), Path::new(to), None)
                .with_context(|| format!("rename {from}"))
        }
    }

    pub fn remove(&self, path: &str, sudo: bool) -> Result<()> {
        if sudo {
            self.must(&format!("rm -rf -- {}", sh_quote(path)), true)?;
            return Ok(());
        }
        self.remove_recursive(path)
    }

    fn remove_recursive(&self, path: &str) -> Result<()> {
        let stat = self
            .sftp
            .lstat(Path::new(path))
            .with_context(|| format!("stat {path}"))?;
        if stat.is_dir() {
            for (p, _) in self.sftp.readdir(Path::new(path))? {
                let name = rbasename(&p.to_string_lossy());
                if name == "." || name == ".." || name.is_empty() {
                    continue;
                }
                self.remove_recursive(&rjoin(path, &name))?;
            }
            self.sftp
                .rmdir(Path::new(path))
                .with_context(|| format!("rmdir {path}"))
        } else {
            self.sftp
                .unlink(Path::new(path))
                .with_context(|| format!("rm {path}"))
        }
    }

    // ---- transfers ---------------------------------------------------------

    /// Byte total of a remote file or tree, for the progress bar.
    pub fn remote_tree_size(&self, path: &str, sudo: bool) -> u64 {
        if sudo {
            let cmd = format!("du -sb -- {} 2>/dev/null | cut -f1", sh_quote(path));
            if let Ok((out, _, 0)) = self.exec_sudo(&cmd)
                && let Ok(n) = out.trim().parse::<u64>()
            {
                return n;
            }
            return 0;
        }
        self.sftp_tree_size(path)
    }

    fn sftp_tree_size(&self, path: &str) -> u64 {
        let stat = match self.sftp.lstat(Path::new(path)) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        if !stat.is_dir() {
            return stat.size.unwrap_or(0);
        }
        let mut total = 0;
        if let Ok(items) = self.sftp.readdir(Path::new(path)) {
            for (p, _) in items {
                let name = rbasename(&p.to_string_lossy());
                if name == "." || name == ".." || name.is_empty() {
                    continue;
                }
                total += self.sftp_tree_size(&rjoin(path, &name));
            }
        }
        total
    }

    /// Copy `remote` into the local directory `dest_dir`, keeping its basename.
    pub fn download(
        &mut self,
        remote: &str,
        dest_dir: &Path,
        sudo: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let name = rbasename(remote);
        let local = dest_dir.join(&name);
        if !sudo {
            return self.download_tree(remote, &local, progress);
        }
        // Stage a readable copy in /tmp, pull it over SFTP, then clean up.
        let stage = self.make_stage_dir()?;
        let staged = rjoin(&stage, &name);
        let result = (|| -> Result<()> {
            self.must(
                &format!("cp -a -- {} {}/", sh_quote(remote), sh_quote(&stage)),
                true,
            )?;
            self.must(
                &format!(
                    "chown -R -- {} {} && chmod -R u+rwX -- {}",
                    sh_quote(&self.user),
                    sh_quote(&stage),
                    sh_quote(&stage)
                ),
                true,
            )?;
            self.download_tree(&staged, &local, progress)
        })();
        self.cleanup_stage(&stage);
        result
    }

    fn download_tree(
        &self,
        remote: &str,
        local: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let stat = self
            .sftp
            .lstat(Path::new(remote))
            .with_context(|| format!("stat {remote}"))?;
        if stat.is_dir() {
            std::fs::create_dir_all(local).with_context(|| format!("mkdir {}", local.display()))?;
            for (p, _) in self.sftp.readdir(Path::new(remote))? {
                let name = rbasename(&p.to_string_lossy());
                if name == "." || name == ".." || name.is_empty() {
                    continue;
                }
                self.download_tree(&rjoin(remote, &name), &local.join(&name), progress)?;
            }
            if let Some(mode) = stat.perm {
                crate::local::set_mode(local, mode);
            }
            return Ok(());
        }

        let mut rf = self
            .sftp
            .open(Path::new(remote))
            .with_context(|| format!("open {remote}"))?;
        let mut lf =
            StdFile::create(local).with_context(|| format!("create {}", local.display()))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = rf
                .read(&mut buf)
                .with_context(|| format!("read {remote}"))?;
            if n == 0 {
                break;
            }
            lf.write_all(&buf[..n])
                .with_context(|| format!("write {}", local.display()))?;
            progress(n as u64);
        }
        lf.flush().ok();
        drop(lf);
        if let Some(mode) = stat.perm {
            crate::local::set_mode(local, mode);
        }
        Ok(())
    }

    /// Copy `local` into the remote directory `dest_dir`, keeping its basename.
    pub fn upload(
        &mut self,
        local: &Path,
        dest_dir: &str,
        sudo: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| anyhow!("{} has no file name", local.display()))?;
        if !sudo {
            return self.upload_tree(local, &rjoin(dest_dir, &name), progress);
        }
        // Land it in a temp dir we own, then let root move it into place. `cp`
        // into a trailing-slash directory behaves identically for files and
        // directories, which keeps this one code path correct for both.
        //
        // Plain `cp -Rd` rather than `cp -a`: the staged copy is owned by the
        // login user and carries whatever mode the staging chmod left on it,
        // and `-a` would stamp all of that onto the destination. Saving an
        // edit of a root-owned 0640 config would hand it to the login user at
        // 0644. Without `-a`, an existing destination keeps its own mode,
        // owner and group and only its contents change, and a new file is
        // created owned by root, which is what a copy made as root should be.
        let stage = self.make_stage_dir()?;
        let staged = rjoin(&stage, &name);
        let result = (|| -> Result<()> {
            self.upload_tree(local, &staged, progress)?;
            self.must(
                &format!("cp -Rd -- {} {}/", sh_quote(&staged), sh_quote(dest_dir)),
                true,
            )?;
            Ok(())
        })();
        self.cleanup_stage(&stage);
        result
    }

    fn upload_tree(&self, local: &Path, remote: &str, progress: &mut dyn FnMut(u64)) -> Result<()> {
        let meta = std::fs::symlink_metadata(local)
            .with_context(|| format!("stat {}", local.display()))?;
        if meta.is_dir() {
            // Ignore "already exists"; we merge into existing directories.
            let _ = self.sftp.mkdir(Path::new(remote), 0o755);
            for item in std::fs::read_dir(local)
                .with_context(|| format!("read {}", local.display()))?
                .flatten()
            {
                let name = item.file_name().to_string_lossy().to_string();
                self.upload_tree(&item.path(), &rjoin(remote, &name), progress)?;
            }
            return Ok(());
        }

        use std::os::unix::fs::PermissionsExt;
        let mode = (meta.permissions().mode() & 0o7777) as i32;
        let mut lf = StdFile::open(local).with_context(|| format!("open {}", local.display()))?;
        let mut rf = self
            .sftp
            .open_mode(
                Path::new(remote),
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                mode,
                OpenType::File,
            )
            .with_context(|| format!("create {remote}"))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = lf
                .read(&mut buf)
                .with_context(|| format!("read {}", local.display()))?;
            if n == 0 {
                break;
            }
            rf.write_all(&buf[..n])
                .with_context(|| format!("write {remote}"))?;
            progress(n as u64);
        }
        rf.flush().ok();
        Ok(())
    }

    // ---- staging helpers ---------------------------------------------------

    fn make_stage_dir(&mut self) -> Result<String> {
        self.stage_counter += 1;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = format!("/tmp/.sshman-{}-{}", nanos, self.stage_counter);
        self.sftp
            .mkdir(Path::new(&path), 0o700)
            .with_context(|| format!("cannot create staging dir {path}"))?;
        Ok(path)
    }

    fn cleanup_stage(&self, stage: &str) {
        // We own the staging dir, so this needs no privilege. Best effort:
        // never let cleanup mask the real error from the transfer.
        let _ = self.exec(&format!("rm -rf -- {}", sh_quote(stage)));
    }
}

// ---- host key verification -------------------------------------------------

fn verify_host_key(
    sess: &Session,
    host: &str,
    port: u16,
    accept_new: bool,
    replace_existing: bool,
) -> Result<(), ConnectError> {
    let (key, keytype) = sess
        .host_key()
        .ok_or_else(|| ConnectError::Other("server presented no host key".into()))?;
    let fingerprint = fingerprint_of(sess, key);

    let mut known = sess
        .known_hosts()
        .map_err(|e| ConnectError::Other(e.to_string()))?;
    let path = known_hosts_path();
    // A missing known_hosts file is normal on a fresh machine.
    let _ = known.read_file(&path, KnownHostFileKind::OpenSSH);

    match known.check_port(host, port, key) {
        CheckResult::Match => Ok(()),
        CheckResult::NotFound => {
            if !accept_new {
                return Err(ConnectError::UnknownHostKey {
                    fingerprint,
                    keytype: keytype_name(keytype).to_string(),
                });
            }
            record_host_key(&mut known, &path, host, port, key, keytype)
        }
        CheckResult::Mismatch => {
            if !replace_existing {
                return Err(ConnectError::HostKeyMismatch { fingerprint });
            }
            // Only reachable after the user was shown the mismatch and
            // confirmed in as many words. Drop every recorded key for this
            // host before writing the new one, or `check_port` would keep
            // seeing the old one and report a mismatch forever.
            let entry = known_hosts_entry(host, port);
            let stale: Vec<_> = known
                .hosts()
                .map_err(|e| ConnectError::Other(format!("cannot read known_hosts: {e}")))?
                .into_iter()
                .filter(|h| h.name() == Some(entry.as_str()) || h.name() == Some(host))
                .collect();
            for old in &stale {
                known
                    .remove(old)
                    .map_err(|e| ConnectError::Other(format!("cannot drop old host key: {e}")))?;
            }
            record_host_key(&mut known, &path, host, port, key, keytype)
        }
        CheckResult::Failure => Err(ConnectError::Other(
            "host key check failed (known_hosts unreadable?)".into(),
        )),
    }
}

/// How OpenSSH names a host in `known_hosts`: bare for port 22, `[host]:port`
/// otherwise.
fn known_hosts_entry(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

fn record_host_key(
    known: &mut ssh2::KnownHosts,
    path: &Path,
    host: &str,
    port: u16,
    key: &[u8],
    keytype: ssh2::HostKeyType,
) -> Result<(), ConnectError> {
    let fmt = KnownHostKeyFormat::from(keytype);
    if matches!(fmt, KnownHostKeyFormat::Unknown) {
        return Err(ConnectError::Other(
            "server host key type is not recognised".into(),
        ));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    known
        .add(&known_hosts_entry(host, port), key, "added by sshman", fmt)
        .map_err(|e| ConnectError::Other(format!("cannot record host key: {e}")))?;
    known
        .write_file(path, KnownHostFileKind::OpenSSH)
        .map_err(|e| ConnectError::Other(format!("cannot write known_hosts: {e}")))?;
    Ok(())
}

fn fingerprint_of(sess: &Session, key: &[u8]) -> String {
    if let Some(hash) = sess.host_key_hash(HashType::Sha256) {
        return format!(
            "SHA256:{}",
            base64::engine::general_purpose::STANDARD_NO_PAD.encode(hash)
        );
    }
    // No SHA256 support in this libssh2 build; a raw length is better than
    // silently showing nothing the user could compare.
    format!("<{} byte key, no SHA256 available>", key.len())
}

fn keytype_name(t: ssh2::HostKeyType) -> &'static str {
    use ssh2::HostKeyType::*;
    match t {
        Rsa => "ssh-rsa",
        Dss => "ssh-dss",
        Ecdsa256 => "ecdsa-sha2-nistp256",
        Ecdsa384 => "ecdsa-sha2-nistp384",
        Ecdsa521 => "ecdsa-sha2-nistp521",
        Ed25519 => "ssh-ed25519",
        Unknown => "unknown",
    }
}

fn known_hosts_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".ssh")
        .join("known_hosts")
}

// ---- authentication --------------------------------------------------------

fn authenticate(sess: &Session, opts: &ConnectOpts) -> Result<(), ConnectError> {
    let user = &opts.user;
    let methods = sess.auth_methods(user).unwrap_or("").to_string();
    let mut tried: Vec<String> = Vec::new();

    // Some servers accept any user with no auth at all.
    if sess.authenticated() {
        return Ok(());
    }

    // 1. An explicitly supplied key wins over everything else.
    if let Some(key) = &opts.key_path {
        let pub_key = key.with_extension("pub");
        let pub_key = pub_key.exists().then_some(pub_key);
        match sess.userauth_pubkey_file(
            user,
            pub_key.as_deref(),
            key,
            opts.key_passphrase.as_deref(),
        ) {
            Ok(()) if sess.authenticated() => return Ok(()),
            Ok(()) => {}
            Err(e) => tried.push(format!("key {}: {e}", key.display())),
        }
    }

    // 2. ssh-agent, then the usual default key names.
    if opts.key_path.is_none() {
        if sess.userauth_agent(user).is_ok() && sess.authenticated() {
            return Ok(());
        }
        tried.push("ssh-agent: no usable identity".to_string());

        if let Some(home) = dirs::home_dir() {
            for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
                let key = home.join(".ssh").join(name);
                if !key.exists() {
                    continue;
                }
                let pub_key = key.with_extension("pub");
                let pub_key = pub_key.exists().then_some(pub_key);
                match sess.userauth_pubkey_file(
                    user,
                    pub_key.as_deref(),
                    &key,
                    opts.key_passphrase.as_deref(),
                ) {
                    Ok(()) if sess.authenticated() => return Ok(()),
                    Ok(()) => {}
                    // An encrypted key with no passphrase fails here; say so
                    // rather than reporting a bare "auth failed" at the end.
                    Err(e) => tried.push(format!("{name}: {e}")),
                }
            }
        }
    }

    // 3. Password, offered as both `password` and `keyboard-interactive`.
    if let Some(pw) = &opts.password {
        if methods.is_empty() || methods.contains("password") {
            if sess.userauth_password(user, pw).is_ok() && sess.authenticated() {
                return Ok(());
            }
            tried.push("password: rejected".to_string());
        }
        if methods.contains("keyboard-interactive") {
            let mut prompter = PasswordPrompter(pw.clone());
            if sess
                .userauth_keyboard_interactive(user, &mut prompter)
                .is_ok()
                && sess.authenticated()
            {
                return Ok(());
            }
            tried.push("keyboard-interactive: rejected".to_string());
        }
    }

    if sess.authenticated() {
        return Ok(());
    }

    let detail = if tried.is_empty() {
        format!("server offers: {methods}")
    } else {
        tried.join("; ")
    };
    Err(ConnectError::Auth(detail))
}

// ---- output parsing --------------------------------------------------------

/// List a directory using only shell commands.
///
/// Prefers GNU `find -printf` because it emits unambiguous tab-delimited
/// fields; falls back to parsing `ls -la` where find is older or cut down —
/// BSD, busybox, and most minimal container images.
pub fn list_by_running(
    path: &str,
    run: impl Fn(&str) -> Result<(String, String, i32)>,
) -> Result<Vec<FileEntry>> {
    // A trailing slash is what makes both `find` and `ls` look *through* a
    // symlink to the directory it names. Without it they describe the link
    // itself, and entering a symlinked directory shows a single entry: the
    // link you just followed. Harmless on an ordinary directory.
    let q = sh_quote(&format!("{}/", path.trim_end_matches('/')));
    // `%y` is the entry's own type and `%Y` the type it resolves to, which
    // tells us whether a symlink can be descended into without asking once
    // per link.
    let find = format!(r"find {q} -maxdepth 1 -mindepth 1 -printf '%y\t%Y\t%s\t%T@\t%m\t%f\t%l\n'");
    let (out, err, code) = run(&find)?;
    if code == 0 {
        return Ok(parse_find(&out));
    }
    // Distinguish "find is too old" from "you really cannot read this".
    let (out2, err2, code2) = run(&format!(
        "ls -la --time-style=long-iso {q} 2>/dev/null || ls -la {q}"
    ))?;
    if code2 == 0 {
        let mut entries = parse_ls(&out2);
        resolve_symlink_targets(path, &mut entries, &run);
        return Ok(entries);
    }
    let msg = [err.trim(), err2.trim()]
        .iter()
        .find(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("cannot list {path}"));
    bail!("{msg}");
}

/// Work out which of the symlinks in a listing point at directories.
///
/// `ls` says an entry is a symlink but not what it resolves to, so without
/// this a symlink to a directory cannot be entered — which is every symlink
/// on the minimal images whose `find` has no `-printf`. One command covers
/// the whole listing, however many links it holds.
fn resolve_symlink_targets(
    dir: &str,
    entries: &mut [FileEntry],
    run: &impl Fn(&str) -> Result<(String, String, i32)>,
) {
    let links: Vec<&str> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::Symlink)
        .map(|e| e.name.as_str())
        .collect();
    if links.is_empty() {
        return;
    }

    // `test -d` follows symlinks, which is exactly the question being asked.
    let mut script = format!(
        "cd {} 2>/dev/null || exit 0
",
        sh_quote(dir)
    );
    script.push_str("for n in");
    for name in &links {
        script.push(' ');
        script.push_str(&sh_quote(name));
    }
    // Each name on its own line, so ones with spaces survive. The `if` is
    // deliberate: with `&&`, a final entry that is not a directory would make
    // the whole loop exit non-zero and the answer would be thrown away.
    script.push_str("; do if [ -d \"$n\" ]; then printf '%s\\n' \"$n\"; fi; done\nexit 0\n");

    let Ok((out, _, 0)) = run(&script) else {
        return;
    };
    let dirs: std::collections::HashSet<&str> = out.lines().map(str::trim).collect();
    for entry in entries.iter_mut() {
        if entry.kind == EntryKind::Symlink && dirs.contains(entry.name.as_str()) {
            entry.points_to_dir = true;
        }
    }
    // Directories sort first, and some entries have just become directories.
    sort_entries(entries);
}

/// Parse `find -printf '%y\t%Y\t%s\t%T@\t%m\t%f\t%l\n'`.
fn parse_find(out: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in out.lines() {
        if line.is_empty() {
            continue;
        }
        // Bounded split: only the trailing symlink target may contain tabs.
        let f: Vec<&str> = line.splitn(7, '\t').collect();
        if f.len() < 6 {
            continue;
        }
        let kind = match f[0] {
            "d" => EntryKind::Dir,
            "f" => EntryKind::File,
            "l" => EntryKind::Symlink,
            _ => EntryKind::Other,
        };
        let points_to_dir = kind == EntryKind::Symlink && f[1] == "d";
        let size = f[2].parse().unwrap_or(0);
        // %T@ is a float ("1712345678.9012"); we only want whole seconds.
        let mtime = f[3]
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let mode = u32::from_str_radix(f[4], 8).unwrap_or(0);
        let name = f[5].to_string();
        let target = f.get(6).map(|s| s.to_string()).filter(|s| !s.is_empty());
        let type_bits = match kind {
            EntryKind::Dir => 0o040000,
            EntryKind::Symlink => 0o120000,
            EntryKind::File => 0o100000,
            EntryKind::Other => 0,
        };
        entries.push(FileEntry {
            name,
            kind,
            size,
            mtime,
            perms: perm_string(mode | type_bits),
            link_target: target,
            points_to_dir,
        });
    }
    sort_entries(&mut entries);
    entries
}

/// Fallback parser for `ls -la`. Field count is fixed up to the name, so
/// splitting a bounded number of times keeps names containing spaces intact.
fn parse_ls(out: &str) -> Vec<FileEntry> {
    let mut entries = Vec::new();
    for line in out.lines() {
        if line.starts_with("total ") || line.trim().is_empty() {
            continue;
        }
        let mut it = line.split_whitespace();
        let perms = match it.next() {
            Some(p) if p.len() >= 10 => p.to_string(),
            _ => continue,
        };
        let _links = it.next();
        let _owner = it.next();
        let _group = it.next();
        let size: u64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        // Date columns vary between `ls` implementations (2 fields for
        // long-iso, 3 for the default). Detect by looking for a `:` or a
        // 4-digit year in the third field.
        let d1 = it.next().unwrap_or("");
        let d2 = it.next().unwrap_or("");
        let mut rest_start = 0;
        if !(d2.contains(':') && d1.contains('-')) {
            // Default format: "Mon DD HH:MM" — one more field to skip.
            rest_start = 1;
        }
        let mut remainder: Vec<&str> = it.collect();
        if rest_start == 1 && !remainder.is_empty() {
            remainder.remove(0);
        }
        let name_field = remainder.join(" ");
        if name_field.is_empty() || name_field == "." || name_field == ".." {
            continue;
        }
        let (name, link_target) = match name_field.split_once(" -> ") {
            Some((n, t)) => (n.to_string(), Some(t.to_string())),
            None => (name_field, None),
        };
        let kind = match perms.chars().next() {
            Some('d') => EntryKind::Dir,
            Some('l') => EntryKind::Symlink,
            Some('-') => EntryKind::File,
            _ => EntryKind::Other,
        };
        entries.push(FileEntry {
            name,
            kind,
            size,
            mtime: 0, // `ls` gives no epoch; the UI shows a dash for this
            perms,
            link_target,
            points_to_dir: false,
        });
    }
    sort_entries(&mut entries);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_output_is_parsed() {
        // %y %Y %s %T@ %m %f %l
        let out = "d\td\t4096\t1712345678.5\t755\tetc\t\n\
                   f\tf\t1234\t1712345600.0\t644\tnotes.txt\t\n\
                   l\td\t7\t1712345000.1\t777\tbin\t/usr/bin\n";
        let e = parse_find(out);
        assert_eq!(e.len(), 3);
        // Directories sort first, then case-insensitive by name.
        assert_eq!(e[0].name, "bin");
        assert!(e[0].points_to_dir, "symlink to a dir is enterable");
        assert_eq!(e[0].link_target.as_deref(), Some("/usr/bin"));
        assert_eq!(e[1].name, "etc");
        assert_eq!(e[1].kind, EntryKind::Dir);
        assert_eq!(e[1].mtime, 1712345678, "fractional seconds are dropped");
        assert_eq!(e[2].name, "notes.txt");
        assert_eq!(e[2].size, 1234);
        assert_eq!(e[2].perms, "-rw-r--r--");
    }

    #[test]
    fn find_keeps_names_with_spaces() {
        let out = "f\tf\t10\t1700000000\t644\tmy notes v2.txt\t\n";
        let e = parse_find(out);
        assert_eq!(e[0].name, "my notes v2.txt");
    }

    /// A stubbed shell, so the fallback path can be tested without a server:
    /// `find -printf` fails, as it does on the minimal images, and everything
    /// falls through to `ls`.
    struct Stub {
        listing: &'static str,
        dirs: &'static str,
        seen: std::cell::RefCell<Vec<String>>,
    }

    impl Stub {
        fn new(listing: &'static str, dirs: &'static str) -> Self {
            Self {
                listing,
                dirs,
                seen: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn run(&self, cmd: &str) -> Result<(String, String, i32)> {
            self.seen.borrow_mut().push(cmd.to_string());
            if cmd.starts_with("find") {
                return Ok((String::new(), "find: unrecognized: -printf".into(), 1));
            }
            if cmd.starts_with("ls ") {
                return Ok((self.listing.to_string(), String::new(), 0));
            }
            // The symlink probe.
            Ok((self.dirs.to_string(), String::new(), 0))
        }

        fn commands(&self) -> Vec<String> {
            self.seen.borrow().clone()
        }
    }

    #[test]
    fn falls_back_to_ls_and_still_follows_symlinks() {
        let listing = "total 4\n\
                       drwxr-xr-x 2 root root 4096 2024-04-05 12:30 real\n\
                       lrwxrwxrwx 1 root root    9 2024-04-05 12:30 to-dir -> /opt/real\n\
                       lrwxrwxrwx 1 root root    5 2024-04-05 12:30 to-file -> /etc/hosts\n";
        let stub = Stub::new(listing, "to-dir\n");
        let entries = list_by_running("/opt", |cmd| stub.run(cmd)).unwrap();

        let to_dir = entries.iter().find(|e| e.name == "to-dir").unwrap();
        assert!(
            to_dir.points_to_dir,
            "a symlink to a directory must be enterable even without find -printf"
        );
        assert!(to_dir.is_dir_like());
        let to_file = entries.iter().find(|e| e.name == "to-file").unwrap();
        assert!(
            !to_file.points_to_dir,
            "a symlink to a file is not a directory"
        );

        // Directories sort first, and the symlink has just become one.
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["real", "to-dir", "to-file"]);

        // One probe for the whole listing, however many links it holds.
        let probes = stub.commands().iter().filter(|c| c.contains("-d")).count();
        assert_eq!(probes, 1, "the symlink check must not be one call per link");
    }

    #[test]
    fn listing_looks_through_a_symlinked_directory() {
        let stub = Stub::new("total 0\n", "");
        list_by_running("/opt/link-to-dir", |cmd| stub.run(cmd)).unwrap();
        // Without the trailing slash both find and ls describe the link
        // itself, so entering a symlinked directory would show one entry.
        for cmd in stub.commands().iter().filter(|c| c.starts_with("ls ")) {
            assert!(
                cmd.contains("'/opt/link-to-dir/'"),
                "listing must follow the link: {cmd}"
            );
        }
    }

    #[test]
    fn a_symlink_probe_is_skipped_when_there_are_none() {
        let listing = "-rw-r--r-- 1 root root 10 2024-04-05 12:30 plain.txt\n";
        let stub = Stub::new(listing, "");
        list_by_running("/opt", |cmd| stub.run(cmd)).unwrap();
        assert_eq!(
            stub.commands().len(),
            2,
            "just the find attempt and the ls: {:?}",
            stub.commands()
        );
    }

    #[test]
    fn ls_long_iso_is_parsed() {
        let out = "total 12\n\
                   drwxr-xr-x 2 root root 4096 2024-04-05 12:30 conf.d\n\
                   -rw-r----- 1 root shadow 1042 2024-04-01 09:00 shadow\n\
                   lrwxrwxrwx 1 root root 7 2024-03-02 11:11 rc.local -> ../init.d/rc\n";
        let e = parse_ls(out);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].name, "conf.d");
        assert_eq!(e[0].kind, EntryKind::Dir);
        let link = e.iter().find(|x| x.name == "rc.local").unwrap();
        assert_eq!(link.kind, EntryKind::Symlink);
        assert_eq!(link.link_target.as_deref(), Some("../init.d/rc"));
        let shadow = e.iter().find(|x| x.name == "shadow").unwrap();
        assert_eq!(shadow.size, 1042);
    }

    #[test]
    fn ls_default_date_format_is_parsed() {
        let out = "-rw-r--r-- 1 root root 220 Apr  5 12:30 hosts\n";
        let e = parse_ls(out);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "hosts", "the extra date column is skipped");
    }

    #[test]
    fn ls_keeps_names_with_spaces() {
        let out = "-rw-r--r-- 1 root root 220 2024-04-05 12:30 two words.conf\n";
        let e = parse_ls(out);
        assert_eq!(e[0].name, "two words.conf");
    }
}

/// Tests that need a real SSH server. They are skipped unless
/// `SSHMAN_TEST_HOST` is set, so `cargo test` stays hermetic:
///
/// ```text
/// docker run -d --name sshman-test -p 2222:22 sshman-test
/// SSHMAN_TEST_HOST=localhost SSHMAN_TEST_PORT=2222 \
///   SSHMAN_TEST_USER=tester SSHMAN_TEST_PASS=testpass \
///   SSHMAN_TEST_SUDO_PASS=testpass cargo test -- --ignored --test-threads=1
/// ```
///
/// Run them with `HOME` pointed at a scratch directory: accepting the test
/// server's host key writes to `$HOME/.ssh/known_hosts`.
#[cfg(test)]
mod live {
    use super::*;

    fn env(key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn connect() -> Option<Conn> {
        connect_as(&env("SSHMAN_TEST_USER").unwrap_or_else(|| "tester".into()))
    }

    /// The same, as a named user. The test server has more than one, because
    /// what their login shell is turns out to matter.
    fn connect_as(user: &str) -> Option<Conn> {
        let host = env("SSHMAN_TEST_HOST")?;
        let opts = ConnectOpts {
            host,
            port: env("SSHMAN_TEST_PORT")
                .and_then(|p| p.parse().ok())
                .unwrap_or(22),
            user: user.to_string(),
            password: env("SSHMAN_TEST_PASS"),
            key_path: None,
            key_passphrase: None,
            accept_new_host_key: true,
            replace_host_key: false,
        };
        Some(Conn::connect(&opts).expect("connect to the test server"))
    }

    fn no_progress() -> impl FnMut(u64) {
        |_| {}
    }

    /// `Conn` is not `Debug`, so `expect_err` is unavailable here.
    fn expect_refused(result: Result<Conn, ConnectError>, why: &str) -> ConnectError {
        match result {
            Ok(_) => panic!("{why}"),
            Err(e) => e,
        }
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn a_key_installs_where_the_login_shell_is_not_a_posix_shell() {
        // sshd runs a channel `exec` through the *login user's* shell, and
        // every command in this file is written in POSIX shell. Against a
        // user whose shell is fish, installing a key used to come back as
        // `fish: expected end of the statement but found string` — a message
        // about sshman wearing the clothes of a message about your key.
        let Some(c) = connect_as("fishy") else { return };

        // Its own shell really is fish, or this test is proving nothing.
        let (shell, _, _) = c.exec("getent passwd $(id -un) | cut -d: -f7").unwrap();
        assert_eq!(shell.trim(), "/usr/bin/fish", "the test user changed");

        c.exec("rm -f ~/.ssh/authorized_keys").ok();
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIsshmantestkeysshmantestkeysshmantest                    sshman@test";
        assert!(
            c.install_public_key(key).expect("the key would not install"),
            "it said the key was already there"
        );

        // Exactly once, and again without adding a second copy.
        let count = c
            .exec(&format!(
                "grep -c -F -- {} ~/.ssh/authorized_keys",
                sh_quote(key)
            ))
            .unwrap();
        assert_eq!(count.0.trim(), "1");
        assert!(
            !c.install_public_key(key).expect("the second install failed"),
            "a second install duplicated the entry"
        );

        // And with the permissions sshd insists on, or it ignores the file.
        let (modes, _, _) = c
            .exec("stat -c '%a' ~/.ssh ~/.ssh/authorized_keys")
            .unwrap();
        assert_eq!(modes.split_whitespace().collect::<Vec<_>>(), ["700", "600"]);
        c.exec("rm -f ~/.ssh/authorized_keys").ok();
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn lists_and_transfers_as_the_login_user() {
        let Some(mut c) = connect() else { return };

        let entries = c.list(&c.home.clone(), false).expect("list home");
        assert!(
            entries.iter().any(|e| e.name == ".bashrc"),
            "expected a dotfile in the home directory, got {:?}",
            entries.iter().map(|e| &e.name).collect::<Vec<_>>()
        );

        // Upload a file, confirm the server sees the same bytes, pull it back.
        let dir = std::env::temp_dir().join(format!("sshman-it-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hello world.txt");
        std::fs::write(&src, b"round trip\n").unwrap();

        let home = c.home.clone();
        c.upload(&src, &home, false, &mut no_progress())
            .expect("upload");

        let listed = c.list(&home, false).unwrap();
        assert!(listed.iter().any(|e| e.name == "hello world.txt"));

        let (out, _, code) = c
            .exec(&format!(
                "cat {}",
                sh_quote(&rjoin(&home, "hello world.txt"))
            ))
            .unwrap();
        assert_eq!(code, 0);
        assert_eq!(out, "round trip\n", "names with spaces survive quoting");

        let back = dir.join("back");
        std::fs::create_dir_all(&back).unwrap();
        c.download(
            &rjoin(&home, "hello world.txt"),
            &back,
            false,
            &mut no_progress(),
        )
        .expect("download");
        assert_eq!(
            std::fs::read_to_string(back.join("hello world.txt")).unwrap(),
            "round trip\n"
        );

        c.remove(&rjoin(&home, "hello world.txt"), false)
            .expect("remove");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn directory_trees_round_trip() {
        let Some(mut c) = connect() else { return };
        let home = c.home.clone();

        let dir = std::env::temp_dir().join(format!("sshman-tree-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("tree/nested")).unwrap();
        std::fs::write(dir.join("tree/top.txt"), b"top").unwrap();
        std::fs::write(dir.join("tree/nested/deep.txt"), b"deep").unwrap();

        c.upload(&dir.join("tree"), &home, false, &mut no_progress())
            .expect("upload tree");
        let (out, _, _) = c
            .exec(&format!(
                "cat {}",
                sh_quote(&rjoin(&home, "tree/nested/deep.txt"))
            ))
            .unwrap();
        assert_eq!(out, "deep", "nested files are uploaded too");

        let back = dir.join("back");
        std::fs::create_dir_all(&back).unwrap();
        c.download(&rjoin(&home, "tree"), &back, false, &mut no_progress())
            .expect("download tree");
        assert_eq!(
            std::fs::read_to_string(back.join("tree/nested/deep.txt")).unwrap(),
            "deep"
        );

        c.remove(&rjoin(&home, "tree"), false).expect("cleanup");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn sudo_mode_reveals_and_writes_root_only_paths() {
        let Some(mut c) = connect() else { return };
        c.sudo_password = env("SSHMAN_TEST_SUDO_PASS");
        c.check_sudo().expect("sudo should work for the test user");

        // Lay down our own fixtures rather than trusting the image to be
        // pristine — an earlier run may well have edited these files.
        c.exec_sudo(
            "mkdir -p /root/secretdir \
             && echo 'top secret contents' > /root/secretdir/secret.txt \
             && echo 'db_password=hunter2' > /etc/app-private.conf \
             && chmod 600 /etc/app-private.conf \
             && echo 'a file with spaces' > '/root/two words.txt'",
        )
        .expect("set up fixtures");

        // The whole point: SFTP cannot see this, sudo can.
        assert!(
            c.list("/root", false).is_err(),
            "/root must be unreadable as the login user"
        );
        let root = c.list("/root", true).expect("sudo list /root");
        assert!(
            root.iter()
                .any(|e| e.name == "secretdir" && e.is_dir_like())
        );
        assert!(
            root.iter().any(|e| e.name == "two words.txt"),
            "names with spaces survive the find parser"
        );

        // Read a 0600 root-owned file by staging it through /tmp.
        let dir = std::env::temp_dir().join(format!("sshman-sudo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        c.download("/etc/app-private.conf", &dir, true, &mut no_progress())
            .expect("sudo download");
        assert_eq!(
            std::fs::read_to_string(dir.join("app-private.conf")).unwrap(),
            "db_password=hunter2\n"
        );

        // And a directory tree owned by root.
        c.download("/root/secretdir", &dir, true, &mut no_progress())
            .expect("sudo download dir");
        assert_eq!(
            std::fs::read_to_string(dir.join("secretdir/secret.txt")).unwrap(),
            "top secret contents\n"
        );

        // Write into a root-only directory.
        let src = dir.join("planted.txt");
        std::fs::write(&src, b"written as root\n").unwrap();
        c.upload(&src, "/root", true, &mut no_progress())
            .expect("sudo upload");
        let (out, _, code) = c.exec_sudo("cat /root/planted.txt").unwrap();
        assert_eq!(code, 0);
        assert_eq!(out, "written as root\n");

        // Staging directories must not be left behind in /tmp.
        let (leftovers, _, _) = c.exec("ls -1 /tmp | grep '^\\.sshman-' | wc -l").unwrap();
        assert_eq!(leftovers.trim(), "0", "staging dirs are cleaned up");

        c.remove("/root/planted.txt", true).expect("sudo remove");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Saving an edit rewrites the contents and nothing else. The staged copy
    /// a sudo transfer goes through is owned by the login user, so a `cp` that
    /// preserved its attributes would quietly hand root's files over.
    #[test]
    #[ignore = "needs a live SSH server"]
    fn sudo_saves_keep_the_destination_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let Some(mut c) = connect() else { return };
        c.sudo_password = env("SSHMAN_TEST_SUDO_PASS");
        c.check_sudo().expect("sudo should work for the test user");

        let dir = std::env::temp_dir().join(format!("sshman-perm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // 0640 is the interesting mode: root can write it and the login user
        // cannot read it, so the round trip has to go through staging.
        c.exec_sudo(
            "echo original > /etc/sshman-perm.conf \
             && chmod 640 /etc/sshman-perm.conf \
             && chown root:root /etc/sshman-perm.conf",
        )
        .expect("set up the fixture");

        c.download("/etc/sshman-perm.conf", &dir, true, &mut no_progress())
            .expect("sudo download");
        let temp = dir.join("sshman-perm.conf");

        // Stand in for the editor, which writes with the local umask and may
        // replace the file outright rather than writing through it.
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, b"edited\n").unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).unwrap();

        c.upload(&temp, "/etc", true, &mut no_progress())
            .expect("sudo upload");

        let (meta, _, _) = c
            .exec_sudo("stat -c '%a %U:%G' /etc/sshman-perm.conf")
            .unwrap();
        assert_eq!(
            meta.trim(),
            "640 root:root",
            "a saved edit must leave mode and ownership alone"
        );
        let (body, _, _) = c.exec_sudo("cat /etc/sshman-perm.conf").unwrap();
        assert_eq!(body, "edited\n", "the contents are the part that changes");

        // A file that is not there yet has no attributes to keep, and lands
        // owned by the user doing the copying rather than the login user.
        let fresh = dir.join("sshman-perm-new.conf");
        std::fs::write(&fresh, b"new\n").unwrap();
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o600)).unwrap();
        c.upload(&fresh, "/etc", true, &mut no_progress())
            .expect("sudo upload of a new file");
        let (meta, _, _) = c
            .exec_sudo("stat -c '%a %U:%G' /etc/sshman-perm-new.conf")
            .unwrap();
        assert_eq!(meta.trim(), "600 root:root");

        c.exec_sudo("rm -f /etc/sshman-perm.conf /etc/sshman-perm-new.conf")
            .expect("cleanup");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Copying and moving inside the server, which is what a paste does when
    /// one pane fills the screen and there is no other side to copy to.
    #[test]
    #[ignore = "needs a live SSH server"]
    fn pastes_happen_entirely_on_the_far_end() {
        use crate::fileops::{Action, paste_command};

        let Some(mut c) = connect() else { return };
        c.sudo_password = env("SSHMAN_TEST_SUDO_PASS");
        let home = c.home.clone();
        let (src, dst) = (rjoin(&home, "paste/src"), rjoin(&home, "paste/dst"));

        // A name with a space in it, because the command loops over the names
        // in the shell and that is where quoting goes wrong.
        c.exec(&format!(
            "rm -rf {home}/paste && mkdir -p {src} {dst} \
             && echo hello > {src}/'a file.txt' && chmod 640 {src}/'a file.txt'",
            home = sh_quote(&home),
            src = sh_quote(&src),
            dst = sh_quote(&dst),
        ))
        .expect("set up the fixture");

        let names = vec!["a file.txt".to_string()];
        let (_, _, code) = c
            .run(&paste_command(&src, &names, &dst, Action::Copy), false)
            .unwrap();
        assert_eq!(code, 0, "the copy should have run");
        let (meta, _, _) = c
            .exec(&format!("stat -c '%a' {}/'a file.txt'", sh_quote(&dst)))
            .unwrap();
        assert_eq!(meta.trim(), "640", "a copy carries the original's mode");

        // Again, onto what is now there: nothing may be overwritten, and the
        // message has to name the file in the way.
        let (_, err, code) = c
            .run(&paste_command(&src, &names, &dst, Action::Copy), false)
            .unwrap();
        assert_ne!(code, 0, "the second copy must refuse");
        assert!(
            err.contains("a file.txt is already there"),
            "and say which file: {err:?}"
        );

        // A move empties the source rather than duplicating it.
        let onward = rjoin(&home, "paste/onward");
        c.mkdir(&onward, false).expect("mkdir");
        let (_, _, code) = c
            .run(&paste_command(&dst, &names, &onward, Action::Move), false)
            .unwrap();
        assert_eq!(code, 0);
        let (out, _, _) = c
            .exec(&format!(
                "ls {} | wc -l; ls {}",
                sh_quote(&dst),
                sh_quote(&onward)
            ))
            .unwrap();
        assert!(out.starts_with('0'), "the source is empty now: {out:?}");
        assert!(out.contains("a file.txt"), "and the file is over there");

        // And with sudo it reaches where the login user cannot go.
        c.check_sudo().expect("sudo should work for the test user");
        let (_, _, code) = c
            .run(&paste_command(&src, &names, "/root", Action::Copy), true)
            .unwrap();
        assert_eq!(code, 0, "a paste as root lands in /root");
        let (out, _, _) = c.exec_sudo("cat /root/'a file.txt'").unwrap();
        assert_eq!(out, "hello\n");

        c.exec_sudo("rm -f /root/'a file.txt'").expect("cleanup");
        c.exec(&format!("rm -rf {}/paste", sh_quote(&home)))
            .expect("cleanup");
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn exec_reports_output_and_exit_status() {
        let Some(mut c) = connect() else { return };
        c.sudo_password = env("SSHMAN_TEST_SUDO_PASS");
        let (out, _, code) = c.exec("echo hello").unwrap();
        assert_eq!((out.as_str(), code), ("hello\n", 0));

        let (_, err, code) = c.exec("ls /definitely-not-here").unwrap();
        assert_ne!(code, 0);
        assert!(!err.is_empty(), "stderr is captured separately");

        let (out, _, _) = c.exec_sudo("id -un").unwrap();
        assert_eq!(out.trim(), "root", "sudo actually elevates");
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn unknown_and_changed_host_keys_are_caught() {
        let Some(host) = env("SSHMAN_TEST_HOST") else {
            return;
        };
        let port: u16 = env("SSHMAN_TEST_PORT")
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);
        let base = ConnectOpts {
            host,
            port,
            user: env("SSHMAN_TEST_USER").unwrap_or_else(|| "tester".into()),
            password: env("SSHMAN_TEST_PASS"),
            key_path: None,
            key_passphrase: None,
            accept_new_host_key: false,
            replace_host_key: false,
        };

        // known_hosts lives under HOME, so pointing HOME at an empty directory
        // is what makes this host "unknown".
        let empty = std::env::temp_dir().join(format!("sshman-hk-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let real_home = std::env::var("HOME").unwrap();
        // SAFETY: single-threaded (`--test-threads=1`) and restored below.
        unsafe { std::env::set_var("HOME", &empty) };

        let err = expect_refused(Conn::connect(&base), "an unknown host must not connect");
        assert!(
            matches!(err, ConnectError::UnknownHostKey { .. }),
            "expected UnknownHostKey, got {err}"
        );

        // Accepting once records the key; connecting again is then silent.
        let accepting = ConnectOpts {
            accept_new_host_key: true,
            ..base.clone()
        };
        Conn::connect(&accepting).expect("accepting the key lets us in");
        Conn::connect(&base).expect("the key is remembered");

        // Swap the stored key for a different — but still well formed — key of
        // the same type. Mangling the base64 instead would just make the line
        // unparseable, which libssh2 skips, and the test would prove nothing.
        let kh = empty.join(".ssh/known_hosts");
        let line = std::fs::read_to_string(&kh).unwrap();
        // Fields are: host, key type, base64 key, then a free-form comment.
        let mut parts: Vec<String> = line.split_whitespace().map(String::from).collect();
        let mut blob = base64::engine::general_purpose::STANDARD
            .decode(&parts[2])
            .expect("stored key is valid base64");
        // Flip a bit late in the blob, well past the key-type prefix.
        let last = blob.len() - 1;
        blob[last] ^= 0xff;
        parts[2] = base64::engine::general_purpose::STANDARD.encode(&blob);
        std::fs::write(&kh, format!("{}\n", parts.join(" "))).unwrap();

        let err = expect_refused(
            Conn::connect(&accepting),
            "a changed host key must be refused even with accept_new set",
        );
        assert!(
            matches!(err, ConnectError::HostKeyMismatch { .. }),
            "expected HostKeyMismatch, got {err}"
        );

        unsafe { std::env::set_var("HOME", real_home) };
        std::fs::remove_dir_all(&empty).ok();
    }

    #[test]
    #[ignore = "needs a live SSH server"]
    fn resolve_dir_expands_shell_syntax() {
        let Some(c) = connect() else { return };
        assert_eq!(c.resolve_dir("~", false).unwrap(), c.home);
        assert_eq!(c.resolve_dir("~/", false).unwrap(), c.home);
        assert_eq!(c.resolve_dir("/etc/../etc", false).unwrap(), "/etc");
        assert!(c.resolve_dir("/no/such/place", false).is_err());
        assert!(
            c.resolve_dir("/etc/hostname", false).is_err(),
            "a regular file is not somewhere we can navigate to"
        );
    }
}
