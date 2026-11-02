//! `server.properties`, in the Java properties format vanilla reads and writes
//!
//! Like vanilla, the server completes the file on every start: missing and invalid keys take their default and
//! the file is written back with every key in alphabetical order. `HARPAS_*` environment variables override
//! single keys without changing the file (`docs/operations.md`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

/// How the value of a key is checked and written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `true` in any case; anything else is `false`, as Java's `Boolean.valueOf`.
    Bool,
    Int,
    Text,
    /// A name or a number; written back as the name.
    Difficulty,
    /// A name or a number; written back as the name.
    GameMode,
    RegionCompression,
    /// 40 letters and digits, generated when missing or invalid.
    Secret,
}

/// Every key of a Java Edition 26.1 `server.properties` file, with vanilla's default.
const KEYS: &[(&str, &str, Kind)] = &[
    ("accepts-transfers", "false", Kind::Bool),
    ("allow-flight", "false", Kind::Bool),
    ("broadcast-console-to-ops", "true", Kind::Bool),
    ("broadcast-rcon-to-ops", "true", Kind::Bool),
    ("bug-report-link", "", Kind::Text),
    ("difficulty", "easy", Kind::Difficulty),
    ("enable-code-of-conduct", "false", Kind::Bool),
    ("enable-jmx-monitoring", "false", Kind::Bool),
    ("enable-query", "false", Kind::Bool),
    ("enable-rcon", "false", Kind::Bool),
    ("enable-status", "true", Kind::Bool),
    ("enforce-secure-profile", "true", Kind::Bool),
    ("enforce-whitelist", "false", Kind::Bool),
    ("entity-broadcast-range-percentage", "100", Kind::Int),
    ("force-gamemode", "false", Kind::Bool),
    ("function-permission-level", "2", Kind::Int),
    ("gamemode", "survival", Kind::GameMode),
    ("generate-structures", "true", Kind::Bool),
    ("generator-settings", "{}", Kind::Text),
    ("hardcore", "false", Kind::Bool),
    ("hide-online-players", "false", Kind::Bool),
    ("initial-disabled-packs", "", Kind::Text),
    ("initial-enabled-packs", "vanilla", Kind::Text),
    ("level-name", "world", Kind::Text),
    ("level-seed", "", Kind::Text),
    ("level-type", "minecraft:normal", Kind::Text),
    ("log-ips", "true", Kind::Bool),
    ("management-server-enabled", "false", Kind::Bool),
    ("management-server-host", "localhost", Kind::Text),
    ("management-server-port", "0", Kind::Int),
    ("management-server-secret", "", Kind::Secret),
    ("management-server-tls-enabled", "true", Kind::Bool),
    ("management-server-tls-keystore", "", Kind::Text),
    ("management-server-tls-keystore-password", "", Kind::Text),
    ("max-chained-neighbor-updates", "1000000", Kind::Int),
    ("max-players", "20", Kind::Int),
    ("max-tick-time", "60000", Kind::Int),
    ("max-world-size", "29999984", Kind::Int),
    ("motd", "A Minecraft Server", Kind::Text),
    ("network-compression-threshold", "256", Kind::Int),
    ("online-mode", "true", Kind::Bool),
    ("op-permission-level", "4", Kind::Int),
    ("pause-when-empty-seconds", "60", Kind::Int),
    ("player-idle-timeout", "0", Kind::Int),
    ("prevent-proxy-connections", "false", Kind::Bool),
    ("query.port", "25565", Kind::Int),
    ("rate-limit", "0", Kind::Int),
    ("rcon.password", "", Kind::Text),
    ("rcon.port", "25575", Kind::Int),
    (
        "region-file-compression",
        "deflate",
        Kind::RegionCompression,
    ),
    ("require-resource-pack", "false", Kind::Bool),
    ("resource-pack", "", Kind::Text),
    ("resource-pack-id", "", Kind::Text),
    ("resource-pack-prompt", "", Kind::Text),
    ("resource-pack-sha1", "", Kind::Text),
    ("server-ip", "", Kind::Text),
    ("server-port", "25565", Kind::Int),
    ("simulation-distance", "10", Kind::Int),
    ("spawn-protection", "16", Kind::Int),
    ("status-heartbeat-interval", "0", Kind::Int),
    ("sync-chunk-writes", "true", Kind::Bool),
    ("text-filtering-config", "", Kind::Text),
    ("text-filtering-version", "0", Kind::Int),
    ("use-native-transport", "true", Kind::Bool),
    ("view-distance", "10", Kind::Int),
    ("white-list", "false", Kind::Bool),
];

