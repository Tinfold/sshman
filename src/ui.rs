//! All drawing. Nothing here mutates application state except the list
//! scroll offsets that ratatui owns.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap,
};

use tui_term::widget::PseudoTerminal;

use crate::app::{App, ConnectFocus, ConnectForm, Level, LinkState, Mode, Pane, Region, Side};
use crate::types::{EntryKind, FileEntry, ellipsize, fmt_time, human_size};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let gauge_h = if app.progress.is_some() { 1 } else { 0 };
    // One connection needs no tab bar — the title bar already names it.
    let tabs_h = if app.tabs.len() > 1 { 1 } else { 0 };
    let rows = Layout::vertical([
        Constraint::Length(1), // title bar
        Constraint::Length(tabs_h),
        Constraint::Min(3), // panes
        Constraint::Length(gauge_h),
        Constraint::Length(1), // status
        Constraint::Length(1), // key hints
    ])
    .split(area);

    draw_title_bar(f, app, rows[0]);
    app.tab_spans.clear();
    app.tab_bar_row = None;
    if tabs_h == 1 {
        draw_tab_bar(f, app, rows[1]);
    }

    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(rows[2]);

    draw_side(f, app, Side::Local, cols[0]);
    draw_side(f, app, Side::Remote, cols[1]);

    if gauge_h == 1 {
        draw_progress(f, app, rows[3]);
    }
    draw_status(f, app, rows[4]);
    draw_hints(f, app, rows[5]);

    match app.mode {
        Mode::Connect => draw_connect(f, app, area),
        Mode::Picker => draw_picker(f, app, area),
        Mode::Workspaces => draw_workspaces(f, app, area),
        Mode::Forwards => draw_forwards(f, app, area),
        Mode::Prompt => draw_prompt(f, app, area),
        Mode::Confirm => draw_confirm(f, app, area),
        Mode::Output => draw_output(f, app, area),
        Mode::Help => draw_help(f, app, area),
        Mode::Browse => {}
    }
}

fn draw_title_bar(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(" sshman ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
        Span::raw(" "),
    ];
    match app.tab() {
        Some(tab) => {
            let c = &tab.conn;
            let (colour, suffix) = match tab.link {
                LinkState::Live => (Color::Green, ""),
                LinkState::Reconnecting => (Color::Yellow, "  ⟳ reconnecting…"),
                LinkState::Lost => (Color::Red, "  ✗ disconnected"),
            };
            let _ = c;
            spans.push(Span::styled(tab.title(), Style::new().fg(colour).bold()));
            if tab.is_container() {
                spans.push(Span::styled(
                    "  container",
                    Style::new().fg(Color::Blue).bold(),
                ));
            }
            if !suffix.is_empty() {
                spans.push(Span::styled(suffix, Style::new().fg(colour).bold()));
            }
            if app.tabs.len() > 1 {
                spans.push(Span::styled(
                    format!("  tab {}/{}", app.active + 1, app.tabs.len()),
                    Style::new().fg(DIM),
                ));
            }
        }
        None => spans.push(Span::styled(
            "not connected",
            Style::new().fg(Color::Red).bold(),
        )),
    }
    if app.sudo() {
        spans.push(Span::raw("  "));
        // A container's elevation is `-u 0`, not sudo; naming it accurately
        // matters when the two are side by side in different tabs.
        let label = if app.tab().is_some_and(|t| t.is_container()) {
            " ROOT "
        } else {
            " SUDO "
        };
        spans.push(Span::styled(
            label,
            Style::new().fg(Color::Black).bg(Color::Red).bold(),
        ));
    }
    // A workspace can leave connections waiting on a password. That must not
    // scroll away with the next status message, so it lives here.
    let forwards = app.forward_count();
    if forwards > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" ⇄ {forwards} "),
            Style::new().fg(Color::Black).bg(Color::Blue).bold(),
        ));
    }
    if !app.needs_password.is_empty() {
        let waiting: Vec<&str> = app
            .needs_password
            .iter()
            .map(|(label, _)| label.as_str())
            .collect();
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" needs a password: {} — press C ", waiting.join(", ")),
            Style::new().fg(Color::Black).bg(Color::Yellow).bold(),
        ));
    }
    if let Some(task) = app.tasks.iter().rev().find(|t| !t.is_empty()) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("● {task}"),
            Style::new().fg(Color::Yellow),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The row of open servers. Each chip carries its number, so `Alt-<n>` is
