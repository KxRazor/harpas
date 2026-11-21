//! The whitelist, player and IP bans and operators, from vanilla's `whitelist.json`, `banned-players.json`,
//! `banned-ips.json` and `ops.json` in the server folder, checked when a player logs in as vanilla's
//! `PlayerList.canPlayerLogin` checks them (`docs/operations.md`).

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use jiff::civil::DateTime;
use jiff::tz::{Offset, TimeZone};
use jiff::{Span, Timestamp, ZonedDateTime};
use log::warn;
use serde_json::{Map, Value};

/// What a player who logs in gets, before the player limit is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not banned and whitelisted, or the whitelist is off.
    Allow {
        /// An operator with `bypassesPlayerLimit`.
        bypasses_player_limit: bool,
    },
    /// Login Disconnect with this JSON text component.
    Refuse(String),
    /// The ban that matched has expired. Vanilla notices only after finding the ban, fails to read it and closes
    /// the connection without a message; the expired bans of that list are gone for the next login.
    Fail,
}

/// The four lists, as vanilla's `DedicatedPlayerList` keeps them.
pub struct AccessLists {
    /// `white-list` in `server.properties`.
    white_list: bool,
    bans: StoredList<(Profile, Ban)>,
    ip_bans: StoredList<Ban>,
    ops: StoredList<(Profile, Operator)>,
    whitelist: StoredList<Profile>,
    /// The time zone of the dates in the files and in ban messages, the system's as in vanilla.
    zone: TimeZone,
}

impl AccessLists {
    /// Lists that refuse nobody, without files.
    #[cfg(test)]
    pub fn none() -> Self {
        Self::new(Path::new(""), false, TimeZone::UTC)
    }

    fn new(dir: &Path, white_list: bool, zone: TimeZone) -> Self {
        Self {
            white_list,
            bans: StoredList::new(dir.join("banned-players.json")),
            ip_bans: StoredList::new(dir.join("banned-ips.json")),
            ops: StoredList::new(dir.join("ops.json")),
            whitelist: StoredList::new(dir.join("whitelist.json")),
            zone,
        }
    }

    /// Loads the lists from `dir` and writes them back in vanilla's order: the bans and operators always,
    /// the whitelist only if it has no file yet. Unlike vanilla, a file that cannot be read is left as it is
    /// instead of being overwritten with what could be read of it.
    pub fn load(dir: &Path, white_list: bool) -> Self {
        Self::load_at(dir, white_list, TimeZone::system(), Timestamp::now())
    }

    fn load_at(dir: &Path, white_list: bool, zone: TimeZone, now: Timestamp) -> Self {
        let mut lists = Self::new(dir, white_list, zone);
        let zone = &lists.zone;
        if lists.bans.load(now, "user banlist") {
            lists.bans.save(zone, "user banlist");
        }
        if lists.ip_bans.load(now, "ip banlist") {
            lists.ip_bans.save(zone, "ip banlist");
        }
        let ops = lists.ops.load(now, "operators list");
        let whitelist = lists.whitelist.load(now, "white-list");
        if ops {
            lists.ops.save(zone, "operators list");
        }
        if whitelist && !lists.whitelist.path.exists() {
            lists.whitelist.save(zone, "white-list");
        }
        lists
    }

    /// Checks a player who logs in from `ip`, in vanilla's order: player bans, the whitelist (which operators
    /// skip), IP bans. The player limit, checked last, is the connection's.
    pub fn check(&mut self, uuid: u128, ip: IpAddr, now: Timestamp) -> Verdict {
        let key = uuid_key(uuid);
        if self.bans.entries.contains_key(&key) {
            self.bans.remove_expired(now);
            return match self.bans.entries.get(&key) {
                Some((_, ban)) => Verdict::Refuse(self.ban_text("banned", ban)),
                None => Verdict::Fail,
            };
        }
        let op = self.ops.entries.get(&key);
        if self.white_list && op.is_none() && !self.whitelist.entries.contains_key(&key) {
            return Verdict::Refuse(
                r#"{"translate":"multiplayer.disconnect.not_whitelisted"}"#.into(),
            );
        }
        let bypasses_player_limit = op.is_some_and(|(_, op)| op.bypasses_player_limit);
        let ip = ip_key(ip);
        if self.ip_bans.entries.contains_key(&ip) {
            self.ip_bans.remove_expired(now);
            return match self.ip_bans.entries.get(&ip) {
                Some(ban) => Verdict::Refuse(self.ban_text("banned_ip", ban)),
                None => Verdict::Fail,
            };
        }
        Verdict::Allow {
            bypasses_player_limit,
        }
    }

