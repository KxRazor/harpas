//! The owner-provided Mojang server that the parity tests compare against.

use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct TempDir(pub PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-worldgen-vanilla-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).expect("create temporary directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An unpacked reference server, with Java sources compiled against it.
pub struct Reference {
    pub temp: TempDir,
    pub server_jar: PathBuf,
    classpath: OsString,
}

impl Reference {
    /// Unpacks the server in `HARPAS_VANILLA_SERVER_DIR`, or returns `None` if the variable is not set.
    pub fn from_env() -> Option<Self> {
        let directory = PathBuf::from(std::env::var_os("HARPAS_VANILLA_SERVER_DIR")?);
        let eula = fs::read_to_string(directory.join("eula.txt")).expect("reference EULA");
        assert!(
            eula.lines().any(|line| line.trim() == "eula=true"),
            "the reference server's EULA must already be accepted"
        );
        let temp = TempDir::new();
        let (server_jar, paths) = unpack_reference(&directory.join("server.jar"), &temp.0);
        let classpath = std::env::join_paths(paths).expect("Java classpath");
        Some(Self {
            temp,
            server_jar,
            classpath,
        })
    }

    /// Compiles `(file name, source)` pairs together.
    pub fn compile(&self, sources: &[(&str, &str)]) {
        let mut javac = Command::new("javac");
        javac
            .current_dir(&self.temp.0)
            .arg("-cp")
            .arg(&self.classpath);
        for (name, source) in sources {
            let path = self.temp.0.join(name);
            fs::write(&path, source).unwrap();
            javac.arg(path);
        }
        run(&mut javac);
    }

    /// Runs a compiled class to completion.
    pub fn java(&self, class: &str, args: impl IntoIterator<Item = String>) {
        run(Command::new("java")
            .current_dir(&self.temp.0)
            .arg("-cp")
            .arg(&self.classpath)
            .arg(class)
            .args(args));
    }
}

fn unpack_reference(jar: &Path, dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let mut archive =
        zip::ZipArchive::new(File::open(jar).expect("open server jar")).expect("read server jar");
    let mut classpath = vec![dir.to_path_buf()];
    let mut server = None;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read jar entry");
        let name = entry.name().expect("jar entry name");
        let is_server = name.starts_with("META-INF/versions/") && name.ends_with(".jar");
        let is_library = name.starts_with("META-INF/libraries/") && name.ends_with(".jar");
        if !is_server && !is_library {
            continue;
        }
        let destination = dir.join(format!("reference-{index}.jar"));
        std::io::copy(&mut entry, &mut File::create(&destination).unwrap()).unwrap();
        if is_server {
            assert!(
                server.replace(destination.clone()).is_none(),
                "multiple server jars"
            );
        }
        classpath.push(destination);
    }
    (
        server.expect("server jar contains a versioned jar"),
        classpath,
    )
}

fn run(command: &mut Command) {
    let output = command.output().expect("run Java parity probe");
    assert!(
        output.status.success(),
        "Java parity probe failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