/// discoverable without reading the help.
fn draw_tab_bar(f: &mut Frame, app: &mut App, area: Rect) {
    app.tab_bar_row = Some(area.y);
    let mut spans = Vec::new();
    let mut x = area.x;

    for (index, tab) in app.tabs.iter().enumerate() {
        let active = index == app.active;
        let marker = match tab.link {
            LinkState::Live => "",
            LinkState::Reconnecting => " ⟳",
            LinkState::Lost => " ✗",
        };
        let sudo = if tab.sudo { " #" } else { "" };
        let text = format!(" {} {}{sudo}{marker} ", index + 1, tab.title());
        let width = text.chars().count() as u16;

        let style = if active {
            Style::new().fg(Color::Black).bg(ACCENT).bold()
        } else if tab.link != LinkState::Live {
            Style::new().fg(Color::Red)
        } else {
            Style::new().fg(Color::Gray)
        };
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));

        app.tab_spans.push((x, x + width, index));
        x += width + 1;
        if x >= area.right() {
            break;
        }
    }
    spans.push(Span::styled(
        "  T new · W close · Ctrl-←/→ switch",
        Style::new().fg(DIM),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One side of the screen: the file list, and its shell underneath when open.
fn draw_side(f: &mut Frame, app: &mut App, side: Side, area: Rect) {
    let (files_area, shell_area) = match app.has_shell(side) {
        // Always leave the file list at least three rows, however far the
        // shell has been grown.
        true => {
            let height = app.shell_height.min(area.height.saturating_sub(3)).max(3);
            let parts =
                Layout::vertical([Constraint::Min(3), Constraint::Length(height)]).split(area);
            (parts[0], Some(parts[1]))
        }
        false => (area, None),
    };

    app.files_area[side.index()] = files_area;
    app.shell_area[side.index()] = shell_area;

    let label = match side {
        Side::Local => "LOCAL",
        Side::Remote => "REMOTE",
    };
    let path = app.path_of(side);
    let sudo = side == Side::Remote && app.sudo();
    let live = side == Side::Local || app.connected();
    let files_focused = app.focus == side && app.region == Region::Files;
    let shell_focused = app.focus == side && app.region == Region::Shell;

    draw_pane(
        f,
        files_area,
        label,
        &path,
        app.pane_mut(side),
        files_focused,
        sudo,
        live,
    );

    if let Some(area) = shell_area {
        draw_shell(f, app, side, area, shell_focused);
    }
}

/// The embedded terminal. The vt100 screen borrows its parser, so the widget
/// has to be built and rendered inside the lock.
fn draw_shell(f: &mut Frame, app: &mut App, side: Side, area: Rect, focused: bool) {
    let alive = app.shell(side).map(|s| s.is_alive()).unwrap_or(false);
    let label = app.shell(side).map(|s| s.label.clone()).unwrap_or_default();
    let scrolled = app.shell(side).map(|s| s.scrollback()).unwrap_or(0);

    let colour = if !alive {
        Color::DarkGray
    } else if side == Side::Remote && app.sudo() {
        Color::Red
    } else if focused {
        ACCENT
    } else {
        Color::Gray
    };

    let mut title = vec![
        Span::styled(" SHELL ", Style::new().fg(colour).bold()),
        Span::styled(label, Style::new().fg(Color::White)),
    ];
    if !alive {
        title.push(Span::styled(" [exited]", Style::new().fg(Color::DarkGray)));
    }
    title.push(Span::raw(" "));

    let mut hint = Vec::new();
    if scrolled > 0 {
        hint.push(Span::styled(
            format!(" ↑{scrolled} lines back "),
            Style::new().fg(Color::Yellow),
        ));
    }
    hint.push(Span::styled(
        if focused {
            " F6 back to files "
        } else {
            " F6 to focus "
        },
        Style::new().fg(DIM),
    ));

    let block = Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(if focused {
            Style::new().fg(colour).bold()
        } else {
            Style::new().fg(DIM)
        })
        .title_top(Line::from(title))
        .title_bottom(Line::from(hint).right_aligned());

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(shell) = app.shell_mut(side) else {
        return;
    };
    // Keep the emulator and the far end matched to the space we actually drew.
    shell.ensure_size(inner.height, inner.width);
    let cursor = focused.then(|| shell.cursor()).flatten();
    shell.with_screen(|screen| {
        f.render_widget(PseudoTerminal::new(screen), inner);
    });

    if let Some((row, col)) = cursor {
        let (x, y) = (inner.x + col, inner.y + row);
        if x < inner.right() && y < inner.bottom() {
            f.set_cursor_position((x, y));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    f: &mut Frame,
    area: Rect,
    label: &str,
    path: &str,
    pane: &mut Pane,
    focused: bool,
    sudo: bool,
    live: bool,
) {
    let border_style = if focused {
        Style::new().fg(ACCENT).bold()
    } else {
        Style::new().fg(DIM)
    };
    let label_style = if sudo {
        Style::new().fg(Color::Red).bold()
    } else if focused {
        Style::new().fg(ACCENT).bold()
    } else {
        Style::new().fg(Color::Gray)
    };

    // Reserve room for the label and the borders when shortening the path.
    let path_room = area.width.saturating_sub(label.len() as u16 + 6) as usize;
    let title = Line::from(vec![
        Span::styled(format!(" {label} "), label_style),
        Span::styled(
            ellipsize(path, path_room.max(8)),
            Style::new().fg(Color::White),
        ),
        Span::raw(" "),
    ]);

    let mut bottom = Vec::new();
    if !pane.filter.is_empty() {
        bottom.push(Span::styled(
            format!(" /{} ", pane.filter),
            Style::new().fg(Color::Yellow),
        ));
    }
    if !pane.marked.is_empty() {
        bottom.push(Span::styled(
            format!(" {} marked ", pane.marked.len()),
            Style::new().fg(Color::Yellow).bold(),
        ));
    }
    bottom.push(Span::styled(
        format!(" {} items ", pane.view.len()),
        Style::new().fg(DIM),
    ));

    let block = Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(border_style)
        .title_top(title)
        .title_bottom(Line::from(bottom).right_aligned());

    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = &pane.error {
        let p = Paragraph::new(vec![
            Line::from(Span::styled(
                "cannot read this directory",
                Style::new().fg(Color::Red).bold(),
            )),
            Line::raw(""),
            Line::from(Span::styled(err.clone(), Style::new().fg(Color::Red))),
            Line::raw(""),
            Line::from(Span::styled(
                "press s to try again as root",
                Style::new().fg(DIM),
            )),
        ])
        .wrap(Wrap { trim: true });
        f.render_widget(p, inner.inner(Margin::new(1, 1)));
        return;
    }

    if !live {
        let p = Paragraph::new("no connection")
            .style(Style::new().fg(DIM))
            .alignment(Alignment::Center);
        f.render_widget(p, centered_line(inner));
        return;
    }

    if pane.loading {
        let p = Paragraph::new("loading…")
            .style(Style::new().fg(Color::Yellow))
            .alignment(Alignment::Center);
        f.render_widget(p, centered_line(inner));
        return;
    }

    if pane.view.is_empty() {
        // "empty directory" would be a lie when everything in it is hidden.
        let msg = if !pane.filter.is_empty() {
            "nothing matches the filter".to_string()
        } else if pane.all.is_empty() {
            "empty directory".to_string()
        } else {
            format!("{} hidden entries — press . to show", pane.all.len())
        };
        let p = Paragraph::new(msg)
            .style(Style::new().fg(DIM))
            .alignment(Alignment::Center);
        f.render_widget(p, centered_line(inner));
        return;
    }

    let cols = Columns::for_width(inner.width);
    let items: Vec<ListItem> = pane
        .view
        .iter()
        .map(|e| ListItem::new(entry_line(e, pane.marked.contains(&e.name), cols)))
        .collect();

    let list = List::new(items).highlight_style(
        Style::new()
            .bg(if focused { ACCENT } else { DIM })
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut pane.state);
}

/// Which columns fit in the available width. Names always win; the metadata
/// columns drop off from the left as the pane narrows.
#[derive(Clone, Copy)]
struct Columns {
    perms: bool,
    size: bool,
    time: bool,
}

impl Columns {
    fn for_width(w: u16) -> Self {
        Self {
            time: w >= 56,
            size: w >= 28,
            perms: w >= 40,
        }
    }
}

fn entry_line<'a>(e: &FileEntry, marked: bool, cols: Columns) -> Line<'a> {
    let mut spans = Vec::new();
    spans.push(if marked {
        Span::styled("*", Style::new().fg(Color::Yellow).bold())
    } else {
        Span::raw(" ")
    });

    if cols.perms {
        spans.push(Span::styled(
            format!("{:<10} ", e.perms),
            Style::new().fg(DIM),
        ));
    }
    if cols.size {
        let text = if e.kind == EntryKind::Dir {
            "<DIR>".to_string()
        } else {
            human_size(e.size)
        };
        spans.push(Span::styled(
            format!("{text:>8} "),
            Style::new().fg(Color::Gray),
        ));
    }
    if cols.time {
        let text = if e.mtime == 0 {
            "               -".to_string()
        } else {
            fmt_time(e.mtime)
        };
        spans.push(Span::styled(format!("{text} "), Style::new().fg(DIM)));
    }

    let name_style = match e.kind {
        EntryKind::Dir => Style::new().fg(Color::Blue).bold(),
        EntryKind::Symlink => Style::new().fg(Color::Magenta),
        EntryKind::Other => Style::new().fg(Color::Yellow),
        EntryKind::File => {
            if e.perms.contains('x') {
                Style::new().fg(Color::Green)
            } else {
                Style::new()
            }
        }
    };
    let mut name = e.name.clone();
    if e.is_dir_like() {
        name.push('/');
    }
    spans.push(Span::styled(name, name_style));

    if let Some(target) = &e.link_target {
        spans.push(Span::styled(
            format!(" → {target}"),
            Style::new().fg(DIM).italic(),
        ));
    }
    Line::from(spans)
}

fn draw_progress(f: &mut Frame, app: &App, area: Rect) {
    let Some((label, done, total)) = &app.progress else {
        return;
    };
    let ratio = if *total == 0 {
        0.0
    } else {
        (*done as f64 / *total as f64).clamp(0.0, 1.0)
    };
    let text = if *total == 0 {
        format!("{label}  {}", human_size(*done))
    } else {
        format!(
            "{label}  {} / {}  ({:.0}%)",
            human_size(*done),
            human_size(*total),
            ratio * 100.0
        )
    };
    let gauge = Gauge::default()
        .gauge_style(Style::new().fg(ACCENT).bg(Color::Black))
        .ratio(ratio)
        .label(text);
    f.render_widget(gauge, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let style = match app.status_level {
        Level::Info => Style::new().fg(Color::White),
        Level::Good => Style::new().fg(Color::Green),
        Level::Bad => Style::new().fg(Color::Red).bold(),
    };
    let icon = match app.status_level {
        Level::Info => "  ",
        Level::Good => "✓ ",
        Level::Bad => "✗ ",
    };
    let text = format!("{icon}{}", app.status);
    f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

fn draw_hints(f: &mut Frame, app: &App, area: Rect) {
    let hints: &[(&str, &str)] = match app.mode {
        // A focused shell takes every key, so only the way out is worth showing.
        Mode::Browse if app.region == Region::Shell => &[
            ("F6", "back to files"),
            ("Ctrl-]", "same"),
            ("", "every other key goes to the shell"),
        ],
        Mode::Browse => &[
            ("Tab", "pane"),
            ("↵", "open"),
            ("Space", "mark"),
            ("c", "copy →"),
            ("e", "edit"),
            ("S", "shell"),
            ("T", "tab"),
            ("w", "workspaces"),
            ("p", "ports"),
            (":", "cmd"),
            ("s", "sudo"),
            ("d", "del"),
            ("?", "help"),
            ("q", "quit"),
        ],
        Mode::Connect => &[
            ("Tab", "section"),
            ("↑↓", "choose"),
            ("↵", "connect"),
            ("Del", "forget"),
            ("Esc", "back"),
        ],
        Mode::Prompt => &[("↵", "confirm"), ("Esc", "cancel")],
        Mode::Confirm
            if app
                .confirm
                .as_ref()
                .is_some_and(|c| c.require_phrase.is_some()) =>
        {
            &[("type the word", "then ↵"), ("Esc", "cancel")]
        }
        Mode::Confirm => &[("y", "yes"), ("n", "no")],
        Mode::Picker => &[("↑↓", "choose"), ("↵", "open in a tab"), ("Esc", "cancel")],
        Mode::Forwards => &[
            ("a", "add"),
            ("d", "stop"),
            ("↑↓", "choose"),
            ("Esc", "close"),
        ],
        Mode::Workspaces => &[
            ("↑↓", "choose"),
            ("↵", "open"),
            ("s", "save what is open"),
            ("Del", "forget"),
            ("Esc", "close"),
        ],
        Mode::Output => &[("↑↓", "scroll"), ("Esc", "close")],
        Mode::Help => &[("↑↓", "scroll"), ("any key", "close")],
    };
    let mut spans = Vec::new();
    for (k, v) in hints {
        if !k.is_empty() {
            spans.push(Span::styled(
                format!(" {k} "),
                Style::new().fg(Color::Black).bg(Color::Gray),
            ));
        }
        spans.push(Span::styled(format!(" {v}  "), Style::new().fg(DIM)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---- overlays --------------------------------------------------------------

/// How many saved servers to show at once before the list scrolls.
const RECENT_WINDOW: usize = 7;

fn draw_connect(f: &mut Frame, app: &App, area: Rect) {
    let shown = app.history.len().min(RECENT_WINDOW);
    // header + rows + blank separator, or nothing at all when there is no
    // history to offer.
    let recent_height = if app.history.is_empty() { 0 } else { shown + 2 };
    // Errors can run long, so reserve two rows for the wrap.
    let extra = usize::from(app.form.error.is_some()) * 2
        + usize::from(app.form.hint.is_some())
        + usize::from(app.form.connecting);
    let height = (ConnectForm::FIELDS + recent_height + extra + 4) as u16;
    let rect = centered(area, 78, height);
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            " Connect to a server ",
            Style::new().fg(ACCENT).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " Tab switches section · Enter connects ",
                Style::new().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 1));
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = Vec::new();
    let on_list = app.connect_focus == ConnectFocus::Recent;

    if !app.history.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "Recent servers",
                if on_list {
                    Style::new().fg(ACCENT).bold()
                } else {
                    Style::new().fg(Color::Gray).bold()
                },
            ),
            Span::styled("   ↑↓ choose · Del forgets", Style::new().fg(DIM).italic()),
        ]));

        // Scroll the window so the selection is always visible.
        let start = app.history_sel.saturating_sub(RECENT_WINDOW - 1);
        for (offset, entry) in app
            .history
            .entries
            .iter()
            .enumerate()
            .skip(start)
            .take(RECENT_WINDOW)
        {
            let selected = offset == app.history_sel;
            let (marker, style) = match (selected, on_list) {
                (true, true) => ("▸ ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
                (true, false) => ("▸ ", Style::new().fg(ACCENT)),
                _ => ("  ", Style::new().fg(Color::White)),
            };
            let key_note = match &entry.key_path {
                Some(path) => format!("  key: {}", crate::types::rbasename(path)),
                None => String::new(),
            };
            // A named server shows its address too, so two similarly named
            // ones stay distinguishable.
            let address = if entry.has_name() {
                format!("  {}", entry.address())
            } else {
                String::new()
            };
            lines.push(Line::from(vec![
                Span::styled(marker, Style::new().fg(ACCENT)),
                Span::styled(format!("{:<22}", ellipsize(&entry.label(), 22)), style),
                Span::styled(format!("{address:<20}"), Style::new().fg(Color::Gray)),
                Span::styled(
                    format!("{:>9}", crate::history::relative_time(entry.last_connected)),
                    Style::new().fg(DIM),
                ),
                Span::styled(key_note, Style::new().fg(DIM).italic()),
            ]));
        }
        lines.push(Line::raw(""));
    }

    let labels = ["Host", "Port", "User", "Key file", "Password", "Name"];
    let values = [
        app.form.host.display(),
        app.form.port.display(),
        app.form.user.display(),
        app.form.key.display(),
        app.form.password.display(),
        app.form.name.display(),
    ];
    let placeholders = [
        "hostname or user@host (~/.ssh/config aliases work)",
        "22",
        "$USER",
        "blank = ssh-agent, then ~/.ssh/id_*",
        "blank = key auth only",
        "optional — what you want to call this server",
    ];

    let form_start = lines.len() as u16;
    for i in 0..ConnectForm::CHECKBOX {
        let focused = app.connect_focus == ConnectFocus::Form && app.form.field == i;
        let marker = if focused { "▸ " } else { "  " };
        let label_style = if focused {
            Style::new().fg(ACCENT).bold()
        } else {
            Style::new().fg(Color::Gray)
        };
        let (value, value_style) = if values[i].is_empty() {
            (placeholders[i].to_string(), Style::new().fg(DIM).italic())
        } else {
            (values[i].clone(), Style::new().fg(Color::White))
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::new().fg(ACCENT)),
            Span::styled(format!("{:<10}", labels[i]), label_style),
            Span::styled(value, value_style),
        ]));
    }

    // The checkbox row.
    {
        let focused =
            app.connect_focus == ConnectFocus::Form && app.form.field == ConnectForm::CHECKBOX;
        let marker = if focused { "▸ " } else { "  " };
        let box_glyph = if app.form.install_key { "[x]" } else { "[ ]" };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::new().fg(ACCENT)),
            Span::styled(
                format!("{box_glyph} "),
                if app.form.install_key {
                    Style::new().fg(Color::Green).bold()
                } else {
                    Style::new().fg(Color::Gray)
                },
            ),
            Span::styled(
                "Install my public key for passwordless login",
                if focused {
                    Style::new().fg(ACCENT).bold()
                } else {
                    Style::new().fg(Color::Gray)
                },
            ),
            Span::styled(
                if focused { "   (Space toggles)" } else { "" },
                Style::new().fg(DIM).italic(),
            ),
        ]));
    }

    if app.form.connecting {
        lines.push(Line::from(Span::styled(
            "  connecting…",
            Style::new().fg(Color::Yellow).bold(),
        )));
    }
    if let Some(err) = &app.form.error {
        lines.push(Line::from(Span::styled(
            format!("  {err}"),
            Style::new().fg(Color::Red).bold(),
        )));
    }
    if let Some(hint) = &app.form.hint {
        lines.push(Line::from(Span::styled(
            format!("  {hint}"),
            Style::new().fg(Color::Yellow).bold(),
        )));
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    // Park the terminal cursor in the focused field so typing feels native.
    // On the list there is no text entry, so no cursor is shown.
    if app.connect_focus == ConnectFocus::Form && app.form.field < ConnectForm::CHECKBOX {
        let cursor_row = inner.y + form_start + app.form.field as u16;
        let cursor_col = inner.x + 12 + cursor_offset(app) as u16;
        if cursor_row < inner.bottom() && cursor_col < inner.right() {
            f.set_cursor_position((cursor_col, cursor_row));
        }
    }
}

