# sshman

A two-pane SSH file manager in the terminal. Your machine on the left, the
server on the right. Copy files either way with one key, open any file in your
own editor, and switch the remote pane to `sudo` when you need to see root-only
paths. **Split any pane** for a real shell — local or on the server, as many as
you like, with text you can select and copy out — or arrange a tab as a file
tree, your editor and a terminal, where clicking a file opens it in the editor
beside you. Open **several servers at once in tabs**, each with its own
directory, panes and sudo state. Servers you connect to are remembered, so next
time you pick one from a list, and a dropped connection comes back on its own. **Docker containers work as
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
 F6  back to files   Ctrl-]  sshman keys   drag  select   every other key goes to the shell
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
| `Tab` | move to the next file list (`Shift-Tab` the previous) |
| `↑ ↓` / `k j`, `PgUp` `PgDn`, `g` `G` | move |
| `Enter` / `→` | enter a directory, or open a file in `$EDITOR` |
| `←` / `h` | parent directory |
| `~` | home directory |
| `f` | type a path to jump to |
| `t` | point the other file list at this directory |
| `/` | filter as you type (`Esc` clears it) |
| `.` | show/hide dotfiles |
| `R` | reload both panes |
| `Space` | mark a file |
| `a` | mark all / clear marks |
| **`c`** / `F5` | **copy to the other machine's directory** (see below when there is no other side on screen) |
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
| `S` | cut the focused pane in two, with a terminal below — or close the last one |
| `\|` | the same, with the terminal beside it |
| `T` | the same, with another file list beside it |
| `F9` | close the focused pane, from anywhere |
| `A` | pick a ready-made arrangement for this tab |
| `F6` | move the keyboard between the file list and the shell |
| `Ctrl-]` | command mode: hand the keyboard to sshman (see below) |
| `Alt-↑↓←→` | move the keyboard to the pane that way |
| `Alt-Shift-↑↓←→` | move the border nearest the focused pane |
| `Ctrl-]` `g` | pick the focused pane up and move it |
| `y` / `Y` | copy what is picked out in a shell / paste it into one |
| `m` / `F3` | give the whole screen to the focused pane, or undo that |
| `=` | even the borders up again |
| `s` | toggle sudo mode |
| `n` / `F7`, `r` / `F2`, `d` / `F8` | mkdir, rename, delete |
| `C` | connection screen: connect to a server, always in a new tab |
| `?` | help |
| `q` | quit |

The mouse works too: scroll wheel, click to focus a pane and select a row, drag
any border between panes to resize, click the `[⤢]` in a pane's corner to
maximise it or the `[✕]` beside it to close it, and click the `[+]` at the
right of the top bar for a new tab on this machine. Dragging across a shell
pane picks text out and copies it when the button comes up. In a shell pane a
program that has asked for the mouse gets it instead — btop's clicks, a pager's
wheel — and holding `Shift` is the way past that, to the pane's own scrollback
and to selecting over it.

Copying acts on your marked files, or on the row under the cursor if you have
not marked anything. Directories are copied recursively, and existing files at
the destination are overwritten. Deleting always asks first.

## Command mode

A focused shell takes every key — that is what makes it a real terminal — so
the rest of sshman is behind one chord. **`Ctrl-]` hands the keyboard to
sshman.** Every key then does exactly what it does with a file list focused:
`C` connects, `w` is the workspaces, `,` is the settings, `S` opens a shell,
`q` quits. There is one set of keys, not one set per place you happen to be
standing.

On top of those it adds the four that only make sense while sshman is holding
the keyboard:

| | |
|---|---|
| `↑` `↓` `←` `→` | move to the pane that way, without going into it |
| `↵` | hand the keyboard to the pane you have moved to |
| `Shift-↑↓←→` | move the border nearest it |
| `g` | pick the pane up, to move the pane rather than the keyboard |
| `Esc` / `Ctrl-]` | put the keyboard back where it was |

The arrows move without handing anything over, so they **walk past a shell
rather than falling into it** — you can cross three panes to reach the fourth
and only then press `↵`. The pane the arrows are on says ` ↵ use this pane ` on
its bottom border, so there is never a question of which one that is. `↵` into
a terminal puts you back to typing in it; `Esc` puts the keyboard back where
you took it from.

### Moving a pane

