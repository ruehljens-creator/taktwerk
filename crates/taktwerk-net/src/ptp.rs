//! PTP-Netz-Client: empfängt IEEE-1588-Nachrichten (Multicast 224.0.1.129,
//! Ports 319/320) und liefert sie geparst ([`taktwerk_core::ptp::wire`]).
//!
//! PTP nutzt zwei Ports: **319** (Event: Sync, Delay_Req) und **320** (General:
//! Announce, Follow_Up, Delay_Resp). Der Listener joint beide und mischt sie.
//! Aus Announce entsteht ein BMCA-Datensatz; Sync/Follow_Up speisen den Servo
//! ([`taktwerk_core::ptp::servo`]). Das Timestamping ist hier Software (lokale
//! Empfangszeit); HW-Timestamping (Linux `SO_TIMESTAMPING`) ist eine spätere,
//! optionale Verfeinerung hinter derselben Struktur.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use taktwerk_core::ptp::slave::SlaveState;
use taktwerk_core::ptp::wire::{
    build_delay_req, Announce, DelayResp, MessageType, PortIdentity, PtpHeader, PtpTimestamp,
    TimestampedMsg,
};
use taktwerk_core::ptp::{BmcaOrder, ClockDataset, ClockIdentity};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::time::{interval, Duration, MissedTickBehavior};

use crate::multicast::{bind_receiver, bind_sender, MulticastConfig};

/// PTP-Multicast-Adresse (primär) und Ports.
pub const PTP_MULTICAST: &str = "224.0.1.129";
pub const PTP_EVENT_PORT: u16 = 319;
pub const PTP_GENERAL_PORT: u16 = 320;

fn ptp_config(interface: Ipv4Addr, port: u16) -> MulticastConfig {
    let group: Ipv4Addr = PTP_MULTICAST
        .parse()
        .expect("PTP_MULTICAST konstant gültig");
    MulticastConfig::new(group, port).with_interface(interface)
}

/// Bindet den Event-Socket (Port 319) und tritt der PTP-Gruppe bei.
pub fn bind_ptp_event(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    bind_receiver(&ptp_config(interface, PTP_EVENT_PORT))
}

/// Bindet den General-Socket (Port 320) und tritt der PTP-Gruppe bei.
pub fn bind_ptp_general(interface: Ipv4Addr) -> io::Result<UdpSocket> {
    bind_receiver(&ptp_config(interface, PTP_GENERAL_PORT))
}

/// Eine empfangene, klassifizierte PTP-Nachricht.
#[derive(Debug, Clone)]
pub enum PtpMessage {
    Announce(Announce),
    Sync(TimestampedMsg),
    FollowUp(TimestampedMsg),
    DelayResp(DelayResp),
    /// Anderer Typ (Management, Signaling, …) — nur der Header.
    Other(PtpHeader),
}

impl PtpMessage {
    /// Kurzname des Nachrichtentyps (für Logs).
    pub fn kind(&self) -> &'static str {
        match self {
            PtpMessage::Announce(_) => "Announce",
            PtpMessage::Sync(_) => "Sync",
            PtpMessage::FollowUp(_) => "Follow_Up",
            PtpMessage::DelayResp(_) => "Delay_Resp",
            PtpMessage::Other(_) => "Other",
        }
    }

    /// Clock-Identity des Absenders (aus dem Header).
    pub fn source_identity(&self) -> ClockIdentity {
        match self {
            PtpMessage::Announce(a) => a.header.source_port.clock_identity,
            PtpMessage::Sync(m) | PtpMessage::FollowUp(m) => m.header.source_port.clock_identity,
            PtpMessage::DelayResp(d) => d.header.source_port.clock_identity,
            PtpMessage::Other(h) => h.source_port.clock_identity,
        }
    }
}

/// Empfängt PTP-Nachrichten von beiden Ports (319 Event, 320 General).
pub struct PtpListener {
    event: UdpSocket,
    general: UdpSocket,
    buf_event: Vec<u8>,
    buf_general: Vec<u8>,
}

impl PtpListener {
    pub fn new(event: UdpSocket, general: UdpSocket) -> Self {
        Self {
            event,
            general,
            buf_event: vec![0u8; 1500],
            buf_general: vec![0u8; 1500],
        }
    }

    /// Bindet beide PTP-Sockets auf einem Interface und baut den Listener.
    pub fn bind(interface: Ipv4Addr) -> io::Result<Self> {
        Ok(Self::new(
            bind_ptp_event(interface)?,
            bind_ptp_general(interface)?,
        ))
    }

