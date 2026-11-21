//! Compares logins refused by bans and the whitelist, and let through by operators, with an owner-provided,
//! EULA-accepted Minecraft 26.1 server, and the list files both servers write back when they start.
//!
//! Both servers get the same `banned-players.json`, `ops.json` and `whitelist.json`, with `white-list=true` and
//! `max-players=0`, so that only operators who bypass the limit get in. The same offline players log in to each,
//! in the same order, and what each answers must be the same: the Login Disconnect text, Login Success, or a
//! connection closed without a packet. A second start adds `banned-ips.json` with the loopback address.
//! Design: `docs/operations.md`, `docs/parity.md`.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use harpas_java::{FrameReader, write_frame};
use harpas_protocol_java::v26_1::{PROTOCOL, handshaking, login};

const BANS: &str = r#"[
  {"uuid":"375aeb75-17e7-3800-bc1e-0e47a704624b","name":"Banned","created":"2026-01-01 10:00:00 +0000","source":"Server","expires":"2099-12-31 23:59:59 +0000","reason":"Griefing <&>"},
  {"uuid":"183b2027-009d-31c7-a30e-1d4c35fb324e","name":"BannedForever","created":"2026-1-1 10:0:0 +0000","source":"Server","expires":"forever","reason":"Banned by an operator."},
  {"uuid":"6b2f1f19-6458-3741-b0f1-a6ca4d6e663c","name":"Expired","created":"2026-01-01 10:00:00 +0000","source":"Server","expires":"2026-02-01 00:00:00 +0000","reason":"Old"},
  {"uuid":"bd059e9e-6395-3397-9132-3027c6391459","name":"NoReason","created":"2026-01-01 10:00:00 GMT+01:00"},
  {"name":"NoUuid"}
]"#;

const OPS: &str = r#"[
  {"uuid":"db8264e5-422a-3b66-af54-65ee9ca2b5c9","name":"OpNoBypass","level":4,"bypassesPlayerLimit":false},
  {"uuid":"52b29d89-234c-3f7a-a045-cbb57fa5ea25","name":"OpBypass","level":7,"bypassesPlayerLimit":true}
]"#;

const WHITELIST: &str = r#"[{"uuid":"de57d176-6e35-3fb0-8026-03364c537494","name":"Listed"}]"#;

const IP_BANS: &str = r#"[
  {"ip":"127.0.0.1","created":"2026-01-01 10:00:00 +0000","source":"Server","expires":"2099-06-01 12:00:00 +0000","reason":"Proxy"}
]"#;

/// Players in the order they log in: an expired ban is noticed by the first login that finds it.
const PLAYERS: [&str; 10] = [
    "Expired",
    "Expired",
    "Banned",
    "BannedForever",
    "NoReason",
    "NotListed",
    "OpNoBypass",
    "OpBypass",
    "Listed",
    "Expired",
];

const FILES: [&str; 4] = [
    "banned-players.json",
    "banned-ips.json",
    "ops.json",
    "whitelist.json",
];

struct Folder(PathBuf);

