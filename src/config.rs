//! Settings that outlive a session.
//!
//! Kept beside the saved servers and workspaces, in the same place and the
//! same format, so there is one directory to look in and one to back up.
//!
//! Everything here is optional. An absent file, an unreadable one, or one
//! written by a future version with fields we do not know about all mean the
//! same thing: fall back to what sshman would have done anyway.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// The program `e` opens files with. Empty or absent defers to `$VISUAL`,
    /// then `$EDITOR`, then `vi`, which is where it came from before there
    /// was anywhere to write it down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,

    /// Which set of colours to draw in, by name — one of the files in
    /// `themes/`, or one of your own. An absent name means the terminal's
    /// own, which is what sshman looked like before there were any others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    /// The keystrokes that make the editor in an editor pane open a file,
    /// with `{file}` standing in for the path, as it is. Absent means the
    /// ones sshman knows for the editor you use, and an empty string means it
    /// knows none: the pane is treated as a shell prompt and the editor run
    /// as a command, quoted for the shell.
    ///
    /// `\\e` is escape, `\\r` a return, and `\\C-x` a control character, so
    /// `"\\e:e {file}\\r"` is what vim wants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_open: Option<String>,

    /// Whether to paint the background a theme names, or leave the terminal's
    /// own showing through. Absent means paint it.
    ///
    /// Painting one is ordinary cell painting inside the alternate screen, the
    /// same thing a full-screen editor does; nothing about the terminal itself
    /// is changed, and leaving sshman puts it back either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,

    /// Whether a shell pane's colours come from the theme or from the
    /// terminal's own palette. Absent means the theme's.
    ///
    /// It is the same idea as the background: for those panes sshman is the
    /// terminal emulator, and this is the colour scheme it is set to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_colours: Option<String>,

    /// Whether a file list keeps up with changes sshman had no hand in —
    /// files appearing, going away or being renamed under it. Absent means it
    /// does; `off` means it shows what it read when it read it, and nothing
    /// changes until you ask.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<String>,

    /// Keys of your own, by what they do: `"quit": ["Q"]`.
    ///
    /// Only what you have changed is written here; everything else keeps the
    /// scheme sshman ships, so this file says what you decided rather than
    /// repeating fifty things you did not.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keys: BTreeMap<String, Vec<String>>,

    /// The file this came from, and where it goes back to. Not part of the
    /// file itself, and `None` when there is nowhere to write — which is also
    /// how the tests keep their hands off the real one.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let mut config: Self = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        config.path = path;
        config
    }

    /// Settings kept in `path` rather than the usual place.
    #[cfg(test)]
    pub fn at(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Self::default()
        }
    }

    /// The theme asked for, as written. Whether there is a file of that name
    /// is [`Themes`](crate::theme::Themes)' business, not the config file's:
    /// a theme you have set is a theme you have set, even on a machine where
    /// its file has not been copied yet.
    pub fn theme_name(&self) -> Option<&str> {
        self.theme
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// The editor to use, given what the environment says.
    ///
    /// A setting written here wins over `$EDITOR`: someone who has told
    /// sshman which editor to use means it, and `$EDITOR` is set on nearly
    /// every machine, so the other way round would make the setting useless.
    pub fn editor(&self) -> String {
        self.editor
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .map(String::from)
            .unwrap_or_else(default_editor)
    }

    /// Whether to paint the background a theme names. Anything but the word
    /// `terminal` means paint it, so a file written by a later version that
    /// knows more answers than this one still does something sensible.
    /// Whether to colour a shell pane's text from the theme.
    pub fn theme_the_shell(&self) -> bool {
        !matches!(
            self.shell_colours.as_deref().map(str::trim),
            Some("terminal") | Some("none")
        )
    }

    /// Whether the file lists follow their directories. Anything but a word
    /// meaning no leaves them following, on the same principle as the
    /// background: a file from a later version that knows more answers than
    /// this one still does something sensible.
    pub fn watching(&self) -> bool {
        !matches!(
            self.watch.as_deref().map(str::trim),
            Some("off") | Some("no") | Some("manual")
        )
    }

    pub fn paint_background(&self) -> bool {
        !matches!(
            self.background.as_deref().map(str::trim),
            Some("terminal") | Some("none")
        )
    }

    /// The keystrokes that open a file in `program`, ready to have `{file}`
    /// put in them. Empty when there is nothing known about that editor, in
    /// which case the pane is a shell prompt and the editor is run as a
    /// command instead.
    pub fn editor_open(&self, program: &str) -> String {
        match self.editor_open.as_deref() {
            Some(spec) => unescape(spec),
            None => unescape(default_open(program)),
        }
    }

    /// Write the settings back. The caller is told what went wrong, since a
    /// setting that silently failed to save is worse than one that never
    /// claimed to.
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(std::io::Error::other("no config directory to write to"));
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // Written beside then renamed, so an interrupted write cannot leave a
        // half-file where the settings were.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

/// One thing the settings pane can change.
///
/// Adding another is a variant and three arms: what it is called, what it
/// does, and where its value comes from. The pane and its keys need no
/// changes at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    Editor,
    EditorOpen,
    Theme,
    Background,
    ShellColours,
    Watch,
    Keys,
}

