//! RTP-Receiver: Netz → RTP-Parse → Samples.
//!
//! Empfaengt UDP-Datagramme, parst den RTP-Header ([`taktwerk_core::rtp`]) und
//! dekodiert den L24/L16-Payload in interleavte i32-Samples. Der Receiver ist
//! bewusst schlank: Er liefert Paket fuer Paket mit Header (Sequence/Timestamp),
//! damit die naechste Stufe (Jitter-Puffer/ASRC, spaeter) Reihenfolge und
//! Luecken selbst behandeln kann.

use std::io;
use std::net::SocketAddr;

use taktwerk_core::rtp::{self, RtpHeader};
use taktwerk_core::StreamProfile;
use tokio::net::UdpSocket;

/// Ein empfangenes, dekodiertes RTP-Paket.
#[derive(Debug, Clone)]
pub struct ReceivedPacket {
    pub header: RtpHeader,
    /// Absenderadresse (Quelle) des Datagramms.
    pub from: SocketAddr,
    /// Interleavte i32-Samples (linksbuendig, vgl. `rtp::decode_payload`).
    pub samples: Vec<i32>,
}

impl ReceivedPacket {
    /// Anzahl Frames (Sample-Tupel ueber alle Kanaele) im Paket.
    pub fn frames(&self, channels: u8) -> usize {
        if channels == 0 {
            0
        } else {
            self.samples.len() / channels as usize
        }
    }
}

/// RTP-Empfaenger fuer einen Stream mit bekanntem Profil.
pub struct RtpReceiver {
    socket: UdpSocket,
    profile: StreamProfile,
    /// Empfangspuffer, gross genug fuer ein volles Paket + Reserve fuer
    /// abweichende Sender (Header, evtl. CSRC/Extension).
    buf: Vec<u8>,
}

impl RtpReceiver {
    /// Erzeugt einen Receiver auf einem (typischerweise via [`crate::bind_receiver`]
    /// der Gruppe beigetretenen) Socket.
    pub fn new(socket: UdpSocket, profile: StreamProfile) -> Self {
        // 2 KiB deckt Level A (≤1152 Byte Payload + Header) mit Reserve ab.
        let cap = (rtp::RTP_HEADER_MIN_LEN + profile.payload_bytes() + 512).max(2048);
        Self {
            socket,
            profile,
            buf: vec![0u8; cap],
        }
    }

    /// Wartet auf das naechste RTP-Paket und gibt es dekodiert zurueck.
    /// Pakete mit fehlerhaftem Header werden als Fehler gemeldet, nicht
    /// verschluckt — die Aufrufebene entscheidet ueber Toleranz.
    pub async fn recv(&mut self) -> io::Result<ReceivedPacket> {
        let (n, from) = self.socket.recv_from(&mut self.buf).await?;
        let datagram = &self.buf[..n];
        let (header, payload_off) = RtpHeader::parse(datagram)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut samples = Vec::new();
        rtp::decode_payload(
            &datagram[payload_off..],
            self.profile.encoding,
            &mut samples,
        )
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(ReceivedPacket {
            header,
            from,
            samples,
        })
    }

    /// Lokale Socket-Adresse (nuetzlich fuer Tests/Diagnose).
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sender::RtpSender;
    use std::net::Ipv4Addr;

    /// End-to-End ueber Unicast-Loopback: Sender → Receiver, ohne Multicast-
    /// Routing. Prueft die komplette RTP-Framing-Pipeline (Header, L24-Payload,
    /// Sequence/Timestamp-Fortschritt).
    #[tokio::test]
    async fn loopback_roundtrip_l24() {
        let profile = StreamProfile::level_a(2);

        // Receiver an ephemerem Loopback-Port.
        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let mut rx = RtpReceiver::new(rx_sock, profile);

        // Sender an denselben Port.
        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut tx = RtpSender::new(tx_sock, rx_addr, profile, 97, 0x1234_5678, 1000);

        // Ein Paket voll: frames_per_packet * channels Samples, linksbuendig.
        let per_pkt = profile.frames_per_packet() as usize * profile.channels as usize;
        let block: Vec<i32> = (0..per_pkt as i32).map(|i| (i << 8) & !0xff).collect();

        let sent = tx.send_block(&block).await.unwrap();
        assert_eq!(sent, 1);

        let pkt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("timeout: kein Paket empfangen")
            .expect("recv-Fehler");

        assert_eq!(pkt.header.payload_type, 97);
        assert_eq!(pkt.header.sequence, 0);
        assert_eq!(pkt.header.timestamp, 1000);
        assert_eq!(pkt.header.ssrc, 0x1234_5678);
        assert_eq!(
            pkt.frames(profile.channels),
            profile.frames_per_packet() as usize
        );
        assert_eq!(pkt.samples, block);

        // Sender-Zustand ist fortgeschritten.
        assert_eq!(tx.sequence(), 1);
        assert_eq!(tx.timestamp(), 1000 + profile.frames_per_packet());
    }

    /// Ein Mehr-Paket-Block wird in mehrere RTP-Pakete mit fortlaufender
    /// Sequence zerlegt.
    #[tokio::test]
    async fn multi_packet_sequence_advances() {
        let profile = StreamProfile::level_a(2);
        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let mut rx = RtpReceiver::new(rx_sock, profile);

        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut tx = RtpSender::new(tx_sock, rx_addr, profile, 96, 1, 0);

        let per_pkt = profile.frames_per_packet() as usize * profile.channels as usize;
        let block = vec![0i32; per_pkt * 3]; // drei Pakete
        let sent = tx.send_block(&block).await.unwrap();
        assert_eq!(sent, 3);

        let mut seqs = Vec::new();
        for _ in 0..3 {
            let pkt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("timeout")
                .unwrap();
            seqs.push(pkt.header.sequence);
        }
        seqs.sort_unstable();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn ragged_block_is_rejected() {
        let profile = StreamProfile::level_a(2);
        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let dest: SocketAddr = (Ipv4Addr::LOCALHOST, 9).into();
        let mut tx = RtpSender::new(tx_sock, dest, profile, 96, 1, 0);
        // 5 Samples passen nicht in 2-Kanal-Pakete (96 Samples/Paket).
        let err = tx.send_block(&[0, 0, 0, 0, 0]).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
