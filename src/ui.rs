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

use crate::app::{
    App, Arrangement, ConnectFocus, ConnectForm, Level, LinkState, MenuItem, Mode, Pane, Side,
};
use crate::config::Setting;
use crate::keys::Action;
use crate::layout::{Areas, Slot};
use crate::shell::Shell;
use crate::theme::Theme;
use crate::types::{EntryKind, FileEntry, ellipsize, fmt_time, human_size};

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // Behind everything, before anything: widgets that name no background of
    // their own leave this one showing, and the ones that do — the chips —
    // paint over it. A theme naming none leaves `Reset` here, which is the
    // terminal's own and so no change at all.
    f.buffer_mut()
        .set_style(area, Style::new().bg(app.background()));
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
    app.tab_close_buttons.clear();
    app.tab_bar_row = None;
    if tabs_h == 1 {
        draw_tab_bar(f, app, rows[1]);
    }

    // A pane whose terminal has gone is not drawn where it used to be, and
    // the keyboard is never left pointing at one.
    app.settle_focus();

    // Recorded for the mouse, and worked out once here so a click is matched
    // against exactly what was drawn.
    app.panes_area = rows[2];
    app.areas = match app.zoomed {
        // Zoomed, the focused pane is the arrangement: there are no borders
        // to drag and nothing else to click on.
        true => Areas {
            panes: vec![(app.focus, rows[2])],
            dividers: Vec::new(),
        },
        false => app.layout.areas(rows[2]),
    };
    app.term_inner.clear();
    app.zoom_buttons.clear();
    app.close_buttons.clear();
    app.pane_titles.clear();
    app.crumbs.clear();

    for (slot, rect) in app.areas.panes.clone() {
        draw_slot(f, app, slot, rect);
    }

    if gauge_h == 1 {
        draw_progress(f, app, rows[3]);
    }
    draw_status(f, app, rows[4]);
    draw_hints(f, app, rows[5]);

    // Over the panes, under anything modal: a label about a tab is the
    // smallest thing on the screen and the least entitled to be in the way of
    // a box somebody opened.
    draw_tab_tip(f, app, rows[1]);
    // And over that: a menu is the thing being read.
    draw_menu(f, app, area);

    match app.mode {
        Mode::Connect => draw_connect(f, app, area),
        Mode::Picker => draw_picker(f, app, area),
        Mode::Workspaces => draw_workspaces(f, app, area),
        Mode::Settings => draw_settings(f, app, area),
        Mode::Arrange => draw_arrange(f, app, area),
        Mode::Themes => draw_themes(f, app, area),
        Mode::Keys => draw_keys(f, app, area),
        Mode::Forwards => draw_forwards(f, app, area),
        Mode::Prompt => draw_prompt(f, app, area),
        Mode::Confirm => draw_confirm(f, app, area),
        Mode::Output => draw_output(f, app, area),
        Mode::Help => draw_help(f, app, area),
        Mode::Browse => {}
    }
}

/// `[+]`, in cells.
const NEW_TAB_W: u16 = 3;

/// Where the new-tab button goes: the end of the top bar, which is there
/// whatever else is — unlike the row of tabs, which only appears once there
/// is more than one of them, and so is missing exactly when you want another.
fn new_tab_button_area(area: Rect) -> Rect {
    Rect {
        x: area.right().saturating_sub(NEW_TAB_W + 1),
        y: area.y,
        width: NEW_TAB_W,
        height: 1,
    }
}

fn draw_title_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let mut spans = vec![
        Span::styled(
            " sshman ",
            Style::new().fg(theme.on_accent).bg(theme.accent).bold(),
        ),
        Span::raw(" "),
    ];
    match app.tab() {
        Some(tab) => {
            let c = &tab.conn;
            let (colour, suffix) = match tab.link {
                LinkState::Live => (theme.good, ""),
                LinkState::Reconnecting => (theme.warn, "  ⟳ reconnecting…"),
                LinkState::Lost => (theme.bad, "  ✗ disconnected"),
            };
            let _ = c;
            spans.push(Span::styled(tab.title(), Style::new().fg(colour).bold()));
            if tab.is_container() {
                spans.push(Span::styled(
                    "  container",
                    Style::new().fg(theme.info).bold(),
                ));
            }
            if tab.is_local() {
                spans.push(Span::styled(
                    "  this machine",
                    Style::new().fg(theme.info).bold(),
                ));
            }
            if !suffix.is_empty() {
                spans.push(Span::styled(suffix, Style::new().fg(colour).bold()));
            }
            if app.tabs.len() > 1 {
                spans.push(Span::styled(
                    format!("  tab {}/{}", app.active + 1, app.tabs.len()),
                    Style::new().fg(theme.dim),
                ));
            }
        }
        None => spans.push(Span::styled(
            "not connected",
            Style::new().fg(theme.bad).bold(),
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
            Style::new().fg(theme.on_accent).bg(theme.bad).bold(),
        ));
    }
    if app.zoomed {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            " ZOOM ",
            Style::new().fg(theme.on_accent).bg(theme.accent).bold(),
        ));
    }
    // What is on the clipboard has to survive the walk to wherever it is going,
    // and the status line will have moved on by then.
    if let Some(clip) = &app.clip {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(
                " {} {} {} ",
                if clip.cut { "✂" } else { "⧉" },
                clip.names.len(),
                if clip.cut { "cut" } else { "copied" }
            ),
            Style::new().fg(theme.on_accent).bg(theme.info).bold(),
        ));
    }
    // A workspace can leave connections waiting on a password. That must not
    // scroll away with the next status message, so it lives here.
    let forwards = app.forward_count();
    if forwards > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" ⇄ {forwards} "),
            Style::new().fg(theme.on_accent).bg(theme.info).bold(),
        ));
    }
    if !app.needs_password.is_empty() {
        let waiting: Vec<&str> = app
            .needs_password
            .iter()
            .map(|waiting| waiting.label.as_str())
            .collect();
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" needs a password: {} — press C ", waiting.join(", ")),
            Style::new().fg(theme.on_accent).bg(theme.warn).bold(),
        ));
    }
    if let Some(task) = app.current_task() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("● {task}"),
            Style::new().fg(theme.warn),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);

    // Drawn last so a long title cannot bury it, and only where it works:
    // the keys it stands in for are the browsing keys.
    app.new_tab_button = None;
    if app.mode == Mode::Browse && area.width > NEW_TAB_W + 8 {
        let rect = new_tab_button_area(area);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "[+]",
                Style::new().fg(theme.accent).bold(),
            ))),
            rect,
        );
        app.new_tab_button = Some(rect);
    }
}

/// `‹12 `: a chevron, a count of tabs that way, and a space.
const TAB_MARK: u16 = 5;

/// `✕ `: the button that closes a tab, and the space after it.
const TAB_CLOSE: &str = "✕ ";

/// How much of a tab's name survives when there are several. Enough to tell
/// two servers apart, and never so wide that one tab crowds the rest out.
fn tab_title_budget(width: u16, tabs: usize) -> usize {
    (width as usize / tabs.max(1))
        .saturating_sub(8 + TAB_CLOSE.chars().count())
        .clamp(6, 24)
}

/// The run of tabs that fits, always including the one on screen.
///
/// Widening from the active tab outwards rather than scrolling from the left
/// keeps the tab you are looking at on screen without the row jumping about:
/// stepping one along moves the window by one, and only when it has to.
fn tab_window(widths: &[u16], active: usize, room: u16) -> std::ops::Range<usize> {
    if widths.iter().sum::<u16>() <= room {
        return 0..widths.len();
    }
    // Room for a marker at each end, saying how many did not fit.
    let room = room.saturating_sub(TAB_MARK * 2);
    let (mut first, mut last) = (active, active);
    let mut used = widths.get(active).copied().unwrap_or(0);
    loop {
        let mut grew = false;
        if last + 1 < widths.len() && used + widths[last + 1] <= room {
            used += widths[last + 1];
            last += 1;
            grew = true;
        }
        if first > 0 && used + widths[first - 1] <= room {
            used += widths[first - 1];
            first -= 1;
            grew = true;
        }
        if !grew {
            break;
        }
    }
    first..last + 1
}

/// The row of open servers. Each chip carries its number, so `Alt-<n>` is
/// discoverable without reading the help.
fn draw_tab_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    app.tab_bar_row = Some(area.y);

    let budget = tab_title_budget(area.width, app.tabs.len());
    // The name, and then the button that closes it. Two spans rather than
    // one string: the `✕` is a thing you can hit, and it is drawn as one.
    let chips: Vec<String> = app
        .tabs
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            let marker = match tab.link {
                LinkState::Live => "",
                LinkState::Reconnecting => " ⟳",
                LinkState::Lost => " ✗",
            };
            let sudo = if tab.sudo { " #" } else { "" };
            format!(
                " {} {}{sudo}{marker} ",
                index + 1,
                ellipsize(&tab.title(), budget)
            )
        })
        .collect();
    let close_w = TAB_CLOSE.chars().count() as u16;
    // Each chip carries its close button and the space that follows it, so
    // the widths add up to what the row actually takes.
    let widths: Vec<u16> = chips
        .iter()
        .map(|c| c.chars().count() as u16 + close_w + 1)
        .collect();
    let shown = tab_window(&widths, app.active, area.width);

    let mut spans = Vec::new();
    let mut x = area.x;

    // What did not fit, on either side. Clicking one steps that way, which is
    // the only thing the mark could sensibly mean.
    if shown.start > 0 {
        let text = format!("‹{} ", shown.start);
        let width = text.chars().count() as u16;
        spans.push(Span::styled(text, Style::new().fg(theme.dim)));
        app.tab_spans.push((x, x + width, shown.start - 1));
        x += width;
    }

    for index in shown.clone() {
        let active = index == app.active;
        let style = if active {
            Style::new().fg(theme.on_accent).bg(theme.accent).bold()
        } else if app.tabs[index].link != LinkState::Live {
            Style::new().fg(theme.bad)
        } else {
            Style::new().fg(theme.muted)
        };
        // The button keeps the chip's background so it reads as part of it,
        // and a colour of its own so it reads as a button.
        let close_style = match active {
            true => Style::new().fg(theme.on_accent).bg(theme.accent),
            false => Style::new().fg(theme.dim),
        };
        let dragging = app.tab_drag == Some(index);
        let label = chips[index].chars().count() as u16;
        spans.push(Span::styled(
            chips[index].clone(),
            match dragging {
                true => style.add_modifier(Modifier::REVERSED),
                false => style,
            },
        ));
        spans.push(Span::styled(TAB_CLOSE, close_style));
        spans.push(Span::raw(" "));
        // The whole chip goes here — the close button is tested first, so
        // the overlap costs nothing and a click anywhere else still switches.
        app.tab_spans.push((x, x + label + close_w, index));
        app.tab_close_buttons
            .push((x + label, x + label + close_w, index));
        x += label + close_w + 1;
    }

    if shown.end < app.tabs.len() {
        let hidden = app.tabs.len() - shown.end;
        let text = format!("{hidden}› ");
        let width = text.chars().count() as u16;
        spans.push(Span::styled(text, Style::new().fg(theme.dim)));
        app.tab_spans.push((x, x + width, shown.end));
        x += width;
    }

    // The reminder of what the keys are, but only where there is room for it:
    // with the row full of tabs, the tabs are the useful half.
    const HINT: &str = "  + new tab · ✕ or W close · Ctrl-←/→ switch · Ctrl-⇧-←/→ move";
    if x + HINT.chars().count() as u16 <= area.right() {
        spans.push(Span::styled(HINT, Style::new().fg(theme.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The full name of the tab the pointer has been resting on.
///
/// Drawn after the panes rather than with the row it belongs to: it hangs
/// below the row, over whatever is under it, and a box drawn before the thing
/// it covers is a box nobody sees.
fn draw_tab_tip(f: &mut Frame, app: &App, area: Rect) {
    if app.tab_bar_row.is_none() {
        return;
    }
    let budget = tab_title_budget(area.width, app.tabs.len());
    let Some((index, title)) = app.tab_tip(budget) else {
        return;
    };
    // Against the chip it is about, so it is obvious which tab is being
    // named when there are eight of them.
    let Some((start, ..)) = app
        .tab_spans
        .iter()
        .find(|(_, _, at)| *at == index)
        .copied()
    else {
        return;
    };

    let theme = app.theme;
    let text = ellipsize(&title, area.width.saturating_sub(4) as usize);
    let width = text.chars().count() as u16 + 4;
    // Kept on the screen: a tab near the right-hand edge would otherwise hang
    // its name off the side, which is the half you were asking about.
    let x = start.min(area.right().saturating_sub(width));
    let rect = Rect::new(x, area.y + 1, width, 3);
    if rect.bottom() > area.bottom() {
        return;
    }
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.dim));
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(text, Style::new().fg(theme.text)))),
        inner,
    );
}

