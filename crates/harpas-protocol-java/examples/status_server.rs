//! Answers the server list ping and turns logins away, using only the generated codecs.
//!
//! Run it, then add `localhost` to the server list of a Java 26.1 client: the entry shows the MOTD and the
//! latency, and joining shows the disconnect message with the player's name.
//!
//! ```sh
//! cargo run -p harpas-protocol-java --example status_server -- 127.0.0.1:25565
//! ```

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::{env, thread};

use harpas_protocol_java::codec::{self, DecodeError};
use harpas_protocol_java::v26_1::{self, handshaking, login, status};

/// Largest frame accepted before Login; vanilla's status and login packets are much smaller.
const MAX_FRAME: usize = 64 * 1024;

fn main() -> io::Result<()> {
    let address = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:25565".to_owned());
    let listener = TcpListener::bind(&address)?;
    println!("listening on {address}");
    for stream in listener.incoming() {
        let stream = stream?;
        thread::spawn(move || {
            let peer = stream
                .peer_addr()
                .map(|a| a.to_string())
                .unwrap_or_default();
            match serve(stream) {
                Ok(()) => println!("{peer}: done"),
                Err(e) => println!("{peer}: {e}"),
            }
        });
    }
    Ok(())
}

fn serve(mut stream: TcpStream) -> io::Result<()> {
    let handshake = match handshaking::serverbound::Packet::decode(&read_frame(&mut stream)?) {
        Ok(handshaking::serverbound::Packet::Handshake(handshake)) => handshake,
        Err(e) => return Err(invalid(e)),
    };
    println!("{handshake:?}");
    match handshake.intent {
        handshaking::Intent::Status => serve_status(stream),
        handshaking::Intent::Login | handshaking::Intent::Transfer => serve_login(stream),
    }
}

fn serve_status(mut stream: TcpStream) -> io::Result<()> {
    loop {
        let packet =
            status::serverbound::Packet::decode(&read_frame(&mut stream)?).map_err(invalid)?;
        println!("{packet:?}");
        let reply: status::clientbound::Packet = match packet {
            status::serverbound::Packet::StatusRequest(_) => {
                let json = format!(
                    r#"{{"version":{{"name":"Harpas {}","protocol":{}}},"players":{{"max":20,"online":0}},"description":{{"text":"Harpas codegen prototype"}}}}"#,
                    v26_1::MINECRAFT_VERSION,
                    v26_1::PROTOCOL,
                );
                status::clientbound::StatusResponse { json }.into()
            }
            status::serverbound::Packet::PingRequest(ping) => status::clientbound::PongResponse {
                timestamp: ping.timestamp,
            }
            .into(),
        };
        write_frame(&mut stream, |buf| reply.encode(buf))?;
        if let status::clientbound::Packet::PongResponse(_) = reply {
            return Ok(());
        }
    }
}

fn serve_login(mut stream: TcpStream) -> io::Result<()> {
    let packet = login::serverbound::Packet::decode(&read_frame(&mut stream)?).map_err(invalid)?;
    println!("{packet:?}");
    let login::serverbound::Packet::LoginStart(start) = packet else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected Login Start",
        ));
    };
    let reason = format!(
        r#"{{"text":"Hello, {}. Harpas cannot play yet."}}"#,
        start.name
    );
    let reply: login::clientbound::Packet = login::clientbound::LoginDisconnect { reason }.into();
    write_frame(&mut stream, |buf| reply.encode(buf))
}

/// Reads one uncompressed frame: a VarInt length, then the packet.
fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len = 0u32;
    for i in 0..3 {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        len |= u32::from(byte[0] & 0x7f) << (7 * i);
        if byte[0] & 0x80 == 0 {
            let len = len as usize;
            if len > MAX_FRAME {
                return Err(invalid(DecodeError::TooLong {
                    len,
                    max: MAX_FRAME,
                }));
            }
            let mut frame = vec![0; len];
            stream.read_exact(&mut frame)?;
            return Ok(frame);
        }
    }
    Err(invalid(DecodeError::VarIntTooLong))
}

/// Writes one uncompressed frame.
fn write_frame(stream: &mut TcpStream, encode: impl FnOnce(&mut Vec<u8>)) -> io::Result<()> {
    let mut packet = Vec::new();
    encode(&mut packet);
    let mut frame = Vec::with_capacity(packet.len() + 3);
    codec::write_varint(&mut frame, packet.len() as i32);
    frame.extend_from_slice(&packet);
    stream.write_all(&frame)
}

fn invalid(error: DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}
