//! Per-client token bucket (06 §6.2, 08 §8.2). With no WAF on the lean
//! profile this, the replica cap and the backend semaphore are the abuse
//! controls.
//!
//! Clients are keyed by a salted hash of their address (IPv6 by /64), held
//! only in memory; addresses are never logged (09 §9.4.2). The address is
//! taken from `X-Forwarded-For` counting back `trusted_proxy_hops` entries
//! from the right, since entries further left are client-supplied, or, with
//! `USNM_TRUST_CF_CONNECTING_IP`, from Cloudflare's `CF-Connecting-IP` when
//! the request comes from this host (see [`ClientAddress`]). Behind ACA,
//! a verified Cloudflare connection source adds one trusted forwarding hop.

use std::hash::BuildHasher;
use std::net::IpAddr;
use std::time::Duration;

use axum::http::HeaderMap;
use governor::clock::{Clock, DefaultClock};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};

use crate::config::RateLimit;

pub struct Limiter {
    buckets: DefaultKeyedRateLimiter<u64>,
    /// Randomly keyed per process: the salt for client keys.
    salt: std::collections::hash_map::RandomState,
    source: ClientAddress,
}

impl Limiter {
    pub fn new(limit: RateLimit, source: ClientAddress) -> Self {
        Self {
            buckets: RateLimiter::keyed(
                Quota::per_minute(limit.per_minute).allow_burst(limit.burst),
            ),
            salt: Default::default(),
            source,
        }
    }

    /// Take one token for this client, or say how long until one is available.
    pub fn check(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> Result<(), Duration> {
        let key = self.source.of(headers, peer).map_or(0, |ip| self.key(ip));
        self.buckets
            .check_key(&key)
            .map_err(|not_until| not_until.wait_time_from(DefaultClock::default().now()))
    }

    fn key(&self, ip: IpAddr) -> u64 {
        match ip.to_canonical() {
            IpAddr::V4(v4) => self.salt.hash_one(v4.octets()),
            // One household or host usually owns a whole /64.
            IpAddr::V6(v6) => self.salt.hash_one(&v6.octets()[..8]),
        }
    }

    /// Drop idle buckets so memory stays bounded.
    pub fn housekeeping(&self) {
        self.buckets.retain_recent();
        self.buckets.shrink_to_fit();
    }
}

/// Where a request's client address comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAddress {
    /// `hops` entries from the right of `X-Forwarded-For` ([`client_ip`]):
    /// 1 behind Container Apps' ingress.
    Forwarded { hops: usize },
    /// `CF-Connecting-IP`, which Cloudflare's edge sets, but only on a request
    /// from a loopback peer: the cloudflared and proxy on this host. Any
    /// other peer could have written the header itself, so its own address
    /// counts, as it does for a request without the header (a local probe).
    /// `X-Forwarded-For` isn't read.
    CloudflareViaLoopback,
}

impl ClientAddress {
    /// The client address of a request with these headers from `peer`.
    pub fn of(self, headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
        match self {
            Self::Forwarded { hops } => client_ip(headers, peer, hops),
            Self::CloudflareViaLoopback => cf_connecting_ip(headers, peer),
        }
    }
}

/// `CF-Connecting-IP` if `peer` is loopback and the request has the header
/// once, with an address; otherwise `peer`.
fn cf_connecting_ip(headers: &HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
    if !peer.is_some_and(|p| p.to_canonical().is_loopback()) {
        return peer;
    }
    // Two copies can't both be Cloudflare's.
    let mut values = headers.get_all("cf-connecting-ip").iter();
    match (values.next(), values.next()) {
        (Some(v), None) => v.to_str().ok().and_then(|s| parse_ip(s.trim())).or(peer),
        _ => peer,
    }
}

