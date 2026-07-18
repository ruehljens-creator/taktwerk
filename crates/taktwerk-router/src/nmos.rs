//! NMOS-HTTP-APIs (Axum): **IS-04 Node-API** (Ressourcen lesen) und **IS-05
//! Connection-API** (Transportdatei + gestagete/aktive Parameter).
//!
//! Read-fokussiert: Ein Controller kann den Knoten und seinen Sender/Receiver
//! entdecken und die SDP-Transportdatei abrufen. Die App wird eigenständig
//! ausgeliefert ([`app`]) und lässt sich neben der Daemon-REST-API betreiben.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde_json::json;

use crate::receiver::{self, ReceiverControl};
use crate::resources::NmosNode;

type Node = Arc<NmosNode>;

/// Baut die fertige NMOS-App (Node- + Connection-API + Discovery-Wurzeln).
/// `control` ist die steuerbare Senke (unser Receiver) für IS-05-`PATCH`.
pub fn app(node: Node, control: Arc<dyn ReceiverControl>) -> Router {
    let r = "/x-nmos/connection/v1.1/single/receivers";
    Router::new()
        // Discovery-Wurzeln
        .route(
            "/x-nmos",
            get(|| async { Json(json!(["node/", "connection/"])) }),
        )
        .route(
            "/x-nmos/",
            get(|| async { Json(json!(["node/", "connection/"])) }),
        )
        .route("/x-nmos/node", get(|| async { Json(json!(["v1.3/"])) }))
        .route("/x-nmos/node/", get(|| async { Json(json!(["v1.3/"])) }))
        .route(
            "/x-nmos/connection",
            get(|| async { Json(json!(["v1.1/"])) }),
        )
        .route(
            "/x-nmos/connection/",
            get(|| async { Json(json!(["v1.1/"])) }),
        )
        // IS-04 Node-API v1.3
        .route("/x-nmos/node/v1.3", get(is04_root))
        .route("/x-nmos/node/v1.3/", get(is04_root))
        .route("/x-nmos/node/v1.3/self", get(is04_self))
        .route("/x-nmos/node/v1.3/self/", get(is04_self))
        .route("/x-nmos/node/v1.3/devices", get(is04_devices))
        .route("/x-nmos/node/v1.3/devices/", get(is04_devices))
        .route("/x-nmos/node/v1.3/devices/:id", get(is04_device_one))
        .route("/x-nmos/node/v1.3/sources", get(is04_sources))
        .route("/x-nmos/node/v1.3/sources/", get(is04_sources))
        .route("/x-nmos/node/v1.3/sources/:id", get(is04_source_one))
        .route("/x-nmos/node/v1.3/flows", get(is04_flows))
        .route("/x-nmos/node/v1.3/flows/", get(is04_flows))
        .route("/x-nmos/node/v1.3/flows/:id", get(is04_flow_one))
        .route("/x-nmos/node/v1.3/senders", get(is04_senders))
        .route("/x-nmos/node/v1.3/senders/", get(is04_senders))
        .route("/x-nmos/node/v1.3/senders/:id", get(is04_sender_one))
        .route("/x-nmos/node/v1.3/receivers", get(is04_receivers))
        .route("/x-nmos/node/v1.3/receivers/", get(is04_receivers))
        .route("/x-nmos/node/v1.3/receivers/:id", get(is04_receiver_one))
        // IS-05 Connection-API v1.1
        .route(
            "/x-nmos/connection/v1.1",
            get(|| async { Json(json!(["bulk/", "single/"])) }),
        )
        .route(
            "/x-nmos/connection/v1.1/",
            get(|| async { Json(json!(["bulk/", "single/"])) }),
        )
        .route("/x-nmos/connection/v1.1/single", get(is05_single_root))
        .route("/x-nmos/connection/v1.1/single/", get(is05_single_root))
        .route(
            "/x-nmos/connection/v1.1/single/senders",
            get(is05_senders_list),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/",
            get(is05_senders_list),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id",
            get(is05_sender_root),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/",
            get(is05_sender_root),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/constraints",
            get(is05_sender_constraints),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/constraints/",
            get(is05_sender_constraints),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/staged",
            get(is05_sender_staged),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/staged/",
            get(is05_sender_staged),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/active",
            get(is05_sender_active),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/active/",
            get(is05_sender_active),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/transportfile",
            get(is05_transportfile),
        )
        .route(
            "/x-nmos/connection/v1.1/single/senders/:id/transportfile/",
            get(is05_transportfile),
        )
        // IS-05 Receiver (steuerbare Senke) — der Koppelpunkt sitzt auf `staged`.
        .route(r, get(receiver::list))
        .route(&format!("{r}/"), get(receiver::list))
        .route(&format!("{r}/:id"), get(receiver::root))
        .route(&format!("{r}/:id/"), get(receiver::root))
        .route(&format!("{r}/:id/constraints"), get(receiver::constraints))
        .route(&format!("{r}/:id/constraints/"), get(receiver::constraints))
        .route(
            &format!("{r}/:id/staged"),
            get(receiver::staged_get).patch(receiver::staged_patch),
        )
        .route(
            &format!("{r}/:id/staged/"),
            get(receiver::staged_get).patch(receiver::staged_patch),
        )
        .route(&format!("{r}/:id/active"), get(receiver::active_get))
        .route(&format!("{r}/:id/active/"), get(receiver::active_get))
        .with_state(node)
        .layer(Extension(control))
}

