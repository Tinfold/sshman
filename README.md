# sshman

A two-pane SSH file manager in the terminal. Your machine on the left, the
server on the right. Copy files either way with one key, open any file in your
own editor, and switch the remote pane to `sudo` when you need to see root-only
paths. Each pane can have a **live shell underneath it** — a real local shell
below the local tree, a real remote shell below the remote one. Open **several
servers at once in tabs**, each with its own directory, shell and sudo state.
Servers you connect to are remembered, so next time you pick one from a list,
and a dropped connection comes back on its own. **Docker containers work as
targets too** — local ones or ones running on a server — and behave exactly
like a host — Docker or Podman.

Single self-contained binary, no runtime dependencies, no agent to install on
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
└─────────────────────────── F6 to focus ──┘┗━━━━━━━━━━━━━━━━━━━ F6 back to files ━━━━━┛
 ↑ app.conf  1.4M / 2.1M  (67%)
✓ 1 item(s) copied to remote
 F6  back to files   Ctrl-]  same   every other key goes to the shell
```

## Build

```sh
cargo build --release
# -> target/release/sshman
```

On macOS that links against your system/Homebrew OpenSSL. For a binary you can
copy to another machine, statically link it:

```sh
cargo build --release --features vendored
```

## Use

```sh
sshman                          # connection form
sshman --local                  # a tab on this machine, no server involved
sshman web01                    # a ~/.ssh/config alias, resolved like ssh does
sshman deploy@10.0.0.5 -p 2222
sshman web01 -i ~/.ssh/deploy_key --remote-path /etc/nginx
sshman web01 -W                 # show the form so you can type a password
```

Authentication is tried in the order you would expect: an explicit `-i` key,
then `ssh-agent`, then `~/.ssh/id_ed25519` / `id_ecdsa` / `id_rsa`, then a
password if you supplied one (over both `password` and `keyboard-interactive`).

Host keys are checked against `~/.ssh/known_hosts`. An unknown host shows you
its SHA256 fingerprint and asks.

A **changed** host key stops everything and shows you what changed. That means
either the server was rebuilt or someone is intercepting the connection, and
nothing visible from here can tell you which — so the dialog says so plainly
and, if you decide to go ahead, makes you type the word `replace` rather than
accept it with a single keystroke. Doing so drops the old key and records the
new one. `Esc` leaves `known_hosts` untouched.

## Keys

| | |
|---|---|
| `Tab` | switch panes |
| `↑ ↓` / `k j`, `PgUp` `PgDn`, `g` `G` | move |
| `Enter` / `→` | enter a directory, or open a file in `$EDITOR` |
| `←` / `h` | parent directory |
| `~` | home directory |
| `f` | type a path to jump to |
| `t` | point the other pane at this directory |
| `/` | filter as you type (`Esc` clears it) |
| `.` | show/hide dotfiles |
| `R` | reload both panes |
| `Space` | mark a file |
| `a` | mark all / clear marks |
| **`c`** / `F5` | **copy to the other pane's directory** (see below when zoomed) |
| `M` | cut, to be put down elsewhere on the same side |
| `P` | paste what `c` or `M` picked up into this directory |
| `e` / `F4` | edit in `$EDITOR` |
| `E` | edit with a program you name, just this once |
| `,` | settings: theme, editor — what sshman remembers between sessions |
| `v` | view in `$PAGER` |
| `:` | run a command in the remote pane's directory |
| `!` | full-screen shell in that directory: `ssh` for a server, `exec` into a container, a login shell on this machine |
| `o` | show the last command's output again |
| `z` / `x` / `X` | pack / unpack / list an archive |
| `D` | open a Docker container in a new tab |
| `L` | open a one-pane tab on this machine, with no server involved |
| `p` | forwarded ports |
| `N` | name the server on screen |
| `w` | workspaces: saved sets of connections |
| `W` | close the tab on screen |
| `Ctrl-←` / `Ctrl-→`, `Alt-1`…`Alt-9` | move between tabs |
| `S` | open/close a shell under the focused pane |
| `F6` / `Ctrl-]` | move the keyboard between the file list and the shell |
| `m` / `F3` | give the whole screen to the focused pane, or undo that |
| `Alt-←` / `Alt-→` | move the divider between the two sides |
| `Alt-↑` / `Alt-↓` | move the top edge of the shell pane |
| `=` | back to an even split |
| `s` | toggle sudo mode |
| `n` / `F7`, `r` / `F2`, `d` / `F8` | mkdir, rename, delete |
| `C` | connection screen: connect to a server, always in a new tab |
| `?` | help |
| `q` | quit |

The mouse works too: scroll wheel, click to focus a pane and select a row, drag
any border between panes to resize, and click the `[⤢]` in a pane's corner to
maximise it. In a shell pane, a program that has asked for the mouse gets it —
btop's clicks, a pager's wheel — and holding `Shift` scrolls the pane's own
history instead.

Copying acts on your marked files, or on the row under the cursor if you have
not marked anything. Directories are copied recursively, and existing files at
the destination are overwritten. Deleting always asks first.

## Pane sizes

The two sides start on an even split with the shell, when there is one, taking
the bottom of its column. Both dividers move: `Alt-←` and `Alt-→` for the one
down the middle, `Alt-↑` and `Alt-↓` for the top
edge of the shell, and dragging either border with the mouse does the same. The
file list keeps three rows whatever the shell asks for, and neither side can be
squeezed below a fifth of the width.

**Sizes belong to the tab.** A server you set up wide is still wide when you
come back to it, and the tab beside it keeps its own arrangement. A new tab
opens with the sizes that were on screen, so setting up a split once carries
into the next connection rather than snapping back, and `=` puts the tab on
screen back to an even split without touching the others. Workspaces write the
sizes down with everything else they remember.

Zoom is not a size, so it does not belong to a tab: it follows you across them,
as described below.

`m` gives the whole screen to whatever is focused, and `m`, `F3` or `Esc` gives
the other panes back. Every pane also carries a button in its top-right corner
— `[⤢]` to maximise, `[⤡]` to put it back — so the mouse can do it too, and
clicking the far pane's button blows up that pane rather than the focused one.

The zoom follows the focus rather than pinning one pane, so `Tab` and `F6` work
zoomed exactly as they do at any other size: you stay
zoomed, on whatever you moved to. `F3` does the same job from inside a shell,
where every other key belongs to the shell — including `m`.

A zoomed shell is resized on the far end like any other, so full-screen `top`
or `vim` gets the whole terminal.

Each tab remembers whether you were in its files or in its shell. Switching
tabs while zoomed into a shell shows the other tab's shell, or its file list
when that tab has no shell open — and coming back puts you in the shell you
left, still running. Unzoomed both are on screen either way, so switching tabs
there leaves the keyboard on the file list rather than dropping it into a shell
that would swallow the `Ctrl-←`/`Ctrl-→` you are cycling with.

## Moving files about within one side

With both panes on screen `c` copies across the middle. There is no across when
one pane fills the screen, so there `c` picks the selection up instead and `P`
puts it down in whatever directory you have navigated to since. `M` is the same
but a move. The title bar shows what is being carried until it lands, and `Esc`
drops it.

Both halves of a paste run as one `cp -a` or `mv` on one machine — nothing is
copied down and sent back — so it is as fast as the server is, works under sudo,
and keeps modes, ownership and timestamps. Files never leave the side they came
from: a clipboard picked up on the remote pane will not paste into the local one
(`c` on an unzoomed pane is the key that copies between the two).

Unlike `c`, a paste never overwrites. If any name is already taken in the
destination, the whole paste stops and says which one, leaving everything as it
was.

## Archives

`z` packs whatever is marked (or the row under the cursor) into an archive you
name; the suffix picks the format — `.tar`, `.tar.gz`/`.tgz`, `.tar.bz2`,
`.tar.xz`. `x` unpacks the archive under the cursor, defaulting to a directory
named after it so an archive without a single top-level folder cannot scatter
files across the pane. `X` lists what an archive holds without unpacking it.

All three work on either side, and run where the files are: packing a remote
directory never pulls it across the network first. In sudo mode the remote tar
runs as root, so root-only files are included. Archives made here are packed
without macOS's AppleDouble metadata, so unpacking on a Linux server does not
litter `._` files beside every real one.

## Containers: Docker and Podman

Press `D` to open a container in a new tab. Which machine is asked follows the
pane you are on:

- on the **local** pane, containers on this machine;
- on a **server's** pane, containers running on that server.

Either way you get a chooser, and the container opens as an ordinary tab:
browse it, copy files in and out, edit them in your editor, pack archives, run
commands, open a shell. Nothing else in the program knows the difference.

```sh
sshman --docker                    # go straight to the chooser for this machine
sshman --docker --runtime podman   # when both are installed and you want podman
```

**Podman works the same as Docker.** The runtime is discovered on whichever
machine holds the containers — this one, or the server — preferring `docker`
and falling back to `podman`, so a podman-only host needs no configuration at
all. `--runtime` (or `SSHMAN_CONTAINER_RUNTIME`) forces a specific one, by name
or by path, and it is checked when given rather than assumed, so a typo is
reported straight away. Whichever was found is named in the chooser's title,
and is remembered for that tab, so its shells and reconnects use the same one.

Nothing here is docker-specific beyond the command names: `ps --format`,
`inspect -f`, `exec -u/-it` and `cp -a` all mean the same thing to podman.
Rootless podman works too — it lists the containers that user can see.

A container tab shows `container` in the title bar. `s` switches it to uid 0
inside the container — the equivalent of sudo mode, and it needs no password,
because reaching the docker daemon already implies that much authority. The
badge reads `ROOT` rather than `SUDO` to keep the two honest when tabs of both
kinds are open side by side.

Under the hood, commands are `<runtime> exec` — run here for a local container,
or through the SSH connection for one on a server. Transfers use `<runtime> cp`;
for
a container on a server the file is staged in a temporary directory on that
server and moved the rest of the way over SFTP, which is where the progress bar
comes from. The staging directory is removed afterwards. Listing falls back
from GNU `find` to `ls -la`, so minimal images without a full findutils still
browse correctly.

Containers are addressed by id, so renaming one mid-session changes nothing.
They are deliberately **not** added to the remembered-servers list: a container
id is not something worth offering next week.

## Forwarded ports

`p` lists the ports carried from the server on screen to this machine; `a`
adds one, `d` stops it. The shorthand is what you would expect:

| you type | what happens |
|---|---|
| `3000` | `localhost:3000` here reaches port 3000 on the server |
| `8080:3000` | `localhost:8080` here reaches port 3000 on the server |
| `8080:db:5432` | `localhost:8080` here reaches `db:5432` **as the server sees it** |

That last form is the useful one: `db` is resolved by the server, so a database
that only listens on a private network becomes reachable from a client here.
The same goes for a service bound to the server's own loopback — invisible from
outside, but a forward reaches it.

Forwards bind `127.0.0.1` only, deliberately: a forward is for reaching
something yourself, and binding every interface would quietly republish a
private service to your whole network. The title bar shows `⇄ n` while any are
running, the list counts the connections each has carried, and **they are saved
with the workspace**, so reopening one brings its tunnels back up.

Each forward runs on its own SSH connection, for the same reason the shells do,
so a busy tunnel never holds up a file transfer.

## A tab on this machine

Not everything worth doing needs a server. `L` opens a tab that is just this
machine: **one pane**, no far side, starting in the directory the local pane was
showing. `sshman --local` starts on one.

```
 sshman  fedora  this machine  tab 1/2
 1 fedora   2 web01    C new · W close · Ctrl-←/→ switch
