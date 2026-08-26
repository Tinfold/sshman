//! Port forwarding.
//!
//! A forward binds a port on this machine and carries every connection to it
//! across SSH to a host and port reachable from the server — the same job as
//! `ssh -L`. It is what makes a service that only listens inside a private
//! network, or only on the server's loopback, reachable from a browser here.
//!
//! Each forward runs on **its own SSH connection**, for the same reason the
//! shells do: the worker's file operations are blocking calls, and a forward
//! has to be pumped continuously. Its session is put in non-blocking mode so
//! one thread can service the listener and every open connection at once.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::sshconn::{ConnectOpts, establish};

/// The address a forward binds when the shorthand does not name one:
/// loopback, so that a port opened for yourself is not quietly published to
/// the network.
pub const LOOPBACK: &str = "127.0.0.1";

/// Where a forward starts and ends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Spec {
    /// Address bound on this machine. [`LOOPBACK`] unless you asked for
    /// another, in which case the forward is reachable from wherever that
    /// address is.
    pub local_host: String,
    /// Port bound on this machine.
    pub local_port: u16,
    /// Host to reach *from the server* — usually its own loopback.
    pub remote_host: String,
    pub remote_port: u16,
}

impl Spec {
    /// Parse the shorthand people actually type — the same shapes `ssh -L`
    /// takes:
    ///
    /// - `3000`                  — 3000 here to 3000 on the server
    /// - `8080:3000`             — 8080 here to 3000 on the server
    /// - `8080:db:5432`          — 8080 here to db:5432, as seen by the server
    /// - `0.0.0.0:8080:db:5432`  — the same, bound where the network can see it
    ///
    /// The first part of the four-part form is the address bound *here*: an
    /// interface's address, `*` for every one of them, or an IPv6 literal in
    /// brackets. Without it a forward binds loopback only.
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            bail!("give a port, like 3000 or 8080:3000");
        }
        let parts = split_parts(text)?;
        let port = |s: &str| -> Result<u16> {
            match s.parse::<u16>() {
                Ok(0) => bail!("0 is not a port"),
                Ok(p) => Ok(p),
                Err(_) => bail!("{s:?} is not a port number"),
            }
        };

        match parts.as_slice() {
            [one] => {
                let p = port(one)?;
                Ok(Self {
                    local_host: LOOPBACK.into(),
                    local_port: p,
                    remote_host: "localhost".into(),
                    remote_port: p,
                })
            }
            [local, remote] => Ok(Self {
                local_host: LOOPBACK.into(),
                local_port: port(local)?,
                remote_host: "localhost".into(),
                remote_port: port(remote)?,
            }),
            [local, host, remote] => {
                if host.is_empty() {
                    bail!("the middle part should be a host name");
                }
                Ok(Self {
                    local_host: LOOPBACK.into(),
                    local_port: port(local)?,
                    remote_host: host.clone(),
                    remote_port: port(remote)?,
                })
            }
            [bind, local, host, remote] => {
                if host.is_empty() {
                    bail!("the third part should be a host name");
                }
                Ok(Self {
                    local_host: bind_address(bind)?,
                    local_port: port(local)?,
                    remote_host: host.clone(),
                    remote_port: port(remote)?,
                })
            }
            _ => bail!("too many colons — try 8080:host:3000, or 0.0.0.0:8080:host:3000"),
        }
    }

    /// Whether this forward is reachable from anywhere but this machine.
    pub fn is_public(&self) -> bool {
        !matches!(self.local_host.as_str(), LOOPBACK | "localhost" | "::1")
    }

    /// The shorthand that would recreate this, kept short when it can be.
    pub fn to_spec_string(&self) -> String {
        // An address of its own has to be said, and saying it means the long
        // form: there is no shorthand with a bind address and nothing else.
        if self.local_host != LOOPBACK {
            return format!(
                "{}:{}:{}:{}",
                bracketed(&self.local_host),
                self.local_port,
                self.remote_host,
                self.remote_port
            );
        }
        if self.remote_host == "localhost" {
            if self.local_port == self.remote_port {
                self.local_port.to_string()
            } else {
                format!("{}:{}", self.local_port, self.remote_port)
            }
        } else {
            format!(
                "{}:{}:{}",
                self.local_port, self.remote_host, self.remote_port
            )
        }
    }

    pub fn describe(&self) -> String {
        let here = match self.local_host.as_str() {
            LOOPBACK => "localhost".to_string(),
            "0.0.0.0" | "::" => "every interface".to_string(),
            other => bracketed(other),
        };
        format!(
            "{here}:{} → {}:{}",
            self.local_port, self.remote_host, self.remote_port
        )
    }
}

