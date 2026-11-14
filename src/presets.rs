//! Well-known targets offered in the target field's list: public DNS resolvers (always up, anycast nearby) over
//! IPv4 and IPv6, and a few big sites by name.

/// A ready-made target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    /// What gets traced.
    pub target: &'static str,
    /// Who runs it.
    pub owner: &'static str,
}

/// A titled group of presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    /// Group title shown in the list.
    pub title: &'static str,
    /// The presets.
    pub presets: &'static [Preset],
}

/// All groups in list order.
pub const GROUPS: [Group; 3] = [
    Group {
        title: "IPv4 DNS",
        presets: &[
            Preset { target: "8.8.8.8", owner: "Google" },
            Preset { target: "1.1.1.1", owner: "Cloudflare" },
            Preset { target: "9.9.9.9", owner: "Quad9" },
            Preset { target: "208.67.222.222", owner: "OpenDNS" },
            Preset { target: "77.88.8.8", owner: "Yandex" },
        ],
    },
    Group {
        title: "IPv6 DNS",
        presets: &[
            Preset { target: "2001:4860:4860::8888", owner: "Google" },
            Preset { target: "2606:4700:4700::1111", owner: "Cloudflare" },
            Preset { target: "2620:fe::fe", owner: "Quad9" },
            Preset { target: "2620:119:35::35", owner: "OpenDNS" },
            Preset { target: "2a02:6b8::feed:0ff", owner: "Yandex" },
        ],
    },
    Group {
        title: "Sites",
        presets: &[
            Preset { target: "google.com", owner: "Google" },
            Preset { target: "cloudflare.com", owner: "Cloudflare" },
            Preset { target: "github.com", owner: "GitHub" },
            Preset { target: "ya.ru", owner: "Yandex" },
        ],
    },
];

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;

    #[test]
    fn dns_presets_are_valid_addresses_of_their_family() {
        for (group, ipv6) in [(&GROUPS[0], false), (&GROUPS[1], true)] {
            for preset in group.presets {
                let ip: IpAddr = preset.target.parse().expect("preset is an IP literal");
                assert_eq!(ip.is_ipv6(), ipv6, "{} in {}", preset.target, group.title);
            }
        }
    }
}
