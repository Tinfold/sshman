//! A glyph in front of a file name.
//!
//! Off by default, because an icon is only an improvement in a terminal whose
//! font has the glyph: a Nerd Font draws `` where anything else draws a
//! box, and a box in front of every name is worse than no icon at all. So
//! this is a setting with three answers rather than a switch with two — the
//! Nerd Font set, an emoji set for a terminal without one, and none.
//!
//! One table serves both sets. Each row names what a file is once and gives
//! the two glyphs that say it, which is what keeps them from drifting apart:
//! a language added to one is added to the other in the same breath.
//!
//! The Nerd Font glyphs are taken from the ranges that have not moved between
//! Nerd Fonts v2 and v3 — Font Awesome, Devicons, Seti — so the same table
//! draws the same icons whichever version is installed.
//!
//! The emoji set is deliberately coarser. There is no crab for C and no
//! gopher for Perl, so files whose language has no emoji of its own share a
//! scroll: the set is there to say *what kind of thing this is* at a glance,
//! which is most of what an icon is for, and a terminal with a Nerd Font has
//! the specific one waiting.

use crate::types::{EntryKind, FileEntry};

/// Which set of icons a file list draws with, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Icons {
    /// Names alone, which is what sshman looked like before there were icons.
    #[default]
    Off,
    /// A Nerd Font's own glyphs: one column each, and one per language.
    Nerd,
    /// Emoji, for a terminal whose font is not a patched one. Two columns
    /// each, and coarser about what it can tell apart.
    Emoji,
}

impl Icons {
    /// The three answers, in the order stepping through them visits.
    pub const ALL: [Icons; 3] = [Icons::Off, Icons::Nerd, Icons::Emoji];

    /// The word written in the config file. Short and lowercase, since it is
    /// something a person may well type there by hand.
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Nerd => "nerd",
            Self::Emoji => "emoji",
        }
    }

    /// What the settings pane shows for it.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Off => "none — just the names",
            Self::Nerd => "Nerd Font glyphs",
            Self::Emoji => "emoji",
        }
    }

    /// Read a word from the config file. Anything we do not recognise means
    /// no icons — the same as an absent setting, and the same as a file
    /// written by a later version that knows a set this one does not: a name
    /// we cannot draw is better left undrawn than guessed at.
    pub fn named(word: &str) -> Self {
        match word.trim().to_lowercase().as_str() {
            "nerd" | "nerdfont" | "nerd-font" | "nerdfonts" | "on" | "yes" => Self::Nerd,
            "emoji" | "unicode" | "plain" => Self::Emoji,
            _ => Self::Off,
        }
    }

    /// The next set along, wrapping, so `←`/`→` walk the three in a ring.
    pub fn stepped(self, step: isize) -> Self {
        let at = Self::ALL.iter().position(|&set| set == self).unwrap_or(0) as isize;
        let len = Self::ALL.len() as isize;
        Self::ALL[(at + step).rem_euclid(len) as usize]
    }

    /// The glyph to draw in front of this entry, or `None` when there are no
    /// icons to draw.
    ///
    /// What a thing *is* comes before what it is called: a symlink is drawn
    /// as a link whatever it is named, and only an ordinary file is looked up
    /// by its name at all.
    pub fn of(self, entry: &FileEntry) -> Option<&'static str> {
        let (nerd, emoji) = match entry.kind {
            EntryKind::Dir => FOLDER,
            EntryKind::Symlink => LINK,
            EntryKind::Other => DEVICE,
            EntryKind::File => match glyphs(&entry.name) {
                Some(pair) => pair,
                // Nothing known about the name, so the last thing left to go
                // on is whether it can be run.
                None if entry.perms.contains('x') => RUNNABLE,
                None => FILE,
            },
        };
        match self {
            Self::Off => None,
            Self::Nerd => Some(nerd),
            Self::Emoji => Some(emoji),
        }
    }
}

/// One icon, in both sets: the Nerd Font glyph and the emoji that stands in
/// for it.
type Pair = (&'static str, &'static str);

