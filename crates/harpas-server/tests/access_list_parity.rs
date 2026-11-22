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
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use harpas_java::{FrameReader, write_frame};
use harpas_protocol_java::v26_1::{PROTOCOL, handshaking, login};

mod reference;
use reference::{Folder, Reference, Server};

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
    let folder = Folder::new("access");
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
        fs::write(dir.join("usercache.json"), r#"[{"name":"OpBypass","uuid":"52b29d89-234c-3f7a-a045-cbb57fa5ea25","expiresOn":"2099-12-31 23:59:59 +0000"}]"#).unwrap();
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
    [
        (&mut reference, &vanilla, ports[0]),
        (&mut server, &harpas, ports[1]),
    ]
    .map(|(server, dir, port)| {
        let mut answers: Vec<_> = PLAYERS.iter().map(|name| log_in(port, name)).collect();
        let files = FILES
            .iter()
            .map(|file| fs::read_to_string(dir.join(file)).unwrap_or_else(|_| "missing".into()))
            .collect();
        writeln!(
            server.child.stdin.as_mut().unwrap(),
            "ban opbypass Test reason"
        )
        .unwrap();
        server.wait_for("Banned OpBypass: Test reason");
        answers.push(log_in(port, "OpBypass"));
        writeln!(server.child.stdin.as_mut().unwrap(), "pardon OPBYPASS").unwrap();
        server.wait_for("Unbanned OpBypass");
        answers.push(log_in(port, "OpBypass"));
        (answers, files)
    })
}

#[test]
#[ignore = "needs Mojang's server: docs/parity.md#locally"]
fn logins_and_list_files_match_vanilla() {
    let Reference { jar, eula } = Reference::required();

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
        assert_eq!(
            expected[10],
            r#"disconnect {"translate":"multiplayer.disconnect.banned.reason","with":["Test reason"]}"#
        );
        assert_eq!(expected[11], expected[7]);
        for ((name, expected), actual) in PLAYERS
            .into_iter()
            .chain(["OpBypass after ban", "OpBypass after pardon"])
            .zip(&expected)
            .zip(&actual)
        {
            assert_eq!(expected, actual, "{name} with IP bans {ip_bans}");
        }
        for ((file, expected), actual) in FILES.iter().zip(&expected_files).zip(&actual_files) {
            assert_eq!(expected, actual, "{file} with IP bans {ip_bans}");
        }
    }
}