┏ THIS MACHINE /home/you/downloads ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ drwxr-xr-x    <DIR> 2026-08-20 21:37 archive/                               ┃
```

`S` opens a shell on this machine underneath it, and everything else a pane can
do works there: `e` edits, `z` packs, `d` deletes, `/` filters. With no other
side to copy to, `c` picks files up and `P` puts them down, the same keys a
zoomed pane uses — which makes it a good place to shuffle a directory around
your own disk with marks rather than typed paths.

`s` turns on sudo there too. That is real `sudo`, asking for your password and
feeding it to `sudo -S` on stdin exactly as the server side does, so a local tab
can browse and write root-owned places your own account cannot.

The one thing it cannot do is install an SSH key, since there is no login to set
up. Forwarded ports are equally beside the point, so a workspace saves a local
tab's directory, name and pane sizes and nothing else.

## Names

A server can be called what you actually call it. Type one in the **Name**
field on the connection screen, press `N` on a connected tab, or press `n` on a
saved server in the recent list. The name then replaces the address in the tab
bar, the title bar and the recent list — which still shows the address beside
it, so two similarly named boxes stay distinguishable.

Names are remembered with the server, and reconnecting without retyping one
does not wipe it. An empty name clears it. A container tab can be named too,
but only for that session: a container id is not worth remembering.

## Workspaces

A workspace is the answer to "the four things I always open together". Press
`w`, then `s`, and everything currently open is saved under a name. Press `w`
and `Enter` on one to open it again.

```
╭ Workspaces ─────────────────────────────────────────────────────────╮
│ morning            3 connections   production, beta box, cache      │
│ incident           2 connections   db1, db2                         │
╰─────────────────────────────────────────────────────────────────────╯
```

Each member remembers **which directory it was showing** and **the pane sizes
it was using**, so reopening puts you back where you were rather than at three
home directories in three identical panes — and the local pane's directory is
restored too. A workspace saved by an older version simply has no sizes in it,
and opens at whatever is on screen. Containers are saved by name rather than by
the id they happened to have, so a workspace survives them being recreated.

```sh
sshman -w morning          # open a workspace straight away
sshman --list-workspaces   # see what is saved
```

Connections open in parallel, each on its own worker, so a workspace of five
servers comes up in about the time one does.

**Passwords are never saved** — not in workspaces, not anywhere. A server that
can only be reached with a password therefore cannot reconnect on its own, so
it is listed in the title bar as waiting, and `C` offers it already filled in
with only the password missing. Servers on keys or an agent just reconnect. If
you want a workspace to come up untouched, tick *Install my public key* when
you first connect.

## Tabs: several servers at once

`C` opens the connection screen again and the server you pick arrives as a new
tab; `W` closes the one on screen. `Ctrl-←`/`Ctrl-→` move between them, `Alt-1`
… `Alt-9` jump straight to one, and clicking a tab works too. The bar only
appears once you have more than one.

```
 sshman  deploy@web01  tab 1/3
 1 deploy@web01    2 root@db1:2222 #    3 me@10.0.0.5 ⟳     C new · W close · Ctrl-←/→ switch
