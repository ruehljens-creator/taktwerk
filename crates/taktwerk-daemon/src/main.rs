//! # taktwerkd — Taktwerk Control-Plane-Daemon
//!
//! Startet einen headless AES67-Knoten mit REST-API (Axum): sendet auf Wunsch
//! einen Stream (TX-Loop, [`tasks::start_tx`]), kündigt ihn per SAP an und
//! entdeckt fremde Streams per SAP-Discovery ([`tasks::discovery_task`]).
//!
//! Konfiguration über Umgebungsvariablen (mit Defaults):
//! - `TAKTWERK_NAME`  — Anzeigename            (Default: Hostname-artig "taktwerk")
//! - `TAKTWERK_IFACE` — Interface-IP (IPv4)    (Default: 0.0.0.0 = OS-Default)
//! - `TAKTWERK_HTTP`  — REST-Bind-Adresse      (Default: 127.0.0.1:7788)
//! - `TAKTWERK_CH`    — Default-Kanäle          (Default: 2)

mod handlers;
mod state;
mod tasks;

use std::net::{Ipv4Addr, SocketAddr};

use axum::routing::{get, post};
use axum::Router;

use taktwerk_core::StreamProfile;

use crate::state::{AppState, NodeInfo};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::var("TAKTWERK_NAME").unwrap_or_else(|_| "taktwerk".into());
    let iface: Ipv4Addr = std::env::var("TAKTWERK_IFACE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(Ipv4Addr::UNSPECIFIED);
    let http: SocketAddr = std::env::var("TAKTWERK_HTTP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 7788)));
    let channels: u8 = std::env::var("TAKTWERK_CH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);
    let nmos_http: SocketAddr = std::env::var("TAKTWERK_NMOS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 7789)));

    let node = NodeInfo {
        name: name.clone(),
        interface: iface,
        profile: StreamProfile::level_a(channels),
    };
    let app_state = AppState::new(node);

    // SAP-Discovery im Hintergrund starten.
    tokio::spawn(tasks::discovery_task(iface, app_state.clone()));

    // NMOS-Control-Plane (IS-04/IS-05) als eigener Server (berührt den Audiopfad nicht).
    {
        let nmos_node = std::sync::Arc::new(taktwerk_router::NmosNode::new(
            name.clone(),
            iface.to_string(),
            nmos_http.port(),
            iface.to_string(),
            StreamProfile::level_a(channels),
            "239.69.83.67",
            5004,
        ));
        let nmos_app = taktwerk_router::app(nmos_node);
        match tokio::net::TcpListener::bind(nmos_http).await {
            Ok(l) => {
                println!("NMOS IS-04/IS-05 auf http://{nmos_http}/x-nmos/");
                tokio::spawn(async move {
                    let _ = axum::serve(l, nmos_app).await;
                });
            }
            Err(e) => eprintln!("NMOS-Server konnte nicht binden ({nmos_http}): {e}"),
        }
    }

    let app = Router::new()
        .route("/health", get(handlers::health))
        .route("/node", get(handlers::node))
        .route("/streams/discovered", get(handlers::discovered))
        .route("/streams/tx", get(handlers::tx_status))
        .route("/streams/tx/start", post(handlers::tx_start))
        .route("/streams/tx/stop", post(handlers::tx_stop))
        .route("/streams/rx", get(handlers::rx_status))
        .route("/streams/rx/subscribe", post(handlers::rx_subscribe))
        .route("/streams/rx/unsubscribe", post(handlers::rx_unsubscribe))
        .with_state(app_state);

    println!("taktwerkd \"{name}\" — REST auf http://{http}  (Interface {iface})");
    let listener = tokio::net::TcpListener::bind(http).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Beendet den Server sauber bei Ctrl-C.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    println!("\nShutdown-Signal empfangen, beende taktwerkd.");
}
