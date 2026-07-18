//! RTP-Sender: interleavte Samples → RTP-Pakete → Netz.
//!
//! Nimmt Bloecke interleavter i32-Samples (wie sie ein `AudioBackend` liefert),
//! zerlegt sie in paketgrosse Portionen (Level A: 48 Frames = 1 ms) und schickt
//! je ein RTP-Paket. Sequence-Nummer und Media-Clock-Timestamp laufen korrekt
//! mit. Die Paketierung selbst liegt im Kern ([`taktwerk_core::rtp`]) — hier ist
//! nur der zustandsbehaftete Sende-Loop.

use std::io;
use std::net::SocketAddr;

use taktwerk_core::rtp::{self, RtpHeader, RTP_HEADER_MIN_LEN};
use taktwerk_core::StreamProfile;
use tokio::net::UdpSocket;

/// Zustandsbehafteter RTP-Sender fuer genau einen Stream.
pub struct RtpSender {
    socket: UdpSocket,
    dest: SocketAddr,
    profile: StreamProfile,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    /// Wiederverwendeter Sendepuffer (Header + Payload), keine Allokation pro Paket.
    packet: Vec<u8>,
}

impl RtpSender {
    /// Erzeugt einen Sender. `ssrc` identifiziert die Quelle; `timestamp_start`
    /// ist der Media-Clock-Startwert (oft 0 oder ein zufaelliger Offset).
    pub fn new(
        socket: UdpSocket,
        dest: SocketAddr,
        profile: StreamProfile,
        payload_type: u8,
        ssrc: u32,
        timestamp_start: u32,
    ) -> Self {
        let cap = RTP_HEADER_MIN_LEN + profile.payload_bytes();
        Self {
            socket,
            dest,
            profile,
            payload_type,
            ssrc,
            sequence: 0,
            timestamp: timestamp_start,
            packet: vec![0u8; cap],
        }
    }

    /// Aktuelle Sequence-Nummer (naechstes Paket).
    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    /// Aktueller Media-Clock-Timestamp.
    pub fn timestamp(&self) -> u32 {
        self.timestamp
    }

    /// Samples pro vollem Paket (frames_per_packet * channels).
    fn samples_per_packet(&self) -> usize {
        self.profile.frames_per_packet() as usize * self.profile.channels as usize
    }

    /// Sendet einen Block interleavter Samples als eine Folge von RTP-Paketen.
    /// Die Blocklaenge muss ein ganzzahliges Vielfaches der Paketgroesse sein
    /// (frames_per_packet * channels); sonst [`io::ErrorKind::InvalidInput`].
    pub async fn send_block(&mut self, samples: &[i32]) -> io::Result<usize> {
        let per_pkt = self.samples_per_packet();
        if per_pkt == 0 || samples.len() % per_pkt != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Blocklaenge ist kein Vielfaches der Paketgroesse",
            ));
        }
        let mut sent = 0;
        for chunk in samples.chunks_exact(per_pkt) {
            self.send_one(chunk).await?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Sendet genau ein Paket (chunk == samples_per_packet Samples).
    async fn send_one(&mut self, chunk: &[i32]) -> io::Result<()> {
        let header = RtpHeader {
            marker: false,
            payload_type: self.payload_type,
            sequence: self.sequence,
            timestamp: self.timestamp,
            ssrc: self.ssrc,
        };
        header
            .write(&mut self.packet)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let payload_len = rtp::encode_payload(
            chunk,
            self.profile.encoding,
            &mut self.packet[RTP_HEADER_MIN_LEN..],
        )
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        let total = RTP_HEADER_MIN_LEN + payload_len;
        self.socket.send_to(&self.packet[..total], self.dest).await?;

        // Zustand fortschreiben: Sequence +1 (wrap), Timestamp += Frames/Paket.
        self.sequence = self.sequence.wrapping_add(1);
        self.timestamp = self
            .timestamp
            .wrapping_add(self.profile.frames_per_packet());
        Ok(())
    }
}
