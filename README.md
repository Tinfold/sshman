# sshman

A two-pane SSH file manager in the terminal. Your machine on the left, the
server on the right. Copy files either way with one key, open any file in your
own editor, and switch the remote pane to `sudo` when you need to see root-only
paths. **Split any pane** for a real shell — local or on the server, as many as
you like, with text you can select and copy out — or arrange a tab as a file
tree, your editor and a terminal, where clicking a file opens it in the editor
beside you. Open **several servers at once in tabs**, each with its own
directory, panes and sudo state, and drag them into whatever order you want.
Servers you connect to are remembered, so next time you pick one from a list,
and a dropped connection comes back on its own. **Everything that was open
comes back** — starting sshman offers you the last session, down to which
directory each pane and each shell was in, whether or not you saved anything.
**Docker containers work as targets too** — local ones or ones running on a
server — and behave exactly like a host — Docker or Podman.

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
└──────────────────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
 ↑ app.conf  1.4M / 2.1M  (67%)
✓ 1 item(s) copied to remote
 Ctrl-]  sshman keys   drag  select   every other key goes to the shell
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
sshman                          # connection form — offers the last session
sshman --local                  # a tab on this machine, no server involved
sshman --resume                 # everything that was open last time, no question
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
| `R` | reload both panes now (they keep up on their own — see below) |
| `Space` | mark a file |
| `a` | mark all / clear marks |
| **`c`** / `F5` | **copy to the other machine's directory** (see below when there is no other side on screen) |
| `M` | cut, to be put down elsewhere on the same side |
| `P` | paste what `c` or `M` picked up into this directory |
| `e` / `F4` | edit in `$EDITOR` |
| `E` | edit with a program you name, just this once |
| `,` | settings: theme, background, keys, editor, shell — what sshman remembers |
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
| `Ctrl-Shift-←` / `Ctrl-Shift-→` | move this tab along the row |
| `i` | an editor pane beside this one, or close the one there is |
| `S` | cut the focused pane in two, with a terminal below — or close the last one |
| `\|` | a terminal beside the focused pane, closing nothing |
| `_` | a terminal below it, closing nothing |
| `T` | another file list beside it, closing nothing |
| `F9` | close the focused pane, from anywhere |
| `A` | pick a ready-made arrangement for this tab |
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
| `q` | quit — it asks first, and the same key again is yes |

The mouse works too: the row under the pointer lights up as you move over a
file list, one click focuses that pane and puts the cursor on the row, and two
clicks mean the row — a directory opens, a file goes to your editor, exactly as
`Enter` would. The path along a pane's top edge is a trail you can click your
way back along: every piece of it points at the directory it names, so
`/etc/nginx/sites` is three steps back up to `/etc` in one click, and a path
too long for the pane loses whole pieces off the front rather than characters
out of the middle. The wheel scrolls, dragging any border between panes resizes
them, the `[⤢]` in a pane's corner maximises it and the `[✕]` beside it closes
it, and the `[+]` at the right of the top bar opens a new tab on this machine.
Each tab's chip carries a `✕` of its own, and dragging a chip along the row
moves that tab. Dragging across a shell pane picks text out and copies it when
the button comes up. In a shell pane a program that has asked for the mouse
gets it instead — btop's clicks, a pager's wheel — and holding `Shift` is the
way past that, to the pane's own scrollback and to selecting over it.

