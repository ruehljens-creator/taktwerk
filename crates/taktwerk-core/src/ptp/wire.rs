//! PTP-Wire-Format (IEEE 1588-2008) — Parsen und Bauen der Nachrichten, die
//! Taktwerk braucht: **Announce** (→ BMCA-Datensatz), **Sync** und **Follow_Up**
//! (→ Master-Zeitstempel für den Servo).
//!
//! Reiner Byte-Code, OS-neutral und unit-testbar. Der Netz-Client (UDP 319/320,
//! Multicast 224.0.1.129) liegt in `taktwerk-net`; hier ist nur das Format.
//!
//! Gemeinsamer Header = 34 Byte; danach folgt der nachrichtenspezifische Body.

use super::{ClockDataset, ClockIdentity};

/// Länge des gemeinsamen PTP-Headers.
pub const PTP_HEADER_LEN: usize = 34;

/// PTP-Nachrichtentypen (unteres Nibble von Byte 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Sync,
    DelayReq,
    FollowUp,
    DelayResp,
    Announce,
    Signaling,
    Management,
    /// Alles andere (Pdelay_* etc.), roher Wert.
    Other(u8),
}

impl MessageType {
    pub fn from_u8(v: u8) -> Self {
        match v & 0x0F {
            0x0 => MessageType::Sync,
            0x1 => MessageType::DelayReq,
            0x8 => MessageType::FollowUp,
            0x9 => MessageType::DelayResp,
            0xB => MessageType::Announce,
            0xC => MessageType::Signaling,
            0xD => MessageType::Management,
            other => MessageType::Other(other),
        }
    }

    pub fn to_u8(self) -> u8 {
        match self {
            MessageType::Sync => 0x0,
            MessageType::DelayReq => 0x1,
            MessageType::FollowUp => 0x8,
            MessageType::DelayResp => 0x9,
            MessageType::Announce => 0xB,
            MessageType::Signaling => 0xC,
            MessageType::Management => 0xD,
            MessageType::Other(v) => v & 0x0F,
        }
    }
}

/// Fehler beim Parsen einer PTP-Nachricht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtpError {
    TooShort,
    /// Nicht der erwartete Nachrichtentyp.
    WrongType,
    /// Versionsfeld != 2.
    BadVersion,
}

/// Ein PTP-Zeitstempel: 48-bit Sekunden + 32-bit Nanosekunden.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PtpTimestamp {
    pub seconds: u64,
    pub nanos: u32,
}

impl PtpTimestamp {
    pub const LEN: usize = 10;

    /// Gesamtzeit in Nanosekunden.
    pub fn total_nanos(&self) -> u128 {
        self.seconds as u128 * 1_000_000_000 + self.nanos as u128
    }

    /// Baut einen Zeitstempel aus einer Gesamt-Nanosekundenzahl (negativ → 0).
    pub fn from_nanos(total: i128) -> Self {
        let total = total.max(0) as u128;
        Self {
            seconds: (total / 1_000_000_000) as u64,
            nanos: (total % 1_000_000_000) as u32,
        }
    }

    /// Parst 10 Byte (6 Byte Sekunden BE + 4 Byte Nanos BE).
    pub fn parse(b: &[u8]) -> Result<Self, PtpError> {
        if b.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let seconds = ((b[0] as u64) << 40)
            | ((b[1] as u64) << 32)
            | ((b[2] as u64) << 24)
            | ((b[3] as u64) << 16)
            | ((b[4] as u64) << 8)
            | (b[5] as u64);
        let nanos = u32::from_be_bytes([b[6], b[7], b[8], b[9]]);
        Ok(Self { seconds, nanos })
    }

    /// Schreibt 10 Byte an den Anfang von `out`.
    pub fn write(&self, out: &mut [u8]) -> Result<(), PtpError> {
        if out.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let s = self.seconds;
        out[0] = (s >> 40) as u8;
        out[1] = (s >> 32) as u8;
        out[2] = (s >> 24) as u8;
        out[3] = (s >> 16) as u8;
        out[4] = (s >> 8) as u8;
        out[5] = s as u8;
        out[6..10].copy_from_slice(&self.nanos.to_be_bytes());
        Ok(())
    }
}