/// The client address: `hops` entries from the right of `X-Forwarded-For`
/// (all header lines, in order). If the header has fewer entries than there
/// are trusted proxies, or the chosen entry isn't an address, no entry can be
/// shown to be proxy-added, so this falls back to the peer address. For the
/// one-hop ACA mode, an ingress-added Cloudflare source permits one more
/// address from the right, retaining the actual visitor instead of the edge.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>, hops: usize) -> Option<IpAddr> {
    if hops == 0 {
        return peer;
    }
    let entries: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    match entries.len().checked_sub(hops) {
        Some(i) => {
            let proxy = parse_ip(entries[i]);
            // ACA appends its verified connection source. Only peel the
            // extra hop when that source is a published Cloudflare network;
            // direct requests cannot opt into trusting a spoofed CF header.
            if hops == 1 && proxy.is_some_and(cloudflare_edge) && i > 0 {
                parse_ip(entries[i - 1]).or(peer)
            } else {
                proxy.or(peer)
            }
        }
        None => peer,
    }
}

// https://www.cloudflare.com/ips/ (verified 27 October 2026).
fn cloudflare_edge(ip: IpAddr) -> bool {
    let (value, ranges): (u128, &[(u128, u32)]) = match ip.to_canonical() {
        IpAddr::V4(ip) => (
            u32::from(ip).into(),
            &[
                (0xadf53000, 20),
                (0x6715f400, 22),
                (0x6716c800, 22),
                (0x671f0400, 22),
                (0x8d654000, 18),
                (0x6ca2c000, 18),
                (0xbe5df000, 20),
                (0xbc726000, 20),
                (0xc5eaf000, 22),
                (0xc6298000, 17),
                (0xa29e0000, 15),
                (0x68100000, 13),
                (0x68180000, 14),
                (0xac400000, 13),
                (0x83004800, 22),
            ],
        ),
        IpAddr::V6(ip) => (
            u128::from(ip),
            &[
                (0x2400cb00000000000000000000000000, 32),
                (0x26064700000000000000000000000000, 32),
                (0x2803f800000000000000000000000000, 32),
                (0x2405b500000000000000000000000000, 32),
                (0x24058100000000000000000000000000, 32),
                (0x2a0698c0000000000000000000000000, 29),
                (0x2c0ff248000000000000000000000000, 32),
            ],
        ),
    };
    let bits = if ip.to_canonical().is_ipv4() { 32 } else { 128 };
    ranges
        .iter()
        .any(|(network, prefix)| value >> (bits - prefix) == network >> (bits - prefix))
}

