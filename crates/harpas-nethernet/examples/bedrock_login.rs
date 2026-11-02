//! Takes a Bedrock client from the server list to its Login packet over NetherNet, then turns it away as
//! "server full".
//!
//! Signaling listens on TCP port 19132 and WebRTC on UDP port 19133 of the machine's LAN address. Add the server in
//! a Bedrock 26.60 client with that address and port 19132: the entry shows the status, and joining goes through
//! signaling, WebRTC, Request Network Settings, Network Settings (zlib) and Login. The client then shows the
//! server-full message, which proves it decompressed and read a packet from the server. The client asks once
//! whether to trust the server, because signaling is plain HTTP.
//!
//! ```sh
//! cargo run -p harpas-nethernet --example bedrock_login -- [lan-ip]
//! ```

use std::io::{self, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use harpas_nethernet::identity::Identity;
use harpas_nethernet::signaling::{self, Request, Status};
use harpas_nethernet::{Event, PeerId, Server};

const SIGNALING_PORT: u16 = 19132;
const WEBRTC_PORT: u16 = 19133;
const IDENTITY_FILE: &str = "target/nethernet-identity.pem";
const BEDROCK_PROTOCOL: u32 = 2225;
const REQUEST_NETWORK_SETTINGS: u32 = 0xc1;
const NETWORK_SETTINGS: u32 = 0x8f;
const LOGIN: u32 = 0x01;
const PLAY_STATUS: u32 = 0x02;
const PLAY_STATUS_SERVER_FULL: i32 = 7;
const COMPRESSION_ZLIB: u8 = 0x00;
const COMPRESSION_NONE: u8 = 0xff;

/// An offer from the signaling thread, and where to send its answer.
type OfferRequest = (SocketAddr, String, Sender<Option<String>>);

fn main() -> io::Result<()> {
    let ip = match env::args().nth(1) {
        Some(ip) => ip.parse().expect("an IP address"),
        None => lan_ip()?,
    };
    let identity = load_identity()?;
    let socket = UdpSocket::bind((ip, WEBRTC_PORT))?;
    let mut server = Server::new(socket.local_addr()?, identity, 64);
    let status = Status {
        name: "Harpas NetherNet prototype".into(),
        protocol: BEDROCK_PROTOCOL,
        version: "1.26.60".into(),
        level: "Harpas".into(),
        players: 0,
        max_players: 10,
        game_type: 0,
    };
    let (offers_tx, offers) = mpsc::channel::<OfferRequest>();
    let listener = TcpListener::bind(("0.0.0.0", SIGNALING_PORT))?;
    let status = Arc::new(status.to_json());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (status, offers) = (Arc::clone(&status), offers_tx.clone());
            thread::spawn(move || {
                if let Err(e) = serve_signaling(stream, &status, &offers) {
                    println!("signaling: {e}");
                }
            });
        }
    });
    println!("signaling on tcp 0.0.0.0:{SIGNALING_PORT}, WebRTC on udp {ip}:{WEBRTC_PORT}");
    println!("add the server in Bedrock as {ip}, port {SIGNALING_PORT}");

    let mut compressed = Vec::new();
    let mut buf = vec![0u8; 2048];
    let mut out = Vec::new();
    let mut next = Instant::now();
    loop {
        while let Ok((from, sdp, reply)) = offers.try_recv() {
            match server.answer(Instant::now(), &sdp) {
                Ok((peer, answer)) => {
                    println!("{from}: offer answered, peer {}", peer.0);
                    let _ = reply.send(Some(answer));
                }
                Err(e) => {
                    println!("{from}: offer refused: {e:?}");
                    let _ = reply.send(None);
                }
            }
        }
        let wait = next
            .saturating_duration_since(Instant::now())
            .clamp(Duration::from_millis(1), Duration::from_millis(10));
        socket.set_read_timeout(Some(wait))?;
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => server.receive(Instant::now(), from, &buf[..len]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {}
            Err(e) => return Err(e),
        }
        next = server.poll(Instant::now(), &mut out);
        for (to, datagram) in out.drain(..) {
            socket.send_to(&datagram, to)?;
        }
        while let Some(event) = server.poll_event() {
            match event {
                Event::Connected(peer) => println!("peer {}: connected", peer.0),
                Event::Game(peer, batch) => {
                    handle_batch(&mut server, &mut compressed, peer, &batch)
                }
                Event::Closed(peer, reason) => {
                    compressed.retain(|p| *p != peer);
                    println!("peer {}: closed ({reason})", peer.0);
                }
            }
        }
    }
}

/// The address of the interface that routes to the internet; no packet is sent.
fn lan_ip() -> io::Result<IpAddr> {
    let probe = UdpSocket::bind("0.0.0.0:0")?;
    probe.connect("192.0.2.1:9")?;
    Ok(probe.local_addr()?.ip())
}

/// Keeps the identity across runs, since clients remember the server's key.
fn load_identity() -> io::Result<Identity> {
    if let Some(identity) = fs::read_to_string(IDENTITY_FILE)
        .ok()
        .and_then(|pem| Identity::from_pem(&pem))
    {
        return Ok(identity);
    }
    let identity = Identity::generate();
    fs::write(IDENTITY_FILE, identity.to_pem())?;
    Ok(identity)
}

