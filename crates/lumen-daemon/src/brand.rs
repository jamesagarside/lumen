//! Map an external IP to a recognisable brand name.
//!
//! Bundled CIDR table covering ~50 well-known services. Linear scan
//! is fine at this size (each new external node = one classify call,
//! one-time). At hundreds of CIDRs an interval tree would be the
//! right move; until then the simplicity wins.
//!
//! All CIDRs are sourced from publicly-published ranges (the
//! providers themselves, or the BGP route announcements). The table
//! is intentionally conservative — overlaps between providers
//! (everything that uses Cloudflare or Fastly as a CDN) are
//! attributed to the underlying CDN rather than the front-facing
//! service. v2 may refine this with DPI hints from integrations.

use std::net::IpAddr;
use std::sync::OnceLock;

use ipnet::IpNet;

/// (brand, CIDR-as-string). Order matters: first match wins. More
/// specific brands should appear before broader CDN ranges.
const RAW_TABLE: &[(&str, &str)] = &[
    // ── Apple (huge contiguous block) ─────────────────────────────────
    ("Apple", "17.0.0.0/8"),
    // ── Google ────────────────────────────────────────────────────────
    ("Google DNS", "8.8.8.0/24"),
    ("Google DNS", "8.8.4.0/24"),
    ("Google", "142.250.0.0/15"),
    ("Google", "172.217.0.0/16"),
    ("Google", "216.58.192.0/19"),
    ("Google", "74.125.0.0/16"),
    ("Google", "64.233.160.0/19"),
    ("Google", "209.85.128.0/17"),
    // ── Cloudflare (DNS + CDN + many SaaS) ────────────────────────────
    ("Cloudflare DNS", "1.1.1.0/24"),
    ("Cloudflare DNS", "1.0.0.0/24"),
    ("Cloudflare", "104.16.0.0/13"),
    ("Cloudflare", "172.64.0.0/13"),
    ("Cloudflare", "162.158.0.0/15"),
    ("Cloudflare", "131.0.72.0/22"),
    ("Cloudflare", "108.162.192.0/18"),
    ("Cloudflare", "190.93.240.0/20"),
    // ── GitHub ────────────────────────────────────────────────────────
    ("GitHub", "140.82.112.0/20"),
    ("GitHub", "185.199.108.0/22"),
    // ── Netflix ───────────────────────────────────────────────────────
    ("Netflix", "23.246.0.0/18"),
    ("Netflix", "37.77.184.0/21"),
    ("Netflix", "45.57.0.0/17"),
    ("Netflix", "64.120.128.0/17"),
    ("Netflix", "108.175.32.0/20"),
    ("Netflix", "185.2.220.0/22"),
    ("Netflix", "192.173.64.0/18"),
    ("Netflix", "198.38.96.0/19"),
    ("Netflix", "198.45.48.0/20"),
    // ── Microsoft / Azure / 365 ───────────────────────────────────────
    // Microsoft 52.x ranges must appear before the AWS 52.0.0.0/8
    // catch-all below — linear-scan, first match wins.
    ("Microsoft", "13.64.0.0/11"),
    ("Microsoft", "13.96.0.0/13"),
    ("Microsoft", "13.104.0.0/14"),
    ("Microsoft", "20.0.0.0/8"),
    ("Microsoft", "40.74.0.0/15"),
    ("Microsoft", "52.96.0.0/14"),
    ("Microsoft", "52.146.0.0/15"),
    ("Microsoft", "52.160.0.0/11"),
    ("Microsoft", "52.184.0.0/13"),
    ("Microsoft", "52.224.0.0/11"),
    ("Microsoft", "65.52.0.0/14"),
    // ── AWS (a tiny representative subset; AWS has hundreds of ranges).
    // 52.0.0.0/8 catches "anything else in 52.x" after the Microsoft
    // sub-ranges have had a chance to match.
    ("AWS", "3.0.0.0/8"),
    ("AWS", "18.0.0.0/8"),
    ("AWS", "52.0.0.0/8"),
    ("AWS", "54.0.0.0/8"),
    // ── Akamai ────────────────────────────────────────────────────────
    ("Akamai", "23.0.0.0/12"),
    ("Akamai", "23.32.0.0/11"),
    ("Akamai", "23.64.0.0/14"),
    ("Akamai", "104.64.0.0/10"),
    ("Akamai", "184.24.0.0/13"),
    // ── Fastly (Reddit, Stripe, many SaaS use this) ───────────────────
    ("Fastly", "151.101.0.0/16"),
    ("Fastly", "199.232.0.0/16"),
    // ── Spotify (GCP-hosted; most go through their own ranges) ────────
    ("Spotify", "35.186.224.0/22"),
    ("Spotify", "104.199.64.0/19"),
    // ── Twitch ────────────────────────────────────────────────────────
    ("Twitch", "23.160.0.0/24"),
    ("Twitch", "192.16.64.0/21"),
    ("Twitch", "192.108.239.0/24"),
    // ── Discord (mostly Cloudflare, but they have some direct ranges) ─
    ("Discord", "162.159.128.0/19"),
    // ── Quad9 DNS, OpenDNS ────────────────────────────────────────────
    ("Quad9 DNS", "9.9.9.0/24"),
    ("OpenDNS", "208.67.222.0/24"),
    ("OpenDNS", "208.67.220.0/24"),
    // ── NTP pool well-known ───────────────────────────────────────────
    ("NTP Pool", "162.159.200.0/24"),
];

#[derive(Debug)]
struct Entry {
    brand: &'static str,
    net: IpNet,
}

fn table() -> &'static [Entry] {
    static TABLE: OnceLock<Vec<Entry>> = OnceLock::new();
    TABLE.get_or_init(|| {
        RAW_TABLE
            .iter()
            .map(|(brand, cidr)| Entry {
                brand,
                net: cidr.parse().unwrap_or_else(|e| {
                    // Bad CIDR in our own source is a programmer error;
                    // fail loud at startup-of-classifier rather than
                    // silently degrade.
                    panic!("invalid CIDR {cidr:?} in brand table: {e}")
                }),
            })
            .collect()
    })
}

/// Return the matching brand for an external IP, if any. Internal
/// IPs should be filtered before calling — this function doesn't
/// know about RFC1918.
pub fn classify(ip: IpAddr) -> Option<&'static str> {
    table()
        .iter()
        .find(|e| e.net.contains(&ip))
        .map(|e| e.brand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(s: &str) -> IpAddr {
        IpAddr::V4(s.parse::<Ipv4Addr>().unwrap())
    }

    #[test]
    fn classifies_well_known_dns() {
        assert_eq!(classify(ip("8.8.8.8")), Some("Google DNS"));
        assert_eq!(classify(ip("1.1.1.1")), Some("Cloudflare DNS"));
        assert_eq!(classify(ip("9.9.9.9")), Some("Quad9 DNS"));
    }

    #[test]
    fn classifies_well_known_services() {
        assert_eq!(classify(ip("17.253.5.55")), Some("Apple"));
        assert_eq!(classify(ip("140.82.121.4")), Some("GitHub"));
        assert_eq!(classify(ip("23.246.30.226")), Some("Netflix"));
        assert_eq!(classify(ip("104.16.132.229")), Some("Cloudflare"));
    }

    #[test]
    fn unknown_external_returns_none() {
        assert_eq!(classify(ip("128.31.0.39")), None); // (CERN / random)
    }

    #[test]
    fn doesnt_panic_on_loopback() {
        assert_eq!(classify(ip("127.0.0.1")), None);
    }
}