    /// Wartet auf die nächste (parsebare) PTP-Nachricht von einem der Ports.
    /// Gibt Nachricht, Absender und Datagramm-Größe (Bytes) zurück.
    pub async fn recv(&mut self) -> io::Result<(PtpMessage, SocketAddr, usize)> {
        loop {
            let (datagram, from) = tokio::select! {
                r = self.event.recv_from(&mut self.buf_event) => {
                    let (n, from) = r?;
                    (&self.buf_event[..n], from)
                }
                r = self.general.recv_from(&mut self.buf_general) => {
                    let (n, from) = r?;
                    (&self.buf_general[..n], from)
                }
            };
            let n = datagram.len();
            match classify(datagram) {
                Some(msg) => {
                    tracing::debug!(%from, kind = msg.kind(), bytes = n, "PTP-Nachricht empfangen");
                    return Ok((msg, from, n));
                }
                None => continue, // unparsebar / uninteressant → weiterhören
            }
        }
    }
}

/// Klassifiziert ein Datagramm anhand seines PTP-Headers.
fn classify(datagram: &[u8]) -> Option<PtpMessage> {
    let header = PtpHeader::parse(datagram).ok()?;
    match header.message_type {
        MessageType::Announce => Announce::parse(datagram).ok().map(PtpMessage::Announce),
        MessageType::Sync => TimestampedMsg::parse(datagram).ok().map(PtpMessage::Sync),
        MessageType::FollowUp => TimestampedMsg::parse(datagram)
            .ok()
            .map(PtpMessage::FollowUp),
        MessageType::DelayResp => DelayResp::parse(datagram).ok().map(PtpMessage::DelayResp),
        _ => Some(PtpMessage::Other(header)),
    }
}

/// TwoStep-Flag im PTP-Header (Sync mit two-step → Follow_Up folgt).
const FLAG_TWO_STEP: u16 = 0x0200;
/// ptpTimescale-Flag (Announce): unsere Zeit ist die PTP-Zeitskala, nicht ARB.
const FLAG_PTP_TIMESCALE: u16 = 0x0008;

/// Lokale Systemzeit in Nanosekunden (Software-Timestamp; gleiche Uhr wie
/// `SystemTimeSource`, damit Offsets zu `PtpTimeSource` zusammenpassen).
fn now_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

/// Live-Status des PTP-Slaves (für Daemon/UI).
#[derive(Debug, Clone, Default)]
pub struct PtpSlaveStatus {
    pub synced: bool,
    pub offset_ns: i64,
    pub path_delay_ns: i64,
    /// Grandmaster-Clock-Identity (aus Announce), falls gesehen.
    pub grandmaster: Option<ClockIdentity>,
}

/// PTP-**Slave**: lockt an den Grandmaster (Sync/Follow_Up + Delay_Req/Resp),
/// füttert den Servo und schreibt den Offset in `offset_handle` (→ `PtpTimeSource`).
pub struct PtpSlave {
    listener: PtpListener,
    send_sock: UdpSocket,
    dest: std::net::SocketAddr,
    our_identity: ClockIdentity,
    state: SlaveState,
    offset_handle: Arc<AtomicI64>,
    status: Arc<Mutex<PtpSlaveStatus>>,
    seq: u16,
}

impl PtpSlave {
    /// Baut den Slave auf einem Interface. `our_identity` = eigene Clock-Identity;
    /// `offset_handle` wird mit dem laufenden Offset (Slave−Master, ns) beschrieben.
    pub fn bind(
        iface: Ipv4Addr,
        our_identity: ClockIdentity,
        offset_handle: Arc<AtomicI64>,
        status: Arc<Mutex<PtpSlaveStatus>>,
    ) -> io::Result<Self> {
        let listener = PtpListener::bind(iface)?;
        let group: Ipv4Addr = PTP_MULTICAST.parse().unwrap();
        let send_sock = bind_sender(
            &MulticastConfig::new(group, PTP_EVENT_PORT).with_interface(iface),
            true,
        )?;
        Ok(Self {
            listener,
            send_sock,
            dest: std::net::SocketAddr::from((group, PTP_EVENT_PORT)),
            our_identity,
            state: SlaveState::new(0.1),
            offset_handle,
            status,
            seq: 0,
        })
    }

