//! SAP-Discovery über das Netz: den eigenen Stream ankündigen und fremde
//! Ankündigungen einsammeln.
//!
//! Baut auf [`taktwerk_core::sap`] (SAP-Header) und [`taktwerk_core::sdp`]
//! (SDP-Payload) auf und verdrahtet beides mit UDP-Multicast. AES67-Altgeräte
//! ohne NMOS finden Streams genau so.
//!
//! - [`SapAnnouncer`] — sendet Announce/Deletion für eine [`AudioSession`].
//! - [`SapListener`]  — empfängt Announcements und liefert [`SapEvent`]s.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use taktwerk_core::sap::{SapPacket, SAP_MULTICAST, SAP_PORT};
use taktwerk_core::sdp::AudioSession;
use tokio::net::UdpSocket;

use crate::multicast::{bind_receiver, bind_sender, MulticastConfig};

/// MIME-Typ des SAP-Payloads.
const SDP_MIME: &str = "application/sdp";

/// SAP-Multicast-Config (well-known Gruppe/Port) für ein Interface.
fn sap_config(interface: Ipv4Addr) -> MulticastConfig {
    let group: Ipv4Addr = SAP_MULTICAST
        .parse()
        .expect("SAP_MULTICAST konstant gültig");
    MulticastConfig::new(group, SAP_PORT).with_interface(interface)
}

/// Bindet einen Sende-Socket für SAP-Announcements.
pub fn bind_sap_announcer(interface: Ipv4Addr, multicast_loop: bool) -> io::Result<UdpSocket> {
    bind_sender(&sap_config(interface), multicast_loop)
}

/// Bindet einen Empfangs-Socket und tritt der SAP-Gruppe bei.
pub fn bind_sap_listener(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    bind_receiver(&sap_config(interface))
}

/// Kündigt eine [`AudioSession`] periodisch per SAP an.
pub struct SapAnnouncer {
    socket: UdpSocket,
    dest: SocketAddr,
    source: [u8; 4],
    msg_id_hash: u16,
    sdp: String,
}

impl SapAnnouncer {
    /// `source_ip` ist die eigene Absenderadresse (steht im SAP-Header und in der
    /// SDP-`o=`-Zeile). Der `msg_id_hash` wird aus dem SDP-Inhalt abgeleitet, so
    /// dass sich Empfänger denselben Stream über wiederholte Announcements hinweg
    /// merken und eine Inhaltsänderung erkennen können.
    pub fn new(socket: UdpSocket, source_ip: Ipv4Addr, session: &AudioSession) -> Self {
        let sdp = session.to_sdp();
        let msg_id_hash = fnv16(sdp.as_bytes());
        Self {
            socket,
            dest: SocketAddr::from((SAP_MULTICAST.parse::<Ipv4Addr>().unwrap(), SAP_PORT)),
            source: source_ip.octets(),
            msg_id_hash,
            sdp,
        }
    }

    /// Der SAP-Message-ID-Hash dieses Announcers (identifiziert den Stream).
    pub fn msg_id_hash(&self) -> u16 {
        self.msg_id_hash
    }

    async fn send(&self, announce: bool) -> io::Result<()> {
        let pkt = SapPacket {
            announce,
            msg_id_hash: self.msg_id_hash,
            source: self.source,
            payload_type: Some(SDP_MIME),
            payload: self.sdp.as_bytes(),
        };
        let bytes = pkt.to_bytes();
        self.socket.send_to(&bytes, self.dest).await?;
        tracing::debug!(
            hash = self.msg_id_hash,
            announce,
            bytes = bytes.len(),
            "SAP gesendet"
        );
        Ok(())
    }

    /// Sendet ein Announcement (Session existiert / ist aktuell).
    pub async fn announce(&self) -> io::Result<()> {
        self.send(true).await
    }

    /// Sendet eine Deletion (Session wird zurückgezogen).
    pub async fn delete(&self) -> io::Result<()> {
        self.send(false).await
    }
}

/// Ein empfangenes SAP-Ereignis.
#[derive(Debug, Clone)]
pub struct SapEvent {
    /// true = Announce, false = Deletion.
    pub announce: bool,
    /// Message-ID-Hash (Stream-Instanz).
    pub msg_id_hash: u16,
    /// Absender laut SAP-Header.
    pub source: [u8; 4],
    /// Absenderadresse des Datagramms.
    pub from: SocketAddr,
    /// Größe des UDP-Datagramms in Bytes (für Traffic-Zählung).
    pub bytes: usize,
    /// Geparste Session (None, wenn der SDP-Payload nicht lesbar war).
    pub session: Option<AudioSession>,
}

