//! How the panes are arranged.
//!
//! One of these belongs to each tab, and one more to the screen you get before
//! anything is connected, so a server you have set up wide stays wide when you
//! come back to it. A workspace writes them down with everything else it
//! remembers about a tab.
//!
//! An arrangement is a tree: every node is either one pane or two arrangements
//! side by side, with a percentage saying how the space between them is
//! divided. The pair sshman starts with — your machine on the left, the server
//! on the right — is not a special case in the drawing code, it is just the
//! tree you begin with:
//!
//! ```text
//! Split{ across, 50%, Files(local), Files(remote) }
//! ```
//!
//! so splitting a pane, closing one, dragging a border and zooming are all one
//! set of operations rather than one set per shape the screen might take.
//!
//! Only the arrangement lives here. Which pane has the keyboard and whether
//! one of them is zoomed follow you from tab to tab instead of being stored
//! per tab, so they are the app's business rather than this struct's.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

/// Which of the two machines a pane is looking at: the one sshman is running
/// on, or the one the tab is connected to.
///
/// This is about whose files and whose shell, not about where on screen the
/// pane ended up — an arrangement can put the two in any order, or leave one
/// of them out.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(rename_all = "lowercase")]
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
}

/// Tells one terminal from another on the same machine. Handed out by the app,
/// which owns the terminals themselves; a pane only ever holds the number.
pub type TermId = u32;

/// Tells one file list from another on the same machine, the same way.
pub type TreeId = u32;

/// The file list every machine starts with, and the one everything that means
/// "this machine's directory" — a shell's working directory, what a workspace
/// writes down — is about. Fixed rather than handed out, so an arrangement
/// saved before there could be more than one still names it.
pub const MAIN: TreeId = 0;

/// One pane: whose it is, and what it shows.
///
/// Both kinds are as many as you like. Which machine a file list is on decides
/// how its files are reached — through the worker, or straight off the disk —
/// and everything else about it, the directory included, belongs to the pane.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Hash)]
#[serde(tag = "pane", rename_all = "lowercase")]
pub enum Slot {
    Files {
        host: Side,
        /// Absent in arrangements written before a machine could have more
        /// than one file list, which is exactly what [`MAIN`] means.
        #[serde(default, skip_serializing_if = "is_main")]
        id: TreeId,
    },
    Term {
        host: Side,
        id: TermId,
    },
}

fn is_main(id: &TreeId) -> bool {
    *id == MAIN
}

impl Slot {
    /// The file list a machine has always had.
    pub fn files(host: Side) -> Self {
        Self::Files { host, id: MAIN }
    }

    pub fn tree(host: Side, id: TreeId) -> Self {
        Self::Files { host, id }
    }

    pub fn term(host: Side, id: TermId) -> Self {
        Self::Term { host, id }
    }

    pub fn host(self) -> Side {
        match self {
            Self::Files { host, .. } | Self::Term { host, .. } => host,
        }
    }

    /// The number that tells this pane from the others on its machine.
    pub fn id(self) -> u32 {
        match self {
            Self::Files { id, .. } | Self::Term { id, .. } => id,
        }
    }

    pub fn is_term(self) -> bool {
        matches!(self, Self::Term { .. })
    }

    pub fn is_files(self) -> bool {
        matches!(self, Self::Files { .. })
    }
}

/// Which way a split divides the space it was given.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Side by side, with a border between the columns.
    Across,
    /// One above the other.
    Down,
}

/// Either a pane, or two arrangements sharing the space.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(untagged)]
pub enum Node {
    Leaf(Slot),
    Split {
        dir: Dir,
        /// Share of the space the first child gets, as a percentage.
        ratio: u16,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// How far a divider can be pushed either way. A pane narrow enough to show
/// only its border is not a pane, and dragging back out of one would be
/// fiddly.
const MIN_RATIO: u16 = 10;
const MAX_RATIO: u16 = 90;

/// The least a pane can be cut down to and still be worth drawing: a border
/// either side and a row of its own between them.
const MIN_W: u16 = 8;
const MIN_H: u16 = 3;

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(from = "Stored", into = "Stored")]
pub struct Layout {
    pub root: Node,
}

/// A layout as a file holds it. Arrangements written before there were any —
/// a percentage for the divider and a height for the shell — still open: the
/// percentage is the one thing they say that still means something, since a
/// workspace never reopened the shells anyway.
#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum Stored {
    Tree(Node),
    Sides {
        split_pct: u16,
        #[serde(default)]
        shell_height: u16,
    },
}