fn cursor_offset(app: &App) -> usize {
    match app.form.field {
        0 => app.form.host.cursor,
        1 => app.form.port.cursor,
        2 => app.form.user.cursor,
        3 => app.form.key.cursor,
        4 => app.form.password.cursor,
        _ => app.form.name.cursor,
    }
}

/// The container chooser: one row per running container.
fn draw_picker(f: &mut Frame, app: &App, area: Rect) {
    let Some(picker) = &app.picker else { return };

    let height = (picker.items.len() as u16 + 4).min(area.height.saturating_sub(4));
    let rect = centered(area, 92, height.max(7));
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            format!(" {} ", picker.title),
            Style::new().fg(ACCENT).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓ choose · ↵ opens it in a tab · Esc cancels ",
                Style::new().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let name_width = 24usize;
    let image_width = 28usize;
    let items: Vec<ListItem> = picker
        .items
        .iter()
        .map(|c| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<name_width$} ", ellipsize(&c.name, name_width)),
                    Style::new().fg(Color::White).bold(),
                ),
                Span::styled(
                    format!("{:<image_width$} ", ellipsize(&c.image, image_width)),
                    Style::new().fg(Color::Gray),
                ),
                Span::styled(c.status.clone(), Style::new().fg(Color::Green)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(picker.selected));
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state);
}

