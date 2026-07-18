//! Hintergrund-Tasks: SAP-Discovery (Netz → Zustand) und der TX-Streaming-Loop.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// true, wenn ein echtes Audiogerät genutzt werden soll (`TAKTWERK_AUDIO=cpal`).
fn audio_device() -> bool {
    std::env::var("TAKTWERK_AUDIO")
        .map(|v| v.eq_ignore_ascii_case("cpal"))
        .unwrap_or(false)
}

/// Optionaler Gerätename aus einer Env-Var (leer/ungesetzt → None = Default-Gerät).
fn audio_name(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|s| !s.trim().is_empty())
}
use taktwerk_core::sdp::{AudioSession, PtpRefClock};
use taktwerk_core::StreamProfile;
use taktwerk_endpoint::{RxStream, TxStream};
use taktwerk_net::{
    bind_receiver, bind_sap_announcer, bind_sap_listener, bind_sender, MulticastConfig,
    PtpListener, RtpReceiver, SapAnnouncer, SapListener,
};
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::monitor::{Proto, TrafficMonitor};
use crate::state::{now_unix, AppState, DiscoveredEntry};

/// IPv4 aus einer Socket-Adresse (nur IPv4-Traffic wird verbucht).
fn ipv4(addr: SocketAddr) -> Option<Ipv4Addr> {
    match addr {
        SocketAddr::V4(v4) => Some(*v4.ip()),
        SocketAddr::V6(_) => None,
    }
}

/// Formatiert eine PTP-Clock-Identity als lesbaren Gerätenamen.
fn ptp_name(id: [u8; 8]) -> String {
    let hex: Vec<String> = id.iter().map(|b| format!("{b:02x}")).collect();
    format!("PTP {}", hex.join(":"))
}

