use std::{collections::BTreeSet, net::IpAddr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressKind {
    Loopback,
    Lan,
    Tailscale,
    Public,
    Unusable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkAddress {
    pub interface_name: String,
    pub address: IpAddr,
    pub kind: AddressKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerSelection {
    pub lan_enabled: bool,
    pub tailscale_enabled: bool,
}

#[must_use]
pub fn classify_address(address: IpAddr) -> AddressKind {
    if address.is_unspecified() || address.is_multicast() {
        return AddressKind::Unusable;
    }
    if address.is_loopback() {
        return AddressKind::Loopback;
    }
    if is_tailscale(address) {
        return AddressKind::Tailscale;
    }
    match address {
        IpAddr::V4(address) if address.is_private() || address.is_link_local() => AddressKind::Lan,
        IpAddr::V6(address) if address.is_unique_local() => AddressKind::Lan,
        IpAddr::V4(_) | IpAddr::V6(_) => AddressKind::Public,
    }
}

/// Enumerates host interfaces without modifying operating-system network state.
///
/// # Errors
///
/// Returns an I/O error when the platform interface list cannot be read.
pub fn enumerate_addresses() -> std::io::Result<Vec<NetworkAddress>> {
    let mut addresses = if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| {
            let address = interface.ip();
            NetworkAddress {
                interface_name: interface.name,
                address,
                kind: classify_address(address),
            }
        })
        .collect::<Vec<_>>();
    addresses.sort_by(|left, right| {
        left.interface_name
            .cmp(&right.interface_name)
            .then_with(|| left.address.to_string().cmp(&right.address.to_string()))
    });
    addresses.dedup_by(|left, right| left.address == right.address);
    Ok(addresses)
}

#[must_use]
pub fn select_listener_addresses(
    addresses: &[NetworkAddress],
    selection: ListenerSelection,
) -> Vec<IpAddr> {
    let mut selected = BTreeSet::from([IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)]);
    for item in addresses {
        let enabled = match item.kind {
            AddressKind::Lan => selection.lan_enabled,
            AddressKind::Tailscale => selection.tailscale_enabled,
            AddressKind::Loopback => true,
            AddressKind::Public | AddressKind::Unusable => false,
        };
        if enabled {
            selected.insert(item.address);
        }
    }
    selected.into_iter().collect()
}

#[must_use]
pub fn is_tailscale(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            octets[0] == 100 && (64..=127).contains(&octets[1])
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: &str, value: &str) -> NetworkAddress {
        let address = value.parse().unwrap();
        NetworkAddress {
            interface_name: name.to_owned(),
            address,
            kind: classify_address(address),
        }
    }

    #[test]
    fn classifies_private_tailscale_and_public_addresses() {
        assert_eq!(
            classify_address("127.0.0.1".parse().unwrap()),
            AddressKind::Loopback
        );
        assert_eq!(
            classify_address("192.168.1.5".parse().unwrap()),
            AddressKind::Lan
        );
        assert_eq!(
            classify_address("100.100.20.30".parse().unwrap()),
            AddressKind::Tailscale
        );
        assert_eq!(
            classify_address("8.8.8.8".parse().unwrap()),
            AddressKind::Public
        );
        assert_eq!(
            classify_address("0.0.0.0".parse().unwrap()),
            AddressKind::Unusable
        );
    }

    #[test]
    fn listener_selection_supports_lan_and_tailscale_together() {
        let addresses = vec![
            address("Loopback", "::1"),
            address("Ethernet", "192.168.1.5"),
            address("Tailscale", "100.100.20.30"),
            address("Public", "8.8.8.8"),
        ];
        let selected = select_listener_addresses(
            &addresses,
            ListenerSelection {
                lan_enabled: true,
                tailscale_enabled: true,
            },
        );
        assert!(selected.contains(&"127.0.0.1".parse().unwrap()));
        assert!(selected.contains(&"::1".parse().unwrap()));
        assert!(selected.contains(&"192.168.1.5".parse().unwrap()));
        assert!(selected.contains(&"100.100.20.30".parse().unwrap()));
        assert!(!selected.contains(&"8.8.8.8".parse().unwrap()));
    }
}
