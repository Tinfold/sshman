//! Interactive shells embedded in the TUI, one under each file pane.
//!
//! Each shell is a real terminal: a pty for the local side, an SSH channel with
//! a pty for the remote side. Bytes from the shell are fed into a `vt100`
//! parser, and the resulting screen is drawn straight into the pane, so `vim`,
//! `top` and colours all behave.
//!
//! The remote shell deliberately runs on **its own SSH connection**. The file
//! transfer connection is driven by blocking calls on the worker thread; a
//! shell has to be read continuously, and a blocking read on a shared session
//! would hold libssh2's lock and stall every listing and transfer. A second
//! connection keeps the two entirely independent, and reuses the same host-key
//! and authentication path so nothing is weakened.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::sshconn::{ConnectOpts, establish};
use crate::types::sh_quote;

/// Lines of history kept above the visible screen.
const SCROLLBACK: usize = 5_000;

enum Msg {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// What to start on the far end of an SSH connection.
enum RemoteLaunch {
    /// The user's login shell, changing to this directory once it is ready.
    LoginShell(String),
    /// One command with a terminal attached — `docker exec -it …`.
    Command(String),
}

/// A running (or finished) shell session.
pub struct Shell {
    parser: Arc<Mutex<vt100::Parser>>,
    tx: Sender<Msg>,
    alive: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
    /// How far the view is scrolled back, in lines. Reset by any keystroke.
    scrollback: usize,
    pub label: String,
}

impl Shell {
    fn new(
        label: String,
        rows: u16,
        cols: u16,
    ) -> (Self, Receiver<Msg>, Arc<Mutex<vt100::Parser>>) {
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let (tx, rx) = channel();
        let shell = Self {
            parser: Arc::clone(&parser),
            tx,
            alive: Arc::new(AtomicBool::new(true)),
            rows,
            cols,
            scrollback: 0,
            label,
        };
        let parser_for_thread = Arc::clone(&parser);
        (shell, rx, parser_for_thread)
    }

    /// A shell on this machine, started in `cwd`.
    pub fn spawn_local(cwd: &Path, rows: u16, cols: u16) -> Self {
        Self::spawn_local_inner("local".into(), cwd, None, rows, cols)
    }

    /// A local pty running a command line rather than a login shell — how a
    /// container on this machine is entered, via `docker exec -it`.
    pub fn spawn_local_command(label: String, cmdline: String, rows: u16, cols: u16) -> Self {
        Self::spawn_local_inner(label, Path::new("."), Some(cmdline), rows, cols)
    }

    fn spawn_local_inner(
        label: String,
        cwd: &Path,
        command: Option<String>,
        rows: u16,
        cols: u16,
    ) -> Self {
        let (shell, rx, parser) = Self::new(label, rows, cols);
        let alive = Arc::clone(&shell.alive);
        let cwd = cwd.to_path_buf();
        thread::Builder::new()
            .name("local-shell".into())
            .spawn(move || {
                if let Err(e) = run_local(&cwd, command, rows, cols, &parser, &rx, &alive) {
                    write_notice(&parser, &format!("could not start a shell: {e}"));
                }
                alive.store(false, Ordering::Relaxed);
                write_notice(&parser, "[shell exited — press S to start a new one]");
            })
            .expect("spawn local shell thread");
        shell
    }

    /// A shell on the server, on a connection of its own, starting in `cwd`.
    pub fn spawn_remote(opts: &ConnectOpts, cwd: &str, rows: u16, cols: u16) -> Self {
        Self::spawn_remote_inner(
            format!("{}@{}", opts.user, opts.host),
            opts,
            RemoteLaunch::LoginShell(cwd.to_string()),
            rows,
            cols,
        )
    }

    /// A shell inside a container on a server: the same SSH connection of its
    /// own, but running `docker exec` instead of a login shell.
    pub fn spawn_remote_command(
        label: String,
        opts: &ConnectOpts,
        cmdline: String,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self::spawn_remote_inner(label, opts, RemoteLaunch::Command(cmdline), rows, cols)
    }