/// Lauscht dauerhaft auf SAP-Announcements und pflegt die Discovery-Tabelle.
pub async fn discovery_task(iface: Ipv4Addr, state: AppState) {
    let sock = match bind_sap_listener(iface) {
        Ok(s) => s,
        Err(e) => {
            error!(%iface, "SAP-Listener-Bind fehlgeschlagen: {e}");
            return;
        }
    };
    let mut listener = SapListener::new(sock);
    info!(%iface, "SAP-Discovery aktiv");
    loop {
        match listener.recv().await {
            Ok(ev) => {
                // Traffic verbuchen (SAP), Name aus der Session, wenn vorhanden.
                if let Some(ip) = ipv4(ev.from) {
                    let name = ev.session.as_ref().map(|s| s.session_name.clone());
                    state
                        .monitor
                        .lock()
                        .unwrap()
                        .record(Proto::Sap, ip, ev.bytes, name);
                }
                let mut map = state.discovered.lock().unwrap();
                if ev.announce {
                    if let Some(s) = ev.session {
                        debug!(
                            hash = ev.msg_id_hash,
                            name = %s.session_name,
                            group = %s.multicast_addr,
                            port = s.port,
                            "SAP-Announce entdeckt"
                        );
                        map.insert(
                            ev.msg_id_hash,
                            DiscoveredEntry {
                                session_name: s.session_name,
                                multicast_addr: s.multicast_addr,
                                port: s.port,
                                channels: s.profile.channels,
                                sample_rate: s.profile.sample_rate,
                                source: Ipv4Addr::from(ev.source),
                                via: "SAP",
                                last_seen: now_unix(),
                            },
                        );
                    }
                } else {
                    debug!(hash = ev.msg_id_hash, "SAP-Deletion");
                    map.remove(&ev.msg_id_hash);
                }
            }
            Err(e) => warn!("SAP-recv-Fehler: {e}"),
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
    /// Media-Clock für den RTP-Start-Timestamp (System- oder PTP-gelockt).
    pub clock: Arc<dyn taktwerk_core::clock::TimeSource>,
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
        clock,
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

    // Media-Clock aus dem State (System oder PTP-gelockt) für den Start-Timestamp.
    // Capture aus echtem Gerät, wenn TAKTWERK_AUDIO=cpal (sonst Stille/headless).
    let mut tx = TxStream::new(
        taktwerk_audio::open_with(
            profile,
            true,
            false,
            audio_device(),
            taktwerk_audio::DeviceSelection {
                capture: audio_name("TAKTWERK_AUDIO_IN"),
                playback: None,
            },
        ),
        media_sock,
        dest,
        profile,
        payload_type,
        ssrc,
        clock.as_ref(),
    );

    let packets = Arc::new(AtomicU64::new(0));
    let packets_task = packets.clone();
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    info!(%dest, ch = profile.channels, ssrc = format!("{ssrc:#x}"), "TX-Stream gestartet");
    let handle = tokio::spawn(async move {
        let mut media_tick = interval(Duration::from_micros(profile.ptime_us as u64));
        media_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // AES67 empfiehlt periodische SAP-Announcements (~alle paar Sekunden).
        let mut sap_tick = interval(Duration::from_secs(5));

        // Erstes Announcement sofort.
        if let Err(e) = announcer.announce().await {
            warn!("SAP-Announce-Fehler: {e}");
        } else {
            debug!(%dest, "SAP-Announce gesendet");
        }

        // Transiente Sendefehler tolerieren: Ein einzelnes `send_to` kann auf
        // manchen OS kurz fehlschlagen (z. B. macOS `No route to host`, während
        // die interface-gebundene Multicast-Route neu geklont wird). Ein AES67-
        // Sender darf den Stream deshalb nicht abbrechen — Paket verwerfen,
        // weiterlaufen. Erst bei *anhaltendem* Fehler (~1 s ununterbrochen) geben
        // wir auf (Kabel gezogen / Interface weg).
        let mut consecutive_err: u32 = 0;
        const MAX_CONSECUTIVE_ERR: u32 = 1000;
        loop {
            tokio::select! {
                _ = media_tick.tick() => {
                    match tx.pump_once().await {
                        Ok(()) => {
                            consecutive_err = 0;
                            let n = tx.packets_sent();
                            packets_task.store(n, Ordering::Relaxed);
                            if n % 1000 == 0 {
                                debug!(packets = n, "TX läuft");
                            }
                        }
                        Err(e) => {
                            consecutive_err += 1;
                            if consecutive_err == 1 || consecutive_err % 250 == 0 {
                                warn!(consecutive_err, "TX-Sende-Fehler (transient, übersprungen): {e}");
                            }
                            if consecutive_err >= MAX_CONSECUTIVE_ERR {
                                error!(consecutive_err, "TX-Sende-Fehler anhaltend — Stream beendet: {e}");
                                break;
                            }
                        }
                    }
                }
                _ = sap_tick.tick() => {
                    match announcer.announce().await {
                        Ok(()) => debug!("SAP-Announce (periodisch)"),
                        Err(e) => warn!("SAP-Announce-Fehler: {e}"),
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
        info!(packets = tx.packets_sent(), "TX-Stream gestoppt");
    });

    Ok((shutdown_tx, packets, handle))
}

/// Startet ein RX-Abonnement: tritt der Multicast-Gruppe bei und empfängt den
/// Stream in ein (headless) Backend. Empfangene Pakete werden zusätzlich als
/// RTP-Traffic im Monitor verbucht. Gibt (Shutdown, Live-Zähler, Handle) zurück.
pub fn start_rx(
    iface: Ipv4Addr,
    group: Ipv4Addr,
    port: u16,
    profile: StreamProfile,
    monitor: Arc<std::sync::Mutex<TrafficMonitor>>,
) -> std::io::Result<(
    watch::Sender<bool>,
    Arc<AtomicU64>,
    tokio::task::JoinHandle<()>,
)> {
    let mcfg = MulticastConfig::new(group, port).with_interface(iface);
    let sock = bind_receiver(&mcfg)?;
    let receiver = RtpReceiver::new(sock, profile);

    // Traffic-Kanal: RxStream meldet (Quelle, Bytes) je Paket; ein Drain-Task
    // verbucht sie in den Monitor. Endet automatisch, wenn RxStream droppt.
    let (traffic_tx, mut traffic_rx) = tokio::sync::mpsc::unbounded_channel();
    // Playback auf echtes Gerät, wenn TAKTWERK_AUDIO=cpal (sonst headless).
    let backend = taktwerk_audio::open_with(
        profile,
        false,
        true,
        audio_device(),
        taktwerk_audio::DeviceSelection {
            capture: None,
            playback: audio_name("TAKTWERK_AUDIO_OUT"),
        },
    );
    let rx = RxStream::new(receiver, backend).with_traffic(traffic_tx);
    let mon = monitor.clone();
    tokio::spawn(async move {
        while let Some((from, bytes)) = traffic_rx.recv().await {
            if let Some(ip) = ipv4(from) {
                mon.lock().unwrap().record(Proto::Rtp, ip, bytes, None);
            }
        }
    });

    let packets = rx.packet_counter();
    let counter = packets.clone();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    info!(%group, port, ch = profile.channels, "RX-Abonnement gestartet");
    let handle = tokio::spawn(async move {
        if let Err(e) = rx.run(shutdown_rx).await {
            error!("RX-Empfangs-Fehler: {e}");
        }
        info!(
            packets = counter.load(Ordering::Relaxed),
            "RX-Abonnement beendet"
        );
    });
    Ok((shutdown_tx, packets, handle))
}

/// Lauscht dauerhaft auf PTP-Multicast und verbucht jede Nachricht als
/// PTP-Traffic (Gerätename aus der Clock-Identity des Absenders).
pub async fn ptp_monitor_task(iface: Ipv4Addr, state: AppState) {
    let mut listener = match PtpListener::bind(iface) {
        Ok(l) => l,
        Err(e) => {
            warn!(%iface, "PTP-Listener-Bind fehlgeschlagen: {e} (kein PTP-Monitor)");
            return;
        }
    };
    info!(%iface, "PTP-Monitor aktiv (224.0.1.129:319/320)");
    loop {
        match listener.recv().await {
            Ok((msg, from, bytes)) => {
                if let Some(ip) = ipv4(from) {
                    let name = ptp_name(msg.source_identity());
                    state
                        .monitor
                        .lock()
                        .unwrap()
                        .record(Proto::Ptp, ip, bytes, Some(name));
                }
            }
            Err(e) => warn!("PTP-recv-Fehler: {e}"),
        }
    }
}

/// Aktualisiert im Sekundentakt die Traffic-Raten (pps/bps) des Monitors.
pub async fn rate_task(state: AppState) {
    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        state.monitor.lock().unwrap().tick();
    }
}
