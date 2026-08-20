//! Background SSH worker.
//!
//! Every operation that touches the network runs on this thread. The UI thread
//! only ever sends a `Req` and drains `Resp`s, so a slow server or a large
//! transfer can never freeze rendering or input.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::backend::{Backend, BackendKind, Descriptor, Target, connect};
use crate::sshconn::ConnectError;
use crate::types::{FileEntry, rbasename, rparent};

#[derive(Debug)]
pub enum Req {
    Connect(Box<Target>),
    /// `None` turns sudo mode off; `Some` sets the password and verifies it.
    SetSudo(Option<String>),
    List {
        path: String,
        sudo: bool,
        seq: u64,
    },
    /// Resolve a user-typed path (`~`, `..`) and then list it.
    GoTo {
        path: String,
        sudo: bool,
        seq: u64,
    },
    Exec {
        cmd: String,
        cwd: String,
        sudo: bool,
    },
    Upload {
        items: Vec<PathBuf>,
        dest: String,
        sudo: bool,
    },
    Download {
        items: Vec<String>,
        dest: PathBuf,
        sudo: bool,
    },
    Mkdir {
        path: String,
        sudo: bool,
    },
    Rename {
        from: String,
        to: String,
        sudo: bool,
    },
    Delete {
        paths: Vec<String>,
        sudo: bool,
    },
    /// Pull a remote file to a temp path so an external editor can open it.
    FetchForEdit {
        path: String,
        sudo: bool,
        editor: String,
    },
    /// Ask this connection's host what containers it is running.
    ListContainers,
    /// Pack files in `dir` into `archive`.
    Archive {
        dir: String,
        names: Vec<String>,
        archive: String,
        sudo: bool,
    },
    /// Unpack `archive` from `dir` into `dest`.
    Extract {
        dir: String,
        archive: String,
        dest: String,
        sudo: bool,
    },
    /// Append our public key to the server's `~/.ssh/authorized_keys`, so the
    /// next login needs no password.
    InstallKey {
        public_key: String,
    },
    /// Send an edited temp file back where it came from.
    PushEdit {
        temp: PathBuf,
        path: String,
        sudo: bool,
    },
    Quit,
}

#[derive(Debug)]
pub enum Resp {
    Connected {
        kind: BackendKind,
        user: String,
        host: String,
        port: u16,
        home: String,
    },
    ConnectFailed {
        msg: String,
        issue: Option<HostKeyIssue>,
        /// The server was reachable and its key was fine; only the
        /// credentials were rejected. The UI uses this to put the cursor
        /// straight in the password box.
        auth_failed: bool,
    },
    Listing {
        path: String,
        entries: Vec<FileEntry>,
        seq: u64,
    },
    ListFailed {
        path: String,
        msg: String,
    },
    ExecDone {
        cmd: String,
        output: String,
        code: i32,
    },
    Progress {
        label: String,
        done: u64,
        total: u64,
    },
    Done {
        msg: String,
        refresh_local: bool,
        refresh_remote: bool,
    },
    Failed(String),
    EditReady {
        temp: PathBuf,
        remote: String,
        sudo: bool,
        editor: String,
    },
    SudoState {
        enabled: bool,
        msg: String,
    },
    /// The connection went away. A reconnect is already being attempted.
    Disconnected {
        reason: String,
    },
    Reconnecting {
        attempt: u32,
        max: u32,
    },
    /// Back on our feet; `home` may have changed if the server did.
    Reconnected {
        home: String,
        /// Whether root access survived the reconnect.
        elevated: bool,
    },
    /// Reconnection gave up. The session is over until the user connects again.
    ReconnectFailed {
        msg: String,
    },
    /// Containers running on this connection's host, and the runtime that
    /// listed them.
    Containers {
        runtime: String,
        list: Vec<crate::docker::Container>,
    },
    TaskStart(String),
    TaskEnd,
}