/// The menu a right click opened: what can be done to what was pointed at.
///
/// Drawn where the click was, and moved only as far as it has to be to stay
/// on the screen — a menu that jumped to the middle would be a menu you had
/// to go and find, and the whole point of it is that it is under your hand.
fn draw_menu(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(menu) = &app.menu else { return };
    let theme = app.theme;
    // Wide enough for the longest row and the key beside it. Worked out from
    // the rows rather than fixed, because the rows differ: a menu over an
    // archive has two more in it than one over a text file.
    let widest = menu
        .items
        .iter()
        .map(|item| match item {
            MenuItem::Do(label, action) => {
                label.chars().count() + 3 + app.keymap.shown(*action).chars().count()
            }
            MenuItem::Rule => 0,
        })
        .max()
        .unwrap_or(10);
    let width = (widest as u16 + 4).min(area.width);
    let height = (menu.items.len() as u16 + 2).min(area.height);
    // Below and to the right of the pointer where there is room, and back
    // inside the screen where there is not.
    let (x, y) = menu.anchor;
    let x = x.min(area.right().saturating_sub(width)).max(area.x);
    let y = match y + 1 + height <= area.bottom() {
        true => y + 1,
        // No room below, so above — and if there is no room there either,
        // as high as it can go.
        false => y.saturating_sub(height).max(area.y),
    };
    let rect = Rect::new(x, y, width, height);

    clear_under(f, rect, app.background());
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent));
    let inner = block.inner(rect);
    f.render_widget(block, rect);

    // How far down it starts, for a menu taller than the terminal it is in:
    // far enough that the lit row is on the screen, and no further.
    let shown = inner.height as usize;
    let mut scroll = menu.scroll.min(menu.items.len().saturating_sub(shown));
    if menu.cursor < scroll {
        scroll = menu.cursor;
    } else if menu.cursor >= scroll + shown {
        scroll = menu.cursor + 1 - shown;
    }

    let room = inner.width as usize;
    let lines: Vec<Line> = menu
        .items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(shown)
        .map(|(at, item)| match item {
            MenuItem::Rule => {
                Line::from(Span::styled("─".repeat(room), Style::new().fg(theme.dim)))
            }
            MenuItem::Do(label, action) => {
                let lit = at == menu.cursor;
                let key = app.keymap.shown(*action);
                // The key on the right, and the gap between stretched to fill
                // the box, so the whole lit row is one bar rather than a word
                // with a highlight round it.
                let gap = room
                    .saturating_sub(label.chars().count() + key.chars().count() + 2)
                    .max(1);
                let style = match lit {
                    true => Style::new().fg(theme.on_accent).bg(theme.accent),
                    false => Style::new().fg(theme.text),
                };
                let key_style = match lit {
                    true => style,
                    false => Style::new().fg(theme.dim),
                };
                Line::from(vec![
                    Span::styled(format!(" {label}"), style),
                    Span::styled(" ".repeat(gap), style),
                    Span::styled(format!("{key} "), key_style),
                ])
            }
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);

    // The separators joined to the frame, so a rule reads as a line across
    // the menu rather than as a row of dashes inside it. Done to the buffer
    // afterwards because the border belongs to the block and the rules
    // belong to the text, and this is the one place they meet.
    let buf = f.buffer_mut();
    for (at, _) in menu
        .items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(shown)
        .filter(|(_, item)| matches!(item, MenuItem::Rule))
    {
        let y = inner.y + (at - scroll) as u16;
        for (x, edge) in [(rect.x, "├"), (rect.right() - 1, "┤")] {
            if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.set_symbol(edge).set_fg(theme.dim);
            }
        }
    }

    // Where it landed, for the mouse. Written back every frame, the way the
    // tabs and the crumbs are.
    if let Some(menu) = &mut app.menu {
        menu.area = rect;
        menu.scroll = scroll;
    }
}

/// One pane, wherever the arrangement put it.
fn draw_slot(f: &mut Frame, app: &mut App, slot: Slot, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let buttons = place_buttons(app, slot, area);
    let focused = app.focus == slot;
    match slot {
        Slot::Files { .. } => draw_files(f, app, slot, area, focused, buttons),
        Slot::Term { .. } => draw_shell(f, app, slot, area, focused, buttons),
    }
    // What the pane is in the middle of, said on the pane itself: which one
    // the arrows are pointing at while sshman holds the keyboard, which one
    // has been picked up, and where a dragged one would land.
    let theme = app.theme;
    let chip = if app.carrying && focused {
        Some((" ✥ moving this pane ", theme.warn))
    } else if app.moving == Some(slot) {
        Some((" ✥ moving ", theme.warn))
    } else if app.moving.is_some() && app.move_over == Some(slot) {
        Some((" change places ", theme.good))
    } else if app.commanding && focused {
        Some((" ↵ use this pane ", theme.accent))
    } else {
        None
    };
    if let Some((text, colour)) = chip
        && area.width >= MIN_WIDTH_FOR_BUTTON
    {
        let line = Line::from(Span::styled(
            text,
            Style::new().fg(theme.on_accent).bg(colour).bold(),
        ));
        let at = Rect {
            x: area.x + 2,
            y: area.bottom().saturating_sub(1),
            width: (area.width - 3).min(text.chars().count() as u16),
            height: 1,
        };
        f.render_widget(line, at);
    }
}

/// What a file list is called: whose it is, and — with more than one on that
/// machine — which of them, counted in the order they are drawn rather than by
/// the number the pane happens to hold.
fn files_label(app: &App, slot: Slot) -> String {
    let base = match slot.host() {
        Side::Local => "LOCAL",
        // A tab pointed at this machine has no remote side to speak of, and
        // calling it one beside the pane it is a copy of would be a lie.
        Side::Remote if app.tab().is_some_and(|t| t.is_local()) => "THIS MACHINE",
        Side::Remote => "REMOTE",
    };
    match app.tree_number(slot) {
        Some(n) => format!("{base} {n}"),
        None => base.to_string(),
    }
}

/// A file list, and everything the border around it has to say.
fn draw_files(
    f: &mut Frame,
    app: &mut App,
    slot: Slot,
    area: Rect,
    focused: bool,
    buttons: Buttons,
) {
    let side = slot.host();
    let label = files_label(app, slot);
    place_title(app, slot, area, &label);
    let target = app.target() == Some(slot);
    let path = app.path_of(slot);
    let sudo = side == Side::Remote && app.sudo();
    let live = side == Side::Local || app.connected();
    let theme = app.theme;
    let hovered = Hovered {
        row: app.hovered_row(slot),
        crumb: app.hovered_crumb(slot).map(str::to_string),
    };
    let trail = place_crumbs(app, slot, area, &label, &path, buttons);
    draw_pane(
        f,
        area,
        &label,
        &trail,
        app.pane_mut(slot),
        focused,
        sudo,
        live,
        target,
        hovered,
        buttons,
        theme,
    );
}

/// The embedded terminal. The vt100 screen borrows its parser, so the widget
/// has to be built and rendered inside the lock.
fn draw_shell(
    f: &mut Frame,
    app: &mut App,
    slot: Slot,
    area: Rect,
    focused: bool,
    buttons: Buttons,
) {
    let theme = app.theme;
    let alive = app.shell(slot).map(|s| s.is_alive()).unwrap_or(false);
    let label = app.shell(slot).map(|s| s.label.clone()).unwrap_or_default();
    let scrolled = app.shell(slot).map(|s| s.scrollback()).unwrap_or(0);
    let editor = app.term(slot).is_some_and(|t| t.is_editor());
    place_title(app, slot, area, if editor { "EDITOR" } else { "SHELL" });

    let colour = if !alive {
        theme.dim
    } else if slot.host() == Side::Remote && app.sudo() {
        theme.bad
    } else if focused {
        theme.accent
    } else {
        theme.muted
    };

    let mut title = vec![
        // An editor pane says so: it is the one a click in a file list opens
        // things in, and looks like any other terminal otherwise.
        Span::styled(
            if editor { " EDITOR " } else { " SHELL " },
            Style::new().fg(colour).bold(),
        ),
        Span::styled(label, Style::new().fg(theme.text)),
    ];
    if !alive {
        title.push(Span::styled(" [exited]", Style::new().fg(theme.dim)));
    }
    title.push(Span::raw(" "));

    // All the bottom edge has to say: how far back the view is scrolled,
    // when it is scrolled at all.
    let mut hint = Vec::new();
    if scrolled > 0 {
        hint.push(Span::styled(
            format!(" ↑{scrolled} lines back "),
            Style::new().fg(theme.warn),
        ));
    }

    let block = Block::bordered()
        .border_type(if focused {
            BorderType::Thick
        } else {
            BorderType::Plain
        })
        .border_style(if focused {
            Style::new().fg(colour).bold()
        } else {
            Style::new().fg(theme.dim)
        })
        .title_top(Line::from(title))
        .title_bottom(Line::from(hint).right_aligned());
    let block = block.title_top(pane_buttons(theme, focused, buttons));

    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    // What the program inside is drawing on, and so what a mouse event has to
    // be measured against.
    app.term_inner.push((slot, inner));

    let selection = app.shell(slot).and_then(Shell::selection);
    let background = app.background();
    let palette = app.shell_palette();
    let text = theme.text;
    let Some(shell) = app.shell_mut(slot) else {
        return;
    };
    // Keep the emulator and the far end matched to the space we actually drew.
    shell.ensure_size(inner.height, inner.width);
    let cursor = focused.then(|| shell.cursor()).flatten();
    shell.with_screen(|screen| {
        f.render_widget(PseudoTerminal::new(screen), inner);
    });

    // A terminal's colours are whatever its emulator is set to, and for these
    // panes sshman *is* the emulator. So the cells the program inside left to
    // the default take the theme's, and the sixteen it asked for by number are
    // looked up in the theme rather than in the terminal behind us — while an
    // exact colour it named is the colour it gets.
    if background != Color::Reset || palette.is_some() {
        let buffer = f.buffer_mut();
        for y in inner.y..inner.bottom() {
            for x in inner.x..inner.right() {
                let cell = &mut buffer[(x, y)];
                if cell.bg == Color::Reset && background != Color::Reset {
                    cell.bg = background;
                }
                if let Some(palette) = palette {
                    cell.fg = themed(cell.fg, palette, text);
                    cell.bg = themed(cell.bg, palette, background);
                }
            }
        }
    }

    // Text picked out with the mouse, marked by turning the cells inside out
    // rather than by painting a colour over them — sshman has no background
    // of its own to use, and reversing whatever the program drew reads as a
    // selection in any theme, on any terminal.
    if let Some(selection) = selection {
        let buffer = f.buffer_mut();
        for row in 0..inner.height {
            let Some((from, to)) = selection.columns(row, inner.width) else {
                continue;
            };
            for col in from..to {
                let (x, y) = (inner.x + col, inner.y + row);
                if x < inner.right() && y < inner.bottom() {
                    buffer[(x, y)].modifier ^= Modifier::REVERSED;
                }
            }
        }
    }

    if let Some((row, col)) = cursor {
        let (x, y) = (inner.x + col, inner.y + row);
        if x < inner.right() && y < inner.bottom() {
            f.set_cursor_position((x, y));
        }
    }
}

/// What the buttons in a pane's corner should offer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Buttons {
    /// The pane already has the whole screen.
    pub zoomed: bool,
    /// Zooming would change what is on screen.
    pub zoom: bool,
    /// There is another pane to give this one's space back to.
    pub close: bool,
}

/// The buttons in the top-right corner of every pane: a way to do with the
/// mouse what the keyboard does with `m` and `F9`.
///
/// The zoom is the same button either way round — it maximises a pane that is
/// sharing the screen, and restores one that has taken it all — so its glyph
/// says which of the two it would do next.
fn pane_buttons(theme: Theme, focused: bool, buttons: Buttons) -> Line<'static> {
    let style = if focused {
        Style::new().fg(theme.accent).bold()
    } else {
        Style::new().fg(theme.dim)
    };
    let mut spans = Vec::new();
    if buttons.close {
        spans.push(Span::styled(
            "[✕]",
            if focused {
                Style::new().fg(theme.bad).bold()
            } else {
                style
            },
        ));
    }
    if buttons.zoom {
        spans.push(Span::styled(
            if buttons.zoomed { "[⤡]" } else { "[⤢]" },
            style,
        ));
    }
    Line::from(spans).right_aligned()
}

/// Where a button was drawn, counting back from the corner: 0 is the
/// rightmost. A right-aligned title ends one cell inside the corner, and each
/// button is [`BUTTON_W`] wide, which is what the mouse has to hit.
fn button_area(area: Rect, from_right: u16) -> Rect {
    Rect {
        x: area.right().saturating_sub(BUTTON_W * (from_right + 1) + 1),
        y: area.y,
        width: BUTTON_W,
        height: 1,
    }
}

/// Which buttons a pane gets, and where each of them lands.
///
/// The renderer records them as it goes, so a click is matched against what
/// was actually drawn rather than against a second guess at the layout.
/// Where a pane's name is written, which is what a drag takes hold of to move
/// the pane. Recorded by the renderer, so a click is matched against what was
/// actually drawn.
fn place_title(app: &mut App, slot: Slot, area: Rect, label: &str) {
    // Zoomed there is nowhere to drop it: the drag would be a click that did
    // nothing and looked like it should have.
    if app.areas.panes.len() < 2 {
        return;
    }
    // ` LABEL `, starting one cell in from the corner.
    let width = label.chars().count() as u16 + 2;
    if width + 4 > area.width {
        return;
    }
    app.pane_titles.push((
        Rect {
            x: area.x + 1,
            y: area.y,
            width,
            height: 1,
        },
        slot,
    ));
}

/// One piece of the path drawn in a pane's title.
struct Crumb {
    /// What to draw, its separator included.
    text: String,
    /// The directory it names, or `None` for the `…` that stands in for the
    /// pieces there was no room for — which names nothing and leads nowhere.
    path: Option<String>,
}

/// What the pointer is resting on in one file list.
#[derive(Default)]
struct Hovered {
    row: Option<usize>,
    crumb: Option<String>,
}

/// How much of the top border is left for the path once the label, the
/// corners and any buttons have taken theirs.
fn path_room(area: Rect, label: &str, buttons: Buttons) -> usize {
    let shown = u16::from(buttons.zoom) + u16::from(buttons.close);
    let button_room = if shown > 0 { BUTTON_W * shown + 1 } else { 0 };
    area.width
        .saturating_sub(label.chars().count() as u16 + 6 + button_room) as usize
}

