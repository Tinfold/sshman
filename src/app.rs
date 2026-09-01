//! Application state and key handling.
//!
//! Local filesystem work happens inline (it is fast); anything touching the
//! network is sent to the worker thread and answered asynchronously.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::backend::{BackendKind, Target};
use crate::config::{Config, Kind, Setting};
use crate::forward::{Forward, Spec as ForwardSpec};
use crate::history::History;
use crate::input::TextInput;
use crate::keys::{Action, Keymap};
pub use crate::layout::Side;
use crate::layout::{self, Areas, Dir, Divider, Layout, Slot, TermId, TreeId};
use crate::local;
use crate::shell::Shell;
use crate::sshconn::ConnectOpts;
use crate::theme::{self, Theme, Themes};
use crate::types::{FileEntry, rbasename, rjoin, rparent};
use crate::watch;
use crate::worker::{HostKeyIssue, Req, Resp};
use crate::workspace::{Item as WorkspaceItem, PaneDirs, Workspaces};
use ratatui::style::Color;

/// How long the pointer has to sit on a tab before sshman says what it is.
///
/// Long enough that crossing the row on the way somewhere else says nothing,
/// short enough that stopping to ask does not feel like waiting.
const TAB_TIP_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Connect,
    /// Choosing a container to open.
    Picker,
    /// Managing forwarded ports.
    Forwards,
    /// Choosing a saved set of connections.
    Workspaces,
    /// Changing what is kept in the config file.
    Settings,
    /// Choosing how this tab's panes are arranged.
    Arrange,
    /// Choosing the colours to draw in.
    Themes,
    /// Looking at which key asks for what, and changing one.
    Keys,
    Browse,
    Prompt,
    Confirm,
    Output,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Info,
    Good,
    Bad,
}

#[derive(Debug, Clone)]
pub enum PromptKind {
    Command,
    Mkdir(Slot),
    Rename(Slot, String),
    Filter(Slot),
    GoTo(Slot),
    SudoPassword,
    /// Ask which program to open `name` with, then run it in that pane.
    OpenWith(Slot, String),
    /// A name for the server on screen.
    NameTab,
    /// A name for the highlighted server in the recent list.
    NameSaved(usize),
    /// A name to save the current set of connections under.
    SaveWorkspace,
    /// A port to forward from the server on screen.
    AddForward,
    /// Name for a new archive holding the given entries.
    Archive(Slot, Vec<String>),
    /// Directory to unpack the named archive into.
    Extract(Slot, String),
    /// The editor to open files with from now on, and next time.
    SetEditor,
    /// The keystrokes that open a file in an editor pane.
    SetEditorOpen,
    /// The shell a shell pane starts from now on, and next time.
    SetShell,
}

/// One of the ready-made ways to arrange a tab's panes.
///
/// Anything these build can be built by hand — split a pane, close a pane,
/// drag the borders — so they are a starting point rather than a set of modes
/// sshman can be in. A workspace writes down whatever you end up with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arrangement {
    Sides,
    Single,
    TwoLists,
    Terminal,
    Editor,
}

impl Arrangement {
    pub const ALL: &'static [Arrangement] = &[
        Arrangement::Sides,
        Arrangement::Single,
        Arrangement::TwoLists,
        Arrangement::Terminal,
        Arrangement::Editor,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sides => "Side by side",
            Self::Single => "One pane",
            Self::TwoLists => "Two lists here",
            Self::Terminal => "Files and a terminal",
            Self::Editor => "Editor",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Self::Sides => "this machine and the server, the way sshman opens",
            Self::Single => "the pane you are on, filling the tab",
            Self::TwoLists => "two directories of the same machine, to copy between",
            Self::Terminal => "a narrow file list, and a terminal beside it",
            Self::Editor => "a file list, your editor beside it, a terminal underneath",
        }
    }

    /// What the status line says once it is arranged.
    fn done(self) -> &'static str {
        match self {
            Self::Sides => "side by side again",
            Self::Single => "one pane",
            Self::TwoLists => "two lists — f points one somewhere, c copies between them",
            Self::Terminal => "a terminal beside the files",
            Self::Editor => "editor pane open — clicking a file opens it there",
        }
    }
}

pub struct PromptState {
    pub kind: PromptKind,
    pub title: String,
    pub input: TextInput,
    pub hist_idx: Option<usize>,
    /// Where to go when this prompt closes — prompts opened from the
    /// connection screen must not dump you into the file panes.
    pub return_to: Mode,
}

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteLocal(Vec<PathBuf>),
    DeleteRemote(Vec<String>),
    /// Leave sshman. Everything open goes with it, which is why it is asked
    /// about rather than done.
    Quit,
    AcceptHostKey,
    /// Overwrite the recorded host key for this server. Guarded by a typed
    /// phrase, because the innocent explanation and an attack look identical
    /// from here.
    ReplaceHostKey,
    /// Open everything the last session had open. Asked on the way in, so
    /// that coming back does not depend on remembering a flag.
    RestoreSession,
}

pub struct ConfirmState {
    pub title: String,
    pub body: Vec<String>,
    pub action: ConfirmAction,
    pub danger: bool,
    /// When set, `y` is not enough: the user must type this word exactly.
    pub require_phrase: Option<String>,
    pub input: TextInput,
    /// Where to go when this closes. Nearly always the file panes, but a
    /// question asked from an overlay — "really quit?" over the connection
    /// screen — must not answer itself by dumping you somewhere else.
    pub return_to: Mode,
}

impl ConfirmState {
    fn simple(title: &str, body: Vec<String>, action: ConfirmAction, danger: bool) -> Self {
        Self {
            title: title.to_string(),
            body,
            action,
            danger,
            require_phrase: None,
            input: TextInput::default(),
            return_to: Mode::Browse,
        }
    }

    /// True when the dialog will accept a confirmation right now.
    pub fn satisfied(&self) -> bool {
        match &self.require_phrase {
            None => true,
            Some(word) => self.input.value.trim() == word,
        }
    }
}

/// A remote file that has been pulled to a temp path for editing. `sig` is the
/// (mtime, len) pair captured before the editor ran, so an unchanged file is
/// not needlessly written back.
#[derive(Debug, Clone)]
pub struct PendingEdit {
    pub temp: PathBuf,
    pub remote: String,
    pub sudo: bool,
    pub sig: Option<(i64, u64)>,
    /// Which tab the file came from, so it goes back to the same server even
    /// if you switched tabs while the editor was open.
    pub tab: usize,
}

/// A pane border being dragged with the mouse. The drag lives across events:
/// it starts on the press, follows every move, and ends on the release.
///
/// It holds the split it belongs to rather than the pane beside it: a border
/// is the one thing two panes have in common, and the same drag moves both.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Drag {
    /// Which split's border this is, as turns taken from the root.
    pub path: Vec<u8>,
    pub dir: Dir,
    /// The space that split divides, which is what the drag is measured
    /// against.
    pub area: Rect,
}

/// Files waiting to be pasted somewhere else on the side they came from.
///
/// The names are relative to `dir`, exactly as a pane holds them, so a paste
/// is one shell command run with `dir` as its working directory.
#[derive(Clone, Debug)]
pub struct Clip {
    pub side: Side,
    pub dir: String,
    pub names: Vec<String>,
    /// A move: the originals go away when it lands.
    pub cut: bool,
}

impl Clip {
    pub fn action(&self) -> crate::fileops::Action {
        if self.cut {
            crate::fileops::Action::Move
        } else {
            crate::fileops::Action::Copy
        }
    }
}

/// Something the main loop must do with the terminal released.
/// Which pane to re-read after an editor has been and gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refresh {
    Local,
    Remote,
    Neither,
}

pub enum UiAction {
    Editor {
        program: String,
        path: PathBuf,
        push_back: Option<PendingEdit>,
        refresh: Refresh,
    },
    Shell,
    Quit,
}

#[derive(Default)]
pub struct Pane {
    pub all: Vec<FileEntry>,
    pub view: Vec<FileEntry>,
    pub state: ListState,
    pub marked: HashSet<String>,
    pub filter: String,
    pub show_hidden: bool,
    pub error: Option<String>,
    pub loading: bool,
}

impl Pane {
    pub fn set_entries(&mut self, entries: Vec<FileEntry>) {
        let keep = self.selected_name();
        self.all = entries;
        // Marks referring to entries that no longer exist would silently
        // widen the next copy or delete, so drop them.
        let names: HashSet<&String> = self.all.iter().map(|e| &e.name).collect();
        self.marked.retain(|m| names.contains(m));
        self.error = None;
        self.loading = false;
        self.refresh_view(keep.as_deref());
    }

    /// Fold in a listing nobody asked for.
    ///
    /// Two things separate this from [`set_entries`](Self::set_entries), both
    /// following from the user not having asked and quite possibly being in
    /// the middle of something: a listing that turns out to be identical is
    /// dropped rather than rebuilt, and when the file under the cursor has
    /// gone the cursor stays on that row rather than springing back to the
    /// top of the list.
    pub fn absorb_entries(&mut self, entries: Vec<FileEntry>) {
        if watch::signature(&entries) == watch::signature(&self.all) {
            return;
        }
        let row = self.state.selected();
        let was = self.selected_name();
        self.set_entries(entries);
        if let (Some(name), Some(row)) = (was, row)
            && !self.view.iter().any(|e| e.name == name)
        {
            self.select_index(row);
        }
    }

    pub fn refresh_view(&mut self, keep: Option<&str>) {
        let needle = self.filter.to_lowercase();
        self.view = self
            .all
            .iter()
            .filter(|e| self.show_hidden || !e.is_hidden())
            .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
            .cloned()
            .collect();

        let idx = keep
            .and_then(|name| self.view.iter().position(|e| e.name == name))
            .or_else(|| (!self.view.is_empty()).then_some(0));
        self.state.select(idx);
    }

    pub fn selected(&self) -> Option<&FileEntry> {
        self.state.selected().and_then(|i| self.view.get(i))
    }

    pub fn selected_name(&self) -> Option<String> {
        self.selected().map(|e| e.name.clone())
    }

    /// Names to act on: everything marked, or the cursor row if nothing is.
    pub fn targets(&self) -> Vec<String> {
        if !self.marked.is_empty() {
            return self
                .all
                .iter()
                .filter(|e| self.marked.contains(&e.name))
                .map(|e| e.name.clone())
                .collect();
        }
        self.selected_name().into_iter().collect()
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.view.is_empty() {
            self.state.select(None);
            return;
        }
        let last = self.view.len() as isize - 1;
        let cur = self.state.selected().unwrap_or(0) as isize;
        self.state
            .select(Some((cur + delta).clamp(0, last) as usize));
    }

    pub fn select_index(&mut self, i: usize) {
        if !self.view.is_empty() {
            self.state.select(Some(i.min(self.view.len() - 1)));
        }
    }

    pub fn toggle_mark(&mut self) {
        if let Some(name) = self.selected_name()
            && !self.marked.remove(&name)
        {
            self.marked.insert(name);
        }
    }

    pub fn on_dir_change(&mut self) {
        self.marked.clear();
        self.filter.clear();
        self.error = None;
        self.loading = true;
        self.state.select(None);
    }
}

/// One file list on this machine, and the directory it is showing.
///
/// The first — [`layout::MAIN`] — is the one everything that means "this
/// machine's directory" is about: where a workspace says the local pane was,
/// where `L` opens a tab. The rest are yours to point wherever you like.
pub struct LocalTree {
    pub id: TreeId,
    pub pane: Pane,
    pub cwd: PathBuf,
    /// How the directory itself looked when it was last read, so a change to
    /// what is in it can be noticed without reading it again. See
    /// [`crate::watch`].
    stamp: Option<watch::Stamp>,
}

impl LocalTree {
    fn new(id: TreeId, cwd: PathBuf) -> Self {
        Self {
            id,
            pane: Pane::default(),
            cwd,
            stamp: None,
        }
    }

    /// Read the directory again.
    ///
    /// `quiet` marks a re-read nobody asked for, which is gentler with the
    /// cursor — see [`Pane::absorb_entries`].
    fn load(&mut self, quiet: bool) {
        // Stamped before the read rather than after, so a change landing
        // between the two is seen next time round instead of being taken as
        // already accounted for.
        self.stamp = watch::stamp(&self.cwd);
        match local::list_dir(&self.cwd) {
            Ok(entries) if quiet => self.pane.absorb_entries(entries),
            Ok(entries) => self.pane.set_entries(entries),
            Err(e) => {
                self.pane.all.clear();
                self.pane.view.clear();
                self.pane.state.select(None);
                self.pane.loading = false;
                self.pane.error = Some(e.to_string());
            }
        }
    }
}

/// The same on the far end of a connection, where a listing is a round trip
/// rather than a read.
pub struct RemoteTree {
    pub id: TreeId,
    pub pane: Pane,
    pub cwd: String,
    /// Guards against a stale listing overwriting a newer one.
    seq: u64,
    /// Name to put the cursor on once the next listing arrives.
    pending_select: Option<String>,
    /// A "has this changed?" question is out with the worker. Only one at a
    /// time, so a slow link cannot build a queue of them.
    polling: bool,
}

impl RemoteTree {
    fn new(id: TreeId, cwd: String) -> Self {
        Self {
            id,
            pane: Pane::default(),
            cwd,
            seq: 0,
            pending_select: None,
            polling: false,
        }
    }
}

/// What the mouse is resting on, when it is resting on something a click
/// would act upon. Only one thing can be under the pointer at a time, which
/// is why this is one value rather than a flag per kind of target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hover {
    /// A row of a file list, by the pane it is in and its place in the view.
    Row(Slot, usize),
    /// One piece of the path in a pane's title, by the directory it names.
    Crumb(Slot, String),
}

/// A menu of what can be done to what was right-clicked.
///
/// It belongs to the pane it was opened over rather than to the one with the
/// keyboard, because a right click is a sentence about the thing under the
/// pointer: opening the menu focuses that pane, and choosing a row runs
/// against it.
pub struct Menu {
    pub at: Slot,
    pub items: Vec<MenuItem>,
    /// Which row is lit. Never a rule — see [`Menu::step`].
    pub cursor: usize,
    /// Where the click was, which is where the box wants its top-left corner.
    /// The drawing may put it elsewhere to keep it on the screen.
    pub anchor: (u16, u16),
    /// Where the box actually landed, written down by the drawing so a click
    /// can be matched to a row. Rebuilt every frame, like every other hit box.
    pub area: Rect,
    /// The first row shown, for a menu taller than the terminal it is in.
    ///
    /// Also written by the drawing, and for the same reason: how much of the
    /// menu fits is a fact about the screen, and the screen is what the
    /// drawing knows about. A twenty-row menu in an eighteen-row terminal has
    /// to give somewhere, and rows you cannot reach are worse than rows you
    /// have to walk to.
    pub scroll: usize,
}

/// One row of a [`Menu`].
pub enum MenuItem {
    /// Something to do, and what to call it here.
    ///
    /// The label is the menu's rather than the action's: [`Action::blurb`] is
    /// a sentence, written for a list of fifty where each one has to explain
    /// itself, and a menu row wants a verb.
    Do(&'static str, Action),
    /// A line between groups. Never lit, never chosen.
    Rule,
}

impl Menu {
    /// Move the light, stepping over the rules and stopping at the ends.
    ///
    /// Stopping rather than wrapping: a menu of nine rows is short enough to
    /// see all of at once, and a highlight that jumps from the bottom to the
    /// top is a highlight you have to go looking for.
    pub fn step(&mut self, by: isize) {
        let mut at = self.cursor as isize;
        loop {
            at += by;
            if at < 0 {
                return;
            }
            match self.items.get(at as usize) {
                None => return,
                Some(MenuItem::Rule) => continue,
                Some(MenuItem::Do(..)) => {
                    self.cursor = at as usize;
                    return;
                }
            }
        }
    }

    /// What choosing the lit row would do.
    pub fn chosen(&self) -> Option<Action> {
        match self.items.get(self.cursor) {
            Some(MenuItem::Do(_, action)) => Some(*action),
            _ => None,
        }
    }

    /// The row a point on the screen is on, if it is on one at all. A rule is
    /// not a row you can be on.
    pub fn row_at(&self, column: u16, row: u16) -> Option<usize> {
        if column < self.area.x
            || column >= self.area.right()
            || row <= self.area.y
            || row + 1 >= self.area.bottom()
        {
            return None;
        }
        let at = self.scroll + (row - self.area.y - 1) as usize;
        matches!(self.items.get(at), Some(MenuItem::Do(..))).then_some(at)
    }

    /// Whether a point is inside the box at all, border included. A click on
    /// the frame has not left the menu.
    pub fn hits(&self, column: u16, row: u16) -> bool {
        self.area
            .contains(ratatui::layout::Position::new(column, row))
    }
}

/// One embedded terminal, and what it is there for.
pub struct Term {
    pub id: TermId,
    pub shell: Shell,
    /// Set on a terminal that opens files rather than one you type in: the
    /// keystrokes that make the program inside it open a path, with `{file}`
    /// standing in for the name. An empty one means the pane is at a shell
    /// prompt, so the editor is run as a command instead.
    pub opens: Option<String>,
}

impl Term {
    /// Is this the pane the file lists send files to?
    pub fn is_editor(&self) -> bool {
        self.opens.is_some()
    }
}

pub struct ConnectForm {
    pub host: TextInput,
    pub port: TextInput,
    pub user: TextInput,
    pub key: TextInput,
    pub password: TextInput,
    /// What you want to call this server. Optional.
    pub name: TextInput,
    pub field: usize,
    /// Install our public key on the server after connecting, so the next
    /// login needs no password.
    pub install_key: bool,
    pub error: Option<String>,
    /// A short actionable follow-up, shown on its own line so a long error
    /// message cannot crowd it out.
    pub hint: Option<String>,
    pub connecting: bool,
}

impl ConnectForm {
    /// Six text fields plus the checkbox.
    pub const FIELDS: usize = 7;
    /// Index of the "install my key" checkbox — the one row that is not text.
    pub const CHECKBOX: usize = 6;
    /// Index of the password field, which errors send the cursor to.
    pub const PASSWORD: usize = 4;

    pub fn new(opts: &ConnectOpts) -> Self {
        Self {
            host: TextInput::new(opts.host.clone()),
            port: TextInput::new(opts.port.to_string()),
            user: TextInput::new(opts.user.clone()),
            key: TextInput::new(
                opts.key_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            ),
            password: {
                let mut t = TextInput::masked();
                if let Some(p) = &opts.password {
                    t.set(p.clone());
                }
                t
            },
            name: TextInput::default(),
            field: 0,
            install_key: false,
            error: None,
            hint: None,
            connecting: false,
        }
    }

    /// The focused text field, or `None` when the checkbox has focus.
    pub fn current(&mut self) -> Option<&mut TextInput> {
        match self.field {
            0 => Some(&mut self.host),
            1 => Some(&mut self.port),
            2 => Some(&mut self.user),
            3 => Some(&mut self.key),
            4 => Some(&mut self.password),
            5 => Some(&mut self.name),
            _ => None,
        }
    }
}

pub struct ConnInfo {
    pub user: String,
    pub host: String,
    pub port: u16,
    pub home: String,
}

/// The container chooser.
pub struct PickerState {
    pub title: String,
    pub items: Vec<crate::docker::Container>,
    pub selected: usize,
    /// Which machine these came from: `None` for this one, or the server whose
    /// containers they are.
    pub via: Option<ConnectOpts>,
    /// The runtime that listed them, reused to open the one you choose.
    pub runtime: String,
}

/// Result of a local command run off the UI thread — packing an archive, say,
/// which can take long enough that doing it inline would freeze the screen.
pub struct LocalOutcome {
    pub message: String,
    pub failed: bool,
    /// Output to show in the viewer, when the command was one whose output is
    /// the point (listing an archive, say) rather than a side effect.
    pub output: Option<(String, String)>,
    /// Containers found on this machine, when that is what was asked for.
    pub containers: Option<(String, Vec<crate::docker::Container>)>,
}

/// Which worker a message came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RespSource {
    /// A connection attempt that has not become a tab yet, by its id.
    Pending(u64),
    Tab(usize),
}

/// State of the connection behind the remote pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkState {
    Live,
    /// Dropped; the worker is trying to rebuild it.
    Reconnecting,
    /// Dropped and given up on.
    Lost,
}

/// What the connection screen's keys act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConnectFocus {
    /// The list of servers connected to before.
    Recent,
    /// The host/port/user/key/password fields.
    Form,
}

/// One connected server: its own SSH worker, file pane, shell and sudo state.
/// Tabs are fully independent — a transfer on one does not touch another.
pub struct RemoteTab {
    /// What this tab is connected to — a server or a container.
    pub target: Target,
    pub kind: BackendKind,
    /// A name you gave this server, shown instead of its address.
    pub name: Option<String>,
    pub conn: ConnInfo,
    pub link: LinkState,
    pub sudo: bool,
    /// This tab's file lists, the first of which is [`layout::MAIN`].
    pub trees: Vec<RemoteTree>,
    /// This tab's own terminals. They go when it does — a pty on a
    /// connection that has been closed has nothing on the other end.
    pub terms: Vec<Term>,
    /// Terminals a workspace said were editor panes, waiting to be opened.
    /// Emptied as they are, since after that each terminal says for itself.
    wants_editor: Vec<TermId>,
    /// Where a workspace said this tab's panes were pointed. Read as each
    /// pane is opened; a pane it says nothing about opens in the tab's own
    /// directory, which is what every tab did before this was written down.
    wants_dir: PaneDirs,
    /// Which pane had the keyboard when you last left this tab, so coming
    /// back puts you where you were. It matters most zoomed, where the
    /// focused pane is the only one you can see at all.
    pub focus: Slot,
    /// How this tab's panes were last arranged. A server you set up wide is
    /// still wide when you come back to it, and a workspace writes this down
    /// along with everything else about the tab.
    pub layout: Layout,
    /// Whether this tab was showing one pane full screen. Its own, the same
    /// way its arrangement is: a server you are watching a log on stays
    /// zoomed while the tab beside it stays split.
    pub zoomed: bool,
    /// What this tab's worker is doing, if anything. See
    /// [`PendingConnect::task`] for why it lives here.
    pub task: Option<String>,
    /// Ports carried from this server to this machine.
    pub forwards: Vec<Forward>,
    tx: Sender<Req>,
    pub rx: Receiver<Resp>,
}

impl RemoteTab {
    /// This tab's directory: the one its first file list is showing, which is
    /// what a shell opens in and what a workspace writes down.
    pub fn cwd(&self) -> &str {
        self.tree(layout::MAIN)
            .map(|t| t.cwd.as_str())
            .unwrap_or("")
    }

    pub fn tree(&self, id: TreeId) -> Option<&RemoteTree> {
        self.trees.iter().find(|t| t.id == id)
    }

    pub fn tree_mut(&mut self, id: TreeId) -> Option<&mut RemoteTree> {
        self.trees.iter_mut().find(|t| t.id == id)
    }

    /// Short label for the tab bar: your name for it when there is one.
    pub fn title(&self) -> String {
        if let Some(name) = &self.name
            && !name.trim().is_empty()
        {
            return name.clone();
        }
        self.address()
    }

    /// The address, regardless of any name.
    pub fn address(&self) -> String {
        match self.kind {
            // A container's host field already reads `name` or `name@server`,
            // and this machine's is its hostname — neither wants a user@ in
            // front of it.
            BackendKind::Container | BackendKind::Local => self.conn.host.clone(),
            BackendKind::Ssh if self.conn.port == 22 => {
                format!("{}@{}", self.conn.user, self.conn.host)
            }
            BackendKind::Ssh => {
                format!("{}@{}:{}", self.conn.user, self.conn.host, self.conn.port)
            }
        }
    }

    pub fn is_container(&self) -> bool {
        self.kind == BackendKind::Container
    }

    /// A tab pointed at the machine sshman is running on.
    pub fn is_local(&self) -> bool {
        self.kind == BackendKind::Local
    }

    /// The SSH details behind this tab, if it has any.
    pub fn ssh_opts(&self) -> Option<&ConnectOpts> {
        self.target.ssh_opts()
    }
}

/// A connection being attempted. It has a worker but is not a tab yet — only
/// servers we actually reach become tabs, so a failed attempt leaves no
/// wreckage behind.
pub struct PendingConnect {
    /// Identifies this attempt among several running at once — opening a
    /// workspace starts one per connection.
    pub id: u64,
    /// True when this came from the connection form, so its errors belong on
    /// the form rather than in the status line.
    pub from_form: bool,
    pub target: Target,
    /// A name typed on the form for this server, if any.
    pub name: Option<String>,
    /// Ports to start forwarding once this connection is up, from a workspace.
    pub forwards: Vec<String>,
    /// The panes for the tab this becomes: a workspace's, or the ones on
    /// screen, so opening a server does not throw away the split you just set
    /// up.
    pub layout: Layout,
    /// Which of those panes' terminals were opening files, from a workspace.
    pub editors: Vec<TermId>,
    /// Where a workspace said those panes were pointed.
    pub dirs: PaneDirs,
    /// What its worker is doing, if anything. Held here rather than in a list
    /// of its own so that giving up on an attempt takes the label with it: a
    /// worker whose replies nobody is reading any more cannot leave "…" in
    /// the title bar for the rest of the session.
    pub task: Option<String>,
    tx: Sender<Req>,
    pub rx: Receiver<Resp>,
    initial_dir: Option<String>,
    /// Install the public key once this connection succeeds.
    install_key: bool,
}

/// A connection a workspace could not make on its own, because the only way
/// in is a password and a workspace does not keep one.
///
/// Everything else the workspace asked for is kept with it, so that supplying
/// the password picks the connection up exactly where it was left rather than
/// opening a bare tab at a home directory.
pub struct Waiting {
    pub label: String,
    pub opts: ConnectOpts,
    path: Option<String>,
    forwards: Vec<String>,
    layout: Layout,
    editors: Vec<TermId>,
    dirs: PaneDirs,
}

pub struct App {
    pub mode: Mode,
    /// The pane with the keyboard.
    pub focus: Slot,
    /// Where every pane and every border between them was last drawn,
    /// recorded by the renderer so the mouse can be matched to a pane exactly
    /// instead of by working the arrangement out a second time.
    pub areas: Areas,
    /// The area inside a terminal pane's border: what the program running in
    /// it thinks the screen is, and so what a mouse event is measured against.
    pub term_inner: Vec<(Slot, Rect)>,
    /// Where the new-tab button was drawn, when it was.
    pub new_tab_button: Option<Rect>,
    /// Where each pane's buttons were drawn, and which pane they belong to.
    pub zoom_buttons: Vec<(Rect, Slot)>,
    pub close_buttons: Vec<(Rect, Slot)>,
    /// Where each pane's name was drawn, which is what the mouse takes hold
    /// of to move the pane.
    pub pane_titles: Vec<(Rect, Slot)>,
    /// Screen span of each tab chip, recorded by the renderer so a click can
    /// be matched to a tab.
    pub tab_spans: Vec<(u16, u16, usize)>,
    /// The span of the `✕` inside each chip, which closes that tab rather
    /// than switching to it. Recorded separately so the rest of the chip
    /// stays a plain "go here".
    pub tab_close_buttons: Vec<(u16, u16, usize)>,
    pub tab_bar_row: Option<u16>,
    /// A tab the pointer has come to rest on, and since when.
    ///
    /// A chip is only as wide as the row can afford — with eight tabs open
    /// that is six characters, and `web01.prod…` and `web01.stag…` are the
    /// same six. Resting on one says the whole name. The clock is here rather
    /// than in the drawing because the drawing happens sixteen times a second
    /// whether anything moved or not, and "how long has the pointer been
    /// there" is a fact about the pointer.
    pub tab_rest: Option<(usize, Instant)>,
    /// A tab being dragged along the row by its chip. Holds where it is now,
    /// which moves as the pointer passes over its neighbours.
    pub tab_drag: Option<usize>,
    /// Rows the scrollable overlays were last drawn with, so scrolling can
    /// stop at the end of the content instead of running into blank space.
    pub output_view_height: u16,
    pub help_view_height: u16,
    /// The arrangement on screen. Each tab keeps its own copy, and this is
    /// the one being drawn — the tab you are looking at hands its panes over
    /// here, and takes them back when you leave it.
    pub layout: Layout,
    /// Only the focused pane is drawn, filling the space both sides usually
    /// share. It follows the focus rather than remembering a pane of its own:
    /// whatever you are looking at is what is zoomed.
    pub zoomed: bool,
    /// The block all the panes are drawn in.
    pub panes_area: Rect,
    pub drag: Option<Drag>,
    /// What `c` or `M` picked up, waiting for `P`.
    pub clip: Option<Clip>,
    /// The file lists on this machine. They outlive any one tab, the same way
    /// the local terminals do, and the first is [`layout::MAIN`].
    pub local: Vec<LocalTree>,
    /// Terminals on this machine. They outlive any one tab, the same way the
    /// local file list does, so a tab you come back to still has the shell you
    /// left running in it.
    pub local_terms: Vec<Term>,
    /// The same waiting list as a tab's, for this machine's terminals.
    wants_editor: Vec<TermId>,
    /// And the same for where this machine's panes were pointed. They are
    /// shared between the tabs, so this belongs to the session rather than to
    /// any one of them.
    wants_dirs: PaneDirs,
    /// The number the next terminal gets, on either machine. One counter for
    /// all of them, so a number never means two different panes.
    next_term_id: TermId,
    /// The same for file lists. [`layout::MAIN`] is never handed out: every
    /// machine has one from the start.
    next_tree_id: TreeId,
    /// The file list the keyboard was on before this one, which is what `c`
    /// copies to and `t` points when there are more than two.
    previous: Option<Slot>,
    /// One per connected server; `active` selects the one on screen.
    pub tabs: Vec<RemoteTab>,
    pub active: usize,
    /// Connections in flight, not yet tabs. A workspace opens several at once.
    pub pending: Vec<PendingConnect>,
    next_pending_id: u64,
    /// Returned by the pane accessors when no server is connected, so callers
    /// do not each have to handle "there is no remote side yet".
    empty_pane: Pane,

    pub status: String,
    pub status_level: Level,
    pub progress: Option<(String, u64, u64)>,
    /// Work started on this machine rather than by a worker — packing an
    /// archive, looking for containers. Balanced by construction: the thread
    /// always reports back.
    pub local_tasks: Vec<String>,

    pub form: ConnectForm,
    pub history: History,
    pub connect_focus: ConnectFocus,
    pub history_sel: usize,
    pub prompt: Option<PromptState>,
    pub confirm: Option<ConfirmState>,
    pub picker: Option<PickerState>,
    pub workspaces: Workspaces,
    /// The session sshman was in last time, read once at startup and left
    /// alone after that. What is being written down as you work replaces the
    /// file, not this: "restore the previous session" has to keep meaning the
    /// one before this, however long this one runs.
    pub previous_session: Option<crate::workspace::Workspace>,
    pub workspace_sel: usize,
    pub settings_sel: usize,
    pub arrangement_sel: usize,
    pub action_sel: usize,
    /// The action waiting for a key to be pressed for it.
    pub rebinding: Option<Action>,
    pub theme_sel: usize,
    /// The theme that was on when the chooser opened, so `Esc` can put it
    /// back: the list draws in the theme under the cursor as you move through
    /// it, which means the screen has already changed by the time you decide
    /// against it.
    theme_before: Option<(String, Theme)>,
    pub forward_sel: usize,
    /// Connections from a workspace that could not be made without a
    /// password. Kept so the user is told, and so `C` can offer them.
    pub needs_password: Vec<Waiting>,
    pub output: Vec<String>,
    pub output_title: String,
    pub output_scroll: u16,
    pub help_scroll: u16,

    /// The keyboard is sshman's rather than the focused pane's.
    ///
    /// `Ctrl-]` turns it on from anywhere, and `↵` hands the keyboard back to
    /// whatever pane you have moved to. It is what makes every sshman key
    /// reachable from inside a shell, where otherwise they all belong to the
    /// program running in it.
    pub commanding: bool,
    /// The pane the keyboard came from, so `Esc` can put it back.
    command_from: Option<Slot>,
    /// The focused pane has been picked up: the arrows shove it about the
    /// arrangement rather than moving the keyboard through it.
    pub carrying: bool,
    /// A pane being dragged by its name with the mouse, and the pane it would
    /// change places with if the button came up now.
    pub moving: Option<Slot>,
    pub move_over: Option<Slot>,
    /// Text picked out of a terminal pane, kept so it can be put back into
    /// one even where the system clipboard cannot be reached.
    pub copied: Option<String>,
    /// Text to hand the terminal sshman is running in, so it reaches the
    /// system clipboard. Drained by the main loop, which owns the terminal.
    pub clipboard_out: Option<String>,
    /// The pane a selection is being dragged out in. The drag lives across
    /// events, and follows the mouse out of the pane it started in.
    pub selecting: Option<Slot>,
    /// What the pointer is over, drawn lit so you can see what a click would
    /// land on before you make it. Nothing to do with the cursor: hovering
    /// never moves it.
    pub hover: Option<Hover>,
    /// The menu a right click opened, while one is open. See [`Menu`].
    pub menu: Option<Menu>,
    /// Where each piece of each pane's path is drawn, so a click on one can
    /// be turned back into the directory it names. Rebuilt every frame.
    pub crumbs: Vec<(Rect, Slot, String)>,
    /// The last click on a file list, so a second one on the same row soon
    /// enough can be told from two separate clicks. See [`App::click_row`].
    last_click: Option<(Slot, usize, Instant)>,

    pub cmd_history: Vec<String>,
    /// Settings that outlive the session. The editor in use is derived from
    /// this and the environment, and kept beside it so the hot path is a
    /// clone rather than a lookup.
    pub config: Config,
    pub editor: String,
    /// The colours in use, from the config. Kept here so drawing is a read
    /// rather than a lookup and a fallback on every span.
    pub theme: Theme,
    /// What those colours are called, which is the part that is written down.
    pub theme_name: String,
    /// Every theme there is: the ones sshman ships and any found on disk.
    pub themes: Themes,
    /// Which keys ask for what: the scheme sshman ships, with whatever the
    /// config file changed over the top.
    pub keymap: Keymap,
    pub pager: String,
    /// The details the connection screen is working with — the active tab's
    /// once connected, or what the user is typing for a new one.
    pub opts: ConnectOpts,
    pub host_key_issue: Option<HostKeyIssue>,
    pub pending_action: Option<UiAction>,
    pub should_quit: bool,

    /// Where the first connection should start, from `--remote-path`.
    initial_remote: Option<String>,
    local_tx: Sender<LocalOutcome>,
    local_rx: Receiver<LocalOutcome>,

    /// When the file lists were last looked at for changes nobody told us
    /// about: the directories on this machine, the full read of one of them
    /// that catches a file changing in place, and the question put to the
    /// server. See [`crate::watch`].
    watched_local: Instant,
    watched_deep: Instant,
    watched_remote: Instant,

    /// When the session was last written down, and what was written. Kept so
    /// that a session nobody is touching costs one comparison a second
    /// rather than a file write.
    cached_session: Instant,
    session_written: Option<String>,
}