/// A host key problem the UI has to present as a decision, not just an error.
#[derive(Debug, Clone)]
pub enum HostKeyIssue {
    Unknown {
        fingerprint: String,
        keytype: String,
    },
    Mismatch {
        fingerprint: String,
    },
}

pub fn spawn() -> (Sender<Req>, Receiver<Resp>) {
    let (req_tx, req_rx) = mpsc::channel::<Req>();
    let (resp_tx, resp_rx) = mpsc::channel::<Resp>();
    thread::Builder::new()
        .name("ssh-worker".into())
        .spawn(move || worker_loop(req_rx, resp_tx))
        .expect("spawn ssh worker");
    (req_tx, resp_rx)
}

/// Rate-limited progress reporter. Without the throttle a fast local transfer
/// floods the channel with messages the UI cannot draw anyway.
struct Progress<'a> {
    tx: &'a Sender<Resp>,
    label: String,
    done: u64,
    total: u64,
    last: Instant,
}

impl<'a> Progress<'a> {
    fn new(tx: &'a Sender<Resp>, label: String, total: u64) -> Self {
        let p = Self {
            tx,
            label,
            done: 0,
            total,
            last: Instant::now(),
        };
        p.emit();
        p
    }

    fn emit(&self) {
        let _ = self.tx.send(Resp::Progress {
            label: self.label.clone(),
            done: self.done,
            total: self.total,
        });
    }

    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed() >= Duration::from_millis(80) {
            self.last = Instant::now();
            self.emit();
        }
    }
}

/// How long the worker sits idle before checking the connection is still
/// there. Short enough that a dropped link is noticed while you are reading
/// the screen, long enough to be free.
const IDLE_PROBE: Duration = Duration::from_secs(20);

/// Reconnect attempts, and the pause before each one.
const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(8),
    Duration::from_secs(8),
];