/// Saved sets of connections.
fn draw_workspaces(f: &mut Frame, app: &App, area: Rect) {
    let rows = app.workspaces.len().max(1) as u16;
    let rect = centered(
        area,
        84,
        (rows + 4).min(area.height.saturating_sub(4)).max(7),
    );
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            " Workspaces ",
            Style::new().fg(ACCENT).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↵ open · s saves what is open now · Del forgets · Esc closes ",
                Style::new().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    if app.workspaces.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No workspaces saved yet.",
                    Style::new().fg(Color::White),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Open the servers and containers you want, then press s to",
                    Style::new().fg(DIM),
                )),
                Line::from(Span::styled(
                    "save them together under a name.",
                    Style::new().fg(DIM),
                )),
            ]),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .workspaces
        .entries
        .iter()
        .map(|w| {
            // The members are the point, so name them rather than just counting.
            let members: Vec<String> = w.items.iter().map(|i| i.describe()).collect();
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<18} ", ellipsize(&w.name, 18)),
                    Style::new().fg(Color::White).bold(),
                ),
                Span::styled(
                    format!("{:<15} ", w.summary()),
                    Style::new().fg(Color::Gray),
                ),
                Span::styled(
                    ellipsize(&members.join(", "), 42),
                    Style::new().fg(DIM).italic(),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.workspace_sel));
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state);
}

