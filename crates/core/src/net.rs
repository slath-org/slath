// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The address policy for outbound connections to user-supplied URLs
//! (repository imports, later webhooks).
//!
//! Denied: loopback, private (RFC 1918), shared address space
//! (100.64.0.0/10), link-local (including cloud metadata at
//! 169.254.169.254), unique-local (fc00::/7), multicast, unspecified,
//! broadcast, documentation and benchmarking ranges, the "this network"
//! block, and IPv6 forms that embed an IPv4 address (IPv4-mapped,
//! IPv4-compatible, NAT64 64:ff9b::/96, 6to4, Teredo) when that address
//! is denied. Administrators may allow CIDRs explicitly.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// An IPv4 or IPv6 network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl FromStr for Cidr {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (a, p) = s.split_once('/').unwrap_or((s, ""));
        let addr: IpAddr = a
            .trim()
            .parse()
            .map_err(|_| format!("invalid network {s:?} (example: 10.1.0.0/16)"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = if p.is_empty() {
            max
        } else {
            p.trim()
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= max)
                .ok_or_else(|| format!("invalid prefix length in {s:?}"))?
        };
        Ok(Self { addr, prefix })
    }
}

impl Cidr {
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => {
                let mask = u32::MAX
                    .checked_shl(32 - u32::from(self.prefix))
                    .unwrap_or(0);
                u32::from(n) & mask == u32::from(a) & mask
            }
            (IpAddr::V6(n), IpAddr::V6(a)) => {
                let mask = u128::MAX
                    .checked_shl(128 - u32::from(self.prefix))
                    .unwrap_or(0);
                u128::from(n) & mask == u128::from(a) & mask
            }
            _ => false,
        }
    }
}

fn v4_denied(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || ip.is_documentation()
        || o[0] == 0 // "this network"
        || (o[0] == 100 && (o[1] & 0xC0) == 64) // shared address space
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xFE) == 18) // benchmarking
        || o[0] >= 240 // reserved
}

/// The IPv4 address embedded in an IPv6 address, if it has one.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let s = ip.segments();
    let tail = Ipv4Addr::new(
        (s[6] >> 8) as u8,
        (s[6] & 0xFF) as u8,
        (s[7] >> 8) as u8,
        (s[7] & 0xFF) as u8,
    );
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    // IPv4-compatible (deprecated) ::a.b.c.d, NAT64 64:ff9b::/96 and 64:ff9b:1::/48.
    if s[..6] == [0; 6] || (s[0] == 0x64 && s[1] == 0xff9b) {
        return Some(tail);
    }
    // 6to4 2002:AABB:CCDD::/48.
    if s[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            (s[1] & 0xFF) as u8,
            (s[2] >> 8) as u8,
            (s[2] & 0xFF) as u8,
        ));
    }
    // Teredo 2001:0::/32 (the client address is inverted).
    if s[0] == 0x2001 && s[1] == 0 {
        let a = !s[6];
        let b = !s[7];
        return Some(Ipv4Addr::new(
            (a >> 8) as u8,
            (a & 0xFF) as u8,
            (b >> 8) as u8,
            (b & 0xFF) as u8,
        ));
    }
    None
}

fn v6_denied(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    if (s[0] & 0xFE00) == 0xFC00 // unique local
        || (s[0] & 0xFFC0) == 0xFE80 // link local
        || (s[0] & 0xFFC0) == 0xFEC0 // site local (deprecated)
        || (s[0] == 0x2001 && s[1] == 0x0DB8) // documentation
        || s[0] == 0x0100 && s[1..4] == [0, 0, 0]
    // discard-only 100::/64
    {
        return true;
    }
    embedded_v4(ip).is_some_and(v4_denied) || s[..6] == [0; 6]
}

/// Whether connecting to `ip` is denied (unless an allowed network covers it).
#[must_use]
pub fn is_denied(ip: IpAddr, allowed: &[Cidr]) -> bool {
    if allowed.iter().any(|c| c.contains(ip)) {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => v4_denied(v4),
        IpAddr::V6(v6) => v6_denied(v6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied(s: &str) -> bool {
        is_denied(s.parse().unwrap(), &[])
    }

    #[test]
    fn internal_addresses_are_denied() {
        for ip in [
            "127.0.0.1",
            "127.255.255.254",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "169.254.169.254",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:7f00:1",
            "::127.0.0.1",
            "64:ff9b::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:0001::1",
            "2002:c0a8:0101::1",
            "fd00::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
        ] {
            assert!(denied(ip), "{ip}");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for ip in [
            "93.184.216.34",
            "1.1.1.1",
            "100.63.255.255",
            "172.32.0.1",
            "2606:4700:4700::1111",
            "64:ff9b::5db8:d822",
            "::ffff:93.184.216.34",
        ] {
            assert!(!denied(ip), "{ip}");
        }
    }

    #[test]
    fn allowlisted_networks_override() {
        let allow: Vec<Cidr> = vec!["10.1.0.0/16".parse().unwrap(), "fd00::/8".parse().unwrap()];
        assert!(!is_denied("10.1.2.3".parse().unwrap(), &allow));
        assert!(is_denied("10.2.0.1".parse().unwrap(), &allow));
        assert!(!is_denied("fd12::1".parse().unwrap(), &allow));
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("nope".parse::<Cidr>().is_err());
        let one: Cidr = "127.0.0.1".parse().unwrap();
        assert!(one.contains("127.0.0.1".parse().unwrap()));
        assert!(!one.contains("127.0.0.2".parse().unwrap()));
    }
}