fn worker_loop(rx: Receiver<Req>, tx: Sender<Resp>) {
    let mut conn: Option<Box<dyn Backend>> = None;
    let mut sudo_ready = false;
    // The secret that unlocked root, kept so elevation survives a reconnect.
    let mut elevation_secret: Option<String> = None;
    // Kept so a dropped connection can be rebuilt without asking again.
    let mut saved_target: Option<Target> = None;

    loop {
        let req = match rx.recv_timeout(IDLE_PROBE) {
            Ok(req) => req,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Idle: make sure the link is still there, so a drop is
                // noticed while the user is reading rather than at the moment
                // they next ask for something.
                if conn.as_ref().is_some_and(|c| !c.is_alive()) {
                    recover(
                        &mut conn,
                        &saved_target,
                        &elevation_secret,
                        &mut sudo_ready,
                        &tx,
                        "the connection dropped",
                    );
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        if matches!(req, Req::Quit) {
            break;
        }

        // Connect is the only request valid without a live connection.
        if let Req::Connect(target) = req {
            let _ = tx.send(Resp::TaskStart(format!(
                "connecting to {}…",
                target.describe()
            )));
            match connect(&target) {
                Ok(c) => {
                    let Descriptor {
                        kind,
                        user,
                        host,
                        port,
                        home,
                    } = c.descriptor();
                    let _ = tx.send(Resp::Connected {
                        kind,
                        user,
                        host,
                        port,
                        home,
                    });
                    conn = Some(c);
                    sudo_ready = false;
                    elevation_secret = None;
                    // A host-key grant applies to the attempt it was given
                    // for. Reconnects must not silently re-apply it.
                    saved_target = Some(target.without_host_key_grants());
                }
                Err(e) => {
                    let issue = match &e {
                        ConnectError::UnknownHostKey {
                            fingerprint,
                            keytype,
                        } => Some(HostKeyIssue::Unknown {
                            fingerprint: fingerprint.clone(),
                            keytype: keytype.clone(),
                        }),
                        ConnectError::HostKeyMismatch { fingerprint } => {
                            Some(HostKeyIssue::Mismatch {
                                fingerprint: fingerprint.clone(),
                            })
                        }
                        _ => None,
                    };
                    let _ = tx.send(Resp::ConnectFailed {
                        msg: e.to_string(),
                        issue,
                        auth_failed: matches!(e, ConnectError::Auth(_)),
                    });
                }
            }
            let _ = tx.send(Resp::TaskEnd);
            continue;
        }

        if conn.is_none() {
            let _ = tx.send(Resp::Failed("not connected".into()));
            continue;
        }

        // Sudo requests were accepted by the UI optimistically; if the password
        // never checked out, refuse rather than silently listing as the login
        // user and showing a misleading view.
        if req_wants_sudo(&req) && !sudo_ready {
            let _ = tx.send(Resp::Failed(
                "sudo mode is not active (press s to enable it)".into(),
            ));
            continue;
        }

        let label = describe(&req);
        let _ = tx.send(Resp::TaskStart(label));
        let failed = {
            let c = conn.as_mut().expect("checked above");
            handle(c.as_mut(), req, &tx, &mut sudo_ready, &mut elevation_secret)
        };
        let _ = tx.send(Resp::TaskEnd);

        // An operation can fail for ordinary reasons — no such file, no
        // permission — so only a failure *plus* a dead link means the
        // connection is what broke.
        if failed && conn.as_ref().is_some_and(|c| !c.is_alive()) {
            recover(
                &mut conn,
                &saved_target,
                &elevation_secret,
                &mut sudo_ready,
                &tx,
                "the connection dropped",
            );
        }
    }
}

/// Replace a dead connection, keeping root access if it was on.
fn recover(
    conn: &mut Option<Box<dyn Backend>>,
    target: &Option<Target>,
    elevation_secret: &Option<String>,
    sudo_ready: &mut bool,
    tx: &Sender<Resp>,
    reason: &str,
) {
    let had_elevation = *sudo_ready;
    *conn = None;
    *sudo_ready = false;

    let _ = tx.send(Resp::Disconnected {
        reason: reason.to_string(),
    });

    let Some(target) = target else {
        let _ = tx.send(Resp::ReconnectFailed {
            msg: "no connection details to retry with".into(),
        });
        return;
    };

    let max = RETRY_DELAYS.len() as u32;
    let mut last_error = String::from("unknown error");
    for (index, delay) in RETRY_DELAYS.iter().enumerate() {
        thread::sleep(*delay);
        let _ = tx.send(Resp::Reconnecting {
            attempt: index as u32 + 1,
            max,
        });
        match connect(target) {
            Ok(mut fresh) => {
                // Put root access back, so a drop mid-session does not
                // silently return you to the unprivileged view.
                if had_elevation {
                    *sudo_ready = fresh.enable_elevation(elevation_secret.clone()).is_ok();
                }
                let _ = tx.send(Resp::Reconnected {
                    home: fresh.descriptor().home,
                    elevated: *sudo_ready,
                });
                *conn = Some(fresh);
                return;
            }
            Err(e) => last_error = e.to_string(),
        }
    }
    let _ = tx.send(Resp::ReconnectFailed {
        msg: format!("gave up after {max} attempts: {last_error}"),
    });
}

fn req_wants_sudo(req: &Req) -> bool {
    match req {
        Req::List { sudo, .. }
        | Req::GoTo { sudo, .. }
        | Req::Exec { sudo, .. }
        | Req::Upload { sudo, .. }
        | Req::Download { sudo, .. }
        | Req::Mkdir { sudo, .. }
        | Req::Rename { sudo, .. }
        | Req::Delete { sudo, .. }
        | Req::FetchForEdit { sudo, .. }
        | Req::Archive { sudo, .. }
        | Req::Extract { sudo, .. }
        | Req::PushEdit { sudo, .. } => *sudo,
        _ => false,
    }
}

fn describe(req: &Req) -> String {
    match req {
        Req::List { path, .. } | Req::GoTo { path, .. } => format!("listing {path}…"),
        Req::Exec { cmd, .. } => format!("running {cmd}…"),
        Req::Upload { items, .. } => format!("uploading {} item(s)…", items.len()),
        Req::Download { items, .. } => format!("downloading {} item(s)…", items.len()),
        Req::InstallKey { .. } => "installing your public key…".into(),
        Req::ListContainers => "looking for containers…".into(),
        Req::Archive { archive, .. } => format!("packing {archive}…"),
        Req::Extract { archive, .. } => format!("unpacking {archive}…"),
        Req::Mkdir { path, .. } => format!("creating {path}…"),
        Req::Rename { from, .. } => format!("renaming {from}…"),
        Req::Delete { paths, .. } => format!("deleting {} item(s)…", paths.len()),
        Req::FetchForEdit { path, .. } => format!("fetching {path}…"),
        Req::PushEdit { path, .. } => format!("saving {path}…"),
        Req::SetSudo(_) => "checking sudo…".into(),
        Req::Connect(_) | Req::Quit => String::new(),
    }
}

/// Wraps the response channel so the loop can tell whether the operation it
/// just ran reported a failure — which is its cue to check whether the
/// connection is what broke.
struct Reply<'a> {
    tx: &'a Sender<Resp>,
    failed: bool,
}

impl<'a> Reply<'a> {
    fn send(&mut self, resp: Resp) {
        self.failed |= matches!(resp, Resp::Failed(_) | Resp::ListFailed { .. });
        let _ = self.tx.send(resp);
    }

    /// The bare channel, for helpers that only ever report progress.
    fn tx(&self) -> &'a Sender<Resp> {
        self.tx
    }
}