    fn publish(&self) {
        let off = self.state.offset_ns();
        self.offset_handle.store(off, Ordering::Relaxed);
        let mut st = self.status.lock().unwrap();
        st.synced = self.state.is_synced();
        st.offset_ns = off;
        st.path_delay_ns = self.state.path_delay_ns();
    }

    /// Läuft bis zum Shutdown: verarbeitet PTP-Nachrichten und sendet periodisch
    /// Delay_Req, um die Pfad-Verzögerung zu bestimmen.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        let mut delay_tick = interval(Duration::from_secs(1));
        delay_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                res = self.listener.recv() => {
                    let (msg, _from, _bytes) = res?;
                    match msg {
                        PtpMessage::Announce(a) => {
                            self.status.lock().unwrap().grandmaster = Some(a.gm_identity);
                        }
                        PtpMessage::Sync(m) => {
                            let t2 = now_nanos();
                            let two_step = (m.header.flags & FLAG_TWO_STEP) != 0;
                            self.state.on_sync(
                                m.header.sequence_id,
                                m.timestamp.total_nanos() as i128,
                                two_step,
                                t2,
                            );
                            self.publish();
                        }
                        PtpMessage::FollowUp(m) => {
                            self.state.on_follow_up(
                                m.header.sequence_id,
                                m.timestamp.total_nanos() as i128,
                            );
                            self.publish();
                        }
                        PtpMessage::DelayResp(d) => {
                            let is_us = d.requesting_port.clock_identity == self.our_identity;
                            self.state.on_delay_resp(
                                d.header.sequence_id,
                                is_us,
                                d.receive_timestamp.total_nanos() as i128,
                            );
                            self.publish();
                        }
                        PtpMessage::Other(_) => {}
                    }
                }
                _ = delay_tick.tick() => {
                    let our_port = PortIdentity { clock_identity: self.our_identity, port: 1 };
                    if let Ok(req) = build_delay_req(our_port, self.seq) {
                        let t3 = now_nanos();
                        self.state.on_delay_req_sent(self.seq, t3);
                        let _ = self.send_sock.send_to(&req, self.dest).await;
                        self.seq = self.seq.wrapping_add(1);
                    }
                }
                r = shutdown.changed() => {
                    if r.is_err() || *shutdown.borrow() { break; }
                }
            }
        }
        Ok(())
    }
}

/// Live-Status des PTP-**Masters** (für Daemon/UI).
#[derive(Debug, Clone, Default)]
pub struct PtpMasterStatus {
    /// true = wir sind aktiver Grandmaster (kein besserer GM im Netz).
    pub active: bool,
    pub announces_sent: u64,
    pub syncs_sent: u64,
    pub delay_resps_sent: u64,
    /// Ein besserer fremder GM (falls gesehen) — dann treten wir zurück.
    pub better_master: Option<ClockIdentity>,
}

/// PTP-**Master/Grandmaster**: sendet Announce + Sync/Follow_Up (two-step) und
/// beantwortet Delay_Req mit Delay_Resp. Ein **BMCA-Yield** sorgt dafür, dass wir
/// zurücktreten, sobald ein besserer fremder GM annonciert — so entstehen keine
/// zwei konkurrierenden Master. Timestamping ist Software (lokale Systemzeit);
/// HW-Timestamping ist die spätere Verfeinerung hinter derselben Struktur.
pub struct PtpMaster {
    listener: PtpListener,
    send: UdpSocket,
    group: Ipv4Addr,
    our_identity: ClockIdentity,
    priority1: u8,
    status: Arc<Mutex<PtpMasterStatus>>,
    announce_seq: u16,
    sync_seq: u16,
    /// Bester fremder Datensatz (für BMCA-Yield) + Stale-Zähler.
    best_foreign: Option<ClockDataset>,
    foreign_stale: u8,
}

impl PtpMaster {
    /// Baut den Master auf einem Interface. `priority1` steuert die BMCA-Stärke
    /// (kleiner = stärker; 128 = Standard). `status` wird laufend aktualisiert.
    pub fn bind(
        iface: Ipv4Addr,
        our_identity: ClockIdentity,
        priority1: u8,
        status: Arc<Mutex<PtpMasterStatus>>,
    ) -> io::Result<Self> {
        let listener = PtpListener::bind(iface)?;
        let group: Ipv4Addr = PTP_MULTICAST.parse().unwrap();
        // Kein Multicast-Loop: wir müssen unsere eigenen Nachrichten nicht hören.
        let send = bind_sender(
            &MulticastConfig::new(group, PTP_EVENT_PORT).with_interface(iface),
            false,
        )?;
        Ok(Self {
            listener,
            send,
            group,
            our_identity,
            priority1,
            status,
            announce_seq: 0,
            sync_seq: 0,
            best_foreign: None,
            foreign_stale: 0,
        })
    }

