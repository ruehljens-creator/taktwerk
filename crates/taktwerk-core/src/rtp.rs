//! RTP (RFC 3550) fuer AES67 — Header und L24/L16-Payload.
//!
//! Bewusst minimal und alloc-arm: Der Kern erzeugt/parst Header und wandelt
//! zwischen interleavten i32-Samples und dem big-endian-PCM-Payload. Das
//! Versenden (Sockets/Multicast) macht `taktwerk-net`.
//!
//! AES67 nutzt dynamische Payload-Types (96/97), keine CSRCs, keine Extensions
//! im Normalfall. Der Parser bleibt dennoch tolerant und ueberspringt CSRC-Liste
//! und Extension-Header, falls vorhanden.

use crate::Encoding;

/// Fester Teil des RTP-Headers (ohne CSRC/Extension) = 12 Byte.
pub const RTP_HEADER_MIN_LEN: usize = 12;

/// RTP-Version 2 (die einzige gebraeuchliche).
pub const RTP_VERSION: u8 = 2;

/// Fehler beim Parsen eines RTP-Pakets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpError {
    /// Puffer kuerzer als der minimale Header.
    TooShort,
    /// Versionsfeld != 2.
    BadVersion,
    /// Payload-Laenge passt nicht zu Kanalzahl * Bytes/Sample.
    RaggedPayload,
}

impl core::fmt::Display for RtpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            RtpError::TooShort => "RTP-Puffer zu kurz",
            RtpError::BadVersion => "RTP-Version != 2",
            RtpError::RaggedPayload => "Payload-Laenge nicht durch Frame-Groesse teilbar",
        };
        f.write_str(s)
    }
}

impl std::error::Error for RtpError {}

/// Geparster RTP-Header (nur die Felder, die AES67 braucht).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: u16,
    /// Media-Clock-Timestamp in Sample-Ticks (bei 48 kHz: +48 pro 1-ms-Paket).
    pub timestamp: u32,
    pub ssrc: u32,
}

impl RtpHeader {
    /// Serialisiert den 12-Byte-Fixheader (V=2, P=0, X=0, CC=0) an den Anfang
    /// von `out`. Gibt die Zahl geschriebener Bytes zurueck.
    pub fn write(&self, out: &mut [u8]) -> Result<usize, RtpError> {
        if out.len() < RTP_HEADER_MIN_LEN {
            return Err(RtpError::TooShort);
        }
        out[0] = RTP_VERSION << 6; // V=2, Padding=0, Extension=0, CC=0
        out[1] = ((self.marker as u8) << 7) | (self.payload_type & 0x7f);
        out[2..4].copy_from_slice(&self.sequence.to_be_bytes());
        out[4..8].copy_from_slice(&self.timestamp.to_be_bytes());
        out[8..12].copy_from_slice(&self.ssrc.to_be_bytes());
        Ok(RTP_HEADER_MIN_LEN)
    }

    /// Parst Header aus `buf` und gibt (Header, Offset des Payloads) zurueck.
    /// Ueberspringt CSRC-Liste und (falls X=1) den Extension-Header tolerant.
    pub fn parse(buf: &[u8]) -> Result<(RtpHeader, usize), RtpError> {
        if buf.len() < RTP_HEADER_MIN_LEN {
            return Err(RtpError::TooShort);
        }
        let version = buf[0] >> 6;
        if version != RTP_VERSION {
            return Err(RtpError::BadVersion);
        }
        let has_ext = (buf[0] & 0x10) != 0;
        let csrc_count = (buf[0] & 0x0f) as usize;

        let header = RtpHeader {
            marker: (buf[1] & 0x80) != 0,
            payload_type: buf[1] & 0x7f,
            sequence: u16::from_be_bytes([buf[2], buf[3]]),
            timestamp: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
            ssrc: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        };

        let mut offset = RTP_HEADER_MIN_LEN + csrc_count * 4;
        if has_ext {
            // Extension-Header: 2 Byte Profile-ID + 2 Byte Laenge (in 32-bit-Woertern)
            if buf.len() < offset + 4 {
                return Err(RtpError::TooShort);
            }
            let ext_words = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]) as usize;
            offset += 4 + ext_words * 4;
        }
        if buf.len() < offset {
            return Err(RtpError::TooShort);
        }
        Ok((header, offset))
    }
}