/// Ports carried from the server on screen to this machine.
fn draw_forwards(f: &mut Frame, app: &App, area: Rect) {
    let forwards: &[crate::forward::Forward] = match app.tab() {
        Some(tab) => &tab.forwards,
        None => &[],
    };
    let title = app.tab().map(|t| t.title()).unwrap_or_default();

    let rect = centered(
        area,
        78,
        (forwards.len() as u16 + 5)
            .min(area.height.saturating_sub(4))
            .max(8),
    );
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            format!(" Forwarded ports — {title} "),
            Style::new().fg(ACCENT).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " a adds · d stops · Esc closes ",
                Style::new().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    if forwards.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Nothing forwarded from this server.",
                    Style::new().fg(Color::White),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Press a and give a port: 3000 forwards it to the same port",
                    Style::new().fg(DIM),
                )),
                Line::from(Span::styled(
                    "here; 8080:3000 changes the local one; 8080:db:5432 reaches",
                    Style::new().fg(DIM),
                )),
                Line::from(Span::styled(
                    "a host the server can see. Saved with the workspace.",
                    Style::new().fg(DIM),
                )),
            ]),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = forwards
        .iter()
        .map(|forward| {
            let (state, style) = match (forward.is_running(), forward.error()) {
                (true, _) => ("listening".to_string(), Style::new().fg(Color::Green)),
                (false, Some(err)) => (err, Style::new().fg(Color::Red)),
                (false, None) => ("stopped".into(), Style::new().fg(Color::DarkGray)),
            };
            let carried = forward.connection_count();
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<34}", forward.spec.describe()),
                    Style::new().fg(Color::White).bold(),
                ),
                Span::styled(format!("{:<12}", ellipsize(&state, 12)), style),
                Span::styled(
                    match carried {
                        0 => "no connections yet".to_string(),
                        1 => "1 connection".to_string(),
                        n => format!("{n} connections"),
                    },
                    Style::new().fg(DIM),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.forward_sel()));
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(ACCENT)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state);
}

fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let Some(prompt) = &app.prompt else { return };
    let rect = centered(area, 78, 5);
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            format!(" {} ", prompt.title),
            Style::new().fg(ACCENT).bold(),
        )));
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let value = prompt.input.display();
    // Scroll the field horizontally once the text outruns the box.
    let width = inner.width.saturating_sub(2) as usize;
    let skip = prompt.input.cursor.saturating_sub(width);
    let shown: String = value.chars().skip(skip).take(width).collect();

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("› ", Style::new().fg(ACCENT).bold()),
            Span::raw(shown),
        ])),
        Rect {
            y: inner.y + 1,
            height: 1,
            ..inner
        },
    );
    let col = inner.x + 2 + (prompt.input.cursor - skip) as u16;
    if col < inner.right() {
        f.set_cursor_position((col, inner.y + 1));
    }
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let Some(state) = &app.confirm else { return };
    let extra = if state.require_phrase.is_some() { 3 } else { 0 };
    let height = (state.body.len() as u16 + 5 + extra).min(area.height.saturating_sub(2));
    let rect = centered(area, 74, height.max(7));
    f.render_widget(Clear, rect);

    let color = if state.danger {
        Color::Red
    } else {
        Color::Yellow
    };
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(color))
        .title_top(Line::from(Span::styled(
            format!(" {} ", state.title),
            Style::new().fg(color).bold(),
        )));
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let mut lines: Vec<Line> = state
        .body
        .iter()
        .map(|l| Line::from(Span::raw(l.clone())))
        .collect();
    lines.push(Line::raw(""));

    match &state.require_phrase {
        // A typed phrase instead of a single keypress, so the dangerous answer
        // cannot be given by reflex.
        Some(word) => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("Type {word} to continue: "),
                    Style::new().fg(color).bold(),
                ),
                Span::styled(state.input.display(), Style::new().fg(Color::White)),
                Span::styled("▏", Style::new().fg(color)),
            ]));
            lines.push(Line::raw(""));
            let ready = state.satisfied();
            lines.push(Line::from(vec![
                Span::styled(
                    " Enter ",
                    if ready {
                        Style::new().fg(Color::Black).bg(color).bold()
                    } else {
                        Style::new().fg(Color::DarkGray).bg(Color::Black)
                    },
                ),
                Span::styled(
                    if ready {
                        " go ahead    "
                    } else {
                        " (type the word first)    "
                    },
                    Style::new().fg(DIM),
                ),
                Span::styled(
                    " Esc ",
                    Style::new().fg(Color::Black).bg(Color::Gray).bold(),
                ),
                Span::raw(" cancel"),
            ]));
        }
        None => lines.push(Line::from(vec![
            Span::styled(" y ", Style::new().fg(Color::Black).bg(color).bold()),
            Span::raw(" yes    "),
            Span::styled(" n ", Style::new().fg(Color::Black).bg(Color::Gray).bold()),
            Span::raw(" no"),
        ])),
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_output(f: &mut Frame, app: &mut App, area: Rect) {
    let rect = centered(
        area,
        area.width.saturating_sub(8),
        area.height.saturating_sub(4),
    );
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            format!(
                " {} ",
                ellipsize(&app.output_title, rect.width.saturating_sub(4) as usize)
            ),
            Style::new().fg(ACCENT).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓/PgUp/PgDn scroll · Esc close ",
                Style::new().fg(DIM),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let lines: Vec<Line> = app
        .output
        .iter()
        .map(|l| Line::from(Span::raw(l.clone())))
        .collect();
    app.output_view_height = inner.height;
    f.render_widget(Paragraph::new(lines).scroll((app.output_scroll, 0)), inner);
}