/// Split the shorthand on colons, leaving an IPv6 literal in brackets whole.
///
/// `[::1]:8080:db:5432` is four parts, not seven: the brackets are there to
/// say exactly that, and are dropped once they have.
fn split_parts(text: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut rest = text;
    if let Some(after) = rest.strip_prefix('[') {
        let Some((literal, tail)) = after.split_once(']') else {
            bail!("unclosed [ — an IPv6 address is written like [::1]:8080:host:3000");
        };
        parts.push(literal.trim().to_string());
        rest = match tail.strip_prefix(':') {
            Some(tail) => tail,
            // Nothing but the address: no port to bind, so nothing to do.
            None if tail.is_empty() => bail!("give a port as well, like [::1]:8080:host:3000"),
            None => bail!("expected a colon after the address"),
        };
    }
    parts.extend(rest.split(':').map(|p| p.trim().to_string()));
    Ok(parts)
}

/// What to bind, from what was typed in the first part.
fn bind_address(text: &str) -> Result<String> {
    match text {
        // `ssh` spells "every interface" both of these ways, and so does
        // everyone typing from memory.
        "*" | "" => Ok("0.0.0.0".into()),
        // A name resolves at bind time, which is where a wrong one is worth
        // reporting: this only has to know it is not a port.
        other => Ok(other.to_string()),
    }
}

/// An IPv6 literal needs its brackets back before it goes next to a port.
fn bracketed(host: &str) -> String {
    match host.contains(':') {
        true => format!("[{host}]"),
        false => host.to_string(),
    }
}

/// A running forward.
pub struct Forward {
    pub spec: Spec,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicU64>,
    failed: Arc<std::sync::Mutex<Option<String>>>,
}