/// Kodiert interleavte Samples (ein i32 pro Kanal-Frame, obere Bits = Nutzsignal)
/// in big-endian-PCM. Bei L24 werden die oberen 24 Bit jedes i32 genommen, bei
/// L16 die oberen 16 Bit. `out` muss `samples.len() * bytes_per_sample` fassen.
pub fn encode_payload(samples: &[i32], enc: Encoding, out: &mut [u8]) -> Result<usize, RtpError> {
    let bps = enc.bytes_per_sample();
    let need = samples.len() * bps;
    if out.len() < need {
        return Err(RtpError::TooShort);
    }
    match enc {
        Encoding::L24 => {
            for (i, &s) in samples.iter().enumerate() {
                let b = s.to_be_bytes(); // [MSB, .., LSB]
                out[i * 3] = b[0];
                out[i * 3 + 1] = b[1];
                out[i * 3 + 2] = b[2];
            }
        }
        Encoding::L16 => {
            for (i, &s) in samples.iter().enumerate() {
                let b = s.to_be_bytes();
                out[i * 2] = b[0];
                out[i * 2 + 1] = b[1];
            }
        }
    }
    Ok(need)
}

/// Dekodiert big-endian-PCM zurueck in i32-Samples (linksbuendig: das Nutzsignal
/// steht in den oberen Bits, untere Bits 0). So bleibt der Wertebereich ueber
/// L16/L24/L32 konsistent und ASRC/DSP rechnet einheitlich.
pub fn decode_payload(
    payload: &[u8],
    enc: Encoding,
    out: &mut Vec<i32>,
) -> Result<usize, RtpError> {
    let bps = enc.bytes_per_sample();
    if payload.len() % bps != 0 {
        return Err(RtpError::RaggedPayload);
    }
    let n = payload.len() / bps;
    out.clear();
    out.reserve(n);
    match enc {
        Encoding::L24 => {
            for chunk in payload.chunks_exact(3) {
                let v = i32::from_be_bytes([chunk[0], chunk[1], chunk[2], 0]);
                out.push(v);
            }
        }
        Encoding::L16 => {
            for chunk in payload.chunks_exact(2) {
                let v = i32::from_be_bytes([chunk[0], chunk[1], 0, 0]);
                out.push(v);
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = RtpHeader {
            marker: true,
            payload_type: 97,
            sequence: 0xBEEF,
            timestamp: 0x0011_2233,
            ssrc: 0xDEAD_C0DE,
        };
        let mut buf = [0u8; RTP_HEADER_MIN_LEN];
        let n = h.write(&mut buf).unwrap();
        assert_eq!(n, RTP_HEADER_MIN_LEN);
        let (parsed, off) = RtpHeader::parse(&buf).unwrap();
        assert_eq!(parsed, h);
        assert_eq!(off, RTP_HEADER_MIN_LEN);
    }

    #[test]
    fn parse_skips_csrc() {
        // CC=2 → 2 CSRC-Woerter (8 Byte) nach dem Fixheader
        let mut buf = vec![0u8; RTP_HEADER_MIN_LEN + 8 + 4];
        buf[0] = (RTP_VERSION << 6) | 0x02; // V=2, CC=2
        let (_h, off) = RtpHeader::parse(&buf).unwrap();
        assert_eq!(off, RTP_HEADER_MIN_LEN + 8);
    }

    #[test]
    fn rejects_bad_version() {
        let buf = [0u8; RTP_HEADER_MIN_LEN];
        assert_eq!(RtpHeader::parse(&buf), Err(RtpError::BadVersion));
    }

    #[test]
    fn l24_payload_roundtrip() {
        // Werte linksbuendig (untere 8 Bit 0), damit L24-Roundtrip exakt ist.
        let samples: Vec<i32> = [1i32, -1, 0x7fff_ff00, -0x8000_0000i32]
            .iter()
            .map(|&x| x & !0xff)
            .collect();
        let enc = Encoding::L24;
        let mut bytes = vec![0u8; samples.len() * enc.bytes_per_sample()];
        encode_payload(&samples, enc, &mut bytes).unwrap();
        let mut back = Vec::new();
        let n = decode_payload(&bytes, enc, &mut back).unwrap();
        assert_eq!(n, samples.len());
        assert_eq!(back, samples);
    }

    #[test]
    fn l24_encodes_big_endian() {
        let samples = [0x12_34_56_00i32]; // MSB..LSB = 12 34 56 00
        let mut bytes = [0u8; 3];
        encode_payload(&samples, Encoding::L24, &mut bytes).unwrap();
        assert_eq!(bytes, [0x12, 0x34, 0x56]);
    }

    #[test]
    fn ragged_payload_detected() {
        let mut out = Vec::new();
        // 4 Byte sind bei L24 (3 Byte/Sample) nicht teilbar
        assert_eq!(
            decode_payload(&[0, 0, 0, 0], Encoding::L24, &mut out),
            Err(RtpError::RaggedPayload)
        );
    }
}