impl From<Stored> for Layout {
    fn from(stored: Stored) -> Self {
        match stored {
            Stored::Tree(root) => Self { root },
            Stored::Sides { split_pct, .. } => Self::sides(split_pct),
        }
    }
}

impl From<Layout> for Stored {
    fn from(layout: Layout) -> Self {
        Stored::Tree(layout.root)
    }
}

impl Default for Layout {
    fn default() -> Self {
        Self::sides(50)
    }
}

impl Layout {
    /// Your machine and the server, side by side: what sshman opens with.
    pub fn sides(split_pct: u16) -> Self {
        Self {
            root: Node::Split {
                dir: Dir::Across,
                ratio: split_pct.clamp(MIN_RATIO, MAX_RATIO),
                first: Box::new(Node::Leaf(Slot::files(Side::Local))),
                second: Box::new(Node::Leaf(Slot::files(Side::Remote))),
            },
        }
    }

    /// One pane, filling everything. What a tab on this machine opens with:
    /// putting the local half beside it would be the same filesystem drawn
    /// twice.
    pub fn only(slot: Slot) -> Self {
        Self {
            root: Node::Leaf(slot),
        }
    }

    /// Every pane in the arrangement, in the order they are drawn.
    pub fn slots(&self) -> Vec<Slot> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    pub fn contains(&self, slot: Slot) -> bool {
        self.slots().contains(&slot)
    }

    /// How many panes there are. Never zero.
    pub fn panes(&self) -> usize {
        self.slots().len()
    }

    /// The first pane, for when the one that had the keyboard has gone.
    pub fn first(&self) -> Slot {
        first_leaf(&self.root)
    }

    /// The first pane matching `want`, if there is one.
    pub fn find(&self, want: impl Fn(Slot) -> bool) -> Option<Slot> {
        self.slots().into_iter().find(|s| want(*s))
    }

    /// Divide `at` in two and put `new` in the half that opens up. `ratio` is
    /// the share the pane that was already there keeps.
    pub fn split(&mut self, at: Slot, dir: Dir, new: Slot, ratio: u16) -> bool {
        let Some(path) = self.path_to(at) else {
            return false;
        };
        let Some(node) = self.node_at_mut(&path) else {
            return false;
        };
        *node = Node::Split {
            dir,
            ratio: ratio.clamp(MIN_RATIO, MAX_RATIO),
            first: Box::new(Node::Leaf(at)),
            second: Box::new(Node::Leaf(new)),
        };
        true
    }

    /// Take a pane out; its neighbour takes the space. The last pane cannot
    /// be closed — an arrangement with nothing in it is not an arrangement.
    pub fn remove(&mut self, slot: Slot) -> bool {
        if self.panes() < 2 || !self.contains(slot) {
            return false;
        }
        self.retain(|s| s != slot);
        true
    }

    /// Keep only the panes `keep` says still exist, closing up after the rest.
    ///
    /// This is how a terminal that has been shut, or a tab that has gone,
    /// leaves the arrangement: nothing has to find the split it was part of
    /// and undo it by hand.
    pub fn retain(&mut self, mut keep: impl FnMut(Slot) -> bool) {
        if let Some(root) = prune(&self.root, &mut keep) {
            self.root = root;
        } else {
            // Everything went. There is always a file list to fall back on.
            *self = Self::default();
        }
    }