/// How a setting is changed: by typing a value, or by stepping through the
/// ones there are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Choice,
}

impl Setting {
    pub const ALL: &'static [Setting] = &[
        Setting::Editor,
        Setting::EditorOpen,
        Setting::Theme,
        Setting::Background,
        Setting::ShellColours,
        Setting::Watch,
        Setting::Keys,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::EditorOpen => "Opens with",
            Self::Theme => "Theme",
            Self::Background => "Background",
            Self::ShellColours => "Shell colours",
            Self::Watch => "Keeping up",
            Self::Keys => "Keys",
        }
    }

    /// One line saying what this setting is for.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Editor => "the program e opens files with",
            Self::EditorOpen => "the keys that open {file} in an editor pane",
            Self::Theme => "the colours to draw in",
            Self::Background => "the theme's own, or whatever the terminal is set to",
            Self::ShellColours => "what a shell pane's own output is coloured from",
            Self::Watch => "whether a list keeps up with changes from outside",
            Self::Keys => "which key asks for what",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::Editor | Self::EditorOpen => Kind::Text,
            Self::Theme | Self::Background | Self::ShellColours | Self::Watch | Self::Keys => {
                Kind::Choice
            }
        }
    }
}

impl Config {
    /// What this setting is set to, whether it was set here or inherited.
    pub fn value(&self, setting: Setting) -> String {
        match setting {
            Setting::Editor => self.editor(),
            Setting::EditorOpen => match self.editor_open.as_deref() {
                Some(spec) => spec.to_string(),
                None => match default_open(&self.editor()) {
                    "" => "(run at the prompt)".into(),
                    spec => spec.to_string(),
                },
            },
            Setting::Theme => self.theme_name().unwrap_or(theme::DEFAULT).to_string(),
            Setting::Background => match self.paint_background() {
                true => "the theme's own".into(),
                false => "the terminal's".into(),
            },
            Setting::ShellColours => match self.theme_the_shell() {
                true => "the theme's own".into(),
                false => "the terminal's".into(),
            },
            Setting::Watch => match self.watching() {
                true => "lists follow their directories".into(),
                false => "only when you ask".into(),
            },
            Setting::Keys => match self.keys.len() {
                0 => "the ones sshman ships".into(),
                1 => "1 key of your own".into(),
                n => format!("{n} keys of your own"),
            },
        }
    }

    /// Where that value came from, so the pane can say whether changing it
    /// here would be changing anything.
    pub fn origin(&self, setting: Setting) -> &'static str {
        match setting {
            Setting::Theme => match self.is_set(Setting::Theme) {
                true => "set here",
                false => "the default",
            },
            Setting::EditorOpen => match self.editor_open.is_some() {
                true => "set here",
                false => "for your editor",
            },
            Setting::Background => match self.background.is_some() {
                true => "set here",
                false => "the default",
            },
            Setting::ShellColours => match self.shell_colours.is_some() {
                true => "set here",
                false => "the default",
            },
            Setting::Watch => match self.watch.is_some() {
                true => "set here",
                false => "the default",
            },
            Setting::Keys => match self.keys.is_empty() {
                true => "the default",
                false => "set here",
            },
            Setting::Editor => {
                if self.editor.as_deref().is_some_and(|e| !e.trim().is_empty()) {
                    "set here"
                } else if env_set("VISUAL") {
                    "from $VISUAL"
                } else if env_set("EDITOR") {
                    "from $EDITOR"
                } else {
                    "the fallback"
                }
            }
        }
    }

    /// Has this setting been given a value of its own, rather than inheriting
    /// one? Only a setting of your own is worth offering to clear.
    pub fn is_set(&self, setting: Setting) -> bool {
        match setting {
            Setting::Editor => self.editor.is_some(),
            Setting::EditorOpen => self.editor_open.is_some(),
            Setting::Background => self.background.is_some(),
            Setting::ShellColours => self.shell_colours.is_some(),
            Setting::Watch => self.watch.is_some(),
            Setting::Keys => !self.keys.is_empty(),
            Setting::Theme => self.theme_name().is_some(),
        }
    }
}

