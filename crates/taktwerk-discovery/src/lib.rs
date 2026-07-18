//! # taktwerk-discovery
//!
//! **RAVENNA-Discovery** über die Wege, die RAVENNA (anders als AES67/SAP)
//! tatsächlich nutzt:
//! - [`mdns`] — **mDNS/DNS-SD**: RAVENNA-Sessions im Netz **finden** und den
//!   eigenen Stream als RAVENNA-Session **anbieten** (via `mdns-sd`, reines Rust).
//! - [`rtsp`] — **RTSP `DESCRIBE`**: die SDP einer Session **holen** (Client) und
//!   die eigene SDP **liefern** (Server-Antwort).
//!
//! Die Medien-/Timing-Basis (RTP L24, PTP, SDP mit RFC 7273) ist bereits
//! RAVENNA-kompatibel (`taktwerk-core`/`taktwerk-net`); dieses Crate ergänzt die
//! fehlende Discovery-/Beschreibungs-Schicht.

pub mod mdns;
pub mod rtsp;

pub use mdns::{MdnsDiscovery, RavennaSession, RAVENNA_SERVICE, RAVENNA_SUBTYPE};
pub use rtsp::describe;