impl App {
    pub fn new(
        opts: ConnectOpts,
        local_start: PathBuf,
        remote_start: Option<String>,
        auto_connect: bool,
    ) -> Self {
        let config = Config::load();
        let editor = config.editor();
        // Every pane that opens a shell asks for this, wherever it is opened
        // from, so it is settled once here rather than passed down.
        crate::shell::set_default_shell(config.shell().map(str::to_string));
        let mut themes = Themes::load();
        let theme_name = config.theme_name().unwrap_or(theme::DEFAULT).to_string();
        let theme = themes.by_name(&theme_name).unwrap_or_else(|| {
            // Asked for by name in the config, but there is no file of that
            // name here. Say so where the themes are listed rather than
            // quietly drawing in something else.
            if config.theme_name().is_some() {
                themes.problems.push(format!(
                    "no theme called {theme_name:?} — drawing in {}",
                    theme::DEFAULT
                ));
            }
            Theme::default()
        });
        let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
        let history = History::load();
        let (local_tx, local_rx) = std::sync::mpsc::channel();

        let mut app = Self {
            mode: if auto_connect {
                Mode::Browse
            } else {
                Mode::Connect
            },
            focus: Slot::files(Side::Local),
            areas: Areas::default(),
            term_inner: Vec::new(),
            new_tab_button: None,
            zoom_buttons: Vec::new(),
            close_buttons: Vec::new(),
            pane_titles: Vec::new(),
            tab_spans: Vec::new(),
            tab_close_buttons: Vec::new(),
            tab_bar_row: None,
            tab_rest: None,
            tab_drag: None,
            output_view_height: 1,
            help_view_height: 1,
            layout: Layout::default(),
            zoomed: false,
            panes_area: Rect::ZERO,
            drag: None,
            hover: None,
            menu: None,
            crumbs: Vec::new(),
            last_click: None,
            clip: None,
            local: vec![LocalTree::new(layout::MAIN, local_start)],
            local_terms: Vec::new(),
            wants_editor: Vec::new(),
            wants_dirs: PaneDirs::default(),
            next_term_id: 0,
            next_tree_id: layout::MAIN,
            previous: None,
            tabs: Vec::new(),
            active: 0,
            pending: Vec::new(),
            next_pending_id: 0,
            empty_pane: Pane::default(),
            status: "Connecting…".into(),
            status_level: Level::Info,
            progress: None,
            local_tasks: Vec::new(),
            // Start on the recent list when there is one — that is the whole
            // point of remembering servers.
            connect_focus: if history.is_empty() {
                ConnectFocus::Form
            } else {
                ConnectFocus::Recent
            },
            history_sel: 0,
            history,
            form: ConnectForm::new(&opts),
            prompt: None,
            confirm: None,
            picker: None,
            workspaces: Workspaces::load(),
            previous_session: crate::workspace::Session::load(),
            workspace_sel: 0,
            settings_sel: 0,
            arrangement_sel: 0,
            action_sel: 0,
            rebinding: None,
            theme_sel: 0,
            theme_before: None,
            forward_sel: 0,
            needs_password: Vec::new(),
            output: Vec::new(),
            output_title: String::new(),
            output_scroll: 0,
            help_scroll: 0,
            commanding: false,
            command_from: None,
            carrying: false,
            moving: None,
            move_over: None,
            copied: None,
            clipboard_out: None,
            selecting: None,
            cmd_history: Vec::new(),
            keymap: Keymap::with(&config.keys),
            config,
            editor,
            theme,
            theme_name,
            themes,
            pager,
            opts,
            host_key_issue: None,
            pending_action: None,
            should_quit: false,
            initial_remote: remote_start,
            local_tx,
            local_rx,
            watched_local: Instant::now(),
            watched_deep: Instant::now(),
            watched_remote: Instant::now(),
            cached_session: Instant::now(),
            session_written: None,
        };
        app.reload_local();
        if auto_connect {
            app.start_connect();
        } else {
            app.status = "Pick a server or fill in the details, then press Enter.".into();
        }
        app
    }

    // ---- helpers -----------------------------------------------------------

    /// The tab on screen, if any server is connected.
    pub fn tab(&self) -> Option<&RemoteTab> {
        self.tabs.get(self.active)
    }

    /// Send a request to the active tab's worker. Silently ignored when
    /// nothing is connected — every caller already reports that case.
    fn send(&self, req: Req) {
        if let Some(tab) = self.tab() {
            let _ = tab.tx.send(req);
        }
    }

    pub fn connected(&self) -> bool {
        self.tab().is_some()
    }

    /// A tab pointed at the machine sshman is running on.
    pub fn on_local_tab(&self) -> bool {
        self.tab().is_some_and(|t| t.is_local())
    }

    /// Which machine the keyboard is on.
    pub fn host(&self) -> Side {
        self.focus.host()
    }

    /// Is the keyboard in a terminal rather than a file list?
    pub fn in_term(&self) -> bool {
        self.focus.is_term()
    }

    /// Would zooming change what is on screen? It would not when there is
    /// only the one pane, which already has everything.
    pub fn zoom_has_anything_to_hide(&self) -> bool {
        self.layout.panes() > 1
    }

    /// Is the other machine's file list on screen to copy to?
    ///
    /// Three ways for it not to be: zoom, a tab on this machine, which has no
    /// far side to put beside its own, and an arrangement that simply has no
    /// room for it. In all three the keys that act across the middle have
    /// nothing to act on, and `c` picks files up instead.
    pub fn other_side_on_screen(&self) -> bool {
        !self.zoomed && self.layout.contains(Slot::files(self.host().other()))
    }

    /// Keep the keyboard on a pane that is actually there.
    ///
    /// Every arrangement is tidied here first: panes with nothing behind them
    /// any more are dropped, and terminals nothing is showing are shut down.
    /// Doing it in one place means no operation on the tree has to remember
    /// to clean up after itself.
    pub fn settle_focus(&mut self) {
        self.ensure_trees();
        self.ensure_terms();
        if self.prune_layout() {
            // The tab's copy has to follow, or adopting it again would bring
            // the pane that has gone back with it.
            self.stash_layout();
        }
        self.drop_unused_panes();
        if !self.layout.contains(self.focus) {
            self.focus = self
                .layout
                .find(Slot::is_files)
                .unwrap_or_else(|| self.layout.first());
        }
    }

    /// Drop panes whose terminal has gone — shut by hand, or taken down with
    /// the tab that owned it.
    /// Give every file list the arrangement names something to show.
    ///
    /// A workspace remembers the panes a tab had, and a tab opens with the
    /// arrangement that was on screen; either can name a list that has not
    /// been made yet. Making it here means an arrangement is never quietly
    /// reduced to fit what happens to exist.
    fn ensure_trees(&mut self) {
        for slot in self.layout.slots() {
            let Slot::Files { host, id } = slot else {
                continue;
            };
            // Ids come from one counter, so a restored arrangement must not
            // hand out a number that is already in use.
            self.next_tree_id = self.next_tree_id.max(id);
            match host {
                Side::Local => {
                    if !self.local.iter().any(|t| t.id == id) {
                        // Where a workspace said this list was looking, as
                        // long as it is still a directory, and otherwise
                        // wherever this machine's first list is.
                        let cwd = self
                            .wants_dirs
                            .tree(id)
                            .map(crate::local::expand)
                            .filter(|path| path.is_dir())
                            .unwrap_or_else(|| self.local_cwd());
                        self.local.push(LocalTree::new(id, cwd));
                        self.reload_local();
                    }
                }
                Side::Remote => {
                    let Some(tab) = self.tabs.get_mut(self.active) else {
                        continue;
                    };
                    if tab.tree(id).is_some() {
                        continue;
                    }
                    let cwd = tab
                        .wants_dir
                        .tree(id)
                        .map(str::to_string)
                        .unwrap_or_else(|| tab.cwd().to_string());
                    tab.trees.push(RemoteTree::new(id, cwd.clone()));
                    if !cwd.is_empty() {
                        self.goto_remote(slot, cwd);
                    }
                }
            }
        }
    }

    /// Start a terminal for every terminal pane the arrangement names and
    /// nothing is behind.
    ///
    /// A workspace remembers the panes a tab had, terminals among them, and
    /// this is what makes them come back. Nothing of the session can: a pty
    /// whose process has ended is gone. What comes back is a fresh shell in
    /// the same place and the same directory — the part that was yours to
    /// arrange, rather than the shell's to remember.
    fn ensure_terms(&mut self) {
        for (index, slot) in self.terminal_panes() {
            let Slot::Term { host, id } = slot else {
                continue;
            };
            if self.term_of(index, slot).is_some() {
                continue;
            }
            // Where it goes: what it was showing when it was written down,
            // and otherwise wherever its machine is now. There is nowhere to
            // open a remote one until its tab has said where it is.
            let here = self.term_start_dir(index, host, id);
            if host == Side::Remote && here.is_empty() {
                continue;
            }
            // Ids come from one counter, so a restored pane must not be handed
            // a number that is already spoken for.
            self.next_term_id = self.next_term_id.max(id);

            let wanted = match host {
                Side::Local => &mut self.wants_editor,
                Side::Remote => match self.tabs.get_mut(index) {
                    Some(tab) => &mut tab.wants_editor,
                    None => continue,
                },
            };
            let was_editor = match wanted.iter().position(|w| *w == id) {
                Some(at) => {
                    wanted.remove(at);
                    true
                }
                None => false,
            };
            let (run, opens) = match was_editor {
                true => {
                    let program = self.editor.clone();
                    let opens = self.config.editor_open(&program);
                    (Some(program), Some(opens))
                }
                false => (None, None),
            };
            self.open_term_in(index, host, id, here, run, opens);
        }
    }

    /// Every terminal pane any tab is arranged around, with the tab it
    /// belongs to.
    ///
    /// Not only the tab on screen: a workspace that opened four servers with
    /// a shell on each meant four shells. One that starts only when you first
    /// look at its tab is a shell whose first minute you missed — and on a
    /// slow link, a tab you have to sit and wait in before it is any use.
    fn terminal_panes(&self) -> Vec<(usize, Slot)> {
        let mut panes: Vec<(usize, Slot)> = self
            .layout
            .slots()
            .into_iter()
            .filter(|slot| slot.is_term())
            .map(|slot| (self.active, slot))
            .collect();
        for (index, tab) in self.tabs.iter().enumerate() {
            if index == self.active {
                continue;
            }
            panes.extend(
                tab.layout
                    .slots()
                    .into_iter()
                    .filter(|slot| slot.is_term())
                    .map(|slot| (index, slot)),
            );
        }
        panes
    }

    /// The terminal behind a pane on a named tab. This machine's are shared,
    /// so for those the tab does not come into it.
    fn term_of(&self, tab: usize, slot: Slot) -> Option<&Term> {
        let Slot::Term { host, id } = slot else {
            return None;
        };
        match host {
            Side::Local => self.local_terms.iter().find(|t| t.id == id),
            Side::Remote => self.tabs.get(tab)?.terms.iter().find(|t| t.id == id),
        }
    }

    /// The directory a terminal should open in: the one it was in when a
    /// workspace wrote it down, and otherwise wherever its machine is now.
    fn term_start_dir(&self, tab: usize, host: Side, id: TermId) -> String {
        let saved = match host {
            Side::Local => self
                .wants_dirs
                .shell(id)
                // A directory that has since gone would stop the shell
                // starting at all, which is a poor trade for a `cd`.
                .filter(|path| crate::local::expand(path).is_dir()),
            Side::Remote => self.tabs.get(tab).and_then(|t| t.wants_dir.shell(id)),
        };
        if let Some(dir) = saved {
            return dir.to_string();
        }
        match host {
            Side::Local => self.local_cwd().display().to_string(),
            Side::Remote => self
                .tabs
                .get(tab)
                .map(|t| t.cwd().to_string())
                .unwrap_or_default(),
        }
    }

    /// Says whether it took anything away.
    fn prune_layout(&mut self) -> bool {
        let before = self.layout.panes();
        let terms: Vec<TermId> = self.local_terms.iter().map(|t| t.id).collect();
        let trees: Vec<TreeId> = self.local.iter().map(|t| t.id).collect();
        let waiting = self.remote_cwd().is_empty();
        let (tab_terms, tab_trees): (Vec<TermId>, Vec<TreeId>) = match self.tab() {
            Some(tab) => (
                tab.terms.iter().map(|t| t.id).collect(),
                tab.trees.iter().map(|t| t.id).collect(),
            ),
            None => (Vec::new(), Vec::new()),
        };
        self.layout.retain(|slot| match slot {
            Slot::Files {
                host: Side::Local,
                id,
            } => trees.contains(&id),
            // The far side's own list stays even with nothing connected: it
            // is the pane that says so.
            Slot::Files {
                host: Side::Remote,
                id,
            } => id == layout::MAIN || tab_trees.contains(&id),
            Slot::Term {
                host: Side::Local,
                id,
            } => terms.contains(&id),
            // A terminal pane on a tab that has not said where it is yet is
            // waiting to be opened, not left over from one that has gone: it
            // is what a workspace restores, and closing it up here would take
            // it away a frame before it could arrive.
            Slot::Term {
                host: Side::Remote,
                id,
            } => tab_terms.contains(&id) || waiting,
        });
        self.layout.panes() != before
    }

    /// Let go of what no arrangement is showing any more. Dropping a terminal
    /// tells its thread to end the session; dropping a file list forgets the
    /// directory it was in.
    ///
    /// A pane on this machine can be shown by any tab, so it only goes once
    /// none of them is showing it. A tab's own panes are its own. The first
    /// file list on either machine is never let go of: it is what "this
    /// machine's directory" means, whether or not a pane is showing it.
    fn drop_unused_panes(&mut self) {
        let mut shown = self.layout.slots();
        for tab in &self.tabs {
            shown.extend(
                tab.layout
                    .slots()
                    .into_iter()
                    .filter(|s| s.host() == Side::Local),
            );
        }
        self.local_terms
            .retain(|t| shown.contains(&Slot::term(Side::Local, t.id)));
        self.local
            .retain(|t| t.id == layout::MAIN || shown.contains(&Slot::tree(Side::Local, t.id)));

        let mine: Vec<Slot> = self.layout.slots();
        let active = self.active;
        if let Some(tab) = self.tabs.get_mut(active) {
            tab.terms
                .retain(|t| mine.contains(&Slot::term(Side::Remote, t.id)));
            tab.trees
                .retain(|t| t.id == layout::MAIN || mine.contains(&Slot::tree(Side::Remote, t.id)));
        }
    }

    /// Which of a machine's file lists this is, counted in the order they are
    /// drawn. `None` when that machine has only the one, and so nothing to
    /// tell apart.
    pub fn tree_number(&self, slot: Slot) -> Option<usize> {
        if !slot.is_files() {
            return None;
        }
        let among: Vec<Slot> = self
            .layout
            .slots()
            .into_iter()
            .filter(|s| s.is_files() && s.host() == slot.host())
            .collect();
        if among.len() < 2 {
            return None;
        }
        among.iter().position(|s| *s == slot).map(|at| at + 1)
    }

    /// What a pane is called, for the status line.
    fn pane_name(&self, slot: Slot) -> String {
        // A tab on this machine has no far side, so calling its pane "remote"
        // beside the machine it is running on would be a lie.
        let whose = match slot.host() {
            Side::Remote if self.on_local_tab() => "this tab",
            host => side_name(host),
        };
        match slot {
            Slot::Files { .. } => match self.tree_number(slot) {
                Some(n) => format!("{whose} files {n}"),
                None => format!("{whose} files"),
            },
            Slot::Term { .. } if self.term(slot).is_some_and(Term::is_editor) => {
                format!("{whose} editor")
            }
            Slot::Term { .. } => format!("{whose} shell"),
        }
    }

    /// Is sudo mode on for the tab on screen?
    pub fn sudo(&self) -> bool {
        self.tab().is_some_and(|t| t.sudo)
    }

    /// The tab's own directory — its first file list's. What `:` runs a
    /// command in, and what a shell opens on.
    pub fn remote_cwd(&self) -> String {
        self.tab().map(|t| t.cwd().to_string()).unwrap_or_default()
    }

    pub fn set_status(&mut self, msg: impl Into<String>, level: Level) {
        self.status = msg.into();
        self.status_level = level;
    }

    /// The file list a pane is showing.
    ///
    /// A pane that is not a file list, or one whose machine is not connected,
    /// gets the scratch pane: reads find it empty and edits go nowhere, which
    /// is exactly right when there is nothing to navigate.
    pub fn pane(&self, slot: Slot) -> &Pane {
        match slot {
            Slot::Files {
                host: Side::Local,
                id,
            } => match self.local.iter().find(|t| t.id == id) {
                Some(tree) => &tree.pane,
                None => &self.empty_pane,
            },
            Slot::Files {
                host: Side::Remote,
                id,
            } => match self.tab().and_then(|tab| tab.tree(id)) {
                Some(tree) => &tree.pane,
                None => &self.empty_pane,
            },
            Slot::Term { .. } => &self.empty_pane,
        }
    }

    pub fn pane_mut(&mut self, slot: Slot) -> &mut Pane {
        match slot {
            Slot::Files {
                host: Side::Local,
                id,
            } => match self.local.iter_mut().find(|t| t.id == id) {
                Some(tree) => &mut tree.pane,
                None => &mut self.empty_pane,
            },
            Slot::Files {
                host: Side::Remote,
                id,
            } => match self
                .tabs
                .get_mut(self.active)
                .and_then(|tab| tab.tree_mut(id))
            {
                Some(tree) => &mut tree.pane,
                None => &mut self.empty_pane,
            },
            Slot::Term { .. } => &mut self.empty_pane,
        }
    }