/// The keystrokes sshman knows for the editors people use, by the program's
/// own name. An editor we know nothing about gets an empty spec, which means
/// "run it at the prompt" rather than a guess that would type nonsense into
/// whatever is on screen.
fn default_open(editor: &str) -> &'static str {
    // `code -w`, `/usr/bin/nvim`, `emacsclient -nw`: what matters is the
    // program, not the path it was found at or the flags after it.
    let program = editor
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    match program {
        // Escape first: the editor may well be in insert mode.
        "vi" | "vim" | "nvim" | "view" | "kak" => "\\e:e {file}\\r",
        "hx" | "helix" => "\\e:o {file}\\r",
        "emacs" | "emacsclient" => "\\C-x\\C-f{file}\\r",
        _ => "",
    }
}

/// Turn the escapes a config file can write into the bytes they stand for.
/// Anything else after a backslash is left as it was typed, so a Windows-ish
/// path in a spec does not quietly lose characters.
fn unescape(spec: &str) -> String {
    let mut out = String::with_capacity(spec.len());
    let mut chars = spec.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('e') => out.push('\x1b'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            // `\C-x` is the control character x, the way a terminal writes it.
            Some('C') => {
                let mut rest = chars.clone();
                if rest.next() == Some('-')
                    && let Some(key) = rest.next()
                    && key.is_ascii_alphabetic()
                {
                    out.push(((key.to_ascii_uppercase() as u8) & 0x1f) as char);
                    chars = rest;
                } else {
                    out.push('C');
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn env_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.trim().is_empty())
}

/// What the environment offers when nothing has been configured.
pub fn default_editor() -> String {
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "vi".into())
}

/// Where everything sshman remembers between sessions lives: this file, the
/// saved servers, the workspaces, and any themes of your own.
pub fn config_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("sshman"))
}

