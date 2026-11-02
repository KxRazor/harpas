//! Minecraft's game data, extracted from Mojang's server jar on the owner's machine
//!
//! World generation, recipes, loot tables, tags and structures are data files inside Mojang's server jar, which
//! Harpas does not redistribute. On the first start the jar is downloaded from Mojang, verified, and only its
//! `data/` files are extracted into `.harpas/vanilla-data/<version>/`; the jar is never run and is deleted
//! afterwards. `harpas vanilla-data install --from <server.jar>` does the same with a jar the owner provides
//! (`docs/world.md#vanilla-game-data`).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use log::info;
use serde_json::Value;
use sha1::{Digest, Sha1};

/// The Minecraft version whose data the server uses.
pub const VERSION: &str = harpas_protocol_java::v26_1::MINECRAFT_VERSION;

/// The SHA-1 and size of Mojang's server jar for [`VERSION`], as its version manifest gives them. Pinned, so
/// that a jar provided offline is checked as strictly as a download, and a changed manifest is noticed.
const SERVER_JAR_SHA1: &str = "3872a7f07a1a595e651aef8b058dfc2bb3772f46";
const SERVER_JAR_SIZE: u64 = 60_417_588;

const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// Where Mojang serves the pinned jar, for owners who download it themselves.
fn server_jar_url() -> String {
    format!("https://piston-data.mojang.com/v1/objects/{SERVER_JAR_SHA1}/server.jar")
}

/// The directory that holds the data of [`VERSION`] in the server folder `server`.
pub fn dir(server: &Path) -> PathBuf {
    server.join(".harpas").join("vanilla-data").join(VERSION)
}

/// Returns the data directory, downloading and extracting the data first if it is not there.
pub fn provision(server: &Path) -> io::Result<PathBuf> {
    let dir = dir(server);
    if dir.is_dir() {
        return Ok(dir);
    }
    info!("Downloading the Minecraft {VERSION} server jar from Mojang to extract its game data");
    let parent = dir.parent().expect("the data directory has a parent");
    fs::create_dir_all(parent)?;
    let jar = parent.join(format!(".{VERSION}-server.jar.partial"));
    let result = download(&jar).and_then(|()| unpack(&jar, &dir));
    let _ = fs::remove_file(&jar);
    result.map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "Could not provision the Minecraft {VERSION} game data: {e}. Check the network connection and \
                 HTTPS_PROXY, or download {} yourself and run `harpas vanilla-data install --from \
                 <server.jar>`",
                server_jar_url()
            ),
        )
    })?;
    Ok(dir)
}

/// Extracts the data from a server jar the owner provides, replacing any data already installed.
pub fn install_from(server: &Path, jar: &Path) -> io::Result<PathBuf> {
    let mut file =
        File::open(jar).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", jar.display())))?;
    let (sha1, size) = hash(&mut file, &mut io::sink())?;
    check(&sha1, size)?;
    let dir = dir(server);
    fs::create_dir_all(dir.parent().expect("the data directory has a parent"))?;
    unpack(jar, &dir)?;
    Ok(dir)
}

/// Downloads the server jar into `to`, following Mojang's version manifest, and verifies it.
fn download(to: &Path) -> io::Result<()> {
    let agent = ureq::Agent::config_builder()
        .tls_config(crate::http::tls().map_err(io::Error::other)?)
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .new_agent();
    let get = |url: &str| agent.get(url).call().map_err(io::Error::other);
    let manifest = get(MANIFEST)?
        .body_mut()
        .read_to_vec()
        .map_err(io::Error::other)?;
    let manifest: Value = serde_json::from_slice(&manifest).map_err(invalid)?;
    let (url, sha1) = version_entry(&manifest)?;
    let version = get(&url)?
        .body_mut()
        .read_to_vec()
        .map_err(io::Error::other)?;
    let jar_url = server_download(&version, &sha1)?;
    let mut response = get(&jar_url)?;
    let mut file = File::create(to)?;
    let (sha1, size) = hash(&mut response.body_mut().as_reader(), &mut file)?;
    file.sync_all()?;
    check(&sha1, size)
}

