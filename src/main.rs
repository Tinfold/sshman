//! sshman — a two-pane SSH file manager.
//!
//! Left pane is your machine, right pane is the server. Copy either way,
//! open files in whatever editor you like, run remote commands, and flip the
//! remote side into sudo mode when you need to see root-only paths.

mod app;
mod archive;
mod backend;
mod config;
mod docker;
mod fileops;
mod forward;
mod history;
mod input;
mod keys;
mod layout;
mod local;
mod shell;
mod sshcfg;
mod sshconn;
mod theme;
mod types;
mod ui;
mod watch;
mod worker;
mod workspace;

use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
    enable_raw_mode, supports_keyboard_enhancement,
};
use ratatui::layout::Position;

use app::{App, Mode, UiAction};
use backend::Target;
use sshconn::ConnectOpts;
use types::sh_quote;

#[derive(Parser, Debug)]
#[command(
    name = "sshman",
    version,
    about = "Two-pane SSH file manager: browse local and remote side by side",
    long_about = "Browse your machine and a server side by side.\n\
                  Copy files either way, edit them in your own editor, run remote\n\
                  commands, and switch the remote pane to sudo when you need to\n\
                  see root-only paths.\n\n\
                  With no arguments you get a connection form. Host aliases from\n\
                  ~/.ssh/config are honoured."
)]
struct Args {
    /// Server to connect to, as HOST or USER@HOST (a ~/.ssh/config alias works)
    target: Option<String>,

    /// Port to connect on
    #[arg(short = 'p', long)]
    port: Option<u16>,

    /// Login user (overrides USER@ in the target)
    #[arg(short = 'l', long)]
    user: Option<String>,

    /// Private key to authenticate with
    #[arg(short = 'i', long, value_name = "KEYFILE")]
    identity: Option<PathBuf>,

    /// Show the connection form so you can type a password
    #[arg(short = 'W', long)]
    ask_password: bool,

    /// Directory to open in the local pane
    #[arg(long, value_name = "DIR")]
    local_path: Option<PathBuf>,

    /// Directory to open in the remote pane (default: your home)
    #[arg(long, value_name = "DIR")]
    remote_path: Option<String>,

    /// Skip the connection screen and pick a container on this machine
    #[arg(short = 'd', long)]
    docker: bool,

    /// Skip the connection screen and open a tab on this machine
    #[arg(short = 'L', long)]
    local: bool,

    /// Container runtime to use: docker, podman, or a path. Detected when
    /// not given, preferring docker.
    #[arg(long, value_name = "PROGRAM")]
    runtime: Option<String>,

    /// Editor to open files with, for this run only. The saved setting (`,`)
    /// is what sticks.
    #[arg(long, value_name = "PROGRAM")]
    editor: Option<String>,

    /// Open a saved workspace: reconnects everything it holds
    #[arg(short = 'w', long, value_name = "NAME")]
    workspace: Option<String>,

    /// Reopen whatever was open last time: the same servers, panes and
    /// directories. sshman writes this down as you go, so it survives being
    /// closed any way at all.
    #[arg(short = 'r', long)]
    resume: bool,

    /// List saved workspaces and exit
    #[arg(long)]
    list_workspaces: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Passed through the environment so it reaches the worker threads and the
    // shells they spawn without being threaded through every call.
    if let Some(runtime) = &args.runtime {
        // SAFETY: single-threaded — no workers or shells exist yet.
        unsafe { std::env::set_var("SSHMAN_CONTAINER_RUNTIME", runtime) };
    }

    if args.list_workspaces {
        let saved = workspace::Workspaces::load();
        if saved.is_empty() {
            println!("No workspaces saved yet.");
        }
        for w in &saved.entries {
            let members: Vec<String> = w.items.iter().map(|i| i.describe()).collect();
            println!("{:<20} {:<16} {}", w.name, w.summary(), members.join(", "));
        }
        // Not a workspace, but the thing most people are looking for when
        // they run this, and `--resume` is how it opens.
        if let Some(session) = workspace::Session::load().filter(|s| !s.items.is_empty()) {
            let members: Vec<String> = session.items.iter().map(|i| i.describe()).collect();
            println!(
                "\n{:<20} {:<16} {}   (sshman --resume)",
                session.name,
                session.summary(),
                members.join(", ")
            );
        }
        return Ok(());
    }

