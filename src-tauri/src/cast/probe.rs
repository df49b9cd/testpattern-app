//! Finding Chromecasts without mDNS. Many routers don't pass multicast
//! between Wi-Fi clients (AP isolation, IGMP snooping), so `_googlecast._tcp`
//! never arrives although the device is reachable. Every Cast device also
//! answers `http://<ip>:8008/setup/eureka_info` with its name and a stable
//! id, which is enough to list it: remembered devices are re-checked, the
//! local /24 networks are swept for the Cast port, and the user can add an
//! address by hand.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde::Deserialize;

use super::Device;

/// Cast channel (TLS) and the setup API (plain HTTP) every device runs.
pub const CAST_PORT: u16 = 8009;
const SETUP_PORT: u16 = 8008;

#[derive(Deserialize)]
struct EurekaInfo {
    name: Option<String>,
    /// UPnP UDN: stable per device, also the mDNS `id` with the dashes removed
    ssdp_udn: Option<String>,
    mac_address: Option<String>,
}

/// mDNS TXT `id` and eureka `ssdp_udn` name the same device: one key for both.
pub fn normalize_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_lowercase()
}

/// Asks `ip` whether it is a Cast device; its name and id if so.
pub async fn probe(http: &reqwest::Client, ip: IpAddr) -> Option<Device> {
    let url = format!("http://{}/setup/eureka_info?params=name,device_info", SocketAddr::new(ip, SETUP_PORT));
    let info: EurekaInfo = http.get(url).timeout(Duration::from_secs(3)).send().await.ok()?.json().await.ok()?;
    let id = info.ssdp_udn.or(info.mac_address).map(|i| normalize_id(&i)).filter(|i| !i.is_empty())?;
    Some(Device {
        id,
        name: info.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("Chromecast ({ip})")),
        // eureka_info doesn't say; a device that answers is a Cast device
        model: String::new(),
        addr: SocketAddr::new(ip, CAST_PORT),
        fullname: String::new(),
    })
}

/// This computer's private IPv4 networks of at most 256 addresses (larger
/// ones would take too long to sweep; "Add by address" covers them).
pub fn local_networks() -> Vec<(Ipv4Addr, u8)> {
    let Ok(ifaces) = if_addrs::get_if_addrs() else { return Vec::new() };
    let mut out: Vec<(Ipv4Addr, u8)> = ifaces
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if v4.ip.is_private() => Some((v4.ip, v4.prefixlen.max(24))),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup_by_key(|(ip, prefix)| u32::from(*ip) >> (32 - *prefix as u32));
    out
}

/// Every host address of the network `ip/prefix` except `ip` itself.
pub fn hosts(ip: Ipv4Addr, prefix: u8) -> Vec<Ipv4Addr> {
    let prefix = prefix.clamp(24, 30) as u32;
    let mask = u32::MAX << (32 - prefix);
    let net = u32::from(ip) & mask;
    let broadcast = net | !mask;
    (net + 1..broadcast).map(Ipv4Addr::from).filter(|h| *h != ip).collect()
}

/// Addresses on `networks` with the Cast port open (a ~1 s parallel sweep).
pub async fn sweep(networks: &[(Ipv4Addr, u8)]) -> Vec<IpAddr> {
    let mut tasks = tokio::task::JoinSet::new();
    for &(ip, prefix) in networks {
        for host in hosts(ip, prefix) {
            tasks.spawn(async move {
                let addr = SocketAddr::new(IpAddr::V4(host), CAST_PORT);
                let open = tokio::time::timeout(Duration::from_millis(900), tokio::net::TcpStream::connect(addr))
                    .await
                    .is_ok_and(|c| c.is_ok());
                open.then_some(IpAddr::V4(host))
            });
        }
    }
    let mut found = Vec::new();
    while let Some(r) = tasks.join_next().await {
        if let Ok(Some(ip)) = r {
            found.push(ip);
        }
    }
    found.sort();
    found
}

/// What the user typed: an IPv4 address, optionally with the Cast port.
pub fn parse_address(input: &str) -> Option<IpAddr> {
    let s = input.trim();
    let host = s.strip_suffix(&format!(":{CAST_PORT}")).unwrap_or(s);
    host.parse::<Ipv4Addr>().ok().filter(|ip| !ip.is_loopback() && !ip.is_unspecified()).map(IpAddr::V4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdns_and_setup_ids_match() {
        // mDNS TXT id vs. eureka_info ssdp_udn of the same Chromecast
        assert_eq!(normalize_id("ce33bf0d7d2ab19d3115eca1c2a3b577"), normalize_id("ce33bf0d-7d2a-b19d-3115-eca1c2a3b577"));
        assert_eq!(normalize_id("B0:2A:43:3D:1D:DC"), "b02a433d1ddc");
    }

    #[test]
    fn sweeps_the_local_network_only() {
        let h = hosts(Ipv4Addr::new(192, 168, 8, 12), 24);
        assert_eq!(h.len(), 253); // .1–.254 without ourselves
        assert_eq!(h.first(), Some(&Ipv4Addr::new(192, 168, 8, 1)));
        assert_eq!(h.last(), Some(&Ipv4Addr::new(192, 168, 8, 254)));
        assert!(!h.contains(&Ipv4Addr::new(192, 168, 8, 12)));
        // larger networks are cut to the /24 around us
        assert_eq!(hosts(Ipv4Addr::new(10, 0, 3, 7), 16).len(), 253);
    }

    #[test]
    fn parses_typed_addresses() {
        assert_eq!(parse_address(" 192.168.8.17 "), Some("192.168.8.17".parse().unwrap()));
        assert_eq!(parse_address("192.168.8.17:8009"), Some("192.168.8.17".parse().unwrap()));
        assert_eq!(parse_address("127.0.0.1"), None);
        assert_eq!(parse_address("tv.local"), None);
        assert_eq!(parse_address("192.168.8"), None);
    }
}
