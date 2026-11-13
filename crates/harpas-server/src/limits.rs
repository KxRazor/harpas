//! Limits on connections that are not playing yet, so that floods cannot take every thread or login slot
//!
//! A connection counts until it reaches Play, so players behind one address, as on a school or home network, are
//! limited only while they connect. An IPv6 address counts as its /64 prefix, the smallest block a network
//! gives one customer, so that one host cannot avoid the limits by changing the rest of its address. Loopback
//! connections, such as a proxy on the same machine, skip the per-address limits (`docs/java-connection.md`).
//!
//! One customer of a hosting company often has a /48, which is 65,536 of those prefixes, so the connections
//! of an IPv6 address also count for its /48 and its /32, the block of a whole network, with higher limits:
//! many unrelated people share those blocks, as the customers of a mobile carrier do.
//!
//! When the total is reached, a new connection takes the place of the oldest one of the address that holds the
//! most, if that address holds at least two more than the newcomer's; failing that, of the /48 that holds the
//! most, and then of the /32. A few addresses or blocks that fill every place therefore cannot keep everyone
//! else out; only as many networks as there are places can.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Connections not yet in Play, from every address.
const MAX_PENDING: usize = 256;
/// Connections not yet in Play from one address.
const MAX_PENDING_PER_ADDRESS: usize = 8;
/// Connections not yet in Play from the larger blocks of an IPv6 address, by prefix length. The limits per
/// [`WINDOW`] stay with the address: a block holds too many unrelated people to share those.
const MAX_PENDING_PER_BLOCK: [(u8, usize); 2] = [(48, 32), (32, 64)];
/// New connections from one address per [`WINDOW`]; a server list refresh and a join take two.
const CONNECTIONS_PER_WINDOW: usize = 20;
/// Login attempts from one address per [`WINDOW`].
const LOGINS_PER_WINDOW: usize = 5;
const WINDOW: Duration = Duration::from_secs(10);

/// The message Bukkit and Paper give a throttled login.
pub const THROTTLED: &str = "Connection throttled! Please wait before reconnecting.";

/// The address `ip` is limited as: an IPv4 address itself, also when mapped into IPv6, and an IPv6 address's
/// /64 prefix.
fn limited_as(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(ip) if !ip.is_loopback() => {
            let prefix = u128::from(ip) & !u128::from(u64::MAX);
            IpAddr::V6(Ipv6Addr::from(prefix))
        }
        ip => ip,
    }
}

/// A block of IPv6 addresses: its prefix length and its first address.
type Block = (u8, Ipv6Addr);

/// The larger blocks that the connections of `ip`, as [`limited_as`] gives it, count for, with the most each
/// may hold. An IPv4 address and loopback have none.
fn blocks_of(ip: IpAddr) -> impl Iterator<Item = (Block, usize)> {
    let ip = match ip {
        IpAddr::V6(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    };
    ip.into_iter().flat_map(|ip| {
        MAX_PENDING_PER_BLOCK.map(|(length, most)| {
            let prefix = u128::from(ip) & (u128::MAX << (128 - length));
            ((length, Ipv6Addr::from(prefix)), most)
        })
    })
}

/// The connection limits shared by every connection.
#[derive(Default)]
pub struct Limits {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pending: usize,
    addresses: HashMap<IpAddr, Address>,
    /// How many connections not yet in Play each larger block holds.
    blocks: HashMap<Block, usize>,
    last_sweep: Option<Instant>,
    next_permit: u64,
}

#[derive(Default)]
struct Address {
    /// The connections not yet in Play, oldest first: each permit's number and its flag to give up its place.
    pending: VecDeque<(u64, Arc<AtomicBool>)>,
    connections: VecDeque<Instant>,
    logins: VecDeque<Instant>,
}

impl Address {
    fn forget_before(&mut self, cutoff: Instant) {
        for times in [&mut self.connections, &mut self.logins] {
            while times.front().is_some_and(|&t| t < cutoff) {
                times.pop_front();
            }
        }
    }

    fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.connections.is_empty() && self.logins.is_empty()
    }
}

