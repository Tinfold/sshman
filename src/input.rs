//! A minimal single-line text field with cursor movement and word deletion.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Default, Clone, Debug)]
pub struct TextInput {
    pub value: String,
    /// Cursor position measured in characters, not bytes.
    pub cursor: usize,
    /// Render as `•••` — used for passwords.
    pub masked: bool,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            masked: false,
        }
    }

    pub fn masked() -> Self {
        Self {
            masked: true,
            ..Default::default()
        }
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.value.chars().count();
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    /// Byte offset of the character cursor, for splicing.
    fn byte_at(&self, char_idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_idx)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    pub fn display(&self) -> String {
        if self.masked {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }

    /// Insert pasted text at the cursor.
    ///
    /// These are single-line fields, so only the first line of a multi-line
    /// paste is taken — joining the lines would silently invent a value the
    /// user never copied, such as a host name made of two. Control characters
    /// are dropped so a stray escape sequence cannot scramble the display.
    ///
    /// Returns true when part of the paste was discarded, so the caller can
    /// say so rather than letting it vanish quietly.
    pub fn insert_str(&mut self, text: &str) -> bool {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        let had_more = lines.any(|line| !line.trim().is_empty());

        let cleaned: String = first.chars().filter(|c| !c.is_control()).collect();
        let lost_chars = cleaned.chars().count() != first.chars().count();

        if !cleaned.is_empty() {
            let at = self.byte_at(self.cursor);
            self.value.insert_str(at, &cleaned);
            self.cursor += cleaned.chars().count();
        }
        had_more || lost_chars
    }

    /// Returns true when the key was consumed.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if ctrl => match c {
                'a' => self.cursor = 0,
                'e' => self.cursor = self.value.chars().count(),
                'u' => {
                    let b = self.byte_at(self.cursor);
                    self.value.replace_range(..b, "");
                    self.cursor = 0;
                }
                'k' => {
                    let b = self.byte_at(self.cursor);
                    self.value.truncate(b);
                }
                'w' => self.delete_word(),
                _ => return false,
            },
            KeyCode::Char(c) if alt => match c {
                'b' => self.cursor = self.prev_word(),
                'f' => self.cursor = self.next_word(),
                _ => return false,
            },
            KeyCode::Char(c) => {
                let b = self.byte_at(self.cursor);
                self.value.insert(b, c);
                self.cursor += 1;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let start = self.byte_at(self.cursor - 1);
                    let end = self.byte_at(self.cursor);
                    self.value.replace_range(start..end, "");
                    self.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.value.chars().count() {
                    let start = self.byte_at(self.cursor);
                    let end = self.byte_at(self.cursor + 1);
                    self.value.replace_range(start..end, "");
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.value.chars().count());
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.chars().count(),
            _ => return false,
        }
        true
    }

    fn prev_word(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut i = self.cursor;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    fn next_word(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut i = self.cursor;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        i
    }

    fn delete_word(&mut self) {
        let start = self.prev_word();
        let sb = self.byte_at(start);
        let cb = self.byte_at(self.cursor);
        self.value.replace_range(sb..cb, "");
        self.cursor = start;
    }
}