fn serve_signaling(
    mut stream: TcpStream,
    status: &str,
    offers: &Sender<OfferRequest>,
) -> io::Result<()> {
    let from = stream.peer_addr()?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut input = Vec::new();
    let mut buf = [0u8; 4096];
    let request = loop {
        let len = stream.read(&mut buf)?;
        if len == 0 {
            return Ok(());
        }
        input.extend_from_slice(&buf[..len]);
        match signaling::parse(&input) {
            Ok(Some(request)) => break request,
            Ok(None) => continue,
            Err(e) => {
                println!("{from}: {e}");
                return stream.write_all(&signaling::error_response(&e));
            }
        }
    };
    let response = match request {
        Request::Status => {
            println!("{from}: GET /v1/join");
            signaling::response(200, "OK", "application/json", status.as_bytes())
        }
        Request::Offer { network_id, sdp } => {
            println!(
                "{from}: POST /v1/join/{network_id}, {} bytes of SDP",
                sdp.len()
            );
            let (reply_tx, reply) = mpsc::channel();
            offers
                .send((from, sdp, reply_tx))
                .expect("the main loop runs");
            match reply.recv_timeout(Duration::from_secs(10)) {
                Ok(Some(answer)) => {
                    signaling::response(200, "OK", "application/sdp", answer.as_bytes())
                }
                _ => signaling::response(
                    503,
                    "Service Unavailable",
                    "text/plain",
                    b"Service unavailable",
                ),
            }
        }
    };
    stream.write_all(&response)
}

fn handle_batch(server: &mut Server, compressed: &mut Vec<PeerId>, peer: PeerId, batch: &[u8]) {
    let payload = if compressed.contains(&peer) {
        match batch.split_first() {
            Some((&COMPRESSION_ZLIB, data)) => {
                match miniz_oxide::inflate::decompress_to_vec_with_limit(data, 1 << 22) {
                    Ok(data) => data,
                    Err(e) => return println!("peer {}: bad compressed batch: {e:?}", peer.0),
                }
            }
            Some((&COMPRESSION_NONE, data)) => data.to_vec(),
            other => {
                return println!(
                    "peer {}: unknown compression {:?}",
                    peer.0,
                    other.map(|(a, _)| a)
                );
            }
        }
    } else {
        batch.to_vec()
    };
    let mut rest = &payload[..];
    while !rest.is_empty() {
        let Some(len) = read_varint(&mut rest).filter(|len| *len as usize <= rest.len()) else {
            return println!("peer {}: malformed batch", peer.0);
        };
        let (packet, after) = rest.split_at(len as usize);
        rest = after;
        handle_packet(server, compressed, peer, packet);
    }
}

fn handle_packet(
    server: &mut Server,
    compressed: &mut Vec<PeerId>,
    peer: PeerId,
    mut packet: &[u8],
) {
    let Some(header) = read_varint(&mut packet) else {
        return;
    };
    match header & 0x3ff {
        REQUEST_NETWORK_SETTINGS if packet.len() >= 4 => {
            let protocol = i32::from_be_bytes(packet[..4].try_into().unwrap());
            println!(
                "peer {}: Request Network Settings, protocol {protocol}",
                peer.0
            );
            let mut settings = Vec::new();
            settings.extend_from_slice(&1u16.to_le_bytes());
            settings.extend_from_slice(&u16::from(COMPRESSION_ZLIB).to_le_bytes());
            settings.push(0);
            settings.push(0);
            settings.extend_from_slice(&0f32.to_le_bytes());
            server.send(peer, &batch(NETWORK_SETTINGS, &settings));
            compressed.push(peer);
        }
        LOGIN if packet.len() >= 4 => {
            let protocol = i32::from_be_bytes(packet[..4].try_into().unwrap());
            let mut request = &packet[4..];
            let len = read_varint(&mut request).unwrap_or(0);
            let name = display_name(request).unwrap_or_else(|| "?".to_owned());
            println!(
                "peer {}: Login, protocol {protocol}, {len} bytes of connection request, player {name}",
                peer.0
            );
            let status = batch(PLAY_STATUS, &PLAY_STATUS_SERVER_FULL.to_be_bytes());
            let mut compressed_batch = vec![COMPRESSION_ZLIB];
            compressed_batch.extend(miniz_oxide::deflate::compress_to_vec(&status, 6));
            server.send(peer, &compressed_batch);
        }
        id => println!("peer {}: packet {id:#x}, {} bytes", peer.0, packet.len()),
    }
}

/// One packet as a batch: its length, its header and its body.
fn batch(id: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::new();
    write_varint(&mut packet, id);
    packet.extend_from_slice(body);
    let mut batch = Vec::new();
    write_varint(&mut batch, packet.len() as u32);
    batch.extend(packet);
    batch
}

fn read_varint(input: &mut &[u8]) -> Option<u32> {
    let mut value = 0u32;
    for i in 0..5 {
        let (&byte, rest) = input.split_first()?;
        *input = rest;
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn write_varint(buf: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        buf.push(value as u8 | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

/// Finds the player's name in the JWTs of the connection request, without verifying them.
fn display_name(request: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(request);
    for token in
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
    {
        let Some(payload) = token.split('.').nth(1) else {
            continue;
        };
        let Some(json) = base64url(payload).and_then(|bytes| String::from_utf8(bytes).ok()) else {
            continue;
        };
        for key in [
            "\"displayName\":\"",
            "\"ThirdPartyName\":\"",
            "\"xname\":\"",
        ] {
            if let Some(start) = json.find(key) {
                return json[start + key.len()..]
                    .split('"')
                    .next()
                    .map(str::to_owned);
            }
        }
    }
    None
}

fn base64url(text: &str) -> Option<Vec<u8>> {
    let (mut bits, mut count, mut out) = (0u32, 0, Vec::new());
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = bits << 6 | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}