    /// The directory a file list is showing, for building paths with. Empty
    /// when there is nothing behind that pane.
    pub fn dir_of(&self, slot: Slot) -> String {
        match slot {
            Slot::Files {
                host: Side::Local,
                id,
            } => self
                .local
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.cwd.display().to_string())
                .unwrap_or_default(),
            Slot::Files {
                host: Side::Remote,
                id,
            } => self
                .tab()
                .and_then(|tab| tab.tree(id))
                .map(|t| t.cwd.clone())
                .unwrap_or_default(),
            Slot::Term { .. } => String::new(),
        }
    }

    /// The same, as a pane's header shows it.
    pub fn path_of(&self, slot: Slot) -> String {
        match self.dir_of(slot) {
            dir if dir.is_empty() => "—".to_string(),
            dir => dir,
        }
    }

    /// A machine's own directory: the one its first file list is showing.
    fn main_dir(&self, host: Side) -> String {
        match host {
            Side::Local => self.local_cwd().display().to_string(),
            Side::Remote => self.remote_cwd(),
        }
    }

    /// This machine's own directory: the one the first local file list is
    /// showing. What a shell opens in, and what a workspace writes down.
    pub fn local_cwd(&self) -> PathBuf {
        self.local
            .iter()
            .find(|t| t.id == layout::MAIN)
            .map(|t| t.cwd.clone())
            .unwrap_or_default()
    }

    /// The file list `c` copies to and `t` points: the other one when there
    /// are two, and otherwise the one you were on before this.
    ///
    /// Zoomed there is no other pane on screen, so there is nothing to copy
    /// across to and `c` picks files up instead.
    pub fn target(&self) -> Option<Slot> {
        if self.zoomed {
            return None;
        }
        let here = self.focus;
        let others: Vec<Slot> = self
            .layout
            .slots()
            .into_iter()
            .filter(|s| s.is_files() && *s != here)
            .collect();
        match others.len() {
            0 => None,
            1 => Some(others[0]),
            _ => self
                .previous
                .filter(|p| others.contains(p))
                .or_else(|| others.first().copied()),
        }
    }

    /// Move the keyboard to a pane, remembering where it came from.
    pub fn focus_pane(&mut self, slot: Slot) {
        if slot == self.focus {
            return;
        }
        if self.focus.is_files() {
            self.previous = Some(self.focus);
        }
        self.focus = slot;
    }

    // ---- loading -----------------------------------------------------------

    /// Re-read every file list on this machine. Two of them may well be
    /// looking at the same directory, and a copy that lands in one has to
    /// show up in the other.
    pub fn reload_local(&mut self) {
        for tree in &mut self.local {
            tree.load(false);
        }
    }

    pub fn reload_remote(&mut self) {
        self.reload_tab(self.active);
    }

    // ---- keeping up with changes nobody told us about ---------------------

    /// Look for changes to the directories on screen that sshman had no hand
    /// in: a build dropping files into one, the shell in the pane below
    /// deleting some, someone else's `mv` on the server.
    ///
    /// Called from the main loop between frames, and cheap to call that
    /// often: each side decides for itself whether enough time has passed to
    /// be worth another look. [`crate::watch`] has the reasoning behind the
    /// two sides being watched in quite different ways.
    pub fn watch_dirs(&mut self) {
        if !self.config.watching() {
            return;
        }
        self.watch_here();
        self.watch_there();
    }

    /// The directories on this machine, by their own timestamps.
    fn watch_here(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.watched_local) < watch::LOCAL {
            return;
        }
        self.watched_local = now;
        // Now and then a list short enough to afford it is read in full as
        // well, since a file being written to moves nothing about the
        // directory holding it.
        let deep = now.duration_since(self.watched_deep) >= watch::LOCAL_DEEP;
        if deep {
            self.watched_deep = now;
        }

        for tree in &mut self.local {
            let moved = watch::stamp(&tree.cwd) != tree.stamp;
            let worth_reading = deep && tree.pane.all.len() <= watch::DEEP_LIMIT;
            if moved || worth_reading {
                tree.load(true);
            }
        }
    }

    /// The directories on the server you are looking at, by asking it.
    ///
    /// Only the tab on screen, and only one question per pane at a time: the
    /// point is a list that keeps up, not a connection kept busy.
    fn watch_there(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.watched_remote) < watch::REMOTE {
            return;
        }
        self.watched_remote = now;

        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        if tab.link != LinkState::Live {
            return;
        }
        let sudo = tab.sudo;
        let mut reqs = Vec::new();
        for tree in &mut tab.trees {
            if tree.cwd.is_empty() || tree.pane.loading || tree.polling {
                continue;
            }
            tree.polling = true;
            reqs.push(Req::Poll {
                path: tree.cwd.clone(),
                sudo,
                tree: tree.id,
                seq: tree.seq,
                sig: watch::signature(&tree.pane.all),
            });
        }
        for req in reqs {
            let _ = tab.tx.send(req);
        }
    }

    fn goto_local(&mut self, slot: Slot, path: PathBuf) {
        if !path.is_dir() {
            self.set_status(format!("not a directory: {}", path.display()), Level::Bad);
            return;
        }
        let Slot::Files {
            host: Side::Local,
            id,
        } = slot
        else {
            return;
        };
        // Canonicalise so `..` chains stay tidy, but keep the original if the
        // path cannot be resolved (a dangling symlink, say).
        let cwd = std::fs::canonicalize(&path).unwrap_or(path);
        if let Some(tree) = self.local.iter_mut().find(|t| t.id == id) {
            tree.cwd = cwd;
            tree.pane.on_dir_change();
        }
        self.reload_local();
    }

    fn goto_remote(&mut self, slot: Slot, path: String) {
        let Slot::Files {
            host: Side::Remote,
            id,
        } = slot
        else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(self.active) else {
            self.set_status("not connected", Level::Bad);
            return;
        };
        let sudo = tab.sudo;
        let Some(tree) = tab.tree_mut(id) else {
            return;
        };
        tree.pane.on_dir_change();
        tree.seq += 1;
        let req = Req::GoTo {
            path,
            sudo,
            tree: id,
            seq: tree.seq,
        };
        let _ = tab.tx.send(req);
    }

    /// Start a connection on a worker of its own. It only becomes a tab once
    /// the server actually answers.
    pub fn start_connect(&mut self) {
        self.form.connecting = true;
        self.form.error = None;
        self.form.hint = None;
        self.set_status(
            format!(
                "Connecting to {}@{}:{}…",
                self.opts.user, self.opts.host, self.opts.port
            ),
            Level::Info,
        );

        // Only one form-driven attempt at a time: two Enters should not leave
        // a stray worker connecting in the background.
        self.drop_form_attempts();
        self.connect_to_inner(Target::Ssh(self.opts.clone()), String::new(), true);
    }

    fn drop_form_attempts(&mut self) {
        self.pending.retain(|p| {
            if p.from_form {
                let _ = p.tx.send(Req::Quit);
            }
            !p.from_form
        });
    }

    /// Start connecting to any target on a worker of its own. It only becomes
    /// a tab once the far end actually answers.
    pub fn connect_to(&mut self, target: Target, status: String) {
        self.connect_to_inner(target, status, false)
    }

    /// What a workspace was asking for when it hit a password prompt, if this
    /// is the connection it was asking about.
    fn waiting_for(&self, target: &Target) -> Option<&Waiting> {
        let opts = target.ssh_opts()?;
        self.needs_password.iter().find(|w| {
            w.opts.user == opts.user && w.opts.host == opts.host && w.opts.port == opts.port
        })
    }

    fn connect_to_inner(&mut self, target: Target, status: String, from_form: bool) {
        if !status.is_empty() {
            self.set_status(status, Level::Info);
        }

        // A workspace parks a connection it could not make without a password.
        // Typing one in is that same connection carrying on, so it opens where
        // the workspace said, with the panes and the ports it asked for.
        let asked_for = self.waiting_for(&target).map(|w| {
            (
                w.path.clone(),
                w.forwards.clone(),
                w.layout.clone(),
                w.editors.clone(),
                w.dirs.clone(),
            )
        });

        self.next_pending_id += 1;
        let (tx, rx) = crate::worker::spawn();
        let _ = tx.send(Req::Connect(Box::new(target.clone())));
        self.pending.push(PendingConnect {
            id: self.next_pending_id,
            from_form,
            target,
            // Only a form-driven attempt takes the name from the form. A
            // container picked from the chooser, or a workspace member, would
            // otherwise inherit whatever name was last typed there.
            name: from_form
                .then(|| self.form.name.value.trim().to_string())
                .filter(|n| !n.is_empty()),
            forwards: Vec::new(),
            editors: Vec::new(),
            dirs: PaneDirs::default(),
            // The arrangement on screen, so opening a server does not throw
            // away the split you just set up — minus the panes belonging to
            // the tab you were on, whose terminals are not this one's to show.
            layout: {
                let mut layout = self.layout.clone();
                layout.retain(|slot| !(slot.is_term() && slot.host() == Side::Remote));
                // A tab with nowhere to show the server it just reached would
                // be no tab at all.
                match layout.find(|s| s.is_files() && s.host() == Side::Remote) {
                    Some(_) => layout,
                    None => Layout::default(),
                }
            },
            task: None,
            tx,
            rx,
            // Copied, not taken: an attempt can be retried — after accepting a
            // host key, say — and the retry must carry the same intent. Both
            // are cleared once a connection actually succeeds.
            initial_dir: self.initial_remote.clone(),
            install_key: self.form.install_key,
        });

        if let Some((path, forwards, layout, editors, dirs)) = asked_for
            && let Some(pending) = self.pending.last_mut()
        {
            pending.initial_dir = path.or(pending.initial_dir.take());
            pending.forwards = forwards;
            pending.layout = layout;
            pending.editors = editors;
            pending.dirs = dirs;
        }
    }

    // ---- worker messages ---------------------------------------------------

    /// Collect everything the workers have finished since the last frame:
    /// the connection being attempted, then each tab in turn.
    pub fn drain_workers(&mut self) {
        let mut inbox: Vec<(RespSource, Resp)> = Vec::new();
        for pending in &self.pending {
            while let Ok(resp) = pending.rx.try_recv() {
                inbox.push((RespSource::Pending(pending.id), resp));
            }
        }
        for index in 0..self.tabs.len() {
            while let Ok(resp) = self.tabs[index].rx.try_recv() {
                inbox.push((RespSource::Tab(index), resp));
            }
        }
        while let Ok(outcome) = self.local_rx.try_recv() {
            self.local_tasks.pop();
            if let Some((title, text)) = outcome.output {
                self.show_output(title, text);
            }
            if let Some((runtime, list)) = outcome.containers {
                self.open_picker(
                    list,
                    None,
                    runtime.clone(),
                    format!("Containers on this machine ({runtime})"),
                );
            }
            self.set_status(
                outcome.message,
                if outcome.failed {
                    Level::Bad
                } else {
                    Level::Good
                },
            );
            self.reload_local();
        }
        // Collected first, then handled: handling can add or remove tabs, and
        // the borrow of the channels has to be over before that happens.
        for (source, resp) in inbox {
            self.handle_resp(source, resp);
        }
    }

    /// Run a shell command on this machine off the UI thread, reporting the
    /// outcome when it finishes.
    fn spawn_local_command(&mut self, label: String, cmd: String, success: String) {
        self.spawn_local_inner(label, cmd, success, None)
    }

    /// As above, but the command's stdout is shown in the output viewer.
    fn spawn_local_output(&mut self, label: String, cmd: String, title: String) {
        self.spawn_local_inner(label, cmd, title.clone(), Some(title))
    }

    fn spawn_local_inner(
        &mut self,
        label: String,
        cmd: String,
        success: String,
        show_title: Option<String>,
    ) {
        self.local_tasks.push(label);
        let tx = self.local_tx.clone();
        std::thread::Builder::new()
            .name("local-task".into())
            .spawn(move || {
                // `/bin/sh` and not `$SHELL`: what arrives here is one of
                // sshman's own POSIX strings — a `tar` line, or the paste
                // guard with its `for … do … done` in it. See
                // [`crate::local::POSIX_SHELL`].
                let outcome = match std::process::Command::new(crate::local::POSIX_SHELL)
                    .arg("-c")
                    .arg(&cmd)
                    .output()
                {
                    Ok(out) if out.status.success() => {
                        // Warnings on stderr from something that still
                        // succeeded — tar coping with a file that changed
                        // while it was read, say — are worth passing on.
                        let warning = String::from_utf8_lossy(&out.stderr);
                        let mut message = success;
                        if let Some(line) = warning.lines().find(|l| !l.trim().is_empty()) {
                            message.push_str(&format!(" — {}", line.trim()));
                        }
                        LocalOutcome {
                            message,
                            failed: false,
                            output: show_title
                                .map(|t| (t, String::from_utf8_lossy(&out.stdout).to_string())),
                            containers: None,
                        }
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let detail = stderr
                            .lines()
                            .map(str::trim)
                            .find(|l| !l.is_empty())
                            .unwrap_or("no output")
                            .to_string();
                        LocalOutcome {
                            message: format!("failed: {detail}"),
                            failed: true,
                            output: None,
                            containers: None,
                        }
                    }
                    Err(e) => LocalOutcome {
                        message: format!("could not run the command: {e}"),
                        failed: true,
                        output: None,
                        containers: None,
                    },
                };
                let _ = tx.send(outcome);
            })
            .expect("spawn local task thread");
    }

    /// Fill the output viewer and bring it up.
    fn show_output(&mut self, title: String, text: String) {
        self.stash_output(title, text);
        self.mode = Mode::Output;
    }

    /// Fill the viewer without stealing the screen — for a background tab.
    fn stash_output(&mut self, title: String, text: String) {
        self.output_title = title;
        self.output = if text.trim().is_empty() {
            vec!["(no output)".into()]
        } else {
            text.lines().map(|l| l.to_string()).collect()
        };
        self.output_scroll = 0;
    }

    /// Show what an archive holds, without unpacking it.
    fn list_archive(&mut self, at: Slot) {
        let Some(entry) = self.pane(at).selected().cloned() else {
            return;
        };
        if !crate::archive::is_archive(&entry.name) {
            self.set_status(format!("{} is not a tar archive", entry.name), Level::Bad);
            return;
        }
        let dir = self.path_of(at);
        let cmd = crate::archive::list_command(&dir, &entry.name);
        match at.host() {
            Side::Local => self.spawn_local_output(
                format!("reading {}…", entry.name),
                cmd,
                format!("contents of {}", entry.name),
            ),
            Side::Remote => self.send(Req::Exec {
                cmd: crate::archive::list_command(".", &entry.name),
                cwd: dir,
                sudo: self.sudo(),
            }),
        }
    }

    // ---- archives -----------------------------------------------------------

    /// Ask for a name, then pack whatever is marked (or under the cursor).
    fn start_archive(&mut self, at: Slot) {
        let names = self.pane(at).targets();
        if names.is_empty() {
            self.set_status("nothing selected to pack", Level::Info);
            return;
        }
        let dir = self.path_of(at);
        let dir_name = match at.host() {
            Side::Local => PathBuf::from(&dir)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            Side::Remote => rbasename(&dir),
        };
        let suggestion = crate::archive::suggested_name(&names, &dir_name);
        self.open_prompt(
            PromptKind::Archive(at, names.clone()),
            format!("Pack {} item(s) from {dir} into", names.len()),
            suggestion,
        );
    }

    /// Ask where to unpack the archive under the cursor.
    fn start_extract(&mut self, at: Slot) {
        let Some(entry) = self.pane(at).selected().cloned() else {
            return;
        };
        if entry.is_dir_like() {
            self.set_status("that is a directory, not an archive", Level::Info);
            return;
        }
        if !crate::archive::is_archive(&entry.name) {
            self.set_status(
                format!(
                    "{} is not a tar archive (.tar, .tar.gz, .tgz, .tar.bz2, .tar.xz)",
                    entry.name
                ),
                Level::Bad,
            );
            return;
        }
        let dest = crate::archive::stem_of(&entry.name);
        self.open_prompt(
            PromptKind::Extract(at, entry.name.clone()),
            format!("Unpack {} into directory", entry.name),
            dest,
        );
    }

    fn run_archive(&mut self, at: Slot, names: Vec<String>, archive: String) {
        let dir = self.path_of(at);
        match at.host() {
            Side::Local => {
                let cmd = crate::archive::create_command(
                    &dir,
                    &archive,
                    &names,
                    crate::archive::Tar::Local,
                );
                self.spawn_local_command(
                    format!("packing {archive}…"),
                    cmd,
                    format!(
                        "packed {} item(s) into {archive} ({})",
                        names.len(),
                        crate::archive::format_of(&archive)
                            .unwrap_or(crate::archive::Compression::Gzip)
                            .describe()
                    ),
                );
            }
            Side::Remote => self.send(Req::Archive {
                dir,
                names,
                archive,
                sudo: self.sudo(),
            }),
        }
    }

    fn run_extract(&mut self, at: Slot, archive: String, dest: String) {
        let dir = self.path_of(at);
        match at.host() {
            Side::Local => {
                let cmd = crate::archive::extract_command(&dir, &archive, &dest);
                self.spawn_local_command(
                    format!("unpacking {archive}…"),
                    cmd,
                    format!("unpacked {archive} into {dest}/"),
                );
            }
            Side::Remote => self.send(Req::Extract {
                dir,
                archive,
                dest,
                sudo: self.sudo(),
            }),
        }
    }

    /// Ask every worker to stop. Threads would die with the process anyway;
    /// this closes the SSH sessions politely on the way out.
    pub fn shutdown(&mut self) {
        // One last look before everything is torn down, so a tab opened in
        // the last second is still there next time. Straight to the write:
        // this is the one that cannot wait its turn.
        self.write_session();
        for pending in self.pending.drain(..) {
            let _ = pending.tx.send(Req::Quit);
        }
        for tab in &self.tabs {
            let _ = tab.tx.send(Req::Quit);
        }
        // Dropping the tabs stops their terminals too.
        self.tabs.clear();
        self.local_terms.clear();
    }

    /// Note what a worker has started or finished doing.
    ///
    /// The gauge belongs to the tab on screen, so it goes when that tab stops
    /// working — not when some other tab happens to finish.
    fn set_task(&mut self, source: RespSource, label: Option<String>) {
        let active = self.active;
        match source {
            RespSource::Pending(id) => {
                if let Some(pending) = self.pending.iter_mut().find(|p| p.id == id) {
                    pending.task = label;
                }
            }
            RespSource::Tab(index) => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.task = label;
                    if index == active && tab.task.is_none() {
                        self.progress = None;
                    }
                }
            }
        }
    }

    /// What to say is going on, when anything is.
    ///
    /// The tab you are looking at comes first — that is the work you are
    /// waiting on — and anything else still running gets a look in after it,
    /// so a connection being made in the background still says so.
    pub fn current_task(&self) -> Option<&str> {
        self.tab()
            .and_then(|t| t.task.as_deref())
            .or_else(|| self.local_tasks.last().map(String::as_str))
            .or_else(|| self.pending.iter().find_map(|p| p.task.as_deref()))
            .or_else(|| self.tabs.iter().find_map(|t| t.task.as_deref()))
            .filter(|label| !label.is_empty())
    }

    /// Fold in one worker message. `source` says which worker it came from,
    /// so a background tab's chatter cannot be mistaken for the active one's.
    pub fn handle_resp(&mut self, source: RespSource, resp: Resp) {
        // Task labels go in the title bar, not the status line: routine
        // listings would otherwise wipe out the result of whatever the user
        // just did and then sit there stale.
        match resp {
            Resp::TaskStart(label) => return self.set_task(source, Some(label)),
            Resp::TaskEnd => return self.set_task(source, None),
            _ => {}
        }

        match source {
            RespSource::Pending(id) => self.handle_pending_resp(id, resp),
            RespSource::Tab(index) => self.handle_tab_resp(index, resp),
        }
    }

    /// Messages from a connection that has not become a tab yet.
    fn handle_pending_resp(&mut self, id: u64, resp: Resp) {
        match resp {
            Resp::Connected {
                kind,
                user,
                host,
                port,
                home,
            } => {
                let Some(position) = self.pending.iter().position(|p| p.id == id) else {
                    return;
                };
                let pending = self.pending.remove(position);
                let start = pending.initial_dir.clone().unwrap_or_else(|| home.clone());
                // Consumed now that a connection has actually been made, so
                // the next tab starts from a clean slate.
                self.initial_remote = None;
                self.form.install_key = false;

                // Only plain servers go in the recent list: a container is
                // identified by an id that will not exist next week.
                // A name typed on the form wins; otherwise keep whatever was
                // stored for this server last time.
                let typed = pending.name.clone();
                let (note, tab_name) = match (&pending.target, kind) {
                    (Target::Ssh(opts), BackendKind::Ssh) => {
                        let note = match self.history.record(opts, typed.clone()) {
                            Ok(()) => String::new(),
                            Err(e) => format!(" (could not save to the server list: {e})"),
                        };
                        (note, typed.or_else(|| self.history.name_for(opts)))
                    }
                    _ => (String::new(), typed),
                };
                self.history_sel = 0;

                // Host-key grants apply to the attempt they were given for.
                // Left set, a later mismatch — on the shell's own connection
                // or after a reconnect — would be waved through in silence.
                let target = pending.target.without_host_key_grants();
                if let Some(opts) = target.ssh_opts() {
                    self.opts = opts.clone();
                }

                self.show_new_tab(RemoteTab {
                    target,
                    kind,
                    name: tab_name,
                    conn: ConnInfo {
                        user: user.clone(),
                        host: host.clone(),
                        port,
                        home,
                    },
                    link: LinkState::Live,
                    sudo: false,
                    trees: {
                        let mut trees: Vec<RemoteTree> = pending
                            .layout
                            .slots()
                            .into_iter()
                            .filter_map(|slot| match slot {
                                Slot::Files {
                                    host: Side::Remote,
                                    id,
                                } => Some(RemoteTree::new(id, String::new())),
                                _ => None,
                            })
                            .collect();
                        if !trees.iter().any(|t| t.id == layout::MAIN) {
                            trees.insert(0, RemoteTree::new(layout::MAIN, String::new()));
                        }
                        trees
                    },
                    terms: Vec::new(),
                    wants_editor: pending.editors.clone(),
                    wants_dir: pending.dirs.clone(),
                    focus: Slot::files(Side::Remote),
                    layout: pending.layout.clone(),
                    zoomed: false,
                    task: None,
                    forwards: Vec::new(),
                    tx: pending.tx,
                    rx: pending.rx,
                });

                self.form.connecting = false;
                self.host_key_issue = None;
                if let Some(opts) = pending.target.ssh_opts() {
                    self.needs_password.retain(|waiting| {
                        !(waiting.opts.user == opts.user
                            && waiting.opts.host == opts.host
                            && waiting.opts.port == opts.port)
                    });
                }
                self.mode = Mode::Browse;
                self.focus = Slot::files(Side::Local);
                // Unless the tab that just opened has no local half to focus.
                self.settle_focus();
                let title = self.tab().map(|t| t.title()).unwrap_or_default();
                // Nothing was connected to for a local tab, so saying so
                // would be nonsense. Point at what it can do instead.
                let msg = match self.tab().is_some_and(|t| t.is_local()) {
                    true => format!("{title}: a tab on this machine — S opens a shell in it"),
                    false => format!("Connected to {title}{note}"),
                };
                self.set_status(msg, Level::Good);
                // Every list this tab opened with is pointed where it was
                // when the workspace was saved, and otherwise at the door the
                // server let us in by. The arrangement decides how many there
                // are; `dirs` decides where each of them looks.
                let lists: Vec<(Slot, String)> = self
                    .layout
                    .slots()
                    .into_iter()
                    .filter(|slot| slot.is_files() && slot.host() == Side::Remote)
                    .map(|slot| {
                        let was = pending.dirs.tree(slot.id()).map(str::to_string);
                        (slot, was.unwrap_or_else(|| start.clone()))
                    })
                    .collect();
                for (slot, dir) in lists {
                    self.goto_remote(slot, dir);
                }

                // Ports a workspace recorded for this connection.
                if !pending.forwards.is_empty() {
                    let index = self.tabs.len() - 1;
                    let specs = pending.forwards.clone();
                    self.restore_forwards(index, &specs);
                }

                // Do this once, right after the login that proved we can get
                // in — that is the moment a password login can be turned into
                // a key login.
                if pending.install_key {
                    self.install_public_key();
                }
            }

            Resp::ConnectFailed {
                msg,
                issue,
                auth_failed,
            } => {
                // Retire the worker: this attempt is over either way.
                let (from_form, target_opts, name, asked_for) =
                    match self.pending.iter().position(|p| p.id == id) {
                        Some(position) => {
                            let pending = self.pending.remove(position);
                            let _ = pending.tx.send(Req::Quit);
                            let asked_for = (
                                pending.initial_dir.clone(),
                                pending.forwards.clone(),
                                pending.layout.clone(),
                                pending.editors.clone(),
                                pending.dirs.clone(),
                            );
                            (
                                pending.from_form,
                                pending.target.ssh_opts().cloned(),
                                pending.name.clone(),
                                asked_for,
                            )
                        }
                        None => return,
                    };
                self.form.connecting = false;

                // A workspace opening in the background must not hijack the
                // screen with its errors; say what failed and carry on.
                if !from_form {
                    // Passwords are deliberately never stored, so a
                    // password-only server in a workspace always lands here.
                    // Queue it rather than losing it, and say so once.
                    if auth_failed && let Some(opts) = target_opts.clone() {
                        let label = name.clone().unwrap_or_else(|| {
                            if opts.port == 22 {
                                format!("{}@{}", opts.user, opts.host)
                            } else {
                                format!("{}@{}:{}", opts.user, opts.host, opts.port)
                            }
                        });
                        if !self.needs_password.iter().any(|w| w.label == label) {
                            let (path, forwards, layout, editors, dirs) = asked_for;
                            self.needs_password.push(Waiting {
                                label,
                                opts,
                                path,
                                forwards,
                                layout,
                                editors,
                                dirs,
                            });
                        }
                        let waiting: Vec<&str> = self
                            .needs_password
                            .iter()
                            .map(|w| w.label.as_str())
                            .collect();
                        self.set_status(
                            format!(
                                "{} need a password — press C to connect {}",
                                waiting.len(),
                                waiting.join(", ")
                            ),
                            Level::Bad,
                        );
                    } else {
                        self.set_status(msg, Level::Bad);
                    }
                    return;
                }
                match issue {
                    Some(HostKeyIssue::Unknown {
                        fingerprint,
                        keytype,
                    }) => {
                        self.host_key_issue = Some(HostKeyIssue::Unknown {
                            fingerprint: fingerprint.clone(),
                            keytype: keytype.clone(),
                        });
                        self.confirm = Some(ConfirmState::simple(
                            "Unknown host key",
                            vec![
                                format!(
                                    "The authenticity of {}:{} can't be established.",
                                    self.opts.host, self.opts.port
                                ),
                                String::new(),
                                format!("  key type:    {keytype}"),
                                format!("  fingerprint: {fingerprint}"),
                                String::new(),
                                "Accept and add it to ~/.ssh/known_hosts?".into(),
                            ],
                            ConfirmAction::AcceptHostKey,
                            false,
                        ));
                        self.mode = Mode::Confirm;
                    }
                    Some(HostKeyIssue::Mismatch { fingerprint }) => {
                        // This is what a man-in-the-middle looks like, and it
                        // is also what a rebuilt server looks like. We cannot
                        // tell them apart, so lay out the facts and make the
                        // user type the word rather than reflexively hit `y`.
                        self.host_key_issue = Some(HostKeyIssue::Mismatch {
                            fingerprint: fingerprint.clone(),
                        });
                        self.confirm = Some(ConfirmState {
                            title: "HOST KEY CHANGED".into(),
                            body: vec![
                                format!(
                                    "The key offered by {}:{} does not match the one",
                                    self.opts.host, self.opts.port
                                ),
                                "recorded in ~/.ssh/known_hosts.".into(),
                                String::new(),
                                format!("  key now offered: {fingerprint}"),
                                String::new(),
                                "This happens when a server is rebuilt or its host key is".into(),
                                "rotated — and it is also exactly what an interception".into(),
                                "attack looks like. There is no way to tell from here.".into(),
                                String::new(),
                                "Only continue if you can confirm that fingerprint through".into(),
                                "some other channel (the provider's console, a colleague,".into(),
                                "`ssh-keyscan` from a machine you trust).".into(),
                                String::new(),
                                "Replacing the recorded key cannot be undone.".into(),
                            ],
                            action: ConfirmAction::ReplaceHostKey,
                            danger: true,
                            require_phrase: Some("replace".into()),
                            input: TextInput::default(),
                            return_to: Mode::Browse,
                        });
                        self.mode = Mode::Confirm;
                        self.set_status("host key mismatch — not connected", Level::Bad);
                    }
                    None => {
                        self.mode = Mode::Connect;
                        self.form.error = Some(msg.clone());
                        // Saved servers never carry a password, so picking one
                        // that wants a password lands here. Put the cursor
                        // where the user needs it rather than making them hunt.
                        if auth_failed {
                            self.connect_focus = ConnectFocus::Form;
                            self.form.field = ConnectForm::PASSWORD;
                            self.form.hint =
                                Some("Type a password below, then press Enter.".into());
                        }
                        self.set_status(msg, Level::Bad);
                    }
                }
            }

            _ => {}
        }
    }

    /// Messages from a connected tab.
    fn handle_tab_resp(&mut self, index: usize, resp: Resp) {
        let is_active = index == self.active;
        // Background tabs still report, but say which server they mean.
        let label = self
            .tabs
            .get(index)
            .map(|t| t.title())
            .unwrap_or_else(|| "?".into());
        let tag = |msg: String| {
            if is_active {
                msg
            } else {
                format!("[{label}] {msg}")
            }
        };

        match resp {
            Resp::Listing {
                path,
                entries,
                tree,
                seq,
            } => {
                let Some(tree) = self.tabs.get_mut(index).and_then(|t| t.tree_mut(tree)) else {
                    return;
                };
                if seq != tree.seq {
                    return; // a newer request has already been sent
                }
                tree.cwd = path;
                tree.pane.set_entries(entries);
                if let Some(name) = tree.pending_select.take()
                    && let Some(i) = tree.pane.view.iter().position(|e| e.name == name)
                {
                    tree.pane.select_index(i);
                }
            }

            Resp::Polled { tree, seq, entries } => {
                let Some(tree) = self.tabs.get_mut(index).and_then(|t| t.tree_mut(tree)) else {
                    return;
                };
                tree.polling = false;
                // Anything the user asked for in the meantime has the last
                // word: the answer is about the directory that pane *was*
                // showing.
                if seq != tree.seq {
                    return;
                }
                if let Some(entries) = entries {
                    tree.pane.absorb_entries(entries);
                }
            }

            Resp::ListFailed { path, msg, tree } => {
                if let Some(tree) = self.tabs.get_mut(index).and_then(|t| t.tree_mut(tree)) {
                    // Move the pane to the directory that failed, so the header
                    // and the error agree — and so enabling sudo retries *this*
                    // path rather than silently reloading the previous one.
                    tree.cwd = path;
                    tree.pane.all.clear();
                    tree.pane.view.clear();
                    tree.pane.state.select(None);
                    tree.pane.loading = false;
                    tree.pane.error = Some(msg.clone());
                }
                self.set_status(tag(msg), Level::Bad);
            }

            Resp::ExecDone { cmd, output, code } => {
                let title = format!("$ {cmd}   (exit {code})");
                if is_active {
                    self.show_output(title, output.clone());
                } else {
                    self.stash_output(title, output.clone());
                }
                self.set_status(
                    tag(format!("{cmd} → exit {code}")),
                    if code == 0 { Level::Good } else { Level::Bad },
                );
                // A command may well have changed what the pane shows.
                self.reload_tab(index);
            }

            Resp::Progress { label, done, total } => {
                if is_active {
                    self.progress = Some((label, done, total));
                }
            }

            Resp::Done {
                msg,
                refresh_local,
                refresh_remote,
            } => {
                self.set_status(tag(msg), Level::Good);
                if refresh_local {
                    self.reload_local();
                }
                if refresh_remote {
                    self.reload_tab(index);
                }
            }

            Resp::Failed(msg) => self.set_status(tag(msg), Level::Bad),

            Resp::Containers { runtime, list } => {
                let via = self.tabs.get(index).and_then(|t| t.ssh_opts()).cloned();
                let host = self
                    .tabs
                    .get(index)
                    .map(|t| t.conn.host.clone())
                    .unwrap_or_default();
                let title = format!("Containers on {host} ({runtime})");
                self.open_picker(list, via, runtime, title);
            }

            Resp::EditReady {
                temp,
                remote,
                sudo,
                editor,
            } => {
                let sig = file_signature(&temp);
                self.pending_action = Some(UiAction::Editor {
                    program: editor,
                    path: temp.clone(),
                    push_back: Some(PendingEdit {
                        temp,
                        remote,
                        sudo,
                        sig,
                        tab: index,
                    }),
                    // The push-back reloads the pane when it lands.
                    refresh: Refresh::Neither,
                });
            }

            Resp::SudoState { enabled, msg } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.sudo = enabled;
                }
                self.set_status(tag(msg), if enabled { Level::Good } else { Level::Bad });
                self.reload_tab(index);
            }

            Resp::Disconnected { reason } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.link = LinkState::Reconnecting;
                    // Sudo does not survive on its own; the worker re-checks it
                    // once the connection is back and tells us either way.
                    tab.sudo = false;
                    for tree in &mut tab.trees {
                        tree.pane.loading = true;
                    }
                }
                self.set_status(tag(format!("{reason} — reconnecting…")), Level::Bad);
            }

            Resp::Reconnecting { attempt, max } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.link = LinkState::Reconnecting;
                }
                self.set_status(tag(format!("reconnecting ({attempt}/{max})…")), Level::Info);
            }

            Resp::Reconnected { home, elevated } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.link = LinkState::Live;
                    tab.conn.home = home;
                    // The worker re-establishes root access itself; mirror
                    // whatever it managed rather than assuming.
                    tab.sudo = elevated;
                }
                self.set_status(
                    tag("reconnected — picking up where you left off".into()),
                    Level::Good,
                );
                // Back to the directory that was on screen, not to home.
                self.reload_tab(index);
            }

            Resp::ReconnectFailed { msg } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    tab.link = LinkState::Lost;
                    for tree in &mut tab.trees {
                        tree.pane.loading = false;
                        tree.pane.error = Some(msg.clone());
                    }
                }
                self.set_status(tag(format!("{msg} — press C to connect again")), Level::Bad);
            }

            // Only a pending connection produces these.
            Resp::Connected { .. }
            | Resp::ConnectFailed { .. }
            | Resp::TaskStart(_)
            | Resp::TaskEnd => {}
        }
    }

    /// Reload a specific tab's listing, whether or not it is on screen.
    /// Re-read every file list on a tab, for the same reason the local ones
    /// are re-read together.
    fn reload_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        let sudo = tab.sudo;
        let mut reqs = Vec::new();
        for tree in &mut tab.trees {
            if tree.cwd.is_empty() {
                continue;
            }
            tree.seq += 1;
            tree.pane.loading = true;
            reqs.push(Req::List {
                path: tree.cwd.clone(),
                sudo,
                tree: tree.id,
                seq: tree.seq,
            });
        }
        for req in reqs {
            let _ = tab.tx.send(req);
        }
    }

    /// Called by the main loop once an external editor has exited.
    pub fn after_editor(&mut self, push_back: Option<PendingEdit>, refresh: Refresh) {
        match refresh {
            Refresh::Local => self.reload_local(),
            Refresh::Remote => self.reload_remote(),
            Refresh::Neither => {}
        }
        let Some(edit) = push_back else { return };
        let now = file_signature(&edit.temp);
        if now.is_none() {
            self.set_status(
                format!("{} vanished — nothing saved", edit.temp.display()),
                Level::Bad,
            );
            return;
        }
        if now == edit.sig {
            self.set_status(
                format!("{} unchanged — not uploaded", rbasename(&edit.remote)),
                Level::Info,
            );
            if let Some(dir) = edit.temp.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
            return;
        }
        // Back to the server it came from, even if the user has since
        // switched tabs.
        let Some(tab) = self.tabs.get(edit.tab) else {
            self.set_status(
                format!(
                    "that server's tab is gone — your edit is at {}",
                    edit.temp.display()
                ),
                Level::Bad,
            );
            return;
        };
        let _ = tab.tx.send(Req::PushEdit {
            temp: edit.temp,
            path: edit.remote,
            sudo: edit.sudo,
        });
    }

    // ---- key handling ------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // The pointer's highlight is about where the pointer is, and a key
        // can move the list out from under it. Anyone typing has stopped
        // aiming with the mouse anyway. The label naming a tab goes with it,
        // and for a stronger reason: a key can close the tab it is about.
        self.hover = None;
        self.tab_rest = None;

        // A menu that is open has the keyboard. It is a question with the
        // answers on the screen, and the list underneath is not what the
        // arrows are for while it is there.
        if self.menu.is_some() {
            return self.menu_key(key);
        }

        // With the keyboard handed over, every key is sshman's — including
        // the ones a shell would otherwise swallow.
        if self.mode == Mode::Browse && self.commanding {
            self.command_key(key);
            return;
        }

        // Zooming is window management rather than input, so it keeps working
        // while a shell has the keyboard — otherwise a zoomed shell could
        // only be shrunk by leaving it first.
        if self.mode == Mode::Browse && key.code == KeyCode::F(3) {
            self.toggle_zoom();
            return;
        }
        // So is closing one, and for the same reason: a shell you are typing
        // in is exactly the pane you are most likely to want rid of.
        if self.mode == Mode::Browse && key.code == KeyCode::F(9) {
            self.close_pane(self.focus);
            return;
        }

        // A focused shell owns the keyboard. Every key goes to it — Ctrl-C has
        // to interrupt the running command, not quit sshman — so the escape
        // key is checked first and is the only way back out.
        if self.mode == Mode::Browse && self.in_term() {
            // The one key it is the shell's business to let go of, asked of
            // the keymap like any other — so rebinding it rebinds it here as
            // well as in a file list.
            if self.keymap.action(&key) == Some(Action::Command) {
                self.enter_command();
            } else if let Some(shell) = self.shell_mut(self.focus) {
                shell.send_key(key);
            } else {
                // The terminal went away underneath us.
                self.settle_focus();
            }
            return;
        }

        // Ctrl-C quits from anywhere else — after being asked, and pressing
        // it a second time is the answer.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.ask_quit();
            return;
        }
        match self.mode {
            Mode::Connect => self.connect_key(key),
            Mode::Picker => self.picker_key(key),
            Mode::Workspaces => self.workspace_key(key),
            Mode::Settings => self.settings_key(key),
            Mode::Arrange => self.arrange_key(key),
            Mode::Themes => self.theme_key(key),
            Mode::Keys => self.keys_key(key),
            Mode::Forwards => self.forward_key(key),
            Mode::Browse => self.browse_key(key),
            Mode::Prompt => self.prompt_key(key),
            Mode::Confirm => self.confirm_key(key),
            Mode::Output => self.output_key(key),
            Mode::Help => self.help_key(key),
        }
    }

    fn connect_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.dismiss_connect_screen(),
            KeyCode::Enter => self.submit_connect_form(),
            // Tab always jumps between the list and the form; the arrows walk
            // the list first and then carry on into the fields.
            KeyCode::Tab => self.move_connect_focus(1),
            KeyCode::BackTab => self.move_connect_focus(-1),
            KeyCode::Down => {
                if self.connect_focus == ConnectFocus::Recent
                    && self.history_sel + 1 < self.history.len()
                {
                    self.move_history_selection(1);
                } else {
                    self.move_connect_focus(1);
                }
            }
            KeyCode::Up => {
                if self.connect_focus == ConnectFocus::Recent && self.history_sel > 0 {
                    self.move_history_selection(-1);
                } else {
                    self.move_connect_focus(-1);
                }
            }
            // Forgetting a saved server, only meaningful on the list.
            KeyCode::Delete if self.connect_focus == ConnectFocus::Recent => {
                self.forget_selected_server();
            }
            KeyCode::Char('n') if self.connect_focus == ConnectFocus::Recent => {
                if let Some(entry) = self.history.get(self.history_sel) {
                    let current = entry.name.clone().unwrap_or_default();
                    let address = entry.address();
                    let index = self.history_sel;
                    self.open_prompt(
                        PromptKind::NameSaved(index),
                        format!("Name for {address} (empty clears it)"),
                        current,
                    );
                }
            }
            KeyCode::Char(' ')
                if self.connect_focus == ConnectFocus::Form
                    && self.form.field == ConnectForm::CHECKBOX =>
            {
                self.form.install_key = !self.form.install_key;
            }
            _ => {
                if self.connect_focus == ConnectFocus::Form
                    && let Some(input) = self.form.current()
                {
                    input.handle(key);
                }
            }
        }
    }

    /// Focus moves through one ring: the recent list (when there is one),
    /// then each form field.
    fn move_connect_focus(&mut self, delta: isize) {
        let has_list = !self.history.is_empty();
        let list_slots = usize::from(has_list);
        let len = (ConnectForm::FIELDS + list_slots) as isize;

        let pos = match self.connect_focus {
            ConnectFocus::Recent => 0,
            ConnectFocus::Form => (self.form.field + list_slots) as isize,
        };
        let next = (pos + delta).rem_euclid(len) as usize;

        if has_list && next == 0 {
            self.connect_focus = ConnectFocus::Recent;
        } else {
            self.connect_focus = ConnectFocus::Form;
            self.form.field = next - list_slots;
        }
    }

    /// Up/Down within the recent list. Kept separate from focus movement so
    /// the list behaves like a list once you are in it.
    fn move_history_selection(&mut self, delta: isize) {
        if self.history.is_empty() {
            return;
        }
        let last = self.history.len() as isize - 1;
        self.history_sel = (self.history_sel as isize + delta).clamp(0, last) as usize;
        self.sync_form_from_history();
    }

    /// Mirror the highlighted server into the form, so you can Tab across and
    /// adjust one field — a different key, say — before connecting.
    fn sync_form_from_history(&mut self) {
        if let Some(entry) = self.history.get(self.history_sel) {
            let opts = entry.to_opts();
            self.form.host.set(opts.host.clone());
            self.form.port.set(opts.port.to_string());
            self.form.user.set(opts.user.clone());
            self.form.key.set(
                opts.key_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            );
            // Passwords are never stored, so there is nothing to mirror.
            self.form.password.clear();
        }
    }

    fn forget_selected_server(&mut self) {
        if let Some(entry) = self.history.remove(self.history_sel) {
            self.set_status(format!("forgot {}", entry.label()), Level::Info);
        }
        if self.history.is_empty() {
            self.connect_focus = ConnectFocus::Form;
            self.history_sel = 0;
        } else {
            self.history_sel = self.history_sel.min(self.history.len() - 1);
            self.sync_form_from_history();
        }
    }

    fn submit_connect_form(&mut self) {
        // Picking from the list connects straight away with the saved details.
        if self.connect_focus == ConnectFocus::Recent {
            if let Some(entry) = self.history.get(self.history_sel) {
                self.opts = entry.to_opts();
                self.form = ConnectForm::new(&self.opts);
                self.start_connect();
                return;
            }
            self.connect_focus = ConnectFocus::Form;
            return;
        }

        let raw_host = self.form.host.value.trim().to_string();
        if raw_host.is_empty() {
            self.form.error = Some("host is required".into());
            return;
        }
        // Accept "user@host" typed into the host field.
        let (mut user, mut host) = match raw_host.split_once('@') {
            Some((u, h)) => (u.to_string(), h.to_string()),
            None => (self.form.user.value.trim().to_string(), raw_host),
        };
        let mut port: u16 = self.form.port.value.trim().parse().unwrap_or(22);
        let mut key_path = {
            let k = self.form.key.value.trim();
            (!k.is_empty()).then(|| local::expand(k))
        };

        // Apply ~/.ssh/config for anything the user left blank.
        let cfg = crate::sshcfg::lookup(&host);
        if user.is_empty() {
            user = cfg.user.unwrap_or_else(default_user);
        }
        if self.form.port.value.trim().is_empty() {
            port = cfg.port.unwrap_or(22);
        }
        if key_path.is_none() {
            key_path = cfg.identity_file;
        }
        if let Some(hostname) = cfg.hostname {
            host = hostname;
        }

        let password = {
            let p = self.form.password.value.clone();
            (!p.is_empty()).then_some(p)
        };

        self.opts = ConnectOpts {
            host,
            port,
            user,
            password,
            key_path,
            key_passphrase: None,
            accept_new_host_key: false,
            replace_host_key: false,
        };
        self.form.user.set(self.opts.user.clone());
        self.form.port.set(self.opts.port.to_string());
        self.start_connect();
    }

    /// The key after `Ctrl-]`: the pane commands, for when the keyboard is in
    /// a shell and every ordinary key belongs to it.
    ///
    /// They are the browsing keys in lower case — `s` for the shell `S` opens,
    /// `x` for the close `F9` does — so there is one set to remember rather
    /// than two.
    fn command_key(&mut self, key: KeyEvent) {
        // A pane that has been picked up: the arrows move the pane itself,
        // and everything else puts it down first.
        if self.carrying && self.carry_key(key) {
            return;
        }

        // Moving between panes is what the arrows are for while sshman has
        // the keyboard, and h j k l alongside them for the same reason they
        // move the cursor in a file list.
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Ctrl with an arrow is the tab strip's, wherever it is pressed:
            // the panes answer a bare arrow, and let the chord through to the
            // keymap below so tabs switch in here as they do anywhere else.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Left | KeyCode::Char('h') if !shift => {
                return self.move_focus(Dir::Across, false);
            }
            KeyCode::Right | KeyCode::Char('l') if !shift => {
                return self.move_focus(Dir::Across, true);
            }
            KeyCode::Up | KeyCode::Char('k') if !shift => {
                return self.move_focus(Dir::Down, false);
            }
            KeyCode::Down | KeyCode::Char('j') if !shift => {
                return self.move_focus(Dir::Down, true);
            }
            // Shift moves the border rather than the keyboard: the same pair
            // of ideas the file list spells Alt and Alt-Shift.
            KeyCode::Left => return self.resize_pane(Dir::Across, -2),
            KeyCode::Right => return self.resize_pane(Dir::Across, 2),
            KeyCode::Up => return self.resize_pane(Dir::Down, -3),
            KeyCode::Down => return self.resize_pane(Dir::Down, 3),

            // The keyboard goes back to a pane only when you say so, which is
            // what lets the arrows walk past a shell without falling into it.
            KeyCode::Char('g') => return self.toggle_carry(),
            KeyCode::Enter => return self.leave_command(false),
            KeyCode::Esc => return self.leave_command(true),
            _ if self.keymap.action(&key) == Some(Action::Command) => {
                return self.leave_command(true);
            }
            _ => {}
        }

        // Everything else is an ordinary sshman key, and does here exactly
        // what it does with a file list focused: there is one set of keys,
        // not one set per place you happen to be standing.
        self.browse_key(key);

        // A key that opened something, or that is taking the terminal away,
        // has moved on from arranging panes.
        if self.mode != Mode::Browse || self.pending_action.is_some() {
            self.commanding = false;
            self.command_from = None;
        }
    }

    /// The keys that only mean something while a pane is picked up. Says
    /// whether it took the key.
    fn carry_key(&mut self, key: KeyEvent) -> bool {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            // Ctrl-arrow means the tab strip even here, so it puts the pane
            // down first rather than shoving it somewhere on the way out.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.carrying = false;
                return false;
            }
            // Shove it past its neighbour, again and again if you like: the
            // keyboard goes with it, so the arrows keep meaning the same
            // thing however far it has travelled.
            KeyCode::Left | KeyCode::Char('h') if !shift => self.shove(Dir::Across, false),
            KeyCode::Right | KeyCode::Char('l') if !shift => self.shove(Dir::Across, true),
            KeyCode::Up | KeyCode::Char('k') if !shift => self.shove(Dir::Down, false),
            KeyCode::Down | KeyCode::Char('j') if !shift => self.shove(Dir::Down, true),

            // Shift sends it the whole way: a column or a row of its own,
            // against that edge of the tab.
            KeyCode::Left => self.send_pane_to_edge(Dir::Across, true),
            KeyCode::Right => self.send_pane_to_edge(Dir::Across, false),
            KeyCode::Up => self.send_pane_to_edge(Dir::Down, true),
            KeyCode::Down => self.send_pane_to_edge(Dir::Down, false),

            KeyCode::Char('g') | KeyCode::Esc => {
                self.carrying = false;
                self.set_status("put down — arrows move the keyboard again", Level::Info);
            }
            KeyCode::Enter => {
                self.carrying = false;
                self.leave_command(false);
            }
            // Anything else puts the pane down and then means what it always
            // means, so a stray key is never a trap.
            _ => {
                self.carrying = false;
                return false;
            }
        }
        true
    }

    /// Pick the focused pane up, or put it down again.
    fn toggle_carry(&mut self) {
        if self.carrying {
            self.carrying = false;
            self.set_status("put down", Level::Info);
            return;
        }
        if self.layout.panes() < 2 {
            self.set_status("there is nowhere to move the only pane", Level::Info);
            return;
        }
        self.carrying = true;
        let name = self.pane_name(self.focus);
        self.set_status(
            format!("moving {name} — arrows shove it, Shift-arrows send it to an edge, ↵ drops it"),
            Level::Good,
        );
    }

    /// Change places with the pane across the nearest border that way. The
    /// keyboard follows the pane rather than the place.
    fn shove(&mut self, dir: Dir, forward: bool) {
        let Some(other) = self.layout.neighbour(self.focus, dir, forward) else {
            self.set_status("no pane that way to change places with", Level::Info);
            return;
        };
        let here = self.focus;
        if self.layout.swap(here, other) {
            self.stash_layout();
            let name = self.pane_name(here);
            self.set_status(format!("moved {name}"), Level::Good);
        }
    }

    /// Send the focused pane to one edge of the tab, as a column or a row of
    /// its own.
    fn send_pane_to_edge(&mut self, dir: Dir, first: bool) {
        let here = self.focus;
        if self.layout.send_to_edge(here, dir, first) {
            self.stash_layout();
            let name = self.pane_name(here);
            let edge = match (dir, first) {
                (Dir::Across, true) => "the left",
                (Dir::Across, false) => "the right",
                (Dir::Down, true) => "the top",
                (Dir::Down, false) => "the bottom",
            };
            self.set_status(format!("{name} sent to {edge}"), Level::Good);
        } else {
            self.set_status("there is nowhere to send it", Level::Info);
        }
    }

    /// A pane dragged by its name onto another one changes places with it.
    pub fn drop_moved_pane(&mut self) {
        let (Some(from), Some(onto)) = (self.moving.take(), self.move_over.take()) else {
            self.moving = None;
            self.move_over = None;
            return;
        };
        if from == onto || !self.layout.swap(from, onto) {
            return;
        }
        self.stash_layout();
        self.focus_pane(from);
        let name = self.pane_name(from);
        self.set_status(format!("moved {name}"), Level::Good);
    }

    /// Take the keyboard for sshman.
    fn enter_command(&mut self) {
        self.commanding = true;
        self.command_from = Some(self.focus);
        self.set_status(
            "sshman has the keyboard — arrows move, ↵ hands it to the pane, Esc puts it back",
            Level::Info,
        );
    }

    /// Give the keyboard back: to the pane you have moved to, or — `back` —
    /// to the one you took it from.
    fn leave_command(&mut self, back: bool) {
        self.commanding = false;
        self.carrying = false;
        if back
            && let Some(slot) = self.command_from.take()
            && self.layout.contains(slot)
        {
            self.focus = slot;
        }
        self.command_from = None;
        self.settle_focus();
        let name = self.pane_name(self.focus);
        self.set_status(
            format!("{name} — Ctrl-] takes the keyboard back"),
            Level::Info,
        );
    }

    // ---- picking text out of a terminal --------------------------------------

    /// Copy what is picked out in the focused terminal.
    fn copy_selection(&mut self) {
        match self.shell(self.focus).and_then(Shell::selected_text) {
            Some(text) => self.copy(text),
            None => self.set_status(
                "nothing picked out — drag across the text first",
                Level::Info,
            ),
        }
    }

    /// Hold on to some text, and hand it to the terminal sshman is running in
    /// so it reaches the system clipboard.
    pub fn copy(&mut self, text: String) {
        let lines = text.lines().count();
        let chars = text.chars().count();
        self.clipboard_out = Some(text.clone());
        self.copied = Some(text);
        self.set_status(
            match lines {
                0 | 1 => format!("copied {chars} character(s)"),
                n => format!("copied {n} lines"),
            },
            Level::Good,
        );
    }

    /// What the main loop has to hand the terminal, if anything.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.clipboard_out.take()
    }

    /// Pick up anything a program inside a pane asked to put on the
    /// clipboard — `y` in vim, `tmux copy-selection`, anything else that
    /// speaks `OSC 52`.
    ///
    /// It is the same request as sshman's own copy and takes the same route
    /// out, so it also becomes what [`Action::PasteText`] types into a pane.
    pub fn take_shell_clipboard(&mut self) {
        if let Some(text) = crate::shell::take_copied() {
            self.copy(text);
        }
    }

    /// Type what was copied into the focused terminal. The system clipboard
    /// is the terminal's own business; this is the way that works when it
    /// cannot be reached at all.
    fn paste_copied(&mut self) {
        let Some(text) = self.copied.clone() else {
            self.set_status(
                "nothing copied yet — drag over a shell, then y",
                Level::Info,
            );
            return;
        };
        match self.shell_mut(self.focus) {
            Some(shell) => {
                shell.paste(&text);
                self.set_status("pasted", Level::Good);
            }
            None => self.set_status("not a terminal", Level::Info),
        }
    }

    /// A key while browsing: whatever the keymap says it asks for.
    fn browse_key(&mut self, key: KeyEvent) {
        // Alt-1 … Alt-9 jump straight to a tab. Nine bindings for one idea,
        // and the number is the whole of it, so they are not in the keymap.
        if key.modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(c) = key.code
            && let Some(n) = c.to_digit(10)
            && n >= 1
        {
            self.goto_tab(n as usize - 1);
            return;
        }
        if let Some(action) = self.keymap.action(&key) {
            self.run(action);
        }
    }

    /// Do the thing itself, whichever key asked for it.
    fn run(&mut self, action: Action) {
        let at = self.focus;
        match action {
            Action::Quit => self.ask_quit(),
            Action::MoveTabLeft => self.move_tab(-1),
            Action::MoveTabRight => self.move_tab(1),
            // Cancel backs out of whatever narrowing is in effect. It
            // deliberately does not quit: losing a session to a stray Esc is
            // infuriating.
            Action::Cancel => {
                let pane = self.pane_mut(at);
                if !pane.filter.is_empty() {
                    let keep = pane.selected_name();
                    pane.filter.clear();
                    pane.refresh_view(keep.as_deref());
                    self.set_status("filter cleared", Level::Info);
                } else if !pane.marked.is_empty() {
                    pane.marked.clear();
                    self.set_status("marks cleared", Level::Info);
                } else if self.zoomed {
                    self.toggle_zoom();
                } else if self.clip.is_some() {
                    self.clip = None;
                    self.set_status("clipboard cleared", Level::Info);
                } else {
                    self.set_status("press q to quit", Level::Info);
                }
            }

            // Stepping through the file lists in the order they are drawn: with
            // the two sshman opens with it crosses the middle, and with more it
            // reaches every one of them.
            Action::NextList | Action::PreviousList => {
                let back = action == Action::PreviousList;
                match self.next_files_pane(at, back) {
                    Some(next) => self.focus_pane(next),
                    None => self.set_status(
                        "this tab has one file list — T opens another, C adds a server",
                        Level::Info,
                    ),
                }
            }

            Action::Down => self.pane_mut(at).move_by(1),
            Action::Up => self.pane_mut(at).move_by(-1),
            Action::PageDown => self.pane_mut(at).move_by(15),
            Action::PageUp => self.pane_mut(at).move_by(-15),
            Action::Top => self.pane_mut(at).select_index(0),
            Action::Bottom => {
                let last = self.pane(at).view.len().saturating_sub(1);
                self.pane_mut(at).select_index(last);
            }

            Action::Parent => self.go_up(at),
            Action::Open => self.activate(at),

            Action::Mark => {
                self.pane_mut(at).toggle_mark();
                self.pane_mut(at).move_by(1);
            }
            Action::MarkAll => {
                let pane = self.pane_mut(at);
                if pane.marked.is_empty() {
                    pane.marked = pane.view.iter().map(|e| e.name.clone()).collect();
                } else {
                    pane.marked.clear();
                }
            }

            // With another list on screen this is the copy across to it. With
            // none — zoomed, or arranged without one — there is no across, so
            // it picks the selection up for a paste elsewhere on the same
            // filesystem.
            Action::Copy => {
                if self.other_side_on_screen() {
                    self.copy_to_target();
                } else {
                    self.yank(false);
                }
            }
            Action::Cut => self.yank(true),
            Action::Paste => self.paste_clip(),

            Action::Edit => self.edit_selected(at),
            Action::EditWith => {
                if let Some(name) = self.pane(at).selected_name() {
                    let mut input = TextInput::new(self.editor.clone());
                    input.cursor = input.value.chars().count();
                    self.prompt = Some(PromptState {
                        kind: PromptKind::OpenWith(at, name.clone()),
                        title: format!("Open {name} with"),
                        input,
                        hist_idx: None,
                        return_to: self.mode,
                    });
                    self.mode = Mode::Prompt;
                }
            }
            Action::View => {
                let pager = self.pager.clone();
                self.open_with(at, pager);
            }

            Action::NewDirectory => self.open_prompt(
                PromptKind::Mkdir(at),
                format!("New directory in {}", self.path_of(at)),
                String::new(),
            ),
            Action::Rename => {
                if let Some(name) = self.pane(at).selected_name() {
                    self.open_prompt(
                        PromptKind::Rename(at, name.clone()),
                        format!("Rename {name} to"),
                        name,
                    );
                }
            }
            Action::Delete => self.request_delete(at),

            Action::Reload => {
                self.reload_local();
                self.reload_remote();
                self.set_status("refreshed", Level::Info);
            }
            Action::Hidden => {
                let keep = self.pane(at).selected_name();
                let pane = self.pane_mut(at);
                pane.show_hidden = !pane.show_hidden;
                pane.refresh_view(keep.as_deref());
            }
            Action::Filter => self.open_prompt(
                PromptKind::Filter(at),
                "Filter".into(),
                self.pane(at).filter.clone(),
            ),
            Action::GoTo => self.open_prompt(
                PromptKind::GoTo(at),
                format!("Go to directory ({})", self.pane_name(at)),
                self.path_of(at),
            ),

            Action::RemoteCommand => {
                if !self.connected() {
                    self.set_status("not connected", Level::Bad);
                } else {
                    self.open_prompt(
                        PromptKind::Command,
                        format!("Remote command in {}", self.remote_cwd()),
                        String::new(),
                    );
                }
            }
            Action::FullShell => {
                if !self.connected() {
                    self.set_status("not connected", Level::Bad);
                } else {
                    self.pending_action = Some(UiAction::Shell);
                }
            }
            Action::Settings => self.open_settings(),
            Action::Home => self.go_home(at),
            Action::Containers => self.find_containers(at),
            Action::NameTab => self.start_rename_tab(),
            Action::Workspaces => self.open_workspaces(),
            Action::Ports => self.open_forwards(),
            Action::Archive => self.start_archive(at),
            Action::Extract => self.start_extract(at),
            Action::ListArchive => self.list_archive(at),

            // ---- panes ----
            Action::Zoom => self.toggle_zoom(),
            Action::Even => self.reset_layout(),
            Action::Arrange => self.open_arrangements(),
            Action::Split => self.split_with_term(Dir::Across, 50),
            Action::SplitDown => self.split_with_term(Dir::Down, 50),
            Action::NewList => self.split_with_tree(Dir::Across, 50),
            Action::ClosePane => self.close_pane(self.focus),
            Action::Command => self.enter_command(),
            Action::FocusLeft => self.move_focus(Dir::Across, false),
            Action::FocusRight => self.move_focus(Dir::Across, true),
            Action::FocusUp => self.move_focus(Dir::Down, false),
            Action::FocusDown => self.move_focus(Dir::Down, true),
            Action::BorderLeft => self.resize_pane(Dir::Across, -2),
            Action::BorderRight => self.resize_pane(Dir::Across, 2),
            Action::BorderUp => self.resize_pane(Dir::Down, -3),
            Action::BorderDown => self.resize_pane(Dir::Down, 3),

            // What a drag in a shell picked out, and putting it back into one.
            Action::CopyText => self.copy_selection(),
            Action::PasteText => self.paste_copied(),

            Action::Shell => self.toggle_shell(),
            Action::Sudo => self.toggle_sudo(),
            Action::Mirror => self.mirror_path(),
            Action::Output => {
                if self.output.is_empty() {
                    self.set_status("no command output yet", Level::Info);
                } else {
                    self.mode = Mode::Output;
                }
            }
            // The connection screen. Whatever it connects to arrives as a new
            // tab, leaving the ones you have alone, so there is one key for
            // both "connect" and "another server please".
            Action::Connect => self.open_connect_screen(),
            // A tab that needs no server at all.
            Action::LocalTab => self.open_local_tab(),
            Action::EditorPane => self.toggle_editor_pane(),
            Action::CloseTab => self.close_tab(),
            Action::NextTab => self.cycle_tab(1),
            Action::PreviousTab => self.cycle_tab(-1),
            Action::Help => {
                self.help_scroll = 0;
                self.mode = Mode::Help;
            }
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, title: String, initial: String) {
        let masked = matches!(kind, PromptKind::SudoPassword);
        let mut input = if masked {
            TextInput::masked()
        } else {
            TextInput::new(initial)
        };
        if masked {
            input.clear();
        }
        self.prompt = Some(PromptState {
            kind,
            title,
            input,
            hist_idx: None,
            return_to: self.mode,
        });
        self.mode = Mode::Prompt;
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.prompt.as_mut() else {
            self.mode = Mode::Browse;
            return;
        };
        match key.code {
            KeyCode::Esc => {
                // Cancelling a filter should also clear it, otherwise the pane
                // stays mysteriously narrowed.
                if let PromptKind::Filter(side) = prompt.kind {
                    let back = prompt.return_to;
                    self.prompt = None;
                    self.mode = back;
                    let keep = self.pane(side).selected_name();
                    let pane = self.pane_mut(side);
                    pane.filter.clear();
                    pane.refresh_view(keep.as_deref());
                    return;
                }
                let back = prompt.return_to;
                self.prompt = None;
                self.mode = back;
            }
            KeyCode::Enter => self.submit_prompt(),
            KeyCode::Up if matches!(prompt.kind, PromptKind::Command) => {
                let len = self.cmd_history.len();
                if len > 0 {
                    let idx = match prompt.hist_idx {
                        None => len - 1,
                        Some(i) => i.saturating_sub(1),
                    };
                    prompt.hist_idx = Some(idx);
                    let value = self.cmd_history[idx].clone();
                    prompt.input.set(value);
                }
            }
            KeyCode::Down if matches!(prompt.kind, PromptKind::Command) => {
                if let Some(i) = prompt.hist_idx {
                    if i + 1 < self.cmd_history.len() {
                        prompt.hist_idx = Some(i + 1);
                        let value = self.cmd_history[i + 1].clone();
                        prompt.input.set(value);
                    } else {
                        prompt.hist_idx = None;
                        prompt.input.clear();
                    }
                }
            }
            _ => {
                prompt.input.handle(key);
                // Filtering is live: retype and the list narrows as you go.
                if let PromptKind::Filter(side) = prompt.kind {
                    let value = prompt.input.value.clone();
                    let keep = self.pane(side).selected_name();
                    let pane = self.pane_mut(side);
                    pane.filter = value;
                    pane.refresh_view(keep.as_deref());
                }
            }
        }
    }

    fn submit_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            self.mode = Mode::Browse;
            return;
        };
        self.mode = prompt.return_to;
        let value = prompt.input.value.trim().to_string();

        match prompt.kind {
            PromptKind::Command => {
                if value.is_empty() {
                    return;
                }
                self.cmd_history.retain(|c| c != &value);
                self.cmd_history.push(value.clone());
                self.send(Req::Exec {
                    cmd: value,
                    cwd: self.remote_cwd(),
                    sudo: self.sudo(),
                });
            }
            PromptKind::Mkdir(at) => {
                if value.is_empty() {
                    return;
                }
                let dir = self.dir_of(at);
                match at.host() {
                    Side::Local => {
                        let path = PathBuf::from(dir).join(&value);
                        match local::mkdir(&path) {
                            Ok(()) => {
                                self.set_status(format!("created {value}"), Level::Good);
                                self.reload_local();
                            }
                            Err(e) => self.set_status(e.to_string(), Level::Bad),
                        }
                    }
                    Side::Remote => self.send(Req::Mkdir {
                        path: rjoin(&dir, &value),
                        sudo: self.sudo(),
                    }),
                }
            }
            PromptKind::Rename(at, old) => {
                if value.is_empty() || value == old {
                    return;
                }
                let dir = self.dir_of(at);
                match at.host() {
                    Side::Local => {
                        let from = PathBuf::from(&dir).join(&old);
                        let to = PathBuf::from(&dir).join(&value);
                        match local::rename(&from, &to) {
                            Ok(()) => {
                                self.set_status(format!("renamed to {value}"), Level::Good);
                                self.reload_local();
                            }
                            Err(e) => self.set_status(e.to_string(), Level::Bad),
                        }
                    }
                    Side::Remote => self.send(Req::Rename {
                        from: rjoin(&dir, &old),
                        to: rjoin(&dir, &value),
                        sudo: self.sudo(),
                    }),
                }
            }
            PromptKind::Filter(at) => {
                let keep = self.pane(at).selected_name();
                let pane = self.pane_mut(at);
                pane.filter = value;
                pane.refresh_view(keep.as_deref());
            }
            PromptKind::GoTo(at) => {
                if value.is_empty() {
                    return;
                }
                match at.host() {
                    Side::Local => self.goto_local(at, local::expand(&value)),
                    Side::Remote => self.goto_remote(at, value),
                }
            }
            PromptKind::SudoPassword => {
                self.send(Req::SetSudo(Some(prompt.input.value.clone())));
            }
            PromptKind::OpenWith(side, name) => {
                if value.is_empty() {
                    return;
                }
                self.launch_on(side, &name, value);
            }
            PromptKind::NameTab => {
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                tab.name = (!value.is_empty()).then(|| value.clone());
                // Persist it against the saved server, so it is there next time.
                let stored = match (&tab.target, tab.kind) {
                    (Target::Ssh(opts), BackendKind::Ssh) => {
                        let opts = opts.clone();
                        self.history.record(&opts, Some(value.clone())).is_ok()
                    }
                    // A container's id is not worth remembering, so its name
                    // lasts only as long as the tab.
                    _ => false,
                };
                let title = self.tab().map(|t| t.title()).unwrap_or_default();
                self.set_status(
                    if value.is_empty() {
                        "name cleared".to_string()
                    } else if stored {
                        format!("named {title} — remembered for next time")
                    } else {
                        format!("named {title} for this session")
                    },
                    Level::Good,
                );
            }
            PromptKind::NameSaved(index) => {
                let label = self
                    .history
                    .rename(index, &value)
                    .map(|e| e.label())
                    .unwrap_or_default();
                self.sync_form_from_history();
                self.set_status(format!("saved as {label}"), Level::Good);
            }
            PromptKind::AddForward => {
                if value.is_empty() {
                    return;
                }
                self.add_forward(&value);
            }
            PromptKind::SaveWorkspace => {
                if value.is_empty() {
                    return;
                }
                self.save_workspace(&value);
            }
            PromptKind::Archive(side, names) => {
                if value.is_empty() {
                    return;
                }
                self.pane_mut(side).marked.clear();
                self.run_archive(side, names, value);
            }
            PromptKind::Extract(side, archive) => {
                if value.is_empty() {
                    return;
                }
                self.run_extract(side, archive, value);
            }
            PromptKind::SetEditor => self.set_editor(value),
            PromptKind::SetEditorOpen => self.set_editor_open(value),
            PromptKind::SetShell => self.set_shell(value),
        }
    }

    pub fn open_settings(&mut self) {
        self.settings_sel = self.settings_sel.min(Setting::ALL.len().saturating_sub(1));
        self.mode = Mode::Settings;
    }

    /// The setting the cursor is on.
    pub fn selected_setting(&self) -> Setting {
        Setting::ALL[self.settings_sel.min(Setting::ALL.len() - 1)]
    }

    fn settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(',') => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_sel = (self.settings_sel + 1).min(Setting::ALL.len() - 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_sel = self.settings_sel.saturating_sub(1);
            }
            // Enter opens a setting; the arrows step through it in place, for
            // when you know which way you are going.
            KeyCode::Enter => self.open_setting(self.selected_setting()),
            KeyCode::Right | KeyCode::Char('l') => {
                self.change_setting(self.selected_setting(), 1);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.change_setting(self.selected_setting(), -1);
            }
            // Only a setting of your own can be cleared; the inherited value
            // underneath it is not ours to remove.
            KeyCode::Delete | KeyCode::Backspace => {
                let setting = self.selected_setting();
                if self.config.is_set(setting) {
                    self.clear_setting(setting);
                } else {
                    self.set_status(
                        format!("{} is not set here — nothing to clear", setting.label()),
                        Level::Info,
                    );
                }
            }
            _ => {}
        }
    }

    /// Open a setting: a chooser for the ones with a list to look through, a
    /// prompt for the ones you type an answer to.
    fn open_setting(&mut self, setting: Setting) {
        match setting {
            Setting::Theme => self.open_themes(),
            Setting::Keys => self.open_keys(),
            // There are only two answers, so opening it is the same as
            // stepping it: a list of two would be a list for its own sake.
            Setting::Background | Setting::ShellColours | Setting::Watch | Setting::Resume => {
                self.change_setting(setting, 1)
            }
            Setting::Editor | Setting::EditorOpen | Setting::Shell => self.ask_for_setting(setting),
        }
    }

    // ---- choosing a theme ---------------------------------------------------

    pub fn open_themes(&mut self) {
        if self.themes.entries.is_empty() {
            self.set_status("there are no themes to choose from", Level::Bad);
            return;
        }
        self.theme_before = Some((self.theme_name.clone(), self.theme));
        self.theme_sel = self
            .themes
            .entries
            .iter()
            .position(|named| named.name == self.theme_name)
            .unwrap_or(0);
        self.mode = Mode::Themes;
    }

    fn theme_key(&mut self, key: KeyEvent) {
        let last = self.themes.entries.len().saturating_sub(1);
        let step = |at: usize, by: isize| (at as isize + by).clamp(0, last as isize) as usize;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.theme_sel = step(self.theme_sel, 1),
            KeyCode::Up | KeyCode::Char('k') => self.theme_sel = step(self.theme_sel, -1),
            KeyCode::PageDown => self.theme_sel = step(self.theme_sel, 8),
            KeyCode::PageUp => self.theme_sel = step(self.theme_sel, -8),
            KeyCode::Home => self.theme_sel = 0,
            KeyCode::End => self.theme_sel = last,
            // Keep the one on screen. It is already what you are looking at,
            // so this only writes it down.
            KeyCode::Enter => {
                let named = self.themes.entries[self.theme_sel.min(last)].clone();
                self.theme_before = None;
                self.mode = Mode::Settings;
                self.set_theme(named);
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                if let Some((name, theme)) = self.theme_before.take() {
                    self.theme_name = name;
                    self.theme = theme;
                }
                self.mode = Mode::Settings;
                self.set_status("theme unchanged", Level::Info);
                return;
            }
            _ => return,
        }
        self.preview_theme();
    }

    /// Draw in the theme the cursor is on without writing it down: choosing by
    /// looking is the whole point of a list of colours.
    fn preview_theme(&mut self) {
        if let Some(named) = self.themes.entries.get(self.theme_sel) {
            self.theme = named.theme;
            self.theme_name = named.name.clone();
        }
    }

    // ---- which key asks for what --------------------------------------------

    pub fn open_keys(&mut self) {
        self.action_sel = self.action_sel.min(Action::ALL.len() - 1);
        self.rebinding = None;
        self.mode = Mode::Keys;
    }

    /// The action the cursor is on.
    pub fn selected_action(&self) -> Action {
        Action::ALL[self.action_sel.min(Action::ALL.len() - 1)]
    }

    fn keys_key(&mut self, key: KeyEvent) {
        // Waiting on a key: whatever is pressed is the answer, so nothing else
        // can be read out of it. Esc is the one way out, since a rebind you
        // cannot get out of would be a trap.
        if let Some(action) = self.rebinding {
            self.rebinding = None;
            if key.code == KeyCode::Esc {
                self.set_status("left as it was", Level::Info);
                return;
            }
            self.rebind(action, crate::keys::Chord::of(&key));
            return;
        }
        let last = Action::ALL.len() - 1;
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.action_sel = (self.action_sel + 1).min(last);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.action_sel = self.action_sel.saturating_sub(1);
            }
            KeyCode::PageDown => self.action_sel = (self.action_sel + 10).min(last),
            KeyCode::PageUp => self.action_sel = self.action_sel.saturating_sub(10),
            KeyCode::Home => self.action_sel = 0,
            KeyCode::End => self.action_sel = last,
            KeyCode::Enter => {
                self.rebinding = Some(self.selected_action());
                let action = self.selected_action();
                self.set_status(
                    format!("press the key for {} — Esc leaves it", action.name()),
                    Level::Info,
                );
            }
            KeyCode::Delete | KeyCode::Backspace => {
                let action = self.selected_action();
                self.keymap.reset(action);
                self.save_keys(format!("{}: back to the key it ships with", action.name()));
            }
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Settings,
            _ => {}
        }
    }

    /// Give an action a key, taking it off whatever had it.
    fn rebind(&mut self, action: Action, chord: crate::keys::Chord) {
        let taken = self.keymap.bind(action, chord);
        let done = match taken {
            Some(from) => format!(
                "{} is {chord} — taken from {}, which now has no key",
                action.name(),
                from.name()
            ),
            None => format!("{} is {chord}", action.name()),
        };
        self.save_keys(done);
    }

    /// Write the keys back, saying so either way.
    fn save_keys(&mut self, done: String) {
        self.config.keys = self.keymap.overrides();
        self.save_config(done);
    }

    /// Change a setting: ask for a value, or step to the next one there is.
    ///
    /// `step` only means anything to a setting with a list to walk; typing
    /// one in has no direction to go.
    fn change_setting(&mut self, setting: Setting, step: isize) {
        match setting.kind() {
            Kind::Text => self.ask_for_setting(setting),
            Kind::Choice => match setting {
                Setting::Theme => self.set_theme(self.themes.cycle(&self.theme_name, step)),
                Setting::Background => self.toggle_background(),
                Setting::ShellColours => self.toggle_shell_colours(),
                Setting::Watch => self.toggle_watch(),
                Setting::Resume => self.toggle_resume(),
                Setting::Keys => self.open_keys(),
                Setting::Editor | Setting::EditorOpen | Setting::Shell => {}
            },
        }
    }

    /// Ask for a new value, coming back to the settings pane afterwards.
    fn ask_for_setting(&mut self, setting: Setting) {
        let (kind, title, current) = match setting {
            Setting::Editor => (
                PromptKind::SetEditor,
                "Editor (empty uses $VISUAL, then $EDITOR)".to_string(),
                self.config.editor.clone().unwrap_or_default(),
            ),
            Setting::EditorOpen => (
                PromptKind::SetEditorOpen,
                "Keys that open {file} — empty asks your editor's own".to_string(),
                self.config.editor_open.clone().unwrap_or_default(),
            ),
            Setting::Shell => (
                PromptKind::SetShell,
                format!(
                    "Shell for a shell pane (empty uses {})",
                    crate::config::default_shell()
                ),
                self.config.shell.clone().unwrap_or_default(),
            ),
            // Nothing to type: they are chosen from a list.
            Setting::Theme
            | Setting::Background
            | Setting::ShellColours
            | Setting::Watch
            | Setting::Resume
            | Setting::Keys => {
                return;
            }
        };
        self.open_prompt(kind, title, current);
    }

    fn clear_setting(&mut self, setting: Setting) {
        match setting {
            Setting::Editor => self.set_editor(String::new()),
            Setting::EditorOpen => self.set_editor_open(String::new()),
            Setting::Shell => self.set_shell(String::new()),
            Setting::Background => {
                self.config.background = None;
                self.save_config("background: the theme's own again".into());
            }
            Setting::ShellColours => {
                self.config.shell_colours = None;
                self.save_config("shell colours: the theme's own again".into());
            }
            Setting::Watch => {
                self.config.watch = None;
                self.save_config("the file lists follow their directories again".into());
            }
            Setting::Resume => {
                self.config.resume = None;
                self.save_config("starting up offers the last session again".into());
            }
            Setting::Keys => {
                self.keymap = Keymap::default();
                self.config.keys.clear();
                self.save_config("keys: the ones sshman ships, all of them".into());
            }
            Setting::Theme => {
                self.config.theme = None;
                self.theme_name = theme::DEFAULT.to_string();
                self.theme = self.themes.by_name(&self.theme_name).unwrap_or_default();
                self.save_config(format!("theme cleared — back to {}", self.theme_name));
            }
        }
    }

    /// Paint the theme's background, or leave the terminal's own showing
    /// through.
    ///
    /// Painting one is ordinary cell painting inside the alternate screen —
    /// what any full-screen program does — so this is only ever about what is
    /// on the screen now. Nothing about the terminal is changed, and leaving
    /// sshman puts it back whichever way this is set.
    fn toggle_background(&mut self) {
        let paint = !self.config.paint_background();
        self.config.background = Some(match paint {
            true => "theme".into(),
            false => "terminal".into(),
        });
        let done = match (paint, self.theme.bg) {
            (false, _) => "background: whatever the terminal is set to".to_string(),
            (true, Color::Reset) => format!(
                "background: the theme's own — though {} names none, being the theme that does not",
                self.theme_name
            ),
            (true, _) => format!("background: {}'s own", self.theme_name),
        };
        self.save_config(done);
    }

    /// Colour a shell pane's own output from the theme, or leave the
    /// terminal's palette to it.
    ///
    /// The same idea as the background, and for the same reason: for those
    /// panes sshman is the terminal emulator, so the colour scheme is its to
    /// set. Only the sixteen a program asks for by number are touched — a
    /// program that named an exact colour gets the colour it named.
    /// Have the file lists follow their directories, or leave them showing
    /// what they read when they read it.
    ///
    /// Off is for a directory that is expensive to look at — a network mount
    /// that wakes a spinning disk, a server you are being careful with — and
    /// for anyone who would rather a list held still while they worked in it.
    /// The reload key is unaffected either way.
    fn toggle_watch(&mut self) {
        let follow = !self.config.watching();
        self.config.watch = Some(match follow {
            true => "on".into(),
            false => "off".into(),
        });
        let done = match follow {
            true => "the file lists keep up with their directories".to_string(),
            false => format!(
                "the file lists hold still — {} refreshes them",
                self.keymap.shown(Action::Reload)
            ),
        };
        self.save_config(done);
    }

    /// Whether starting up asks about the session before this one.
    fn toggle_resume(&mut self) {
        let offer = !self.config.offering_resume();
        self.config.resume = Some(match offer {
            true => "on".into(),
            false => "off".into(),
        });
        let done = match offer {
            true => "starting up offers the session before this one".to_string(),
            false => format!(
                "starting up says nothing — {} still has it, or `sshman --resume`",
                self.keymap.shown(Action::Workspaces)
            ),
        };
        self.save_config(done);
    }

    fn toggle_shell_colours(&mut self) {
        let theirs = !self.config.theme_the_shell();
        self.config.shell_colours = Some(match theirs {
            true => "theme".into(),
            false => "terminal".into(),
        });
        let done = match (theirs, self.theme.bg) {
            (false, _) => "shell colours: the terminal's own palette".to_string(),
            (true, Color::Reset) => format!(
                "shell colours: the theme's — though {} has none of its own, being the theme that has not",
                self.theme_name
            ),
            (true, _) => format!("shell colours: {}'s own", self.theme_name),
        };
        self.save_config(done);
    }

    /// What to paint behind everything, or [`Color::Reset`] for whatever the
    /// terminal is already set to.
    pub fn background(&self) -> Color {
        match self.config.paint_background() {
            true => self.theme.bg,
            false => Color::Reset,
        }
    }

    /// The sixteen a shell pane's output is coloured from, or `None` to leave
    /// the terminal's own palette to it.
    ///
    /// A theme that paints no background is not offered: it has not taken the
    /// screen over, so the pairing of its colours with whatever is behind them
    /// is not one it chose.
    pub fn shell_palette(&self) -> Option<[Color; 16]> {
        let owns_the_screen = self.theme.bg != Color::Reset;
        (self.config.theme_the_shell() && owns_the_screen).then_some(self.theme.ansi)
    }

    /// Draw in these colours from now on, and next time.
    fn set_theme(&mut self, named: theme::Named) {
        self.theme = named.theme;
        self.theme_name = named.name.clone();
        self.config.theme = Some(named.name.clone());
        match named.about {
            Some(about) => self.save_config(format!("theme: {} — {about}", named.name)),
            None => self.save_config(format!("theme: {}", named.name)),
        }
    }

    /// Write the settings back, saying so either way. A setting that silently
    /// failed to save is worse than one that never claimed to.
    fn save_config(&mut self, done: String) {
        match self.config.save() {
            Ok(()) => self.set_status(done, Level::Good),
            Err(e) => self.set_status(
                format!("{done}, but it could not be saved: {e}"),
                Level::Bad,
            ),
        }
    }

    /// Remember the keystrokes that open a file in an editor pane. An empty
    /// answer goes back to the ones sshman knows for your editor.
    fn set_editor_open(&mut self, value: String) {
        let value = value.trim().to_string();
        self.config.editor_open = (!value.is_empty()).then_some(value);
        let done = match self.config.editor_open.is_some() {
            true => "an editor pane opens files with the keys you gave".to_string(),
            false => {
                let editor = self.editor.clone();
                match self.config.editor_open(&editor).is_empty() {
                    true => format!("cleared — an editor pane will run {editor} at its prompt"),
                    false => format!("cleared — back to the keys sshman knows for {editor}"),
                }
            }
        };
        self.save_config(done);
    }

    /// Remember which editor to open files with.
    ///
    /// An empty answer clears the setting rather than storing a blank one, so
    /// the way back to `$VISUAL` and `$EDITOR` is to rub it out.
    fn set_editor(&mut self, value: String) {
        let value = value.trim().to_string();
        self.config.editor = (!value.is_empty()).then_some(value);
        self.editor = self.config.editor();
        let editor = self.editor.clone();
        let done = match self.config.editor.is_some() {
            true => format!("editor set to {editor}"),
            false => format!("editor cleared — using {editor}"),
        };
        self.save_config(done);
    }

    /// The shell a new shell pane starts, here and on the servers.
    ///
    /// It takes for the next pane opened, not for the ones already running: a
    /// shell you are in the middle of using is not something to restart out
    /// from under you.
    fn set_shell(&mut self, value: String) {
        let value = value.trim().to_string();
        self.config.shell = (!value.is_empty()).then_some(value);
        crate::shell::set_default_shell(self.config.shell().map(str::to_string));
        let done = match self.config.shell() {
            Some(shell) => format!("new shell panes will run {shell}"),
            None => format!(
                "shell cleared — new panes use {} here, and the login shell on a server",
                crate::config::default_shell()
            ),
        };
        self.save_config(done);
    }

    fn confirm_key(&mut self, key: KeyEvent) {
        // Dialogs guarded by a phrase are text entry first: `y` is just a
        // letter there, and only an exact match plus Enter goes ahead.
        let guarded = self
            .confirm
            .as_ref()
            .is_some_and(|c| c.require_phrase.is_some());
        if guarded && !matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            if let Some(state) = self.confirm.as_mut() {
                state.input.handle(key);
            }
            return;
        }

        // The quit dialog answers to the quit key too, so pressing `q` twice
        // leaves — the dialog is a guard against the stray first press, not a
        // toll on everyone who meant it.
        if matches!(
            self.confirm.as_ref().map(|c| &c.action),
            Some(ConfirmAction::Quit)
        ) && self.keymap.action(&key) == Some(Action::Quit)
        {
            self.ask_quit();
            return;
        }

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                if self.confirm.as_ref().is_some_and(|c| !c.satisfied()) {
                    let word = self
                        .confirm
                        .as_ref()
                        .and_then(|c| c.require_phrase.clone())
                        .unwrap_or_default();
                    self.set_status(
                        format!("type {word} to confirm, or Esc to cancel"),
                        Level::Bad,
                    );
                    return;
                }
                let Some(state) = self.confirm.take() else {
                    self.mode = Mode::Browse;
                    return;
                };
                self.mode = state.return_to;
                match state.action {
                    ConfirmAction::Quit => self.pending_action = Some(UiAction::Quit),
                    ConfirmAction::DeleteLocal(paths) => {
                        let mut failed = Vec::new();
                        let total = paths.len();
                        for p in &paths {
                            if let Err(e) = local::remove(p) {
                                failed.push(e.to_string());
                            }
                        }
                        for tree in &mut self.local {
                            tree.pane.marked.clear();
                        }
                        self.reload_local();
                        if failed.is_empty() {
                            self.set_status(format!("{total} item(s) deleted"), Level::Good);
                        } else {
                            self.set_status(failed.join("; "), Level::Bad);
                        }
                    }
                    ConfirmAction::DeleteRemote(paths) => {
                        if let Some(tab) = self.tabs.get_mut(self.active) {
                            for tree in &mut tab.trees {
                                tree.pane.marked.clear();
                            }
                        }
                        self.send(Req::Delete {
                            paths,
                            sudo: self.sudo(),
                        });
                    }
                    ConfirmAction::AcceptHostKey => {
                        self.opts.accept_new_host_key = true;
                        self.start_connect();
                    }
                    ConfirmAction::ReplaceHostKey => {
                        self.opts.replace_host_key = true;
                        self.set_status(
                            format!("replacing the recorded key for {}", self.opts.host),
                            Level::Info,
                        );
                        self.start_connect();
                    }
                    ConfirmAction::RestoreSession => self.restore_previous_session(),
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                let was_host_key = matches!(
                    self.confirm.as_ref().map(|c| &c.action),
                    Some(ConfirmAction::AcceptHostKey | ConfirmAction::ReplaceHostKey)
                );
                let back = self
                    .confirm
                    .as_ref()
                    .map(|c| c.return_to)
                    .unwrap_or(Mode::Browse);
                self.confirm = None;
                if was_host_key && !self.connected() {
                    self.mode = Mode::Connect;
                    self.form.error = Some("host key rejected".into());
                } else {
                    self.mode = back;
                }
                self.set_status("cancelled", Level::Info);
            }
            _ => {}
        }
    }

    fn output_key(&mut self, key: KeyEvent) {
        let last = scroll_limit(self.output.len(), self.output_view_height);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') => {
                self.output_scroll = (self.output_scroll + 1).min(last)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.output_scroll = self.output_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.output_scroll = (self.output_scroll + 15).min(last),
            KeyCode::PageUp => self.output_scroll = self.output_scroll.saturating_sub(15),
            KeyCode::Home | KeyCode::Char('g') => self.output_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.output_scroll = last,
            _ => {}
        }
    }

    fn help_key(&mut self, key: KeyEvent) {
        let last = scroll_limit(crate::ui::HELP.len(), self.help_view_height);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.help_scroll = (self.help_scroll + 1).min(last)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.help_scroll = self.help_scroll.saturating_sub(1)
            }
            KeyCode::PageDown => self.help_scroll = (self.help_scroll + 15).min(last),
            KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(15),
            KeyCode::Home | KeyCode::Char('g') => self.help_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.help_scroll = last,
            _ => self.mode = Mode::Browse,
        }
    }

    // ---- actions -----------------------------------------------------------

    fn go_up(&mut self, at: Slot) {
        let cwd = self.dir_of(at);
        if cwd.is_empty() {
            return;
        }
        match at.host() {
            Side::Local => {
                let here = PathBuf::from(&cwd);
                let Some(parent) = here.parent().map(Path::to_path_buf) else {
                    return;
                };
                let leaving = here.file_name().map(|n| n.to_string_lossy().to_string());
                self.goto_local(at, parent);
                // Put the cursor back on the directory we just left.
                if let Some(name) = leaving
                    && let Some(i) = self.pane(at).view.iter().position(|e| e.name == name)
                {
                    self.pane_mut(at).select_index(i);
                }
            }
            Side::Remote => {
                if cwd == "/" {
                    return;
                }
                let leaving = rbasename(&cwd);
                self.goto_remote(at, rparent(&cwd));
                if let Some(tree) = self
                    .tabs
                    .get_mut(self.active)
                    .and_then(|tab| tab.tree_mut(at.id()))
                {
                    tree.pending_select = Some(leaving);
                }
            }
        }
    }

    fn activate(&mut self, at: Slot) {
        let Some(entry) = self.pane(at).selected().cloned() else {
            return;
        };
        if entry.is_dir_like() {
            let dir = self.dir_of(at);
            match at.host() {
                Side::Local => self.goto_local(at, PathBuf::from(dir).join(&entry.name)),
                Side::Remote => self.goto_remote(at, rjoin(&dir, &entry.name)),
            }
        } else {
            self.edit_selected(at);
        }
    }

    /// Open the file under the cursor in your editor: in the editor pane when
    /// this machine has one, and otherwise by standing aside for it.
    fn edit_selected(&mut self, at: Slot) {
        let Some(entry) = self.pane(at).selected().cloned() else {
            return;
        };
        if entry.is_dir_like() {
            self.set_status("that is a directory — press Enter to open it", Level::Info);
            return;
        }
        if self.send_to_editor(at, &entry.name) {
            return;
        }
        let program = self.editor.clone();
        self.launch_on(at, &entry.name, program);
    }

    fn open_with(&mut self, at: Slot, program: String) {
        let Some(entry) = self.pane(at).selected().cloned() else {
            return;
        };
        if entry.is_dir_like() {
            self.set_status("that is a directory — press Enter to open it", Level::Info);
            return;
        }
        self.launch_on(at, &entry.name, program);
    }

    /// Where `name` really is, when it is on this machine.
    ///
    /// The left pane's files are here by definition, and so are a "this
    /// machine" tab's: that tab reaches the same filesystem through the same
    /// code a server uses, but nothing has to travel to get at it. Which side
    /// of the screen a pane is on says nothing about that.
    ///
    /// Sudo mode is the exception. A root-owned file is not ours to open, so
    /// it goes the long way round — fetched as root, edited, pushed back as
    /// root — which is the only way to edit it at all.
    fn on_this_machine(&self, at: Slot, name: &str) -> Option<PathBuf> {
        let dir = self.dir_of(at);
        if dir.is_empty() {
            return None;
        }
        match at.host() {
            Side::Local => Some(PathBuf::from(dir).join(name)),
            Side::Remote if self.on_local_tab() && !self.sudo() => {
                Some(PathBuf::from(rjoin(&dir, name)))
            }
            Side::Remote => None,
        }
    }

    /// Open `name` on `side` with `program`.
    ///
    /// A file on this machine goes straight to the editor, at its own path —
    /// so an editor that looks around itself for a project finds the real
    /// tree, and a save is a save rather than a copy back. Anything else is
    /// fetched first and pushed back when the editor exits.
    fn launch_on(&mut self, at: Slot, name: &str, program: String) {
        if let Some(path) = self.on_this_machine(at, name) {
            if path.is_dir() {
                self.set_status("that is a directory", Level::Info);
                return;
            }
            self.pending_action = Some(UiAction::Editor {
                program,
                path,
                push_back: None,
                refresh: match at.host() {
                    Side::Local => Refresh::Local,
                    Side::Remote => Refresh::Remote,
                },
            });
            return;
        }
        if !self.connected() {
            self.set_status("not connected", Level::Bad);
            return;
        }
        self.send(Req::FetchForEdit {
            path: rjoin(&self.dir_of(at), name),
            sudo: self.sudo(),
            editor: program,
        });
    }

    /// Copy what is marked into the other file list on screen.
    ///
    /// Across the middle that is an upload or a download. Between two lists on
    /// the same machine it is one command run there, with nothing travelling
    /// in either direction — the same thing `P` does, and for the same reason.
    fn copy_to_target(&mut self) {
        let from = self.focus;
        let Some(to) = self.target() else {
            self.set_status("no other file list on screen to copy to", Level::Info);
            return;
        };
        if (from.host() == Side::Remote || to.host() == Side::Remote) && !self.connected() {
            self.set_status("not connected", Level::Bad);
            return;
        }
        let names = self.pane(from).targets();
        if names.is_empty() {
            self.set_status("nothing selected", Level::Info);
            return;
        }
        let (src, dest) = (self.dir_of(from), self.dir_of(to));
        if src.is_empty() || dest.is_empty() {
            self.set_status("that pane has no directory yet", Level::Info);
            return;
        }
        if from.host() == to.host() && src == dest {
            self.set_status("both lists are in the same directory", Level::Info);
            return;
        }
        let count = names.len();
        let sudo = self.sudo();
        match (from.host(), to.host()) {
            (Side::Local, Side::Remote) => self.send(Req::Upload {
                items: names.iter().map(|n| PathBuf::from(&src).join(n)).collect(),
                dest,
                sudo,
            }),
            (Side::Remote, Side::Local) => self.send(Req::Download {
                items: names.iter().map(|n| rjoin(&src, n)).collect(),
                dest: PathBuf::from(dest),
                sudo,
            }),
            (Side::Remote, Side::Remote) => self.send(Req::Paste {
                dir: src,
                names: names.clone(),
                dest,
                cut: false,
                sudo,
            }),
            (Side::Local, Side::Local) => {
                let cmd = crate::fileops::paste_command(
                    &src,
                    &names,
                    &dest,
                    crate::fileops::Action::Copy,
                );
                self.spawn_local_command(
                    format!("copying {count} item(s)"),
                    cmd,
                    format!("{count} item(s) copied into {dest}"),
                );
            }
        }
        self.pane_mut(from).marked.clear();
    }

    /// Pick up what is marked, to be put down elsewhere on the same side.
    ///
    /// This is what `c` does when one pane fills the screen: there is no other
    /// side on show to copy to, and the useful thing to do with a selection is
    /// carry it to another directory of the same filesystem.
    fn yank(&mut self, cut: bool) {
        let at = self.focus;
        if at.host() == Side::Remote && !self.connected() {
            self.set_status("not connected", Level::Bad);
            return;
        }
        let names = self.pane(at).targets();
        if names.is_empty() {
            self.set_status("nothing selected", Level::Info);
            return;
        }
        let count = names.len();
        self.clip = Some(Clip {
            side: at.host(),
            dir: self.dir_of(at),
            names,
            cut,
        });
        self.pane_mut(at).marked.clear();
        let verb = if cut { "cut" } else { "copied" };
        self.set_status(
            format!("{count} item(s) {verb} — go to a directory and press P"),
            Level::Good,
        );
    }

    /// Put what `c` or `M` picked up into the directory on screen.
    fn paste_clip(&mut self) {
        let Some(clip) = self.clip.clone() else {
            self.set_status("nothing to paste — c copies, M cuts", Level::Info);
            return;
        };
        let at = self.focus;
        let side = at.host();
        // Both halves of a paste run as one command on one machine, so the
        // clipboard cannot cross the middle. `c` with another list on screen
        // is the key that copies between the two.
        if clip.side != side {
            self.set_status(
                format!(
                    "that came from the {} side — Tab back to it, or use c to copy across",
                    side_name(clip.side)
                ),
                Level::Bad,
            );
            return;
        }
        if side == Side::Remote && !self.connected() {
            self.set_status("not connected", Level::Bad);
            return;
        }
        let dest = self.dir_of(at);
        let action = clip.action();
        let count = clip.names.len();
        let verb = action.past_tense();

        match side {
            Side::Local => {
                let cmd = crate::fileops::paste_command(&clip.dir, &clip.names, &dest, action);
                self.spawn_local_command(
                    format!("{verb} {count} item(s)"),
                    cmd,
                    format!("{count} item(s) {verb} into {dest}"),
                );
            }
            Side::Remote => self.send(Req::Paste {
                dir: clip.dir.clone(),
                names: clip.names.clone(),
                dest,
                cut: clip.cut,
                sudo: self.sudo(),
            }),
        }
        // A copy can land in several places; a move only happens once.
        if clip.cut {
            self.clip = None;
        }
    }

    fn request_delete(&mut self, at: Slot) {
        let names = self.pane(at).targets();
        if names.is_empty() {
            self.set_status("nothing selected", Level::Info);
            return;
        }
        let mut body = vec![format!(
            "Permanently delete {} item(s) from {}:",
            names.len(),
            self.path_of(at)
        )];
        for n in names.iter().take(10) {
            body.push(format!("  {n}"));
        }
        if names.len() > 10 {
            body.push(format!("  … and {} more", names.len() - 10));
        }
        if at.host() == Side::Remote && self.sudo() {
            body.push(String::new());
            body.push("SUDO MODE IS ON — this deletes as root.".into());
        }

        let dir = self.dir_of(at);
        let action = match at.host() {
            Side::Local => ConfirmAction::DeleteLocal(
                names.iter().map(|n| PathBuf::from(&dir).join(n)).collect(),
            ),
            Side::Remote => {
                ConfirmAction::DeleteRemote(names.iter().map(|n| rjoin(&dir, n)).collect())
            }
        };
        self.confirm = Some(ConfirmState::simple("Confirm delete", body, action, true));
        self.mode = Mode::Confirm;
    }

    /// Jump a pane to its home directory. On the remote side that is the
    /// directory the server put us in at login.
    fn go_home(&mut self, at: Slot) {
        match at.host() {
            Side::Local => {
                if let Some(home) = dirs::home_dir() {
                    self.goto_local(at, home);
                }
            }
            Side::Remote => {
                if let Some(home) = self.tab().map(|t| t.conn.home.clone()) {
                    self.goto_remote(at, home);
                }
            }
        }
    }

    /// Copy our public key to the server's `authorized_keys`.
    fn install_public_key(&mut self) {
        match find_public_key(&self.opts) {
            Ok((path, key)) => {
                self.set_status(format!("installing {}…", path.display()), Level::Info);
                self.send(Req::InstallKey { public_key: key });
            }
            Err(e) => self.set_status(e, Level::Bad),
        }
    }

    /// Text pasted into the terminal, delivered as one event because bracketed
    /// paste is on. It has to reach whatever is currently taking input — the
    /// connection form, an open prompt, or a focused shell.
    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.mode {
            // A shell is a real terminal: hand it the text exactly as it came,
            // newlines and all.
            Mode::Browse if self.in_term() => {
                if let Some(shell) = self.shell_mut(self.focus) {
                    shell.paste(text);
                }
            }
            Mode::Connect => {
                let Some(input) = self.form.current() else {
                    return; // the checkbox has focus; nowhere to put text
                };
                let trimmed = input.insert_str(text);
                self.note_trimmed_paste(trimmed);
            }
            Mode::Prompt => {
                let Some(prompt) = self.prompt.as_mut() else {
                    return;
                };
                let trimmed = prompt.input.insert_str(text);
                // Filtering happens as you type, so a pasted filter has to
                // narrow the list straight away too.
                let live_filter = match prompt.kind {
                    PromptKind::Filter(side) => Some((side, prompt.input.value.clone())),
                    _ => None,
                };
                if let Some((side, value)) = live_filter {
                    let keep = self.pane(side).selected_name();
                    let pane = self.pane_mut(side);
                    pane.filter = value;
                    pane.refresh_view(keep.as_deref());
                }
                self.note_trimmed_paste(trimmed);
            }
            // Nothing on screen is taking text.
            _ => {}
        }
    }

    fn note_trimmed_paste(&mut self, trimmed: bool) {
        if trimmed {
            self.set_status(
                "pasted the first line only — this field holds a single line",
                Level::Info,
            );
        }
    }

    /// Give the server on screen a name.
    fn start_rename_tab(&mut self) {
        let Some(tab) = self.tab() else {
            self.set_status("no server on screen to name", Level::Bad);
            return;
        };
        let current = tab.name.clone().unwrap_or_default();
        let address = tab.address();
        self.open_prompt(
            PromptKind::NameTab,
            format!("Name for {address} (empty clears it)"),
            current,
        );
    }

    // ---- port forwards ------------------------------------------------------

    /// Index of the forward the list is pointing at, kept valid as the list
    /// changes underneath it.
    pub fn forward_sel(&self) -> usize {
        self.forward_sel
            .min(self.tab().map_or(0, |t| t.forwards.len()).saturating_sub(1))
    }

    fn open_forwards(&mut self) {
        let Some((local_container, empty)) = self.tab().map(|t| {
            (
                t.is_container() && t.ssh_opts().is_none(),
                t.forwards.is_empty(),
            )
        }) else {
            self.set_status("connect to a server first", Level::Bad);
            return;
        };
        if local_container {
            self.set_status(
                "a container here already publishes its ports — forward from a server instead",
                Level::Info,
            );
            return;
        }
        self.mode = Mode::Forwards;
        if empty {
            self.set_status("no forwards yet — a adds one", Level::Info);
        }
    }

    fn forward_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') => self.forward_sel += 1,
            KeyCode::Up | KeyCode::Char('k') => {
                self.forward_sel = self.forward_sel.saturating_sub(1)
            }
            KeyCode::Char('a') => self.open_prompt(
                PromptKind::AddForward,
                "Forward port (3000, 8080:3000, 8080:host:3000, 0.0.0.0:8080:host:3000)".into(),
                String::new(),
            ),
            KeyCode::Delete | KeyCode::Char('d') => {
                let index = self.forward_sel();
                let Some(tab) = self.tabs.get_mut(self.active) else {
                    return;
                };
                if index < tab.forwards.len() {
                    // Dropping it closes the listener and its connections.
                    let gone = tab.forwards.remove(index);
                    self.set_status(
                        format!("stopped forwarding {}", gone.spec.describe()),
                        Level::Info,
                    );
                }
            }
            _ => {}
        }
    }

    fn add_forward(&mut self, text: &str) {
        let spec = match ForwardSpec::parse(text) {
            Ok(spec) => spec,
            Err(e) => {
                self.set_status(e.to_string(), Level::Bad);
                return;
            }
        };
        let Some(tab) = self.tab() else {
            self.set_status("not connected", Level::Bad);
            return;
        };
        // A container has no SSH of its own; forward over the server hosting it.
        let Some(opts) = tab.ssh_opts().cloned() else {
            self.set_status("this tab has no server to forward over", Level::Bad);
            return;
        };
        if tab
            .forwards
            .iter()
            .any(|f| f.spec.local_port == spec.local_port && f.spec.local_host == spec.local_host)
        {
            self.set_status(
                format!("port {} is already forwarded here", spec.local_port),
                Level::Bad,
            );
            return;
        }

        match Forward::start(&opts, spec.clone()) {
            Ok(forward) => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.forwards.push(forward);
                }
                // Binding past loopback is worth saying out loud: it is the
                // difference between a port for you and a port for the
                // network you are on.
                self.set_status(
                    match spec.is_public() {
                        true => format!(
                            "forwarding {} — reachable from the network",
                            spec.describe()
                        ),
                        false => format!("forwarding {}", spec.describe()),
                    },
                    Level::Good,
                );
            }
            Err(e) => self.set_status(e.to_string(), Level::Bad),
        }
    }

    /// Every forward across every tab, for the title bar's count.
    pub fn forward_count(&self) -> usize {
        self.tabs.iter().map(|t| t.forwards.len()).sum()
    }

    /// Restart the forwards a workspace recorded for a tab.
    fn restore_forwards(&mut self, index: usize, specs: &[String]) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let Some(opts) = tab.ssh_opts().cloned() else {
            return;
        };
        let via = tab.title();
        let mut started = 0;
        let mut problems = Vec::new();
        for text in specs {
            match ForwardSpec::parse(text)
                .and_then(|spec| Forward::start(&opts, spec.clone()).map(|f| (spec, f)))
            {
                Ok((_, forward)) => {
                    if let Some(tab) = self.tabs.get_mut(index) {
                        tab.forwards.push(forward);
                    }
                    started += 1;
                }
                Err(e) => problems.push(format!("{text}: {e}")),
            }
        }
        if started > 0 {
            self.set_status(format!("{via}: {started} port(s) forwarded"), Level::Good);
        }
        if !problems.is_empty() {
            self.set_status(
                format!("{via}: could not forward {}", problems.join(", ")),
                Level::Bad,
            );
        }
    }

    // ---- workspaces ---------------------------------------------------------

    pub fn open_workspaces(&mut self) {
        self.workspace_sel = self
            .workspace_sel
            .min(self.workspace_rows().saturating_sub(1));
        self.mode = Mode::Workspaces;
        if self.workspaces.is_empty() && self.previous_session.is_none() {
            self.set_status(
                "no workspaces yet — s saves what you have open now",
                Level::Info,
            );
        }
    }

    /// How many rows the workspace list has: the saved ones, and the session
    /// before this one above them when there was one.
    pub fn workspace_rows(&self) -> usize {
        self.workspaces.len() + usize::from(self.previous_session.is_some())
    }

    /// Whether the first row is the session before this one rather than a
    /// workspace. It is listed there because it is the one people reach for
    /// most and the one nobody thought to save.
    pub fn session_row(&self) -> bool {
        self.previous_session.is_some()
    }

    /// The saved workspace a row stands for, or `None` for the session row.
    pub fn workspace_at(&self, row: usize) -> Option<usize> {
        match self.session_row() {
            true if row == 0 => None,
            true => Some(row - 1),
            false => Some(row),
        }
    }

    fn workspace_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') => {
                self.workspace_sel =
                    (self.workspace_sel + 1).min(self.workspace_rows().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.workspace_sel = self.workspace_sel.saturating_sub(1);
            }
            KeyCode::Char('s') => {
                // Naming it after the row you are on replaces that workspace,
                // which is what `s` on top of one is usually for. The session
                // row is not a name anyone means to save under.
                let suggestion = self
                    .workspace_at(self.workspace_sel)
                    .and_then(|index| self.workspaces.get(index))
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                self.open_prompt(
                    PromptKind::SaveWorkspace,
                    format!("Save these {} connection(s) as", self.tabs.len()),
                    suggestion,
                );
            }
            KeyCode::Delete => {
                match self.workspace_at(self.workspace_sel) {
                    Some(index) => {
                        if let Some(removed) = self.workspaces.remove(index) {
                            self.set_status(
                                format!("forgot workspace {}", removed.name),
                                Level::Info,
                            );
                        }
                    }
                    // Forgetting the last session is forgetting a file, not a
                    // list entry: the one being written down now takes its
                    // place the moment anything changes.
                    None => {
                        crate::workspace::Session::forget();
                        self.previous_session = None;
                        self.set_status("forgot the previous session", Level::Info);
                    }
                }
                self.workspace_sel = self
                    .workspace_sel
                    .min(self.workspace_rows().saturating_sub(1));
            }
            KeyCode::Enter => {
                self.mode = Mode::Browse;
                match self.workspace_at(self.workspace_sel) {
                    Some(index) => self.launch_workspace_at(index),
                    None => self.restore_previous_session(),
                }
            }
            _ => {}
        }
    }

    /// Ask, on the way in, whether to pick up where the last session left
    /// off.
    ///
    /// The session is written down as you work, so there is always something
    /// to come back to; what there was not, until this, was any reminder of
    /// it. `--resume` only helps someone who remembered to type it, which is
    /// nobody at the moment they wanted it. Saying no leaves the session
    /// where it is — it is still the first row of the workspace list (`w`)
    /// for the rest of the run.
    pub fn offer_previous_session(&mut self) {
        let Some(session) = self
            .previous_session
            .clone()
            .filter(|s| !s.items.is_empty())
        else {
            return;
        };
        let mut body = vec![
            format!(
                "{}, last open {}:",
                session.summary(),
                crate::history::relative_time(session.saved_at)
            ),
            String::new(),
        ];
        // Name them rather than counting them: "3 connections" is not enough
        // to decide by, and the names are what you remember them as. A very
        // long session is summarised at the end instead of growing a dialog
        // taller than the terminal.
        const SHOWN: usize = 8;
        for item in session.items.iter().take(SHOWN) {
            body.push(format!(
                "  {}",
                crate::types::ellipsize(&item.describe(), 68)
            ));
        }
        if let Some(rest) = session.items.len().checked_sub(SHOWN).filter(|n| *n > 0) {
            body.push(format!("  … and {rest} more"));
        }
        body.push(String::new());
        body.push(format!(
            "n starts fresh — {} brings this back later either way.",
            self.keymap.shown(Action::Workspaces)
        ));

        let mut state = ConfirmState::simple(
            "Come back to your last session?",
            body,
            ConfirmAction::RestoreSession,
            false,
        );
        // Asked over the connection screen, which is where saying no leaves
        // you: there are no file panes to fall back to yet.
        state.return_to = self.mode;
        self.confirm = Some(state);
        self.mode = Mode::Confirm;
    }

    /// Open everything the last session had open.
    pub fn restore_previous_session(&mut self) {
        let Some(session) = self.previous_session.clone() else {
            self.set_status("no previous session to come back to", Level::Info);
            return;
        };
        if session.items.is_empty() {
            self.set_status("the previous session had nothing open", Level::Info);
            return;
        }
        self.launch_workspace(&session);
    }

    /// Turn the tabs on screen into something that can be saved.
    fn workspace_items(&self) -> Vec<WorkspaceItem> {
        self.tabs.iter().filter_map(workspace_item_for).collect()
    }

    /// Where this machine's panes are pointed. They are shared between the
    /// tabs, so they belong to the workspace rather than to any one of them.
    fn local_pane_dirs(&self) -> crate::workspace::PaneDirs {
        pane_dirs(
            self.local
                .iter()
                .map(|tree| (tree.id, tree.cwd.display().to_string())),
            self.local_terms
                .iter()
                .map(|term| (term.id, term.shell.cwd())),
        )
    }

    fn save_workspace(&mut self, name: &str) {
        if self.tabs.is_empty() {
            self.set_status("nothing open to save", Level::Bad);
            return;
        }
        let items = self.workspace_items();
        let skipped = self.tabs.len() - items.len();
        let local = Some(self.local_cwd().display().to_string());
        let local_editors = self
            .local_terms
            .iter()
            .filter(|term| term.is_editor())
            .map(|term| term.id)
            .collect();
        let local_dirs = self.local_pane_dirs();

        match self
            .workspaces
            .save(name, local, local_editors, local_dirs, items)
        {
            Ok(replaced) => {
                let verb = if replaced { "replaced" } else { "saved" };
                let mut msg = format!("{verb} workspace {name}");
                if skipped > 0 {
                    // Only containers reached through another container, which
                    // we cannot rebuild.
                    msg.push_str(&format!(" ({skipped} tab(s) could not be saved)"));
                }
                self.set_status(msg, Level::Good);
            }
            Err(e) => self.set_status(format!("could not save workspace: {e}"), Level::Bad),
        }
    }

    /// How this session looks written down, in the shape a workspace takes.
    ///
    /// Everything a workspace holds and nothing more: what each tab was
    /// connected to, how its panes were arranged, and where every one of
    /// them was pointed. No passwords, because none are kept anywhere.
    pub fn session_snapshot(&self) -> crate::workspace::Workspace {
        crate::workspace::Workspace {
            name: crate::workspace::SESSION_NAME.to_string(),
            local_path: Some(self.local_cwd().display().to_string())
                .filter(|path| !path.is_empty()),
            local_editors: self
                .local_terms
                .iter()
                .filter(|term| term.is_editor())
                .map(|term| term.id)
                .collect(),
            local_dirs: self.local_pane_dirs(),
            items: self.workspace_items(),
            saved_at: crate::workspace::now(),
        }
    }

    /// Keep the file that says where this session got to up to date.
    ///
    /// Called from the main loop rather than on the way out, because there is
    /// no way out to hook: a terminal closed on sshman, a machine that went to
    /// sleep and never woke, a panic — none of them give anyone a chance to
    /// write anything. What is on disk when the process stops is what comes
    /// back, so the answer is to keep it close to true all the time.
    ///
    /// A session nobody is touching costs one comparison a second: the
    /// snapshot is only written when it says something different.
    pub fn cache_session(&mut self) {
        const EVERY: Duration = Duration::from_secs(1);
        if self.cached_session.elapsed() < EVERY {
            return;
        }
        self.cached_session = Instant::now();
        self.write_session();
    }

    /// Write the snapshot down, if it says anything new.
    fn write_session(&mut self) {
        // Nothing open is not a session worth coming back to, and writing it
        // down would throw away the one that is already there — which is
        // exactly what you want on the way out of a workspace you opened by
        // mistake, and exactly what you do not want on the way in.
        if self.tabs.is_empty() {
            return;
        }
        let snapshot = self.session_snapshot();
        // The time of the write moves every second whether or not anything
        // else does, so it is left out of what is compared: it is a note
        // about the write rather than part of what is being written down.
        let Ok(json) = serde_json::to_string(&crate::workspace::Workspace {
            saved_at: 0,
            ..snapshot.clone()
        }) else {
            return;
        };
        if self.session_written.as_deref() == Some(json.as_str()) {
            return;
        }
        if crate::workspace::Session::save(&snapshot).is_ok() {
            self.session_written = Some(json);
        }
    }

    fn launch_workspace_at(&mut self, index: usize) {
        let Some(workspace) = self.workspaces.get(index).cloned() else {
            return;
        };
        self.launch_workspace(&workspace);
    }

    /// Reconnect everything a workspace holds, each on its own worker so they
    /// come up in parallel rather than one after another.
    pub fn launch_workspace(&mut self, workspace: &crate::workspace::Workspace) {
        if let Some(path) = &workspace.local_path {
            let path = crate::local::expand(path);
            if path.is_dir() {
                self.goto_local(Slot::files(Side::Local), path);
            }
        }
        // This machine's panes are shared, so which of them opened files and
        // where each of them was pointed are the workspace's to say rather
        // than any one tab's.
        self.wants_editor = workspace.local_editors.clone();
        self.wants_dirs = workspace.local_dirs.clone();
        if workspace.items.is_empty() {
            self.set_status(
                format!("workspace {} is empty", workspace.name),
                Level::Info,
            );
            return;
        }
        for item in &workspace.items {
            // The saved directory becomes that tab's starting point.
            self.initial_remote = item.path().map(String::from);
            let name = item.name().map(String::from);
            let target = item.to_target();
            let layout = item.layout();
            self.connect_to(target, String::new());
            // `connect_to` copies `initial_remote`, so hand the rest over too.
            if let Some(pending) = self.pending.last_mut() {
                pending.name = name;
                pending.forwards = item.forwards().to_vec();
                // A workspace saved before sizes were remembered has none, and
                // opens with whatever is on screen.
                if let Some(layout) = layout {
                    pending.layout = layout;
                }
                pending.editors = item.editors().to_vec();
                pending.dirs = item.dirs().clone();
            }
        }
        self.initial_remote = None;
        self.set_status(
            format!(
                "opening workspace {} — {}",
                workspace.name,
                workspace.summary()
            ),
            Level::Info,
        );
    }

    /// Open a tab on the machine sshman is running on.
    ///
    /// It behaves like every other tab — two panes, transfers, archives, its
    /// own shell, sudo — with the far side being here. Useful on its own for
    /// moving things about locally, and useful as somewhere to stand when
    /// nothing is worth connecting to.
    pub fn open_local_tab(&mut self) {
        self.mode = Mode::Browse;
        // Your filetree as it is on screen, rather than a home directory you
        // have already navigated away from.
        self.initial_remote = Some(self.local_cwd().display().to_string());
        self.connect_to(Target::Local, String::new());
        self.initial_remote = None;
    }

    // ---- containers ---------------------------------------------------------

    /// Look for containers to open. Which docker daemon is asked follows the
    /// pane you are on: the local pane means this machine, the remote pane
    /// means the server that tab is connected to.
    /// Go straight to the container chooser for this machine, with no server
    /// in the picture — what `--docker` does.
    pub fn browse_local_containers(&mut self) {
        self.mode = Mode::Browse;
        self.find_containers(Slot::files(Side::Local));
    }

    fn find_containers(&mut self, at: Slot) {
        match at.host() {
            Side::Local => {
                self.local_tasks
                    .push("looking for local containers…".into());
                let tx = self.local_tx.clone();
                std::thread::Builder::new()
                    .name("docker-ps".into())
                    .spawn(move || {
                        let found = crate::backend::preferred_runtime();
                        let outcome = match crate::docker::detect_runtime(None, found.as_deref())
                            .and_then(|rt| {
                                crate::docker::list_containers(None, &rt).map(|list| (rt, list))
                            }) {
                            Ok((runtime, list)) => LocalOutcome {
                                message: format!(
                                    "{} container(s) running here ({runtime})",
                                    list.len()
                                ),
                                failed: false,
                                output: None,
                                containers: Some((runtime, list)),
                            },
                            Err(e) => LocalOutcome {
                                message: format!("cannot list containers: {e}"),
                                failed: true,
                                output: None,
                                containers: None,
                            },
                        };
                        let _ = tx.send(outcome);
                    })
                    .expect("spawn docker ps thread");
            }
            Side::Remote => {
                let Some(tab) = self.tab() else {
                    self.set_status("not connected", Level::Bad);
                    return;
                };
                if tab.is_container() {
                    self.set_status(
                        "this tab is already a container — use the local pane for this machine",
                        Level::Info,
                    );
                    return;
                }
                self.send(Req::ListContainers);
            }
        }
    }

    fn open_picker(
        &mut self,
        items: Vec<crate::docker::Container>,
        via: Option<ConnectOpts>,
        runtime: String,
        title: String,
    ) {
        if items.is_empty() {
            self.set_status(format!("{title}: nothing running"), Level::Info);
            return;
        }
        self.picker = Some(PickerState {
            title,
            items,
            selected: 0,
            via,
            runtime,
        });
        self.mode = Mode::Picker;
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.picker.as_mut() else {
            self.mode = Mode::Browse;
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.picker = None;
                self.mode = Mode::Browse;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                picker.selected = (picker.selected + 1).min(picker.items.len() - 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                picker.selected = picker.selected.saturating_sub(1);
            }
            KeyCode::Home | KeyCode::Char('g') => picker.selected = 0,
            KeyCode::End | KeyCode::Char('G') => picker.selected = picker.items.len() - 1,
            KeyCode::Enter => {
                let Some(picker) = self.picker.take() else {
                    return;
                };
                self.mode = Mode::Browse;
                let container = &picker.items[picker.selected];
                let label = container.name.clone();
                self.connect_to(
                    Target::Docker {
                        via: picker.via,
                        // Addressed by id: stable even if it is renamed.
                        container: container.id.clone(),
                        runtime: picker.runtime.clone(),
                    },
                    format!("opening container {label}…"),
                );
            }
            _ => {}
        }
    }

    // ---- tabs ---------------------------------------------------------------

    /// Back out of the connection screen.
    ///
    /// This never quits, even with nothing connected: the local pane, its
    /// shell, archives and local containers all work on their own, so being
    /// dropped out of the program for pressing Escape would be wrong. `q`
    /// quits.
    fn dismiss_connect_screen(&mut self) {
        self.drop_form_attempts();
        self.form.connecting = false;
        self.mode = Mode::Browse;
        if self.tabs.is_empty() {
            self.set_status(
                "nothing connected — C connects, L opens a tab on this machine, q quits",
                Level::Info,
            );
        }
    }

    /// Open the connection screen to add another server.
    pub fn open_connect_screen(&mut self) {
        // A workspace may have left connections waiting on a password; offer
        // those first, filled in, so all that is missing is the password.
        if let Some(waiting) = self.needs_password.first() {
            let (label, opts) = (waiting.label.clone(), waiting.opts.clone());
            self.opts = opts;
            self.form = ConnectForm::new(&self.opts);
            self.form.name.set(label.clone());
            self.connect_focus = ConnectFocus::Form;
            self.form.field = ConnectForm::PASSWORD;
            self.mode = Mode::Connect;
            let remaining = self.needs_password.len();
            self.set_status(
                format!("{label} needs a password ({remaining} waiting)"),
                Level::Info,
            );
            return;
        }

        self.form = ConnectForm::new(&self.opts);
        self.form.host.clear();
        self.connect_focus = if self.history.is_empty() {
            ConnectFocus::Form
        } else {
            ConnectFocus::Recent
        };
        self.history_sel = 0;
        self.mode = Mode::Connect;
        self.set_status(
            "pick a server or type one in — it opens in a new tab",
            Level::Info,
        );
    }

    pub fn cycle_tab(&mut self, delta: isize) {
        if self.tabs.len() < 2 {
            return;
        }
        let len = self.tabs.len() as isize;
        let next = (self.active as isize + delta).rem_euclid(len) as usize;
        self.goto_tab(next);
    }

    pub fn goto_tab(&mut self, index: usize) {
        if index >= self.tabs.len() || index == self.active {
            return;
        }
        self.stash_layout();
        // The clipboard holds names relative to a directory on one machine.
        // Carried to another tab they would mean a different machine's files,
        // or nothing at all, so it does not travel.
        if self.clip.as_ref().is_some_and(|c| c.side == Side::Remote) {
            self.clip = None;
        }
        let leaving = self.focus;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.focus = leaving;
        }
        self.active = index;
        self.adopt_layout();
        self.focus = self.focus_for_tab(leaving);
        self.settle_focus();
        // Keep the connection form in step with what is on screen.
        if let Some(opts) = self.tab().and_then(|t| t.ssh_opts()) {
            self.opts = opts.clone();
        }
        let title = self.tab().map(|t| t.title()).unwrap_or_default();
        self.set_status(
            format!("tab {}/{}: {title}", self.active + 1, self.tabs.len()),
            Level::Info,
        );
    }

    /// Ask before leaving.
    ///
    /// Quitting takes every shell, every connection and any transfer still
    /// running down with it, and `q` sits one key away from half the file
    /// keys. Pressing it again — or `y`, or `↵` — goes.
    pub fn ask_quit(&mut self) {
        // Asked twice means meant: the second press is the answer to the
        // first, which is what makes `q q` and `Ctrl-C Ctrl-C` work.
        if let Some(state) = self
            .confirm
            .take_if(|c| matches!(c.action, ConfirmAction::Quit))
        {
            self.mode = state.return_to;
            self.pending_action = Some(UiAction::Quit);
            return;
        }

        let mut body = match self.tabs.len() {
            0 => vec!["Nothing is open.".to_string()],
            1 => vec!["This is open:".to_string()],
            n => vec![format!("These {n} tabs are open:")],
        };
        for tab in self.tabs.iter().take(8) {
            body.push(format!("  {}", tab.title()));
        }
        if self.tabs.len() > 8 {
            body.push(format!("  … and {} more", self.tabs.len() - 8));
        }
        // Work in flight is the one thing quitting actually loses, so it is
        // said plainly rather than left to be noticed afterwards.
        if let Some(task) = self.current_task() {
            body.push(String::new());
            body.push(format!("Still going: {task}"));
        }
        if !self.pending.is_empty() {
            body.push(format!("Still connecting: {} more", self.pending.len()));
        }
        // Only where it is true: nothing is written down while nothing is
        // open, and promising it back would be a lie told at the worst moment.
        if !self.tabs.is_empty() {
            body.push(String::new());
            body.push("This session is written down as you go, so `sshman --resume`".into());
            body.push("or `previous session` on the workspace list brings it back.".into());
        }

        let danger = self.current_task().is_some();
        let mut state = ConfirmState::simple("Leave sshman?", body, ConfirmAction::Quit, danger);
        // Asked from wherever you were: cancelling has to put you back there
        // rather than dropping you into the file panes. The exception is
        // another dialog, which this one has just replaced — there is nothing
        // left to go back to.
        state.return_to = match self.mode {
            Mode::Confirm => Mode::Browse,
            other => other,
        };
        self.confirm = Some(state);
        self.mode = Mode::Confirm;
        self.set_status("q or y leaves, Esc stays", Level::Info);
    }

    /// Close the tab on screen, ending its SSH session and its shell.
    pub fn close_tab(&mut self) {
        self.close_tab_at(self.active);
    }

    /// The same for any tab, whether or not it is the one on screen — what
    /// the `✕` on a chip asks for.
    pub fn close_tab_at(&mut self, index: usize) {
        if index >= self.tabs.len() {
            self.set_status("no tab to close", Level::Info);
            return;
        }
        // The arrangement on screen belongs to the active tab, and after the
        // removal `active` may well name a different one. Handing it back
        // first is what keeps the two from being mixed up.
        self.stash_layout();
        let was_active = index == self.active;
        let tab = self.tabs.remove(index);
        let title = tab.title();
        let _ = tab.tx.send(Req::Quit);
        drop(tab); // takes its terminals' sessions down with it

        // Whatever was on screen stays on screen unless it is what just went.
        if index < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.layout = Layout::default();
            self.focus = Slot::files(Side::Local);
            self.zoomed = false;
            self.settle_focus();
            self.set_status(format!("closed {title} — no servers left"), Level::Info);
        } else {
            if was_active {
                self.adopt_layout();
                let leaving = self.focus;
                self.focus = self.focus_for_tab(leaving);
                if let Some(opts) = self.tab().and_then(|t| t.ssh_opts()) {
                    self.opts = opts.clone();
                }
            }
            self.settle_focus();
            self.set_status(format!("closed {title}"), Level::Info);
        }
    }

    /// Shove the tab on screen one place along the row, wrapping at the ends
    /// the same way stepping between them does.
    pub fn move_tab(&mut self, delta: isize) {
        if self.tabs.len() < 2 {
            self.set_status("only one tab — nowhere to move it", Level::Info);
            return;
        }
        let len = self.tabs.len() as isize;
        let to = (self.active as isize + delta).rem_euclid(len) as usize;
        let from = self.active;
        if self.move_tab_to(from, to) {
            let title = self.tab().map(|t| t.title()).unwrap_or_default();
            self.set_status(
                format!("{title} moved to {}/{}", to + 1, self.tabs.len()),
                Level::Info,
            );
        }
    }

    /// Take the tab at `from` out of the row and put it back at `to`. The tab
    /// on screen is still the tab on screen afterwards, wherever either of
    /// them ended up. Says whether anything moved.
    pub fn move_tab_to(&mut self, from: usize, to: usize) -> bool {
        let len = self.tabs.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        // `active` names a position, and the positions have just shifted
        // under it. Follow the tab it was naming rather than the index.
        self.active = match self.active {
            a if a == from => to,
            a if from < a && a <= to => a - 1,
            a if to <= a && a < from => a + 1,
            a => a,
        };
        true
    }

    // ---- terminals ---------------------------------------------------------

    /// Every terminal on a machine: this one's, or the tab on screen.
    pub fn terms(&self, host: Side) -> &[Term] {
        match host {
            Side::Local => &self.local_terms,
            Side::Remote => self.tab().map(|t| t.terms.as_slice()).unwrap_or(&[]),
        }
    }

    fn terms_mut(&mut self, host: Side) -> Option<&mut Vec<Term>> {
        match host {
            Side::Local => Some(&mut self.local_terms),
            Side::Remote => self.tabs.get_mut(self.active).map(|t| &mut t.terms),
        }
    }

    /// The terminal a pane is showing, if that pane is a terminal at all.
    pub fn term(&self, slot: Slot) -> Option<&Term> {
        let Slot::Term { host, id } = slot else {
            return None;
        };
        self.terms(host).iter().find(|t| t.id == id)
    }

    pub fn term_mut(&mut self, slot: Slot) -> Option<&mut Term> {
        let Slot::Term { host, id } = slot else {
            return None;
        };
        self.terms_mut(host)?.iter_mut().find(|t| t.id == id)
    }

    pub fn shell(&self, slot: Slot) -> Option<&Shell> {
        self.term(slot).map(|t| &t.shell)
    }

    pub fn shell_mut(&mut self, slot: Slot) -> Option<&mut Shell> {
        self.term_mut(slot).map(|t| &mut t.shell)
    }

    /// Start a terminal on `host` and give it a number, without putting it on
    /// screen — the caller decides where it goes.
    ///
    /// `run` is a command line to run instead of a login shell, and `opens`
    /// marks the result as the pane that files are sent to.
    fn new_term(
        &mut self,
        beside: Slot,
        run: Option<String>,
        opens: Option<String>,
    ) -> Option<Slot> {
        self.next_term_id += 1;
        let id = self.next_term_id;
        // A terminal opens where the pane it came from is looking, which with
        // several file lists on one machine is the only sensible answer to
        // "which directory".
        let here = self.dir_of(beside);
        self.open_term(beside.host(), id, here, run, opens)
    }

    /// The same, for a terminal whose number is already decided — one a saved
    /// arrangement named. It opens on the tab you are looking at.
    fn open_term(
        &mut self,
        host: Side,
        id: TermId,
        here: String,
        run: Option<String>,
        opens: Option<String>,
    ) -> Option<Slot> {
        self.open_term_in(self.active, host, id, here, run, opens)
    }

    /// And the same on any tab, whether or not it is the one on screen: a
    /// workspace opens several at once, and the shells on all of them are
    /// started rather than waiting to be looked at.
    fn open_term_in(
        &mut self,
        index: usize,
        host: Side,
        id: TermId,
        here: String,
        run: Option<String>,
        opens: Option<String>,
    ) -> Option<Slot> {
        // A placeholder size: the first draw calls `ensure_size` with the
        // space the pane actually got, which resizes both emulator and pty.
        const ROWS: u16 = 24;
        const COLS: u16 = 80;

        let here = (!here.is_empty()).then_some(here);
        let shell = match host {
            Side::Local => {
                let cwd = here.map(PathBuf::from).unwrap_or_else(|| self.local_cwd());
                match &run {
                    None => Shell::spawn_local(&cwd, ROWS, COLS),
                    Some(cmd) => {
                        Shell::spawn_local_in("local".into(), &cwd, cmd.clone(), ROWS, COLS)
                    }
                }
            }
            Side::Remote => {
                let Some(tab) = self.tabs.get(index) else {
                    self.set_status("not connected", Level::Bad);
                    return None;
                };
                let cwd = here.unwrap_or_else(|| tab.cwd().to_string());
                let label = tab.title();
                // Where the server put us at login, which is the one thing
                // that makes sense of a shell reporting it is in `~`.
                let home = tab.conn.home.clone();
                // This tab's own credentials, and its own connection, so a
                // busy shell never stalls transfers — on this tab or any other.
                match (&tab.target, tab.ssh_opts()) {
                    // Already here: the same shell the local pane opens, in
                    // this tab's directory.
                    (Target::Local, _) => match &run {
                        None => Shell::spawn_local(Path::new(&cwd), ROWS, COLS),
                        Some(cmd) => {
                            Shell::spawn_local_in(label, Path::new(&cwd), cmd.clone(), ROWS, COLS)
                        }
                    },
                    // A container is entered with `docker exec -it`, run
                    // either here or on the server that hosts it.
                    (
                        Target::Docker {
                            container, runtime, ..
                        },
                        ssh,
                    ) => {
                        let cmdline = match &run {
                            None => {
                                crate::docker::interactive_shell_command(runtime, container, None)
                            }
                            Some(cmd) => crate::docker::exec_command(runtime, container, cmd),
                        };
                        match ssh {
                            None => Shell::spawn_local_command(label, cmdline, ROWS, COLS),
                            Some(opts) => {
                                Shell::spawn_remote_command(label, opts, cmdline, ROWS, COLS)
                            }
                        }
                    }
                    (Target::Ssh(opts), _) => match &run {
                        None => Shell::spawn_remote(opts, &cwd, Some(&home), ROWS, COLS),
                        Some(cmd) => Shell::spawn_remote_command(
                            label,
                            opts,
                            format!(
                                "cd {} 2>/dev/null; exec {cmd}",
                                crate::types::sh_quote(&cwd)
                            ),
                            ROWS,
                            COLS,
                        ),
                    },
                }
            }
        };

        let term = Term { id, shell, opens };
        match host {
            Side::Local => self.local_terms.push(term),
            Side::Remote => self.tabs.get_mut(index)?.terms.push(term),
        }
        Some(Slot::term(host, id))
    }

    /// Divide the focused pane and put a new terminal in the half that opens
    /// up. `ratio` is the share the pane that was already there keeps.
    fn split_with_term(&mut self, dir: Dir, ratio: u16) {
        let at = self.focus;
        let Some(slot) = self.new_term(at, None, None) else {
            return;
        };
        if !self.layout.split(at, dir, slot, ratio) {
            return;
        }
        self.focus = slot;
        self.zoomed = false;
        self.stash_layout();
        let name = self.pane_name(slot);
        self.set_status(
            format!("{name} open — Ctrl-] returns to the files"),
            Level::Good,
        );
    }

    /// Divide the focused pane and put another file list in the half that
    /// opens up, on the same machine and looking at the same directory.
    ///
    /// Somewhere to point at a second directory: `c` copies between two lists
    /// on one machine as readily as across the middle, and both of them run
    /// the copy where the files are.
    fn split_with_tree(&mut self, dir: Dir, ratio: u16) {
        let at = self.focus;
        let Some(slot) = self.add_tree(at) else {
            return;
        };
        if !self.layout.split(at, dir, slot, ratio) {
            return;
        }
        self.focus_pane(slot);
        self.zoomed = false;
        self.stash_layout();
        self.set_status(
            "another file list — f points it somewhere, c copies between them",
            Level::Good,
        );
    }

    /// Another file list on the same machine as `like`, looking at the same
    /// directory, without putting it on screen — the caller decides where it
    /// goes.
    fn add_tree(&mut self, like: Slot) -> Option<Slot> {
        let host = like.host();
        if host == Side::Remote && !self.connected() {
            self.set_status("not connected", Level::Bad);
            return None;
        }
        self.next_tree_id += 1;
        let id = self.next_tree_id;
        let slot = Slot::tree(host, id);
        // Beside a terminal there is no directory to take: a pty's own is not
        // something anything here can know. That machine's first list says
        // where it is instead.
        let cwd = match self.dir_of(like) {
            dir if dir.is_empty() => self.main_dir(host),
            dir => dir,
        };
        match host {
            Side::Local => self.local.push(LocalTree::new(id, PathBuf::from(&cwd))),
            Side::Remote => self
                .tabs
                .get_mut(self.active)?
                .trees
                .push(RemoteTree::new(id, cwd.clone())),
        }
        match host {
            Side::Local => self.reload_local(),
            Side::Remote => self.goto_remote(slot, cwd),
        }
        Some(slot)
    }

    /// Open a terminal below the focused pane, or shut the last one opened on
    /// this machine.
    ///
    /// One key either way, the way `S` has always worked. An editor pane is
    /// left alone: it is not the shell this key means, and closing the thing
    /// you are editing in by accident would be its own kind of rude.
    fn toggle_shell(&mut self) {
        let host = self.host();
        let open: Vec<Slot> = self
            .layout
            .slots()
            .into_iter()
            .filter(|s| s.host() == host && self.term(*s).is_some_and(|t| !t.is_editor()))
            .collect();
        match open.last() {
            Some(slot) => self.close_pane(*slot),
            None => self.split_with_term(Dir::Down, 70),
        }
    }

    /// Open an editor pane beside the focused one, or close the one this
    /// machine already has.
    ///
    /// The ready-made arrangement (`A`) builds a whole tab around one. This
    /// is that pane on its own, for a tab you have already arranged by hand
    /// and now want to read a file in — and the same key takes it away again,
    /// the way `S` does for a shell.
    fn toggle_editor_pane(&mut self) {
        let host = self.host();
        if let Some(open) = self.editor_pane(host) {
            self.close_pane(open);
            return;
        }
        let at = self.focus;
        let Some(editor) = self.new_editor_term(at) else {
            return;
        };
        // Beside rather than below: an editor wants the height, and the file
        // list it is opening from wants only enough width to read names in.
        if !self.layout.split(at, Dir::Across, editor, 40) {
            // Nothing was placed, so nothing was opened: the terminal goes
            // with the next tidy-up rather than running out of sight.
            self.settle_focus();
            return;
        }
        // The keyboard stays where it was. The point of this pane is picking
        // files and watching them open beside you, not typing in it.
        self.settle_focus();
        self.stash_layout();
        let program = self.editor.clone();
        self.set_status(
            format!("{program} pane open — e or a click opens a file in it"),
            Level::Good,
        );
    }

    /// Move the keyboard to the pane across the nearest border running that
    /// way. Nothing over there leaves you where you are, rather than wrapping
    /// round to the far end of the screen.
    fn move_focus(&mut self, dir: Dir, forward: bool) {
        let Some(next) = self.layout.neighbour(self.focus, dir, forward) else {
            self.set_status("no pane that way", Level::Info);
            return;
        };
        self.focus = next;
        let name = self.pane_name(next);
        self.set_status(name, Level::Info);
    }

    /// Which pane should have the keyboard on the tab just moved to.
    ///
    /// The local file list is the same pane on every tab, so being on it when
    /// you switch means staying on it. Otherwise the tab gets its own pane
    /// back — except unzoomed, where landing in a terminal would be a trap:
    /// it swallows the Ctrl-arrows that were cycling the tabs.
    fn focus_for_tab(&self, leaving: Slot) -> Slot {
        if leaving.host() == Side::Local && self.layout.contains(leaving) {
            return leaving;
        }
        // A terminal carries over to a tab that has one, so switching tabs
        // zoomed into a shell does not drop you into a file list.
        if leaving.is_term()
            && let Some(term) = self
                .layout
                .find(|s| s.is_term() && s.host() == Side::Remote)
        {
            return term;
        }
        // Zoomed, the focused pane is the only one that can be seen at all,
        // so a tab left in its terminal has to come back to it. Unzoomed the
        // terminal is on screen either way, and landing in it would be a trap:
        // it swallows the Ctrl-arrows that were cycling the tabs.
        if self.zoomed
            && let Some(slot) = self.tab().map(|t| t.focus)
            && slot.host() == Side::Remote
            && self.layout.contains(slot)
        {
            return slot;
        }
        self.files_pane(Side::Remote)
    }

    /// The file list after this one in the order they are drawn, wrapping.
    /// `None` when this is the only one there is.
    fn next_files_pane(&self, from: Slot, back: bool) -> Option<Slot> {
        let lists: Vec<Slot> = self
            .layout
            .slots()
            .into_iter()
            .filter(|s| s.is_files())
            .collect();
        if lists.len() < 2 {
            return None;
        }
        let at = lists.iter().position(|s| *s == from).unwrap_or(0) as isize;
        let step = if back { -1 } else { 1 };
        let next = (at + step).rem_euclid(lists.len() as isize) as usize;
        Some(lists[next])
    }

    /// The file list on a machine, or the nearest thing this arrangement has.
    fn files_pane(&self, host: Side) -> Slot {
        self.layout
            .find(|s| s.is_files() && s.host() == host)
            .or_else(|| self.layout.find(Slot::is_files))
            .unwrap_or_else(|| self.layout.first())
    }

    /// Close a pane; its neighbour takes the space back.
    fn close_pane(&mut self, slot: Slot) {
        let name = self.pane_name(slot);
        if !self.layout.remove(slot) {
            self.set_status(
                "the last pane cannot be closed — W closes the tab",
                Level::Info,
            );
            return;
        }
        self.zoomed = false;
        self.settle_focus();
        self.stash_layout();
        self.set_status(format!("{name} closed"), Level::Info);
    }

    /// Hand the arrangement on screen back to the tab that owns it. Anything
    /// that changes which tab is active does this first, or the tab being
    /// left behind would forget whatever you had just done to it.
    ///
    /// Every resize and every split goes through here, so the tab's copy is
    /// never behind what is drawn — a workspace saved without switching tabs
    /// first would otherwise write down the panes from before you moved them.
    fn stash_layout(&mut self) {
        let live = self.layout.clone();
        let zoomed = self.zoomed;
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.layout = live;
            tab.zoomed = zoomed;
        }
    }

    /// Move the border nearest the focused pane. Positive gives the first
    /// pane of that split — the one on the left, or on top — more room.
    fn resize_pane(&mut self, dir: Dir, delta: i16) {
        let from = self.focus;
        if self.layout.resize_near(from, dir, delta) {
            self.stash_layout();
            return;
        }
        let why = match (self.zoomed, dir) {
            (true, _) => "one pane fills the screen — m brings the others back",
            (false, Dir::Across) => "nothing beside this pane to take room from",
            (false, Dir::Down) => "nothing above or below this pane — S opens a shell",
        };
        self.set_status(why, Level::Info);
    }

    /// Take hold of a border, and follow it until the button comes up.
    pub fn start_drag(&mut self, divider: &Divider, x: u16, y: u16) {
        self.drag = Some(Drag {
            path: divider.path.clone(),
            dir: divider.dir,
            area: divider.area,
        });
        self.drag_to(x, y);
    }

    /// Put the border being dragged where the mouse is.
    pub fn drag_to(&mut self, x: u16, y: u16) {
        let Some(drag) = self.drag.clone() else {
            return;
        };
        self.layout.drag(&drag.path, drag.dir, drag.area, x, y);
        self.stash_layout();
    }

    /// Put a tab that has just been made on screen.
    ///
    /// The arrangement it was built with is its own — a workspace's, or the
    /// one that was on screen — so the tab being left behind has to be handed
    /// its own back first, while `active` still points at it. Stashing after
    /// the push would write it over the new tab instead, since with no tabs
    /// yet open `active` is already the index the new one lands on.
    fn show_new_tab(&mut self, mut tab: RemoteTab) {
        self.stash_layout();
        // A tab on this machine has no far side: the local pane beside it
        // would be the same filesystem drawn twice.
        if tab.is_local() {
            tab.layout.retain(|slot| slot.host() != Side::Local);
            // Unless that leaves it with no file list at all.
            if !tab.layout.contains(Slot::files(Side::Remote)) {
                tab.layout = Layout::only(Slot::files(Side::Remote));
            }
        }
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.adopt_layout();
    }

    /// Draw the active tab's arrangement from here on.
    fn adopt_layout(&mut self) {
        if let Some((layout, zoomed)) = self.tab().map(|tab| (tab.layout.clone(), tab.zoomed)) {
            self.layout = layout;
            self.zoomed = zoomed;
        }
    }

    /// Share every border evenly again.
    ///
    /// Only the sizes: the panes you have opened stay open, and only the tab
    /// on screen is touched — the others keep the shape you gave them.
    fn reset_layout(&mut self) {
        self.layout.even();
        self.zoomed = false;
        self.stash_layout();
        self.set_status("panes evened up", Level::Info);
    }

    // ---- arrangements ------------------------------------------------------

    pub fn open_arrangements(&mut self) {
        self.arrangement_sel = self
            .arrangement_sel
            .min(Arrangement::ALL.len().saturating_sub(1));
        self.mode = Mode::Arrange;
    }

    fn arrange_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('A') => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') => {
                self.arrangement_sel = (self.arrangement_sel + 1).min(Arrangement::ALL.len() - 1);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.arrangement_sel = self.arrangement_sel.saturating_sub(1);
            }
            KeyCode::Enter => {
                let which = Arrangement::ALL[self.arrangement_sel.min(Arrangement::ALL.len() - 1)];
                self.mode = Mode::Browse;
                self.arrange(which);
            }
            _ => {}
        }
    }

    /// Rearrange the tab on screen.
    ///
    /// Terminals the new arrangement has no room for are shut, the same as if
    /// you had closed their panes one at a time — an arrangement is what is on
    /// screen, and nothing is kept running out of sight.
    fn arrange(&mut self, which: Arrangement) {
        let host = self.host();
        let files = Slot::files(host);
        match which {
            Arrangement::Sides => {
                self.layout = match self.on_local_tab() {
                    // Putting the local half beside a tab on this machine
                    // would be the same filesystem drawn twice.
                    true => Layout::only(Slot::files(Side::Remote)),
                    false => Layout::default(),
                };
            }
            Arrangement::Single => self.layout = Layout::only(files),
            Arrangement::TwoLists => {
                self.layout = Layout::only(files);
                self.focus = files;
                if let Some(second) = self.add_tree(files) {
                    self.layout.split(files, Dir::Across, second, 50);
                }
            }
            Arrangement::Terminal => {
                self.layout = Layout::only(files);
                if let Some(term) = self.new_term(files, None, None) {
                    self.layout.split(files, Dir::Across, term, 40);
                    self.focus = term;
                }
            }
            Arrangement::Editor => {
                self.layout = Layout::only(files);
                let Some(editor) = self.new_editor_term(files) else {
                    // Nothing was started, so nothing is arranged around it.
                    self.settle_focus();
                    self.stash_layout();
                    return;
                };
                self.layout.split(files, Dir::Across, editor, 30);
                if let Some(shell) = self.new_term(files, None, None) {
                    self.layout.split(editor, Dir::Down, shell, 70);
                }
                // The keyboard stays in the file list: the point of this one
                // is picking files and watching them open beside you.
                self.focus = files;
            }
        }
        self.zoomed = false;
        self.settle_focus();
        self.stash_layout();
        self.set_status(which.done(), Level::Good);
    }

    // ---- the editor pane ---------------------------------------------------

    /// Start a terminal with your editor already running in it.
    fn new_editor_term(&mut self, beside: Slot) -> Option<Slot> {
        let program = self.editor.clone();
        let opens = self.config.editor_open(&program);
        self.new_term(beside, Some(program), Some(opens))
    }

    /// The pane files are sent to on this machine, if there is one on screen.
    pub fn editor_pane(&self, host: Side) -> Option<Slot> {
        self.layout
            .find(|slot| slot.host() == host && self.term(slot).is_some_and(Term::is_editor))
    }

    /// Open a file in the editor pane rather than by standing aside for an
    /// editor of our own. Says whether it went.
    ///
    /// Only files on the machine the pane is talking to: the pane is a
    /// terminal *there*, so a path from the other side would mean nothing in
    /// it. Everything else still goes the long way round, fetched and pushed
    /// back.
    fn send_to_editor(&mut self, at: Slot, name: &str) -> bool {
        let side = at.host();
        let Some(slot) = self.editor_pane(side) else {
            return false;
        };
        // A remote pane on a tab in sudo mode is reading root's files through
        // the worker, which the shell in the pane cannot do.
        if side == Side::Remote && self.sudo() && !self.on_local_tab() {
            return false;
        }
        let dir = self.dir_of(at);
        if dir.is_empty() {
            return false;
        }
        let path = match side {
            Side::Local => PathBuf::from(dir).join(name).display().to_string(),
            Side::Remote => rjoin(&dir, name),
        };
        let program = self.editor.clone();

        // An editor that has been quit took its pty with it. A fresh one
        // opens in the same pane, on the file directly.
        if !self.shell(slot).is_some_and(Shell::is_alive) {
            let opens = self.config.editor_open(&program);
            let run = format!("{program} {}", crate::types::sh_quote(&path));
            let Some(fresh) = self.new_term(slot, Some(run), Some(opens)) else {
                return false;
            };
            self.layout.replace(slot, fresh);
            if self.focus == slot {
                self.focus = fresh;
            }
            self.settle_focus();
            self.stash_layout();
            self.set_status(
                format!("{name} — the editor pane was restarted"),
                Level::Good,
            );
            return true;
        }

        let keys = match self.term(slot).and_then(|t| t.opens.clone()) {
            // The path goes in as it is: what the keys around it are typed
            // into is the editor's own command line, not a shell, and every
            // editor escapes differently. A path with spaces in it wants keys
            // of your own.
            Some(open) if !open.is_empty() => open.replace("{file}", &path),
            // Nothing is known about this editor, so the pane is treated as
            // the shell prompt it is and the editor run as a command — where
            // shell quoting is exactly what is wanted.
            _ => format!("{program} {}\r", crate::types::sh_quote(&path)),
        };
        if let Some(shell) = self.shell_mut(slot) {
            shell.type_in(&keys);
        }
        self.set_status(format!("{name} → editor pane"), Level::Good);
        true
    }

    /// A click on a row of a file list.
    ///
    /// One click moves the cursor there and nothing else — except with an
    /// editor pane open, where clicking a file opens it there, the way
    /// clicking in a file tree works everywhere else.
    ///
    /// Two clicks on the same row mean the row: a directory opens, a file
    /// goes to your editor. That is `Enter` under the pointer, and it is
    /// deliberately not what one click does — a list you cannot put the
    /// cursor on without going somewhere is a list you cannot look at.
    pub fn click_row(&mut self, slot: Slot, index: usize) {
        /// Long enough not to need a steady hand, short enough that two
        /// deliberate clicks on one row are still two clicks.
        const AGAIN_WITHIN: Duration = Duration::from_millis(400);

        let now = Instant::now();
        let again = self
            .last_click
            .is_some_and(|(s, i, at)| s == slot && i == index && now - at < AGAIN_WITHIN);
        // A double click is finished business: the third click of three
        // starts again rather than opening the row a second time.
        self.last_click = (!again).then_some((slot, index, now));

        self.focus_pane(slot);
        self.pane_mut(slot).select_index(index);
        if again {
            self.activate(slot);
            return;
        }
        if self.editor_pane(slot.host()).is_none() {
            return;
        }
        let Some(entry) = self.pane(slot).selected().cloned() else {
            return;
        };
        if !entry.is_dir_like() {
            self.send_to_editor(slot, &entry.name);
        }
    }

    // ---- the menu a right click opens ------------------------------------

    /// Right click in a file list: what can be done to what is under the
    /// pointer.
    ///
    /// The row is aimed at first, the way it would be by a left click, so
    /// that "Rename" renames what you pointed at rather than what the cursor
    /// happened to be on. Marks are the exception: a right click on a row
    /// that is *part of a selection* is about the selection, so the cursor is
    /// left where it is and the marks stand. That is the rule every file
    /// manager uses, and the one that makes "mark six, right click, delete"
    /// mean what it looks like it means.
    pub fn open_menu(&mut self, at: Slot, on: Option<usize>, column: u16, row: u16) {
        if !at.is_files() {
            return;
        }
        self.focus_pane(at);
        if let Some(index) = on {
            let name = self.pane(at).view.get(index).map(|e| e.name.clone());
            let inside = name.is_some_and(|name| self.pane(at).marked.contains(&name));
            if !inside {
                self.pane_mut(at).select_index(index);
            }
        }
        // A right click is not a click for the purposes of "twice quickly
        // opens it": otherwise a left click, then a right click on the same
        // row, then a left click would walk into a directory nobody asked to
        // enter.
        self.last_click = None;
        let items = self.menu_items(at, on.is_some());
        let cursor = items
            .iter()
            .position(|item| matches!(item, MenuItem::Do(..)))
            .unwrap_or(0);
        self.menu = Some(Menu {
            at,
            items,
            cursor,
            anchor: (column, row),
            area: Rect::default(),
            scroll: 0,
        });
    }

    /// What goes in the menu, which depends on what was clicked.
    ///
    /// Rows that could not do anything are left out rather than greyed out.
    /// A menu is a list of what is possible here; "Extract" over a text file
    /// and "Paste" with an empty clipboard are not possibilities, and a row
    /// that refuses to work is worse than one that was never offered.
    fn menu_items(&self, at: Slot, on_row: bool) -> Vec<MenuItem> {
        use MenuItem::{Do, Rule};
        let mut items = Vec::new();
        if on_row {
            let entry = self.pane(at).selected();
            let directory = entry.is_some_and(FileEntry::is_dir_like);
            let archive = entry.is_some_and(|e| crate::archive::is_archive(&e.name));
            items.push(Do(if directory { "Enter" } else { "Open" }, Action::Open));
            if !directory {
                items.push(Do("Edit", Action::Edit));
                items.push(Do("View", Action::View));
            }
            items.push(Rule);
            items.push(Do("Copy to the other list", Action::Copy));
            items.push(Do("Cut", Action::Cut));
            if self.clip.is_some() {
                items.push(Do("Paste", Action::Paste));
            }
            items.push(Rule);
            items.push(Do("Rename…", Action::Rename));
            items.push(Do("Delete", Action::Delete));
            items.push(Rule);
            if archive {
                items.push(Do("Extract…", Action::Extract));
                items.push(Do("List what it holds", Action::ListArchive));
            }
            items.push(Do("Pack into an archive…", Action::Archive));
            items.push(Rule);
            items.push(Do("Mark", Action::Mark));
            items.push(Do("Mark all", Action::MarkAll));
        } else {
            // The space below the last entry, which is about the directory
            // rather than about anything in it.
            if self.clip.is_some() {
                items.push(Do("Paste", Action::Paste));
                items.push(Rule);
            }
            items.push(Do("New directory…", Action::NewDirectory));
            items.push(Do("Go to…", Action::GoTo));
            items.push(Do("Home", Action::Home));
            items.push(Do("The directory above", Action::Parent));
            items.push(Rule);
            items.push(Do("Mark all", Action::MarkAll));
            items.push(Do("Show hidden files", Action::Hidden));
            items.push(Do("Point the other list here", Action::Mirror));
        }
        items.push(Rule);
        items.push(Do("A shell here", Action::Shell));
        items.push(Do("Reload", Action::Reload));
        items
    }

    pub fn close_menu(&mut self) {
        self.menu = None;
    }

    /// Keys while the menu is open.
    ///
    /// It has them all, but only briefly: the keys that work a menu work it,
    /// and every other key closes it and is swallowed. Modal in the sense
    /// that a menu is a question, and not in the sense that you can get stuck
    /// in one — a keyboard that had stopped answering would be a far worse
    /// bug than a keystroke that did nothing but put a menu away.
    pub fn menu_key(&mut self, key: KeyEvent) {
        let Some(menu) = &mut self.menu else { return };
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => menu.step(-1),
            KeyCode::Down | KeyCode::Char('j') => menu.step(1),
            KeyCode::Home => {
                menu.cursor = 0;
                if menu.chosen().is_none() {
                    menu.step(1);
                }
            }
            KeyCode::End => {
                menu.cursor = menu.items.len().saturating_sub(1);
                if menu.chosen().is_none() {
                    menu.step(-1);
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.choose_menu(),
            _ => self.close_menu(),
        }
    }

    /// Do what the lit row says, and close the menu.
    ///
    /// The menu goes first. Several of these open a box of their own — a
    /// rename asks for a name — and a menu still on the screen behind it
    /// would be a menu drawn over the question it asked.
    pub fn choose_menu(&mut self) {
        let Some(menu) = self.menu.take() else { return };
        let Some(action) = menu.chosen() else { return };
        self.focus_pane(menu.at);
        self.run(action);
    }

    /// Choose the row a click landed on, or close the menu when it landed
    /// somewhere else. Answers whether the click was the menu's business.
    pub fn click_menu(&mut self, column: u16, row: u16) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        if !menu.hits(column, row) {
            // A click outside is "not that, then". It closes the menu and
            // does nothing else: a menu you dismissed should not also move
            // your cursor to wherever you happened to dismiss it.
            self.close_menu();
            return true;
        }
        // A click on the border or on a rule is inside the menu, so it is
        // not a dismissal and it is not a choice either.
        if let Some(at) = menu.row_at(column, row) {
            menu.cursor = at;
            self.choose_menu();
        }
        true
    }

    /// The pointer moving over an open menu lights the row under it, the same
    /// way it lights a row of a file list.
    pub fn point_at_menu(&mut self, column: u16, row: u16) {
        if let Some(menu) = &mut self.menu
            && let Some(at) = menu.row_at(column, row)
        {
            menu.cursor = at;
        }
    }

    /// The pointer is over a row of a file list.
    pub fn hover_row(&mut self, slot: Slot, index: usize) {
        self.hover = Some(Hover::Row(slot, index));
    }

    /// The pointer is over one piece of a pane's path.
    pub fn hover_crumb(&mut self, slot: Slot, path: String) {
        self.hover = Some(Hover::Crumb(slot, path));
    }

    /// The pointer is over nothing a click would land on — a border, a
    /// terminal, the space below the last entry.
    pub fn clear_hover(&mut self) {
        self.hover = None;
    }

    /// The row of a file list the pointer is lighting up, if it is this one.
    pub fn hovered_row(&self, slot: Slot) -> Option<usize> {
        match &self.hover {
            Some(Hover::Row(s, i)) if *s == slot => Some(*i),
            _ => None,
        }
    }

    /// The piece of this pane's path the pointer is lighting up.
    pub fn hovered_crumb(&self, slot: Slot) -> Option<&str> {
        match &self.hover {
            Some(Hover::Crumb(s, path)) if *s == slot => Some(path.as_str()),
            _ => None,
        }
    }

    /// Which tab's chip is at a column of the tab row, if any.
    ///
    /// Only a chip. The chevrons at the ends of the row are in `tab_spans`
    /// too — each stands for the tab just off screen that way, so a click on
    /// one steps there — but a chevron is not that tab's chip, and pointing
    /// at it is not pointing at the tab. A chip is one that got a close
    /// button, which is only drawn on the ones actually on the row.
    pub fn tab_index_at(&self, column: u16) -> Option<usize> {
        let (_, _, index) = self
            .tab_spans
            .iter()
            .find(|(start, end, _)| column >= *start && column < *end)
            .copied()?;
        self.tab_close_buttons
            .iter()
            .any(|(_, _, at)| *at == index)
            .then_some(index)
    }

    /// The pointer is over a tab's chip. Starts the clock, or leaves it
    /// running where it is already on this one — moving about inside a chip
    /// is still resting on that chip.
    pub fn rest_on_tab(&mut self, index: usize) {
        if self.tab_rest.is_some_and(|(on, _)| on == index) {
            return;
        }
        self.tab_rest = Some((index, Instant::now()));
    }

    /// The pointer is somewhere that is not a tab, or the keyboard has it.
    pub fn clear_tab_rest(&mut self) {
        self.tab_rest = None;
    }

    /// The tab whose full name is worth putting on the screen: one the
    /// pointer has been sitting on long enough to be asking about, whose
    /// chip could not show the whole of the name.
    ///
    /// The width is what the drawing shortened the name to, which is the only
    /// thing that knows whether anything was lost.
    pub fn tab_tip(&self, budget: usize) -> Option<(usize, String)> {
        let (index, since) = self.tab_rest?;
        if since.elapsed() < TAB_TIP_DELAY {
            return None;
        }
        let title = self.tabs.get(index)?.title();
        (title.chars().count() > budget).then_some((index, title))
    }

    /// A click on one piece of the path in a pane's title: the pane goes
    /// there, however far back up the trail it is.
    pub fn click_crumb(&mut self, slot: Slot, path: String) {
        self.focus_pane(slot);
        self.goto(slot, path);
    }

    /// Point a file list at a directory, by whichever route its machine
    /// needs. The two halves of this are not interchangeable: one is a path
    /// on the filesystem sshman is running on, the other a string that only
    /// means anything to the server.
    pub fn goto(&mut self, slot: Slot, path: String) {
        match slot.host() {
            Side::Local => self.goto_local(slot, PathBuf::from(path)),
            Side::Remote => self.goto_remote(slot, path),
        }
    }

    /// A click on a pane's zoom button.
    ///
    /// The zoom follows the focus, so the focus goes to the pane whose button
    /// was clicked before anything is zoomed — clicking another pane's button
    /// blows up that pane, which is the only thing the click could mean.
    pub fn click_zoom_button(&mut self, slot: Slot) {
        self.focus = slot;
        self.toggle_zoom();
    }

    /// A click on a pane's close button. It shuts the pane it is drawn in,
    /// whether or not that is the pane with the keyboard — which is the whole
    /// point of there being one in every corner.
    pub fn click_close_button(&mut self, slot: Slot) {
        self.close_pane(slot);
    }

    /// Give the whole area to the focused pane, or hand it back.
    ///
    /// The zoom follows the focus afterwards, so moving between panes keeps
    /// working the way it does at any other size — you stay zoomed, on
    /// whatever you moved to — and there is nothing to remember about which
    /// pane was blown up.
    fn toggle_zoom(&mut self) {
        // With one pane there is nothing for a zoom to hide. Un-zooming is
        // always allowed, so a pane closed while zoomed cannot strand anyone.
        if !self.zoomed && !self.zoom_has_anything_to_hide() {
            self.set_status(
                "this tab is a single pane — S opens a shell to zoom past",
                Level::Info,
            );
            return;
        }
        self.zoomed = !self.zoomed;
        if self.zoomed {
            let what = self.pane_name(self.focus);
            self.set_status(
                format!("{what} fills the screen — m or F3 brings the other panes back"),
                Level::Info,
            );
        } else {
            self.set_status("every pane again", Level::Info);
        }
    }

    fn toggle_sudo(&mut self) {
        if !self.connected() {
            self.set_status("not connected", Level::Bad);
            return;
        }
        if self.sudo() {
            self.send(Req::SetSudo(None));
            return;
        }
        // Reaching a container's docker daemon already implies the authority
        // `-u 0` grants, so there is nothing to ask for.
        if self.tab().is_some_and(|t| t.is_container()) {
            self.send(Req::SetSudo(Some(String::new())));
            return;
        }
        self.open_prompt(
            PromptKind::SudoPassword,
            "sudo password (empty if NOPASSWD)".into(),
            String::new(),
        );
    }

    /// Point the other machine's file list at the same path as this one.
    fn mirror_path(&mut self) {
        let Some(to) = self.target() else {
            self.set_status("no other file list on screen to point", Level::Info);
            return;
        };
        let path = self.dir_of(self.focus);
        if path.is_empty() {
            return;
        }
        match to.host() {
            Side::Remote => self.goto_remote(to, path),
            Side::Local => {
                let here = PathBuf::from(&path);
                if here.is_dir() {
                    self.goto_local(to, here);
                } else {
                    self.set_status(format!("no local directory {path}"), Level::Bad);
                }
            }
        }
    }
}

