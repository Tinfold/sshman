//! What the worker talks to: an SSH server, a container, or this machine.
//!
//! Everything above this point — panes, transfers, archives, shells, sudo —
//! is written against this trait, so a container behaves like a server
//! without the rest of the program knowing the difference, and so does a tab
//! pointed at the machine you are sitting at. They differ in only a few
//! visible ways, all captured here: how elevation is granted, whether an SSH
//! key can be installed, and what the tab is called.

use std::path::Path;

use anyhow::{Result, bail};

use crate::docker::DockerConn;
use crate::local::LocalConn;
use crate::sshconn::{Conn, ConnectError, ConnectOpts};
use crate::types::FileEntry;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackendKind {
    Ssh,
    Container,
    /// This machine, reached by running things rather than by connecting.
    Local,
}

/// What to connect to.
#[derive(Clone, Debug)]
pub enum Target {
    Ssh(ConnectOpts),
    /// The machine sshman is running on. Nothing to dial, nothing to drop.
    Local,
    Docker {
        /// `None` for a container on this machine; otherwise the server whose
        /// container runtime holds it.
        via: Option<ConnectOpts>,
        container: String,
        /// `docker` or `podman` — settled when the container was chosen, and
        /// kept so a reconnect or a shell uses the same one.
        runtime: String,
    },
}

impl Target {
    /// The SSH details behind this target, if any — used to reconnect, and to
    /// open a shell on the same server.
    pub fn ssh_opts(&self) -> Option<&ConnectOpts> {
        match self {
            Self::Ssh(opts) => Some(opts),
            Self::Docker { via, .. } => via.as_ref(),
            Self::Local => None,
        }
    }

    /// Clear the one-shot host-key grants, so a reconnect cannot silently
    /// re-apply a decision the user made about a single earlier attempt.
    pub fn without_host_key_grants(&self) -> Self {
        let strip = |mut o: ConnectOpts| {
            o.accept_new_host_key = false;
            o.replace_host_key = false;
            o
        };
        match self {
            Self::Local => Self::Local,
            Self::Ssh(o) => Self::Ssh(strip(o.clone())),
            Self::Docker {
                via,
                container,
                runtime,
            } => Self::Docker {
                via: via.clone().map(strip),
                container: container.clone(),
                runtime: runtime.clone(),
            },
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Local => "this machine".into(),
            Self::Ssh(o) => format!("{}@{}:{}", o.user, o.host, o.port),
            Self::Docker {
                via: None,
                container,
                runtime,
            } => format!("{runtime} container {container}"),
            Self::Docker {
                via: Some(o),
                container,
                runtime,
            } => format!("{runtime} container {container} on {}", o.host),
        }
    }
}

/// Identity of a live connection, for the title bar and tab.
#[derive(Clone, Debug)]
pub struct Descriptor {
    pub kind: BackendKind,
    pub user: String,
    pub host: String,
    pub port: u16,
    pub home: String,
}

