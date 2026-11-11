//! Limits on connections that are not playing yet, so that floods cannot take every thread or login slot
//!
//! A connection counts until it reaches Play, so players behind one address, as on a school or home network, are
//! limited only while they connect. An IPv6 address counts as its /64 prefix, the smallest block a network
//! gives one customer, so that one host cannot avoid the limits by changing the rest of its address. Loopback
//! connections, such as a proxy on the same machine, skip the per-address limits (`docs/java-connection.md`).

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Connections not yet in Play, from every address.
const MAX_PENDING: usize = 256;
/// Connections not yet in Play from one address.
const MAX_PENDING_PER_ADDRESS: usize = 8;
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

/// The connection limits shared by every connection.
#[derive(Default)]
pub struct Limits {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    pending: usize,
    addresses: HashMap<IpAddr, Address>,
    last_sweep: Option<Instant>,
}

#[derive(Default)]
struct Address {
    pending: usize,
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
        self.pending == 0 && self.connections.is_empty() && self.logins.is_empty()
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
        if state.pending >= MAX_PENDING {
            return None;
        }
        if !ip.is_loopback() {
            let address = state.addresses.entry(ip).or_default();
            address.forget_before(now.checked_sub(WINDOW).unwrap_or(now));
            if address.pending >= MAX_PENDING_PER_ADDRESS
                || address.connections.len() >= CONNECTIONS_PER_WINDOW
            {
                return None;
            }
            address.connections.push_back(now);
            address.pending += 1;
        }
        state.pending += 1;
        Some(Permit {
            limits: Arc::clone(self),
            ip,
            pending: true,
        })
    }
}

impl State {
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

    /// The connection has reached Play and no longer counts against the limits.
    pub fn entered_play(&mut self) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let mut state = self.limits.lock();
        state.pending -= 1;
        if let Some(address) = state.addresses.get_mut(&self.ip) {
            address.pending -= 1;
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
