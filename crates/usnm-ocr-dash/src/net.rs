//! Address ranges for `--allow`.

use std::net::IpAddr;
use std::str::FromStr;

/// An address range: `192.168.0.0/24`, or one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    net: IpAddr,
    bits: u8,
}

impl FromStr for Cidr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (addr, bits) = match s.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (s, None),
        };
        let net: IpAddr = addr.parse().map_err(|_| format!("{s}: not an address"))?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let bits = match bits {
            Some(b) => b
                .parse::<u8>()
                .ok()
                .filter(|b| *b <= max)
                .ok_or_else(|| format!("{s}: the prefix must be 0 to {max}"))?,
            None => max,
        };
        let c = Cidr { net, bits };
        if c.mask(net) != net {
            return Err(format!("{s}: host bits set"));
        }
        Ok(c)
    }
}

impl Cidr {
    fn mask(&self, ip: IpAddr) -> IpAddr {
        match ip {
            IpAddr::V4(v) => {
                let m = u32::MAX.checked_shl(32 - u32::from(self.bits)).unwrap_or(0);
                IpAddr::V4((u32::from(v) & m).into())
            }
            IpAddr::V6(v) => {
                let m = u128::MAX
                    .checked_shl(128 - u32::from(self.bits))
                    .unwrap_or(0);
                IpAddr::V6((u128::from(v) & m).into())
            }
        }
    }

    /// Whether `ip` (an IPv4-mapped IPv6 address counts as IPv4) is in
    /// the range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.net.is_ipv4() && self.mask(ip) == self.net
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let lan: Cidr = "192.168.0.0/24".parse().unwrap();
        assert!(lan.contains("192.168.0.243".parse().unwrap()));
        assert!(lan.contains("::ffff:192.168.0.12".parse().unwrap()));
        assert!(!lan.contains("192.168.1.5".parse().unwrap()));
        assert!(!lan.contains("10.0.0.1".parse().unwrap()));
        let one: Cidr = "192.168.0.12".parse().unwrap();
        assert!(one.contains("192.168.0.12".parse().unwrap()));
        assert!(!one.contains("192.168.0.13".parse().unwrap()));
        assert!("192.168.0.1/24".parse::<Cidr>().is_err());
        assert!("192.168.0.0/33".parse::<Cidr>().is_err());
        assert!("lan".parse::<Cidr>().is_err());
        let all: Cidr = "0.0.0.0/0".parse().unwrap();
        assert!(all.contains("8.8.8.8".parse().unwrap()));
        assert!(!all.contains("::1".parse().unwrap()));
    }
}