    /// Vanilla's ban message, `multiplayer.disconnect.<kind>.reason` with the expiry date if there is one.
    fn ban_text(&self, kind: &str, ban: &Ban) -> String {
        let reason = match &ban.reason {
            Some(reason) => json_string(reason),
            None => r#"{"translate":"multiplayer.disconnect.banned.reason.default"}"#.into(),
        };
        let mut text =
            format!(r#"{{"translate":"multiplayer.disconnect.{kind}.reason","with":[{reason}]"#);
        if let Some(expires) = ban.expires {
            let zoned = expires.to_zoned_datetime(self.zone.clone());
            // `yyyy-MM-dd 'at' HH:mm:ss z` in `Locale.ROOT`, whose zone names are UTC, GMT or GMT±hh:mm.
            let date = format!(
                "{} {}",
                zoned.strftime("%Y-%m-%d at %H:%M:%S"),
                zone_name(&zoned)
            );
            let _ = write!(
                text,
                r#","extra":[{{"translate":"multiplayer.disconnect.{kind}.expiration","with":[{}]}}]"#,
                json_string(&date)
            );
        }
        text.push('}');
        text
    }
}

/// A player in a list: vanilla's `NameAndId`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    uuid: u128,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ban {
    created: Timestamp,
    source: String,
    /// None for bans without an end, saved as `forever`.
    expires: Option<Timestamp>,
    reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Operator {
    level: i32,
    bypasses_player_limit: bool,
}

/// A value written into a list file.
enum Field {
    Text(String),
    Int(i32),
    Bool(bool),
}

/// An entry of a list file: its key in the list and its fields.
trait Entry: Sized {
    /// The entry of a file's object, or None for an object vanilla skips. An error stops loading the file.
    fn read(object: &Map<String, Value>, now: Timestamp) -> Result<Option<(String, Self)>, String>;
    fn write(&self, key: &str, zone: &TimeZone) -> Vec<(&'static str, Field)>;
}

impl Entry for Profile {
    fn read(object: &Map<String, Value>, _: Timestamp) -> Result<Option<(String, Self)>, String> {
        if !object.contains_key("uuid") || !object.contains_key("name") {
            return Ok(None);
        }
        let Some(uuid) = text(object, "uuid")?.as_deref().and_then(java_uuid) else {
            return Ok(None);
        };
        let name = text(object, "name")?.unwrap_or_default();
        Ok(Some((uuid_key(uuid), Profile { uuid, name })))
    }

    fn write(&self, _: &str, _: &TimeZone) -> Vec<(&'static str, Field)> {
        vec![
            ("uuid", Field::Text(uuid_key(self.uuid))),
            ("name", Field::Text(self.name.clone())),
        ]
    }
}

impl Entry for (Profile, Operator) {
    fn read(object: &Map<String, Value>, now: Timestamp) -> Result<Option<(String, Self)>, String> {
        let Some((key, profile)) = Profile::read(object, now)? else {
            return Ok(None);
        };
        // `PermissionLevel.byId` clamps to 0..=4.
        let level = match object.get("level") {
            Some(level) => java_int(level)?.clamp(0, 4),
            None => 0,
        };
        let bypasses_player_limit = match object.get("bypassesPlayerLimit") {
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                text(object, "bypassesPlayerLimit")?.is_some_and(|t| t.eq_ignore_ascii_case("true"))
            }
            None => false,
        };
        let op = Operator {
            level,
            bypasses_player_limit,
        };
        Ok(Some((key, (profile, op))))
    }

    fn write(&self, key: &str, zone: &TimeZone) -> Vec<(&'static str, Field)> {
        let mut fields = self.0.write(key, zone);
        fields.push(("level", Field::Int(self.1.level)));
        fields.push((
            "bypassesPlayerLimit",
            Field::Bool(self.1.bypasses_player_limit),
        ));
        fields
    }
}

impl Entry for (Profile, Ban) {
    fn read(object: &Map<String, Value>, now: Timestamp) -> Result<Option<(String, Self)>, String> {
        let Some((key, profile)) = Profile::read(object, now)? else {
            return Ok(None);
        };
        Ok(Some((key, (profile, Ban::read(object, now)?))))
    }

