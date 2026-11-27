//! Helpers for parity tests that start both dedicated servers.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub struct Reference {
    pub jar: PathBuf,
    pub eula: String,
}

impl Reference {
    pub fn required() -> Self {
        let reference = PathBuf::from(
            std::env::var_os("HARPAS_VANILLA_SERVER_DIR")
                .expect("HARPAS_VANILLA_SERVER_DIR is not set (docs/parity.md#locally)"),
        );
        let eula = fs::read_to_string(reference.join("eula.txt")).expect("reference EULA");
        assert!(
            eula.lines().any(|line| line.trim() == "eula=true"),
            "the reference server's EULA must already be accepted"
        );
        let jar = fs::canonicalize(reference.join("server.jar")).expect("reference server.jar");
        Self { jar, eula }
    }
}

pub struct Folder(pub PathBuf);

impl Folder {
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-{label}-parity-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A server whose output can be awaited.
pub struct Server {
    pub child: Child,
    lines: Receiver<String>,
    log: String,
}

impl Server {
    pub fn start(program: &OsStr, args: &[&OsStr], dir: &Path) -> Self {
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("HARPAS_") {
                command.env_remove(name);
            }
        }
        let mut child = command.spawn().unwrap();
        let (sender, lines) = mpsc::channel();
        let streams: [Option<Box<dyn Read + Send>>; 2] = [
            child.stdout.take().map(|s| Box::new(s) as _),
            child.stderr.take().map(|s| Box::new(s) as _),
        ];
        for stream in streams.into_iter().flatten() {
            let sender = sender.clone();
            thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            child,
            lines,
            log: String::new(),
        }
    }

    pub fn wait_for(&mut self, text: &str) {
        self.wait_for_any(&[text]);
    }

    pub fn wait_for_any(&mut self, texts: &[&str]) -> usize {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(200)) {
                self.log.push_str(&line);
                self.log.push('\n');
                if let Some(index) = texts.iter().position(|text| line.contains(text)) {
                    return index;
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before {texts:?}: {}",
                self.log
            );
            assert!(
                Instant::now() < deadline,
                "server did not reach {texts:?}: {}",
                self.log
            );
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
