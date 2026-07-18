//! SDP (RFC 4566) fuer AES67 / ST-2110-30 Level A.
//!
//! Zwei Richtungen, bewusst asymmetrisch (§7.1 des Projektbriefs):
//! - **Bauen** (unser Sender-Announce): streng, konservativ auf Level A —
//!   damit sowohl AES67- als auch 2110-30-Receiver den Stream annehmen.
//! - **Parsen** (fremde Announcements): tolerant — unbekannte/`private`
//!   Attribute werden ignoriert statt als Fehler gewertet, genau wie ein
//!   robuster AES67-Empfaenger es tut.
//!
//! Enthaelt die RFC-7273-Clock-Referenz (`ts-refclk` / `mediaclk`), damit die
//! Empfaengerseite Media-Clock und PTP-Grandmaster zuordnen kann.

use crate::{Encoding, StreamProfile};

/// Fehler beim Parsen einer SDP-Beschreibung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdpError {
    /// Pflichtzeile fehlt (z. B. keine `m=audio`-Zeile).
    Missing(&'static str),
    /// Wert konnte nicht interpretiert werden.
    Malformed(&'static str),
}

impl core::fmt::Display for SdpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SdpError::Missing(s) => write!(f, "SDP: Pflichtangabe fehlt: {s}"),
            SdpError::Malformed(s) => write!(f, "SDP: ungueltiger Wert: {s}"),
        }
    }
}

impl std::error::Error for SdpError {}

/// Eine RFC-7273-PTP-Clock-Referenz (`a=ts-refclk:ptp=...`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtpRefClock {
    /// Grandmaster-Identity als EUI-64-String, z. B. `00-11-22-FF-FE-33-44-55`.
    pub gmid: String,
    /// PTP-Domain.
    pub domain: u8,
}

/// Beschreibung eines AES67-Audio-Streams — Ein-/Ausgabemodell fuer SDP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSession {
    pub session_name: String,
    /// Quelladresse (Unicast) fuer die `o=`- und Session-`c=`-Zeile.
    pub origin_unicast: String,
    /// Multicast-Zieladresse der Media-Plane.
    pub multicast_addr: String,
    pub port: u16,
    pub payload_type: u8,
    pub profile: StreamProfile,
    /// PTP-Clock-Referenz (RFC 7273). None → `a=ts-refclk:localmac` entfaellt.
    pub refclk: Option<PtpRefClock>,
    /// `a=mediaclk:direct=<offset>` — Startoffset der Media-Clock.
    pub mediaclk_offset: u32,
}

impl AudioSession {
    /// Baut eine strikte, Level-A-konforme SDP-Beschreibung.
    pub fn to_sdp(&self) -> String {
        let StreamProfile {
            sample_rate,
            channels,
            ptime_us,
            encoding,
        } = self.profile;
        let ptime_ms = ptime_us as f64 / 1000.0;
        let rtpmap = format!("{}/{}/{}", encoding.rtpmap_name(), sample_rate, channels);

        let mut s = String::with_capacity(512);
        s.push_str("v=0\r\n");
        // o=<user> <sess-id> <sess-version> IN IP4 <unicast>
        s.push_str(&format!("o=- 0 0 IN IP4 {}\r\n", self.origin_unicast));
        s.push_str(&format!("s={}\r\n", self.session_name));
        s.push_str(&format!("c=IN IP4 {}/32\r\n", self.multicast_addr));
        s.push_str("t=0 0\r\n");
        // Media-Zeile
        s.push_str(&format!(
            "m=audio {} RTP/AVP {}\r\n",
            self.port, self.payload_type
        ));
        s.push_str(&format!("a=rtpmap:{} {}\r\n", self.payload_type, rtpmap));
        s.push_str(&format!("a=ptime:{}\r\n", trim_num(ptime_ms)));
        s.push_str("a=recvonly\r\n");
        // RFC 7273 Clock-Referenz
        if let Some(rc) = &self.refclk {
            s.push_str(&format!(
                "a=ts-refclk:ptp=IEEE1588-2008:{}:{}\r\n",
                rc.gmid, rc.domain
            ));
        }
        s.push_str(&format!("a=mediaclk:direct={}\r\n", self.mediaclk_offset));
        s
    }

