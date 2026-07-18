//! PTP-Netz-Client: empfängt IEEE-1588-Nachrichten (Multicast 224.0.1.129,
//! Ports 319/320) und liefert sie geparst ([`taktwerk_core::ptp::wire`]).
//!
//! PTP nutzt zwei Ports: **319** (Event: Sync, Delay_Req) und **320** (General:
//! Announce, Follow_Up, Delay_Resp). Der Listener joint beide und mischt sie.
//! Aus Announce entsteht ein BMCA-Datensatz; Sync/Follow_Up speisen den Servo
//! ([`taktwerk_core::ptp::servo`]). Das Timestamping ist hier Software (lokale
//! Empfangszeit); HW-Timestamping (Linux `SO_TIMESTAMPING`) ist eine spätere,
//! optionale Verfeinerung hinter derselben Struktur.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use taktwerk_core::ptp::wire::{Announce, MessageType, PtpHeader, TimestampedMsg};
use taktwerk_core::ptp::ClockIdentity;
use tokio::net::UdpSocket;

use crate::multicast::{bind_receiver, MulticastConfig};

/// PTP-Multicast-Adresse (primär) und Ports.
pub const PTP_MULTICAST: &str = "224.0.1.129";
pub const PTP_EVENT_PORT: u16 = 319;
pub const PTP_GENERAL_PORT: u16 = 320;

fn ptp_config(interface: Ipv4Addr, port: u16) -> MulticastConfig {
    let group: Ipv4Addr = PTP_MULTICAST
        .parse()
        .expect("PTP_MULTICAST konstant gültig");
    MulticastConfig::new(group, port).with_interface(interface)
}

/// Bindet den Event-Socket (Port 319) und tritt der PTP-Gruppe bei.
pub fn bind_ptp_event(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    bind_receiver(&ptp_config(interface, PTP_EVENT_PORT))
}

/// Bindet den General-Socket (Port 320) und tritt der PTP-Gruppe bei.
pub fn bind_ptp_general(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    bind_receiver(&ptp_config(interface, PTP_GENERAL_PORT))
}

/// Eine empfangene, klassifizierte PTP-Nachricht.
#[derive(Debug, Clone)]
pub enum PtpMessage {
    Announce(Announce),
    Sync(TimestampedMsg),
    FollowUp(TimestampedMsg),
    /// Anderer Typ (Delay_Resp, Management, …) — nur der Header.
    Other(PtpHeader),
}

impl PtpMessage {
    /// Kurzname des Nachrichtentyps (für Logs).
    pub fn kind(&self) -> &'static str {
        match self {
            PtpMessage::Announce(_) => "Announce",
            PtpMessage::Sync(_) => "Sync",
            PtpMessage::FollowUp(_) => "Follow_Up",
            PtpMessage::Other(_) => "Other",
        }
    }

    /// Clock-Identity des Absenders (aus dem Header).
    pub fn source_identity(&self) -> ClockIdentity {
        match self {
            PtpMessage::Announce(a) => a.header.source_port.clock_identity,
            PtpMessage::Sync(m) | PtpMessage::FollowUp(m) => m.header.source_port.clock_identity,
            PtpMessage::Other(h) => h.source_port.clock_identity,
        }
    }
}

/// Empfängt PTP-Nachrichten von beiden Ports (319 Event, 320 General).
pub struct PtpListener {
    event: UdpSocket,
    general: UdpSocket,
    buf_event: Vec<u8>,
    buf_general: Vec<u8>,
}

impl PtpListener {
    pub fn new(event: UdpSocket, general: UdpSocket) -> Self {
        Self {
            event,
            general,
            buf_event: vec![0u8; 1500],
            buf_general: vec![0u8; 1500],
        }
    }

    /// Bindet beide PTP-Sockets auf einem Interface und baut den Listener.
    pub fn bind(interface: Ipv4Addr) -> io::Result<Self> {
        Ok(Self::new(
            bind_ptp_event(interface)?,
            bind_ptp_general(interface)?,
        ))
    }

    /// Wartet auf die nächste (parsebare) PTP-Nachricht von einem der Ports.
    /// Gibt Nachricht, Absender und Datagramm-Größe (Bytes) zurück.
    pub async fn recv(&mut self) -> io::Result<(PtpMessage, SocketAddr, usize)> {
        loop {
            let (datagram, from) = tokio::select! {
                r = self.event.recv_from(&mut self.buf_event) => {
                    let (n, from) = r?;
                    (&self.buf_event[..n], from)
                }
                r = self.general.recv_from(&mut self.buf_general) => {
                    let (n, from) = r?;
                    (&self.buf_general[..n], from)
                }
            };
            let n = datagram.len();
            match classify(datagram) {
                Some(msg) => {
                    tracing::debug!(%from, kind = msg.kind(), bytes = n, "PTP-Nachricht empfangen");
                    return Ok((msg, from, n));
                }
                None => continue, // unparsebar / uninteressant → weiterhören
            }
        }
    }
}

/// Klassifiziert ein Datagramm anhand seines PTP-Headers.
fn classify(datagram: &[u8]) -> Option<PtpMessage> {
    let header = PtpHeader::parse(datagram).ok()?;
    match header.message_type {
        MessageType::Announce => Announce::parse(datagram).ok().map(PtpMessage::Announce),
        MessageType::Sync => TimestampedMsg::parse(datagram).ok().map(PtpMessage::Sync),
        MessageType::FollowUp => TimestampedMsg::parse(datagram)
            .ok()
            .map(PtpMessage::FollowUp),
        _ => Some(PtpMessage::Other(header)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taktwerk_core::ptp::wire::{PortIdentity, PtpTimestamp};

    fn announce_bytes() -> Vec<u8> {
        let ann = Announce {
            header: PtpHeader {
                message_type: MessageType::Announce,
                version: 2,
                message_length: Announce::LEN as u16,
                domain: 0,
                flags: 0,
                correction: 0,
                source_port: PortIdentity {
                    clock_identity: [0xAB; 8],
                    port: 1,
                },
                sequence_id: 1,
                control: 5,
                log_message_interval: 1,
            },
            origin_timestamp: PtpTimestamp::default(),
            current_utc_offset: 37,
            gm_priority1: 128,
            gm_clock_class: 6,
            gm_clock_accuracy: 0x21,
            gm_offset_scaled_log_variance: 0,
            gm_priority2: 128,
            gm_identity: [0xAB; 8],
            steps_removed: 0,
            time_source: 0x20,
        };
        let mut buf = vec![0u8; Announce::LEN];
        ann.write(&mut buf).unwrap();
        buf
    }

    #[test]
    fn classify_announce() {
        let buf = announce_bytes();
        match classify(&buf) {
            Some(PtpMessage::Announce(a)) => {
                assert_eq!(a.gm_clock_class, 6);
                assert_eq!(a.to_clock_dataset().clock_identity, [0xAB; 8]);
            }
            other => panic!("erwartete Announce, bekam {other:?}"),
        }
    }

    #[test]
    fn classify_ignores_garbage() {
        assert!(classify(&[0u8; 4]).is_none()); // zu kurz für Header
    }
}