fn config_path() -> Option<PathBuf> {
    Some(config_dir()?.join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_editor_wins_over_the_environment() {
        let config = Config {
            editor: Some("hx".into()),
            ..Config::default()
        };
        assert_eq!(config.editor(), "hx");
    }

    #[test]
    fn nothing_configured_falls_back_to_the_environment() {
        // Whatever this machine has, the fallback is what the app used before
        // there was a setting at all.
        assert_eq!(Config::default().editor(), default_editor());
    }

    #[test]
    fn a_blank_setting_is_not_a_setting() {
        for blank in ["", "   "] {
            let config = Config {
                editor: Some(blank.into()),
                ..Config::default()
            };
            assert_eq!(
                config.editor(),
                default_editor(),
                "{blank:?} must not be run as a program"
            );
        }
    }

    #[test]
    fn a_setting_is_taken_as_typed_apart_from_the_spaces_around_it() {
        let config = Config {
            editor: Some("  code -w  ".into()),
            ..Config::default()
        };
        assert_eq!(config.editor(), "code -w");
    }

    #[test]
    fn settings_round_trip_through_json() {
        let config = Config {
            editor: Some("nvim".into()),
            ..Config::default()
        };
        let text = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&text).unwrap(), config);
        assert!(
            !text.contains("path"),
            "where it lives is not part of what it says: {text}"
        );
    }

    #[test]
    fn an_editor_pane_gets_the_keys_its_editor_wants() {
        let config = Config {
            editor: Some("nvim".into()),
            ..Config::default()
        };
        assert_eq!(config.editor_open("nvim"), "\x1b:e {file}\r");
        // The path it was found at and the flags after it are not the editor.
        assert_eq!(config.editor_open("/usr/bin/vim -p"), "\x1b:e {file}\r");
        assert_eq!(config.editor_open("hx"), "\x1b:o {file}\r");
        assert_eq!(config.editor_open("emacs"), "\x18\x06{file}\r");
    }

    #[test]
    fn an_editor_we_know_nothing_about_is_not_guessed_at() {
        // An empty spec means the pane is a shell prompt, and the editor is
        // run as a command — rather than typing nonsense into whatever is on
        // screen.
        let config = Config::default();
        assert_eq!(config.editor_open("some-editor"), "");
        assert_eq!(config.editor_open(""), "");
        assert_eq!(
            config.value(Setting::EditorOpen),
            "(run at the prompt)",
            "and the settings pane says so"
        );
    }

    #[test]
    fn keys_of_your_own_win_over_the_ones_we_know() {
        let config = Config {
            editor_open: Some("\\e:edit {file}\\r".into()),
            ..Config::default()
        };
        assert_eq!(config.editor_open("vim"), "\x1b:edit {file}\r");
        assert!(config.is_set(Setting::EditorOpen));
    }

    #[test]
    fn escapes_are_read_the_way_a_terminal_writes_them() {
        assert_eq!(unescape("plain"), "plain");
        assert_eq!(unescape("\\e"), "\x1b");
        assert_eq!(unescape("a\\rb\\nc\\td"), "a\rb\nc\td");
        assert_eq!(unescape("\\C-x\\C-f"), "\x18\x06");
        assert_eq!(unescape("\\C-A"), "\x01", "however it is capitalised");
        assert_eq!(unescape("C:\\\\dir"), "C:\\dir", "a doubled one is one");
        // An escape we do not know is left as it was typed, rather than
        // quietly losing the backslash.
        assert_eq!(unescape("\\q"), "\\q");
        assert_eq!(unescape("\\Cx"), "Cx", "not a control character");
        assert_eq!(unescape("ends with one \\"), "ends with one \\");
    }

    #[test]
    fn a_theme_is_remembered_by_name() {
        let mut config = Config::default();
        assert_eq!(config.theme_name(), None, "nothing set means the default");
        assert_eq!(config.value(Setting::Theme), theme::DEFAULT);
        assert!(!config.is_set(Setting::Theme));

        config.theme = Some("monokai".into());
        assert_eq!(config.theme_name(), Some("monokai"));
        assert!(config.is_set(Setting::Theme));
        assert_eq!(config.value(Setting::Theme), "monokai");
    }

    #[test]
    fn a_theme_whose_file_is_not_here_is_still_the_one_you_asked_for() {
        // Set on a machine where the file lives, read on one where it does
        // not. Finding nothing to draw with is handled where the themes are;
        // the setting itself is yours, and offering to clear it is the point.
        let config = Config {
            theme: Some("something-of-my-own".into()),
            ..Config::default()
        };
        assert!(config.is_set(Setting::Theme));
        assert_eq!(config.value(Setting::Theme), "something-of-my-own");
    }

    #[test]
    fn the_pane_can_say_where_a_value_came_from() {
        let mut config = Config::default();
        // Nothing set here: the value is inherited, and there is nothing of
        // your own to clear.
        assert!(!config.is_set(Setting::Editor));
        assert_ne!(config.origin(Setting::Editor), "set here");

        config.editor = Some("hx".into());
        assert_eq!(config.value(Setting::Editor), "hx");
        assert_eq!(config.origin(Setting::Editor), "set here");
        assert!(config.is_set(Setting::Editor));
    }

    #[test]
    fn every_setting_has_something_to_show_for_itself() {
        let config = Config::default();
        for &setting in Setting::ALL {
            assert!(!setting.label().is_empty());
            assert!(!setting.blurb().is_empty());
            assert!(!config.value(setting).is_empty(), "{setting:?}");
            assert!(!config.origin(setting).is_empty(), "{setting:?}");
        }
    }

    #[test]
    fn saving_writes_where_it_was_told_and_nowhere_else() {
        let dir = std::env::temp_dir().join(format!("sshman-cfg-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("config.json");

        let mut config = Config::at(path.clone());
        config.editor = Some("hx".into());
        config.save().expect("the directory is made on the way");

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<Config>(&text).unwrap().editor(),
            "hx"
        );
        // Nothing is left beside it from the write-then-rename.
        assert!(!path.with_extension("json.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn settings_with_nowhere_to_live_say_so_rather_than_pretending() {
        let config = Config::default();
        assert!(config.save().is_err());
    }

    #[test]
    fn a_file_from_another_version_still_loads() {
        // Fields we do not know are ignored, and ones we expect may be absent.
        let text = r#"{"editor": "kak", "colour_scheme": "midnight", "tabs": 4}"#;
        let config: Config = serde_json::from_str(text).unwrap();
        assert_eq!(config.editor(), "kak");

        let empty: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.editor, None);
    }
}
