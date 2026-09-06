# CLAUDE.md

Guidance for working in this repository.

## What this is

`sshman` is a terminal two-pane SSH file manager written in Rust (edition 2024).
Local machine on the left, remote on the right. It also does embedded shells,
tabs, port forwarding, containers, sudo mode, themes and session restore.

Single binary, no runtime dependencies, nothing installed on the server.

## Build and test

```sh
cargo build --release              # -> target/release/sshman
cargo build --release --features vendored   # static OpenSSL, portable binary
cargo test                         # ~390 unit tests, no network needed
./testserver/run-live-tests.sh     # live tests against a throwaway container
```

`vendored` only affects OpenSSL; libssh2 is always built from source by
`libssh2-sys`. On macOS the default build links system/Homebrew OpenSSL.

The live test script builds a Debian container with sshd, a sudo-capable user
and root-only files, then runs the `#[ignore]`d tests with `HOME` redirected at
a scratch dir (so accepting the test host key does not touch your real
`known_hosts`). It needs `docker` and `python3`.

## Architecture

Three threads and one rule: **the UI thread never blocks on the network.**

- **UI thread** owns all state (`app.rs`), draws (`ui.rs`), sends `Req` to the
  worker and drains `Resp`. Local filesystem work runs inline here because it
  is fast enough to be imperceptible.
- **Worker thread** (`worker.rs`) does every network operation: listings,
  transfers, commands, reconnects.
- **Shell threads** (`shell.rs`), one per pane, each with its own pty or SSH
  channel. Port forwards (`forward.rs`) likewise get their own SSH connection.

Separate connections are deliberate. Transfers are driven by blocking calls, so
sharing a connection with a shell that must be read continuously would let a
busy shell stall listings and copies.

### Module map

| File | Holds |
|---|---|
| `main.rs` | CLI (clap), startup, event loop |
| `app.rs` | Application state and key handling. The big one. |
| `ui.rs` | All drawing. Mutates nothing but ratatui scroll offsets. |
| `backend.rs` | The trait everything above is written against: SSH server, container, or this machine |
| `sshconn.rs` | SSH/SFTP layer, sudo listing and staged sudo transfers |
| `local.rs` | This machine, as a filesystem and as a `Backend` |
| `docker.rs` | Containers as targets, Docker and Podman |
| `worker.rs` | Background request/response loop |
| `shell.rs` | Embedded ptys, vt100 parsing, kitty keyboard protocol |
| `layout.rs` | The pane tree belonging to each tab |
| `keys.rs` | `Action` enum, `Keymap`, the default scheme |
| `theme.rs` | Twelve colour roles, theme loading and fallback |
| `icons.rs` | The file-type table for both the Nerd Font and emoji sets |
| `config.rs` | `~/.config/sshman/config.json` |
| `history.rs` | Remembered servers |
| `workspace.rs` | Saved sets of connections, and the previous session |
| `forward.rs` | Port forwarding |
| `watch.rs` | Noticing directory changes without being asked |
| `archive.rs` | `tar` command-line building |
| `fileops.rs` | Same-side copy/move command building |
| `clip.rs` | OSC 52 clipboard |
| `sshcfg.rs` | Minimal `~/.ssh/config` reader |
| `types.rs` | `DirEntry` and formatting helpers |
| `input.rs` | Single-line text field |

## Invariants

These are load-bearing. Breaking one is how bugs get shipped.

**Shell commands are POSIX `sh` strings, not argument lists.** The same string
runs locally through `sh -c` and remotely through an SSH channel, so one
builder serves both sides. See `archive.rs` and `fileops.rs`.

**sshman's own work always goes through `/bin/sh`**, regardless of `$SHELL` or
the `shell` setting. Those settings are for the *interactive* shell in a pane
only. This rule exists because someone's `$SHELL` was fish, which has no
`for … do … done`, and copying two files failed with a complaint from a shell
nobody had asked sshman to use. The exception is a line the user typed: `:` and
`$` run in the user's shell, because the user wrote it.

**Every path reaching a shell is single-quoted.** A file called `; rm -rf /` is
just a file. There are tests for this.

**Sudo does not use SFTP.** The SFTP subsystem runs as the login user, so
root-only paths are invisible to it. Sudo mode shells out (`sudo -S`) to list
via `find`, falling back to `ls -la`, and stages transfers through a temp
directory the login user owns. File data is never mixed into the `sudo -S`
stdin stream, because `sudo` reads its password with a buffered read that can
swallow whatever follows.

