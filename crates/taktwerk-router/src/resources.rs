//! NMOS-Ressourcenmodell (IS-04) für einen Taktwerk-Knoten.
//!
//! Bildet den Knoten auf die NMOS-Ressourcen ab: **node → device → source →
//! flow → sender** (der AES67-Sende-Stream) und einen **receiver**. Die IDs sind
//! deterministisch aus dem Node-Namen abgeleitet ([`crate::ids`]). Die JSON-
//! Bodies liefern die IS-04/IS-05-Handler in [`crate::nmos`].

use serde_json::{json, Value};
use taktwerk_core::sdp::{AudioSession, PtpRefClock};
use taktwerk_core::StreamProfile;

use crate::ids::uuid_from;

/// Statischer Snapshot eines Taktwerk-Knotens als NMOS-Ressourcenquelle.
#[derive(Debug, Clone)]
pub struct NmosNode {
    pub node_id: String,
    pub device_id: String,
    pub source_id: String,
    pub flow_id: String,
    pub sender_id: String,
    pub receiver_id: String,
    pub label: String,
    /// Host-IP, unter der die APIs erreichbar sind.
    pub host: String,
    /// Port der HTTP-APIs (Node + Connection).
    pub api_port: u16,
    /// Interface-Name für `interface_bindings`.
    pub interface: String,
    pub profile: StreamProfile,
    pub group: String,
    pub port: u16,
    /// PTP-Clock-Referenz des Knotens (RFC 7273): Master = eigene Identity,
    /// Slave = Grandmaster (falls beim Start bekannt), sonst None.
    pub refclk: Option<PtpRefClock>,
}

impl NmosNode {
    /// Baut den Snapshot aus der Knoten-Konfiguration (IDs deterministisch).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        label: impl Into<String>,
        host: impl Into<String>,
        api_port: u16,
        interface: impl Into<String>,
        profile: StreamProfile,
        group: impl Into<String>,
        port: u16,
        refclk: Option<PtpRefClock>,
    ) -> Self {
        let label = label.into();
        Self {
            node_id: uuid_from(&format!("{label}:node")),
            device_id: uuid_from(&format!("{label}:device")),
            source_id: uuid_from(&format!("{label}:source")),
            flow_id: uuid_from(&format!("{label}:flow")),
            sender_id: uuid_from(&format!("{label}:sender")),
            receiver_id: uuid_from(&format!("{label}:receiver")),
            host: host.into(),
            api_port,
            interface: interface.into(),
            profile,
            group: group.into(),
            port,
            refclk,
            label,
        }
    }

    fn base_href(&self) -> String {
        format!("http://{}:{}", self.host, self.api_port)
    }

    /// Die SDP-Transportdatei des Senders (IS-05 `transportfile`).
    pub fn transport_sdp(&self) -> String {
        AudioSession {
            session_name: self.label.clone(),
            origin_unicast: self.host.clone(),
            multicast_addr: self.group.clone(),
            port: self.port,
            payload_type: 97,
            profile: self.profile,
            refclk: self.refclk.clone(),
            mediaclk_offset: 0,
        }
        .to_sdp()
    }

    // ---- IS-04-Ressourcen ----

    pub fn node_self(&self) -> Value {
        json!({
            "id": self.node_id,
            "version": "0:0",
            "label": self.label,
            "description": "Taktwerk AES67 Node",
            "tags": {},
            "href": format!("{}/", self.base_href()),
            "caps": {},
            "api": {
                "versions": ["v1.3"],
                "endpoints": [{
                    "host": self.host,
                    "port": self.api_port,
                    "protocol": "http"
                }]
            },
            "services": [],
            "clocks": [{
                "name": "clk0",
                "ref_type": "ptp",
                "traceable": false,
                "version": "IEEE1588-2008",
                "gmid": self.refclk.as_ref().map(|r| r.gmid.to_lowercase()).unwrap_or_else(|| "00-00-00-ff-fe-00-00-00".into()),
                "locked": self.refclk.is_some()
            }],
            "interfaces": [{
                "name": self.interface,
                "chassis_id": null,
                "port_id": null
            }]
        })
    }

    pub fn device(&self) -> Value {
        json!({
            "id": self.device_id,
            "version": "0:0",
            "label": self.label,
            "description": "Taktwerk audio device",
            "tags": {},
            "type": "urn:x-nmos:device:audio",
            "node_id": self.node_id,
            "senders": [self.sender_id],
            "receivers": [self.receiver_id],
            "controls": [{
                "href": format!("{}/x-nmos/connection/v1.1/", self.base_href()),
                "type": "urn:x-nmos:control:sr-ctrl/v1.1"
            }]
        })
    }

    pub fn source(&self) -> Value {
        let ch: Vec<Value> = (0..self.profile.channels)
            .map(|i| json!({ "label": format!("Channel {}", i + 1) }))
            .collect();
        json!({
            "id": self.source_id,
            "version": "0:0",
            "label": self.label,
            "description": "Taktwerk audio source",
            "tags": {},
            "caps": {},
            "device_id": self.device_id,
            "parents": [],
            "clock_name": "clk0",
            "format": "urn:x-nmos:format:audio",
            "channels": ch
        })
    }

    pub fn flow(&self) -> Value {
        json!({
            "id": self.flow_id,
            "version": "0:0",
            "label": self.label,
            "description": "Taktwerk audio flow",
            "tags": {},
            "source_id": self.source_id,
            "device_id": self.device_id,
            "parents": [],
            "format": "urn:x-nmos:format:audio",
            "media_type": "audio/L24",
            "sample_rate": { "numerator": self.profile.sample_rate, "denominator": 1 },
            "bit_depth": 24
        })
    }

    pub fn sender(&self) -> Value {
        json!({
            "id": self.sender_id,
            "version": "0:0",
            "label": self.label,
            "description": "Taktwerk AES67 sender",
            "tags": {},
            "flow_id": self.flow_id,
            "transport": "urn:x-nmos:transport:rtp.mcast",
            "device_id": self.device_id,
            "manifest_href": format!("{}/x-nmos/connection/v1.1/single/senders/{}/transportfile/", self.base_href(), self.sender_id),
            "interface_bindings": [self.interface],
            "subscription": { "receiver_id": null, "active": true }
        })
    }

    pub fn receiver(&self) -> Value {
        json!({
            "id": self.receiver_id,
            "version": "0:0",
            "label": self.label,
            "description": "Taktwerk AES67 receiver",
            "tags": {},
            "device_id": self.device_id,
            "transport": "urn:x-nmos:transport:rtp.mcast",
            "interface_bindings": [self.interface],
            "subscription": { "sender_id": null, "active": false },
            "format": "urn:x-nmos:format:audio",
            "caps": { "media_types": ["audio/L24"] }
        })
    }

    // ---- IS-05-Objekte ----

    /// Aktive/gestagete Transport-Parameter des Senders (RTP-Multicast).
    pub fn sender_transport_params(&self) -> Value {
        json!([{
            "destination_ip": self.group,
            "destination_port": self.port,
            "source_ip": self.host,
            "source_port": self.port,
            "rtp_enabled": true
        }])
    }

    pub fn sender_active(&self) -> Value {
        json!({
            "master_enable": true,
            "activation": { "mode": null, "requested_time": null, "activation_time": null },
            "transport_type": "urn:x-nmos:transport:rtp.mcast",
            "transport_params": self.sender_transport_params()
        })
    }
}