/// Lay the path out along the top border as a trail of clickable pieces, and
/// write down where each one landed so a click can be turned back into the
/// directory it names.
///
/// A path too long for the pane loses whole pieces from the front rather than
/// characters from the middle: half a directory name is not something you can
/// click on, and the end of a path is the part worth reading anyway.
fn place_crumbs(
    app: &mut App,
    slot: Slot,
    area: Rect,
    label: &str,
    path: &str,
    buttons: Buttons,
) -> Vec<Crumb> {
    let room = path_room(area, label, buttons);
    let plain = |text: String| vec![Crumb { text, path: None }];
    // Anything that is not a path — "—" for a pane with nowhere to be — is
    // drawn as it always was and leads nowhere.
    if !path.starts_with('/') {
        return plain(ellipsize(path, room.max(8)));
    }
    let pieces = crate::types::crumbs(path);
    let Some((_, whole)) = pieces.last().cloned() else {
        return plain(ellipsize(path, room.max(8)));
    };

    // Each piece carries the separator in front of it, except the first two:
    // the root is a separator, so the piece after it needs none of its own.
    let text_of = |i: usize, name: &str| match i {
        0 | 1 => name.to_string(),
        _ => format!("/{name}"),
    };
    let width_of = |text: &str| text.chars().count();

    // Drop whole pieces off the front until what is left fits, counting the
    // `…` that says some were dropped.
    let mut start = 0;
    let fits = loop {
        let elided = usize::from(start > 0);
        let width: usize = pieces[start..]
            .iter()
            .enumerate()
            .map(|(i, (name, _))| width_of(&text_of(start + i, name)))
            .sum();
        if width + elided <= room {
            break true;
        }
        if start + 1 >= pieces.len() {
            break false;
        }
        start += 1;
    };
    // Not even the last piece fits: there is no trail to draw, so fall back
    // to the shortened path this always drew.
    if !fits {
        return plain(ellipsize(&whole, room.max(8)));
    }

    let mut trail = Vec::with_capacity(pieces.len() - start + 1);
    if start > 0 {
        trail.push(Crumb {
            text: "…".to_string(),
            path: None,
        });
    }
    for (i, (name, leads_to)) in pieces.iter().enumerate().skip(start) {
        let mut text = text_of(i, name);
        // The first piece kept after an elision needs the separator the
        // dropped ones were carrying, or it would run into the `…`.
        if i == start && start > 0 && !text.starts_with('/') {
            text.insert(0, '/');
        }
        trail.push(Crumb {
            text,
            path: Some(leads_to.clone()),
        });
    }

    // Where each of them ended up. The title starts one cell in from the
    // corner, after the label, which is how ratatui draws it.
    let mut x = area.x + 1 + label.chars().count() as u16 + 2;
    for crumb in &trail {
        let width = width_of(&crumb.text) as u16;
        if let Some(leads_to) = &crumb.path {
            app.crumbs.push((
                Rect {
                    x,
                    y: area.y,
                    width,
                    height: 1,
                },
                slot,
                leads_to.clone(),
            ));
        }
        x += width;
    }
    trail
}

fn place_buttons(app: &mut App, slot: Slot, area: Rect) -> Buttons {
    let buttons = Buttons {
        zoomed: app.zoomed,
        zoom: app.zoom_has_anything_to_hide() && area.width >= MIN_WIDTH_FOR_BUTTON,
        // Zoomed there is nothing on screen to give the space back to, and
        // closing the one pane you can see would be a trapdoor.
        close: !app.zoomed && app.layout.panes() > 1 && area.width >= MIN_WIDTH_FOR_TWO_BUTTONS,
    };
    // They are placed from the corner inwards, so a close button with no zoom
    // beside it takes the corner itself.
    let mut from_right = 0;
    if buttons.zoom {
        app.zoom_buttons.push((button_area(area, from_right), slot));
        from_right += 1;
    }
    if buttons.close {
        app.close_buttons
            .push((button_area(area, from_right), slot));
    }
    buttons
}

/// A colour a program in a shell pane asked for, in the theme's terms.
///
/// Only the sixteen it can ask for *by number* are the emulator's to answer —
/// those are names for roles, and the theme is what those names mean here. The
/// rest of the 256 and any exact colour are what the program meant literally,
/// and are left alone.
fn themed(colour: Color, palette: [Color; 16], default: Color) -> Color {
    match colour {
        Color::Reset if default != Color::Reset => default,
        Color::Indexed(i) if (i as usize) < palette.len() => palette[i as usize],
        Color::Black => palette[0],
        Color::Red => palette[1],
        Color::Green => palette[2],
        Color::Yellow => palette[3],
        Color::Blue => palette[4],
        Color::Magenta => palette[5],
        Color::Cyan => palette[6],
        Color::Gray => palette[7],
        Color::DarkGray => palette[8],
        Color::LightRed => palette[9],
        Color::LightGreen => palette[10],
        Color::LightYellow => palette[11],
        Color::LightBlue => palette[12],
        Color::LightMagenta => palette[13],
        Color::LightCyan => palette[14],
        Color::White => palette[15],
        other => other,
    }
}

/// Make room for an overlay.
///
/// `Clear` puts the cells back to the terminal's own colours, which is exactly
/// wrong when the theme has painted a background: the box would be a hole in
/// it. So the background goes back down behind the box.
fn clear_under(f: &mut Frame, rect: Rect, bg: Color) {
    f.render_widget(Clear, rect);
    f.buffer_mut().set_style(rect, Style::new().bg(bg));
}

/// `[⤢]`, in cells.
const BUTTON_W: u16 = 3;

/// Panes too narrow for a button and a title both keep the title.
const MIN_WIDTH_FOR_BUTTON: u16 = 24;

/// Narrower than this and only the zoom fits; the close button is the one to
/// go, since `F9` is not hidden behind a pane being wide enough.
const MIN_WIDTH_FOR_TWO_BUTTONS: u16 = 34;