    fn write(&self, key: &str, zone: &TimeZone) -> Vec<(&'static str, Field)> {
        let mut fields = self.0.write(key, zone);
        fields.extend(self.1.fields(zone));
        fields
    }
}

impl Entry for Ban {
    fn read(object: &Map<String, Value>, now: Timestamp) -> Result<Option<(String, Self)>, String> {
        let Some(ip) = text(object, "ip")? else {
            return Ok(None);
        };
        Ok(Some((ip, Ban::read(object, now)?)))
    }

    fn write(&self, key: &str, zone: &TimeZone) -> Vec<(&'static str, Field)> {
        let mut fields = vec![("ip", Field::Text(key.to_owned()))];
        fields.extend(self.fields(zone));
        fields
    }
}

impl Ban {
    /// `BanListEntry`'s fields: an unreadable creation date is now, an unreadable expiry date never comes.
    fn read(object: &Map<String, Value>, now: Timestamp) -> Result<Self, String> {
        let created = text(object, "created")?;
        let expires = text(object, "expires")?;
        Ok(Ban {
            created: created.as_deref().and_then(parse_date).unwrap_or(now),
            source: text(object, "source")?.unwrap_or_else(|| "(Unknown)".into()),
            expires: expires.as_deref().and_then(parse_date),
            reason: text(object, "reason")?,
        })
    }

    fn fields(&self, zone: &TimeZone) -> Vec<(&'static str, Field)> {
        let date = |at: Timestamp| {
            at.to_zoned_datetime(zone.clone())
                .strftime("%Y-%m-%d %H:%M:%S %z")
                .to_string()
        };
        let mut fields = vec![
            ("created", Field::Text(date(self.created))),
            ("source", Field::Text(self.source.clone())),
            (
                "expires",
                Field::Text(self.expires.map_or_else(|| "forever".into(), date)),
            ),
        ];
        // Gson leaves out the null reason of a ban without one.
        if let Some(reason) = &self.reason {
            fields.push(("reason", Field::Text(reason.clone())));
        }
        fields
    }

    fn expired(&self, now: Timestamp) -> bool {
        self.expires.is_some_and(|expires| expires < now)
    }
}

/// Entries that can expire, which `StoredUserList.get` removes.
trait Expiring {
    fn expired(&self, now: Timestamp) -> bool;
}

impl Expiring for Ban {
    fn expired(&self, now: Timestamp) -> bool {
        Ban::expired(self, now)
    }
}

impl Expiring for (Profile, Ban) {
    fn expired(&self, now: Timestamp) -> bool {
        self.1.expired(now)
    }
}

/// One list file, as vanilla's `StoredUserList` keeps it: a `HashMap` from keys to entries.
struct StoredList<E> {
    path: PathBuf,
    /// In insertion order, from which [`java_order`](Self::java_order) gives the `HashMap`'s.
    entries: IndexMap<String, E>,
    /// The `HashMap`'s table size, which grows past three quarters full and never shrinks.
    capacity: usize,
}

impl<E: Entry> StoredList<E> {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            entries: IndexMap::new(),
            capacity: 16,
        }
    }

    fn insert(&mut self, key: String, entry: E) {
        if !self.entries.contains_key(&key) && self.entries.len() + 1 > self.capacity / 4 * 3 {
            self.capacity *= 2;
        }
        // A key read again keeps its place, as in a `HashMap`.
        self.entries.insert(key, entry);
    }

    /// Reads the file, which may be missing, and returns whether it could be read. `what` names the list in
    /// warnings, as vanilla does.
    fn load(&mut self, now: Timestamp, what: &str) -> bool {
        match self.read(now) {
            Ok(()) => true,
            Err(error) => {
                warn!("Failed to load {what}: {error}");
                false
            }
        }
    }

    fn read(&mut self, now: Timestamp) -> Result<(), String> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        self.entries.clear();
        let text = String::from_utf8_lossy(&bytes);
        if text.trim().is_empty() {
            return Ok(());
        }
        let items = match serde_json::from_str(&text).map_err(|e| e.to_string())? {
            Value::Array(items) => items,
            Value::Null => return Ok(()),
            _ => return Err("Expected a JSON array".into()),
        };
        for item in items {
            let Value::Object(object) = item else {
                return Err(format!("Expected entry to be a JsonObject, was {item}"));
            };
            if let Some((key, entry)) = E::read(&object, now)? {
                self.insert(key, entry);
            }
        }
        Ok(())
    }

    fn save(&self, zone: &TimeZone, what: &str) {
        let entries: Vec<_> = self
            .java_order()
            .into_iter()
            .map(|(key, entry)| entry.write(key, zone))
            .collect();
        let temporary = self.path.with_extension("json.tmp");
        let result =
            fs::write(&temporary, gson(&entries)).and_then(|()| fs::rename(&temporary, &self.path));
        if let Err(error) = result {
            warn!("Failed to save {what}: {error}");
        }
    }

    /// The entries in the order of vanilla's `HashMap`: by bucket of the key's `String.hashCode`, then in
    /// insertion order. Buckets with eight or more keys, which Java turns into trees, are not modeled.
    fn java_order(&self) -> Vec<(&String, &E)> {
        let mut entries: Vec<_> = self.entries.iter().collect();
        let mask = self.capacity - 1;
        entries.sort_by_key(|(key, _)| {
            let hash = key
                .encode_utf16()
                .fold(0i32, |h, c| h.wrapping_mul(31).wrapping_add(i32::from(c)));
            let spread = (hash ^ ((hash as u32) >> 16) as i32) as u32;
            spread as usize & mask
        });
        entries
    }
}