    /// Parst eine (fremde) SDP-Beschreibung tolerant in eine [`AudioSession`].
    /// Unbekannte Attribute werden ignoriert. Erwartet mindestens eine
    /// `m=audio`- und eine `a=rtpmap`-Zeile.
    pub fn parse(sdp: &str) -> Result<AudioSession, SdpError> {
        let mut session_name = String::new();
        let mut origin_unicast = String::new();
        let mut session_conn: Option<String> = None;
        let mut media_conn: Option<String> = None;
        let mut port: Option<u16> = None;
        let mut payload_type: Option<u8> = None;
        let mut rtpmap: Option<(u8, Encoding, u32, u8)> = None;
        let mut ptime_us: Option<u32> = None;
        let mut refclk: Option<PtpRefClock> = None;
        let mut mediaclk_offset: u32 = 0;
        let mut in_media = false;

        for raw in sdp.lines() {
            let line = raw.trim_end_matches('\r');
            let Some((tag, val)) = line.split_once('=') else {
                continue;
            };
            match tag {
                "s" => session_name = val.to_string(),
                "o" => {
                    // o=- <id> <ver> IN IP4 <addr>
                    if let Some(addr) = val.split_whitespace().nth(5) {
                        origin_unicast = addr.to_string();
                    }
                }
                "c" => {
                    let addr = parse_connection(val);
                    if in_media {
                        media_conn = addr;
                    } else {
                        session_conn = addr;
                    }
                }
                "m" => {
                    in_media = true;
                    // m=audio <port> RTP/AVP <pt>
                    let mut it = val.split_whitespace();
                    if it.next() != Some("audio") {
                        continue; // andere Medienart ignorieren
                    }
                    port = it.next().and_then(|p| p.parse().ok());
                    // Proto ueberspringen
                    let _ = it.next();
                    payload_type = it.next().and_then(|p| p.parse().ok());
                }
                "a" => parse_attribute(
                    val,
                    &mut rtpmap,
                    &mut ptime_us,
                    &mut refclk,
                    &mut mediaclk_offset,
                ),
                _ => {} // v, t, ... nicht relevant
            }
        }

        let port = port.ok_or(SdpError::Missing("m=audio"))?;
        let (pt, encoding, sample_rate, channels) = rtpmap.ok_or(SdpError::Missing("a=rtpmap"))?;
        let payload_type = payload_type.unwrap_or(pt);
        let multicast_addr = media_conn
            .or(session_conn)
            .ok_or(SdpError::Missing("c= (Multicast)"))?;
        // ptime default 1 ms, falls nicht angegeben
        let ptime_us = ptime_us.unwrap_or(1_000);

        Ok(AudioSession {
            session_name,
            origin_unicast,
            multicast_addr,
            port,
            payload_type,
            profile: StreamProfile {
                sample_rate,
                channels,
                ptime_us,
                encoding,
            },
            refclk,
            mediaclk_offset,
        })
    }
}

/// `c=IN IP4 239.69.83.67/32` → `239.69.83.67`
fn parse_connection(val: &str) -> Option<String> {
    val.split_whitespace()
        .nth(2)
        .map(|a| a.split('/').next().unwrap_or(a).to_string())
}

fn parse_attribute(
    val: &str,
    rtpmap: &mut Option<(u8, Encoding, u32, u8)>,
    ptime_us: &mut Option<u32>,
    refclk: &mut Option<PtpRefClock>,
    mediaclk_offset: &mut u32,
) {
    if let Some(rest) = val.strip_prefix("rtpmap:") {
        // <pt> L24/48000/8
        let mut it = rest.split_whitespace();
        let pt = it.next().and_then(|p| p.parse::<u8>().ok());
        let enc = it.next();
        if let (Some(pt), Some(enc)) = (pt, enc) {
            let mut parts = enc.split('/');
            let name = parts.next().unwrap_or("");
            let rate = parts.next().and_then(|r| r.parse::<u32>().ok());
            let ch = parts.next().and_then(|c| c.parse::<u8>().ok()).unwrap_or(1);
            let encoding = match name {
                "L24" => Some(Encoding::L24),
                "L16" => Some(Encoding::L16),
                _ => None, // andere Codecs (AM824 etc.) hier nicht unterstuetzt
            };
            if let (Some(encoding), Some(rate)) = (encoding, rate) {
                *rtpmap = Some((pt, encoding, rate, ch));
            }
        }
    } else if let Some(rest) = val.strip_prefix("ptime:") {
        if let Ok(ms) = rest.trim().parse::<f64>() {
            *ptime_us = Some((ms * 1000.0).round() as u32);
        }
    } else if let Some(rest) = val.strip_prefix("ts-refclk:ptp=") {
        // IEEE1588-2008:<gmid>:<domain>
        let mut parts = rest.split(':');
        let _std = parts.next();
        let gmid = parts.next().map(|g| g.to_string());
        let domain = parts.next().and_then(|d| d.parse::<u8>().ok()).unwrap_or(0);
        if let Some(gmid) = gmid {
            *refclk = Some(PtpRefClock { gmid, domain });
        }
    } else if let Some(rest) = val.strip_prefix("mediaclk:direct=") {
        if let Ok(off) = rest.trim().parse::<u32>() {
            *mediaclk_offset = off;
        }
    }
    // alles andere (recvonly, private Tokens, ...) tolerant ignorieren
}