`g` picks the focused pane up. The arrows then move **the pane** rather than
the keyboard:

| | |
|---|---|
| `↑` `↓` `←` `→` | shove it past its neighbour, and again to keep going |
| `Shift-↑↓←→` | send it the whole way to that edge, as a column or a row |
| `↵` | drop it and use it |
| `Esc` / `g` | put it down, keyboard still sshman's |

The pane it is carrying says ` ✥ moving this pane ` on its bottom border, and
the keyboard goes with the pane — so the arrows keep meaning the same thing
however far it has travelled, and you can shove one across three panes without
recounting which way is which.

Shoving swaps two panes and keeps the shape of the arrangement. `Shift` and an
arrow is the one that *changes* the shape: it takes the pane out of wherever it
is, closes the gap behind it, and puts it back as a full column or row against
that edge. A terminal stacked under a file list becomes a terminal beside
everything, which no amount of swapping could do.

**With the mouse:** drag a pane by its name — the ` LOCAL ` or ` SHELL ` at the
start of its top border — and let go over another pane to change them over. The
one being carried says ` ✥ moving `, and the one under the cursor says
` change places `, so there is no guessing where it will land. The name is
tested before the border underneath it, so dragging the border still resizes.

### One rule for the arrows

`h` `j` `k` `l` sit beside the arrows here, as they do in a file list. Where
the arrows are already spoken for — in a file list, where they move the cursor
— the same two ideas are spelled with `Alt`: `Alt-↑↓←→` moves the keyboard
between panes and `Alt-Shift-↑↓←→` moves the border. It is one rule either way:
**arrows move the keyboard, `Shift` and arrows move the border — and after `g`,
the pane itself.**

(`Ctrl-]` is the byte `0x1d`, which most terminals report as `Ctrl-5`. sshman
takes both spellings — before, it only took the one, and the chord quietly did
nothing on the terminals that send the other.)

## Panes

A tab is a set of panes, and the set is a tree: every pane is either something
to look at — a file list, a terminal — or two arrangements sharing the space,
with a percentage saying how the space between them is divided. What sshman
opens with is not a special case, it is just the tree you begin with:

```
Split{ across, 50%, Files(local), Files(remote) }
```

so splitting a pane, closing one, dragging a border and zooming are all one set
of operations rather than one set per shape the screen might take.

**Making panes.** `S` cuts the focused pane in two and puts a terminal in the
half that opens up; `|` does the same sideways, and `T` puts another file list
there instead. A pane is on the machine whose pane you split, so splitting the
remote pane gives you a shell — or a second directory — on the server.

**Closing panes.** `F9` closes the focused pane and its neighbour takes the
space, from anywhere including inside a shell. Every pane also carries an
`[✕]` in its corner, which closes *that* pane whether or not it has the
keyboard — the quickest way to be rid of one you are not in. `S` from a file
list still closes the last terminal that machine has open, the way it always
has. The last pane on a tab cannot be closed; `W` closes the tab.

