//! Application state and key handling.
//!
//! Local filesystem work happens inline (it is fast); anything touching the
//! network is sent to the worker thread and answered asynchronously.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::backend::{BackendKind, Target};
use crate::forward::{Forward, Spec as ForwardSpec};
use crate::history::History;
use crate::input::TextInput;
use crate::local;
use crate::shell::Shell;
use crate::sshconn::ConnectOpts;
use crate::types::{FileEntry, rbasename, rjoin, rparent};
use crate::worker::{HostKeyIssue, Req, Resp};
use crate::workspace::{Item as WorkspaceItem, Workspaces};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Local,
    Remote,
}

impl Side {
    pub fn other(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }

    /// Index into the per-side arrays (shells).
    pub fn index(self) -> usize {
        match self {
            Self::Local => 0,
            Self::Remote => 1,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Connect,
    /// Choosing a container to open.
    Picker,
    /// Managing forwarded ports.
    Forwards,
    /// Choosing a saved set of connections.
    Workspaces,
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
    Mkdir(Side),
    Rename(Side, String),
    Filter(Side),
    GoTo(Side),
    SudoPassword,
    /// Ask which program to open `name` with, then run it on that side.
    OpenWith(Side, String),
    /// A name for the server on screen.
    NameTab,
    /// A name for the highlighted server in the recent list.
    NameSaved(usize),
    /// A name to save the current set of connections under.
    SaveWorkspace,
    /// A port to forward from the server on screen.
    AddForward,
    /// Name for a new archive holding the given entries.
    Archive(Side, Vec<String>),
    /// Directory to unpack the named archive into.
    Extract(Side, String),
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
    AcceptHostKey,
    /// Overwrite the recorded host key for this server. Guarded by a typed
    /// phrase, because the innocent explanation and an attack look identical
    /// from here.
    ReplaceHostKey,
}

pub struct ConfirmState {
    pub title: String,
    pub body: Vec<String>,
    pub action: ConfirmAction,
    pub danger: bool,
    /// When set, `y` is not enough: the user must type this word exactly.
    pub require_phrase: Option<String>,
    pub input: TextInput,
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

/// Something the main loop must do with the terminal released.
pub enum UiAction {
    Editor {
        program: String,
        path: PathBuf,
        push_back: Option<PendingEdit>,
        refresh_local: bool,
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

/// Which half of a side has the keyboard: the file list or its shell.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Region {
    Files,
    Shell,
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
    pub pane: Pane,
    pub cwd: String,
    pub shell: Option<Shell>,
    /// Ports carried from this server to this machine.
    pub forwards: Vec<Forward>,
    tx: Sender<Req>,
    pub rx: Receiver<Resp>,
    /// Guards against a stale listing overwriting a newer one.
    seq: u64,
    /// Name to put the cursor on once the next listing arrives.
    pending_select: Option<String>,
}

impl RemoteTab {
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
            // A container's host field already reads `name` or `name@server`.
            BackendKind::Container => self.conn.host.clone(),
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
    tx: Sender<Req>,
    pub rx: Receiver<Resp>,
    initial_dir: Option<String>,
    /// Install the public key once this connection succeeds.
    install_key: bool,
}

pub struct App {
    pub mode: Mode,
    pub focus: Side,
    pub region: Region,
    /// Where each region was last drawn, recorded by the renderer so the mouse
    /// can be matched to a region exactly instead of by recomputing layout.
    pub files_area: [Rect; 2],
    pub shell_area: [Option<Rect>; 2],
    /// Screen span of each tab chip, recorded by the renderer so a click can
    /// be matched to a tab.
    pub tab_spans: Vec<(u16, u16, usize)>,
    pub tab_bar_row: Option<u16>,
    /// Rows the scrollable overlays were last drawn with, so scrolling can
    /// stop at the end of the content instead of running into blank space.
    pub output_view_height: u16,
    pub help_view_height: u16,
    /// Rows given to a shell pane, including its border.
    pub shell_height: u16,
    pub local: Pane,
    pub local_cwd: PathBuf,
    pub local_shell: Option<Shell>,
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
    pub tasks: Vec<String>,

    pub form: ConnectForm,
    pub history: History,
    pub connect_focus: ConnectFocus,
    pub history_sel: usize,
    pub prompt: Option<PromptState>,
    pub confirm: Option<ConfirmState>,
    pub picker: Option<PickerState>,
    pub workspaces: Workspaces,
    pub workspace_sel: usize,
    pub forward_sel: usize,
    /// Connections from a workspace that could not be made without a
    /// password. Kept so the user is told, and so `C` can offer them.
    pub needs_password: Vec<(String, ConnectOpts)>,
    pub output: Vec<String>,
    pub output_title: String,
    pub output_scroll: u16,
    pub help_scroll: u16,

    pub cmd_history: Vec<String>,
    pub editor: String,
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
}

impl App {
    pub fn new(
        opts: ConnectOpts,
        local_start: PathBuf,
        remote_start: Option<String>,
        auto_connect: bool,
    ) -> Self {
        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".to_string());
        let pager = std::env::var("PAGER").unwrap_or_else(|_| "less".to_string());
        let history = History::load();
        let (local_tx, local_rx) = std::sync::mpsc::channel();

        let mut app = Self {
            mode: if auto_connect {
                Mode::Browse
            } else {
                Mode::Connect
            },
            focus: Side::Local,
            region: Region::Files,
            files_area: [Rect::ZERO; 2],
            shell_area: [None, None],
            tab_spans: Vec::new(),
            tab_bar_row: None,
            output_view_height: 1,
            help_view_height: 1,
            shell_height: 12,
            local: Pane::default(),
            local_cwd: local_start,
            local_shell: None,
            tabs: Vec::new(),
            active: 0,
            pending: Vec::new(),
            next_pending_id: 0,
            empty_pane: Pane::default(),
            status: "Connecting…".into(),
            status_level: Level::Info,
            progress: None,
            tasks: Vec::new(),
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
            workspace_sel: 0,
            forward_sel: 0,
            needs_password: Vec::new(),
            output: Vec::new(),
            output_title: String::new(),
            output_scroll: 0,
            help_scroll: 0,
            cmd_history: Vec::new(),
            editor,
            pager,
            opts,
            host_key_issue: None,
            pending_action: None,
            should_quit: false,
            initial_remote: remote_start,
            local_tx,
            local_rx,
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

    pub fn tab_mut(&mut self) -> Option<&mut RemoteTab> {
        self.tabs.get_mut(self.active)
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

    /// Is sudo mode on for the tab on screen?
    pub fn sudo(&self) -> bool {
        self.tab().is_some_and(|t| t.sudo)
    }

    pub fn remote_cwd(&self) -> String {
        self.tab().map(|t| t.cwd.clone()).unwrap_or_default()
    }

    pub fn set_status(&mut self, msg: impl Into<String>, level: Level) {
        self.status = msg.into();
        self.status_level = level;
    }

    pub fn pane(&self, side: Side) -> &Pane {
        match side {
            Side::Local => &self.local,
            Side::Remote => match self.tab() {
                Some(tab) => &tab.pane,
                None => &self.empty_pane,
            },
        }
    }

    pub fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Local => &mut self.local,
            // With no tab, edits land in the scratch pane and go nowhere,
            // which is exactly right: there is nothing to navigate.
            Side::Remote => match self.tabs.get_mut(self.active) {
                Some(tab) => &mut tab.pane,
                None => &mut self.empty_pane,
            },
        }
    }

    pub fn path_of(&self, side: Side) -> String {
        match side {
            Side::Local => self.local_cwd.display().to_string(),
            Side::Remote => match self.tab() {
                Some(tab) if !tab.cwd.is_empty() => tab.cwd.clone(),
                _ => "—".to_string(),
            },
        }
    }

    // ---- loading -----------------------------------------------------------

    pub fn reload_local(&mut self) {
        match local::list_dir(&self.local_cwd) {
            Ok(entries) => self.local.set_entries(entries),
            Err(e) => {
                self.local.all.clear();
                self.local.view.clear();
                self.local.state.select(None);
                self.local.loading = false;
                self.local.error = Some(e.to_string());
            }
        }
    }

    pub fn reload_remote(&mut self) {
        self.reload_tab(self.active);
    }

    fn goto_local(&mut self, path: PathBuf) {
        if !path.is_dir() {
            self.set_status(format!("not a directory: {}", path.display()), Level::Bad);
            return;
        }
        // Canonicalise so `..` chains stay tidy, but keep the original if the
        // path cannot be resolved (a dangling symlink, say).
        self.local_cwd = std::fs::canonicalize(&path).unwrap_or(path);
        self.local.on_dir_change();
        self.reload_local();
    }

    fn goto_remote(&mut self, path: String) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            self.set_status("not connected", Level::Bad);
            return;
        };
        tab.pane.on_dir_change();
        tab.seq += 1;
        let req = Req::GoTo {
            path,
            sudo: tab.sudo,
            seq: tab.seq,
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

    fn connect_to_inner(&mut self, target: Target, status: String, from_form: bool) {
        if !status.is_empty() {
            self.set_status(status, Level::Info);
        }

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
            tx,
            rx,
            // Copied, not taken: an attempt can be retried — after accepting a
            // host key, say — and the retry must carry the same intent. Both
            // are cleared once a connection actually succeeds.
            initial_dir: self.initial_remote.clone(),
            install_key: self.form.install_key,
        });
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
            self.tasks.pop();
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
        self.tasks.push(label);
        let tx = self.local_tx.clone();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        std::thread::Builder::new()
            .name("local-task".into())
            .spawn(move || {
                let outcome = match std::process::Command::new(shell)
                    .arg("-c")
                    .arg(&cmd)
                    .output()
                {
                    Ok(out) if out.status.success() => {
                        let warning = String::from_utf8_lossy(&out.stderr);
                        let mut message = success;
                        if let Some(line) = warning.lines().find(|l| !l.trim().is_empty()) {
                            message.push_str(&format!(" — tar said: {}", line.trim()));
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
                        message: format!("could not run tar: {e}"),
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
    fn list_archive(&mut self, side: Side) {
        let Some(entry) = self.pane(side).selected().cloned() else {
            return;
        };
        if !crate::archive::is_archive(&entry.name) {
            self.set_status(format!("{} is not a tar archive", entry.name), Level::Bad);
            return;
        }
        let dir = self.path_of(side);
        let cmd = crate::archive::list_command(&dir, &entry.name);
        match side {
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
    fn start_archive(&mut self, side: Side) {
        let names = self.pane(side).targets();
        if names.is_empty() {
            self.set_status("nothing selected to pack", Level::Info);
            return;
        }
        let dir = self.path_of(side);
        let dir_name = match side {
            Side::Local => self
                .local_cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default(),
            Side::Remote => rbasename(&self.remote_cwd()),
        };
        let suggestion = crate::archive::suggested_name(&names, &dir_name);
        self.open_prompt(
            PromptKind::Archive(side, names.clone()),
            format!("Pack {} item(s) from {dir} into", names.len()),
            suggestion,
        );
    }

    /// Ask where to unpack the archive under the cursor.
    fn start_extract(&mut self, side: Side) {
        let Some(entry) = self.pane(side).selected().cloned() else {
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
            PromptKind::Extract(side, entry.name.clone()),
            format!("Unpack {} into directory", entry.name),
            dest,
        );
    }

    fn run_archive(&mut self, side: Side, names: Vec<String>, archive: String) {
        let dir = self.path_of(side);
        match side {
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

    fn run_extract(&mut self, side: Side, archive: String, dest: String) {
        let dir = self.path_of(side);
        match side {
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
        for pending in self.pending.drain(..) {
            let _ = pending.tx.send(Req::Quit);
        }
        for tab in &self.tabs {
            let _ = tab.tx.send(Req::Quit);
        }
        // Dropping the tabs stops their shells too.
        self.tabs.clear();
        self.local_shell = None;
    }

    /// Fold in one worker message. `source` says which worker it came from,
    /// so a background tab's chatter cannot be mistaken for the active one's.
    pub fn handle_resp(&mut self, source: RespSource, resp: Resp) {
        // Task labels go in the title bar, not the status line: routine
        // listings would otherwise wipe out the result of whatever the user
        // just did and then sit there stale.
        match resp {
            Resp::TaskStart(label) => {
                self.tasks.push(label);
                return;
            }
            Resp::TaskEnd => {
                self.tasks.pop();
                if self.tasks.is_empty() {
                    self.progress = None;
                }
                return;
            }
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

                self.tabs.push(RemoteTab {
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
                    pane: Pane::default(),
                    cwd: String::new(),
                    shell: None,
                    forwards: Vec::new(),
                    tx: pending.tx,
                    rx: pending.rx,
                    seq: 0,
                    pending_select: None,
                });
                self.active = self.tabs.len() - 1;

                self.form.connecting = false;
                self.host_key_issue = None;
                if let Some(opts) = pending.target.ssh_opts() {
                    self.needs_password.retain(|(_, waiting)| {
                        !(waiting.user == opts.user
                            && waiting.host == opts.host
                            && waiting.port == opts.port)
                    });
                }
                self.mode = Mode::Browse;
                self.focus = Side::Local;
                self.region = Region::Files;
                let title = self.tab().map(|t| t.title()).unwrap_or_default();
                self.set_status(format!("Connected to {title}{note}"), Level::Good);
                self.goto_remote(start);

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
                let (from_form, target_opts, name) =
                    match self.pending.iter().position(|p| p.id == id) {
                        Some(position) => {
                            let pending = self.pending.remove(position);
                            let _ = pending.tx.send(Req::Quit);
                            (
                                pending.from_form,
                                pending.target.ssh_opts().cloned(),
                                pending.name.clone(),
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
                        if !self.needs_password.iter().any(|(l, _)| l == &label) {
                            self.needs_password.push((label, opts));
                        }
                        let waiting: Vec<&str> = self
                            .needs_password
                            .iter()
                            .map(|(l, _)| l.as_str())
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
            Resp::Listing { path, entries, seq } => {
                let Some(tab) = self.tabs.get_mut(index) else {
                    return;
                };
                if seq != tab.seq {
                    return; // a newer request has already been sent
                }
                tab.cwd = path;
                tab.pane.set_entries(entries);
                if let Some(name) = tab.pending_select.take()
                    && let Some(i) = tab.pane.view.iter().position(|e| e.name == name)
                {
                    tab.pane.select_index(i);
                }
            }

            Resp::ListFailed { path, msg } => {
                if let Some(tab) = self.tabs.get_mut(index) {
                    // Move the pane to the directory that failed, so the header
                    // and the error agree — and so enabling sudo retries *this*
                    // path rather than silently reloading the previous one.
                    tab.cwd = path;
                    tab.pane.all.clear();
                    tab.pane.view.clear();
                    tab.pane.state.select(None);
                    tab.pane.loading = false;
                    tab.pane.error = Some(msg.clone());
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
                    refresh_local: false,
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
                    tab.pane.loading = true;
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
                    tab.pane.loading = false;
                    tab.pane.error = Some(msg.clone());
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
    fn reload_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        if tab.cwd.is_empty() {
            return;
        }
        tab.seq += 1;
        tab.pane.loading = true;
        let req = Req::List {
            path: tab.cwd.clone(),
            sudo: tab.sudo,
            seq: tab.seq,
        };
        let _ = tab.tx.send(req);
    }

    /// Called by the main loop once an external editor has exited.
    pub fn after_editor(&mut self, push_back: Option<PendingEdit>, refresh_local: bool) {
        if refresh_local {
            self.reload_local();
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

        // A focused shell owns the keyboard. Every key goes to it — Ctrl-C has
        // to interrupt the running command, not quit sshman — so the escape
        // key is checked first and is the only way back out.
        if self.mode == Mode::Browse && self.region == Region::Shell {
            if is_shell_escape(&key) {
                self.region = Region::Files;
                self.set_status("file list — F6 goes back to the shell", Level::Info);
            } else if let Some(shell) = self.shell_mut(self.focus) {
                shell.send_key(key);
            } else {
                // The shell went away underneath us.
                self.region = Region::Files;
            }
            return;
        }

        // Ctrl-C quits from anywhere else.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.pending_action = Some(UiAction::Quit);
            return;
        }
        match self.mode {
            Mode::Connect => self.connect_key(key),
            Mode::Picker => self.picker_key(key),
            Mode::Workspaces => self.workspace_key(key),
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

    fn browse_key(&mut self, key: KeyEvent) {
        let side = self.focus;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Checked ahead of plain navigation, which would otherwise swallow
        // these before the modifier is ever looked at.
        if ctrl && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            let delta = if key.code == KeyCode::Up { 1 } else { -1 };
            self.resize_shell_pane(delta);
            return;
        }
        if ctrl && matches!(key.code, KeyCode::Left | KeyCode::Right) {
            let delta = if key.code == KeyCode::Right { 1 } else { -1 };
            self.cycle_tab(delta);
            return;
        }
        // Alt-1 … Alt-9 jump straight to a tab.
        if key.modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(c) = key.code
            && let Some(n) = c.to_digit(10)
            && n >= 1
        {
            self.goto_tab(n as usize - 1);
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.pending_action = Some(UiAction::Quit),
            // Esc backs out of whatever narrowing is in effect. It deliberately
            // does not quit: losing a session to a stray Esc is infuriating.
            KeyCode::Esc => {
                let pane = self.pane_mut(side);
                if !pane.filter.is_empty() {
                    let keep = pane.selected_name();
                    pane.filter.clear();
                    pane.refresh_view(keep.as_deref());
                    self.set_status("filter cleared", Level::Info);
                } else if !pane.marked.is_empty() {
                    pane.marked.clear();
                    self.set_status("marks cleared", Level::Info);
                } else {
                    self.set_status("press q to quit", Level::Info);
                }
            }
            KeyCode::Tab | KeyCode::BackTab => self.focus = self.focus.other(),

            KeyCode::Down | KeyCode::Char('j') => self.pane_mut(side).move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.pane_mut(side).move_by(-1),
            KeyCode::PageDown => self.pane_mut(side).move_by(15),
            KeyCode::PageUp => self.pane_mut(side).move_by(-15),
            KeyCode::Home | KeyCode::Char('g') => self.pane_mut(side).select_index(0),
            KeyCode::End | KeyCode::Char('G') => {
                let last = self.pane(side).view.len().saturating_sub(1);
                self.pane_mut(side).select_index(last);
            }

            KeyCode::Left | KeyCode::Char('h') => self.go_up(side),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.activate(side),

            KeyCode::Char(' ') => {
                self.pane_mut(side).toggle_mark();
                self.pane_mut(side).move_by(1);
            }
            KeyCode::Char('a') => {
                let pane = self.pane_mut(side);
                if pane.marked.is_empty() {
                    pane.marked = pane.view.iter().map(|e| e.name.clone()).collect();
                } else {
                    pane.marked.clear();
                }
            }

            KeyCode::Char('c') | KeyCode::F(5) => self.copy_to_other_side(),
            KeyCode::Char('e') | KeyCode::F(4) => {
                let program = self.editor.clone();
                self.open_with(side, program);
            }
            KeyCode::Char('E') => {
                if let Some(name) = self.pane(side).selected_name() {
                    let mut input = TextInput::new(self.editor.clone());
                    input.cursor = input.value.chars().count();
                    self.prompt = Some(PromptState {
                        kind: PromptKind::OpenWith(side, name.clone()),
                        title: format!("Open {name} with"),
                        input,
                        hist_idx: None,
                        return_to: self.mode,
                    });
                    self.mode = Mode::Prompt;
                }
            }
            KeyCode::Char('v') => {
                let pager = self.pager.clone();
                self.open_with(side, pager);
            }

            KeyCode::Char('n') | KeyCode::F(7) => self.open_prompt(
                PromptKind::Mkdir(side),
                format!("New directory in {}", self.path_of(side)),
                String::new(),
            ),
            KeyCode::Char('r') | KeyCode::F(2) => {
                if let Some(name) = self.pane(side).selected_name() {
                    self.open_prompt(
                        PromptKind::Rename(side, name.clone()),
                        format!("Rename {name} to"),
                        name,
                    );
                }
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::F(8) => self.request_delete(side),

            KeyCode::Char('R') => {
                self.reload_local();
                self.reload_remote();
                self.set_status("refreshed", Level::Info);
            }
            KeyCode::Char('.') => {
                let keep = self.pane(side).selected_name();
                let pane = self.pane_mut(side);
                pane.show_hidden = !pane.show_hidden;
                pane.refresh_view(keep.as_deref());
            }
            KeyCode::Char('/') => self.open_prompt(
                PromptKind::Filter(side),
                "Filter".into(),
                self.pane(side).filter.clone(),
            ),
            KeyCode::Char('f') => self.open_prompt(
                PromptKind::GoTo(side),
                format!("Go to directory ({})", side_name(side)),
                self.path_of(side),
            ),

            KeyCode::Char(':') => {
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
            KeyCode::Char('!') => {
                if !self.connected() {
                    self.set_status("not connected", Level::Bad);
                } else {
                    self.pending_action = Some(UiAction::Shell);
                }
            }
            KeyCode::Char('~') => self.go_home(side),
            KeyCode::Char('D') => self.find_containers(side),
            KeyCode::Char('N') => self.start_rename_tab(),
            KeyCode::Char('w') => self.open_workspaces(),
            KeyCode::Char('p') => self.open_forwards(),
            KeyCode::Char('z') => self.start_archive(side),
            KeyCode::Char('x') => self.start_extract(side),
            KeyCode::Char('X') => self.list_archive(side),

            // ---- embedded shells ----
            KeyCode::Char('S') => self.toggle_shell(side),
            // One key to get into the shell, whether or not it is open yet.
            KeyCode::F(6) => self.enter_shell(side),
            KeyCode::Char(']') if ctrl => self.enter_shell(side),
            KeyCode::Char('s') => self.toggle_sudo(),
            KeyCode::Char('t') => self.mirror_path(),
            KeyCode::Char('o') => {
                if self.output.is_empty() {
                    self.set_status("no command output yet", Level::Info);
                } else {
                    self.mode = Mode::Output;
                }
            }
            // Both open the connection screen; a successful connection always
            // arrives as a new tab, leaving the ones you have alone.
            KeyCode::Char('C') | KeyCode::Char('T') => self.open_connect_screen(),
            KeyCode::Char('W') => self.close_tab(),
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.help_scroll = 0;
                self.mode = Mode::Help;
            }
            _ => {}
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
            PromptKind::Mkdir(side) => {
                if value.is_empty() {
                    return;
                }
                match side {
                    Side::Local => {
                        let path = self.local_cwd.join(&value);
                        match local::mkdir(&path) {
                            Ok(()) => {
                                self.set_status(format!("created {value}"), Level::Good);
                                self.reload_local();
                            }
                            Err(e) => self.set_status(e.to_string(), Level::Bad),
                        }
                    }
                    Side::Remote => self.send(Req::Mkdir {
                        path: rjoin(&self.remote_cwd(), &value),
                        sudo: self.sudo(),
                    }),
                }
            }
            PromptKind::Rename(side, old) => {
                if value.is_empty() || value == old {
                    return;
                }
                match side {
                    Side::Local => {
                        let from = self.local_cwd.join(&old);
                        let to = self.local_cwd.join(&value);
                        match local::rename(&from, &to) {
                            Ok(()) => {
                                self.set_status(format!("renamed to {value}"), Level::Good);
                                self.reload_local();
                            }
                            Err(e) => self.set_status(e.to_string(), Level::Bad),
                        }
                    }
                    Side::Remote => self.send(Req::Rename {
                        from: rjoin(&self.remote_cwd(), &old),
                        to: rjoin(&self.remote_cwd(), &value),
                        sudo: self.sudo(),
                    }),
                }
            }
            PromptKind::Filter(side) => {
                let keep = self.pane(side).selected_name();
                let pane = self.pane_mut(side);
                pane.filter = value;
                pane.refresh_view(keep.as_deref());
            }
            PromptKind::GoTo(side) => {
                if value.is_empty() {
                    return;
                }
                match side {
                    Side::Local => self.goto_local(local::expand(&value)),
                    Side::Remote => self.goto_remote(value),
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
        }
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
                self.mode = Mode::Browse;
                match state.action {
                    ConfirmAction::DeleteLocal(paths) => {
                        let mut failed = Vec::new();
                        let total = paths.len();
                        for p in &paths {
                            if let Err(e) = local::remove(p) {
                                failed.push(e.to_string());
                            }
                        }
                        self.local.marked.clear();
                        self.reload_local();
                        if failed.is_empty() {
                            self.set_status(format!("{total} item(s) deleted"), Level::Good);
                        } else {
                            self.set_status(failed.join("; "), Level::Bad);
                        }
                    }
                    ConfirmAction::DeleteRemote(paths) => {
                        self.pane_mut(Side::Remote).marked.clear();
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
                }
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                let was_host_key = matches!(
                    self.confirm.as_ref().map(|c| &c.action),
                    Some(ConfirmAction::AcceptHostKey | ConfirmAction::ReplaceHostKey)
                );
                self.confirm = None;
                if was_host_key && !self.connected() {
                    self.mode = Mode::Connect;
                    self.form.error = Some("host key rejected".into());
                } else {
                    self.mode = Mode::Browse;
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

    fn go_up(&mut self, side: Side) {
        match side {
            Side::Local => {
                if let Some(parent) = self.local_cwd.parent() {
                    let leaving = self
                        .local_cwd
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string());
                    let parent = parent.to_path_buf();
                    self.goto_local(parent);
                    // Put the cursor back on the directory we just left.
                    if let Some(name) = leaving
                        && let Some(i) = self.local.view.iter().position(|e| e.name == name)
                    {
                        self.local.select_index(i);
                    }
                }
            }
            Side::Remote => {
                let cwd = self.remote_cwd();
                if cwd.is_empty() || cwd == "/" {
                    return;
                }
                let leaving = rbasename(&cwd);
                self.goto_remote(rparent(&cwd));
                if let Some(tab) = self.tab_mut() {
                    tab.pending_select = Some(leaving);
                }
            }
        }
    }

    fn activate(&mut self, side: Side) {
        let Some(entry) = self.pane(side).selected().cloned() else {
            return;
        };
        if entry.is_dir_like() {
            match side {
                Side::Local => self.goto_local(self.local_cwd.join(&entry.name)),
                Side::Remote => self.goto_remote(rjoin(&self.remote_cwd(), &entry.name)),
            }
        } else {
            let program = self.editor.clone();
            self.launch_on(side, &entry.name, program);
        }
    }

    fn open_with(&mut self, side: Side, program: String) {
        let Some(entry) = self.pane(side).selected().cloned() else {
            return;
        };
        if entry.is_dir_like() {
            self.set_status("that is a directory — press Enter to open it", Level::Info);
            return;
        }
        self.launch_on(side, &entry.name, program);
    }

    /// Open `name` on `side` with `program`. Local files go straight to the
    /// editor; remote files are fetched first and pushed back on exit.
    fn launch_on(&mut self, side: Side, name: &str, program: String) {
        match side {
            Side::Local => {
                let path = self.local_cwd.join(name);
                if path.is_dir() {
                    self.set_status("that is a directory", Level::Info);
                    return;
                }
                self.pending_action = Some(UiAction::Editor {
                    program,
                    path,
                    push_back: None,
                    refresh_local: true,
                });
            }
            Side::Remote => {
                if !self.connected() {
                    self.set_status("not connected", Level::Bad);
                    return;
                }
                self.send(Req::FetchForEdit {
                    path: rjoin(&self.remote_cwd(), name),
                    sudo: self.sudo(),
                    editor: program,
                });
            }
        }
    }

    fn copy_to_other_side(&mut self) {
        if !self.connected() {
            self.set_status("not connected", Level::Bad);
            return;
        }
        let from = self.focus;
        let names = self.pane(from).targets();
        if names.is_empty() {
            self.set_status("nothing selected", Level::Info);
            return;
        }
        match from {
            Side::Local => {
                let items: Vec<PathBuf> = names.iter().map(|n| self.local_cwd.join(n)).collect();
                self.send(Req::Upload {
                    items,
                    dest: self.remote_cwd(),
                    sudo: self.sudo(),
                });
            }
            Side::Remote => {
                let items: Vec<String> =
                    names.iter().map(|n| rjoin(&self.remote_cwd(), n)).collect();
                self.send(Req::Download {
                    items,
                    dest: self.local_cwd.clone(),
                    sudo: self.sudo(),
                });
            }
        }
        self.pane_mut(from).marked.clear();
    }

    fn request_delete(&mut self, side: Side) {
        let names = self.pane(side).targets();
        if names.is_empty() {
            self.set_status("nothing selected", Level::Info);
            return;
        }
        let mut body = vec![format!(
            "Permanently delete {} item(s) from {}:",
            names.len(),
            self.path_of(side)
        )];
        for n in names.iter().take(10) {
            body.push(format!("  {n}"));
        }
        if names.len() > 10 {
            body.push(format!("  … and {} more", names.len() - 10));
        }
        if side == Side::Remote && self.sudo() {
            body.push(String::new());
            body.push("SUDO MODE IS ON — this deletes as root.".into());
        }

        let action = match side {
            Side::Local => {
                ConfirmAction::DeleteLocal(names.iter().map(|n| self.local_cwd.join(n)).collect())
            }
            Side::Remote => ConfirmAction::DeleteRemote(
                names.iter().map(|n| rjoin(&self.remote_cwd(), n)).collect(),
            ),
        };
        self.confirm = Some(ConfirmState::simple("Confirm delete", body, action, true));
        self.mode = Mode::Confirm;
    }

    /// Jump a pane to its home directory. On the remote side that is the
    /// directory the server put us in at login.
    fn go_home(&mut self, side: Side) {
        match side {
            Side::Local => {
                if let Some(home) = dirs::home_dir() {
                    self.goto_local(home);
                }
            }
            Side::Remote => {
                if let Some(home) = self.tab().map(|t| t.conn.home.clone()) {
                    self.goto_remote(home);
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
            Mode::Browse if self.region == Region::Shell => {
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
                "Forward port (3000, or 8080:3000, or 8080:host:3000)".into(),
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
            .any(|f| f.spec.local_port == spec.local_port)
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
                self.set_status(format!("forwarding {}", spec.describe()), Level::Good);
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
            .min(self.workspaces.len().saturating_sub(1));
        self.mode = Mode::Workspaces;
        if self.workspaces.is_empty() {
            self.set_status(
                "no workspaces yet — s saves what you have open now",
                Level::Info,
            );
        }
    }

    fn workspace_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Browse,
            KeyCode::Down | KeyCode::Char('j') => {
                self.workspace_sel =
                    (self.workspace_sel + 1).min(self.workspaces.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.workspace_sel = self.workspace_sel.saturating_sub(1);
            }
            KeyCode::Char('s') => {
                let suggestion = self
                    .workspaces
                    .get(self.workspace_sel)
                    .map(|w| w.name.clone())
                    .unwrap_or_default();
                self.open_prompt(
                    PromptKind::SaveWorkspace,
                    format!("Save these {} connection(s) as", self.tabs.len()),
                    suggestion,
                );
            }
            KeyCode::Delete => {
                if let Some(removed) = self.workspaces.remove(self.workspace_sel) {
                    self.set_status(format!("forgot workspace {}", removed.name), Level::Info);
                }
                self.workspace_sel = self
                    .workspace_sel
                    .min(self.workspaces.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                self.mode = Mode::Browse;
                let index = self.workspace_sel;
                self.launch_workspace_at(index);
            }
            _ => {}
        }
    }

    /// Turn the tabs on screen into something that can be saved.
    fn workspace_items(&self) -> Vec<WorkspaceItem> {
        self.tabs.iter().filter_map(workspace_item_for).collect()
    }

    fn save_workspace(&mut self, name: &str) {
        if self.tabs.is_empty() {
            self.set_status("nothing open to save", Level::Bad);
            return;
        }
        let items = self.workspace_items();
        let skipped = self.tabs.len() - items.len();
        let local = Some(self.local_cwd.display().to_string());

        match self.workspaces.save(name, local, items) {
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
                self.goto_local(path);
            }
        }
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
            self.connect_to(target, String::new());
            // `connect_to` copies `initial_remote`, so hand the rest over too.
            if let Some(pending) = self.pending.last_mut() {
                pending.name = name;
                pending.forwards = item.forwards().to_vec();
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

    // ---- containers ---------------------------------------------------------

    /// Look for containers to open. Which docker daemon is asked follows the
    /// pane you are on: the local pane means this machine, the remote pane
    /// means the server that tab is connected to.
    /// Go straight to the container chooser for this machine, with no server
    /// in the picture — what `--docker` does.
    pub fn browse_local_containers(&mut self) {
        self.mode = Mode::Browse;
        self.find_containers(Side::Local);
    }

    fn find_containers(&mut self, side: Side) {
        match side {
            Side::Local => {
                self.tasks.push("looking for local containers…".into());
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
                "no server connected — C connects, D opens a local container, q quits",
                Level::Info,
            );
        }
    }

    /// Open the connection screen to add another server.
    pub fn open_connect_screen(&mut self) {
        // A workspace may have left connections waiting on a password; offer
        // those first, filled in, so all that is missing is the password.
        if let Some((label, opts)) = self.needs_password.first().cloned() {
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
        self.active = index;
        // The new tab may not have a shell open, so do not leave the keyboard
        // pointing at one that is not there.
        if self.focus == Side::Remote && !self.has_shell(Side::Remote) {
            self.region = Region::Files;
        }
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

    /// Close the tab on screen, ending its SSH session and its shell.
    pub fn close_tab(&mut self) {
        if self.tabs.is_empty() {
            self.set_status("no tab to close", Level::Info);
            return;
        }
        let tab = self.tabs.remove(self.active);
        let title = tab.title();
        let _ = tab.tx.send(Req::Quit);
        drop(tab); // takes the shell's session down with it

        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.focus = Side::Local;
            self.region = Region::Files;
            self.set_status(format!("closed {title} — no servers left"), Level::Info);
        } else {
            if self.focus == Side::Remote && !self.has_shell(Side::Remote) {
                self.region = Region::Files;
            }
            if let Some(opts) = self.tab().and_then(|t| t.ssh_opts()) {
                self.opts = opts.clone();
            }
            self.set_status(format!("closed {title}"), Level::Info);
        }
    }

    // ---- embedded shells ---------------------------------------------------

    /// Open or close the shell under a pane.
    fn toggle_shell(&mut self, side: Side) {
        if self.has_shell(side) {
            // Dropping the shell tells its thread to shut the session down.
            self.close_shell(side);
            if self.focus == side {
                self.region = Region::Files;
            }
            self.set_status(format!("{} shell closed", side_name(side)), Level::Info);
        } else {
            self.open_shell(side);
        }
    }

    /// Put the keyboard in a shell, starting one first if need be.
    fn enter_shell(&mut self, side: Side) {
        if !self.has_shell(side) {
            self.open_shell(side);
            return;
        }
        self.focus = side;
        self.region = Region::Shell;
        self.set_status("shell — F6 or Ctrl-] returns to the files", Level::Info);
    }

    fn open_shell(&mut self, side: Side) {
        // A placeholder size: the first draw calls `ensure_size` with the space
        // the pane actually got, which resizes both emulator and pty.
        const ROWS: u16 = 24;
        const COLS: u16 = 80;

        let shell = match side {
            Side::Local => Shell::spawn_local(&self.local_cwd, ROWS, COLS),
            Side::Remote => {
                let Some(tab) = self.tab() else {
                    self.set_status("not connected", Level::Bad);
                    return;
                };
                // This tab's own credentials, and its own connection, so a
                // busy shell never stalls transfers — on this tab or any other.
                match (&tab.target, tab.ssh_opts()) {
                    // A container is entered with `docker exec -it`, run
                    // either here or on the server that hosts it.
                    (
                        Target::Docker {
                            container, runtime, ..
                        },
                        ssh,
                    ) => {
                        let cmdline = crate::docker::interactive_shell_command(runtime, container);
                        let label = tab.title();
                        match ssh {
                            None => Shell::spawn_local_command(label, cmdline, ROWS, COLS),
                            Some(opts) => {
                                Shell::spawn_remote_command(label, opts, cmdline, ROWS, COLS)
                            }
                        }
                    }
                    (Target::Ssh(opts), _) => {
                        Shell::spawn_remote(opts, &tab.cwd.clone(), ROWS, COLS)
                    }
                }
            }
        };
        self.set_shell(side, Some(shell));
        self.focus = side;
        self.region = Region::Shell;
        self.set_status(
            format!(
                "{} shell open in {} — F6 or Ctrl-] returns to the files",
                side_name(side),
                self.path_of(side)
            ),
            Level::Good,
        );
    }

    /// Grow (positive) or shrink the shell pane on the focused side.
    fn resize_shell_pane(&mut self, delta: i16) {
        if !self.has_shell(self.focus) {
            return;
        }
        let next = (self.shell_height as i16 + delta).clamp(3, 60);
        self.shell_height = next as u16;
    }

    /// The local pane's shell, or the active tab's.
    pub fn shell(&self, side: Side) -> Option<&Shell> {
        match side {
            Side::Local => self.local_shell.as_ref(),
            Side::Remote => self.tab().and_then(|t| t.shell.as_ref()),
        }
    }

    pub fn shell_mut(&mut self, side: Side) -> Option<&mut Shell> {
        match side {
            Side::Local => self.local_shell.as_mut(),
            Side::Remote => self
                .tabs
                .get_mut(self.active)
                .and_then(|t| t.shell.as_mut()),
        }
    }

    fn set_shell(&mut self, side: Side, shell: Option<Shell>) {
        match side {
            Side::Local => self.local_shell = shell,
            Side::Remote => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.shell = shell;
                }
            }
        }
    }

    /// Dropping the shell tells its thread to shut the session down.
    fn close_shell(&mut self, side: Side) {
        self.set_shell(side, None);
    }

    /// True when `side` has a shell taking up part of the pane.
    pub fn has_shell(&self, side: Side) -> bool {
        self.shell(side).is_some()
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

    /// Point the other pane at the same path as the focused one.
    fn mirror_path(&mut self) {
        match self.focus {
            Side::Local => {
                let path = self.local_cwd.display().to_string();
                self.goto_remote(path);
            }
            Side::Remote => {
                let path = PathBuf::from(&self.remote_cwd());
                if path.is_dir() {
                    self.goto_local(path);
                } else {
                    self.set_status(
                        format!("no local directory {}", self.remote_cwd()),
                        Level::Bad,
                    );
                }
            }
        }
    }
}

/// The one chord that gets the keyboard back out of a focused shell.
/// Everything else has to reach the shell, including Esc and Ctrl-C.
fn is_shell_escape(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::F(6) => true,
        KeyCode::Char(']') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
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
    let path = Some(tab.cwd.clone()).filter(|p| !p.is_empty());
    match &tab.target {
        Target::Ssh(opts) => Some(WorkspaceItem::Ssh {
            user: opts.user.clone(),
            host: opts.host.clone(),
            port: opts.port,
            key_path: opts.key_path.as_ref().map(|p| p.display().to_string()),
            name: tab.name.clone(),
            path,
            forwards: saved_forwards(tab),
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
                })
            }),
            path,
            forwards: saved_forwards(tab),
        }),
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