```

A `#` marks a tab in sudo mode and `⟳` one that is reconnecting. Each tab is a
separate SSH connection with its own directory, marks, filter, shell, sudo
state and transfers — a large copy on one tab does not hold up another, and
turning on sudo in one does not touch the rest. The local pane is shared, since
there is only one of your machine.

## Shells in the panes

Press `S` and a real shell opens under the focused file tree — a local shell
under the local pane, a shell on the server under the remote pane. Both can be
open at once. They are proper terminals, not a command box: `vim`, `top`,
`less`, colours and Ctrl-C all behave, because each one runs on a pty with a
terminal emulator behind it.

`F6` (or `Ctrl-]`) moves the keyboard between the file list and the shell. While
the shell has focus **every** key goes to it, including `Ctrl-C`, `Esc` and `q`
— `F6` is the way back out. The footer says so whenever a shell is focused.

- The local shell starts in the local pane's directory, running `$SHELL`.
- The remote shell starts in the remote pane's directory.
- `Alt-↑` / `Alt-↓` resize the pane; the scroll wheel moves through history.
- Full-screen programs work: `vim`, `top`, `btop`. One that asks for the mouse
  is given it — clicks, drags and the wheel all reach it — and `Shift` with the
  wheel scrolls the pane's own history regardless.