    /// Put a different pane where this one was, keeping the space it had.
    pub fn replace(&mut self, old: Slot, new: Slot) -> bool {
        let Some(path) = self.path_to(old) else {
            return false;
        };
        match self.node_at_mut(&path) {
            Some(node) => {
                *node = Node::Leaf(new);
                true
            }
            None => false,
        }
    }

    /// Where each pane goes, and where each divider between them lands.
    pub fn areas(&self, area: Rect) -> Areas {
        let mut out = Areas::default();
        let mut path = Vec::new();
        walk(&self.root, area, &mut path, &mut out);
        out
    }

    /// Move the divider nearest `from` in the given direction. Positive gives
    /// the first pane of that split more room.
    ///
    /// "Nearest" means the innermost split above the focused pane that divides
    /// the space the way asked for, which is the one the eye picks out: the
    /// border you are up against.
    pub fn resize_near(&mut self, from: Slot, dir: Dir, delta: i16) -> bool {
        let Some(path) = self.ancestor(from, dir) else {
            return false;
        };
        if let Some(Node::Split { ratio, .. }) = self.node_at_mut(&path) {
            *ratio = (*ratio as i16 + delta).clamp(MIN_RATIO as i16, MAX_RATIO as i16) as u16;
            return true;
        }
        false
    }

    /// Put a divider where the mouse is, from a drag across the space that
    /// split was given.
    pub fn drag(&mut self, path: &[u8], dir: Dir, area: Rect, x: u16, y: u16) {
        let (offset, extent) = match dir {
            Dir::Across => (x.saturating_sub(area.x), area.width),
            Dir::Down => (y.saturating_sub(area.y), area.height),
        };
        if extent == 0 {
            return;
        }
        let pct = (offset.min(extent) as u32 * 100 / extent as u32) as u16;
        if let Some(Node::Split { ratio, .. }) = self.node_at_mut(path) {
            *ratio = pct.clamp(MIN_RATIO, MAX_RATIO);
        }
    }

    /// Share every split evenly. The shape you have arranged is kept; only
    /// the sizes go back to the middle.
    pub fn even(&mut self) {
        fn level(node: &mut Node) {
            if let Node::Split {
                ratio,
                first,
                second,
                ..
            } = node
            {
                *ratio = 50;
                level(first);
                level(second);
            }
        }
        level(&mut self.root);
    }

    /// The pane on the other side of the nearest divider running that way, for
    /// moving the keyboard about.
    pub fn neighbour(&self, from: Slot, dir: Dir, forward: bool) -> Option<Slot> {
        let path = self.path_to(from)?;
        for depth in (0..path.len()).rev() {
            let Some(Node::Split {
                dir: split_dir,
                first,
                second,
                ..
            }) = self.node_at(&path[..depth])
            else {
                continue;
            };
            // Only a split we are on the near side of has anything on the
            // other side of it to move to.
            if *split_dir == dir && (path[depth] == 0) == forward {
                let into = if forward { second } else { first };
                return Some(nearest_leaf(into, !forward));
            }
        }
        None
    }

    /// An arrangement read back from a file, which is to say one we cannot
    /// trust: percentages out of range, and shapes nothing would draw.
    pub fn sane(mut self) -> Self {
        fn fix(node: &mut Node) {
            if let Node::Split {
                ratio,
                first,
                second,
                ..
            } = node
            {
                *ratio = (*ratio).clamp(MIN_RATIO, MAX_RATIO);
                fix(first);
                fix(second);
            }
        }
        fix(&mut self.root);
        // Two panes showing the same thing would each act on the other's
        // keystrokes; the first one wins and the rest close up.
        let mut seen = Vec::new();
        self.retain(|slot| {
            let first = !seen.contains(&slot);
            seen.push(slot);
            first
        });
        self
    }

    // ---- walking the tree --------------------------------------------------