const FOLDER: Pair = ("\u{f07b}", "📁");
const LINK: Pair = ("\u{f0c1}", "🔗");
const DEVICE: Pair = ("\u{f2db}", "🔌");
const FILE: Pair = ("\u{f15b}", "📄");
const RUNNABLE: Pair = ("\u{f0e7}", "⚡");
const CODE: Pair = ("\u{f121}", "📜");
const TEXT: Pair = ("\u{f15c}", "📝");
const CONFIG: Pair = ("\u{f013}", "🔧");
const SHELL: Pair = ("\u{f489}", "🐚");
const GIT: Pair = ("\u{e702}", "🌿");
const KEY: Pair = ("\u{f084}", "🔑");
const LOCKED: Pair = ("\u{f023}", "🔒");
const ARCHIVE: Pair = ("\u{f1c6}", "📦");
const PACKAGE: Pair = ("\u{f187}", "📦");
const IMAGE: Pair = ("\u{f1c5}", "📷");
const AUDIO: Pair = ("\u{f1c7}", "🎵");
const VIDEO: Pair = ("\u{f1c8}", "🎬");
const PDF: Pair = ("\u{f1c1}", "📕");
const DOCUMENT: Pair = ("\u{f1c2}", "📄");
const SHEET: Pair = ("\u{f1c3}", "📊");
const SLIDES: Pair = ("\u{f1c4}", "📊");
const DATABASE: Pair = ("\u{f1c0}", "💾");
const BINARY: Pair = ("\u{f085}", "🔩");
const BUILD: Pair = ("\u{f0ad}", "🔨");
const BOOK: Pair = ("\u{f02d}", "📖");
const LICENSE: Pair = ("\u{f0e3}", "📜");
const DOCKER: Pair = ("\u{f308}", "🐳");
const NODE: Pair = ("\u{e718}", "📦");
const RUST: Pair = ("\u{e7a8}", "🦀");

/// The icon a file name asks for, or `None` when nothing about it is
/// recognised.
///
/// A whole name wins over a suffix — `Cargo.toml` is Rust's rather than
/// TOML's — and a long suffix wins over a short one, so `.tar.gz` is an
/// archive rather than whatever `.gz` alone would be.
fn glyphs(name: &str) -> Option<Pair> {
    let lower = name.to_lowercase();
    by_whole_name(&lower)
        .or_else(|| by_suffix(&lower))
        .or_else(|| by_extension(extension(&lower)))
}

/// Files that are known by their name rather than by what they end in. Most
/// of them have no extension at all, which is exactly why they are here.
fn by_whole_name(lower: &str) -> Option<Pair> {
    Some(match lower {
        "dockerfile"
        | "containerfile"
        | ".dockerignore"
        | "docker-compose.yml"
        | "docker-compose.yaml"
        | "compose.yml"
        | "compose.yaml" => DOCKER,
        "makefile" | "gnumakefile" | "cmakelists.txt" | "meson.build" | "justfile"
        | "build.gradle" | "pom.xml" | "configure" | "configure.ac" => BUILD,
        "cargo.toml" | "cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" => RUST,
        "package.json" | "package-lock.json" | "yarn.lock" | "tsconfig.json" => NODE,
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" | ".mailmap" => GIT,
        "license" | "licence" | "license.md" | "licence.md" | "license.txt" | "copying"
        | "copyright" | "notice" => LICENSE,
        "readme" | "readme.md" | "readme.txt" | "readme.rst" | "changelog" | "changelog.md"
        | "contributing.md" | "authors" => BOOK,
        // A file of secrets, whatever it is spelled like.
        ".env" | ".netrc" | ".pgpass" | "known_hosts" | "authorized_keys" | "id_rsa"
        | "id_rsa.pub" | "id_ed25519" | "id_ed25519.pub" => KEY,
        ".bashrc" | ".bash_profile" | ".bash_logout" | ".zshrc" | ".zprofile" | ".profile"
        | ".inputrc" | "config.fish" => SHELL,
        ".vimrc" | ".gvimrc" | ".editorconfig" | ".tmux.conf" | ".ssh_config" | "sshd_config"
        | "ssh_config" | "fstab" | "hosts" | "passwd" | "crontab" => CONFIG,
        _ => return None,
    })
}

