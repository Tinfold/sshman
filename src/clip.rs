//! Getting a copy out of sshman and onto the system clipboard.
//!
//! Two ways at once, because neither one works everywhere:
//!
//!   * `OSC 52`, which asks the terminal to do it. This is the one that has
//!     to work: a copy made in a pane connected to a server is text that only
//!     exists on the far end of an `ssh` connection, and no amount of talking
//!     to the local display server will fetch it. Not every terminal
//!     implements it — every terminal built on VTE, which is `xfce4-terminal`
//!     and `gnome-terminal` and most of what ships with a Linux desktop, does
//!     not — and several that do have it off by default, because a program
//!     that can silently rewrite your clipboard can rewrite a command you are
//!     about to paste into a shell.
//!
//!   * The clipboard tool the desktop ships, if there is one and there is a
//!     desktop to talk to. This one always works locally and never works
//!     remotely, which is the exact complement of the other.
//!
//! Doing both means a copy lands by whichever of the two is available. In a
//! VTE terminal that is the desktop tool, which is why copying out of a local
//! pane now works there; a copy out of a *remote* pane in one of those still
//! has only the escape sequence to travel by, and that is the terminal's
//! decision rather than ours.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use base64::Engine;

/// Terminals cap what they will take — tmux's default is around 74k — so more
/// than this is sent as much as fits rather than being dropped whole.
const LIMIT: usize = 64 * 1024;

/// The escape sequence that asks the terminal to take this text, wrapped for
/// whatever is between us and it.
pub fn sequence(text: &str) -> String {
    let end = (0..=LIMIT.min(text.len()))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&text.as_bytes()[..end]);
    wrapped(&format!("\x1b]52;c;{encoded}\x07"))
}

/// Hand the text to the desktop's own clipboard tool, if this machine has one.
///
/// No size limit: the tool reads its standard input and has no escape
/// sequence to be truncated by, so a copy too large for the terminal still
/// lands here.
pub fn to_desktop(text: &str) {
    if let Some(helper) = helper() {
        helper.write(text);
    }
}

/// What is between sshman and the terminal that owns the clipboard.
///
/// A terminal multiplexer reads the escape sequences going past and acts on
/// the ones it understands, which for `OSC 52` means eating it: the copy
/// reaches tmux and stops there. Both of them have a way of saying "this one
/// is not for you", and it is the only way a copy made inside sshman inside
/// tmux reaches the machine in front of you.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Through {
    Tmux,
    Screen,
}

fn through() -> Option<Through> {
    // `TMUX` first: inside tmux `TERM` is usually `screen` or `tmux`, and the
    // wrapping the two want is not the same.
    if set("TMUX") {
        return Some(Through::Tmux);
    }
    let term = std::env::var("TERM").unwrap_or_default();
    (term.starts_with("screen") && !term.starts_with("screen.")).then_some(Through::Screen)
}

fn wrapped(sequence: &str) -> String {
    wrap_as(through(), sequence)
}

fn wrap_as(through: Option<Through>, sequence: &str) -> String {
    match through {
        None => sequence.to_string(),
        // tmux takes the whole thing inside one of its own, with every escape
        // in the payload doubled so that the inner sequence's terminator is
        // not read as the outer one's.
        Some(Through::Tmux) => {
            format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"))
        }
        // screen passes a device-control string through untouched, but will
        // not carry one longer than its own string buffer — so it goes in
        // pieces, each one a device-control string of its own. Nothing is
        // written between them, so the terminal sees one unbroken sequence.
        Some(Through::Screen) => {
            let mut out = String::new();
            for chunk in sequence.as_bytes().chunks(400) {
                out.push_str("\x1bP");
                out.push_str(&String::from_utf8_lossy(chunk));
                out.push_str("\x1b\\");
            }
            out
        }
    }
}

/// One of the small programs a desktop ships for this, and how to run it.
#[derive(Clone, Copy)]
struct Helper {
    command: &'static str,
    args: &'static [&'static str],
}

