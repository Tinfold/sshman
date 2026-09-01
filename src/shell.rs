//! Interactive shells embedded in the TUI, one under each file pane.
//!
//! Each shell is a real terminal: a pty for the local side, an SSH channel with
//! a pty for the remote side. Bytes from the shell are fed into a `vt100`
//! parser, and the resulting screen is drawn straight into the pane, so `vim`,
//! `top`, `btop` and colours all behave. One thing is rewritten on the way
//! in — see [`Hvp`] — because the parser knows only one of the two spellings
//! of "put the cursor here".
//!
//! Mouse events go the other way, to programs that have asked for them. What
//! counts as asking, and how the answer should be spelled, is tracked by the
//! parser from the sequences the program itself sent.
//!
//! The remote shell deliberately runs on **its own SSH connection**. The file
//! transfer connection is driven by blocking calls on the worker thread; a
//! shell has to be read continuously, and a blocking read on a shared session
//! would hold libssh2's lock and stall every listing and transfer. A second
//! connection keeps the two entirely independent, and reuses the same host-key
//! and authentication path so nothing is weakened.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

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
/// Text picked out with the mouse, in the coordinates of what is on screen:
/// the cell the drag started on and the one it has reached.
///
/// It runs in reading order rather than as a rectangle — the whole of every
/// row between the two ends — which is what a terminal selection has always
/// meant and what the text of it has to agree with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    anchor: (u16, u16),
    head: (u16, u16),
    /// The button is still down, so the far end still follows the mouse.
    dragging: bool,
}