/// An explicit runtime choice from `--runtime` or `SSHMAN_CONTAINER_RUNTIME`.
/// Read from the environment so it reaches the worker threads without being
/// threaded through every call.
pub fn preferred_runtime() -> Option<String> {
    std::env::var("SSHMAN_CONTAINER_RUNTIME")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

pub trait Backend: Send {
    fn descriptor(&self) -> Descriptor;
    fn is_alive(&self) -> bool;

    fn run(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)>;

    /// The same, for a line the person typed rather than one sshman built.
    ///
    /// Everything but a tab on this machine already runs a command in the
    /// account's own shell — that is what an SSH exec channel does — so the
    /// default is `run` and only the local backend has anything to say. See
    /// [`crate::local::POSIX_SHELL`] for why it does.
    fn run_yours(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        self.run(cmd, elevated)
    }

    fn list(&self, path: &str, elevated: bool) -> Result<Vec<FileEntry>>;
    fn resolve_dir(&self, path: &str, elevated: bool) -> Result<String>;
    fn mkdir(&self, path: &str, elevated: bool) -> Result<()>;
    fn rename(&self, from: &str, to: &str, elevated: bool) -> Result<()>;
    fn remove(&self, path: &str, elevated: bool) -> Result<()>;
    fn tree_size(&self, path: &str, elevated: bool) -> u64;

    fn download(
        &mut self,
        path: &str,
        dest: &Path,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()>;
    fn upload(
        &mut self,
        local: &Path,
        dest: &str,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()>;

    /// Turn on root access, returning the message to show. `secret` is the
    /// sudo password for a server; a container has no use for it.
    fn enable_elevation(&mut self, secret: Option<String>) -> Result<String>;
    fn disable_elevation(&mut self);

    fn install_public_key(&self, public_key: &str) -> Result<bool>;

    /// Containers running on the machine this connection reaches, along with
    /// the runtime that listed them.
    fn list_containers(&self) -> Result<(String, Vec<crate::docker::Container>)>;
}

/// Build a connection for a target.
pub fn connect(target: &Target) -> Result<Box<dyn Backend>, ConnectError> {
    match target {
        // Nothing to connect to: it is already here.
        Target::Local => Ok(Box::new(LocalConn::open())),
        Target::Ssh(opts) => Ok(Box::new(Conn::connect(opts)?)),
        Target::Docker {
            via,
            container,
            runtime,
        } => {
            let ssh = match via {
                Some(opts) => Some(Conn::connect(opts)?),
                None => None,
            };
            let conn = DockerConn::open(ssh, container, runtime)
                .map_err(|e| ConnectError::Other(e.to_string()))?;
            Ok(Box::new(conn))
        }
    }
}

// ---- SSH -------------------------------------------------------------------

impl Backend for Conn {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            kind: BackendKind::Ssh,
            user: self.user.clone(),
            host: self.host.clone(),
            port: self.port,
            home: self.home.clone(),
        }
    }

    fn is_alive(&self) -> bool {
        Conn::is_alive(self)
    }

    fn run(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        Conn::run(self, cmd, elevated)
    }

    fn list(&self, path: &str, elevated: bool) -> Result<Vec<FileEntry>> {
        Conn::list(self, path, elevated)
    }

    fn resolve_dir(&self, path: &str, elevated: bool) -> Result<String> {
        Conn::resolve_dir(self, path, elevated)
    }

    fn mkdir(&self, path: &str, elevated: bool) -> Result<()> {
        Conn::mkdir(self, path, elevated)
    }

    fn rename(&self, from: &str, to: &str, elevated: bool) -> Result<()> {
        Conn::rename(self, from, to, elevated)
    }

    fn remove(&self, path: &str, elevated: bool) -> Result<()> {
        Conn::remove(self, path, elevated)
    }

    fn tree_size(&self, path: &str, elevated: bool) -> u64 {
        self.remote_tree_size(path, elevated)
    }

    fn download(
        &mut self,
        path: &str,
        dest: &Path,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        Conn::download(self, path, dest, elevated, progress)
    }

    fn upload(
        &mut self,
        local: &Path,
        dest: &str,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        Conn::upload(self, local, dest, elevated, progress)
    }

    fn enable_elevation(&mut self, secret: Option<String>) -> Result<String> {
        // An empty password is meaningful: it is what NOPASSWD sudo expects.
        self.sudo_password = Some(secret.unwrap_or_default());
        match self.check_sudo() {
            Ok(()) => Ok("sudo mode ON — remote pane now runs as root".into()),
            Err(e) => {
                self.sudo_password = None;
                Err(e)
            }
        }
    }

    fn disable_elevation(&mut self) {
        self.sudo_password = None;
    }

    fn install_public_key(&self, public_key: &str) -> Result<bool> {
        Conn::install_public_key(self, public_key)
    }

    fn list_containers(&self) -> Result<(String, Vec<crate::docker::Container>)> {
        // Which runtime the server has is settled here, not assumed.
        let runtime = crate::docker::detect_runtime(Some(self), preferred_runtime().as_deref())?;
        let list = crate::docker::list_containers(Some(self), &runtime)?;
        Ok((runtime, list))
    }
}

// ---- container -------------------------------------------------------------

impl Backend for DockerConn {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            kind: BackendKind::Container,
            user: self.user.clone(),
            host: self.label(),
            // Containers have no port; the UI hides it rather than showing 0.
            port: 0,
            home: self.home.clone(),
        }
    }

    fn is_alive(&self) -> bool {
        DockerConn::is_alive(self)
    }

    fn run(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        self.exec_in(cmd, elevated)
    }

    fn list(&self, path: &str, elevated: bool) -> Result<Vec<FileEntry>> {
        DockerConn::list(self, path, elevated)
    }

    fn resolve_dir(&self, path: &str, elevated: bool) -> Result<String> {
        DockerConn::resolve_dir(self, path, elevated)
    }

    fn mkdir(&self, path: &str, elevated: bool) -> Result<()> {
        DockerConn::mkdir(self, path, elevated)
    }

    fn rename(&self, from: &str, to: &str, elevated: bool) -> Result<()> {
        DockerConn::rename(self, from, to, elevated)
    }

    fn remove(&self, path: &str, elevated: bool) -> Result<()> {
        DockerConn::remove(self, path, elevated)
    }

    fn tree_size(&self, path: &str, elevated: bool) -> u64 {
        DockerConn::tree_size(self, path, elevated)
    }

    fn download(
        &mut self,
        path: &str,
        dest: &Path,
        _elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        // `docker cp` copies with the daemon's authority, so it already reads
        // anything in the container; elevation does not apply.
        DockerConn::download(self, path, dest, progress)
    }

    fn upload(
        &mut self,
        local: &Path,
        dest: &str,
        _elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        DockerConn::upload(self, local, dest, progress)
    }

    fn enable_elevation(&mut self, _secret: Option<String>) -> Result<String> {
        self.check_elevation()?;
        Ok("root mode ON — running as uid 0 inside the container".into())
    }

    fn disable_elevation(&mut self) {}

    fn install_public_key(&self, _public_key: &str) -> Result<bool> {
        bail!("a container is not reached over SSH, so there is no key to install")
    }

    fn list_containers(&self) -> Result<(String, Vec<crate::docker::Container>)> {
        // Containers inside containers are not something we go looking for.
        bail!("this tab is a container; open containers from a server or local tab")
    }
}

