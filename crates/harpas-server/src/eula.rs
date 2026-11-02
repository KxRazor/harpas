//! Acceptance of Mojang's EULA, which comes before anything is downloaded from Mojang
//!
//! The owner accepts it once, with `eula = true` in `harpas.toml` or `HARPAS_EULA=true`; a vanilla `eula.txt`
//! with `eula=true` counts too. On an interactive console the server asks and saves the answer; otherwise it
//! stops and says what to add (`docs/world.md`).

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use log::{info, warn};

use crate::properties::Properties;

/// Where Mojang publishes the Minecraft EULA, as vanilla links it from `eula.txt`.
pub const URL: &str = "https://aka.ms/MinecraftEULA";

/// Why the server does not start without an acceptance, and how to give it.
const NOT_ACCEPTED: &str = "Harpas downloads Minecraft's game data from Mojang, whose files are subject to the \
    Minecraft EULA (https://aka.ms/MinecraftEULA). If you accept it, add `eula = true` to harpas.toml or set \
    HARPAS_EULA=true";

/// Returns once the owner of the server folder `dir` has accepted the EULA, asking on an interactive console.
/// `settings` is the folder's `harpas.toml`.
pub fn require(dir: &Path, settings: &toml::Table) -> io::Result<()> {
    let env = std::env::var("HARPAS_EULA").ok();
    let stdin = io::stdin();
    let terminal = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut output = io::stdout();
    let console = terminal.then_some((
        &mut input as &mut dyn BufRead,
        &mut output as &mut dyn Write,
    ));
    resolve(dir, settings, env.as_deref(), console)
}

/// [`require`], with the environment variable and the console given.
fn resolve(
    dir: &Path,
    settings: &toml::Table,
    env: Option<&str>,
    console: Option<(&mut dyn BufRead, &mut dyn Write)>,
) -> io::Result<()> {
    if accepted(dir, settings, env)? {
        return Ok(());
    }
    let Some((input, output)) = console else {
        return Err(io::Error::other(NOT_ACCEPTED));
    };
    info!(
        "Harpas downloads Minecraft's game data from Mojang, whose files are subject to the Minecraft EULA"
    );
    info!("Read it at {URL}");
    if !ask(input, output)? {
        return Err(io::Error::other(NOT_ACCEPTED));
    }
    let path = dir.join("harpas.toml");
    match save(&path) {
        Ok(()) => info!("Mojang's EULA accepted; saved in {}", path.display()),
        Err(e) => warn!(
            "Mojang's EULA accepted for this start, but {} could not be saved: {e}",
            path.display()
        ),
    }
    Ok(())
}

/// Whether the EULA is accepted: `HARPAS_EULA`, or else `eula` in `harpas.toml`, is true, or `eula.txt` says so.
fn accepted(dir: &Path, settings: &toml::Table, env: Option<&str>) -> io::Result<bool> {
    let harpas = match (env, settings.get("eula")) {
        // Like Java's `Boolean.parseBoolean`, as `server.properties` booleans.
        (Some(value), _) => value.eq_ignore_ascii_case("true"),
        (None, Some(toml::Value::Boolean(value))) => *value,
        (None, Some(value)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("harpas.toml sets `eula` to {value}; it must be true or false"),
            ));
        }
        (None, None) => false,
    };
    // Vanilla reads `eula.txt` as a properties file and the value with `Boolean.parseBoolean`.
    let vanilla = Properties::load(&dir.join("eula.txt"))?
        .get("eula")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"));
    Ok(harpas || vanilla)
}