Every one of these can be [something else](#keys-of-your-own).

Copying acts on your marked files, or on the row under the cursor if you have
not marked anything. Directories are copied recursively, and existing files at
the destination are overwritten. Deleting always asks first.

## Keys of your own

Everything above is the scheme sshman ships with, and every one of those keys
can be something else. Press `,`, pick **Keys**, and you get the list of
everything sshman can be asked to do and which key asks for it:

```
╭ Keys ──────────────────────────────────────────────────────────────────────╮
│    S                shell           Panes — a shell below this pane        │
│    |                split           a shell beside this pane               │
│    _                split-down      a shell below, closing nothing         │
│  ▸ T                new-list        another file list beside this pane     │
│    F9               close-pane      close the focused pane                 │
│    m / F3           zoom            give the whole screen to this pane     │
╰────────── ↑↓ choose · ↵ then press a key · Del resets it · Esc closes ──────╯
```

`↵` on a line and then the key you want. It is **taken off whatever had it**,
so nothing ends up meaning two things, and the status line says what it was
taken from. `Del` puts one back to the key it ships with, and clearing the
whole **Keys** setting puts them all back.

The hint bar at the bottom follows, so it shows the keys *you* have rather than
the ones sshman ships with. (The help screen still lists the shipped scheme,
and says so.)

### In the config file

Only what you changed is written down, so the file says what you decided rather
than repeating fifty things you did not:

```json
// ~/.config/sshman/config.json
{
  "keys": {
    "quit": ["Q"],
    "zoom": ["z", "F3"]
  }
}
```

An action names its keys, rather than a key naming its action, so giving one
two keys is a list — which is how `zoom` ships answering to both `m` and `F3`.
An action the file does not name keeps what it had. A key a line asks for is
taken off whatever else had it, exactly as pressing it in the list would.

Keys are written the way you would say them: `q`, `S`, `F5`, `ctrl-]`,
`alt-left`, `alt-shift-right`, `space`, `esc`, `enter`, `tab`, `del`. A capital
letter carries its own shift, so `S` and `shift-s` are the same keystroke
written two ways. A line sshman cannot read — a key it does not know, a name it
has nothing for, or two actions asking for the same key — is reported in the
settings pane rather than quietly doing nothing.

**What cannot be rebound** is the modal set: the arrows that move between panes
while `Ctrl-]` has the keyboard, `↵` to use a pane, `Esc` to back out of an
overlay, `Alt-1`…`Alt-9` for tabs. Those are how you get *around* sshman rather
than what you do with it, and a rebound one would be a way to lock yourself out
of a box you had just opened.

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
half that opens up; `|` does the same sideways, `_` does it downwards, and `T`
puts another file list there instead. A pane is on the machine whose pane you
split, so splitting the remote pane gives you a shell — or a second directory —
on the server.

`S` is the one key that also takes back: from a file list it closes the last
terminal that machine has open. `|` and `_` never close anything, which is what
they are for — a second and a third shell without losing the first.

**Closing panes.** `F9` closes the focused pane and its neighbour takes the
space, from anywhere including inside a shell. Every pane also carries an
`[✕]` in its corner, which closes *that* pane whether or not it has the
keyboard — the quickest way to be rid of one you are not in. `S` from a file
list still closes the last terminal that machine has open, the way it always
has. The last pane on a tab cannot be closed; `W` closes the tab.

**Moving between them.** `Tab` steps through the file lists in the order they
are drawn, so with the two sshman opens with it crosses the middle, and with
more it reaches every one of them. `Alt-↑↓←→` moves to the pane across the
nearest border that way, and clicking one does the same. `Ctrl-]` is
[command mode](#command-mode), which reaches every pane and every other
sshman key from inside a terminal.

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

The zoom follows the focus rather than pinning one pane, so `Tab` works
zoomed exactly as it does at any other size: you stay zoomed, on whatever you
moved to. `F3` does the same job from inside a shell, where every other key
belongs to the shell — including `m`.

A zoomed shell is resized on the far end like any other, so full-screen `top`
or `vim` gets the whole terminal.

**The zoom belongs to the tab**, along with its arrangement: a tab left full
screen on a log stays that way while the tab beside it stays split, and coming
back to it is coming back to what you left rather than to whatever the last tab
happened to be doing.

Each tab also remembers which pane had the keyboard. Switching tabs while
zoomed into a shell shows the other tab's shell, or its file list when that tab
has none — and coming back puts you in the shell you left, still running.
Unzoomed both are on screen either way, so switching tabs there leaves the
keyboard on the file list rather than dropping it into a shell that would
swallow the `Ctrl-←`/`Ctrl-→` you are cycling with. The local file list is the
same pane on every tab, so being on it when you switch means staying on it.

## Keeping up with changes

A file list follows the directory it is showing. Files that appear, go away or
are renamed under it show up on their own, whether it was a build writing them,
a shell in the pane below, or somebody else on the server. `R` is still there
to reload both panes on the spot; it is no longer the only way to see what
happened.

The cursor and your marks are kept across a refresh you did not ask for. When
the file under the cursor is the one that went away, the cursor stays on that
row — where the next file along now is — rather than springing back to the top
of the list.

The two sides are watched in the way that is cheap on each:

- **This machine** by the directory's own timestamp, which moves whenever an
  entry is added, removed or renamed. That is a single `stat` a couple of times
  a second. Every few seconds a short list is also read in full, so a file
  being written to shows its new size without anything about the directory
  around it having changed. Lists longer than a couple of thousand entries skip
  that second part, and are watched by the timestamp alone.
- **A server** by asking it, every few seconds, for the listing of the
  directory on screen — and only for the tab you are looking at. The worker
  hashes the answer and says nothing at all when it matches what the pane
  already has, so an unchanged directory costs a message and no redraw. A poll
  never reports an error and never empties a pane: if the directory has gone or
  the link is wedged, the pane keeps what it has until you ask for yourself.

Neither side is a subscription, so a change is seen within a poll rather than
the instant it lands. In exchange there is nothing to set up, nothing to leak,
and a directory three networks away behaves like one on this machine.

If you would rather a list held still — a network mount that wakes a disk every
time it is looked at, a server you are being careful with — `,` → **Keeping up**
turns it off, and `R` goes back to being how a pane is refreshed. It is written
down as `"watch": "off"` in the config file.

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
| `0.0.0.0:8080:db:5432` | the same, bound where the rest of your network can reach it |
| `192.168.1.10:8080:db:5432` | the same, bound on one interface only |
| `[::1]:8080:db:5432` | an IPv6 address, in brackets so its colons are not read as separators |

The three-part form is the useful one: `db` is resolved by the server, so a
database that only listens on a private network becomes reachable from a client
here. The same goes for a service bound to the server's own loopback —
invisible from outside, but a forward reaches it.

**Forwards bind `127.0.0.1` unless you put an address in front of them.** A
forward is usually for reaching something yourself, and binding every interface
by default would quietly republish a private service to your whole network — so
that is something you ask for rather than something you get. The four-part form
is how you ask: an interface's address, or `*` (or `0.0.0.0`) for all of them,
which is the same spelling `ssh -L` takes. One bound past loopback is drawn in
the warning colour in the list, and says so when it starts.

The title bar shows `⇄ n` while any are running, the list counts the
connections each has carried, and **they are saved with the workspace** — bind
address and all — so reopening one brings its tunnels back up.

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

Each member remembers **the panes it was arranged into** and **the directory
every one of them was pointed at**, so reopening puts you back where you were
rather than at three home directories in three identical panes. That is every
pane, not just the first: a tab split into two file lists comes back with both
of them where you left them, and the lists and shells on your own machine are
restored the same way. A workspace saved by an older version simply has no
panes in it, and opens at whatever is on screen. Containers are saved by name
rather than by the id they happened to have, so a workspace survives them being
recreated.

**Shells come back too, running.** The arrangement includes the terminal panes,
so a tab you left split with a shell on the server opens split with a shell on
the server — in the same place, and in the directory that shell was in rather
than the one it was started in. A pane that was your
[editor](#the-editor-pane) opens as an editor again. They open on every tab a
workspace holds rather than only the one you happen to look at first, so a
workspace of four servers with a shell each comes back as four running shells.
What cannot come back is the *session*: a pty whose process has ended is gone,
so what you get is a fresh shell where the old one was. The part a workspace
can keep is the part that was yours to arrange rather than the shell's to
remember.

A remote shell waits for its tab to say where it is before opening, so a
workspace of five servers does not try to start shells on connections that are
still being made.

### How sshman knows where a shell is

A pane is a pty, and a pty carries characters rather than state, so a shell's
directory cannot simply be read off. sshman finds it three ways, in the order
it trusts them:

- **Asking the kernel.** For a shell on this machine, `/proc` says exactly
  where the process is. Always right, and always available on Linux.
- **`OSC 7`.** The escape sequence whose whole meaning is "I am in this
  directory". Right wherever it arrives — a server as readily as here — but
  only sent by shells whose prompt has been set up to send it.
- **The window title.** Most shells set it from their prompt, and the
  convention is `user@host: directory` — which is what the stock prompt does on
  every Debian- and Fedora-descended system. A guess rather than a report, so
  it is only read when the title takes that shape exactly, and never over
  either of the above.

Failing all three the shell is written down where it was started, which is
still the right answer for a shell nobody has `cd`-ed anywhere. If you want
this exact on a server whose shell says nothing, add `OSC 7` to its prompt:

```sh
# ~/.bashrc on the server
PROMPT_COMMAND='printf "\033]7;file://%s%s\033\\" "$HOSTNAME" "$PWD"'
```

## The previous session

You do not have to have saved anything. sshman writes down where the session
got to as you work — the same tabs, panes and directories a workspace holds —
so it survives being closed any way at all: quitting, a terminal window shut on
it, a laptop that went to sleep and never woke up.

**Starting sshman with nothing in particular to open offers it.** Run `sshman`
on its own and it asks — naming the servers it would reconnect and when they
were last open — before you have touched anything; `y` brings them back, `n`
starts fresh and leaves the session where it is. That is the point: coming back
should not depend on remembering a flag at the one moment you wanted it.
Anything on the command line has already said what to open, so a server, a
workspace, `--resume`, `-d` or `-L` are taken at their word and nothing is
asked. **Coming back** in the settings pane (`,`) turns the question off for
good.

```sh
sshman --resume            # everything that was open last time, without asking
```

It also sits at the top of the workspace list as **previous session**, so `w`
and `Enter` brings it back without leaving the keyboard, and `Del` on that row
forgets it. It is exactly a workspace you never had to name, and holds no more
than one does: no passwords, and no shell history.

The record is kept up to date as you go rather than written on the way out,
because there is no way out to hook — a closed window never comes back to
sshman at all. Whatever is on disk when the process stops is what comes back.
Nothing is written while nothing is open, so quitting an empty sshman does not
throw away the session before it.

```sh
sshman -w morning          # open a workspace straight away
sshman --list-workspaces   # see what is saved
```

Connections open in parallel, each on its own worker, so a workspace of five
servers comes up in about the time one does.

**Passwords are never saved** — not in workspaces, not anywhere. A server that
can only be reached with a password therefore cannot reconnect on its own, so
it is listed in the title bar as waiting, and `C` offers it already filled in
with only the password missing. Typing one in is that same connection carrying
on, not a new one: it opens on the directory the workspace saved, with the
panes and the forwarded ports it asked for. Servers on keys or an agent just reconnect. If
you want a workspace to come up untouched, tick *Install my public key* when
you first connect.

## Tabs: several servers at once

Clicking the `[+]` at the right of the top bar opens a tab straight away, on
this machine, in the directory you were looking at — a new tab asks you
nothing, and `L` does the same from the keyboard. `C` opens the connection
screen instead, and the server you pick arrives as its own tab. `W` closes the
one on screen, and each chip carries a `✕` that closes that tab whether or not
it is the one you are looking at. `Ctrl-←`/`Ctrl-→` move between them, `Alt-1`
… `Alt-9` jump straight to one, and clicking a tab works too. The bar only
appears once you have more than one.

```
 sshman  deploy@web01  tab 1/3
 1 deploy@web01 ✕   2 root@db1:2222 # ✕   3 me@10.0.0.5 ⟳ ✕   + new tab · ✕ or W close · Ctrl-←/→ switch
```

**Tabs move.** `Ctrl-Shift-←` and `Ctrl-Shift-→` shove the one on screen a
place along the row, wrapping at the ends the way stepping between them does,
and dragging a chip with the mouse does the same — the tabs change places as
the pointer crosses them, so what is under it is where the tab lands. Whichever
way you move one, the tab you were looking at is still the tab you are looking
at.

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
beside it instead and `_` below it, and neither of those closes anything, so
they are the two to reach for once you already have one open. As many as you
like, on either machine. They are proper
terminals, not a command box: `vim`, `top`, `less`, colours and Ctrl-C all
behave, because each one runs on a pty with a terminal emulator behind it.

Clicking a pane moves the keyboard into it, and out of it again. While the
shell has focus **every** key goes to it, including `Ctrl-C`, `Esc` and `q` —
`Ctrl-]` is [command mode](#command-mode), which hands the keyboard back to
sshman and reaches the rest of it without leaving the shell at all. The footer
lists the keys the shell does not swallow whenever one is focused.

- The local shell starts in the local pane's directory, running `$SHELL` — or
  [whichever shell you named](#which-shell-a-pane-starts).
- The remote shell starts in the remote pane's directory.
- `Alt-↑` / `Alt-↓` resize the pane; the scroll wheel moves through history.
- Full-screen programs work: `vim`, `top`, `btop`. One that asks for the mouse
  is given it — clicks, drags and the wheel all reach it — and `Shift` with the
  wheel scrolls the pane's own history regardless.
- The colours are the theme's, or the terminal's — see
  [the colours a shell pane draws in](#the-colours-a-shell-pane-draws-in).
- Pasting into a focused shell works, and is handed on as a bracketed paste
  when the program inside has asked for one — so a multi-line paste lands in
  the shell's line editor instead of running line by line before you have read
  it.
- `Shift-↵` reaches the program inside, when it has asked to be able to tell it
  from `↵` — see [keys that used to be the same
  key](#keys-that-used-to-be-the-same-key).
- When a shell exits, the pane says so; `S` closes it, `S` again starts a fresh
  one.
- A terminal belongs to the tab whose pane you opened it in. Switching tabs
  hides it rather than ending it, and it is still there when you come back.

### Keys that used to be the same key

`Shift-↵` has, for most of the history of terminals, been the same byte as `↵`.
There was nowhere in the encoding to say which was pressed, so a program that
wants one to send the line and the other to open a new one inside it — every
chat-shaped prompt written in the last few years, among others — could not have
it. The way out is the kitty keyboard protocol, and it takes agreement at both
ends: sshman's own terminal has to be willing to tell sshman which key was
pressed, and the program in the pane has to ask sshman for the difference.

sshman does both halves. On startup it asks its own terminal for unambiguous
keys, and takes no for an answer — a terminal that cannot do it is left exactly
as it was. To a program in a pane it then behaves as a terminal that supports
the protocol: it answers the query, remembers what was pushed and popped, and
spells `Shift-↵` as `CSI 13;2u` for the program that asked. A program that
asked for nothing gets `\r`, the way it always did, and so does every program
in every pane when sshman's own terminal said no.

It grants the one flag it can honestly honour — *disambiguate escape codes*,
which is the one that makes `Shift-↵` a key of its own. The rest of the
protocol asks for key **releases** and for every key as an escape code; sshman
is not being told about releases by its own terminal, so agreeing to pass them
on would be promising events that could never arrive. A program that asks for
everything is told what it actually got, which is what the protocol expects a
terminal supporting part of it to say.

Ghostty, kitty, foot, WezTerm and recent Alacritty all say yes. `xterm`, the
macOS Terminal and older builds say no, and sshman behaves there exactly as it
did before any of this.

### Which shell a pane starts

Out of the box a shell pane runs `$SHELL` here and the account's login shell on
a server, which is almost always what you want and needs no setting at all.
When it is not — `$SHELL` is what your terminal emulator was told, and that is
not always what you would like sshman to open — press `,`, pick **Shell**, and
name one:

```json
// ~/.config/sshman/config.json
{
  "shell": "fish"
}
```

A whole command line is allowed, so `bash --norc` and `nix develop -c fish` are
both fine; the words after the program are its arguments. It takes for the next
pane you open rather than for the ones already running — a shell you are in the
middle of using is not something to restart out from under you.

On a server the same name is used, **if the server has it**: sshman checks with
`command -v` before it `exec`s, so a box that has never heard of fish leaves you
in the login shell rather than in nothing at all. `!` — the full-screen shell —
does the same.

This is only ever the *interactive* shell in a pane. **sshman's own work goes
through `/bin/sh`**, whatever this says and whatever `$SHELL` says — listing a
directory root cannot read, packing an archive, the guard that keeps a paste
from overwriting anything. Those are one set of POSIX shell strings, built once
and run on both sides, because the far end of a connection is whatever `sh`
that account has. Handing them to `$SHELL` worked until somebody's `$SHELL` was
fish, which has no `for … do … done`: copying two files between local panes
failed with a complaint from a shell nobody had asked sshman to use, about a
line nobody had written. So naming a shell here — or using one — cannot break
anything but your own prompt, which is what it always should have meant.

The exception is the line *you* typed. `:` runs a command in the shell you
write commands in, because you wrote it; that is also what happens on the far
side, where an exec channel is read by the account's own login shell.

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

**A program inside a pane can copy too.** `"+y` in vim, `y` in a tmux copy
mode, anything else that speaks OSC 52: sshman is the terminal for those panes,
so the sequence stops there, and what it asks for is passed on to the terminal
sshman is itself running in. Nothing is swallowed — a copy from inside a pane
reaches the system clipboard by exactly the route a drag does, and is kept the
same way, so `Y` types it into another pane. (Before this, those copies went
nowhere at all.)

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
╰──────── ↑↓ look · ↵ keeps it · Esc puts the old one back · 24 themes ────╯
```

Each row carries its own palette as a row of blocks, so you can scan the list
without visiting every theme — and **the whole screen draws in whichever one
the cursor is on**, because a palette is only worth judging at the size you are
going to read it at. `↵` keeps the one you are looking at and writes it down;
`Esc` puts back the one you had, on the screen and in the file. The list
scrolls, so there can be as many themes as you like.

`←`/`→` on the **Theme** row still steps through them in place, for when you
know which way you are going. These are the twenty-four it ships with, and any
of your own:

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
| `ayu` | Ayu Dark, near-black and orange |
| `github` | GitHub Dark |
| `material` | Material Palenight, the purple one |
| `zenburn` | Zenburn, the low-contrast one everything muted descends from |
| `contrast` | black and bright, for daylight or for eyes that would rather not squint |
| `solarized-light` | **Solarized Light**, a light one |
| `latte` | **Catppuccin Latte**, a light one |
| `github-light` | **GitHub Light**, a light one |
| `gruvbox-light` | **Gruvbox Light**, a light one |

The last four are **light**: they paint a light background and dark text, so
they work on any terminal rather than only a light one. They are last in the
list because most terminals are dark.

`contrast` is the one to reach for when the others are too quiet. Everything in
it clears 7:1 against black, which is the strictest readability bar anybody
sets — it is picked for how far apart the colours are rather than for how they
sit together.

Each palette is taken from the theme's own source rather than from memory:
Nord from `nordtheme.com`, Dracula from the specification at
`draculatheme.com`, Tokyo Night from `folke/tokyonight.nvim`, Solarized from
Ethan Schoonover's sixteen values, Everforest from `sainnhe/everforest`, Rosé
Pine from `rose-pine/palette`, Kanagawa from `rebelot/kanagawa.nvim`, gruvbox
from `morhetz/gruvbox`, Mariana from the scheme in Sublime's own packages (its
values are HSL there, converted here), Afterglow from
`YabataDesign/afterglow-theme`, Darcula from the colour scheme in
`JetBrains/intellij-community`, Ayu from `dempfi/ayu`, GitHub's two from the
primer palette GitHub publishes, Material Palenight from `material-theme/vsc-material-theme`, Zenburn
from Jani Nurminen's original, and `contrast` from Protesilaos Stavrou's Modus
Vivendi. Where a theme names no secondary text colour,
`muted` is a blend of its comment and foreground; where a role is a judgement
call rather than a value, the file's `about` line says so.

### Backgrounds

Most themes name a background as well, and sshman paints it — including behind
a shell pane, wherever the program running in it has not painted its own. For
those panes sshman *is* the terminal emulator, so its background is the default
one, and `vim`'s or `btop`'s own colours sit on top of it exactly as they would
anywhere else.

This is ordinary cell painting inside the alternate screen — the same thing any
full-screen program does. **Nothing about the terminal itself is changed**: no
escape sequence sets its colours, so there is nothing to restore, nothing left
behind if sshman is killed, and no other pane or tab of the same window is
touched. Leaving sshman puts the screen back the way `nvim` does.

`,` → **Background** switches between the theme's own and the terminal's, and
the change is instant, so you can look at both. The `terminal` theme names no
background at all, being the theme whose whole point is matching what you have
already set up — pick it and the terminal's shows through whatever this setting
says.

A theme that paints its own background has taken responsibility for the pairing,
so there is a test that its text can actually be read against it.

### The colours a shell pane draws in

The same idea, carried through: `,` → **Shell colours** decides whether a shell
pane's *own output* — `ls`, a prompt, `git diff` — is coloured from the theme or
from the terminal's palette. The theme's, by default.

Only the sixteen a program can ask for **by number** are touched, because those
are names for roles rather than colours: "red" is whatever red means here, and
the theme is what it means. So `ls` shows directories in the theme's `dir` and
`git` shows a deletion in its `bad`. The rest of the 256, and any exact colour a
program named, are what it meant literally and are passed through untouched —
`btop`'s gradients and a truecolor editor scheme come out exactly as written.

A theme can spell the sixteen out itself:

```json
{
  "name": "mine",
  "ansi": [
    "#21222c", "#ff5555", "#50fa7b", "#f1fa8c",
    "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
    "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5",
    "#d6acff", "#ff92df", "#a4ffff", "#ffffff"
  ]
}
```

Left out, they are worked out from the roles it already has — red from `bad`,
green from `good`, blue from `dir`, and so on, since that is what those roles
mean. A theme that paints no background is never asked: it has not taken the
screen over, so the pairing of its colours with whatever is behind them is not
one it chose. That is why `terminal` leaves a shell pane entirely alone.

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
│    Shell      fish  (set here)                                 │
│               the shell a shell pane starts                    │
╰────────── ↵ opens it · ←→ steps it · Del clears · Esc closes ──╯
```

`↵` opens a setting: a prompt for the ones you type an answer to, and for
**Theme** and **Keys** their [choosers](#keys-of-your-own). Each setting shows what it is set to and
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
the editor a pane and leave it there. Press `i` and one opens beside the pane
you are on, and `i` again takes it away — the same key either way, the way `S`
works for a shell. `A` then **Editor** builds a whole tab around one instead: a
file list down the left, your editor beside it, and a terminal underneath.

```
┌ THIS MACHINE ~/src ──[⤢]┐┌ EDITOR ────────────────────────────[⤢]┐
│ drwxr-xr-x  <DIR> src/  ││ 1 # sshman                            │
│ -rw-r--r--  30.5K README││ 2                                     │
│ ...                     ││ ...                                   │
└─────────────── 7 items ─┘└───────────────────────────────────────┘
                           ┌ SHELL ─────────────────────────────[⤢]┐
                           │ ~/src$ cargo test                     │
                           └───────────────────────────────────────┘
```

Clicking a file in the list opens it in that pane, and `e` does the same from
the keyboard. With no editor pane open a single click only moves the cursor —
opening a file on one click would be a surprise otherwise — and it takes two to
open the file, which stands your editor up over the whole screen the way `Enter`
does.

The pane is a terminal on **the machine whose file list you are in**. Arrange
the remote pane that way and the editor is running on the server, over that
tab's own connection, editing the file where it lives: nothing is downloaded,
nothing is pushed back, and a save is a save. (Sudo mode is the exception — a
root-owned file still goes the long way round, since the shell in the pane
cannot read it either.)

sshman knows the keystrokes for vim, neovim, helix, kakoune, emacs and
textfold. For any other editor it treats the pane as the shell prompt it is and
runs your editor as a command, which works for anything. To spell it out yourself, press `,` and
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
first, because the editor may well be in insert mode. textfold's is `\ee{file}\r`
with no escape in front, because its Alt-E opens a path box over whatever else
is on its screen and there is nothing to get out of first. An empty setting
means "run it at the prompt", where the path *is* quoted for the shell.

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

## Leaving

`q` asks before it goes, because it sits one key away from half the file keys
and everything open goes with it — the shells, the connections, and anything
still transferring. The dialog names the tabs it would close and says what is
still running; pressing `q` again is the answer to it, so the second press
leaves. `Ctrl-C` behaves the same way from anywhere.

Nothing is lost either way: the session is [written down as you
go](#the-previous-session), and `sshman --resume` brings it back.

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
