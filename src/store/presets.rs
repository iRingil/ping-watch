//! Well-known targets the target list starts with: public DNS resolvers (always up, anycast nearby) over IPv4 and
//! IPv6, and a few big sites by name. The user can edit or delete them like any other entry.

/// A ready-made target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// What gets traced.
    pub target: &'static str,
    /// Who runs it, the entry's initial name.
    pub owner: &'static str,
}

/// All presets in list order.
pub const ALL: [Preset; 14] = [
    Preset { target: "8.8.8.8", owner: "Google DNS" },
    Preset { target: "1.1.1.1", owner: "Cloudflare DNS" },
    Preset { target: "9.9.9.9", owner: "Quad9 DNS" },
    Preset { target: "208.67.222.222", owner: "OpenDNS" },
    Preset { target: "77.88.8.8", owner: "Yandex DNS" },
    Preset { target: "2001:4860:4860::8888", owner: "Google DNS" },
    Preset { target: "2606:4700:4700::1111", owner: "Cloudflare DNS" },
    Preset { target: "2620:fe::fe", owner: "Quad9 DNS" },
    Preset { target: "2620:119:35::35", owner: "OpenDNS" },
    Preset { target: "2a02:6b8::feed:0ff", owner: "Yandex DNS" },
    Preset { target: "google.com", owner: "Google" },
    Preset { target: "cloudflare.com", owner: "Cloudflare" },
    Preset { target: "github.com", owner: "GitHub" },
    Preset { target: "ya.ru", owner: "Yandex" },
];

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    #[test]
    fn dns_presets_are_ip_literals_five_of_each_family() {
        let ips: Vec<IpAddr> = ALL.iter().filter_map(|preset| preset.target.parse().ok()).collect();
        assert_eq!(ips.iter().filter(|ip| ip.is_ipv4()).count(), 5);
        assert_eq!(ips.iter().filter(|ip| ip.is_ipv6()).count(), 5);
    }
}