impl Folder {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "harpas-access-parity-{}-{}",
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
struct Server {
    child: Child,
    lines: Receiver<String>,
    log: String,
}

impl Server {
    fn start(program: &OsStr, args: &[&OsStr], dir: &Path) -> Self {
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

    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(200)) {
                self.log.push_str(&line);
                self.log.push('\n');
                if line.contains(text) {
                    return;
                }
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "server exited before {text}: {}",
                self.log
            );
            assert!(
                Instant::now() < deadline,
                "server did not reach {text}: {}",
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

fn send(client: &mut TcpStream, encode: impl FnOnce(&mut Vec<u8>)) {
    let mut frame = Vec::new();
    write_frame(&mut frame, encode);
    client.write_all(&frame).unwrap();
}

/// Logs `name` in to the server on `port` and describes the answer.
fn log_in(port: u16, name: &str) -> String {
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let handshake: handshaking::serverbound::Packet = handshaking::serverbound::Handshake {
        protocol_version: PROTOCOL,
        server_address: "localhost".into(),
        server_port: port,
        intent: handshaking::Intent::Login,
    }
    .into();
    let start: login::serverbound::Packet = login::serverbound::LoginStart {
        name: name.into(),
        player_uuid: 0,
    }
    .into();
    send(&mut client, |b| handshake.encode(b));
    send(&mut client, |b| start.encode(b));
    let mut frames = FrameReader::default();
    loop {
        if let Some(frame) = frames.next_frame().unwrap() {
            return match login::clientbound::Packet::decode(&frame) {
                Ok(login::clientbound::Packet::LoginDisconnect(disconnect)) => {
                    format!("disconnect {}", disconnect.reason)
                }
                Ok(login::clientbound::Packet::LoginSuccess(_)) => "success".into(),
                other => format!("unexpected {other:?}"),
            };
        }
        let mut buf = [0; 16 * 1024];
        match client.read(&mut buf) {
            Ok(0) => return "closed".into(),
            Ok(count) => frames.push(&buf[..count]),
            Err(error) if error.kind() == ErrorKind::ConnectionReset => return "closed".into(),
            Err(error) => panic!("{name}: {error}"),
        }
    }
}

/// Starts both servers with the same lists, logs every player in to each, and returns what each answered and
/// the list files each wrote, vanilla's first.
fn run(jar: &Path, eula: &str, ip_bans: bool) -> [(Vec<String>, Vec<String>); 2] {
    let folder = Folder::new();
    let vanilla = folder.0.join("vanilla");
    let harpas = folder.0.join("harpas");
    let first = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let second = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ports = [
        first.local_addr().unwrap().port(),
        second.local_addr().unwrap().port(),
    ];
    drop((first, second));
    // The world does not matter. Vanilla gets a flat one, which starts sooner; Harpas generates only normal
    // worlds.
    for (dir, port, level_type) in [(&vanilla, ports[0], "flat"), (&harpas, ports[1], "normal")] {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("eula.txt"), eula).unwrap();
        fs::write(
            dir.join("server.properties"),
            format!(
                "server-ip=127.0.0.1\nserver-port={port}\nonline-mode=false\n\
                 network-compression-threshold=-1\nview-distance=2\nsimulation-distance=2\n\
                 level-type=minecraft:{level_type}\ngenerate-structures=false\n\
                 white-list=true\nmax-players=0\n"
            ),
        )
        .unwrap();
        fs::write(dir.join("banned-players.json"), BANS).unwrap();
        fs::write(dir.join("ops.json"), OPS).unwrap();
        fs::write(dir.join("whitelist.json"), WHITELIST).unwrap();
        if ip_bans {
            fs::write(dir.join("banned-ips.json"), IP_BANS).unwrap();
        }
    }
    let install = Command::new(env!("CARGO_BIN_EXE_harpas"))
        .current_dir(&harpas)
        .args(["vanilla-data", "install", "--from"])
        .arg(jar)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "vanilla data installation failed: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    let mut reference = Server::start(
        OsStr::new("java"),
        &[
            OsStr::new("-Xmx1G"),
            OsStr::new("-jar"),
            jar.as_os_str(),
            OsStr::new("nogui"),
        ],
        &vanilla,
    );
    let mut server = Server::start(OsStr::new(env!("CARGO_BIN_EXE_harpas")), &[], &harpas);
    reference.wait_for("Done (");
    server.wait_for("Done (");
    [(&vanilla, ports[0]), (&harpas, ports[1])].map(|(dir, port)| {
        let answers = PLAYERS.iter().map(|name| log_in(port, name)).collect();
        let files = FILES
            .iter()
            .map(|file| fs::read_to_string(dir.join(file)).unwrap_or_else(|_| "missing".into()))
            .collect();
        (answers, files)
    })
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn logins_and_list_files_match_vanilla() {
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

    for ip_bans in [false, true] {
        let [(expected, expected_files), (actual, actual_files)] = run(&jar, &eula, ip_bans);
        println!("With IP bans: {ip_bans}");
        for (name, answer) in PLAYERS.iter().zip(&expected) {
            println!("  {name}: {answer}");
        }
        // Guard against a vanilla run that would make a broken Harpas pass.
        assert_eq!(expected[0], "closed");
        assert!(expected.iter().any(|a| a == "success") != ip_bans);
        assert!(expected.iter().any(|a| a.contains("not_whitelisted")));
        for ((name, expected), actual) in PLAYERS.iter().zip(&expected).zip(&actual) {
            assert_eq!(expected, actual, "{name} with IP bans {ip_bans}");
        }
        for ((file, expected), actual) in FILES.iter().zip(&expected_files).zip(&actual_files) {
            assert_eq!(expected, actual, "{file} with IP bans {ip_bans}");
        }
    }
}
