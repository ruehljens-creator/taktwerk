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
//! - `TAKTWERK_NMOS`  — NMOS-Bind-Adresse      (Default: 127.0.0.1:7789)
//! - `TAKTWERK_CH`    — Default-Kanäle          (Default: 2)
//! - `TAKTWERK_LOG` / `TAKTWERK_LOG_FILE` — Debug-Log (siehe [`logging`]).

mod config;
mod handlers;
mod logging;
mod monitor;
mod ravenna;
mod routing;
mod state;
mod tasks;

use std::net::{Ipv4Addr, SocketAddr};

use axum::routing::{get, post};
use axum::Router;
use tracing::{error, info};

use taktwerk_core::StreamProfile;

use crate::state::{AppState, NodeInfo};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Optionale TOML-Konfig vor allem anderen in die Umgebung spiegeln (Env hat
    // Vorrang). Danach liest der restliche Daemon wie gewohnt aus `TAKTWERK_*`.
    config::FileConfig::load().apply_to_env();

    // Debug-Log so früh wie möglich einrichten; Guard bis Programmende halten.
    let _log_guard = logging::init();

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
        .unwrap_or(2)
        .clamp(1, taktwerk_core::MAX_CHANNELS);
    let nmos_http: SocketAddr = std::env::var("TAKTWERK_NMOS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 7789)));
    let ptp_slave = std::env::var("TAKTWERK_PTP_SLAVE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let node = NodeInfo {
        name: name.clone(),
        interface: iface,
        profile: StreamProfile::aes67(channels),
        ptp_slave,
        nmos_host: iface.to_string(),
        nmos_port: nmos_http.port(),
    };
    let mut app_state = AppState::new(node);

    // PTP-Slave optional: an den Grandmaster locken und die Media-Clock darauf
    // ausrichten (PtpTimeSource). Der Guard hält den Slave am Leben.
    let _ptp_guard = if ptp_slave && !iface.is_unspecified() {
        let pts =
            taktwerk_core::ptp::servo::PtpTimeSource::new(taktwerk_core::clock::SystemTimeSource);
        let offset_handle = pts.offset_handle();
        app_state.clock = std::sync::Arc::new(pts);
        let identity = clock_identity_from(&name);
        match taktwerk_net::PtpSlave::bind(iface, identity, offset_handle, app_state.ptp.clone()) {
            Ok(slave) => {
                let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
                tokio::spawn(async move {
                    let _ = slave.run(stop_rx).await;
                });
                info!(%iface, "PTP-Slave aktiv (Lock an Grandmaster)");
                Some(stop_tx)
            }
            Err(e) => {
                error!("PTP-Slave-Start fehlgeschlagen: {e}");
                None
            }
        }
    } else {
        None
    };

    // Hintergrund-Tasks: SAP-Discovery, PTP-Monitor, Traffic-Raten-Ticker.
    tokio::spawn(tasks::discovery_task(iface, app_state.clone()));
    tokio::spawn(tasks::ptp_monitor_task(iface, app_state.clone()));
    tokio::spawn(tasks::rate_task(app_state.clone()));

    // RAVENNA: mDNS-Discovery + eigenen Stream als RAVENNA-Session anbieten
    // (mDNS-Advertise + RTSP-Server für DESCRIBE).
    let rtsp_http: SocketAddr = std::env::var("TAKTWERK_RTSP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 8554)));
    match taktwerk_discovery::MdnsDiscovery::new() {
        Ok(mdns) => {
            tokio::spawn(ravenna::rtsp_server(rtsp_http, app_state.clone()));
            ravenna::advertise(&mdns, &name, iface, rtsp_http.port());
            // Eigenen Node als NMOS-Node-API annoncieren (für die Kreuzschiene).
            if !iface.is_unspecified() {
                if let Err(e) = mdns.register_nmos_node(&name, &name, iface, nmos_http.port()) {
                    error!("NMOS-Node-Advertise fehlgeschlagen: {e}");
                }
            }
            tokio::spawn(routing::nmos_discovery_task(
                app_state.clone(),
                mdns.clone(),
            ));
            tokio::spawn(ravenna::discovery_task(app_state.clone(), mdns));
        }
        Err(e) => error!("mDNS/RAVENNA nicht verfügbar: {e}"),
    }

    // NMOS-Control-Plane (IS-04/IS-05) als eigener Server (berührt den Audiopfad nicht).
    {
        let nmos_node = std::sync::Arc::new(taktwerk_router::NmosNode::new(
            name.clone(),
            iface.to_string(),
            nmos_http.port(),
            iface.to_string(),
            StreamProfile::aes67(channels),
            "239.69.83.67",
            5004,
        ));
        // Steuerbare Senke (unser Receiver) für IS-05-PATCH aus der Kreuzschiene.
        let control: std::sync::Arc<dyn taktwerk_router::ReceiverControl> =
            std::sync::Arc::new(routing::DaemonReceiverControl(app_state.clone()));
        let nmos_app = taktwerk_router::app(nmos_node, control);
        match tokio::net::TcpListener::bind(nmos_http).await {
            Ok(l) => {
                info!(%nmos_http, "NMOS IS-04/IS-05 aktiv unter /x-nmos/");
                tokio::spawn(async move {
                    let _ = axum::serve(l, nmos_app).await;
                });
            }
            Err(e) => error!(%nmos_http, "NMOS-Server konnte nicht binden: {e}"),
        }
    }

    let app = Router::new()
        .route("/", get(handlers::ui))
        .route("/ui", get(handlers::ui))
        .route("/health", get(handlers::health))
        .route("/node", get(handlers::node))
        .route("/devices", get(handlers::devices))
        .route("/traffic", get(handlers::traffic))
        .route("/ptp", get(handlers::ptp))
        .route("/registry", get(routing::registry))
        .route("/route", post(routing::route))
        .route("/streams/discovered", get(handlers::discovered))
        .route("/streams/tx", get(handlers::tx_status))
        .route("/streams/tx/start", post(handlers::tx_start))
        .route("/streams/tx/stop", post(handlers::tx_stop))
        .route("/streams/rx", get(handlers::rx_status))
        .route("/streams/rx/subscribe", post(handlers::rx_subscribe))
        .route("/streams/rx/unsubscribe", post(handlers::rx_unsubscribe))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(http).await?;
    info!(node = %name, %http, %iface, "taktwerkd bereit — REST + Web-UI aktiv");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

/// Leitet eine (deterministische) PTP-Clock-Identity aus dem Knotennamen ab.
/// EUI-64-Form mit gesetztem „locally administered"-Bit; stabil pro Name.
fn clock_identity_from(name: &str) -> [u8; 8] {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut id = h.to_be_bytes();
    id[0] = (id[0] & 0xfe) | 0x02; // unicast + locally administered
    id
}

/// Beendet den Server sauber bei Ctrl-C.
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    info!("Shutdown-Signal empfangen, beende taktwerkd.");
}