/// Whether `key` is a vanilla `server.properties` key.
pub fn is_vanilla_key(key: &str) -> bool {
    KEYS.iter().any(|&(name, _, _)| name == key)
}

/// The keys of a `server.properties` file and their values, with environment overrides on top.
#[derive(Debug, Default)]
pub struct Properties {
    values: HashMap<String, String>,
    overrides: HashMap<String, String>,
}

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
        Self {
            values: map,
            overrides: HashMap::new(),
        }
    }

    /// Gives every missing or invalid vanilla key its default, and writes known values in vanilla's form.
    /// Keys vanilla does not know are kept, as vanilla keeps them.
    pub fn complete(&mut self) {
        for &(key, default, kind) in KEYS {
            let value = match self.values.get(key) {
                Some(value) => normalize(kind, value).unwrap_or_else(|| fallback(kind, default)),
                None => fallback(kind, default),
            };
            self.values.insert(key.to_owned(), value);
        }
    }

    /// Writes the file as vanilla does: a header, then every key in alphabetical order, in UTF-8.
    pub fn store(&self, path: &Path, timestamp: &str) -> io::Result<()> {
        fs::write(path, self.to_file(timestamp))
    }

    fn to_file(&self, timestamp: &str) -> String {
        let newline = if cfg!(windows) { "\r\n" } else { "\n" };
        let mut keys: Vec<_> = self.values.keys().collect();
        keys.sort();
        let mut out = format!("#Minecraft server properties{newline}#{timestamp}{newline}");
        for key in keys {
            let _ = write!(
                out,
                "{}={}{newline}",
                escape(key, true),
                escape(&self.values[key], false)
            );
        }
        out
    }

    /// Applies `HARPAS_*` overrides: `max-players` is `HARPAS_MAX_PLAYERS` and `rcon.port` is
    /// `HARPAS_RCON_PORT`. Overrides of vanilla keys are checked like the file; an invalid one is returned and
    /// not applied. Variables that name no key are left for `harpas.toml`.
    pub fn apply_env(&mut self, vars: impl IntoIterator<Item = (String, String)>) -> Vec<String> {
        let mut rejected = Vec::new();
        for (name, value) in vars {
            let Some(suffix) = name.strip_prefix("HARPAS_") else {
                continue;
            };
            let key = KEYS
                .iter()
                .map(|&(key, _, _)| key)
                .chain(self.values.keys().map(String::as_str))
                .find(|key| env_name(key) == suffix);
            let Some(key) = key.map(str::to_owned) else {
                continue;
            };
            let value = match KEYS.iter().find(|&&(k, _, _)| k == key) {
                Some(&(_, _, kind)) => match normalize(kind, &value) {
                    Some(value) => value,
                    None => {
                        rejected.push(name);
                        continue;
                    }
                },
                None => value,
            };
            self.overrides.insert(key, value);
        }
        rejected
    }

    /// The value of `key`, if set.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.overrides
            .get(key)
            .or_else(|| self.values.get(key))
            .map(String::as_str)
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

/// The environment variable name of `key`, without the `HARPAS_` prefix.
fn env_name(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            '-' | '.' => '_',
            c => c.to_ascii_uppercase(),
        })
        .collect()
}

/// A value in the form vanilla writes back, or `None` if vanilla would reject it.
fn normalize(kind: Kind, value: &str) -> Option<String> {
    let named = |names: &[&str]| {
        let lower = value.trim().to_ascii_lowercase();
        let index = match lower.parse::<usize>() {
            Ok(index) => index,
            Err(_) => names.iter().position(|&name| name == lower)?,
        };
        names.get(index).map(|&name| name.to_owned())
    };
    match kind {
        Kind::Bool => Some(value.eq_ignore_ascii_case("true").to_string()),
        Kind::Int => value.trim().parse::<i64>().ok().map(|v| v.to_string()),
        Kind::Text => Some(value.to_owned()),
        Kind::Difficulty => named(&["peaceful", "easy", "normal", "hard"]),
        Kind::GameMode => named(&["survival", "creative", "adventure", "spectator"]),
        Kind::RegionCompression => {
            matches!(value, "deflate" | "lz4" | "none").then(|| value.to_owned())
        }
        Kind::Secret => (value.len() == 40 && value.bytes().all(|b| b.is_ascii_alphanumeric()))
            .then(|| value.to_owned()),
    }
}