/// The URL and SHA-1 of the version file of [`VERSION`] in Mojang's version manifest.
fn version_entry(manifest: &Value) -> io::Result<(String, String)> {
    let entry = manifest["versions"]
        .as_array()
        .and_then(|versions| versions.iter().find(|v| v["id"] == VERSION))
        .ok_or_else(|| invalid(format!("Mojang's version manifest has no {VERSION}")))?;
    match (entry["url"].as_str(), entry["sha1"].as_str()) {
        (Some(url), Some(sha1)) => Ok((url.to_owned(), sha1.to_owned())),
        _ => Err(invalid(format!(
            "Mojang's version manifest lists {VERSION} without a URL and SHA-1"
        ))),
    }
}

/// The server jar URL in the version file `bytes`, once the file matches `sha1` and names the pinned jar.
fn server_download(bytes: &[u8], sha1: &str) -> io::Result<String> {
    if hex(&Sha1::digest(bytes)) != sha1 {
        return Err(invalid(format!(
            "the {VERSION} version file does not match its SHA-1"
        )));
    }
    let version: Value = serde_json::from_slice(bytes).map_err(invalid)?;
    let server = &version["downloads"]["server"];
    if server["sha1"] != SERVER_JAR_SHA1 {
        return Err(invalid(format!(
            "Mojang's manifest names a different {VERSION} server jar than the one this Harpas was built for"
        )));
    }
    server["url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("the {VERSION} version file has no server URL")))
}

/// Copies `from` into `to`, returning the SHA-1 and size of what was copied.
fn hash(from: &mut impl Read, to: &mut impl Write) -> io::Result<(String, u64)> {
    let mut hasher = Sha1::new();
    let mut buf = vec![0; 64 * 1024];
    let mut size = 0;
    loop {
        let read = from.read(&mut buf)?;
        if read == 0 {
            return Ok((hex(&hasher.finalize()), size));
        }
        hasher.update(&buf[..read]);
        to.write_all(&buf[..read])?;
        size += read as u64;
    }
}

/// Whether a jar with this SHA-1 and size is the pinned server jar.
fn check(sha1: &str, size: u64) -> io::Result<()> {
    if sha1 == SERVER_JAR_SHA1 && size == SERVER_JAR_SIZE {
        return Ok(());
    }
    Err(invalid(format!(
        "this is not Mojang's Minecraft {VERSION} server jar (SHA-1 {sha1}, expected {SERVER_JAR_SHA1})"
    )))
}

/// Extracts the `data/` files and `version.json` of the game jar bundled in the server jar `jar` into `dir`.
/// They are extracted next to it first and renamed into place, replacing data already there, so an interrupted
/// extraction leaves nothing and keeps what was installed.
fn unpack(jar: &Path, dir: &Path) -> io::Result<()> {
    let partial = dir.with_file_name(format!(".{VERSION}.partial"));
    if partial.exists() {
        fs::remove_dir_all(&partial)?;
    }
    let result = extract(jar, &partial).and_then(|count| {
        if dir.exists() {
            fs::remove_dir_all(dir)?;
        }
        fs::rename(&partial, dir)?;
        info!(
            "Extracted {count} Minecraft {VERSION} data files into {}",
            dir.display()
        );
        Ok(())
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&partial);
    }
    result
}

fn extract(jar: &Path, into: &Path) -> io::Result<usize> {
    // Since 1.18 the server jar is a bundler: the game itself is a jar inside it, listed in
    // `META-INF/versions.list`.
    let mut bundler = zip::ZipArchive::new(File::open(jar)?).map_err(invalid)?;
    let mut inner = Vec::new();
    bundler
        .by_name(&format!("META-INF/versions/{VERSION}/server-{VERSION}.jar"))
        .map_err(invalid)?
        .read_to_end(&mut inner)?;
    let mut game = zip::ZipArchive::new(io::Cursor::new(inner)).map_err(invalid)?;
    fs::create_dir_all(into)?;
    let mut count = 0;
    for index in 0..game.len() {
        let mut entry = game.by_index(index).map_err(invalid)?;
        let Some(name) = entry.enclosed_name() else {
            continue;
        };
        let wanted = name.starts_with("data") || name == Path::new("version.json");
        if !wanted || entry.is_dir() {
            continue;
        }
        let path = into.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut File::create(path)?)?;
        count += 1;
    }
    Ok(count)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn invalid(error: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use zip::write::SimpleFileOptions;

    use super::*;

    /// A server folder that is removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "harpas-vanilla-data-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for (name, bytes) in files {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// A server jar laid out like Mojang's, bundling a game jar with these files.
    fn server_jar(folder: &Folder, game: &[(&str, &[u8])]) -> PathBuf {
        let inner = zip(game);
        let name = format!("META-INF/versions/{VERSION}/server-{VERSION}.jar");
        let path = folder.0.join("server.jar");
        fs::write(
            &path,
            zip(&[
                ("net/minecraft/bundler/Main.class", b"\xca\xfe"),
                (&name, &inner),
            ]),
        )
        .unwrap();
        path
    }

    #[test]
    fn extracts_only_the_data_files() {
        let folder = Folder::new();
        let jar = server_jar(
            &folder,
            &[
                ("version.json", b"{}"),
                ("data/minecraft/worldgen/noise/a.json", b"[1]"),
                ("data/minecraft/tags/block/b.json", b"[2]"),
                ("net/minecraft/server/Main.class", b"\xca\xfe"),
                ("assets/minecraft/lang/en_us.json", b"{}"),
                ("../escape.json", b"{}"),
            ],
        );
        let dir = dir(&folder.0);
        unpack(&jar, &dir).unwrap();
        assert_eq!(
            fs::read(dir.join("data/minecraft/worldgen/noise/a.json")).unwrap(),
            b"[1]"
        );
        assert_eq!(
            fs::read(dir.join("data/minecraft/tags/block/b.json")).unwrap(),
            b"[2]"
        );
        assert!(dir.join("version.json").is_file());
        assert!(!dir.join("net").exists());
        assert!(!dir.join("assets").exists());
        assert!(!dir.parent().unwrap().join("escape.json").exists());
        assert!(!dir.with_file_name(format!(".{VERSION}.partial")).exists());
    }

    #[test]
    fn a_failed_extraction_leaves_nothing() {
        let folder = Folder::new();
        let jar = folder.0.join("server.jar");
        fs::write(&jar, zip(&[("version.json", b"{}")])).unwrap();
        let dir = dir(&folder.0);
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        assert!(unpack(&jar, &dir).is_err());
        assert!(!dir.exists());
        assert!(!dir.with_file_name(format!(".{VERSION}.partial")).exists());
    }

    #[test]
    fn only_the_pinned_jar_installs() {
        let folder = Folder::new();
        let jar = server_jar(&folder, &[("data/minecraft/a.json", b"{}")]);
        let error = install_from(&folder.0, &jar).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(SERVER_JAR_SHA1));
        assert!(!dir(&folder.0).exists());
        assert!(check(SERVER_JAR_SHA1, SERVER_JAR_SIZE).is_ok());
        assert!(check(SERVER_JAR_SHA1, SERVER_JAR_SIZE - 1).is_err());
    }

    #[test]
    fn hashes_what_it_copies() {
        let mut copy = Vec::new();
        let (sha1, size) = hash(&mut &b"abc"[..], &mut copy).unwrap();
        assert_eq!(sha1, "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!((size, copy.as_slice()), (3, &b"abc"[..]));
    }

    #[test]
    fn follows_the_manifest_to_the_pinned_jar() {
        let manifest = serde_json::json!({ "versions": [
            { "id": "26.1.1", "url": "https://example.com/26.1.1.json", "sha1": "1" },
            { "id": VERSION, "url": "https://example.com/26.1.json", "sha1": "2" },
        ]});
        assert_eq!(
            version_entry(&manifest).unwrap(),
            ("https://example.com/26.1.json".to_owned(), "2".to_owned())
        );
        assert!(version_entry(&serde_json::json!({ "versions": [] })).is_err());

        let version = |sha1: &str| {
            serde_json::json!({ "downloads": { "server": { "sha1": sha1, "url": server_jar_url() } } })
                .to_string()
        };
        let pinned = version(SERVER_JAR_SHA1);
        let sha1 = hex(&Sha1::digest(pinned.as_bytes()));
        assert_eq!(
            server_download(pinned.as_bytes(), &sha1).unwrap(),
            server_jar_url()
        );
        // A version file that does not match the manifest, or that names another jar, is refused.
        assert!(server_download(pinned.as_bytes(), "0").is_err());
        let other = version("0000");
        let sha1 = hex(&Sha1::digest(other.as_bytes()));
        assert!(server_download(other.as_bytes(), &sha1).is_err());
    }
}