    let (opts, auto_connect) = build_opts(&args);
    let local_start = match &args.local_path {
        Some(p) => local::expand(&p.to_string_lossy()),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    // A named workspace takes over from the connection screen: it already
    // says what to connect to. So does `--resume`, which is the same thing
    // asked of the session that wrote itself down.
    let requested = match (&args.workspace, args.resume) {
        (Some(name), _) => match workspace::Workspaces::load().find(name).cloned() {
            Some(found) => Some(found),
            None => anyhow::bail!("no workspace called {name:?} — see `sshman --list-workspaces`"),
        },
        (None, true) => match workspace::Session::load().filter(|s| !s.items.is_empty()) {
            Some(session) => Some(session),
            None => anyhow::bail!(
                "nothing to resume — no session has been recorded on this machine yet"
            ),
        },
        (None, false) => None,
    };

    let mut app = App::new(
        opts,
        local_start,
        args.remote_path.clone(),
        auto_connect && requested.is_none(),
    );
    if let Some(workspace) = requested {
        app.launch_workspace(&workspace);
    }
    // For this run only: a flag is an override, not a decision to remember.
    if let Some(editor) = args
        .editor
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        app.editor = editor.to_string();
    }
    if args.docker {
        app.browse_local_containers();
    }
    if args.local {
        app.open_local_tab();
    }

    let mut terminal = setup_terminal().context("cannot set up the terminal")?;
    let result = run(&mut terminal, &mut app);
    restore_terminal(&mut terminal).ok();
    app.shutdown();

    result
}

/// Turn CLI arguments into connection options, filling gaps from
/// `~/.ssh/config` exactly as `ssh` would.
fn build_opts(args: &Args) -> (ConnectOpts, bool) {
    let mut opts = ConnectOpts {
        port: 22,
        ..Default::default()
    };

    let Some(target) = &args.target else {
        // No target: show the form, prefilled with whatever was supplied.
        opts.user = args.user.clone().unwrap_or_else(current_user);
        opts.port = args.port.unwrap_or(22);
        opts.key_path = args.identity.clone();
        return (opts, false);
    };

    let (user_from_target, host_alias) = match target.split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h.to_string()),
        None => (None, target.clone()),
    };

    let cfg = sshcfg::lookup(&host_alias);
    opts.user = args
        .user
        .clone()
        .or(user_from_target)
        .or(cfg.user)
        .unwrap_or_else(current_user);
    opts.port = args.port.or(cfg.port).unwrap_or(22);
    opts.key_path = args.identity.clone().or(cfg.identity_file);
    opts.host = cfg.hostname.unwrap_or(host_alias);

    (opts, !args.ask_password)
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Ask the terminal to stop conflating keys that a program inside a shell
/// pane may well want to tell apart.
///
/// Without this, Shift-↵ and ↵ arrive as the same byte — the terminal has no
/// way to say which was pressed — so sshman cannot pass on the difference no
/// matter what the program inside asked for. `DISAMBIGUATE_ESCAPE_CODES` is
/// the smallest flag that fixes it: modified keys with no traditional
/// spelling come as `CSI … u`, and everything with a traditional spelling
/// keeps it, so nothing else about the key handling changes.
fn enable_rich_keys(out: &mut impl Write) {
    if !supports_keyboard_enhancement().unwrap_or(false) {
        return;
    }
    if execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        shell::set_rich_keys(true);
    }
}