/// Port-Identity: Clock-Identity (EUI-64) + Portnummer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortIdentity {
    pub clock_identity: ClockIdentity,
    pub port: u16,
}

/// Gemeinsamer PTP-Header (die für uns relevanten Felder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtpHeader {
    pub message_type: MessageType,
    pub version: u8,
    pub message_length: u16,
    pub domain: u8,
    pub flags: u16,
    pub correction: i64,
    pub source_port: PortIdentity,
    pub sequence_id: u16,
    pub control: u8,
    pub log_message_interval: i8,
}

impl PtpHeader {
    /// Parst den 34-Byte-Header.
    pub fn parse(b: &[u8]) -> Result<Self, PtpError> {
        if b.len() < PTP_HEADER_LEN {
            return Err(PtpError::TooShort);
        }
        let version = b[1] & 0x0F;
        if version != 2 {
            return Err(PtpError::BadVersion);
        }
        let mut clock_identity = [0u8; 8];
        clock_identity.copy_from_slice(&b[20..28]);
        Ok(Self {
            message_type: MessageType::from_u8(b[0]),
            version,
            message_length: u16::from_be_bytes([b[2], b[3]]),
            domain: b[4],
            flags: u16::from_be_bytes([b[6], b[7]]),
            correction: i64::from_be_bytes([b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]]),
            source_port: PortIdentity {
                clock_identity,
                port: u16::from_be_bytes([b[28], b[29]]),
            },
            sequence_id: u16::from_be_bytes([b[30], b[31]]),
            control: b[32],
            log_message_interval: b[33] as i8,
        })
    }

    /// Schreibt den 34-Byte-Header (transportSpecific/minorVersion = 0).
    pub fn write(&self, out: &mut [u8]) -> Result<(), PtpError> {
        if out.len() < PTP_HEADER_LEN {
            return Err(PtpError::TooShort);
        }
        for x in out[..PTP_HEADER_LEN].iter_mut() {
            *x = 0;
        }
        out[0] = self.message_type.to_u8();
        out[1] = 0x02; // versionPTP = 2
        out[2..4].copy_from_slice(&self.message_length.to_be_bytes());
        out[4] = self.domain;
        out[6..8].copy_from_slice(&self.flags.to_be_bytes());
        out[8..16].copy_from_slice(&self.correction.to_be_bytes());
        out[20..28].copy_from_slice(&self.source_port.clock_identity);
        out[28..30].copy_from_slice(&self.source_port.port.to_be_bytes());
        out[30..32].copy_from_slice(&self.sequence_id.to_be_bytes());
        out[32] = self.control;
        out[33] = self.log_message_interval as u8;
        Ok(())
    }
}

/// Body einer **Announce**-Nachricht (die BMCA-relevanten Felder).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announce {
    pub header: PtpHeader,
    pub origin_timestamp: PtpTimestamp,
    pub current_utc_offset: i16,
    pub gm_priority1: u8,
    pub gm_clock_class: u8,
    pub gm_clock_accuracy: u8,
    pub gm_offset_scaled_log_variance: u16,
    pub gm_priority2: u8,
    pub gm_identity: ClockIdentity,
    pub steps_removed: u16,
    pub time_source: u8,
}

impl Announce {
    /// Gesamtlänge einer Announce-Nachricht in Byte.
    pub const LEN: usize = 64;

    /// Parst eine vollständige Announce-Nachricht (Header + Body).
    pub fn parse(b: &[u8]) -> Result<Self, PtpError> {
        let header = PtpHeader::parse(b)?;
        if header.message_type != MessageType::Announce {
            return Err(PtpError::WrongType);
        }
        if b.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let origin_timestamp = PtpTimestamp::parse(&b[34..44])?;
        let mut gm_identity = [0u8; 8];
        gm_identity.copy_from_slice(&b[53..61]);
        Ok(Self {
            header,
            origin_timestamp,
            current_utc_offset: i16::from_be_bytes([b[44], b[45]]),
            gm_priority1: b[47],
            gm_clock_class: b[48],
            gm_clock_accuracy: b[49],
            gm_offset_scaled_log_variance: u16::from_be_bytes([b[50], b[51]]),
            gm_priority2: b[52],
            gm_identity,
            steps_removed: u16::from_be_bytes([b[61], b[62]]),
            time_source: b[63],
        })
    }