Writing back under sudo copies contents only, so an existing file keeps its
mode, owner and group. New files land owned by root.

**The sudo password lives in memory for the session only.** Never on disk,
never on a command line. Same for SSH passwords: `history.json` stores user,
host, port and key path, nothing else, and is written `0600`.

**Every colour on screen is one of twelve theme roles**: accent, dim, text,
muted, good, warn, bad, dir, link, exec, info, on_accent. Two tests enforce it.
One walks every screen in every theme and fails if any cell is painted a colour
the theme did not choose. The other checks text on a coloured chip is readable
against it, that being the only pairing sshman composes itself. Do not
hard-code a colour.

**Icons come from one table, one row at a time**, so a type added to the Nerd
Font set is added to the emoji set in the same breath. A test measures every
glyph's width, since one wide icon among narrow ones leaves names ragged.
Resolution order is: what a thing *is* (dir, symlink) before what it is called,
then whole filename, then longest suffix.

**Containers are addressed by id while running, saved by name.** An id is not
worth remembering next week; renaming one mid-session changes nothing.

## Things worth knowing before changing them

**The pane tree.** A tab's layout is a tree: each node is either a leaf pane (a
file list, a terminal, an editor) or a split of two arrangements with a
percentage. The opening two-pane view is not a special case, just
`Split{across, 50%, Files(local), Files(remote)}`. Splitting, closing,
dragging a border and zooming are therefore one set of operations rather than
one per shape.

The arrangement and the zoom belong to the tab. Local file lists and terminals
are shared across tabs (there is only one local machine); each tab's
arrangement decides which it shows.

**Where a shell is.** A pty carries characters, not state, so a shell's cwd
cannot be read off. Three sources, in order of trust: `/proc` (local only,
always right), `OSC 7` (right anywhere, but only if the prompt sends it), the
window title matched strictly against `user@host: dir`. Failing all three, the
directory it started in.

**Directory watching.** Local is a `stat` on the directory mtime a couple of
times a second, plus a full re-read of short lists every few seconds so a
growing file shows its size. Lists over a couple of thousand entries skip the
second part. Remote is a poll of the visible tab only; the worker hashes the
listing and stays silent when it matches, so an unchanged directory costs a
message and no redraw. A poll never reports an error and never empties a pane.
Cursor position and marks survive a refresh the user did not ask for.

**Kitty keyboard protocol.** sshman asks its own terminal for unambiguous keys
at startup and takes no for an answer. To programs in a pane it then acts as a
terminal supporting exactly one flag, *disambiguate escape codes*, which is
what makes `Shift-↵` (`CSI 13;2u`) a distinct key. It does not claim key
releases, because its own terminal is not reporting them and promising events
that can never arrive is worse than saying no.

**Rebindable vs not.** The browsing layer is rebindable; the modal set is not
(pane arrows under `Ctrl-]`, `↵` to enter a pane, `Esc` to back out, `Alt-1`..`9`).
Those are how you get *around* sshman, and a rebound one is a way to lock
yourself out of a box you just opened. Config stores actions naming their keys,
not the reverse, so an action can have several. Only overrides are written.

**Config files** live in `~/.config/sshman/` (or `$XDG_CONFIG_HOME/sshman/`):
`config.json`, `hosts.json`, `workspaces.json`, `themes/`. Everything in
`config.json` is optional; an absent, unreadable, or future-version file all
mean the same thing, which is fall back to the default. Never fail to start
over a config file.

**Themes are JSON files, not tables in the source.** The 24 in `themes/` are
built into the binary. Files in `~/.config/sshman/themes/` load beside them and
one taking an existing name replaces it. `base` inherits, missing roles fall
back to `terminal`. A theme that paints no background is never asked for ANSI
colours, since it has not taken the screen over.

**Session restore is written as you go**, not on the way out. There is no way
out to hook: a closed terminal window never comes back to sshman. Whatever is
on disk when the process stops is what comes back. Nothing is written while
nothing is open.

## Style

The prose in module docs and in the UI explains *why*, in plain words, and
avoids jargon where an ordinary word does. Match it. Commit subjects in this
repo are descriptive sentences, not conventional-commit prefixes:

```
A list that says what a thing is before you read its name
A server whose login shell is fish is still a server we can talk to
A pane that runs something, and comes back running it
```

Prefer adding a test to `#[cfg(test)]` in the module being changed; that is
where all 390 of them live.
