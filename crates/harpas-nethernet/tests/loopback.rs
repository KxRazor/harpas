//! A str0m client joins the server in memory, with the data channels Bedrock clients open, and exchanges
//! segmented batches both ways.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use harpas_nethernet::identity::Identity;
use harpas_nethernet::segment;
use harpas_nethernet::server::{RELIABLE_CHANNEL, UNRELIABLE_CHANNEL};
use harpas_nethernet::{Event, Server};
use str0m::change::SdpAnswer;
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event as RtcEvent, Input, Output, Rtc};

const SERVER: &str = "10.0.0.1:19133";
const CLIENT: &str = "10.0.0.2:50000";

struct Client {
    rtc: Rtc,
    reliable: ChannelId,
    reassembly: segment::Reassembly,
    received: Vec<Vec<u8>>,
    open: bool,
}

impl Client {
    /// Polls the client, returning datagrams it sends.
    fn poll(&mut self, now: Instant) -> Vec<Vec<u8>> {
        self.rtc.handle_input(Input::Timeout(now)).unwrap();
        let mut out = Vec::new();
        loop {
            match self.rtc.poll_output().unwrap() {
                Output::Timeout(_) => return out,
                Output::Transmit(t) => out.push(t.contents.to_vec()),
                Output::Event(RtcEvent::ChannelOpen(id, _)) if id == self.reliable => {
                    self.open = true
                }
                Output::Event(RtcEvent::ChannelData(data)) if data.id == self.reliable => {
                    if let Some(batch) = self.reassembly.push(&data.data).unwrap() {
                        self.received.push(batch);
                    }
                }
                Output::Event(_) => {}
            }
        }
    }

    fn receive(&mut self, now: Instant, datagram: &[u8]) {
        let input = Input::Receive(
            now,
            Receive {
                proto: Protocol::Udp,
                source: SERVER.parse().unwrap(),
                destination: CLIENT.parse().unwrap(),
                contents: datagram.try_into().unwrap(),
            },
        );
        self.rtc.handle_input(input).unwrap();
    }
}

/// Moves datagrams between client and server until `done` holds, advancing a simulated clock.
fn run(
    client: &mut Client,
    server: &mut Server,
    now: &mut Instant,
    mut done: impl FnMut(&Client, &[Event]) -> bool,
) -> Vec<Event> {
    let client_addr: SocketAddr = CLIENT.parse().unwrap();
    let mut events = Vec::new();
    for _ in 0..2000 {
        for datagram in client.poll(*now) {
            server.receive(*now, client_addr, &datagram);
        }
        let mut out = Vec::new();
        server.poll(*now, &mut out);
        for (to, datagram) in out {
            assert_eq!(to, client_addr);
            client.receive(*now, &datagram);
        }
        events.extend(std::iter::from_fn(|| server.poll_event()));
        if done(client, &events) {
            return events;
        }
        *now += Duration::from_millis(5);
    }
    panic!("did not finish; events: {events:?}");
}

#[test]
fn bedrock_like_client_exchanges_batches() {
    let mut now = Instant::now();
    let mut server = Server::new(SERVER.parse().unwrap(), Identity::generate(), 8);

    let mut rtc = Rtc::builder().build(now);
    rtc.add_local_candidate(Candidate::host(CLIENT.parse().unwrap(), "udp").unwrap());
    let mut change = rtc.sdp_api();
    let reliable = change.add_channel_with_config(ChannelConfig {
        label: RELIABLE_CHANNEL.into(),
        ordered: true,
        reliability: Reliability::Reliable,
        ..Default::default()
    });
    change.add_channel_with_config(ChannelConfig {
        label: UNRELIABLE_CHANNEL.into(),
        ordered: false,
        reliability: Reliability::MaxRetransmits { retransmits: 0 },
        ..Default::default()
    });
    let (offer, pending) = change.apply().unwrap();

    let (peer, answer) = server.answer(now, &offer.to_sdp_string()).unwrap();
    assert!(answer.contains("a=identity:"), "{answer}");
    let answer = SdpAnswer::from_sdp_string(&answer).unwrap();
    rtc.sdp_api().accept_answer(pending, answer).unwrap();
    let mut client = Client {
        rtc,
        reliable,
        reassembly: Default::default(),
        received: Vec::new(),
        open: false,
    };

    let events = run(&mut client, &mut server, &mut now, |client, events| {
        client.open && !events.is_empty()
    });
    assert_eq!(events, [Event::Connected(peer)]);

    // A 25,000-byte batch from the client: three segments.
    let batch: Vec<u8> = (0..25_000u32).map(|i| i as u8).collect();
    let mut channel = client.rtc.channel(client.reliable).unwrap();
    for message in segment::split(&batch).unwrap() {
        assert!(channel.write(true, &message).unwrap());
    }
    let events = run(&mut client, &mut server, &mut now, |_, events| {
        !events.is_empty()
    });
    assert_eq!(events, [Event::Game(peer, batch)]);

    // And one from the server.
    let reply: Vec<u8> = (0..12_345u32).map(|i| (i * 3) as u8).collect();
    assert!(server.send(peer, &reply));
    run(&mut client, &mut server, &mut now, |client, _| {
        !client.received.is_empty()
    });
    assert_eq!(client.received, [reply]);
}

#[test]
fn bad_offers_are_refused() {
    let now = Instant::now();
    let mut server = Server::new(SERVER.parse().unwrap(), Identity::generate(), 0);
    assert!(matches!(
        server.answer(now, "v=0"),
        Err(harpas_nethernet::OfferError::Full)
    ));
    let mut server = Server::new(SERVER.parse().unwrap(), Identity::generate(), 1);
    assert!(matches!(
        server.answer(now, "not sdp"),
        Err(harpas_nethernet::OfferError::Invalid(_))
    ));
    assert_eq!(server.peer_count(), 0);
}