    fn spawn_remote_inner(
        label: String,
        opts: &ConnectOpts,
        launch: RemoteLaunch,
        rows: u16,
        cols: u16,
    ) -> Self {
        let (shell, rx, parser) = Self::new(label, rows, cols);
        let alive = Arc::clone(&shell.alive);
        let opts = opts.clone();
        write_notice(&parser, &format!("connecting to {}…", opts.host));
        thread::Builder::new()
            .name("remote-shell".into())
            .spawn(move || {
                if let Err(e) = run_remote(&opts, launch, rows, cols, &parser, &rx, &alive) {
                    write_notice(&parser, &format!("shell connection failed: {e}"));
                }
                alive.store(false, Ordering::Relaxed);
                write_notice(&parser, "[shell exited — press S to start a new one]");
            })
            .expect("spawn remote shell thread");
        shell
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Render with the parser locked. The vt100 screen borrows the parser, so
    /// drawing has to happen inside the lock.
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let guard = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.screen())
    }

    /// Match the emulator and the far end to the space the pane actually got.
    /// A no-op when nothing changed, so it is safe to call every frame.
    pub fn ensure_size(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        {
            let mut guard = self.parser.lock().unwrap_or_else(|e| e.into_inner());
            guard.screen_mut().set_size(rows, cols);
        }
        let _ = self.tx.send(Msg::Resize(rows, cols));
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        if let Some(bytes) = encode_key(&key) {
            // Typing should snap the view back to the prompt, the way a real
            // terminal does.
            self.set_scrollback(0);
            let _ = self.tx.send(Msg::Bytes(bytes));
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.set_scrollback(0);
        let _ = self.tx.send(Msg::Bytes(text.as_bytes().to_vec()));
    }

    /// Scroll the view; positive scrolls back into history.
    pub fn scroll(&mut self, lines: isize) {
        let target = (self.scrollback as isize + lines).max(0) as usize;
        self.set_scrollback(target.min(SCROLLBACK));
    }

    fn set_scrollback(&mut self, lines: usize) {
        if self.scrollback == lines {
            return;
        }
        self.scrollback = lines;
        let mut guard = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        guard.screen_mut().set_scrollback(lines);
    }

    pub fn scrollback(&self) -> usize {
        self.scrollback
    }

    /// Where the terminal cursor should sit, if it is visible and we are not
    /// scrolled back into history.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        if self.scrollback > 0 || !self.is_alive() {
            return None;
        }
        let guard = self.parser.lock().unwrap_or_else(|e| e.into_inner());
        let screen = guard.screen();
        if screen.hide_cursor() {
            return None;
        }
        let (row, col) = screen.cursor_position();
        Some((row, col))
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Close);
    }
}

/// Put a line of our own into the shell's screen — used for status and errors,
/// so problems appear where the user is already looking.
fn write_notice(parser: &Arc<Mutex<vt100::Parser>>, text: &str) {
    let mut guard = parser.lock().unwrap_or_else(|e| e.into_inner());
    guard.process(format!("\r\n{text}\r\n").as_bytes());
}

fn feed(parser: &Arc<Mutex<vt100::Parser>>, bytes: &[u8]) {
    let mut guard = parser.lock().unwrap_or_else(|e| e.into_inner());
    guard.process(bytes);
}

// ---- local ----------------------------------------------------------------

fn run_local(
    cwd: &Path,
    command: Option<String>,
    rows: u16,
    cols: u16,
    parser: &Arc<Mutex<vt100::Parser>>,
    rx: &Receiver<Msg>,
    alive: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let program = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
    let mut cmd = match &command {
        // Run through a shell so the command line can use the user's PATH and
        // ordinary shell syntax.
        Some(line) => {
            let mut c = CommandBuilder::new(&program);
            c.arg("-c");
            c.arg(line);
            c
        }
        None => CommandBuilder::new(&program),
    };
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd)?;
    // Drop our handle on the slave side, otherwise the pty never reports EOF
    // when the shell exits.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    let reader_parser = Arc::clone(parser);
    let reader_alive = Arc::clone(alive);
    thread::Builder::new()
        .name("local-shell-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => feed(&reader_parser, &buf[..n]),
                }
            }
            reader_alive.store(false, Ordering::Relaxed);
        })
        .expect("spawn local shell reader");

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Bytes(bytes)) => {
                if writer.write_all(&bytes).is_err() {
                    break;
                }
                let _ = writer.flush();
            }
            Ok(Msg::Resize(rows, cols)) => {
                let _ = master.resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                });
            }
            Ok(Msg::Close) | Err(RecvTimeoutError::Disconnected) => {
                let _ = child.kill();
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                // Also our chance to notice the shell exiting on its own.
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
            }
        }
    }
    let _ = child.wait();
    Ok(())
}

// ---- remote ---------------------------------------------------------------

