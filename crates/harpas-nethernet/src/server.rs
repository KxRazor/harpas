//! WebRTC peers on one UDP socket, without doing any I/O itself
//!
//! The caller answers signaling requests with [`Server::answer`], passes every datagram of the UDP socket to
//! [`Server::receive`], calls [`Server::poll`] for the datagrams to send and the time of the next call, and reads
//! [`Event`]s. str0m runs ICE, DTLS and SCTP for each peer.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event as RtcEvent, IceConnectionState, Input, Output, Rtc};

use crate::identity::Identity;
use crate::segment::{self, Reassembly};

/// Label of the data channel game batches use.
pub const RELIABLE_CHANNEL: &str = "ReliableDataChannel";
/// Label of the second data channel clients open.
pub const UNRELIABLE_CHANNEL: &str = "UnreliableDataChannel";

/// Identifies a peer for as long as the server runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerId(pub u64);

/// What happened on a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The reliable data channel opened: game batches can flow.
    Connected(PeerId),
    /// A game batch arrived.
    Game(PeerId, Vec<u8>),
    /// The peer is gone.
    Closed(PeerId, String),
}

/// Why an offer could not be answered.
#[derive(Debug)]
pub enum OfferError {
    /// The server already has as many peers as it accepts.
    Full,
    /// The offer is not valid SDP, or not a data channel offer str0m accepts.
    Invalid(String),
}

struct Peer {
    id: PeerId,
    rtc: Rtc,
    reliable: Option<ChannelId>,
    reassembly: Reassembly,
    created: Instant,
}

/// The NetherNet peers of one UDP socket.
pub struct Server {
    local: SocketAddr,
    identity: Identity,
    peers: Vec<Peer>,
    next_id: u64,
    max_peers: usize,
    events: VecDeque<Event>,
}

/// Time for ICE, DTLS and SCTP to open the reliable channel after the answer.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

impl Server {
    /// A server whose UDP socket is bound to `local`, the address put in answers as the host candidate.
    pub fn new(local: SocketAddr, identity: Identity, max_peers: usize) -> Server {
        Server {
            local,
            identity,
            peers: Vec::new(),
            next_id: 0,
            max_peers,
            events: VecDeque::new(),
        }
    }