impl<E: Expiring> StoredList<E> {
    fn remove_expired(&mut self, now: Timestamp) {
        self.entries.retain(|_, entry| !entry.expired(now));
    }
}

/// A string field, as Gson's `getAsString` reads one: None when it is missing, an error when it is not a
/// primitive.
fn text(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(Value::Number(number)) => Ok(Some(number.to_string())),
        Some(Value::Bool(value)) => Ok(Some(value.to_string())),
        Some(other) => Err(format!("{key} is not a string: {other}")),
    }
}

/// Gson's `getAsInt`.
fn java_int(value: &Value) -> Result<i32, String> {
    match value {
        Value::Number(number) => match number.as_i64() {
            Some(int) => Ok(int as i32),
            None => Ok(number.as_f64().unwrap_or_default() as i32),
        },
        Value::String(text) => text
            .parse()
            .map_err(|_| format!("For input string: \"{text}\"")),
        other => Err(format!("{other} is not a number")),
    }
}

/// Java's `UUID.fromString`, which also takes groups of other lengths, such as `1-2-3-4-5`.
fn java_uuid(text: &str) -> Option<u128> {
    if text.len() > 36 {
        return None;
    }
    let parts: Vec<_> = text.split('-').collect();
    let [a, b, c, d, e] = parts[..] else {
        return None;
    };
    let hex = |part: &str| i64::from_str_radix(part, 16).ok().map(|v| v as u64);
    let most = (hex(a)? & 0xffff_ffff) << 32 | (hex(b)? & 0xffff) << 16 | hex(c)? & 0xffff;
    let least = (hex(d)? & 0xffff) << 48 | hex(e)? & 0xffff_ffff_ffff;
    Some(u128::from(most) << 64 | u128::from(least))
}

/// `UUID.toString`: the key of a player in every list.
fn uuid_key(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The key of an address in `banned-ips.json`, as vanilla's `IpBanList.getIpFromAddress` cuts it from
/// `/address:port`. For IPv6 that leaves only `[` and the first group, as vanilla does.
fn ip_key(ip: IpAddr) -> String {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{:x}", ip.segments()[0]),
    }
}