/// Endings that are more than an extension: two dots, or a whole word at the
/// end of a name that has one.
fn by_suffix(lower: &str) -> Option<Pair> {
    const SUFFIXES: &[(&str, Pair)] = &[
        (".tar.gz", ARCHIVE),
        (".tar.bz2", ARCHIVE),
        (".tar.xz", ARCHIVE),
        (".tar.zst", ARCHIVE),
        (".lock", LOCKED),
        (".d.ts", CODE),
    ];
    SUFFIXES
        .iter()
        .find(|(suffix, _)| lower.ends_with(suffix))
        .map(|(_, pair)| *pair)
}

/// What comes after the last dot, or `""` for a name with no dot in it and
/// for a dotfile with nothing after its name — `.bashrc` is a name, not an
/// extension `rc`.
fn extension(lower: &str) -> &str {
    match lower.rfind('.') {
        Some(0) | None => "",
        Some(at) => &lower[at + 1..],
    }
}

fn by_extension(ext: &str) -> Option<Pair> {
    Some(match ext {
        // ---- languages, each with its own glyph where a font has one ------
        "rs" => RUST,
        "c" => ("\u{e61e}", "📜"),
        "h" | "hh" => ("\u{f0fd}", "📜"),
        "cpp" | "cxx" | "cc" | "hpp" | "hxx" => ("\u{e61d}", "📜"),
        "cs" => ("\u{e648}", "📜"),
        "py" | "pyw" | "pyi" => ("\u{e73c}", "🐍"),
        "js" | "mjs" | "cjs" => ("\u{e781}", "📜"),
        "ts" | "tsx" => ("\u{e628}", "📘"),
        "jsx" => ("\u{e7ba}", "📜"),
        "go" => ("\u{e627}", "🐹"),
        "rb" | "gemspec" => ("\u{e739}", "💎"),
        "java" | "class" | "jar" => ("\u{e738}", "☕"),
        "php" => ("\u{e73d}", "📜"),
        "swift" => ("\u{e755}", "📜"),
        "lua" => ("\u{e620}", "🌙"),
        "pl" | "pm" => ("\u{e769}", "📜"),
        "hs" | "lhs" => ("\u{e777}", "📜"),
        "ex" | "exs" => ("\u{e62d}", "💧"),
        "erl" | "hrl" => ("\u{e7b1}", "📜"),
        "scala" | "sbt" => ("\u{e737}", "📜"),
        "clj" | "cljs" | "cljc" => ("\u{e768}", "📜"),
        "dart" => ("\u{e798}", "📜"),
        "vim" => ("\u{e62b}", "📜"),
        "sh" | "bash" | "zsh" | "fish" | "ksh" | "ash" => SHELL,
        "sql" | "psql" => DATABASE,
        "asm" | "s" => CODE,

        // ---- markup, styles, data ----------------------------------------
        "html" | "htm" | "xhtml" => ("\u{e736}", "🌐"),
        "css" => ("\u{e749}", "🎨"),
        "scss" | "sass" | "less" | "styl" => ("\u{e603}", "🎨"),
        "json" | "jsonc" | "json5" | "ndjson" => ("\u{e60b}", "🧾"),
        "xml" | "xsl" | "xsd" | "plist" => CODE,
        "md" | "markdown" | "mdx" | "rst" | "adoc" | "org" => ("\u{f48a}", "📝"),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "properties" | "desktop" | "service" => {
            CONFIG
        }
        "csv" | "tsv" => SHEET,
        "txt" | "text" | "log" | "out" | "err" | "diff" | "patch" => TEXT,

        // ---- things that are not read as text ----------------------------
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff" | "avif"
        | "heic" | "xcf" | "psd" | "svg" => IMAGE,
        "mp3" | "wav" | "flac" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma" | "mid"
        | "midi" => AUDIO,
        "mp4" | "mkv" | "mov" | "avi" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" => VIDEO,
        "pdf" | "ps" | "epub" | "mobi" | "djvu" => PDF,
        "doc" | "docx" | "odt" | "rtf" | "pages" => DOCUMENT,
        "xls" | "xlsx" | "ods" | "numbers" => SHEET,
        // `.key` is left to the certificates below: on the machines sshman
        // talks to it is a private key far more often than a Keynote.
        "ppt" | "pptx" | "odp" => SLIDES,
        "ttf" | "otf" | "woff" | "woff2" | "eot" => ("\u{f031}", "🔤"),

        // ---- packed up, or already built ---------------------------------
        "tar" | "tgz" | "tbz" | "tbz2" | "txz" | "gz" | "bz2" | "xz" | "zst" | "lz4" | "lzma"
        | "zip" | "7z" | "rar" | "cab" | "ar" => ARCHIVE,
        "deb" | "rpm" | "apk" | "pkg" | "dmg" | "msi" | "iso" | "img" | "snap" | "flatpak"
        | "appimage" | "whl" | "egg" => PACKAGE,
        "so" | "dylib" | "dll" | "a" | "o" | "obj" | "lib" | "elf" | "ko" => BINARY,
        "exe" | "bin" | "com" | "app" | "wasm" => RUNNABLE,
        "db" | "sqlite" | "sqlite3" | "mdb" | "dump" => DATABASE,

        // ---- keys, certificates, and the things that hold them -----------
        "pem" | "key" | "pub" | "crt" | "cer" | "der" | "p12" | "pfx" | "gpg" | "asc" | "sig"
        | "kbx" | "keytab" => KEY,

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn file(name: &str) -> FileEntry {
        FileEntry {
            name: name.into(),
            kind: EntryKind::File,
            size: 0,
            mtime: 0,
            perms: "-rw-r--r--".into(),
            link_target: None,
            points_to_dir: false,
        }
    }

    #[test]
    fn nothing_is_drawn_until_a_set_is_chosen() {
        assert_eq!(Icons::default(), Icons::Off);
        assert_eq!(Icons::Off.of(&file("main.rs")), None);
        assert!(Icons::Nerd.of(&file("main.rs")).is_some());
    }

    #[test]
    fn a_word_we_do_not_know_means_no_icons() {
        // The same as an absent setting: a set we cannot draw is better left
        // undrawn than guessed at.
        assert_eq!(Icons::named(""), Icons::Off);
        assert_eq!(Icons::named("   "), Icons::Off);
        assert_eq!(Icons::named("something-from-a-later-version"), Icons::Off);
        assert_eq!(Icons::named("off"), Icons::Off);
        assert_eq!(Icons::named("Nerd"), Icons::Nerd, "however it is written");
        assert_eq!(Icons::named("on"), Icons::Nerd, "the obvious thing to type");
        assert_eq!(Icons::named(" emoji "), Icons::Emoji);
        assert_eq!(Icons::named("unicode"), Icons::Emoji);
    }

    #[test]
    fn every_set_says_what_it_is_and_answers_to_its_own_name() {
        for set in Icons::ALL {
            assert!(!set.describe().is_empty(), "{set:?}");
            assert_eq!(Icons::named(set.name()), set, "{set:?} round trips");
        }
    }

    #[test]
    fn stepping_walks_the_three_in_a_ring() {
        assert_eq!(Icons::Off.stepped(1), Icons::Nerd);
        assert_eq!(Icons::Nerd.stepped(1), Icons::Emoji);
        assert_eq!(Icons::Emoji.stepped(1), Icons::Off, "round again");
        assert_eq!(Icons::Off.stepped(-1), Icons::Emoji, "and the other way");
    }

    #[test]
    fn what_a_thing_is_comes_before_what_it_is_called() {
        // A directory named like a tarball is still a directory, and a
        // symlink is drawn as one whatever it points at.
        let mut dir = file("backup.tar.gz");
        dir.kind = EntryKind::Dir;
        assert_eq!(Icons::Nerd.of(&dir), Some(FOLDER.0));

        let mut link = file("notes.md");
        link.kind = EntryKind::Symlink;
        link.points_to_dir = true;
        assert_eq!(Icons::Nerd.of(&link), Some(LINK.0));
    }

    #[test]
    fn a_whole_name_wins_over_the_suffix_underneath_it() {
        assert_eq!(glyphs("Cargo.toml"), Some(RUST), "not merely TOML");
        assert_eq!(glyphs("cargo.toml"), Some(RUST), "however it is spelled");
        assert_eq!(glyphs("settings.toml"), Some(CONFIG));
        assert_eq!(glyphs("Dockerfile"), Some(DOCKER));
        assert_eq!(glyphs("README.md"), Some(BOOK));
        assert_eq!(
            glyphs("notes.md"),
            by_extension("md"),
            "any other one is not"
        );
    }

    #[test]
    fn a_long_suffix_wins_over_a_short_one() {
        assert_eq!(glyphs("notes.tar.gz"), Some(ARCHIVE));
        assert_eq!(glyphs("Cargo.lock"), Some(RUST), "and a name over both");
        assert_eq!(glyphs("yarn.lock"), Some(NODE));
        assert_eq!(glyphs("flake.lock"), Some(LOCKED));
    }

    #[test]
    fn a_dotfile_is_a_name_rather_than_an_extension() {
        assert_eq!(extension(".bashrc"), "");
        assert_eq!(extension("main.rs"), "rs");
        assert_eq!(extension("no-dot-here"), "");
        assert_eq!(extension("a.b.c"), "c");
        assert_eq!(glyphs(".bashrc"), Some(SHELL));
    }

    #[test]
    fn a_file_we_know_nothing_about_falls_back_to_what_it_can_do() {
        assert_eq!(Icons::Nerd.of(&file("whatever.qqq")), Some(FILE.0));
        let mut runnable = file("whatever.qqq");
        runnable.perms = "-rwxr-xr-x".into();
        assert_eq!(Icons::Nerd.of(&runnable), Some(RUNNABLE.0));
        // Something we do know is not overruled by the execute bit.
        let mut script = file("deploy.py");
        script.perms = "-rwxr-xr-x".into();
        assert_eq!(Icons::Nerd.of(&script), Some("\u{e73c}"));
    }

    #[test]
    fn a_set_draws_every_icon_the_same_width() {
        // The name column is the last one, so a row only has to line up with
        // the rows around it — but within one set every glyph must measure
        // the same, or the names go ragged. Nerd Font glyphs are one column
        // and emoji are two, and neither may be mixed with the other.
        let mut names: Vec<String> = vec![
            "a-directory".into(),
            "some-link".into(),
            "unknown.qqq".into(),
        ];
        names.extend(EVERY_EXTENSION.iter().map(|ext| format!("file.{ext}")));
        names.extend(EVERY_NAME.iter().map(|name| name.to_string()));

        for (set, want) in [(Icons::Nerd, 1), (Icons::Emoji, 2)] {
            for name in &names {
                let mut entry = file(name);
                entry.kind = match name.as_str() {
                    "a-directory" => EntryKind::Dir,
                    "some-link" => EntryKind::Symlink,
                    _ => EntryKind::File,
                };
                let icon = set.of(&entry).expect("a set that is on draws one");
                assert_eq!(
                    Span::raw(icon).width(),
                    want,
                    "{icon:?} for {name} is the wrong width for {set:?}"
                );
            }
        }
    }

    /// Every extension the table answers to, so the width test above covers
    /// the whole of it rather than the few a hand-written list remembered.
    const EVERY_EXTENSION: &[&str] = &[
        "rs", "c", "h", "cpp", "cs", "py", "js", "ts", "jsx", "go", "rb", "java", "php", "swift",
        "lua", "pl", "hs", "ex", "erl", "scala", "clj", "dart", "vim", "sh", "sql", "asm", "html",
        "css", "scss", "json", "xml", "svg", "md", "toml", "csv", "txt", "png", "mp3", "mp4",
        "pdf", "docx", "xlsx", "pptx", "ttf", "tar", "deb", "so", "exe", "db", "pem",
    ];

    /// And every whole name, for the same reason.
    const EVERY_NAME: &[&str] = &[
        "Dockerfile",
        "Makefile",
        "Cargo.toml",
        "package.json",
        ".gitignore",
        "LICENSE",
        "README.md",
        ".env",
        ".bashrc",
        ".vimrc",
        "notes.tar.gz",
        "flake.lock",
    ];
}
