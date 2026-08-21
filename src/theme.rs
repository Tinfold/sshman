//! Colours, by what they mean rather than what they are.
//!
//! Every colour in the interface is one of the roles below, so a theme is a
//! table of twelve values and nothing else has to know which one is on.
//!
//! The tables themselves are not in this file. Each one is a small JSON file:
//! the seven sshman ships live in `themes/` and are built into the binary, so
//! there is nothing to install, and any file dropped in
//! `~/.config/sshman/themes/` is loaded beside them. A file taking a name
//! sshman already uses replaces it, which is how you rewrite one of ours
//! without forking anything.
//!
//! Only foregrounds are themed. sshman never paints a background of its own:
//! the terminal's shows through, which is what makes it sit properly inside
//! whatever you have already set up — and it is the only honest thing to do
//! next to a shell pane, where the program running in it paints its own.

use std::path::PathBuf;

use ratatui::style::Color;
use serde::Deserialize;

/// The twelve roles, and nothing else — a theme's name lives beside it in
/// [`Named`], so the colours stay small enough to copy on every span.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    /// Focused borders, titles, the marks that say "you are here".
    pub accent: Color,
    /// Unfocused borders, hints, anything deliberately in the background.
    pub dim: Color,
    /// Ordinary text you are meant to read: paths, file names, values.
    pub text: Color,
    /// Text that is there when you look for it: sizes, dates, labels.
    pub muted: Color,
    /// It worked.
    pub good: Color,
    /// Worth a second look: marks, warnings, a listing still loading.
    pub warn: Color,
    /// It did not work, or it is about to do something irreversible.
    pub bad: Color,
    /// Directories in a listing.
    pub dir: Color,
    /// Symlinks in a listing.
    pub link: Color,
    /// Files you could run.
    pub exec: Color,
    /// Badges that are telling you something rather than warning you: a
    /// container tab, a count of forwarded ports, what is on the clipboard.
    pub info: Color,
    /// Text drawn *on* a coloured chip, so it has to contrast with the
    /// colours above rather than with the terminal.
    pub on_accent: Color,
}

/// The theme to fall back on. It is the one colours file that is also written
/// here, because sshman has to be able to draw the screen that tells you your
/// themes could not be read. `themes/terminal.json` says the same thing, and
/// a test below holds the two together.
pub const FALLBACK: Theme = Theme {
    accent: Color::Cyan,
    dim: Color::DarkGray,
    text: Color::White,
    muted: Color::Gray,
    good: Color::Green,
    warn: Color::Yellow,
    bad: Color::Red,
    dir: Color::Blue,
    link: Color::Magenta,
    exec: Color::Green,
    info: Color::Blue,
    on_accent: Color::Black,
};

/// What [`FALLBACK`] is called, and so what a config file with nothing in it
/// is asking for.
pub const DEFAULT: &str = "terminal";

impl Default for Theme {
    fn default() -> Self {
        FALLBACK
    }
}

/// A theme and the name you ask for it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub theme: Theme,
    /// The line the file gives about itself, if it gives one.
    pub about: Option<String>,
}

/// The themes there are: the ones built in, then anything found on disk.
#[derive(Clone, Debug, Default)]
pub struct Themes {
    pub entries: Vec<Named>,
    /// Files that could not be used, in the words to show someone who is
    /// wondering where their theme went. A theme file with a typo in it is
    /// worth a complaint; silently having one fewer theme is not.
    pub problems: Vec<String>,
}

/// The files sshman ships. Built into the binary so a copied executable is
/// still a whole program, and readable in `themes/` so a new one is a matter
/// of copying the nearest.
const BUILT_IN: &[(&str, &str)] = &[
    ("terminal.json", include_str!("../themes/terminal.json")),
    ("catppuccin.json", include_str!("../themes/catppuccin.json")),
    ("monokai.json", include_str!("../themes/monokai.json")),
    ("gruvbox.json", include_str!("../themes/gruvbox.json")),
    ("mariana.json", include_str!("../themes/mariana.json")),
    ("afterglow.json", include_str!("../themes/afterglow.json")),
    ("darcula.json", include_str!("../themes/darcula.json")),
];

