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

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Html;
use axum::Json;
use serde::{Deserialize, Serialize};

use taktwerk_core::StreamProfile;

use crate::state::{AppState, RxControl, TxControl};
use crate::tasks::{start_rx, start_tx, TxParams};

/// Query-Parameter `?id=` zum gezielten Stoppen eines Stroms (fehlt → alle).
#[derive(Deserialize)]
pub struct StreamIdQuery {
    pub id: Option<String>,
}

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
    /// Stream-Schlüssel ("group:port").
    pub id: String,
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
    /// Abo-Schlüssel ("group:port" bzw. der Kreuzschienen-Receiver).
    pub id: String,
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

/// PTP-Status: Rolle (master/slave/off) + rollenspezifische Felder.
pub async fn ptp(State(state): State<AppState>) -> Json<serde_json::Value> {
    let fmt_id = |id: [u8; 8]| {
        id.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    };
    let role = if state.node.ptp_master {
        "master"
    } else if state.node.ptp_slave {
        "slave"
    } else {
        "off"
    };

    // Slave-Sicht.
    let st = state.ptp.lock().unwrap().clone();
    let grandmaster = st.grandmaster.map(fmt_id);
    // Master-Sicht.
    let m = state.ptp_master.lock().unwrap().clone();

    Json(serde_json::json!({
        "role": role,
        "enabled": state.node.ptp_slave || state.node.ptp_master,
        // Slave-Felder
        "synced": st.synced,
        "offset_ns": st.offset_ns,
        "path_delay_ns": st.path_delay_ns,
        "grandmaster": grandmaster,
        // Master-Felder
        "master_active": m.active,
        "announces_sent": m.announces_sent,
        "syncs_sent": m.syncs_sent,
        "delay_resps_sent": m.delay_resps_sent,
        "better_master": m.better_master.map(fmt_id),
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

/// `GET /streams/tx` — Liste **aller** laufenden Sende-Ströme.
pub async fn tx_status(State(state): State<AppState>) -> Json<Vec<TxStatusDto>> {
    Json(list_tx(&state))
}

fn list_tx(state: &AppState) -> Vec<TxStatusDto> {
    let mut v: Vec<TxStatusDto> = state
        .tx
        .lock()
        .unwrap()
        .iter()
        .map(|(id, c)| TxStatusDto {
            id: id.clone(),
            running: c.running,
            dest: c.dest.clone(),
            channels: c.channels,
            packets_sent: c.packets.load(Ordering::Relaxed),
        })
        .collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
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

    let id = format!("{group}:{port}");
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
        refclk: state.ptp_refclk(),
    };

    // Lock über Prüfung + Start + Insert halten (start_tx ist synchron, kein
    // await) — sonst könnten zwei gleichzeitige Requests derselben id beide die
    // Prüfung passieren und der zweite Insert würde den ersten Task verwaisen.
    let mut tx_map = state.tx.lock().unwrap();
    if tx_map.contains_key(&id) {
        return Err((StatusCode::CONFLICT, format!("TX {id} läuft bereits")));
    }
    let (shutdown, packets, handle) =
        start_tx(params).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let dto = TxStatusDto {
        id: id.clone(),
        running: true,
        dest: Some(id.clone()),
        channels,
        packets_sent: packets.load(Ordering::Relaxed),
    };
    tx_map.insert(
        id.clone(),
        TxControl {
            running: true,
            dest: Some(id),
            channels,
            packets,
            shutdown: Some(shutdown),
            handle: Some(handle),
        },
    );
    Ok(Json(dto))
}

/// `POST /streams/tx/stop?id=group:port` — einen Strom stoppen (ohne `id` alle).
pub async fn tx_stop(
    State(state): State<AppState>,
    Query(q): Query<StreamIdQuery>,
) -> Json<Vec<TxStatusDto>> {
    let removed = take_streams(&mut state.tx.lock().unwrap(), q.id.as_deref());
    for mut c in removed {
        if let Some(s) = c.shutdown.take() {
            let _ = s.send(true);
        }
        if let Some(h) = c.handle.take() {
            let _ = h.await;
        }
    }
    Json(list_tx(&state))
}

/// `GET /streams/rx` — Liste **aller** laufenden Empfangs-Abos.
pub async fn rx_status(State(state): State<AppState>) -> Json<Vec<RxStatusDto>> {
    Json(list_rx(&state))
}

fn list_rx(state: &AppState) -> Vec<RxStatusDto> {
    let mut v: Vec<RxStatusDto> = state
        .rx
        .lock()
        .unwrap()
        .iter()
        .map(|(id, c)| RxStatusDto {
            id: id.clone(),
            running: c.running,
            source: c.source.clone(),
            channels: c.channels,
            packets_recv: c.packets.load(Ordering::Relaxed),
        })
        .collect();
    v.sort_by(|a, b| a.id.cmp(&b.id));
    v
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

    let id = format!("{group}:{port}");
    // Wie bei tx_start: Lock über Prüfung + Start + Insert (start_rx ist synchron).
    let mut rx_map = state.rx.lock().unwrap();
    if rx_map.contains_key(&id) {
        return Err((StatusCode::CONFLICT, format!("RX {id} läuft bereits")));
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

    let dto = RxStatusDto {
        id: id.clone(),
        running: true,
        source: Some(id.clone()),
        channels,
        packets_recv: packets.load(Ordering::Relaxed),
    };
    rx_map.insert(
        id.clone(),
        RxControl {
            running: true,
            source: Some(id),
            channels,
            packets,
            shutdown: Some(shutdown),
            handle: Some(handle),
            active_sdp: None,
        },
    );
    Ok(Json(dto))
}

/// `POST /streams/rx/unsubscribe?id=group:port` — ein Abo lösen (ohne `id` alle).
pub async fn rx_unsubscribe(
    State(state): State<AppState>,
    Query(q): Query<StreamIdQuery>,
) -> Json<Vec<RxStatusDto>> {
    let removed = take_streams(&mut state.rx.lock().unwrap(), q.id.as_deref());
    for mut c in removed {
        if let Some(s) = c.shutdown.take() {
            let _ = s.send(true);
        }
        if let Some(h) = c.handle.take() {
            let _ = h.await;
        }
    }
    Json(list_rx(&state))
}

/// Entfernt entweder den Strom `id` oder (bei `None`) alle aus der Map und gibt
/// die entfernten Steuerzustände zum geordneten Stoppen zurück.
fn take_streams<T>(map: &mut std::collections::HashMap<String, T>, id: Option<&str>) -> Vec<T> {
    let keys: Vec<String> = match id {
        Some(k) if map.contains_key(k) => vec![k.to_string()],
        Some(_) => vec![],
        None => map.keys().cloned().collect(),
    };
    keys.into_iter().filter_map(|k| map.remove(&k)).collect()
}
