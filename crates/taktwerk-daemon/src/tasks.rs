//! Hintergrund-Tasks: SAP-Discovery (Netz → Zustand) und der TX-Streaming-Loop.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use taktwerk_audio::NullBackend;
use taktwerk_core::clock::SystemTimeSource;
use taktwerk_core::sdp::{AudioSession, PtpRefClock};
use taktwerk_core::StreamProfile;
use taktwerk_endpoint::TxStream;
use taktwerk_net::{
    bind_sap_announcer, bind_sap_listener, bind_sender, MulticastConfig, SapAnnouncer, SapListener,
};
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};

use crate::state::{now_unix, AppState, DiscoveredEntry};

/// Lauscht dauerhaft auf SAP-Announcements und pflegt die Discovery-Tabelle.
pub async fn discovery_task(iface: Ipv4Addr, state: AppState) {
    let sock = match bind_sap_listener(iface) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[discovery] SAP-Listener-Bind fehlgeschlagen: {e}");
            return;
        }
    };
    let mut listener = SapListener::new(sock);
    println!("[discovery] SAP-Listener aktiv auf Interface {iface}");
    loop {
        match listener.recv().await {
            Ok(ev) => {
                let mut map = state.discovered.lock().unwrap();
                if ev.announce {
                    if let Some(s) = ev.session {
                        map.insert(
                            ev.msg_id_hash,
                            DiscoveredEntry {
                                session_name: s.session_name,
                                multicast_addr: s.multicast_addr,
                                port: s.port,
                                channels: s.profile.channels,
                                sample_rate: s.profile.sample_rate,
                                source: Ipv4Addr::from(ev.source),
                                last_seen: now_unix(),
                            },
                        );
                    }
                } else {
                    map.remove(&ev.msg_id_hash);
                }
            }
            Err(e) => eprintln!("[discovery] recv-Fehler: {e}"),
        }
    }
}

/// Parameter für den TX-Loop.
pub struct TxParams {
    pub iface: Ipv4Addr,
    pub group: Ipv4Addr,
    pub port: u16,
    pub profile: StreamProfile,
    pub payload_type: u8,
    pub ssrc: u32,
    pub node_name: String,
}

/// Startet den TX-Loop und gibt (Shutdown-Sender, Paketzähler, JoinHandle) zurück.
/// Bindet die Sockets sofort (Fehler landen beim Aufrufer), der Loop läuft dann
/// im Hintergrund bis zum Shutdown-Signal.
pub fn start_tx(
    params: TxParams,
) -> std::io::Result<(
    watch::Sender<bool>,
    Arc<AtomicU64>,
    tokio::task::JoinHandle<()>,
)> {
    let TxParams {
        iface,
        group,
        port,
        profile,
        payload_type,
        ssrc,
        node_name,
    } = params;

    let mcfg = MulticastConfig::new(group, port).with_interface(iface);
    let dest: SocketAddr = mcfg.dest();

    // Media-Sende-Socket + SAP-Announce-Socket sofort binden (Fehler → Aufrufer).
    let media_sock = bind_sender(&mcfg, true)?;
    let sap_sock = bind_sap_announcer(iface, true)?;

    let session = AudioSession {
        session_name: format!("{node_name} · {group}:{port}"),
        origin_unicast: iface.to_string(),
        multicast_addr: group.to_string(),
        port,
        payload_type,
        profile,
        refclk: Some(PtpRefClock {
            // Platzhalter-GMID bis PTP steht (Phase 1); Struktur bleibt gleich.
            gmid: "00-00-00-FF-FE-00-00-00".into(),
            domain: 0,
        }),
        mediaclk_offset: 0,
    };
    let announcer = SapAnnouncer::new(sap_sock, iface, &session);

    let clock = SystemTimeSource;
    let mut tx = TxStream::new(
        Box::new(NullBackend::new(profile)),
        media_sock,
        dest,
        profile,
        payload_type,
        ssrc,
        &clock,
    );

    let packets = Arc::new(AtomicU64::new(0));
    let packets_task = packets.clone();
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    let handle = tokio::spawn(async move {
        let mut media_tick = interval(Duration::from_micros(profile.ptime_us as u64));
        media_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // AES67 empfiehlt periodische SAP-Announcements (~alle paar Sekunden).
        let mut sap_tick = interval(Duration::from_secs(5));

        // Erstes Announcement sofort.
        if let Err(e) = announcer.announce().await {
            eprintln!("[tx] SAP-Announce-Fehler: {e}");
        }

        loop {
            tokio::select! {
                _ = media_tick.tick() => {
                    if let Err(e) = tx.pump_once().await {
                        eprintln!("[tx] Sende-Fehler: {e}");
                        break;
                    }
                    packets_task.store(tx.packets_sent(), Ordering::Relaxed);
                }
                _ = sap_tick.tick() => {
                    if let Err(e) = announcer.announce().await {
                        eprintln!("[tx] SAP-Announce-Fehler: {e}");
                    }
                }
                res = shutdown_rx.changed() => {
                    if res.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
            }
        }
        // Beim Stoppen die Session zurückziehen.
        let _ = announcer.delete().await;
        println!("[tx] gestoppt nach {} Paketen", tx.packets_sent());
    });

    Ok((shutdown_tx, packets, handle))
}
