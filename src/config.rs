//! Settings that outlive a session.
//!
//! Kept beside the saved servers and workspaces, in the same place and the
//! same format, so there is one directory to look in and one to back up.
//!
//! Everything here is optional. An absent file, an unreadable one, or one
//! written by a future version with fields we do not know about all mean the
//! same thing: fall back to what sshman would have done anyway.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::Theme;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// The program `e` opens files with. Empty or absent defers to `$VISUAL`,
    /// then `$EDITOR`, then `vi`, which is where it came from before there
    /// was anywhere to write it down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,

    /// Which set of colours to draw in, by name. An absent or unknown one
    /// means the terminal's own, which is what sshman looked like before
    /// there were any others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

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

    /// The colours to draw in. A name we do not recognise — a theme from a
    /// later version, or a typo — falls back rather than failing.
    pub fn theme(&self) -> Theme {
        self.theme
            .as_deref()
            .and_then(Theme::by_name)
            .unwrap_or_default()
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
    Theme,
}

/// How a setting is changed: by typing a value, or by stepping through the
/// ones there are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Choice,
}

impl Setting {
    pub const ALL: &'static [Setting] = &[Setting::Editor, Setting::Theme];

    pub fn label(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::Theme => "Theme",
        }
    }

    /// One line saying what this setting is for.
    pub fn blurb(self) -> &'static str {
        match self {
            Self::Editor => "the program e opens files with",
            Self::Theme => "the colours to draw in",
        }
    }

    pub fn kind(self) -> Kind {
        match self {
            Self::Editor => Kind::Text,
            Self::Theme => Kind::Choice,
        }
    }
}

impl Config {
    /// What this setting is set to, whether it was set here or inherited.
    pub fn value(&self, setting: Setting) -> String {
        match setting {
            Setting::Editor => self.editor(),
            Setting::Theme => self.theme().name.to_string(),
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
            // A name we cannot use is not a setting, however it got there.
            Setting::Theme => self
                .theme
                .as_deref()
                .is_some_and(|n| Theme::by_name(n).is_some()),
        }
    }
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

fn config_path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs::home_dir()?.join(".config"),
    };
    Some(base.join("sshman").join("config.json"))
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
    fn a_theme_is_remembered_by_name() {
        let mut config = Config::default();
        assert_eq!(config.theme(), crate::theme::TERMINAL, "the default");

        config.theme = Some("monokai".into());
        assert_eq!(config.theme(), crate::theme::MONOKAI);
        assert!(config.is_set(Setting::Theme));
        assert_eq!(config.value(Setting::Theme), "monokai");
    }

    #[test]
    fn a_theme_we_cannot_draw_falls_back_rather_than_failing() {
        // From a later version, or a typo in a hand-edited file.
        let config = Config {
            theme: Some("dracula".into()),
            ..Config::default()
        };
        assert_eq!(config.theme(), crate::theme::TERMINAL);
        assert!(
            !config.is_set(Setting::Theme),
            "and the pane does not claim it as a setting of yours"
        );
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
