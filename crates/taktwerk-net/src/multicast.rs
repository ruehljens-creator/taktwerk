//! Multicast-UDP-Sockets fuer AES67.
//!
//! Kapselt die (leicht OS-abhaengigen) Socket-Optionen hinter zwei Helfern:
//! [`bind_receiver`] (bindet + tritt der Gruppe bei) und [`bind_sender`]
//! (waehlt Interface + TTL). Beide liefern eine `tokio::net::UdpSocket`.
//!
//! Portabilitaets-Entscheidungen:
//! - **Bind an `0.0.0.0:port`** (nicht an die Gruppenadresse). Linux erlaubt
//!   den Bind an die Gruppe, Windows nicht — `INADDR_ANY` funktioniert ueberall.
//! - **`SO_REUSEADDR`** aktiv, damit mehrere Empfaenger dieselbe Gruppe/denselben
//!   Port teilen koennen (Standard bei AES67-Discovery/-Media).
//! - **IGMP:** Any-Source-Join (`join_multicast_v4`) deckt IGMPv2 **und** v3 ab
//!   (AES67-Pflicht ist nur v2). Source-Specific (SSM, IGMPv3) ist als spaetere
//!   Erweiterung vorgesehen — Struktur haelt das offen (`source`-Feld folgt dann).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

/// Konfiguration einer Multicast-Verbindung (IPv4).
#[derive(Debug, Clone, Copy)]
pub struct MulticastConfig {
    /// Multicast-Gruppenadresse (z. B. 239.69.83.67).
    pub group: Ipv4Addr,
    /// UDP-Port (AES67-Media typ. 5004).
    pub port: u16,
    /// Lokales Interface fuer Join/Send. `0.0.0.0` = vom OS gewaehltes Default-IF.
    pub interface: Ipv4Addr,
    /// Multicast-TTL beim Senden (Hop-Reichweite). Default via [`Self::new`].
    pub ttl: u32,
}

impl MulticastConfig {
    /// Config mit sinnvollem Default-TTL (32 = Site-Scope).
    pub fn new(group: Ipv4Addr, port: u16) -> Self {
        Self {
            group,
            port,
            interface: Ipv4Addr::UNSPECIFIED,
            ttl: 32,
        }
    }

    /// Setzt das lokale Interface (Builder-Stil).
    pub fn with_interface(mut self, iface: Ipv4Addr) -> Self {
        self.interface = iface;
        self
    }

    /// Setzt die Multicast-TTL (Builder-Stil).
    pub fn with_ttl(mut self, ttl: u32) -> Self {
        self.ttl = ttl;
        self
    }

    /// Zieladresse (Gruppe:Port) fuer `send_to`.
    pub fn dest(&self) -> SocketAddr {
        SocketAddr::from(SocketAddrV4::new(self.group, self.port))
    }
}

/// Gemeinsame Basis: gebundener socket2-UDP-Socket mit `SO_REUSEADDR`,
/// nonblocking. Multicast-Optionen setzen die Aufrufer noch auf `socket2`
/// (nicht alle davon existieren auf `tokio::net::UdpSocket`).
fn base_socket(bind: SocketAddrV4) -> io::Result<Socket> {
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    sock.bind(&SocketAddr::from(bind).into())?;
    sock.set_nonblocking(true)?;
    Ok(sock)
}

/// Wandelt einen fertig konfigurierten socket2-Socket in einen tokio-Socket.
fn into_tokio(sock: Socket) -> io::Result<UdpSocket> {
    let std_sock: std::net::UdpSocket = sock.into();
    UdpSocket::from_std(std_sock)
}

/// Bindet einen **Empfangs-Socket** an `0.0.0.0:port` und tritt der Multicast-
/// Gruppe auf dem gewaehlten Interface bei (IGMP-Join).
pub fn bind_receiver(cfg: &MulticastConfig) -> io::Result<UdpSocket> {
    let sock = base_socket(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, cfg.port))?;
    sock.join_multicast_v4(&cfg.group, &cfg.interface)?;
    into_tokio(sock)
}