fn draw_help(f: &mut Frame, app: &mut App, area: Rect) {
    let rect = centered(area, 76, area.height.saturating_sub(4).min(34));
    f.render_widget(Clear, rect);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title_top(Line::from(Span::styled(
            " Keys ",
            Style::new().fg(ACCENT).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(" any key to close ", Style::new().fg(DIM))).right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let mut lines = Vec::new();
    for (key, desc) in HELP {
        if key.is_empty() {
            lines.push(Line::from(Span::styled(
                desc.to_string(),
                Style::new().fg(ACCENT).bold(),
            )));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<14}"), Style::new().fg(Color::Yellow)),
                Span::raw(desc.to_string()),
            ]));
        }
    }
    app.help_view_height = inner.height;
    f.render_widget(Paragraph::new(lines).scroll((app.help_scroll, 0)), inner);
}

pub const HELP: &[(&str, &str)] = &[
    ("", "Moving around"),
    ("Tab", "switch between the local and remote pane"),
    ("↑ ↓ / k j", "move the cursor"),
    ("PgUp PgDn", "move a page"),
    ("g / G", "jump to the top / bottom"),
    (
        "→ l / Enter",
        "enter a directory, or open a file in your editor",
    ),
    ("← h", "go to the parent directory"),
    ("f", "type a path to jump to"),
    ("t", "point the other pane at this directory"),
    ("~", "jump to the home directory"),
    ("/", "filter the listing as you type"),
    (".", "show or hide dotfiles"),
    ("R", "reload both panes"),
    ("", ""),
    ("", "Selecting and copying"),
    ("Space", "mark the file under the cursor"),
    ("a", "mark everything, or clear all marks"),
    ("c / F5", "copy marked files to the other pane's directory"),
    (
        "",
        "  (marks are optional — with none, the cursor row is used)",
    ),
    ("", ""),
    ("", "Shells inside the panes"),
    ("S", "open or close a shell under the focused pane"),
    (
        "F6 / Ctrl-]",
        "move the keyboard between the files and the shell",
    ),
    (
        "",
        "  While the shell has focus every key goes to it, including",
    ),
    ("", "  Ctrl-C and Esc. F6 is the way back out."),
    ("Ctrl-↑ / Ctrl-↓", "make the shell pane taller or shorter"),
    ("wheel", "scroll the shell's history"),
    (
        "",
        "  The local shell starts in the local pane's directory; the",
    ),
    (
        "",
        "  remote one opens its own SSH connection so a busy shell",
    ),
    ("", "  never holds up a transfer."),
    ("", ""),
    ("", "Editing and running things"),
    (
        "e / F4",
        "open in $EDITOR; remote files come back automatically",
    ),
    ("E", "open with a program you name"),
    ("v", "open in $PAGER"),
    (":", "run a command in the remote pane's directory"),
    ("!", "hand the whole terminal to an ssh shell (full screen)"),
    ("o", "show the last command's output again"),
    ("", ""),
    ("", "Archives"),
    ("z", "pack what is marked into a .tar.gz (you name it)"),
    ("x", "unpack the archive under the cursor into a directory"),
    ("X", "list what an archive holds, without unpacking it"),
    (
        "",
        "  Works on either side. The suffix you type picks the format:",
    ),
    ("", "  .tar, .tar.gz/.tgz, .tar.bz2, .tar.xz."),
    ("", ""),
    ("", "Docker containers"),
    ("D", "open a container in a new tab"),
    (
        "",
        "  On the local pane that means a container on this machine; on",
    ),
    (
        "",
        "  a server's pane, one running on that server. A container tab",
    ),
    (
        "",
        "  behaves like any other: browse, copy, edit, archive, shell.",
    ),
    (
        "s",
        "in a container, switches to uid 0 — no password needed",
    ),
    (
        "",
        "  Start with `sshman --docker` to go straight to the chooser.",
    ),
    ("", ""),
    ("", "Forwarded ports"),
    ("p", "ports carried from the server to this machine"),
    (
        "",
        "  a adds one, d stops it. 3000 forwards that port to the same",
    ),
    (
        "",
        "  one here; 8080:3000 changes the local port; 8080:db:5432",
    ),
    ("", "  reaches a host only the server can see."),
    (
        "",
        "  They bind 127.0.0.1 only, and are saved with the workspace.",
    ),
    ("", ""),
    ("", "Names and workspaces"),
    ("N", "name the server on screen (empty clears it)"),
    (
        "",
        "  Names replace the address in the tab and the recent list, and",
    ),
    (
        "",
        "  are remembered. On the connection screen, n names the",
    ),
    ("", "  highlighted saved server."),
    ("w", "workspaces: saved sets of connections"),
    (
        "",
        "  s saves everything open now under a name, Enter reopens a",
    ),
    (
        "",
        "  set, Del forgets one. Each member remembers its directory.",
    ),
    ("", "  Start with `sshman -w NAME`, or list them with"),
    (
        "",
        "  `sshman --list-workspaces`. Passwords are never saved, so a",
    ),
    (
        "",
        "  password-only server is flagged in the title bar for C.",
    ),
    ("", ""),
    ("", "Servers and tabs"),
    ("T", "connect to another server, in a new tab"),
    (
        "W",
        "close the tab on screen (ends that session and its shell)",
    ),
    ("Ctrl-← / Ctrl-→", "move between tabs"),
    ("Alt-1 … Alt-9", "jump straight to a tab"),
    (
        "",
        "  Each tab is its own connection: its own directory, shell,",
    ),
    (
        "",
        "  sudo state and transfers. Nothing on one waits for another.",
    ),
    (
        "",
        "  Servers you reach are remembered — ↑↓ picks one on the",
    ),
    (
        "",
        "  connection screen, Del forgets it. Passwords are never saved.",
    ),
    (
        "",
        "  If a connection drops it reconnects on its own, keeping the",
    ),
    ("", "  directory you were in."),
    ("", ""),
    ("", "Root access"),
    ("s", "toggle sudo mode for the remote pane"),
    (
        "",
        "  Sudo mode lists, copies, edits and deletes as root, so",
    ),
    (
        "",
        "  root-only paths become visible. SFTP alone cannot do this.",
    ),
    ("", ""),
    ("", "Housekeeping"),
    ("n / F7", "create a directory"),
    ("r / F2", "rename"),
    ("d / F8", "delete (asks first)"),
    ("C", "connection screen: recent servers and the form"),
    (
        "",
        "  Servers you connect to are remembered. On that screen ↑↓",
    ),
    (
        "",
        "  picks one, Enter connects, Del forgets. Passwords are",
    ),
    ("", "  never saved."),
    ("q / Ctrl-C", "quit"),
];

// ---- geometry --------------------------------------------------------------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn centered_line(area: Rect) -> Rect {
    Rect {
        y: area.y + area.height / 2,
        height: 1,
        ..area
    }
}