    /// Peers at once, including those still connecting.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Answers an SDP offer and creates its peer. Returns the answer to send back, with the server identity.
    pub fn answer(&mut self, now: Instant, offer: &str) -> Result<(PeerId, String), OfferError> {
        if self.peers.len() >= self.max_peers {
            return Err(OfferError::Full);
        }
        let offer =
            SdpOffer::from_sdp_string(offer).map_err(|e| OfferError::Invalid(e.to_string()))?;
        // ICE-lite: the server only answers connectivity checks at its known address.
        let mut rtc = Rtc::builder().set_ice_lite(true).build(now);
        let candidate =
            Candidate::host(self.local, "udp").map_err(|e| OfferError::Invalid(e.to_string()))?;
        rtc.add_local_candidate(candidate);
        let answer = rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|e| OfferError::Invalid(e.to_string()))?;
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let sdp = self.identity.add_to_sdp(&answer.to_sdp_string(), unix);
        let id = PeerId(self.next_id);
        self.next_id += 1;
        self.peers.push(Peer {
            id,
            rtc,
            reliable: None,
            reassembly: Reassembly::default(),
            created: now,
        });
        Ok((id, sdp))
    }

    /// Handles a datagram received on the UDP socket.
    pub fn receive(&mut self, now: Instant, from: SocketAddr, datagram: &[u8]) {
        let Ok(contents) = datagram.try_into() else {
            return;
        };
        let input = Input::Receive(
            now,
            Receive {
                proto: Protocol::Udp,
                source: from,
                destination: self.local,
                contents,
            },
        );
        if let Some(peer) = self.peers.iter_mut().find(|peer| peer.rtc.accepts(&input))
            && let Err(e) = peer.rtc.handle_input(input)
        {
            peer.rtc.disconnect();
            self.events.push_back(Event::Closed(peer.id, e.to_string()));
        }
    }

    /// Sends a game batch on a peer's reliable channel. Returns false if the peer is not connected.
    pub fn send(&mut self, peer: PeerId, batch: &[u8]) -> bool {
        let Some(peer) = self.peers.iter_mut().find(|p| p.id == peer) else {
            return false;
        };
        let Some(id) = peer.reliable else {
            return false;
        };
        let Some(messages) = segment::split(batch) else {
            return false;
        };
        let Some(mut channel) = peer.rtc.channel(id) else {
            return false;
        };
        messages
            .iter()
            .all(|message| channel.write(true, message).unwrap_or(false))
    }

    /// Closes a peer.
    pub fn close(&mut self, peer: PeerId) {
        if let Some(peer) = self.peers.iter_mut().find(|p| p.id == peer) {
            peer.rtc.disconnect();
        }
    }

    /// Advances every peer to `now`: collects datagrams to send in `out` and returns when to call again.
    pub fn poll(&mut self, now: Instant, out: &mut Vec<(SocketAddr, Vec<u8>)>) -> Instant {
        let mut next = now + std::time::Duration::from_millis(100);
        for peer in &mut self.peers {
            if peer.reliable.is_none() && now.duration_since(peer.created) >= CONNECT_TIMEOUT {
                peer.rtc.disconnect();
                self.events
                    .push_back(Event::Closed(peer.id, "timed out connecting".into()));
                continue;
            }
            if peer.rtc.handle_input(Input::Timeout(now)).is_err() {
                peer.rtc.disconnect();
            }
            while peer.rtc.is_alive() {
                match peer.rtc.poll_output() {
                    Ok(Output::Timeout(at)) => {
                        next = next.min(at);
                        break;
                    }
                    Ok(Output::Transmit(transmit)) => {
                        out.push((transmit.destination, transmit.contents.to_vec()))
                    }
                    Ok(Output::Event(event)) => handle_event(peer, event, &mut self.events),
                    Err(e) => {
                        peer.rtc.disconnect();
                        self.events.push_back(Event::Closed(peer.id, e.to_string()));
                    }
                }
            }
        }
        let events = &mut self.events;
        self.peers.retain(|peer| {
            let alive = peer.rtc.is_alive();
            if !alive
                && !events
                    .iter()
                    .any(|e| matches!(e, Event::Closed(id, _) if *id == peer.id))
            {
                events.push_back(Event::Closed(peer.id, "disconnected".into()));
            }
            alive
        });
        next
    }

    /// Takes the next event.
    pub fn poll_event(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

fn handle_event(peer: &mut Peer, event: RtcEvent, events: &mut VecDeque<Event>) {
    match event {
        RtcEvent::ChannelOpen(id, label)
            if label == RELIABLE_CHANNEL && peer.reliable.is_none() =>
        {
            peer.reliable = Some(id);
            events.push_back(Event::Connected(peer.id));
        }
        RtcEvent::ChannelOpen(_, label) if label == UNRELIABLE_CHANNEL => {}
        RtcEvent::ChannelOpen(_, label) => {
            peer.rtc.disconnect();
            events.push_back(Event::Closed(
                peer.id,
                format!("unexpected data channel {label:?}"),
            ));
        }
        RtcEvent::ChannelData(data) if Some(data.id) == peer.reliable => {
            match peer.reassembly.push(&data.data) {
                Ok(Some(batch)) => events.push_back(Event::Game(peer.id, batch)),
                Ok(None) => {}
                Err(e) => {
                    peer.rtc.disconnect();
                    events.push_back(Event::Closed(peer.id, e.to_string()));
                }
            }
        }
        RtcEvent::ChannelClose(id) if Some(id) == peer.reliable => {
            peer.rtc.disconnect();
            events.push_back(Event::Closed(peer.id, "reliable channel closed".into()));
        }
        RtcEvent::IceConnectionStateChange(IceConnectionState::Disconnected) => {
            peer.rtc.disconnect();
            events.push_back(Event::Closed(peer.id, "ICE disconnected".into()));
        }
        _ => {}
    }
}