fn run_remote(
    opts: &ConnectOpts,
    launch: RemoteLaunch,
    rows: u16,
    cols: u16,
    parser: &Arc<Mutex<vt100::Parser>>,
    rx: &Receiver<Msg>,
    alive: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let sess = establish(opts).map_err(|e| anyhow::anyhow!("{e}"))?;

    let mut channel = sess.channel_session()?;
    channel.request_pty(
        "xterm-256color",
        None,
        Some((cols as u32, rows as u32, 0, 0)),
    )?;
    // A login shell gets `cd` sent to it once its prompt appears; a command
    // is started directly with the terminal already attached.
    let mut startup_cd = None;
    match &launch {
        RemoteLaunch::LoginShell(cwd) => {
            channel.shell()?;
            // Sending this before the first prompt makes the pty echo it a
            // second time above the prompt, which looks like a glitch.
            startup_cd = Some(format!("cd {}\n", sh_quote(cwd)));
        }
        RemoteLaunch::Command(cmdline) => channel.exec(cmdline)?,
    }

    // Non-blocking from here on: this thread has to interleave reading output
    // with writing keystrokes. Safe to do because this session is ours alone.
    sess.set_blocking(false);

    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();

    loop {
        let mut idle = true;

        // Anything the user typed, plus whatever a short write left over.
        loop {
            match rx.try_recv() {
                Ok(Msg::Bytes(bytes)) => pending.extend_from_slice(&bytes),
                Ok(Msg::Resize(rows, cols)) => {
                    let _ = channel.request_pty_size(cols as u32, rows as u32, None, None);
                }
                Ok(Msg::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = channel.send_eof();
                    let _ = channel.close();
                    return Ok(());
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if !pending.is_empty() {
            match channel.write(&pending) {
                Ok(0) => {}
                Ok(n) => {
                    pending.drain(..n);
                    idle = false;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }

        match channel.read(&mut buf) {
            Ok(0) => {
                if channel.eof() {
                    break;
                }
            }
            Ok(n) => {
                feed(parser, &buf[..n]);
                idle = false;
                // The prompt is up; now it is safe to change directory.
                if let Some(command) = startup_cd.take() {
                    pending.extend_from_slice(command.as_bytes());
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }

        match channel.stderr().read(&mut buf) {
            Ok(n) if n > 0 => {
                feed(parser, &buf[..n]);
                idle = false;
            }
            _ => {}
        }

        if !alive.load(Ordering::Relaxed) {
            break;
        }
        if idle {
            // Nothing happening: yield rather than spin. Well under a frame,
            // so typing still feels immediate.
            thread::sleep(Duration::from_millis(5));
        }
    }

    let _ = channel.close();
    Ok(())
}

// ---- key encoding ----------------------------------------------------------

/// Turn a crossterm key event back into the bytes a terminal would send.
/// Returns `None` for keys that produce nothing.
pub fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let bytes = match key.code {
        KeyCode::Char(c) if ctrl => vec![control_byte(c)?],
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            let mut encoded = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut encoded).as_bytes());
            out
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        // Terminals overwhelmingly send DEL for backspace, and Ctrl-H for
        // ctrl-backspace.
        KeyCode::Backspace if ctrl => vec![0x08],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor_key(b'A', ctrl, alt, shift),
        KeyCode::Down => cursor_key(b'B', ctrl, alt, shift),
        KeyCode::Right => cursor_key(b'C', ctrl, alt, shift),
        KeyCode::Left => cursor_key(b'D', ctrl, alt, shift),
        KeyCode::Home => cursor_key(b'H', ctrl, alt, shift),
        KeyCode::End => cursor_key(b'F', ctrl, alt, shift),
        KeyCode::Insert => tilde_key(2),
        KeyCode::Delete => tilde_key(3),
        KeyCode::PageUp => tilde_key(5),
        KeyCode::PageDown => tilde_key(6),
        KeyCode::F(n) => function_key(n)?,
        _ => return None,
    };
    Some(bytes)
}

/// The C0 control character a Ctrl-key chord produces.
fn control_byte(c: char) -> Option<u8> {
    let b = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' => 0,
        '[' => 27,
        '\\' => 28,
        ']' => 29,
        '^' => 30,
        '_' => 31,
        '?' => 127,
        _ => return None,
    };
    Some(b)
}

/// Arrow and Home/End keys: plain `ESC [ X`, or `ESC [ 1 ; m X` when modified.
fn cursor_key(final_byte: u8, ctrl: bool, alt: bool, shift: bool) -> Vec<u8> {
    match modifier_code(ctrl, alt, shift) {
        None => vec![0x1b, b'[', final_byte],
        Some(m) => format!("\x1b[1;{m}{}", final_byte as char).into_bytes(),
    }
}

fn tilde_key(n: u8) -> Vec<u8> {
    format!("\x1b[{n}~").into_bytes()
}

fn function_key(n: u8) -> Option<Vec<u8>> {
    let seq = match n {
        1 => "\x1bOP".to_string(),
        2 => "\x1bOQ".to_string(),
        3 => "\x1bOR".to_string(),
        4 => "\x1bOS".to_string(),
        5 => "\x1b[15~".to_string(),
        6..=8 => format!("\x1b[{}~", n + 11),  // 17..19
        9..=12 => format!("\x1b[{}~", n + 11), // 20..23
        _ => return None,
    };
    Some(seq.into_bytes())
}

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_code(ctrl: bool, alt: bool, shift: bool) -> Option<u8> {
    let bits = u8::from(shift) | (u8::from(alt) << 1) | (u8::from(ctrl) << 2);
    (bits != 0).then_some(bits + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyEventKind;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    fn encode(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
        encode_key(&key(code, modifiers)).expect("key should encode")
    }

    #[test]
    fn plain_characters_pass_through() {
        assert_eq!(encode(KeyCode::Char('a'), KeyModifiers::NONE), b"a");
        assert_eq!(
            encode(KeyCode::Char('£'), KeyModifiers::NONE),
            "£".as_bytes()
        );
    }

    #[test]
    fn control_chords_become_control_codes() {
        // Ctrl-C must reach the shell as an interrupt, not quit the app.
        assert_eq!(encode(KeyCode::Char('c'), KeyModifiers::CONTROL), [0x03]);
        assert_eq!(encode(KeyCode::Char('d'), KeyModifiers::CONTROL), [0x04]);
        assert_eq!(encode(KeyCode::Char('z'), KeyModifiers::CONTROL), [0x1a]);
        assert_eq!(encode(KeyCode::Char('['), KeyModifiers::CONTROL), [0x1b]);
    }

    #[test]
    fn alt_prefixes_with_escape() {
        assert_eq!(encode(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
    }

    #[test]
    fn enter_and_backspace_use_terminal_conventions() {
        assert_eq!(encode(KeyCode::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(encode(KeyCode::Backspace, KeyModifiers::NONE), [0x7f]);
    }

    #[test]
    fn arrows_are_escape_sequences() {
        assert_eq!(encode(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        assert_eq!(encode(KeyCode::Left, KeyModifiers::NONE), b"\x1b[D");
        // Modified arrows carry the xterm modifier parameter.
        assert_eq!(encode(KeyCode::Up, KeyModifiers::CONTROL), b"\x1b[1;5A");
        assert_eq!(encode(KeyCode::Right, KeyModifiers::SHIFT), b"\x1b[1;2C");
    }

    #[test]
    fn navigation_and_function_keys() {
        assert_eq!(encode(KeyCode::PageUp, KeyModifiers::NONE), b"\x1b[5~");
        assert_eq!(encode(KeyCode::Delete, KeyModifiers::NONE), b"\x1b[3~");
        assert_eq!(encode(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(encode(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
    }

    #[test]
    fn unmappable_keys_are_dropped() {
        assert!(encode_key(&key(KeyCode::Null, KeyModifiers::NONE)).is_none());
        assert!(encode_key(&key(KeyCode::F(20), KeyModifiers::NONE)).is_none());
    }

    #[test]
    fn local_shell_runs_a_command_and_reports_exit() {
        // A real pty, a real shell: the whole local path end to end.
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        shell.paste("echo embedded-shell-works\n");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let seen = shell.with_screen(|s| s.contents().contains("embedded-shell-works"));
            if seen {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "shell never produced the expected output:\n{}",
                shell.with_screen(|s| s.contents())
            );
            thread::sleep(Duration::from_millis(50));
        }

        assert!(shell.is_alive());
        shell.paste("exit\n");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while shell.is_alive() {
            assert!(
                std::time::Instant::now() < deadline,
                "shell did not exit after `exit`"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn resizing_reshapes_the_emulator() {
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        assert_eq!(shell.with_screen(|s| s.size()), (24, 80));
        shell.ensure_size(30, 100);
        assert_eq!(shell.with_screen(|s| s.size()), (30, 100));
        // Idempotent: safe to call on every frame.
        shell.ensure_size(30, 100);
        assert_eq!(shell.with_screen(|s| s.size()), (30, 100));
    }
}