#[allow(clippy::too_many_arguments)]
fn draw_pane(
    f: &mut Frame,
    area: Rect,
    label: &str,
    trail: &[Crumb],
    pane: &mut Pane,
    focused: bool,
    sudo: bool,
    live: bool,
    // This is the list `c` copies into.
    target: bool,
    // What the pointer is over, drawn lit so a click can be aimed.
    hovered: Hovered,
    buttons: Buttons,
    theme: Theme,
) {
    let border_style = if focused {
        Style::new().fg(theme.accent).bold()
    } else {
        Style::new().fg(theme.dim)
    };
    let label_style = if sudo {
        Style::new().fg(theme.bad).bold()
    } else if focused {
        Style::new().fg(theme.accent).bold()
    } else {
        Style::new().fg(theme.muted)
    };

    let mut title = vec![Span::styled(format!(" {label} "), label_style)];
    title.extend(trail.iter().map(|crumb| {
        let style = match crumb.path.is_some() {
            true => Style::new().fg(theme.text),
            // The `…` standing in for the pieces that did not fit is not one
            // of them, and must not look like something to click.
            false => Style::new().fg(theme.dim),
        };
        let lit = crumb.path.as_deref() == hovered.crumb.as_deref() && crumb.path.is_some();
        Span::styled(
            crumb.text.clone(),
            match lit {
                true => style.add_modifier(Modifier::UNDERLINED),
                false => style,
            },
        )
    }));
    title.push(Span::raw(" "));
    let title = Line::from(title);

    let mut bottom = Vec::new();
    if !pane.filter.is_empty() {
        bottom.push(Span::styled(
            format!(" /{} ", pane.filter),
            Style::new().fg(theme.warn),
        ));
    }
    if !pane.marked.is_empty() {
        bottom.push(Span::styled(
            format!(" {} marked ", pane.marked.len()),
            Style::new().fg(theme.warn).bold(),
        ));
    }
    bottom.push(Span::styled(
        format!(" {} items ", pane.view.len()),
        Style::new().fg(theme.dim),
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
    // Which list `c` copies into, said on the list itself: with more than two
    // on screen, "the other pane" is not something you should have to work out.
    let block = match target && area.width >= MIN_WIDTH_FOR_BUTTON {
        true => block.title_bottom(Line::from(Span::styled(
            " c copies here ",
            Style::new().fg(theme.on_accent).bg(theme.accent),
        ))),
        false => block,
    };
    let block = block.title_top(pane_buttons(theme, focused, buttons));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if let Some(err) = &pane.error {
        let p = Paragraph::new(vec![
            Line::from(Span::styled(
                "cannot read this directory",
                Style::new().fg(theme.bad).bold(),
            )),
            Line::raw(""),
            Line::from(Span::styled(err.clone(), Style::new().fg(theme.bad))),
            Line::raw(""),
            Line::from(Span::styled(
                "press s to try again as root",
                Style::new().fg(theme.dim),
            )),
        ])
        .wrap(Wrap { trim: true });
        f.render_widget(p, inner.inner(Margin::new(1, 1)));
        return;
    }

    if !live {
        let p = Paragraph::new("no connection")
            .style(Style::new().fg(theme.dim))
            .alignment(Alignment::Center);
        f.render_widget(p, centered_line(inner));
        return;
    }

    if pane.loading {
        let p = Paragraph::new("loading…")
            .style(Style::new().fg(theme.warn))
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
            .style(Style::new().fg(theme.dim))
            .alignment(Alignment::Center);
        f.render_widget(p, centered_line(inner));
        return;
    }

    let cols = Columns::for_width(inner.width);
    let items: Vec<ListItem> = pane
        .view
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let item = ListItem::new(entry_line(e, pane.marked.contains(&e.name), cols, theme));
            // Under the pointer: underlined rather than filled in, so it
            // cannot be mistaken for the cursor — which is the row the
            // keyboard is on, and stays where it is while the mouse moves.
            match Some(i) == hovered.row {
                true => item.style(Style::new().add_modifier(Modifier::UNDERLINED)),
                false => item,
            }
        })
        .collect();

    let list = List::new(items).highlight_style(
        Style::new()
            .bg(if focused { theme.accent } else { theme.dim })
            .fg(theme.on_accent)
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

fn entry_line<'a>(e: &FileEntry, marked: bool, cols: Columns, theme: Theme) -> Line<'a> {
    let mut spans = Vec::new();
    spans.push(if marked {
        Span::styled("*", Style::new().fg(theme.warn).bold())
    } else {
        Span::raw(" ")
    });

    if cols.perms {
        spans.push(Span::styled(
            format!("{:<10} ", e.perms),
            Style::new().fg(theme.dim),
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
            Style::new().fg(theme.muted),
        ));
    }
    if cols.time {
        let text = if e.mtime == 0 {
            "               -".to_string()
        } else {
            fmt_time(e.mtime)
        };
        spans.push(Span::styled(format!("{text} "), Style::new().fg(theme.dim)));
    }

    let name_style = match e.kind {
        EntryKind::Dir => Style::new().fg(theme.dir).bold(),
        EntryKind::Symlink => Style::new().fg(theme.link),
        EntryKind::Other => Style::new().fg(theme.warn),
        EntryKind::File => {
            if e.perms.contains('x') {
                Style::new().fg(theme.good)
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
            Style::new().fg(theme.dim).italic(),
        ));
    }
    Line::from(spans)
}

fn draw_progress(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
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
        .gauge_style(Style::new().fg(theme.accent).bg(theme.on_accent))
        .ratio(ratio)
        .label(text);
    f.render_widget(gauge, area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let style = match app.status_level {
        Level::Info => Style::new().fg(theme.text),
        Level::Good => Style::new().fg(theme.good),
        Level::Bad => Style::new().fg(theme.bad).bold(),
    };
    let icon = match app.status_level {
        Level::Info => "  ",
        Level::Good => "✓ ",
        Level::Bad => "✗ ",
    };
    let text = format!("{icon}{}", app.status);
    f.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

/// A key and what it does, as the bottom line lists them.
/// One hint as its list writes it. A key beginning `@` names an action rather
/// than a keystroke, and is shown as whatever that action answers to now —
/// otherwise rebinding one would leave the bar advertising a key that no
/// longer does anything.
type Hint<'a> = (&'a str, &'a str);

/// The same, with the keys filled in.
type Shown = (String, &'static str);

/// Shown in place of what did not fit.
fn more() -> Shown {
    (String::new(), "…")
}

/// Every hint with its key as it is on this keyboard.
fn shown_hints(app: &App, hints: &[Hint<'static>]) -> Vec<Shown> {
    hints
        .iter()
        .map(|(key, text)| {
            let key = match key.strip_prefix('@').and_then(Action::by_name) {
                Some(action) => match app.keymap.first(action) {
                    Some(chord) => chord.to_string(),
                    None => "—".to_string(),
                },
                None => (*key).to_string(),
            };
            (key, *text)
        })
        .collect()
}

/// How many hints are kept whatever happens, counted from the end.
///
/// Every list ends with its ways out — `?` and `q`, or `Esc` — and a screen
/// too narrow to show everything is exactly when you need those most.
const PINNED: usize = 2;

/// One hint's width on screen: ` key ` and then ` what it does  `.
fn hint_width(hint: &Shown) -> usize {
    let (key, text) = hint;
    let key_width = match key.is_empty() {
        true => 0,
        false => key.chars().count() + 2,
    };
    key_width + text.chars().count() + 3
}

/// The hints that fit, in the order they matter.
///
/// The lists are written most-useful-first, so cutting from the end keeps the
/// keys worth knowing. Anything dropped leaves a `…` behind, so a short line
/// reads as abbreviated rather than as all there is.
fn fit_hints(hints: &[Shown], width: u16) -> Vec<Shown> {
    let width = width as usize;
    let total: usize = hints.iter().map(hint_width).sum();
    if total <= width || hints.len() <= PINNED {
        return hints.to_vec();
    }

    let split = hints.len() - PINNED;
    let tail = &hints[split..];
    let mut used: usize = tail.iter().map(hint_width).sum::<usize>() + hint_width(&more());
    let mut out = Vec::new();
    for hint in &hints[..split] {
        let hint_width = hint_width(hint);
        if used + hint_width > width {
            break;
        }
        out.push(hint.clone());
        used += hint_width;
    }
    out.push(more());
    out.extend_from_slice(tail);
    out
}

/// Every hint list, in the order each is written: most useful first, ways out
/// last, because [`fit_hints`] cuts from the end but keeps the tail.
const SHELL_ZOOMED: &[Hint] = &[
    ("@command", "sshman keys"),
    ("@zoom", "unzoom"),
    ("@close-pane", "close this pane"),
    ("", "every other key goes to the shell"),
];
const SHELL: &[Hint] = &[
    ("@command", "sshman keys"),
    ("drag", "select"),
    ("@zoom", "zoom"),
    ("@close-pane", "close this pane"),
    ("", "every other key goes to the shell"),
];
/// While sshman has the keyboard: the pane keys, and then every other sshman
/// key, which works here exactly as it does with a file list focused.
const COMMAND: &[Hint] = &[
    ("↑↓←→", "pane"),
    ("↵", "use it"),
    ("Shift-↑↓←→", "resize"),
    ("g", "move it"),
    ("@shell", "shell"),
    ("@new-list", "list"),
    ("@close-pane", "close"),
    ("@zoom", "zoom"),
    ("@arrange", "arrange"),
    ("@copy-text", "copy"),
    ("@connect", "connect"),
    ("@help", "help"),
    ("Esc", "back"),
];
/// And while one of them has been picked up.
const CARRYING: &[Hint] = &[
    ("↑↓←→", "shove it past its neighbour"),
    ("Shift-↑↓←→", "send it to that edge"),
    ("↵", "drop it and use it"),
    ("Esc", "put it down"),
];
/// With one pane the keys that act across the middle have nothing to act on,
/// so the ones that work inside one filesystem take their place. The only
/// difference between the two ways of getting there is what `m` does next.
const ZOOMED: &[Hint] = &[
    ("@next-list", "side"),
    ("@open", "open"),
    ("@mark", "mark"),
    ("@copy", "copy"),
    ("@cut", "cut"),
    ("@paste", "paste"),
    ("@zoom", "unzoom"),
    ("@edit", "edit"),
    ("@delete", "del"),
    ("@help", "help"),
    ("@quit", "quit"),
];
const LOCAL_TAB: &[Hint] = &[
    ("@open", "open"),
    ("@mark", "mark"),
    ("@copy", "copy"),
    ("@cut", "cut"),
    ("@paste", "paste"),
    ("@shell", "shell"),
    ("@new-list", "new list"),
    ("@close-pane", "close pane"),
    ("@arrange", "arrange"),
    ("@edit", "edit"),
    ("@delete", "del"),
    ("@connect", "server"),
    ("@help", "help"),
    ("@quit", "quit"),
];
const BROWSE: &[Hint] = &[
    ("@next-list", "pane"),
    ("@open", "open"),
    ("@mark", "mark"),
    ("@copy", "copy →"),
    ("@edit", "edit"),
    ("@delete", "del"),
    ("@zoom", "zoom"),
    ("@shell", "shell"),
    ("@new-list", "new list"),
    ("@close-pane", "close pane"),
    ("@arrange", "arrange"),
    ("@remote-command", "cmd"),
    ("@sudo", "sudo"),
    ("@connect", "connect"),
    ("@workspaces", "workspaces"),
    ("@ports", "ports"),
    ("@local-tab", "local tab"),
    ("@settings", "settings"),
    ("@help", "help"),
    ("@quit", "quit"),
];
const CONNECT: &[Hint] = &[
    ("Tab", "section"),
    ("↑↓", "choose"),
    ("↵", "connect"),
    ("Del", "forget"),
    ("Esc", "back"),
];
const PROMPT: &[Hint] = &[("↵", "confirm"), ("Esc", "cancel")];
const CONFIRM_PHRASE: &[Hint] = &[("type the word", "then ↵"), ("Esc", "cancel")];
const MENU: &[Hint] = &[("↑↓", "choose"), ("↵", "do it"), ("Esc", "put it away")];
const CONFIRM: &[Hint] = &[("y", "yes"), ("n", "no")];
const PICKER: &[Hint] = &[("↑↓", "choose"), ("↵", "open in a tab"), ("Esc", "cancel")];
const FORWARDS: &[Hint] = &[
    ("a", "add"),
    ("d", "stop"),
    ("↑↓", "choose"),
    ("Esc", "close"),
];
const WORKSPACES: &[Hint] = &[
    ("↑↓", "choose"),
    ("↵", "open"),
    ("s", "save what is open"),
    ("Del", "forget"),
    ("Esc", "close"),
];
const SETTINGS: &[Hint] = &[
    ("↑↓", "choose"),
    ("↵", "change"),
    ("Del", "clear"),
    ("Esc", "close"),
];
const ARRANGE: &[Hint] = &[("↑↓", "choose"), ("↵", "arrange"), ("Esc", "close")];
const THEMES: &[Hint] = &[
    ("↑↓", "look through them"),
    ("↵", "keep this one"),
    ("Esc", "put the old one back"),
];
const KEYS: &[Hint] = &[
    ("↑↓", "choose"),
    ("↵", "then press the key you want"),
    ("Del", "back to the one it ships with"),
    ("Esc", "close"),
];
const REBINDING: &[Hint] = &[("", "press the key you want"), ("Esc", "leave it as it is")];
const OUTPUT: &[Hint] = &[("↑↓", "scroll"), ("Esc", "close")];
const HELP_HINTS: &[Hint] = &[("↑↓", "scroll"), ("any key", "close")];

/// All of them, for the test that they are all written the right way round.
#[cfg(test)]
const ALL_HINTS: &[&[Hint]] = &[
    SHELL_ZOOMED,
    SHELL,
    COMMAND,
    CARRYING,
    ZOOMED,
    LOCAL_TAB,
    BROWSE,
    CONNECT,
    PROMPT,
    CONFIRM_PHRASE,
    CONFIRM,
    PICKER,
    FORWARDS,
    WORKSPACES,
    SETTINGS,
    ARRANGE,
    THEMES,
    KEYS,
    REBINDING,
    OUTPUT,
    HELP_HINTS,
];

/// Which list belongs on screen right now.
fn hints_for(app: &App) -> &'static [Hint<'static>] {
    match app.mode {
        // A menu is a question with its answers on the screen; the keys that
        // work it are the only ones worth naming while it is open.
        Mode::Browse if app.menu.is_some() => MENU,
        // Waiting on the key after Ctrl-], the only useful thing to show is
        // what that key can be.
        Mode::Browse if app.carrying => CARRYING,
        Mode::Browse if app.commanding => COMMAND,
        // A focused shell takes every key, so only the way out is worth
        // showing.
        Mode::Browse if app.in_term() && app.zoomed => SHELL_ZOOMED,
        Mode::Browse if app.in_term() => SHELL,
        Mode::Browse if app.zoomed => ZOOMED,
        Mode::Browse if app.on_local_tab() => LOCAL_TAB,
        Mode::Browse => BROWSE,
        Mode::Connect => CONNECT,
        Mode::Prompt => PROMPT,
        Mode::Confirm
            if app
                .confirm
                .as_ref()
                .is_some_and(|c| c.require_phrase.is_some()) =>
        {
            CONFIRM_PHRASE
        }
        Mode::Confirm => CONFIRM,
        Mode::Picker => PICKER,
        Mode::Forwards => FORWARDS,
        Mode::Workspaces => WORKSPACES,
        Mode::Settings => SETTINGS,
        Mode::Arrange => ARRANGE,
        Mode::Themes => THEMES,
        Mode::Keys if app.rebinding.is_some() => REBINDING,
        Mode::Keys => KEYS,
        Mode::Output => OUTPUT,
        Mode::Help => HELP_HINTS,
    }
}

fn draw_hints(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let hints = shown_hints(app, hints_for(app));
    let mut spans = Vec::new();
    for (k, v) in &fit_hints(&hints, area.width) {
        if !k.is_empty() {
            spans.push(Span::styled(
                format!(" {k} "),
                Style::new().fg(theme.on_accent).bg(theme.muted),
            ));
        }
        spans.push(Span::styled(format!(" {v}  "), Style::new().fg(theme.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---- overlays --------------------------------------------------------------

/// How many saved servers to show at once before the list scrolls.
const RECENT_WINDOW: usize = 7;

fn draw_connect(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
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
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            " Connect to a server ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " Tab switches section · Enter connects ",
                Style::new().fg(theme.dim),
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
                    Style::new().fg(theme.accent).bold()
                } else {
                    Style::new().fg(theme.muted).bold()
                },
            ),
            Span::styled(
                "   ↑↓ choose · Del forgets",
                Style::new().fg(theme.dim).italic(),
            ),
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
                (true, true) => (
                    "▸ ",
                    Style::new().fg(theme.on_accent).bg(theme.accent).bold(),
                ),
                (true, false) => ("▸ ", Style::new().fg(theme.accent)),
                _ => ("  ", Style::new().fg(theme.text)),
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
                Span::styled(marker, Style::new().fg(theme.accent)),
                Span::styled(format!("{:<22}", ellipsize(&entry.label(), 22)), style),
                Span::styled(format!("{address:<20}"), Style::new().fg(theme.muted)),
                Span::styled(
                    format!("{:>9}", crate::history::relative_time(entry.last_connected)),
                    Style::new().fg(theme.dim),
                ),
                Span::styled(key_note, Style::new().fg(theme.dim).italic()),
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
            Style::new().fg(theme.accent).bold()
        } else {
            Style::new().fg(theme.muted)
        };
        let (value, value_style) = if values[i].is_empty() {
            (
                placeholders[i].to_string(),
                Style::new().fg(theme.dim).italic(),
            )
        } else {
            (values[i].clone(), Style::new().fg(theme.text))
        };
        lines.push(Line::from(vec![
            Span::styled(marker, Style::new().fg(theme.accent)),
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
            Span::styled(marker, Style::new().fg(theme.accent)),
            Span::styled(
                format!("{box_glyph} "),
                if app.form.install_key {
                    Style::new().fg(theme.good).bold()
                } else {
                    Style::new().fg(theme.muted)
                },
            ),
            Span::styled(
                "Install my public key for passwordless login",
                if focused {
                    Style::new().fg(theme.accent).bold()
                } else {
                    Style::new().fg(theme.muted)
                },
            ),
            Span::styled(
                if focused { "   (Space toggles)" } else { "" },
                Style::new().fg(theme.dim).italic(),
            ),
        ]));
    }

    if app.form.connecting {
        lines.push(Line::from(Span::styled(
            "  connecting…",
            Style::new().fg(theme.warn).bold(),
        )));
    }
    if let Some(err) = &app.form.error {
        lines.push(Line::from(Span::styled(
            format!("  {err}"),
            Style::new().fg(theme.bad).bold(),
        )));
    }
    if let Some(hint) = &app.form.hint {
        lines.push(Line::from(Span::styled(
            format!("  {hint}"),
            Style::new().fg(theme.warn).bold(),
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
    let theme = app.theme;
    let Some(picker) = &app.picker else { return };

    let height = (picker.items.len() as u16 + 4).min(area.height.saturating_sub(4));
    let rect = centered(area, 92, height.max(7));
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            format!(" {} ", picker.title),
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓ choose · ↵ opens it in a tab · Esc cancels ",
                Style::new().fg(theme.dim),
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
                    Style::new().fg(theme.text).bold(),
                ),
                Span::styled(
                    format!("{:<image_width$} ", ellipsize(&c.image, image_width)),
                    Style::new().fg(theme.muted),
                ),
                Span::styled(c.status.clone(), Style::new().fg(theme.good)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(picker.selected));
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(theme.accent)
            .fg(theme.on_accent)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state);
}

/// Saved sets of connections.
/// What is kept in the config file, and what it is set to now.
///
/// The options come from [`Setting::ALL`], so a new one appears here with no
/// changes to this function.
fn draw_settings(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    // Two rows a setting, then a line saying where the themes came from and
    // one for every file that could not be read.
    let rows =
        (Setting::ALL.len() * 2 + 2 + app.themes.problems.len() + app.keymap.problems.len()) as u16;
    let rect = centered(
        area,
        74,
        (rows + 4).min(area.height.saturating_sub(4)).max(7),
    );
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            " Settings ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↵ opens it · ←→ steps it · Del clears · Esc closes ",
                Style::new().fg(theme.dim),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let mut lines = Vec::new();
    for (index, setting) in Setting::ALL.iter().enumerate() {
        let chosen = index == app.settings_sel;
        let name = Style::new().fg(if chosen { theme.text } else { theme.muted });
        let value_style = match app.config.is_set(*setting) {
            true => Style::new().fg(theme.accent).bold(),
            // Inherited from somewhere else, so it is shown but not claimed.
            false => Style::new().fg(theme.muted),
        };
        lines.push(Line::from(vec![
            Span::styled(
                if chosen { " ▸ " } else { "   " },
                Style::new().fg(theme.accent),
            ),
            Span::styled(
                format!("{:<14}", setting.label()),
                if chosen { name.bold() } else { name },
            ),
            Span::styled(ellipsize(&app.config.value(*setting), 32), value_style),
            Span::styled(
                format!("  ({})", app.config.origin(*setting)),
                Style::new().fg(theme.dim),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            format!("   {:<14}{}", "", setting.blurb()),
            Style::new().fg(theme.dim),
        )));
    }

    // Where the colours come from, since a theme is now a file you can copy
    // and edit rather than something only a new binary could change.
    lines.push(Line::from(vec![
        Span::styled(format!("   {:<14}", ""), Style::new().fg(theme.dim)),
        Span::styled(
            format!("{} themes", app.themes.entries.len()),
            Style::new().fg(theme.muted),
        ),
        Span::styled(
            match crate::theme::themes_dir() {
                Some(dir) => format!(" · add your own in {}", ellipsize(&shorten_home(&dir), 34)),
                None => String::new(),
            },
            Style::new().fg(theme.dim),
        ),
    ]));
    // Anything in the config file that could not be used, said where the
    // setting it belongs to is: a line that quietly did nothing is worse than
    // one that was never written.
    for problem in app.themes.problems.iter().chain(&app.keymap.problems) {
        lines.push(Line::from(Span::styled(
            format!("   {}", ellipsize(problem, 67)),
            Style::new().fg(theme.bad),
        )));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

/// Which key asks for what, and how to say otherwise.
fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let height = (Action::ALL.len() as u16 + 4).min(area.height.saturating_sub(4));
    let rect = centered(area, 78, height.max(7));
    clear_under(f, rect, app.background());

    let waiting = app.rebinding.is_some();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if waiting { theme.warn } else { theme.accent }))
        .title_top(Line::from(Span::styled(
            " Keys ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                match waiting {
                    true => " press the key you want · Esc leaves it ".to_string(),
                    false => {
                        " ↑↓ choose · ↵ then press a key · Del resets it · Esc closes ".to_string()
                    }
                },
                Style::new().fg(theme.dim),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    const KEYS_WIDTH: usize = 20;
    let mut group = "";
    let items: Vec<ListItem> = Action::ALL
        .iter()
        .enumerate()
        .map(|(index, action)| {
            let chosen = index == app.action_sel;
            let name = Style::new().fg(if chosen { theme.text } else { theme.muted });
            // The heading goes on the first of its own, so fifty rows read as
            // five short lists rather than one long one.
            let heading = (action.group() != group).then(|| {
                group = action.group();
                group
            });
            let mut spans = vec![Span::styled(
                if chosen { " ▸ " } else { "   " },
                Style::new().fg(theme.accent),
            )];
            let keys = match (chosen, waiting) {
                (true, true) => "press a key".to_string(),
                _ => app.keymap.shown(*action),
            };
            spans.push(Span::styled(
                format!("{:<KEYS_WIDTH$}", ellipsize(&keys, KEYS_WIDTH)),
                match (chosen, waiting) {
                    (true, true) => Style::new().fg(theme.warn).bold(),
                    (true, false) => Style::new().fg(theme.accent).bold(),
                    _ => Style::new().fg(theme.muted),
                },
            ));
            spans.push(Span::styled(
                format!("{:<16}", action.name()),
                if chosen { name.bold() } else { name },
            ));
            spans.push(Span::styled(
                match heading {
                    Some(group) => format!("{group} — {}", action.blurb()),
                    None => action.blurb().to_string(),
                },
                Style::new().fg(if heading.is_some() {
                    theme.info
                } else {
                    theme.dim
                }),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.action_sel.min(Action::ALL.len() - 1)));
    f.render_stateful_widget(List::new(items), inner, &mut state);
}

/// The themes there are, each drawn in its own colours — and the one under
/// the cursor already on the screen behind the list, since a palette is only
/// worth judging at the size you are going to read it at.
fn draw_themes(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let count = app.themes.entries.len();
    let height = (count as u16 + 4).min(area.height.saturating_sub(4));
    let rect = centered(area, 88, height.max(7));
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            " Themes ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                format!(" ↑↓ look · ↵ keeps it · Esc puts the old one back · {count} themes "),
                Style::new().fg(theme.dim),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    const NAME: usize = 16;
    let about_room = (inner.width as usize).saturating_sub(NAME + SWATCH + 6);
    let items: Vec<ListItem> = app
        .themes
        .entries
        .iter()
        .enumerate()
        .map(|(index, named)| {
            let chosen = index == app.theme_sel;
            let name = Style::new().fg(if chosen { theme.text } else { theme.muted });
            let mut spans = vec![
                Span::styled(
                    if chosen { " ▸ " } else { "   " },
                    Style::new().fg(theme.accent),
                ),
                Span::styled(
                    format!("{:<NAME$}", ellipsize(&named.name, NAME)),
                    if chosen { name.bold() } else { name },
                ),
            ];
            spans.extend(swatch(named.theme));
            spans.push(Span::styled(
                format!(
                    "  {}",
                    ellipsize(named.about.as_deref().unwrap_or(""), about_room)
                ),
                Style::new().fg(theme.dim),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();

    // The list widget scrolls to keep the selection on screen, which is what
    // this is for: there are more themes than a box this size can hold.
    let mut state = ListState::default();
    state.select(Some(app.theme_sel.min(count.saturating_sub(1))));
    f.render_stateful_widget(List::new(items), inner, &mut state);
}

/// How many cells a swatch takes.
const SWATCH: usize = 10;

/// A theme's palette at a glance: one block per role that carries a colour of
/// its own, drawn in that theme's colours rather than the one that is on.
fn swatch(theme: Theme) -> Vec<Span<'static>> {
    [
        theme.accent,
        theme.dir,
        theme.link,
        theme.good,
        theme.warn,
        theme.bad,
        theme.info,
        theme.text,
        theme.muted,
        theme.dim,
    ]
    .into_iter()
    .map(|colour| Span::styled("█", Style::new().fg(colour)))
    .collect()
}

/// The list of ready-made arrangements, in the shape of the settings pane:
/// a name, and a line saying what you would get.
fn draw_arrange(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let rows = (Arrangement::ALL.len() * 2) as u16;
    let rect = centered(
        area,
        74,
        (rows + 4).min(area.height.saturating_sub(4)).max(7),
    );
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            " Arrange this tab ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↵ arranges · S splits · | splits sideways · Esc closes ",
                Style::new().fg(theme.dim),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let mut lines = Vec::new();
    for (index, which) in Arrangement::ALL.iter().enumerate() {
        let chosen = index == app.arrangement_sel;
        let name = Style::new().fg(if chosen { theme.text } else { theme.muted });
        lines.push(Line::from(vec![
            Span::styled(
                if chosen { " ▸ " } else { "   " },
                Style::new().fg(theme.accent),
            ),
            Span::styled(which.label(), if chosen { name.bold() } else { name }),
        ]));
        lines.push(Line::from(Span::styled(
            format!("   {}", which.blurb()),
            Style::new().fg(theme.dim),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// A path with your home directory written the way you would write it.
fn shorten_home(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    match dirs::home_dir().map(|home| home.display().to_string()) {
        Some(home) if !home.is_empty() && shown.starts_with(&home) => {
            format!("~{}", &shown[home.len()..])
        }
        _ => shown,
    }
}

fn draw_workspaces(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let rows = app.workspace_rows().max(1) as u16;
    let rect = centered(
        area,
        84,
        (rows + 4).min(area.height.saturating_sub(4)).max(7),
    );
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            " Workspaces ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↵ open · s saves what is open now · Del forgets · Esc closes ",
                Style::new().fg(theme.dim),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    if app.workspaces.is_empty() && !app.session_row() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "No workspaces saved yet.",
                    Style::new().fg(theme.text),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Open the servers and containers you want, then press s to",
                    Style::new().fg(theme.dim),
                )),
                Line::from(Span::styled(
                    "save them together under a name.",
                    Style::new().fg(theme.dim),
                )),
            ]),
            inner,
        );
        return;
    }

    // The session before this one sits above the saved ones: it is the entry
    // people reach for most and the only one nobody had to think to save.
    let listed = app
        .previous_session
        .iter()
        .map(|w| (true, w))
        .chain(app.workspaces.entries.iter().map(|w| (false, w)));
    let items: Vec<ListItem> = listed
        .map(|(session, w)| {
            // The members are the point, so name them rather than just counting.
            let members: Vec<String> = w.items.iter().map(|i| i.describe()).collect();
            let name = match session {
                true => Style::new().fg(theme.accent).bold(),
                false => Style::new().fg(theme.text).bold(),
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{:<18} ", ellipsize(&w.name, 18)), name),
                Span::styled(
                    format!("{:<15} ", w.summary()),
                    Style::new().fg(theme.muted),
                ),
                Span::styled(
                    ellipsize(&members.join(", "), 42),
                    Style::new().fg(theme.dim).italic(),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.workspace_sel));
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(theme.accent)
            .fg(theme.on_accent)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state);
}

/// Ports carried from the server on screen to this machine.
fn draw_forwards(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
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
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            format!(" Forwarded ports — {title} "),
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " a adds · d stops · Esc closes ",
                Style::new().fg(theme.dim),
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
                    Style::new().fg(theme.text),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "Press a and give a port: 3000 forwards it to the same port",
                    Style::new().fg(theme.dim),
                )),
                Line::from(Span::styled(
                    "here; 8080:3000 changes the local one; 8080:db:5432 reaches",
                    Style::new().fg(theme.dim),
                )),
                Line::from(Span::styled(
                    "a host the server can see. A forward binds 127.0.0.1 unless",
                    Style::new().fg(theme.dim),
                )),
                Line::from(Span::styled(
                    "you put an address first: 0.0.0.0:8080:db:5432. Saved with",
                    Style::new().fg(theme.dim),
                )),
                Line::from(Span::styled("the workspace.", Style::new().fg(theme.dim))),
            ]),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = forwards
        .iter()
        .map(|forward| {
            let (state, style) = match (forward.is_running(), forward.error()) {
                (true, _) => ("listening".to_string(), Style::new().fg(theme.good)),
                (false, Some(err)) => (err, Style::new().fg(theme.bad)),
                (false, None) => ("stopped".into(), Style::new().fg(theme.dim)),
            };
            let carried = forward.connection_count();
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<40}", ellipsize(&forward.spec.describe(), 39)),
                    // A forward the network can reach is not the usual case,
                    // and is worth being able to see at a glance.
                    match forward.spec.is_public() {
                        true => Style::new().fg(theme.warn).bold(),
                        false => Style::new().fg(theme.text).bold(),
                    },
                ),
                Span::styled(format!("{:<12}", ellipsize(&state, 12)), style),
                Span::styled(
                    match carried {
                        0 => "no connections yet".to_string(),
                        1 => "1 connection".to_string(),
                        n => format!("{n} connections"),
                    },
                    Style::new().fg(theme.dim),
                ),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.forward_sel()));
    let list = List::new(items).highlight_style(
        Style::new()
            .bg(theme.accent)
            .fg(theme.on_accent)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, inner, &mut state);
}

fn draw_prompt(f: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme;
    let Some(prompt) = &app.prompt else { return };
    let rect = centered(area, 78, 5);
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            format!(" {} ", prompt.title),
            Style::new().fg(theme.accent).bold(),
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
            Span::styled("› ", Style::new().fg(theme.accent).bold()),
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
    let theme = app.theme;
    let Some(state) = &app.confirm else { return };
    let extra = if state.require_phrase.is_some() { 3 } else { 0 };
    let height = (state.body.len() as u16 + 5 + extra).min(area.height.saturating_sub(2));
    let rect = centered(area, 74, height.max(7));
    clear_under(f, rect, app.background());

    let color = if state.danger { theme.bad } else { theme.warn };
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
                Span::styled(state.input.display(), Style::new().fg(theme.text)),
                Span::styled("▏", Style::new().fg(color)),
            ]));
            lines.push(Line::raw(""));
            let ready = state.satisfied();
            lines.push(Line::from(vec![
                Span::styled(
                    " Enter ",
                    if ready {
                        Style::new().fg(theme.on_accent).bg(color).bold()
                    } else {
                        Style::new().fg(theme.dim).bg(theme.on_accent)
                    },
                ),
                Span::styled(
                    if ready {
                        " go ahead    "
                    } else {
                        " (type the word first)    "
                    },
                    Style::new().fg(theme.dim),
                ),
                Span::styled(
                    " Esc ",
                    Style::new().fg(theme.on_accent).bg(theme.muted).bold(),
                ),
                Span::raw(" cancel"),
            ]));
        }
        None => lines.push(Line::from(vec![
            Span::styled(" y ", Style::new().fg(theme.on_accent).bg(color).bold()),
            Span::raw(" yes    "),
            Span::styled(
                " n ",
                Style::new().fg(theme.on_accent).bg(theme.muted).bold(),
            ),
            Span::raw(" no"),
        ])),
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_output(f: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme;
    let rect = centered(
        area,
        area.width.saturating_sub(8),
        area.height.saturating_sub(4),
    );
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            format!(
                " {} ",
                ellipsize(&app.output_title, rect.width.saturating_sub(4) as usize)
            ),
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " ↑↓/PgUp/PgDn scroll · Esc close ",
                Style::new().fg(theme.dim),
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
    let theme = app.theme;
    let rect = centered(area, 76, area.height.saturating_sub(4).min(34));
    clear_under(f, rect, app.background());

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.accent))
        .title_top(Line::from(Span::styled(
            " Keys ",
            Style::new().fg(theme.accent).bold(),
        )))
        .title_bottom(
            Line::from(Span::styled(
                " any key to close ",
                Style::new().fg(theme.dim),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    f.render_widget(block, rect);

    let mut lines = Vec::new();
    for (key, desc) in HELP {
        if key.is_empty() {
            lines.push(Line::from(Span::styled(
                desc.to_string(),
                Style::new().fg(theme.accent).bold(),
            )));
        } else {
            lines.push(Line::from(vec![
                Span::styled(format!("  {key:<14}"), Style::new().fg(theme.warn)),
                Span::raw(desc.to_string()),
            ]));
        }
    }
    app.help_view_height = inner.height;
    f.render_widget(Paragraph::new(lines).scroll((app.help_scroll, 0)), inner);
}

pub const HELP: &[(&str, &str)] = &[
    (
        "",
        "The keys below are the ones sshman ships with. Any you have",
    ),
    (
        "",
        "changed are in , → Keys, which is also where you change one:",
    ),
    ("", "↵ on a line, then press the key you want."),
    ("", ""),
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
    (
        "R",
        "reload both panes now — they follow changes on their own",
    ),
    ("", ""),
    ("", "The mouse, in a file list"),
    ("click", "focus that pane and put the cursor on the row"),
    ("double click", "the same as ↵: enter it, or open it"),
    (
        "right click",
        "a menu of what can be done to what is under it",
    ),
    ("", "  Over a row it is about the file — open, edit, copy,"),
    (
        "",
        "  rename, delete, unpack; over the space below the last",
    ),
    (
        "",
        "  one it is about the directory. Every row carries the key",
    ),
    ("", "  that does the same thing, so the menu teaches the"),
    (
        "",
        "  keyboard rather than replacing it. It aims at the row",
    ),
    (
        "",
        "  under the pointer first — unless that row is one you have",
    ),
    (
        "",
        "  marked, and then the marks stand and it is about all of",
    ),
    ("", "  them."),
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
    ("", "Moving files about within one side"),
    ("M", "cut marked files, to be put down elsewhere"),
    ("P", "paste them into the directory on screen"),
    ("", "  With no other side on screen to copy to — zoomed, or"),
    ("", "  arranged without one — there is no across, so"),
    (
        "",
        "  c picks files up instead of copying across. Both keys stay",
    ),
    (
        "",
        "  on the side they started on: a paste is one command on one",
    ),
    (
        "",
        "  machine, and nothing is ever overwritten — a name already",
    ),
    ("", "  in the way stops the whole paste with a message."),
    ("Esc", "drop what was picked up"),
    ("", ""),
    ("", "Panes"),
    (
        "",
        "  A tab is a set of panes: file lists and terminals, split",
    ),
    (
        "",
        "  any way you like. It opens as this machine beside the",
    ),
    ("", "  server, and every arrangement is built from there."),
    ("", ""),
    (
        "",
        "  More than one file list on a machine is allowed, each in",
    ),
    ("", "  its own directory. c copies into the list marked"),
    (
        "",
        "  \"c copies here\" — the other one, or the one you were in",
    ),
    ("", "  last — whether that is across the middle or two"),
    ("", "  directories of the same machine, where the copy runs"),
    (
        "",
        "  there rather than travelling. t points it at this one.",
    ),
    ("A", "pick a ready-made arrangement for this tab"),
    ("S", "cut the focused pane in two, with a terminal below"),
    (
        "",
        "  or, when this machine already has one, close the last.",
    ),
    ("|", "a terminal beside this pane, closing nothing"),
    ("_", "a terminal below it, closing nothing"),
    ("T", "another file list beside it, closing nothing"),
    ("F9", "close the focused pane, from anywhere"),
    ("[✕]", "the same, for the pane it is drawn in"),
    ("Alt-↑↓←→", "move the keyboard to the pane that way"),
    (
        "",
        "  Alt is what a file list needs, the arrows being its own.",
    ),
    (
        "",
        "  Ctrl-] says the same without it, and h j k l go beside the",
    ),
    ("", "  arrows in both."),
    ("Ctrl-]", "command mode: every sshman key, from anywhere"),
    (
        "m / F3",
        "give the whole screen to the focused pane, or undo that",
    ),
    ("", "  The zoom follows the focus, so Tab keeps working and"),
    (
        "",
        "  m, F3 or Esc brings the other panes back. F3 works from",
    ),
    (
        "",
        "  inside a shell, where every other key is the shell's.",
    ),
    (
        "",
        "  The zoom belongs to the tab: one left full screen on a log",
    ),
    (
        "",
        "  stays that way while the tab beside it stays split. Switching",
    ),
    (
        "",
        "  tabs zoomed into a shell shows the other tab's shell, or its",
    ),
    (
        "",
        "  files when it has none — and coming back puts you in the",
    ),
    ("", "  shell you left, full screen again."),
    ("Alt-Shift-↑↓←→", "move the border nearest the focused pane"),
    ("drag a border", "the same, with the mouse"),
    ("=", "even the borders up again, for the tab on screen"),
    (
        "",
        "  The arrangement belongs to the tab, so each server keeps",
    ),
    (
        "",
        "  the shape you gave it and a new tab opens with the one on",
    ),
    ("", "  screen. A workspace saves it with everything else."),
    ("", ""),
    ("", "The editor pane"),
    (
        "i",
        "an editor pane beside this one, or close the one there is",
    ),
    (
        "A",
        "then Editor: a file list, your editor, a terminal below",
    ),
    (
        "",
        "  i is that pane on its own, for a tab you have already",
    ),
    (
        "",
        "  arranged by hand; the arrangement builds a whole tab around",
    ),
    (
        "",
        "  one. The editor runs in a pane of its own, and clicking a file",
    ),
    (
        "",
        "  in the list opens it there — e does the same from the",
    ),
    (
        "",
        "  keyboard. On a server the pane is a shell on that server,",
    ),
    (
        "",
        "  so the file is edited where it lives with nothing to fetch",
    ),
    (
        "",
        "  or push back. sshman knows the keys for vim, helix, kakoune",
    ),
    (
        "",
        "  and emacs; for anything else it runs your editor at the",
    ),
    (
        "",
        "  pane's prompt. , sets the keys for an editor of your own.",
    ),
    ("", ""),
    ("", "A tab on this machine"),
    ("L", "open one: a single pane, no server involved"),
    (
        "",
        "  It opens where the local pane was, S gives it a shell here,",
    ),
    (
        "",
        "  and everything a pane does works in it. With no other side",
    ),
    (
        "",
        "  to copy to, c picks files up and P puts them down. s turns",
    ),
    (
        "",
        "  on the real sudo and asks for your password. Starting with",
    ),
    ("", "  sshman --local opens straight onto one."),
    ("", ""),
    ("", "Shells inside the panes"),
    (
        "S",
        "open a shell under the focused pane, or close the last",
    ),
    (
        "",
        "  While the shell has focus every key goes to it, including",
    ),
    (
        "",
        "  Ctrl-C and Esc. Ctrl-] is the way back out, and so is",
    ),
    ("", "  clicking another pane."),
    ("", ""),
    ("", "Command mode: every sshman key, from anywhere"),
    ("Ctrl-]", "hand the keyboard to sshman rather than the pane"),
    (
        "",
        "  A focused shell takes every key, so this is how the rest of",
    ),
    (
        "",
        "  sshman is reached without leaving it. Every key then does",
    ),
    (
        "",
        "  exactly what it does with a file list focused — C connects,",
    ),
    (
        "",
        "  w is the workspaces, , is the settings — and these as well:",
    ),
    ("", ""),
    (
        "↑ ↓ ← →",
        "move to the pane that way, without going into it",
    ),
    ("↵", "hand the keyboard to the pane you have moved to"),
    ("Shift-↑↓←→", "move the border nearest it"),
    ("Ctrl-← →", "the tab before this one, and the next"),
    ("g", "pick the pane up, to move it rather than the keyboard"),
    ("", "  ↑ ↓ ← →  shove it past its neighbour"),
    ("", "  Shift-↑↓←→  send it to that edge, a column or a row"),
    ("", "  ↵  drop it and use it     Esc  put it down"),
    (
        "",
        "  The keyboard goes with the pane, so the arrows keep meaning",
    ),
    (
        "",
        "  the same thing however far it has travelled. Dragging a pane",
    ),
    (
        "",
        "  by its name and letting go over another does the same with",
    ),
    ("", "  the mouse."),
    ("Esc / Ctrl-]", "put the keyboard back where it was"),
    (
        "",
        "  The arrows move without handing anything over, so they walk",
    ),
    (
        "",
        "  past a shell rather than falling into it. ↵ is what says",
    ),
    ("", "  \"this one\" — into a shell, if that is what it is."),
    ("", ""),
    ("", "Picking text out of a shell"),
    ("drag", "select text; it is copied when the button comes up"),
    (
        "",
        "  The selection is marked by turning those cells inside out.",
    ),
    (
        "",
        "  It goes to the system clipboard through the terminal —",
    ),
    (
        "",
        "  which works over SSH, where nothing else could — and is",
    ),
    (
        "",
        "  kept, so Ctrl-] p types it into any shell pane. A program",
    ),
    (
        "",
        "  that has asked for the mouse gets the drag instead; hold",
    ),
    ("", "  Shift to select over one of those."),
    (
        "",
        "  A program inside a pane copying for itself — \"+y in vim, a",
    ),
    (
        "",
        "  tmux copy mode — reaches the same clipboard the same way.",
    ),
    ("wheel", "scroll the shell's history"),
    (
        "",
        "  A program that wants the mouse — btop, a pager — gets it",
    ),
    (
        "",
        "  instead: clicks, drags and the wheel all reach it. Hold",
    ),
    ("", "  Shift to scroll the pane's own history anyway."),
    ("F3", "give the shell the whole screen, or undo that"),
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
    (",", "settings: what sshman remembers between sessions"),
    (
        "",
        "  Keys lists everything sshman can be asked to do and which",
    ),
    (
        "",
        "  key asks for it. ↵ on one, then press the key you want —",
    ),
    (
        "",
        "  it is taken off whatever had it, and Del puts it back. Only",
    ),
    (
        "",
        "  what you changed is written to the config file, and a key",
    ),
    ("", "  can be given to two things there but not here."),
    (
        "",
        "  Background says whether a theme's own is painted, or the",
    ),
    (
        "",
        "  terminal's is left showing. Painting one is cell painting",
    ),
    (
        "",
        "  inside the alternate screen, the same thing a full-screen",
    ),
    (
        "",
        "  editor does — nothing about the terminal itself changes,",
    ),
    ("", "  and leaving sshman puts it back either way."),
    (
        "",
        "  Shell colours says the same about a shell pane's own output:",
    ),
    (
        "",
        "  the sixteen a program asks for by number are names for roles,",
    ),
    (
        "",
        "  and the theme is what they mean here. An exact colour it",
    ),
    ("", "  named is passed on as it named it."),
    (
        "",
        "  ↵ opens the one under the cursor: a prompt for the ones you",
    ),
    (
        "",
        "  type an answer to, and for the theme a list of every one",
    ),
    (
        "",
        "  there is, each drawn in its own colours, with the whole",
    ),
    (
        "",
        "  screen in whichever the cursor is on. ↵ keeps it, Esc puts",
    ),
    (
        "",
        "  the old one back. ←/→ step a setting in place without",
    ),
    ("", "  opening anything."),
    (
        "",
        "  Del clears one back to whatever it would have been. Kept in",
    ),
    ("", "  the config file, ~/.config/sshman/config.json."),
    (
        "",
        "  The editor lives here, used in place of $VISUAL and $EDITOR;",
    ),
    (
        "",
        "  --editor overrides it for a single run. The theme lives here",
    ),
    (
        "",
        "  too: terminal, catppuccin, monokai, gruvbox, mariana,",
    ),
    ("", "  afterglow or darcula."),
    (
        "e / F4",
        "open in your editor, in place when the file is on this",
    ),
    ("", "  machine; a file on a server comes back automatically"),
    ("E", "open with a program you name"),
    ("v", "open in $PAGER"),
    (":", "run a command in the remote pane's directory"),
    ("!", "hand the whole terminal to a shell where the pane is:"),
    (
        "",
        "  ssh for a server, exec into a container, a login shell on",
    ),
    ("", "  this machine"),
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
        "  They bind 127.0.0.1 unless you put an address in front —",
    ),
    (
        "",
        "  0.0.0.0:8080:db:5432 opens it to the network, [::1] for v6.",
    ),
    ("", "  They are saved with the workspace."),
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
        "  set, Del forgets one. Each member remembers the panes it was",
    ),
    (
        "",
        "  arranged into, terminals among them, and the directory every",
    ),
    (
        "",
        "  one of them was pointed at — so a tab left split with a shell",
    ),
    (
        "",
        "  in /var/log opens split with a shell in /var/log. The shells",
    ),
    (
        "",
        "  come back running, on every tab rather than only the one you",
    ),
    (
        "",
        "  look at first. The session itself cannot: a pty whose process",
    ),
    (
        "",
        "  has ended is gone, so what you get is a fresh shell where the",
    ),
    ("", "  old one was."),
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
    ("", "The last session, without saving anything"),
    (
        "",
        "  sshman writes down where the session got to as you work, so",
    ),
    (
        "",
        "  it survives being closed any way at all — quit, a closed",
    ),
    (
        "",
        "  window, a machine that never woke up. `previous session` at",
    ),
    (
        "",
        "  the top of the workspace list brings it back, and so does",
    ),
    (
        "",
        "  `sshman --resume`. Del on that row forgets it. It is exactly",
    ),
    (
        "",
        "  a workspace you never had to name, and holds no more than one",
    ),
    ("", "  does — no passwords, and no shell history."),
    ("", ""),
    ("", "Servers and tabs"),
    ("C", "the connection screen: a saved server or a new one"),
    (
        "[+]",
        "the same, clicked: it sits at the end of the top bar",
    ),
    ("", "  Whatever it connects to opens in a tab of its own."),
    (
        "W",
        "close the tab on screen (ends that session and its shell)",
    ),
    ("✕", "the same for any tab, clicked on its own chip"),
    (
        "hover a chip",
        "the whole name, where the row had to cut it short",
    ),
    ("Ctrl-← / Ctrl-→", "move between tabs"),
    (
        "Ctrl-⇧-← / →",
        "move this tab along the row, wrapping at the ends",
    ),
    ("drag", "the same with the mouse: take a chip and let go"),
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
    (
        "q / Ctrl-C",
        "quit — it asks first, and the same key again is yes",
    ),
    (
        "",
        "  Everything open goes with it, so the second press is the",
    ),
    (
        "",
        "  answer to the first. What was open is written down either",
    ),
    ("", "  way: `sshman --resume` brings it back."),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::sshconn::ConnectOpts;
    use crate::theme::Themes;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    /// Draw a whole frame and hand back the screen itself, for tests that
    /// care about how a cell is painted rather than what it says.
    fn painted(width: u16, height: u16, setup: impl FnOnce(&mut App)) -> (App, Buffer) {
        let dir = std::env::temp_dir().join(format!("sshman-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(ConnectOpts::default(), dir.clone(), None, false);
        app.mode = Mode::Browse;
        setup(&mut app);

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        std::fs::remove_dir_all(&dir).ok();
        (app, buffer)
    }

    /// Draw a whole frame and hand back what landed on the screen, along with
    /// the app that recorded where it put things.
    fn frame(width: u16, height: u16, setup: impl FnOnce(&mut App)) -> (App, Vec<String>) {
        let dir = std::env::temp_dir().join(format!("sshman-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new(ConnectOpts::default(), dir.clone(), None, false);
        app.mode = Mode::Browse;
        setup(&mut app);

        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let rows = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        std::fs::remove_dir_all(&dir).ok();
        (app, rows)
    }

    /// A file list with one archive in it, and a menu open over that row.
    fn menu_over_an_archive(width: u16, height: u16, cursor: usize) -> Vec<String> {
        frame(width, height, |app| {
            let files = Slot::files(Side::Local);
            app.pane_mut(files).set_entries(vec![FileEntry {
                name: "notes.tar.gz".into(),
                kind: EntryKind::File,
                size: 12,
                mtime: 0,
                perms: "-rw-r--r--".into(),
                link_target: None,
                points_to_dir: false,
            }]);
            app.pane_mut(files).select_index(0);
            app.open_menu(files, Some(0), 6, 3);
            if let Some(menu) = &mut app.menu {
                menu.cursor = cursor;
            }
        })
        .1
    }

    #[test]
    fn the_menu_says_what_can_be_done_and_which_key_does_it() {
        let rows = menu_over_an_archive(90, 24, 0);
        let screen = rows.join("\n");
        assert!(screen.contains("Rename…"), "{screen}");
        // The key beside each row, so the menu teaches the keyboard rather
        // than replacing it.
        assert!(screen.contains("r / F2"), "{screen}");
        // Only offered because this row is an archive.
        assert!(screen.contains("Extract…"), "{screen}");
        // The rules are joined to the frame rather than floating inside it.
        assert!(
            rows.iter()
                .any(|row| row.contains("├") && row.contains("┤")),
            "{screen}"
        );
    }

    #[test]
    fn a_menu_taller_than_the_terminal_still_reaches_its_last_row() {
        // Twenty rows of menu in fourteen rows of terminal. Cut off at the
        // bottom, the rows past the edge could never be chosen — so it
        // follows the light down instead.
        let rows = menu_over_an_archive(90, 14, 18);
        let screen = rows.join("\n");
        assert!(screen.contains("A shell here"), "{screen}");
        assert!(
            screen.contains("Reload"),
            "the last row is still off: {screen}"
        );
        // And it went no further than it had to: the top of the menu is what
        // scrolled away, not the row the light is on.
        assert!(!screen.contains(" Open "), "it did not scroll: {screen}");
    }

    /// What is actually on the screen where a button was recorded.
    fn button_text(rows: &[String], rect: Rect) -> String {
        rows[rect.y as usize]
            .chars()
            .skip(rect.x as usize)
            .take(rect.width as usize)
            .collect()
    }

    /// A line of hints with their keys already filled in, which is the form
    /// the fitting works on.
    fn hints() -> Vec<Shown> {
        [
            ("Tab", "pane"),
            ("↵", "open"),
            ("Space", "mark"),
            ("c", "copy →"),
            ("?", "help"),
            ("q", "quit"),
        ]
        .into_iter()
        .map(|(k, v): (&str, &'static str)| (k.to_string(), v))
        .collect()
    }

    fn line_width(hints: &[Shown]) -> usize {
        hints.iter().map(hint_width).sum()
    }

    #[test]
    fn a_line_that_fits_is_left_alone() {
        let hints = hints();
        let wide = line_width(&hints) as u16;
        assert_eq!(fit_hints(&hints, wide), hints);
        assert_eq!(fit_hints(&hints, wide + 40), hints);
    }

    #[test]
    fn a_narrow_line_keeps_the_ways_out_and_says_it_was_cut() {
        let hints = hints();
        let fitted = fit_hints(&hints, 40);
        assert!(line_width(&fitted) <= 40, "{fitted:?}");
        assert_eq!(
            &fitted[fitted.len() - 2..],
            &hints[hints.len() - 2..],
            "help and quit survive whatever else goes"
        );
        assert!(
            fitted.contains(&more()),
            "and the cut is visible: {fitted:?}"
        );
        // What is kept comes off the front, in order.
        assert_eq!(fitted[0], hints[0]);
    }

    #[test]
    fn the_narrower_it_gets_the_less_is_shown() {
        let hints = hints();
        let mut last = usize::MAX;
        for width in [200, 60, 40, 30, 20] {
            let fitted = fit_hints(&hints, width);
            assert!(
                fitted.len() <= last,
                "{width} showed more than the one before"
            );
            last = fitted.len();
        }
        // Even with nothing to spare, the ways out are still there.
        let squeezed = fit_hints(&hints, 1);
        assert_eq!(&squeezed[squeezed.len() - 2..], &hints[hints.len() - 2..]);
    }

    #[test]
    fn every_hint_that_names_an_action_names_a_real_one() {
        // A `@name` that matched nothing would show as itself, advertising a
        // key that is not a key.
        for list in ALL_HINTS {
            for (key, _) in *list {
                if let Some(name) = key.strip_prefix('@') {
                    assert!(Action::by_name(name).is_some(), "no action called {name:?}");
                }
            }
        }
    }

    #[test]
    fn the_bar_shows_the_keys_you_have_rather_than_the_ones_it_ships_with() {
        let (app, rows) = frame(110, 30, |app| {
            app.keymap
                .bind(Action::Quit, crate::keys::Chord::parse("Q").expect("a key"));
        });
        let bar = rows.last().expect("the hints").clone();
        assert!(bar.contains(" Q  quit"), "{bar}");
        assert!(!bar.contains(" q  quit"), "the old key is gone: {bar}");
        // And the rest of the line is untouched.
        assert!(bar.contains("quit"));
        drop(app);
    }

    #[test]
    fn a_short_list_is_never_cut() {
        // Two hints are all ways out; there is nothing to drop.
        let pair: Vec<Shown> = vec![("↵".to_string(), "confirm"), ("Esc".to_string(), "cancel")];
        assert_eq!(fit_hints(&pair, 1), pair);
    }

    #[test]
    fn keys_are_measured_in_characters_not_bytes() {
        // `Ctrl-←/→` is 8 characters and 12 bytes; measuring bytes would cut
        // the line short of what actually fits.
        assert_eq!(hint_width(&("Ctrl-←/→".to_string(), "tabs")), 8 + 2 + 4 + 3);
        assert_eq!(
            hint_width(&(String::new(), "every other key goes to the shell")),
            36
        );
    }

    #[test]
    fn every_hint_list_ends_with_a_way_out() {
        // The fitting keeps the last two whatever happens, which is only the
        // right rule while the lists are written that way round.
        for hints in ALL_HINTS {
            let tail: Vec<&str> = hints
                .iter()
                .rev()
                .take(PINNED)
                .map(|(key, text)| if key.is_empty() { *text } else { *key })
                .collect();
            assert!(
                tail.iter().any(|k| matches!(
                    *k,
                    "q" | "@quit" | "Esc" | "n" | "any key" | "every other key goes to the shell"
                )),
                "{tail:?} has no way out in it"
            );
        }
    }

    /// The colours actually used to paint the screen.
    fn palette(buffer: &Buffer) -> Vec<Color> {
        buffer.content().iter().map(|cell| cell.fg).collect()
    }

    #[test]
    fn the_chosen_theme_is_what_reaches_the_screen() {
        for named in &Themes::built_in().entries {
            let theme = named.theme;
            let (_, buffer) = painted(110, 30, |app| app.theme = theme);
            let used = palette(&buffer);
            // The focused pane's border and titles are the accent, and the
            // hints below are dim: if a role never arrives, something is
            // still painting a colour of its own.
            assert!(
                used.contains(&theme.accent),
                "{}: nothing is drawn in the accent",
                named.name
            );
            assert!(
                used.contains(&theme.dim),
                "{}: nothing is drawn dim",
                named.name
            );
            assert!(
                used.contains(&theme.text),
                "{}: nothing is drawn as plain text",
                named.name
            );
        }
    }

    #[test]
    fn no_colour_is_painted_that_the_theme_did_not_choose() {
        // Every colour on screen has to come from the palette. A literal left
        // behind in the drawing code shows up here as a colour no theme knows
        // about, whichever theme is on.
        let screens = [
            Mode::Browse,
            Mode::Connect,
            Mode::Settings,
            Mode::Arrange,
            Mode::Themes,
            Mode::Workspaces,
            Mode::Help,
            Mode::Output,
            Mode::Picker,
            Mode::Forwards,
            Mode::Prompt,
            Mode::Confirm,
        ];
        for (named, mode) in Themes::built_in()
            .entries
            .into_iter()
            .flat_map(|t| screens.map(|m| (t.clone(), m)))
        {
            let theme = named.theme;
            let (app, buffer) = painted(110, 30, |app| {
                app.theme = theme;
                app.mode = mode;
            });
            let mut known = vec![
                theme.accent,
                theme.dim,
                theme.text,
                theme.muted,
                theme.good,
                theme.warn,
                theme.bad,
                theme.dir,
                theme.link,
                theme.exec,
                theme.info,
                theme.on_accent,
                // Cells nobody styled keep the terminal's own colour.
                Color::Reset,
            ];
            // The theme chooser draws every theme in its own colours: that is
            // the screen, not a colour that escaped.
            if mode == Mode::Themes {
                for other in &app.themes.entries {
                    known.extend([
                        other.theme.accent,
                        other.theme.dim,
                        other.theme.text,
                        other.theme.muted,
                        other.theme.good,
                        other.theme.warn,
                        other.theme.bad,
                        other.theme.dir,
                        other.theme.link,
                        other.theme.exec,
                        other.theme.info,
                        other.theme.on_accent,
                    ]);
                }
            }
            for colour in palette(&buffer) {
                assert!(
                    known.contains(&colour),
                    "{:?} is painted on the {:?} screen in {}, but is not one \
                     of its colours",
                    colour,
                    mode,
                    named.name
                );
            }
        }
    }

    #[test]
    fn the_top_bar_offers_a_new_tab_button_where_it_says_it_does() {
        let (app, rows) = frame(110, 30, |_| {});
        let rect = app.new_tab_button.expect("there is one");
        assert_eq!(button_text(&rows, rect), "[+]", "at {rect:?}");
        assert_eq!(rect.y, 0, "on the top bar");
    }

    #[test]
    fn the_new_tab_button_is_not_offered_where_it_would_not_work() {
        // Mouse handling belongs to browsing; a button that did nothing when
        // clicked would be worse than no button.
        let (app, _) = frame(110, 30, |app| app.mode = Mode::Connect);
        assert_eq!(app.new_tab_button, None);

        // Nor on a bar too narrow to hold it and a title both.
        let (app, _) = frame(10, 20, |_| {});
        assert_eq!(app.new_tab_button, None);
    }

    #[test]
    fn every_pane_offers_a_zoom_button_where_it_says_it_does() {
        // The button is drawn as a right-aligned title and clicked through a
        // rect worked out separately: if those two ever disagree, the button
        // stops working with no visible sign of why.
        let (app, rows) = frame(110, 30, |_| {});
        assert_eq!(app.zoom_buttons.len(), 2, "one per pane on screen");
        for (rect, ..) in &app.zoom_buttons {
            assert_eq!(button_text(&rows, *rect), "[⤢]", "at {rect:?}");
        }
    }

    #[test]
    fn every_pane_offers_a_close_button_where_it_says_it_does() {
        let (app, rows) = frame(110, 30, |_| {});
        assert_eq!(app.close_buttons.len(), 2, "one per pane on screen");
        for (rect, ..) in &app.close_buttons {
            assert_eq!(button_text(&rows, *rect), "[✕]", "at {rect:?}");
        }
        // Side by side with the zoom, never on top of it.
        for (close, ..) in &app.close_buttons {
            for (zoom, ..) in &app.zoom_buttons {
                assert!(
                    close.right() <= zoom.x || zoom.right() <= close.x,
                    "{close:?} and {zoom:?} overlap"
                );
            }
        }
    }

    #[test]
    fn every_piece_of_a_path_is_clickable_where_it_is_drawn() {
        // The trail is drawn as a title and clicked through rects worked out
        // separately: if those two ever disagree, clicking a crumb quietly
        // goes somewhere else.
        let here = Slot::files(Side::Local);
        let (app, rows) = frame(110, 30, |_| {});
        let path = app.path_of(here);
        let trail: Vec<(Rect, String)> = app
            .crumbs
            .iter()
            .filter(|(_, slot, _)| *slot == here)
            .map(|(rect, _, path)| (*rect, path.clone()))
            .collect();

        let want = crate::types::crumbs(&path);
        assert!(!want.is_empty(), "the pane is somewhere: {path}");
        assert_eq!(trail.len(), want.len(), "a crumb per piece of {path}");

        for (i, ((rect, leads_to), (name, wanted))) in trail.iter().zip(&want).enumerate() {
            let drawn = button_text(&rows, *rect);
            assert_eq!(
                drawn.trim_start_matches('/'),
                match i {
                    0 => "",
                    _ => name.as_str(),
                },
                "piece {i} of {path} is drawn as {drawn:?}"
            );
            assert_eq!(leads_to, wanted, "and leads where it is drawn");
        }

        // Side by side along the top border, never on top of one another.
        for (a, _) in &trail {
            assert_eq!(a.y, app.areas.of(here).unwrap().y);
            for (b, _) in &trail {
                assert!(
                    a == b || a.right() <= b.x || b.right() <= a.x,
                    "{a:?} {b:?}"
                );
            }
        }
    }

    #[test]
    fn a_path_too_long_for_the_pane_loses_whole_pieces() {
        let root = std::env::temp_dir().join(format!("sshman-crumbs-{}", std::process::id()));
        let deep = root.join("one/two/three/four/five/six/seven/eight/nine/ten");
        std::fs::create_dir_all(&deep).unwrap();

        let here = Slot::files(Side::Local);
        let (app, rows) = frame(60, 30, |app| {
            app.goto(here, deep.display().to_string());
        });
        let shown = app.path_of(here);
        let trail: Vec<(Rect, String)> = app
            .crumbs
            .iter()
            .filter(|(_, slot, _)| *slot == here)
            .map(|(rect, _, path)| (*rect, path.clone()))
            .collect();

        assert!(!trail.is_empty(), "the end of the path is still a trail");
        assert!(
            trail.len() < crate::types::crumbs(&shown).len(),
            "and not all of {shown} fits in half of 60 columns"
        );
        // Whatever is left is whole pieces of the real path rather than
        // halves of names, which is the point of dropping them from the front.
        for (rect, path) in &trail {
            let drawn = button_text(&rows, *rect);
            assert!(
                shown.contains(drawn.trim_start_matches('/')),
                "{drawn:?} is not a piece of {shown}"
            );
            assert!(shown.starts_with(path.as_str()), "{path} is not a prefix");
        }
        assert_eq!(
            trail.last().map(|(_, p)| p.as_str()),
            Some(shown.as_str()),
            "and the last piece is where the pane actually is"
        );
        // The pieces that did not fit say so.
        let border: String = rows[app.areas.of(here).unwrap().y as usize].clone();
        assert!(border.contains('…'), "{border}");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_last_pane_on_screen_offers_no_way_to_close_it() {
        // Zoomed there is nothing to give the space back to, and a button
        // that shut the only pane you can see would be a trapdoor.
        let (app, _) = frame(110, 30, |app| app.zoomed = true);
        assert!(app.close_buttons.is_empty(), "{:?}", app.close_buttons);
    }

    #[test]
    fn a_pane_with_room_for_one_button_keeps_the_zoom() {
        // The close button is the one to go: F9 is not hidden behind a pane
        // being wide enough, and the zoom has no other way in with a mouse.
        let (app, _) = frame(60, 30, |_| {});
        assert_eq!(app.zoom_buttons.len(), 2);
        assert!(app.close_buttons.is_empty());
    }

    #[test]
    fn a_zoomed_pane_offers_the_way_back() {
        let (app, rows) = frame(110, 30, |app| app.zoomed = true);
        assert_eq!(app.zoom_buttons.len(), 1, "only one pane is drawn");
        let (rect, ..) = app.zoom_buttons[0];
        assert_eq!(button_text(&rows, rect), "[⤡]");
    }

    #[test]
    fn a_pane_with_no_room_for_a_button_does_not_pretend_to_have_one() {
        // Nothing recorded means nothing to click, rather than a rect over
        // whatever the title happened to leave there.
        let (app, _) = frame(30, 20, |_| {});
        assert!(app.zoom_buttons.is_empty(), "{:?}", app.zoom_buttons);
    }

    #[test]
    fn the_button_does_not_eat_the_path() {
        let (_, rows) = frame(110, 30, |_| {});
        assert!(rows[1].contains("LOCAL"), "{}", rows[1]);
        assert!(
            rows[1].ends_with("[⤢]┓") || rows[1].contains("[⤢]"),
            "{}",
            rows[1]
        );
    }

    #[test]
    fn the_tab_row_always_shows_the_tab_you_are_on() {
        // Twenty tabs cannot fit, so the row is a window on to them. Wherever
        // that window is, the one on screen has to be in it — a tab bar that
        // does not show the current tab is worse than no tab bar.
        let widths = vec![14u16; 20];
        for active in [0, 1, 9, 18, 19] {
            let shown = tab_window(&widths, active, 80);
            assert!(shown.contains(&active), "{active} fell off the row");
            let used: u16 = shown.clone().map(|i| widths[i]).sum();
            assert!(used + TAB_MARK * 2 <= 80, "{shown:?} does not fit");
            assert!(!shown.is_empty());
        }
    }

    #[test]
    fn a_row_with_room_for_every_tab_is_not_windowed() {
        let widths = vec![10u16; 4];
        assert_eq!(tab_window(&widths, 0, 110), 0..4);
    }

    #[test]
    fn a_row_too_narrow_for_even_one_tab_still_shows_the_one_you_are_on() {
        let widths = vec![40u16; 5];
        let shown = tab_window(&widths, 3, 20);
        assert_eq!(shown, 3..4);
    }

    #[test]
    fn names_shrink_as_the_tabs_pile_up() {
        assert_eq!(tab_title_budget(110, 2), 24, "two tabs have room to spare");
        assert!(tab_title_budget(110, 8) < 24);
        assert!(
            tab_title_budget(110, 40) >= 6,
            "but never to nothing at all"
        );
    }

    #[test]
    fn the_tab_row_says_how_many_did_not_fit_and_lets_you_step_to_them() {
        let (app, rows) = frame(60, 20, |app| {
            for host in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"] {
                app.fake_tab(host);
            }
            app.goto_tab(3);
        });
        let row = &rows[1];
        assert!(row.contains('‹') || row.contains('›'), "{row}");
        assert!(
            row.contains(" 4 me@"),
            "the tab on screen is on the row: {row}"
        );
        assert!(row.contains("‹2"), "and the two it scrolled past: {row}");
        // Every chip on the row can be clicked, including the marks.
        assert!(app.tab_spans.iter().any(|(_, _, i)| *i == 3));
        for (start, end, _) in &app.tab_spans {
            assert!(*end <= 60 && start < end, "{start}..{end}");
        }
    }

    #[test]
    fn every_tab_offers_a_way_to_close_it_where_it_says_it_does() {
        let (app, rows) = frame(110, 20, |app| {
            for host in ["alpha", "bravo"] {
                app.fake_tab(host);
            }
        });
        assert_eq!(app.tab_close_buttons.len(), 2, "one per tab on the row");
        for (start, end, index) in &app.tab_close_buttons {
            let drawn: String = rows[1]
                .chars()
                .skip(*start as usize)
                .take((end - start) as usize)
                .collect();
            assert!(drawn.starts_with('✕'), "tab {index}: {drawn:?}");
            // It sits inside the chip it belongs to, which is what makes the
            // rest of the chip still mean "go here".
            let chip = app
                .tab_spans
                .iter()
                .find(|(_, _, i)| i == index)
                .expect("the chip it belongs to");
            assert!(
                chip.0 <= *start && *end <= chip.1,
                "{chip:?} vs {start}..{end}"
            );
        }
    }

    #[test]
    fn the_row_of_tabs_is_not_wider_than_the_screen_once_they_carry_buttons() {
        let (app, _) = frame(60, 20, |app| {
            for host in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"] {
                app.fake_tab(host);
            }
            app.goto_tab(3);
        });
        for (start, end, _) in &app.tab_spans {
            assert!(*end <= 60 && start < end, "{start}..{end}");
        }
        for (start, end, _) in &app.tab_close_buttons {
            assert!(*end <= 60 && start < end, "{start}..{end}");
        }
    }

    #[test]
    fn a_selection_is_drawn_inside_out_over_whatever_the_program_painted() {
        // No colour of sshman's own goes over it: reversing the cells reads as
        // a selection in any theme, on any terminal, whatever is underneath.
        let dir = std::env::temp_dir().join(format!("sshman-sel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (app, buffer) = painted(110, 30, |app| {
            let slot = app.open_test_term(&dir);
            app.focus = slot;
            if let Some(shell) = app.shell_mut(slot) {
                shell.begin_selection(0, 2);
                shell.drag_selection(0, 5);
            }
        });
        let inner = app
            .term_inner
            .first()
            .map(|(_, rect)| *rect)
            .expect("the terminal was drawn");

        let reversed = |x: u16| {
            buffer[(inner.x + x, inner.y)]
                .modifier
                .contains(Modifier::REVERSED)
        };
        assert!(!reversed(1), "just before it");
        for x in 2..=5 {
            assert!(reversed(x), "column {x} is not marked");
        }
        assert!(!reversed(6), "just after it");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every background on the screen, cell by cell.
    fn backgrounds(buffer: &Buffer) -> Vec<Color> {
        buffer.content().iter().map(|cell| cell.bg).collect()
    }

    #[test]
    fn a_theme_that_names_a_background_paints_every_cell() {
        // Including behind the shell panes, where the cells the program
        // inside left unpainted are ours to colour: for those panes sshman is
        // the terminal emulator, and this is what its default background is.
        let dir = std::env::temp_dir().join(format!("sshman-bg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let painted_in = |name: &str| {
            let theme = Themes::built_in().by_name(name).expect("a theme we ship");
            painted(110, 30, |app| {
                app.theme = theme;
                let slot = app.open_test_term(&dir);
                app.focus = slot;
            })
        };

        let (app, buffer) = painted_in("catppuccin");
        assert_ne!(app.background(), Color::Reset, "this theme names one");
        for (at, bg) in backgrounds(&buffer).into_iter().enumerate() {
            assert_ne!(
                bg,
                Color::Reset,
                "cell {at} is still the terminal's own colour"
            );
        }

        // And the theme that names none leaves every one of them alone.
        let (app, buffer) = painted_in("terminal");
        assert_eq!(app.background(), Color::Reset);
        assert!(
            backgrounds(&buffer).contains(&Color::Reset),
            "the terminal's own background has to show through"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shell_pane_is_coloured_from_the_theme() {
        let dir = std::env::temp_dir().join(format!("sshman-ansi-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let catppuccin = Themes::built_in().by_name("catppuccin").expect("shipped");

        // A shell that has printed something in ANSI red — the number one, as
        // ls and git and every prompt ask for it.
        let run = |shell_colours: Option<&str>| {
            painted(110, 30, |app| {
                app.theme = catppuccin;
                app.config.shell_colours = shell_colours.map(String::from);
                let slot = app.open_test_term(&dir);
                app.focus = slot;
                let Some(shell) = app.shell_mut(slot) else {
                    return;
                };
                shell.type_in("printf '\\033[31mRED\\033[0m\\n'\n");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !shell
                    .with_screen(|s| s.rows(0, 80).any(|row| row.trim_end().ends_with("RED")))
                {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the shell said nothing"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            })
        };

        let inner_of = |app: &App| app.term_inner.first().map(|(_, r)| *r).expect("drawn");
        let colours = |app: &App, buffer: &Buffer| {
            let inner = inner_of(app);
            let mut seen = Vec::new();
            for y in inner.y..inner.bottom() {
                for x in inner.x..inner.right() {
                    seen.push((buffer[(x, y)].fg, buffer[(x, y)].bg));
                }
            }
            seen
        };

        let (app, buffer) = run(None);
        let seen = colours(&app, &buffer);
        assert!(
            seen.iter().any(|(fg, _)| *fg == catppuccin.ansi[1]),
            "the shell's red is not the theme's red"
        );
        assert!(
            !seen.iter().any(|(fg, _)| *fg == Color::Indexed(1)),
            "the terminal's own red reached the screen"
        );
        assert!(
            !seen
                .iter()
                .any(|(fg, bg)| *fg == Color::Reset || *bg == Color::Reset),
            "nothing in the pane is left to the terminal to colour"
        );

        // And with the setting the other way, the terminal's palette is left
        // to answer for its own numbers.
        let (app, buffer) = run(Some("terminal"));
        let seen = colours(&app, &buffer);
        assert!(
            seen.iter().any(|(fg, _)| *fg == Color::Indexed(1)),
            "the number should have been passed on untouched"
        );
        assert!(
            !seen.iter().any(|(fg, _)| *fg == catppuccin.ansi[1]),
            "the theme coloured it anyway"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_terminals_own_background_comes_back_when_it_is_asked_for() {
        let (app, buffer) = painted(110, 30, |app| {
            app.theme = Themes::built_in().by_name("catppuccin").expect("shipped");
            app.config.background = Some("terminal".into());
        });
        assert_eq!(app.background(), Color::Reset);
        assert!(backgrounds(&buffer).contains(&Color::Reset));
    }

    #[test]
    fn the_theme_chooser_draws_each_theme_in_its_own_colours() {
        // The swatches are the point of the list: a row of blocks that are
        // the theme's colours, not the colours of the theme that is on.
        // Tall enough that the list does not have to scroll, so every theme
        // is on the screen to look for.
        let (app, buffer) = painted(110, 60, |app| {
            app.open_themes();
            app.theme_sel = 0;
        });
        let painted: Vec<Color> = palette(&buffer);
        for named in &app.themes.entries {
            assert!(
                painted.contains(&named.theme.accent),
                "{}'s accent is not on the screen",
                named.name
            );
        }
    }

    #[test]
    fn a_pane_is_taken_hold_of_where_its_name_is_written() {
        // The mouse moves a pane by its name, so what was recorded to click
        // on has to be where the name actually landed.
        let (app, rows) = frame(110, 30, |_| {});
        assert_eq!(app.pane_titles.len(), 2, "one per pane on screen");
        for (rect, slot) in &app.pane_titles {
            let text: String = rows[rect.y as usize]
                .chars()
                .skip(rect.x as usize)
                .take(rect.width as usize)
                .collect();
            let label = files_label(&app, *slot);
            assert_eq!(text, format!(" {label} "), "at {rect:?}");
        }
    }

    #[test]
    fn the_only_pane_there_is_offers_nothing_to_take_hold_of() {
        // There is nowhere to move it to, so the drag would be a click that
        // did nothing and looked like it should have.
        let (app, _) = frame(110, 30, |app| app.zoomed = true);
        assert!(app.pane_titles.is_empty(), "{:?}", app.pane_titles);
    }

    #[test]
    fn every_pane_the_arrangement_names_is_drawn_where_it_says() {
        // What the mouse is matched against has to be what was painted: a
        // pane recorded somewhere it was not drawn is a click that lands in
        // the wrong place, with nothing on screen to explain why.
        let (app, _) = frame(110, 30, |_| {});
        let slots: Vec<Slot> = app.areas.panes.iter().map(|(s, _)| *s).collect();
        assert_eq!(slots, app.layout.slots());
        for (_, rect) in &app.areas.panes {
            assert!(rect.width > 0 && rect.height > 0, "{rect:?}");
            assert!(app.panes_area.union(*rect) == app.panes_area, "{rect:?}");
        }
        assert_eq!(app.areas.dividers.len(), 1, "one border between the two");
    }

    #[test]
    fn a_zoomed_pane_is_the_only_one_drawn() {
        let (app, _) = frame(110, 30, |app| app.zoomed = true);
        assert_eq!(app.areas.panes.len(), 1);
        assert_eq!(app.areas.panes[0].0, app.focus);
        assert_eq!(app.areas.panes[0].1, app.panes_area);
        assert!(
            app.areas.dividers.is_empty(),
            "and there is no border to drag"
        );
    }
}