- Pasting into a focused shell works (bracketed paste).
- When a shell exits, the pane says so; `S` closes it, `S` again starts a fresh
  one.

The remote shell opens **its own SSH connection**, reusing the same host-key
and authentication path. That is deliberate: file transfers are driven by
blocking calls on a worker thread, and a shell has to be read continuously, so
sharing one connection would let a busy shell stall listings and copies. With
key or agent auth this is invisible. With password auth the saved password is
reused, so it does not ask again.

`!` is still there and is a different thing: it hands the *whole* terminal to a
full-screen `ssh` session, rather than running one inside a pane.

## Remembered servers

Every server you successfully connect to is saved to
`~/.config/sshman/hosts.json` (or `$XDG_CONFIG_HOME/sshman/`), most recent
first. Run `sshman` with no arguments, or press `C`, and they are listed above
the form:

```
 Recent servers   ↑↓ choose · Del forgets
 ▸ deploy@web01                     2h ago  key: deploy_key
   root@db1:2222                    3d ago
   me@10.0.0.5                      12 Mar
```

`↑↓` picks one — which also fills in the form below, so you can Tab across and
change a single field before connecting — `Enter` connects, `Del` forgets.
`Tab` jumps between the list and the form.

**Passwords are never written to disk**, only the user, host, port and which key
file was used. The file is written `0600`. If a saved server wants a password,
connecting from the list fails authentication once and drops the cursor
straight into the password box, so it is one extra keystroke rather than a
retyped hostname.

