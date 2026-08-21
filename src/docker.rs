//! Containers as browsing targets.
//!
//! A container is reached by running `<runtime> exec` rather than by opening a
//! network connection, but everything above this layer treats it exactly like
//! a server: the same panes, transfers, shell and elevation.
//!
//! The runtime is Docker or Podman. Podman's CLI is deliberately
//! docker-compatible for everything used here — `ps --format`, `inspect -f`,
//! `exec -u/-it`, `cp -a` — so the only thing that varies is which binary to
//! call, and that is discovered on whichever machine the containers live on.
//!
//! That machine is either this one or, for a container on a server, the far
//! end of an existing SSH connection. Those are the only two cases, and they
//! differ only in how a command string is dispatched.

use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::sshconn::{Conn, list_by_running};
use crate::types::{FileEntry, rbasename, rjoin, sh_quote};

/// Runtimes we look for, in the order we prefer them.
const KNOWN_RUNTIMES: [&str; 2] = ["docker", "podman"];

/// Find the container runtime on a machine.
///
/// `preferred` is an explicit choice — a name or an absolute path — and is
/// checked rather than trusted, so a typo is reported here instead of turning
/// into a puzzling failure later.
pub fn detect_runtime(via: Option<&Conn>, preferred: Option<&str>) -> Result<String> {
    let candidates: Vec<&str> = match preferred {
        Some(name) => vec![name],
        None => KNOWN_RUNTIMES.to_vec(),
    };

    let mut tried = Vec::new();
    for candidate in &candidates {
        // `command -v` is the POSIX way to ask, and works in the minimal
        // shells a server might give us.
        let probe = format!("command -v {} >/dev/null 2>&1", sh_quote(candidate));
        if let Ok((_, _, 0)) = run_on_host(via, &probe) {
            return Ok((*candidate).to_string());
        }
        tried.push(*candidate);
    }

    let where_ = if via.is_some() {
        "on the server"
    } else {
        "on this machine"
    };
    bail!(
        "no container runtime {where_} (looked for {})",
        tried.join(", ")
    );
}

/// One container offered in the picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
}

/// Ask a docker daemon what is running. `via` selects the daemon: `None` for
/// this machine, or a live SSH connection for a server's.
pub fn list_containers(via: Option<&Conn>, runtime: &str) -> Result<Vec<Container>> {
    // Tab-separated so names and images with spaces stay in one field.
    let cmd = format!(
        "{} ps --no-trunc --format '{{{{.ID}}}}\t{{{{.Names}}}}\t{{{{.Image}}}}\t{{{{.Status}}}}'",
        sh_quote(runtime)
    );
    let (out, err, code) = run_on_host(via, &cmd)?;
    if code != 0 {
        let detail = first_line(if err.trim().is_empty() { &out } else { &err });
        bail!("{detail}");
    }
    Ok(parse_container_list(&out))
}

pub fn parse_container_list(out: &str) -> Vec<Container> {
    let mut containers = Vec::new();
    for line in out.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 || fields[0].trim().is_empty() {
            continue;
        }
        containers.push(Container {
            id: fields[0].trim().to_string(),
            // Podman models names as a list, and some versions render that
            // through the template as `[name]`.
            name: fields[1].trim().trim_matches(['[', ']']).to_string(),
            image: fields[2].trim().to_string(),
            status: fields[3].trim().to_string(),
        });
    }
    containers.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    containers
}