/// A date of `yyyy-MM-dd HH:mm:ss Z` as Java's lenient `SimpleDateFormat` reads one: fields of any width that
/// carry into the next, a zone of `+hhmm`, `GMT` or `UTC` with an optional `±h:mm`, and anything after it.
fn parse_date(text: &str) -> Option<Timestamp> {
    let mut rest = text;
    let mut number = |separator: Option<char>| -> Option<i64> {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let value = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
        if let Some(separator) = separator {
            rest = rest.strip_prefix(separator)?;
        }
        Some(value)
    };
    let year = number(Some('-'))?;
    let month = number(Some('-'))?;
    let day = number(Some(' '))?;
    let hour = number(Some(':'))?;
    let minute = number(Some(':'))?;
    let second = number(Some(' '))?;
    let offset = parse_zone(rest)?;
    let start = DateTime::new(i16::try_from(year).ok()?, 1, 1, 0, 0, 0, 0).ok()?;
    let span = Span::new()
        .try_months(month - 1)
        .ok()?
        .try_days(day - 1)
        .ok()?
        .try_hours(hour)
        .ok()?
        .try_minutes(minute)
        .ok()?
        .try_seconds(second)
        .ok()?;
    let local = start.checked_add(span).ok()?;
    offset.to_timestamp(local).ok()
}

/// The zone at the start of `text`: `+hhmm`, or `GMT`/`UTC` with an optional `±h:mm`.
fn parse_zone(text: &str) -> Option<Offset> {
    let signed = |text: &str, colon: bool| -> Option<i32> {
        let sign = match text.chars().next()? {
            '+' => 1,
            '-' => -1,
            _ => return None,
        };
        let text = &text[1..];
        let (hours, minutes) = if colon {
            let (hours, rest) = text.split_once(':')?;
            (
                hours.parse::<i32>().ok()?,
                rest.get(..2)?.parse::<i32>().ok()?,
            )
        } else {
            (text.get(..2)?.parse().ok()?, text.get(2..4)?.parse().ok()?)
        };
        Some(sign * (hours * 3600 + minutes * 60))
    };
    let seconds = match text.get(..3) {
        Some("GMT" | "UTC") => match &text[3..] {
            rest if rest.starts_with(['+', '-']) => signed(rest, true)?,
            _ => 0,
        },
        _ => signed(text, false)?,
    };
    Offset::from_seconds(seconds).ok()
}

/// The zone of `yyyy-MM-dd 'at' HH:mm:ss z` in Java's `Locale.ROOT`.
fn zone_name(zoned: &ZonedDateTime) -> String {
    let seconds = zoned.offset().seconds();
    if seconds == 0 {
        let utc = [
            "UTC",
            "Etc/UTC",
            "UCT",
            "Etc/UCT",
            "Universal",
            "Etc/Universal",
            "Zulu",
            "Etc/Zulu",
        ];
        return if zoned
            .time_zone()
            .iana_name()
            .is_some_and(|name| utc.contains(&name))
        {
            "UTC".into()
        } else {
            "GMT".into()
        };
    }
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.abs() / 60;
    format!("GMT{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A JSON string, for text components.
fn json_string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// The entries as Gson's pretty printer writes them, escaping HTML characters as Gson does by default.
fn gson(entries: &[Vec<(&str, Field)>]) -> String {
    if entries.is_empty() {
        return "[]".into();
    }
    let objects: Vec<String> = entries
        .iter()
        .map(|fields| {
            let fields: Vec<String> = fields
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Field::Text(text) => gson_string(text),
                        Field::Int(value) => value.to_string(),
                        Field::Bool(value) => value.to_string(),
                    };
                    format!("    {}: {value}", gson_string(name))
                })
                .collect();
            format!("  {{\n{}\n  }}", fields.join(",\n"))
        })
        .collect();
    format!("[\n{}\n]", objects.join(",\n"))
}