### Passwordless login

The connection screen has a checkbox, **Install my public key for passwordless
login** (Space toggles it). Tick it and, once you are in, your public key is
appended to the server's `~/.ssh/authorized_keys` — `~/.ssh` and the file are
created with the permissions sshd insists on, and re-running it never adds a
duplicate. That is `ssh-copy-id`, at the moment you would want it: the first
password login to a new box. Next time, no password.

It uses the `.pub` beside the key you named with `-i`, or the first of
`~/.ssh/id_ed25519.pub`, `id_ecdsa.pub`, `id_rsa.pub`.

### When the connection drops

A dead link is noticed either when you next ask for something or within about
20 seconds of going idle, and sshman reconnects on its own — up to six tries
with a widening gap. The title bar and the tab show `⟳` while it works. When it
comes back you are still in the directory you were in, and sudo mode is
re-established if it was on. If every attempt fails the tab says so and `C`
starts a fresh connection.

Shells do not follow: an embedded shell's session is gone for good when the
link drops, and its pane says `[exited]`. `S` twice starts a new one.

## Themes

Press `,`, pick **Theme**, and `↵` (or `←`/`→`) steps through them. The screen
redraws as you go, so you are choosing by looking rather than by guessing.

| | |
|---|---|
| `terminal` | the default: the terminal's own sixteen colours, so sshman matches whatever the rest of your setup is |
| `catppuccin` | Catppuccin Mocha |
| `monokai` | Monokai |
| `gruvbox` | Gruvbox dark |
| `mariana` | Mariana, the one Sublime Text ships |
| `afterglow` | Afterglow |
| `darcula` | Darcula, as IntelliJ draws it |

Each palette is taken from the theme's own source rather than from memory:
gruvbox from `morhetz/gruvbox`, Mariana from the scheme in Sublime's own
packages (its values are HSL there, converted here), Afterglow from
`YabataDesign/afterglow-theme`, Darcula from the colour scheme in
`JetBrains/intellij-community`.

Themes set foregrounds only. sshman never paints a background of its own, so
your terminal's shows through — which is what lets it sit inside a setup you
have already themed, and the only honest thing to do beside a shell pane, where
the program running in it paints its own colours anyway.

Every colour in the interface is one of twelve roles — accent, dim, text,
muted, good, warn, bad, dir, link, exec, info, and the text drawn on a coloured
chip — so a new theme is a table of twelve values in `src/theme.rs` and a line
in `Theme::ALL`. Nothing else has to know about it. There is a test that walks
every screen in every theme and fails if a single cell is painted a colour the
theme did not choose, so a hard-coded colour cannot creep back in.

