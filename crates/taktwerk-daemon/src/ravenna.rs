//! RAVENNA-Integration: mDNS-Advertise + RTSP-Server (wir sind eine RAVENNA-
//! Session) und mDNS-Discovery + RTSP-`DESCRIBE` (wir finden fremde Sessions).
//!
//! Die Medien selbst (RTP L24, PTP, SDP) sind bereits RAVENNA-kompatibel; hier
//! liegt nur die RAVENNA-typische Discovery-/Beschreibungs-Schicht.

use std::net::{Ipv4Addr, SocketAddr};

use taktwerk_core::sdp::AudioSession;
use taktwerk_discovery::rtsp;
use taktwerk_discovery::{describe, MdnsDiscovery};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

use crate::state::{now_unix, AppState, DiscoveredEntry};

/// Multicast-Gruppe/Port, unter denen wir unseren Stream als RAVENNA-Session
/// beschreiben (identisch zum TX-/NMOS-Default).
const RAVENNA_GROUP: &str = "239.69.83.67";
const RAVENNA_PORT: u16 = 5004;

/// Baut die SDP unseres Knoten-Streams (für RTSP `DESCRIBE`). Wird pro Anfrage
/// gebaut → die Clock-Referenz ist live (echte GMID bei aktivem PTP, sonst ohne).
fn node_sdp(state: &AppState) -> String {
    let n = &state.node;
    AudioSession {
        session_name: n.name.clone(),
        origin_unicast: n.interface.to_string(),
        multicast_addr: RAVENNA_GROUP.to_string(),
        port: RAVENNA_PORT,
        payload_type: 97,
        profile: n.profile,
        refclk: state.ptp_refclk(),
        mediaclk_offset: 0,
    }
    .to_sdp()
}

/// RTSP-Server: beantwortet `OPTIONS`/`DESCRIBE` mit unserer aktuellen SDP.
pub async fn rtsp_server(addr: SocketAddr, state: AppState) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!(%addr, "RTSP-Server-Bind fehlgeschlagen: {e}");
            return;
        }
    };
    info!(%addr, "RTSP-Server aktiv (RAVENNA DESCRIBE)");
    loop {
        let (mut sock, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                warn!("RTSP-accept-Fehler: {e}");
                continue;
            }
        };
        let sdp = node_sdp(&state);
        tokio::spawn(async move {
            let mut buf = Vec::with_capacity(1024);
            let mut tmp = [0u8; 512];
            // Anfrage bis zum Header-Ende lesen.
            loop {
                match sock.read(&mut tmp).await {
                    Ok(0) => return,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
                            break;
                        }
                    }
                    Err(_) => return,
                }
            }
            let req = String::from_utf8_lossy(&buf);
            let resp = rtsp::handle_request(&req, &sdp);
            debug!(%peer, "RTSP-Anfrage beantwortet");
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

/// Bietet den eigenen Stream als RAVENNA-Session per mDNS an.
pub fn advertise(mdns: &MdnsDiscovery, node_name: &str, iface: Ipv4Addr, rtsp_port: u16) {
    if iface.is_unspecified() {
        warn!("RAVENNA-Advertise übersprungen: kein konkretes Interface (TAKTWERK_IFACE)");
        return;
    }
    let host = sanitize_host(node_name);
    let path = format!("/by-name/{node_name}");
    match mdns.register_session(node_name, &host, iface, rtsp_port, &path) {
        Ok(()) => {
            info!(instance = node_name, %iface, rtsp_port, "RAVENNA-Session angeboten (mDNS)")
        }
        Err(e) => warn!("RAVENNA-Advertise fehlgeschlagen: {e}"),
    }
}

/// Browst RAVENNA-Sessions per mDNS, holt ihre SDP per RTSP und trägt sie in die
/// Discovery-/Geräte-Liste ein.
pub async fn discovery_task(state: AppState, mdns: MdnsDiscovery) {
    let mut rx = match mdns.browse() {
        Ok(r) => r,
        Err(e) => {
            warn!("RAVENNA-Discovery (mDNS) nicht verfügbar: {e}");
            return;
        }
    };
    info!("RAVENNA-Discovery (mDNS) aktiv");
    while let Some(session) = rx.recv().await {
        let host = session
            .addr
            .map(|a| a.to_string())
            .unwrap_or_else(|| session.host.clone());
        debug!(instance = %session.instance, %host, port = session.port, "RAVENNA-Session gefunden");
        match describe(&host, session.port, &session.path).await {
            Ok(sdp) => match AudioSession::parse(&sdp) {
                Ok(s) => {
                    let ip = match session.addr {
                        Some(std::net::IpAddr::V4(v4)) => v4,
                        _ => Ipv4Addr::UNSPECIFIED,
                    };
                    state
                        .monitor
                        .lock()
                        .unwrap()
                        .note_device(ip, Some(s.session_name.clone()));
                    let key = fnv16(session.instance.as_bytes());
                    state.discovered.lock().unwrap().insert(
                        key,
                        DiscoveredEntry {
                            session_name: s.session_name,
                            multicast_addr: s.multicast_addr,
                            port: s.port,
                            channels: s.profile.channels,
                            sample_rate: s.profile.sample_rate,
                            source: ip,
                            via: "RAVENNA",
                            last_seen: now_unix(),
                        },
                    );
                    info!(instance = %session.instance, "RAVENNA-Session übernommen (SDP via RTSP)");
                }
                Err(e) => warn!("RAVENNA-SDP nicht parsebar: {e}"),
            },
            Err(e) => debug!(%host, "RTSP-DESCRIBE fehlgeschlagen: {e}"),
        }
    }
}

/// Hostname aus Anzeigename ableiten (nur a–z0–9-, sonst '-').
fn sanitize_host(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if s.is_empty() {
        "taktwerk".into()
    } else {
        s
    }
}

/// FNV-1a-16 (gefaltet) — Schlüssel für die Discovery-Tabelle.
fn fnv16(bytes: &[u8]) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    ((h >> 16) ^ (h & 0xFFFF)) as u16
}
