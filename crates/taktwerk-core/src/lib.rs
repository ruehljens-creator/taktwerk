//! # taktwerk-core
//!
//! Der **plattformneutrale Kern** von Taktwerk. Enthaelt die gesamte AES67-/
//! ST-2110-30-Protokoll- und DSP-Logik als reine, unit-testbare Bausteine —
//! **ohne jede OS- oder Netzwerk-Abhaengigkeit**. Damit kompiliert und testet
//! der Kern identisch auf Linux, macOS und Windows.
//!
//! Was hier NICHT liegt (bewusst): Sockets/Multicast (→ `taktwerk-net`),
//! virtuelle Soundkarte (→ `taktwerk-audio`), mDNS-Discovery, PTP-Timestamping
//! aus dem OS. Diese OS-Naehte sind Traits; der Kern liefert die Datentypen und
//! Algorithmen, die dahinter laufen.
//!
//! ## Module
//! - [`rtp`]   — RTP-Header + L24/L16-Payload (Pack/Depack)
//! - [`sdp`]   — SDP fuer AES67/ST2110-30 Level A (Build/Parse)
//! - [`sap`]   — SAP-Announce/-Parse (Session Announcement Protocol)
//! - [`ptp`]   — IEEE-1588-Datentypen + BMCA-Zustand
//! - [`dsp`]   — ASRC / Clock-Recovery-Servo
//! - [`clock`] — [`clock::TimeSource`]-Naht: Media-Clock/RTP-Timestamps

pub mod clock;
pub mod dsp;
pub mod ptp;
pub mod rtp;
pub mod sap;
pub mod sdp;

/// Das gemeinsame Zielprofil des MVP: AES67-Pflichtbasis == ST-2110-30 Level A.
/// Sendeseitig konservativ auf diese Werte festgelegt (§7.1 des Projektbriefs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamProfile {
    /// Abtastrate in Hz (Level A: 48000).
    pub sample_rate: u32,
    /// Kanaele pro Stream (Level A: <= 8).
    pub channels: u8,
    /// Paketzeit in Mikrosekunden (Level A: 1000 = 1 ms).
    pub ptime_us: u32,
    /// Bittiefe des Payloads.
    pub encoding: Encoding,
}

/// Payload-Codierung nach AES67. L24 ist die Level-A-Basis, L16 optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// 24-bit linear PCM, big-endian, interleaved.
    L24,
    /// 16-bit linear PCM, big-endian, interleaved.
    L16,
}

impl Encoding {
    /// Bytes pro Sample (ein Kanal, ein Frame).
    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Encoding::L24 => 3,
            Encoding::L16 => 2,
        }
    }

    /// Name fuer die SDP-`rtpmap`-Zeile (z. B. `L24`).
    pub const fn rtpmap_name(self) -> &'static str {
        match self {
            Encoding::L24 => "L24",
            Encoding::L16 => "L16",
        }
    }
}

impl StreamProfile {
    /// AES67 / ST-2110-30 **Level A**: 48 kHz, L24, 1 ms Paketzeit.
    pub const fn level_a(channels: u8) -> Self {
        Self {
            sample_rate: 48_000,
            channels,
            ptime_us: 1_000,
            encoding: Encoding::L24,
        }
    }

    /// Anzahl der Frames (Sample-Tupel ueber alle Kanaele) pro RTP-Paket,
    /// abgeleitet aus Abtastrate und Paketzeit. Level A @48k/1ms = 48.
    pub const fn frames_per_packet(&self) -> u32 {
        // ptime_us * sample_rate / 1_000_000
        (self.sample_rate as u64 * self.ptime_us as u64 / 1_000_000) as u32
    }

    /// Nutzlast-Groesse eines vollen RTP-Pakets in Bytes.
    pub const fn payload_bytes(&self) -> usize {
        self.frames_per_packet() as usize
            * self.channels as usize
            * self.encoding.bytes_per_sample()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_a_frames_and_payload() {
        let p = StreamProfile::level_a(8);
        assert_eq!(p.frames_per_packet(), 48); // 48 kHz * 1 ms
                                               // 48 Frames * 8 ch * 3 Byte (L24) = 1152 Byte
        assert_eq!(p.payload_bytes(), 1152);
    }

    #[test]
    fn stereo_l16_payload() {
        let p = StreamProfile {
            sample_rate: 48_000,
            channels: 2,
            ptime_us: 1_000,
            encoding: Encoding::L16,
        };
        assert_eq!(p.frames_per_packet(), 48);
        assert_eq!(p.payload_bytes(), 48 * 2 * 2);
    }
}