    /// Serialisiert die Announce-Nachricht in einen 64-Byte-Puffer.
    pub fn write(&self, out: &mut [u8]) -> Result<(), PtpError> {
        if out.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let mut header = self.header;
        header.message_type = MessageType::Announce;
        header.message_length = Self::LEN as u16;
        header.control = 0x05; // "all others"
        header.write(out)?;
        self.origin_timestamp.write(&mut out[34..44])?;
        out[44..46].copy_from_slice(&self.current_utc_offset.to_be_bytes());
        out[46] = 0;
        out[47] = self.gm_priority1;
        out[48] = self.gm_clock_class;
        out[49] = self.gm_clock_accuracy;
        out[50..52].copy_from_slice(&self.gm_offset_scaled_log_variance.to_be_bytes());
        out[52] = self.gm_priority2;
        out[53..61].copy_from_slice(&self.gm_identity);
        out[61..63].copy_from_slice(&self.steps_removed.to_be_bytes());
        out[63] = self.time_source;
        Ok(())
    }

    /// Baut den BMCA-Datensatz dieser Uhr (→ [`ClockDataset::compare`]).
    pub fn to_clock_dataset(&self) -> ClockDataset {
        ClockDataset {
            priority1: self.gm_priority1,
            clock_class: self.gm_clock_class,
            clock_accuracy: self.gm_clock_accuracy,
            offset_scaled_log_variance: self.gm_offset_scaled_log_variance,
            priority2: self.gm_priority2,
            clock_identity: self.gm_identity,
            steps_removed: self.steps_removed,
        }
    }
}

/// Eine Sync- oder Follow_Up-Nachricht (beide tragen genau einen Zeitstempel).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampedMsg {
    pub header: PtpHeader,
    /// originTimestamp (Sync) bzw. preciseOriginTimestamp (Follow_Up).
    pub timestamp: PtpTimestamp,
}

impl TimestampedMsg {
    /// Gesamtlänge (Header + 10-Byte-Timestamp).
    pub const LEN: usize = PTP_HEADER_LEN + PtpTimestamp::LEN;

    /// Parst eine Sync- oder Follow_Up-Nachricht.
    pub fn parse(b: &[u8]) -> Result<Self, PtpError> {
        let header = PtpHeader::parse(b)?;
        match header.message_type {
            MessageType::Sync | MessageType::FollowUp => {}
            _ => return Err(PtpError::WrongType),
        }
        if b.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let timestamp = PtpTimestamp::parse(&b[34..44])?;
        Ok(Self { header, timestamp })
    }

    /// Serialisiert eine **Sync**- oder **Follow_Up**-Nachricht (Master-Seite).
    /// Setzt Länge und Control passend zum Nachrichtentyp.
    pub fn write(&self, out: &mut [u8]) -> Result<(), PtpError> {
        if out.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let mut header = self.header;
        header.message_length = Self::LEN as u16;
        header.control = match header.message_type {
            MessageType::Sync => 0x00,
            MessageType::FollowUp => 0x02,
            _ => return Err(PtpError::WrongType),
        };
        header.write(out)?;
        self.timestamp.write(&mut out[34..44])?;
        Ok(())
    }
}

/// Eine **Delay_Resp**-Nachricht: der Master timestampt den Empfang unseres
/// Delay_Req und schickt `receiveTimestamp` (t4) + die anfragende PortIdentity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelayResp {
    pub header: PtpHeader,
    /// t4 — Empfangszeit des Delay_Req beim Master.
    pub receive_timestamp: PtpTimestamp,
    /// PortIdentity, deren Delay_Req beantwortet wird (muss unsere sein).
    pub requesting_port: PortIdentity,
}

impl DelayResp {
    pub const LEN: usize = PTP_HEADER_LEN + PtpTimestamp::LEN + 10;

    pub fn parse(b: &[u8]) -> Result<Self, PtpError> {
        let header = PtpHeader::parse(b)?;
        if header.message_type != MessageType::DelayResp {
            return Err(PtpError::WrongType);
        }
        if b.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let receive_timestamp = PtpTimestamp::parse(&b[34..44])?;
        let mut clock_identity = [0u8; 8];
        clock_identity.copy_from_slice(&b[44..52]);
        Ok(Self {
            header,
            receive_timestamp,
            requesting_port: PortIdentity {
                clock_identity,
                port: u16::from_be_bytes([b[52], b[53]]),
            },
        })
    }

