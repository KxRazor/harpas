//! Name resolution for command arguments, using vanilla's profile cache and Mojang's lookup service.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use indexmap::IndexMap;
use jiff::{Span, Timestamp, ZonedDateTime};
use serde_json::Value;

use crate::access::{Profile, java_uuid, parse_date};
use crate::session::format_uuid;

const LOOKUP: &str = "https://api.mojang.com/minecraft/profile/lookup/name";
const CACHE_LIMIT: usize = 1000;

type Cache = IndexMap<String, (Profile, Timestamp)>;

pub struct Profiles {
    online_mode: bool,
    path: Option<PathBuf>,
    cache: Mutex<Cache>,
    agent: OnceLock<Result<ureq::Agent, String>>,
}

impl Profiles {
    pub fn load(dir: &Path, online_mode: bool) -> Self {
        let path = dir.join("usercache.json");
        match Self::read(&path) {
            Ok(cache) => Self {
                online_mode,
                path: Some(path),
                cache: Mutex::new(cache),
                agent: OnceLock::new(),
            },
            Err(error) => {
                log::warn!("Failed to load the profile cache: {error}");
                Self::in_memory(online_mode)
            }
        }
    }

    pub fn in_memory(online_mode: bool) -> Self {
        Self {
            online_mode,
            path: None,
            cache: Mutex::default(),
            agent: OnceLock::new(),
        }
    }

    fn read(path: &Path) -> io::Result<Cache> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(IndexMap::new()),
            Err(error) => return Err(error),
        };
        let entries: Option<Vec<Value>> = serde_json::from_slice(&bytes)?;
        Ok(entries
            .unwrap_or_default()
            .into_iter()
            .take(CACHE_LIMIT)
            .rev()
            .filter_map(|entry| {
                let name = entry["name"].as_str()?.to_owned();
                let uuid = java_uuid(entry["uuid"].as_str()?)?;
                let expires = parse_date(entry["expiresOn"].as_str()?)?;
                Some((name.to_lowercase(), (Profile { uuid, name }, expires)))
            })
            .collect())
    }

    pub fn resolve(&self, name: &str) -> Option<Profile> {
        let key = name.to_lowercase();
        if let Some(profile) = Self::cached(
            &mut self.cache.lock().unwrap_or_else(|e| e.into_inner()),
            &key,
        ) {
            return Some(profile);
        }
        // A cache miss may need I/O. Keep the cache unlocked so logins can register while it runs.
        let profile = self.lookup(&key).or_else(|| {
            (!self.online_mode).then(|| Profile {
                uuid: harpas_java::offline_uuid(&key),
                name: key.clone(),
            })
        });
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(cached) = Self::cached(&mut cache, &key) {
            return Some(cached);
        }
        if let Some(profile) = &profile {
            self.insert(&mut cache, profile.clone());
        }
        profile
    }

    fn cached(cache: &mut Cache, key: &str) -> Option<Profile> {
        let (profile, expires) = cache.shift_remove(key)?;
        if expires <= Timestamp::now() {
            return None;
        }
        cache.insert(key.to_owned(), (profile.clone(), expires));
        Some(profile)
    }

    pub fn record(&self, profile: Profile) {
        let mut cache = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        self.insert(&mut cache, profile);
    }

    fn insert(&self, cache: &mut Cache, profile: Profile) {
        let now = ZonedDateTime::now();
        let Ok(expires) = now.checked_add(Span::new().months(1)) else {
            return;
        };
        let key = profile.name.to_lowercase();
        cache.shift_remove(&key);
        cache.insert(key, (profile, expires.timestamp()));
        if cache.len() > CACHE_LIMIT {
            cache.shift_remove_index(0);
        }
        if let Some(path) = &self.path {
            let entries: Vec<_> = cache.values().rev().map(|(profile, expires)| {
                serde_json::json!({
                    "name": profile.name,
                    "uuid": format_uuid(profile.uuid),
                    "expiresOn": expires.to_zoned_datetime(now.time_zone().clone()).strftime("%Y-%m-%d %H:%M:%S %z").to_string(),
                })
            }).collect();
            let temporary = path.with_extension("json.tmp");
            if let Err(error) = fs::write(&temporary, serde_json::to_vec(&entries).unwrap())
                .and_then(|()| fs::rename(&temporary, path))
            {
                let _ = fs::remove_file(temporary);
                log::warn!("Failed to save the profile cache: {error}");
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<Profile> {
        if name.len() > 16 || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return None;
        }
        let agent = self.agent.get_or_init(|| {
            Ok(ureq::Agent::config_builder()
                .tls_config(crate::http::tls()?)
                .timeout_global(Some(Duration::from_secs(5)))
                .http_status_as_error(false)
                .build()
                .new_agent())
        });
        let mut response = agent
            .as_ref()
            .ok()?
            .get(format!("{LOOKUP}/{name}"))
            .call()
            .ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        let body = response.body_mut().read_to_string().ok()?;
        let json: Value = serde_json::from_str(&body).ok()?;
        let id = json["id"].as_str()?;
        if id.len() != 32 {
            return None;
        }
        Some(Profile {
            uuid: u128::from_str_radix(id, 16).ok()?,
            name: json["name"].as_str()?.into(),
        })
    }
}
