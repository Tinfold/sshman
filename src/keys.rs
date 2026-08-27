//! What the keys do, and which keys do it.
//!
//! Every key that acts on what is on screen names an [`Action`], and a
//! [`Keymap`] says which chords name which. The scheme sshman has always used
//! is the default one, written down here in the same form a config file writes
//! an override, so changing one key is a line rather than a fork.
//!
//! Only the browsing layer is rebindable, which is the layer with the keys in
//! it. The modal ones — the arrows that move between panes while `Ctrl-]` has
//! the keyboard, `↵` to use a pane, `Esc` to back out of an overlay — are how
//! you get *around* sshman rather than what you do with it, and a rebound one
//! would be a way to lock yourself out of a box you had opened.

use std::collections::BTreeMap;
use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// One thing sshman can be asked to do.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Action {
    // Moving about a file list
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    Parent,
    Open,
    NextList,
    PreviousList,
    GoTo,
    Home,
    Filter,
    Hidden,
    Reload,
    Mirror,

    // Choosing and moving files
    Mark,
    MarkAll,
    Copy,
    Cut,
    Paste,
    Cancel,
    Delete,
    Rename,
    NewDirectory,
    Edit,
    EditWith,
    View,
    Archive,
    Extract,
    ListArchive,

    // Panes
    Shell,
    Split,
    SplitDown,
    NewList,
    EditorPane,
    ClosePane,
    Zoom,
    Even,
    Arrange,
    Command,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    BorderLeft,
    BorderRight,
    BorderUp,
    BorderDown,
    CopyText,
    PasteText,

    // The server, and the tabs
    Sudo,
    RemoteCommand,
    FullShell,
    Output,
    Containers,
    NameTab,
    Workspaces,
    Ports,
    Connect,
    LocalTab,
    CloseTab,
    NextTab,
    PreviousTab,
    MoveTabLeft,
    MoveTabRight,

    // sshman itself
    Settings,
    Help,
    Quit,
}