impl Limits {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Admits a new connection from `ip`, or returns `None` if it is over a limit and must be closed.
    pub fn connect(self: &Arc<Self>, ip: IpAddr, now: Instant) -> Option<Permit> {
        let ip = limited_as(ip);
        let mut state = self.lock();
        state.sweep(now);
        let mut holds = 0;
        if !ip.is_loopback() {
            let address = state.addresses.entry(ip).or_default();
            address.forget_before(now.checked_sub(WINDOW).unwrap_or(now));
            if address.pending.len() >= MAX_PENDING_PER_ADDRESS
                || address.connections.len() >= CONNECTIONS_PER_WINDOW
            {
                return None;
            }
            holds = address.pending.len();
            if blocks_of(ip).any(|(block, most)| state.held_by(block) >= most) {
                return None;
            }
        }
        if state.pending >= MAX_PENDING && !state.make_room(ip, holds) {
            return None;
        }
        let number = state.next_permit;
        state.next_permit += 1;
        let evicted = Arc::new(AtomicBool::new(false));
        if !ip.is_loopback() {
            let address = state.addresses.entry(ip).or_default();
            address.connections.push_back(now);
            address.pending.push_back((number, Arc::clone(&evicted)));
            for (block, _) in blocks_of(ip) {
                *state.blocks.entry(block).or_default() += 1;
            }
        }
        state.pending += 1;
        Some(Permit {
            limits: Arc::clone(self),
            ip,
            number,
            evicted,
            pending: true,
        })
    }
}

impl State {
    fn held_by(&self, block: Block) -> usize {
        self.blocks.get(&block).copied().unwrap_or(0)
    }

    /// Frees a place for a connection from `ip`, whose address holds `holds` of them. The oldest connection
    /// of the address that holds the most gives up its place, if that leaves the address with no fewer than
    /// the newcomer's; failing that, the oldest of the /48 that holds the most, by the same rule between
    /// /48s, and then of the /32. An IPv4 newcomer counts as a block that holds what its address does.
    /// Returns whether a place was freed.
    fn make_room(&mut self, ip: IpAddr, holds: usize) -> bool {
        let most = self
            .addresses
            .iter()
            .max_by_key(|(_, address)| address.pending.len())
            .filter(|(_, address)| address.pending.len() >= holds + 2)
            .map(|(&ip, _)| ip);
        if let Some(most) = most {
            self.evict_oldest(|ip| ip == most);
            return true;
        }
        for (length, _) in MAX_PENDING_PER_BLOCK {
            let holds = blocks_of(ip)
                .find(|((own, _), _)| *own == length)
                .map_or(holds, |(block, _)| self.held_by(block));
            let most = self
                .blocks
                .iter()
                .filter(|((other, _), _)| *other == length)
                .max_by_key(|(_, held)| **held)
                .filter(|(_, held)| **held >= holds + 2)
                .map(|(&block, _)| block);
            if let Some(most) = most {
                self.evict_oldest(|ip| blocks_of(ip).any(|(block, _)| block == most));
                return true;
            }
        }
        false
    }

    /// Makes the oldest connection of the addresses that `among` selects give up its place.
    fn evict_oldest(&mut self, among: impl Fn(IpAddr) -> bool) {
        let oldest = self
            .addresses
            .iter()
            .filter(|(ip, _)| among(**ip))
            .filter_map(|(&ip, address)| Some((address.pending.front()?.0, ip)))
            .min();
        let Some((_, ip)) = oldest else {
            return;
        };
        let address = self.addresses.get_mut(&ip).expect("the address was found");
        if let Some((_, evicted)) = address.pending.pop_front() {
            evicted.store(true, Ordering::Relaxed);
            self.left(ip);
        }
    }