/// The value a missing or invalid key takes.
fn fallback(kind: Kind, default: &str) -> String {
    if kind == Kind::Secret {
        generate_secret()
    } else {
        default.to_owned()
    }
}

/// 40 random letters and digits, as vanilla generates for `management-server-secret`.
fn generate_secret() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut secret = String::with_capacity(40);
    let mut bytes = [0; 64];
    while secret.len() < 40 {
        getrandom::fill(&mut bytes).expect("the system random number generator failed");
        // Bytes past the largest multiple of 62 are skipped, so that every character is equally likely.
        for &b in bytes.iter().filter(|&&b| b < 248) {
            if secret.len() < 40 {
                secret.push(ALPHABET[usize::from(b % 62)] as char);
            }
        }
    }
    secret
}

/// Escapes a key or value as Java's `Properties.store` does with a writer, which leaves non-ASCII text as is.
fn escape(text: &str, key: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.chars().enumerate() {
        match c {
            ' ' if key || i == 0 => out.push_str("\\ "),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '=' | ':' | '#' | '!' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
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
    fn completes_and_writes_the_file_as_vanilla_does() {
        let mut properties = Properties::parse(
            "difficulty=2\ngamemode=Creative\nhardcore=yes\nmax-players=lots\n\
             motd=\\ Caf\\u00E9 #1\npvp=false\nregion-file-compression=zip\n",
        );
        properties.complete();
        assert_eq!(properties.get("difficulty"), Some("normal"));
        assert_eq!(properties.get("gamemode"), Some("creative"));
        assert_eq!(properties.get("hardcore"), Some("false"));
        assert_eq!(properties.get("max-players"), Some("20"));
        assert_eq!(properties.get("region-file-compression"), Some("deflate"));
        assert_eq!(properties.get("pvp"), Some("false"));
        let secret = properties
            .get("management-server-secret")
            .unwrap()
            .to_owned();
        assert!(normalize(Kind::Secret, &secret).is_some());

        let file = properties.to_file("Mon Nov 02 12:00:00 CET 2026");
        let newline = if cfg!(windows) { "\r\n" } else { "\n" };
        let lines: Vec<_> = file.split(newline).collect();
        assert_eq!(lines[0], "#Minecraft server properties");
        assert_eq!(lines[1], "#Mon Nov 02 12:00:00 CET 2026");
        assert_eq!(lines[2], "accepts-transfers=false");
        assert!(lines.contains(&"level-type=minecraft\\:normal"));
        assert!(lines.contains(&"motd=\\ Café \\#1"));
        let keys: Vec<_> = lines[2..lines.len() - 1]
            .iter()
            .map(|l| l.split('=').next().unwrap())
            .collect();
        assert_eq!(keys.len(), KEYS.len() + 1);
        assert!(keys.is_sorted());

        // What was written reads back the same, and a second start keeps the generated secret.
        let mut again = Properties::parse(&file);
        again.complete();
        assert_eq!(again.values, properties.values);
    }

    #[test]
    fn environment_overrides_keys_without_changing_the_file() {
        let mut properties = Properties::parse("max-players=5\ncustom-key=a\n");
        properties.complete();
        let rejected = properties.apply_env([
            ("HARPAS_MAX_PLAYERS".to_owned(), "50".to_owned()),
            ("HARPAS_RCON_PORT".to_owned(), "1234".to_owned()),
            ("HARPAS_SERVER_PORT".to_owned(), "high".to_owned()),
            ("HARPAS_CUSTOM_KEY".to_owned(), "b".to_owned()),
            ("HARPAS_EULA".to_owned(), "true".to_owned()),
            ("MAX_PLAYERS".to_owned(), "1".to_owned()),
        ]);
        assert_eq!(rejected, ["HARPAS_SERVER_PORT"]);
        assert_eq!(properties.parsed("max-players", 20u32), 50);
        assert_eq!(properties.get("rcon.port"), Some("1234"));
        assert_eq!(properties.get("server-port"), Some("25565"));
        assert_eq!(properties.get("custom-key"), Some("b"));
        assert!(properties.to_file("").contains("max-players=5"));
    }

    #[test]
    fn invalid_values_take_the_default() {
        let properties = Properties::parse("server-port=lots\nmulti=a\\\n  b\n");
        assert_eq!(properties.parsed("server-port", 25565u16), 25565);
        assert_eq!(properties.parsed("max-players", 20u32), 20);
        assert_eq!(properties.get("multi"), Some("ab"));
    }
}