impl Action {
    /// Every one, in the order the chooser lists them: by what they are for,
    /// so a list of fifty reads as five short ones.
    pub const ALL: &'static [Action] = &[
        Action::Down,
        Action::Up,
        Action::PageDown,
        Action::PageUp,
        Action::Top,
        Action::Bottom,
        Action::Parent,
        Action::Open,
        Action::NextList,
        Action::PreviousList,
        Action::GoTo,
        Action::Home,
        Action::Filter,
        Action::Hidden,
        Action::Reload,
        Action::Mirror,
        Action::Mark,
        Action::MarkAll,
        Action::Copy,
        Action::Cut,
        Action::Paste,
        Action::Cancel,
        Action::Delete,
        Action::Rename,
        Action::NewDirectory,
        Action::Edit,
        Action::EditWith,
        Action::View,
        Action::Archive,
        Action::Extract,
        Action::ListArchive,
        Action::Shell,
        Action::Split,
        Action::SplitDown,
        Action::NewList,
        Action::EditorPane,
        Action::ClosePane,
        Action::Zoom,
        Action::Even,
        Action::Arrange,
        Action::Command,
        Action::FocusLeft,
        Action::FocusRight,
        Action::FocusUp,
        Action::FocusDown,
        Action::BorderLeft,
        Action::BorderRight,
        Action::BorderUp,
        Action::BorderDown,
        Action::CopyText,
        Action::PasteText,
        Action::Sudo,
        Action::RemoteCommand,
        Action::FullShell,
        Action::Output,
        Action::Containers,
        Action::NameTab,
        Action::Workspaces,
        Action::Ports,
        Action::Connect,
        Action::LocalTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PreviousTab,
        Action::MoveTabLeft,
        Action::MoveTabRight,
        Action::Settings,
        Action::Help,
        Action::Quit,
    ];

    /// What a config file calls it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::PageDown => "page-down",
            Self::PageUp => "page-up",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Parent => "parent",
            Self::Open => "open",
            Self::NextList => "next-list",
            Self::PreviousList => "previous-list",
            Self::GoTo => "go-to",
            Self::Home => "home",
            Self::Filter => "filter",
            Self::Hidden => "hidden",
            Self::Reload => "reload",
            Self::Mirror => "mirror",
            Self::Mark => "mark",
            Self::MarkAll => "mark-all",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Paste => "paste",
            Self::Cancel => "cancel",
            Self::Delete => "delete",
            Self::Rename => "rename",
            Self::NewDirectory => "new-directory",
            Self::Edit => "edit",
            Self::EditWith => "edit-with",
            Self::View => "view",
            Self::Archive => "archive",
            Self::Extract => "extract",
            Self::ListArchive => "list-archive",
            Self::Shell => "shell",
            Self::Split => "split",
            Self::SplitDown => "split-down",
            Self::NewList => "new-list",
            Self::EditorPane => "editor-pane",
            Self::ClosePane => "close-pane",
            Self::Zoom => "zoom",
            Self::Even => "even",
            Self::Arrange => "arrange",
            Self::Command => "command",
            Self::FocusLeft => "focus-left",
            Self::FocusRight => "focus-right",
            Self::FocusUp => "focus-up",
            Self::FocusDown => "focus-down",
            Self::BorderLeft => "border-left",
            Self::BorderRight => "border-right",
            Self::BorderUp => "border-up",
            Self::BorderDown => "border-down",
            Self::CopyText => "copy-text",
            Self::PasteText => "paste-text",
            Self::Sudo => "sudo",
            Self::RemoteCommand => "remote-command",
            Self::FullShell => "full-shell",
            Self::Output => "output",
            Self::Containers => "containers",
            Self::NameTab => "name-tab",
            Self::Workspaces => "workspaces",
            Self::Ports => "ports",
            Self::Connect => "connect",
            Self::LocalTab => "local-tab",
            Self::CloseTab => "close-tab",
            Self::NextTab => "next-tab",
            Self::PreviousTab => "previous-tab",
            Self::MoveTabLeft => "move-tab-left",
            Self::MoveTabRight => "move-tab-right",
            Self::Settings => "settings",
            Self::Help => "help",
            Self::Quit => "quit",
        }
    }

    /// One line saying what it does, for the list of them.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Down => "move down the list",
            Self::Up => "move up the list",
            Self::PageDown => "a screenful down",
            Self::PageUp => "a screenful up",
            Self::Top => "the first row",
            Self::Bottom => "the last row",
            Self::Parent => "the directory above this one",
            Self::Open => "enter a directory, or open a file",
            Self::NextList => "the next file list",
            Self::PreviousList => "the one before it",
            Self::GoTo => "type a path to jump to",
            Self::Home => "your home directory",
            Self::Filter => "narrow the list as you type",
            Self::Hidden => "show or hide dotfiles",
            Self::Reload => "read the directories again",
            Self::Mirror => "point the other list at this directory",
            Self::Mark => "mark the row under the cursor",
            Self::MarkAll => "mark everything, or clear the marks",
            Self::Copy => "copy to the other list, or pick up",
            Self::Cut => "pick up, to be moved",
            Self::Paste => "put down what was picked up",
            Self::Cancel => "clear the filter, the marks, the clipboard",
            Self::Delete => "delete what is marked",
            Self::Rename => "rename the row under the cursor",
            Self::NewDirectory => "make a directory",
            Self::Edit => "open in your editor",
            Self::EditWith => "open with a program you name",
            Self::View => "open in your pager",
            Self::Archive => "pack what is marked",
            Self::Extract => "unpack the archive under the cursor",
            Self::ListArchive => "show what an archive holds",
            Self::Shell => "a shell below this pane, or close the last",
            Self::Split => "a shell beside this pane",
            Self::SplitDown => "a shell below, closing nothing",
            Self::NewList => "another file list beside this pane",
            Self::EditorPane => "an editor pane beside this one, or close it",
            Self::ClosePane => "close the focused pane",
            Self::Zoom => "give the whole screen to this pane",
            Self::Even => "even the borders up again",
            Self::Arrange => "pick a ready-made arrangement",
            Self::Command => "hand the keyboard to sshman",
            Self::FocusLeft => "the pane to the left",
            Self::FocusRight => "the pane to the right",
            Self::FocusUp => "the pane above",
            Self::FocusDown => "the pane below",
            Self::BorderLeft => "move the nearest border left",
            Self::BorderRight => "move it right",
            Self::BorderUp => "move the nearest border up",
            Self::BorderDown => "move it down",
            Self::CopyText => "copy what is picked out in a shell",
            Self::PasteText => "put that text into a shell",
            Self::Sudo => "sudo mode, on this tab",
            Self::RemoteCommand => "run a command on the server",
            Self::FullShell => "hand the whole terminal to a shell",
            Self::Output => "the last command's output",
            Self::Containers => "open a container in a tab",
            Self::NameTab => "name the server on screen",
            Self::Workspaces => "saved sets of connections",
            Self::Ports => "forwarded ports",
            Self::Connect => "the connection screen",
            Self::LocalTab => "a tab on this machine",
            Self::CloseTab => "close the tab on screen",
            Self::NextTab => "the next tab",
            Self::PreviousTab => "the tab before it",
            Self::MoveTabLeft => "move this tab one place left",
            Self::MoveTabRight => "move it one place right",
            Self::Settings => "what sshman remembers between sessions",
            Self::Help => "the help",
            Self::Quit => "leave sshman",
        }
    }

    /// The heading this one is listed under.
    pub fn group(self) -> &'static str {
        match self {
            Self::Down
            | Self::Up
            | Self::PageDown
            | Self::PageUp
            | Self::Top
            | Self::Bottom
            | Self::Parent
            | Self::Open
            | Self::NextList
            | Self::PreviousList
            | Self::GoTo
            | Self::Home
            | Self::Filter
            | Self::Hidden
            | Self::Reload
            | Self::Mirror => "Looking around",
            Self::Mark
            | Self::MarkAll
            | Self::Copy
            | Self::Cut
            | Self::Paste
            | Self::Cancel
            | Self::Delete
            | Self::Rename
            | Self::NewDirectory
            | Self::Edit
            | Self::EditWith
            | Self::View
            | Self::Archive
            | Self::Extract
            | Self::ListArchive => "Files",
            Self::Shell
            | Self::Split
            | Self::SplitDown
            | Self::NewList
            | Self::EditorPane
            | Self::ClosePane
            | Self::Zoom
            | Self::Even
            | Self::Arrange
            | Self::Command
            | Self::FocusLeft
            | Self::FocusRight
            | Self::FocusUp
            | Self::FocusDown
            | Self::BorderLeft
            | Self::BorderRight
            | Self::BorderUp
            | Self::BorderDown
            | Self::CopyText
            | Self::PasteText => "Panes",
            Self::Sudo
            | Self::RemoteCommand
            | Self::FullShell
            | Self::Output
            | Self::Containers
            | Self::NameTab
            | Self::Workspaces
            | Self::Ports
            | Self::Connect
            | Self::LocalTab
            | Self::CloseTab
            | Self::NextTab
            | Self::PreviousTab
            | Self::MoveTabLeft
            | Self::MoveTabRight => "Servers and tabs",
            Self::Settings | Self::Help | Self::Quit => "sshman",
        }
    }

    /// The action of that name, or `None` for one we do not have.
    pub fn by_name(name: &str) -> Option<Action> {
        let wanted = name.trim().to_lowercase();
        Self::ALL.iter().copied().find(|a| a.name() == wanted)
    }
}