    /// A connection from `ip` no longer waits to play.
    fn left(&mut self, ip: IpAddr) {
        self.pending -= 1;
        for (block, _) in blocks_of(ip) {
            if let Some(held) = self.blocks.get_mut(&block) {
                *held -= 1;
                if *held == 0 {
                    self.blocks.remove(&block);
                }
            }
        }
    }

    /// Forgets addresses with nothing recent, at most once per window, so that the table cannot grow without
    /// bound.
    fn sweep(&mut self, now: Instant) {
        if self
            .last_sweep
            .is_some_and(|last| now.duration_since(last) < WINDOW)
        {
            return;
        }
        self.last_sweep = Some(now);
        let cutoff = now.checked_sub(WINDOW).unwrap_or(now);
        self.addresses.retain(|_, address| {
            address.forget_before(cutoff);
            !address.is_idle()
        });
    }
}

/// A connection admitted by [`Limits::connect`]; it stops counting once it reaches Play or is dropped.
pub struct Permit {
    limits: Arc<Limits>,
    /// The address the connection is limited as.
    ip: IpAddr,
    number: u64,
    evicted: Arc<AtomicBool>,
    pending: bool,
}

impl Permit {
    /// Records a login attempt; returns `false` if the address has made too many recently.
    pub fn login(&self, now: Instant) -> bool {
        if self.ip.is_loopback() {
            return true;
        }
        let mut state = self.limits.lock();
        let address = state.addresses.entry(self.ip).or_default();
        address.forget_before(now.checked_sub(WINDOW).unwrap_or(now));
        if address.logins.len() >= LOGINS_PER_WINDOW {
            return false;
        }
        address.logins.push_back(now);
        true
    }

    /// Whether the connection lost its place to another address's while the server was full of connections
    /// that are not playing yet, and must be closed.
    pub fn evicted(&self) -> bool {
        self.evicted.load(Ordering::Relaxed)
    }

    /// The connection has reached Play and no longer counts against the limits.
    pub fn entered_play(&mut self) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let mut state = self.limits.lock();
        if self.ip.is_loopback() {
            state.pending -= 1;
            return;
        }
        // An evicted connection's place was already given away.
        let place = state.addresses.get_mut(&self.ip).and_then(|address| {
            let at = address
                .pending
                .iter()
                .position(|(n, _)| *n == self.number)?;
            address.pending.remove(at)
        });
        if place.is_some() {
            state.left(self.ip);
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.entered_play();
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    fn address(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    #[test]
    fn limits_pending_connections_per_address_until_they_play() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        let mut permits: Vec<_> = (0..MAX_PENDING_PER_ADDRESS)
            .map(|_| limits.connect(address(1), now).unwrap())
            .collect();
        assert!(limits.connect(address(1), now).is_none());
        // Another address is not affected.
        assert!(limits.connect(address(2), now).is_some());
        // A player in Play frees its place, as does a closed connection.
        permits[0].entered_play();
        assert!(limits.connect(address(1), now).is_some());
        permits.pop();
        assert!(limits.connect(address(1), now).is_some());
    }

    #[test]
    fn limits_new_connections_and_logins_per_window() {
        let limits = Arc::new(Limits::default());
        let start = Instant::now();
        for _ in 0..CONNECTIONS_PER_WINDOW {
            let permit = limits.connect(address(1), start).unwrap();
            drop(permit);
        }
        assert!(limits.connect(address(1), start).is_none());
        let later = start + WINDOW + Duration::from_millis(1);
        let permit = limits.connect(address(1), later).unwrap();

        for _ in 0..LOGINS_PER_WINDOW {
            assert!(permit.login(later));
        }
        assert!(!permit.login(later));
        assert!(permit.login(later + WINDOW + Duration::from_millis(1)));
    }

    #[test]
    fn loopback_skips_per_address_limits_but_not_the_total() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        // IPv4 loopback as a dual-stack socket reports it.
        let local: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        let mut permits: Vec<_> = (0..MAX_PENDING)
            .map(|_| limits.connect(local, now).unwrap())
            .collect();
        assert!((0..LOGINS_PER_WINDOW * 2).all(|_| permits[0].login(now)));
        assert!(limits.connect(local, now).is_none());
        assert!(limits.connect(address(1), now).is_none());
        permits.pop();
        assert!(limits.connect(address(1), now).is_some());
    }