// ---- IS-04 ----

async fn is04_root() -> impl IntoResponse {
    Json(json!([
        "self/",
        "sources/",
        "flows/",
        "devices/",
        "senders/",
        "receivers/"
    ]))
}
async fn is04_self(State(n): State<Node>) -> impl IntoResponse {
    Json(n.node_self())
}
async fn is04_devices(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([n.device()]))
}
async fn is04_sources(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([n.source()]))
}
async fn is04_flows(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([n.flow()]))
}
async fn is04_senders(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([n.sender()]))
}
async fn is04_receivers(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([n.receiver()]))
}

async fn is04_device_one(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    one(&id, &n.device_id, n.device())
}
async fn is04_source_one(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    one(&id, &n.source_id, n.source())
}
async fn is04_flow_one(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    one(&id, &n.flow_id, n.flow())
}
async fn is04_sender_one(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    one(&id, &n.sender_id, n.sender())
}
async fn is04_receiver_one(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    one(&id, &n.receiver_id, n.receiver())
}

// ---- IS-05 ----

async fn is05_single_root() -> impl IntoResponse {
    Json(json!(["senders/", "receivers/"]))
}
async fn is05_senders_list(State(n): State<Node>) -> impl IntoResponse {
    Json(json!([format!("{}/", n.sender_id)]))
}
async fn is05_sender_root(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    if id != n.sender_id {
        return not_found();
    }
    Json(json!([
        "constraints/",
        "staged/",
        "active/",
        "transportfile/"
    ]))
    .into_response()
}
async fn is05_sender_constraints(
    State(n): State<Node>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if id != n.sender_id {
        return not_found();
    }
    // Ein Transport-Leg, keine einschränkenden Constraints.
    Json(json!([{}])).into_response()
}
async fn is05_sender_staged(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    if id != n.sender_id {
        return not_found();
    }
    Json(n.sender_active()).into_response()
}
async fn is05_sender_active(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    if id != n.sender_id {
        return not_found();
    }
    Json(n.sender_active()).into_response()
}
async fn is05_transportfile(State(n): State<Node>, Path(id): Path<String>) -> impl IntoResponse {
    if id != n.sender_id {
        return not_found();
    }
    tracing::debug!(sender = %id, "NMOS transportfile (SDP) abgerufen");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/sdp")],
        n.transport_sdp(),
    )
        .into_response()
}

// ---- Helfer ----

fn one(id: &str, expected: &str, body: serde_json::Value) -> axum::response::Response {
    if id == expected {
        Json(body).into_response()
    } else {
        not_found()
    }
}

fn not_found() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "code": 404, "error": "Not found" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use taktwerk_core::StreamProfile;

    fn node() -> Node {
        Arc::new(NmosNode::new(
            "testnode",
            "10.0.0.5",
            7789,
            "eth0",
            StreamProfile::level_a(2),
            "239.69.83.67",
            5004,
        ))
    }

    #[test]
    fn transport_sdp_is_valid_aes67() {
        let n = node();
        let sdp = n.transport_sdp();
        assert!(sdp.contains("m=audio 5004 RTP/AVP 97"));
        assert!(sdp.contains("a=rtpmap:97 L24/48000/2"));
    }

    #[test]
    fn sender_references_flow_and_transportfile() {
        let n = node();
        let s = n.sender();
        assert_eq!(s["flow_id"], serde_json::json!(n.flow_id));
        assert!(s["manifest_href"]
            .as_str()
            .unwrap()
            .ends_with("/transportfile/"));
        assert_eq!(s["transport"], "urn:x-nmos:transport:rtp.mcast");
    }

    #[test]
    fn device_links_sender_and_receiver() {
        let n = node();
        let d = n.device();
        assert_eq!(d["senders"], serde_json::json!([n.sender_id]));
        assert_eq!(d["receivers"], serde_json::json!([n.receiver_id]));
    }
}