impl Helper {
    /// Hand it the text on its standard input.
    ///
    /// The child is waited for on a thread of its own. Some of these —
    /// `wl-copy` most of all — stay alive holding the selection until
    /// somebody else takes it, so waiting here would stop sshman until the
    /// next time anything was copied in any program. Not waiting at all would
    /// leave a zombie behind every copy.
    fn write(&self, text: &str) {
        let child = Command::new(self.command)
            .args(self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = child else { return };
        if let Some(stdin) = child.stdin.take() {
            let mut stdin = stdin;
            stdin.write_all(text.as_bytes()).ok();
            // Dropped, and so closed, before the wait: a tool reading to end
            // of input never sees one otherwise.
            drop(stdin);
        }
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                child.wait().ok();
            })
            .ok();
    }
}

fn have(command: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

fn set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// The tool to use, worked out once.
///
/// Once, because this is a `PATH` walk and a handful of environment lookups,
/// and because the answer cannot change while sshman is running: a display
/// server does not appear halfway through a session.
fn helper() -> Option<Helper> {
    static FOUND: OnceLock<Option<Helper>> = OnceLock::new();
    *FOUND.get_or_init(|| {
        // Wayland first, then X11, because a Wayland session usually has an
        // X11 compatibility layer as well and the native one is the one that
        // works. Neither is worth trying without a display to talk to:
        // `xclip` with no `DISPLAY` blocks rather than failing.
        if set("WAYLAND_DISPLAY") && have("wl-copy") {
            return Some(Helper { command: "wl-copy", args: &[] });
        }
        if set("DISPLAY") && have("xclip") {
            return Some(Helper {
                command: "xclip",
                args: &["-selection", "clipboard"],
            });
        }
        if set("DISPLAY") && have("xsel") {
            return Some(Helper {
                command: "xsel",
                args: &["--clipboard", "--input"],
            });
        }
        if have("pbcopy") {
            return Some(Helper { command: "pbcopy", args: &[] });
        }
        // Windows Subsystem for Linux, where the Windows clipboard is the one
        // that matters.
        if have("clip.exe") {
            return Some(Helper { command: "clip.exe", args: &[] });
        }
        if have("termux-clipboard-set") {
            return Some(Helper {
                command: "termux-clipboard-set",
                args: &[],
            });
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_wrapped_for_whatever_is_in_the_way() {
        let plain = "\x1b]52;c;aGk=\x07";

        // Nothing in the way: the sequence itself, untouched.
        assert_eq!(wrap_as(None, plain), plain);

        // tmux eats an `OSC 52` it is not told to pass on, so the copy
        // reaches tmux and stops there — which is what made copying out of
        // sshman inside tmux do nothing at all.
        let through_tmux = wrap_as(Some(Through::Tmux), plain);
        assert!(through_tmux.starts_with("\x1bPtmux;"));
        assert!(through_tmux.ends_with("\x1b\\"));
        assert!(
            through_tmux.contains("\x1b\x1b]52;"),
            "the payload's escapes have to be doubled or its terminator ends \
             the wrapper: {through_tmux:?}"
        );

        // screen carries a device-control string but not an arbitrarily long
        // one, so a large copy goes in pieces with nothing written between.
        let long = format!("\x1b]52;c;{}\x07", "a".repeat(1000));
        let through_screen = wrap_as(Some(Through::Screen), &long);
        assert!(through_screen.starts_with("\x1bP"));
        assert_eq!(
            through_screen.matches("\x1bP").count(),
            3,
            "a thousand bytes is three pieces of four hundred"
        );
        // And putting the pieces back gives exactly what went in.
        let rebuilt: String = through_screen
            .split("\x1b\\")
            .filter_map(|piece| piece.strip_prefix("\x1bP"))
            .collect();
        assert_eq!(rebuilt, long);
    }

    #[test]
    fn a_copy_too_large_for_the_terminal_is_cut_at_a_character() {
        // Terminals stop reading these somewhere past 64k. Cutting in the
        // middle of a character would put invalid UTF-8 on the clipboard, so
        // the cut is moved back to a boundary.
        let text = "é".repeat(LIMIT);
        let sequence = sequence(&text);
        let payload = sequence
            .trim_start_matches("\x1b]52;c;")
            .trim_end_matches('\x07');
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("what we sent is what we encoded");
        let text = String::from_utf8(decoded).expect("cut in the middle of a character");
        assert!(text.chars().all(|c| c == 'é'));
        assert!(!text.is_empty());
    }
}