/// Bindet einen **Sende-Socket** (ephemerer Port), setzt Interface und TTL.
/// `multicast_loop` = true lässt lokal gesendete Pakete auch lokal empfangen
/// (nuetzlich fuer Tests und Ein-Host-Setups).
///
/// **Wichtig (Portabilitaet):** Der Socket bindet an `0.0.0.0`, NICHT an die
/// Interface-IP. Auf macOS führt ein an die Interface-IP gebundener Socket sonst
/// zu `No route to host`, wenn das OS die Multicast-Gruppe über ein *anderes*
/// Interface routet. Die Egress-Wahl macht `IP_MULTICAST_IF`
/// ([`Socket::set_multicast_if_v4`]).
///
/// **macOS/Apple zusätzlich:** Auf einem multi-homed Mac reicht `IP_MULTICAST_IF`
/// nicht — macOS nutzt *scoped routing*: eine an ein Interface gebundene Route
/// (Flag `IFSCOPE`) greift nur, wenn der Socket auch per **Interface-Index**
/// gebunden ist (`IP_BOUND_IF`). Ohne das schlägt Multicast über ein Nicht-
/// Default-Interface mit `No route to host (os error 65)` fehl. Wir binden den
/// Sender daher auf Apple zusätzlich per Index (aus der Interface-IP ermittelt).
/// Auf Linux/Windows ist dieser Block wegkompiliert.
pub fn bind_sender(cfg: &MulticastConfig, multicast_loop: bool) -> io::Result<UdpSocket> {
    let sock = base_socket(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_multicast_ttl_v4(cfg.ttl)?;
    sock.set_multicast_loop_v4(multicast_loop)?;
    if !cfg.interface.is_unspecified() {
        sock.set_multicast_if_v4(&cfg.interface)?;
        bind_to_interface_index(&sock, cfg.interface);
    }
    into_tokio(sock)
}

/// Bindet den Socket auf Apple-Plattformen zusätzlich per **Interface-Index**
/// (`IP_BOUND_IF`), damit macOS' scoped/IFSCOPE-Route zum Tragen kommt. Auf allen
/// anderen Plattformen ein No-op (leere Funktion, wegoptimiert).
#[cfg(target_vendor = "apple")]
fn bind_to_interface_index(sock: &Socket, iface: Ipv4Addr) {
    use std::os::unix::io::AsRawFd;
    // IP_BOUND_IF aus <netinet/in.h> (nicht in libc als Konstante exportiert).
    const IP_BOUND_IF: libc::c_int = 25;

    let Some(idx) = ifindex_for_ipv4(iface) else {
        tracing::warn!(%iface, "macOS: kein Interface-Index zur IP gefunden");
        return;
    };
    let idx: libc::c_uint = idx;
    // SAFETY: gültiger Socket-FD; wir übergeben einen c_uint per Zeiger + korrekte Länge.
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_IP,
            IP_BOUND_IF,
            &idx as *const libc::c_uint as *const libc::c_void,
            std::mem::size_of::<libc::c_uint>() as libc::socklen_t,
        )
    };
    if rc != 0 {
        tracing::warn!(ifindex = idx, %iface, "IP_BOUND_IF setsockopt: {}", io::Error::last_os_error());
    } else {
        tracing::debug!(ifindex = idx, %iface, "macOS: Sender an Interface-Index gebunden (IP_BOUND_IF)");
    }
}

#[cfg(not(target_vendor = "apple"))]
#[inline]
fn bind_to_interface_index(_sock: &Socket, _iface: Ipv4Addr) {}

/// Ermittelt den Interface-Index zu einer IPv4-Adresse via `getifaddrs` +
/// `if_nametoindex` (nur Apple; für die IP_BOUND_IF-Bindung).
#[cfg(target_vendor = "apple")]
fn ifindex_for_ipv4(ip: Ipv4Addr) -> Option<u32> {
    // SAFETY: getifaddrs/freeifaddrs-Paar; wir lesen nur, folgen der ifa_next-
    // Liste bis NULL und geben die Liste am Ende wieder frei.
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut found = None;
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_addr.is_null()
                && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET
                && !ifa.ifa_name.is_null()
            {
                let sin = ifa.ifa_addr as *const libc::sockaddr_in;
                let addr = Ipv4Addr::from(u32::from_be((*sin).sin_addr.s_addr));
                if addr == ip {
                    let idx = libc::if_nametoindex(ifa.ifa_name);
                    if idx != 0 {
                        found = Some(idx);
                        break;
                    }
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        found
    }
}

/// Verlaesst eine zuvor beigetretene Multicast-Gruppe (IGMP-Leave).
pub fn leave(socket: &UdpSocket, cfg: &MulticastConfig) -> io::Result<()> {
    socket.leave_multicast_v4(cfg.group, cfg.interface)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_builder_and_dest() {
        let cfg = MulticastConfig::new(Ipv4Addr::new(239, 69, 83, 67), 5004)
            .with_ttl(8)
            .with_interface(Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(cfg.ttl, 8);
        assert_eq!(cfg.interface, Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(
            cfg.dest(),
            "239.69.83.67:5004".parse::<SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn receiver_can_join_group() {
        // Join auf dem Default-IF darf nicht fehlschlagen (reiner Socket-Test).
        let cfg = MulticastConfig::new(Ipv4Addr::new(239, 199, 199, 199), 0);
        // Port 0 → OS waehlt; Join trotzdem gueltig.
        let cfg = MulticastConfig { port: 0, ..cfg };
        let sock = bind_receiver(&cfg);
        assert!(
            sock.is_ok(),
            "join_multicast_v4 schlug fehl: {:?}",
            sock.err()
        );
    }

    #[tokio::test]
    async fn sender_binds_with_ttl() {
        let cfg = MulticastConfig::new(Ipv4Addr::new(239, 199, 199, 200), 0).with_ttl(4);
        let sock = bind_sender(&cfg, true);
        assert!(sock.is_ok(), "sender-bind schlug fehl: {:?}", sock.err());
    }
}
