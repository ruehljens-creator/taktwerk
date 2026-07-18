//! Gemeinsamer Zustand des Daemons (thread-safe, klonbar für Axum-Handler).

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use taktwerk_core::clock::{SystemTimeSource, TimeSource};
use taktwerk_core::StreamProfile;
use taktwerk_net::PtpSlaveStatus;
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
}

/// Der von allen Handlern geteilte App-Zustand.
#[derive(Clone)]
pub struct AppState {
    pub node: Arc<NodeInfo>,
    pub discovered: Arc<Mutex<HashMap<u16, DiscoveredEntry>>>,
    pub tx: Arc<Mutex<TxControl>>,
    pub rx: Arc<Mutex<RxControl>>,
    /// Geräte- und Traffic-Monitor (SAP/PTP/RTP-Aggregation).
    pub monitor: Arc<Mutex<TrafficMonitor>>,
    /// Media-Clock für RTP-Timestamps — SystemTimeSource oder (bei PTP-Slave)
    /// eine PtpTimeSource, die an den Grandmaster gelockt ist.
    pub clock: Arc<dyn TimeSource>,
    /// Live-Status des PTP-Slaves (leer, wenn Slave nicht aktiv).
    pub ptp: Arc<Mutex<PtpSlaveStatus>>,
}

impl AppState {
    pub fn new(node: NodeInfo) -> Self {
        Self {
            node: Arc::new(node),
            discovered: Arc::new(Mutex::new(HashMap::new())),
            tx: Arc::new(Mutex::new(TxControl::default())),
            rx: Arc::new(Mutex::new(RxControl::default())),
            monitor: Arc::new(Mutex::new(TrafficMonitor::default())),
            clock: Arc::new(SystemTimeSource),
            ptp: Arc::new(Mutex::new(PtpSlaveStatus::default())),
        }
    }
}

/// Aktuelle Unix-Zeit in Sekunden (für `last_seen`).
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
