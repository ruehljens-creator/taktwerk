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
pub mod jitter;
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

/// Maximale Kanalzahl, die Taktwerk pro Stream in einen **Standard-MTU**-Rahmen
/// (1500 B) packt: 64 Kanäle @ 125 µs = 1152 B Payload (siehe [`StreamProfile::aes67`]).
pub const MAX_CHANNELS: u8 = 64;

impl StreamProfile {
    /// AES67 / ST-2110-30 **Level A**: 48 kHz, L24, 1 ms Paketzeit (≤ 8 Kanäle).
    /// Interop-sichere Basis; für höhere Kanalzahlen siehe [`Self::aes67`].
    pub const fn level_a(channels: u8) -> Self {
        Self {
            sample_rate: 48_000,
            channels,
            ptime_us: 1_000,
            encoding: Encoding::L24,
        }
    }

    /// Wählt die **Paketzeit passend zur Kanalzahl**, so dass ein RTP-Paket immer
    /// in einen Standard-Ethernet-Rahmen passt (kein Jumbo nötig) — genau wie
    /// RAVENNA/Dante bei hohen Kanalzahlen. Alle Stufen ergeben 1152 B Payload:
    ///
    /// | Kanäle | ptime  | Frames |
    /// |--------|--------|--------|
    /// | ≤ 8    | 1 ms   | 48     |
    /// | ≤ 16   | 500 µs | 24     |
    /// | ≤ 32   | 250 µs | 12     |
    /// | ≤ 64   | 125 µs | 6      |
    ///
    /// 48 kHz / L24. Bei ≤ 8 Kanälen identisch zu [`Self::level_a`] (voll AES67-
    /// Level-A-kompatibel). Kanäle > [`MAX_CHANNELS`] werden auf 64 begrenzt.
    pub const fn aes67(channels: u8) -> Self {
        let ptime_us = if channels <= 8 {
            1_000
        } else if channels <= 16 {
            500
        } else if channels <= 32 {
            250
        } else {
            125
        };
        let channels = if channels > MAX_CHANNELS {
            MAX_CHANNELS
        } else {
            channels
        };
        Self {
            sample_rate: 48_000,
            channels,
            ptime_us,
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
    fn aes67_high_channel_counts_fit_standard_mtu() {
        // Alle Stufen müssen unter die nutzbare Standard-MTU passen (< 1460 B).
        for ch in [2u8, 8, 16, 32, 64] {
            let p = StreamProfile::aes67(ch);
            assert_eq!(p.channels, ch);
            assert!(
                p.payload_bytes() <= 1440,
                "{ch}ch: {} B zu groß für Standard-MTU",
                p.payload_bytes()
            );
        }
        // Bekannte Eckpunkte.
        assert_eq!(StreamProfile::aes67(8).ptime_us, 1_000);
        assert_eq!(StreamProfile::aes67(32).ptime_us, 250);
        assert_eq!(StreamProfile::aes67(64).ptime_us, 125);
        assert_eq!(StreamProfile::aes67(64).payload_bytes(), 1152);
        // ≤ 8 Kanäle: identisch zu Level A.
        assert_eq!(StreamProfile::aes67(2), StreamProfile::level_a(2));
        // Über dem Maximum wird gedeckelt.
        assert_eq!(StreamProfile::aes67(200).channels, MAX_CHANNELS);
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
