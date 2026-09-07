# sshman

A two-pane SSH file manager for the terminal. Your machine on the left, the
server on the right. Copy files either way with one key, open them in your own
editor, and switch the remote side to sudo when you need to see root-only
paths.

Split any pane into a real shell, local or remote, as many as you like. Open
several servers at once in tabs. Docker and Podman containers work as targets
too, and behave exactly like a host. Everything that was open comes back next
time, whether or not you saved it.

One self-contained binary. No runtime dependencies, and nothing to install on
the server.

```
 sshman  deploy@web01:22   SUDO   ● downloading 2 item(s)…
┌ LOCAL /home/me/work ─────────────────────┐┌ REMOTE /etc/nginx ───────────────────────┐
│ drwxr-xr-x  <DIR> 2026-08-19 10:02 certs/││ drwxr-xr-x  <DIR> 2026-08-11 09:31 sites/│
│*-rw-r--r--  2.1K  2026-08-19 09:58 app.co││ -rw-r--r--  1.5K  2026-08-18 22:14 nginx.│
│ -rw-------  3.0K  2026-08-12 18:44 id_ed2││ lrwxrwxrwx  31B   2026-08-01 12:00 mime.t│
└──────────────────────── 1 marked 3 items ┘└───────────────────────────── 12 items ───┘
┌ SHELL local ─────────────────────────────┐┏ SHELL deploy@web01 ━━━━━━━━━━━━━━━━━━━━━━┓
│ $ git status --short                     │┃ deploy@web01:/etc/nginx$ nginx -t        ┃
│  M app.conf                              │┃ syntax is ok, test is successful         ┃
│ $ █                                      │┃ deploy@web01:/etc/nginx$ █               ┃
└──────────────────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
 ↑ app.conf  1.4M / 2.1M  (67%)
✓ 1 item(s) copied to remote
 Ctrl-]  sshman keys   drag  select   every other key goes to the shell
```

## Install

```sh
cargo build --release
# -> target/release/sshman
```

By default, that links against your system OpenSSL. For a binary you
can copy to another machine, link OpenSSL statically:

```sh
cargo build --release --features vendored
```

## Usage

```sh
sshman                          # connection form, offers to reopen last session
sshman web01                    # a ~/.ssh/config alias, resolved like ssh does
sshman deploy@10.0.0.5 -p 2222
sshman web01 -i ~/.ssh/deploy_key --remote-path /etc/nginx
sshman web01 -W                 # show the form so you can type a password
sshman --local                  # one tab on this machine, no server
sshman --docker                 # pick a container on this machine
sshman --resume                 # reopen everything from last time
sshman -w morning               # open a saved workspace
```

Auth is tried in the usual order: an explicit `-i` key, then `ssh-agent`, then
`~/.ssh/id_ed25519` / `id_ecdsa` / `id_rsa`, then a password if you gave one.

Host keys are checked against `~/.ssh/known_hosts`. An unknown host shows its
SHA256 fingerprint and asks. A changed host key stops everything, shows you what
changed, and makes you type the word `replace` rather than accepting it with a
single keystroke.

## Keys

The essentials:

| | |
|---|---|
| `Tab` | next file list |
| `↑ ↓` / `k j`, `g` `G` | move |
| `Enter` / `→`, `←` / `h` | enter a directory, go up |
| `/` `.` `~` `f` | filter, dotfiles, home, jump to a path |
| `Space` / `a` | mark one / mark all |
| `c` / `F5` | copy to the other side |
| `M` `P` | cut and paste within one machine |
| `e` / `F4`, `v` | edit in `$EDITOR`, view in `$PAGER` |
| `n` `r` `d` | mkdir, rename, delete |
| `z` `x` `X` | pack, unpack, list an archive |
| `s` | sudo mode |
| `:` `!` | run a command / full-screen shell in this directory |
| `S` `\|` `_` `T` | split: shell below, shell beside, shell down, another file list |
| `i` | editor pane beside this one |
| `m` / `F3`, `F9`, `=` | zoom, close pane, even the borders up |
| `A` | pick a ready-made arrangement for this tab |
| `Alt-↑↓←→` | move between panes |
| `C` `W`, `Ctrl-←/→` | new tab, close tab, switch tabs |
| `D` `L` | open a container / a local-only tab |
| `p` `w` `N` | port forwards, workspaces, name this server |
| `,` `?` `q` | settings, help, quit |
| `Ctrl-]` | command mode, from inside a shell |

Press `?` for the full list. Every one of these can be rebound (`,` then
**Keys**), and the hint bar follows your bindings rather than the shipped ones.

The mouse works too: click to focus, double-click to open, right-click for a
menu of what can be done here, drag borders to resize, drag tab chips to
reorder, drag across a shell pane to select and copy. Click any piece of the
path on a pane's top border to jump back to it.

### Command mode

A focused shell takes every key, which is what makes it a real terminal. So the
rest of sshman sits behind one chord: **`Ctrl-]` hands the keyboard back**.
Every key then does what it does with a file list focused, plus four that only
make sense there:

| | |
|---|---|
| `↑↓←→` | move to that pane without entering it |
| `↵` | enter the pane you moved to |
| `Shift-↑↓←→` | move the nearest border |
| `g` | pick the pane up and move the pane instead |

`Esc` or `Ctrl-]` again puts the keyboard back where it was.

## What it does

**Shells in panes.** `S` opens a real shell under the focused pane, local or on
the server depending on which pane you split. `vim`, `top`, `btop`, colours and
Ctrl-C all work, because each one is a pty with a terminal emulator behind it.
`$` gives a pane a command to run instead of a prompt, so a pane can be a log
tail or a build watcher, and it comes back running that.

**Tabs.** Each tab is its own connection with its own directories, panes, marks
and sudo state. A big copy in one does not hold up another. `Ctrl-Shift-←/→` or
dragging a chip reorders them.

**Sudo mode.** `s` asks for your sudo password and verifies it, then the remote
pane lists, copies, edits and deletes as root. This is the part plain SFTP
clients cannot do, since the SFTP subsystem runs as your login user. Transfers
are staged through a temp directory your login user owns. The password stays in
memory for the session and is never written anywhere.

**Containers.** `D` opens a Docker or Podman container in a new tab, either on
this machine or on the server whose pane you are in. It browses, copies, edits
and runs commands exactly like a host. The runtime is detected on whichever
machine holds the containers, so a podman-only box needs no configuration.

**Port forwarding.** `p` lists forwards, `a` adds one. Shorthand is what you
would expect: `3000`, `8080:3000`, `8080:db:5432`, or with an address in front
to bind past loopback. They bind `127.0.0.1` unless you ask otherwise, and they
are saved with the workspace.

**Workspaces and session restore.** `w` then `s` saves everything open under a
name, including where each pane was pointed, how they were arranged, and what
each shell was running. sshman also writes the current session down as you go,
so starting it offers you the last one back even if you never saved anything.
Passwords are never saved.

**Live file lists.** Panes follow the directory they show, so files that appear,
vanish or get renamed show up on their own. Local uses the directory mtime,
remote polls the visible tab and stays silent when nothing changed. `,` then
**Keeping up** turns it off if you would rather it held still.

**Reconnects.** A dropped link is noticed within about 20 seconds and sshman
reconnects on its own, up to six tries. You come back in the same directory,
with sudo mode restored if it was on.

**Themes.** 44 built in, dark and light, plus the `terminal` theme that just
uses your own 16 colours. `,` then **Theme** previews each one across the whole
screen as you move through the list. Themes are JSON files, so dropping one in
`~/.config/sshman/themes/` adds it, and giving it an existing name replaces
that one.

**Icons.** Off by default, since a missing glyph is a box and a box in front of
every name is worse than nothing. `,` then **Icons** turns on either a Nerd Font
set or an emoji set.

## Configuration

Everything lives in `~/.config/sshman/` (or `$XDG_CONFIG_HOME/sshman/`).
Press `,` for the settings pane, which writes the file for you. All of it is
optional:

```json
// ~/.config/sshman/config.json
{
  "editor": "hx",
  "shell": "fish",
  "theme": "gruvbox",
  "icons": "nerd",
  "watch": "off",
  "keys": {
    "quit": ["Q"],
    "zoom": ["z", "F3"]
  }
}
```

`editor` wins over `$VISUAL` and `$EDITOR`. `shell` is the interactive shell a
pane starts, checked with `command -v` on a server before it is used, so naming
one a box does not have leaves you in the login shell rather than in nothing.

Servers you connect to are saved in `hosts.json`, most recent first, so next
time you pick one from a list. Only the user, host, port and key path are
stored, in a `0600` file. Passwords never touch the disk.

### Editing files

`e` opens the file under the cursor in your editor. A local file opens in place.
A remote or container file is downloaded to a temp path, edited, and uploaded
when your editor exits, skipping the upload if you changed nothing. Use a
blocking editor: `code` and `subl` need `-w`.

`i` instead gives the editor a pane of its own, on the machine whose file list
you are in, and clicking a file opens it there. Arrange the remote pane that way
and your editor runs on the server, editing the file where it lives.

sshman knows the keystrokes for vim, neovim, helix, kakoune, emacs and textfold.
For anything else it runs your editor as a command at the prompt, or you can
spell out the keys yourself with `editor_open`.

## Tests

```sh
cargo test                      # ~390 unit tests, no network needed
./testserver/run-live-tests.sh  # live tests against a throwaway container
```

The live script builds a Debian container with sshd, a sudo-capable user and a
few root-only files, then runs the tests that need a real server: connecting,
listing, recursive transfers both ways, the sudo path, and host key acceptance
and mismatch detection.

## Notes

- All network work happens on a background thread, so the interface stays
  responsive during large transfers. Shells and port forwards run on their own
  connections, so a busy shell never stalls a copy.
- Remote paths are POSIX strings throughout, and every path reaching a shell is
  single-quoted. A file called `; rm -rf /` is just a file.
- `!` starts a separate `ssh` process and so authenticates on its own. The `S`
  shell reuses your credentials instead.