impl Themes {
    /// Everything sshman ships, in the order they are listed above.
    pub fn built_in() -> Self {
        let mut themes = Self::default();
        for (file, text) in BUILT_IN {
            themes.add(text, file);
        }
        themes
    }

    /// The built-in themes, plus every `.json` in the themes directory. A file
    /// naming a theme we already have replaces it where it stands, so `,`
    /// still steps through them in a stable order.
    pub fn load() -> Self {
        let mut themes = Self::built_in();
        if let Some(dir) = themes_dir() {
            themes.load_from(&dir);
        }
        themes
    }

    /// Read every `.json` in a directory into this set.
    fn load_from(&mut self, dir: &std::path::Path) {
        let Ok(read) = std::fs::read_dir(dir) else {
            // No themes directory is the ordinary case, not a problem.
            return;
        };
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .collect();
        // Read in a fixed order, so two files claiming one name settle the
        // same way every time rather than by whatever the directory says.
        files.sort();
        for path in files {
            // The file's own name: the directory is the same for all of them,
            // and is named where the count of themes is.
            let shown = match path.file_name() {
                Some(name) => name.to_string_lossy().into_owned(),
                None => path.display().to_string(),
            };
            match std::fs::read_to_string(&path) {
                Ok(text) => self.add(&text, &shown),
                Err(e) => self.problems.push(format!("{shown}: {e}")),
            }
        }
    }

    /// Take one file's worth of theme, or write down why not.
    fn add(&mut self, text: &str, from: &str) {
        let file: FileTheme = match serde_json::from_str::<FileTheme>(text) {
            Ok(file) => file,
            Err(e) => {
                // Serde's line and column are about the file we are quoting
                // back at someone who has it open, and crowd out the part
                // that says what is wrong with it.
                let said = e.to_string();
                let said = said.split(" at line ").next().unwrap_or(&said);
                self.problems.push(format!("{from}: {said}"));
                return;
            }
        };
        let name = file.name.trim().to_lowercase();
        if name.is_empty() {
            self.problems.push(format!("{from}: a theme needs a name"));
            return;
        }
        // Anything left out comes from the theme it is based on, so a file
        // that only wants to change the accent only has to say the accent.
        let base = match &file.base {
            Some(base) => match self.by_name(base) {
                Some(theme) => theme,
                None => {
                    self.problems
                        .push(format!("{from}: there is no theme called {base:?}"));
                    return;
                }
            },
            None => FALLBACK,
        };
        let named = Named {
            theme: file.resolve(base),
            about: file.about,
            name,
        };
        match self.entries.iter_mut().find(|e| e.name == named.name) {
            Some(existing) => *existing = named,
            None => self.entries.push(named),
        }
    }

    /// The theme of that name, or `None` for one we do not have.
    pub fn by_name(&self, name: &str) -> Option<Theme> {
        let wanted = name.trim().to_lowercase();
        self.entries
            .iter()
            .find(|e| e.name == wanted)
            .map(|e| e.theme)
    }

    /// The next one along, for a settings pane that cycles rather than asks
    /// you to type. `step` of -1 goes back. A name we do not have — a theme
    /// whose file has since been deleted — starts from the beginning.
    pub fn cycle(&self, name: &str, step: isize) -> Named {
        if self.entries.is_empty() {
            return Named {
                name: DEFAULT.into(),
                theme: FALLBACK,
                about: None,
            };
        }
        let wanted = name.trim().to_lowercase();
        let at = self
            .entries
            .iter()
            .position(|e| e.name == wanted)
            .map(|at| at as isize + step)
            .unwrap_or(0);
        let len = self.entries.len() as isize;
        self.entries[at.rem_euclid(len) as usize].clone()
    }
}

