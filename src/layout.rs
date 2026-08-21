//! How the panes are arranged.
//!
//! One of these belongs to each tab, and one more to the screen you get before
//! anything is connected, so a server you have set up wide stays wide when you
//! come back to it. A workspace writes them down with everything else it
//! remembers about a tab.
//!
//! Only sizes live here. Which pane has the keyboard and whether one of them
//! is zoomed follow you from tab to tab instead of being stored per tab, so
//! they are the app's business rather than this struct's.

use ratatui::layout::Rect;
use serde::{Deserialize, Serialize};

/// How far the divider between the two sides can be pushed either way. A pane
/// narrow enough to show only its border is not a pane, and dragging back out
/// of one would be fiddly.
const MIN_SPLIT: u16 = 20;
const MAX_SPLIT: u16 = 80;

/// The shell can never squeeze the file list out, nor shrink to a border.
const MIN_SHELL: u16 = 3;
const MAX_SHELL: u16 = 60;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Share of the width the local side gets, as a percentage.
    pub split_pct: u16,
    /// Rows given to a shell pane, including its border.
    pub shell_height: u16,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            split_pct: 50,
            shell_height: 12,
        }
    }
}

impl Layout {
    /// Move the divider between the two sides. Positive widens the local side.
    pub fn nudge_split(&mut self, delta: i16) {
        self.split_pct = clamp(self.split_pct as i16 + delta, MIN_SPLIT, MAX_SPLIT);
    }

    /// Grow (positive) or shrink the shell pane.
    pub fn nudge_shell(&mut self, delta: i16) {
        self.shell_height = clamp(self.shell_height as i16 + delta, MIN_SHELL, MAX_SHELL);
    }

    /// Put the divider under the mouse, from a drag across `area`.
    pub fn split_at(&mut self, area: Rect, x: u16) {
        if area.width == 0 {
            return;
        }
        let offset = x.saturating_sub(area.x).min(area.width) as u32;
        let pct = (offset * 100 / area.width as u32) as i16;
        self.split_pct = clamp(pct, MIN_SPLIT, MAX_SPLIT);
    }

    /// Put the top edge of the shell pane under the mouse. The shell sits
    /// against the bottom of `area`, so the row it starts on is all its height
    /// amounts to — capped at what the panes have between them, since the
    /// drawing keeps rows for the file list on top of that.
    pub fn shell_top_at(&mut self, area: Rect, row: u16) {
        let height = area.bottom().saturating_sub(row) as i16;
        let ceiling = area.height.clamp(MIN_SHELL, MAX_SHELL);
        self.shell_height = clamp(height, MIN_SHELL, ceiling);
    }

    /// Sizes read back from a file, which is to say sizes we cannot trust.
    pub fn sane(self) -> Self {
        Self {
            split_pct: self.split_pct.clamp(MIN_SPLIT, MAX_SPLIT),
            shell_height: self.shell_height.clamp(MIN_SHELL, MAX_SHELL),
        }
    }
}

fn clamp(value: i16, low: u16, high: u16) -> u16 {
    value.clamp(low as i16, high as i16) as u16
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

    #[test]
    fn the_divider_stops_before_either_side_disappears() {
        let mut l = Layout::default();
        l.nudge_split(100);
        assert_eq!(l.split_pct, MAX_SPLIT);
        l.nudge_split(-100);
        assert_eq!(l.split_pct, MIN_SPLIT);
    }

    #[test]
    fn dragging_puts_the_divider_where_the_mouse_is() {
        let mut l = Layout::default();
        l.split_at(PANES, 70);
        assert_eq!(l.split_pct, 70);
        l.split_at(PANES, 0);
        assert_eq!(l.split_pct, MIN_SPLIT, "and stops at the same limits");
        l.split_at(PANES, 200);
        assert_eq!(l.split_pct, MAX_SPLIT);
    }

    #[test]
    fn a_divider_drag_in_a_pane_with_no_width_is_ignored() {
        let mut l = Layout::default();
        l.split_at(Rect::ZERO, 40);
        assert_eq!(l.split_pct, 50, "no width means no percentage to compute");
    }

    #[test]
    fn the_shell_edge_follows_the_mouse_within_its_limits() {
        let mut l = Layout::default();
        // The shell runs from the row under the cursor to the bottom.
        l.shell_top_at(PANES, 22);
        assert_eq!(l.shell_height, 10);
        // Dragged past the top it stops at what the panes hold.
        l.shell_top_at(PANES, 0);
        assert_eq!(l.shell_height, PANES.height);
        // And it never shrinks to nothing.
        l.shell_top_at(PANES, 99);
        assert_eq!(l.shell_height, MIN_SHELL);
    }

    #[test]
    fn sizes_from_a_file_are_not_taken_on_trust() {
        let wild = Layout {
            split_pct: 5000,
            shell_height: 0,
        };
        assert_eq!(
            wild.sane(),
            Layout {
                split_pct: MAX_SPLIT,
                shell_height: MIN_SHELL,
            }
        );
    }
}
