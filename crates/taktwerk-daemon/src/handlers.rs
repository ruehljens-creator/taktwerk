//! HTTP-Handler + JSON-DTOs der REST-API.
//!
//! Endpunkte:
//! - `GET  /health`               — Lebenszeichen
//! - `GET  /node`                 — Knoten-Konfiguration (Name, Interface, Profil)
//! - `GET  /devices`              — alle gesehenen Geräte (IP, Name, Traffic)
//! - `GET  /traffic`              — Netzwerk-Traffic je Protokoll + Summe
//! - `GET  /streams/discovered`   — per SAP entdeckte fremde Streams
//! - `GET  /streams/tx`           — Status des Sende-Stroms
//! - `POST /streams/tx/{start,stop}` — Sende-Strom steuern (+ SAP-Announce)
//! - `GET  /streams/rx`           — Status des Empfangs-Abonnements
//! - `POST /streams/rx/{subscribe,unsubscribe}` — Empfang steuern

use std::net::Ipv4Addr;
use std::sync::atomic::Ordering;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::{Deserialize, Serialize};

use taktwerk_core::StreamProfile;

use crate::state::AppState;
use crate::tasks::{start_rx, start_tx, TxParams};

// ---------- DTOs ----------

#[derive(Serialize)]
pub struct ProfileDto {
    pub channels: u8,
    pub sample_rate: u32,
    pub ptime_us: u32,
    pub encoding: String,
}

impl From<StreamProfile> for ProfileDto {
    fn from(p: StreamProfile) -> Self {
        Self {
            channels: p.channels,
            sample_rate: p.sample_rate,
            ptime_us: p.ptime_us,
            encoding: p.encoding.rtpmap_name().to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct NodeDto {
    pub name: String,
    pub interface: String,
    pub profile: ProfileDto,
}

#[derive(Serialize)]
pub struct DiscoveredDto {
    pub msg_id_hash: u16,
    pub session_name: String,
    pub multicast_addr: String,
    pub port: u16,
    pub channels: u8,
    pub sample_rate: u32,
    pub source: String,
    /// Entdeckungsweg: "SAP" oder "RAVENNA".
    pub via: &'static str,
    pub last_seen: u64,
}

#[derive(Serialize)]
pub struct TxStatusDto {
    pub running: bool,
    pub dest: Option<String>,
    pub channels: u8,
    pub packets_sent: u64,
}

#[derive(Deserialize)]
pub struct TxStartRequest {
    /// Multicast-Gruppe, Default 239.69.83.67.
    pub group: Option<String>,
    /// Port, Default 5004.
    pub port: Option<u16>,
    /// Kanäle (Level A: ≤8), Default 2.
    pub channels: Option<u8>,
}

#[derive(Serialize)]
pub struct RxStatusDto {
    pub running: bool,
    pub source: Option<String>,
    pub channels: u8,
    pub packets_recv: u64,
}

#[derive(Deserialize)]
pub struct RxSubscribeRequest {
    /// Multicast-Gruppe des zu empfangenden Streams (Pflicht).
    pub group: String,
    /// Port, Default 5004.
    pub port: Option<u16>,
    /// Kanäle (Level A: ≤8), Default 2.
    pub channels: Option<u8>,
}

// ---------- Handler ----------

/// Die eingebettete Web-Oberfläche (statisch, spricht die REST-API gleicher Herkunft an).
pub async fn ui() -> Html<&'static str> {
    Html(include_str!("../ui/index.html"))
}

pub async fn health(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "node": state.node.name }))
}

pub async fn node(State(state): State<AppState>) -> Json<NodeDto> {
    Json(NodeDto {
        name: state.node.name.clone(),
        interface: state.node.interface.to_string(),
        profile: state.node.profile.into(),
    })
}

/// Alle im Netz gesehenen Geräte (IP, Name, Traffic je Protokoll).
pub async fn devices(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.monitor.lock().unwrap().devices_json())
}

/// Gesamter Netzwerk-Traffic (pro Protokoll + Summe, mit 1-s-Raten).
pub async fn traffic(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(state.monitor.lock().unwrap().traffic_json())
}

/// PTP-Slave-Status (Lock, Offset, Pfad-Verzögerung, Grandmaster).
pub async fn ptp(State(state): State<AppState>) -> Json<serde_json::Value> {
    let st = state.ptp.lock().unwrap().clone();
    let gm = st.grandmaster.map(|id| {
        id.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    });
    Json(serde_json::json!({
        "enabled": state.node.ptp_slave,
        "synced": st.synced,
        "offset_ns": st.offset_ns,
        "path_delay_ns": st.path_delay_ns,
        "grandmaster": gm,
    }))
}

pub async fn discovered(State(state): State<AppState>) -> Json<Vec<DiscoveredDto>> {
    let map = state.discovered.lock().unwrap();
    let mut list: Vec<DiscoveredDto> = map
        .iter()
        .map(|(hash, e)| DiscoveredDto {
            msg_id_hash: *hash,
            session_name: e.session_name.clone(),
            multicast_addr: e.multicast_addr.clone(),
            port: e.port,
            channels: e.channels,
            sample_rate: e.sample_rate,
            source: e.source.to_string(),
            via: e.via,
            last_seen: e.last_seen,
        })
        .collect();
    list.sort_by(|a, b| a.session_name.cmp(&b.session_name));
    Json(list)
}

