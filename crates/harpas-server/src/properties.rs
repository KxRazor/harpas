//! Reading `server.properties`, in the Java properties format vanilla writes
//!
//! Only reading, and only the keys the server uses so far. Loading every vanilla key, writing the file with
//! its defaults and `harpas.toml` are a later task (`docs/operations.md`).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

/// The keys of a `server.properties` file and their values.
#[derive(Debug, Default)]
pub struct Properties(HashMap<String, String>);

impl Properties {
    /// Reads `path`; a missing file has no keys, so every setting takes vanilla's default.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            // Java writes properties in ISO 8859-1 and escapes the rest as `\uXXXX`; vanilla reads them as
            // UTF-8, so both are accepted.
            Ok(bytes) => Ok(Self::parse(&String::from_utf8_lossy(&bytes))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Parses the contents of a properties file.
    pub fn parse(text: &str) -> Self {
        let mut map = HashMap::new();
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            let mut logical = line.trim_start().to_owned();
            if logical.is_empty() || logical.starts_with('#') || logical.starts_with('!') {
                continue;
            }
            // A line ending in an odd number of backslashes continues on the next one.
            while logical.chars().rev().take_while(|&c| c == '\\').count() % 2 == 1 {
                logical.pop();
                match lines.next() {
                    Some(next) => logical.push_str(next.trim_start()),
                    None => break,
                }
            }
            let (key, value) = split_key(&logical);
            map.insert(unescape(key), unescape(value));
        }
        Self(map)
    }

    /// The value of `key`, if set.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// The value of `key` as a string, or `default`.
    pub fn string(&self, key: &str, default: &str) -> String {
        self.get(key).unwrap_or(default).to_owned()
    }

    /// The value of `key` parsed as `T`, or `default` when it is missing or invalid, as vanilla does.
    pub fn parsed<T: std::str::FromStr>(&self, key: &str, default: T) -> T {
        self.get(key)
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(default)
    }
}

/// Splits a line at the first unescaped `=`, `:` or whitespace, as Java's `Properties` does.
fn split_key(line: &str) -> (&str, &str) {
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '=' || c == ':' || c.is_whitespace() {
            let key = &line[..i];
            let rest = line[i..].trim_start();
            let rest = if !c.is_whitespace() || rest.starts_with(['=', ':']) {
                rest.strip_prefix(['=', ':']).unwrap_or(rest).trim_start()
            } else {
                rest
            };
            return (key, rest);
        }
    }
    (line, "")
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    let mut units = Vec::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            flush(&mut units, &mut out);
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                match u16::from_str_radix(&hex, 16) {
                    // Collected as UTF-16, so that escaped surrogate pairs become one character.
                    Ok(unit) if hex.len() == 4 => units.push(unit),
                    _ => {
                        flush(&mut units, &mut out);
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                flush(&mut units, &mut out);
                out.push(match other {
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    'f' => '\u{c}',
                    other => other,
                });
            }
            None => {}
        }
    }
    flush(&mut units, &mut out);
    out
}

fn flush(units: &mut Vec<u16>, out: &mut String) {
    out.extend(char::decode_utf16(units.drain(..)).map(|c| c.unwrap_or('\u{fffd}')));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_vanilla_writes() {
        let text = "#Minecraft server properties\n#Sat Nov 01 12:00:00 CET 2026\n\
                    motd=\\u00A7aHarpas \\u00F1and\\u00FA \\uD83E\\uDD80\n\
                    server-port=25566\nserver-ip=\nmax-players = 50\n\
                    enforce-secure-profile:false\nlevel-name  my\\ world\n";
        let properties = Properties::parse(text);
        assert_eq!(properties.get("motd"), Some("§aHarpas ñandú 🦀"));
        assert_eq!(properties.parsed("server-port", 25565u16), 25566);
        assert_eq!(properties.get("server-ip"), Some(""));
        assert_eq!(properties.parsed("max-players", 20u32), 50);
        assert!(!properties.parsed("enforce-secure-profile", true));
        assert_eq!(properties.get("level-name"), Some("my world"));
    }

    #[test]
    fn invalid_values_take_the_default() {
        let properties = Properties::parse("server-port=lots\nmulti=a\\\n  b\n");
        assert_eq!(properties.parsed("server-port", 25565u16), 25565);
        assert_eq!(properties.parsed("max-players", 20u32), 20);
        assert_eq!(properties.get("multi"), Some("ab"));
    }
}