/// Put back whatever the terminal was reporting before, if we changed it.
fn disable_rich_keys(out: &mut impl Write) {
    if shell::rich_keys() {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
}

fn setup_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        // Lets a multi-line paste arrive as one event instead of a burst of
        // keystrokes, which matters for pasting into an embedded shell.
        EnableBracketedPaste
    )?;
    enable_rich_keys(&mut stdout);
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    disable_rich_keys(terminal.backend_mut());
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Tui, app: &mut App) -> Result<()> {
    loop {
        // 1. Fold in whatever the workers have finished. Each tab has its own
        //    channel, plus the connection currently being attempted.
        app.drain_workers();

        // 2. Notice anything that changed in a directory on screen without
        //    sshman being the one to change it.
        app.watch_dirs();

        // 3. Keep the record of where this session got to close to true.
        //    There is no "on the way out" to do this in: a terminal window
        //    closed on sshman never comes back here at all.
        app.cache_session();

        // 4. Anything that needs the terminal to itself.
        if let Some(action) = app.pending_action.take() {
            match action {
                UiAction::Quit => return Ok(()),
                UiAction::Editor {
                    program,
                    path,
                    push_back,
                    refresh,
                } => {
                    let outcome = suspended(terminal, || run_editor(&program, &path))?;
                    match outcome {
                        Ok(()) => app.after_editor(push_back, refresh),
                        // Nothing is uploaded when the editor fails. If this
                        // was a remote file, its temp copy is still on disk —
                        // say where, rather than dropping the edit silently.
                        Err(e) => {
                            let msg = match &push_back {
                                Some(edit) => format!(
                                    "{program} failed: {e} — downloaded copy kept at {}",
                                    edit.temp.display()
                                ),
                                None => format!("{program} failed: {e}"),
                            };
                            app.set_status(msg, app::Level::Bad);
                        }
                    }
                }
                UiAction::Shell => {
                    let cmd = shell_command(app);
                    if let Some(cmd) = cmd {
                        let outcome = suspended(terminal, || run_shell(&cmd))?;
                        match outcome {
                            Ok(()) => {
                                app.set_status("back from the shell", app::Level::Info);
                                app.reload_remote();
                            }
                            Err(e) => app.set_status(e.to_string(), app::Level::Bad),
                        }
                    }
                }
            }
            continue;
        }

        // 5. Draw.
        terminal.draw(|f| ui::draw(f, app))?;

        // 6. Wait briefly for input. The timeout is what lets worker messages
        //    and progress updates surface without any input at all.
        if event::poll(Duration::from_millis(60))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    app.on_key(key);
                }
                Event::Mouse(m) => handle_mouse(app, m),
                // Pasted text only makes sense in a shell; elsewhere the
                // prompts take ordinary typing.
                Event::Paste(text) => app.paste(&text),
                Event::Resize(_, _) => terminal.autoresize()?,
                _ => {}
            }
        }

        // Anything copied out of a shell goes to the terminal now, between
        // frames, so it cannot land in the middle of one.
        if let Some(text) = app.take_clipboard() {
            let _ = to_clipboard(terminal, &text);
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Follow a tab being dragged along the row: as the pointer crosses into a
/// neighbour's chip, the two change places, so what you see under the pointer
/// is where the tab will be when you let go.
fn drag_tab_to(app: &mut App, column: u16, row: u16) {
    let Some(from) = app.tab_drag else { return };
    if Some(row) != app.tab_bar_row {
        return;
    }
    let Some((_, _, over)) = app
        .tab_spans
        .iter()
        .find(|(start, end, _)| column >= *start && column < *end)
        .copied()
    else {
        return;
    };
    // The chevrons at either end stand for the tab just off screen that way,
    // so dragging on to one carries this tab past the edge of the row.
    if app.move_tab_to(from, over) {
        app.tab_drag = Some(over);
        app.set_status(
            format!("moved to {}/{}", over + 1, app.tabs.len()),
            app::Level::Info,
        );
    }
}

fn handle_mouse(app: &mut App, m: event::MouseEvent) {
    // The scroll wheel is the only thing worth honouring in the output viewer.
    if app.mode == Mode::Output {
        match m.kind {
            MouseEventKind::ScrollDown => {
                app.output_scroll = app.output_scroll.saturating_add(3);
            }
            MouseEventKind::ScrollUp => {
                app.output_scroll = app.output_scroll.saturating_sub(3);
            }
            _ => {}
        }
        return;
    }
    if app.mode != Mode::Browse {
        return;
    }

    let at = Position::new(m.column, m.row);

    // A selection being dragged owns the mouse until the button comes back
    // up, and follows it out of the pane it started in — a drag that stopped
    // at the border would be a drag you had to be careful with.
    if let Some(slot) = app.selecting {
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                if let Some((col, row)) = cell_in(app, slot, &m)
                    && let Some(shell) = app.shell_mut(slot)
                {
                    shell.drag_selection(row, col);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                app.selecting = None;
                if let Some(text) = app.shell_mut(slot).and_then(shell::Shell::end_selection) {
                    app.copy(text);
                }
            }
            _ => {}
        }
        return;
    }

    // A pane being carried by its name follows the mouse until it is let go
    // of, and lands on whatever pane it is over.
    if app.moving.is_some() {
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                app.move_over = app.areas.at(m.column, m.row);
            }
            MouseEventKind::Up(MouseButton::Left) => app.drop_moved_pane(),
            _ => {}
        }
        return;
    }

    // A tab being dragged along the row owns the mouse until the button comes
    // back up, so that a pointer wandering off the row does not drop it.
    if app.tab_drag.is_some() {
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                drag_tab_to(app, m.column, m.row)
            }
            MouseEventKind::Up(MouseButton::Left) => app.tab_drag = None,
            _ => {}
        }
        return;
    }

    // A drag in progress owns the mouse until the button comes back up, so
    // that leaving the border behind while moving does not drop it.
    if app.drag.is_some() {
        match m.kind {
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Moved => {
                app.drag_to(m.column, m.row);
            }
            MouseEventKind::Up(MouseButton::Left) => app.drag = None,
            _ => {}
        }
        return;
    }

    // The new-tab button, on the top bar above everything else. It opens a
    // tab straight away, on this machine — a new tab is a new tab, and being
    // made to name a server first is the one thing a `[+]` should never do.
    // `C` is still there for a tab that starts somewhere else.
    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
        && app.new_tab_button.is_some_and(|rect| rect.contains(at))
    {
        app.open_local_tab();
        return;
    }

    // The buttons in a pane's corner sit on its border, so they are tested
    // before the resize zones underneath them.
    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
        if let Some((_, slot)) = app
            .close_buttons
            .iter()
            .find(|(rect, _)| rect.contains(at))
            .copied()
        {
            app.click_close_button(slot);
            return;
        }
        if let Some((_, slot)) = app
            .zoom_buttons
            .iter()
            .find(|(rect, _)| rect.contains(at))
            .copied()
        {
            app.click_zoom_button(slot);
            return;
        }
    }

    // A pane is picked up by its name, which is why the name is drawn where
    // it is. Tested before the borders: the name sits on one, and a pane you
    // meant to move is not a border you meant to drag.
    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some((_, slot)) = app
            .pane_titles
            .iter()
            .find(|(rect, _)| rect.contains(at))
            .copied()
    {
        app.focus_pane(slot);
        app.moving = Some(slot);
        app.move_over = None;
        app.set_status(
            "moving this pane — let go over another to change places",
            app::Level::Info,
        );
        return;
    }

    // Pressing on a border starts a resize rather than reaching the pane
    // underneath. Every border is two cells wide to grab: the two panes each
    // draw their own edge, and they sit against each other.
    if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(divider) = app
            .areas
            .dividers
            .iter()
            .find(|d| d.rect.contains(at))
            .cloned()
    {
        app.start_drag(&divider, m.column, m.row);
        return;
    }

    // The tab bar, when there is one.
    if Some(m.row) == app.tab_bar_row {
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            // The `✕` first: it sits inside the chip it belongs to, and a
            // click on it is never a click on the rest of the chip.
            if let Some((_, _, index)) = app
                .tab_close_buttons
                .iter()
                .find(|(start, end, _)| m.column >= *start && m.column < *end)
                .copied()
            {
                app.close_tab_at(index);
                return;
            }
            if let Some((_, _, index)) = app
                .tab_spans
                .iter()
                .find(|(start, end, _)| m.column >= *start && m.column < *end)
                .copied()
            {
                app.goto_tab(index);
                // Held down, the chip comes with the pointer. A press that
                // never moves is just the switch above, so nothing is said
                // about moving until something actually does.
                app.tab_drag = Some(app.active);
            }
        }
        return;
    }

    let Some(slot) = app.areas.at(m.column, m.row) else {
        return;
    };

    if slot.is_term() {
        // Clicking in a terminal puts the keyboard there, the same as clicking
        // in a file list does — whether or not the program inside then gets
        // the click as well.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            app.focus_pane(slot);
        }

        // A program that has asked for the mouse gets it: btop's clicks, a
        // pager's wheel. Shift is the way past that to the pane's own
        // scrollback and to picking text out, the same escape hatch a terminal
        // gives you.
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        if !shift
            && let Some((_, inner)) = app.term_inner.iter().find(|(s, _)| *s == slot).copied()
            && inner.contains(at)
        {
            let (col, row) = (m.column - inner.x, m.row - inner.y);
            if app
                .shell_mut(slot)
                .is_some_and(|shell| shell.send_mouse(&m, col, row))
            {
                return;
            }
        }

        // Nobody inside wanted it, so the mouse is ours: dragging picks text
        // out, the way it does in the terminal sshman is running in.
        if let Some((col, row)) = cell_in(app, slot, &m) {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    app.selecting = Some(slot);
                    if let Some(shell) = app.shell_mut(slot) {
                        shell.begin_selection(row, col);
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(shell) = app.shell_mut(slot) {
                        shell.clear_selection();
                    }
                    return;
                }
                _ => {}
            }
        }

        match m.kind {
            // Positive scrolls back into the shell's history.
            MouseEventKind::ScrollUp => {
                if let Some(shell) = app.shell_mut(slot) {
                    shell.scroll(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(shell) = app.shell_mut(slot) {
                    shell.scroll(-3);
                }
            }
            _ => {}
        }
        return;
    }

    let Some(area) = app.areas.of(slot) else {
        return;
    };
    match m.kind {
        MouseEventKind::ScrollDown => app.pane_mut(slot).move_by(3),
        MouseEventKind::ScrollUp => app.pane_mut(slot).move_by(-3),
        MouseEventKind::Down(MouseButton::Left) => {
            app.focus_pane(slot);
            // The first row inside the block sits just below its border.
            if m.row > area.y {
                let offset = app.pane(slot).state.offset();
                let index = offset + (m.row - area.y - 1) as usize;
                if index < app.pane(slot).view.len() {
                    app.click_row(slot, index);
                }
            }
        }
        _ => {}
    }
}