/// Accepts `1.2.3.4`, `1.2.3.4:5678`, `2001:db8::1` and `[2001:db8::1]:5678`.
fn parse_ip(s: &str) -> Option<IpAddr> {
    s.parse::<IpAddr>()
        .ok()
        .or_else(|| s.parse::<std::net::SocketAddr>().ok().map(|a| a.ip()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    fn xff(values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in values {
            h.append("x-forwarded-for", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn picks_the_entry_the_trusted_proxies_added() {
        let peer = Some("192.0.2.9".parse().unwrap());
        let h = xff(&["6.6.6.6, 1.2.3.4"]);
        assert_eq!(client_ip(&h, peer, 1), Some("1.2.3.4".parse().unwrap()));
        assert_eq!(client_ip(&h, peer, 2), Some("6.6.6.6".parse().unwrap()));
        // More hops than entries: nothing is provably proxy-added.
        assert_eq!(client_ip(&h, peer, 3), peer);
        assert_eq!(client_ip(&h, peer, 0), peer);
        assert_eq!(client_ip(&HeaderMap::new(), peer, 1), peer);
        let h = xff(&["6.6.6.6", "[2001:db8::1]:443"]);
        assert_eq!(client_ip(&h, peer, 1), Some("2001:db8::1".parse().unwrap()));
        assert_eq!(client_ip(&xff(&["garbage"]), peer, 1), peer);
    }

    #[test]
    fn azure_cloudflare_chain_ignores_spoofed_prefix_and_preserves_direct_clients() {
        let peer = Some("192.0.2.9".parse().unwrap());
        assert_eq!(
            client_ip(&xff(&["6.6.6.6, 203.0.113.7, 172.64.1.2"]), peer, 1),
            Some("203.0.113.7".parse().unwrap())
        );
        assert_eq!(
            client_ip(&xff(&["6.6.6.6, 203.0.113.7"]), peer, 1),
            Some("203.0.113.7".parse().unwrap())
        );
        assert_eq!(
            client_ip(&xff(&["2001:db8::7, 2606:4700::1"]), peer, 1),
            Some("2001:db8::7".parse().unwrap())
        );
        assert_eq!(client_ip(&xff(&["garbage, 172.64.1.2"]), peer, 1), peer);
    }

    fn cf(values: &[&str]) -> HeaderMap {
        let mut h = xff(&["198.51.100.20"]);
        for v in values {
            h.append("cf-connecting-ip", v.parse().unwrap());
        }
        h
    }

    #[test]
    fn cf_connecting_ip_counts_only_from_a_loopback_peer() {
        let source = ClientAddress::CloudflareViaLoopback;
        let visitor: IpAddr = "203.0.113.7".parse().unwrap();
        for peer in ["127.0.0.1", "127.0.0.53", "::1", "::ffff:127.0.0.1"] {
            let peer = Some(peer.parse().unwrap());
            assert_eq!(source.of(&cf(&["203.0.113.7"]), peer), Some(visitor));
            assert_eq!(
                source.of(&cf(&["2001:db8::7"]), peer),
                Some("2001:db8::7".parse().unwrap())
            );
            // No header (a local probe), a bad one or two of them: the peer.
            assert_eq!(source.of(&cf(&[]), peer), peer);
            assert_eq!(source.of(&cf(&["garbage"]), peer), peer);
            assert_eq!(source.of(&cf(&["203.0.113.7", "203.0.113.8"]), peer), peer);
        }
        // Any other peer could have set it: its own address, never the header's
        // or X-Forwarded-For's.
        for peer in ["192.0.2.9", "2001:db8::9"] {
            let peer = Some(peer.parse().unwrap());
            assert_eq!(source.of(&cf(&["203.0.113.7"]), peer), peer);
        }
        assert_eq!(source.of(&cf(&["203.0.113.7"]), None), None);
        // The default reads X-Forwarded-For as before and ignores the header.
        let forwarded = ClientAddress::Forwarded { hops: 1 };
        let peer = Some("127.0.0.1".parse().unwrap());
        assert_eq!(
            forwarded.of(&cf(&["203.0.113.7"]), peer),
            Some("198.51.100.20".parse().unwrap())
        );
    }

    #[test]
    fn buckets_are_per_client_and_ipv6_by_prefix() {
        let l = Limiter::new(
            RateLimit {
                per_minute: NonZeroU32::new(1).unwrap(),
                burst: NonZeroU32::new(2).unwrap(),
            },
            ClientAddress::Forwarded { hops: 1 },
        );
        let a = xff(&["1.1.1.1"]);
        assert!(l.check(&a, None).is_ok());
        assert!(l.check(&a, None).is_ok());
        let wait = l.check(&a, None).unwrap_err();
        assert!(wait > Duration::from_secs(1) && wait <= Duration::from_secs(60));
        assert!(l.check(&xff(&["2.2.2.2"]), None).is_ok());
        // A spoofed leftmost entry doesn't create a new bucket.
        assert!(l.check(&xff(&["9.9.9.9, 1.1.1.1"]), None).is_err());
        // Same /64.
        assert!(l.check(&xff(&["2001:db8::1"]), None).is_ok());
        assert!(l.check(&xff(&["2001:db8::2"]), None).is_ok());
        assert!(l.check(&xff(&["2001:db8::3"]), None).is_err());
        assert!(l.check(&xff(&["2001:db8:0:1::1"]), None).is_ok());
    }
}