pub async fn tx_status(State(state): State<AppState>) -> Json<TxStatusDto> {
    Json(current_tx_status(&state))
}

pub async fn tx_start(
    State(state): State<AppState>,
    body: Option<Json<TxStartRequest>>,
) -> Result<Json<TxStatusDto>, (StatusCode, String)> {
    let req = body.map(|Json(b)| b).unwrap_or(TxStartRequest {
        group: None,
        port: None,
        channels: None,
    });

    let group: Ipv4Addr = req
        .group
        .as_deref()
        .unwrap_or("239.69.83.67")
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "ungültige group-Adresse".into()))?;
    let port = req.port.unwrap_or(5004);
    let channels = req.channels.unwrap_or(2);
    if channels == 0 || channels > taktwerk_core::MAX_CHANNELS {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("channels muss 1..={} sein", taktwerk_core::MAX_CHANNELS),
        ));
    }

    let mut tx = state.tx.lock().unwrap();
    if tx.running {
        return Err((StatusCode::CONFLICT, "TX läuft bereits".into()));
    }

    // Paketzeit passend zur Kanalzahl (≤8 = Level A/1 ms, sonst kürzer → MTU-safe).
    let profile = StreamProfile::aes67(channels);
    let params = TxParams {
        iface: state.node.interface,
        group,
        port,
        profile,
        payload_type: 97,
        ssrc: 0x5441_4B54, // "TAKT"
        node_name: state.node.name.clone(),
        clock: state.clock.clone(),
    };

    let (shutdown, packets, handle) =
        start_tx(params).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    tx.running = true;
    tx.dest = Some(format!("{group}:{port}"));
    tx.channels = channels;
    tx.packets = packets;
    tx.shutdown = Some(shutdown);
    tx.handle = Some(handle);
    drop(tx);

    Ok(Json(current_tx_status(&state)))
}

pub async fn tx_stop(State(state): State<AppState>) -> Json<TxStatusDto> {
    let (shutdown, handle) = {
        let mut tx = state.tx.lock().unwrap();
        tx.running = false;
        (tx.shutdown.take(), tx.handle.take())
    };
    if let Some(s) = shutdown {
        let _ = s.send(true);
    }
    if let Some(h) = handle {
        let _ = h.await;
    }
    Json(current_tx_status(&state))
}

fn current_tx_status(state: &AppState) -> TxStatusDto {
    let tx = state.tx.lock().unwrap();
    TxStatusDto {
        running: tx.running,
        dest: tx.dest.clone(),
        channels: tx.channels,
        packets_sent: tx.packets.load(Ordering::Relaxed),
    }
}

pub async fn rx_status(State(state): State<AppState>) -> Json<RxStatusDto> {
    Json(current_rx_status(&state))
}

pub async fn rx_subscribe(
    State(state): State<AppState>,
    Json(req): Json<RxSubscribeRequest>,
) -> Result<Json<RxStatusDto>, (StatusCode, String)> {
    let group: Ipv4Addr = req
        .group
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "ungültige group-Adresse".into()))?;
    let port = req.port.unwrap_or(5004);
    let channels = req.channels.unwrap_or(2);
    if channels == 0 || channels > taktwerk_core::MAX_CHANNELS {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("channels muss 1..={} sein", taktwerk_core::MAX_CHANNELS),
        ));
    }

    let mut rx = state.rx.lock().unwrap();
    if rx.running {
        return Err((StatusCode::CONFLICT, "RX-Abonnement läuft bereits".into()));
    }

    let profile = StreamProfile::aes67(channels);
    let (shutdown, packets, handle) = start_rx(
        state.node.interface,
        group,
        port,
        profile,
        state.monitor.clone(),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    rx.running = true;
    rx.source = Some(format!("{group}:{port}"));
    rx.channels = channels;
    rx.packets = packets;
    rx.shutdown = Some(shutdown);
    rx.handle = Some(handle);
    drop(rx);

    Ok(Json(current_rx_status(&state)))
}

pub async fn rx_unsubscribe(State(state): State<AppState>) -> Json<RxStatusDto> {
    let (shutdown, handle) = {
        let mut rx = state.rx.lock().unwrap();
        rx.running = false;
        (rx.shutdown.take(), rx.handle.take())
    };
    if let Some(s) = shutdown {
        let _ = s.send(true);
    }
    if let Some(h) = handle {
        let _ = h.await;
    }
    Json(current_rx_status(&state))
}

fn current_rx_status(state: &AppState) -> RxStatusDto {
    let rx = state.rx.lock().unwrap();
    RxStatusDto {
        running: rx.running,
        source: rx.source.clone(),
        channels: rx.channels,
        packets_recv: rx.packets.load(Ordering::Relaxed),
    }
}
