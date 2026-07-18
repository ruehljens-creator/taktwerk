//! SAP — Session Announcement Protocol (RFC 2974).
//!
//! AES67-Altgeraete ohne NMOS kuendigen ihre Streams per SAP an (Multicast
//! 239.255.255.255:9875), Payload ist die SDP-Beschreibung ([`crate::sdp`]).
//! Dieser Baustein baut/parst nur den **SAP-Header** und trennt ihn vom
//! SDP-Payload; das eigentliche Versenden macht `taktwerk-net`.
//!
//! Header-Layout (IPv4, ohne Auth/Encryption — der Normalfall):
//! ```text
//! 0                   1                   2                   3
//! |V=1|A|R|T|E|C| auth_len (0) | msg_id_hash | originating source (IPv4) |
//! ```

/// Minimaler SAP-Header (IPv4) = 8 Byte vor dem Payload.
pub const SAP_HEADER_LEN: usize = 8;

/// Well-known SAP-Multicast-Adresse und -Port (global scope).
pub const SAP_MULTICAST: &str = "239.255.255.255";
pub const SAP_PORT: u16 = 9875;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SapError {
    TooShort,
    BadVersion,
    /// Auth-/verschluesselte oder IPv6-Announcements unterstuetzen wir (noch) nicht.
    Unsupported,
}

/// Ein geparstes SAP-Announcement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SapPacket<'a> {
    /// true = Announcement (neue/aktualisierte Session), false = Deletion.
    pub announce: bool,
    /// 16-bit Message-ID-Hash (identifiziert eine Session-Instanz).
    pub msg_id_hash: u16,
    /// Quelladresse (IPv4, big-endian Bytes wie im Header).
    pub source: [u8; 4],
    /// MIME-Typ des Payloads, i. d. R. `application/sdp` (oft implizit).
    pub payload_type: Option<&'a str>,
    /// Der rohe Payload (die SDP-Beschreibung).
    pub payload: &'a [u8],
}

impl<'a> SapPacket<'a> {
    /// Serialisiert Header + Payload in einen neuen Puffer.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(SAP_HEADER_LEN + self.payload.len());
        // V=1 (001), A=0 (IPv4), R=0, T=announce?0:1, E=0, C=0
        let t = if self.announce { 0 } else { 1 };
        out.push(0b0010_0000 | (t << 2));
        out.push(0); // auth_len = 0
        out.extend_from_slice(&self.msg_id_hash.to_be_bytes());
        out.extend_from_slice(&self.source);
        if let Some(pt) = self.payload_type {
            out.extend_from_slice(pt.as_bytes());
            out.push(0); // Nullterminator trennt Typ vom SDP
        }
        out.extend_from_slice(self.payload);
        out
    }

    /// Parst Header + Payload. Auth/Encryption/IPv6 → [`SapError::Unsupported`].
    pub fn parse(buf: &'a [u8]) -> Result<SapPacket<'a>, SapError> {
        if buf.len() < SAP_HEADER_LEN {
            return Err(SapError::TooShort);
        }
        let flags = buf[0];
        if (flags >> 5) != 1 {
            return Err(SapError::BadVersion);
        }
        let addr_type_ipv6 = (flags & 0b0001_0000) != 0;
        let encrypted = (flags & 0b0000_0010) != 0;
        let auth_len = buf[1] as usize;
        if addr_type_ipv6 || encrypted {
            return Err(SapError::Unsupported);
        }
        let announce = (flags & 0b0000_0100) == 0;
        let msg_id_hash = u16::from_be_bytes([buf[2], buf[3]]);
        let source = [buf[4], buf[5], buf[6], buf[7]];

        let mut off = SAP_HEADER_LEN + auth_len * 4;
        if buf.len() < off {
            return Err(SapError::TooShort);
        }
        // Optionaler, nullterminierter MIME-Typ vor dem SDP.
        let rest = &buf[off..];
        let (payload_type, payload) = match rest.iter().position(|&b| b == 0) {
            Some(nul) if looks_like_mime(&rest[..nul]) => {
                let pt = core::str::from_utf8(&rest[..nul]).ok();
                off += nul + 1;
                (pt, &buf[off..])
            }
            _ => (None, rest),
        };
        Ok(SapPacket {
            announce,
            msg_id_hash,
            source,
            payload_type,
            payload,
        })
    }
}

/// Grobe Heuristik: beginnt der Abschnitt mit `application/` (MIME) statt `v=0` (SDP)?
fn looks_like_mime(bytes: &[u8]) -> bool {
    bytes.starts_with(b"application/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_roundtrip_with_mime() {
        let sdp = b"v=0\r\ns=Test\r\n";
        let pkt = SapPacket {
            announce: true,
            msg_id_hash: 0xABCD,
            source: [192, 168, 1, 10],
            payload_type: Some("application/sdp"),
            payload: sdp,
        };
        let bytes = pkt.to_bytes();
        let parsed = SapPacket::parse(&bytes).unwrap();
        assert_eq!(parsed, pkt);
        assert!(parsed.announce);
    }

    #[test]
    fn parse_without_mime() {
        let sdp = b"v=0\r\n";
        let pkt = SapPacket {
            announce: true,
            msg_id_hash: 1,
            source: [10, 0, 0, 1],
            payload_type: None,
            payload: sdp,
        };
        let bytes = pkt.to_bytes();
        let parsed = SapPacket::parse(&bytes).unwrap();
        assert_eq!(parsed.payload, sdp);
        assert!(parsed.payload_type.is_none());
    }

    #[test]
    fn deletion_flag() {
        let pkt = SapPacket {
            announce: false,
            msg_id_hash: 7,
            source: [10, 0, 0, 2],
            payload_type: None,
            payload: b"v=0\r\n",
        };
        let bytes = pkt.to_bytes();
        let parsed = SapPacket::parse(&bytes).unwrap();
        assert!(!parsed.announce);
    }

    #[test]
    fn rejects_encrypted() {
        let mut bytes = SapPacket {
            announce: true,
            msg_id_hash: 0,
            source: [0, 0, 0, 0],
            payload_type: None,
            payload: b"",
        }
        .to_bytes();
        bytes[0] |= 0b0000_0010; // E-Bit
        assert_eq!(SapPacket::parse(&bytes), Err(SapError::Unsupported));
    }
}