/// Asks on the console; only `y` or `yes` accepts.
fn ask(input: &mut dyn BufRead, output: &mut dyn Write) -> io::Result<bool> {
    write!(output, "Do you accept the Minecraft EULA? [y/N] ")?;
    output.flush()?;
    let mut answer = String::new();
    input.read_line(&mut answer)?;
    let answer = answer.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Sets `eula = true` in the `harpas.toml` at `path`, keeping the rest of the file as it is.
fn save(path: &Path) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut lines: Vec<&str> = text.lines().collect();
    // `eula` is a top-level key, so it goes before the first table header.
    let top = lines
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let key = |line: &&str| {
        line.split_once('=')
            .is_some_and(|(key, _)| key.trim() == "eula")
    };
    match lines[..top].iter().position(key) {
        Some(index) => lines[index] = "eula = true",
        None => {
            lines.insert(
                0,
                "# Mojang's EULA, accepted on the console: https://aka.ms/MinecraftEULA",
            );
            lines.insert(1, "eula = true");
        }
    }
    let mut new = lines.join("\n");
    new.push('\n');
    // A file this edit cannot handle, such as one with a multi-line string, is left as it was.
    let table: toml::Table = new.parse().map_err(io::Error::other)?;
    if table.get("eula") != Some(&toml::Value::Boolean(true)) {
        return Err(io::Error::other("add `eula = true` to it by hand"));
    }
    fs::write(path, new)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A server folder that is removed when dropped.
    struct Folder(PathBuf);

    impl Folder {
        fn new(files: &[(&str, &str)]) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "harpas-eula-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            for (name, text) in files {
                fs::write(dir.join(name), text).unwrap();
            }
            Self(dir)
        }

        fn settings(&self) -> toml::Table {
            crate::harpas_toml::load(&self.0.join("harpas.toml")).unwrap()
        }

        fn read(&self, name: &str) -> String {
            fs::read_to_string(self.0.join(name)).unwrap()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn accepted_in(files: &[(&str, &str)], env: Option<&str>) -> io::Result<bool> {
        let folder = Folder::new(files);
        accepted(&folder.0, &folder.settings(), env)
    }

    #[test]
    fn harpas_toml_the_environment_or_eula_txt_accept() {
        assert!(!accepted_in(&[], None).unwrap());
        assert!(accepted_in(&[("harpas.toml", "eula = true\n")], None).unwrap());
        assert!(!accepted_in(&[("harpas.toml", "eula = false\n")], None).unwrap());
        assert!(accepted_in(&[], Some("TRUE")).unwrap());
        assert!(!accepted_in(&[], Some("yes")).unwrap());
        // The environment overrides the file.
        assert!(!accepted_in(&[("harpas.toml", "eula = true\n")], Some("false")).unwrap());
        assert!(accepted_in(&[("harpas.toml", "eula = false\n")], Some("true")).unwrap());
    }

    #[test]
    fn a_vanilla_eula_txt_counts() {
        let vanilla = "#By changing the setting below to TRUE you are indicating your agreement to our EULA \
            (https://aka.ms/MinecraftEULA).\n#Mon Nov 02 10:00:00 CET 2026\neula=true\n";
        assert!(accepted_in(&[("eula.txt", vanilla)], None).unwrap());
        assert!(accepted_in(&[("eula.txt", vanilla)], Some("false")).unwrap());
        assert!(!accepted_in(&[("eula.txt", "eula=false\n")], None).unwrap());
        // `Boolean.parseBoolean` does not trim.
        assert!(!accepted_in(&[("eula.txt", "eula=true \n")], None).unwrap());
    }

    #[test]
    fn eula_must_be_a_boolean() {
        let error = accepted_in(&[("harpas.toml", "eula = \"true\"\n")], None).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn only_yes_accepts_on_the_console() {
        for (answer, expected) in [
            ("y\n", true),
            (" YES \n", true),
            ("n\n", false),
            ("\n", false),
            ("", false),
        ] {
            let mut output = Vec::new();
            assert_eq!(
                ask(&mut answer.as_bytes(), &mut output).unwrap(),
                expected,
                "{answer:?}"
            );
            assert_eq!(output, b"Do you accept the Minecraft EULA? [y/N] ");
        }
    }

    #[test]
    fn a_non_interactive_start_explains_how_to_accept() {
        let folder = Folder::new(&[]);
        let error = resolve(&folder.0, &folder.settings(), None, None).unwrap_err();
        assert!(error.to_string().contains(URL));
        assert!(
            error
                .to_string()
                .contains("`eula = true` to harpas.toml or set HARPAS_EULA=true")
        );
        assert!(!folder.0.join("harpas.toml").exists());
    }

    #[test]
    fn accepting_on_the_console_is_saved_once() {
        let folder = Folder::new(&[("harpas.toml", "[bedrock]\nport = 19132\n")]);
        let mut output = Vec::new();
        let console = Some((
            &mut "y\n".as_bytes() as &mut dyn BufRead,
            &mut output as &mut dyn Write,
        ));
        resolve(&folder.0, &folder.settings(), None, console).unwrap();
        let settings = folder.settings();
        assert_eq!(settings.get("eula"), Some(&toml::Value::Boolean(true)));
        assert_eq!(settings["bedrock"]["port"].as_integer(), Some(19132));
        // The next start does not ask.
        resolve(&folder.0, &settings, None, None).unwrap();
    }

    #[test]
    fn declining_on_the_console_stops_the_server() {
        let folder = Folder::new(&[]);
        let mut output = Vec::new();
        let console = Some((
            &mut "n\n".as_bytes() as &mut dyn BufRead,
            &mut output as &mut dyn Write,
        ));
        assert!(resolve(&folder.0, &folder.settings(), None, console).is_err());
        assert!(!folder.0.join("harpas.toml").exists());
    }

    #[test]
    fn saving_replaces_a_top_level_eula_and_keeps_the_rest() {
        let folder = Folder::new(&[("harpas.toml", "# Mine\neula = false\n\n[mods]\neula = 1\n")]);
        save(&folder.0.join("harpas.toml")).unwrap();
        assert_eq!(
            folder.read("harpas.toml"),
            "# Mine\neula = true\n\n[mods]\neula = 1\n"
        );

        let folder = Folder::new(&[]);
        save(&folder.0.join("harpas.toml")).unwrap();
        assert_eq!(
            folder.settings().get("eula"),
            Some(&toml::Value::Boolean(true))
        );
    }

    #[test]
    fn saving_leaves_a_file_it_cannot_edit_alone() {
        let text = "motd = \"\"\"\neula = false\n\"\"\"\n";
        let folder = Folder::new(&[("harpas.toml", text)]);
        assert!(save(&folder.0.join("harpas.toml")).is_err());
        assert_eq!(folder.read("harpas.toml"), text);
    }
}