/// Returns true when the operation reported a failure.
fn handle(
    c: &mut dyn Backend,
    req: Req,
    tx: &Sender<Resp>,
    sudo_ready: &mut bool,
    elevation_secret: &mut Option<String>,
) -> bool {
    let mut reply = Reply { tx, failed: false };
    handle_inner(c, req, &mut reply, sudo_ready, elevation_secret);
    reply.failed
}

fn handle_inner(
    c: &mut dyn Backend,
    req: Req,
    reply: &mut Reply,
    sudo_ready: &mut bool,
    elevation_secret: &mut Option<String>,
) {
    match req {
        Req::Connect(_) | Req::Quit => unreachable!("handled in the loop"),

        Req::SetSudo(secret) => match secret {
            None => {
                c.disable_elevation();
                *sudo_ready = false;
                *elevation_secret = None;
                reply.send(Resp::SudoState {
                    enabled: false,
                    msg: "sudo mode off".into(),
                });
            }
            Some(secret) => match c.enable_elevation(Some(secret.clone())) {
                Ok(msg) => {
                    *sudo_ready = true;
                    *elevation_secret = Some(secret);
                    reply.send(Resp::SudoState { enabled: true, msg });
                }
                Err(e) => {
                    *sudo_ready = false;
                    *elevation_secret = None;
                    reply.send(Resp::SudoState {
                        enabled: false,
                        msg: format!("sudo failed: {e}"),
                    });
                }
            },
        },

        Req::List { path, sudo, seq } => match c.list(&path, sudo) {
            Ok(entries) => {
                reply.send(Resp::Listing { path, entries, seq });
            }
            Err(e) => {
                reply.send(Resp::ListFailed {
                    path,
                    msg: e.to_string(),
                });
            }
        },

        Req::GoTo { path, sudo, seq } => match c.resolve_dir(&path, sudo) {
            Ok(resolved) => match c.list(&resolved, sudo) {
                Ok(entries) => {
                    reply.send(Resp::Listing {
                        path: resolved,
                        entries,
                        seq,
                    });
                }
                Err(e) => {
                    reply.send(Resp::ListFailed {
                        path: resolved,
                        msg: e.to_string(),
                    });
                }
            },
            Err(e) => {
                reply.send(Resp::Failed(e.to_string()));
            }
        },

        Req::Exec { cmd, cwd, sudo } => {
            // Each exec gets a fresh channel, so `cd` cannot persist between
            // commands. Running inside the pane's directory is what users
            // expect from a file manager's command line.
            let full = format!("cd {} 2>/dev/null; {}", crate::types::sh_quote(&cwd), cmd);
            match c.run(&full, sudo) {
                Ok((out, err, code)) => {
                    let mut output = out;
                    if !err.is_empty() {
                        if !output.is_empty() && !output.ends_with('\n') {
                            output.push('\n');
                        }
                        output.push_str(&err);
                    }
                    reply.send(Resp::ExecDone { cmd, output, code });
                }
                Err(e) => {
                    reply.send(Resp::Failed(e.to_string()));
                }
            }
        }

        Req::Upload { items, dest, sudo } => {
            let total: u64 = items.iter().map(|p| crate::local::tree_size(p)).sum();
            let mut prog = Progress::new(reply.tx(), "upload".into(), total);
            let mut failures = Vec::new();
            let count = items.len();
            for item in &items {
                let name = item
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                prog.label = format!("↑ {name}");
                let mut add = |n: u64| prog.add(n);
                if let Err(e) = c.upload(item, &dest, sudo, &mut add) {
                    failures.push(format!("{name}: {e}"));
                }
            }
            prog.done = prog.total;
            prog.emit();
            send_outcome(reply, count, failures, "copied to remote", false, true);
        }

        Req::Download { items, dest, sudo } => {
            let total: u64 = items.iter().map(|p| c.tree_size(p, sudo)).sum();
            let mut prog = Progress::new(reply.tx(), "download".into(), total);
            let mut failures = Vec::new();
            let count = items.len();
            for item in &items {
                prog.label = format!("↓ {}", rbasename(item));
                let mut add = |n: u64| prog.add(n);
                if let Err(e) = c.download(item, &dest, sudo, &mut add) {
                    failures.push(format!("{}: {e}", rbasename(item)));
                }
            }
            prog.done = prog.total;
            prog.emit();
            send_outcome(reply, count, failures, "copied to local", true, false);
        }

        Req::ListContainers => match c.list_containers() {
            Ok((runtime, list)) => reply.send(Resp::Containers { runtime, list }),
            Err(e) => reply.send(Resp::Failed(format!("cannot list containers: {e}"))),
        },

        Req::Archive {
            dir,
            names,
            archive,
            sudo,
        } => {
            let count = names.len();
            let cmd =
                crate::archive::create_command(&dir, &archive, &names, crate::archive::Tar::Remote);
            match c.run(&cmd, sudo) {
                Ok((_, err, 0)) => {
                    // tar warns on stderr about things it coped with — a file
                    // that changed while being read, say. Worth passing on,
                    // but not a failure.
                    let mut msg = format!("packed {count} item(s) into {archive}");
                    if !err.trim().is_empty() {
                        msg.push_str(&format!(" — tar said: {}", first_line(&err)));
                    }
                    reply.send(Resp::Done {
                        msg,
                        refresh_local: false,
                        refresh_remote: true,
                    });
                }
                Ok((out, err, code)) => reply.send(Resp::Failed(format!(
                    "tar failed (exit {code}): {}",
                    first_line(if err.trim().is_empty() { &out } else { &err })
                ))),
                Err(e) => reply.send(Resp::Failed(e.to_string())),
            }
        }

        Req::Extract {
            dir,
            archive,
            dest,
            sudo,
        } => {
            let cmd = crate::archive::extract_command(&dir, &archive, &dest);
            match c.run(&cmd, sudo) {
                Ok((_, _, 0)) => reply.send(Resp::Done {
                    msg: format!("unpacked {archive} into {dest}/"),
                    refresh_local: false,
                    refresh_remote: true,
                }),
                Ok((out, err, code)) => reply.send(Resp::Failed(format!(
                    "tar failed (exit {code}): {}",
                    first_line(if err.trim().is_empty() { &out } else { &err })
                ))),
                Err(e) => reply.send(Resp::Failed(e.to_string())),
            }
        }

        Req::InstallKey { public_key } => match c.install_public_key(&public_key) {
            Ok(true) => {
                reply.send(Resp::Done {
                    msg: "public key installed — this server will not ask for a password again"
                        .into(),
                    refresh_local: false,
                    refresh_remote: false,
                });
            }
            Ok(false) => {
                reply.send(Resp::Done {
                    msg: "your public key was already installed on this server".into(),
                    refresh_local: false,
                    refresh_remote: false,
                });
            }
            Err(e) => {
                reply.send(Resp::Failed(format!("could not install the key: {e}")));
            }
        },

        Req::Mkdir { path, sudo } => match c.mkdir(&path, sudo) {
            Ok(()) => {
                reply.send(Resp::Done {
                    msg: format!("created {path}"),
                    refresh_local: false,
                    refresh_remote: true,
                });
            }
            Err(e) => {
                reply.send(Resp::Failed(e.to_string()));
            }
        },

        Req::Rename { from, to, sudo } => match c.rename(&from, &to, sudo) {
            Ok(()) => {
                reply.send(Resp::Done {
                    msg: format!("renamed to {}", rbasename(&to)),
                    refresh_local: false,
                    refresh_remote: true,
                });
            }
            Err(e) => {
                reply.send(Resp::Failed(e.to_string()));
            }
        },

        Req::Delete { paths, sudo } => {
            let mut failures = Vec::new();
            let count = paths.len();
            for p in &paths {
                if let Err(e) = c.remove(p, sudo) {
                    failures.push(format!("{}: {e}", rbasename(p)));
                }
            }
            send_outcome(reply, count, failures, "deleted", false, true);
        }

        Req::FetchForEdit { path, sudo, editor } => {
            let dir = match make_edit_dir() {
                Ok(d) => d,
                Err(e) => {
                    reply.send(Resp::Failed(format!("cannot create temp dir: {e}")));
                    return;
                }
            };
            let total = c.tree_size(&path, sudo);
            let mut prog = Progress::new(reply.tx(), format!("↓ {}", rbasename(&path)), total);
            let mut add = |n: u64| prog.add(n);
            match c.download(&path, &dir, sudo, &mut add) {
                Ok(()) => {
                    reply.send(Resp::EditReady {
                        temp: dir.join(rbasename(&path)),
                        remote: path,
                        sudo,
                        editor,
                    });
                }
                Err(e) => {
                    reply.send(Resp::Failed(format!("fetch failed: {e}")));
                }
            }
        }

        Req::PushEdit { temp, path, sudo } => {
            let parent = rparent(&path);
            let total = crate::local::tree_size(&temp);
            let mut prog = Progress::new(reply.tx(), format!("↑ {}", rbasename(&path)), total);
            let mut add = |n: u64| prog.add(n);
            match c.upload(&temp, &parent, sudo, &mut add) {
                Ok(()) => {
                    // The temp copy has served its purpose; leaving it around
                    // would scatter stale versions of edited files in /tmp.
                    if let Some(dir) = temp.parent() {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    reply.send(Resp::Done {
                        msg: format!("saved {path}"),
                        refresh_local: false,
                        refresh_remote: true,
                    });
                }
                Err(e) => {
                    reply.send(Resp::Failed(format!(
                        "save failed: {e} (your edit is still at {})",
                        temp.display()
                    )));
                }
            }
        }
    }
}

fn send_outcome(
    reply: &mut Reply,
    count: usize,
    failures: Vec<String>,
    verb: &str,
    refresh_local: bool,
    refresh_remote: bool,
) {
    if failures.is_empty() {
        reply.send(Resp::Done {
            msg: format!("{count} item(s) {verb}"),
            refresh_local,
            refresh_remote,
        });
    } else {
        let ok = count - failures.len();
        reply.send(Resp::Failed(format!(
            "{ok}/{count} {verb}; failed: {}",
            failures.join(", ")
        )));
    }
}

/// Errors from tools are often several lines; the status bar has one.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

fn make_edit_dir() -> std::io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("sshman-edit-{nanos}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