/// Where a theme of your own goes. Beside the settings and the saved servers,
/// so there is one directory to look in and one to back up.
pub fn themes_dir() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join("themes"))
}

/// A theme as its file writes it. Every colour is optional: what a file leaves
/// out comes from the theme it is based on.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTheme {
    name: String,
    /// A line about where these colours come from. Nothing reads it but a
    /// person, which is the point — JSON has nowhere else to put a comment.
    #[serde(default)]
    about: Option<String>,
    /// The theme to take anything left out from. Absent means the terminal's
    /// own colours.
    #[serde(default)]
    base: Option<String>,

    #[serde(default)]
    accent: Option<Colour>,
    #[serde(default)]
    dim: Option<Colour>,
    #[serde(default)]
    text: Option<Colour>,
    #[serde(default)]
    muted: Option<Colour>,
    #[serde(default)]
    good: Option<Colour>,
    #[serde(default)]
    warn: Option<Colour>,
    #[serde(default)]
    bad: Option<Colour>,
    #[serde(default)]
    dir: Option<Colour>,
    #[serde(default)]
    link: Option<Colour>,
    #[serde(default)]
    exec: Option<Colour>,
    #[serde(default)]
    info: Option<Colour>,
    #[serde(default)]
    on_accent: Option<Colour>,
}

impl FileTheme {
    fn resolve(&self, base: Theme) -> Theme {
        let or = |c: Option<Colour>, fallback: Color| c.map(|c| c.0).unwrap_or(fallback);
        Theme {
            accent: or(self.accent, base.accent),
            dim: or(self.dim, base.dim),
            text: or(self.text, base.text),
            muted: or(self.muted, base.muted),
            good: or(self.good, base.good),
            warn: or(self.warn, base.warn),
            bad: or(self.bad, base.bad),
            dir: or(self.dir, base.dir),
            link: or(self.link, base.link),
            exec: or(self.exec, base.exec),
            info: or(self.info, base.info),
            on_accent: or(self.on_accent, base.on_accent),
        }
    }
}

/// One colour, as a theme file writes it.
#[derive(Clone, Copy, Debug)]
struct Colour(Color);

impl<'de> Deserialize<'de> for Colour {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        parse(&text).map(Colour).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "{text:?} is not a colour (try #rrggbb, a name, or 0-255)"
            ))
        })
    }
}