    /// Which turns to take from the root to reach a pane: 0 for the first
    /// child of a split, 1 for the second.
    pub fn path_to(&self, slot: Slot) -> Option<Vec<u8>> {
        fn find(node: &Node, slot: Slot, path: &mut Vec<u8>) -> bool {
            match node {
                Node::Leaf(here) => *here == slot,
                Node::Split { first, second, .. } => {
                    path.push(0);
                    if find(first, slot, path) {
                        return true;
                    }
                    path.pop();
                    path.push(1);
                    if find(second, slot, path) {
                        return true;
                    }
                    path.pop();
                    false
                }
            }
        }
        let mut path = Vec::new();
        find(&self.root, slot, &mut path).then_some(path)
    }

    /// The innermost split above `from` that divides its space the given way.
    fn ancestor(&self, from: Slot, dir: Dir) -> Option<Vec<u8>> {
        let path = self.path_to(from)?;
        (0..path.len()).rev().find_map(|depth| {
            matches!(self.node_at(&path[..depth]), Some(Node::Split { dir: d, .. }) if *d == dir)
                .then(|| path[..depth].to_vec())
        })
    }

    fn node_at(&self, path: &[u8]) -> Option<&Node> {
        let mut node = &self.root;
        for turn in path {
            let Node::Split { first, second, .. } = node else {
                return None;
            };
            node = if *turn == 0 { first } else { second };
        }
        Some(node)
    }

    fn node_at_mut(&mut self, path: &[u8]) -> Option<&mut Node> {
        let mut node = &mut self.root;
        for turn in path {
            let Node::Split { first, second, .. } = node else {
                return None;
            };
            node = if *turn == 0 { first } else { second };
        }
        Some(node)
    }
}

/// Where everything landed, worked out once by the renderer and kept so the
/// mouse can be matched to a pane exactly rather than by computing the layout
/// a second time.
#[derive(Default, Clone, Debug)]
pub struct Areas {
    pub panes: Vec<(Slot, Rect)>,
    pub dividers: Vec<Divider>,
}

impl Areas {
    pub fn of(&self, slot: Slot) -> Option<Rect> {
        self.panes
            .iter()
            .find(|(s, _)| *s == slot)
            .map(|(_, rect)| *rect)
    }

    /// The pane the mouse is over.
    pub fn at(&self, x: u16, y: u16) -> Option<Slot> {
        self.panes
            .iter()
            .find(|(_, rect)| x >= rect.x && x < rect.right() && y >= rect.y && y < rect.bottom())
            .map(|(slot, _)| *slot)
    }
}

/// A border between two panes, and what dragging it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divider {
    /// Which split this is the border of.
    pub path: Vec<u8>,
    pub dir: Dir,
    /// The two cells the mouse has to hit: the panes each draw their own edge,
    /// and they sit against each other.
    pub rect: Rect,
    /// The space that split was dividing, which is what a drag is measured
    /// against.
    pub area: Rect,
}

fn collect(node: &Node, out: &mut Vec<Slot>) {
    match node {
        Node::Leaf(slot) => out.push(*slot),
        Node::Split { first, second, .. } => {
            collect(first, out);
            collect(second, out);
        }
    }
}

fn first_leaf(node: &Node) -> Slot {
    nearest_leaf(node, false)
}

/// The leaf at one end of an arrangement: the last one when `from_end`.
fn nearest_leaf(node: &Node, from_end: bool) -> Slot {
    match node {
        Node::Leaf(slot) => *slot,
        Node::Split { first, second, .. } => {
            nearest_leaf(if from_end { second } else { first }, from_end)
        }
    }
}