/// Run a command on whichever machine the docker daemon lives on.
fn run_on_host(via: Option<&Conn>, cmd: &str) -> Result<(String, String, i32)> {
    match via {
        Some(conn) => conn.exec(cmd),
        None => {
            let out = Command::new("/bin/sh").arg("-c").arg(cmd).output()?;
            Ok((
                String::from_utf8_lossy(&out.stdout).to_string(),
                String::from_utf8_lossy(&out.stderr).to_string(),
                out.status.code().unwrap_or(-1),
            ))
        }
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

/// A container opened as a browsing target.
pub struct DockerConn {
    /// `None` when the daemon is on this machine; otherwise the SSH connection
    /// its commands are dispatched through, which also carries file staging.
    ssh: Option<Conn>,
    /// The id we address the container by — stable even if it is renamed.
    id: String,
    /// The friendly name, for the tab.
    pub name: String,
    /// Which binary drives it: `docker` or `podman`.
    pub runtime: String,
    pub user: String,
    pub home: String,
}

impl DockerConn {
    /// Attach to a container, learning who we are and where we start.
    pub fn open(ssh: Option<Conn>, container: &str, runtime: &str) -> Result<Self> {
        let mut conn = Self {
            ssh,
            id: container.to_string(),
            name: container.to_string(),
            runtime: runtime.to_string(),
            user: "root".into(),
            home: "/".into(),
        };

        // Prove the container is reachable before anything else reports a
        // confusing error further along.
        let (out, err, code) = conn.exec_in("printf ready", false)?;
        if code != 0 || !out.contains("ready") {
            let detail = first_line(if err.trim().is_empty() { &out } else { &err });
            bail!("cannot exec into {container}: {detail}");
        }

        if let Ok((out, _, 0)) = conn.exec_in("id -un 2>/dev/null || echo root", false) {
            let user = out.trim();
            if !user.is_empty() {
                conn.user = user.to_string();
            }
        }
        // The image's WORKDIR is where a shell would land, so start there.
        if let Ok((out, _, 0)) = conn.exec_in("pwd", false) {
            let dir = out.trim();
            if dir.starts_with('/') {
                conn.home = dir.to_string();
            }
        }
        if let Ok((out, _, 0)) = conn.run_host(&format!(
            "{} inspect -f '{{{{.Name}}}}' {}",
            sh_quote(runtime),
            sh_quote(&conn.id)
        )) {
            let name = out.trim().trim_start_matches('/');
            if !name.is_empty() {
                conn.name = name.to_string();
            }
        }
        Ok(conn)
    }

    /// A label for the tab: `container` locally, `container@server` remotely.
    pub fn label(&self) -> String {
        match &self.ssh {
            Some(conn) => format!("{}@{}", self.name, conn.host),
            None => self.name.clone(),
        }
    }

    fn run_host(&self, cmd: &str) -> Result<(String, String, i32)> {
        run_on_host(self.ssh.as_ref(), cmd)
    }

    /// Run a command inside the container. `elevated` is the container's
    /// answer to sudo: `-u 0:0` needs no password, because reaching the docker
    /// daemon at all already implies that much authority.
    pub fn exec_in(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        let as_root = if elevated { "-u 0:0 " } else { "" };
        self.run_host(&format!(
            "{} exec {as_root}{} /bin/sh -c {}",
            sh_quote(&self.runtime),
            sh_quote(&self.id),
            sh_quote(cmd)
        ))
    }

    pub fn is_alive(&self) -> bool {
        if let Some(conn) = &self.ssh
            && !conn.is_alive()
        {
            return false;
        }
        matches!(
            self.run_host(&format!(
                "{} inspect -f '{{{{.State.Running}}}}' {}",
                sh_quote(&self.runtime),
                sh_quote(&self.id)
            )),
            Ok((out, _, 0)) if out.trim() == "true"
        )
    }

    /// Check that `-u 0:0` really lands us as root.
    pub fn check_elevation(&self) -> Result<()> {
        let (out, err, code) = self.exec_in("id -u", true)?;
        if code != 0 || out.trim() != "0" {
            let detail = first_line(if err.trim().is_empty() { &out } else { &err });
            bail!("{detail}");
        }
        Ok(())
    }

    pub fn list(&self, path: &str, elevated: bool) -> Result<Vec<FileEntry>> {
        list_by_running(path, |cmd| self.exec_in(cmd, elevated))
    }

    pub fn resolve_dir(&self, path: &str, elevated: bool) -> Result<String> {
        let expanded = if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            rjoin(&self.home, rest)
        } else {
            path.to_string()
        };
        let (out, _, code) = self.exec_in(
            &format!("cd {} 2>/dev/null && pwd", sh_quote(&expanded)),
            elevated,
        )?;
        if code == 0 && !out.trim().is_empty() {
            return Ok(out.trim().to_string());
        }
        bail!("no such directory: {path}");
    }

    pub fn mkdir(&self, path: &str, elevated: bool) -> Result<()> {
        self.must(&format!("mkdir {}", sh_quote(path)), elevated)
    }

    pub fn rename(&self, from: &str, to: &str, elevated: bool) -> Result<()> {
        self.must(
            &format!("mv -- {} {}", sh_quote(from), sh_quote(to)),
            elevated,
        )
    }

    pub fn remove(&self, path: &str, elevated: bool) -> Result<()> {
        self.must(&format!("rm -rf -- {}", sh_quote(path)), elevated)
    }

    fn must(&self, cmd: &str, elevated: bool) -> Result<()> {
        let (out, err, code) = self.exec_in(cmd, elevated)?;
        if code != 0 {
            let detail = first_line(if err.trim().is_empty() { &out } else { &err });
            bail!("{detail}");
        }
        Ok(())
    }

    pub fn tree_size(&self, path: &str, elevated: bool) -> u64 {
        let cmd = format!(
            "du -sb -- {} 2>/dev/null | cut -f1 || du -sk -- {} 2>/dev/null | cut -f1",
            sh_quote(path),
            sh_quote(path)
        );
        match self.exec_in(&cmd, elevated) {
            Ok((out, _, 0)) => out.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    /// Copy out of the container.
    ///
    /// `docker cp` runs with the daemon's authority, so it reads paths the
    /// container user could not — elevation never enters into it.
    pub fn download(
        &mut self,
        path: &str,
        dest_dir: &Path,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let name = rbasename(path);
        match self.ssh.take() {
            // Local daemon: straight onto the filesystem.
            None => {
                let cmd = format!(
                    "{} cp -a {}:{} {}",
                    sh_quote(&self.runtime),
                    sh_quote(&self.id),
                    sh_quote(path),
                    sh_quote(&dest_dir.to_string_lossy())
                );
                let result = self.must_host(&cmd);
                if result.is_ok() {
                    progress(crate::local::tree_size(&dest_dir.join(&name)));
                }
                result
            }
            // Remote daemon: out of the container onto the server, then over
            // SFTP, which is the part worth showing progress for.
            Some(mut conn) => {
                let stage = make_stage(&conn)?;
                let staged = rjoin(&stage, &name);
                let result = (|| -> Result<()> {
                    must_ssh(
                        &conn,
                        &format!(
                            "{} cp -a {}:{} {}",
                            sh_quote(&self.runtime),
                            sh_quote(&self.id),
                            sh_quote(path),
                            sh_quote(&stage)
                        ),
                    )?;
                    conn.download(&staged, dest_dir, false, progress)
                })();
                let _ = conn.exec(&format!("rm -rf -- {}", sh_quote(&stage)));
                self.ssh = Some(conn);
                result
            }
        }
    }

    /// Copy into the container.
    ///
    /// `docker cp` stamps the source's mode and ownership onto whatever it
    /// lands on, existing files included, so saving an edit would rewrite the
    /// file's permissions to those of the temp copy it came back from. There
    /// is no flag that turns that off — `-a` only makes it worse — so the
    /// destination's own attributes are read first and put back afterwards.
    pub fn upload(
        &mut self,
        local: &Path,
        dest_dir: &str,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let dest = rjoin(dest_dir, &name);
        let before = self.attrs(&dest);
        let result = self.copy_in(local, dest_dir, &name, progress);
        if result.is_ok() {
            self.restore_attrs(&dest, before);
        }
        result
    }

    /// Mode and numeric owner of a path in the container, if it is there.
    ///
    /// Read as root: `docker cp` writes with the daemon's authority whatever
    /// the container user could do, so the restore has to reach just as far.
    fn attrs(&self, path: &str) -> Option<(String, String)> {
        let cmd = format!("stat -c '%a %u:%g' -- {}", sh_quote(path));
        let (out, _, 0) = self.exec_in(&cmd, true).ok()? else {
            return None;
        };
        let mut fields = out.split_whitespace();
        Some((fields.next()?.to_string(), fields.next()?.to_string()))
    }

    /// Put back what [`Self::attrs`] saw before the copy. Best effort: the
    /// bytes are already in place, and failing the save over this would be a
    /// worse answer than a file whose mode needs a second look.
    fn restore_attrs(&self, path: &str, before: Option<(String, String)>) {
        let Some((mode, owner)) = before else { return };
        let _ = self.exec_in(
            &format!(
                "chmod {} -- {} && chown {} -- {}",
                sh_quote(&mode),
                sh_quote(path),
                sh_quote(&owner),
                sh_quote(path)
            ),
            true,
        );
    }

    fn copy_in(
        &mut self,
        local: &Path,
        dest_dir: &str,
        name: &str,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        match self.ssh.take() {
            None => {
                let cmd = format!(
                    "{} cp -a {} {}:{}",
                    sh_quote(&self.runtime),
                    sh_quote(&local.to_string_lossy()),
                    sh_quote(&self.id),
                    sh_quote(dest_dir)
                );
                let result = self.must_host(&cmd);
                if result.is_ok() {
                    progress(crate::local::tree_size(local));
                }
                result
            }
            Some(mut conn) => {
                let stage = make_stage(&conn)?;
                let result = (|| -> Result<()> {
                    conn.upload(local, &stage, false, progress)?;
                    must_ssh(
                        &conn,
                        &format!(
                            "{} cp -a {} {}:{}",
                            sh_quote(&self.runtime),
                            sh_quote(&rjoin(&stage, name)),
                            sh_quote(&self.id),
                            sh_quote(dest_dir)
                        ),
                    )
                })();
                let _ = conn.exec(&format!("rm -rf -- {}", sh_quote(&stage)));
                self.ssh = Some(conn);
                result
            }
        }
    }

    fn must_host(&self, cmd: &str) -> Result<()> {
        let (out, err, code) = self.run_host(cmd)?;
        if code != 0 {
            let detail = first_line(if err.trim().is_empty() { &out } else { &err });
            bail!("{detail}");
        }
        Ok(())
    }
}

/// The command line that drops you into a container interactively.
///
/// Minimal images often have no bash, so it picks whichever shell is actually
/// there rather than failing on a missing `/bin/bash`.
///
/// `cwd` is where to start: the directory being browsed, for the shell that
/// takes over the terminal, or `None` for the image's own working directory,
/// which is where a pane's embedded shell opens.
/// Run one command inside a container, with a terminal attached — how an
/// editor pane on a container tab starts the editor.
pub fn exec_command(runtime: &str, container: &str, cmd: &str) -> String {
    format!(
        "{} exec -it {} /bin/sh -c {}",
        sh_quote(runtime),
        sh_quote(container),
        sh_quote(&format!("exec {cmd}"))
    )
}

pub fn interactive_shell_command(runtime: &str, container: &str, cwd: Option<&str>) -> String {
    let workdir = match cwd.filter(|p| !p.is_empty()) {
        Some(path) => format!("-w {} ", sh_quote(path)),
        None => String::new(),
    };
    format!(
        "{} exec -it {}{} /bin/sh -c {}",
        sh_quote(runtime),
        workdir,
        sh_quote(container),
        sh_quote("exec $(command -v bash || command -v ash || command -v sh)")
    )
}

/// A scratch directory on the server, owned by the login user, for moving
/// files between SFTP and `<runtime> cp`.
fn make_stage(conn: &Conn) -> Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = format!("/tmp/.sshman-docker-{nanos}");
    must_ssh(conn, &format!("mkdir -p {}", sh_quote(&path)))?;
    Ok(path)
}

fn must_ssh(conn: &Conn, cmd: &str) -> Result<()> {
    let (out, err, code) = conn.exec(cmd)?;
    if code != 0 {
        let detail = first_line(if err.trim().is_empty() { &out } else { &err });
        bail!("{detail}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_docker_ps_output() {
        let out = "abc123\tweb\tnginx:latest\tUp 3 hours\n\
                   def456\tdb\tpostgres:16\tUp 2 days (healthy)\n";
        let list = parse_container_list(out);
        assert_eq!(list.len(), 2);
        // Sorted by name, so `db` comes before `web`.
        assert_eq!(list[0].name, "db");
        assert_eq!(list[0].image, "postgres:16");
        assert_eq!(list[0].status, "Up 2 days (healthy)");
        assert_eq!(list[1].name, "web");
        assert_eq!(list[1].id, "abc123");
    }

    #[test]
    fn tolerates_blank_and_short_lines() {
        let out = "\nabc\tonly-three\tfields\n\ndef\tok\timg\tUp\n";
        let list = parse_container_list(out);
        assert_eq!(list.len(), 1, "short lines are skipped");
        assert_eq!(list[0].name, "ok");
    }

    #[test]
    fn names_with_spaces_survive() {
        let out = "id1\tmy container\tsome image:1\tUp 1 second\n";
        let list = parse_container_list(out);
        assert_eq!(list[0].name, "my container");
        assert_eq!(list[0].image, "some image:1");
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::*;

    /// `detect_runtime` probes with `command -v`, which we can drive locally by
    /// pointing it at names that do and do not exist.
    #[test]
    fn finds_a_runtime_that_exists() {
        // `sh` is on every machine that can run these tests at all.
        assert_eq!(detect_runtime(None, Some("sh")).unwrap(), "sh");
    }

    #[test]
    fn reports_a_missing_runtime_by_name() {
        let err = detect_runtime(None, Some("definitely-not-a-runtime")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("definitely-not-a-runtime"), "{msg}");
        assert!(msg.contains("this machine"), "{msg}");
    }

    #[test]
    fn prefers_docker_then_podman() {
        assert_eq!(KNOWN_RUNTIMES, ["docker", "podman"]);
    }

    #[test]
    fn podman_bracketed_names_are_unwrapped() {
        // Some podman versions render the name list through the template as
        // `[name]`, since it models names as a slice.
        let list = parse_container_list("id1\t[webapp]\timg:1\tUp 2 minutes\n");
        assert_eq!(list[0].name, "webapp");
    }

    #[test]
    fn shell_command_uses_the_chosen_runtime() {
        let cmd = interactive_shell_command("podman", "my-app", None);
        assert!(cmd.starts_with("'podman' exec -it 'my-app'"), "{cmd}");
    }

    #[test]
    fn a_shell_can_start_where_the_pane_is() {
        let cmd = interactive_shell_command("docker", "my-app", Some("/etc/nginx"));
        assert!(cmd.contains("-w '/etc/nginx' 'my-app'"), "{cmd}");
        // An empty path is no path at all, not a `-w ''` that would fail.
        let cmd = interactive_shell_command("docker", "my-app", Some(""));
        assert!(!cmd.contains("-w"), "{cmd}");
    }
}

/// Live tests against a real container, driven the same way as the SSH ones:
/// set `SSHMAN_TEST_CONTAINER` to a running container's name.
#[cfg(test)]
mod live {
    use super::*;

    fn open() -> Option<DockerConn> {
        let name = std::env::var("SSHMAN_TEST_CONTAINER")
            .ok()
            .filter(|v| !v.is_empty())?;
        let runtime = detect_runtime(None, None).expect("a container runtime");
        Some(DockerConn::open(None, &name, &runtime).expect("open the test container"))
    }

    fn no_progress() -> impl FnMut(u64) {
        |_| {}
    }

    /// `docker cp` would otherwise write the temp copy's mode and uid over the
    /// file being saved, so an edit of a 0600 root-owned file would come back
    /// 0644 owned by whoever happens to share the host uid.
    #[test]
    #[ignore = "needs a live container"]
    fn saves_keep_the_destination_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let Some(mut c) = open() else { return };
        c.must(
            "echo original > /etc/sshman-perm.conf \
             && chmod 600 /etc/sshman-perm.conf \
             && chown 0:0 /etc/sshman-perm.conf",
            true,
        )
        .expect("set up the fixture");

        let dir = std::env::temp_dir().join(format!("sshman-dperm-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        c.download("/etc/sshman-perm.conf", &dir, &mut no_progress())
            .expect("download");

        let temp = dir.join("sshman-perm.conf");
        std::fs::remove_file(&temp).unwrap();
        std::fs::write(&temp, b"edited\n").unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).unwrap();
        c.upload(&temp, "/etc", &mut no_progress()).expect("upload");

        let (meta, _, _) = c
            .exec_in("stat -c '%a %u:%g' /etc/sshman-perm.conf", true)
            .unwrap();
        assert_eq!(
            meta.trim(),
            "600 0:0",
            "a saved edit must leave mode and ownership alone"
        );
        let (body, _, _) = c.exec_in("cat /etc/sshman-perm.conf", true).unwrap();
        assert_eq!(body, "edited\n", "the contents are the part that changes");

        c.must("rm -f /etc/sshman-perm.conf", true)
            .expect("cleanup");
        std::fs::remove_dir_all(&dir).ok();
    }
}