**Moving between them.** `Tab` steps through the file lists in the order they
are drawn, so with the two sshman opens with it crosses the middle, and with
more it reaches every one of them. `Alt-↑↓←→` moves to the pane across the
nearest border that way, and clicking one does the same. `F6` goes in and out
of a terminal in one press, and `Ctrl-]` is [command mode](#command-mode).

### More than one file list

`T` gives the machine you are on a second file list, opening where the first
one is looking, and `f` points it somewhere else. There can be as many as you
have room for, on either machine or both — two directories of a server side by
side, or of your own machine, or the four of them at once.

Everything that acts between two panes acts between **the focused list and the
one marked `c copies here`**: the other one when there are two, and otherwise
the one you were in last. It is drawn on the pane itself, so there is nothing
to work out.

```
┏ THIS MACHINE 1 ~/src ━━━━━━━━━[✕][⤢]┓┌ THIS MACHINE 2 ~/src/ui ──[✕][⤢]┐
┃ -rw-r--r--  229K app.rs             ┃│ -rw-r--r--  8.1K pane.rs        │
┃ -rw-r--r--  8.1K archive.rs         ┃│ -rw-r--r--  13K  hints.rs       │
┗━━━━━━━━━━━━━━━━━━━━━━━━━━ 20 items ━┛└ c copies here ────── 2 items ───┘
```

`c` between two lists on the same machine runs one `cp -a` **there** — nothing
is copied down and sent back — so it is as fast as that machine is, works under
sudo, and keeps modes, ownership and timestamps. Across the middle it is still
an upload or a download. Either way `t` points the marked list at the directory
you are in, and `M`/`P` still cut and paste within one machine.

**Ready-made arrangements.** `A` offers four, and each is a starting point
rather than a mode sshman is in — anything they build can be built by hand:

| | |
|---|---|
| Side by side | this machine and the server, the way sshman opens |
| One pane | the pane you are on, filling the tab |
| Two lists here | two directories of the same machine, to copy between |
| Files and a terminal | a narrow file list, and a terminal beside it |
| Editor | a file list, your editor beside it, a terminal underneath |

Rearranging closes the terminals the new arrangement has no room for, the same
as closing their panes one at a time: an arrangement is what is on screen, and
nothing is left running out of sight.

**Sizes.** `Alt-←` and `Alt-→` move the border nearest the focused pane;
`Alt-↑` and `Alt-↓` do the same up and down. Dragging any border with the mouse
does it too. No pane is ever cut below eight columns or three rows, and no
border can be pushed past a tenth of the space it divides.

**The arrangement belongs to the tab.** A server you set up wide is still wide
when you come back to it, and the tab beside it keeps its own shape. A new tab
opens with the arrangement that was on screen — minus the panes belonging to
the tab you were on, whose terminals are not the new one's to show — and `=`
evens the borders up on the tab on screen without touching the others.
Workspaces write the whole arrangement down with everything else they remember.

Zoom is not part of the arrangement, so it does not belong to a tab: it follows
you across them, as described below.

`m` gives the whole screen to whatever is focused, and `m`, `F3` or `Esc` gives
the other panes back. Every pane also carries a button in its top-right corner
— `[⤢]` to maximise, `[⤡]` to put it back — so the mouse can do it too, and
clicking another pane's button blows up that pane rather than the focused one.

The zoom follows the focus rather than pinning one pane, so `Tab` and `F6` work
zoomed exactly as they do at any other size: you stay zoomed, on whatever you
moved to. `F3` does the same job from inside a shell, where every other key
belongs to the shell — including `m`.

A zoomed shell is resized on the far end like any other, so full-screen `top`
or `vim` gets the whole terminal.

Each tab remembers which pane had the keyboard. Switching tabs while zoomed
into a shell shows the other tab's shell, or its file list when that tab has
none — and coming back puts you in the shell you left, still running. Unzoomed
both are on screen either way, so switching tabs there leaves the keyboard on
the file list rather than dropping it into a shell that would swallow the
`Ctrl-←`/`Ctrl-→` you are cycling with. The local file list is the same pane on
every tab, so being on it when you switch means staying on it.

## Moving files about within one side

With another file list on screen `c` copies into it. There is no other list
when it is not there — zoomed, on a tab that is only this machine, or in an
arrangement with no room for one — so there `c` picks the selection up instead
and `P` puts it down in whatever directory you have navigated to since. `M` is
the same but a move. The title bar shows what is being carried until it lands,
and `Esc` drops it.

Both halves of a paste run as one `cp -a` or `mv` on one machine — nothing is
copied down and sent back — so it is as fast as the server is, works under sudo,
and keeps modes, ownership and timestamps. Files never leave the side they came
from: a clipboard picked up on the remote pane will not paste into the local one
(`c` with another list on screen is the key that copies between the two).

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
can browse and write root-owned places your own account cannot. That is the one
case where `e` does not open a file in place: a root-owned file is not yours to
open, so it is fetched as root, edited, and pushed back as root.

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

Clicking the `[+]` at the right of the top bar opens a tab straight away, on
this machine, in the directory you were looking at — a new tab asks you
nothing, and `L` does the same from the keyboard. `C` opens the connection
screen instead, and the server you pick arrives as its own tab. `W` closes the
one on screen. `Ctrl-←`/`Ctrl-→` move between them, `Alt-1`
… `Alt-9` jump straight to one, and clicking a tab works too. The bar only
appears once you have more than one.

```
 sshman  deploy@web01  tab 1/3
 1 deploy@web01    2 root@db1:2222 #    3 me@10.0.0.5 ⟳    + or L new tab · C connect · W close · Ctrl-←/→ switch
```

With more tabs than the row can hold it becomes a window on to them, always
showing the one you are on, with `‹3` and `4›` at the ends saying how many did
not fit — clicking either steps that way. Names shorten as the tabs pile up
rather than the row running off the screen, and the reminder of the keys at the
end gives up its space first.

```
 ‹3  4 web04   5 web05   6 db1 #   7 cache ⟳   2›
```

A `#` marks a tab in sudo mode and `⟳` one that is reconnecting. Each tab is a
separate SSH connection with its own panes, directories, marks, filter, sudo
state and transfers — a large copy on one tab does not hold up another, and
turning on sudo in one does not touch the rest. The lists and terminals on
*your* machine are shared, since there is only one of it: each tab's
arrangement decides which of them it shows, and a shell left running in one tab
is still running when another shows it.

## Shells in the panes

Press `S` and a real shell opens under the focused pane — a local shell under
the local pane, a shell on the server under the remote pane. `|` puts one
beside it instead. As many as you like, on either machine. They are proper
terminals, not a command box: `vim`, `top`, `less`, colours and Ctrl-C all
behave, because each one runs on a pty with a terminal emulator behind it.

`F6` moves the keyboard between the file list and the shell. While the shell
has focus **every** key goes to it, including `Ctrl-C`, `Esc` and `q` — `F6` is
the way back out, and `Ctrl-]` is [command mode](#command-mode), which reaches
the rest of sshman without leaving the shell at all. The footer says so
whenever a shell is focused.

- The local shell starts in the local pane's directory, running `$SHELL`.
- The remote shell starts in the remote pane's directory.
- `Alt-↑` / `Alt-↓` resize the pane; the scroll wheel moves through history.
- Full-screen programs work: `vim`, `top`, `btop`. One that asks for the mouse
  is given it — clicks, drags and the wheel all reach it — and `Shift` with the
  wheel scrolls the pane's own history regardless.
- Pasting into a focused shell works, and is handed on as a bracketed paste
  when the program inside has asked for one — so a multi-line paste lands in
  the shell's line editor instead of running line by line before you have read
  it.
- When a shell exits, the pane says so; `S` closes it, `S` again starts a fresh
  one.
- A terminal belongs to the tab whose pane you opened it in. Switching tabs
  hides it rather than ending it, and it is still there when you come back.

### Selecting text in a shell

Drag across a shell pane and the text is picked out, marked by turning those
cells inside out — sshman has no background of its own to paint over them, and
reversing whatever the program drew reads as a selection in any theme. The
selection runs in reading order, the whole of every row between its two ends,
and a row that wrapped runs on into the next rather than gaining a newline that
was never typed.

Letting the button up copies it, the way selecting in a terminal does. It goes
to the system clipboard through the terminal itself (OSC 52), which is the only
way that works when sshman is at the far end of an SSH connection: there is no
display to talk to, only the terminal, and the terminal owns the clipboard.
Some terminals need it turned on — `set-clipboard on` in tmux, `clipboard_control`
in kitty. sshman keeps the text either way, so `Y` types it into any shell pane
whether or not the clipboard could be reached — and `y` copies the selection
again, for when a drag reached the clipboard but you would rather it had not.

Scrollback is included: scroll back with the wheel and drag over what you find
there. A program that has asked for the mouse — `btop`, `vim`, a pager — gets
the drag instead, and holding `Shift` is the way past it, the same escape hatch
a terminal gives you. Anything that redraws what is under a selection — a
keystroke, a scroll, a resize — lets go of it rather than leaving a highlight
over text that has moved on.

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

Press `,`, pick **Theme**, and `↵` opens the chooser:

```
╭ Themes ─────────────────────────────────────────────────────────────────╮
│    gruvbox      ██████████  Gruvbox dark, from morhetz/gruvbox.          │
│    everforest   ██████████  Everforest dark medium, from sainnhe/…       │
│  ▸ solarized    ██████████  Solarized Dark, from Ethan Schoonover's…     │
│    onedark      ██████████  One Dark, as Atom drew it and half the…      │
│    monokai      ██████████  Monokai, as everyone remembers it.           │
╰──────── ↑↓ look · ↵ keeps it · Esc puts the old one back · 17 themes ────╯
```

Each row carries its own palette as a row of blocks, so you can scan the list
without visiting every theme — and **the whole screen draws in whichever one
the cursor is on**, because a palette is only worth judging at the size you are
going to read it at. `↵` keeps the one you are looking at and writes it down;
`Esc` puts back the one you had, on the screen and in the file. The list
scrolls, so there can be as many themes as you like.

`←`/`→` on the **Theme** row still steps through them in place, for when you
know which way you are going. These are the sixteen it ships with, and any of
your own:

| | |
|---|---|
| `terminal` | the default: the terminal's own sixteen colours, so sshman matches whatever the rest of your setup is |
| `catppuccin` | Catppuccin Mocha |
| `dracula` | Dracula |
| `nord` | Nord |
| `tokyonight` | Tokyo Night |
| `gruvbox` | Gruvbox dark |
| `everforest` | Everforest dark, the green one |
| `solarized` | Solarized Dark |
| `onedark` | One Dark, as Atom drew it |
| `monokai` | Monokai |
| `kanagawa` | Kanagawa, ink and paper |
| `rosepine` | Rosé Pine |
| `mariana` | Mariana, the one Sublime Text ships |
| `afterglow` | Afterglow |
| `darcula` | Darcula, as IntelliJ draws it |
| `solarized-light` | **Solarized Light** |
| `latte` | **Catppuccin Latte** |

The last two are for a terminal with a **light background**. sshman paints no
background of its own, so a dark theme on a light terminal is white text on
white — these are the way round that works, and they are last in the cycle
because most terminals are dark.

Each palette is taken from the theme's own source rather than from memory:
Nord from `nordtheme.com`, Dracula from the specification at
`draculatheme.com`, Tokyo Night from `folke/tokyonight.nvim`, Solarized from
Ethan Schoonover's sixteen values, Everforest from `sainnhe/everforest`, Rosé
Pine from `rose-pine/palette`, Kanagawa from `rebelot/kanagawa.nvim`, gruvbox
from `morhetz/gruvbox`, Mariana from the scheme in Sublime's own packages (its
values are HSL there, converted here), Afterglow from
`YabataDesign/afterglow-theme`, Darcula from the colour scheme in
`JetBrains/intellij-community`. Where a theme names no secondary text colour,
`muted` is a blend of its comment and foreground; where a role is a judgement
call rather than a value, the file's `about` line says so.

Themes set foregrounds only. sshman never paints a background of its own, so
your terminal's shows through — which is what lets it sit inside a setup you
have already themed, and the only honest thing to do beside a shell pane, where
the program running in it paints its own colours anyway.

Every colour in the interface is one of twelve roles — accent, dim, text,
muted, good, warn, bad, dir, link, exec, info, and the text drawn on a coloured
chip. Two tests hold them to it: one walks every screen in every theme and
fails if a single cell is painted a colour the theme did not choose, and one
checks that the text on a coloured chip can actually be read against it —
that being the one pairing sshman puts together itself, everything else being
drawn against a background it does not own. A hard-coded colour cannot creep
back in, and a palette that looked fine in the file but not on the screen is
caught before it ships.

### Themes of your own

A theme is a file, not a table in the source. The sixteen above live in
`themes/` and are built into the binary, so there is nothing to install; drop
any `.json` file in `~/.config/sshman/themes/` and it is loaded beside them.
A file that takes a name sshman already uses replaces it, which is how you
rewrite one of ours without forking anything.

```json
{
  "name": "midnight",
  "about": "anything you like — JSON has nowhere else to put a comment",

  "accent": "#7aa2f7",
  "dim": "#3b4261",
  "text": "#c0caf5",
  "muted": "#9aa5ce",
  "good": "#9ece6a",
  "warn": "#e0af68",
  "bad": "#f7768e",
  "dir": "#7dcfff",
  "link": "#bb9af7",
  "exec": "#9ece6a",
  "info": "#2ac3de",
  "on_accent": "#1a1b26"
}
```

Colours are written as `#rrggbb`, as `#rgb`, as one of the sixteen terminal
colours by name (`cyan`, `dark gray`, `bright-red`), or as a number from 0 to
255 for a slot in the 256-colour cube.

Every colour is optional. Give a `"base"` and what you leave out comes from
there, so a theme that only wants a different accent is three lines:

```json
{ "name": "mine", "base": "gruvbox", "accent": "#d3869b" }
```

With no `base`, what is left out comes from `terminal`.

The settings pane (`,`) counts the themes it found and names the directory to
put more in. A file it could not use is listed there too, with the reason — a
colour it does not recognise, a role spelled wrong, a `base` that is not
there — rather than quietly going missing.

## Editing files

`e` opens the file under the cursor in your editor; `E` asks which program to
use for that one file, so anything works — `nano`, `hx`, `code -w`, even a
non-interactive filter like `sed -i`.

Which editor `e` means is yours to decide. Out of the box it is `$VISUAL`, then
`$EDITOR`, then `vi`. Press `,` for the settings, pick **Editor**, and name one
instead:

```
╭ Settings ──────────────────────────────────────────────────────╮
│  ▸ Editor     hx  (set here)                                   │
│               the program e opens files with                   │
│    Opens with \e:o {file}\r  (for your editor)                  │
│               the keys that open {file} in an editor pane      │
╰────────── ↵ opens it · ←→ steps it · Del clears · Esc closes ──╯
```

`↵` opens a setting: a prompt for the ones you type an answer to, and for
**Theme** the [chooser](#themes). Each setting shows what it is set to and
where that came from, so you can tell
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

A file **on this machine** is opened where it lies — its real path, with the
rest of its directory around it. That is true of the left pane and of a "this
machine" tab alike: they are the same filesystem, so they behave the same way,
and a save is a save rather than a copy back.

For a file on a server or in a container, sshman downloads it to a temp path,
releases the terminal, runs your editor, and uploads it again when the editor
exits. A file you did not
change is not re-uploaded. If your editor exits non-zero, nothing is uploaded
and sshman tells you where the downloaded copy is, so an edit is never lost.

The editor is started **in the file's own directory**, not wherever sshman was
launched, so anything that works out which project it is in by looking around
itself — an LSP, a fuzzy finder, `git` — finds the right tree.

Because the program is run through your shell, `code -w` and similar work as
written. Use a *blocking* editor: `code` and `subl` need `-w`, or the editor
returns immediately and sshman will think you made no changes.

### The editor pane

All of that is sshman standing aside for your editor. The other way is to give
the editor a pane and leave it there: press `A`, pick **Editor**, and the tab
becomes a file list down the left, your editor beside it, and a terminal
underneath.

```
┌ THIS MACHINE ~/src ──[⤢]┐┌ EDITOR ────────────────────────────[⤢]┐
│ drwxr-xr-x  <DIR> src/  ││ 1 # sshman                            │
│ -rw-r--r--  30.5K README││ 2                                     │
│ ...                     ││ ...                                   │
└─────────────── 7 items ─┘└──────────────────────── F6 to focus ──┘
                           ┌ SHELL ─────────────────────────────[⤢]┐
                           │ ~/src$ cargo test                     │
                           └──────────────────────── F6 to focus ──┘
```

Clicking a file in the list opens it in that pane, and `e` does the same from
the keyboard. With no editor pane open a click only moves the cursor, as it
always has — opening a file on a single click would be a surprise otherwise.

The pane is a terminal on **the machine whose file list you are in**. Arrange
the remote pane that way and the editor is running on the server, over that
tab's own connection, editing the file where it lives: nothing is downloaded,
nothing is pushed back, and a save is a save. (Sudo mode is the exception — a
root-owned file still goes the long way round, since the shell in the pane
cannot read it either.)

sshman knows the keystrokes for vim, neovim, helix, kakoune and emacs. For any
other editor it treats the pane as the shell prompt it is and runs your editor
as a command, which works for anything. To spell it out yourself, press `,` and
pick **Opens with**:

```json
// ~/.config/sshman/config.json
{
  "editor": "hx",
  "editor_open": "\\e:o {file}\\r"
}
```

`{file}` is the path, as it is: what the keys around it are typed into is the
editor's own command line, not a shell, and every editor escapes differently —
a path with spaces in it wants keys of your own. `\e` is escape, `\r` a return,
`\t` a tab and `\C-x` a control character, so vim's is `\e:e {file}\r` — escape
first, because the editor may well be in insert mode. An empty setting means
"run it at the prompt", where the path *is* quoted for the shell.

If you quit the editor, the pane goes with it; opening the next file starts a
fresh one on that file.

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