    /// Unser eigener BMCA-Datensatz (frei laufende Uhr, Klasse 248).
    fn our_dataset(&self) -> ClockDataset {
        ClockDataset {
            priority1: self.priority1,
            clock_class: 248,     // free-running (ehrlich: kein GPS/Atom)
            clock_accuracy: 0xFE, // unknown
            offset_scaled_log_variance: 0xFFFF,
            priority2: 128,
            clock_identity: self.our_identity,
            steps_removed: 0,
        }
    }

    /// Sind wir aktiver GM? (Nur wenn kein besserer fremder GM bekannt ist.)
    fn is_active(&self) -> bool {
        match &self.best_foreign {
            Some(f) => ClockDataset::compare(&self.our_dataset(), f) == BmcaOrder::ABetter,
            None => true,
        }
    }

    async fn send_to_port(&self, buf: &[u8], port: u16) {
        let dest = std::net::SocketAddr::from((self.group, port));
        let _ = self.send.send_to(buf, dest).await;
    }

    fn base_header(&self, mt: MessageType, seq: u16, log_interval: i8, flags: u16) -> PtpHeader {
        PtpHeader {
            message_type: mt,
            version: 2,
            message_length: 0, // setzt der jeweilige write()
            domain: 0,
            flags,
            correction: 0,
            source_port: PortIdentity {
                clock_identity: self.our_identity,
                port: 1,
            },
            sequence_id: seq,
            control: 0,
            log_message_interval: log_interval,
        }
    }

    async fn send_announce(&mut self) {
        let ds = self.our_dataset();
        let ann = Announce {
            // Flag ptpTimescale (0x0008) setzen — sonst deuten Empfänger unsere
            // Zeit als ARB-Zeitskala ("not using PTP timescale").
            header: self.base_header(
                MessageType::Announce,
                self.announce_seq,
                0,
                FLAG_PTP_TIMESCALE,
            ),
            origin_timestamp: PtpTimestamp::from_nanos(now_nanos()),
            current_utc_offset: 37,
            gm_priority1: ds.priority1,
            gm_clock_class: ds.clock_class,
            gm_clock_accuracy: ds.clock_accuracy,
            gm_offset_scaled_log_variance: ds.offset_scaled_log_variance,
            gm_priority2: ds.priority2,
            gm_identity: self.our_identity,
            steps_removed: 0,
            time_source: 0xA0, // INTERNAL_OSCILLATOR
        };
        let mut buf = [0u8; Announce::LEN];
        if ann.write(&mut buf).is_ok() {
            self.send_to_port(&buf, PTP_GENERAL_PORT).await;
            self.announce_seq = self.announce_seq.wrapping_add(1);
            self.status.lock().unwrap().announces_sent += 1;
        }
    }

    async fn send_sync_pair(&mut self) {
        let t1 = now_nanos();
        // Sync (Event/319) mit two-step-Flag; preciseOriginTimestamp folgt im Follow_Up.
        let sync = TimestampedMsg {
            header: self.base_header(MessageType::Sync, self.sync_seq, -2, FLAG_TWO_STEP),
            timestamp: PtpTimestamp::from_nanos(t1),
        };
        let mut sbuf = [0u8; TimestampedMsg::LEN];
        if sync.write(&mut sbuf).is_ok() {
            self.send_to_port(&sbuf, PTP_EVENT_PORT).await;
        }
        // Follow_Up (General/320) trägt den (präzisen) Sende-Zeitstempel t1.
        let fup = TimestampedMsg {
            header: self.base_header(MessageType::FollowUp, self.sync_seq, -2, 0),
            timestamp: PtpTimestamp::from_nanos(t1),
        };
        let mut fbuf = [0u8; TimestampedMsg::LEN];
        if fup.write(&mut fbuf).is_ok() {
            self.send_to_port(&fbuf, PTP_GENERAL_PORT).await;
        }
        self.sync_seq = self.sync_seq.wrapping_add(1);
        self.status.lock().unwrap().syncs_sent += 1;
    }