/// Drop the panes `keep` rejects, closing the splits they leave behind.
fn prune(node: &Node, keep: &mut impl FnMut(Slot) -> bool) -> Option<Node> {
    match node {
        Node::Leaf(slot) => keep(*slot).then(|| node.clone()),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => match (prune(first, keep), prune(second, keep)) {
            (Some(first), Some(second)) => Some(Node::Split {
                dir: *dir,
                ratio: *ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            // One side left: it takes the whole space the pair had.
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
    }
}

fn walk(node: &Node, area: Rect, path: &mut Vec<u8>, out: &mut Areas) {
    match node {
        Node::Leaf(slot) => out.panes.push((*slot, area)),
        Node::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let (a, b, rect) = cut(area, *dir, *ratio);
            out.dividers.push(Divider {
                path: path.clone(),
                dir: *dir,
                rect,
                area,
            });
            path.push(0);
            walk(first, a, path, out);
            path.pop();
            path.push(1);
            walk(second, b, path, out);
            path.pop();
        }
    }
}

/// Divide a rectangle, and say where the border between the halves fell.
fn cut(area: Rect, dir: Dir, ratio: u16) -> (Rect, Rect, Rect) {
    match dir {
        Dir::Across => {
            let width = share(area.width, ratio, MIN_W);
            let first = Rect { width, ..area };
            let second = Rect {
                x: area.x + width,
                width: area.width - width,
                ..area
            };
            let border = Rect {
                x: area.x + width.saturating_sub(1),
                width: 2.min(area.width),
                ..area
            };
            (first, second, border)
        }
        Dir::Down => {
            let height = share(area.height, ratio, MIN_H);
            let first = Rect { height, ..area };
            let second = Rect {
                y: area.y + height,
                height: area.height - height,
                ..area
            };
            let border = Rect {
                y: area.y + height.saturating_sub(1),
                height: 2.min(area.height),
                ..area
            };
            (first, second, border)
        }
    }
}

/// How much of `total` the first of two panes gets: its share, but never so
/// little that either of them stops being a pane. Squeezed past what two panes
/// need at all, they simply halve what there is.
fn share(total: u16, ratio: u16, min: u16) -> u16 {
    if total < min * 2 {
        return total / 2;
    }
    let want = (total as u32 * ratio as u32 / 100) as u16;
    want.clamp(min, total - min)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANES: Rect = Rect {
        x: 0,
        y: 2,
        width: 100,
        height: 30,
    };

    fn local() -> Slot {
        Slot::files(Side::Local)
    }

    fn remote() -> Slot {
        Slot::files(Side::Remote)
    }

    #[test]
    fn the_arrangement_sshman_opens_with_is_two_panes_side_by_side() {
        let l = Layout::default();
        assert_eq!(l.slots(), [local(), remote()]);
        let areas = l.areas(PANES);
        assert_eq!(areas.of(local()).unwrap().width, 50);
        assert_eq!(areas.of(remote()).unwrap().width, 50);
        assert_eq!(areas.dividers.len(), 1);
    }

    #[test]
    fn splitting_a_pane_puts_the_new_one_beside_it() {
        let mut l = Layout::default();
        let term = Slot::term(Side::Remote, 1);
        assert!(l.split(remote(), Dir::Down, term, 70));
        assert_eq!(l.slots(), [local(), remote(), term]);

        let areas = l.areas(PANES);
        let files = areas.of(remote()).unwrap();
        let shell = areas.of(term).unwrap();
        assert_eq!(files.x, shell.x, "the new pane took the same column");
        assert_eq!(files.height + shell.height, PANES.height);
        assert!(
            files.height > shell.height,
            "70% of it stayed with the files"
        );
        assert_eq!(shell.y, files.bottom(), "and it sits underneath");
    }

    #[test]
    fn splitting_a_pane_that_is_not_there_changes_nothing() {
        let mut l = Layout::default();
        assert!(!l.split(Slot::term(Side::Local, 9), Dir::Down, remote(), 50));
        assert_eq!(l, Layout::default());
    }

    #[test]
    fn closing_a_pane_gives_its_space_to_the_one_beside_it() {
        let mut l = Layout::default();
        let term = Slot::term(Side::Remote, 1);
        l.split(remote(), Dir::Down, term, 70);
        assert!(l.remove(term));
        assert_eq!(l.slots(), [local(), remote()]);
        assert_eq!(
            l.areas(PANES).of(remote()).unwrap().height,
            PANES.height,
            "the files took the rows back"
        );
    }

    #[test]
    fn the_last_pane_cannot_be_closed() {
        let mut l = Layout::only(remote());
        assert!(!l.remove(remote()));
        assert_eq!(l.slots(), [remote()]);
    }

    #[test]
    fn panes_that_have_gone_leave_the_arrangement_behind_them() {
        // A terminal shut, or a tab closed with its terminals.
        let mut l = Layout::default();
        let one = Slot::term(Side::Remote, 1);
        let two = Slot::term(Side::Remote, 2);
        l.split(remote(), Dir::Down, one, 60);
        l.split(one, Dir::Across, two, 50);
        assert_eq!(l.panes(), 4);

        l.retain(|s| !s.is_term());
        assert_eq!(l.slots(), [local(), remote()]);
    }

    #[test]
    fn an_arrangement_emptied_of_everything_falls_back_rather_than_vanishing() {
        let mut l = Layout::default();
        l.retain(|_| false);
        assert_eq!(l, Layout::default());
    }

    #[test]
    fn every_pane_keeps_enough_room_to_be_one() {
        let mut l = Layout::default();
        let term = Slot::term(Side::Remote, 1);
        l.split(remote(), Dir::Down, term, 99);

        let tall = l.areas(PANES);
        assert!(tall.of(term).unwrap().height >= MIN_H, "{tall:?}");

        // Even in a window too small for two panes, neither is left at zero.
        let tiny = Rect {
            width: 10,
            height: 4,
            ..PANES
        };
        for (_, rect) in l.areas(tiny).panes {
            assert!(rect.width > 0 && rect.height > 0, "{rect:?}");
        }
    }

    #[test]
    fn the_divider_stops_before_either_side_disappears() {
        let mut l = Layout::default();
        for _ in 0..100 {
            l.resize_near(local(), Dir::Across, 5);
        }
        assert_eq!(l.areas(PANES).of(remote()).unwrap().width, 10);
        for _ in 0..100 {
            l.resize_near(local(), Dir::Across, -5);
        }
        assert_eq!(l.areas(PANES).of(local()).unwrap().width, 10);
    }

    #[test]
    fn resizing_moves_the_nearest_divider_running_that_way() {
        let mut l = Layout::default();
        let term = Slot::term(Side::Remote, 1);
        l.split(remote(), Dir::Down, term, 50);

        // From inside the shell, up and down is its own border with the file
        // list above it, not the one between the two machines.
        assert!(l.resize_near(term, Dir::Down, -20));
        let areas = l.areas(PANES);
        assert_eq!(
            areas.of(local()).unwrap().width,
            50,
            "the columns did not move"
        );
        assert!(areas.of(term).unwrap().height > areas.of(remote()).unwrap().height);

        // And sideways from there is the one between the machines, which is
        // the only split running that way.
        assert!(l.resize_near(term, Dir::Across, -20));
        assert_eq!(l.areas(PANES).of(local()).unwrap().width, 30);
    }

    #[test]
    fn there_is_nothing_to_resize_in_a_single_pane() {
        let mut l = Layout::only(remote());
        assert!(!l.resize_near(remote(), Dir::Across, 5));
        assert!(!l.resize_near(remote(), Dir::Down, 5));
    }

    #[test]
    fn dragging_puts_the_divider_where_the_mouse_is() {
        let mut l = Layout::default();
        let divider = l.areas(PANES).dividers[0].clone();
        l.drag(&divider.path, divider.dir, divider.area, 70, 10);
        assert_eq!(l.areas(PANES).of(local()).unwrap().width, 70);

        // And stops at the same limits as everything else.
        l.drag(&divider.path, divider.dir, divider.area, 0, 10);
        assert_eq!(l.areas(PANES).of(local()).unwrap().width, 10);
    }

    #[test]
    fn a_drag_in_a_pane_with_no_width_is_ignored() {
        let mut l = Layout::default();
        l.drag(&[], Dir::Across, Rect::ZERO, 40, 40);
        assert_eq!(l, Layout::default());
    }

    #[test]
    fn the_divider_a_drag_grabs_is_the_one_it_is_on() {
        // Two splits, so the paths have to tell them apart.
        let mut l = Layout::default();
        let term = Slot::term(Side::Local, 1);
        l.split(local(), Dir::Down, term, 50);
        let areas = l.areas(PANES);
        assert_eq!(areas.dividers.len(), 2);

        let down = areas
            .dividers
            .iter()
            .find(|d| d.dir == Dir::Down)
            .expect("the one under the file list");
        l.drag(&down.path, down.dir, down.area, 0, 8);
        let after = l.areas(PANES);
        assert_eq!(after.of(local()).unwrap().height, 6);
        assert_eq!(after.of(remote()).unwrap().width, 50, "and only that one");
    }

    #[test]
    fn evening_up_keeps_the_shape_and_only_moves_the_borders() {
        let mut l = Layout::default();
        let term = Slot::term(Side::Remote, 1);
        l.split(remote(), Dir::Down, term, 80);
        l.resize_near(local(), Dir::Across, 20);

        l.even();
        assert_eq!(l.slots(), [local(), remote(), term], "nothing closed");
        let areas = l.areas(PANES);
        assert_eq!(areas.of(local()).unwrap().width, 50);
        assert_eq!(areas.of(term).unwrap().height, 15);
    }

    #[test]
    fn the_keyboard_moves_to_whatever_is_across_the_border() {
        let mut l = Layout::default();
        let term = Slot::term(Side::Remote, 1);
        l.split(remote(), Dir::Down, term, 60);

        assert_eq!(l.neighbour(local(), Dir::Across, true), Some(remote()));
        assert_eq!(l.neighbour(remote(), Dir::Across, false), Some(local()));
        assert_eq!(l.neighbour(remote(), Dir::Down, true), Some(term));
        assert_eq!(l.neighbour(term, Dir::Down, false), Some(remote()));

        // Nothing that way is nothing, rather than wrapping round to the far
        // side of the screen.
        assert_eq!(l.neighbour(local(), Dir::Across, false), None);
        assert_eq!(l.neighbour(local(), Dir::Down, true), None);
        assert_eq!(l.neighbour(term, Dir::Down, true), None);

        // Crossing the columns from the shell reaches the pane beside it.
        assert_eq!(l.neighbour(term, Dir::Across, false), Some(local()));
    }

    #[test]
    fn an_arrangement_from_a_file_is_not_taken_on_trust() {
        let term = Slot::term(Side::Remote, 1);
        let wild = Layout {
            root: Node::Split {
                dir: Dir::Across,
                ratio: 5000,
                first: Box::new(Node::Leaf(local())),
                second: Box::new(Node::Split {
                    dir: Dir::Down,
                    ratio: 0,
                    // The same pane twice: each would act on the other's keys.
                    first: Box::new(Node::Leaf(term)),
                    second: Box::new(Node::Leaf(term)),
                }),
            },
        }
        .sane();
        assert_eq!(wild.slots(), [local(), term]);
        let areas = wild.areas(PANES);
        assert_eq!(areas.of(local()).unwrap().width, 90);
    }

    #[test]
    fn arrangements_round_trip_through_json() {
        let mut l = Layout::default();
        l.split(remote(), Dir::Down, Slot::term(Side::Remote, 3), 70);
        let text = serde_json::to_string(&l).unwrap();
        assert_eq!(serde_json::from_str::<Layout>(&text).unwrap(), l);
    }

    #[test]
    fn sizes_written_down_before_there_were_arrangements_still_open() {
        // What a workspace saved when a layout was a percentage and a height.
        // Shells were never reopened by one, so the percentage is the whole of
        // what it still has to say.
        let old = r#"{"split_pct": 70, "shell_height": 20}"#;
        let l: Layout = serde_json::from_str(old).unwrap();
        assert_eq!(l.slots(), [local(), remote()]);
        assert_eq!(l.areas(PANES).of(local()).unwrap().width, 70);

        // Including one whose percentage nothing would draw.
        let wild: Layout = serde_json::from_str(r#"{"split_pct": 5000}"#).unwrap();
        assert_eq!(wild.areas(PANES).of(local()).unwrap().width, 90);
    }
}