## Editing files

`e` opens the file under the cursor in your editor; `E` asks which program to
use for that one file, so anything works — `nano`, `hx`, `code -w`, even a
non-interactive filter like `sed -i`.

Which editor `e` means is yours to decide. Out of the box it is `$VISUAL`, then
`$EDITOR`, then `vi`. Press `,` for the settings, pick **Editor**, and name one
instead:

```
╭ Settings ──────────────────────────────────────────────────────╮
│  ▸ Editor    hx  (set here)                                    │
│              the program e opens files with                    │
╰──────────────────────── ↵ change · Del clears · Esc closes ────╯
```

Each setting shows what it is set to and where that came from, so you can tell
an answer of your own from one inherited from the environment. `Del` clears one
back to whatever it would have been. They live in one file:

```json
// ~/.config/sshman/config.json
{
  "editor": "hx"
}
```

That setting wins over `$VISUAL` and `$EDITOR`, which is the point of having it —
`$EDITOR` is set on nearly every machine, so the other way round would leave it
useless. An empty answer clears it and hands you back to the environment, and
`--editor <program>` overrides it for a single run without changing what is
saved.

For a remote file, sshman downloads it to a temp path, releases the terminal,
runs your editor, and uploads it again when the editor exits. A file you did not
change is not re-uploaded. If your editor exits non-zero, nothing is uploaded
and sshman tells you where the downloaded copy is, so an edit is never lost.

Because the program is run through your shell, `code -w` and similar work as
written. Use a *blocking* editor: `code` and `subl` need `-w`, or the editor
returns immediately and sshman will think you made no changes.

## Sudo mode

This is the part that plain SFTP clients cannot do. The SFTP subsystem runs as
your login user, so files like `/root` or a `0600` file owned by root are
invisible to it no matter what you ask for.

Press `s`, give your sudo password (leave it empty if you have `NOPASSWD`), and
sshman verifies it before the mode turns on. From then on the remote pane
**lists, copies, edits and deletes as root**, and `:` commands run under sudo
too. The title bar shows a red `SUDO` badge whenever this is active.

If a directory is unreadable, the pane says so and moves there anyway, so
pressing `s` retries that same path.

How it works: listing shells out to `find` (falling back to `ls -la` on systems
without GNU find), and transfers are staged through a temporary directory in
`/tmp` that your login user owns — root copies the file there and hands over
ownership, then ordinary SFTP moves the bytes. The staging directory is removed
afterwards. Writing back copies the contents only: a file that is already there
keeps its own mode, owner and group, so saving an edit of a root-owned `0640`
config leaves it a root-owned `0640` config. New files land owned by root.

The sudo password is held in memory for the session only, is never written to
disk or passed on a command line, and is fed to `sudo -S` on stdin. File data is
never mixed into that stdin stream: `sudo` reads its password with a buffered
read that can swallow whatever follows it, which is exactly why transfers are
staged instead of piped.

## Notes

- All network work happens on a background thread, so the interface stays
  responsive during large transfers, and transfers show progress. Shells run on
  threads of their own.
- Remote paths are handled as POSIX strings throughout, and every path reaching
  a shell is single-quoted — a file called `; rm -rf /` is just a file.
- `!` starts a separate `ssh` process, so it authenticates on its own. With
  `ssh-agent` this is seamless; with password auth you will be asked again.
  The `S` shell does not have this problem — it reuses your credentials.

## Tests

```sh
cargo test
```

The parsers, path handling and shell quoting are covered by ordinary unit
tests. The tests that need a real server are `#[ignore]`d; a throwaway one is
included:

```sh
./testserver/run-live-tests.sh
```

That builds and starts a Debian container with sshd, a sudo-capable user and a
few root-only files, then runs the live tests against it — connecting,
listing, recursive transfers in both directions, the whole sudo path, and
host-key acceptance and mismatch detection.

The local shell is covered by an ordinary `cargo test`: it spawns a real pty,
runs a command in it and waits for the process to exit.
