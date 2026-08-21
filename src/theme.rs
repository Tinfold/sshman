//! Colours, by what they mean rather than what they are.
//!
//! Every colour in the interface is one of the roles below, so a theme is a
//! table of eleven values and nothing else has to know which one is on.
//!
//! Only foregrounds are themed. sshman never paints a background of its own:
//! the terminal's shows through, which is what makes it sit properly inside
//! whatever you have already set up — and it is the only honest thing to do
//! next to a shell pane, where the program running in it paints its own.

use ratatui::style::Color;

/// Only the name is written down; the colours are here, so a theme can be
/// improved without rewriting anybody's config file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    /// What the theme is called, and what goes in the config file.
    pub name: &'static str,
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

/// The colours sshman has always used: the terminal's own sixteen, so it
/// matches whatever the rest of your terminal is set to.
pub const TERMINAL: Theme = Theme {
    name: "terminal",
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

/// Catppuccin Mocha.
pub const CATPPUCCIN: Theme = Theme {
    name: "catppuccin",
    accent: Color::Rgb(0xcb, 0xa6, 0xf7),    // mauve
    dim: Color::Rgb(0x6c, 0x70, 0x86),       // overlay0
    text: Color::Rgb(0xcd, 0xd6, 0xf4),      // text
    muted: Color::Rgb(0xa6, 0xad, 0xc8),     // subtext0
    good: Color::Rgb(0xa6, 0xe3, 0xa1),      // green
    warn: Color::Rgb(0xf9, 0xe2, 0xaf),      // yellow
    bad: Color::Rgb(0xf3, 0x8b, 0xa8),       // red
    dir: Color::Rgb(0x89, 0xb4, 0xfa),       // blue
    link: Color::Rgb(0xf5, 0xc2, 0xe7),      // pink
    exec: Color::Rgb(0xa6, 0xe3, 0xa1),      // green
    info: Color::Rgb(0x94, 0xe2, 0xd5),      // teal
    on_accent: Color::Rgb(0x1e, 0x1e, 0x2e), // base
};

/// Monokai, as everyone remembers it.
pub const MONOKAI: Theme = Theme {
    name: "monokai",
    accent: Color::Rgb(0x66, 0xd9, 0xef), // cyan
    dim: Color::Rgb(0x75, 0x71, 0x5e),    // comment
    text: Color::Rgb(0xf8, 0xf8, 0xf2),   // foreground
    muted: Color::Rgb(0xa5, 0x9f, 0x85),
    good: Color::Rgb(0xa6, 0xe2, 0x2e),      // green
    warn: Color::Rgb(0xe6, 0xdb, 0x74),      // yellow
    bad: Color::Rgb(0xf9, 0x26, 0x72),       // pink
    dir: Color::Rgb(0xae, 0x81, 0xff),       // purple
    link: Color::Rgb(0xfd, 0x97, 0x1f),      // orange
    exec: Color::Rgb(0xa6, 0xe2, 0x2e),      // green
    info: Color::Rgb(0xae, 0x81, 0xff),      // purple
    on_accent: Color::Rgb(0x27, 0x28, 0x22), // background
};

/// Gruvbox dark, from morhetz/gruvbox.
pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    accent: Color::Rgb(0xfa, 0xbd, 0x2f),    // bright yellow
    dim: Color::Rgb(0x92, 0x83, 0x74),       // gray
    text: Color::Rgb(0xeb, 0xdb, 0xb2),      // light1
    muted: Color::Rgb(0xbd, 0xae, 0x93),     // light3
    good: Color::Rgb(0xb8, 0xbb, 0x26),      // bright green
    warn: Color::Rgb(0xfe, 0x80, 0x19),      // bright orange
    bad: Color::Rgb(0xfb, 0x49, 0x34),       // bright red
    dir: Color::Rgb(0x83, 0xa5, 0x98),       // bright blue
    link: Color::Rgb(0xd3, 0x86, 0x9b),      // bright purple
    exec: Color::Rgb(0xb8, 0xbb, 0x26),      // bright green
    info: Color::Rgb(0x8e, 0xc0, 0x7c),      // bright aqua
    on_accent: Color::Rgb(0x28, 0x28, 0x28), // dark0
};

/// Mariana, from the colour scheme Sublime Text ships. Its own values are in
/// HSL; these are the same colours in the form ratatui wants.
pub const MARIANA: Theme = Theme {
    name: "mariana",
    accent: Color::Rgb(0x66, 0x99, 0xcc),    // blue
    dim: Color::Rgb(0x64, 0x73, 0x82),       // blue4
    text: Color::Rgb(0xd8, 0xde, 0xe9),      // white3
    muted: Color::Rgb(0xa6, 0xac, 0xb9),     // blue6
    good: Color::Rgb(0x99, 0xc7, 0x94),      // green
    warn: Color::Rgb(0xfa, 0xc7, 0x61),      // orange3
    bad: Color::Rgb(0xec, 0x5f, 0x66),       // red
    dir: Color::Rgb(0x5f, 0xb4, 0xb4),       // blue5, the teal one
    link: Color::Rgb(0xc6, 0x95, 0xc6),      // pink
    exec: Color::Rgb(0x99, 0xc7, 0x94),      // green
    info: Color::Rgb(0xf9, 0xae, 0x58),      // orange
    on_accent: Color::Rgb(0x30, 0x38, 0x41), // blue3, the background
};

