//! A small `~/.ssh/config` reader, so typing a host alias here does the same
//! thing it does with the `ssh` command.
//!
//! Supports the handful of keywords that affect how we connect. Following
//! OpenSSH, the first value obtained for a keyword wins.

use std::path::PathBuf;

#[derive(Debug, Default, Clone)]
pub struct HostConfig {
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
}

pub fn lookup(alias: &str) -> HostConfig {
    let path = match dirs::home_dir() {
        Some(h) => h.join(".ssh").join("config"),
        None => return HostConfig::default(),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return HostConfig::default(),
    };
    parse(&text, alias)
}

fn parse(text: &str, alias: &str) -> HostConfig {
    let mut cfg = HostConfig::default();
    let mut applies = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Keywords may be separated by whitespace or '='.
        let (key, value) = match line.split_once(['=', ' ', '\t']) {
            Some((k, v)) => (
                k.trim().to_lowercase(),
                v.trim_start_matches(['=', ' ']).trim(),
            ),
            None => continue,
        };

        if key == "host" {
            applies = value
                .split_whitespace()
                .any(|pat| matches_pattern(pat, alias));
            continue;
        }
        // `Match` blocks use criteria we do not evaluate; treat them as a
        // block boundary rather than silently applying the wrong settings.
        if key == "match" {
            applies = false;
            continue;
        }
        if !applies {
            continue;
        }

        match key.as_str() {
            "hostname" if cfg.hostname.is_none() => cfg.hostname = Some(value.to_string()),
            "user" if cfg.user.is_none() => cfg.user = Some(value.to_string()),
            "port" if cfg.port.is_none() => cfg.port = value.parse().ok(),
            "identityfile" if cfg.identity_file.is_none() => {
                cfg.identity_file = Some(crate::local::expand(value));
            }
            _ => {}
        }
    }
    cfg
}

/// Glob matching for `Host` patterns: `*` matches any run, `?` any single
/// character. A leading `!` negation is treated as "no match" for our purposes.
fn matches_pattern(pattern: &str, name: &str) -> bool {
    if let Some(rest) = pattern.strip_prefix('!') {
        return !matches_pattern(rest, name);
    }
    glob(pattern.as_bytes(), name.as_bytes())
}

fn glob(pat: &[u8], s: &[u8]) -> bool {
    match pat.first() {
        None => s.is_empty(),
        Some(b'*') => {
            // Try consuming zero or more characters with the rest of the pattern.
            (0..=s.len()).any(|i| glob(&pat[1..], &s[i..]))
        }
        Some(b'?') => !s.is_empty() && glob(&pat[1..], &s[1..]),
        Some(c) => !s.is_empty() && s[0] == *c && glob(&pat[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_up_matching_block() {
        let text = "\
Host web*
    HostName 10.0.0.5
    User deploy
    Port 2222

Host other
    HostName nope.example
";
        let cfg = parse(text, "web1");
        assert_eq!(cfg.hostname.as_deref(), Some("10.0.0.5"));
        assert_eq!(cfg.user.as_deref(), Some("deploy"));
        assert_eq!(cfg.port, Some(2222));
    }

    #[test]
    fn ignores_non_matching_block() {
        let text = "Host web\n  HostName 10.0.0.5\n";
        assert!(parse(text, "db").hostname.is_none());
    }

    #[test]
    fn first_value_wins() {
        let text = "Host a\n  User first\nHost a\n  User second\n";
        assert_eq!(parse(text, "a").user.as_deref(), Some("first"));
    }

    #[test]
    fn handles_equals_separator() {
        let text = "Host=a\n  HostName=1.2.3.4\n";
        assert_eq!(parse(text, "a").hostname.as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn glob_wildcards() {
        assert!(matches_pattern("*.example.com", "www.example.com"));
        assert!(matches_pattern("web?", "web1"));
        assert!(!matches_pattern("web?", "web10"));
    }
}
