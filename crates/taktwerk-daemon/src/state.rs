//! Gemeinsamer Zustand des Daemons (thread-safe, klonbar für Axum-Handler).

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use taktwerk_core::clock::{SystemTimeSource, TimeSource};
use taktwerk_core::sdp::PtpRefClock;
use taktwerk_core::StreamProfile;
use taktwerk_net::{PtpMasterStatus, PtpSlaveStatus};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::monitor::TrafficMonitor;

/// Statische Knoten-Konfiguration (Anzeigename + Netz-Interface).
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub name: String,
    pub interface: Ipv4Addr,
    pub profile: StreamProfile,
    /// Ob der PTP-Slave (Lock an Grandmaster) aktiviert ist.
    pub ptp_slave: bool,
    /// Ob der PTP-Master/Grandmaster-Modus aktiviert ist.
    pub ptp_master: bool,
    /// PTP-Domain des Knotens (ST 2059/Broadcast üblich: 127; AES67-Default 0).
    pub ptp_domain: u8,
    /// NMOS-Host (für die Kreuzschiene, um den eigenen Receiver zu adressieren).
    pub nmos_host: String,
    /// NMOS-Port.
    pub nmos_port: u16,
}

/// Ein per NMOS-mDNS (`_nmos-node._tcp`) entdeckter fremder Node und seine
/// steuerbaren Receiver (für die Kreuzschiene).
#[derive(Debug, Clone)]
pub struct NmosPeer {
    /// Host der NMOS-API (Node + Connection).
    pub host: String,
    /// Port der NMOS-API.
    pub port: u16,
    /// Receiver dieses Nodes: (receiver_id, label).
    pub receivers: Vec<(String, String)>,
    pub last_seen: u64,
}

/// Ein entdeckter fremder Stream (per SAP oder RAVENNA/mDNS).
#[derive(Debug, Clone)]
pub struct DiscoveredEntry {
    pub session_name: String,
    pub multicast_addr: String,
    pub port: u16,
    pub channels: u8,
    pub sample_rate: u32,
    pub source: Ipv4Addr,
    /// Entdeckungsweg: "SAP" oder "RAVENNA".
    pub via: &'static str,
    /// Unix-Sekunden des letzten Announcements.
    pub last_seen: u64,
}

/// Fester Schlüssel des per NMOS-Kreuzschiene gesteuerten Receivers (ein logischer
/// Sink, der die Quelle wechselt). REST-Abos nutzen dagegen "group:port".
pub const NMOS_RX_ID: &str = "nmos-receiver";

/// Steuerzustand des Sende-Stroms (TX).
#[derive(Default)]
pub struct TxControl {
    pub running: bool,
    pub dest: Option<String>,
    pub channels: u8,
    pub packets: Arc<AtomicU64>,
    pub shutdown: Option<watch::Sender<bool>>,
    pub handle: Option<JoinHandle<()>>,
}

/// Steuerzustand des Empfangs-Stroms (RX-Abonnement).
#[derive(Default)]
pub struct RxControl {
    pub running: bool,
    pub source: Option<String>,
    pub channels: u8,
    pub packets: Arc<AtomicU64>,
    pub shutdown: Option<watch::Sender<bool>>,
    pub handle: Option<JoinHandle<()>>,
    /// SDP, mit der das aktuelle Abonnement gesetzt wurde (via IS-05/Kreuzschiene).
    pub active_sdp: Option<String>,
}

/// Der von allen Handlern geteilte App-Zustand.
#[derive(Clone)]
pub struct AppState {
    pub node: Arc<NodeInfo>,
    pub discovered: Arc<Mutex<HashMap<u16, DiscoveredEntry>>>,
    /// Laufende Sende-Ströme (Multi-Stream), Schlüssel = "group:port".
    pub tx: Arc<Mutex<HashMap<String, TxControl>>>,
    /// Laufende Empfangs-Abos (Multi-Stream), Schlüssel = "group:port" bzw. der
    /// feste Schlüssel [`NMOS_RX_ID`] für den per Kreuzschiene gesteuerten Receiver.
    pub rx: Arc<Mutex<HashMap<String, RxControl>>>,
    /// Geräte- und Traffic-Monitor (SAP/PTP/RTP-Aggregation).
    pub monitor: Arc<Mutex<TrafficMonitor>>,
    /// Media-Clock für RTP-Timestamps — SystemTimeSource oder (bei PTP-Slave)
    /// eine PtpTimeSource, die an den Grandmaster gelockt ist.
    pub clock: Arc<dyn TimeSource>,
    /// Live-Status des PTP-Slaves (leer, wenn Slave nicht aktiv).
    pub ptp: Arc<Mutex<PtpSlaveStatus>>,
    /// Live-Status des PTP-Masters (leer, wenn Master nicht aktiv).
    pub ptp_master: Arc<Mutex<PtpMasterStatus>>,
    /// GNSS-Status (via gpsd; leer/`connected=false` ohne Hardware).
    pub gnss: crate::clockmon::GnssHandle,
    /// Geschätzte Uhr-Drift gegen die PTP-Referenz (Clock-Panel).
    pub drift: crate::clockmon::DriftHandle,
    /// Gewählte Referenzquelle fürs Clock-Panel ("auto"/"ptp"/"gnss"/"system").
    pub clock_ref: Arc<Mutex<String>>,
    /// Per NMOS-mDNS entdeckte fremde Nodes (Instanz → Peer), für die Matrix.
    pub nmos_peers: Arc<Mutex<HashMap<String, NmosPeer>>>,
}

impl AppState {
    /// RFC-7273-Clock-Referenz für ausgehende SDP je nach PTP-Rolle:
    /// **Master** = eigene Clock-Identity (wir sind der GM), **Slave** = die
    /// gelernte Grandmaster-Identity (falls schon gesehen), sonst `None`
    /// (kein PTP → kein `a=ts-refclk`).
    pub fn ptp_refclk(&self) -> Option<PtpRefClock> {
        let id = if self.node.ptp_master {
            Some(crate::clock_identity_from(&self.node.name))
        } else if self.node.ptp_slave {
            self.ptp.lock().unwrap().grandmaster
        } else {
            None
        }?;
        Some(PtpRefClock {
            gmid: fmt_eui64(id),
            domain: self.node.ptp_domain,
        })
    }

    pub fn new(node: NodeInfo) -> Self {
        Self {
            node: Arc::new(node),
            discovered: Arc::new(Mutex::new(HashMap::new())),
            tx: Arc::new(Mutex::new(HashMap::new())),
            rx: Arc::new(Mutex::new(HashMap::new())),
            monitor: Arc::new(Mutex::new(TrafficMonitor::default())),
            clock: Arc::new(SystemTimeSource),
            ptp: Arc::new(Mutex::new(PtpSlaveStatus::default())),
            ptp_master: Arc::new(Mutex::new(PtpMasterStatus::default())),
            gnss: Arc::new(Mutex::new(Default::default())),
            drift: Arc::new(Mutex::new(Default::default())),
            clock_ref: Arc::new(Mutex::new("auto".into())),
            nmos_peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// Formatiert eine EUI-64-Clock-Identity als `XX-XX-…-XX` (RFC-7273-gmid).
fn fmt_eui64(id: [u8; 8]) -> String {
    id.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join("-")
}

/// Aktuelle Unix-Zeit in Sekunden (für `last_seen`).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