    /// Serialisiert eine **Delay_Resp** (Master-Antwort auf einen Delay_Req).
    pub fn write(&self, out: &mut [u8]) -> Result<(), PtpError> {
        if out.len() < Self::LEN {
            return Err(PtpError::TooShort);
        }
        let mut header = self.header;
        header.message_type = MessageType::DelayResp;
        header.message_length = Self::LEN as u16;
        header.control = 0x03; // Delay_Resp
        header.write(out)?;
        self.receive_timestamp.write(&mut out[34..44])?;
        out[44..52].copy_from_slice(&self.requesting_port.clock_identity);
        out[52..54].copy_from_slice(&self.requesting_port.port.to_be_bytes());
        Ok(())
    }
}

/// Baut eine **Delay_Req**-Nachricht (44 Byte). `origin_timestamp` darf 0 sein —
/// der Master timestampt den Empfang selbst; entscheidend ist unsere lokale
/// Sendezeit t3, die der Aufrufer separat festhält.
pub fn build_delay_req(
    source: PortIdentity,
    sequence_id: u16,
    domain: u8,
) -> Result<[u8; 44], PtpError> {
    let header = PtpHeader {
        message_type: MessageType::DelayReq,
        version: 2,
        message_length: 44,
        domain,
        flags: 0,
        correction: 0,
        source_port: source,
        sequence_id,
        control: 0x01,              // Delay_Req
        log_message_interval: 0x7f, // per Spec für Delay_Req
    };
    let mut buf = [0u8; 44];
    header.write(&mut buf)?;
    // originTimestamp (10 Byte) bleibt 0.
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::super::BmcaOrder;
    use super::*;

    #[test]
    fn timestamp_roundtrip() {
        let ts = PtpTimestamp {
            seconds: 0x0000_1234_5678,
            nanos: 999_999_999,
        };
        let mut buf = [0u8; 10];
        ts.write(&mut buf).unwrap();
        assert_eq!(PtpTimestamp::parse(&buf).unwrap(), ts);
        assert_eq!(
            ts.total_nanos(),
            0x1234_5678u128 * 1_000_000_000 + 999_999_999
        );
    }

    #[test]
    fn header_roundtrip() {
        let h = PtpHeader {
            message_type: MessageType::Sync,
            version: 2,
            message_length: 44,
            domain: 0,
            flags: 0x0200,
            correction: 12345,
            source_port: PortIdentity {
                clock_identity: [1, 2, 3, 4, 5, 6, 7, 8],
                port: 1,
            },
            sequence_id: 42,
            control: 0x00,
            log_message_interval: -3,
        };
        let mut buf = [0u8; PTP_HEADER_LEN];
        h.write(&mut buf).unwrap();
        assert_eq!(PtpHeader::parse(&buf).unwrap(), h);
    }

    #[test]
    fn header_rejects_wrong_version() {
        let mut buf = [0u8; PTP_HEADER_LEN];
        buf[1] = 0x01; // version 1
        assert_eq!(PtpHeader::parse(&buf), Err(PtpError::BadVersion));
    }

    #[test]
    fn announce_roundtrip_and_bmca_dataset() {
        let ann = Announce {
            header: PtpHeader {
                message_type: MessageType::Announce,
                version: 2,
                message_length: Announce::LEN as u16,
                domain: 0,
                flags: 0,
                correction: 0,
                source_port: PortIdentity {
                    clock_identity: [0xAA; 8],
                    port: 1,
                },
                sequence_id: 7,
                control: 0x05,
                log_message_interval: 1,
            },
            origin_timestamp: PtpTimestamp {
                seconds: 100,
                nanos: 500,
            },
            current_utc_offset: 37,
            gm_priority1: 128,
            gm_clock_class: 6,
            gm_clock_accuracy: 0x21,
            gm_offset_scaled_log_variance: 0x4E5D,
            gm_priority2: 128,
            gm_identity: [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22, 0x33],
            steps_removed: 0,
            time_source: 0x20, // GPS
        };
        let mut buf = [0u8; Announce::LEN];
        ann.write(&mut buf).unwrap();
        let parsed = Announce::parse(&buf).unwrap();
        assert_eq!(parsed, ann);

        let ds = parsed.to_clock_dataset();
        assert_eq!(ds.clock_class, 6);
        assert_eq!(
            ds.clock_identity,
            [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22, 0x33]
        );
        assert_eq!(ds.priority1, 128);
    }

    #[test]
    fn announce_rejects_wrong_type() {
        // Header mit Sync-Typ, aber als Announce geparst.
        let mut buf = [0u8; Announce::LEN];
        buf[1] = 0x02;
        buf[0] = MessageType::Sync.to_u8();
        assert_eq!(Announce::parse(&buf), Err(PtpError::WrongType));
    }

    /// Interop-Regression: eine **echte Announce von linuxptp `ptp4l` 4.2**
    /// (vom Draht mit tcpdump gefangen, Grandmaster-Rolle). Beweist, dass unser
    /// Parser das reale Wire-Format einer Fremdimplementierung korrekt liest.
    #[test]
    fn parse_real_linuxptp_announce() {
        // 64-Byte PTP-Payload (ohne Eth/IP/UDP), messageType 0xB.
        let bytes: [u8; 64] = [
            0x0b, 0x12, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x90, 0x1b, 0x0e, 0xff, 0xfe, 0x4b, 0xf8, 0x6e,
            0x00, 0x01, 0x00, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x25, 0x00, 0x80, 0xf8, 0xfe, 0xff, 0xff, 0x80, 0x90, 0x1b, 0x0e,
            0xff, 0xfe, 0x4b, 0xf8, 0x6e, 0x00, 0x00, 0xa0,
        ];
        let ann = Announce::parse(&bytes).expect("echte ptp4l-Announce muss parsen");
        assert_eq!(ann.header.message_type, MessageType::Announce);
        assert_eq!(ann.header.sequence_id, 3);
        assert_eq!(ann.header.source_port.port, 1);
        let gmid = [0x90, 0x1b, 0x0e, 0xff, 0xfe, 0x4b, 0xf8, 0x6e];
        assert_eq!(ann.gm_identity, gmid);
        assert_eq!(ann.header.source_port.clock_identity, gmid);
        assert_eq!(ann.gm_priority1, 128);
        assert_eq!(ann.gm_priority2, 128);
        assert_eq!(ann.gm_clock_class, 248);
        assert_eq!(ann.gm_clock_accuracy, 0xFE);
        assert_eq!(ann.gm_offset_scaled_log_variance, 0xFFFF);
        assert_eq!(ann.current_utc_offset, 37);
        assert_eq!(ann.time_source, 0xA0); // INTERNAL_OSCILLATOR

        // BMCA-Datensatz stimmt und ist gegen einen besseren GM vergleichbar.
        let ds = ann.to_clock_dataset();
        assert_eq!(ds.clock_class, 248);
        let better = ClockDataset {
            priority1: 1,
            clock_identity: [0x11; 8], // andere Uhr, sonst wäre der Vergleich "Same"
            ..ds
        };
        assert_eq!(ClockDataset::compare(&better, &ds), BmcaOrder::ABetter);
    }

    #[test]
    fn delay_req_build_and_header_roundtrip() {
        let src = PortIdentity {
            clock_identity: [7; 8],
            port: 1,
        };
        let buf = build_delay_req(src, 99, 127).unwrap();
        let h = PtpHeader::parse(&buf).unwrap();
        assert_eq!(h.message_type, MessageType::DelayReq);
        assert_eq!(h.sequence_id, 99);
        assert_eq!(h.source_port, src);
        assert_eq!(h.control, 0x01);
        assert_eq!(h.domain, 127, "Domain muss durchgereicht werden");
    }

    #[test]
    fn delay_resp_parse() {
        // Delay_Resp bauen: Header (Typ 0x9) + receiveTimestamp + requestingPort.
        let mut buf = [0u8; DelayResp::LEN];
        let h = PtpHeader {
            message_type: MessageType::DelayResp,
            version: 2,
            message_length: DelayResp::LEN as u16,
            domain: 0,
            flags: 0,
            correction: 0,
            source_port: PortIdentity {
                clock_identity: [0xAA; 8],
                port: 1,
            },
            sequence_id: 99,
            control: 0x03,
            log_message_interval: 0x7f,
        };
        h.write(&mut buf[..PTP_HEADER_LEN]).unwrap();
        PtpTimestamp {
            seconds: 5,
            nanos: 500,
        }
        .write(&mut buf[34..44])
        .unwrap();
        buf[44..52].copy_from_slice(&[7u8; 8]); // requesting clock identity
        buf[52..54].copy_from_slice(&1u16.to_be_bytes());

        let resp = DelayResp::parse(&buf).unwrap();
        assert_eq!(resp.header.sequence_id, 99);
        assert_eq!(
            resp.receive_timestamp.total_nanos(),
            5 * 1_000_000_000 + 500
        );
        assert_eq!(resp.requesting_port.clock_identity, [7u8; 8]);
    }

    #[test]
    fn timestamp_from_nanos_roundtrip() {
        let ts = PtpTimestamp::from_nanos(42 * 1_000_000_000 + 123_456);
        assert_eq!(ts.seconds, 42);
        assert_eq!(ts.nanos, 123_456);
        assert_eq!(PtpTimestamp::from_nanos(-5).seconds, 0); // negativ → 0
    }

    #[test]
    fn timestamped_write_roundtrip() {
        for mt in [MessageType::Sync, MessageType::FollowUp] {
            let msg = TimestampedMsg {
                header: PtpHeader {
                    message_type: mt,
                    version: 2,
                    message_length: 0,
                    domain: 0,
                    flags: 0x0200,
                    correction: 0,
                    source_port: PortIdentity {
                        clock_identity: [0x42; 8],
                        port: 1,
                    },
                    sequence_id: 11,
                    control: 0,
                    log_message_interval: -3,
                },
                timestamp: PtpTimestamp::from_nanos(7_000_000_042),
            };
            let mut buf = [0u8; TimestampedMsg::LEN];
            msg.write(&mut buf).unwrap();
            let parsed = TimestampedMsg::parse(&buf).unwrap();
            assert_eq!(parsed.header.message_type, mt);
            assert_eq!(parsed.timestamp, msg.timestamp);
            assert_eq!(parsed.header.sequence_id, 11);
        }
    }

    #[test]
    fn delay_resp_write_roundtrip() {
        let resp = DelayResp {
            header: PtpHeader {
                message_type: MessageType::DelayResp,
                version: 2,
                message_length: 0,
                domain: 0,
                flags: 0,
                correction: 0,
                source_port: PortIdentity {
                    clock_identity: [0x55; 8],
                    port: 1,
                },
                sequence_id: 88,
                control: 0,
                log_message_interval: 0x7f,
            },
            receive_timestamp: PtpTimestamp::from_nanos(9_000_000_500),
            requesting_port: PortIdentity {
                clock_identity: [0x77; 8],
                port: 2,
            },
        };
        let mut buf = [0u8; DelayResp::LEN];
        resp.write(&mut buf).unwrap();
        let parsed = DelayResp::parse(&buf).unwrap();
        assert_eq!(parsed.header.sequence_id, 88);
        assert_eq!(parsed.receive_timestamp, resp.receive_timestamp);
        assert_eq!(parsed.requesting_port, resp.requesting_port);
    }

    #[test]
    fn sync_and_followup_parse() {
        for mt in [MessageType::Sync, MessageType::FollowUp] {
            let h = PtpHeader {
                message_type: mt,
                version: 2,
                message_length: TimestampedMsg::LEN as u16,
                domain: 0,
                flags: 0,
                correction: 0,
                source_port: PortIdentity {
                    clock_identity: [9; 8],
                    port: 1,
                },
                sequence_id: 3,
                control: 0,
                log_message_interval: -3,
            };
            let ts = PtpTimestamp {
                seconds: 42,
                nanos: 123_456,
            };
            let mut buf = [0u8; TimestampedMsg::LEN];
            h.write(&mut buf[..PTP_HEADER_LEN]).unwrap();
            ts.write(&mut buf[34..44]).unwrap();
            let parsed = TimestampedMsg::parse(&buf).unwrap();
            assert_eq!(parsed.header.message_type, mt);
            assert_eq!(parsed.timestamp, ts);
        }
    }
}