/// `#cba6f7`, `#fff`, `cyan`, `bright-red`, or `137` for a slot in the
/// 256-colour cube.
fn parse(text: &str) -> Option<Color> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        // Three digits is the shorthand every stylesheet uses: each digit
        // stands for itself twice over, so `f` is `ff`.
        let (r, g, b) = match hex.len() {
            3 => {
                let digit = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).map(|v| v * 17);
                (digit(0).ok()?, digit(1).ok()?, digit(2).ok()?)
            }
            6 => {
                let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16);
                (byte(0).ok()?, byte(2).ok()?, byte(4).ok()?)
            }
            _ => return None,
        };
        return Some(Color::Rgb(r, g, b));
    }
    if let Ok(n) = text.parse::<u8>() {
        return Some(Color::Indexed(n));
    }
    // `dark gray`, `dark-grey` and `darkgray` are all the same colour, and
    // nobody should have to remember which spelling we picked.
    let name: String = text
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | ' '))
        .flat_map(char::to_lowercase)
        .collect();
    Some(match name.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" | "lightgray" | "lightgrey" => Color::Gray,
        "darkgray" | "darkgrey" | "brightblack" => Color::DarkGray,
        "lightred" | "brightred" => Color::LightRed,
        "lightgreen" | "brightgreen" => Color::LightGreen,
        "lightyellow" | "brightyellow" => Color::LightYellow,
        "lightblue" | "brightblue" => Color::LightBlue,
        "lightmagenta" | "brightmagenta" => Color::LightMagenta,
        "lightcyan" | "brightcyan" => Color::LightCyan,
        "white" | "brightwhite" => Color::White,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_themes_we_ship_all_load() {
        let themes = Themes::built_in();
        assert!(
            themes.problems.is_empty(),
            "a theme sshman ships does not load: {:?}",
            themes.problems
        );
        assert_eq!(themes.entries.len(), BUILT_IN.len());
        for name in ["terminal", "catppuccin", "monokai", "gruvbox"] {
            assert!(themes.by_name(name).is_some(), "{name} is missing");
        }
    }

    #[test]
    fn the_fallback_and_its_file_say_the_same_thing() {
        // The one palette written twice: here, so there is always something to
        // draw with, and in `themes/terminal.json`, so it can be read and
        // copied like any other. They must not drift apart.
        assert_eq!(Themes::built_in().by_name(DEFAULT), Some(FALLBACK));
    }

    #[test]
    fn themes_are_found_by_name_however_it_is_typed() {
        let themes = Themes::built_in();
        assert_eq!(themes.by_name("  Monokai "), themes.by_name("monokai"));
        assert!(themes.by_name("monokai").is_some());
    }

    #[test]
    fn a_theme_we_do_not_have_is_not_invented() {
        // The caller falls back to the default rather than getting a
        // half-filled palette.
        let themes = Themes::built_in();
        assert_eq!(themes.by_name("nothing-of-the-sort"), None);
        assert_eq!(themes.by_name(""), None);
    }

    #[test]
    fn cycling_goes_round_in_both_directions() {
        let themes = Themes::built_in();
        let first = themes.entries[0].clone();
        let last = themes.entries[themes.entries.len() - 1].clone();
        assert_eq!(
            themes.cycle(&first.name, -1),
            last,
            "back from the first wraps to the end"
        );
        assert_eq!(
            themes.cycle(&last.name, 1),
            first,
            "and on from the last comes home"
        );

        // All the way round lands where it started.
        let mut at = first.name.clone();
        for _ in 0..themes.entries.len() {
            at = themes.cycle(&at, 1).name;
        }
        assert_eq!(at, first.name);
    }

    #[test]
    fn cycling_from_a_theme_that_is_no_longer_there_still_gets_somewhere() {
        // Its file was deleted while the name stayed in the config.
        let themes = Themes::built_in();
        assert_eq!(themes.cycle("deleted", 1), themes.entries[0]);
    }

    #[test]
    fn cycling_with_no_themes_at_all_still_has_something_to_draw_with() {
        let none = Themes::default();
        assert_eq!(none.cycle("anything", 1).theme, FALLBACK);
    }

    #[test]
    fn themes_are_read_from_the_directory_they_are_kept_in() {
        let dir = std::env::temp_dir().join(format!("sshman-themes-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("midnight.json"),
            r##"{"name": "midnight", "accent": "#010203"}"##,
        )
        .unwrap();
        // A file that is not a theme file is not read at all.
        std::fs::write(dir.join("notes.txt"), "not json, not looked at").unwrap();

        let mut themes = Themes::built_in();
        let before = themes.entries.len();
        themes.load_from(&dir);

        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        assert_eq!(themes.entries.len(), before + 1);
        assert_eq!(
            themes.by_name("midnight").unwrap().accent,
            Color::Rgb(1, 2, 3)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_themes_directory_is_not_a_complaint() {
        let mut themes = Themes::built_in();
        themes.load_from(std::path::Path::new("/nowhere/at/all/sshman"));
        assert!(themes.problems.is_empty(), "nobody has to make one");
    }

    #[test]
    fn a_file_only_has_to_say_what_it_changes() {
        let mut themes = Themes::built_in();
        themes.add(
            r##"{"name": "mine", "base": "monokai", "accent": "#ff0000"}"##,
            "mine.json",
        );
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        let mine = themes.by_name("mine").expect("it was added");
        let monokai = themes.by_name("monokai").unwrap();
        assert_eq!(mine.accent, Color::Rgb(0xff, 0, 0));
        assert_eq!(mine.text, monokai.text, "the rest comes from the base");
    }

    #[test]
    fn a_file_with_no_base_starts_from_the_terminals_own_colours() {
        let mut themes = Themes::default();
        themes.add(
            r##"{"name": "sparse", "accent": "#ff0000"}"##,
            "sparse.json",
        );
        let sparse = themes.by_name("sparse").unwrap();
        assert_eq!(sparse.dim, FALLBACK.dim);
    }

    #[test]
    fn a_file_can_replace_a_theme_we_ship() {
        let mut themes = Themes::built_in();
        let before = themes.entries.len();
        themes.add(
            r##"{"name": "Gruvbox", "accent": "#010203"}"##,
            "gruvbox.json",
        );
        assert_eq!(
            themes.entries.len(),
            before,
            "it took the name, not another slot"
        );
        assert_eq!(
            themes.by_name("gruvbox").unwrap().accent,
            Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn a_file_we_cannot_use_is_complained_about_rather_than_ignored() {
        let mut themes = Themes::built_in();
        let before = themes.entries.len();
        for (text, what) in [
            (
                r##"{"name": "bad", "accent": "puce"}"##,
                "an unknown colour",
            ),
            (
                r##"{"name": "bad", "acccent": "red"}"##,
                "a misspelled role",
            ),
            (r##"{"name": "  ", "accent": "red"}"##, "no name"),
            (
                r##"{"name": "bad", "base": "nope"}"##,
                "a base we do not have",
            ),
            ("{oh dear", "not being JSON at all"),
        ] {
            let problems = themes.problems.len();
            themes.add(text, "theirs.json");
            assert_eq!(
                themes.problems.len(),
                problems + 1,
                "{what} passed without a word"
            );
        }
        assert_eq!(themes.entries.len(), before, "and none of them was taken");
    }

    #[test]
    fn colours_are_written_the_way_people_write_them() {
        assert_eq!(parse("#cba6f7"), Some(Color::Rgb(0xcb, 0xa6, 0xf7)));
        assert_eq!(parse("#fff"), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(parse("  #000  "), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(parse("cyan"), Some(Color::Cyan));
        assert_eq!(parse("DarkGray"), Some(Color::DarkGray));
        assert_eq!(parse("dark grey"), Some(Color::DarkGray));
        assert_eq!(parse("bright-red"), Some(Color::LightRed));
        assert_eq!(parse("137"), Some(Color::Indexed(137)));

        assert_eq!(parse("#12345"), None, "a length that is neither");
        assert_eq!(parse("#gggggg"), None, "not hex");
        assert_eq!(parse("#ααα"), None, "not even ascii");
        assert_eq!(parse("256"), None, "off the end of the cube");
        assert_eq!(parse("puce"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn every_theme_is_complete_and_named_for_the_config_file() {
        for named in &Themes::built_in().entries {
            assert!(!named.name.is_empty());
            assert_eq!(
                named.name.to_lowercase(),
                named.name,
                "names are matched in lower case: {}",
                named.name
            );
        }
    }

    #[test]
    fn nothing_that_has_to_be_told_apart_is_the_same_colour() {
        for named in &Themes::built_in().entries {
            let theme = named.theme;
            // A focused border beside an unfocused one, text on a chip
            // against the chip, a directory against a plain file.
            for (a, b, what) in [
                (theme.accent, theme.dim, "accent and dim"),
                (theme.accent, theme.on_accent, "a chip and its text"),
                (theme.text, theme.dim, "text and dim"),
                (theme.dir, theme.text, "directories and files"),
                (theme.good, theme.bad, "good news and bad"),
            ] {
                assert_ne!(a, b, "{} in {}", what, named.name);
            }
        }
    }
}