/// A terminal pane for the drawing tests, which cannot reach the helpers in
/// this file's own test module.
#[cfg(test)]
impl App {
    pub fn open_test_term(&mut self, cwd: &std::path::Path) -> Slot {
        self.next_term_id += 1;
        let id = self.next_term_id;
        self.local_terms.push(Term {
            id,
            shell: Shell::spawn_local(cwd, 24, 80),
            opens: None,
        });
        let slot = Slot::term(Side::Local, id);
        self.layout
            .split(Slot::files(Side::Local), Dir::Down, slot, 50);
        self.stash_layout();
        slot
    }
}

/// Tabs for the tests to draw and switch between. The worker at the other end
/// of the channel is nobody, which is all they need: nothing here sends it a
/// request.
#[cfg(test)]
impl App {
    pub fn fake_tab(&mut self, host: &str) {
        self.fake_tab_with(host, Layout::default());
    }

    pub fn fake_tab_with(&mut self, host: &str, layout: Layout) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (_, resp_rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        self.show_new_tab(RemoteTab {
            target: Target::Ssh(ConnectOpts::default()),
            kind: BackendKind::Ssh,
            name: None,
            conn: ConnInfo {
                user: "me".into(),
                host: host.into(),
                port: 22,
                home: "/home/me".into(),
            },
            link: LinkState::Live,
            sudo: false,
            trees: vec![RemoteTree::new(layout::MAIN, "/home/me".into())],
            terms: Vec::new(),
            wants_editor: Vec::new(),
            wants_dir: PaneDirs::default(),
            focus: Slot::files(Side::Remote),
            layout,
            zoomed: false,
            task: None,
            forwards: Vec::new(),
            tx,
            rx: resp_rx,
        });
    }
}