fn gson_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '=' | '\'' | '\u{2028}' | '\u{2029}' | '\0'..='\u{1f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::{Ipv4Addr, Ipv6Addr};

    const BANNED: u128 = 0x375aeb75_17e7_3800_bc1e_0e47a704624b;
    const FOREVER: u128 = 0x183b2027_009d_31c7_a30e_1d4c35fb324e;
    const EXPIRED: u128 = 0x6b2f1f19_6458_3741_b0f1_a6ca4d6e663c;
    const NO_REASON: u128 = 0xbd059e9e_6395_3397_9132_3027c6391459;
    const LISTED: u128 = 0xde57d176_6e35_3fb0_8026_03364c537494;
    const OP: u128 = 0xdb8264e5_422a_3b66_af54_65ee9ca2b5c9;
    const OP_BYPASS: u128 = 0x52b29d89_234c_3f7a_a045_cbb57fa5ea25;
    const STRANGER: u128 = 0x2644a612_6618_38f1_831c_f70b3502d2a2;

    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("harpas-access-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bogota() -> TimeZone {
        TimeZone::fixed(Offset::constant(-5))
    }

    fn now() -> Timestamp {
        "2026-11-21T22:03:23Z".parse().unwrap()
    }

    const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// The files of the vanilla 26.1 server run that the expected messages and files below come from.
    fn vanilla_folder(name: &str) -> Folder {
        let folder = Folder::new(name);
        fs::write(
            folder.0.join("banned-players.json"),
            r#"[
  {"uuid":"375aeb75-17e7-3800-bc1e-0e47a704624b","name":"Banned","created":"2026-01-01 10:00:00 +0000","source":"Server","expires":"2099-12-31 23:59:59 +0000","reason":"Griefing"},
  {"uuid":"183b2027-009d-31c7-a30e-1d4c35fb324e","name":"BannedForever","created":"2026-01-01 10:00:00 +0000","source":"Server","expires":"forever","reason":"Banned by an operator."},
  {"uuid":"6b2f1f19-6458-3741-b0f1-a6ca4d6e663c","name":"Expired","created":"2026-01-01 10:00:00 +0000","source":"Server","expires":"2026-02-01 00:00:00 +0000","reason":"Old"},
  {"uuid":"bd059e9e-6395-3397-9132-3027c6391459","name":"NoReason"}
]"#,
        )
        .unwrap();
        fs::write(
            folder.0.join("ops.json"),
            r#"[
  {"uuid":"db8264e5-422a-3b66-af54-65ee9ca2b5c9","name":"OpNoBypass","level":4,"bypassesPlayerLimit":false},
  {"uuid":"52b29d89-234c-3f7a-a045-cbb57fa5ea25","name":"OpBypass","level":2,"bypassesPlayerLimit":true}
]"#,
        )
        .unwrap();
        fs::write(
            folder.0.join("whitelist.json"),
            r#"[{"uuid":"de57d176-6e35-3fb0-8026-03364c537494","name":"Listed"}]"#,
        )
        .unwrap();
        folder
    }

    #[test]
    fn files_are_rewritten_as_vanilla_rewrites_them() {
        let folder = vanilla_folder("rewrite");
        AccessLists::load_at(&folder.0, true, bogota(), now());
        // Vanilla's `HashMap` order, its dates in the system's zone and its defaults.
        assert_eq!(
            fs::read_to_string(folder.0.join("banned-players.json")).unwrap(),
            r#"[
  {
    "uuid": "183b2027-009d-31c7-a30e-1d4c35fb324e",
    "name": "BannedForever",
    "created": "2026-01-01 05:00:00 -0500",
    "source": "Server",
    "expires": "forever",
    "reason": "Banned by an operator."
  },
  {
    "uuid": "6b2f1f19-6458-3741-b0f1-a6ca4d6e663c",
    "name": "Expired",
    "created": "2026-01-01 05:00:00 -0500",
    "source": "Server",
    "expires": "2026-01-31 19:00:00 -0500",
    "reason": "Old"
  },
  {
    "uuid": "375aeb75-17e7-3800-bc1e-0e47a704624b",
    "name": "Banned",
    "created": "2026-01-01 05:00:00 -0500",
    "source": "Server",
    "expires": "2099-12-31 18:59:59 -0500",
    "reason": "Griefing"
  },
  {
    "uuid": "bd059e9e-6395-3397-9132-3027c6391459",
    "name": "NoReason",
    "created": "2026-11-21 17:03:23 -0500",
    "source": "(Unknown)",
    "expires": "forever"
  }
]"#
        );
        assert_eq!(
            fs::read_to_string(folder.0.join("banned-ips.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(folder.0.join("ops.json")).unwrap(),
            r#"[
  {
    "uuid": "52b29d89-234c-3f7a-a045-cbb57fa5ea25",
    "name": "OpBypass",
    "level": 2,
    "bypassesPlayerLimit": true
  },
  {
    "uuid": "db8264e5-422a-3b66-af54-65ee9ca2b5c9",
    "name": "OpNoBypass",
    "level": 4,
    "bypassesPlayerLimit": false
  }
]"#
        );
        // An existing whitelist is not written.
        assert_eq!(
            fs::read_to_string(folder.0.join("whitelist.json")).unwrap(),
            r#"[{"uuid":"de57d176-6e35-3fb0-8026-03364c537494","name":"Listed"}]"#
        );
    }

    #[test]
    fn logins_are_checked_in_vanillas_order_with_its_messages() {
        let folder = vanilla_folder("check");
        let mut lists = AccessLists::load_at(&folder.0, true, bogota(), now());
        let check = |lists: &mut AccessLists, uuid| lists.check(uuid, LOOPBACK, now());
        let allow = |bypasses_player_limit| Verdict::Allow {
            bypasses_player_limit,
        };
        let refuse = |text: &str| Verdict::Refuse(text.into());

        // The first banned player found with an expired ban fails, and the next login of that player goes on.
        assert_eq!(check(&mut lists, EXPIRED), Verdict::Fail);
        assert_eq!(
            check(&mut lists, EXPIRED),
            refuse(r#"{"translate":"multiplayer.disconnect.not_whitelisted"}"#)
        );
        assert_eq!(
            check(&mut lists, BANNED),
            refuse(
                r#"{"translate":"multiplayer.disconnect.banned.reason","with":["Griefing"],"extra":[{"translate":"multiplayer.disconnect.banned.expiration","with":["2099-12-31 at 18:59:59 GMT-05:00"]}]}"#
            )
        );
        assert_eq!(
            check(&mut lists, FOREVER),
            refuse(
                r#"{"translate":"multiplayer.disconnect.banned.reason","with":["Banned by an operator."]}"#
            )
        );
        assert_eq!(
            check(&mut lists, NO_REASON),
            refuse(
                r#"{"translate":"multiplayer.disconnect.banned.reason","with":[{"translate":"multiplayer.disconnect.banned.reason.default"}]}"#
            )
        );
        assert_eq!(
            check(&mut lists, STRANGER),
            refuse(r#"{"translate":"multiplayer.disconnect.not_whitelisted"}"#)
        );
        // Operators skip the whitelist, and only those with `bypassesPlayerLimit` skip the limit.
        assert_eq!(check(&mut lists, OP), allow(false));
        assert_eq!(check(&mut lists, OP_BYPASS), allow(true));
        assert_eq!(check(&mut lists, LISTED), allow(false));
        lists.white_list = false;
        assert_eq!(check(&mut lists, STRANGER), allow(false));
        assert!(matches!(check(&mut lists, BANNED), Verdict::Refuse(_)));

        // IP bans come after the whitelist.
        lists.white_list = true;
        let ban = Ban {
            created: now(),
            source: "Server".into(),
            expires: Some("2030-06-01T12:00:00Z".parse().unwrap()),
            reason: Some("Proxy".into()),
        };
        lists.ip_bans.insert("127.0.0.1".into(), ban);
        assert_eq!(
            check(&mut lists, STRANGER),
            refuse(r#"{"translate":"multiplayer.disconnect.not_whitelisted"}"#)
        );
        assert_eq!(
            check(&mut lists, OP_BYPASS),
            refuse(
                r#"{"translate":"multiplayer.disconnect.banned_ip.reason","with":["Proxy"],"extra":[{"translate":"multiplayer.disconnect.banned_ip.expiration","with":["2030-06-01 at 07:00:00 GMT-05:00"]}]}"#
            )
        );
        assert_eq!(
            lists.check(LISTED, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), now()),
            allow(false)
        );
        assert!(
            matches!(check(&mut lists, BANNED), Verdict::Refuse(t) if t.contains(".banned.reason"))
        );
    }

    #[test]
    fn unreadable_files_are_kept() {
        let folder = Folder::new("unreadable");
        let broken =
            r#"[{"uuid":"de57d176-6e35-3fb0-8026-03364c537494","name":"Listed","level":4}, 7]"#;
        fs::write(folder.0.join("ops.json"), broken).unwrap();
        fs::write(folder.0.join("banned-ips.json"), "{not json").unwrap();
        let mut lists = AccessLists::load_at(&folder.0, false, bogota(), now());
        assert_eq!(
            fs::read_to_string(folder.0.join("ops.json")).unwrap(),
            broken
        );
        assert_eq!(
            fs::read_to_string(folder.0.join("banned-ips.json")).unwrap(),
            "{not json"
        );
        // What was read before the bad entry is used, as in vanilla.
        assert_eq!(
            lists.check(LISTED, LOOPBACK, now()),
            Verdict::Allow {
                bypasses_player_limit: false
            }
        );
        assert_eq!(lists.ops.entries.len(), 1);
        // Missing files are created, the whitelist included.
        assert_eq!(
            fs::read_to_string(folder.0.join("banned-players.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(folder.0.join("whitelist.json")).unwrap(),
            "[]"
        );
    }

    #[test]
    fn entries_follow_vanillas_parsing() {
        let read = |json: &str| {
            let Value::Object(object) = serde_json::from_str(json).unwrap() else {
                unreachable!()
            };
            <(Profile, Operator)>::read(&object, now())
        };
        let op =
            read(r#"{"uuid":"1-2-3-4-5","name":"A","level":"9","bypassesPlayerLimit":"TRUE"}"#)
                .unwrap()
                .unwrap();
        assert_eq!(op.0, "00000001-0002-0003-0004-000000000005");
        assert_eq!(
            op.1.1,
            Operator {
                level: 4,
                bypasses_player_limit: true
            }
        );
        assert_eq!(read(r#"{"uuid":"x","name":"A"}"#), Ok(None));
        assert_eq!(read(r#"{"name":"A"}"#), Ok(None));
        assert!(read(r#"{"uuid":"1-2-3-4-5","name":"A","level":"high"}"#).is_err());
        assert_eq!(
            read(r#"{"uuid":"1-2-3-4-5","name":"A","level":-3}"#)
                .unwrap()
                .unwrap()
                .1
                .1
                .level,
            0
        );
        assert_eq!(java_uuid("1-2-3-4"), None);
        assert_eq!(java_uuid("00000001-0002-0003-0004-0000000000050"), None);

        let date = |text| parse_date(text).map(|t: Timestamp| t.to_string());
        assert_eq!(
            date("2026-1-2 3:4:5 +0000").as_deref(),
            Some("2026-01-02T03:04:05Z")
        );
        assert_eq!(
            date("2026-13-01 00:00:00 +0000").as_deref(),
            Some("2027-01-01T00:00:00Z")
        );
        assert_eq!(
            date("2026-01-01 00:00:00 GMT+01:00").as_deref(),
            Some("2025-12-31T23:00:00Z")
        );
        assert_eq!(
            date("2026-01-01 00:00:00 UTC trailing").as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(date("2026-01-01 00:00:00 -05:00"), None);
        assert_eq!(date("forever"), None);
    }

    #[test]
    fn addresses_and_zones_are_named_as_in_java() {
        assert_eq!(ip_key(LOOPBACK), "127.0.0.1");
        assert_eq!(ip_key("::ffff:10.0.0.1".parse().unwrap()), "10.0.0.1");
        assert_eq!(ip_key(IpAddr::V6(Ipv6Addr::LOCALHOST)), "[0");
        assert_eq!(ip_key("2001:db8::1".parse().unwrap()), "[2001");

        let at = |zone: TimeZone| zone_name(&now().to_zoned_datetime(zone));
        assert_eq!(at(TimeZone::UTC), "UTC");
        assert_eq!(at(TimeZone::get("Europe/London").unwrap()), "GMT");
        assert_eq!(at(bogota()), "GMT-05:00");
        assert_eq!(
            at(TimeZone::fixed(Offset::from_seconds(19800).unwrap())),
            "GMT+05:30"
        );
        assert_eq!(
            gson_string("a<b>&='\"\\\n\u{1}"),
            r#""a\u003cb\u003e\u0026\u003d\u0027\"\\\n\u0001""#
        );
    }

    #[test]
    fn the_table_order_follows_javas_hash_map_as_it_grows() {
        let mut list = StoredList::<Profile>::new(PathBuf::new());
        for i in 0..13 {
            list.insert(
                format!("k{i}"),
                Profile {
                    uuid: 0,
                    name: String::new(),
                },
            );
        }
        assert_eq!(list.capacity, 32);
        // Keys "k0" to "k12" in `new HashMap<>()` with 13 puts, as Java iterates them.
        let order: Vec<_> = list
            .java_order()
            .into_iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert_eq!(
            order,
            [
                "k0", "k1", "k2", "k3", "k4", "k5", "k11", "k6", "k10", "k7", "k8", "k12", "k9"
            ]
        );
    }
}