// ---- this machine ----------------------------------------------------------

impl Backend for LocalConn {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            kind: BackendKind::Local,
            user: self.user.clone(),
            host: self.host.clone(),
            // No port, the same as a container; the UI hides it.
            port: 0,
            home: self.home.clone(),
        }
    }

    /// This machine is here for as long as sshman is, so there is nothing to
    /// probe and nothing that could drop.
    fn is_alive(&self) -> bool {
        true
    }

    fn run(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        LocalConn::run(self, cmd, elevated)
    }

    fn run_yours(&self, cmd: &str, elevated: bool) -> Result<(String, String, i32)> {
        LocalConn::run_yours(self, cmd, elevated)
    }

    fn list(&self, path: &str, elevated: bool) -> Result<Vec<FileEntry>> {
        LocalConn::list(self, path, elevated)
    }

    fn resolve_dir(&self, path: &str, elevated: bool) -> Result<String> {
        LocalConn::resolve_dir(self, path, elevated)
    }

    fn mkdir(&self, path: &str, elevated: bool) -> Result<()> {
        LocalConn::mkdir(self, path, elevated)
    }

    fn rename(&self, from: &str, to: &str, elevated: bool) -> Result<()> {
        LocalConn::rename(self, from, to, elevated)
    }

    fn remove(&self, path: &str, elevated: bool) -> Result<()> {
        LocalConn::remove(self, path, elevated)
    }

    fn tree_size(&self, path: &str, _elevated: bool) -> u64 {
        crate::local::tree_size(Path::new(path))
    }

    /// Both directions are a copy from one part of this machine to another.
    fn download(
        &mut self,
        path: &str,
        dest: &Path,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        self.copy(path, &dest.to_string_lossy(), elevated, progress)
    }

    fn upload(
        &mut self,
        local: &Path,
        dest: &str,
        elevated: bool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        self.copy(&local.to_string_lossy(), dest, elevated, progress)
    }

    fn enable_elevation(&mut self, secret: Option<String>) -> Result<String> {
        self.set_sudo_password(secret);
        if let Err(e) = self.check_sudo() {
            // Never leave a password behind that did not work.
            self.set_sudo_password(None);
            return Err(e);
        }
        Ok("sudo mode ON — this tab now reads and writes as root".into())
    }

    fn disable_elevation(&mut self) {
        self.set_sudo_password(None);
    }

    fn install_public_key(&self, _public_key: &str) -> Result<bool> {
        bail!("this tab is the machine you are on, so there is no login to set up")
    }

    fn list_containers(&self) -> Result<(String, Vec<crate::docker::Container>)> {
        let runtime = crate::docker::detect_runtime(None, preferred_runtime().as_deref())?;
        let list = crate::docker::list_containers(None, &runtime)?;
        Ok((runtime, list))
    }
}