/// Empfängt SAP-Announcements aus dem Netz.
pub struct SapListener {
    socket: UdpSocket,
    buf: Vec<u8>,
}

impl SapListener {
    pub fn new(socket: UdpSocket) -> Self {
        // SAP-Announcements sind klein; 4 KiB reicht mit Reserve für große SDPs.
        Self {
            socket,
            buf: vec![0u8; 4096],
        }
    }

    /// Wartet auf das nächste SAP-Datagramm und liefert es als [`SapEvent`].
    /// Ein nicht lesbarer SDP-Payload macht das Event nicht ungültig — dann ist
    /// `session == None` (tolerant, wie ein AES67-Empfänger).
    pub async fn recv(&mut self) -> io::Result<SapEvent> {
        let (n, from) = self.socket.recv_from(&mut self.buf).await?;
        let datagram = &self.buf[..n];
        let pkt = SapPacket::parse(datagram)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
        let session = AudioSession::parse(&String::from_utf8_lossy(pkt.payload)).ok();
        Ok(SapEvent {
            announce: pkt.announce,
            msg_id_hash: pkt.msg_id_hash,
            source: pkt.source,
            from,
            bytes: n,
            session,
        })
    }
}

/// Kleiner FNV-1a-16-Hash (gefaltet) für die SAP-Message-ID.
fn fnv16(bytes: &[u8]) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    ((h >> 16) ^ (h & 0xFFFF)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use taktwerk_core::sdp::{AudioSession, PtpRefClock};
    use taktwerk_core::StreamProfile;

    fn session() -> AudioSession {
        AudioSession {
            session_name: "Taktwerk Test".into(),
            origin_unicast: "192.168.1.20".into(),
            multicast_addr: "239.69.83.67".into(),
            port: 5004,
            payload_type: 97,
            profile: StreamProfile::level_a(2),
            refclk: Some(PtpRefClock {
                gmid: "00-11-22-FF-FE-33-44-55".into(),
                domain: 0,
            }),
            mediaclk_offset: 0,
        }
    }

    #[test]
    fn hash_is_stable_and_content_sensitive() {
        let s = session();
        let a = fnv16(s.to_sdp().as_bytes());
        let b = fnv16(s.to_sdp().as_bytes());
        assert_eq!(a, b, "gleicher Inhalt → gleicher Hash");

        let mut s2 = session();
        s2.port = 5006;
        let c = fnv16(s2.to_sdp().as_bytes());
        assert_ne!(a, c, "geänderter Inhalt → anderer Hash");
    }

    /// End-to-End über Unicast-Loopback: Announcer → Listener (ohne die
    /// well-known SAP-Gruppe, damit der Test kein Multicast-Routing braucht).
    #[tokio::test]
    async fn announce_is_received_and_parsed() {
        let sess = session();

        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let mut listener = SapListener::new(rx_sock);

        // Announcer, der direkt an den Listener-Port sendet.
        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut ann = SapAnnouncer::new(tx_sock, Ipv4Addr::new(192, 168, 1, 20), &sess);
        ann.dest = rx_addr; // Test-Override: an den Loopback-Listener

        ann.announce().await.unwrap();

        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv())
            .await
            .expect("timeout")
            .unwrap();

        assert!(ev.announce);
        assert_eq!(ev.msg_id_hash, ann.msg_id_hash());
        let got = ev.session.expect("SDP sollte parsebar sein");
        assert_eq!(got.multicast_addr, "239.69.83.67");
        assert_eq!(got.port, 5004);
        assert_eq!(got.profile, StreamProfile::level_a(2));
    }

    #[tokio::test]
    async fn deletion_flag_survives() {
        let sess = session();
        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let mut listener = SapListener::new(rx_sock);
        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut ann = SapAnnouncer::new(tx_sock, Ipv4Addr::new(10, 0, 0, 1), &sess);
        ann.dest = rx_addr;

        ann.delete().await.unwrap();
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), listener.recv())
            .await
            .expect("timeout")
            .unwrap();
        assert!(!ev.announce);
    }
}
