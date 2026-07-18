//! IS-05-**Receiver**-Endpunkte: machen den Knoten zu einer *steuerbaren* Senke.
//! Ein Controller (unsere Kreuzschiene oder ein fremder) verbindet uns per
//! `PATCH …/receivers/{id}/staged` (activate_immediate) mit einem Sender —
//! die Aktion wird über [`ReceiverControl`] an den Daemon (RX-Abonnement)
//! durchgereicht.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use serde_json::{json, Value};

use crate::resources::NmosNode;

/// Steuer-Naht: der Daemon implementiert das eigentliche Abonnieren/Trennen.
pub trait ReceiverControl: Send + Sync {
    /// Verbindet den Receiver mit dem durch `sdp` beschriebenen Sender.
    fn connect(&self, sdp: &str) -> Result<(), String>;
    /// Trennt den Receiver (unsubscribe).
    fn disconnect(&self) -> Result<(), String>;
    /// Aktuell aktive SDP (falls verbunden).
    fn active_sdp(&self) -> Option<String>;
    /// Ob der Receiver gerade verbunden ist.
    fn connected(&self) -> bool;
}

type Node = Arc<NmosNode>;
type Ctl = Arc<dyn ReceiverControl>;

fn not_found() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "code": 404, "error": "Not found" })),
    )
        .into_response()
}

/// IS-05 receiver connection-Objekt (staged/active).
fn connection(ctl: &Ctl) -> Value {
    json!({
        "master_enable": ctl.connected(),
        "sender_id": null,
        "activation": { "mode": null, "requested_time": null, "activation_time": null },
        "transport_type": "urn:x-nmos:transport:rtp.mcast",
        "transport_file": {
            "data": ctl.active_sdp(),
            "type": ctl.active_sdp().map(|_| "application/sdp"),
        },
        "transport_params": [{ "rtp_enabled": ctl.connected() }]
    })
}

pub async fn list(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([format!("{}/", n.receiver_id)]))
}

pub async fn root(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    if id != n.receiver_id {
        return not_found();
    }
    Json(json!(["constraints/", "staged/", "active/"])).into_response()
}

pub async fn constraints(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    if id != n.receiver_id {
        return not_found();
    }
    Json(json!([{}])).into_response()
}

pub async fn staged_get(
    State(n): State<Node>,
    Extension(ctl): Extension<Ctl>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if id != n.receiver_id {
        return not_found();
    }
    Json(connection(&ctl)).into_response()
}

pub async fn active_get(
    State(n): State<Node>,
    Extension(ctl): Extension<Ctl>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if id != n.receiver_id {
        return not_found();
    }
    Json(connection(&ctl)).into_response()
}

/// **Der Koppelpunkt:** `PATCH staged` mit `transport_file.data` = Sender-SDP.
/// master_enable=true → verbinden, false → trennen.
pub async fn staged_patch(
    State(n): State<Node>,
    Extension(ctl): Extension<Ctl>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if id != n.receiver_id {
        return Err((StatusCode::NOT_FOUND, "unbekannter Receiver".into()));
    }
    let master_enable = body["master_enable"].as_bool().unwrap_or(true);
    let sdp = body["transport_file"]["data"].as_str();
    if master_enable {
        match sdp {
            Some(sdp) if !sdp.is_empty() => ctl
                .connect(sdp)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?,
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "transport_file.data (SDP) fehlt".into(),
                ))
            }
        }
    } else {
        ctl.disconnect()
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    }
    Ok(Json(connection(&ctl)))
}