/// Where in a terminal pane's own grid the mouse is, clamped to it so a drag
/// that has left the pane still means the edge it left by.
fn cell_in(app: &App, slot: layout::Slot, m: &event::MouseEvent) -> Option<(u16, u16)> {
    let (_, inner) = app.term_inner.iter().find(|(s, _)| *s == slot).copied()?;
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let col = m.column.clamp(inner.x, inner.right() - 1) - inner.x;
    let row = m.row.clamp(inner.y, inner.bottom() - 1) - inner.y;
    Some((col, row))
}

/// Hand text to the terminal sshman is running in, so it reaches the system
/// clipboard.
///
/// OSC 52 is the only way that works from inside a terminal that may itself be
/// at the far end of an SSH connection: there is no display to talk to, only
/// the terminal, and it is the terminal that owns the clipboard.
fn to_clipboard(terminal: &mut Tui, text: &str) -> Result<()> {
    use base64::Engine;

    // Terminals cap what they will take — tmux's default is around 74k — so
    // more than this is sent as much as fits rather than being dropped whole.
    const LIMIT: usize = 64 * 1024;
    let end = (0..=LIMIT.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&text.as_bytes()[..end]);
    write!(terminal.backend_mut(), "\x1b]52;c;{encoded}\x07")?;
    terminal.backend_mut().flush()?;
    Ok(())
}