    async fn answer_delay_req(&mut self, req: &PtpHeader) {
        let resp = DelayResp {
            // logMessageInterval = 0 (1 s minDelayReqInterval); 0x7f wäre "bogus".
            header: self.base_header(MessageType::DelayResp, req.sequence_id, 0, 0),
            receive_timestamp: PtpTimestamp::from_nanos(now_nanos()), // t4
            requesting_port: req.source_port,
        };
        let mut buf = [0u8; DelayResp::LEN];
        if resp.write(&mut buf).is_ok() {
            self.send_to_port(&buf, PTP_GENERAL_PORT).await;
            self.status.lock().unwrap().delay_resps_sent += 1;
        }
    }

    /// Verarbeitet eine fremde Announce für den BMCA-Yield.
    fn note_foreign_announce(&mut self, a: &Announce) {
        if a.gm_identity == self.our_identity {
            return; // eigene Announce (falls doch geloopt)
        }
        let ds = a.to_clock_dataset();
        self.best_foreign = Some(ds);
        self.foreign_stale = 0;
        let better = ClockDataset::compare(&self.our_dataset(), &ds) == BmcaOrder::BBetter;
        let mut st = self.status.lock().unwrap();
        st.better_master = if better { Some(a.gm_identity) } else { None };
        if better {
            tracing::warn!(gm = ?a.gm_identity, "besserer PTP-GM erkannt — Master tritt zurück");
        }
    }

    /// Läuft bis zum Shutdown: sendet periodisch Announce/Sync/Follow_Up (solange
    /// aktiv) und beantwortet Delay_Req.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        let mut announce_tick = interval(Duration::from_secs(1));
        announce_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut sync_tick = interval(Duration::from_millis(250));
        sync_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                res = self.listener.recv() => {
                    let (msg, _from, _bytes) = res?;
                    match msg {
                        PtpMessage::Announce(a) => self.note_foreign_announce(&a),
                        PtpMessage::Other(h) if h.message_type == MessageType::DelayReq => {
                            self.answer_delay_req(&h).await;
                        }
                        _ => {} // eigene/andere Nachrichten ignorieren
                    }
                }
                _ = announce_tick.tick() => {
                    // Stale-Zähler: verschwindet der fremde GM, werden wir wieder aktiv.
                    if self.best_foreign.is_some() {
                        self.foreign_stale = self.foreign_stale.saturating_add(1);
                        if self.foreign_stale >= 3 {
                            self.best_foreign = None;
                            self.status.lock().unwrap().better_master = None;
                        }
                    }
                    let active = self.is_active();
                    self.status.lock().unwrap().active = active;
                    if active {
                        self.send_announce().await;
                    }
                }
                _ = sync_tick.tick() => {
                    if self.is_active() {
                        self.send_sync_pair().await;
                    }
                }
                r = shutdown.changed() => {
                    if r.is_err() || *shutdown.borrow() { break; }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taktwerk_core::ptp::wire::{PortIdentity, PtpTimestamp};

    fn announce_bytes() -> Vec<u8> {
        let ann = Announce {
            header: PtpHeader {
                message_type: MessageType::Announce,
                version: 2,
                message_length: Announce::LEN as u16,
                domain: 0,
                flags: 0,
                correction: 0,
                source_port: PortIdentity {
                    clock_identity: [0xAB; 8],
                    port: 1,
                },
                sequence_id: 1,
                control: 5,
                log_message_interval: 1,
            },
            origin_timestamp: PtpTimestamp::default(),
            current_utc_offset: 37,
            gm_priority1: 128,
            gm_clock_class: 6,
            gm_clock_accuracy: 0x21,
            gm_offset_scaled_log_variance: 0,
            gm_priority2: 128,
            gm_identity: [0xAB; 8],
            steps_removed: 0,
            time_source: 0x20,
        };
        let mut buf = vec![0u8; Announce::LEN];
        ann.write(&mut buf).unwrap();
        buf
    }

    #[test]
    fn classify_announce() {
        let buf = announce_bytes();
        match classify(&buf) {
            Some(PtpMessage::Announce(a)) => {
                assert_eq!(a.gm_clock_class, 6);
                assert_eq!(a.to_clock_dataset().clock_identity, [0xAB; 8]);
            }
            other => panic!("erwartete Announce, bekam {other:?}"),
        }
    }

    #[test]
    fn classify_ignores_garbage() {
        assert!(classify(&[0u8; 4]).is_none()); // zu kurz für Header
    }
}
