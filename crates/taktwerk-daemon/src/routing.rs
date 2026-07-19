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

use taktwerk_discovery::MdnsDiscovery;
use tracing::{debug, info, warn};

use crate::state::{now_unix, AppState, NmosPeer, RxControl, NMOS_RX_ID};
use crate::tasks::start_rx;

/// Baut die SDP eines Streams aus Gruppe/Port/Kanälen.
fn build_sdp(host: &str, group: &str, port: u16, channels: u8) -> String {
    AudioSession {
        session_name: format!("route {group}:{port}"),
        origin_unicast: host.to_string(),
        multicast_addr: group.to_string(),
        port,
        payload_type: 97,
        profile: StreamProfile::aes67(channels),
        refclk: None,
        mediaclk_offset: 0,
    }
    .to_sdp()
}

/// Stoppt das per Kreuzschiene gesteuerte RX-Abonnement (ohne zu awaiten).
fn stop_rx(state: &AppState) {
    if let Some(mut c) = state.rx.lock().unwrap().remove(NMOS_RX_ID) {
        if let Some(s) = c.shutdown.take() {
            let _ = s.send(true);
        }
        // Handle wird verworfen (Task läuft aus).
    }
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
        let state = &self.0;

        stop_rx(state); // evtl. bestehendes Abo lösen
                        // Profil direkt aus der SDP übernehmen (echte Kanalzahl + Paketzeit des Senders).
        let profile = session.profile;
        let (shutdown, packets, handle) = start_rx(
            state.node.interface,
            group,
            port,
            profile,
            state.monitor.clone(),
        )
        .map_err(|e| e.to_string())?;
        state.rx.lock().unwrap().insert(
            NMOS_RX_ID.to_string(),
            RxControl {
                running: true,
                source: Some(format!("{group}:{port}")),
                channels: profile.channels,
                packets,
                shutdown: Some(shutdown),
                handle: Some(handle),
                active_sdp: Some(sdp.to_string()),
            },
        );
        Ok(())
    }

    fn disconnect(&self) -> Result<(), String> {
        stop_rx(&self.0);
        Ok(())
    }

    fn active_sdp(&self) -> Option<String> {
        self.0
            .rx
            .lock()
            .unwrap()
            .get(NMOS_RX_ID)
            .and_then(|c| c.active_sdp.clone())
    }

    fn connected(&self) -> bool {
        self.0.rx.lock().unwrap().contains_key(NMOS_RX_ID)
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
    // Tatsächlich laufende eigene Sende-Ströme (Multi-Stream).
    {
        let tx = state.tx.lock().unwrap();
        for (id, c) in tx.iter() {
            senders.push(json!({
                "id": format!("self-{id}"),
                "name": format!("{} · {id}", n.name),
                "group": id.split(':').next().unwrap_or(""),
                "port": c.dest.as_deref().and_then(|d| d.rsplit(':').next()).unwrap_or("5004"),
                "channels": c.channels,
                "via": "self-live",
            }));
        }
    }
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
    // Receiver-Spalten: eigener (Kreuzschienen-Sink) + entdeckte fremde NMOS-Nodes.
    let (self_connected, self_source) = {
        let rx = state.rx.lock().unwrap();
        match rx.get(NMOS_RX_ID) {
            Some(c) => (true, c.source.clone()),
            None => (false, None),
        }
    };
    let mut receivers = vec![json!({
        "id": uuid_from(&format!("{}:receiver", n.name)),
        "name": format!("{} (self)", n.name),
        "nmos_host": n.nmos_host,
        "nmos_port": n.nmos_port,
        "connected": self_connected,
        "source": self_source,
    })];
    {
        let peers = state.nmos_peers.lock().unwrap();
        for peer in peers.values() {
            for (rid, label) in &peer.receivers {
                receivers.push(json!({
                    "id": rid,
                    "name": label,
                    "nmos_host": peer.host,
                    "nmos_port": peer.port,
                    "connected": false,
                    "source": null,
                    "last_seen": peer.last_seen,
                }));
            }
        }
    }

    Json(json!({ "senders": senders, "receivers": receivers }))
}

/// Browst NMOS-Node-APIs per mDNS und sammelt deren Receiver (IS-04) als
/// steuerbare Ziele der Kreuzschiene. Der eigene Node wird übersprungen.
pub async fn nmos_discovery_task(state: AppState, mdns: MdnsDiscovery) {
    let mut rx = match mdns.browse_nmos_nodes() {
        Ok(r) => r,
        Err(e) => {
            warn!("NMOS-Node-Discovery nicht verfügbar: {e}");
            return;
        }
    };
    info!("NMOS-Node-Discovery (mDNS) aktiv");
    while let Some(svc) = rx.recv().await {
        // Eigenen Node überspringen.
        if svc.port == state.node.nmos_port {
            continue;
        }
        let host = svc
            .addr
            .map(|a| a.to_string())
            .unwrap_or_else(|| svc.host.clone());
        match taktwerk_router::controller::get_json(&host, svc.port, "/x-nmos/node/v1.3/receivers")
            .await
        {
            Ok(body) => {
                let recv: Vec<(String, String)> = serde_json::from_str::<Vec<Value>>(&body)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|r| {
                        let id = r["id"].as_str()?.to_string();
                        let label = r["label"].as_str().unwrap_or("").to_string();
                        Some((id, label))
                    })
                    .collect();
                if !recv.is_empty() {
                    debug!(instance = %svc.instance, count = recv.len(), "NMOS-Node: Receiver übernommen");
                    state.nmos_peers.lock().unwrap().insert(
                        svc.instance.clone(),
                        NmosPeer {
                            host,
                            port: svc.port,
                            receivers: recv,
                            last_seen: now_unix(),
                        },
                    );
                }
            }
            Err(e) => debug!(%host, "IS-04-Receiver-Abfrage fehlgeschlagen: {e}"),
        }
    }
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