/// Hand the terminal back to the shell, run `f`, then take it over again.
fn suspended<T>(terminal: &mut Tui, f: impl FnOnce() -> T) -> Result<T> {
    disable_raw_mode()?;
    // The program about to run gets the terminal exactly as it found it,
    // including how keys are reported: an editor that asked for nothing
    // unusual should not be handed sshman's settings.
    disable_rich_keys(terminal.backend_mut());
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor().ok();
    io::stdout().flush().ok();

    let out = f();

    enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        Clear(ClearType::All)
    )?;
    if shell::rich_keys() {
        let _ = execute!(
            terminal.backend_mut(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
    terminal.hide_cursor().ok();
    force_full_redraw(terminal);
    Ok(out)
}

/// Make the next draw repaint every cell.
///
/// `Terminal::clear()` would also do this, but as of ratatui 0.30 it first asks
/// the terminal where the cursor is and blocks waiting for the reply. Terminals
/// that never answer turn that into an error, which would kill the session the
/// moment the user came back from their editor. Resetting both buffers has the
/// same effect with no round trip: `swap_buffers` wipes the back buffer and
/// flips, so calling it twice empties both and leaves the index where it began.
fn force_full_redraw(terminal: &mut Tui) {
    terminal.swap_buffers();
    terminal.swap_buffers();
}

/// Run `program` on `path` through a shell, so `EDITOR="code -w"` and friends
/// work as written.
fn run_editor(program: &str, path: &std::path::Path) -> Result<()> {
    let line = format!("{program} {}", sh_quote(&path.to_string_lossy()));
    let mut command = Command::new(shell());
    command.arg("-c").arg(&line);
    // Start it in the file's own directory rather than wherever sshman was
    // launched from. Editors work out what project they are in by looking
    // around where they start, so the difference decides whether tooling
    // finds the rest of the tree or nothing at all.
    if let Some(parent) = path.parent() {
        command.current_dir(parent);
    }
    let status = command
        .status()
        .with_context(|| format!("cannot run {program}"))?;
    if !status.success() {
        anyhow::bail!("exited with {status}");
    }
    Ok(())
}

fn run_shell(cmd: &str) -> Result<()> {
    Command::new(shell())
        .arg("-c")
        .arg(cmd)
        .status()
        .context("cannot start ssh")?;
    Ok(())
}

/// The `ssh` invocation for an interactive shell, starting in the directory
/// the remote pane is showing.
fn shell_command(app: &App) -> Option<String> {
    let tab = app.tab()?;
    // A tab on this machine needs no ssh to reach it: just a login shell,
    // where its pane is pointed.
    if tab.is_local() {
        let cwd = app.remote_cwd();
        return Some(format!(
            "cd {} 2>/dev/null; {}",
            sh_quote(&cwd),
            login_shell(app)
        ));
    }
    // A container is entered by running its runtime, not by dialling it. On a
    // server that means an ssh whose payload is the `exec`, so the two nest
    // rather than one replacing the other.
    if let Target::Docker {
        container, runtime, ..
    } = &tab.target
    {
        let inner = docker::interactive_shell_command(runtime, container, Some(&app.remote_cwd()));
        return Some(match tab.ssh_opts() {
            None => inner,
            Some(opts) => format!("{} {}", ssh_prefix(opts), sh_quote(&inner)),
        });
    }
    let conn = &tab.conn;
    let mut cmd = String::from("ssh -t");
    if conn.port != 22 {
        cmd.push_str(&format!(" -p {}", conn.port));
    }
    if let Some(key) = &app.opts.key_path {
        cmd.push_str(&format!(" -i {}", sh_quote(&key.to_string_lossy())));
    }
    cmd.push_str(&format!(" {}@{}", conn.user, conn.host));
    let cwd = app.remote_cwd();
    if !cwd.is_empty() {
        let inner = format!("cd {} 2>/dev/null; {}", sh_quote(&cwd), login_shell(app));
        cmd.push(' ');
        cmd.push_str(&sh_quote(&inner));
    }
    Some(cmd)
}

/// The `exec` that hands the whole terminal to a shell, as a shell line.
///
/// Without a setting that is `"$SHELL" -l`, which keeps the login shell and
/// its rc files wherever the line ends up running — this machine or a server.
/// With one, it is that shell, guarded so a server that has never heard of it
/// falls back to the login shell rather than to nothing at all.
fn login_shell(app: &App) -> String {
    let fallback = "exec \"$SHELL\" -l";
    match app.config.shell() {
        None => fallback.to_string(),
        Some(shell) => format!(
            "command -v {} >/dev/null 2>&1 && exec {shell}; {fallback}",
            sh_quote(shell.split_whitespace().next().unwrap_or(shell)),
        ),
    }
}

/// `ssh -t` with the details needed to reach `opts`, for a command that runs
/// on the far end.
fn ssh_prefix(opts: &ConnectOpts) -> String {
    let mut cmd = String::from("ssh -t");
    if opts.port != 22 {
        cmd.push_str(&format!(" -p {}", opts.port));
    }
    if let Some(key) = &opts.key_path {
        cmd.push_str(&format!(" -i {}", sh_quote(&key.to_string_lossy())));
    }
    cmd.push_str(&format!(" {}@{}", opts.user, opts.host));
    cmd
}

fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}