impl Selection {
    /// The two ends, put back into reading order.
    pub fn span(self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Is anything actually picked out? A press with no drag is not a
    /// selection, it is a click.
    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    /// The columns of `row` that are in it, as a half-open range. Rows in the
    /// middle are covered from end to end.
    pub fn columns(self, row: u16, cols: u16) -> Option<(u16, u16)> {
        let (start, end) = self.span();
        if row < start.0 || row > end.0 {
            return None;
        }
        let from = if row == start.0 { start.1 } else { 0 };
        let to = if row == end.0 { end.1 + 1 } else { cols };
        (from < to).then(|| (from, to.min(cols)))
    }
}

pub struct Shell {
    parser: Arc<Mutex<vt100::Parser>>,
    tx: Sender<Msg>,
    alive: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
    /// How far the view is scrolled back, in lines. Reset by any keystroke.
    scrollback: usize,
    /// Text picked out with the mouse, if any. It is about what is on screen,
    /// so anything that redraws what is on screen — a keystroke, a scroll, a
    /// resize — lets go of it rather than leaving a highlight over text that
    /// has moved on.
    selection: Option<Selection>,
    /// Which of the kitty keyboard protocol's flags the program inside has
    /// turned on, so [`encode_key`] knows whether it may spell a key the new
    /// way. Shared with the thread reading that program's output, which is
    /// where the asking is noticed.
    kitty: Arc<Mutex<KittyKeys>>,
    /// What the program inside has let slip about where it is. Shared with
    /// the thread reading that program's output, which is where it is heard.
    /// See [`Shell::cwd`].
    reported_cwd: Arc<Mutex<Reported>>,
    /// The home directory on the machine this shell is on, so that a shell
    /// reporting `~/work` can be taken to mean somewhere in particular.
    home: Option<String>,
    /// The process id of a shell on this machine, once it has one. Zero means
    /// there is none to ask — a remote shell, or one inside a container,
    /// where the process on this end is only the pipe to it.
    pid: Arc<AtomicU32>,
    /// The directory this shell was started in, which is what its directory
    /// is until something says otherwise.
    start_dir: Option<String>,
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
            selection: None,
            kitty: Arc::new(Mutex::new(KittyKeys::default())),
            reported_cwd: Arc::new(Mutex::new(Reported::default())),
            pid: Arc::new(AtomicU32::new(0)),
            home: None,
            start_dir: None,
            label,
        };
        let parser_for_thread = Arc::clone(&parser);
        (shell, rx, parser_for_thread)
    }

    /// A shell on this machine, started in `cwd`.
    pub fn spawn_local(cwd: &Path, rows: u16, cols: u16) -> Self {
        Self::spawn_local_inner("local".into(), cwd, Start::Shell, rows, cols)
    }

    /// A local pty running a command line rather than a login shell — how a
    /// container on this machine is entered, via `docker exec -it`.
    pub fn spawn_local_command(label: String, cmdline: String, rows: u16, cols: u16) -> Self {
        Self::spawn_local_inner(label, Path::new("."), Start::Composed(cmdline), rows, cols)
    }

    /// The same, but started in a directory of its own: how an editor pane on
    /// this machine opens where the file list is looking.
    pub fn spawn_local_in(
        label: String,
        cwd: &Path,
        cmdline: String,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self::spawn_local_inner(label, cwd, Start::Composed(cmdline), rows, cols)
    }

    /// A pane running the command *you* gave it, in the shell you write
    /// commands in. See [`Start::Yours`].
    pub fn spawn_local_running(
        label: String,
        cwd: &Path,
        command: String,
        rows: u16,
        cols: u16,
    ) -> Self {
        Self::spawn_local_inner(label, cwd, Start::Yours(command), rows, cols)
    }

    fn spawn_local_inner(label: String, cwd: &Path, command: Start, rows: u16, cols: u16) -> Self {
        let (mut shell, rx, parser) = Self::new(label, rows, cols);
        shell.start_dir = Some(cwd.display().to_string());
        shell.home = dirs::home_dir().map(|home| home.display().to_string());
        let alive = Arc::clone(&shell.alive);
        let kitty = Arc::clone(&shell.kitty);
        let reported = Arc::clone(&shell.reported_cwd);
        // A shell of your own can be asked where it is directly; a pane
        // running a command — `docker exec`, an editor, a `tail -f` you
        // assigned — is a process whose directory says nothing about the
        // pane, so it is not asked.
        let pid = match command {
            Start::Shell => Arc::clone(&shell.pid),
            Start::Composed(_) | Start::Yours(_) => Arc::new(AtomicU32::new(0)),
        };
        let tx = shell.tx.clone();
        let cwd = cwd.to_path_buf();
        thread::Builder::new()
            .name("local-shell".into())
            .spawn(move || {
                let wiring = Wiring {
                    parser: &parser,
                    rx: &rx,
                    tx: &tx,
                    alive: &alive,
                    kitty: &kitty,
                    cwd: &reported,
                    pid: &pid,
                };
                if let Err(e) = run_local(&cwd, command, rows, cols, &wiring) {
                    write_notice(&parser, &format!("could not start a shell: {e}"));
                }
                alive.store(false, Ordering::Relaxed);
                write_notice(&parser, "[shell exited — press S to start a new one]");
            })
            .expect("spawn local shell thread");
        shell
    }

    /// A shell on the server, on a connection of its own, starting in `cwd`.
    ///
    /// `home` is that user's home directory on that server, which the tab
    /// already knows from logging in. It is only ever used to make sense of a
    /// shell that says it is in `~`.
    pub fn spawn_remote(
        opts: &ConnectOpts,
        cwd: &str,
        home: Option<&str>,
        rows: u16,
        cols: u16,
    ) -> Self {
        let mut shell = Self::spawn_remote_inner(
            format!("{}@{}", opts.user, opts.host),
            opts,
            RemoteLaunch::LoginShell(cwd.to_string()),
            rows,
            cols,
        );
        shell.home = home.map(str::to_string);
        shell
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
        let (mut shell, rx, parser) = Self::new(label, rows, cols);
        if let RemoteLaunch::LoginShell(cwd) = &launch {
            shell.start_dir = Some(cwd.clone());
        }
        let alive = Arc::clone(&shell.alive);
        let kitty = Arc::clone(&shell.kitty);
        let reported = Arc::clone(&shell.reported_cwd);
        // Nothing on this machine to ask: the process at this end is the SSH
        // session, not the shell.
        let pid = Arc::new(AtomicU32::new(0));
        let tx = shell.tx.clone();
        let opts = opts.clone();
        write_notice(&parser, &format!("connecting to {}…", opts.host));
        thread::Builder::new()
            .name("remote-shell".into())
            .spawn(move || {
                let wiring = Wiring {
                    parser: &parser,
                    rx: &rx,
                    tx: &tx,
                    alive: &alive,
                    kitty: &kitty,
                    cwd: &reported,
                    pid: &pid,
                };
                if let Err(e) = run_remote(&opts, launch, rows, cols, &wiring) {
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

    /// Where this shell is now, as well as it can be known.
    ///
    /// A pty carries characters, not state, so nothing here can simply be
    /// read off. There are three ways to find out, in the order they are
    /// trusted:
    ///
    /// 1. Ask the kernel which directory the process is in. Exact, and only
    ///    possible for a shell running on this machine.
    /// 2. `OSC 7`, the sequence whose whole meaning is "I am in this
    ///    directory". Exact wherever it arrives, but many shells never send
    ///    it unless their prompt has been set up to.
    /// 3. The window title, which most shells do set from their prompt and
    ///    which conventionally reads `user@host: directory`. A guess — a
    ///    title is the shell's to write, not a promise — so it is only read
    ///    when it takes that shape exactly, and never over either of the
    ///    above.
    ///
    /// Failing all three, the directory the shell was started in, which is
    /// still the right answer for a shell nobody has `cd`-ed anywhere.
    pub fn cwd(&self) -> Option<String> {
        let pid = self.pid.load(Ordering::Relaxed);
        if pid != 0
            && let Some(dir) = cwd_of_process(pid)
        {
            return Some(dir);
        }
        let reported = self
            .reported_cwd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        reported
            .exact
            .or_else(|| reported.from_title.and_then(|dir| self.at_home(&dir)))
            .or_else(|| self.start_dir.clone())
    }

    /// Turn a directory a shell named into one anything else could use: the
    /// `~` a prompt writes is only a path once you know whose home it is.
    fn at_home(&self, dir: &str) -> Option<String> {
        if let Some(rest) = dir.strip_prefix('~') {
            let home = self.home.as_deref()?;
            return match rest.strip_prefix('/') {
                Some(under) => Some(format!("{}/{under}", home.trim_end_matches('/'))),
                None if rest.is_empty() => Some(home.to_string()),
                // `~someone-else` is a home we have no way to find.
                None => None,
            };
        }
        dir.starts_with('/').then(|| dir.to_string())
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
        // The grid it was picked out of is about to be a different shape.
        self.selection = None;
        {
            let mut guard = self.parser.lock().unwrap_or_else(|e| e.into_inner());
            guard.screen_mut().set_size(rows, cols);
        }
        let _ = self.tx.send(Msg::Resize(rows, cols));
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        self.selection = None;
        let kitty = self.kitty.lock().unwrap_or_else(|e| e.into_inner()).flags();
        if let Some(bytes) = encode_key(&key, kitty) {
            // Typing should snap the view back to the prompt, the way a real
            // terminal does.
            self.set_scrollback(0);
            let _ = self.tx.send(Msg::Bytes(bytes));
        }
    }

    /// Hand a mouse event to the program inside, if it asked for them.
    ///
    /// `col` and `row` are zero-based within the shell's own area. Returns
    /// whether it was taken, so the caller can fall back to doing something
    /// with the event itself — scrolling the pane's history, most usefully.
    pub fn send_mouse(&mut self, event: &MouseEvent, col: u16, row: u16) -> bool {
        let (mode, encoding) = {
            let parser = self.parser.lock().unwrap_or_else(|e| e.into_inner());
            let screen = parser.screen();
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
            )
        };
        if mode == MouseProtocolMode::None {
            return false;
        }
        // The program wants the mouse, so the event is its business whether or
        // not this particular one is worth sending on.
        if let Some(bytes) = encode_mouse(event, col, row, mode, encoding) {
            let _ = self.tx.send(Msg::Bytes(bytes));
        }
        true
    }

    /// Start picking text out at a cell.
    pub fn begin_selection(&mut self, row: u16, col: u16) {
        let at = self.clamp(row, col);
        self.selection = Some(Selection {
            anchor: at,
            head: at,
            dragging: true,
        });
    }

    /// Follow the mouse while the button is down.
    pub fn drag_selection(&mut self, row: u16, col: u16) {
        let at = self.clamp(row, col);
        if let Some(selection) = &mut self.selection
            && selection.dragging
        {
            selection.head = at;
        }
    }

    /// The button came up. Hands back what was picked out, if anything.
    pub fn end_selection(&mut self) -> Option<String> {
        let selection = self.selection.as_mut()?;
        selection.dragging = false;
        if selection.is_empty() {
            // A click, not a drag.
            self.selection = None;
            return None;
        }
        self.selected_text()
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection.filter(|s| !s.is_empty())
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// The text of what is picked out, as the terminal itself would give it:
    /// a row that wrapped runs on into the next rather than gaining a newline
    /// that was never typed.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?.span();
        let text =
            self.with_screen(|screen| screen.contents_between(start.0, start.1, end.0, end.1 + 1));
        (!text.is_empty()).then_some(text)
    }

    /// A mouse position, brought inside the grid.
    fn clamp(&self, row: u16, col: u16) -> (u16, u16) {
        (
            row.min(self.rows.saturating_sub(1)),
            col.min(self.cols.saturating_sub(1)),
        )
    }

    /// Put text in as though it had been pasted.
    ///
    /// A program that has asked for bracketed paste gets it bracketed, which
    /// is what stops a shell running the lines of a multi-line paste one after
    /// another before you have looked at them.
    pub fn paste(&mut self, text: &str) {
        self.set_scrollback(0);
        let bracketed = self.with_screen(|screen| screen.bracketed_paste());
        let mut bytes = Vec::with_capacity(text.len() + 12);
        if bracketed {
            bytes.extend_from_slice(b"\x1b[200~");
        }
        bytes.extend_from_slice(text.as_bytes());
        if bracketed {
            bytes.extend_from_slice(b"\x1b[201~");
        }
        let _ = self.tx.send(Msg::Bytes(bytes));
    }

    /// Type into the terminal as though the keys had been pressed: the bytes
    /// go in exactly as given.
    ///
    /// How a file picked in a file list reaches the editor running in an
    /// editor pane. It is deliberately *not* a paste: an editor that has asked
    /// for bracketed paste would take `\e:e file\r` as text to insert rather
    /// than as the keys to open something.
    pub fn type_in(&mut self, text: &str) {
        self.set_scrollback(0);
        let _ = self.tx.send(Msg::Bytes(text.as_bytes().to_vec()));
    }

    /// Scroll the view; positive scrolls back into history.
    pub fn scroll(&mut self, lines: isize) {
        self.selection = None;
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

/// Hand `bytes` from the program inside to the screen, and return anything
/// it has to be told in reply — the answer to a question it asked about the
/// keyboard, which is the only thing here that talks back.
#[must_use]
fn feed(hvp: &mut Hvp, parser: &Arc<Mutex<vt100::Parser>>, bytes: &[u8]) -> Vec<u8> {
    let fixed = hvp.rewrite(bytes);
    let mut guard = parser.lock().unwrap_or_else(|e| e.into_inner());
    guard.process(&fixed);
    std::mem::take(&mut hvp.replies)
}

// ---- the kitty keyboard protocol -------------------------------------------

/// The flags sshman is willing to turn on: `disambiguate escape codes`, and
/// nothing else.
///
/// That one is the whole point of the exercise — it is what lets a program
/// tell `Shift-↵` from `↵` — and it is also the only one sshman can honestly
/// offer. The others ask for key releases and for every key as an escape
/// code, neither of which sshman's own terminal is reporting to it, so
/// agreeing to them would be promising events that could never arrive.
const KITTY_SUPPORTED: u8 = 0b1;

/// How deep a program may stack keyboard modes before we stop remembering.
/// The protocol names the same number.
const KITTY_STACK: usize = 16;

/// What the program inside a shell pane has asked for from the kitty keyboard
/// protocol.
///
/// A program pushes the flags it wants, pops back to what was there before
/// when it is done, and may ask at any point what is actually on — sshman
/// answers with what it granted rather than what was requested, which is how
/// the protocol expects a terminal that supports only part of it to behave.
#[derive(Default, Debug, PartialEq, Eq)]
struct KittyKeys {
    /// Modes below the current one, innermost last. Empty means the plain
    /// old encoding, which is where every shell starts.
    stack: Vec<u8>,
}

impl KittyKeys {
    fn flags(&self) -> u8 {
        self.stack.last().copied().unwrap_or(0)
    }

    fn push(&mut self, flags: u8) {
        if self.stack.len() == KITTY_STACK {
            self.stack.remove(0);
        }
        self.stack.push(flags & KITTY_SUPPORTED);
    }

    fn pop(&mut self, count: usize) {
        let keep = self.stack.len().saturating_sub(count.max(1));
        self.stack.truncate(keep);
    }

    /// `CSI = flags ; mode u`: set them, add to them, or take them away.
    fn set(&mut self, flags: u8, mode: u8) {
        let now = self.flags();
        let next = match mode {
            2 => now | flags,
            3 => now & !flags,
            _ => flags,
        } & KITTY_SUPPORTED;
        match self.stack.last_mut() {
            Some(top) => *top = next,
            None => self.stack.push(next),
        }
    }
}

/// Whether sshman's own terminal agreed to report keys unambiguously.
///
/// It gates the whole protocol: if `Shift-↵` cannot reach sshman in the first
/// place, saying yes to a program that asks for it would be promising
/// something we have no way to deliver.
static RICH_KEYS: AtomicBool = AtomicBool::new(false);

pub fn set_rich_keys(on: bool) {
    RICH_KEYS.store(on, Ordering::Relaxed);
}

pub fn rich_keys() -> bool {
    RICH_KEYS.load(Ordering::Relaxed)
}

/// Read a `CSI … u` sequence the program inside sent and do what it asks.
///
/// `params` is what came between the `CSI` and the `u`, private prefix and
/// all. Returns the bytes to send back, if it was a question.
fn kitty_request(keys: &Arc<Mutex<KittyKeys>>, params: &[u8]) -> Vec<u8> {
    // Nothing to offer: sshman's own terminal is not telling it which of the
    // ambiguous keys was pressed, so it has none of them to pass on. Saying
    // nothing is what a terminal without the protocol does, and it is what
    // the program inside is already prepared for.
    if !rich_keys() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(params);
    let (prefix, rest) = text.split_at(1);
    let number = |s: &str| s.trim().parse::<u32>().ok();
    let mut keys = keys.lock().unwrap_or_else(|e| e.into_inner());
    match prefix {
        // "What is on?" — answered with what we granted.
        "?" => return format!("\x1b[?{}u", keys.flags()).into_bytes(),
        ">" => keys.push(number(rest).unwrap_or(1) as u8),
        "<" => keys.pop(number(rest).unwrap_or(1) as usize),
        "=" => {
            let (flags, mode) = match rest.split_once(';') {
                Some((f, m)) => (number(f).unwrap_or(0), number(m).unwrap_or(1)),
                None => (number(rest).unwrap_or(0), 1),
            };
            keys.set(flags as u8, mode as u8);
        }
        _ => {}
    }
    Vec::new()
}

// ---- which shell a pane starts ---------------------------------------------

/// The shell the settings name, if any. One process, one answer, and every
/// pane opened after the setting changes gets the new one — which is why it
/// lives here rather than being carried down through every caller that opens
/// a pane.
static DEFAULT_SHELL: Mutex<Option<String>> = Mutex::new(None);

pub fn set_default_shell(shell: Option<String>) {
    *DEFAULT_SHELL.lock().unwrap_or_else(|e| e.into_inner()) = shell;
}

fn configured_shell() -> Option<String> {
    DEFAULT_SHELL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// The program a shell pane on this machine runs.
pub fn local_shell() -> String {
    configured_shell().unwrap_or_else(crate::config::default_shell)
}

/// Everything the program inside says, on its way to the screen.
///
/// Two things happen here. `CSI … f` is rewritten into `CSI … H`, and
/// `CSI … u` — the program asking about the keyboard — is answered and taken
/// out of the stream, since it was never anything to draw.
///
/// The rewrite is there because the two are the same command — HVP and CUP
/// both move the cursor to a row and column — but `vt100` implements only the
/// `H` spelling. Anything that
/// prefers `f` has every one of its positioning commands silently dropped,
/// and its careful full-screen layout arrives as one long wrapped run of
/// text. btop does exactly that: hundreds of `f`, not one `H`.
///
/// A read can end in the middle of a sequence, so a partial one is held here
/// until the byte that ends it turns up. Anything that stops looking like a
/// sequence is passed through exactly as it came, which is the only safe
/// thing to do with bytes we do not understand.
struct Hvp {
    /// The `ESC [ …` seen so far, when a sequence is still arriving.
    partial: Vec<u8>,
    /// The keyboard modes the program inside has asked for. `CSI … u` never
    /// reaches the screen: it is a conversation about the keyboard, and this
    /// is sshman's half of it.
    kitty: Arc<Mutex<KittyKeys>>,
    /// What that conversation owes the program, waiting to be written back.
    replies: Vec<u8>,
    /// Listens for the shell saying where it is. Unlike the keyboard
    /// conversation this only watches: what it hears goes to the screen
    /// untouched, because it is the terminal's business as well as ours.
    osc: OscWatch,
}

/// Longer than any real CSI sequence. Past this we are not looking at one,
/// so it goes to the parser untouched rather than being buffered for ever.
const MAX_CSI: usize = 64;

impl Hvp {
    fn new(kitty: Arc<Mutex<KittyKeys>>, cwd: Arc<Mutex<Reported>>) -> Self {
        Self {
            partial: Vec::new(),
            kitty,
            replies: Vec::new(),
            osc: OscWatch::new(cwd),
        }
    }

    fn rewrite(&mut self, input: &[u8]) -> Vec<u8> {
        self.osc.feed(input);
        let mut out = Vec::with_capacity(input.len() + self.partial.len());
        for &byte in input {
            if self.partial.is_empty() {
                if byte == 0x1b {
                    self.partial.push(byte);
                } else {
                    out.push(byte);
                }
                continue;
            }
            // One byte in: only `ESC [` opens a sequence we care about.
            if self.partial.len() == 1 {
                if byte == b'[' {
                    self.partial.push(byte);
                } else {
                    out.append(&mut self.partial);
                    out.push(byte);
                }
                continue;
            }
            match byte {
                // Parameter and intermediate bytes: still arriving.
                0x20..=0x3f if self.partial.len() < MAX_CSI => {
                    self.partial.push(byte);
                }
                // The final byte, which says what the sequence was.
                0x40..=0x7e => {
                    // `CSI ? u`, `CSI > … u` and friends are the program
                    // asking about the keyboard, not something to draw. They
                    // are answered here and go no further.
                    let params = &self.partial[2..];
                    if byte == b'u' && matches!(params.first(), Some(b'?' | b'>' | b'<' | b'=')) {
                        let reply = kitty_request(&self.kitty, params);
                        self.replies.extend_from_slice(&reply);
                        self.partial.clear();
                        continue;
                    }
                    out.append(&mut self.partial);
                    out.push(if byte == b'f' { b'H' } else { byte });
                }
                // Not a sequence after all, or an implausibly long one.
                _ => {
                    out.append(&mut self.partial);
                    out.push(byte);
                }
            }
        }
        out
    }
}

// ---- where the shell is ----------------------------------------------------

/// The most an OSC payload may run to before we stop keeping it. Paths are
/// well under this.
const MAX_OSC: usize = 4096;

/// The same, for `OSC 52`, which carries base64 rather than a path and so is
/// as long as whatever was copied. Comfortably past what the terminal will
/// take on the way back out — `to_clipboard` is where a very large copy is
/// actually trimmed — so nothing usable is dropped here for being long.
const MAX_OSC_CLIPBOARD: usize = 128 * 1024;

/// The last thing a program inside a pane asked to put on the system
/// clipboard, waiting for the main loop to hand it to the terminal.
///
/// One process, one terminal, one clipboard, so this is shared rather than
/// carried per pane: whichever pane copied last is the one that copied.
static COPIED: Mutex<Option<String>> = Mutex::new(None);

/// Take whatever a program inside a pane has copied since this was last
/// asked, if anything.
pub fn take_copied() -> Option<String> {
    COPIED.lock().unwrap_or_else(|e| e.into_inner()).take()
}

/// The text an `OSC 52` sequence is asking to put on the clipboard, if that
/// is what the payload is.
///
/// The shape is `52 ; <selection> ; <base64>`, where the selection names
/// which of the X selections is meant — sshman has only the one clipboard to
/// offer, so it is read past rather than honoured. A payload of `?` is the
/// question rather than the statement: the program asking what is *on* the
/// clipboard, which cannot be answered from here, since sshman has no
/// clipboard of its own to read.
fn clipboard_text(payload: &[u8]) -> Option<String> {
    let rest = payload.strip_prefix(b"52;")?;
    let split = rest.iter().position(|&b| b == b';')?;
    let data = &rest[split + 1..];
    if data == b"?" {
        return None;
    }
    // Emitters differ on whether they pad, and neither spelling is wrong.
    let decoded = base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT
        .decode(data)
        .ok()?;
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

/// What a shell has let slip about where it is. See [`Shell::cwd`], which
/// decides what to make of it.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
struct Reported {
    /// From `OSC 7`, whose whole meaning is this and nothing else.
    exact: Option<String>,
    /// Read out of the window title, which is a guess. Kept as the shell
    /// wrote it, `~` and all.
    from_title: Option<String>,
}

/// Watches the stream for a shell saying which directory it is in.
///
/// A pane is a pty, and a pty carries characters rather than state, so the
/// only way to know is to hear the shell mention it. Two things do:
/// `ESC ] 7 ; file://host/path ST`, which means exactly that, and the window
/// title, which most shells set from their prompt and which conventionally
/// reads `user@host: directory`. Neither is required of anyone, so what this
/// hears is a bonus on top of the directory the shell was started in rather
/// than a replacement for it.
///
/// It also hears `OSC 52`, which is a program asking for something to go on
/// the system clipboard. That one has nowhere else to go — see
/// [`clipboard_text`].
///
/// Nothing here is swallowed. Every byte it sees has already been passed on
/// to the screen, since a terminal has its own uses for the same sequences.
struct OscWatch {
    /// The payload of an OSC sequence as it arrives; `None` between them.
    partial: Option<Vec<u8>>,
    /// The last byte was `ESC`, which is half of both the opening `ESC ]` and
    /// the closing `ESC \`.
    esc: bool,
    cwd: Arc<Mutex<Reported>>,
}

impl OscWatch {
    fn new(cwd: Arc<Mutex<Reported>>) -> Self {
        Self {
            partial: None,
            esc: false,
            cwd,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match &mut self.partial {
                None => {
                    if self.esc && byte == b']' {
                        self.partial = Some(Vec::new());
                    }
                    self.esc = byte == 0x1b;
                }
                Some(buf) => {
                    // Either terminator ends it: a BEL, or the `ESC \` whose
                    // ESC is already sitting on the end of the payload.
                    let st = self.esc && byte == b'\\';
                    if byte == 0x07 || st {
                        if st && buf.last() == Some(&0x1b) {
                            buf.pop();
                        }
                        let payload = std::mem::take(buf);
                        self.partial = None;
                        self.esc = false;
                        self.note(&payload);
                        continue;
                    }
                    self.esc = byte == 0x1b;
                    // A clipboard sequence is as long as what was copied; a
                    // path is not. Past whichever cap applies we are not
                    // looking at either, and holding on would be a leak.
                    let cap = match buf.starts_with(b"52;") {
                        true => MAX_OSC_CLIPBOARD,
                        false => MAX_OSC,
                    };
                    if buf.len() >= cap {
                        self.partial = None;
                        self.esc = false;
                        continue;
                    }
                    buf.push(byte);
                }
            }
        }
    }

    /// Take a finished OSC payload and keep whatever it said about where the
    /// shell is.
    fn note(&mut self, payload: &[u8]) {
        // A program asking for something to go on the system clipboard.
        // sshman has no clipboard of its own — the terminal it is running in
        // owns that — so this is caught here and passed along. Nothing else
        // would: the screen has already seen the sequence and made nothing of
        // it, which is exactly why `y` inside vim used to do nothing at all.
        if let Some(text) = clipboard_text(payload) {
            *COPIED.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
            return;
        }
        let Ok(text) = std::str::from_utf8(payload) else {
            return;
        };
        let mut seen = self.cwd.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = osc7_path(text) {
            seen.exact = Some(path);
        } else if let Some(dir) = title_dir(text) {
            seen.from_title = Some(dir);
        }
    }
}

/// The directory an OSC payload names, if it names one.
///
/// The shape is `7;file://host/path`, where the host is whichever machine the
/// shell is on and so nothing to check against — a shell on the server quite
/// correctly names the server. A bare `7;/path` is accepted too: some shells
/// send that, and it means the same thing.
fn osc7_path(text: &str) -> Option<String> {
    let rest = text.strip_prefix("7;")?;
    let path = match rest.strip_prefix("file://") {
        Some(after) => &after[after.find('/')?..],
        None => rest,
    };
    let decoded = percent_decoded(path);
    // Anything that is not an absolute path is not something we could put a
    // file list or a new shell into.
    decoded.starts_with('/').then_some(decoded)
}

/// Undo the `%20`-style escaping a URL uses. Bytes that are not valid UTF-8
/// once decoded leave the text as it was, which is the safe answer for a path.
fn percent_decoded(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'%' if i + 2 < raw.len() => match u8::from_str_radix(&text[i + 1..i + 3], 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(raw[i]);
                    i += 1;
                }
            },
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// The directory a window title mentions, if it is one of the titles a shell
/// prompt writes.
///
/// The shape is `user@host: directory`, which is what the stock prompt on
/// every Debian- and Fedora-descended system sets and what a good many people
/// keep. Insisting on the whole shape — an `@`, then a colon, then something
/// that could be a path — is what keeps a program's own title out of this: an
/// editor showing `hosts: /etc/hosts` has no `@` in it, and one that somehow
/// did would still have to be naming a directory to fool anyone.
///
/// The `~` a prompt writes is left as it is. Only the shell's own machine
/// knows whose home that is; see [`Shell::at_home`].
fn title_dir(payload: &str) -> Option<String> {
    // `OSC 0` sets the icon name and the title, `1` the icon name, `2` the
    // title. Shells use 0.
    let (kind, title) = payload.split_once(';')?;
    if !matches!(kind, "0" | "1" | "2") {
        return None;
    }
    let (who, where_) = title.split_once(':')?;
    // A user and a host, and nothing that would make this a sentence.
    if !who.contains('@') || who.contains(' ') || who.contains('/') {
        return None;
    }
    let dir = where_.trim();
    (dir.starts_with('/') || dir.starts_with('~')).then(|| dir.to_string())
}

/// The directory a process on this machine is in, asked of the kernel.
///
/// Only Linux keeps this somewhere readable. Everywhere else a shell has to
/// say where it is for us to know, which is what [`OscWatch`] is for.
#[cfg(target_os = "linux")]
fn cwd_of_process(pid: u32) -> Option<String> {
    let link = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    Some(link.to_str()?.to_string())
}

#[cfg(not(target_os = "linux"))]
fn cwd_of_process(_pid: u32) -> Option<String> {
    None
}

// ---- local ----------------------------------------------------------------

/// Everything a running session is wired to: the screen it draws on, the
/// keystrokes coming its way, the channel it answers on, and the two things
/// both sides need to agree about — whether it is still alive, and what it
/// has asked of the keyboard.
struct Wiring<'a> {
    parser: &'a Arc<Mutex<vt100::Parser>>,
    rx: &'a Receiver<Msg>,
    tx: &'a Sender<Msg>,
    alive: &'a Arc<AtomicBool>,
    kitty: &'a Arc<Mutex<KittyKeys>>,
    /// Where the program inside says it is, filled in as it says so.
    cwd: &'a Arc<Mutex<Reported>>,
    /// Where to write the process id, for a shell that has one on this
    /// machine. Left at zero for one that does not.
    pid: &'a Arc<AtomicU32>,
}

/// What a pane on this machine starts.
///
/// Three things, and which shell reads them is the whole of the difference.
/// See [`crate::local::POSIX_SHELL`], which is where this rule is written
/// down for the commands that are not panes.
pub enum Start {
    /// Your shell, waiting for you to type in it. The ordinary pane.
    Shell,
    /// A line sshman composed: a `docker exec -it`, an editor on a file.
    /// Read by `/bin/sh`, because that is the language it was written in.
    Composed(String),
    /// A line *you* wrote — the command a pane was assigned. Read by the
    /// shell you write lines in, which is the one this pane would have opened
    /// had you not given it anything to run. The far side does the same: a
    /// command on a server is `exec`ed by that account's own login shell, so
    /// this is the two halves agreeing rather than a special case.
    Yours(String),
}

fn run_local(cwd: &Path, command: Start, rows: u16, cols: u16, w: &Wiring) -> anyhow::Result<()> {
    let Wiring {
        parser,
        rx,
        tx,
        alive,
        kitty,
        cwd: reported,
        pid,
    } = w;
    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = match &command {
        // Run through a shell so the command line can use the user's PATH and
        // ordinary shell syntax. Which shell is not the user's choice here:
        // this is sshman running something it composed, and it composed it in
        // the one language every shell in the family understands, which is
        // not the language fish speaks. See [`crate::local::POSIX_SHELL`].
        Start::Composed(line) => {
            let mut c = CommandBuilder::new(crate::local::POSIX_SHELL);
            c.arg("-c");
            c.arg(line);
            c
        }
        // Your line, so your shell — the same one this pane would have opened
        // for you to type it into. A setting that says `fish` means a command
        // you assigned is read by fish.
        Start::Yours(line) => {
            let mut c = argv_for(&local_shell());
            c.arg("-c");
            c.arg(line);
            c
        }
        // A prompt for the user, so it is their shell, and the setting is
        // where they said which one that is.
        Start::Shell => argv_for(&local_shell()),
    };
    cmd.cwd(cwd);
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd)?;
    // The one thing that can be asked, later, where this shell has got to.
    pid.store(child.process_id().unwrap_or(0), Ordering::Relaxed);
    // Drop our handle on the slave side, otherwise the pty never reports EOF
    // when the shell exits.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let master = pair.master;

    let reader_parser = Arc::clone(parser);
    let reader_alive = Arc::clone(alive);
    let reader_kitty = Arc::clone(kitty);
    let reader_cwd = Arc::clone(reported);
    // The reader answers the keyboard questions it sees, and the writer loop
    // below is the only thing holding the pty's writing end — so a reply goes
    // round through the same channel a keystroke does.
    let reader_tx = (*tx).clone();
    thread::Builder::new()
        .name("local-shell-reader".into())
        .spawn(move || {
            let mut buf = [0u8; 8192];
            let mut hvp = Hvp::new(reader_kitty, reader_cwd);
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let reply = feed(&mut hvp, &reader_parser, &buf[..n]);
                        if !reply.is_empty() && reader_tx.send(Msg::Bytes(reply)).is_err() {
                            break;
                        }
                    }
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

/// Turn a shell setting into something to run. A whole command line is
/// allowed — `bash --norc`, `nix develop -c fish` — so the words after the
/// program are its arguments rather than part of its name.
fn argv_for(line: &str) -> CommandBuilder {
    let mut words = line.split_whitespace();
    let program = words.next().unwrap_or("/bin/sh");
    let mut cmd = CommandBuilder::new(program);
    for arg in words {
        cmd.arg(arg);
    }
    cmd
}

// ---- remote ---------------------------------------------------------------

fn run_remote(
    opts: &ConnectOpts,
    launch: RemoteLaunch,
    rows: u16,
    cols: u16,
    w: &Wiring,
) -> anyhow::Result<()> {
    let Wiring {
        parser,
        rx,
        alive,
        kitty,
        cwd: reported,
        ..
    } = w;
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
            let mut line = format!("cd {}", sh_quote(cwd));
            // A shell of your own, if the server has it. The `command -v`
            // guard matters: an `exec` of something that is not there takes
            // the login shell down with it, and being dropped into nothing
            // because a server does not run the same shells your laptop does
            // would be a poor way to find that out.
            if let Some(shell) = configured_shell() {
                line.push_str(&format!(
                    "; command -v {0} >/dev/null 2>&1 && exec {1}",
                    sh_quote(shell.split_whitespace().next().unwrap_or(&shell)),
                    shell,
                ));
            }
            line.push('\n');
            startup_cd = Some(line);
        }
        RemoteLaunch::Command(cmdline) => channel.exec(cmdline)?,
    }

    // Non-blocking from here on: this thread has to interleave reading output
    // with writing keystrokes. Safe to do because this session is ours alone.
    sess.set_blocking(false);

    let mut buf = [0u8; 8192];
    // One per stream: they land on the same screen, but a sequence split
    // across reads is only ever split within the stream carrying it.
    let (mut hvp_out, mut hvp_err) = (
        Hvp::new(Arc::clone(kitty), Arc::clone(reported)),
        Hvp::new(Arc::clone(kitty), Arc::clone(reported)),
    );
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
                pending.extend_from_slice(&feed(&mut hvp_out, parser, &buf[..n]));
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
                pending.extend_from_slice(&feed(&mut hvp_err, parser, &buf[..n]));
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

// ---- mouse encoding --------------------------------------------------------

/// Turn a mouse event into the bytes the program inside the shell is waiting
/// for, or `None` when it wants nothing of the sort.
///
/// `col` and `row` are zero-based and relative to the shell's own area, since
/// the program has no idea it is sitting in a pane. Which events are wanted,
/// and how they are spelled, are both the program's choice: it asked for a
/// mode and an encoding, and the parser has been keeping track.
pub fn encode_mouse(
    event: &MouseEvent,
    col: u16,
    row: u16,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    if mode == MouseProtocolMode::None {
        return None;
    }
    // The wheel and a press are reported in every mode; the rest depends on
    // how much the program asked to hear about.
    let wanted = match event.kind {
        MouseEventKind::ScrollUp
        | MouseEventKind::ScrollDown
        | MouseEventKind::ScrollLeft
        | MouseEventKind::ScrollRight
        | MouseEventKind::Down(_) => true,
        MouseEventKind::Up(_) => mode != MouseProtocolMode::Press,
        MouseEventKind::Drag(_) => matches!(
            mode,
            MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion
        ),
        MouseEventKind::Moved => mode == MouseProtocolMode::AnyMotion,
    };
    if !wanted {
        return None;
    }

    let button = match event.kind {
        MouseEventKind::Down(b) | MouseEventKind::Up(b) | MouseEventKind::Drag(b) => match b {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
        },
        // The wheel is reported as buttons 4 to 7, which carry the 64 bit.
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        MouseEventKind::ScrollLeft => 66,
        MouseEventKind::ScrollRight => 67,
        // No button is down while merely moving: 3 is the "none" slot.
        MouseEventKind::Moved => 3,
    };
    let motion = matches!(event.kind, MouseEventKind::Drag(_) | MouseEventKind::Moved);
    let mut code = button + if motion { 32 } else { 0 };
    let mods = event.modifiers;
    if mods.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if mods.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let release = matches!(event.kind, MouseEventKind::Up(_));

    // Terminals count from one.
    let (col, row) = (col as u32 + 1, row as u32 + 1);
    match encoding {
        // The only encoding that can say which button was released, and the
        // only one without a limit on how far right a click can be.
        MouseProtocolEncoding::Sgr => Some(
            format!(
                "\x1b[<{code};{col};{row}{}",
                if release { 'm' } else { 'M' }
            )
            .into_bytes(),
        ),
        // The original encoding: three bytes, each offset by 32, so nothing
        // past column 223 can be described at all. A release is button 3.
        MouseProtocolEncoding::Default => {
            let code = if release { 3 + (code & !3) } else { code };
            let (a, b, c) = (code + 32, col + 32, row + 32);
            if a > 255 || b > 255 || c > 255 {
                return None;
            }
            Some(vec![0x1b, b'[', b'M', a as u8, b as u8, c as u8])
        }
        // As above, but each number is a character, which buys a few more
        // columns at the cost of being ambiguous past 2015.
        MouseProtocolEncoding::Utf8 => {
            let code = if release { 3 + (code & !3) } else { code };
            let mut out = vec![0x1b, b'[', b'M'];
            for value in [code + 32, col + 32, row + 32] {
                match char::from_u32(value) {
                    Some(c) => out.extend_from_slice(c.to_string().as_bytes()),
                    None => return None,
                }
            }
            Some(out)
        }
    }
}

// ---- key encoding ----------------------------------------------------------

/// Turn a crossterm key event back into the bytes a terminal would send.
/// Returns `None` for keys that produce nothing.
///
/// `kitty` is what the program inside has turned on of the kitty keyboard
/// protocol. While it is zero — which is where every shell starts, and where
/// most of them stay — every key is spelled the way terminals have always
/// spelled it. The one thing the protocol buys us is the keys that have no
/// traditional spelling at all: `Shift-↵` is the same byte as `↵` and always
/// has been, so a program wanting a newline out of it has to ask for the new
/// encoding, and this is where it gets it.
pub fn encode_key(key: &KeyEvent, kitty: u8) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // Only ever true for a program that asked, and only for a chord that has
    // nowhere else to go.
    let modified = ctrl || alt || shift;
    let rich = kitty & KITTY_SUPPORTED != 0 && modified;

    let bytes = match key.code {
        KeyCode::Enter if rich => csi_u(13, ctrl, alt, shift),
        KeyCode::Tab if rich => csi_u(9, ctrl, alt, shift),
        KeyCode::BackTab if kitty & KITTY_SUPPORTED != 0 => csi_u(9, ctrl, alt, true),
        KeyCode::Backspace if rich => csi_u(127, ctrl, alt, shift),
        KeyCode::Esc if rich => csi_u(27, ctrl, alt, shift),
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

/// A key spelled the kitty way: `ESC [ codepoint ; modifiers u`, where the
/// codepoint is the one the unmodified key stands for.
fn csi_u(code: u32, ctrl: bool, alt: bool, shift: bool) -> Vec<u8> {
    match modifier_code(ctrl, alt, shift) {
        None => format!("\x1b[{code}u").into_bytes(),
        Some(m) => format!("\x1b[{code};{m}u").into_bytes(),
    }
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

    /// Whether sshman's terminal reports keys richly is one answer for the
    /// whole process, so the two tests that change it take turns.
    static RICH: Mutex<()> = Mutex::new(());

    fn hvp() -> Hvp {
        Hvp::new(
            Arc::new(Mutex::new(KittyKeys::default())),
            Arc::new(Mutex::new(Reported::default())),
        )
    }

    /// Everything a stream of bytes let slip about where the shell is.
    fn reported(chunks: &[&[u8]]) -> Reported {
        let seen = Arc::new(Mutex::new(Reported::default()));
        let mut watch = OscWatch::new(Arc::clone(&seen));
        for chunk in chunks {
            watch.feed(chunk);
        }
        let heard = seen.lock().unwrap();
        heard.clone()
    }

    /// The directory a stream reported outright, if it reported one.
    fn heard(chunks: &[&[u8]]) -> Option<String> {
        reported(chunks).exact
    }

    #[test]
    fn a_shell_saying_where_it_is_is_heard() {
        assert_eq!(
            heard(&[b"\x1b]7;file://web01/etc/nginx\x07"]),
            Some("/etc/nginx".into()),
            "the host names the machine the shell is on, which is not ours to check"
        );
        // The other terminator, which is what most shells actually send.
        assert_eq!(
            heard(&[b"\x1b]7;file://localhost/srv\x1b\\"]),
            Some("/srv".into())
        );
        // And the bare form some shells use.
        assert_eq!(heard(&[b"\x1b]7;/var/log\x07"]), Some("/var/log".into()));
    }

    #[test]
    fn a_directory_split_across_two_reads_is_still_heard() {
        assert_eq!(
            heard(&[b"\x1b]7;file://h/home/me/wo", b"rk/app\x1b\\"]),
            Some("/home/me/work/app".into())
        );
    }

    #[test]
    fn spaces_in_a_reported_directory_come_back_as_spaces() {
        assert_eq!(
            heard(&[b"\x1b]7;file://h/home/me/My%20Files\x07"]),
            Some("/home/me/My Files".into())
        );
    }

    #[test]
    fn nothing_else_a_program_paints_is_taken_for_a_directory() {
        // One of these is a copy, and the clipboard is one per process.
        let _turn = CLIP.lock().unwrap_or_else(|e| e.into_inner());
        // A window title, which is a guess rather than a report.
        assert_eq!(heard(&[b"\x1b]0;me@web01: ~\x07"]), None);
        // A clipboard sequence, which is not a place.
        assert_eq!(heard(&[b"\x1b]52;c;L2V0Yy9uZ2lueA==\x07"]), None);
        // Ordinary output that happens to mention one.
        assert_eq!(heard(&[b"7;file:///etc\n"]), None);
    }

    #[test]
    fn a_prompt_that_sets_the_window_title_gives_its_directory_away() {
        // The stock prompt on every Debian- and Fedora-descended system.
        assert_eq!(
            reported(&[b"\x1b]0;tester@web01: /etc/ssh\x07"]).from_title,
            Some("/etc/ssh".into())
        );
        // `~` is left as the shell wrote it: only that machine knows whose.
        assert_eq!(
            reported(&[b"\x1b]0;tester@web01: ~/work\x07"]).from_title,
            Some("~/work".into())
        );
        // A program's own title is not a prompt, and does not count.
        assert_eq!(reported(&[b"\x1b]2;vim: /etc/hosts\x07"]).from_title, None);
        assert_eq!(reported(&[b"\x1b]0;make -j8\x07"]).from_title, None);
        assert_eq!(
            reported(&[b"\x1b]0;me@host: not a path\x07"]).from_title,
            None
        );
        // And a title never overrules the sequence that means exactly this.
        let both = reported(&[b"\x1b]7;file://h/srv\x07\x1b]0;me@h: ~\x07"]);
        assert_eq!(both.exact, Some("/srv".into()));
    }

    #[test]
    fn a_home_a_prompt_wrote_as_a_squiggle_is_made_into_a_path() {
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        shell.home = Some("/home/tester".into());
        assert_eq!(shell.at_home("~"), Some("/home/tester".into()));
        assert_eq!(shell.at_home("~/work"), Some("/home/tester/work".into()));
        assert_eq!(shell.at_home("/etc"), Some("/etc".into()));
        assert_eq!(shell.at_home("~someone"), None, "someone else's home");
        assert_eq!(shell.at_home("work"), None, "and nothing relative");
        shell.home = None;
        assert_eq!(shell.at_home("~/work"), None, "with no home to stand on");
        shell.type_in("exit\n");
    }

    /// The clipboard a program copies to is one per process, so the tests
    /// that look at it take turns.
    static CLIP: Mutex<()> = Mutex::new(());

    /// What a stream of bytes asked to put on the clipboard.
    fn copied(chunks: &[&[u8]]) -> Option<String> {
        let seen = Arc::new(Mutex::new(Reported::default()));
        let mut watch = OscWatch::new(seen);
        take_copied();
        for chunk in chunks {
            watch.feed(chunk);
        }
        take_copied()
    }

    #[test]
    fn a_program_copying_to_the_clipboard_is_heard() {
        let _turn = CLIP.lock().unwrap_or_else(|e| e.into_inner());
        // What vim writes for `"+yy` with `clipboard=osc52`.
        assert_eq!(copied(&[b"\x1b]52;c;aGVsbG8=\x07"]), Some("hello".into()));
        // The other terminator, and the selection nobody but X cares about.
        assert_eq!(copied(&[b"\x1b]52;p;aGVsbG8=\x1b\\"]), Some("hello".into()));
        // Split across two reads, the way a long one arrives.
        assert_eq!(
            copied(&[b"\x1b]52;c;aGVs", b"bG8=\x07"]),
            Some("hello".into())
        );
        // Emitters differ on padding, and neither spelling is wrong.
        assert_eq!(copied(&[b"\x1b]52;c;aGVsbG8\x07"]), Some("hello".into()));
    }

    #[test]
    fn a_program_asking_what_is_on_the_clipboard_gets_no_answer() {
        let _turn = CLIP.lock().unwrap_or_else(|e| e.into_inner());
        // sshman has no clipboard of its own to read back.
        assert_eq!(copied(&[b"\x1b]52;c;?\x07"]), None);
        // Nor is anything else an OSC says a copy.
        assert_eq!(copied(&[b"\x1b]0;me@web01: ~\x07"]), None);
        assert_eq!(copied(&[b"\x1b]7;file://h/etc\x07"]), None);
        // Not base64 at all, so not something to hand on.
        assert_eq!(copied(&[b"\x1b]52;c;not base64!\x07"]), None);
    }

    #[test]
    fn a_copy_too_large_to_pass_on_is_dropped_rather_than_held() {
        let _turn = CLIP.lock().unwrap_or_else(|e| e.into_inner());
        let mut huge = b"\x1b]52;c;".to_vec();
        huge.extend(std::iter::repeat_n(b'A', MAX_OSC_CLIPBOARD * 2));
        huge.extend(b"\x07");
        assert_eq!(copied(&[&huge]), None);
    }

    #[test]
    fn a_copy_reaches_the_screen_as_well() {
        let _turn = CLIP.lock().unwrap_or_else(|e| e.into_inner());
        // A terminal that does understand OSC 52 is downstream of this one,
        // so the sequence is passed on rather than eaten.
        let out = fixed(b"\x1b]52;c;aGVsbG8=\x07ok");
        assert_eq!(out, b"\x1b]52;c;aGVsbG8=\x07ok");
    }

    #[test]
    fn a_reported_directory_reaches_the_screen_as_well() {
        // The shell's report is the terminal's business too — a title bar is
        // drawn from the same sequences — so nothing may be swallowed.
        let out = fixed(b"\x1b]7;file://h/tmp\x07ok");
        assert_eq!(out, b"\x1b]7;file://h/tmp\x07ok");
    }

    /// Feed a stream in one go.
    fn fixed(input: &[u8]) -> Vec<u8> {
        hvp().rewrite(input)
    }

    fn mouse(kind: MouseEventKind, mods: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: mods,
        }
    }

    fn sgr(kind: MouseEventKind, col: u16, row: u16) -> Option<String> {
        encode_mouse(
            &mouse(kind, KeyModifiers::NONE),
            col,
            row,
            MouseProtocolMode::AnyMotion,
            MouseProtocolEncoding::Sgr,
        )
        .map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn a_click_is_reported_where_the_program_thinks_it_happened() {
        // Zero-based inside the pane, one-based on the wire.
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Left), 0, 0).as_deref(),
            Some("\x1b[<0;1;1M")
        );
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Right), 11, 4).as_deref(),
            Some("\x1b[<2;12;5M")
        );
        // Only SGR can say which button came up; the final letter says which.
        assert_eq!(
            sgr(MouseEventKind::Up(MouseButton::Left), 3, 3).as_deref(),
            Some("\x1b[<0;4;4m")
        );
    }

    #[test]
    fn the_wheel_and_motion_carry_their_own_button_numbers() {
        assert_eq!(
            sgr(MouseEventKind::ScrollUp, 0, 0).as_deref(),
            Some("\x1b[<64;1;1M")
        );
        assert_eq!(
            sgr(MouseEventKind::ScrollDown, 0, 0).as_deref(),
            Some("\x1b[<65;1;1M")
        );
        // Dragging is the button plus the motion bit.
        assert_eq!(
            sgr(MouseEventKind::Drag(MouseButton::Left), 0, 0).as_deref(),
            Some("\x1b[<32;1;1M")
        );
        // Moving with nothing held is the empty button slot plus that bit.
        assert_eq!(
            sgr(MouseEventKind::Moved, 0, 0).as_deref(),
            Some("\x1b[<35;1;1M")
        );
    }

    #[test]
    fn modifiers_are_added_to_the_button() {
        let event = mouse(
            MouseEventKind::Down(MouseButton::Left),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let out = encode_mouse(
            &event,
            0,
            0,
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Sgr,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[<24;1;1M", "8 + 16");
    }

    #[test]
    fn a_program_only_hears_about_what_it_asked_for() {
        let cases = [
            (MouseProtocolMode::None, false, false, false),
            (MouseProtocolMode::Press, false, false, false),
            (MouseProtocolMode::PressRelease, true, false, false),
            (MouseProtocolMode::ButtonMotion, true, true, false),
            (MouseProtocolMode::AnyMotion, true, true, true),
        ];
        for (mode, release, drag, moved) in cases {
            let sent = |kind| {
                encode_mouse(
                    &mouse(kind, KeyModifiers::NONE),
                    0,
                    0,
                    mode,
                    MouseProtocolEncoding::Sgr,
                )
                .is_some()
            };
            assert_eq!(
                sent(MouseEventKind::Up(MouseButton::Left)),
                release,
                "{mode:?}"
            );
            assert_eq!(
                sent(MouseEventKind::Drag(MouseButton::Left)),
                drag,
                "{mode:?}"
            );
            assert_eq!(sent(MouseEventKind::Moved), moved, "{mode:?}");
            // A press and the wheel are reported by every mode that is on.
            let on = mode != MouseProtocolMode::None;
            assert_eq!(
                sent(MouseEventKind::Down(MouseButton::Left)),
                on,
                "{mode:?}"
            );
            assert_eq!(sent(MouseEventKind::ScrollUp), on, "{mode:?}");
        }
    }

    #[test]
    fn the_original_encoding_offsets_every_byte_by_thirty_two() {
        let out = encode_mouse(
            &mouse(
                MouseEventKind::Down(MouseButton::Middle),
                KeyModifiers::NONE,
            ),
            9,
            4,
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Default,
        )
        .unwrap();
        assert_eq!(out, b"\x1b[M\x21\x2a\x25", "button 1, column 10, row 5");

        // It cannot say which button was released, so it says "some button".
        let up = encode_mouse(
            &mouse(MouseEventKind::Up(MouseButton::Middle), KeyModifiers::NONE),
            0,
            0,
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Default,
        )
        .unwrap();
        assert_eq!(up[3], 32 + 3);
    }

    #[test]
    fn a_click_too_far_right_for_the_original_encoding_is_dropped() {
        // Three bytes, each offset by 32: past 223 there is nothing to send.
        let far = encode_mouse(
            &mouse(MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE),
            400,
            0,
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Default,
        );
        assert_eq!(far, None, "better nothing than a click somewhere else");
        // SGR has no such limit.
        assert_eq!(
            sgr(MouseEventKind::Down(MouseButton::Left), 400, 0).as_deref(),
            Some("\x1b[<0;401;1M")
        );
    }

    #[test]
    fn absolute_positioning_reaches_the_parser_however_it_is_spelled() {
        // What btop sends, and what vt100 needs to see instead.
        assert_eq!(fixed(b"\x1b[12;40f"), b"\x1b[12;40H");
        // The other spelling is already right, and must not be touched.
        assert_eq!(fixed(b"\x1b[12;40H"), b"\x1b[12;40H");
    }

    #[test]
    fn nothing_else_is_disturbed() {
        for seq in [
            &b"\x1b[1C"[..],           // cursor forward
            b"\x1b[38;2;204;204;204m", // 24-bit colour
            b"\x1b[?2026h",            // synchronised output
            b"\x1b[2J",
            b"plain text with an f in it",
            b"\x1b]0;a title\x07",
            b"",
        ] {
            assert_eq!(fixed(seq), seq, "{:?} must arrive as it was sent", seq);
        }
    }

    #[test]
    fn a_sequence_split_across_reads_is_still_rewritten() {
        // A read can end anywhere, including between the ESC and the [.
        for cut in 1..9 {
            let whole = b"\x1b[12;40fXY";
            let (a, b) = whole.split_at(cut);
            let mut hvp = hvp();
            let mut out = hvp.rewrite(a);
            out.extend(hvp.rewrite(b));
            assert_eq!(
                out, b"\x1b[12;40HXY",
                "split after {cut} byte(s) lost the sequence"
            );
        }
    }

    #[test]
    fn an_escape_that_leads_nowhere_is_handed_over_untouched() {
        // ESC on its own, then something that is not a sequence at all.
        assert_eq!(fixed(b"\x1bXhello"), b"\x1bXhello");
        // A CSI interrupted by a control byte: not ours to interpret.
        assert_eq!(fixed(b"\x1b[12;\x07f"), b"\x1b[12;\x07f");
    }

    #[test]
    fn an_implausibly_long_sequence_is_not_buffered_for_ever() {
        let mut input = b"\x1b[".to_vec();
        input.extend(std::iter::repeat_n(b'1', MAX_CSI * 2));
        input.push(b'f');
        let out = fixed(&input);
        assert_eq!(out.len(), input.len(), "everything comes back out");
        assert!(
            out.ends_with(b"1f"),
            "and untouched, since this is not a sequence we know"
        );
    }

    #[test]
    fn the_stream_around_a_rewrite_is_intact() {
        let input = b"before\x1b[3;9fmiddle\x1b[1Cafter\x1b[2;2f";
        assert_eq!(fixed(input), b"before\x1b[3;9Hmiddle\x1b[1Cafter\x1b[2;2H");
    }

    use ratatui::crossterm::event::KeyEventKind;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: ratatui::crossterm::event::KeyEventState::NONE,
        }
    }

    /// What a shell that has asked for nothing unusual is sent.
    fn encode(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
        encode_key(&key(code, modifiers), 0).expect("key should encode")
    }

    /// What a program that has turned the kitty protocol on is sent.
    fn encode_rich(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
        encode_key(&key(code, modifiers), KITTY_SUPPORTED).expect("key should encode")
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
    fn shift_enter_is_still_a_return_until_a_program_asks_for_the_difference() {
        // Which is what a terminal has always sent, and what every shell
        // prompt in the world is waiting for.
        assert_eq!(encode(KeyCode::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(encode(KeyCode::Enter, KeyModifiers::SHIFT), b"\r");
        assert_eq!(encode(KeyCode::Enter, KeyModifiers::CONTROL), b"\r");

        // Having asked, it gets told apart: 13 is the codepoint the key
        // stands for, 2 is shift.
        assert_eq!(
            encode_rich(KeyCode::Enter, KeyModifiers::SHIFT),
            b"\x1b[13;2u"
        );
        assert_eq!(
            encode_rich(KeyCode::Enter, KeyModifiers::CONTROL),
            b"\x1b[13;5u"
        );
        // And the plain one is left alone even then, since it was never
        // ambiguous.
        assert_eq!(encode_rich(KeyCode::Enter, KeyModifiers::NONE), b"\r");
    }

    #[test]
    fn asking_for_the_keyboard_protocol_is_answered_and_kept_off_the_screen() {
        let _rich = RICH.lock().unwrap_or_else(|e| e.into_inner());
        let keys = Arc::new(Mutex::new(KittyKeys::default()));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(4, 20, 0)));
        let mut hvp = Hvp::new(Arc::clone(&keys), Arc::new(Mutex::new(Reported::default())));

        // A terminal that cannot tell the keys apart itself has nothing to
        // offer, and says so by saying nothing.
        set_rich_keys(false);
        assert!(feed(&mut hvp, &parser, b"\x1b[?u").is_empty());
        assert_eq!(keys.lock().unwrap().flags(), 0);

        set_rich_keys(true);
        assert_eq!(feed(&mut hvp, &parser, b"\x1b[?u"), b"\x1b[?0u");
        // Asked for everything; granted the one flag sshman can honour, and
        // the next question is answered with what was granted rather than
        // what was asked.
        assert!(feed(&mut hvp, &parser, b"\x1b[>15u").is_empty());
        assert_eq!(feed(&mut hvp, &parser, b"\x1b[?u"), b"\x1b[?1u");
        assert_eq!(encode(KeyCode::Enter, KeyModifiers::SHIFT), b"\r");

        // Popped back to where it started, the way a program does on its way
        // out.
        assert!(feed(&mut hvp, &parser, b"\x1b[<1u").is_empty());
        assert_eq!(keys.lock().unwrap().flags(), 0);

        // None of that was anything to draw.
        let screen = parser.lock().unwrap();
        assert_eq!(screen.screen().contents().trim(), "");
        drop(screen);
        set_rich_keys(false);
    }

    #[test]
    fn a_shell_setting_may_carry_its_own_arguments() {
        let cmd = argv_for("bash --norc -i");
        assert!(cmd.get_argv()[0].to_string_lossy().ends_with("bash"));
        assert_eq!(cmd.get_argv()[1], "--norc");
        assert_eq!(cmd.get_argv()[2], "-i");
    }

    #[test]
    fn unmappable_keys_are_dropped() {
        assert!(encode_key(&key(KeyCode::Null, KeyModifiers::NONE), 0).is_none());
        assert!(encode_key(&key(KeyCode::F(20), KeyModifiers::NONE), 0).is_none());
    }

    fn selection(anchor: (u16, u16), head: (u16, u16)) -> Selection {
        Selection {
            anchor,
            head,
            dragging: false,
        }
    }

    #[test]
    fn a_selection_reads_the_same_dragged_either_way() {
        let forwards = selection((1, 4), (3, 2));
        let backwards = selection((3, 2), (1, 4));
        assert_eq!(forwards.span(), ((1, 4), (3, 2)));
        assert_eq!(
            backwards.span(),
            forwards.span(),
            "dragging up is dragging down"
        );
    }

    #[test]
    fn a_press_with_no_drag_is_a_click_rather_than_a_selection() {
        assert!(selection((2, 5), (2, 5)).is_empty());
        assert!(!selection((2, 5), (2, 6)).is_empty());
    }

    #[test]
    fn a_selection_covers_whole_rows_between_its_ends() {
        // Reading order, not a rectangle: from part-way along one row, through
        // everything between, to part-way along another.
        let s = selection((1, 4), (3, 2));
        assert_eq!(s.columns(0, 80), None, "above it");
        assert_eq!(s.columns(1, 80), Some((4, 80)), "from the cell it started");
        assert_eq!(s.columns(2, 80), Some((0, 80)), "all of the row between");
        assert_eq!(
            s.columns(3, 80),
            Some((0, 3)),
            "up to and including the end"
        );
        assert_eq!(s.columns(4, 80), None, "below it");
    }

    #[test]
    fn a_selection_on_one_row_is_the_cells_between_its_ends() {
        let s = selection((2, 3), (2, 6));
        assert_eq!(s.columns(2, 80), Some((3, 7)));
        // And never wider than the pane, however far the mouse went.
        assert_eq!(selection((2, 3), (2, 200)).columns(2, 80), Some((3, 80)));
    }

    #[test]
    fn text_is_picked_out_of_the_screen_and_let_go_of_when_it_moves() {
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        shell.type_in("printf 'alpha\\nbravo\\n'\n");

        // A row of its own, not the echoed command line that also holds the
        // word: what is being picked out here is the output.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let at = loop {
            let rows: Vec<String> = shell.with_screen(|s| s.rows(0, 80).collect());
            if let Some(at) = rows.iter().position(|r| r.trim() == "alpha")
                && rows.get(at + 1).is_some_and(|r| r.trim() == "bravo")
            {
                break at as u16;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the shell never printed anything:\n{}",
                shell.with_screen(|s| s.contents())
            );
            thread::sleep(Duration::from_millis(50));
        };

        shell.begin_selection(at, 0);
        shell.drag_selection(at + 1, 4);
        let text = shell.end_selection().expect("something was picked out");
        assert_eq!(text, "alpha\nbravo");

        // Anything that redraws what is under it lets the selection go, rather
        // than leaving a highlight over text that has moved on.
        shell.begin_selection(at, 0);
        shell.drag_selection(at, 4);
        assert!(shell.selection().is_some());
        shell.scroll(3);
        assert!(shell.selection().is_none());

        shell.type_in("exit\n");
    }

    #[test]
    fn a_real_shell_gets_a_real_answer_about_the_keyboard() {
        // The whole local path: a program prints the question, sshman's
        // reader notices it, and the answer goes back down the pty the same
        // way a keystroke would. `-icanon` is what lets a reply with no
        // newline on the end of it be read at all, `-echo` keeps it off the
        // screen, and `head -c 5` takes exactly the reply.
        let _rich = RICH.lock().unwrap_or_else(|e| e.into_inner());
        set_rich_keys(true);
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        shell.type_in("stty -echo -icanon; printf '\\033[?u'; head -c 5 | od -An -c\n");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            // `od -An -c` spells the reply out: escape, then `[?0u`.
            let seen = shell.with_screen(|s| s.contents().contains("033   [   ?   0   u"));
            if seen {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the query went unanswered:\n{}",
                shell.with_screen(|s| s.contents())
            );
            thread::sleep(Duration::from_millis(50));
        }
        set_rich_keys(false);
    }

    #[test]
    fn a_local_shell_that_moves_says_where_it_went() {
        // A real pty and a real shell: the point is that `cd` inside one is
        // noticed from out here, which is what a saved session writes down.
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        assert_eq!(shell.cwd().as_deref(), Some("/"), "where it was started");

        shell.type_in("cd /tmp\n");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            // `/tmp` is a symlink on some systems, so the real path is what
            // the kernel reports and either spelling is right.
            let now = shell.cwd().unwrap_or_default();
            if now.ends_with("/tmp") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the shell's directory never caught up: {now:?}"
            );
            thread::sleep(Duration::from_millis(50));
        }
        shell.type_in("exit\n");
    }

    #[test]
    fn local_shell_runs_a_command_and_reports_exit() {
        // A real pty, a real shell: the whole local path end to end.
        let mut shell = Shell::spawn_local(Path::new("/"), 24, 80);
        shell.type_in("echo embedded-shell-works\n");

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
        shell.type_in("exit\n");
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
