//! Kreuzschiene: die steuerbare Senke ([`DaemonReceiverControl`], von IS-05
//! `PATCH staged` getrieben) sowie die REST-Endpunkte `/registry`
//! (Sender + Receiver) und `/route` (Koppelpunkt setzen/lösen via IS-05).

use std::net::Ipv4Addr;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use taktwerk_core::sdp::AudioSession;
use taktwerk_core::StreamProfile;
use taktwerk_router::ids::uuid_from;
use taktwerk_router::{controller, ReceiverControl};

use crate::state::AppState;
use crate::tasks::start_rx;

/// Baut die SDP eines Streams aus Gruppe/Port/Kanälen.
fn build_sdp(host: &str, group: &str, port: u16, channels: u8) -> String {
    AudioSession {
        session_name: format!("route {group}:{port}"),
        origin_unicast: host.to_string(),
        multicast_addr: group.to_string(),
        port,
        payload_type: 97,
        profile: StreamProfile::level_a(channels),
        refclk: None,
        mediaclk_offset: 0,
    }
    .to_sdp()
}

/// Stoppt ein laufendes RX-Abonnement (ohne zu awaiten).
fn stop_rx(state: &AppState) {
    let mut rx = state.rx.lock().unwrap();
    rx.running = false;
    if let Some(s) = rx.shutdown.take() {
        let _ = s.send(true);
    }
    rx.handle.take(); // Task läuft aus, Handle verwerfen
    rx.active_sdp = None;
}

/// Die steuerbare Senke: setzt ein IS-05-`PATCH` in ein RX-Abonnement um.
pub struct DaemonReceiverControl(pub AppState);

impl ReceiverControl for DaemonReceiverControl {
    fn connect(&self, sdp: &str) -> Result<(), String> {
        let session = AudioSession::parse(sdp).map_err(|e| e.to_string())?;
        let group: Ipv4Addr = session
            .multicast_addr
            .parse()
            .map_err(|_| format!("ungültige Multicast-Gruppe: {}", session.multicast_addr))?;
        let port = session.port;
        let channels = session.profile.channels;
        let state = &self.0;

        stop_rx(state); // evtl. bestehendes Abo lösen
        let profile = StreamProfile::level_a(channels);
        let (shutdown, packets, handle) = start_rx(
            state.node.interface,
            group,
            port,
            profile,
            state.monitor.clone(),
        )
        .map_err(|e| e.to_string())?;
        let mut rx = state.rx.lock().unwrap();
        rx.running = true;
        rx.source = Some(format!("{group}:{port}"));
        rx.channels = channels;
        rx.packets = packets;
        rx.shutdown = Some(shutdown);
        rx.handle = Some(handle);
        rx.active_sdp = Some(sdp.to_string());
        Ok(())
    }

    fn disconnect(&self) -> Result<(), String> {
        stop_rx(&self.0);
        Ok(())
    }

    fn active_sdp(&self) -> Option<String> {
        self.0.rx.lock().unwrap().active_sdp.clone()
    }

    fn connected(&self) -> bool {
        self.0.rx.lock().unwrap().running
    }
}

/// `GET /registry` — alle bekannten **Sender** (eigener + entdeckte) und
/// **Receiver** (eigener, inkl. NMOS-Koordinaten für den Koppelpunkt).
pub async fn registry(State(state): State<AppState>) -> Json<Value> {
    let n = &state.node;
    // Eigener Sender (der Default-Stream, den wir anbieten).
    let mut senders = vec![json!({
        "id": uuid_from(&format!("{}:sender", n.name)),
        "name": n.name,
        "group": "239.69.83.67",
        "port": 5004,
        "channels": n.profile.channels,
        "via": "self",
    })];
    // Entdeckte Sender (SAP/RAVENNA).
    {
        let disc = state.discovered.lock().unwrap();
        for e in disc.values() {
            senders.push(json!({
                "id": format!("disc-{}", e.session_name),
                "name": e.session_name,
                "group": e.multicast_addr,
                "port": e.port,
                "channels": e.channels,
                "via": e.via,
            }));
        }
    }
    // Eigener Receiver (steuerbare Senke) mit NMOS-Koordinaten.
    let receivers = json!([{
        "id": uuid_from(&format!("{}:receiver", n.name)),
        "name": n.name,
        "nmos_host": n.nmos_host,
        "nmos_port": n.nmos_port,
        "connected": state.rx.lock().unwrap().running,
        "source": state.rx.lock().unwrap().source,
    }]);

    Json(json!({ "senders": senders, "receivers": receivers }))
}

/// Request für `POST /route` — Koppelpunkt setzen/lösen.
#[derive(Deserialize)]
pub struct RouteRequest {
    /// NMOS-Host des Ziel-Receivers.
    pub host: String,
    /// NMOS-Port des Ziel-Receivers.
    pub port: u16,
    /// NMOS-Receiver-ID.
    pub receiver_id: String,
    /// Sender-Multicast-Gruppe (beim Verbinden).
    pub group: Option<String>,
    /// Sender-Medien-Port (Default 5004).
    pub media_port: Option<u16>,
    /// Kanäle (Default 2).
    pub channels: Option<u8>,
    /// true → Koppelpunkt lösen statt setzen.
    pub disconnect: Option<bool>,
}

/// `POST /route` — verbindet einen Sender mit einem (NMOS-)Receiver via IS-05,
/// oder löst die Verbindung. Das ist der Klick auf einen Koppelpunkt im Grid.
pub async fn route(
    State(state): State<AppState>,
    Json(req): Json<RouteRequest>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let _ = &state; // (state derzeit nicht direkt nötig; Symmetrie/Erweiterung)
    let code = if req.disconnect.unwrap_or(false) {
        controller::disconnect_receiver(&req.host, req.port, &req.receiver_id)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
    } else {
        let group = req
            .group
            .ok_or((StatusCode::BAD_REQUEST, "group fehlt".to_string()))?;
        let mport = req.media_port.unwrap_or(5004);
        let channels = req.channels.unwrap_or(2);
        let sdp = build_sdp(&req.host, &group, mport, channels);
        controller::connect_receiver(&req.host, req.port, &req.receiver_id, &sdp)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
    };
    Ok(Json(
        json!({ "status": code, "ok": (200..300).contains(&code) }),
    ))
}
