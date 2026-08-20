//! sshman — a two-pane SSH file manager.
//!
//! Left pane is your machine, right pane is the server. Copy either way,
//! open files in whatever editor you like, run remote commands, and flip the
//! remote side into sudo mode when you need to see root-only paths.

mod app;
mod archive;
mod backend;
mod docker;
mod forward;
mod history;
mod input;
mod local;
mod shell;
mod sshcfg;
mod sshconn;
mod types;
mod ui;
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
    Event, KeyEventKind, MouseButton, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::Position;

use app::{App, Mode, Region, Side, UiAction};
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

    /// Container runtime to use: docker, podman, or a path. Detected when
    /// not given, preferring docker.
    #[arg(long, value_name = "PROGRAM")]
    runtime: Option<String>,

    /// Open a saved workspace: reconnects everything it holds
    #[arg(short = 'w', long, value_name = "NAME")]
    workspace: Option<String>,

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
        return Ok(());
    }

    let (opts, auto_connect) = build_opts(&args);
    let local_start = match &args.local_path {
        Some(p) => local::expand(&p.to_string_lossy()),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };

    // A named workspace takes over from the connection screen: it already
    // says what to connect to.
    let requested = match &args.workspace {
        Some(name) => match workspace::Workspaces::load().find(name).cloned() {
            Some(found) => Some(found),
            None => anyhow::bail!("no workspace called {name:?} — see `sshman --list-workspaces`"),
        },
        None => None,
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
    if args.docker {
        app.browse_local_containers();
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
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
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

        // 2. Anything that needs the terminal to itself.
        if let Some(action) = app.pending_action.take() {
            match action {
                UiAction::Quit => return Ok(()),
                UiAction::Editor {
                    program,
                    path,
                    push_back,
                    refresh_local,
                } => {
                    let outcome = suspended(terminal, || run_editor(&program, &path))?;
                    match outcome {
                        Ok(()) => app.after_editor(push_back, refresh_local),
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

        // 3. Draw.
        terminal.draw(|f| ui::draw(f, app))?;

        // 4. Wait briefly for input. The timeout is what lets worker messages
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

        if app.should_quit {
            return Ok(());
        }
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

    // The tab bar, when there is one.
    if Some(m.row) == app.tab_bar_row {
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some((_, _, index)) = app
                .tab_spans
                .iter()
                .find(|(start, end, _)| m.column >= *start && m.column < *end)
                .copied()
        {
            app.goto_tab(index);
        }
        return;
    }

    // Shell panes sit inside their side's column, so test them first.
    for side in [Side::Local, Side::Remote] {
        let Some(area) = app.shell_area[side.index()] else {
            continue;
        };
        if !area.contains(at) {
            continue;
        }
        match m.kind {
            // Positive scrolls back into the shell's history.
            MouseEventKind::ScrollUp => {
                if let Some(shell) = app.shell_mut(side) {
                    shell.scroll(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(shell) = app.shell_mut(side) {
                    shell.scroll(-3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                app.focus = side;
                app.region = Region::Shell;
            }
            _ => {}
        }
        return;
    }

    for side in [Side::Local, Side::Remote] {
        let area = app.files_area[side.index()];
        if !area.contains(at) {
            continue;
        }
        match m.kind {
            MouseEventKind::ScrollDown => app.pane_mut(side).move_by(3),
            MouseEventKind::ScrollUp => app.pane_mut(side).move_by(-3),
            MouseEventKind::Down(MouseButton::Left) => {
                app.focus = side;
                app.region = Region::Files;
                // The first row inside the block sits just below its border.
                if m.row > area.y {
                    let offset = app.pane(side).state.offset();
                    let index = offset + (m.row - area.y - 1) as usize;
                    if index < app.pane(side).view.len() {
                        app.pane_mut(side).select_index(index);
                    }
                }
            }
            _ => {}
        }
        return;
    }
}

/// Hand the terminal back to the shell, run `f`, then take it over again.
fn suspended<T>(terminal: &mut Tui, f: impl FnOnce() -> T) -> Result<T> {
    disable_raw_mode()?;
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
    let status = Command::new(shell())
        .arg("-c")
        .arg(&line)
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
    let conn = &app.tab()?.conn;
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
        // `exec $SHELL -l` keeps the user's normal login shell and rc files.
        let inner = format!("cd {} 2>/dev/null; exec \"$SHELL\" -l", sh_quote(&cwd));
        cmd.push(' ');
        cmd.push_str(&sh_quote(&inner));
    }
    Some(cmd)
}

fn shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}