/// Formatiert eine Zahl ohne unnoetige Nachkommastellen (`1.0` → `1`).
fn trim_num(x: f64) -> String {
    if (x.fract()).abs() < f64::EPSILON {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_session() -> AudioSession {
        AudioSession {
            session_name: "Taktwerk 8ch".to_string(),
            origin_unicast: "192.168.1.10".to_string(),
            multicast_addr: "239.69.83.67".to_string(),
            port: 5004,
            payload_type: 97,
            profile: StreamProfile::level_a(8),
            refclk: Some(PtpRefClock {
                gmid: "00-11-22-FF-FE-33-44-55".to_string(),
                domain: 0,
            }),
            mediaclk_offset: 0,
        }
    }

    #[test]
    fn build_contains_level_a_lines() {
        let sdp = sample_session().to_sdp();
        assert!(sdp.contains("m=audio 5004 RTP/AVP 97"));
        assert!(sdp.contains("a=rtpmap:97 L24/48000/8"));
        assert!(sdp.contains("a=ptime:1\r\n"));
        assert!(sdp.contains("c=IN IP4 239.69.83.67/32"));
        assert!(sdp.contains("ts-refclk:ptp=IEEE1588-2008:00-11-22-FF-FE-33-44-55:0"));
    }

    #[test]
    fn build_then_parse_roundtrip() {
        let orig = sample_session();
        let parsed = AudioSession::parse(&orig.to_sdp()).unwrap();
        assert_eq!(parsed.multicast_addr, "239.69.83.67");
        assert_eq!(parsed.port, 5004);
        assert_eq!(parsed.payload_type, 97);
        assert_eq!(parsed.profile, StreamProfile::level_a(8));
        assert_eq!(parsed.refclk, orig.refclk);
    }

    #[test]
    fn parse_tolerates_unknown_attributes() {
        let sdp = "v=0\r\n\
                   o=- 1443716955 1443716955 IN IP4 192.168.1.5\r\n\
                   s=Foreign Device\r\n\
                   c=IN IP4 239.1.2.3/32\r\n\
                   t=0 0\r\n\
                   a=x-vendor-secret:whatever\r\n\
                   m=audio 5004 RTP/AVP 96\r\n\
                   a=rtpmap:96 L24/48000/2\r\n\
                   a=ptime:1\r\n\
                   a=private-token:ignore-me\r\n\
                   a=mediaclk:direct=12345\r\n";
        let parsed = AudioSession::parse(sdp).unwrap();
        assert_eq!(parsed.profile.channels, 2);
        assert_eq!(parsed.profile.encoding, Encoding::L24);
        assert_eq!(parsed.mediaclk_offset, 12345);
        assert!(parsed.refclk.is_none());
    }

    #[test]
    fn parse_missing_media_errors() {
        let sdp = "v=0\r\ns=Nothing\r\nt=0 0\r\n";
        // Ohne m=audio-Zeile fehlt zuerst der Port.
        assert_eq!(AudioSession::parse(sdp), Err(SdpError::Missing("m=audio")));
    }

    #[test]
    fn parse_media_level_connection_wins() {
        // Session-c und Media-c unterschiedlich → Media-c muss gewinnen
        let sdp = "v=0\r\n\
                   c=IN IP4 239.0.0.1/32\r\n\
                   m=audio 5004 RTP/AVP 96\r\n\
                   c=IN IP4 239.9.9.9/32\r\n\
                   a=rtpmap:96 L16/48000/2\r\n";
        let parsed = AudioSession::parse(sdp).unwrap();
        assert_eq!(parsed.multicast_addr, "239.9.9.9");
        assert_eq!(parsed.profile.encoding, Encoding::L16);
    }
}