impl Forward {
    /// Bind the local port and start carrying connections.
    ///
    /// Binding happens here rather than on the thread so that "port already in
    /// use" is reported immediately, while the user is looking at the prompt.
    pub fn start(opts: &ConnectOpts, spec: Spec) -> Result<Self> {
        // Loopback unless the shorthand named somewhere else: a forward is
        // usually for reaching something yourself, and binding every interface
        // is not something to do without being asked.
        let listener =
            TcpListener::bind((spec.local_host.as_str(), spec.local_port)).map_err(|e| {
                anyhow::anyhow!(
                    "cannot listen on {}:{}: {e}",
                    bracketed(&spec.local_host),
                    spec.local_port
                )
            })?;
        listener.set_nonblocking(true)?;

        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(std::sync::Mutex::new(None));

        let forward = Self {
            spec: spec.clone(),
            stop: Arc::clone(&stop),
            connections: Arc::clone(&connections),
            failed: Arc::clone(&failed),
        };

        let opts = opts.clone();
        thread::Builder::new()
            .name(format!("forward-{}", spec.local_port))
            .spawn(move || {
                if let Err(e) = run(&opts, &spec, listener, &stop, &connections) {
                    *failed.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                }
                stop.store(true, Ordering::Relaxed);
            })
            .expect("spawn forward thread");

        Ok(forward)
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    /// How many connections have been carried since it started.
    pub fn connection_count(&self) -> u64 {
        self.connections.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> Option<String> {
        self.failed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One connection being carried: a local socket paired with its SSH channel.
struct Pipe {
    socket: TcpStream,
    channel: ssh2::Channel,
    /// Bytes read from one side that the other has not accepted yet.
    to_channel: Vec<u8>,
    to_socket: Vec<u8>,
    done: bool,
}

fn run(
    opts: &ConnectOpts,
    spec: &Spec,
    listener: TcpListener,
    stop: &Arc<AtomicBool>,
    connections: &Arc<AtomicU64>,
) -> Result<()> {
    let session = establish(opts).map_err(|e| anyhow::anyhow!("{e}"))?;
    // One thread services the listener and every open connection, so nothing
    // here may block.
    session.set_blocking(false);

    let mut pipes: Vec<Pipe> = Vec::new();
    let mut buf = [0u8; 32 * 1024];

    while !stop.load(Ordering::Relaxed) {
        let mut idle = true;

        match listener.accept() {
            Ok((socket, _)) => {
                socket.set_nonblocking(true)?;
                // `direct-tcpip` asks the server to open the far side. The
                // server resolves the host, so `localhost` means *its*
                // loopback — which is the point.
                //
                // Opening a channel is a round trip, and on a non-blocking
                // session it just reports "would block" rather than waiting.
                // Blocking for this one call is far simpler than unwinding a
                // half-open channel across loop iterations; it costs the other
                // connections a few milliseconds while the channel is set up.
                session.set_blocking(true);
                let opened =
                    session.channel_direct_tcpip(&spec.remote_host, spec.remote_port, None);
                session.set_blocking(false);
                match opened {
                    Ok(channel) => {
                        connections.fetch_add(1, Ordering::Relaxed);
                        pipes.push(Pipe {
                            socket,
                            channel,
                            to_channel: Vec::new(),
                            to_socket: Vec::new(),
                            done: false,
                        });
                    }
                    // Refused at the far end: drop this one and keep serving.
                    Err(_) => drop(socket),
                }
                idle = false;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }

        for pipe in pipes.iter_mut() {
            if pump(pipe, &mut buf) {
                idle = false;
            }
        }
        pipes.retain(|p| !p.done);

        if idle {
            thread::sleep(Duration::from_millis(2));
        }
    }
    Ok(())
}

/// Move whatever is ready in either direction. Returns whether anything moved.
fn pump(pipe: &mut Pipe, buf: &mut [u8]) -> bool {
    let mut moved = false;

    // Local socket → SSH channel.
    if pipe.to_channel.is_empty() {
        match pipe.socket.read(buf) {
            Ok(0) => {
                // The browser hung up; tell the far end so it does too.
                let _ = pipe.channel.send_eof();
                pipe.done = true;
            }
            Ok(n) => {
                pipe.to_channel.extend_from_slice(&buf[..n]);
                moved = true;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => pipe.done = true,
        }
    }
    if !pipe.to_channel.is_empty() {
        match pipe.channel.write(&pipe.to_channel) {
            Ok(0) => {}
            Ok(n) => {
                pipe.to_channel.drain(..n);
                moved = true;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => pipe.done = true,
        }
    }

    // SSH channel → local socket.
    if pipe.to_socket.is_empty() {
        match pipe.channel.read(buf) {
            Ok(0) => {
                if pipe.channel.eof() {
                    pipe.done = true;
                }
            }
            Ok(n) => {
                pipe.to_socket.extend_from_slice(&buf[..n]);
                moved = true;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => pipe.done = true,
        }
    }
    if !pipe.to_socket.is_empty() {
        match pipe.socket.write(&pipe.to_socket) {
            Ok(0) => {}
            Ok(n) => {
                pipe.to_socket.drain(..n);
                moved = true;
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => pipe.done = true,
        }
    }

    // Only stop once whatever is buffered has been handed over.
    if pipe.done && (!pipe.to_socket.is_empty() || !pipe.to_channel.is_empty()) {
        pipe.done = false;
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_port_means_the_same_port_at_both_ends() {
        let spec = Spec::parse("3000").unwrap();
        assert_eq!(spec.local_port, 3000);
        assert_eq!(spec.remote_port, 3000);
        assert_eq!(spec.remote_host, "localhost");
        assert_eq!(spec.to_spec_string(), "3000");
    }

    #[test]
    fn two_parts_map_one_port_to_another() {
        let spec = Spec::parse("8080:3000").unwrap();
        assert_eq!((spec.local_port, spec.remote_port), (8080, 3000));
        assert_eq!(spec.remote_host, "localhost");
        assert_eq!(spec.to_spec_string(), "8080:3000");
    }

    #[test]
    fn three_parts_name_a_host_the_server_can_reach() {
        let spec = Spec::parse("5433:db.internal:5432").unwrap();
        assert_eq!(spec.local_port, 5433);
        assert_eq!(spec.remote_host, "db.internal");
        assert_eq!(spec.remote_port, 5432);
        assert_eq!(spec.to_spec_string(), "5433:db.internal:5432");
        assert_eq!(spec.describe(), "localhost:5433 → db.internal:5432");
    }

    #[test]
    fn whitespace_is_forgiven() {
        assert_eq!(Spec::parse("  8080 : 3000 ").unwrap().local_port, 8080);
    }

    #[test]
    fn an_address_in_front_says_where_to_bind() {
        let spec = Spec::parse("0.0.0.0:8080:db:5432").unwrap();
        assert_eq!(spec.local_host, "0.0.0.0");
        assert_eq!((spec.local_port, spec.remote_port), (8080, 5432));
        assert_eq!(spec.remote_host, "db");
        assert!(spec.is_public());
        assert_eq!(spec.describe(), "every interface:8080 → db:5432");

        // One interface rather than all of them.
        let spec = Spec::parse("192.168.1.10:8080:localhost:3000").unwrap();
        assert_eq!(spec.local_host, "192.168.1.10");
        assert!(spec.is_public());
        assert_eq!(spec.describe(), "192.168.1.10:8080 → localhost:3000");

        // `*` is how `ssh` spells every interface, and it means the same here.
        assert_eq!(Spec::parse("*:8080:db:5432").unwrap().local_host, "0.0.0.0");

        // Loopback said out loud is still loopback.
        let spec = Spec::parse("127.0.0.1:8080:db:5432").unwrap();
        assert!(!spec.is_public());
        assert_eq!(spec.describe(), "localhost:8080 → db:5432");
    }

    #[test]
    fn a_forward_with_no_address_in_front_stays_on_loopback() {
        for text in ["3000", "8080:3000", "5433:db:5432"] {
            let spec = Spec::parse(text).unwrap();
            assert_eq!(spec.local_host, LOOPBACK, "{text} should not be published");
            assert!(!spec.is_public());
        }
    }

    #[test]
    fn an_ipv6_address_keeps_its_brackets_out_of_the_colons() {
        let spec = Spec::parse("[::1]:8080:db:5432").unwrap();
        assert_eq!(spec.local_host, "::1");
        assert_eq!((spec.local_port, spec.remote_port), (8080, 5432));
        assert!(!spec.is_public(), "v6 loopback is still loopback");
        assert_eq!(spec.to_spec_string(), "[::1]:8080:db:5432");

        let spec = Spec::parse("[::]:8080:db:5432").unwrap();
        assert_eq!(spec.local_host, "::");
        assert!(spec.is_public());
        assert_eq!(spec.describe(), "every interface:8080 → db:5432");
    }

    #[test]
    fn nonsense_is_rejected_with_a_readable_reason() {
        for bad in [
            "",
            "   ",
            "abc",
            "80:",
            "0",
            "70000",
            "1:2:3:4:5",
            "8080::3000",
            "0.0.0.0:8080::5432",
            "[::1",
            "[::1]",
            "[::1]8080",
        ] {
            let err = Spec::parse(bad)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} should not parse"))
                .to_string();
            assert!(!err.is_empty(), "{bad:?} needs a reason");
        }
    }

    #[test]
    fn a_round_trip_through_the_shorthand_is_stable() {
        for text in [
            "3000",
            "8080:3000",
            "5433:db:5432",
            "0.0.0.0:8080:db:5432",
            "192.168.1.10:8080:localhost:3000",
            "[::1]:8080:db:5432",
        ] {
            let spec = Spec::parse(text).unwrap();
            assert_eq!(spec.to_spec_string(), text);
            assert_eq!(Spec::parse(&spec.to_spec_string()).unwrap(), spec);
        }
    }
}