/// Afterglow, from YabataDesign's theme for Sublime Text.
pub const AFTERGLOW: Theme = Theme {
    name: "afterglow",
    accent: Color::Rgb(0xcc, 0x78, 0x32), // keyword orange
    dim: Color::Rgb(0x79, 0x79, 0x79),    // comment
    text: Color::Rgb(0xd6, 0xd6, 0xd6),   // foreground
    muted: Color::Rgb(0xcc, 0xcc, 0xcc),
    good: Color::Rgb(0xb4, 0xc9, 0x73),      // string green
    warn: Color::Rgb(0xe5, 0xb5, 0x67),      // yellow
    bad: Color::Rgb(0xc4, 0x58, 0x37),       // red
    dir: Color::Rgb(0x6c, 0x99, 0xbb),       // blue
    link: Color::Rgb(0xa1, 0x61, 0x7a),      // mauve
    exec: Color::Rgb(0xb4, 0xc9, 0x73),      // string green
    info: Color::Rgb(0xd0, 0xd0, 0xff),      // lavender
    on_accent: Color::Rgb(0x2e, 0x2e, 0x2e), // background
};

/// Darcula, from the scheme JetBrains ships with IntelliJ.
pub const DARCULA: Theme = Theme {
    name: "darcula",
    accent: Color::Rgb(0xcc, 0x78, 0x32),    // keyword orange
    dim: Color::Rgb(0x60, 0x63, 0x66),       // the UI grey
    text: Color::Rgb(0xa9, 0xb7, 0xc6),      // identifiers
    muted: Color::Rgb(0x80, 0x80, 0x80),     // comment
    good: Color::Rgb(0x6a, 0x87, 0x59),      // string green
    warn: Color::Rgb(0xff, 0xc6, 0x6d),      // function yellow
    bad: Color::Rgb(0xbc, 0x3f, 0x3c),       // error
    dir: Color::Rgb(0x68, 0x97, 0xbb),       // number blue
    link: Color::Rgb(0x98, 0x76, 0xaa),      // field purple
    exec: Color::Rgb(0x6a, 0x87, 0x59),      // string green
    info: Color::Rgb(0x28, 0x7b, 0xde),      // hyperlink blue
    on_accent: Color::Rgb(0x2b, 0x2b, 0x2b), // editor background
};

impl Theme {
    pub const ALL: &'static [Theme] = &[
        TERMINAL, CATPPUCCIN, MONOKAI, GRUVBOX, MARIANA, AFTERGLOW, DARCULA,
    ];

    /// The theme of that name, or `None` for one we do not have.
    pub fn by_name(name: &str) -> Option<Theme> {
        let wanted = name.trim().to_lowercase();
        Self::ALL.iter().find(|t| t.name == wanted).copied()
    }

    /// The next one along, for a settings pane that cycles rather than asks
    /// you to type. `step` of -1 goes back.
    pub fn cycle(self, step: isize) -> Theme {
        let at = Self::ALL
            .iter()
            .position(|t| t.name == self.name)
            .unwrap_or(0) as isize;
        let len = Self::ALL.len() as isize;
        Self::ALL[(at + step).rem_euclid(len) as usize]
    }
}

impl Default for Theme {
    fn default() -> Self {
        TERMINAL
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn themes_are_found_by_name_however_it_is_typed() {
        assert_eq!(Theme::by_name("catppuccin"), Some(CATPPUCCIN));
        assert_eq!(Theme::by_name("  Monokai "), Some(MONOKAI));
        assert_eq!(Theme::by_name("terminal"), Some(TERMINAL));
    }

    #[test]
    fn a_theme_we_do_not_have_is_not_invented() {
        // The caller falls back to the default rather than getting a
        // half-filled palette.
        assert_eq!(Theme::by_name("dracula"), None);
        assert_eq!(Theme::by_name(""), None);
    }

    #[test]
    fn cycling_goes_round_in_both_directions() {
        let first = Theme::ALL[0];
        let last = Theme::ALL[Theme::ALL.len() - 1];
        assert_eq!(
            first.cycle(-1),
            last,
            "back from the first wraps to the end"
        );
        assert_eq!(last.cycle(1), first, "and on from the last comes home");

        // All the way round lands where it started.
        let mut theme = first;
        for _ in 0..Theme::ALL.len() {
            theme = theme.cycle(1);
        }
        assert_eq!(theme, first);
    }

    #[test]
    fn every_theme_is_complete_and_named_for_the_config_file() {
        for theme in Theme::ALL {
            assert!(!theme.name.is_empty());
            assert_eq!(
                theme.name.to_lowercase(),
                theme.name,
                "names are matched in lower case: {}",
                theme.name
            );
            assert_eq!(
                Theme::by_name(theme.name),
                Some(*theme),
                "{} cannot be asked for by name",
                theme.name
            );
        }
    }

    #[test]
    fn nothing_that_has_to_be_told_apart_is_the_same_colour() {
        for theme in Theme::ALL {
            // A focused border beside an unfocused one, text on a chip
            // against the chip, a directory against a plain file.
            for (a, b, what) in [
                (theme.accent, theme.dim, "accent and dim"),
                (theme.accent, theme.on_accent, "a chip and its text"),
                (theme.text, theme.dim, "text and dim"),
                (theme.dir, theme.text, "directories and files"),
                (theme.good, theme.bad, "good news and bad"),
            ] {
                assert_ne!(a, b, "{} in {}", what, theme.name);
            }
        }
    }
}