    #[test]
    fn a_full_server_takes_a_place_from_the_address_that_holds_the_most() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        // 32 addresses fill every place with 8 connections each.
        let mut permits: Vec<Vec<Permit>> = (0..(MAX_PENDING / MAX_PENDING_PER_ADDRESS) as u8)
            .map(|last| {
                (0..MAX_PENDING_PER_ADDRESS)
                    .map(|_| limits.connect(address(last), now).unwrap())
                    .collect()
            })
            .collect();
        assert_eq!(limits.lock().pending, MAX_PENDING);
        // They cannot add more, but another address gets in at the cost of the oldest connection of one of them.
        assert!(limits.connect(address(0), now).is_none());
        let player = limits.connect(address(200), now).unwrap();
        assert!(!player.evicted());
        assert_eq!(limits.lock().pending, MAX_PENDING);
        let evicted: Vec<_> = permits
            .iter()
            .enumerate()
            .filter(|(_, held)| held.iter().any(Permit::evicted))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(evicted.len(), 1);
        let held = &mut permits[evicted[0]];
        assert!(held[0].evicted());
        assert!(held[1..].iter().all(|permit| !permit.evicted()));
        // Closing the evicted connection does not free a second place.
        held.remove(0);
        assert_eq!(limits.lock().pending, MAX_PENDING);
        // A loopback connection gets in the same way.
        let local = limits
            .connect(IpAddr::V4(Ipv4Addr::LOCALHOST), now)
            .unwrap();
        assert_eq!(limits.lock().pending, MAX_PENDING);
        drop((player, local));
        assert_eq!(limits.lock().pending, MAX_PENDING - 2);
    }

    #[test]
    fn a_full_server_refuses_a_connection_when_no_address_holds_two_more() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        // One connection from each of 254 addresses, and two from another.
        let address =
            |n: usize| IpAddr::V4(Ipv4Addr::new(203, 0, (n / 256) as u8, (n % 256) as u8));
        let mut permits: Vec<_> = (0..MAX_PENDING - 2)
            .map(|n| limits.connect(address(n), now).unwrap())
            .collect();
        permits.push(limits.connect(address(1000), now).unwrap());
        permits.push(limits.connect(address(1000), now).unwrap());
        // An address that already holds one would only trade places, so it is refused.
        assert!(limits.connect(address(0), now).is_none());
        assert!(permits.iter().all(|permit| !permit.evicted()));
        // A new address takes the older of the two.
        let player = limits.connect(address(2000), now).unwrap();
        assert!(permits[MAX_PENDING - 2].evicted());
        // Now every address holds one, and the next new address is refused.
        assert!(limits.connect(address(3000), now).is_none());
        drop(player);
    }

    #[test]
    fn ipv6_addresses_share_the_limits_of_their_64_bit_prefix() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        let host = |prefix: u16, host: u16| -> IpAddr {
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, prefix, 0, 0, 0, host))
        };
        let permits: Vec<_> = (0..MAX_PENDING_PER_ADDRESS as u16)
            .map(|i| limits.connect(host(1, i), now).unwrap())
            .collect();
        assert!(limits.connect(host(1, 0x100), now).is_none());
        // Logins count for the prefix too.
        assert!((0..LOGINS_PER_WINDOW).all(|i| permits[i].login(now)));
        assert!(!permits[LOGINS_PER_WINDOW].login(now));
        // Another prefix is not affected, and IPv6 loopback still skips the limits.
        assert!(limits.connect(host(2, 0), now).is_some());
        let local = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert!((0..MAX_PENDING_PER_ADDRESS * 2).all(|_| limits.connect(local, now).is_some()));
    }

    /// An address of the /64 `prefix` in the /48 `site` of the /32 `network`.
    fn v6(network: u16, site: u16, prefix: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x2001, network, site, prefix, 0, 0, 0, 1))
    }

    #[test]
    fn the_prefixes_of_one_ipv6_block_share_its_places() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        // One connection from each /64 of a /48 never reaches the limit of an address.
        let site: Vec<_> = (0..32)
            .map(|prefix| limits.connect(v6(1, 1, prefix), now).unwrap())
            .collect();
        assert!(limits.connect(v6(1, 1, 32), now).is_none());
        // The /48s of one /32 share its places too: 32 more from another fill it.
        let network: Vec<_> = (0..32)
            .map(|prefix| limits.connect(v6(1, 2, prefix), now).unwrap())
            .collect();
        assert!(limits.connect(v6(1, 3, 0), now).is_none());
        // Another network and IPv4 are not affected.
        let other = limits.connect(v6(2, 1, 0), now).unwrap();
        assert!(limits.connect(address(1), now).is_some());
        assert_eq!(limits.lock().blocks.len(), 5);

        // A connection that plays or closes frees its place in every block.
        drop(site);
        assert!(limits.connect(v6(1, 3, 0), now).is_some());
        drop((network, other));
        assert!(limits.lock().blocks.is_empty());
        assert_eq!(limits.lock().pending, 0);
    }

    #[test]
    fn a_full_server_takes_a_place_from_the_ipv6_block_that_holds_the_most() {
        let limits = Arc::new(Limits::default());
        let now = Instant::now();
        // Four networks fill every place, each with two /48s of 32 prefixes that hold one connection.
        let permits: Vec<_> = (0..MAX_PENDING as u16)
            .map(|n| limits.connect(v6(n / 64, n / 32, n % 32), now).unwrap())
            .collect();
        assert_eq!(limits.lock().pending, MAX_PENDING);
        // No address holds two, so the newcomer takes the oldest place of a /48.
        let player = limits.connect(address(1), now).unwrap();
        let evicted: Vec<_> = (0..MAX_PENDING).filter(|&n| permits[n].evicted()).collect();
        assert_eq!(evicted.len(), 1);
        assert_eq!(evicted[0] % 32, 0, "not the oldest of its /48");
        assert_eq!(limits.lock().pending, MAX_PENDING);
        // The /48 that lost a place holds 31. A second newcomer takes from one that still holds 32.
        let second = limits.connect(address(2), now).unwrap();
        let evicted: Vec<_> = (0..MAX_PENDING)
            .filter(|&n| permits[n].evicted())
            .map(|n| n / 32)
            .collect();
        assert_eq!(evicted.len(), 2);
        assert_ne!(evicted[0], evicted[1]);
        // A prefix of a new /48 in a network that holds fewer takes a place from a full /48 as well.
        let network = evicted[0] as u16 / 2;
        let third = limits.connect(v6(network, 100, 0), now).unwrap();
        assert_eq!(limits.lock().pending, MAX_PENDING);
        assert_eq!(
            (0..MAX_PENDING).filter(|&n| permits[n].evicted()).count(),
            3
        );
        drop((player, second, third, permits));
        assert_eq!(limits.lock().pending, 0);
        assert!(limits.lock().blocks.is_empty());
    }

    #[test]
    fn idle_addresses_are_forgotten() {
        let limits = Arc::new(Limits::default());
        let start = Instant::now();
        drop(limits.connect(address(1), start));
        assert_eq!(limits.lock().addresses.len(), 1);
        drop(limits.connect(address(2), start + WINDOW * 2));
        assert_eq!(limits.lock().addresses.len(), 1);
    }
}