/// Find the public key to install: the companion of an explicitly chosen
/// private key, otherwise the usual defaults in preference order.
fn find_public_key(opts: &ConnectOpts) -> Result<(PathBuf, String), String> {
    let mut candidates = Vec::new();
    if let Some(private) = &opts.key_path {
        // `id_ed25519` -> `id_ed25519.pub`, not `id.pub`, so append rather
        // than replace the extension.
        let mut name = private.as_os_str().to_os_string();
        name.push(".pub");
        candidates.push(PathBuf::from(name));
    }
    if let Some(home) = dirs::home_dir() {
        for name in ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"] {
            candidates.push(home.join(".ssh").join(name));
        }
    }

    for path in &candidates {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let key = text.trim().to_string();
        if key.starts_with("ssh-") || key.starts_with("ecdsa-") || key.starts_with("sk-") {
            return Ok((path.clone(), key));
        }
    }
    Err(format!(
        "no public key found (looked for {}). Create one with ssh-keygen.",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// The furthest a view can scroll and still be showing content: enough that
/// the last line sits at the bottom of the box, never past it.
fn scroll_limit(lines: usize, view_height: u16) -> u16 {
    lines.saturating_sub(view_height.max(1) as usize) as u16
}

/// Describe a tab well enough to rebuild it later, or `None` when it cannot
/// be rebuilt — a container reached through another container.
fn workspace_item_for(tab: &RemoteTab) -> Option<WorkspaceItem> {
    let path = Some(tab.cwd().to_string()).filter(|p| !p.is_empty());
    // The arrangement already says where this tab's terminals were; all that
    // is left to say is which of them opened files.
    let editors: Vec<TermId> = tab
        .terms
        .iter()
        .filter(|term| term.is_editor())
        .map(|term| term.id)
        .collect();
    let dirs = pane_dirs(
        tab.trees.iter().map(|tree| (tree.id, tree.cwd.clone())),
        tab.terms.iter().map(|term| (term.id, term.shell.cwd())),
    );
    match &tab.target {
        Target::Local => Some(WorkspaceItem::Local {
            path,
            name: tab.name.clone(),
            layout: Some(tab.layout.clone()),
            editors,
            dirs,
        }),
        Target::Ssh(opts) => Some(WorkspaceItem::Ssh {
            user: opts.user.clone(),
            host: opts.host.clone(),
            port: opts.port,
            key_path: opts.key_path.as_ref().map(|p| p.display().to_string()),
            name: tab.name.clone(),
            path,
            forwards: saved_forwards(tab),
            layout: Some(tab.layout.clone()),
            editors,
            dirs,
        }),
        Target::Docker {
            via,
            runtime,
            container,
        } => Some(WorkspaceItem::Container {
            // Saved by name, not by the running id: an id does not survive the
            // container being recreated, and a workspace is meant to.
            container: container_name_of(tab).unwrap_or_else(|| container.clone()),
            runtime: runtime.clone(),
            via: via.as_ref().map(|opts| {
                Box::new(WorkspaceItem::Ssh {
                    user: opts.user.clone(),
                    host: opts.host.clone(),
                    port: opts.port,
                    key_path: opts.key_path.as_ref().map(|p| p.display().to_string()),
                    name: None,
                    path: None,
                    forwards: Vec::new(),
                    // Not a tab of its own: it is how the container is
                    // reached, so it has no panes of its own.
                    layout: None,
                    editors: Vec::new(),
                    dirs: Default::default(),
                })
            }),
            path,
            forwards: saved_forwards(tab),
            layout: Some(tab.layout.clone()),
            editors,
            dirs,
        }),
    }
}

/// Gather where a set of panes were pointed, ready to be written down.
///
/// A pane with nothing to say — a terminal whose directory could not be
/// found out, a file list that never finished loading — is left out rather
/// than written down as empty, so restoring falls back to the tab's own
/// directory instead of trying to `cd` to nowhere.
fn pane_dirs(
    trees: impl Iterator<Item = (TreeId, String)>,
    shells: impl Iterator<Item = (TermId, Option<String>)>,
) -> crate::workspace::PaneDirs {
    crate::workspace::PaneDirs {
        trees: trees
            .filter(|(_, path)| !path.is_empty())
            .map(|(id, path)| crate::workspace::PaneDir { id, path })
            .collect(),
        shells: shells
            .filter_map(|(id, path)| Some((id, path?)))
            .filter(|(_, path)| !path.is_empty())
            .map(|(id, path)| crate::workspace::PaneDir { id, path })
            .collect(),
    }
}

/// The forwards worth writing down: the ones still running.
fn saved_forwards(tab: &RemoteTab) -> Vec<String> {
    tab.forwards
        .iter()
        .filter(|f| f.is_running())
        .map(|f| f.spec.to_spec_string())
        .collect()
}

/// The container's friendly name, which the tab title carries.
fn container_name_of(tab: &RemoteTab) -> Option<String> {
    if !tab.is_container() {
        return None;
    }
    // `conn.host` is `name` locally and `name@server` for a remote one.
    let host = &tab.conn.host;
    Some(host.split('@').next().unwrap_or(host).to_string())
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Local => "local",
        Side::Remote => "remote",
    }
}

fn default_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

fn file_signature(path: &std::path::Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EntryKind;
    use crate::workspace::PaneDir;
    use ratatui::crossterm::event::KeyEventState;
    use std::path::Path;

    /// An app looking at a scratch directory, with nothing connected — the
    /// local side is the half that works without a server. Without a server
    /// to connect to it would open on the connection screen, and these are
    /// all about the keys that browsing takes.
    fn app_in(dir: &Path) -> App {
        let mut app = App::new(ConnectOpts::default(), dir.to_path_buf(), None, false);
        app.mode = Mode::Browse;
        app.status.clear();
        // Whatever this machine has saved is none of a test's business, and
        // a test must never write over it either.
        app.workspaces = Workspaces::default();
        app.previous_session = None;
        app
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sshman-app-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        std::fs::write(dir.join("one.txt"), b"one\n").unwrap();
        dir
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    }

    fn press(app: &mut App, c: char) {
        app.on_key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    }

    /// The pane on this machine that everything starts on.
    fn here() -> Slot {
        Slot::files(Side::Local)
    }

    fn select(app: &mut App, name: &str) {
        let index = app
            .pane(here())
            .view
            .iter()
            .position(|e| e.name == name)
            .unwrap_or_else(|| panic!("{name} is not in the pane"));
        app.pane_mut(here()).select_index(index);
    }

    #[test]
    fn c_copies_across_until_a_pane_is_zoomed() {
        let dir = scratch("c-key");
        let mut app = app_in(&dir);
        select(&mut app, "one.txt");

        // Both panes on screen: c is the copy to the other side, which
        // without a server has nowhere to go.
        press(&mut app, 'c');
        assert!(app.clip.is_none(), "c must not pick anything up here");
        assert_eq!(app.status, "not connected");

        // Zoomed there is no other side, so the same key picks files up.
        press(&mut app, 'm');
        assert!(app.zoomed);
        press(&mut app, 'c');
        let clip = app.clip.as_ref().expect("c picked the file up");
        assert_eq!(clip.names, ["one.txt"]);
        assert!(!clip.cut, "c copies");
        assert_eq!(clip.side, Side::Local);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_copy_lands_in_the_directory_on_screen() {
        let dir = scratch("paste");
        let mut app = app_in(&dir);
        select(&mut app, "one.txt");
        press(&mut app, 'm');
        press(&mut app, 'c');

        app.goto_local(here(), dir.join("dst"));
        press(&mut app, 'P');

        // The copy runs off the UI thread, as every local command does.
        let landed = dir.join("dst/one.txt");
        for _ in 0..200 {
            if landed.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(std::fs::read_to_string(&landed).unwrap(), "one\n");
        assert!(
            dir.join("one.txt").exists(),
            "a copy leaves the original alone"
        );
        assert!(app.clip.is_some(), "and can be pasted somewhere else too");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_paste_onto_a_name_already_there_says_so_and_writes_nothing() {
        let dir = scratch("clobber");
        std::fs::write(dir.join("dst/one.txt"), b"do not lose me\n").unwrap();
        let mut app = app_in(&dir);
        select(&mut app, "one.txt");
        press(&mut app, 'm');
        press(&mut app, 'c');

        app.goto_local(here(), dir.join("dst"));
        press(&mut app, 'P');
        for _ in 0..200 {
            app.drain_workers();
            if app.status_level == Level::Bad {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            app.status.contains("one.txt is already there"),
            "it has to name the file in the way: {}",
            app.status
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("dst/one.txt")).unwrap(),
            "do not lose me\n",
            "and leave it alone"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_cut_is_used_up_by_the_paste_that_lands_it() {
        let dir = scratch("cut");
        let mut app = app_in(&dir);
        select(&mut app, "one.txt");
        press(&mut app, 'M');
        assert!(app.clip.as_ref().unwrap().cut);

        app.goto_local(here(), dir.join("dst"));
        press(&mut app, 'P');
        assert!(app.clip.is_none(), "a move only happens once");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_clipboard_does_not_cross_the_middle() {
        let dir = scratch("cross");
        let mut app = app_in(&dir);
        app.clip = Some(Clip {
            side: Side::Remote,
            dir: "/etc".into(),
            names: vec!["hosts".into()],
            cut: false,
        });
        press(&mut app, 'P');
        assert!(
            app.status.contains("remote"),
            "it has to say why: {}",
            app.status
        );
        assert!(app.clip.is_some(), "and hold on to what it has");
        assert!(
            !dir.join("hosts").exists(),
            "nothing may be written locally"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn nothing_is_picked_up_from_an_empty_selection() {
        let dir = scratch("empty");
        let mut app = app_in(&dir);
        app.pane_mut(here()).state.select(None);
        press(&mut app, 'M');
        assert!(app.clip.is_none());
        assert_eq!(app.status, "nothing selected");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A tab that is connected as far as the UI is concerned. The worker at
    /// the other end of the channel is nobody, which is all these need: the
    /// keys under test never send a request.
    fn fake_tab(app: &mut App, host: &str, shell: Option<Shell>) {
        fake_tab_with(app, host, shell, Layout::default());
    }

    /// A tab on this machine, the kind `L` opens.
    fn fake_local_tab(app: &mut App, cwd: &str) {
        fake_tab_with(app, "here", None, Layout::default());
        let tab = app.tabs.last_mut().unwrap();
        tab.kind = BackendKind::Local;
        tab.target = Target::Local;
        tab.trees[0].cwd = cwd.to_string();
        // What `show_new_tab` does for a tab that says it is local from the
        // start: there is no far side to put beside it.
        tab.layout.retain(|slot| slot.host() != Side::Local);
        tab.trees[0].pane.set_entries(vec![FileEntry {
            name: "one.txt".into(),
            kind: EntryKind::File,
            size: 4,
            mtime: 0,
            perms: "-rw-r--r--".into(),
            link_target: None,
            points_to_dir: false,
        }]);
        app.adopt_layout();
        app.settle_focus();
    }

    /// Put a terminal on a machine and open a pane for it, the way `S` does.
    fn add_term(app: &mut App, host: Side, shell: Shell) -> Slot {
        app.next_term_id += 1;
        let id = app.next_term_id;
        app.terms_mut(host)
            .expect("somewhere to put it")
            .push(Term {
                id,
                shell,
                opens: None,
            });
        let slot = Slot::term(host, id);
        let at = app.files_pane(host);
        app.layout.split(at, Dir::Down, slot, 70);
        app.stash_layout();
        slot
    }

    fn fake_tab_with(app: &mut App, host: &str, shell: Option<Shell>, layout: Layout) {
        app.fake_tab_with(host, layout);
        if let Some(shell) = shell {
            add_term(app, Side::Remote, shell);
        }
    }

    /// A file entry, for the tests that hand a pane a listing directly.
    fn remote_entry(name: &str) -> FileEntry {
        FileEntry {
            name: name.into(),
            kind: EntryKind::File,
            size: 1,
            mtime: 0,
            perms: "-rw-r--r--".into(),
            link_target: None,
            points_to_dir: false,
        }
    }

    /// Wind the clocks back far enough that the next look is due.
    fn due_for_a_look(app: &mut App) {
        let ages = Instant::now() - watch::REMOTE * 4;
        app.watched_local = ages;
        app.watched_deep = ages;
        app.watched_remote = ages;
    }

    #[test]
    fn a_file_that_appears_shows_up_without_anyone_asking() {
        let dir = scratch("watch-local");
        let mut app = app_in(&dir);
        let names = |app: &App| -> Vec<String> {
            app.pane(here())
                .view
                .iter()
                .map(|e| e.name.clone())
                .collect()
        };
        assert!(!names(&app).contains(&"late.txt".to_string()));

        std::fs::write(dir.join("late.txt"), b"hello\n").unwrap();
        std::fs::remove_file(dir.join("one.txt")).unwrap();
        due_for_a_look(&mut app);
        app.watch_dirs();

        let now = names(&app);
        assert!(now.contains(&"late.txt".to_string()), "{now:?}");
        assert!(!now.contains(&"one.txt".to_string()), "{now:?}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_taken_from_under_the_cursor_leaves_it_on_the_row_it_was_on() {
        let dir = scratch("watch-cursor");
        std::fs::write(dir.join("two.txt"), b"two\n").unwrap();
        std::fs::write(dir.join("three.txt"), b"three\n").unwrap();
        let mut app = app_in(&dir);

        // dst/, then one.txt, three.txt, two.txt.
        select(&mut app, "three.txt");
        let row = app.pane(here()).state.selected();
        std::fs::remove_file(dir.join("three.txt")).unwrap();
        due_for_a_look(&mut app);
        app.watch_dirs();

        assert_eq!(app.pane(here()).state.selected(), row, "the cursor stayed");
        assert_eq!(
            app.pane(here()).selected_name().as_deref(),
            Some("two.txt"),
            "which is now the next file along, not the top of the list"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_list_that_holds_still_is_left_alone_when_that_is_what_you_asked_for() {
        let dir = scratch("watch-off");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));
        app.config.watch = Some("off".into());

        std::fs::write(dir.join("late.txt"), b"hello\n").unwrap();
        due_for_a_look(&mut app);
        app.watch_dirs();
        assert!(
            !app.pane(here()).view.iter().any(|e| e.name == "late.txt"),
            "nothing is watched with it off"
        );

        // The reload key still works, which is the whole of what "off" means.
        press(&mut app, 'R');
        assert!(app.pane(here()).view.iter().any(|e| e.name == "late.txt"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_server_is_asked_whether_its_directory_moved_on_and_the_answer_lands() {
        let dir = scratch("watch-remote");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "server", None);
        app.tabs[0].trees[0]
            .pane
            .set_entries(vec![remote_entry("one.txt")]);

        due_for_a_look(&mut app);
        app.watch_dirs();
        assert!(app.tabs[0].trees[0].polling, "the question went out");

        let seq = app.tabs[0].trees[0].seq;
        app.handle_resp(
            RespSource::Tab(0),
            Resp::Polled {
                tree: layout::MAIN,
                seq,
                entries: Some(vec![remote_entry("one.txt"), remote_entry("two.txt")]),
            },
        );
        assert!(!app.tabs[0].trees[0].polling, "and was answered");
        let names: Vec<String> = app.tabs[0].trees[0]
            .pane
            .view
            .iter()
            .map(|e| e.name.clone())
            .collect();
        assert_eq!(names, ["one.txt", "two.txt"]);
        assert!(app.status.is_empty(), "quietly: {}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_answer_about_a_directory_the_pane_has_left_is_dropped() {
        let dir = scratch("watch-stale");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "server", None);
        app.tabs[0].trees[0]
            .pane
            .set_entries(vec![remote_entry("one.txt")]);

        due_for_a_look(&mut app);
        app.watch_dirs();
        let asked = app.tabs[0].trees[0].seq;

        // The user goes somewhere else while the question is out.
        app.goto_remote(Slot::files(Side::Remote), "/etc".into());
        app.handle_resp(
            RespSource::Tab(0),
            Resp::Polled {
                tree: layout::MAIN,
                seq: asked,
                entries: Some(vec![remote_entry("stale.txt")]),
            },
        );
        assert!(
            !app.tabs[0].trees[0]
                .pane
                .view
                .iter()
                .any(|e| e.name == "stale.txt"),
            "an answer about the old directory must not land in the new one"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tab_you_left_in_its_shell_comes_back_to_its_shell() {
        let dir = scratch("tab-shell");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", Some(Shell::spawn_local(&dir, 24, 80)));
        fake_tab(&mut app, "two", None);
        app.goto_tab(0);
        let shell = app.layout.find(Slot::is_term).expect("the first tab's");
        app.focus = shell;
        app.zoomed = true;

        // The second tab has no shell, so there is none to show, and it was
        // never zoomed: the zoom belongs to the tab that was zoomed.
        app.goto_tab(1);
        assert!(!app.in_term());
        assert!(!app.zoomed, "the other tab is still split");

        // Going back must land where we left off, not on the file list, and
        // full screen again.
        app.goto_tab(0);
        assert_eq!(app.focus, shell, "the shell we were in has to come back");
        assert!(app.zoomed, "and so does the zoom it was left in");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cycling_tabs_unzoomed_never_lands_in_a_shell() {
        let dir = scratch("tab-cycle");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", Some(Shell::spawn_local(&dir, 24, 80)));
        // The second tab was left in its shell.
        app.tabs[1].focus = app.layout.find(Slot::is_term).expect("the second tab's");
        app.goto_tab(0);
        app.focus = Slot::files(Side::Remote);

        app.goto_tab(1);
        assert_eq!(
            app.focus,
            Slot::files(Side::Remote),
            "the shell is on screen anyway, and it would swallow the \
             Ctrl-arrows that got us here"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shell_carries_over_to_a_tab_that_has_one() {
        let dir = scratch("tab-carry");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", Some(Shell::spawn_local(&dir, 24, 80)));
        fake_tab(&mut app, "two", Some(Shell::spawn_local(&dir, 24, 80)));
        app.goto_tab(0);
        app.focus = app.layout.find(Slot::is_term).expect("the first tab's");
        app.zoomed = true;

        app.goto_tab(1);
        assert!(
            app.in_term(),
            "the other tab has a shell, so a zoomed shell must show it"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn switching_tabs_leaves_the_local_side_alone() {
        let dir = scratch("tab-local");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);
        app.goto_tab(0);
        app.focus = Slot::files(Side::Local);

        app.goto_tab(1);
        assert_eq!(
            app.focus,
            Slot::files(Side::Local),
            "the keyboard is on this machine; tabs are the other side"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_terminal_is_still_running_when_you_come_back_to_its_tab() {
        // Its pane belongs to the tab that opened it, so switching away hides
        // it — but nothing is shut down, and the session is waiting.
        let dir = scratch("tab-keep");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", Some(Shell::spawn_local(&dir, 24, 80)));
        fake_tab(&mut app, "two", None);

        assert!(app.layout.find(Slot::is_term).is_none(), "not on this tab");
        assert_eq!(app.tabs[0].terms.len(), 1, "still running on the other");

        app.goto_tab(0);
        assert!(
            app.layout.find(Slot::is_term).is_some(),
            "and back it comes"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shell_opens_in_a_pane_of_its_own_and_s_closes_it_again() {
        let dir = scratch("split-shell");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        app.focus = Slot::files(Side::Remote);

        press(&mut app, 'S');
        assert_eq!(app.layout.panes(), 3, "the remote pane was cut in two");
        assert!(app.in_term(), "and the keyboard went into it");
        assert_eq!(app.tabs[0].terms.len(), 1);

        // A focused terminal owns the keyboard, so S only means "close it"
        // once you have clicked back onto the files.
        app.focus_pane(Slot::files(Side::Remote));
        assert!(!app.in_term());
        press(&mut app, 'S');
        assert_eq!(app.layout.panes(), 2);
        assert_eq!(app.focus, Slot::files(Side::Remote));
        assert!(app.tabs[0].terms.is_empty(), "and the session is over");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_second_shell_opens_either_way_up_without_closing_the_first() {
        let dir = scratch("split-both-ways");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        app.focus = Slot::files(Side::Remote);

        press(&mut app, '|');
        assert_eq!(app.layout.panes(), 3, "a shell beside the files");
        // S here would close that one again, which is exactly what these two
        // keys are for.
        app.focus_pane(Slot::files(Side::Remote));
        press(&mut app, '_');
        assert_eq!(app.layout.panes(), 4, "and one below, the other still up");
        assert_eq!(app.tabs[0].terms.len(), 2, "both still running");

        // One border each way, so the two keys did not do the same thing.
        let dividers = app.layout.areas(Rect::new(0, 0, 80, 24)).dividers;
        assert!(dividers.iter().any(|d| d.dir == Dir::Across));
        assert!(dividers.iter().any(|d| d.dir == Dir::Down));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_terminal_no_pane_is_showing_is_not_left_running() {
        let dir = scratch("orphan");
        let mut app = app_in(&dir);
        let slot = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        assert_eq!(app.local_terms.len(), 1);

        // Taken out of the arrangement by hand, as an arrangement change does.
        app.layout.retain(|s| s != slot);
        app.settle_focus();
        assert!(app.local_terms.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_last_pane_is_not_closable() {
        let dir = scratch("last-pane");
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, dir.to_str().unwrap());
        assert_eq!(app.layout.panes(), 1, "a tab on this machine is one pane");

        app.close_pane(app.focus);
        assert_eq!(app.layout.panes(), 1);
        assert!(app.status.contains("W closes the tab"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_keyboard_moves_from_pane_to_pane() {
        let dir = scratch("move-focus");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", Some(Shell::spawn_local(&dir, 24, 80)));
        let shell = app.layout.find(Slot::is_term).expect("the tab's");
        app.focus = Slot::files(Side::Local);

        app.move_focus(Dir::Across, true);
        assert_eq!(app.focus, Slot::files(Side::Remote));
        app.move_focus(Dir::Down, true);
        assert_eq!(app.focus, shell);
        app.move_focus(Dir::Across, false);
        assert_eq!(app.focus, Slot::files(Side::Local));
        // Nothing that way leaves the keyboard where it was.
        app.move_focus(Dir::Across, false);
        assert_eq!(app.focus, Slot::files(Side::Local));
        assert_eq!(app.status, "no pane that way");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn t_opens_another_list_on_the_same_machine_where_this_one_is() {
        let dir = scratch("two-lists");
        let mut app = app_in(&dir);
        press(&mut app, 'T');

        assert_eq!(app.layout.panes(), 3, "the local pane was cut in two");
        assert_eq!(app.local.len(), 2, "and there is a list behind the new one");
        let second = app.focus;
        assert!(second.is_files() && second.host() == Side::Local);
        assert_eq!(
            app.dir_of(second),
            app.dir_of(here()),
            "it opens where the one it came from is looking"
        );

        // Closing it again lets go of the directory it was in.
        app.close_pane(second);
        assert_eq!(app.local.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn c_copies_between_two_lists_on_the_same_machine() {
        let dir = scratch("same-machine-copy");
        let mut app = app_in(&dir);
        press(&mut app, 'T');
        let second = app.focus;
        app.goto_local(second, dir.join("dst"));

        app.focus_pane(here());
        select(&mut app, "one.txt");
        assert_eq!(
            app.target(),
            Some(second),
            "the other list is where it goes"
        );
        press(&mut app, 'c');

        // The copy runs off the UI thread, as every local command does.
        let landed = dir.join("dst/one.txt");
        for _ in 0..200 {
            if landed.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(landed.exists(), "nothing arrived in the other list");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copying_into_the_directory_you_are_already_in_is_refused() {
        let dir = scratch("same-dir");
        let mut app = app_in(&dir);
        press(&mut app, 'T');
        app.focus_pane(here());
        select(&mut app, "one.txt");

        press(&mut app, 'c');
        assert!(app.status.contains("same directory"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn with_more_than_two_lists_c_copies_to_the_one_you_were_just_in() {
        let dir = scratch("three-lists");
        let mut app = app_in(&dir);
        press(&mut app, 'T');
        let second = app.focus;
        press(&mut app, 'T');
        let third = app.focus;
        assert_eq!(app.layout.panes(), 4);

        // Coming to the third from the second, the second is "the other one".
        assert_eq!(app.target(), Some(second));

        app.focus_pane(here());
        assert_eq!(app.target(), Some(third), "and now the one just left");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_zoomed_list_has_nothing_to_copy_across_to() {
        let dir = scratch("zoom-target");
        let mut app = app_in(&dir);
        press(&mut app, 'T');
        app.zoomed = true;
        assert_eq!(app.target(), None);
        assert!(!app.other_side_on_screen());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_list_keeps_its_own_directory_and_its_own_cursor() {
        let dir = scratch("own-dir");
        let mut app = app_in(&dir);
        press(&mut app, 'T');
        let second = app.focus;
        app.goto_local(second, dir.join("dst"));

        assert_eq!(app.dir_of(here()), dir.display().to_string());
        assert_eq!(app.dir_of(second), dir.join("dst").display().to_string());
        assert_eq!(
            app.local_cwd(),
            dir,
            "and this machine's own directory is still the first one's"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_arrangement_that_names_a_list_gets_one_made_for_it() {
        // What reopening a workspace does: the panes were written down, the
        // lists behind them were not.
        let dir = scratch("restore-lists");
        let mut app = app_in(&dir);
        let second = Slot::tree(Side::Local, 7);
        app.layout.split(here(), Dir::Across, second, 50);

        app.settle_focus();
        assert_eq!(app.layout.panes(), 3, "the pane was kept, not pruned away");
        assert_eq!(app.dir_of(second), dir.display().to_string());
        assert!(
            app.next_tree_id >= 7,
            "and its number is not handed out again"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_goes_to_the_editor_pane_when_there_is_one() {
        let dir = scratch("editor-pane");
        let mut app = app_in(&dir);
        // A terminal marked as the one that opens files, as the Editor
        // arrangement makes.
        let slot = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.term_mut(slot).expect("just made").opens = Some("\x1b:e {file}\r".into());
        assert_eq!(app.editor_pane(Side::Local), Some(slot));

        app.focus = Slot::files(Side::Local);
        select(&mut app, "one.txt");
        press(&mut app, 'e');

        assert!(
            app.pending_action.is_none(),
            "sshman does not stand aside when the editor is already on screen"
        );
        assert!(app.status.contains("editor pane"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn with_no_editor_pane_a_file_still_opens_the_old_way() {
        let dir = scratch("editor-none");
        let mut app = app_in(&dir);
        app.focus = Slot::files(Side::Local);
        select(&mut app, "one.txt");
        press(&mut app, 'e');
        assert!(
            matches!(app.pending_action, Some(UiAction::Editor { .. })),
            "the terminal is handed over, the way it always was"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clicking_a_file_only_opens_it_where_there_is_somewhere_to_open_it() {
        let dir = scratch("click-open");
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);

        let at = app
            .pane(here())
            .view
            .iter()
            .position(|e| e.name == "one.txt")
            .expect("the scratch file");

        // No editor pane: a click moves the cursor and nothing else, since
        // opening a file would be a surprise.
        app.click_row(files, at);
        assert_eq!(app.pane(here()).selected_name().as_deref(), Some("one.txt"));
        assert!(app.pending_action.is_none());
        assert!(app.status.is_empty(), "{}", app.status);

        let slot = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.term_mut(slot).expect("just made").opens = Some(String::new());
        // A click of its own rather than the second half of a double, which
        // would be testing the other door into the editor.
        app.last_click = None;
        app.click_row(files, at);
        assert!(app.status.contains("editor pane"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_clicks_on_a_directory_open_it_and_one_click_does_not() {
        let dir = scratch("click-through");
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);
        let at = app
            .pane(here())
            .view
            .iter()
            .position(|e| e.name == "dst")
            .expect("the scratch directory");

        // One click is aim, not action: the cursor moves and the list stays.
        app.click_row(files, at);
        assert_eq!(app.pane(here()).selected_name().as_deref(), Some("dst"));
        assert!(
            !app.local_cwd().ends_with("dst"),
            "one click must not walk into it"
        );

        // The second, soon enough and on the same row, means the row.
        app.click_row(files, at);
        assert!(
            app.local_cwd().ends_with("dst"),
            "two clicks open it, at {}",
            app.local_cwd().display()
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_clicks_far_enough_apart_are_two_clicks() {
        let dir = scratch("click-slow");
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);
        let at = app
            .pane(here())
            .view
            .iter()
            .position(|e| e.name == "dst")
            .expect("the scratch directory");

        app.click_row(files, at);
        // The same row again, but long enough afterwards to be a second
        // approach rather than a double click.
        app.last_click = Some((files, at, Instant::now() - Duration::from_secs(5)));
        app.click_row(files, at);
        assert!(
            !app.local_cwd().ends_with("dst"),
            "a slow second click is still one click"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_pointer_lights_a_row_without_taking_the_cursor_off_its_own() {
        let dir = scratch("hover");
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);
        let on = app.pane(here()).selected_name();
        let at = app
            .pane(here())
            .view
            .iter()
            .position(|e| e.name == "one.txt")
            .expect("the scratch file");

        app.hover_row(files, at);
        assert_eq!(app.hovered_row(files), Some(at));
        assert_eq!(
            app.hovered_row(Slot::files(Side::Remote)),
            None,
            "the row belongs to the list it is in"
        );
        assert_eq!(
            app.pane(here()).selected_name(),
            on,
            "hovering is not selecting"
        );

        // A key can move the list out from under the pointer, so the light
        // goes out rather than pointing at whatever slid under it.
        key(&mut app, KeyCode::Down);
        assert_eq!(app.hovered_row(files), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resting_on_a_tab_says_the_whole_name_once_the_chip_has_cut_it() {
        let dir = scratch("tab-tip");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01.production.example.com", None);
        fake_tab(&mut app, "web02.production.example.com", None);
        // What the drawing would have left behind: two chips, each with a
        // close button, and a chevron standing for a tab off the end.
        app.tab_spans = vec![(0, 12, 0), (13, 25, 1), (25, 29, 5)];
        app.tab_close_buttons = vec![(10, 12, 0), (23, 25, 1)];
        app.tab_bar_row = Some(1);

        app.rest_on_tab(1);
        // Not yet: crossing the row on the way somewhere else says nothing.
        assert_eq!(app.tab_tip(8), None, "it spoke before it was asked");

        app.tab_rest = Some((1, Instant::now() - Duration::from_secs(2)));
        let (index, name) = app.tab_tip(8).expect("the name of the tab under it");
        assert_eq!(index, 1);
        assert_eq!(name, "me@web02.production.example.com");
        // And nothing to say where the chip already showed the whole thing.
        assert_eq!(app.tab_tip(80), None);

        // A chevron is not a chip: it stands for a tab off the end of the row
        // and pointing at it is not pointing at that tab.
        assert_eq!(app.tab_index_at(26), None);
        assert_eq!(app.tab_index_at(14), Some(1));

        // A key can close the tab the label is about.
        key(&mut app, KeyCode::Down);
        assert_eq!(app.tab_tip(8), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_menu_over_a_marked_row_is_about_everything_that_is_marked() {
        // The rule every file manager uses, and the one that makes "mark
        // six, right click, delete" mean what it looks like it means: a right
        // click *inside* a selection leaves the selection alone. On a row
        // that is not part of one it aims first, so "Rename" renames what you
        // pointed at rather than what the cursor happened to be sitting on.
        let dir = scratch("menu-marks");
        std::fs::write(dir.join("two.txt"), "x").unwrap();
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);
        app.pane_mut(files).set_entries(vec![
            remote_entry("one.txt"),
            remote_entry("two.txt"),
            remote_entry("three.txt"),
        ]);
        app.pane_mut(files).select_index(0);
        app.pane_mut(files).marked.insert("two.txt".into());
        app.pane_mut(files).marked.insert("three.txt".into());

        // Row 1 is `two.txt`, which is marked.
        app.open_menu(files, Some(1), 4, 4);
        assert_eq!(
            app.pane(files).marked.len(),
            2,
            "a right click inside the selection threw it away"
        );
        assert_eq!(
            app.pane(files).state.selected(),
            Some(0),
            "and it moved the cursor off it as well"
        );
        assert_eq!(app.pane(files).targets().len(), 2);

        // Row 0 is not marked, so that one is aimed at.
        app.close_menu();
        app.open_menu(files, Some(0), 4, 4);
        assert_eq!(app.pane(files).state.selected(), Some(0));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_keys_that_work_a_menu_work_it_and_the_rest_put_it_away() {
        let dir = scratch("menu-keys");
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);
        app.pane_mut(files)
            .set_entries(vec![remote_entry("one.txt")]);
        app.pane_mut(files).select_index(0);

        app.open_menu(files, Some(0), 4, 4);
        let menu = app.menu.as_ref().expect("a menu");
        let first = menu.chosen().expect("it opened on a rule");
        assert_eq!(first, Action::Open, "the first row is the obvious one");

        // Down walks to the next choice, stepping over the rules rather than
        // stopping on one.
        key(&mut app, KeyCode::Down);
        let menu = app.menu.as_ref().expect("a menu");
        assert!(menu.chosen().is_some(), "the light landed on a rule");
        assert_ne!(menu.chosen(), Some(first));

        // Up at the top stays at the top rather than wrapping round.
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Up);
        assert_eq!(app.menu.as_ref().expect("a menu").chosen(), Some(first));

        // Esc puts it away without doing anything.
        key(&mut app, KeyCode::Esc);
        assert!(app.menu.is_none());

        // And so does any key that is not one of the menu's, rather than the
        // keyboard going quiet until somebody guesses Esc.
        app.open_menu(files, Some(0), 4, 4);
        key(&mut app, KeyCode::Char('q'));
        assert!(app.menu.is_none(), "a menu nothing would close");
        assert!(app.confirm.is_none(), "and it quit on the way out");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_clicked_in_the_tree_is_not_sent_to_the_editor() {
        let dir = scratch("click-dir");
        let mut app = app_in(&dir);
        let files = Slot::files(Side::Local);
        let slot = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.term_mut(slot).expect("just made").opens = Some(String::new());

        let at = app
            .pane(here())
            .view
            .iter()
            .position(|e| e.name == "dst")
            .expect("the scratch directory");
        app.click_row(files, at);
        assert!(!app.status.contains("editor pane"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_editor_arrangement_is_a_tree_an_editor_and_a_terminal() {
        let dir = scratch("arrange-editor");
        let mut app = app_in(&dir);
        app.focus = Slot::files(Side::Local);
        app.arrange(Arrangement::Editor);

        assert_eq!(app.layout.panes(), 3);
        assert_eq!(
            app.focus,
            Slot::files(Side::Local),
            "picking files is the point"
        );
        assert!(app.editor_pane(Side::Local).is_some());
        assert_eq!(app.local_terms.len(), 2, "the editor, and a shell under it");

        // The file list is on the left, the editor to its right, the terminal
        // under the editor.
        let areas = app.layout.areas(Rect::new(0, 0, 100, 30));
        let files = areas.of(Slot::files(Side::Local)).unwrap();
        let editor = areas.of(app.editor_pane(Side::Local).unwrap()).unwrap();
        assert!(files.x < editor.x && files.width < editor.width);
        let shell = areas
            .panes
            .iter()
            .find(|(s, _)| s.is_term() && *s != app.editor_pane(Side::Local).unwrap())
            .map(|(_, r)| *r)
            .expect("the terminal");
        assert_eq!(shell.x, editor.x, "underneath it, not beside it");
        assert!(shell.y > editor.y);

        // And going back closes what it opened rather than leaving terminals
        // running out of sight.
        app.arrange(Arrangement::Sides);
        assert_eq!(app.layout, Layout::default());
        assert!(app.local_terms.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_terminal_pane_that_nothing_is_behind_gets_a_terminal() {
        // What reopening a workspace does: the panes were written down, the
        // shells that were in them were not — a pty whose process has ended
        // cannot be. A fresh one opens in the same place.
        let dir = scratch("restore-terms");
        let mut app = app_in(&dir);
        let slot = Slot::term(Side::Local, 9);
        app.layout.split(here(), Dir::Down, slot, 70);

        app.settle_focus();
        assert_eq!(app.layout.panes(), 3, "the pane was kept, not pruned away");
        assert!(app.term(slot).is_some(), "and there is a shell in it");
        assert!(
            app.next_term_id >= 9,
            "its number is not handed out to another"
        );

        // And only once: the next frame finds it already there.
        app.settle_focus();
        assert_eq!(app.local_terms.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_restored_editor_pane_is_an_editor_pane_again() {
        let dir = scratch("restore-editor");
        let mut app = app_in(&dir);
        let slot = Slot::term(Side::Local, 4);
        app.layout.split(here(), Dir::Across, slot, 30);
        // What the workspace said about this machine's terminals.
        app.wants_editor = vec![4];

        app.settle_focus();
        assert_eq!(app.editor_pane(Side::Local), Some(slot));
        assert!(
            app.wants_editor.is_empty(),
            "asked for once — after that the terminal says for itself"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_workspace_writes_down_the_panes_a_tab_had() {
        let dir = scratch("ws-panes");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", Some(Shell::spawn_local(&dir, 24, 80)));
        let shell = app.layout.find(Slot::is_term).expect("the tab's");
        app.term_mut(shell).expect("just made").opens = Some(String::new());

        let items = app.workspace_items();
        assert_eq!(items.len(), 1);
        let saved = items[0].layout().expect("the panes");
        assert!(
            saved.contains(shell),
            "the terminal's pane is part of the arrangement"
        );
        assert_eq!(
            items[0].editors(),
            [shell.id()],
            "and it is written down as the one that opens files"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_remote_terminal_waits_until_its_tab_has_somewhere_to_open_it() {
        let dir = scratch("restore-remote");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        if let Some(tab) = app.tabs.get_mut(0) {
            tab.trees[0].cwd.clear();
        }
        let slot = Slot::term(Side::Remote, 3);
        app.layout
            .split(Slot::files(Side::Remote), Dir::Down, slot, 70);

        app.settle_focus();
        assert!(app.term(slot).is_none(), "nowhere to open it yet");
        assert!(
            app.layout.contains(slot),
            "but the pane waits rather than being closed up"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_waiting_terminal_opens_as_soon_as_its_tab_says_where_it_is() {
        let dir = scratch("restore-remote-later");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        let slot = Slot::term(Side::Remote, 3);
        app.layout
            .split(Slot::files(Side::Remote), Dir::Down, slot, 70);
        if let Some(tab) = app.tabs.get_mut(0) {
            tab.trees[0].cwd.clear();
        }
        app.settle_focus();
        assert!(app.term(slot).is_none());

        // The listing arrives, and the pane that was waiting fills in.
        if let Some(tab) = app.tabs.get_mut(0) {
            tab.trees[0].cwd = dir.display().to_string();
        }
        app.settle_focus();
        assert!(
            app.term(slot).is_some(),
            "the shell opened where it was left"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_tab_does_not_inherit_the_terminals_of_the_one_before_it() {
        let dir = scratch("no-inherit");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", Some(Shell::spawn_local(&dir, 24, 80)));
        // The panes a second connection would open with.
        let mut carried = app.layout.clone();
        carried.retain(|slot| !(slot.is_term() && slot.host() == Side::Remote));
        assert!(carried.find(Slot::is_term).is_none());
        assert_eq!(carried.panes(), 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zoom_follows_the_focus_and_esc_undoes_it() {
        let dir = scratch("zoom");
        let mut app = app_in(&dir);
        assert!(!app.zoomed);
        press(&mut app, 'm');
        assert!(app.zoomed);
        // Esc is the other way out, once it has nothing else to clear.
        key(&mut app, KeyCode::Esc);
        assert!(!app.zoomed);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A connection attempt in flight, as the connection form makes one.
    /// Ctrl-], as a terminal actually sends it: the byte 0x1d, which arrives
    /// spelled `Ctrl-5` unless the terminal can say which key was pressed.
    fn command(app: &mut App) {
        app.on_key(KeyEvent {
            code: KeyCode::Char('5'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
    }

    #[test]
    fn every_sshman_key_is_reachable_from_inside_a_shell() {
        let dir = scratch("command");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);
        assert!(app.in_term());

        command(&mut app);
        assert!(app.commanding, "the keyboard is sshman's now");

        // The pane keys, spelled the way the file list spells them.
        press(&mut app, '|');
        assert_eq!(app.layout.panes(), 4, "a second shell opened beside it");
        assert!(app.commanding, "and the keyboard is still sshman's");

        // And the ones that have nothing to do with panes at all, which
        // before this meant leaving the shell to reach them.
        press(&mut app, 'w');
        assert_eq!(app.mode, Mode::Workspaces);
        assert!(!app.commanding, "opening something moves on from arranging");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_arrows_walk_past_a_shell_without_falling_into_it() {
        let dir = scratch("walk");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(here());

        command(&mut app);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.focus, shell, "the keyboard is pointing at the shell");
        assert!(
            app.commanding,
            "but the shell does not have it, or the next arrow would be typed"
        );
        key(&mut app, KeyCode::Up);
        assert_eq!(app.focus, here(), "so the arrows keep walking");

        // ↵ is what hands it over.
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert!(!app.commanding);
        assert_eq!(app.focus, shell);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ctrl_and_an_arrow_still_switches_tabs_while_sshman_has_the_keyboard() {
        let dir = scratch("command-tabs");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);
        fake_tab(&mut app, "three", None);
        assert_eq!(app.active, 2, "the newest tab is the one on screen");
        let shell = add_term(&mut app, Side::Remote, Shell::spawn_local(&dir, 24, 80));

        let chord = |app: &mut App, code| {
            app.on_key(KeyEvent {
                code,
                modifiers: KeyModifiers::CONTROL,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            });
        };

        command(&mut app);
        chord(&mut app, KeyCode::Left);
        assert_eq!(app.active, 1, "the tab before this one");
        assert!(app.commanding, "and the keyboard is still sshman's");
        chord(&mut app, KeyCode::Right);
        assert_eq!(app.active, 2, "and back again");

        // A bare arrow is still the panes': the chord is the only thing the
        // tab strip takes.
        app.focus_pane(app.files_pane(Side::Remote));
        key(&mut app, KeyCode::Down);
        assert_eq!(app.focus, shell, "it moved the keyboard");
        assert_eq!(app.active, 2, "without leaving the tab");

        // Picked up, the chord puts the pane down rather than shoving it.
        press(&mut app, 'g');
        assert!(app.carrying);
        let rect = |app: &App| app.layout.areas(Rect::new(0, 0, 100, 30)).of(shell);
        let before = rect(&app);
        chord(&mut app, KeyCode::Right);
        assert!(!app.carrying, "put down on the way past");
        assert_eq!(app.active, 0, "and the tab switched");
        app.goto_tab(2);
        assert_eq!(rect(&app), before, "the pane stayed where it was");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pane_can_be_shoved_past_its_neighbours() {
        let dir = scratch("shove");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);

        command(&mut app);
        press(&mut app, 'g');
        assert!(app.carrying);

        let rect = |app: &App, slot| app.layout.areas(Rect::new(0, 0, 100, 30)).of(slot);
        let below = rect(&app, shell).expect("drawn");
        let files = rect(&app, here()).expect("drawn");
        assert!(below.y > files.y, "the shell starts underneath");

        key(&mut app, KeyCode::Up);
        assert_eq!(rect(&app, shell), Some(files), "and has changed places");
        assert_eq!(rect(&app, here()), Some(below));
        assert_eq!(app.focus, shell, "the keyboard went with the pane");
        assert!(app.carrying, "still holding it, so the arrows keep shoving");
        assert_eq!(app.layout.panes(), 3, "and nothing opened or closed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pane_can_be_sent_the_whole_way_to_an_edge() {
        // What a swap could never do: a pane stacked under another becomes a
        // column beside everything.
        let dir = scratch("edge");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);

        command(&mut app);
        press(&mut app, 'g');
        app.on_key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });

        let area = Rect::new(0, 0, 100, 30);
        let rect = app.layout.areas(area).of(shell).expect("drawn");
        assert_eq!(rect.height, area.height, "the full height of the tab");
        assert_eq!(rect.right(), area.right(), "hard against the right");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pane_is_put_down_by_anything_that_is_not_a_move() {
        let dir = scratch("put-down");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);

        // Esc puts it down without giving the keyboard back: one step at a
        // time, so a change of mind costs one key.
        command(&mut app);
        press(&mut app, 'g');
        key(&mut app, KeyCode::Esc);
        assert!(!app.carrying);
        assert!(app.commanding, "still sshman's keyboard");

        // And a key that means something else puts it down and then means it,
        // so a stray key is never a trap.
        press(&mut app, 'g');
        press(&mut app, 'm');
        assert!(!app.carrying);
        assert!(app.zoomed, "the m still zoomed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pane_dragged_onto_another_changes_places_with_it() {
        let dir = scratch("drag-pane");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));

        // What the mouse records on the way past.
        app.moving = Some(shell);
        app.move_over = Some(here());
        app.drop_moved_pane();

        let areas = app.layout.areas(Rect::new(0, 0, 100, 30));
        assert!(
            areas.of(shell).unwrap().y < areas.of(here()).unwrap().y,
            "the shell came up to where the file list was"
        );
        assert_eq!(app.focus, shell, "and the keyboard went with it");
        assert!(app.moving.is_none() && app.move_over.is_none());

        // Let go over itself, or over nothing, and nothing happens.
        let before = app.layout.clone();
        app.moving = Some(shell);
        app.move_over = Some(shell);
        app.drop_moved_pane();
        app.moving = Some(shell);
        app.drop_moved_pane();
        assert_eq!(app.layout, before);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn esc_puts_the_keyboard_back_where_it_was() {
        let dir = scratch("command-esc");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);

        command(&mut app);
        key(&mut app, KeyCode::Up);
        assert_eq!(app.focus, here(), "moved away");
        key(&mut app, KeyCode::Esc);
        assert!(!app.commanding);
        assert_eq!(
            app.focus, shell,
            "and put back, still typing where you were"
        );

        // Ctrl-] again does the same, so a double tap is never a surprise.
        command(&mut app);
        command(&mut app);
        assert!(!app.commanding);
        assert_eq!(app.focus, shell);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_command_key_never_reaches_the_shell() {
        let dir = scratch("command-eats");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);

        command(&mut app);
        press(&mut app, 'm');
        assert!(app.zoomed, "it zoomed rather than typing an m");
        assert_eq!(app.layout.panes(), 3, "and nothing was opened or closed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copied_text_is_held_and_handed_to_the_terminal_once() {
        let dir = scratch("copy");
        let mut app = app_in(&dir);
        app.copy("two\nlines".into());

        assert_eq!(app.status, "copied 2 lines");
        assert_eq!(app.copied.as_deref(), Some("two\nlines"));
        assert_eq!(app.take_clipboard().as_deref(), Some("two\nlines"));
        assert_eq!(
            app.take_clipboard(),
            None,
            "the terminal is handed it once, not on every frame"
        );
        assert_eq!(
            app.copied.as_deref(),
            Some("two\nlines"),
            "but sshman keeps it, for pasting back into a pane"
        );

        app.copy("one line".into());
        assert_eq!(app.status, "copied 8 character(s)");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn there_is_nothing_to_copy_without_a_selection() {
        let dir = scratch("copy-none");
        let mut app = app_in(&dir);
        let shell = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));
        app.focus_pane(shell);

        command(&mut app);
        press(&mut app, 'y');
        assert!(app.clipboard_out.is_none());
        assert!(app.status.contains("nothing picked out"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_new_tab_does_not_start_by_asking_where_to() {
        // What the `[+]` button does. A tab on this machine needs nothing
        // filled in and nothing to reach, so no form comes up and no
        // connection is required of you.
        let dir = scratch("newtab");
        let mut app = app_in(&dir);
        app.open_local_tab();

        assert_eq!(app.mode, Mode::Browse, "nothing to fill in");
        assert_eq!(app.pending.len(), 1, "a tab is on its way");
        assert!(matches!(app.pending[0].target, Target::Local));
        assert_eq!(
            app.pending[0].initial_dir.as_deref(),
            Some(dir.to_string_lossy().as_ref()),
            "and it opens on the directory you were already looking at"
        );
    }

    fn fake_pending(app: &mut App) -> u64 {
        let (tx, rx) = std::sync::mpsc::channel();
        let (_, resp_rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        app.next_pending_id += 1;
        let id = app.next_pending_id;
        app.pending.push(PendingConnect {
            id,
            from_form: true,
            target: Target::Ssh(ConnectOpts::default()),
            name: None,
            forwards: Vec::new(),
            editors: Vec::new(),
            dirs: PaneDirs::default(),
            layout: Layout::default(),
            task: None,
            tx,
            rx: resp_rx,
            initial_dir: None,
            install_key: false,
        });
        id
    }

    #[test]
    fn giving_up_on_a_connection_takes_its_label_with_it() {
        let dir = scratch("task-pending");
        let mut app = app_in(&dir);
        let id = fake_pending(&mut app);
        app.handle_resp(
            RespSource::Pending(id),
            Resp::TaskStart("connecting to nowhere…".into()),
        );
        assert_eq!(app.current_task(), Some("connecting to nowhere…"));

        // The attempt is dropped while its worker is still going, so the
        // worker's "finished" will never be read. The label has to go with
        // the attempt, or it sits in the title bar for the rest of the
        // session.
        app.dismiss_connect_screen();
        assert_eq!(app.current_task(), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn closing_a_tab_takes_its_label_with_it() {
        let dir = scratch("task-tab");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        app.handle_resp(
            RespSource::Tab(0),
            Resp::TaskStart("copying 3 items…".into()),
        );
        assert_eq!(app.current_task(), Some("copying 3 items…"));

        app.close_tab();
        assert_eq!(app.current_task(), None, "the worker went with the tab");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_gauge_belongs_to_the_tab_on_screen() {
        let dir = scratch("task-gauge");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);
        app.active = 0;

        // A transfer on the tab being watched.
        app.handle_resp(RespSource::Tab(0), Resp::TaskStart("↓ big.iso".into()));
        app.progress = Some(("↓ big.iso".into(), 1, 2));

        // Another tab finishing something of its own must not clear it.
        app.handle_resp(RespSource::Tab(1), Resp::TaskStart("listing…".into()));
        app.handle_resp(RespSource::Tab(1), Resp::TaskEnd);
        assert!(app.progress.is_some(), "that was somebody else's work");

        app.handle_resp(RespSource::Tab(0), Resp::TaskEnd);
        assert!(app.progress.is_none(), "and this one is finished");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_label_follows_the_tab_you_are_looking_at() {
        let dir = scratch("task-follow");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);
        app.handle_resp(RespSource::Tab(0), Resp::TaskStart("one is busy…".into()));
        app.handle_resp(RespSource::Tab(1), Resp::TaskStart("two is busy…".into()));

        app.active = 1;
        assert_eq!(app.current_task(), Some("two is busy…"));
        app.active = 0;
        assert_eq!(
            app.current_task(),
            Some("one is busy…"),
            "what the tab on screen is doing comes first"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Where `e` would send this file: its real path, or nowhere (meaning it
    /// has to be fetched first).
    fn edit_target(app: &mut App, at: Slot, name: &str) -> Option<PathBuf> {
        app.pending_action = None;
        app.launch_on(at, name, "true".into());
        match app.pending_action.take() {
            Some(UiAction::Editor { path, .. }) => Some(path),
            _ => None,
        }
    }

    #[test]
    fn a_file_on_this_machine_is_edited_where_it_lies() {
        let dir = scratch("edit-in-place");
        std::fs::create_dir_all(dir.join("project/src")).unwrap();
        std::fs::write(dir.join("project/src/main.rs"), b"x").unwrap();
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, &dir.join("project/src").display().to_string());

        // The left pane and a "this machine" tab are the same filesystem, so
        // they have to behave the same way: the editor gets the real path,
        // with the rest of the project around it, not a lone copy in /tmp.
        assert_eq!(
            edit_target(&mut app, Slot::files(Side::Remote), "main.rs"),
            Some(dir.join("project/src/main.rs"))
        );
        assert_eq!(
            edit_target(&mut app, here(), "anything.txt"),
            Some(dir.join("anything.txt"))
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_file_on_a_server_still_comes_here_first() {
        let dir = scratch("edit-remote");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "server", None);
        app.tabs[0].trees[0].cwd = "/etc".into();

        // Nothing to open in place: it is on another machine.
        assert_eq!(
            edit_target(&mut app, Slot::files(Side::Remote), "hosts"),
            None
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_root_owned_file_goes_the_long_way_round_even_on_this_machine() {
        let dir = scratch("edit-sudo");
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, "/etc");
        app.tabs[0].sudo = true;

        // Your editor cannot open it, so it is fetched as root, edited, and
        // pushed back as root — the only way that edit can happen at all.
        assert_eq!(
            edit_target(&mut app, Slot::files(Side::Remote), "shadow"),
            None
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_tab_on_this_machine_is_a_single_pane() {
        let dir = scratch("local-tab");
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, "/etc");

        assert!(!app.other_side_on_screen(), "there is no other side to it");
        assert_eq!(
            app.focus,
            Slot::files(Side::Remote),
            "the keyboard cannot sit on a pane that is not drawn"
        );

        // Tab has nowhere to go, and must not park the keyboard off screen.
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Slot::files(Side::Remote));
        assert!(app.status.contains("one file list"), "{}", app.status);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_theme_chooser_shows_each_one_as_you_look_at_it() {
        let dir = scratch("themes");
        let mut app = app_in(&dir);
        // Pointed at a scratch file: a test must never write over the
        // settings of whoever is running it.
        app.config = Config::at(dir.join("config.json"));
        app.set_theme(app.themes.entries[0].clone());
        let before = app.theme;

        press(&mut app, ',');
        while app.selected_setting() != Setting::Theme {
            key(&mut app, KeyCode::Down);
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Themes);
        assert_eq!(app.theme_sel, 0, "opening on the one you are using");

        // Moving through the list draws in each theme, without writing any of
        // them down: choosing by looking is the whole point.
        key(&mut app, KeyCode::Down);
        assert_eq!(app.theme_sel, 1);
        assert_eq!(app.theme, app.themes.entries[1].theme, "the screen changed");
        assert_eq!(
            app.config.theme_name(),
            Some(app.themes.entries[0].name.as_str()),
            "but nothing was saved"
        );

        std::fs::remove_dir_all(&dir).ok();
        assert_ne!(before, app.theme);
    }

    #[test]
    fn a_theme_is_kept_by_choosing_it_and_dropped_by_backing_out() {
        let dir = scratch("themes-keep");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));
        app.set_theme(app.themes.entries[0].clone());
        let (was, was_name) = (app.theme, app.theme_name.clone());

        // Backing out puts the one you had back, on the screen and in the file.
        app.open_themes();
        key(&mut app, KeyCode::End);
        assert_ne!(app.theme, was, "the last one is on screen");
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Settings, "back to where it was opened from");
        assert_eq!(app.theme, was);
        assert_eq!(app.theme_name, was_name);
        assert_eq!(app.config.theme_name(), Some(was_name.as_str()));

        // Choosing keeps it, and writes it down.
        app.open_themes();
        key(&mut app, KeyCode::End);
        let last = app.themes.entries.last().expect("there are themes").clone();
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Settings);
        assert_eq!(app.theme, last.theme);
        assert_eq!(app.config.theme_name(), Some(last.name.as_str()));

        // And it is in the file, not just in this session.
        let saved = std::fs::read_to_string(dir.join("config.json")).expect("written");
        assert!(saved.contains(&last.name), "{saved}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_theme_chooser_stops_at_both_ends_of_the_list() {
        let dir = scratch("themes-ends");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));

        app.open_themes();
        for _ in 0..100 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(app.theme_sel, app.themes.entries.len() - 1);
        for _ in 0..100 {
            key(&mut app, KeyCode::Up);
        }
        assert_eq!(app.theme_sel, 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_key_can_be_given_to_something_else_and_put_back() {
        let dir = scratch("keys");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));

        press(&mut app, ',');
        for _ in 0..Setting::ALL.len() - 1 {
            key(&mut app, KeyCode::Down);
        }
        assert_eq!(app.selected_setting(), Setting::Keys);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Keys);

        // Down to quit, then the key you want.
        key(&mut app, KeyCode::End);
        assert_eq!(app.selected_action(), Action::Quit);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.rebinding, Some(Action::Quit), "waiting on a key");
        press(&mut app, 'Q');
        assert_eq!(app.rebinding, None);

        // It is the key now, and the old one is not.
        assert_eq!(app.keymap.shown(Action::Quit), "Q");
        assert_eq!(app.config.keys["quit"], ["Q"], "and it is written down");
        app.mode = Mode::Browse;
        press(&mut app, 'q');
        assert!(app.confirm.is_none(), "q no longer quits");
        press(&mut app, 'Q');
        assert_eq!(app.mode, Mode::Confirm, "the new key asks first");
        press(&mut app, 'Q');
        assert!(matches!(app.pending_action, Some(UiAction::Quit)));

        // Del puts it back, and takes the line out of the file with it.
        app.pending_action = None;
        app.open_keys();
        key(&mut app, KeyCode::End);
        key(&mut app, KeyCode::Delete);
        assert_eq!(app.keymap.shown(Action::Quit), "q");
        assert!(app.config.keys.is_empty(), "nothing left to remember");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_key_taken_from_one_thing_is_taken_from_it() {
        let dir = scratch("keys-steal");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));

        app.open_keys();
        app.action_sel = Action::ALL
            .iter()
            .position(|a| *a == Action::Help)
            .expect("help is in the list");
        key(&mut app, KeyCode::Enter);
        press(&mut app, 'w');

        assert_eq!(app.keymap.shown(Action::Help), "w");
        assert_eq!(
            app.keymap.shown(Action::Workspaces),
            "—",
            "it cannot be on both"
        );
        assert!(
            app.status.contains("taken from workspaces"),
            "{}",
            app.status
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn waiting_on_a_key_can_be_backed_out_of() {
        let dir = scratch("keys-esc");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));

        app.open_keys();
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.rebinding, None);
        assert_eq!(app.mode, Mode::Keys, "still in the list, nothing changed");
        assert!(app.config.keys.is_empty());

        key(&mut app, KeyCode::Esc);
        assert_eq!(
            app.mode,
            Mode::Settings,
            "and out to where it was opened from"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keys_from_the_config_file_are_in_use_from_the_start() {
        let dir = scratch("keys-config");
        let mut app = app_in(&dir);
        // What a hand-edited file says.
        app.keymap = crate::keys::Keymap::with(&std::collections::BTreeMap::from([(
            "zoom".to_string(),
            vec!["z".to_string()],
        )]));

        press(&mut app, 'z');
        assert!(app.zoomed, "z zooms");
        press(&mut app, 'm');
        assert!(app.zoomed, "and m does not");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_editor_can_be_set_and_is_remembered() {
        let dir = scratch("editor");
        let mut app = app_in(&dir);
        // Pointed at a scratch file: a test must never write over the
        // settings of whoever is running it.
        app.config = Config::at(dir.join("config.json"));

        press(&mut app, ',');
        assert_eq!(app.mode, Mode::Settings);
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Prompt, "the chosen setting is asked about");

        for c in "hx".chars() {
            press(&mut app, c);
        }
        key(&mut app, KeyCode::Enter);

        assert_eq!(app.editor, "hx", "and in use straight away");
        assert_eq!(
            app.mode,
            Mode::Settings,
            "answering comes back to the pane it was asked from"
        );
        let written = std::fs::read_to_string(dir.join("config.json")).unwrap();
        assert!(written.contains("hx"), "{written}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clearing_the_editor_goes_back_to_the_environment() {
        let dir = scratch("editor-clear");
        let mut app = app_in(&dir);
        app.config = Config::at(dir.join("config.json"));
        app.set_editor("  ".into());

        assert_eq!(app.config.editor, None, "a blank one is not stored");
        assert_eq!(app.editor, crate::config::default_editor());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zoom_is_only_offered_where_it_would_change_something() {
        let dir = scratch("zoom-useful");
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, "/etc");

        // One pane, no shell: there is nothing a zoom could hide.
        assert!(!app.zoom_has_anything_to_hide());
        press(&mut app, 'm');
        assert!(!app.zoomed, "and pressing for one must not pretend");
        assert!(app.status.contains("single pane"), "{}", app.status);

        // A shell under it is something to zoom past.
        let shell = add_term(&mut app, Side::Remote, Shell::spawn_local(&dir, 24, 80));
        assert!(app.zoom_has_anything_to_hide());
        press(&mut app, 'm');
        assert!(app.zoomed);

        // A shell closed while zoomed must not strand anyone.
        app.layout.retain(|s| s != shell);
        app.settle_focus();
        press(&mut app, 'm');
        assert!(!app.zoomed, "un-zooming is always allowed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn c_picks_files_up_on_a_local_tab_without_zooming() {
        let dir = scratch("local-yank");
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, "/etc");
        app.tabs[0].trees[0].pane.select_index(0);

        press(&mut app, 'c');
        let clip = app.clip.as_ref().expect("c picked it up");
        assert_eq!(clip.names, ["one.txt"]);
        assert_eq!(clip.dir, "/etc", "relative to the tab's own directory");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_clipboard_does_not_follow_you_to_another_tab() {
        let dir = scratch("clip-tab");
        let mut app = app_in(&dir);
        fake_local_tab(&mut app, "/etc");
        fake_tab(&mut app, "server", None);
        app.goto_tab(0);
        app.tabs[0].trees[0].pane.select_index(0);
        press(&mut app, 'c');
        assert!(app.clip.is_some());

        // Those names mean a directory on the tab they came from. On another
        // tab they would mean another machine's files, or nothing.
        app.goto_tab(1);
        assert!(app.clip.is_none(), "it stays where it was picked up");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn each_tab_keeps_its_own_pane_sizes() {
        let dir = scratch("tab-sizes");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);
        app.active = 0;
        app.adopt_layout();

        // Set the first tab up wide.
        let wide = Layout::sides(30);
        app.layout = wide.clone();
        app.stash_layout();
        app.goto_tab(1);
        assert_eq!(
            app.layout,
            Layout::default(),
            "a tab that was never resized is still the default"
        );

        // The second tab gets sizes of its own.
        app.resize_pane(Dir::Across, 6);
        let second = app.layout.clone();
        app.goto_tab(0);
        assert_eq!(app.layout, wide, "the first tab's sizes come back with it");
        app.goto_tab(1);
        assert_eq!(app.layout, second, "and so do the second's");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_first_tab_opens_with_the_sizes_it_was_given() {
        let dir = scratch("first-tab");
        let mut app = app_in(&dir);
        let saved = Layout::sides(40);
        // Nothing is open, so `active` already points at where this one
        // lands. Its own sizes have to survive that.
        fake_tab_with(&mut app, "one", None, saved.clone());
        assert_eq!(app.layout, saved);
        assert_eq!(app.tabs[0].layout, saved);

        // And the tab already on screen keeps what it had when a second
        // arrives with sizes of its own.
        app.resize_pane(Dir::Across, -4);
        let first = app.layout.clone();
        fake_tab_with(&mut app, "two", None, Layout::default());
        assert_eq!(app.layout, Layout::default(), "the new tab is on screen");
        assert_eq!(app.tabs[0].layout, first, "the old one is unharmed");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_workspace_records_the_sizes_on_screen_not_the_ones_from_before() {
        let dir = scratch("ws-sizes");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        app.focus = Slot::files(Side::Remote);
        app.panes_area = Rect::new(0, 2, 100, 30);

        // Resize, then save without switching tabs first — the tab's own copy
        // has to be up to date by then.
        app.on_key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::ALT | KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        let live = app.layout.clone();
        assert_ne!(live, Layout::default(), "the key did something");

        let items = app.workspace_items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].layout(), Some(live));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resetting_only_touches_the_tab_on_screen() {
        let dir = scratch("reset-one");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);
        app.goto_tab(0);
        let wide = Layout::sides(35);
        app.layout = wide.clone();
        app.stash_layout();
        app.goto_tab(1);
        press(&mut app, '=');
        assert_eq!(app.layout, Layout::default());

        app.goto_tab(0);
        assert_eq!(app.layout, wide, "the other tab keeps what it was given");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resetting_puts_the_layout_back() {
        let dir = scratch("reset");
        let mut app = app_in(&dir);
        app.layout = Layout::sides(75);
        app.zoomed = true;
        press(&mut app, '=');
        assert_eq!(
            app.layout,
            Layout::default(),
            "the borders go back to the middle"
        );
        assert!(!app.zoomed);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- the row of tabs ---------------------------------------------------

    #[test]
    fn a_tab_can_be_shoved_along_the_row() {
        let dir = scratch("tab-move");
        let mut app = app_in(&dir);
        for host in ["one", "two", "three"] {
            fake_tab(&mut app, host, None);
        }
        app.goto_tab(1);

        app.move_tab(1);
        assert_eq!(titles(&app), ["one", "three", "two"]);
        assert_eq!(
            app.active, 2,
            "the tab you were looking at is still on screen"
        );
        assert_eq!(app.tab().unwrap().conn.host, "two");

        // And off the end, which wraps the way stepping between them does.
        app.move_tab(1);
        assert_eq!(titles(&app), ["two", "one", "three"]);
        assert_eq!(app.active, 0);

        app.move_tab(-1);
        assert_eq!(titles(&app), ["one", "three", "two"]);
        assert_eq!(app.tab().unwrap().conn.host, "two");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn moving_a_tab_leaves_the_one_on_screen_on_screen() {
        let dir = scratch("tab-move-other");
        let mut app = app_in(&dir);
        for host in ["one", "two", "three", "four"] {
            fake_tab(&mut app, host, None);
        }
        app.goto_tab(3);

        // A tab dragged past the one you are looking at must not carry the
        // screen with it.
        assert!(app.move_tab_to(0, 2));
        assert_eq!(titles(&app), ["two", "three", "one", "four"]);
        assert_eq!(app.tab().unwrap().conn.host, "four", "still on screen");

        assert!(app.move_tab_to(3, 0));
        assert_eq!(titles(&app), ["four", "two", "three", "one"]);
        assert_eq!(app.active, 0, "and it followed itself to the front");

        assert!(!app.move_tab_to(1, 1), "nowhere to move is not a move");
        assert!(!app.move_tab_to(0, 9), "and neither is off the end");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn any_tab_can_be_closed_not_only_the_one_on_screen() {
        let dir = scratch("tab-close-any");
        let mut app = app_in(&dir);
        for host in ["one", "two", "three"] {
            fake_tab(&mut app, host, None);
        }
        app.goto_tab(2);
        // The tab on screen keeps whatever was done to it right up to the
        // moment another one is closed.
        app.layout = Layout::sides(80);

        app.close_tab_at(0);
        assert_eq!(titles(&app), ["two", "three"]);
        assert_eq!(app.active, 1, "what was on screen is still on screen");
        assert_eq!(app.tab().unwrap().conn.host, "three");
        assert_eq!(app.layout, Layout::sides(80), "and still looks like itself");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_zoom_belongs_to_the_tab_it_was_asked_for_on() {
        let dir = scratch("tab-zoom");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "one", None);
        fake_tab(&mut app, "two", None);

        app.goto_tab(0);
        app.toggle_zoom();
        assert!(app.zoomed);

        app.goto_tab(1);
        assert!(!app.zoomed, "the other tab was never zoomed");
        app.goto_tab(0);
        assert!(app.zoomed, "and this one still is");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The tabs in the order the row draws them, by the host each is on.
    fn titles(app: &App) -> Vec<String> {
        app.tabs.iter().map(|t| t.conn.host.clone()).collect()
    }

    // ---- leaving -----------------------------------------------------------

    #[test]
    fn quitting_asks_first() {
        let dir = scratch("quit-ask");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01", None);

        press(&mut app, 'q');
        assert_eq!(app.mode, Mode::Confirm);
        assert!(app.pending_action.is_none(), "nothing has happened yet");
        assert!(
            app.confirm
                .as_ref()
                .unwrap()
                .body
                .iter()
                .any(|l| l.contains("web01")),
            "and it says what would go with it"
        );

        // Saying no puts everything back.
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Browse);
        assert!(app.pending_action.is_none());
        assert_eq!(app.tabs.len(), 1, "and nothing was closed on the way");

        // Asked again and meant: the same key is the answer.
        press(&mut app, 'q');
        press(&mut app, 'q');
        assert!(matches!(app.pending_action, Some(UiAction::Quit)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn being_asked_about_quitting_does_not_close_what_you_were_looking_at() {
        let dir = scratch("quit-return");
        let mut app = app_in(&dir);
        app.open_workspaces();
        assert_eq!(app.mode, Mode::Workspaces);

        app.ask_quit();
        key(&mut app, KeyCode::Esc);
        assert_eq!(
            app.mode,
            Mode::Workspaces,
            "cancelling puts you back where you were asked"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- an editor pane on its own -----------------------------------------

    #[test]
    fn an_editor_pane_opens_and_closes_on_one_key() {
        let dir = scratch("editor-pane");
        let mut app = app_in(&dir);
        assert!(app.editor_pane(Side::Local).is_none());

        press(&mut app, 'i');
        let pane = app.editor_pane(Side::Local).expect("an editor pane");
        assert!(app.layout.contains(pane));
        assert!(
            app.term(pane).is_some_and(Term::is_editor),
            "and the file lists know to send files to it"
        );
        assert_eq!(app.focus, here(), "the keyboard stays on the files");

        press(&mut app, 'i');
        assert!(
            app.editor_pane(Side::Local).is_none(),
            "the same key takes it away again"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- what a workspace writes down --------------------------------------

    #[test]
    fn a_workspace_writes_down_where_every_pane_was_pointed() {
        let dir = scratch("ws-dirs");
        std::fs::create_dir_all(dir.join("dst/deep")).unwrap();
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01", None);

        // A second list on this machine, pointed somewhere else.
        let second = app.add_tree(here()).expect("another list");
        app.goto_local(second, dir.join("dst"));
        // And a second on the server.
        let far = app
            .add_tree(Slot::files(Side::Remote))
            .expect("another list");
        app.tabs[0].tree_mut(far.id()).unwrap().cwd = "/var/log".into();

        let dirs = app.local_pane_dirs();
        assert_eq!(
            dirs.tree(second.id()).map(std::path::PathBuf::from),
            Some(dir.join("dst")),
            "this machine's second list says where it is"
        );
        let item = workspace_item_for(&app.tabs[0]).expect("a saveable tab");
        assert_eq!(
            item.dirs().tree(far.id()),
            Some("/var/log"),
            "and so does the server's"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shell_is_written_down_with_the_directory_it_is_in() {
        let dir = scratch("ws-shell-dir");
        let mut app = app_in(&dir);
        let slot = add_term(&mut app, Side::Local, Shell::spawn_local(&dir, 24, 80));

        let dirs = app.local_pane_dirs();
        assert_eq!(
            dirs.shell(slot.id()).map(std::path::PathBuf::from),
            Some(dir.clone()),
            "a shell that has not moved is written down where it started"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pane_a_workspace_named_opens_where_the_workspace_left_it() {
        let dir = scratch("ws-restore-dir");
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        let mut app = app_in(&dir);

        // What launching a workspace leaves behind for the panes to read.
        app.wants_dirs = PaneDirs {
            trees: vec![PaneDir {
                id: 7,
                path: dir.join("dst").display().to_string(),
            }],
            shells: vec![PaneDir {
                id: 8,
                path: dir.join("dst").display().to_string(),
            }],
        };
        app.layout
            .split(here(), Dir::Across, Slot::tree(Side::Local, 7), 50);
        app.layout
            .split(here(), Dir::Down, Slot::term(Side::Local, 8), 50);
        app.settle_focus();

        let tree = app.local.iter().find(|t| t.id == 7).expect("the list");
        assert_eq!(tree.cwd, dir.join("dst"), "the list opened where it was");
        assert_eq!(
            app.term_start_dir(0, Side::Local, 8)
                .parse::<std::path::PathBuf>()
                .unwrap(),
            dir.join("dst"),
            "and so did the shell"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_that_has_gone_does_not_stop_a_pane_opening() {
        let dir = scratch("ws-restore-gone");
        let mut app = app_in(&dir);
        app.wants_dirs = PaneDirs {
            trees: vec![PaneDir {
                id: 7,
                path: "/no/such/place".into(),
            }],
            shells: vec![PaneDir {
                id: 8,
                path: "/no/such/place".into(),
            }],
        };
        app.layout
            .split(here(), Dir::Across, Slot::tree(Side::Local, 7), 50);
        app.settle_focus();

        assert_eq!(
            app.local.iter().find(|t| t.id == 7).expect("the list").cwd,
            dir,
            "a list falls back to where this machine is"
        );
        assert_eq!(
            app.term_start_dir(0, Side::Local, 8),
            dir.display().to_string(),
            "and so does a shell, which would not start at all otherwise"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_shell_a_workspace_asked_for_starts_on_every_tab_not_only_the_one_on_screen() {
        let dir = scratch("ws-background-shells");
        let mut app = app_in(&dir);
        let cwd = dir.display().to_string();
        fake_local_tab(&mut app, &cwd);
        fake_local_tab(&mut app, &cwd);

        // What a restored workspace leaves behind: a pane for a terminal that
        // has not been started yet, on a tab that is not on screen.
        let far = Slot::term(Side::Remote, 41);
        let near = Slot::term(Side::Remote, 42);
        app.tabs[0]
            .layout
            .split(Slot::files(Side::Remote), Dir::Down, far, 60);
        app.layout
            .split(Slot::files(Side::Remote), Dir::Down, near, 60);
        app.stash_layout();
        assert_eq!(app.active, 1, "the second tab is the one on screen");

        app.settle_focus();

        assert!(
            app.tabs[1].terms.iter().any(|t| t.id == 42),
            "the tab on screen gets its shell"
        );
        assert!(
            app.tabs[0].terms.iter().any(|t| t.id == 41),
            "and so does the one behind it, rather than waiting to be looked at"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- the session --------------------------------------------------------

    #[test]
    fn the_session_is_written_down_in_the_shape_a_workspace_takes() {
        let dir = scratch("session-shape");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01", None);
        app.tabs[0].trees[0].cwd = "/etc".into();

        let snapshot = app.session_snapshot();
        assert_eq!(snapshot.name, crate::workspace::SESSION_NAME);
        assert_eq!(snapshot.items.len(), 1);
        assert_eq!(snapshot.items[0].path(), Some("/etc"));
        assert_eq!(
            snapshot.local_path,
            Some(dir.display().to_string()),
            "and this machine's half of it"
        );

        // It round-trips through the file it is kept in.
        let text = serde_json::to_string(&snapshot).unwrap();
        let back: crate::workspace::Workspace = serde_json::from_str(&text).unwrap();
        assert_eq!(back.items[0].path(), Some("/etc"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_previous_session_sits_at_the_top_of_the_workspace_list() {
        let dir = scratch("session-row");
        let mut app = app_in(&dir);
        // Pushed rather than saved: a test has no business writing over the
        // list this machine actually keeps.
        app.workspaces.entries.push(crate::workspace::Workspace {
            name: "prod".into(),
            local_path: None,
            local_editors: Vec::new(),
            local_dirs: PaneDirs::default(),
            items: Vec::new(),
            saved_at: 0,
        });
        assert_eq!(app.workspace_rows(), 1);
        assert_eq!(
            app.workspace_at(0),
            Some(0),
            "with none, row 0 is a saved one"
        );

        app.previous_session = Some(app.session_snapshot());
        assert_eq!(app.workspace_rows(), 2);
        assert_eq!(app.workspace_at(0), None, "the session goes above them");
        assert_eq!(app.workspace_at(1), Some(0));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn coming_back_to_the_previous_session_reconnects_what_it_held() {
        let dir = scratch("session-restore");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01", None);
        app.tabs[0].trees[0].cwd = "/srv".into();
        // Taken before anything else happens, the way startup takes it.
        app.previous_session = Some(app.session_snapshot());
        app.tabs.clear();
        app.pending.clear();

        app.restore_previous_session();
        assert_eq!(app.pending.len(), 1, "it starts connecting again");
        assert_eq!(
            app.pending[0].initial_dir.as_deref(),
            Some("/srv"),
            "and opens where it was left"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn starting_up_offers_the_session_before_this_one() {
        let dir = scratch("session-offer");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01", None);
        // The fixture's tab has no options behind it, and the offer names
        // what a real one would be reconnected as.
        app.tabs[0].target = Target::Ssh(ConnectOpts {
            user: "me".into(),
            host: "web01".into(),
            port: 22,
            ..Default::default()
        });
        app.previous_session = Some(app.session_snapshot());
        app.tabs.clear();
        app.pending.clear();
        app.mode = Mode::Connect;

        app.offer_previous_session();
        assert_eq!(app.mode, Mode::Confirm, "it asks rather than assuming");
        let asked = app.confirm.as_ref().expect("a question");
        assert!(matches!(asked.action, ConfirmAction::RestoreSession));
        assert!(!asked.danger, "coming back is not a dangerous answer");
        assert!(
            asked.body.iter().any(|l| l.contains("me@web01")),
            "it names what it would open: {:?}",
            asked.body
        );

        // Yes opens it, exactly as `--resume` would have.
        press(&mut app, 'y');
        assert_eq!(app.pending.len(), 1);
        assert!(app.confirm.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn saying_no_to_the_offer_leaves_the_session_where_it_was() {
        let dir = scratch("session-declined");
        let mut app = app_in(&dir);
        fake_tab(&mut app, "web01", None);
        app.previous_session = Some(app.session_snapshot());
        app.tabs.clear();
        app.pending.clear();
        app.mode = Mode::Connect;

        app.offer_previous_session();
        press(&mut app, 'n');
        assert_eq!(
            app.mode,
            Mode::Connect,
            "no goes back to the connection screen, not to panes that are not there"
        );
        assert!(app.pending.is_empty(), "and connects to nothing");
        assert!(
            app.previous_session.is_some(),
            "the session is still there for w to reach"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_session_with_nothing_in_it_is_not_worth_asking_about() {
        let dir = scratch("session-empty");
        let mut app = app_in(&dir);
        app.mode = Mode::Connect;

        app.previous_session = None;
        app.offer_previous_session();
        assert_eq!(app.mode, Mode::Connect);

        // Recorded, but with no connections to come back to.
        app.previous_session = Some(app.session_snapshot());
        assert!(app.previous_session.as_ref().unwrap().items.is_empty());
        app.offer_previous_session();
        assert_eq!(app.mode, Mode::Connect, "nothing to offer, so nothing said");
        assert!(app.confirm.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
