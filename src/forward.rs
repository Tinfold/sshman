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

/// Where a forward starts and ends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Spec {
    /// Port bound on this machine.
    pub local_port: u16,
    /// Host to reach *from the server* — usually its own loopback.
    pub remote_host: String,
    pub remote_port: u16,
}

impl Spec {
    /// Parse the shorthand people actually type:
    ///
    /// - `3000`                  — 3000 here to 3000 on the server
    /// - `8080:3000`             — 8080 here to 3000 on the server
    /// - `8080:db:5432`          — 8080 here to db:5432, as seen by the server
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() {
            bail!("give a port, like 3000 or 8080:3000");
        }
        let parts: Vec<&str> = text.split(':').map(str::trim).collect();
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
                    local_port: p,
                    remote_host: "localhost".into(),
                    remote_port: p,
                })
            }
            [local, remote] => Ok(Self {
                local_port: port(local)?,
                remote_host: "localhost".into(),
                remote_port: port(remote)?,
            }),
            [local, host, remote] => {
                if host.is_empty() {
                    bail!("the middle part should be a host name");
                }
                Ok(Self {
                    local_port: port(local)?,
                    remote_host: (*host).to_string(),
                    remote_port: port(remote)?,
                })
            }
            _ => bail!("too many colons — try 8080:host:3000"),
        }
    }

    /// The shorthand that would recreate this, kept short when it can be.
    pub fn to_spec_string(&self) -> String {
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
        format!(
            "localhost:{} → {}:{}",
            self.local_port, self.remote_host, self.remote_port
        )
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
        // Loopback only: a forward is for reaching something yourself, and
        // binding every interface would quietly publish it to the network.
        let listener = TcpListener::bind(("127.0.0.1", spec.local_port))
            .map_err(|e| anyhow::anyhow!("cannot listen on port {}: {e}", spec.local_port))?;
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
    fn nonsense_is_rejected_with_a_readable_reason() {
        for bad in [
            "",
            "   ",
            "abc",
            "80:",
            "0",
            "70000",
            "1:2:3:4",
            "8080::3000",
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
        for text in ["3000", "8080:3000", "5433:db:5432"] {
            let spec = Spec::parse(text).unwrap();
            assert_eq!(spec.to_spec_string(), text);
            assert_eq!(Spec::parse(&spec.to_spec_string()).unwrap(), spec);
        }
    }
}