/// One keystroke, as a keymap talks about it.
///
/// A capital letter carries its own shift, so `S` and `Shift-s` are the same
/// chord written two ways and are kept the first way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        Self { code, mods }.tidied()
    }

    /// The chord a keystroke is.
    pub fn of(key: &KeyEvent) -> Self {
        Self::new(key.code, key.modifiers)
    }

    /// Shift is part of a letter rather than a modifier of it, so `Shift-s`
    /// is written down as the `S` a terminal actually sends. Nothing else in
    /// a chord is ours to keep.
    fn tidied(mut self) -> Self {
        self.mods &= KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        if let KeyCode::Char(c) = self.code {
            if self.mods.contains(KeyModifiers::SHIFT) && c.is_lowercase() {
                // Only letters: which character shift makes of a digit is the
                // keyboard's business, and the terminal has already decided.
                if let Some(capital) = c.to_uppercase().next() {
                    self.code = KeyCode::Char(capital);
                }
            }
            self.mods.remove(KeyModifiers::SHIFT);
        }
        self
    }

    /// `q`, `Ctrl-]`, `Alt-Shift-Left`, `F5`, `Space`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut mods = KeyModifiers::NONE;
        let mut rest = text;
        loop {
            // A lone `-` is the key itself, not a separator with nothing after.
            let Some((head, tail)) = rest.split_once(['-', '+']) else {
                break;
            };
            if tail.is_empty() {
                break;
            }
            match head.to_lowercase().as_str() {
                "ctrl" | "control" | "c" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" | "m" => mods |= KeyModifiers::ALT,
                "shift" | "s" => mods |= KeyModifiers::SHIFT,
                _ => break,
            }
            rest = tail;
        }
        let code = key_code(rest)?;
        Some(Self::new(code, mods))
    }
}

fn key_code(text: &str) -> Option<KeyCode> {
    let mut chars = text.chars();
    if let (Some(only), None) = (chars.next(), chars.next()) {
        return Some(KeyCode::Char(only));
    }
    let name = text.to_lowercase();
    if let Some(number) = name.strip_prefix('f')
        && let Ok(n) = number.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Some(KeyCode::F(n));
    }
    Some(match name.as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" | "ret" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" | "shift-tab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" | "bs" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        _ => return None,
    })
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "Ctrl-")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "Alt-")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "Shift-")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::BackTab => write!(f, "Shift-Tab"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Delete => write!(f, "Del"),
            KeyCode::Insert => write!(f, "Insert"),
            KeyCode::Up => write!(f, "↑"),
            KeyCode::Down => write!(f, "↓"),
            KeyCode::Left => write!(f, "←"),
            KeyCode::Right => write!(f, "→"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            KeyCode::PageUp => write!(f, "PgUp"),
            KeyCode::PageDown => write!(f, "PgDn"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// The scheme sshman is written down as using, as a config file would write
/// it: an action, and the chords that ask for it.
const DEFAULTS: &[(Action, &[&str])] = &[
    (Action::Down, &["down", "j"]),
    (Action::Up, &["up", "k"]),
    (Action::PageDown, &["pagedown"]),
    (Action::PageUp, &["pageup"]),
    (Action::Top, &["home", "g"]),
    (Action::Bottom, &["end", "G"]),
    (Action::Parent, &["left", "h"]),
    (Action::Open, &["enter", "right", "l"]),
    (Action::NextList, &["tab"]),
    (Action::PreviousList, &["backtab"]),
    (Action::GoTo, &["f"]),
    (Action::Home, &["~"]),
    (Action::Filter, &["/"]),
    (Action::Hidden, &["."]),
    (Action::Reload, &["R"]),
    (Action::Mirror, &["t"]),
    (Action::Mark, &["space"]),
    (Action::MarkAll, &["a"]),
    (Action::Copy, &["c", "F5"]),
    (Action::Cut, &["M"]),
    (Action::Paste, &["P"]),
    (Action::Cancel, &["esc"]),
    (Action::Delete, &["d", "del", "F8"]),
    (Action::Rename, &["r", "F2"]),
    (Action::NewDirectory, &["n", "F7"]),
    (Action::Edit, &["e", "F4"]),
    (Action::EditWith, &["E"]),
    (Action::View, &["v"]),
    (Action::Archive, &["z"]),
    (Action::Extract, &["x"]),
    (Action::ListArchive, &["X"]),
    (Action::Shell, &["S"]),
    (Action::Split, &["|"]),
    (Action::SplitDown, &["_"]),
    (Action::NewList, &["T"]),
    (Action::EditorPane, &["i"]),
    (Action::ClosePane, &["F9"]),
    (Action::Zoom, &["m", "F3"]),
    (Action::Even, &["="]),
    (Action::Arrange, &["A"]),
    (Action::Command, &["ctrl-]", "ctrl-5"]),
    (Action::FocusLeft, &["alt-left", "alt-h"]),
    (Action::FocusRight, &["alt-right", "alt-l"]),
    (Action::FocusUp, &["alt-up", "alt-k"]),
    (Action::FocusDown, &["alt-down", "alt-j"]),
    (Action::BorderLeft, &["alt-shift-left"]),
    (Action::BorderRight, &["alt-shift-right"]),
    (Action::BorderUp, &["alt-shift-up"]),
    (Action::BorderDown, &["alt-shift-down"]),
    (Action::CopyText, &["y"]),
    (Action::PasteText, &["Y"]),
    (Action::Sudo, &["s"]),
    (Action::RemoteCommand, &[":"]),
    (Action::FullShell, &["!"]),
    (Action::Output, &["o"]),
    (Action::Containers, &["D"]),
    (Action::NameTab, &["N"]),
    (Action::Workspaces, &["w"]),
    (Action::Ports, &["p"]),
    (Action::Connect, &["C"]),
    (Action::LocalTab, &["L"]),
    (Action::CloseTab, &["W"]),
    (Action::NextTab, &["ctrl-right"]),
    (Action::PreviousTab, &["ctrl-left"]),
    (Action::MoveTabLeft, &["ctrl-shift-left"]),
    (Action::MoveTabRight, &["ctrl-shift-right"]),
    (Action::Settings, &[","]),
    (Action::Help, &["?", "F1"]),
    (Action::Quit, &["q"]),
];

/// Which chords ask for what.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    binds: BTreeMap<Action, Vec<Chord>>,
    /// Bindings in a config file that could not be used, in the words to show
    /// someone wondering why their key does nothing.
    pub problems: Vec<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        let mut binds = BTreeMap::new();
        for (action, chords) in DEFAULTS {
            binds.insert(
                *action,
                chords.iter().filter_map(|c| Chord::parse(c)).collect(),
            );
        }
        Self {
            binds,
            problems: Vec::new(),
        }
    }
}

impl Keymap {
    /// The default scheme with a config file's changes over the top. An action
    /// the file names takes the chords it gives and no others; one it does not
    /// name keeps what it had.
    pub fn with(overrides: &BTreeMap<String, Vec<String>>) -> Self {
        let mut map = Self::default();
        let mut claimed: Vec<(Chord, Action)> = Vec::new();
        for (name, chords) in overrides {
            let Some(action) = Action::by_name(name) else {
                map.problems
                    .push(format!("{name}: sshman has nothing by that name"));
                continue;
            };
            let mut wanted = Vec::new();
            for text in chords {
                match Chord::parse(text) {
                    Some(chord) => wanted.push(chord),
                    None => map
                        .problems
                        .push(format!("{name}: {text:?} is not a key sshman can read")),
                }
            }
            for chord in &wanted {
                match claimed.iter().find(|(held, _)| held == chord) {
                    Some((_, first)) => map.problems.push(format!(
                        "{chord} is on both {} and {} — {} wins",
                        first.name(),
                        action.name(),
                        first.name()
                    )),
                    None => claimed.push((*chord, action)),
                }
            }
            map.binds.insert(action, wanted);
        }
        // A key a file gives to one thing is taken off whatever else had it,
        // the same as pressing it in the list does. Otherwise asking for
        // `"zoom": ["z"]` would leave z packing an archive, which is not what
        // anyone writing that line meant.
        for (chord, owner) in &claimed {
            for (action, chords) in map.binds.iter_mut() {
                if action != owner {
                    chords.retain(|held| held != chord);
                }
            }
        }
        map
    }

    /// A chord asking for two things at once only ever does the first, so say
    /// so rather than leaving someone to wonder. Only what the defaults
    /// themselves say; a file's overrides are checked as they are read.
    #[cfg(test)]
    fn report_clashes(&mut self) {
        let mut seen: Vec<(Chord, Action)> = Vec::new();
        let mut clashes = Vec::new();
        for (action, chords) in &self.binds {
            for chord in chords {
                match seen.iter().find(|(held, _)| held == chord) {
                    Some((_, first)) => clashes.push(format!(
                        "{chord} is on both {} and {} — {} wins",
                        first.name(),
                        action.name(),
                        first.name()
                    )),
                    None => seen.push((*chord, *action)),
                }
            }
        }
        self.problems.extend(clashes);
    }

    /// What a keystroke asks for, if anything.
    pub fn action(&self, key: &KeyEvent) -> Option<Action> {
        let chord = Chord::of(key);
        self.binds
            .iter()
            .find(|(_, chords)| chords.contains(&chord))
            .map(|(action, _)| *action)
    }

    /// The chords an action answers to.
    pub fn chords(&self, action: Action) -> &[Chord] {
        self.binds.get(&action).map_or(&[], Vec::as_slice)
    }

    /// The chord to show when there is room for one: the first, which is the
    /// one the scheme leads with.
    pub fn first(&self, action: Action) -> Option<Chord> {
        self.chords(action).first().copied()
    }

    /// How the chords for an action are written on screen: `m / F3`.
    pub fn shown(&self, action: Action) -> String {
        match self.chords(action) {
            [] => "—".into(),
            chords => chords
                .iter()
                .map(Chord::to_string)
                .collect::<Vec<_>>()
                .join(" / "),
        }
    }

    /// Make a chord *the* way to ask for an action, taking it off whatever
    /// else had it. Says what it was taken from.
    ///
    /// It replaces rather than adds, because "press the key you want" asks for
    /// one key. An action that answers to two — zoom to both `m` and `F3` — is
    /// something a config file can say, being a sentence rather than a
    /// keystroke.
    pub fn bind(&mut self, action: Action, chord: Chord) -> Option<Action> {
        let taken = self
            .binds
            .iter()
            .find(|(other, chords)| **other != action && chords.contains(&chord))
            .map(|(other, _)| *other);
        if let Some(taken) = taken {
            self.binds
                .entry(taken)
                .or_default()
                .retain(|held| *held != chord);
        }
        self.binds.insert(action, vec![chord]);
        taken
    }

    /// Put an action back to the chords it started with.
    pub fn reset(&mut self, action: Action) {
        let chords = DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, chords)| chords.iter().filter_map(|c| Chord::parse(c)).collect())
            .unwrap_or_default();
        self.binds.insert(action, chords);
    }

    /// Everything that differs from the scheme sshman ships, in the form a
    /// config file writes it. What is unchanged is left out, so a file says
    /// only what you have actually decided.
    pub fn overrides(&self) -> BTreeMap<String, Vec<String>> {
        let plain = Self::default();
        let mut out = BTreeMap::new();
        for action in Action::ALL {
            let mine = self.chords(*action);
            if mine != plain.chords(*action) {
                out.insert(
                    action.name().to_string(),
                    mine.iter().map(Chord::to_string).collect(),
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn the_default_scheme_is_the_one_sshman_has_always_had() {
        let map = Keymap::default();
        assert_eq!(
            map.action(&key(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
        assert_eq!(
            map.action(&key(KeyCode::Char('S'), KeyModifiers::SHIFT)),
            Some(Action::Shell),
            "a capital carries its own shift"
        );
        assert_eq!(
            map.action(&key(KeyCode::F(3), KeyModifiers::NONE)),
            Some(Action::Zoom)
        );
        assert_eq!(
            map.action(&key(KeyCode::Left, KeyModifiers::ALT)),
            Some(Action::FocusLeft)
        );
        assert_eq!(
            map.action(&key(KeyCode::Left, KeyModifiers::ALT | KeyModifiers::SHIFT)),
            Some(Action::BorderLeft),
            "shift moves the border rather than the keyboard"
        );
        assert_eq!(
            map.action(&key(KeyCode::Char('#'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn every_action_starts_with_a_key_and_a_name_of_its_own() {
        let map = Keymap::default();
        let mut names = Vec::new();
        for action in Action::ALL {
            assert!(
                !map.chords(*action).is_empty(),
                "{} has no key at all",
                action.name()
            );
            assert_eq!(Action::by_name(action.name()), Some(*action));
            assert!(!action.blurb().is_empty());
            assert!(
                names.iter().all(|n| *n != action.name()),
                "{}",
                action.name()
            );
            names.push(action.name());
        }
        assert_eq!(names.len(), Action::ALL.len());
    }

    #[test]
    fn no_two_actions_start_out_sharing_a_key() {
        let mut map = Keymap::default();
        map.report_clashes();
        assert!(map.problems.is_empty(), "{:?}", map.problems);
    }

    #[test]
    fn chords_are_written_the_way_people_write_them() {
        let round = |text: &str| Chord::parse(text).map(|c| c.to_string());
        assert_eq!(round("q").as_deref(), Some("q"));
        assert_eq!(round("F5").as_deref(), Some("F5"));
        assert_eq!(round("ctrl-]").as_deref(), Some("Ctrl-]"));
        assert_eq!(round("Alt-Left").as_deref(), Some("Alt-←"));
        assert_eq!(round("alt+shift+left").as_deref(), Some("Alt-Shift-←"));
        assert_eq!(round("space").as_deref(), Some("Space"));
        assert_eq!(round("esc").as_deref(), Some("Esc"));
        assert_eq!(round("-").as_deref(), Some("-"), "a key, not a separator");
        assert_eq!(
            Chord::parse("S"),
            Chord::parse("shift-s"),
            "two ways of writing the same keystroke"
        );
        assert_eq!(round("nonsense"), None);
        assert_eq!(round(""), None);
    }

    #[test]
    fn a_config_file_changes_only_what_it_names() {
        let mut overrides = BTreeMap::new();
        overrides.insert("quit".to_string(), vec!["Q".to_string()]);
        let map = Keymap::with(&overrides);

        assert!(map.problems.is_empty(), "{:?}", map.problems);
        assert_eq!(
            map.action(&key(KeyCode::Char('Q'), KeyModifiers::SHIFT)),
            Some(Action::Quit)
        );
        assert_eq!(
            map.action(&key(KeyCode::Char('q'), KeyModifiers::NONE)),
            None,
            "the old key is not left behind as well"
        );
        assert_eq!(
            map.action(&key(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(Action::Workspaces),
            "and everything it did not name is untouched"
        );
    }

    #[test]
    fn a_binding_that_cannot_be_used_is_complained_about() {
        let mut overrides = BTreeMap::new();
        overrides.insert("quit".into(), vec!["nonsense".into()]);
        overrides.insert("fly-to-the-moon".into(), vec!["F9".into()]);
        // Two of them asking for the same key, which is the one clash a file
        // can make that sshman cannot settle for it.
        overrides.insert("help".into(), vec!["w".into()]);
        overrides.insert("output".into(), vec!["w".into()]);
        let map = Keymap::with(&overrides);

        assert_eq!(map.problems.len(), 3, "{:?}", map.problems);
        assert!(map.problems.iter().any(|p| p.contains("nonsense")));
        assert!(map.problems.iter().any(|p| p.contains("fly-to-the-moon")));
        assert!(
            map.problems.iter().any(|p| p.contains("wins")),
            "a key on two of them is worth saying out loud: {:?}",
            map.problems
        );
    }

    #[test]
    fn a_key_a_file_asks_for_is_taken_off_whatever_had_it() {
        let mut overrides = BTreeMap::new();
        overrides.insert("zoom".to_string(), vec!["z".to_string()]);
        let map = Keymap::with(&overrides);

        assert!(map.problems.is_empty(), "{:?}", map.problems);
        assert_eq!(
            map.action(&key(KeyCode::Char('z'), KeyModifiers::NONE)),
            Some(Action::Zoom),
            "the line you wrote is the one that counts"
        );
        assert!(
            map.chords(Action::Archive).is_empty(),
            "and the thing that had it has it no longer"
        );
    }

    #[test]
    fn an_action_can_be_given_a_key_that_something_else_had() {
        let mut map = Keymap::default();
        let chord = Chord::parse("w").expect("a key");
        assert_eq!(map.bind(Action::Help, chord), Some(Action::Workspaces));
        assert_eq!(
            map.action(&key(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(Action::Help)
        );
        assert!(
            !map.chords(Action::Workspaces).contains(&chord),
            "it cannot be on both"
        );
        assert_eq!(
            map.chords(Action::Help),
            [chord],
            "and the key you pressed is the whole of it"
        );

        // And putting it back is one word.
        map.reset(Action::Help);
        map.reset(Action::Workspaces);
        assert_eq!(map, Keymap::default());
    }

    #[test]
    fn only_what_you_changed_is_written_down() {
        let mut map = Keymap::default();
        assert!(
            map.overrides().is_empty(),
            "an untouched scheme says nothing"
        );

        map.bind(Action::Quit, Chord::parse("Q").unwrap());
        let written = map.overrides();
        assert_eq!(written.len(), 1);
        assert_eq!(
            written["quit"],
            vec!["Q".to_string()],
            "the key you pressed, and not the one it replaced"
        );

        // And what is written down comes back as what it was.
        assert_eq!(Keymap::with(&written), map);
    }
}
